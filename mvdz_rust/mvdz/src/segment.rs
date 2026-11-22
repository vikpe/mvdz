//! Transform-coded segments: the structural walk, the numeric groups and the stream bodies
//! (FORMAT.md section 3).

use crate::budget::Budget;
use crate::coder::rans::ModelsBlock;
use crate::coder::{self, rans};
use crate::error::{Error, Result, bad};
use crate::fields::{put_usize, read_bytes, read_u32, read_usize};
use crate::format::{MAX_MODELS_LEN, MAX_SEGMENT_RAW};
use crate::numeric::{GROUPS, Group, NumericContext, NumericIo};
use crate::parallel::par_map;
use crate::proto::{Proto, frame_len};
use crate::streams::{LAZY_STREAM_COUNT, RAW, STREAM_COUNT, TAIL, is_blob, is_lazy, is_numeric};
use crate::walk::{Records, SymbolIo, Walker};

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
            symbols: Vec::from_fn(LAZY_STREAM_COUNT, |_| Vec::new()),
            contexts: Vec::from_fn(LAZY_STREAM_COUNT, |_| Vec::new()),
            blobs: Vec::from_fn(STREAM_COUNT, |_| Vec::new()),
        }
    }
}

impl SymbolIo for EncodeIo {
    #[inline(always)]
    fn byte(&mut self, stream: usize, context: u32, value: u8) -> u8 {
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
    lazy: [rans::ByteDecoder<'a>; LAZY_STREAM_COUNT],
    /// The unread rest of each blob stream.
    blobs: Vec<&'a [u8]>,
    pub(crate) is_over: bool,
}

impl SymbolIo for DecodeIo<'_> {
    #[inline(always)]
    fn byte(&mut self, stream: usize, context: u32, _: u8) -> u8 {
        debug_assert!(
            context <= 0xffff,
            "stream {stream}: context {context:#x} over 16 bits"
        );
        if let Some(value) = self.lazy[stream].get(context) {
            value
        } else {
            self.is_over = true;
            // an exhausted stream reads as end tokens so every loop stops
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

#[derive(Debug, Clone, Copy)]
struct StreamBody<'a> {
    bytes: &'a [u8],
    count: usize,
}

fn walk_decode<'a>(
    lazy: [rans::ByteDecoder<'a>; LAZY_STREAM_COUNT],
    blobs: Vec<&'a [u8]>,
    dst: &mut [u8],
    records: &mut Records,
    proto: Proto,
    tail: usize,
) -> Result<()> {
    let io = DecodeIo {
        lazy,
        blobs,
        is_over: false,
    };
    let mut walker = Walker::decoder(dst, records, io, proto);
    walker.run(tail);
    if walker.has_error || walker.io.is_over {
        return bad("structure");
    }
    if !walker.io.lazy.iter().all(rans::ByteDecoder::is_done)
        || !walker.io.blobs.iter().all(|blob| blob.is_empty())
    {
        return bad("unconsumed stream data");
    }
    Ok(())
}

fn run_group(
    group: Group,
    records: &Records,
    dst: &mut [u8],
    stream_bodies: &[Option<StreamBody>],
    file_models: Option<&rans::Models>,
) -> Result<()> {
    let mut decoders: [rans::NumericDecoder; MAX_GROUP_STREAMS] =
        std::array::from_fn(|_| rans::NumericDecoder::empty());
    for (stream_index, &stream) in group.streams().iter().enumerate() {
        if let Some(StreamBody { bytes, count }) = stream_bodies[stream] {
            decoders[stream_index] = rans::NumericDecoder::new(
                stream,
                bytes,
                count,
                coder::file_model(file_models, stream)?,
            )?;
        }
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
    io: EncodeIo,
    records: Records,
    tail: usize,
    start_proto: Proto,
}

impl Walked {
    pub(crate) fn unstructured_len(&self) -> usize {
        self.io.blobs[RAW].len() + self.io.blobs[TAIL].len()
    }
}

/// `None`: the transform cannot represent the segment.
pub(crate) fn walk(src: &[u8], proto: Proto) -> Option<(Walked, Proto)> {
    let mut records = Records::growable(src.len());
    let (io, tail, end_proto, has_error) = {
        let mut walker = Walker::encoder(src, &mut records, EncodeIo::new(), proto);
        let tail = walker.run(0);
        (walker.io, tail, walker.proto, walker.has_error)
    };
    if has_error {
        return None;
    }
    Some((
        Walked {
            io,
            records,
            tail,
            start_proto: proto,
        },
        end_proto,
    ))
}

pub(crate) fn walk_tail(src: &[u8], proto: Proto) -> Walked {
    let mut io = EncodeIo::new();
    io.blobs[TAIL] = src.to_vec();
    let records = Records::growable(0);
    Walked {
        io,
        records,
        tail: src.len(),
        start_proto: proto,
    }
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
    tallies: Vec<std::sync::Mutex<Option<rans::Tally>>>,
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
            tallies: Vec::from_fn(STREAM_COUNT, |_| std::sync::Mutex::default()),
            numeric_positions,
        }
    }
}

impl ModelBuilder {
    pub(crate) fn add(&self, segments: &[(&Walked, &Numerics)], threads: usize) {
        // streams are independent: in parallel, each over the segments in order
        par_map(threads, 0..STREAM_COUNT, |stream| {
            let mut tally = self.tallies[stream]
                .lock()
                .expect("no panic while counting");
            let mut add = |alphabet: usize, symbols: &[u8], contexts: &[u16]| {
                if !symbols.is_empty() {
                    tally
                        .get_or_insert_with(|| rans::Tally::new(alphabet))
                        .add(symbols, contexts);
                }
            };
            for &(walked, numerics) in segments {
                if stream < LAZY_STREAM_COUNT {
                    add(256, &walked.io.symbols[stream], &walked.io.contexts[stream]);
                }
                if let Some((group_index, stream_index)) = self.numeric_positions[stream] {
                    add(
                        rans::NUMERIC_ALPHABET,
                        &numerics[group_index][stream_index].symbols,
                        &numerics[group_index][stream_index].contexts,
                    );
                }
            }
        });
    }

    pub(crate) fn models(self, threads: usize) -> Vec<Option<rans::FileModel>> {
        let mut models: Vec<Option<rans::FileModel>> =
            par_map(threads, 0..STREAM_COUNT, |stream| {
                let tally = self.tallies[stream]
                    .lock()
                    .expect("no panic while counting");
                tally.as_ref().and_then(rans::Tally::file_model)
            });
        fit_models_block(&mut models, MAX_MODELS_LEN);
        models
    }
}

/// `ids` ascending, each with a model.
pub(crate) fn models_body(models: &[Option<rans::FileModel>], ids: &[usize]) -> Vec<u8> {
    let mut body = Vec::new();
    put_usize(&mut body, ids.len());
    for &stream in ids {
        let bytes = &models[stream].as_ref().expect("a model").bytes;
        put_usize(&mut body, stream);
        put_usize(&mut body, bytes.len());
        body.extend_from_slice(bytes);
    }
    body
}

/// Drops the largest headers until the models block is at most `max` bytes (FORMAT.md section
/// 7); a segment with a dropped stream is then not transform-coded.
fn fit_models_block(models: &mut [Option<rans::FileModel>], max: u64) {
    loop {
        let ids: Vec<usize> = (0..models.len())
            .filter(|&stream| models[stream].is_some())
            .collect();
        if ids.is_empty() || models_body(models, &ids).len() as u64 <= max {
            return;
        }
        let largest = ids.into_iter().max_by_key(|&stream| {
            let header_len = models[stream].as_ref().map_or(0, |model| model.bytes.len());
            (header_len, stream)
        });
        models[largest.expect("a model")] = None;
    }
}

/// A structural or numeric stream as it is stored.
#[derive(Debug)]
struct CodedStream {
    stream: usize,
    count: usize,
    body: Vec<u8>,
}

/// `None` if a structural or numeric stream has no model in `file_models` (by stream id).
/// `model_ids` gets the streams coded with a model.
pub(crate) fn encode(
    walked: &Walked,
    numerics: &Numerics,
    model_ids: &mut Vec<usize>,
    file_models: &[Option<rans::FileModel>],
) -> Option<Vec<u8>> {
    let file_model = |stream: usize| file_models.get(stream)?.as_ref();
    let mut coded_streams: Vec<CodedStream> = Vec::new();
    for (stream, symbols) in walked.io.symbols.iter().enumerate() {
        if !symbols.is_empty() {
            coded_streams.push(CodedStream {
                stream,
                count: symbols.len(),
                body: rans::encode_bytes(symbols, &walked.io.contexts[stream], file_model(stream)?),
            });
        }
    }
    for (group, group_numerics) in GROUPS.iter().zip(numerics) {
        for (&stream, numeric) in group.streams().iter().zip(group_numerics) {
            if !numeric.symbols.is_empty() {
                coded_streams.push(CodedStream {
                    stream,
                    count: numeric.symbols.len(),
                    body: rans::encode_num(numeric, file_model(stream)?),
                });
            }
        }
    }
    coded_streams.sort_by_key(|coded| coded.stream);
    // the blob streams share one body and follow in the table: their ids are the highest
    let blobs = blob_ids(walked);
    let blob_body = if blobs.is_empty() {
        Vec::new()
    } else {
        let blob_streams: Vec<&[u8]> = blobs
            .iter()
            .map(|&stream| walked.io.blobs[stream].as_slice())
            .collect();
        crate::deflate::compress(&blob_streams.concat())
    };
    let mut out = Vec::new();
    for flags in [
        walked.start_proto.fte,
        walked.start_proto.fte2,
        walked.start_proto.mvd1,
    ] {
        out.extend_from_slice(&flags.to_le_bytes());
    }
    put_usize(&mut out, walked.tail);
    put_usize(&mut out, coded_streams.len() + blobs.len());
    for coded in &coded_streams {
        put_usize(&mut out, coded.stream);
        put_usize(&mut out, coded.count);
        put_usize(&mut out, coded.body.len());
        model_ids.push(coded.stream);
    }
    for &stream in &blobs {
        put_usize(&mut out, stream);
        put_usize(&mut out, walked.io.blobs[stream].len());
    }
    put_usize(&mut out, blob_body.len());
    for coded in &coded_streams {
        out.extend_from_slice(&coded.body);
    }
    out.extend_from_slice(&blob_body);
    Some(out)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FrameScan {
    pub(crate) duration_ms: u64,
    pub(crate) framed_len: usize,
}

pub(crate) fn frame_scan(demo: &[u8]) -> FrameScan {
    let mut scan = FrameScan::default();
    while let Some(len) = frame_len(demo, scan.framed_len) {
        scan.duration_ms += u64::from(demo[scan.framed_len]);
        scan.framed_len += len;
    }
    scan
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamEntry {
    id: usize,
    count: usize,
    /// 0 for a blob stream: its bytes are in the blob body.
    len: usize,
}

#[derive(Debug)]
pub(crate) struct TransformHeader {
    proto: Proto,
    tail: usize,
    entries: Vec<StreamEntry>,
    /// By stream id; 0 for a stream not in the table.
    counts: [usize; STREAM_COUNT],
    /// The blob body, as stored and decompressed.
    blob_len: usize,
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
    let tail = read_usize(&mut rest).ok_or(header_error)?;
    let stream_count = read_usize(&mut rest).ok_or(header_error)?;
    if tail > raw_len {
        return bad("tail length");
    }
    if stream_count > STREAM_COUNT {
        return bad("stream count");
    }
    let table_error = Error::Corrupt("stream table");
    let mut entries = Vec::with_capacity(stream_count);
    let mut counts = [0usize; STREAM_COUNT];
    let mut was_seen = [false; STREAM_COUNT];
    let mut total = 0usize;
    let mut blob_total = 0usize;
    for _ in 0..stream_count {
        let id = read_usize(&mut rest).ok_or(table_error)?;
        let is_blob = is_blob(id);
        if id >= STREAM_COUNT || !(is_lazy(id) || is_numeric(id) || is_blob) {
            return bad("stream id");
        }
        let count = read_usize(&mut rest).ok_or(table_error)?;
        let len = if is_blob {
            0
        } else {
            read_usize(&mut rest).ok_or(table_error)?
        };
        if is_blob {
            blob_total = blob_total.checked_add(count).ok_or(table_error)?;
        }
        if was_seen[id] {
            return bad("duplicate stream");
        }
        was_seen[id] = true;
        // no stream holds more symbols than the output has bytes (plus slack)
        if count > raw_len.saturating_add(16) {
            return bad("stream count");
        }
        counts[id] = count;
        total = total.checked_add(len).ok_or(table_error)?;
        entries.push(StreamEntry { id, count, len });
    }
    let blob_len = read_usize(&mut rest).ok_or(table_error)?;
    if (blob_total == 0) != (blob_len == 0) {
        return bad("blob body");
    }
    total = total.checked_add(blob_len).ok_or(table_error)?;
    if total != rest.len() {
        return bad("stream bodies");
    }
    Ok(TransformHeader {
        proto: Proto::new(fte, fte2, mvd1),
        tail,
        entries,
        counts,
        blob_len,
        blob_total,
        body_offset: body.len() - rest.len(),
    })
}

/// `dst` is the exact raw length; the caller checks the CRC.
pub(crate) fn decode(
    body: &[u8],
    transform_header: &TransformHeader,
    dst: &mut [u8],
    models_block: Option<&ModelsBlock>,
    budget: &Budget,
) -> Result<()> {
    let file_models = models_block.map(ModelsBlock::models);
    let mut rest = body
        .get(transform_header.body_offset..)
        .ok_or(Error::Corrupt("stream body"))?;
    let mut stream_bodies: Vec<Option<StreamBody>> = vec![None; STREAM_COUNT];
    for entry in &transform_header.entries {
        if !is_blob(entry.id) {
            let bytes = read_bytes(&mut rest, entry.len).ok_or(Error::Corrupt("stream body"))?;
            stream_bodies[entry.id] = Some(StreamBody {
                bytes,
                count: entry.count,
            });
        }
    }
    let blob_body =
        read_bytes(&mut rest, transform_header.blob_len).ok_or(Error::Corrupt("stream body"))?;
    let mut charge = budget.alloc(transform_header.blob_total as u64)?;
    let mut records = Records::fixed(&transform_header.counts, &mut charge)?;
    let blob_bytes = if transform_header.blob_total > 0 {
        crate::deflate::decompress(blob_body, transform_header.blob_total)?
    } else {
        Vec::new()
    };
    let mut blobs: Vec<&[u8]> = vec![&[]; STREAM_COUNT];
    let mut at = 0;
    for entry in &transform_header.entries {
        if is_blob(entry.id) {
            blobs[entry.id] = &blob_bytes[at..at + entry.count];
            at += entry.count;
        }
    }
    let mut lazy: [rans::ByteDecoder; LAZY_STREAM_COUNT] =
        std::array::from_fn(|_| rans::ByteDecoder::empty());
    for (stream, decoder) in lazy.iter_mut().enumerate() {
        if let Some(StreamBody { bytes, count }) = stream_bodies[stream] {
            *decoder =
                rans::ByteDecoder::new(bytes, count, coder::file_model(file_models, stream)?)?;
        }
    }
    walk_decode(
        lazy,
        blobs,
        dst,
        &mut records,
        transform_header.proto,
        transform_header.tail,
    )?;
    for group in GROUPS {
        run_group(group, &records, dst, &stream_bodies, file_models)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;
    use crate::streams::{UC, UC_A, UC_MIN_SYMBOLS};
    use anyhow::Context as _;
    use pretty_assertions::assert_eq;

    fn build_models(segments: &[(&Walked, &Numerics)]) -> Vec<Option<rans::FileModel>> {
        let model_builder = ModelBuilder::default();
        model_builder.add(segments, 1);
        model_builder.models(1)
    }

    fn decoded(
        body: &[u8],
        transform_header: &TransformHeader,
        len: usize,
        models_block: Option<&ModelsBlock>,
    ) -> anyhow::Result<Vec<u8>> {
        let mut dst = vec![0u8; len];
        decode(
            body,
            transform_header,
            &mut dst,
            models_block,
            &Budget::unlimited(),
        )?;
        Ok(dst)
    }

    fn coded(src: &[u8]) -> anyhow::Result<(Vec<u8>, ModelsBlock)> {
        let (walked, _) = walk(src, Proto::default()).context("the demo walks")?;
        let numerics = numerics(src, &walked);
        let models = build_models(&[(&walked, &numerics)]);
        let ids: Vec<usize> = (0..models.len())
            .filter(|&stream| models[stream].is_some())
            .collect();
        let models_block = ModelsBlock::new(&models_body(&models, &ids), 1, &Budget::unlimited())?;
        let body =
            encode(&walked, &numerics, &mut Vec::new(), &models).context("a transform body")?;
        Ok((body, models_block))
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
        let (body, models_block) = coded(&src)?;
        let transform_header = parse_transform_header(&body, src.len())?;
        let dst = decoded(&body, &transform_header, src.len(), Some(&models_block))?;
        assert!(dst == src);
        Ok(())
    }
    /// usercmd blocks on and off the grid, other lengths, two in one payload; streams `UC` and
    /// `UC_A` only where a segment has one.
    #[test]
    fn usercmd_round_trip() -> anyhow::Result<()> {
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
        let (body, models_block) = coded(&src)?;
        let transform_header = parse_transform_header(&body, src.len())?;
        let uc_count = transform_header
            .entries
            .iter()
            .find(|entry| entry.id == UC)
            .context("a uc stream")?
            .count;
        assert!(uc_count >= (300 + 9) * UC_MIN_SYMBOLS, "{uc_count}");
        let dst = decoded(&body, &transform_header, src.len(), Some(&models_block))?;
        assert!(dst == src);
        // without usercmd blocks neither stream is in the table
        let plain = demo();
        let (body, _) = coded(&plain)?;
        let transform_header = parse_transform_header(&body, plain.len())?;
        assert!(
            transform_header
                .entries
                .iter()
                .all(|entry| entry.id != UC && entry.id != UC_A)
        );
        Ok(())
    }

    /// Over the limit, the largest headers go first; their segments are not transform-coded.
    #[test]
    fn models_block_limit() -> anyhow::Result<()> {
        let src = demo();
        let (walked, _) = walk(&src, Proto::default()).context("the demo walks")?;
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
                && all.iter().any(|&stream| is_lazy(stream))
        );
        let len = models_body(&models, &all).len() as u64;
        let header_len =
            |stream: usize| models[stream].as_ref().map_or(0, |model| model.bytes.len());
        let largest = *all
            .iter()
            .max_by_key(|&&stream| (header_len(stream), stream))
            .context("a model")?;
        fit_models_block(&mut models, len);
        assert_eq!(ids(&models), all);
        fit_models_block(&mut models, len - 1);
        let kept = ids(&models);
        assert_eq!(
            kept,
            all.iter()
                .copied()
                .filter(|&stream| stream != largest)
                .collect::<Vec<_>>()
        );
        assert!(
            encode(&walked, &numerics, &mut Vec::new(), &models).is_none(),
            "a stream without a model: no transform body"
        );
        // nothing fits: no model is kept
        fit_models_block(&mut models, 0);
        assert!(ids(&models).is_empty());
        Ok(())
    }
}
