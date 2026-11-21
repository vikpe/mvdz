//! Transform-coded segments: the structural walk, the numeric groups and the stream bodies
//! (FORMAT.md section 3).

use crate::budget::Budget;
use crate::coder::rans::ModelsBlock;
use crate::coder::{self, rans};
use crate::error::{Error, Result, bad};
use crate::fields::{put_u32, read_bytes, read_u32, read_usize};
use crate::format::{MAX_MODELS_LEN, MAX_SEGMENT_RAW};
use crate::numeric::{GROUPS, Group, NumericContext, NumericIo};
use crate::parallel::par_map;
use crate::proto::{Proto, frame_len};
use crate::streams::{
    BLOB_BODY, LAZY_STREAM_COUNT, PE_BITS, PI_NUM, RAW, SND_NUM, STREAM_COUNT, TAIL, TE_TYPE, UC,
    UC_MIN_SYMBOLS, is_blob, is_lazy, is_numeric,
};
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
    blobs: Vec<(&'a [u8], usize)>,
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
        let (blob, position) = &mut self.blobs[stream];
        let taken = blob.get(*position..position.checked_add(len)?)?;
        *position += len;
        Some(taken)
    }
    fn blob_rest(&self, stream: usize) -> &[u8] {
        let (blob, position) = &self.blobs[stream];
        &blob[(*position).min(blob.len())..]
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
    blobs: Vec<(&'a [u8], usize)>,
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
    let mut walker: Walker<false, DecodeIo> = Walker::new(&[], dst, records, io, proto);
    walker.run(tail);
    if walker.has_error || walker.io.is_over {
        return bad("structure");
    }
    if !walker
        .io
        .lazy
        .iter()
        .all(super::coder::rans::ByteDecoder::is_done)
        || !walker
            .io
            .blobs
            .iter()
            .all(|(blob, position)| *position == blob.len())
    {
        return bad("unconsumed stream data");
    }
    Ok(())
}

fn run_group(
    group: Group,
    records: &Records,
    dst: &mut [u8],
    entries: &[Option<StreamBody>],
    file_models: Option<&rans::Models>,
) -> Result<()> {
    let mut decoders: [rans::NumericDecoder; MAX_GROUP_STREAMS] =
        std::array::from_fn(|_| rans::NumericDecoder::empty());
    for (stream_index, &stream) in group.streams().iter().enumerate() {
        if let Some(StreamBody { bytes: body, count }) = entries[stream] {
            decoders[stream_index] = rans::NumericDecoder::new(
                stream,
                body,
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
    if io.is_over
        || !io
            .decoders
            .iter()
            .all(super::coder::rans::NumericDecoder::is_done)
    {
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
        let mut walker: Walker<true, EncodeIo> =
            Walker::new(src, &mut [], &mut records, EncodeIo::new(), proto);
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

#[derive(Debug, Clone, Copy)]
enum Kind {
    Lazy,
    Numeric,
    /// All blob streams as one DEFLATE stream.
    Blobs,
}

/// The non-empty ones, in id order.
fn blob_ids(walked: &Walked) -> Vec<usize> {
    (0..STREAM_COUNT)
        .filter(|&stream| is_blob(stream) && !walked.io.blobs[stream].is_empty())
        .collect()
}

/// `None` for a structural or numeric stream without a model in the file.
fn pack_one(
    kind: Kind,
    stream: usize,
    walked: &Walked,
    numeric: Option<&rans::NumericSymbols>,
    file: Option<&rans::FileModel>,
) -> Option<Vec<u8>> {
    match kind {
        Kind::Lazy => Some(rans::encode_bytes(
            &walked.io.symbols[stream],
            &walked.io.contexts[stream],
            file?,
        )),
        Kind::Numeric => Some(rans::encode_num(numeric?, file?)),
        Kind::Blobs => {
            let ids = blob_ids(walked);
            let mut all = Vec::with_capacity(ids.iter().map(|&id| walked.io.blobs[id].len()).sum());
            for id in ids {
                all.extend_from_slice(&walked.io.blobs[id]);
            }
            Some(crate::deflate::compress(&all))
        }
    }
}

/// Per group (in [`GROUPS`] order) and stream of the group.
pub(crate) type Numerics = Vec<Vec<rans::NumericSymbols>>;

/// Run once by the encoder: the models are trained on these values and the segment is coded
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
                .map(super::coder::rans::NumericSymbolWriter::finish)
                .collect()
        })
        .collect()
}

/// One model per structural and numeric stream from its per-context counts over every segment
/// (FORMAT.md section 8 rule 4). Segments are added a few at a time; only counts are kept.
#[derive(Debug)]
pub(crate) struct Trainer {
    tallies: Vec<std::sync::Mutex<Option<rans::Tally>>>,
    /// (group, index in the group)
    numeric_positions: Vec<Option<(usize, usize)>>,
}

impl Default for Trainer {
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

impl Trainer {
    pub(crate) fn add(&self, segments: &[(&Walked, &Numerics)], threads: usize) {
        // streams are independent: in parallel, each over the segments in order
        par_map(threads, STREAM_COUNT, |stream| {
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
        let mut models: Vec<Option<rans::FileModel>> = par_map(threads, STREAM_COUNT, |stream| {
            let tally = self.tallies[stream]
                .lock()
                .expect("no panic while counting");
            tally
                .as_ref()
                .and_then(super::coder::rans::Tally::file_model)
        });
        fit_models_block(&mut models, MAX_MODELS_LEN);
        models
    }
}

/// `ids` ascending, each with a model.
pub(crate) fn models_body(models: &[Option<rans::FileModel>], ids: &[usize]) -> Vec<u8> {
    let mut body = Vec::new();
    put_u32(&mut body, ids.len());
    for &stream in ids {
        let bytes = &models[stream].as_ref().expect("a model").bytes;
        put_u32(&mut body, stream);
        put_u32(&mut body, bytes.len());
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

/// `None` if a structural or numeric stream has no model in `file_models` (by stream id).
/// `model_ids` gets the streams coded with a model.
pub(crate) fn encode(
    walked: &Walked,
    numerics: &Numerics,
    model_ids: &mut Vec<usize>,
    file_models: &[Option<rans::FileModel>],
) -> Option<Vec<u8>> {
    // every non-empty stream is a job: (stream, kind, group, index in group)
    let mut jobs: Vec<(usize, Kind, usize, usize)> = Vec::new();
    for (stream, symbols) in walked.io.symbols.iter().enumerate() {
        if !symbols.is_empty() {
            jobs.push((stream, Kind::Lazy, 0, 0));
        }
    }
    for (group_index, group) in GROUPS.iter().enumerate() {
        for (stream_index, &stream) in group.streams().iter().enumerate() {
            if !numerics[group_index][stream_index].symbols.is_empty() {
                jobs.push((stream, Kind::Numeric, group_index, stream_index));
            }
        }
    }
    // the blob streams: one job, after every stream in id order
    let blobs = blob_ids(walked);
    let blob_total: usize = blobs
        .iter()
        .map(|&stream| walked.io.blobs[stream].len())
        .sum();
    if !blobs.is_empty() {
        jobs.push((BLOB_BODY, Kind::Blobs, 0, 0));
    }
    let mut packed: Vec<Vec<u8>> = Vec::with_capacity(jobs.len());
    for &(stream, kind, group_index, stream_index) in &jobs {
        let numeric = matches!(kind, Kind::Numeric).then(|| &numerics[group_index][stream_index]);
        let file = file_models.get(stream).and_then(|model| model.as_ref());
        packed.push(pack_one(kind, stream, walked, numeric, file)?);
    }
    // table order: stream id; the blob streams follow (their ids are the
    // highest), each with its own entry but one shared body
    let blob_job = jobs.iter().position(|job| matches!(job.1, Kind::Blobs));
    let mut table_jobs: Vec<usize> = (0..jobs.len())
        .filter(|&job| Some(job) != blob_job)
        .collect();
    table_jobs.sort_by_key(|&job| jobs[job].0);
    let count = |job: usize| match jobs[job].1 {
        Kind::Lazy => walked.io.symbols[jobs[job].0].len(),
        Kind::Numeric => numerics[jobs[job].2][jobs[job].3].symbols.len(),
        Kind::Blobs => blob_total,
    };
    let blob_body: &[u8] = match blob_job {
        Some(job) => &packed[job],
        None => &[],
    };
    let mut out = Vec::new();
    for flags in [
        walked.start_proto.fte,
        walked.start_proto.fte2,
        walked.start_proto.mvd1,
    ] {
        out.extend_from_slice(&flags.to_le_bytes());
    }
    put_u32(&mut out, walked.tail);
    put_u32(&mut out, table_jobs.len() + blobs.len());
    for &job in &table_jobs {
        let body = &packed[job];
        put_u32(&mut out, jobs[job].0);
        put_u32(&mut out, count(job));
        put_u32(&mut out, body.len());
        model_ids.push(jobs[job].0);
    }
    for &stream in &blobs {
        put_u32(&mut out, stream);
        put_u32(&mut out, walked.io.blobs[stream].len());
    }
    put_u32(&mut out, blob_body.len());
    for &job in &table_jobs {
        out.extend_from_slice(&packed[job]);
    }
    out.extend_from_slice(blob_body);
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

#[derive(Debug)]
pub(crate) struct TransformHeader {
    proto: Proto,
    tail: usize,
    /// (id, count, body length); body length 0 for a blob stream
    entries: Vec<(usize, usize, usize)>,
    blob_len: usize,
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
    let mut was_seen = [false; STREAM_COUNT];
    let mut total = 0usize;
    let mut blob_total = 0usize;
    for _ in 0..stream_count {
        let id = read_usize(&mut rest).ok_or(table_error)?;
        if id >= STREAM_COUNT || !(is_lazy(id) || is_numeric(id) || is_blob(id)) {
            return bad("stream id");
        }
        let count = read_usize(&mut rest).ok_or(table_error)?;
        let len = if is_blob(id) {
            0
        } else {
            read_usize(&mut rest).ok_or(table_error)?
        };
        if is_blob(id) {
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
        total = total.checked_add(len).ok_or(table_error)?;
        entries.push((id, count, len));
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
        blob_len,
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
    let stream_count = transform_header.entries.len();
    let file_models = models_block.map(super::coder::rans::ModelsBlock::models);
    let mut bodies: Vec<&[u8]> = Vec::with_capacity(stream_count);
    let mut rest = body
        .get(transform_header.body_offset..)
        .ok_or(Error::Corrupt("stream body"))?;
    for &(_, _, len) in &transform_header.entries {
        bodies.push(read_bytes(&mut rest, len).ok_or(Error::Corrupt("stream body"))?);
    }
    let blob_body =
        read_bytes(&mut rest, transform_header.blob_len).ok_or(Error::Corrupt("stream body"))?;
    let count_of = |stream: usize| {
        transform_header
            .entries
            .iter()
            .find(|entry| entry.0 == stream)
            .map_or(0, |entry| entry.1)
    };
    let records_bytes = 12 * count_of(PI_NUM)
        + 16 * count_of(PE_BITS)
        + 20 * count_of(SND_NUM)
        + 20 * count_of(TE_TYPE)
        + 8 * (count_of(UC) / UC_MIN_SYMBOLS);
    let mut scratch = records_bytes as u64;
    for &(id, count, _) in &transform_header.entries {
        if is_blob(id) {
            scratch += count as u64;
        }
    }
    let _charge = budget.alloc(scratch)?;
    let blob_total: usize = (0..stream_count)
        .filter(|&index| is_blob(transform_header.entries[index].0))
        .map(|index| transform_header.entries[index].1)
        .sum();
    let blob_bytes = if blob_total > 0 {
        crate::deflate::decompress(blob_body, blob_total)?
    } else {
        Vec::new()
    };
    let mut blob_bufs: Vec<(&[u8], usize)> = vec![(&[][..], 0); STREAM_COUNT];
    let mut at = 0;
    for &(id, count, _) in transform_header
        .entries
        .iter()
        .filter(|entry| is_blob(entry.0))
    {
        blob_bufs[id].0 = &blob_bytes[at..at + count];
        at += count;
    }
    let mut numeric_entries: Vec<Option<StreamBody>> = vec![None; STREAM_COUNT];
    let mut lazy_entries: Vec<Option<StreamBody>> = vec![None; LAZY_STREAM_COUNT];
    for index in 0..stream_count {
        let (id, count, _) = transform_header.entries[index];
        if is_lazy(id) {
            lazy_entries[id] = Some(StreamBody {
                bytes: bodies[index],
                count,
            });
        } else if is_numeric(id) {
            numeric_entries[id] = Some(StreamBody {
                bytes: bodies[index],
                count,
            });
        }
    }
    let mut records = Records::fixed(
        count_of(PI_NUM),
        count_of(PE_BITS),
        count_of(SND_NUM),
        count_of(TE_TYPE),
        count_of(UC) / UC_MIN_SYMBOLS,
    )?;
    let mut lazy: [rans::ByteDecoder; LAZY_STREAM_COUNT] =
        std::array::from_fn(|_| rans::ByteDecoder::empty());
    for (id, entry) in lazy_entries.iter().enumerate() {
        if let Some(StreamBody {
            bytes: stream_body,
            count,
        }) = *entry
        {
            lazy[id] =
                rans::ByteDecoder::new(stream_body, count, coder::file_model(file_models, id)?)?;
        }
    }
    walk_decode(
        lazy,
        blob_bufs,
        dst,
        &mut records,
        transform_header.proto,
        transform_header.tail,
    )?;
    for group in GROUPS {
        run_group(group, &records, dst, &numeric_entries, file_models)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;
    use crate::streams::UC_A;
    use anyhow::Context as _;
    use pretty_assertions::assert_eq;

    fn train(segments: &[(&Walked, &Numerics)]) -> Vec<Option<rans::FileModel>> {
        let trainer = Trainer::default();
        trainer.add(segments, 1);
        trainer.models(1)
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
        let models = train(&[(&walked, &numerics)]);
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
            .find(|entry| entry.0 == UC)
            .context("a uc stream")?
            .1;
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
                .all(|entry| entry.0 != UC && entry.0 != UC_A)
        );
        Ok(())
    }

    /// Over the limit, the largest headers go first; their segments are not transform-coded.
    #[test]
    fn models_block_limit() -> anyhow::Result<()> {
        let src = demo();
        let (walked, _) = walk(&src, Proto::default()).context("the demo walks")?;
        let numerics = numerics(&src, &walked);
        let mut models = train(&[(&walked, &numerics), (&walked, &numerics)]);
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
