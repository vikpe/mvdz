//! Encoding: walk, code, assemble, and check that the file decodes to its input.

use crate::budget::Budget;
use crate::coder::rans::FileModel;
use crate::decode::{DemoCheck, decode_segment, scan_layout};
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
    compress(bytes, thread_count(max_threads)).map(|(encoded, _)| encoded)
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

#[derive(Debug, Clone, Default)]
pub(crate) struct EncodeStats {
    pub(crate) unstructured_len: usize,
    /// Nonzero only for a transform bug.
    pub(crate) self_check_fallbacks: usize,
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
struct FirstPass {
    file_models: Vec<Option<FileModel>>,
    start_protos: Vec<Proto>,
    is_walkable: Vec<bool>,
    durations_ms: Vec<u64>,
}

impl FirstPass {
    /// Every segment stored as it is.
    fn all_tail(segments: &[&[u8]]) -> Self {
        Self {
            file_models: Vec::new(),
            start_protos: vec![Proto::default(); segments.len()],
            is_walkable: vec![false; segments.len()],
            durations_ms: segments
                .iter()
                .map(|segment| frame_scan(segment).duration_ms)
                .collect(),
        }
    }
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
fn run_first_pass(segments: &[&[u8]], threads: usize) -> FirstPass {
    let model_builder = segment::ModelBuilder::default();
    let mut proto = Proto::default();
    let mut first_pass = FirstPass::all_tail(segments);
    let mut first = 0;
    while first < segments.len() {
        // the first segment alone: it sets the proto of most demos
        let batch_len = if first == 0 {
            1
        } else {
            threads.min(segments.len() - first)
        };
        let batch_proto = proto;
        let mut batch = par_map(threads, &segments[first..first + batch_len], |&raw| {
            count_segment(raw, batch_proto)
        });
        for (index, counted) in (first..).zip(batch.iter_mut()) {
            if counted.start_proto != proto {
                *counted = count_segment(segments[index], proto);
            }
            proto = counted.end_proto;
            first_pass.start_protos[index] = counted.start_proto;
            first_pass.is_walkable[index] = counted.walked.is_some();
        }
        let segments: Vec<(&Walked, &Numerics)> = batch
            .iter()
            .filter_map(|counted| counted.walked.as_ref())
            .map(|(walked, numerics)| (walked, numerics))
            .collect();
        model_builder.add(&segments, threads);
        first += batch_len;
    }
    first_pass.file_models = model_builder.models(threads);
    first_pass
}

/// Threads never change the file written.
pub(crate) fn compress(src: &[u8], threads: usize) -> Result<(Vec<u8>, EncodeStats)> {
    compress_inner(src, threads, false)
}

#[cfg(test)]
pub(crate) fn compress_all_tail(src: &[u8], threads: usize) -> Result<(Vec<u8>, EncodeStats)> {
    compress_inner(src, threads, true)
}

fn compress_inner(src: &[u8], threads: usize, is_all_tail: bool) -> Result<(Vec<u8>, EncodeStats)> {
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
    let first_pass_or_all_tail = || {
        if is_all_tail {
            FirstPass::all_tail(&segments)
        } else {
            run_first_pass(&segments, threads)
        }
    };
    let (first_pass, (crc, sha256)) = if threads > 1 && HAS_THREADS {
        std::thread::scope(|scope| {
            let hashing = scope.spawn(hashes);
            (first_pass_or_all_tail(), hashing.join().expect("hashing"))
        })
    } else {
        (first_pass_or_all_tail(), hashes())
    };
    let header = Header {
        size_bytes: src.len() as u64,
        duration_ms: first_pass.durations_ms.iter().sum(),
        crc32: crc,
        sha256,
    };
    let mut bodies = code_segments(&segments, &first_pass, threads)?;
    let file_models = &first_pass.file_models;
    let self_check_error = || Error::Internal("the encoder's output does not decode to its input");
    let mut fallbacks = 0;
    loop {
        let (out, mut stats) = assemble(&header, file_models, &bodies);
        // the container itself does not decode: nothing to fall back to
        let is_failing = failing_segments(&out, &segments, threads).ok_or_else(self_check_error)?;
        if !is_failing.contains(&true) {
            stats.self_check_fallbacks = fallbacks;
            return Ok((out, stats));
        }
        for index in (0..segments.len()).filter(|&index| is_failing[index]) {
            if bodies[index].is_all_tail {
                return Err(self_check_error());
            }
            bodies[index] = tail_body(segments[index], &first_pass, index)?;
            fallbacks += 1;
        }
    }
}

fn code_segments(segments: &[&[u8]], first_pass: &FirstPass, threads: usize) -> Result<Vec<Body>> {
    let bodies = par_map(threads, segments.iter().enumerate(), |(index, &raw)| {
        let walked = first_pass.is_walkable[index]
            .then(|| segment::walk(raw, first_pass.start_protos[index]))
            .flatten();
        if let Some((walked, _)) = walked
            && let Ok(body) = Body::new(
                raw,
                &walked,
                first_pass.durations_ms[index],
                &first_pass.file_models,
                false,
            )
        {
            return Ok(body);
        }
        tail_body(raw, first_pass, index)
    });
    bodies.into_iter().collect()
}

fn tail_body(raw: &[u8], first_pass: &FirstPass, index: usize) -> Result<Body> {
    let walked = segment::walk_tail(raw, first_pass.start_protos[index]);
    Body::new(raw, &walked, first_pass.durations_ms[index], &[], true)
}

#[derive(Debug)]
struct Body {
    bytes: Vec<u8>,
    is_all_tail: bool,
    unstructured_len: usize,
    model_ids: Vec<usize>,
}

impl Body {
    fn new(
        raw: &[u8],
        walked: &Walked,
        duration_ms: u64,
        file_models: &[Option<FileModel>],
        is_all_tail: bool,
    ) -> Result<Self> {
        let numerics = segment::numerics(raw, walked);
        let mut model_ids = Vec::new();
        let mut bytes =
            segment_header_bytes(raw.len() as u32, duration_ms as u32, crc32fast::hash(raw));
        let coded = segment::encode(walked, &numerics, &mut model_ids, file_models)
            .filter(|coded| coded.len() as u64 <= MAX_SEGMENT_CODED)
            .ok_or(Error::Internal("a segment that cannot be coded"))?;
        bytes.extend_from_slice(&coded);
        Ok(Self {
            bytes,
            is_all_tail,
            unstructured_len: walked.unstructured_len(),
            model_ids,
        })
    }
}

fn assemble(
    header: &Header,
    file_models: &[Option<FileModel>],
    bodies: &[Body],
) -> (Vec<u8>, EncodeStats) {
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
    let mut stats = EncodeStats::default();
    for body in bodies {
        segment_block(&mut out, &body.bytes);
        stats.unstructured_len += body.unstructured_len;
    }
    (out, stats)
}

/// Never holds the decoded demo whole.
pub(crate) fn failing_segments(
    file: &[u8],
    segments: &[&[u8]],
    threads: usize,
) -> Option<Vec<bool>> {
    let budget = Budget::unlimited();
    let layout = scan_layout(file, threads, &budget).ok()?;
    if layout.segments.len() != segments.len() {
        return None;
    }
    let timings = par_map(
        threads,
        layout.segments.iter().zip(segments),
        |((span, header), &want)| {
            let mut dst = vec![0u8; want.len()];
            let body = span.body(file)?;
            decode_segment(&layout, body, header, &mut dst, &budget)
                .ok()
                .filter(|_| dst == want)
        },
    );
    let is_failing: Vec<bool> = timings.iter().map(std::option::Option::is_none).collect();
    if is_failing.iter().any(|&is_failed| is_failed) {
        return Some(is_failing);
    }
    // every segment decoded to the input, so check over it; a failure here is not
    // fixable by falling back
    let mut check = DemoCheck::default();
    for (timing, &want) in timings.into_iter().zip(segments) {
        check.segment(want, timing?).ok()?;
    }
    check.finish(&layout).ok()?;
    Some(is_failing)
}
