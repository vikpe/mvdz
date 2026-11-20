//! Compression: walk, code, assemble, and check that the file decodes to
//! the input (falling back per segment to DEFLATE).

use crate::budget::{Budget, Limits};
use crate::coder::rans::Shared;
use crate::decode::{SliceSrc, decode_segment, scan_layout};
use crate::error::{Error, Result};
use crate::format::*;
use crate::proto::{Proto, frame_len};
use crate::segment::{
    self, DEFLATE_LEVEL, Nums, SEGMENT, StreamStat, Walked, cut_points, frame_scan,
};
use crate::util::{crc32, par_map};
use sha2::{Digest, Sha256};

/// How the encoder runs. Nothing here changes the file it writes.
#[derive(Clone, Debug)]
pub struct Config {
    /// Worker threads.
    pub threads: usize,
    /// Decode the result and compare (falling back per segment on a
    /// mismatch). On by default; off only for experiments.
    pub verify: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            threads: 1,
            verify: true,
        }
    }
}

/// What the encoder did.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub segments: usize,
    /// Segments coded as DEFLATE instead of the transform.
    pub fallback_segments: usize,
    /// Input bytes the transform stored raw (escapes and unframeable tail).
    pub escaped: usize,
    pub streams: Vec<StreamStat>,
}

/// True if `b` starts like an MVD demo: a whole first frame that carries
/// messages (a non-empty `dem_read`/`dem_all`/... payload, or hidden or
/// multi-target data; not a bare `dem_set`), and either its messages parse
/// without an escape or, if that first frame is damaged, the next 16
/// frames are whole too. A frame's length field must fit the input, so
/// other files almost never frame: text, gzip, PNG, TIFF and random bytes
/// fail on the first frame.
pub fn starts_like_mvd(b: &[u8]) -> bool {
    let bad = |b: &[u8], p: usize| b.len() >= p + 2 && matches!(b[p + 1] & 7, 0 | 7);
    if b.len() < 2 {
        return false;
    }
    let head = match b[1] & 7 {
        0 | 2 | 7 => return false,
        3 => 10,
        _ => 6,
    };
    let Some(n) = frame_len(b, 0) else {
        return false;
    };
    if n <= head {
        return false;
    }
    if segment::walk(&b[..n], Proto::default()).is_some_and(|(wk, _)| wk.escaped() == 0) {
        return true;
    }
    let mut p = n;
    for _ in 0..16 {
        if bad(b, p) {
            return false;
        }
        match frame_len(b, p) {
            Some(k) => p += k,
            None => return false,
        }
    }
    true
}

/// One segment as the encoder sees it.
pub struct Planned {
    pub walked: Option<Walked>,
    /// the numeric values of the walk, once the predictors have run
    pub nums: Option<Nums>,
    /// its frame time: the sum of its frame time bytes
    pub dur_ms: u64,
}

/// Walks every segment in order (the protocol state carries over).
pub fn plan(src: &[u8], cuts: &[usize], proto: &mut Proto) -> Vec<Planned> {
    let mut out = Vec::with_capacity(cuts.len());
    for w in cuts.windows(2) {
        let s = &src[w[0]..w[1]];
        out.push(Planned {
            walked: walk_one(s, proto),
            nums: None,
            dur_ms: frame_scan(s).1,
        });
    }
    out
}

/// Walks one segment and updates the protocol state.
pub fn walk_one(s: &[u8], proto: &mut Proto) -> Option<Walked> {
    let (wk, pend) = segment::walk(s, *proto)?;
    *proto = pend;
    Some(wk)
}

/// The body of a segment coded without the transform: the whole segment
/// as one raw DEFLATE stream. If the best compression breaks the ratio
/// limit, the stream is written as stored blocks instead.
pub fn raw_body(s: &[u8], dur_ms: u64) -> Vec<u8> {
    let mut b = seg_hdr_bytes(s.len() as u64, dur_ms, crc32(s), C_DEFLATE);
    let mut z = crate::deflate::compress(s, DEFLATE_LEVEL);
    if !seg_ratio_ok(s.len() as u64, (b.len() + z.len()) as u64) {
        z = crate::deflate::compress(s, 0);
    }
    b.extend_from_slice(&z);
    b
}

/// The body of a walked segment with numeric values `nums`, or None if one
/// of its streams has no model in `shared` (the file's models) or it breaks
/// the ratio limit.
pub fn transform_body(
    s: &[u8],
    wk: &Walked,
    nums: &Nums,
    dur_ms: u64,
    threads: usize,
    st: &mut Vec<StreamStat>,
    shared: &[Option<Shared>],
) -> Option<Vec<u8>> {
    let mut b = seg_hdr_bytes(s.len() as u64, dur_ms, crc32(s), C_TRANSFORM);
    b.extend_from_slice(&segment::encode(wk, nums, threads, st, shared)?);
    seg_ratio_ok(s.len() as u64, b.len() as u64).then_some(b)
}

/// Compresses a demo into segments of [`SEGMENT`] raw bytes.
pub fn compress(src: &[u8], cfg: &Config) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, SEGMENT, cfg, false)
}

/// Compresses a demo into segments of about `segment` raw bytes (0: one
/// segment). Every reader decodes such files, but the format's encoder
/// writes [`SEGMENT`] only: this is for tests and test vectors, which need
/// files of several segments from small demos.
#[doc(hidden)]
pub fn compress_segmented(src: &[u8], segment: usize, cfg: &Config) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, segment, cfg, false)
}

/// Compresses a demo with every segment coded without the transform (as
/// DEFLATE): the layout the encoder falls back to. For test vectors.
#[doc(hidden)]
pub fn compress_untransformed(
    src: &[u8],
    segment: usize,
    cfg: &Config,
) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, segment, cfg, true)
}

fn compress_inner(
    src: &[u8],
    segment: usize,
    cfg: &Config,
    force_raw: bool,
) -> Result<(Vec<u8>, Info)> {
    let threads = cfg.threads.max(1);
    // an empty demo has no segment
    let cuts = if src.is_empty() {
        vec![0]
    } else {
        cut_points(src, segment)
    };
    let nseg = cuts.len() - 1;
    if nseg > MAX_SEGMENTS {
        return Err(Error::Invalid("too many segments"));
    }
    if cuts.windows(2).any(|w| (w[1] - w[0]) as u64 > MAX_SEG_RAW) {
        return Err(Error::Invalid("segment over 4 GiB"));
    }
    let mut proto = Proto::default();
    let mut plans = plan(src, &cuts, &mut proto);
    if plans.iter().any(|p| p.dur_ms > u32::MAX as u64) {
        return Err(Error::Invalid("segment over 2^32 ms"));
    }
    let header = Header {
        size: src.len() as u64,
        duration_ms: plans.iter().map(|p| p.dur_ms).sum(),
        crc: crc32(src),
        sha256: Sha256::digest(src).into(),
    };
    let (outer, inner) = if nseg >= threads {
        (threads, 1)
    } else {
        (1, threads)
    };
    // the numeric predictors run once over every walked segment: the
    // models are trained on their values and the segments coded from them
    if !force_raw {
        let nums = par_map(outer, nseg, |i| {
            let wk = plans[i].walked.as_ref()?;
            Some(segment::numerics(&src[cuts[i]..cuts[i + 1]], wk, inner))
        });
        for (p, n) in plans.iter_mut().zip(nums) {
            p.nums = n;
        }
    }
    // one model per rANS stream from its symbols in every segment, in the
    // models block (FORMAT.md §12)
    let shared = if nseg > 0 && !force_raw {
        let segs: Vec<(&Walked, &Nums)> = plans
            .iter()
            .filter_map(|p| Some((p.walked.as_ref()?, p.nums.as_ref()?)))
            .collect();
        segment::train(&segs, threads)
    } else {
        Vec::new()
    };
    // segments the transform could not code: raw from the start
    let mut raw: Vec<bool> = plans
        .iter()
        .map(|p| force_raw || p.walked.is_none())
        .collect();
    for _round in 0..2 {
        let (out, info) = assemble(
            src, &cuts, &plans, &raw, &header, &shared, outer, inner,
        );
        if !cfg.verify {
            return Ok((out, info));
        }
        let bad = failing_segments(&out, src, &cuts, threads);
        match bad {
            Some(b) if b.iter().all(|&x| !x) => return Ok((out, info)),
            Some(b) => {
                for (r, x) in raw.iter_mut().zip(b) {
                    *r |= x;
                }
            }
            // the container itself does not decode: nothing to fall back to
            None => break,
        }
    }
    Err(Error::Internal(
        "the encoder's output does not decode to its input",
    ))
}

/// Builds the file from the plans; `raw[i]` codes segment i without the
/// transform.
#[allow(clippy::too_many_arguments)]
fn assemble(
    src: &[u8],
    cuts: &[usize],
    plans: &[Planned],
    raw: &[bool],
    header: &Header,
    shared: &[Option<Shared>],
    outer: usize,
    inner: usize,
) -> (Vec<u8>, Info) {
    let nseg = plans.len();
    let bodies: Vec<(Vec<u8>, bool, Vec<StreamStat>)> = par_map(outer, nseg, |i| {
        let s = &src[cuts[i]..cuts[i + 1]];
        let mut st = Vec::new();
        if let (Some(wk), Some(nums), false) = (&plans[i].walked, &plans[i].nums, raw[i]) {
            let p = &plans[i];
            if let Some(b) = transform_body(s, wk, nums, p.dur_ms, inner, &mut st, shared) {
                return (b, false, st);
            }
        }
        (raw_body(s, plans[i].dur_ms), true, Vec::new())
    });
    let mut out = header.bytes().to_vec();
    // the models: the shared model of every stream some segment codes with it
    let mut used = vec![false; shared.len()];
    for st in bodies.iter().flat_map(|b| &b.2) {
        if st.id != crate::streams::BLOB_BODY && !crate::streams::is_blob(st.id) {
            used[st.id] = true;
        }
    }
    let ids: Vec<usize> = (0..used.len()).filter(|&sid| used[sid]).collect();
    if ids.is_empty() {
        models_block(&mut out, &[]);
    } else {
        models_block(&mut out, &segment::tables_body(shared, &ids));
    }
    let mut info = Info {
        segments: nseg,
        ..Default::default()
    };
    for (i, (b, fb, st)) in bodies.into_iter().enumerate() {
        segment_block(&mut out, &b);
        info.fallback_segments += fb as usize;
        info.escaped += plans[i]
            .walked
            .as_ref()
            .filter(|_| !fb)
            .map_or(0, |w| w.escaped());
        info.streams.extend(st);
    }
    (out, info)
}

/// Decodes `file` and compares it with `src`: None if the container does
/// not parse, else per segment whether it fails. Every segment passing
/// means the whole file decodes (the whole-file checks are run too).
pub fn failing_segments(
    file: &[u8],
    src: &[u8],
    cuts: &[usize],
    threads: usize,
) -> Option<Vec<bool>> {
    if let Ok(back) = crate::decode::decompress(file, threads, &Limits::unlimited()) {
        if back == src {
            return Some(vec![false; cuts.len() - 1]);
        }
    }
    let budget = Budget::unlimited();
    let lay = scan_layout(&mut SliceSrc(file), threads, &budget).ok()?;
    if lay.segs.len() != cuts.len() - 1 {
        return None;
    }
    let bad = par_map(threads, lay.segs.len(), |i| {
        let (s, h) = &lay.segs[i];
        let want = &src[cuts[i]..cuts[i + 1]];
        let mut dst = vec![0u8; want.len()];
        let body = &file[s.body_off() as usize..s.end() as usize];
        decode_segment(&lay, body, h, &mut dst, 1, &budget).is_err() || dst != want
    });
    // a file whose segments all decode but which still fails is not
    // fixable by falling back
    if bad.iter().all(|&x| !x) {
        return None;
    }
    Some(bad)
}
