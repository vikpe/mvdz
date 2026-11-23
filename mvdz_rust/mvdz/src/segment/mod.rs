//! Transform-coded segments: the walk and the stream bodies
//! (FORMAT.md 3).

use crate::error::{Error, Result, bad};
use crate::fields::{put_usize, read_bytes, read_u32, read_usize};
use crate::format::{MAX_MODELS_LEN, MAX_MODELS_RAW_LEN, MAX_SEGMENT_RAW};
use crate::lz;
use crate::parallel::par_map;
use crate::protocol::{FrameAt, FrameRun, ProtocolState, frame_at, frame_len, frame_run};
use crate::rans;
use crate::streams::{
    FIRST_BLOB_STREAM, LZ_DISTANCE, LZ_LENGTH, LZ_LITERAL, RAW, STREAM_COUNT, TAIL, is_blob,
    is_structural,
};
use crate::walker::{SegmentIo, SegmentWalker};
use std::io::{self, Read};

#[cfg(feature = "stats")]
pub mod stats;

pub(crate) const SEGMENT_TARGET_RAW: usize = 256 * 1024;

/// The streams of a segment, by stream id: decode reads them, encode collects what the walk
/// codes. The one part of the walk that depends on the direction.
#[derive(Debug)]
pub(crate) struct Streams<'a, const IS_ENCODE: bool> {
    writers: Vec<rans::StreamWriter>,
    encode_blobs: Vec<Vec<u8>>,
    decoders: [rans::StreamDecoder<'a>; FIRST_BLOB_STREAM],
    /// The unread rest of each.
    decode_blobs: Vec<&'a [u8]>,
    #[cfg(feature = "stats")]
    ablation: stats::Ablation,
}

impl Streams<'_, true> {
    fn encoder() -> Self {
        Self {
            writers: Vec::from_fn(FIRST_BLOB_STREAM, |_| rans::StreamWriter::default()),
            encode_blobs: Vec::from_fn(STREAM_COUNT, |_| Vec::new()),
            decoders: std::array::from_fn(|_| rans::StreamDecoder::empty()),
            decode_blobs: Vec::new(),
            #[cfg(feature = "stats")]
            ablation: stats::Ablation::None,
        }
    }
}

impl<'a> Streams<'a, false> {
    fn decoder(
        decoders: [rans::StreamDecoder<'a>; FIRST_BLOB_STREAM],
        decode_blobs: Vec<&'a [u8]>,
    ) -> Self {
        Self {
            writers: Vec::new(),
            encode_blobs: Vec::new(),
            decoders,
            decode_blobs,
            #[cfg(feature = "stats")]
            ablation: stats::Ablation::None,
        }
    }
}

impl<const IS_ENCODE: bool> Streams<'_, IS_ENCODE> {
    /// The next symbol of a structural stream; encode collects `source`. `None`: the stream
    /// is absent or the context has an empty table.
    #[inline(always)]
    pub(crate) fn symbol(&mut self, stream: usize, context: u32, source: u8) -> Option<u8> {
        #[cfg(feature = "stats")]
        if let Some(blob) = self.ablated_blob(stream) {
            self.encode_blobs[blob].push(source);
            return Some(source);
        }
        if IS_ENCODE {
            self.writers[stream].push_symbol(context, source);
            return Some(source);
        }
        debug_assert!(
            context <= 0xffff,
            "stream {stream}: context {context:#x} over 16 bits"
        );
        self.decoders[stream].symbol(context)
    }

    /// As [`Self::symbol`], for a residual of a numeric stream.
    #[inline(always)]
    pub(crate) fn residual(&mut self, stream: usize, context: u32, source: i32) -> Option<i32> {
        #[cfg(feature = "stats")]
        if let Some(blob) = self.ablated_blob(stream) {
            self.collect_ablated_residual(blob, stream, source);
            return Some(source);
        }
        if IS_ENCODE {
            self.writers[stream].push_value(context, source);
            return Some(source);
        }
        self.decoders[stream].value(context)
    }

    /// Encode: appends to a blob stream.
    #[inline]
    pub(crate) fn collect_blob(&mut self, stream: usize, bytes: &[u8]) {
        self.encode_blobs[stream].extend_from_slice(bytes);
    }

    /// Decode: fills `out` with the next bytes of a blob stream; `false` if it has fewer.
    #[inline]
    pub(crate) fn take_blob(&mut self, stream: usize, out: &mut [u8]) -> bool {
        match read_bytes(&mut self.decode_blobs[stream], out.len()) {
            Some(bytes) => {
                out.copy_from_slice(bytes);
                true
            }
            None => false,
        }
    }

    /// Decode: the unread rest of a blob stream.
    pub(crate) fn blob_rest(&self, stream: usize) -> &[u8] {
        self.decode_blobs[stream]
    }
}

fn walk_decode(streams: Streams<'_, false>, dst: &mut [u8], protocol: ProtocolState) -> Result<()> {
    let tail = streams.decode_blobs[TAIL].len();
    let mut walker = SegmentWalker::new(SegmentIo::decoder(dst, streams), protocol);
    walker.run(tail);
    let Some((streams, _)) = walker.finish() else {
        return bad("structure");
    };
    if !streams.decoders.iter().all(rans::StreamDecoder::is_done)
        || !streams.decode_blobs.iter().all(|blob| blob.is_empty())
    {
        return bad("unconsumed stream data");
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Walked {
    /// By stream id: the structural and numeric streams, the blob tokens' included.
    streams: Vec<rans::StreamSymbols>,
    /// By stream id.
    blobs: Vec<Vec<u8>>,
    raw_len: usize,
    start_protocol: ProtocolState,
}

impl Walked {
    fn new(walked: Streams<'_, true>, raw_len: usize, protocol: ProtocolState) -> Self {
        let blob_bytes: Vec<u8> = (walked.encode_blobs.iter()).flatten().copied().collect();
        let tokens = lz::tokens(&blob_bytes);
        let mut streams: Vec<rans::StreamSymbols> = (walked.writers.into_iter())
            .map(rans::StreamWriter::finish)
            .collect();
        streams[LZ_LITERAL] = tokens.literals;
        streams[LZ_LENGTH] = tokens.lens;
        streams[LZ_DISTANCE] = tokens.dists;
        Self {
            streams,
            blobs: walked.encode_blobs,
            raw_len,
            start_protocol: protocol,
        }
    }

    pub(crate) fn unstructured_len(&self) -> usize {
        self.blobs[RAW].len() + self.blobs[TAIL].len()
    }
}

/// `None`: the transform cannot represent the segment.
pub(crate) fn walk(src: &[u8], protocol: ProtocolState) -> Option<(Walked, ProtocolState)> {
    walk_streams(src, protocol, Streams::encoder())
}

fn walk_streams(
    src: &[u8],
    protocol: ProtocolState,
    streams: Streams<'_, true>,
) -> Option<(Walked, ProtocolState)> {
    let mut walker = SegmentWalker::new(SegmentIo::encoder(src, streams), protocol);
    walker.run(src.len() - framed_len(src));
    let (streams, end_protocol) = walker.finish()?;
    Some((Walked::new(streams, src.len(), protocol), end_protocol))
}

/// The whole frames from the start.
fn framed_len(src: &[u8]) -> usize {
    let mut end = 0;
    while let Some(len) = frame_len(src, end) {
        end += len;
    }
    end
}

pub(crate) fn walk_tail(src: &[u8], protocol: ProtocolState) -> Walked {
    let mut streams = Streams::encoder();
    streams.encode_blobs[TAIL] = src.to_vec();
    Walked::new(streams, src.len(), protocol)
}

/// Of the blob streams of a segment together (FORMAT.md 7).
fn max_blob_total(raw_len: usize) -> usize {
    raw_len.saturating_mul(2)
}

/// The non-empty ones, in id order.
fn blob_ids(walked: &Walked) -> Vec<usize> {
    (0..STREAM_COUNT)
        .filter(|&stream| is_blob(stream) && !walked.blobs[stream].is_empty())
        .collect()
}

/// One model per structural and numeric stream from its per-context counts over every segment
/// (FORMAT.md 8 rule 4). Segments are added a few at a time; only counts are kept.
#[derive(Debug)]
pub(crate) struct ModelBuilder {
    tallies: Vec<Option<rans::Tally>>,
}

impl Default for ModelBuilder {
    fn default() -> Self {
        Self {
            tallies: Vec::from_fn(FIRST_BLOB_STREAM, |_| None),
        }
    }
}

impl ModelBuilder {
    pub(crate) fn add(&mut self, segments: &[&Walked], threads: usize) {
        // streams are independent: in parallel, each over the segments in order
        par_map(
            threads,
            self.tallies.iter_mut().enumerate(),
            |(stream, tally)| {
                let alphabet = if is_structural(stream) {
                    256
                } else {
                    rans::NUMERIC_ALPHABET
                };
                for walked in segments {
                    let coded = &walked.streams[stream];
                    if !coded.symbols.is_empty() {
                        tally
                            .get_or_insert_with(|| rans::Tally::new(alphabet))
                            .add(&coded.symbols, &coded.contexts);
                    }
                }
            },
        );
    }

    pub(crate) fn models(self, threads: usize) -> Vec<Option<rans::FileModel>> {
        let mut models: Vec<Option<rans::FileModel>> = par_map(threads, &self.tallies, |tally| {
            tally.as_ref().map(rans::Tally::file_model)
        });
        fit_models_block(&mut models, MAX_MODELS_LEN, MAX_MODELS_RAW_LEN);
        models
    }
}

/// `ids` ascending, each with a model.
pub(crate) fn models_body(models: &[Option<rans::FileModel>], ids: &[usize]) -> Vec<u8> {
    models_block(models, ids).body
}

fn models_block(models: &[Option<rans::FileModel>], ids: &[usize]) -> rans::ModelsBlock {
    let listed: Vec<(usize, &[u8])> = ids
        .iter()
        .map(|&stream| (stream, &models[stream].as_ref().expect("a model").bytes[..]))
        .collect();
    rans::write_models_block(&listed)
}

/// Drops the largest models until the models block is within the limits of FORMAT.md
/// 7; a segment with a dropped stream is then not transform-coded. The models of
/// the blob tokens stay: every fallback but the stored segment needs them.
fn fit_models_block(models: &mut [Option<rans::FileModel>], max_len: u64, max_raw_len: u64) {
    loop {
        let ids: Vec<usize> = (0..models.len())
            .filter(|&stream| models[stream].is_some())
            .collect();
        if ids.is_empty() {
            return;
        }
        let block = models_block(models, &ids);
        if block.body.len() as u64 <= max_len && block.raw_len as u64 <= max_raw_len {
            return;
        }
        let is_dropped_last =
            |stream: usize| [LZ_LITERAL, LZ_LENGTH, LZ_DISTANCE].contains(&stream);
        let largest = (ids.into_iter())
            .filter(|&stream| !is_dropped_last(stream))
            .max_by_key(|&stream| {
                let raw_len = models[stream].as_ref().map_or(0, |model| model.bytes.len());
                (raw_len, stream)
            });
        let Some(largest) = largest else {
            return;
        };
        models[largest] = None;
    }
}

/// A structural or numeric stream as it is stored.
#[derive(Debug)]
struct CodedStream {
    stream: usize,
    body: Vec<u8>,
}

/// `None` if a structural or numeric stream has no model in `file_models` (by stream id) or
/// a symbol the model has no code for, or the blob streams hold more than the format allows.
pub(crate) fn encode(walked: &Walked, file_models: &[Option<rans::FileModel>]) -> Option<Vec<u8>> {
    let file_model = |stream: usize| file_models.get(stream)?.as_ref();
    let blob_total: usize = walked.blobs.iter().map(Vec::len).sum();
    if blob_total > max_blob_total(walked.raw_len) {
        return None;
    }
    let mut coded_streams: Vec<CodedStream> = Vec::new();
    for (stream, symbols) in walked.streams.iter().enumerate() {
        if !symbols.symbols.is_empty() {
            coded_streams.push(CodedStream {
                stream,
                body: rans::encode_stream(symbols, file_model(stream)?)?,
            });
        }
    }
    // the blob streams have no body of their own: their bytes are the tokens' (`lz.rs`)
    let blobs = blob_ids(walked);
    let mut out = Vec::new();
    let protocol = walked.start_protocol;
    for flags in [protocol.fte, protocol.fte2, protocol.mvd1] {
        out.extend_from_slice(&flags.to_le_bytes());
    }
    put_usize(&mut out, coded_streams.len() + blobs.len());
    for coded in &coded_streams {
        put_usize(&mut out, coded.stream);
        put_usize(&mut out, coded.body.len());
    }
    for &stream in &blobs {
        put_usize(&mut out, stream);
        put_usize(&mut out, walked.blobs[stream].len());
    }
    for coded in &coded_streams {
        out.extend_from_slice(&coded.body);
    }
    Some(out)
}

/// The segment as it is, behind a transform header without streams (FORMAT.md 3.1):
/// the one layout with a size bound, for a segment no model fits.
pub(crate) fn stored(raw: &[u8], protocol: ProtocolState) -> Vec<u8> {
    /// `fte`, `fte2`, `mvd1` and the stream count.
    const TRANSFORM_FIELDS_LEN: usize = 4 * 4;
    const NO_STREAMS: usize = 0;
    let mut out = Vec::with_capacity(TRANSFORM_FIELDS_LEN + raw.len());
    for flags in [protocol.fte, protocol.fte2, protocol.mvd1] {
        out.extend_from_slice(&flags.to_le_bytes());
    }
    put_usize(&mut out, NO_STREAMS);
    out.extend_from_slice(raw);
    out
}

/// The segments of a demo, cut as it is read (FORMAT.md 8 rule 2).
#[derive(Debug)]
pub(crate) struct SegmentReader<R> {
    input: R,
    is_input_done: bool,
    /// Read, not yet in a segment.
    unread: Vec<u8>,
    /// The whole frames at the start of `unread`.
    framed_len: usize,
    /// `Some` while the bytes after them do not frame.
    frame_run_search_offset: Option<usize>,
}

impl<R: Read> SegmentReader<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            input,
            is_input_done: false,
            unread: Vec::new(),
            framed_len: 0,
            frame_run_search_offset: None,
        }
    }

    /// `None` at the end of the demo.
    pub(crate) fn next_segment(&mut self) -> io::Result<Option<Vec<u8>>> {
        const READ_LEN: u64 = 64 * 1024;
        loop {
            if let Some(len) = self.next_segment_len() {
                self.framed_len = 0;
                return Ok(Some(self.unread.drain(..len).collect()));
            }
            if self.is_input_done {
                return Ok(None);
            }
            let read_len = (&mut self.input)
                .take(READ_LEN)
                .read_to_end(&mut self.unread)?;
            self.is_input_done = read_len == 0;
        }
    }

    /// `None`: the next cut needs more of the input, or nothing is left.
    fn next_segment_len(&mut self) -> Option<usize> {
        const MAX_LEN: usize = MAX_SEGMENT_RAW as usize;
        loop {
            let Some(search) = self.frame_run_search_offset else {
                match frame_at(&self.unread, self.framed_len) {
                    FrameAt::Whole(len) if self.framed_len + len <= MAX_LEN => {
                        self.framed_len += len;
                        if self.framed_len >= SEGMENT_TARGET_RAW {
                            return Some(self.framed_len);
                        }
                    }
                    FrameAt::Incomplete if !self.is_input_done => return None,
                    _ if self.unread.is_empty() => return None,
                    // not before the target length: a break in the frames makes no
                    // segment short
                    _ => self.frame_run_search_offset = Some(SEGMENT_TARGET_RAW),
                }
                continue;
            };
            if search >= MAX_LEN {
                self.frame_run_search_offset = Some(search - MAX_LEN);
                return Some(MAX_LEN);
            }
            if search >= self.unread.len() {
                let is_last_segment = self.is_input_done && !self.unread.is_empty();
                return is_last_segment.then_some(self.unread.len());
            }
            match frame_run(&self.unread, search) {
                FrameRun::Yes => {
                    self.frame_run_search_offset = None;
                    if search > 0 {
                        return Some(search);
                    }
                }
                FrameRun::Incomplete if !self.is_input_done => return None,
                FrameRun::Incomplete | FrameRun::No => {
                    self.frame_run_search_offset = Some(search + 1);
                }
            }
        }
    }
}

/// The segment bounds of a demo held whole, from 0 to its length.
#[cfg(any(test, feature = "fuzz", feature = "stats"))]
pub(crate) fn cut_points(src: &[u8]) -> Vec<usize> {
    let mut segments = SegmentReader::new(src);
    let mut cuts = vec![0];
    while let Ok(Some(segment)) = segments.next_segment() {
        cuts.push(cuts[cuts.len() - 1] + segment.len());
    }
    cuts
}

/// The time bytes of the whole frames `segment` starts with, summed.
pub(crate) fn duration_ms(segment: &[u8]) -> u32 {
    let mut duration_ms = 0;
    let mut offset = 0;
    while let Some(len) = frame_len(segment, offset) {
        duration_ms += u32::from(segment[offset]);
        offset += len;
    }
    duration_ms
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamEntry {
    id: usize,
    /// The body length of a structural or numeric stream, the byte count of a blob stream.
    size: usize,
}

#[derive(Debug)]
pub(crate) struct TransformHeader {
    protocol: ProtocolState,
    /// Empty for a stored segment.
    entries: Vec<StreamEntry>,
    blob_total: usize,
    body_offset: usize,
}

/// `raw_len`: the decoded length.
pub(crate) fn parse_transform_header(body: &[u8], raw_len: usize) -> Result<TransformHeader> {
    let header_error = Error::Corrupt("segment header");
    let mut rest = body;
    let fte = read_u32(&mut rest).ok_or(header_error)?;
    let fte2 = read_u32(&mut rest).ok_or(header_error)?;
    let mvd1 = read_u32(&mut rest).ok_or(header_error)?;
    let stream_count = read_usize(&mut rest).ok_or(header_error)?;
    if stream_count > STREAM_COUNT {
        return bad("stream count");
    }
    let table_error = Error::Corrupt("stream table");
    let mut entries = Vec::with_capacity(stream_count);
    let mut total = 0usize;
    let mut blob_total = 0usize;
    for _ in 0..stream_count {
        let id = read_usize(&mut rest).ok_or(table_error)?;
        if id >= STREAM_COUNT {
            return bad("stream id");
        }
        let size = read_usize(&mut rest).ok_or(table_error)?;
        if is_blob(id) {
            blob_total = blob_total.checked_add(size).ok_or(table_error)?;
        } else {
            total = total.checked_add(size).ok_or(table_error)?;
        }
        if entries
            .last()
            .is_some_and(|previous: &StreamEntry| id <= previous.id)
        {
            return bad("stream order");
        }
        entries.push(StreamEntry { id, size });
    }
    // refused here, before the blob bytes are allocated
    if blob_total > max_blob_total(raw_len) {
        return bad("blob streams");
    }
    let is_stored = stream_count == 0;
    if is_stored {
        if rest.len() != raw_len {
            return bad("stored segment");
        }
    } else if total != rest.len() {
        return bad("stream bodies");
    }
    Ok(TransformHeader {
        protocol: ProtocolState::new(fte, fte2, mvd1),
        entries,
        blob_total,
        body_offset: body.len() - rest.len(),
    })
}

/// `dst` is the exact raw length; the caller checks the CRC.
pub(crate) fn decode(
    body: &[u8],
    transform_header: &TransformHeader,
    dst: &mut [u8],
    file_models: Option<&rans::Models>,
) -> Result<()> {
    let mut rest = body
        .get(transform_header.body_offset..)
        .ok_or(Error::Corrupt("stream body"))?;
    let is_stored = transform_header.entries.is_empty();
    if is_stored {
        if rest.len() != dst.len() {
            return bad("stored segment");
        }
        dst.copy_from_slice(rest);
        return Ok(());
    }
    let mut decoders: [rans::StreamDecoder; FIRST_BLOB_STREAM] =
        std::array::from_fn(|_| rans::StreamDecoder::empty());
    for entry in &transform_header.entries {
        if !is_blob(entry.id) {
            let body = read_bytes(&mut rest, entry.size).ok_or(Error::Corrupt("stream body"))?;
            let model = rans::file_model(file_models, entry.id)?;
            decoders[entry.id] = rans::StreamDecoder::new(body, model)?;
        }
    }
    let blob_bytes = lz::expand(&mut decoders, transform_header.blob_total)?;
    // the literals are checked with the other streams, after the walk
    let are_blob_tokens_done = decoders[LZ_LENGTH].is_done() && decoders[LZ_DISTANCE].is_done();
    if !are_blob_tokens_done {
        return bad("blob tokens");
    }
    let mut blobs: Vec<&[u8]> = vec![&[]; STREAM_COUNT];
    let mut at = 0;
    for entry in &transform_header.entries {
        if is_blob(entry.id) {
            blobs[entry.id] = &blob_bytes[at..at + entry.size];
            at += entry.size;
        }
    }
    walk_decode(
        Streams::decoder(decoders, blobs),
        dst,
        transform_header.protocol,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streams::{
        COMMAND, ENTITY_BITS, ESCAPE_LENGTH, USERCMD, USERCMD_ANGLE, is_numeric, is_structural,
    };
    use anyhow::Context as _;
    use pretty_assertions::assert_eq;

    fn build_models(segments: &[&Walked]) -> Vec<Option<rans::FileModel>> {
        let mut model_builder = ModelBuilder::default();
        model_builder.add(segments, 1);
        model_builder.models(1)
    }

    fn decoded(
        body: &[u8],
        transform_header: &TransformHeader,
        len: usize,
        file_models: Option<&rans::Models>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut dst = vec![0u8; len];
        decode(body, transform_header, &mut dst, file_models)?;
        Ok(dst)
    }

    fn coded(src: &[u8]) -> anyhow::Result<(Vec<u8>, rans::Models)> {
        let (walked, _) = walk(src, ProtocolState::default()).context("the demo walks")?;
        coded_walked(&walked)
    }

    fn coded_walked(walked: &Walked) -> anyhow::Result<(Vec<u8>, rans::Models)> {
        let models = build_models(&[walked]);
        let ids: Vec<usize> = (0..models.len())
            .filter(|&stream| models[stream].is_some())
            .collect();
        let file_models = rans::read_models_block(&models_body(&models, &ids), 1, u64::MAX)?;
        let body = encode(walked, &models).context("a transform body")?;
        Ok((body, file_models))
    }

    fn demo() -> Vec<u8> {
        let mut demo = Vec::new();
        for frame in 0..12u8 {
            let mut payload = vec![42, 0];
            payload.extend_from_slice(&0x3fu16.to_le_bytes());
            payload.push(frame);
            for field in 0..6u16 {
                payload.extend_from_slice(&(100 * field + u16::from(frame) * 7).to_le_bytes());
            }
            payload.extend_from_slice(&[47, 1, 2]);
            payload.extend_from_slice(&(1u16 << 9 | 5).to_le_bytes());
            payload.extend_from_slice(&(10 + u16::from(frame)).to_le_bytes());
            payload.extend_from_slice(&[0, 0]);
            payload.extend_from_slice(&[6, 9, 0, 1]);
            payload.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
            payload.extend_from_slice(&[3, 1, frame, 8, 2]);
            payload.extend_from_slice(b"hi\0");
            demo.push(13);
            demo.push(6);
            demo.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            demo.extend_from_slice(&payload);
        }
        demo
    }

    #[test]
    fn cuts_follow_the_frames_and_the_segment_cap() {
        let max_len = MAX_SEGMENT_RAW as usize;
        let frame = [&[1, 6][..], &100u32.to_le_bytes(), &[1; 100]].concat();
        let frames_per_segment = SEGMENT_TARGET_RAW.div_ceil(frame.len());
        let framed = frame.repeat(frames_per_segment + 20);
        let cut_inside_a_frame = &framed[..framed.len() - 1];
        let no_frames = vec![0u8; max_len];
        let frames_after_the_cap = [&no_frames[..], &frame.repeat(16)].concat();
        let first_cut = frames_per_segment * frame.len();
        assert_eq!(cut_points(&[]), [0]);
        assert_eq!(cut_points(&framed), [0, first_cut, framed.len()]);
        assert_eq!(
            cut_points(cut_inside_a_frame),
            [0, first_cut, cut_inside_a_frame.len()]
        );
        assert_eq!(
            cut_points(&frames_after_the_cap),
            [0, max_len, frames_after_the_cap.len()]
        );
        assert_eq!(cut_points(&no_frames.repeat(2)), [0, max_len, 2 * max_len]);
    }

    #[test]
    fn segment_round_trip() -> anyhow::Result<()> {
        let src = demo();
        let (body, file_models) = coded(&src)?;
        let transform_header = parse_transform_header(&body, src.len())?;
        let dst = decoded(&body, &transform_header, src.len(), Some(&file_models))?;
        assert!(dst == src);
        Ok(())
    }

    /// Entity 5 keeps its bits (`ENTITY_BITS` symbol 0 under its own context) while entities
    /// 10 to 39 toggle theirs, so the fallback table does not decode entity 5's symbols.
    fn entity_demo() -> Vec<u8> {
        let mut demo = Vec::new();
        for frame in 0..12u8 {
            let mut payload = vec![crate::protocol::SVC_PACKET_ENTITIES];
            payload.extend_from_slice(&(1u16 << 11 | 5).to_le_bytes());
            payload.extend_from_slice(&[frame, 1]);
            for entity in 10..40u16 {
                let origin_bit = 9 + u16::from(frame % 2);
                payload.extend_from_slice(&(1u16 << origin_bit | entity).to_le_bytes());
                payload.extend_from_slice(&[entity as u8, frame]);
            }
            payload.extend_from_slice(&[0, 0]);
            demo.push(13);
            demo.push(6);
            demo.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            demo.extend_from_slice(&payload);
        }
        demo
    }

    /// FORMAT.md 4.8: the entity keeps 7 bits of its word; bit 7 of the `ENTITY_BITS` symbol
    /// falls off, so the output is the demo.
    #[test]
    fn entity_bits_symbol_bit_7_falls_off() -> anyhow::Result<()> {
        let src = entity_demo();
        let (mut walked, _) = walk(&src, ProtocolState::default()).context("the demo walks")?;
        walked.streams[ENTITY_BITS].symbols[0] |= 0x80;
        let (body, file_models) = coded_walked(&walked)?;
        let transform_header = parse_transform_header(&body, src.len())?;
        let dst = decoded(&body, &transform_header, src.len(), Some(&file_models))?;
        assert!(dst == src);
        Ok(())
    }
    /// FORMAT.md 4.5: one command escaped, as an encoder that sizes it would; the next is
    /// walked.
    #[test]
    fn walk_goes_on_after_an_escape() -> anyhow::Result<()> {
        const SVC_NOP: u8 = 1;
        const DEM_ALL: u8 = 6;
        let payload = [SVC_NOP, SVC_NOP];
        let payload_len = (payload.len() as u32).to_le_bytes();
        let src = [&[0, DEM_ALL][..], &payload_len, &payload].concat();
        let (mut walked, _) = walk(&src, ProtocolState::default()).context("the demo walks")?;
        let command_after_escape_context = 1 << 4 | u16::from(DEM_ALL) << 12;
        walked.streams[COMMAND].symbols[0] = 0;
        walked.streams[COMMAND].contexts[1] = command_after_escape_context;
        walked.blobs[ESCAPE_LENGTH] = 1u32.to_le_bytes().to_vec();
        walked.blobs[RAW] = vec![SVC_NOP];
        let blob_bytes: Vec<u8> = walked.blobs.iter().flatten().copied().collect();
        let tokens = lz::tokens(&blob_bytes);
        walked.streams[LZ_LITERAL] = tokens.literals;
        walked.streams[LZ_LENGTH] = tokens.lens;
        walked.streams[LZ_DISTANCE] = tokens.dists;
        let (body, file_models) = coded_walked(&walked)?;
        let transform_header = parse_transform_header(&body, src.len())?;
        let dst = decoded(&body, &transform_header, src.len(), Some(&file_models))?;
        assert_eq!(dst, src);
        Ok(())
    }

    /// usercmd blocks on and off the grid, other lengths, two in one payload; streams `USERCMD` and
    /// `USERCMD_ANGLE` only where a segment has one.
    #[test]
    fn usercmd_round_trip() -> anyhow::Result<()> {
        // FORMAT.md 4.3: player, dropnum, msec, grid, three 2-byte moves, buttons, impulse
        const USERCMD_MIN_SYMBOLS: usize = 12;
        let mut src = demo();
        let odd = [-0.0f32, f32::NAN, 180.0, -180.0, 1.2345, 359.99];
        for frame in 0..300u32 {
            let mut payload = Vec::new();
            for block in 0..=u32::from(frame % 37 == 0) {
                let mut usercmd = vec![((frame + block) % 5) as u8, u8::from(frame % 50 == 0), 13];
                let yaw = f32::from(((frame * 97) as u16).cast_signed()) * (45.0 / 8192.0);
                let pitch = if frame % 11 == 0 {
                    odd[(frame as usize / 11) % odd.len()]
                } else {
                    -f32::from(frame as u16) * (45.0 / 8192.0)
                };
                for angle in [pitch, yaw, 0.0] {
                    usercmd.extend_from_slice(&angle.to_le_bytes());
                }
                for movement in [if frame % 3 == 0 { 400i16 } else { 0 }, -508, 0] {
                    usercmd.extend_from_slice(&movement.to_le_bytes());
                }
                usercmd.extend_from_slice(&[(frame % 4) as u8, u8::from(frame % 90 == 0)]);
                payload.extend_from_slice(&23u32.to_le_bytes());
                payload.extend_from_slice(&1u16.to_le_bytes());
                payload.extend_from_slice(&usercmd);
            }
            if frame % 50 == 7 {
                payload.extend_from_slice(&3u32.to_le_bytes());
                payload.extend_from_slice(&1u16.to_le_bytes());
                payload.extend_from_slice(&[1, 2, 3]);
            }
            src.push(if frame % 3 == 0 { 13 } else { 0 });
            src.push(3);
            src.extend_from_slice(&0u32.to_le_bytes());
            src.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            src.extend_from_slice(&payload);
        }
        src.extend(demo());
        let (walked, _) = walk(&src, ProtocolState::default()).context("the demo walks")?;
        let usercmd_symbol_count = walked.streams[USERCMD].symbols.len();
        assert!(
            usercmd_symbol_count >= (300 + 9) * USERCMD_MIN_SYMBOLS,
            "{usercmd_symbol_count}"
        );
        let (body, file_models) = coded(&src)?;
        let transform_header = parse_transform_header(&body, src.len())?;
        let dst = decoded(&body, &transform_header, src.len(), Some(&file_models))?;
        assert!(dst == src);
        // without usercmd blocks neither stream is in the table
        let plain = demo();
        let (body, _) = coded(&plain)?;
        let transform_header = parse_transform_header(&body, plain.len())?;
        assert!(
            transform_header
                .entries
                .iter()
                .all(|entry| entry.id != USERCMD && entry.id != USERCMD_ANGLE)
        );
        Ok(())
    }

    #[test]
    fn blob_streams_over_twice_the_segment_are_refused() {
        use crate::streams::SET;
        let raw_len = 100;
        // fte, fte2, mvd1, the stream count; then (id, count) per blob stream
        let header = |blob_streams: &[(usize, usize)]| {
            let mut body = Vec::new();
            for field in [0, 0, 0, blob_streams.len()] {
                put_usize(&mut body, field);
            }
            for &(id, count) in blob_streams {
                put_usize(&mut body, id);
                put_usize(&mut body, count);
            }
            parse_transform_header(&body, raw_len).map(|_| ())
        };
        assert_eq!(header(&[(RAW, raw_len), (TAIL, raw_len)]), Ok(()));
        assert_eq!(
            header(&[(RAW, raw_len), (TAIL, raw_len), (SET, 1)]),
            Err(Error::Corrupt("blob streams"))
        );
    }

    /// Over the limit, the largest headers go first; their segments are not transform-coded.
    #[test]
    fn models_block_limit() -> anyhow::Result<()> {
        let src = demo();
        let (walked, _) = walk(&src, ProtocolState::default()).context("the demo walks")?;
        let mut models = build_models(&[&walked, &walked]);
        let ids = |candidates: &[Option<rans::FileModel>]| -> Vec<usize> {
            (0..candidates.len())
                .filter(|&stream| candidates[stream].is_some())
                .collect()
        };
        let all = ids(&models);
        assert!(
            all.iter().any(|&stream| is_numeric(stream))
                && all.iter().any(|&stream| is_structural(stream))
        );
        let block = models_block(&models, &all);
        let (len, raw_len) = (block.body.len() as u64, block.raw_len as u64);
        let is_blob_token_model =
            |stream: usize| [LZ_LITERAL, LZ_LENGTH, LZ_DISTANCE].contains(&stream);
        let raw_model_len =
            |stream: usize| models[stream].as_ref().map_or(0, |model| model.bytes.len());
        let largest = *all
            .iter()
            .filter(|&&stream| !is_blob_token_model(stream))
            .max_by_key(|&&stream| (raw_model_len(stream), stream))
            .context("a model")?;
        let without_largest: Vec<usize> = (all.iter().copied())
            .filter(|&stream| stream != largest)
            .collect();
        fit_models_block(&mut models, len, raw_len);
        assert_eq!(ids(&models), all);
        for (max_len, max_raw_len) in [(len - 1, raw_len), (len, raw_len - 1)] {
            let mut fitted = build_models(&[&walked, &walked]);
            fit_models_block(&mut fitted, max_len, max_raw_len);
            assert_eq!(ids(&fitted), without_largest);
            assert!(
                encode(&walked, &fitted).is_none(),
                "a stream without a model: no transform body"
            );
        }
        // nothing fits: only the models of the blob tokens are kept
        fit_models_block(&mut models, 0, 0);
        assert!(ids(&models).into_iter().all(is_blob_token_model));
        Ok(())
    }
}
