//! Compression: walk, code, assemble, and check that the file decodes to
//! the input (falling back per segment to all tail).

use crate::budget::Budget;
use crate::coder::rans::Shared;
use crate::decode::{DemoCheck, SliceSrc, decode_segment, scan_layout};
use crate::error::{Error, Result};
use crate::format::{
    Header, MAX_SEGMENT_CODED, MAX_SEGMENTS, SEGMENT_FIELDS_LEN, models_block, segment_block,
    segment_header_bytes,
};
use crate::proto::{Proto, frame_len};
use crate::segment::{self, Numerics, Walked, cut_points, frame_scan};
use crate::util::{HAS_THREADS, crc32, par_map};
use sha2::{Digest, Sha256};

/// How the encoder runs. Nothing here changes the file it writes.
#[derive(Clone, Debug)]
pub struct Config {
    /// Worker threads (0: one per logical core).
    pub threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { threads: 1 }
    }
}

/// What the encoder did.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub segments: usize,
    /// Input bytes the transform stored raw (escapes and unframeable tail).
    pub escaped: usize,
    /// Segments that failed the self-check and were coded as all tail instead:
    /// nonzero only for a transform bug.
    pub self_check_fallbacks: usize,
}

/// True if `bytes` starts like an MVD demo.
///
/// That is a whole first frame that carries
/// messages (a non-empty `dem_read`/`dem_all`/... payload, or hidden or
/// multi-target data; not a bare `dem_set`), and either its messages parse
/// without an escape or, if that first frame is damaged, the next 16
/// frames are whole too. A frame's length field must fit the input, so
/// other files almost never frame: text, gzip, PNG, TIFF and random bytes
/// fail on the first frame.
pub(crate) fn starts_like_mvd(bytes: &[u8]) -> bool {
    let is_bad_frame = |bytes: &[u8], offset: usize| {
        bytes.len() >= offset + 2 && matches!(bytes[offset + 1] & 7, 0 | 7)
    };
    if bytes.len() < 2 {
        return false;
    }
    let head = match bytes[1] & 7 {
        0 | 2 | 7 => return false,
        3 => 10,
        _ => 6,
    };
    let Some(first_len) = frame_len(bytes, 0) else {
        return false;
    };
    if first_len <= head {
        return false;
    }
    if segment::walk(&bytes[..first_len], Proto::default())
        .is_some_and(|(walked, _)| walked.escaped() == 0)
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

/// What the first pass over the demo leaves: the file's models, and per
/// segment what the second pass needs to code it on its own.
struct Trained {
    shared: Vec<Option<Shared>>,
    /// the protocol state each segment starts in
    protos: Vec<Proto>,
    /// whether the transform can represent the segment
    is_walkable: Vec<bool>,
    /// each segment's frame time: the sum of its frame time bytes
    durations: Vec<u64>,
}

/// The first pass: walks every segment in order (the protocol state
/// carries over), runs its numeric predictors and counts its symbols for
/// the models (FORMAT.md §12). `threads` segments are held at a time, their
/// predictors run in parallel; nothing but the counts is kept.
fn train(src: &[u8], cuts: &[usize], threads: usize) -> Trained {
    let segment_count = cuts.len() - 1;
    let segment_bytes = |index: usize| &src[cuts[index]..cuts[index + 1]];
    let trainer = segment::Trainer::default();
    let mut proto = Proto::default();
    let mut trained = Trained {
        shared: Vec::new(),
        protos: Vec::with_capacity(segment_count),
        is_walkable: Vec::with_capacity(segment_count),
        durations: (0..segment_count)
            .map(|index| frame_scan(segment_bytes(index)).1)
            .collect(),
    };
    for first in (0..segment_count).step_by(threads) {
        let batch_len = threads.min(segment_count - first);
        let walks: Vec<Option<Walked>> = (first..first + batch_len)
            .map(|index| {
                trained.protos.push(proto);
                let (walked, end_proto) = segment::walk(segment_bytes(index), proto)?;
                proto = end_proto;
                Some(walked)
            })
            .collect();
        let numerics = par_map(threads, batch_len, |batch_index| {
            Some(segment::numerics(
                segment_bytes(first + batch_index),
                walks[batch_index].as_ref()?,
            ))
        });
        let segments: Vec<(&Walked, &Numerics)> = walks
            .iter()
            .zip(&numerics)
            .filter_map(|(walked, numerics)| Some((walked.as_ref()?, numerics.as_ref()?)))
            .collect();
        trainer.add(&segments, threads);
        trained
            .is_walkable
            .extend(walks.iter().map(std::option::Option::is_some));
    }
    trained.shared = trainer.models(threads);
    trained
}

/// The body of a walked segment with numeric values `numerics`, or None if
/// one of its streams has no model in `shared` (the file's models) or it is
/// longer than a coded segment may be.
pub(crate) fn transform_body(
    raw: &[u8],
    walked: &Walked,
    numerics: &Numerics,
    duration_ms: u64,
    model_ids: &mut Vec<usize>,
    shared: &[Option<Shared>],
) -> Option<Vec<u8>> {
    let mut body = segment_header_bytes(raw.len() as u32, duration_ms as u32, crc32(raw));
    body.extend_from_slice(&segment::encode(walked, numerics, model_ids, shared)?);
    ((body.len() - SEGMENT_FIELDS_LEN) as u64 <= MAX_SEGMENT_CODED).then_some(body)
}

/// Compresses a demo into segments of [`segment::SEGMENT`] raw bytes.
pub(crate) fn compress(src: &[u8], config: &Config) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, config, false)
}

/// Compresses a demo with every segment coded as all tail: the layout the
/// encoder falls back to. For tests and test vectors.
#[cfg(test)]
pub(crate) fn compress_all_tail(src: &[u8], config: &Config) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, config, true)
}

fn compress_inner(src: &[u8], config: &Config, is_all_tail: bool) -> Result<(Vec<u8>, Info)> {
    let threads = crate::util::threads(config.threads);
    // an empty demo has no segment
    let cuts = if src.is_empty() {
        vec![0]
    } else {
        cut_points(src)
    };
    let segment_count = cuts.len() - 1;
    if segment_count > MAX_SEGMENTS {
        return Err(Error::Invalid("too many segments"));
    }
    // the first pass: the models. Its walk runs on one thread: with more,
    // the demo's checksum and SHA-256 are computed beside it
    let hashes = || (crc32(src), <[u8; 32]>::from(Sha256::digest(src)));
    let train = || {
        if is_all_tail {
            Trained {
                shared: Vec::new(),
                protos: vec![Proto::default(); segment_count],
                is_walkable: vec![false; segment_count],
                durations: (0..segment_count)
                    .map(|index| frame_scan(&src[cuts[index]..cuts[index + 1]]).1)
                    .collect(),
            }
        } else {
            train(src, &cuts, threads)
        }
    };
    let (trained, (crc, sha256)) = if threads > 1 && HAS_THREADS {
        std::thread::scope(|scope| {
            let hashing = scope.spawn(hashes);
            (train(), hashing.join().expect("hashing"))
        })
    } else {
        (train(), hashes())
    };
    let header = Header {
        size: src.len() as u64,
        duration_ms: trained.durations.iter().sum(),
        crc,
        sha256,
    };
    let mut bodies = code_segments(src, &cuts, &trained, threads)?;
    let shared = &trained.shared;
    let self_check_error = || Error::Internal("the encoder's output does not decode to its input");
    let mut fallbacks = 0;
    loop {
        let (out, mut info) = assemble(&header, shared, &bodies);
        // None: the container itself does not decode, nothing to fall back to
        let is_failing =
            failing_segments(&out, src, &cuts, threads).ok_or_else(self_check_error)?;
        if !is_failing.contains(&true) {
            info.self_check_fallbacks = fallbacks;
            return Ok((out, info));
        }
        for index in (0..segment_count).filter(|&index| is_failing[index]) {
            if bodies[index].is_all_tail {
                return Err(self_check_error());
            }
            bodies[index] = tail_body(src, &cuts, &trained, index)?;
            fallbacks += 1;
        }
    }
}

/// The second pass: every segment walked again from the protocol state it starts in
/// and coded with the models, segments in parallel; one the transform cannot code is
/// all tail.
fn code_segments(
    src: &[u8],
    cuts: &[usize],
    trained: &Trained,
    threads: usize,
) -> Result<Vec<Body>> {
    let bodies = par_map(threads, cuts.len() - 1, |index| {
        let raw = &src[cuts[index]..cuts[index + 1]];
        let walked = trained.is_walkable[index]
            .then(|| segment::walk(raw, trained.protos[index]))
            .flatten();
        if let Some((walked, _)) = walked
            && let Ok(body) = Body::new(
                raw,
                &walked,
                trained.durations[index],
                &trained.shared,
                false,
            )
        {
            return Ok(body);
        }
        tail_body(src, cuts, trained, index)
    });
    bodies.into_iter().collect()
}

/// Segment `index` coded as all tail: the layout the encoder falls back to.
fn tail_body(src: &[u8], cuts: &[usize], trained: &Trained, index: usize) -> Result<Body> {
    let raw = &src[cuts[index]..cuts[index + 1]];
    let walked = segment::walk_tail(raw, trained.protos[index]);
    Body::new(raw, &walked, trained.durations[index], &[], true)
}

/// One coded segment.
struct Body {
    bytes: Vec<u8>,
    /// coded as all tail
    is_all_tail: bool,
    /// input bytes the transform stored raw
    escaped: usize,
    /// the streams coded with a model
    model_ids: Vec<usize>,
}

impl Body {
    /// Codes segment `raw` from its walk `walked`. An error if a stream has
    /// no model in `shared` or the body is longer than a coded segment may be
    /// (an all-tail body has neither problem).
    fn new(
        raw: &[u8],
        walked: &Walked,
        duration_ms: u64,
        shared: &[Option<Shared>],
        is_all_tail: bool,
    ) -> Result<Self> {
        let numerics = segment::numerics(raw, walked);
        let mut model_ids = Vec::new();
        let bytes = transform_body(raw, walked, &numerics, duration_ms, &mut model_ids, shared)
            .ok_or(Error::Internal("a segment that cannot be coded"))?;
        Ok(Self {
            bytes,
            is_all_tail,
            escaped: walked.escaped(),
            model_ids,
        })
    }
}

/// Builds the file from its segments' bodies.
fn assemble(header: &Header, shared: &[Option<Shared>], bodies: &[Body]) -> (Vec<u8>, Info) {
    let mut out = header.bytes().to_vec();
    // the models: the shared model of every stream some segment codes with it
    let mut is_used = vec![false; shared.len()];
    for &id in bodies.iter().flat_map(|body| &body.model_ids) {
        is_used[id] = true;
    }
    let ids: Vec<usize> = (0..is_used.len()).filter(|&id| is_used[id]).collect();
    if ids.is_empty() {
        models_block(&mut out, &[]);
    } else {
        models_block(&mut out, &segment::tables_body(shared, &ids));
    }
    let mut info = Info {
        segments: bodies.len(),
        ..Default::default()
    };
    for body in bodies {
        segment_block(&mut out, &body.bytes);
        info.escaped += body.escaped;
    }
    (out, info)
}

/// Decodes `file` and compares it with `src`, segment by segment.
///
/// The decoded demo is never held whole. None if the container does not
/// parse, else per segment whether it fails. Every segment passing means
/// the whole file decodes (the whole-file checks are run too).
pub(crate) fn failing_segments(
    file: &[u8],
    src: &[u8],
    cuts: &[usize],
    threads: usize,
) -> Option<Vec<bool>> {
    let budget = Budget::unlimited();
    let layout = scan_layout(&mut SliceSrc(file), threads, &budget).ok()?;
    let segment_count = layout.segments.len();
    if segment_count != cuts.len() - 1 {
        return None;
    }
    let timings = par_map(threads, segment_count, |index| {
        let (section, header) = &layout.segments[index];
        let want = &src[cuts[index]..cuts[index + 1]];
        let mut dst = vec![0u8; want.len()];
        let body = file.get(section.body_range()?)?;
        decode_segment(&layout, body, header, &mut dst, &budget)
            .ok()
            .filter(|_| dst == want)
    });
    let is_failing: Vec<bool> = timings.iter().map(std::option::Option::is_none).collect();
    if is_failing.iter().any(|&is_failed| is_failed) {
        return Some(is_failing);
    }
    // the whole-demo checks, over the input: every segment decoded to it.
    // A file whose segments all decode but which fails these is not
    // fixable by falling back
    let mut check = DemoCheck::default();
    for (index, timing) in timings.into_iter().enumerate() {
        check
            .segment(&src[cuts[index]..cuts[index + 1]], timing?)
            .ok()?;
    }
    check.finish(&layout).ok()?;
    Some(is_failing)
}
