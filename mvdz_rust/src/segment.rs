//! Transform-coded segments: the structural walk, the numeric groups and
//! the stream bodies (FORMAT.md, "Transform segment body").
//!
//! ```text
//! fte uv | fte2 uv | mvd1 uv | tail uv
//! nstreams uv | nstreams x entry | blob_len uv
//! bodies of the structural and numeric streams, in table order
//! blob body: blob_len bytes
//!
//! entry := id uv, count uv, len uv     structural, numeric
//!          id uv, count uv             blob
//! ```
//!
//! A structural or numeric stream is rANS with its model in the file's
//! models block; a segment with such a stream that has no model there is
//! not transform-coded (the encoder deflates it whole).
//! The blob streams have no bodies of their own: their bytes, concatenated
//! in table order, are one raw DEFLATE stream, the blob body. Without blob
//! streams `blob_len` is 0 and there is no blob body.

use crate::budget::Budget;
use crate::coder::rans::FileTables;
use crate::coder::{self, rans};
use crate::error::{Error, Result, bad};
use crate::format::TABLES_MAX;
use crate::numeric::{Buf, Group, NCtx, NumIO, WAVE1, WAVE1_PIPE, WAVE2};
use crate::proto::{Proto, frame_len};
use crate::streams::*;
use crate::util::{HAS_THREADS, Rd, par_map, put_uv};
use crate::walk::{Recs, SymIO, Walker};
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Raw bytes per segment: a segment is cut at the first frame boundary at
/// or after this many bytes.
pub const SEGMENT: usize = 256 * 1024;

/// DEFLATE level of the blob body and of segments that fall back to
/// DEFLATE.
pub const DEFLATE_LEVEL: u32 = 9;

/// Per-stream statistics for reporting. The blob streams of a segment
/// share one body: each has `packed` 0, and the body is reported once
/// under the id [`BLOB_BODY`] with the summed count.
#[derive(Clone, Debug, Default)]
pub struct StreamStat {
    pub id: usize,
    pub count: usize,
    pub packed: usize,
}

// ---------------------------------------------------------------- encode side I/O

pub struct EncIO {
    pub syms: Vec<Vec<u8>>,
    /// the context of each symbol (contexts fit 16 bits)
    pub ctxs: Vec<Vec<u16>>,
    pub blobs: Vec<Vec<u8>>,
}

impl EncIO {
    fn new() -> EncIO {
        EncIO {
            syms: (0..N_LAZY).map(|_| Vec::new()).collect(),
            ctxs: (0..N_LAZY).map(|_| Vec::new()).collect(),
            blobs: (0..N_STREAMS).map(|_| Vec::new()).collect(),
        }
    }
}

impl SymIO for EncIO {
    #[inline(always)]
    fn byte(&mut self, sid: usize, ctx: u32, v: u8) -> u8 {
        debug_assert!(ctx <= 0xffff, "stream {sid}: context {ctx:#x} over 16 bits");
        self.syms[sid].push(v);
        self.ctxs[sid].push(ctx as u16);
        v
    }
    fn put_blob(&mut self, sid: usize, b: &[u8]) {
        self.blobs[sid].extend_from_slice(b);
    }
    fn take_blob(&mut self, _: usize, _: usize) -> Option<&[u8]> {
        None
    }
    fn blob_rest(&self, _: usize) -> &[u8] {
        &[]
    }
}

struct NumEnc {
    vals: Vec<rans::NumSymW>,
}

impl NumIO for NumEnc {
    #[inline(always)]
    fn code(&mut self, k: usize, ctx: NCtx, v: i32) -> i32 {
        self.vals[k].push(&ctx, v);
        v
    }
}

// ---------------------------------------------------------------- decode side I/O

pub struct DecIO<'a> {
    lazy: [rans::ByteDec<'a>; N_LAZY],
    blobs: Vec<(Cow<'a, [u8]>, usize)>,
    pub over: bool,
}

impl SymIO for DecIO<'_> {
    #[inline(always)]
    fn byte(&mut self, sid: usize, ctx: u32, _: u8) -> u8 {
        debug_assert!(ctx <= 0xffff, "stream {sid}: context {ctx:#x} over 16 bits");
        match self.lazy[sid].get(ctx) {
            Some(v) => v,
            None => {
                self.over = true;
                // an exhausted stream reads as end tokens so every loop stops
                255
            }
        }
    }
    fn put_blob(&mut self, _: usize, _: &[u8]) {}
    fn take_blob(&mut self, sid: usize, n: usize) -> Option<&[u8]> {
        let (b, i) = &mut self.blobs[sid];
        let s = b.get(*i..i.checked_add(n)?)?;
        *i += n;
        Some(s)
    }
    fn blob_rest(&self, sid: usize) -> &[u8] {
        let (b, i) = &self.blobs[sid];
        &b[(*i).min(b.len())..]
    }
}

const MAXK: usize = 6;

struct NumDecIO<'a> {
    d: [rans::NumDec<'a>; MAXK],
    over: bool,
}

impl NumIO for NumDecIO<'_> {
    #[inline(always)]
    fn code(&mut self, k: usize, ctx: NCtx, _: i32) -> i32 {
        match self.d[k].get(ctx) {
            Some(v) => v,
            None => {
                self.over = true;
                0
            }
        }
    }
}

/// (body, count) of a present stream.
type StreamEnt<'a> = Option<(&'a [u8], usize)>;

fn walk_dec<'a>(
    lazy: [rans::ByteDec<'a>; N_LAZY],
    blobs: Vec<(Cow<'a, [u8]>, usize)>,
    dst: Buf,
    recs: &Recs,
    proto: Proto,
    tail: usize,
) -> Result<()> {
    let io = DecIO {
        lazy,
        blobs,
        over: false,
    };
    let mut w: Walker<false, DecIO> = Walker::new(&[], dst.ptr(), dst.len(), recs, io, proto);
    w.run(tail);
    if w.err || w.io.over {
        return bad("structure");
    }
    if !w.io.lazy.iter().all(|d| d.done()) || !w.io.blobs.iter().all(|(b, i)| *i == b.len()) {
        return bad("unconsumed stream data");
    }
    Ok(())
}

fn run_group(
    g: Group,
    recs: &Recs,
    buf: Buf,
    ents: &[StreamEnt],
    fm: Option<&rans::Models>,
) -> Result<()> {
    let mut d: [rans::NumDec; MAXK] = std::array::from_fn(|_| rans::NumDec::empty());
    for (k, &sid) in g.streams().iter().enumerate() {
        if let Some((b, c)) = ents[sid] {
            d[k] = rans::NumDec::new(sid, b, c, coder::file_model(fm, sid)?)?;
        }
    }
    let mut io = NumDecIO { d, over: false };
    g.run::<false, NumDecIO>(recs, buf, &mut io);
    if io.over || !io.d.iter().all(|d| d.done()) {
        return bad("numeric stream");
    }
    Ok(())
}

// ---------------------------------------------------------------- segment encode

/// The encode-direction walk of one segment.
pub struct Walked {
    io: EncIO,
    recs: Recs,
    tail: usize,
    proto0: Proto,
}

impl Walked {
    /// Raw bytes the transform could not structure (escapes and tail).
    pub fn escaped(&self) -> usize {
        self.io.blobs[RAW].len() + self.io.blobs[TAIL].len()
    }
}

/// Walks one segment starting in protocol state `proto`; returns the walk
/// and the protocol state at its end, or None if the transform cannot
/// represent the segment.
pub fn walk(src: &[u8], proto: Proto) -> Option<(Walked, Proto)> {
    let recs = Recs::growable(src.len());
    let (io, tail, pend, err) = {
        let mut w: Walker<true, EncIO> =
            Walker::new(src, std::ptr::null_mut(), 0, &recs, EncIO::new(), proto);
        let tail = w.run(0);
        (w.io, tail, w.proto, w.err)
    };
    recs.finish();
    if err {
        return None;
    }
    Some((
        Walked {
            io,
            recs,
            tail,
            proto0: proto,
        },
        pend,
    ))
}

#[derive(Clone, Copy)]
enum Kind {
    Lazy,
    Num,
    /// every blob stream of the segment, as one DEFLATE stream
    Blobs,
}

/// The blob streams of a walked segment, in id order.
fn blob_ids(wk: &Walked) -> Vec<usize> {
    (0..N_STREAMS)
        .filter(|&sid| is_blob(sid) && !wk.io.blobs[sid].is_empty())
        .collect()
}

/// The body of one stream; None for a structural or numeric stream
/// without a model in the file.
fn pack_one(
    kind: Kind,
    sid: usize,
    wk: &Walked,
    num: Option<&rans::NumSyms>,
    file: Option<&rans::Shared>,
) -> Option<Vec<u8>> {
    match kind {
        Kind::Lazy => Some(rans::encode_bytes(
            &wk.io.syms[sid],
            &wk.io.ctxs[sid],
            file?,
        )),
        Kind::Num => Some(rans::encode_num(num?, file?)),
        Kind::Blobs => {
            let ids = blob_ids(wk);
            let mut all = Vec::with_capacity(ids.iter().map(|&i| wk.io.blobs[i].len()).sum());
            for i in ids {
                all.extend_from_slice(&wk.io.blobs[i]);
            }
            Some(crate::deflate::compress(&all, DEFLATE_LEVEL))
        }
    }
}

/// The numeric groups in coding order.
fn groups() -> Vec<Group> {
    WAVE1.iter().chain(WAVE2.iter()).copied().collect()
}

/// The numeric values of a walked segment as they are coded, per group
/// (in [`groups`] order) and stream of the group.
pub type Nums = Vec<Vec<rans::NumSyms>>;

/// Runs the numeric predictors over a walked segment `src`. The encoder
/// does this once: the models are trained on the values and the segment is
/// coded from them.
pub fn numerics(src: &[u8], wk: &Walked, threads: usize) -> Nums {
    let groups = groups();
    let buf = Buf::ro(src);
    par_map(threads, groups.len(), |gi| {
        let g = groups[gi];
        let mut io = NumEnc {
            vals: g
                .streams()
                .iter()
                .map(|&sid| rans::NumSymW::new(sid))
                .collect(),
        };
        g.run::<true, NumEnc>(&wk.recs, buf, &mut io);
        io.vals.into_iter().map(|w| w.finish()).collect()
    })
}

/// Builds the shared models of a file (FORMAT.md §12 rule 11): one per
/// structural and numeric stream, from its per-context counts summed over
/// every walked segment, in segment order. The segments are added a few
/// at a time: only their counts are kept.
pub struct Trainer {
    /// the counts of each stream
    tallies: Vec<std::sync::Mutex<Option<rans::Tally>>>,
    /// where a numeric stream's values are: (group, index in the group)
    at: Vec<Option<(usize, usize)>>,
}

impl Default for Trainer {
    fn default() -> Self {
        let mut at = vec![None; N_STREAMS];
        for (gi, g) in groups().iter().enumerate() {
            for (k, &sid) in g.streams().iter().enumerate() {
                at[sid] = Some((gi, k));
            }
        }
        Trainer {
            tallies: (0..N_STREAMS).map(|_| Default::default()).collect(),
            at,
        }
    }
}

impl Trainer {
    /// Counts the symbols of the next segments `(walk, numeric values)`.
    pub fn add(&self, segs: &[(&Walked, &Nums)], threads: usize) {
        // a stream's counts do not depend on another stream's: streams in
        // parallel, each over the segments in order
        par_map(threads, N_STREAMS, |sid| {
            let mut tally = self.tallies[sid].lock().expect("no panic while counting");
            let mut add = |alpha: usize, syms: &[u8], ctxs: &[u16]| {
                if !syms.is_empty() {
                    tally
                        .get_or_insert_with(|| rans::Tally::new(alpha))
                        .add(syms, ctxs);
                }
            };
            for &(wk, nums) in segs {
                if sid < N_LAZY {
                    add(256, &wk.io.syms[sid], &wk.io.ctxs[sid]);
                }
                if let Some((gi, k)) = self.at[sid] {
                    add(rans::NALPHA, &nums[gi][k].syms, &nums[gi][k].ctxs);
                }
            }
        });
    }

    /// The models, by stream id.
    pub fn models(self, threads: usize) -> Vec<Option<rans::Shared>> {
        let mut models: Vec<Option<rans::Shared>> = par_map(threads, N_STREAMS, |sid| {
            let tally = self.tallies[sid].lock().expect("no panic while counting");
            tally.as_ref().and_then(|t| t.shared())
        });
        fit_tables(&mut models, TABLES_MAX);
        models
    }
}

/// The TABLES body (FORMAT.md §2.6) of the models of streams `ids`
/// (ascending, each with a model).
pub fn tables_body(models: &[Option<rans::Shared>], ids: &[usize]) -> Vec<u8> {
    let mut t = Vec::new();
    put_uv(&mut t, ids.len() as u64);
    for &sid in ids {
        let h = &models[sid].as_ref().expect("a model").hdr;
        put_uv(&mut t, sid as u64);
        put_uv(&mut t, h.len() as u64);
        t.extend_from_slice(h);
    }
    t
}

/// Drops the models with the largest headers until the models block is at
/// most `max` bytes (its limit, FORMAT.md §9); a segment with one of those
/// streams is then not transform-coded.
fn fit_tables(models: &mut [Option<rans::Shared>], max: u64) {
    loop {
        let ids: Vec<usize> = (0..models.len()).filter(|&i| models[i].is_some()).collect();
        if ids.is_empty() || tables_body(models, &ids).len() as u64 <= max {
            return;
        }
        let big = ids.into_iter().max_by_key(|&i| {
            let h = models[i].as_ref().map_or(0, |m| m.hdr.len());
            (h, i)
        });
        models[big.expect("a model")] = None;
    }
}

/// The transform body of a walked segment with numeric values `nums`, or
/// None if one of its structural or numeric streams has no model in
/// `shared` (by stream id).
pub fn encode(
    wk: &Walked,
    nums: &Nums,
    threads: usize,
    stats: &mut Vec<StreamStat>,
    shared: &[Option<rans::Shared>],
) -> Option<Vec<u8>> {
    let groups = groups();
    // every non-empty stream is a job: (sid, kind, group, index in group)
    let mut jobs: Vec<(usize, Kind, usize, usize)> = Vec::new();
    for sid in 0..N_LAZY {
        if !wk.io.syms[sid].is_empty() {
            jobs.push((sid, Kind::Lazy, 0, 0));
        }
    }
    for (gi, g) in groups.iter().enumerate() {
        for (k, &sid) in g.streams().iter().enumerate() {
            if !nums[gi][k].syms.is_empty() {
                jobs.push((sid, Kind::Num, gi, k));
            }
        }
    }
    // the blob streams: one job, after every stream in id order
    let blobs = blob_ids(wk);
    let blob_total: usize = blobs.iter().map(|&sid| wk.io.blobs[sid].len()).sum();
    if !blobs.is_empty() {
        jobs.push((BLOB_BODY, Kind::Blobs, 0, 0));
    }
    let size = |j: &(usize, Kind, usize, usize)| match j.1 {
        Kind::Lazy => wk.io.syms[j.0].len(),
        Kind::Num => nums[j.2][j.3].syms.len() * 2,
        Kind::Blobs => blob_total,
    };
    // biggest first for load balance; output in stream id order
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(size(&jobs[i])));
    let packed_sorted = par_map(threads, jobs.len(), |k| {
        let (sid, kind, gi, i) = jobs[order[k]];
        let num = matches!(kind, Kind::Num).then(|| &nums[gi][i]);
        let file = shared.get(sid).and_then(|m| m.as_ref());
        pack_one(kind, sid, wk, num, file)
    });
    let mut packed: Vec<Vec<u8>> = (0..jobs.len()).map(|_| Vec::new()).collect();
    for (k, p) in packed_sorted.into_iter().enumerate() {
        packed[order[k]] = p?;
    }
    // table order: stream id; the blob streams follow (their ids are the
    // highest), each with its own entry but one shared body
    let blob_job = jobs.iter().position(|j| matches!(j.1, Kind::Blobs));
    let mut ids: Vec<usize> = (0..jobs.len()).filter(|&i| Some(i) != blob_job).collect();
    ids.sort_by_key(|&i| jobs[i].0);
    let count = |i: usize| match jobs[i].1 {
        Kind::Lazy => wk.io.syms[jobs[i].0].len(),
        Kind::Num => nums[jobs[i].2][jobs[i].3].syms.len(),
        Kind::Blobs => blob_total,
    };
    let blob_body: &[u8] = match blob_job {
        Some(i) => &packed[i],
        None => &[],
    };
    let mut out = Vec::new();
    put_uv(&mut out, wk.proto0.fte as u64);
    put_uv(&mut out, wk.proto0.fte2 as u64);
    put_uv(&mut out, wk.proto0.mvd1 as u64);
    put_uv(&mut out, wk.tail as u64);
    put_uv(&mut out, (ids.len() + blobs.len()) as u64);
    for &i in &ids {
        let body = &packed[i];
        put_uv(&mut out, jobs[i].0 as u64);
        put_uv(&mut out, count(i) as u64);
        put_uv(&mut out, body.len() as u64);
        stats.push(StreamStat {
            id: jobs[i].0,
            count: count(i),
            packed: body.len(),
        });
    }
    for &sid in &blobs {
        put_uv(&mut out, sid as u64);
        put_uv(&mut out, wk.io.blobs[sid].len() as u64);
        stats.push(StreamStat {
            id: sid,
            count: wk.io.blobs[sid].len(),
            packed: 0,
        });
    }
    put_uv(&mut out, blob_body.len() as u64);
    if blob_job.is_some() {
        stats.push(StreamStat {
            id: BLOB_BODY,
            count: blob_total,
            packed: blob_body.len(),
        });
    }
    for &i in &ids {
        out.extend_from_slice(&packed[i]);
    }
    out.extend_from_slice(blob_body);
    Some(out)
}

/// Cut points: frame boundaries about every `seg` bytes (0: one segment).
pub fn cut_points(src: &[u8], seg: usize) -> Vec<usize> {
    let mut cuts = vec![0];
    if seg > 0 {
        let mut p = 0;
        let mut start = 0;
        while let Some(n) = frame_len(src, p) {
            p += n;
            if p - start >= seg && p < src.len() {
                cuts.push(p);
                start = p;
            }
        }
    }
    cuts.push(src.len());
    cuts
}

/// Frames, summed frame time (ms) and the length of the whole frames at
/// the start of `b` (the rest is the unframeable tail).
pub fn frame_scan(b: &[u8]) -> (u64, u64, usize) {
    let (mut frames, mut ms, mut p) = (0u64, 0u64, 0usize);
    while let Some(n) = frame_len(b, p) {
        frames += 1;
        ms += b[p] as u64;
        p += n;
    }
    (frames, ms, p)
}

// ---------------------------------------------------------------- decode

/// The parsed header of a transform body.
pub struct Header {
    proto: Proto,
    tail: usize,
    /// (id, count, body length); a blob stream has no body of its own: 0
    ents: Vec<(usize, usize, usize)>,
    /// length of the blob body, after the stream bodies
    blob_len: usize,
    body_off: usize,
}

/// Parses the header and stream table of a transform body that decodes to
/// `raw_len` bytes.
pub fn parse_header(body: &[u8], raw_len: usize) -> Result<Header> {
    let e = Error::Corrupt("segment header");
    let mut r = Rd::new(body);
    let fte = r.u32v().ok_or(e)?;
    let fte2 = r.u32v().ok_or(e)?;
    let mvd1 = r.u32v().ok_or(e)?;
    let tail = r.uz().ok_or(e)?;
    let n = r.uz().ok_or(e)?;
    if tail > raw_len {
        return bad("tail length");
    }
    if n > N_STREAMS {
        return bad("stream count");
    }
    let te = Error::Corrupt("stream table");
    let mut ents = Vec::with_capacity(n);
    let mut seen = [false; N_STREAMS];
    let mut total = 0usize;
    let mut blob_total = 0usize;
    for _ in 0..n {
        let id = r.uz().ok_or(te)?;
        if id >= N_STREAMS || !(is_lazy(id) || is_num(id) || is_blob(id)) {
            return bad("stream id");
        }
        // a blob stream has no body of its own
        let count = r.uz().ok_or(te)?;
        let len = if is_blob(id) { 0 } else { r.uz().ok_or(te)? };
        if is_blob(id) {
            blob_total = blob_total.checked_add(count).ok_or(te)?;
        }
        if seen[id] {
            return bad("duplicate stream");
        }
        seen[id] = true;
        // no stream holds more symbols than the output has bytes (plus slack)
        if count > raw_len.saturating_add(16) {
            return bad("stream count");
        }
        total = total.checked_add(len).ok_or(te)?;
        ents.push((id, count, len));
    }
    let blob_len = r.uz().ok_or(te)?;
    // a blob body exactly when there are blob bytes
    if (blob_total == 0) != (blob_len == 0) {
        return bad("blob body");
    }
    total = total.checked_add(blob_len).ok_or(te)?;
    if total != body.len() - r.i {
        return bad("stream bodies");
    }
    Ok(Header {
        proto: Proto::new(fte, fte2, mvd1),
        tail,
        ents,
        blob_len,
        body_off: r.i,
    })
}

/// Decodes a transform body into `dst` (its exact raw length). The CRC is
/// checked by the caller.
pub fn decode(
    body: &[u8],
    h: &Header,
    dst: &mut [u8],
    tables: Option<&FileTables>,
    threads: usize,
    budget: &Budget,
) -> Result<()> {
    let n = h.ents.len();
    // the file's models
    let fm = tables.map(|t| t.models());
    let mut bodies: Vec<&[u8]> = Vec::with_capacity(n);
    let mut r = Rd {
        b: body,
        i: h.body_off,
    };
    for &(_, _, len) in &h.ents {
        bodies.push(r.take(len).ok_or(Error::Corrupt("stream body"))?);
    }
    let blob_body = r.take(h.blob_len).ok_or(Error::Corrupt("stream body"))?;
    let cnt = |sid: usize| h.ents.iter().find(|e| e.0 == sid).map_or(0, |e| e.1);
    // scratch: the decoded blobs and the record lists
    let recs_bytes = 12 * cnt(PI_NUM)
        + 16 * cnt(PE_BITS)
        + 20 * cnt(SND_NUM)
        + 20 * cnt(TE_TYPE)
        + 8 * (cnt(UC) / UC_MIN_SYMS);
    let mut scratch = recs_bytes as u64;
    for &(id, count, _) in &h.ents {
        if is_blob(id) {
            scratch += count as u64;
        }
    }
    let _charge = budget.alloc(scratch)?;
    // the blob streams first: one DEFLATE stream, cut by their counts
    let blob_total: usize = (0..n)
        .filter(|&i| is_blob(h.ents[i].0))
        .map(|i| h.ents[i].1)
        .sum();
    let blob_bytes = if blob_total > 0 {
        crate::deflate::decompress(blob_body, blob_total)?
    } else {
        Vec::new()
    };
    let mut blob_bufs: Vec<(Cow<[u8]>, usize)> = (0..N_STREAMS)
        .map(|_| (Cow::Borrowed(&[][..]), 0))
        .collect();
    let mut at = 0;
    for &(id, count, _) in h.ents.iter().filter(|e| is_blob(e.0)) {
        blob_bufs[id].0 = Cow::Borrowed(&blob_bytes[at..at + count]);
        at += count;
    }
    let mut num_ents: Vec<StreamEnt> = vec![None; N_STREAMS];
    let mut lazy_ents: Vec<StreamEnt> = vec![None; N_LAZY];
    for i in 0..n {
        let (id, count, _) = h.ents[i];
        if is_lazy(id) {
            lazy_ents[id] = Some((bodies[i], count));
        } else if is_num(id) {
            num_ents[id] = Some((bodies[i], count));
        }
    }
    let recs = Recs::fixed(
        cnt(PI_NUM),
        cnt(PE_BITS),
        cnt(SND_NUM),
        cnt(TE_TYPE),
        cnt(UC) / UC_MIN_SYMS,
    )?;
    let buf = Buf::new(dst);
    let run_g = |g: Group| run_group(g, &recs, buf, &num_ents, fm);
    let blob_cell = std::sync::Mutex::new(Some(blob_bufs));
    let walk = || -> Result<()> {
        let blobs = blob_cell
            .lock()
            .map_err(|_| Error::Internal("lock"))?
            .take()
            .ok_or(Error::Internal("walk"))?;
        let mut lazy: [rans::ByteDec; N_LAZY] = std::array::from_fn(|_| rans::ByteDec::empty());
        for (id, e) in lazy_ents.iter().enumerate() {
            if let Some((b, c)) = *e {
                lazy[id] = rans::ByteDec::new(b, c, coder::file_model(fm, id)?)?;
            }
        }
        walk_dec(lazy, blobs, buf, &recs, h.proto, h.tail)
    };
    if threads > 1 && HAS_THREADS {
        // pipelined: wave-1 groups follow the walker on worker threads
        let next = AtomicUsize::new(0);
        let (wres, gres) = std::thread::scope(|sc| {
            let workers: Vec<_> = (0..(threads - 1).min(WAVE1_PIPE.len()))
                .map(|_| {
                    sc.spawn(|| -> Result<()> {
                        let mut res = Ok(());
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= WAVE1_PIPE.len() {
                                break;
                            }
                            let r = run_g(WAVE1_PIPE[i]);
                            if res.is_ok() {
                                res = r;
                            }
                        }
                        res
                    })
                })
                .collect();
            // finish the record buffers on every exit, unwinding included,
            // so worker threads can never wait forever
            struct Finish<'r>(&'r Recs);
            impl Drop for Finish<'_> {
                fn drop(&mut self) {
                    self.0.finish();
                }
            }
            let guard = Finish(&recs);
            let wres = walk();
            drop(guard);
            let mut gres = Ok(());
            for h in workers {
                let r = h.join().unwrap_or(Err(Error::Internal("worker panicked")));
                if gres.is_ok() {
                    gres = r;
                }
            }
            (wres, gres)
        });
        wres?;
        gres?;
    } else {
        let r = walk();
        recs.finish();
        r?;
        for &g in &WAVE1 {
            run_g(g)?;
        }
    }
    for r in par_map(threads.min(2), WAVE2.len(), |i| run_g(WAVE2[i])) {
        r?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;

    /// The models of the segments `segs`.
    fn train(segs: &[(&Walked, &Nums)]) -> Vec<Option<rans::Shared>> {
        let t = Trainer::default();
        t.add(segs, 1);
        t.models(1)
    }

    /// The segment `body` decoded.
    fn decoded(
        body: &[u8],
        h: &Header,
        len: usize,
        tables: Option<&FileTables>,
        threads: usize,
    ) -> Vec<u8> {
        let mut dst = vec![0u8; len];
        decode(body, h, &mut dst, tables, threads, &Budget::unlimited()).unwrap();
        dst
    }

    /// The transform body of `src` with the models trained on it, and those
    /// models as a reader has them.
    fn coded(src: &[u8]) -> (Vec<u8>, FileTables) {
        let (wk, _) = walk(src, Proto::default()).unwrap();
        let nums = numerics(src, &wk, 1);
        let models = train(&[(&wk, &nums)]);
        let ids: Vec<usize> = (0..models.len()).filter(|&i| models[i].is_some()).collect();
        let ft = FileTables::new(&tables_body(&models, &ids), 1, &Budget::unlimited()).unwrap();
        let body = encode(&wk, &nums, 1, &mut Vec::new(), &models).unwrap();
        (body, ft)
    }

    /// A tiny demo: playerinfo, packet entities, sounds, stats, a print.
    fn demo() -> Vec<u8> {
        let mut d = Vec::new();
        for f in 0..12u8 {
            let mut p = vec![42, 0];
            p.extend_from_slice(&0x3fu16.to_le_bytes());
            p.push(f);
            for k in 0..6u16 {
                p.extend_from_slice(&(100 * k + f as u16 * 7).to_le_bytes());
            }
            p.extend_from_slice(&[47, 1, 2]);
            p.extend_from_slice(&(1u16 << 9 | 5).to_le_bytes());
            p.extend_from_slice(&(10 + f as u16).to_le_bytes());
            p.extend_from_slice(&[0, 0]);
            p.extend_from_slice(&[6, 9, 0, 1]);
            p.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
            p.extend_from_slice(&[3, 1, f, 8, 2]);
            p.extend_from_slice(b"hi\0");
            d.push(13);
            d.push(6);
            d.extend_from_slice(&(p.len() as u32).to_le_bytes());
            d.extend_from_slice(&p);
        }
        d
    }

    #[test]
    fn segment_round_trip() {
        let src = demo();
        for threads in [1, 3] {
            let (body, ft) = coded(&src);
            let h = parse_header(&body, src.len()).unwrap();
            let dst = decoded(&body, &h, src.len(), Some(&ft), threads);
            assert!(dst == src, "{threads}");
        }
    }
    /// usercmd hidden blocks (type 1): on and off the grid, other lengths,
    /// two in one payload; streams UC and UC_A only where a segment has one.
    #[test]
    fn usercmd_round_trip() {
        let mut src = demo();
        let odd = [-0.0f32, f32::NAN, 180.0, -180.0, 1.2345, 359.99];
        for i in 0..300u32 {
            let mut p = Vec::new();
            for j in 0..1 + (i % 37 == 0) as u32 {
                let mut b = vec![((i + j) % 5) as u8, (i % 50 == 0) as u8, 13];
                let yaw = ((i * 97) as u16 as i16 as f32) * (45.0 / 8192.0);
                let pitch = if i % 11 == 0 {
                    odd[(i as usize / 11) % odd.len()]
                } else {
                    -(i as f32) * (45.0 / 8192.0)
                };
                for a in [pitch, yaw, 0.0] {
                    b.extend_from_slice(&a.to_le_bytes());
                }
                for m in [if i % 3 == 0 { 400i16 } else { 0 }, -508, 0] {
                    b.extend_from_slice(&m.to_le_bytes());
                }
                b.extend_from_slice(&[(i % 4) as u8, (i % 90 == 0) as u8]);
                p.extend_from_slice(&23u32.to_le_bytes());
                p.extend_from_slice(&1u16.to_le_bytes());
                p.extend_from_slice(&b);
            }
            if i % 50 == 7 {
                p.extend_from_slice(&3u32.to_le_bytes());
                p.extend_from_slice(&1u16.to_le_bytes());
                p.extend_from_slice(&[1, 2, 3]);
            }
            src.push(if i % 3 == 0 { 13 } else { 0 });
            src.push(3);
            src.extend_from_slice(&0u32.to_le_bytes());
            src.extend_from_slice(&(p.len() as u32).to_le_bytes());
            src.extend_from_slice(&p);
        }
        src.extend(demo());
        for threads in [1, 3] {
            let (body, ft) = coded(&src);
            let h = parse_header(&body, src.len()).unwrap();
            let uc = h.ents.iter().find(|e| e.0 == UC).unwrap().1;
            assert!(uc >= (300 + 9) * UC_MIN_SYMS, "{uc}");
            let dst = decoded(&body, &h, src.len(), Some(&ft), threads);
            assert!(dst == src, "{threads}");
        }
        // without usercmd blocks neither stream is in the table
        let plain = demo();
        let (body, _) = coded(&plain);
        let h = parse_header(&body, plain.len()).unwrap();
        assert!(h.ents.iter().all(|e| e.0 != UC && e.0 != UC_A));
    }

    /// Models a models block over its limit would hold are dropped,
    /// largest header first; a segment with one of their streams is then
    /// not transform-coded.
    #[test]
    fn tables_limit() {
        let src = demo();
        let (wk, _) = walk(&src, Proto::default()).unwrap();
        let nums = numerics(&src, &wk, 1);
        let mut models = train(&[(&wk, &nums), (&wk, &nums)]);
        let ids = |m: &[Option<rans::Shared>]| -> Vec<usize> {
            (0..m.len()).filter(|&i| m[i].is_some()).collect()
        };
        let all = ids(&models);
        assert!(all.iter().any(|&sid| is_num(sid)) && all.iter().any(|&sid| is_lazy(sid)));
        let len = tables_body(&models, &all).len() as u64;
        let hdr = |sid: usize| models[sid].as_ref().unwrap().hdr.len();
        let big = *all.iter().max_by_key(|&&sid| (hdr(sid), sid)).unwrap();
        fit_tables(&mut models, len);
        assert_eq!(ids(&models), all);
        fit_tables(&mut models, len - 1);
        let kept = ids(&models);
        assert_eq!(
            kept,
            all.iter()
                .copied()
                .filter(|&s| s != big)
                .collect::<Vec<_>>()
        );
        assert!(
            encode(&wk, &nums, 1, &mut Vec::new(), &models).is_none(),
            "a stream without a model: no transform body"
        );
        // nothing fits: no model is kept
        fit_tables(&mut models, 0);
        assert!(ids(&models).is_empty());
    }
}
