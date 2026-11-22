//! Transform-coded segments: the structural walk, the numeric groups and the stream bodies
//! (FORMAT.md section 3).

use crate::error::{Error, Result, bad};
use crate::fields::{put_usize, read_bytes, read_u32, read_usize};
use crate::format::{MAX_MODELS_LEN, MAX_MODELS_RAW_LEN, MAX_SEGMENT_RAW};
use crate::lz;
use crate::numeric::{GROUPS, Group, NumericContext, NumericIo};
use crate::parallel::par_map;
use crate::protocol::{ProtocolState, frame_len};
use crate::rans;
use crate::streams::{
    FIRST_NUMERIC_STREAM, LZ_DISTANCE, LZ_LENGTH, LZ_LITERAL, RAW, STREAM_COUNT, TAIL, is_blob,
};
use crate::walker::{RecordLists, SegmentWalker, SymbolIo};

pub(crate) const SEGMENT_TARGET_RAW: usize = 256 * 1024;

#[derive(Debug)]
pub(crate) struct EncodeIo {
    pub(crate) symbols: Vec<Vec<u8>>,
    pub(crate) contexts: Vec<Vec<u16>>,
    pub(crate) blobs: Vec<Vec<u8>>,
}

impl EncodeIo {
    fn new() -> Self {
        Self {
            symbols: Vec::from_fn(FIRST_NUMERIC_STREAM, |_| Vec::new()),
            contexts: Vec::from_fn(FIRST_NUMERIC_STREAM, |_| Vec::new()),
            blobs: Vec::from_fn(STREAM_COUNT, |_| Vec::new()),
        }
    }
}

impl SymbolIo for EncodeIo {
    #[inline(always)]
    fn symbol(&mut self, stream: usize, context: u32, value: u8) -> u8 {
        debug_assert!(
            context <= 0xffff,
            "stream {stream}: context {context:#x} over 16 bits"
        );
        self.symbols[stream].push(value);
        self.contexts[stream].push(context as u16);
        value
    }
    fn put_blob(&mut self, stream: usize, bytes: &[u8]) {
        self.blobs[stream].extend_from_slice(bytes);
    }
    fn take_blob(&mut self, _: usize, _: usize) -> Option<&[u8]> {
        None
    }
    fn blob_rest(&self, _: usize) -> &[u8] {
        &[]
    }
}

#[derive(Debug)]
struct NumericEncodeIo<'a> {
    values: Vec<rans::NumericSymbolWriter>,
    src: &'a [u8],
}

impl NumericIo for NumericEncodeIo<'_> {
    #[inline(always)]
    fn code(&mut self, stream_index: usize, context: NumericContext, value: i32) -> i32 {
        self.values[stream_index].push(context, value);
        value
    }
    fn segment(&self) -> &[u8] {
        self.src
    }
    fn segment_mut(&mut self) -> &mut [u8] {
        &mut []
    }
}

#[derive(Debug)]
pub(crate) struct DecodeIo<'a> {
    structural: [rans::ByteDecoder<'a>; FIRST_NUMERIC_STREAM],
    /// The unread rest of each blob stream.
    blobs: Vec<&'a [u8]>,
    pub(crate) is_over: bool,
}

impl SymbolIo for DecodeIo<'_> {
    #[inline(always)]
    fn symbol(&mut self, stream: usize, context: u32, _: u8) -> u8 {
        debug_assert!(
            context <= 0xffff,
            "stream {stream}: context {context:#x} over 16 bits"
        );
        if let Some(value) = self.structural[stream].get(context) {
            value
        } else {
            self.is_over = true;
            // a stream that is absent or meets an empty table reads as end tokens so every
            // loop stops
            255
        }
    }
    fn put_blob(&mut self, _: usize, _: &[u8]) {}
    fn take_blob(&mut self, stream: usize, len: usize) -> Option<&[u8]> {
        read_bytes(&mut self.blobs[stream], len)
    }
    fn blob_rest(&self, stream: usize) -> &[u8] {
        self.blobs[stream]
    }
}

const MAX_GROUP_STREAMS: usize = 2;

#[derive(Debug)]
struct NumericDecodeIo<'a> {
    decoders: [rans::NumericDecoder<'a>; MAX_GROUP_STREAMS],
    dst: &'a mut [u8],
    is_over: bool,
}

impl NumericIo for NumericDecodeIo<'_> {
    #[inline(always)]
    fn code(&mut self, stream_index: usize, context: NumericContext, _: i32) -> i32 {
        if let Some(value) = self.decoders[stream_index].get(context) {
            value
        } else {
            self.is_over = true;
            0
        }
    }
    fn segment(&self) -> &[u8] {
        self.dst
    }
    fn segment_mut(&mut self) -> &mut [u8] {
        self.dst
    }
}

fn walk_decode<'a>(
    structural: [rans::ByteDecoder<'a>; FIRST_NUMERIC_STREAM],
    blobs: Vec<&'a [u8]>,
    dst: &mut [u8],
    records: &mut RecordLists,
    protocol: ProtocolState,
) -> Result<()> {
    let io = DecodeIo {
        structural,
        blobs,
        is_over: false,
    };
    let mut walker = SegmentWalker::decoder(dst, records, io, protocol);
    walker.run();
    if walker.has_error || walker.io.is_over {
        return bad("structure");
    }
    if !walker.io.structural.iter().all(rans::ByteDecoder::is_done)
        || !walker.io.blobs.iter().all(|blob| blob.is_empty())
    {
        return bad("unconsumed stream data");
    }
    Ok(())
}

/// Empty for a stream the segment does not code.
fn numeric_decoder<'a>(
    stream: usize,
    stream_bodies: &[Option<&'a [u8]>],
    file_models: Option<&'a rans::Models>,
) -> Result<rans::NumericDecoder<'a>> {
    match stream_bodies[stream] {
        Some(body) => {
            rans::NumericDecoder::new(stream, body, rans::file_model(file_models, stream)?)
        }
        None => Ok(rans::NumericDecoder::empty()),
    }
}

fn run_group(
    group: Group,
    records: &RecordLists,
    dst: &mut [u8],
    stream_bodies: &[Option<&[u8]>],
    file_models: Option<&rans::Models>,
) -> Result<()> {
    let mut decoders: [rans::NumericDecoder; MAX_GROUP_STREAMS] =
        std::array::from_fn(|_| rans::NumericDecoder::empty());
    for (stream_index, &stream) in group.streams().iter().enumerate() {
        decoders[stream_index] = numeric_decoder(stream, stream_bodies, file_models)?;
    }
    let mut io = NumericDecodeIo {
        decoders,
        dst,
        is_over: false,
    };
    group.run::<false, NumericDecodeIo>(records, &mut io);
    if io.is_over || !io.decoders.iter().all(rans::NumericDecoder::is_done) {
        return bad("numeric stream");
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Walked {
    /// With the literals of the blob bytes as stream `LZ_LITERAL`.
    io: EncodeIo,
    records: RecordLists,
    blob_lens: rans::NumericSymbols,
    blob_dists: rans::NumericSymbols,
    raw_len: usize,
    start_protocol: ProtocolState,
}

impl Walked {
    fn new(
        mut io: EncodeIo,
        records: RecordLists,
        raw_len: usize,
        protocol: ProtocolState,
    ) -> Self {
        let blob_bytes: Vec<u8> = (io.blobs.iter()).flatten().copied().collect();
        let tokens = lz::tokens(&blob_bytes);
        io.symbols[LZ_LITERAL] = tokens.literals;
        io.contexts[LZ_LITERAL] = tokens.literal_contexts;
        Self {
            io,
            records,
            blob_lens: tokens.lens,
            blob_dists: tokens.dists,
            raw_len,
            start_protocol: protocol,
        }
    }

    pub(crate) fn unstructured_len(&self) -> usize {
        self.io.blobs[RAW].len() + self.io.blobs[TAIL].len()
    }
}

/// `None`: the transform cannot represent the segment.
pub(crate) fn walk(src: &[u8], protocol: ProtocolState) -> Option<(Walked, ProtocolState)> {
    let mut records = RecordLists::for_segment(src.len());
    let (io, end_protocol, has_error) = {
        let mut walker = SegmentWalker::encoder(src, &mut records, EncodeIo::new(), protocol);
        walker.run();
        (walker.io, walker.protocol, walker.has_error)
    };
    if has_error {
        return None;
    }
    Some((Walked::new(io, records, src.len(), protocol), end_protocol))
}

pub(crate) fn walk_tail(src: &[u8], protocol: ProtocolState) -> Walked {
    let mut io = EncodeIo::new();
    io.blobs[TAIL] = src.to_vec();
    Walked::new(io, RecordLists::for_segment(0), src.len(), protocol)
}

/// Of the blob streams of a segment together (FORMAT.md section 7).
fn max_blob_total(raw_len: usize) -> usize {
    raw_len.saturating_mul(2)
}

/// The non-empty ones, in id order.
fn blob_ids(walked: &Walked) -> Vec<usize> {
    (0..STREAM_COUNT)
        .filter(|&stream| is_blob(stream) && !walked.io.blobs[stream].is_empty())
        .collect()
}

/// Per group (in [`GROUPS`] order) and stream of the group.
pub(crate) type Numerics = Vec<Vec<rans::NumericSymbols>>;

/// Run once by the encoder: the models are built from these values and the segment is coded
/// from them.
pub(crate) fn numerics(src: &[u8], walked: &Walked) -> Numerics {
    GROUPS
        .iter()
        .map(|&group| {
            let mut io = NumericEncodeIo {
                values: group
                    .streams()
                    .iter()
                    .map(|&stream| rans::NumericSymbolWriter::new(stream))
                    .collect(),
                src,
            };
            group.run::<true, NumericEncodeIo>(&walked.records, &mut io);
            io.values
                .into_iter()
                .map(rans::NumericSymbolWriter::finish)
                .collect()
        })
        .collect()
}

/// One model per structural and numeric stream from its per-context counts over every segment
/// (FORMAT.md section 8 rule 4). Segments are added a few at a time; only counts are kept.
#[derive(Debug)]
pub(crate) struct ModelBuilder {
    tallies: Vec<Option<rans::Tally>>,
    /// (group, index in the group)
    numeric_positions: Vec<Option<(usize, usize)>>,
}

impl Default for ModelBuilder {
    fn default() -> Self {
        let mut numeric_positions = vec![None; STREAM_COUNT];
        for (group_index, group) in GROUPS.iter().enumerate() {
            for (stream_index, &stream) in group.streams().iter().enumerate() {
                numeric_positions[stream] = Some((group_index, stream_index));
            }
        }
        Self {
            tallies: Vec::from_fn(STREAM_COUNT, |_| None),
            numeric_positions,
        }
    }
}

impl ModelBuilder {
    pub(crate) fn add(&mut self, segments: &[(&Walked, &Numerics)], threads: usize) {
        let numeric_positions = &self.numeric_positions;
        // streams are independent: in parallel, each over the segments in order
        par_map(
            threads,
            self.tallies.iter_mut().enumerate(),
            |(stream, tally)| {
                let mut add = |alphabet: usize, symbols: &[u8], contexts: &[u16]| {
                    if !symbols.is_empty() {
                        tally
                            .get_or_insert_with(|| rans::Tally::new(alphabet))
                            .add(symbols, contexts);
                    }
                };
                for &(walked, numerics) in segments {
                    if stream < FIRST_NUMERIC_STREAM {
                        add(256, &walked.io.symbols[stream], &walked.io.contexts[stream]);
                    }
                    let numeric = match stream {
                        LZ_LENGTH => Some(&walked.blob_lens),
                        LZ_DISTANCE => Some(&walked.blob_dists),
                        _ => numeric_positions[stream].map(|(group_index, stream_index)| {
                            &numerics[group_index][stream_index]
                        }),
                    };
                    if let Some(numeric) = numeric {
                        add(rans::NUMERIC_ALPHABET, &numeric.symbols, &numeric.contexts);
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
/// section 7; a segment with a dropped stream is then not transform-coded. The models of
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
/// `model_ids` gets the streams coded with a model.
pub(crate) fn encode(
    walked: &Walked,
    numerics: &Numerics,
    model_ids: &mut Vec<usize>,
    file_models: &[Option<rans::FileModel>],
) -> Option<Vec<u8>> {
    let file_model = |stream: usize| file_models.get(stream)?.as_ref();
    let blob_tokens = [
        (LZ_LENGTH, &walked.blob_lens),
        (LZ_DISTANCE, &walked.blob_dists),
    ];
    let blob_total: usize = walked.io.blobs.iter().map(Vec::len).sum();
    if blob_total > max_blob_total(walked.raw_len) {
        return None;
    }
    let mut coded_streams: Vec<CodedStream> = Vec::new();
    for (stream, symbols) in walked.io.symbols.iter().enumerate() {
        if !symbols.is_empty() {
            coded_streams.push(CodedStream {
                stream,
                body: rans::encode_bytes(
                    symbols,
                    &walked.io.contexts[stream],
                    file_model(stream)?,
                )?,
            });
        }
    }
    let group_numerics = (GROUPS.iter().zip(numerics))
        .flat_map(|(group, group_numerics)| group.streams().iter().copied().zip(group_numerics));
    for (stream, numeric) in group_numerics.chain(blob_tokens) {
        if !numeric.symbols.is_empty() {
            coded_streams.push(CodedStream {
                stream,
                body: rans::encode_num(numeric, file_model(stream)?)?,
            });
        }
    }
    coded_streams.sort_by_key(|coded| coded.stream);
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
        model_ids.push(coded.stream);
    }
    for &stream in &blobs {
        put_usize(&mut out, stream);
        put_usize(&mut out, walked.io.blobs[stream].len());
    }
    for coded in &coded_streams {
        out.extend_from_slice(&coded.body);
    }
    Some(out)
}

/// The segment as it is, behind a transform header without streams (FORMAT.md section 3.1):
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

/// The first frame boundary at least [`SEGMENT_TARGET_RAW`] bytes in. Past a frame or unframed data
/// longer than [`MAX_SEGMENT_RAW`] the rest is cut at that length.
pub(crate) fn cut_points(src: &[u8]) -> Vec<usize> {
    let max_segment = MAX_SEGMENT_RAW as usize;
    let mut cuts = vec![0];
    let mut segment_start = 0;
    let mut frame_start = 0;
    while frame_start < src.len() {
        let frame_end = frame_len(src, frame_start).map_or(src.len(), |len| frame_start + len);
        let is_frame_past_max = frame_end - segment_start > max_segment;
        if is_frame_past_max {
            while src.len() - segment_start > max_segment {
                segment_start += max_segment;
                cuts.push(segment_start);
            }
            break;
        }
        let is_segment_full = frame_end - segment_start >= SEGMENT_TARGET_RAW;
        if is_segment_full && frame_end < src.len() {
            segment_start = frame_end;
            cuts.push(segment_start);
        }
        frame_start = frame_end;
    }
    cuts.push(src.len());
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
    /// A structural or numeric stream: the length of its body. A blob stream: the number of
    /// its bytes, which come from the tokens.
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
    let mut was_seen = [false; STREAM_COUNT];
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
        if was_seen[id] {
            return bad("duplicate stream");
        }
        was_seen[id] = true;
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
    let mut stream_bodies: Vec<Option<&[u8]>> = vec![None; STREAM_COUNT];
    for entry in &transform_header.entries {
        if !is_blob(entry.id) {
            let body = read_bytes(&mut rest, entry.size).ok_or(Error::Corrupt("stream body"))?;
            stream_bodies[entry.id] = Some(body);
        }
    }
    let mut records = RecordLists::for_segment(dst.len());
    let mut structural: [rans::ByteDecoder; FIRST_NUMERIC_STREAM] =
        std::array::from_fn(|_| rans::ByteDecoder::empty());
    for (stream, decoder) in structural.iter_mut().enumerate() {
        if let Some(body) = stream_bodies[stream] {
            *decoder = rans::ByteDecoder::new(body, rans::file_model(file_models, stream)?)?;
        }
    }
    let mut blob_lens = numeric_decoder(LZ_LENGTH, &stream_bodies, file_models)?;
    let mut blob_dists = numeric_decoder(LZ_DISTANCE, &stream_bodies, file_models)?;
    let blob_bytes = lz::expand(
        &mut structural[LZ_LITERAL],
        &mut blob_lens,
        &mut blob_dists,
        transform_header.blob_total,
    )?;
    // the literals are checked with the structural streams, after the walk
    if !blob_lens.is_done() || !blob_dists.is_done() {
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
        structural,
        blobs,
        dst,
        &mut records,
        transform_header.protocol,
    )?;
    for group in GROUPS {
        run_group(group, &records, dst, &stream_bodies, file_models)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streams::{USERCMD, USERCMD_ANGLE, is_numeric, is_structural};
    use anyhow::Context as _;
    use pretty_assertions::assert_eq;

    fn build_models(segments: &[(&Walked, &Numerics)]) -> Vec<Option<rans::FileModel>> {
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
        let numerics = numerics(src, &walked);
        let models = build_models(&[(&walked, &numerics)]);
        let ids: Vec<usize> = (0..models.len())
            .filter(|&stream| models[stream].is_some())
            .collect();
        let file_models = rans::read_models_block(&models_body(&models, &ids), 1, u64::MAX)?;
        let body =
            encode(&walked, &numerics, &mut Vec::new(), &models).context("a transform body")?;
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
    fn segment_round_trip() -> anyhow::Result<()> {
        let src = demo();
        let (body, file_models) = coded(&src)?;
        let transform_header = parse_transform_header(&body, src.len())?;
        let dst = decoded(&body, &transform_header, src.len(), Some(&file_models))?;
        assert!(dst == src);
        Ok(())
    }
    /// usercmd blocks on and off the grid, other lengths, two in one payload; streams `USERCMD` and
    /// `USERCMD_ANGLE` only where a segment has one.
    #[test]
    fn usercmd_round_trip() -> anyhow::Result<()> {
        // FORMAT.md section 4.3: player, dropnum, msec, grid, three 2-byte moves, buttons, impulse
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
        let usercmd_symbol_count = walked.io.symbols[USERCMD].len();
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
        let numerics = numerics(&src, &walked);
        let mut models = build_models(&[(&walked, &numerics), (&walked, &numerics)]);
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
            let mut fitted = build_models(&[(&walked, &numerics), (&walked, &numerics)]);
            fit_models_block(&mut fitted, max_len, max_raw_len);
            assert_eq!(ids(&fitted), without_largest);
            assert!(
                encode(&walked, &numerics, &mut Vec::new(), &fitted).is_none(),
                "a stream without a model: no transform body"
            );
        }
        // nothing fits: only the models of the blob tokens are kept
        fit_models_block(&mut models, 0, 0);
        assert!(ids(&models).into_iter().all(is_blob_token_model));
        Ok(())
    }
}
