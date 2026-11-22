//! Encoding: walk, code, assemble, and check that the file decodes to its input.

use crate::decode::Decoder;
use crate::error::{Error, Result};
use crate::format::{
    Header, MAX_SEGMENT_CODED, MAX_SEGMENTS, header_bytes, models_block, segment_block,
    segment_header_bytes,
};
use crate::parallel::{HAS_THREADS, par_map, thread_count};
use crate::protocol::{
    DEM_ALL, DEM_MULTIPLE, DEM_READ, DEM_SINGLE, DEM_STATS, ProtocolState, frame_len,
};
use crate::rans::FileModel;
use crate::segment::{self, Numerics, Walked, cut_points};
use sha2::{Digest, Sha256};

/// `max_threads` 0 is all logical cores.
///
/// # Errors
///
/// [`Error::NotMvd`] for bytes that are not an MVD demo, [`Error::DemoTooLarge`] past 65536
/// segments, [`Error::Internal`] when the output fails its self-check (a bug).
pub fn encode(bytes: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    check_demo(bytes)?;
    compress(bytes, thread_count(max_threads))
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
    if segment::walk(&bytes[..first_len], ProtocolState::default())
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
    start_protocol: ProtocolState,
    is_walkable: bool,
}

/// A pass 1 walk of one segment.
struct Counted {
    start_protocol: ProtocolState,
    end_protocol: ProtocolState,
    is_walkable: bool,
    /// All tail when the segment does not walk.
    walked: Walked,
    numerics: Numerics,
}

fn count_segment(raw: &[u8], start_protocol: ProtocolState) -> Counted {
    let (walked, end_protocol, is_walkable) = match segment::walk(raw, start_protocol) {
        Some((walked, end_protocol)) => (walked, end_protocol, true),
        None => (
            segment::walk_tail(raw, start_protocol),
            start_protocol,
            false,
        ),
    };
    Counted {
        start_protocol,
        end_protocol,
        is_walkable,
        numerics: segment::numerics(raw, &walked),
        walked,
    }
}

/// Pass 1 (FORMAT.md section 8) keeps only counts; pass 2 walks again. A segment starts in
/// the protocol the one before ends in, so the segments of a batch are walked in parallel from
/// the end protocol of the batch before, and the few that start in another protocol walked again.
fn run_first_pass<'a>(segments: &[&'a [u8]], threads: usize) -> FirstPass<'a> {
    let mut model_builder = segment::ModelBuilder::default();
    let mut protocol = ProtocolState::default();
    let mut plans = Vec::with_capacity(segments.len());
    let mut first = 0;
    while first < segments.len() {
        // the first segment alone: it sets the protocol of most demos
        let batch_len = if first == 0 {
            1
        } else {
            threads.min(segments.len() - first)
        };
        let batch_protocol = protocol;
        let batch_segments = &segments[first..first + batch_len];
        let mut batch = par_map(threads, batch_segments, |&raw| {
            count_segment(raw, batch_protocol)
        });
        for (&raw, counted) in batch_segments.iter().zip(batch.iter_mut()) {
            if counted.start_protocol != protocol {
                *counted = count_segment(raw, protocol);
            }
            protocol = counted.end_protocol;
            plans.push(SegmentPlan {
                raw,
                start_protocol: counted.start_protocol,
                is_walkable: counted.is_walkable,
            });
        }
        let counted_segments: Vec<(&Walked, &Numerics)> = batch
            .iter()
            .map(|counted| (&counted.walked, &counted.numerics))
            .collect();
        model_builder.add(&counted_segments, threads);
        first += batch_len;
    }
    FirstPass {
        file_models: model_builder.models(threads),
        segments: plans,
    }
}

/// Threads never change the file written.
pub(crate) fn compress(src: &[u8], threads: usize) -> Result<Vec<u8>> {
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
        duration_ms: segments
            .iter()
            .map(|raw| u64::from(segment::duration_ms(raw)))
            .sum(),
        crc32: crc,
        sha256,
    };
    let bodies = code_segments(&first_pass, threads);
    let out = assemble(&header, &first_pass.file_models, &bodies);
    if !decodes_to(&out, &segments, threads) {
        return Err(Error::Internal(
            "the encoder's output does not decode to its input",
        ));
    }
    Ok(out)
}

/// FORMAT.md section 8 rule 5: transform-coded, else all tail, else stored.
fn code_segments(first_pass: &FirstPass, threads: usize) -> Vec<Body> {
    let file_models = &first_pass.file_models;
    par_map(threads, &first_pass.segments, |plan| {
        let walked = (plan.is_walkable)
            .then(|| segment::walk(plan.raw, plan.start_protocol))
            .flatten();
        let transform_coded =
            walked.and_then(|(walked, _)| Body::coded(plan, &walked, file_models));
        let coded = transform_coded.or_else(|| {
            let all_tail = segment::walk_tail(plan.raw, plan.start_protocol);
            Body::coded(plan, &all_tail, file_models)
        });
        coded.unwrap_or_else(|| Body::stored(plan))
    })
}

#[derive(Debug)]
struct Body {
    bytes: Vec<u8>,
    model_ids: Vec<usize>,
}

impl Body {
    /// `None` when the models cannot code the segment or it would be too large.
    fn coded(
        plan: &SegmentPlan,
        walked: &Walked,
        file_models: &[Option<FileModel>],
    ) -> Option<Self> {
        let numerics = segment::numerics(plan.raw, walked);
        let mut model_ids = Vec::new();
        let coded = segment::encode(walked, &numerics, &mut model_ids, file_models)
            .filter(|coded| coded.len() as u64 <= MAX_SEGMENT_CODED)?;
        let mut bytes = segment_fields(plan.raw);
        bytes.extend_from_slice(&coded);
        Some(Self { bytes, model_ids })
    }

    fn stored(plan: &SegmentPlan) -> Self {
        let mut bytes = segment_fields(plan.raw);
        bytes.extend_from_slice(&segment::stored(plan.raw, plan.start_protocol));
        Self {
            bytes,
            model_ids: Vec::new(),
        }
    }
}

fn segment_fields(raw: &[u8]) -> Vec<u8> {
    segment_header_bytes(
        raw.len() as u32,
        segment::duration_ms(raw),
        crc32fast::hash(raw),
    )
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

/// The self-check: never holds the decoded demo whole.
fn decodes_to(file: &[u8], segments: &[&[u8]], threads: usize) -> bool {
    const NO_MEMORY_LIMIT: u64 = u64::MAX;
    let Ok(mut decoder) = Decoder::with_max_memory(file, threads, NO_MEMORY_LIMIT) else {
        return false;
    };
    let is_each_segment_decoded = segments
        .iter()
        .all(|&raw| matches!(decoder.next_segment(), Ok(Some(segment)) if segment == raw));
    // the whole-demo checks, and nothing after the last segment
    is_each_segment_decoded && matches!(decoder.next_segment(), Ok(None))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without models for the blob tokens only the stored segment is left.
    #[test]
    fn segment_no_model_codes_is_stored() {
        let demo = b"not a demo: neither walked nor coded as tail";
        let first_pass = FirstPass {
            file_models: Vec::new(),
            segments: vec![SegmentPlan {
                raw: demo,
                start_protocol: ProtocolState::default(),
                is_walkable: false,
            }],
        };
        let bodies = code_segments(&first_pass, 1);
        let header = Header {
            size_bytes: demo.len() as u64,
            duration_ms: 0,
            crc32: crc32fast::hash(demo),
            sha256: [0; 32],
        };
        let file = assemble(&header, &first_pass.file_models, &bodies);
        assert!(decodes_to(&file, &[demo], 1));
        let stored_len = demo.len() + 16;
        assert!(file.ends_with(demo) && bodies[0].bytes.len() == 12 + stored_len);
    }
}
