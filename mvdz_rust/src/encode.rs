//! Compression: walk, code, assemble, and check that the file decodes to
//! the input (falling back per segment to DEFLATE).

use crate::budget::Budget;
use crate::coder::rans::Shared;
use crate::decode::{DemoCheck, SliceSrc, decode_segment, scan_layout};
use crate::error::{Error, Result};
use crate::format::*;
use crate::proto::{Proto, frame_len};
use crate::segment::{
    self, DEFLATE_LEVEL, Nums, SEGMENT, StreamStat, Walked, cut_points, frame_scan,
};
use crate::util::{HAS_THREADS, crc32, par_map};
use sha2::{Digest, Sha256};

/// How the encoder runs. Nothing here changes the file it writes.
#[derive(Clone, Debug)]
pub struct Config {
    /// Worker threads (0: one per logical core).
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

/// What the first pass over the demo leaves: the file's models, and per
/// segment what the second pass needs to code it on its own.
struct Trained {
    shared: Vec<Option<Shared>>,
    /// the protocol state each segment starts in
    protos: Vec<Proto>,
    /// whether the transform can represent the segment
    walked: Vec<bool>,
    /// each segment's frame time: the sum of its frame time bytes
    durs: Vec<u64>,
}

/// The first pass: walks every segment in order (the protocol state
/// carries over), runs its numeric predictors and counts its symbols for
/// the models (FORMAT.md §12). `window` segments are held at a time, their
/// predictors run in parallel; nothing but the counts is kept.
fn train(src: &[u8], cuts: &[usize], window: usize, inner: usize, threads: usize) -> Trained {
    let nseg = cuts.len() - 1;
    let seg = |i: usize| &src[cuts[i]..cuts[i + 1]];
    let trainer = segment::Trainer::default();
    let mut proto = Proto::default();
    let mut t = Trained {
        shared: Vec::new(),
        protos: Vec::with_capacity(nseg),
        walked: Vec::with_capacity(nseg),
        durs: (0..nseg).map(|i| frame_scan(seg(i)).1).collect(),
    };
    for first in (0..nseg).step_by(window) {
        let n = window.min(nseg - first);
        let walks: Vec<Option<Walked>> = (first..first + n)
            .map(|i| {
                t.protos.push(proto);
                let (wk, pend) = segment::walk(seg(i), proto)?;
                proto = pend;
                Some(wk)
            })
            .collect();
        let nums = par_map(window, n, |k| {
            Some(segment::numerics(seg(first + k), walks[k].as_ref()?, inner))
        });
        let segs: Vec<(&Walked, &Nums)> = walks
            .iter()
            .zip(&nums)
            .filter_map(|(w, n)| Some((w.as_ref()?, n.as_ref()?)))
            .collect();
        trainer.add(&segs, threads);
        t.walked.extend(walks.iter().map(|w| w.is_some()));
    }
    t.shared = trainer.models(threads);
    t
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
pub fn compress_untransformed(src: &[u8], segment: usize, cfg: &Config) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, segment, cfg, true)
}

fn compress_inner(
    src: &[u8],
    segment: usize,
    cfg: &Config,
    force_raw: bool,
) -> Result<(Vec<u8>, Info)> {
    let threads = crate::util::threads(cfg.threads);
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
    let (outer, inner) = if nseg >= threads {
        (threads, 1)
    } else {
        (1, threads)
    };
    // the first pass: the models. Its walk runs on one thread: with more,
    // the demo's checksum and SHA-256 are computed beside it
    let hashes = || (crc32(src), <[u8; 32]>::from(Sha256::digest(src)));
    let train = || {
        if force_raw {
            Trained {
                shared: Vec::new(),
                protos: vec![Proto::default(); nseg],
                walked: vec![false; nseg],
                durs: (0..nseg)
                    .map(|i| frame_scan(&src[cuts[i]..cuts[i + 1]]).1)
                    .collect(),
            }
        } else {
            train(src, &cuts, outer, inner, threads)
        }
    };
    let (t, (crc, sha256)) = if threads > 1 && HAS_THREADS {
        std::thread::scope(|sc| {
            let h = sc.spawn(hashes);
            (train(), h.join().expect("hashing"))
        })
    } else {
        (train(), hashes())
    };
    if t.durs.iter().any(|&d| d > u32::MAX as u64) {
        return Err(Error::Invalid("segment over 2^32 ms"));
    }
    let header = Header {
        size: src.len() as u64,
        duration_ms: t.durs.iter().sum(),
        crc,
        sha256,
    };
    // the second pass: every segment is walked again, from the protocol
    // state it starts in, and coded with the models (segments in parallel,
    // each holding its own walk only). One the transform cannot code is
    // DEFLATE
    let mut bodies: Vec<Body> = par_map(outer, nseg, |i| {
        let s = &src[cuts[i]..cuts[i + 1]];
        if let Some((wk, _)) = t.walked[i].then(|| segment::walk(s, t.protos[i])).flatten() {
            let nums = segment::numerics(s, &wk, inner);
            let mut st = Vec::new();
            if let Some(b) = transform_body(s, &wk, &nums, t.durs[i], inner, &mut st, &t.shared) {
                return Body {
                    bytes: b,
                    fallback: false,
                    escaped: wk.escaped(),
                    streams: st,
                };
            }
        }
        Body::raw(s, t.durs[i])
    });
    let (shared, durs) = (t.shared, t.durs);
    for _round in 0..2 {
        let (out, info) = assemble(&header, &shared, &bodies);
        if !cfg.verify {
            return Ok((out, info));
        }
        let bad = failing_segments(&out, src, &cuts, threads);
        match bad {
            Some(b) if b.iter().all(|&x| !x) => return Ok((out, info)),
            Some(b) => {
                for i in 0..nseg {
                    if b[i] && !bodies[i].fallback {
                        bodies[i] = Body::raw(&src[cuts[i]..cuts[i + 1]], durs[i]);
                    }
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

/// One coded segment.
struct Body {
    bytes: Vec<u8>,
    /// coded as DEFLATE instead of the transform
    fallback: bool,
    /// input bytes the transform stored raw
    escaped: usize,
    streams: Vec<StreamStat>,
}

impl Body {
    fn raw(s: &[u8], dur_ms: u64) -> Body {
        Body {
            bytes: raw_body(s, dur_ms),
            fallback: true,
            escaped: 0,
            streams: Vec::new(),
        }
    }
}

/// Builds the file from its segments' bodies.
fn assemble(header: &Header, shared: &[Option<Shared>], bodies: &[Body]) -> (Vec<u8>, Info) {
    let mut out = header.bytes().to_vec();
    // the models: the shared model of every stream some segment codes with it
    let mut used = vec![false; shared.len()];
    for st in bodies.iter().flat_map(|b| &b.streams) {
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
        segments: bodies.len(),
        ..Default::default()
    };
    for b in bodies {
        segment_block(&mut out, &b.bytes);
        info.fallback_segments += b.fallback as usize;
        info.escaped += b.escaped;
        info.streams.extend(b.streams.iter().cloned());
    }
    (out, info)
}

/// Decodes `file` and compares it with `src`, segment by segment (the
/// decoded demo is never held whole): None if the container does not
/// parse, else per segment whether it fails. Every segment passing means
/// the whole file decodes (the whole-file checks are run too).
pub fn failing_segments(
    file: &[u8],
    src: &[u8],
    cuts: &[usize],
    threads: usize,
) -> Option<Vec<bool>> {
    let budget = Budget::unlimited();
    let lay = scan_layout(&mut SliceSrc(file), threads, &budget).ok()?;
    let nseg = lay.segs.len();
    if nseg != cuts.len() - 1 {
        return None;
    }
    let (outer, inner) = if nseg >= threads {
        (threads, 1)
    } else {
        (1, threads)
    };
    let timings = par_map(outer, nseg, |i| {
        let (s, h) = &lay.segs[i];
        let want = &src[cuts[i]..cuts[i + 1]];
        let mut dst = vec![0u8; want.len()];
        let body = &file[s.body_off() as usize..s.end() as usize];
        decode_segment(&lay, body, h, &mut dst, inner, &budget)
            .ok()
            .filter(|_| dst == want)
    });
    let bad: Vec<bool> = timings.iter().map(|t| t.is_none()).collect();
    if bad.iter().any(|&x| x) {
        return Some(bad);
    }
    // the whole-demo checks, over the input: every segment decoded to it.
    // A file whose segments all decode but which fails these is not
    // fixable by falling back
    let mut chk = DemoCheck::default();
    for (i, tm) in timings.into_iter().enumerate() {
        chk.segment(&src[cuts[i]..cuts[i + 1]], tm?).ok()?;
    }
    chk.finish(&lay).ok()?;
    Some(bad)
}
