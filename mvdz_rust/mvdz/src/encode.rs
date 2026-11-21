//! Compression: walk, code, assemble, and check that the file decodes to
//! the input (falling back per segment to all tail).

use crate::budget::Budget;
use crate::coder::rans::FileModel;
use crate::decode::{DemoCheck, decode_segment, scan_layout};
use crate::error::{Error, Result};
use crate::format::{
    Header, MAX_SEGMENT_CODED, MAX_SEGMENTS, SEGMENT_FIELDS_LEN, header_bytes, models_block,
    segment_block, segment_header_bytes,
};
use crate::proto::{DEM_ALL, DEM_MULTIPLE, DEM_READ, DEM_SINGLE, DEM_STATS, Proto, frame_len};
use crate::segment::{self, Numerics, Walked, cut_points, frame_scan};
use crate::util::{HAS_THREADS, crc32, par_map};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Default)]
pub(crate) struct EncodeStats {
    /// Input bytes the transform stored raw (escapes and unframeable tail).
    pub(crate) unstructured_len: usize,
    /// Segments that failed the self-check and were coded as all tail: nonzero only for
    /// a transform bug.
    pub(crate) self_check_fallbacks: usize,
}

/// A whole first frame with messages (not a bare `dem_set`) that walks without an
/// escape, or, if it is damaged, 16 more whole frames. Text, gzip, PNG, TIFF and
/// random bytes fail on the first frame: its length field must fit the input.
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

/// The file's models, and per segment what the second pass needs to code it on its own.
#[derive(Debug)]
struct Trained {
    file_models: Vec<Option<FileModel>>,
    start_protos: Vec<Proto>,
    is_walkable: Vec<bool>,
    durations_ms: Vec<u64>,
}

/// The first pass (FORMAT.md section 12): walks in order, as the protocol state carries
/// over; `threads` segments at a time run their predictors in parallel. Keeps only counts.
fn train(src: &[u8], cuts: &[usize], threads: usize) -> Trained {
    let segment_count = cuts.len() - 1;
    let segment_bytes = |index: usize| &src[cuts[index]..cuts[index + 1]];
    let trainer = segment::Trainer::default();
    let mut proto = Proto::default();
    let mut trained = Trained {
        file_models: Vec::new(),
        start_protos: Vec::with_capacity(segment_count),
        is_walkable: Vec::with_capacity(segment_count),
        durations_ms: (0..segment_count)
            .map(|index| frame_scan(segment_bytes(index)).duration_ms)
            .collect(),
    };
    for first in (0..segment_count).step_by(threads) {
        let batch_len = threads.min(segment_count - first);
        let walks: Vec<Option<Walked>> = (first..first + batch_len)
            .map(|index| {
                trained.start_protos.push(proto);
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
    trained.file_models = trainer.models(threads);
    trained
}

/// `None` if a stream has no model in `file_models` or the body is over [`MAX_SEGMENT_CODED`].
pub(crate) fn transform_body(
    raw: &[u8],
    walked: &Walked,
    numerics: &Numerics,
    duration_ms: u64,
    model_ids: &mut Vec<usize>,
    file_models: &[Option<FileModel>],
) -> Option<Vec<u8>> {
    let mut body = segment_header_bytes(raw.len() as u32, duration_ms as u32, crc32(raw));
    body.extend_from_slice(&segment::encode(walked, numerics, model_ids, file_models)?);
    ((body.len() - SEGMENT_FIELDS_LEN) as u64 <= MAX_SEGMENT_CODED).then_some(body)
}

/// `threads`: at least 1. Threads never change the file written.
pub(crate) fn compress(src: &[u8], threads: usize) -> Result<(Vec<u8>, EncodeStats)> {
    compress_inner(src, threads, false)
}

/// For tests and test vectors.
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
    let segment_count = cuts.len() - 1;
    if segment_count > MAX_SEGMENTS {
        return Err(Error::Invalid("too many segments"));
    }
    // with more threads, the hashes run beside the first pass
    let hashes = || (crc32(src), <[u8; 32]>::from(Sha256::digest(src)));
    let train = || {
        if is_all_tail {
            Trained {
                file_models: Vec::new(),
                start_protos: vec![Proto::default(); segment_count],
                is_walkable: vec![false; segment_count],
                durations_ms: (0..segment_count)
                    .map(|index| frame_scan(&src[cuts[index]..cuts[index + 1]]).duration_ms)
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
        size_bytes: src.len() as u64,
        duration_ms: trained.durations_ms.iter().sum(),
        crc32: crc,
        sha256,
    };
    let mut bodies = code_segments(src, &cuts, &trained, threads)?;
    let file_models = &trained.file_models;
    let self_check_error = || Error::Internal("the encoder's output does not decode to its input");
    let mut fallbacks = 0;
    loop {
        let (out, mut info) = assemble(&header, file_models, &bodies);
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

/// The second pass, segments in parallel; a segment the transform cannot code is all tail.
fn code_segments(
    src: &[u8],
    cuts: &[usize],
    trained: &Trained,
    threads: usize,
) -> Result<Vec<Body>> {
    let bodies = par_map(threads, cuts.len() - 1, |index| {
        let raw = &src[cuts[index]..cuts[index + 1]];
        let walked = trained.is_walkable[index]
            .then(|| segment::walk(raw, trained.start_protos[index]))
            .flatten();
        if let Some((walked, _)) = walked
            && let Ok(body) = Body::new(
                raw,
                &walked,
                trained.durations_ms[index],
                &trained.file_models,
                false,
            )
        {
            return Ok(body);
        }
        tail_body(src, cuts, trained, index)
    });
    bodies.into_iter().collect()
}

fn tail_body(src: &[u8], cuts: &[usize], trained: &Trained, index: usize) -> Result<Body> {
    let raw = &src[cuts[index]..cuts[index + 1]];
    let walked = segment::walk_tail(raw, trained.start_protos[index]);
    Body::new(raw, &walked, trained.durations_ms[index], &[], true)
}

#[derive(Debug)]
struct Body {
    bytes: Vec<u8>,
    is_all_tail: bool,
    unstructured_len: usize,
    model_ids: Vec<usize>,
}

impl Body {
    /// Errors if a stream has no model in `file_models` or the body is too long; an all-tail
    /// body has neither problem.
    fn new(
        raw: &[u8],
        walked: &Walked,
        duration_ms: u64,
        file_models: &[Option<FileModel>],
        is_all_tail: bool,
    ) -> Result<Self> {
        let numerics = segment::numerics(raw, walked);
        let mut model_ids = Vec::new();
        let bytes = transform_body(
            raw,
            walked,
            &numerics,
            duration_ms,
            &mut model_ids,
            file_models,
        )
        .ok_or(Error::Internal("a segment that cannot be coded"))?;
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
    let mut info = EncodeStats::default();
    for body in bodies {
        segment_block(&mut out, &body.bytes);
        info.unstructured_len += body.unstructured_len;
    }
    (out, info)
}

/// Per segment whether it fails to decode to `src`; `None` if the container does not
/// parse. Runs the whole-file checks too; never holds the decoded demo whole.
pub(crate) fn failing_segments(
    file: &[u8],
    src: &[u8],
    cuts: &[usize],
    threads: usize,
) -> Option<Vec<bool>> {
    let budget = Budget::unlimited();
    let layout = scan_layout(file, threads, &budget).ok()?;
    let segment_count = layout.segments.len();
    if segment_count != cuts.len() - 1 {
        return None;
    }
    let timings = par_map(threads, segment_count, |index| {
        let (span, header) = &layout.segments[index];
        let want = &src[cuts[index]..cuts[index + 1]];
        let mut dst = vec![0u8; want.len()];
        let body = file.get(span.body_range()?)?;
        decode_segment(&layout, body, header, &mut dst, &budget)
            .ok()
            .filter(|_| dst == want)
    });
    let is_failing: Vec<bool> = timings.iter().map(std::option::Option::is_none).collect();
    if is_failing.iter().any(|&is_failed| is_failed) {
        return Some(is_failing);
    }
    // every segment decoded to the input, so check over it; a failure here is not
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
