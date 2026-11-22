//! Encoding: walk, code, assemble, and check that the file decodes to its input.

use crate::coder::rans::FileModel;
use crate::decode::Decoder;
use crate::error::{Error, Result};
use crate::format::{
    Header, MAX_SEGMENT_CODED, MAX_SEGMENTS, header_bytes, models_block, segment_block,
    segment_header_bytes,
};
use crate::parallel::{HAS_THREADS, par_map, thread_count};
use crate::proto::{DEM_ALL, DEM_MULTIPLE, DEM_READ, DEM_SINGLE, DEM_STATS, Proto, frame_len};
use crate::segment::{self, Numerics, Walked, cut_points, frame_scan};
use sha2::{Digest, Sha256};
use std::io::Write;

/// `max_threads` 0 is all logical cores.
///
/// # Errors
///
/// [`Error::NotMvd`] for bytes that are not an MVD demo, [`Error::DemoTooLarge`] past 65536
/// segments, [`Error::Internal`] when the output fails its self-check (a bug).
pub fn encode(bytes: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    check_demo(bytes)?;
    compress(bytes, thread_count(max_threads)).map(|(file, _)| file)
}

/// Nothing is written before `finish`; dropping it without `finish` writes nothing.
#[derive(Debug)]
pub struct Encoder<W: Write> {
    writer: W,
    max_threads: u8,
    bytes: Vec<u8>,
}

impl<W: Write> Encoder<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            max_threads: 0,
            bytes: Vec::new(),
        }
    }

    /// Capped at the logical cores; 0, the default, is all of them.
    #[must_use]
    pub fn max_threads(mut self, max_threads: u8) -> Self {
        self.max_threads = max_threads;
        self
    }

    /// # Errors
    ///
    /// Those of [`encode`], and [`Error::Io`] when writing fails.
    pub fn finish(mut self) -> Result<W> {
        self.writer
            .write_all(&encode(&self.bytes, self.max_threads)?)?;
        self.writer.flush()?;
        Ok(self.writer)
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// FORMAT.md section 8 rule 1: a damaged demo with an intact start passes.
pub(crate) fn check_demo(demo: &[u8]) -> Result<()> {
    const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
    if demo.is_empty() {
        return Err(Error::NotMvd("empty input"));
    }
    if demo.starts_with(&GZIP_MAGIC) {
        return Err(Error::NotMvd("gzip-compressed data (decompress it first)"));
    }
    if !starts_like_mvd(demo) {
        return Err(Error::NotMvd("the first frame does not parse"));
    }
    Ok(())
}

pub(crate) fn starts_like_mvd(bytes: &[u8]) -> bool {
    let is_bad_frame = |bytes: &[u8], offset: usize| {
        bytes.len() >= offset + 2 && !(DEM_READ..=DEM_ALL).contains(&(bytes[offset + 1] & 7))
    };
    if bytes.len() < 2 {
        return false;
    }
    let head = match bytes[1] & 7 {
        DEM_MULTIPLE => 10,
        DEM_READ | DEM_SINGLE | DEM_STATS | DEM_ALL => 6,
        _ => return false,
    };
    let Some(first_len) = frame_len(bytes, 0) else {
        return false;
    };
    if first_len <= head {
        return false;
    }
    if segment::walk(&bytes[..first_len], Proto::default())
        .is_some_and(|(walked, _)| walked.unstructured_len() == 0)
    {
        return true;
    }
    let mut offset = first_len;
    for _ in 0..16 {
        if is_bad_frame(bytes, offset) {
            return false;
        }
        match frame_len(bytes, offset) {
            Some(len) => offset += len,
            None => return false,
        }
    }
    true
}

#[derive(Debug)]
struct FirstPass<'a> {
    file_models: Vec<Option<FileModel>>,
    segments: Vec<SegmentPlan<'a>>,
}

/// What pass 2 needs to code one segment.
#[derive(Debug)]
struct SegmentPlan<'a> {
    raw: &'a [u8],
    start_proto: Proto,
    is_walkable: bool,
    duration_ms: u64,
}

/// A pass 1 walk of one segment.
struct Counted {
    start_proto: Proto,
    end_proto: Proto,
    walked: Option<(Walked, Numerics)>,
}

fn count_segment(raw: &[u8], start_proto: Proto) -> Counted {
    match segment::walk(raw, start_proto) {
        Some((walked, end_proto)) => {
            let numerics = segment::numerics(raw, &walked);
            Counted {
                start_proto,
                end_proto,
                walked: Some((walked, numerics)),
            }
        }
        None => Counted {
            start_proto,
            end_proto: start_proto,
            walked: None,
        },
    }
}

/// Pass 1 (FORMAT.md section 8) keeps only counts; pass 2 walks again. A segment starts in
/// the proto the one before ends in, so the segments of a batch are walked in parallel from
/// the end proto of the batch before, and the few that start in another proto walked again.
fn run_first_pass<'a>(segments: &[&'a [u8]], threads: usize) -> FirstPass<'a> {
    let mut model_builder = segment::ModelBuilder::default();
    let mut proto = Proto::default();
    let mut plans = Vec::with_capacity(segments.len());
    let mut first = 0;
    while first < segments.len() {
        // the first segment alone: it sets the proto of most demos
        let batch_len = if first == 0 {
            1
        } else {
            threads.min(segments.len() - first)
        };
        let batch_proto = proto;
        let batch_segments = &segments[first..first + batch_len];
        let mut batch = par_map(threads, batch_segments, |&raw| {
            count_segment(raw, batch_proto)
        });
        for (&raw, counted) in batch_segments.iter().zip(batch.iter_mut()) {
            if counted.start_proto != proto {
                *counted = count_segment(raw, proto);
            }
            proto = counted.end_proto;
            plans.push(SegmentPlan {
                raw,
                start_proto: counted.start_proto,
                is_walkable: counted.walked.is_some(),
                duration_ms: frame_scan(raw).duration_ms,
            });
        }
        let walked_segments: Vec<(&Walked, &Numerics)> = batch
            .iter()
            .filter_map(|counted| counted.walked.as_ref())
            .map(|(walked, numerics)| (walked, numerics))
            .collect();
        model_builder.add(&walked_segments, threads);
        first += batch_len;
    }
    FirstPass {
        file_models: model_builder.models(threads),
        segments: plans,
    }
}

/// The file, and the count of segments that failed the self-check: nonzero only for a
/// transform bug. Threads never change the file written.
pub(crate) fn compress(src: &[u8], threads: usize) -> Result<(Vec<u8>, usize)> {
    // an empty demo has no segment
    let cuts = if src.is_empty() {
        vec![0]
    } else {
        cut_points(src)
    };
    let segments: Vec<&[u8]> = cuts
        .iter()
        .zip(&cuts[1..])
        .map(|(&start, &end)| &src[start..end])
        .collect();
    if segments.len() > MAX_SEGMENTS {
        return Err(Error::DemoTooLarge);
    }
    let hashes = || (crc32fast::hash(src), <[u8; 32]>::from(Sha256::digest(src)));
    let (first_pass, (crc, sha256)) = if threads > 1 && HAS_THREADS {
        std::thread::scope(|scope| {
            let hashing = scope.spawn(hashes);
            let first_pass = run_first_pass(&segments, threads);
            (first_pass, hashing.join().expect("hashing"))
        })
    } else {
        (run_first_pass(&segments, threads), hashes())
    };
    let header = Header {
        size_bytes: src.len() as u64,
        duration_ms: first_pass
            .segments
            .iter()
            .map(|plan| plan.duration_ms)
            .sum(),
        crc32: crc,
        sha256,
    };
    let mut bodies = code_segments(&first_pass, threads)?;
    let mut fallbacks = 0;
    loop {
        let out = assemble(&header, &first_pass.file_models, &bodies);
        let Some(failing) = first_failing_segment(&out, &segments, threads)? else {
            return Ok((out, fallbacks));
        };
        if bodies[failing].is_all_tail {
            return Err(SELF_CHECK_FAILED);
        }
        bodies[failing] = tail_body(&first_pass.segments[failing])?;
        fallbacks += 1;
    }
}

const SELF_CHECK_FAILED: Error =
    Error::Internal("the encoder's output does not decode to its input");

fn code_segments(first_pass: &FirstPass, threads: usize) -> Result<Vec<Body>> {
    let bodies = par_map(threads, &first_pass.segments, |plan| {
        let walked = plan
            .is_walkable
            .then(|| segment::walk(plan.raw, plan.start_proto))
            .flatten();
        if let Some((walked, _)) = walked
            && let Ok(body) = Body::new(plan, &walked, &first_pass.file_models, false)
        {
            return Ok(body);
        }
        tail_body(plan)
    });
    bodies.into_iter().collect()
}

fn tail_body(plan: &SegmentPlan) -> Result<Body> {
    let walked = segment::walk_tail(plan.raw, plan.start_proto);
    Body::new(plan, &walked, &[], true)
}

#[derive(Debug)]
struct Body {
    bytes: Vec<u8>,
    is_all_tail: bool,
    model_ids: Vec<usize>,
}

impl Body {
    fn new(
        plan: &SegmentPlan,
        walked: &Walked,
        file_models: &[Option<FileModel>],
        is_all_tail: bool,
    ) -> Result<Self> {
        let raw = plan.raw;
        let numerics = segment::numerics(raw, walked);
        let mut model_ids = Vec::new();
        let mut bytes = segment_header_bytes(
            raw.len() as u32,
            plan.duration_ms as u32,
            crc32fast::hash(raw),
        );
        let coded = segment::encode(walked, &numerics, &mut model_ids, file_models)
            .filter(|coded| coded.len() as u64 <= MAX_SEGMENT_CODED)
            .ok_or(Error::Internal("a segment that cannot be coded"))?;
        bytes.extend_from_slice(&coded);
        Ok(Self {
            bytes,
            is_all_tail,
            model_ids,
        })
    }
}

fn assemble(header: &Header, file_models: &[Option<FileModel>], bodies: &[Body]) -> Vec<u8> {
    let mut out = header_bytes(header).to_vec();
    // only the models some segment uses
    let mut is_used = vec![false; file_models.len()];
    for &id in bodies.iter().flat_map(|body| &body.model_ids) {
        is_used[id] = true;
    }
    let ids: Vec<usize> = (0..is_used.len()).filter(|&id| is_used[id]).collect();
    if ids.is_empty() {
        models_block(&mut out, &[]);
    } else {
        models_block(&mut out, &segment::models_body(file_models, &ids));
    }
    for body in bodies {
        segment_block(&mut out, &body.bytes);
    }
    out
}

/// The first segment `file` does not decode to, `None` when it decodes to them all.
/// [`SELF_CHECK_FAILED`] when the file fails outside a segment: nothing to fall back to.
///
/// ponytail: one decode of the file per failing segment, so a transform bug in every
/// segment makes encoding quadratic; decoding on past a failed segment would find them all.
fn first_failing_segment(file: &[u8], segments: &[&[u8]], threads: usize) -> Result<Option<usize>> {
    const NO_MEMORY_LIMIT: u64 = u64::MAX;
    let mut decoder =
        Decoder::with_max_memory(file, threads, NO_MEMORY_LIMIT).map_err(|_| SELF_CHECK_FAILED)?;
    for (index, &raw) in segments.iter().enumerate() {
        let is_decoded = decoder
            .next_segment()
            .is_ok_and(|decoded| decoded.is_some_and(|decoded| decoded.bytes == raw));
        if !is_decoded {
            return Ok(Some(index));
        }
    }
    // the whole-demo checks, and nothing after the last segment
    match decoder.next_segment() {
        Ok(None) => Ok(None),
        _ => Err(SELF_CHECK_FAILED),
    }
}
