//! Compression: walk, code, assemble, and check that the file decodes to
//! the input (falling back per segment to all tail).

use crate::budget::Budget;
use crate::coder::rans::Shared;
use crate::decode::{DemoCheck, SliceSrc, decode_segment, scan_layout};
use crate::error::{Error, Result};
use crate::format::{
    Header, MAX_SEG_CODED, MAX_SEGMENTS, SEG_FIELDS, models_block, seg_hdr_bytes, segment_block,
};
use crate::proto::{Proto, frame_len};
use crate::segment::{self, Nums, StreamStat, Walked, cut_points, frame_scan};
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
    pub streams: Vec<StreamStat>,
}

/// True if `b` starts like an MVD demo.
///
/// That is a whole first frame that carries
/// messages (a non-empty `dem_read`/`dem_all`/... payload, or hidden or
/// multi-target data; not a bare `dem_set`), and either its messages parse
/// without an escape or, if that first frame is damaged, the next 16
/// frames are whole too. A frame's length field must fit the input, so
/// other files almost never frame: text, gzip, PNG, TIFF and random bytes
/// fail on the first frame.
pub(crate) fn starts_like_mvd(b: &[u8]) -> bool {
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
        t.walked
            .extend(walks.iter().map(std::option::Option::is_some));
    }
    t.shared = trainer.models(threads);
    t
}

/// The body of a walked segment with numeric values `nums`, or None if one
/// of its streams has no model in `shared` (the file's models) or it is
/// longer than a coded segment may be.
pub(crate) fn transform_body(
    s: &[u8],
    wk: &Walked,
    nums: &Nums,
    dur_ms: u64,
    threads: usize,
    st: &mut Vec<StreamStat>,
    shared: &[Option<Shared>],
) -> Option<Vec<u8>> {
    let mut b = seg_hdr_bytes(s.len() as u32, dur_ms as u32, crc32(s));
    b.extend_from_slice(&segment::encode(wk, nums, threads, st, shared)?);
    ((b.len() - SEG_FIELDS) as u64 <= MAX_SEG_CODED).then_some(b)
}

/// Compresses a demo into segments of [`segment::SEGMENT`] raw bytes.
pub fn compress(src: &[u8], cfg: &Config) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, cfg, false)
}

/// Compresses a demo with every segment coded as all tail: the layout the
/// encoder falls back to. For tests and test vectors.
#[doc(hidden)]
pub fn compress_all_tail(src: &[u8], cfg: &Config) -> Result<(Vec<u8>, Info)> {
    compress_inner(src, cfg, true)
}

fn compress_inner(src: &[u8], cfg: &Config, all_tail: bool) -> Result<(Vec<u8>, Info)> {
    let threads = crate::util::threads(cfg.threads);
    // an empty demo has no segment
    let cuts = if src.is_empty() {
        vec![0]
    } else {
        cut_points(src)
    };
    let nseg = cuts.len() - 1;
    if nseg > MAX_SEGMENTS {
        return Err(Error::Invalid("too many segments"));
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
        if all_tail {
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
    let header = Header {
        size: src.len() as u64,
        duration_ms: t.durs.iter().sum(),
        crc,
        sha256,
    };
    // the second pass: every segment is walked again, from the protocol
    // state it starts in, and coded with the models (segments in parallel,
    // each holding its own walk only). One the transform cannot code is
    // all tail
    let tail_body = |i: usize| {
        let s = &src[cuts[i]..cuts[i + 1]];
        Body::new(
            s,
            &segment::walk_tail(s, t.protos[i]),
            t.durs[i],
            inner,
            &[],
            true,
        )
    };
    let bodies: Vec<Result<Body>> = par_map(outer, nseg, |i| {
        let s = &src[cuts[i]..cuts[i + 1]];
        if let Some((wk, _)) = t.walked[i].then(|| segment::walk(s, t.protos[i])).flatten()
            && let Ok(b) = Body::new(s, &wk, t.durs[i], inner, &t.shared, false)
        {
            return Ok(b);
        }
        tail_body(i)
    });
    let mut bodies = bodies.into_iter().collect::<Result<Vec<Body>>>()?;
    let shared = &t.shared;
    let fails = || Error::Internal("the encoder's output does not decode to its input");
    loop {
        let (out, info) = assemble(&header, shared, &bodies);
        // None: the container itself does not decode, nothing to fall back to
        let bad = failing_segments(&out, src, &cuts, threads).ok_or_else(fails)?;
        if !bad.contains(&true) {
            return Ok((out, info));
        }
        for i in (0..nseg).filter(|&i| bad[i]) {
            if bodies[i].is_all_tail {
                return Err(fails());
            }
            bodies[i] = tail_body(i)?;
        }
    }
}

/// One coded segment.
struct Body {
    bytes: Vec<u8>,
    /// coded as all tail
    is_all_tail: bool,
    /// input bytes the transform stored raw
    escaped: usize,
    streams: Vec<StreamStat>,
}

impl Body {
    /// Codes segment `s` from its walk `wk`. An error if a stream has no
    /// model in `shared` or the body is longer than a coded segment may be
    /// (an all-tail body has neither problem).
    fn new(
        s: &[u8],
        wk: &Walked,
        dur_ms: u64,
        threads: usize,
        shared: &[Option<Shared>],
        is_all_tail: bool,
    ) -> Result<Self> {
        let nums = segment::numerics(s, wk, threads);
        let mut streams = Vec::new();
        let bytes = transform_body(s, wk, &nums, dur_ms, threads, &mut streams, shared)
            .ok_or(Error::Internal("a segment that cannot be coded"))?;
        Ok(Self {
            bytes,
            is_all_tail,
            escaped: wk.escaped(),
            streams,
        })
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
        info.escaped += b.escaped;
        info.streams.extend(b.streams.iter().cloned());
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
    let lay = scan_layout(&mut SliceSrc(file), threads, &budget).ok()?;
    let nseg = lay.segs.len();
    if nseg != cuts.len() - 1 {
        return None;
    }
    let timings = par_map(threads, nseg, |i| {
        let (s, h) = &lay.segs[i];
        let want = &src[cuts[i]..cuts[i + 1]];
        let mut dst = vec![0u8; want.len()];
        let body = file.get(s.body_range()?)?;
        decode_segment(&lay, body, h, &mut dst, &budget)
            .ok()
            .filter(|_| dst == want)
    });
    let bad: Vec<bool> = timings.iter().map(std::option::Option::is_none).collect();
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
