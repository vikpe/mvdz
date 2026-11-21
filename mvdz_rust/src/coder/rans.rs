//! Tier F: two-pass static rANS with per-context frequency tables.
//!
//! * Symbols: bytes for structural streams (context = the walker's context,
//!   which fits in 16 bits, the table key), or the sign/bit-length bucket of
//!   a residual for numeric streams (context from [`num_ctx`]); numeric
//!   mantissa bits go raw into a separate bit stream (pcodec-style bins +
//!   offsets).
//! * Tables: the encoder counts symbols per context and gives a context its
//!   own table only where that beats the shared fallback table by more than
//!   the table's cost. Counts are sent log-quantised (two steps per octave)
//!   with an adaptive binary coder and normalised to 2^SCALE identically on
//!   both sides (integer arithmetic only).
//! * Coder: 32-bit rANS, 16-bit renormalisation, interleaved states (after
//!   Giesen, "Interleaved entropy coders", 2014; written fresh).
//!   Single-symbol tables cost nothing and are skipped.

use super::rc::{Dec as RcDec, Enc as RcEnc, IntModel};
use crate::budget::{Budget, Charge, with_capacity};
use crate::error::{Error, Result};
use crate::numeric::NCtx;
use crate::streams::{N_STREAMS, PE_OX, PI_AP, PI_OX, SND_O, TE_O, UC_A, is_lazy, is_num};
use crate::util::{Rd, approx_f64, par_map, put_u32};
use std::sync::Arc;

pub(crate) const SCALE: u32 = 12;
const T: u32 = 1 << SCALE;
const L: u32 = 1 << 16;
const NSTATE: usize = 4;

/// Steps per octave of the log-quantised counts.
const STEPS: u32 = 2;
/// 2^(k/2) in 16.16 fixed point, k = 0, 1.
const POW: [u64; STEPS as usize] = [65536, 92682];

/// Reconstructed weight (16.16 fixed point) of a log-quantised count.
#[inline]
fn qval(q: u32) -> u64 {
    if q == 0 {
        return 0;
    }
    let e = q - 1;
    let (o, k) = (e / STEPS, e % STEPS);
    let frac = POW[k as usize];
    if o >= 40 { u64::MAX >> 8 } else { frac << o }
}

/// Log-quantised count: the q whose weight is nearest to c in ratio.
fn qlog(c: u32) -> u32 {
    if c == 0 {
        return 0;
    }
    let x = u64::from(c) << 16;
    let o = c.ilog2();
    let mut e = o * STEPS;
    for k in 0..STEPS {
        if qval(1 + o * STEPS + k) <= x {
            e = o * STEPS + k;
        }
    }
    let lo = qval(1 + e);
    let hi = qval(2 + e);
    // compare x/lo with hi/x  <=>  x*x vs lo*hi
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "x*x against lo*hi is meant"
    )]
    if u128::from(x) * u128::from(x) > u128::from(lo) * u128::from(hi) {
        2 + e
    } else {
        1 + e
    }
}

/// Normalises weights to a total of exactly T, every present symbol >= 1.
fn normalize(w: &[u64]) -> Vec<u32> {
    let tot: u128 = w.iter().map(|&x| u128::from(x)).sum();
    let mut f = vec![0u32; w.len()];
    if tot == 0 {
        return f;
    }
    let mut sum: i64 = 0;
    for (i, &x) in w.iter().enumerate() {
        if x > 0 {
            f[i] = ((u128::from(x) * u128::from(T) / tot) as u32).max(1);
            sum += i64::from(f[i]);
        }
    }
    let mut diff = i64::from(T) - sum;
    while diff != 0 {
        // adjust the largest entry (first one on ties)
        let mut bi = 0;
        for i in 0..f.len() {
            if f[i] > f[bi] {
                bi = i;
            }
        }
        if diff > 0 {
            f[bi] += diff.cast_unsigned() as u32;
            diff = 0;
        } else {
            let take = (-diff)
                .min(i64::from(f[bi]) - 1)
                .min((i64::from(f[bi]) + 1) / 2);
            if take <= 0 {
                break;
            }
            f[bi] -= take.cast_unsigned() as u32;
            diff += take;
        }
    }
    f
}

#[derive(Clone)]
struct Table {
    q: Vec<u32>,
    freq: Vec<u32>,
    cum: Vec<u32>,
    det: Option<u16>,
}

impl Table {
    fn from_q(q: Vec<u32>) -> Self {
        let w: Vec<u64> = q.iter().map(|&x| qval(x)).collect();
        let freq = normalize(&w);
        let mut cum = vec![0u32; freq.len() + 1];
        for i in 0..freq.len() {
            cum[i + 1] = cum[i] + freq[i];
        }
        let present: Vec<usize> = (0..freq.len()).filter(|&i| freq[i] > 0).collect();
        let det = if present.len() == 1 {
            Some(present[0] as u16)
        } else {
            None
        };
        Self { q, freq, cum, det }
    }
}

fn entropy_bits(c: &[u32]) -> f64 {
    let n: u64 = c.iter().map(|&x| u64::from(x)).sum();
    if n == 0 {
        return 0.0;
    }
    let ln = approx_f64(n).log2();
    c.iter()
        .filter(|&&x| x > 0)
        .map(|&x| f64::from(x) * (ln - f64::from(x).log2()))
        .sum()
}

fn cross_bits(c: &[u32], p: &[u32]) -> f64 {
    let n: u64 = p.iter().map(|&x| u64::from(x)).sum();
    let ln = approx_f64(n.max(1)).log2();
    let mut b = 0.0;
    for (i, &x) in c.iter().enumerate() {
        if x > 0 {
            if p[i] == 0 {
                return f64::INFINITY;
            }
            b = f64::mul_add(f64::from(x), ln - f64::from(p[i]).log2(), b);
        }
    }
    b
}

struct Model {
    alpha: usize,
    /// Own contexts, ascending: `own[i]` codes with `tables[i + 1]`.
    own: Vec<u32>,
    map: Vec<u32>,
    tables: Vec<Table>,
}

/// A stream's counts and the contexts that get their own table.
struct Counts {
    alpha: usize,
    own: Vec<u32>,
    own_rows: Vec<u32>,
    rest: Vec<u32>,
}

/// Per-context symbol counts of a stream, summed over any number of symbol
/// lists (the segments of a file, for its shared model).
pub struct Tally {
    alpha: usize,
    row_of: Vec<u32>,
    keys: Vec<u32>,
    rows: Vec<u32>,
    global: Vec<u32>,
}

impl Tally {
    pub fn new(alpha: usize) -> Self {
        Self {
            alpha,
            row_of: Vec::new(),
            keys: Vec::new(),
            rows: Vec::new(),
            global: vec![0; alpha],
        }
    }

    /// Counts the symbols `syms` of a stream, each with its context in
    /// `ctxs`.
    pub fn add(&mut self, syms: &[u8], ctxs: &[u16]) {
        let alpha = self.alpha;
        let maxc = ctxs.iter().copied().max().unwrap_or(0) as usize;
        // row_of, is_own and the model's map are dense arrays sized by the
        // largest context; contexts are 16-bit by spec (§6.12).
        if self.row_of.len() <= maxc {
            self.row_of.resize(maxc + 1, u32::MAX);
        }
        let (row_of, rows, keys, global) = (
            &mut self.row_of,
            &mut self.rows,
            &mut self.keys,
            &mut self.global,
        );
        for (&s, &c) in syms.iter().zip(ctxs) {
            let c = u32::from(c);
            let r = row_of[c as usize];
            let r = if r == u32::MAX {
                let r = keys.len() as u32;
                row_of[c as usize] = r;
                keys.push(c);
                rows.resize(rows.len() + alpha, 0);
                r
            } else {
                r
            };
            rows[r as usize * alpha + s as usize] += 1;
            global[s as usize] += 1;
        }
    }

    /// The model of these counts (a stream's in every segment of a file).
    /// A model whose header breaks the decision limit (rule M2) is made
    /// smaller until it holds: the own contexts with the fewest symbols
    /// join the fallback, an eighth of them at a time. None if even the
    /// fallback table alone breaks the limit.
    pub fn shared(&self) -> Option<Shared> {
        let mut counts = select(self);
        loop {
            let model = build_model(&counts);
            if let Some(hdr) = header(&model) {
                return Some(Shared { model, hdr });
            }
            if counts.own.is_empty() {
                return None;
            }
            counts.fold_smallest(counts.own.len().div_ceil(8));
        }
    }
}

impl Counts {
    /// Moves the `n` own contexts with the fewest symbols (ties: the
    /// highest context first) into the fallback's counts.
    fn fold_smallest(&mut self, n: usize) {
        let alpha = self.alpha;
        let row = |i: usize| &self.own_rows[i * alpha..(i + 1) * alpha];
        let mut order: Vec<usize> = (0..self.own.len()).collect();
        order.sort_by_key(|&i| {
            (
                row(i).iter().map(|&x| u64::from(x)).sum::<u64>(),
                std::cmp::Reverse(i),
            )
        });
        let mut drop = vec![false; self.own.len()];
        for &i in order.iter().take(n) {
            drop[i] = true;
        }
        let mut own = Vec::new();
        let mut own_rows = Vec::new();
        for i in 0..self.own.len() {
            if drop[i] {
                for (r, &c) in self.rest.iter_mut().zip(row(i)) {
                    *r += c;
                }
            } else {
                own.push(self.own[i]);
                own_rows.extend_from_slice(row(i));
            }
        }
        self.own = own;
        self.own_rows = own_rows;
    }
}

/// The tables of a model of these counts: own contexts and the fallback's
/// counts.
fn select(t: &Tally) -> Counts {
    const TABLE_BITS_BASE: f64 = 16.0;
    const TABLE_BITS_PER_SYM: f64 = 5.0;
    let (alpha, row_of, rows, keys) = (t.alpha, &t.row_of, &t.rows, &t.keys);
    let row = |k: u32| &rows[row_of[k as usize] as usize * alpha..][..alpha];
    // selection order is part of the output: most frequent context first
    let totals: Vec<u32> = keys.iter().map(|&k| row(k).iter().sum()).collect();
    let mut order: Vec<usize> = (0..keys.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(totals[i]), keys[i]));
    let keys: Vec<u32> = order.iter().map(|&i| keys[i]).collect();
    let mut own: Vec<u32> = Vec::new();
    let mut fb = t.global.clone();
    for pass in 0..2 {
        own.clear();
        let mut rest = vec![0u32; alpha];
        for &k in &keys {
            let c = row(k);
            let present = approx_f64(c.iter().filter(|&&x| x > 0).count() as u64);
            let own_cost = TABLE_BITS_PER_SYM.mul_add(present, entropy_bits(c) + TABLE_BITS_BASE);
            if own_cost < cross_bits(c, &fb) {
                own.push(k);
            } else {
                for i in 0..alpha {
                    rest[i] += c[i];
                }
            }
        }
        if pass == 0 {
            if rest.iter().all(|&x| x == 0) {
                break;
            }
            fb = rest;
        }
    }
    own.sort_unstable();
    let mut is_own = vec![false; row_of.len()];
    for &k in &own {
        is_own[k as usize] = true;
    }
    let mut rest = vec![0u32; alpha];
    for &k in &keys {
        if !is_own[k as usize] {
            let c = row(k);
            for i in 0..alpha {
                rest[i] += c[i];
            }
        }
    }
    let mut own_rows = Vec::with_capacity(own.len() * alpha);
    for &k in &own {
        own_rows.extend_from_slice(row(k));
    }
    Counts {
        alpha,
        own,
        own_rows,
        rest,
    }
}

fn build_model(c: &Counts) -> Model {
    let alpha = c.alpha;
    let mut tables = vec![Table::from_q(c.rest.iter().map(|&n| qlog(n)).collect())];
    let maxk = c.own.last().map_or(0, |&k| k as usize + 1);
    let mut map = vec![0u32; maxk];
    for (i, &k) in c.own.iter().enumerate() {
        tables.push(Table::from_q(
            c.own_rows[i * alpha..(i + 1) * alpha]
                .iter()
                .map(|&n| qlog(n))
                .collect(),
        ));
        map[k as usize] = (i + 1) as u32;
    }
    Model {
        alpha,
        own: c.own.clone(),
        map,
        tables,
    }
}

// --------------------------------------------------------------- model I/O

const NQC: usize = 17 * 18;

fn write_table(e: &mut RcEnc, m: &mut IntModel, t: &[u32], fb: Option<&[u32]>) {
    let mut prev = 0u32;
    for (i, &q) in t.iter().enumerate() {
        let f = fb.map_or(17, |f| f[i].min(16)) as usize;
        m.put(e, f * 17 + prev.min(16) as usize, q);
        prev = q;
    }
}

fn read_table(d: &mut RcDec, m: &mut IntModel, alpha: usize, fb: Option<&[u32]>) -> Vec<u32> {
    let mut t = vec![0u32; alpha];
    let mut prev = 0u32;
    for i in 0..alpha {
        let f = fb.map_or(17, |f| f[i].min(16)) as usize;
        t[i] = m.get(d, f * 17 + prev.min(16) as usize).min(160);
        prev = t[i];
    }
    t
}

/// The serialized model header and the binary decisions it takes.
fn write_model(m: &Model) -> (Vec<u8>, u64) {
    let mut e = RcEnc::new();
    let mut im = IntModel::new(NQC + 8);
    im.put(&mut e, NQC, m.alpha as u32);
    im.put(&mut e, NQC + 1, (m.tables.len() - 1) as u32);
    let fb = m.tables[0].q.clone();
    write_table(&mut e, &mut im, &fb, None);
    let mut prevk = 0u32;
    for (i, &k) in m.own.iter().enumerate() {
        im.put(&mut e, NQC + 2, if i == 0 { k } else { k - prevk });
        prevk = k;
        write_table(&mut e, &mut im, &m.tables[i + 1].q, Some(&fb));
    }
    let n = e.decisions;
    (e.finish(), n)
}

/// Tables with at most this many symbols are decoded by searching their
/// cumulative frequencies (2 bytes per symbol); wider ones use a slot LUT.
const SEARCH_MAX: usize = 64;

// Context-map entries: an offset into `pool` (search table), or a LUT
// offset with E_LUT, or a deterministic symbol with E_DET, or empty.
const E_DET: u32 = 1 << 31;
const E_LUT: u32 = 1 << 30;
const E_EMPTY: u32 = E_DET | E_LUT;

/// Format limit on the binary decisions a model header may take:
/// `DEC_PER_BYTE * header length + DEC_BASE` (FORMAT.md, rule M2).
pub(crate) const DEC_PER_BYTE: u64 = 128;
pub(crate) const DEC_BASE: u64 = 1024;

/// Decoder-side model. A search table at pool offset o is
/// `[0, c1, .., cn = T, pad..]` (16*chunks + 1 entries) followed by its
/// 16*chunks symbols, so one table is one or two cache lines.
pub(crate) struct DModel {
    map: Vec<u32>,
    fb: u32,
    chunks: usize,
    pool: Vec<u16>,
    luts: Vec<u32>,
    _charge: Option<Charge>,
}

/// Number of entries of `table[..16 * chunks]` that are <= `slot` (below `T`).
/// ponytail: about 6 % slower decode than the SSE2 search it replaced, which needs
/// `unsafe`; `std::simd` once stable.
#[inline(always)]
fn count_le(table: &[u16], chunks: usize, slot: u32) -> usize {
    let slot = slot as u16;
    // one counter per lane: the compiler vectorizes this form
    let mut counts = [0u16; 16];
    for chunk in table[..16 * chunks].as_chunks::<16>().0 {
        for lane in 0..16 {
            counts[lane] += u16::from(chunk[lane] <= slot);
        }
    }
    counts.iter().map(|&count| usize::from(count)).sum()
}

/// A model header's own tables' contexts and q values of every
/// table (the fallback first), and their budget charge.
type Header = (Vec<u32>, Vec<u8>, Charge);

/// Reads a model header (FORMAT.md, "Model header"). `alpha` is the
/// alphabet the stream kind requires.
fn read_header(b: &[u8], alpha_req: usize, budget: &Budget) -> Result<Header> {
    let bad = Error::Corrupt("model");
    let max_dec = (b.len() as u64)
        .saturating_mul(DEC_PER_BYTE)
        .saturating_add(DEC_BASE);
    let mut d = RcDec::new(b);
    let mut im = IntModel::new(NQC + 8);
    let alpha = im.get(&mut d, NQC) as usize;
    let n = im.get(&mut d, NQC + 1) as usize;
    if alpha != alpha_req || n > 65535 {
        return Err(bad);
    }
    // the log-quantised tables (u8: q <= 160) until the layout is known
    let charge = budget.alloc(((n + 1) * alpha + 4 * (n + 1)) as u64)?;
    let mut qs: Vec<u8> = with_capacity((n + 1) * alpha)?;
    let mut keys: Vec<u32> = with_capacity(n)?;
    let fbq = read_table(&mut d, &mut im, alpha, None);
    qs.extend(fbq.iter().map(|&q| q as u8));
    let mut k = 0u32;
    for i in 0..n {
        if d.decisions > max_dec {
            return Err(bad);
        }
        let dk = im.get(&mut d, NQC + 2);
        if i > 0 && dk == 0 {
            return Err(bad);
        }
        k = k.checked_add(dk).ok_or(bad)?;
        if k > 0xffff {
            return Err(bad);
        }
        let t = read_table(&mut d, &mut im, alpha, Some(&fbq));
        keys.push(k);
        qs.extend(t.iter().map(|&q| q as u8));
        if d.overrun() {
            return Err(bad);
        }
    }
    // the header is consumed exactly: the decoder reads one byte per
    // byte the encoder wrote (5 at the start, then one per renormalisation)
    if d.pos() != b.len() || d.decisions > max_dec {
        return Err(bad);
    }
    budget.work(d.decisions)?;
    Ok((keys, qs, charge))
}

/// Reads a model header and builds its decoder model.
pub(crate) fn read_model(b: &[u8], alpha: usize, budget: &Budget) -> Result<DModel> {
    let (keys, qs, _c) = read_header(b, alpha, budget)?;
    // a symbol is present (frequency >= 1) exactly when its q is non-zero
    // (`normalize`), so the layout is known before any table is built;
    // the tables are then built one at a time
    let present = |q: &[u8]| q.iter().filter(|&&x| x > 0).count();
    // one chunk count for every search table of the model: a fixed trip
    // count keeps the search loop free of mispredicted exits
    let chunks = qs
        .chunks(alpha)
        .map(present)
        .filter(|&n| n > 1 && n <= SEARCH_MAX)
        .max()
        .unwrap_or(1)
        .div_ceil(16);
    let (mut npool, mut nlut) = (0usize, 0usize);
    for q in qs.chunks(alpha) {
        let p = present(q);
        if p > SEARCH_MAX {
            nlut += T as usize;
        } else if p > 1 {
            npool += 2 * 16 * chunks + 1;
        }
    }
    let maxk = keys.last().map_or(0, |&k| k as usize + 1);
    let charge = budget.alloc((npool * 2 + nlut * 4 + maxk * 4) as u64)?;
    let mut dm = DModel {
        map: with_capacity(maxk)?,
        fb: E_EMPTY,
        chunks,
        pool: with_capacity(npool)?,
        luts: with_capacity(nlut)?,
        _charge: Some(charge),
    };
    let mut ents = with_capacity(keys.len() + 1)?;
    for q in qs.chunks(alpha) {
        let t = Table::from_q(q.iter().map(|&x| u32::from(x)).collect());
        let present: Vec<usize> = (0..alpha).filter(|&s| t.freq[s] > 0).collect();
        let e = if present.is_empty() {
            E_EMPTY
        } else if let Some(d) = t.det {
            E_DET | u32::from(d)
        } else if present.len() <= SEARCH_MAX {
            let off = dm.pool.len() as u32;
            dm.pool.push(0);
            for &s in &present {
                dm.pool.push(t.cum[s + 1] as u16);
            }
            for _ in present.len()..16 * chunks {
                dm.pool.push(0x7fff);
            }
            for &s in &present {
                dm.pool.push(s as u16);
            }
            for _ in present.len()..16 * chunks {
                dm.pool.push(0);
            }
            off
        } else {
            let off = dm.luts.len() as u32;
            dm.luts.resize(dm.luts.len() + T as usize, 0);
            let lut = &mut dm.luts[off as usize..];
            for &s in &present {
                for j in t.cum[s]..t.cum[s + 1] {
                    lut[j as usize] = s as u32 | t.freq[s] << 8 | (j - t.cum[s]) << 20;
                }
            }
            E_LUT | off
        };
        ents.push(e);
    }
    if dm.pool.len() >= E_LUT as usize || dm.luts.len() >= E_LUT as usize {
        return Err(Error::Limit("model size"));
    }
    dm.fb = ents[0];
    dm.map.resize(maxk, dm.fb);
    for (i, &k) in keys.iter().enumerate() {
        dm.map[k as usize] = ents[i + 1];
    }
    Ok(dm)
}

impl DModel {
    fn empty() -> Self {
        Self {
            map: Vec::new(),
            fb: E_EMPTY,
            chunks: 1,
            pool: Vec::new(),
            luts: Vec::new(),
            _charge: None,
        }
    }
    #[inline(always)]
    fn entry(&self, ctx: u32) -> u32 {
        let c = ctx as usize;
        if c < self.map.len() {
            self.map[c]
        } else {
            self.fb
        }
    }
    /// Decodes one symbol for map entry `e`; `None` for an empty table.
    #[inline(always)]
    fn decode(&self, d: &mut RDec, e: u32) -> Option<u32> {
        if e & E_DET != 0 {
            return if e & E_LUT != 0 {
                None
            } else {
                Some(e & 0xffff)
            };
        }
        let j = d.j;
        d.j = (j + 1) % NSTATE;
        let x = d.x[j];
        let slot = x & (T - 1);
        let (sym, mut x) = if e & E_LUT == 0 {
            let m = 16 * self.chunks;
            // in range: `read_model` points every search entry at 2*m + 1 pool entries
            let t = &self.pool[e as usize..=e as usize + 2 * m];
            let k = count_le(&t[1..], self.chunks, slot);
            let lo = u32::from(t[k]);
            let hi = u32::from(t[k + 1]);
            // wrapping: an initial state from a hostile file may overflow
            // (the result is then garbage that the stream's checks refuse)
            (
                u32::from(t[m + 1 + k]),
                (hi - lo).wrapping_mul(x >> SCALE).wrapping_add(slot - lo),
            )
        } else {
            let le = self.luts[(e & !E_LUT) as usize + slot as usize];
            (
                le & 0xff,
                ((le >> 8) & 0xfff)
                    .wrapping_mul(x >> SCALE)
                    .wrapping_add(le >> 20),
            )
        };
        if x < L {
            x = (x << 16) | u32::from(d.word());
        }
        d.x[j] = x;
        Some(sym)
    }
}

// --------------------------------------------------------------- rANS core

struct REnc {
    x: [u32; NSTATE],
    words: Vec<u16>,
}

impl REnc {
    fn new() -> Self {
        Self {
            x: [L; NSTATE],
            words: Vec::new(),
        }
    }
    #[inline]
    fn put(&mut self, j: usize, freq: u32, cum: u32) {
        let mut x = self.x[j];
        let xmax = ((L >> SCALE) << 16) * freq;
        if x >= xmax {
            self.words.push(x as u16);
            x >>= 16;
        }
        self.x[j] = ((x / freq) << SCALE) + (x % freq) + cum;
    }
    fn finish(mut self) -> Vec<u8> {
        for j in (0..NSTATE).rev() {
            let x = self.x[j];
            self.words.push(x as u16);
            self.words.push((x >> 16) as u16);
        }
        self.words.reverse();
        let mut out = Vec::with_capacity(self.words.len() * 2);
        for w in self.words {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }
}

struct RDec<'a> {
    x: [u32; NSTATE],
    b: &'a [u8],
    pos: usize,
    j: usize,
}

impl<'a> RDec<'a> {
    fn new(b: &'a [u8]) -> Self {
        let mut d = RDec {
            x: [0; NSTATE],
            b,
            pos: 0,
            j: 0,
        };
        for j in 0..NSTATE {
            let hi = u32::from(d.word());
            let lo = u32::from(d.word());
            d.x[j] = hi << 16 | lo;
        }
        d
    }
    #[inline(always)]
    fn word(&mut self) -> u16 {
        let v = self
            .b
            .get(self.pos..)
            .and_then(<[u8]>::first_chunk)
            .map_or(0, |s| u16::from_le_bytes(*s));
        self.pos += 2;
        v
    }
    /// The decoder of an absent stream (no symbol can be decoded).
    fn finished_empty() -> RDec<'static> {
        RDec {
            x: [L; NSTATE],
            b: &[],
            pos: 0,
            j: 0,
        }
    }
    /// Every word read and every state back at its initial value.
    fn finished(&self) -> bool {
        self.pos == self.b.len() && self.x.iter().all(|&x| x == L)
    }
}

fn rans_encode(model: &Model, syms: &[u8], ctxs: &[u16]) -> Vec<u8> {
    let tix: Vec<usize> = ctxs
        .iter()
        .map(|&c| model.map.get(c as usize).copied().unwrap_or(0) as usize)
        .collect();
    let mut coded = 0usize;
    let mut state_of = vec![0u8; syms.len()];
    for (i, &ti) in tix.iter().enumerate() {
        if model.tables[ti].det.is_none() {
            state_of[i] = (coded % NSTATE) as u8;
            coded += 1;
        }
    }
    let mut e = REnc::new();
    for i in (0..syms.len()).rev() {
        let t = &model.tables[tix[i]];
        if t.det.is_some() {
            continue;
        }
        let s = syms[i] as usize;
        e.put(state_of[i] as usize, t.freq[s], t.cum[s]);
    }
    e.finish()
}

/// Appends `rans_len u32 | rans | extra`: a stream body after its model
/// header.
fn put_body(out: &mut Vec<u8>, model: &Model, syms: &[u8], ctxs: &[u16], extra: &[u8]) {
    let body = rans_encode(model, syms, ctxs);
    out.reserve(body.len() + extra.len() + 4);
    put_u32(out, body.len());
    out.extend_from_slice(&body);
    out.extend_from_slice(extra);
}

/// A back end 3 stream body: no model header.
fn pack_body(model: &Model, syms: &[u8], ctxs: &[u16], extra: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    put_body(&mut out, model, syms, ctxs, extra);
    out
}

/// The model header, or None if it breaks the decision limit (rule M2).
fn header(model: &Model) -> Option<Vec<u8>> {
    let (hdr, decisions) = write_model(model);
    (decisions <= (hdr.len() as u64) * DEC_PER_BYTE + DEC_BASE).then_some(hdr)
}

/// A stream body: `rans_len u32 | rans | extra`.
struct Unpacked<'a> {
    rans: &'a [u8],
    extra: &'a [u8],
}

fn unpack(b: &[u8]) -> Result<Unpacked<'_>> {
    let mut r = Rd::new(b);
    let rans = r.bytes().ok_or(Error::Corrupt("rans stream"))?;
    Ok(Unpacked {
        rans,
        extra: r.rest(),
    })
}

// --------------------------------------------------------------- file models

/// A model built from one stream's symbols in every segment of a file
/// ([`Tally::shared`]): its TABLES entry (FORMAT.md §2.6), used by back
/// end 3.
pub struct Shared {
    model: Model,
    /// the model header
    pub(crate) hdr: Vec<u8>,
}

/// The decoder models of a file, by stream id.
pub(crate) type Models = Vec<Option<Arc<DModel>>>;

/// The models of a file (FORMAT.md §2.6): the models block is read once,
/// when the file is opened; that checks it and builds its models.
pub(crate) struct FileTables {
    len: u64,
    models: Models,
}

impl FileTables {
    /// Reads a models block's body: checks it and builds its models
    /// (charged to `budget`) on up to `threads` threads.
    pub(crate) fn new(body: &[u8], threads: usize, budget: &Budget) -> Result<Self> {
        Ok(Self {
            len: body.len() as u64,
            models: read_tables(body, threads, budget)?,
        })
    }

    /// The length of the body the models were read from.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    pub(crate) fn models(&self) -> &Models {
        &self.models
    }
}

/// Reads `n u32 | n x { sid u32 | len u32 | model }`: ids strictly increasing,
/// each model a structural or numeric stream's (§8.3, M1 without a count).
/// The models do not depend on each other: they are built in parallel.
fn read_tables(b: &[u8], threads: usize, budget: &Budget) -> Result<Models> {
    let e = Error::Corrupt("tables");
    let mut r = Rd::new(b);
    let n = r.u32z().ok_or(e)?;
    if n == 0 {
        return Err(e);
    }
    // (stream id, alphabet, model header)
    let mut ents: Vec<(usize, usize, &[u8])> = Vec::new();
    for _ in 0..n {
        let sid = r.u32z().ok_or(e)?;
        if ents.last().is_some_and(|p| sid <= p.0) {
            return Err(e);
        }
        let alpha = if is_lazy(sid) {
            256
        } else if is_num(sid) {
            NALPHA
        } else {
            return Err(e);
        };
        ents.push((sid, alpha, r.bytes().ok_or(e)?));
    }
    if !r.done() {
        return Err(e);
    }
    let built = par_map(threads, ents.len(), |i| {
        read_model(ents[i].2, ents[i].1, budget)
    });
    let mut m: Models = vec![None; N_STREAMS];
    for (ent, model) in ents.iter().zip(built) {
        m[ent.0] = Some(Arc::new(model?));
    }
    Ok(m)
}

// --------------------------------------------------------------- byte streams

#[inline(always)]
fn byte_ctx(c: u32) -> u32 {
    c & 0xffff
}

/// A structural stream coded with its model in the file, `file`.
pub fn encode_bytes(syms: &[u8], ctxs: &[u16], file: &Shared) -> Vec<u8> {
    pack_body(&file.model, syms, ctxs, &[])
}

pub(crate) struct ByteDec<'a> {
    m: Arc<DModel>,
    d: RDec<'a>,
    n: usize,
    used: usize,
}

impl<'a> ByteDec<'a> {
    pub(crate) fn empty() -> Self {
        ByteDec {
            m: Arc::new(DModel::empty()),
            d: RDec::finished_empty(),
            n: 0,
            used: 0,
        }
    }
    /// `file`: the stream's model in the file.
    pub(crate) fn new(b: &'a [u8], n: usize, file: &Arc<DModel>) -> Result<Self> {
        let u = unpack(b)?;
        if !u.extra.is_empty() {
            return Err(Error::Corrupt("rans stream"));
        }
        Ok(ByteDec {
            m: file.clone(),
            d: RDec::new(u.rans),
            n,
            used: 0,
        })
    }
    #[inline(always)]
    pub(crate) fn get(&mut self, ctx: u32) -> Option<u8> {
        self.used += 1;
        let e = self.m.entry(byte_ctx(ctx));
        self.m.decode(&mut self.d, e).map(|v| v as u8)
    }
    /// Exactly the declared count read and the coded data consumed.
    pub(crate) fn done(&self) -> bool {
        self.used == self.n && self.d.finished()
    }
}

// --------------------------------------------------------------- numeric streams

pub(crate) const NALPHA: usize = 65;

/// Static-table context of a numeric value, per stream.
#[inline(always)]
pub(crate) fn num_ctx(sid: usize, c: &NCtx) -> u32 {
    let f = &c.f;
    let cl = |v: u8, m: u8| u32::from(v.min(m));
    match sid {
        PI_OX..=50 => cl(f[0], 19) | cl(f[1], 19) << 5,
        PI_AP..=53 => cl(f[0], 23) | cl(f[1], 23) << 5,
        PE_OX..=56 => cl(f[0], 17) | cl(f[1], 17) << 5 | u32::from(f[3] & 1) << 10,
        SND_O | TE_O => u32::from(f[0]) | u32::from(f[1]) << 4,
        UC_A => cl(f[0], 23) | cl(f[1], 23) << 5 | cl(f[2], 15) << 10 | u32::from(f[3]) << 14,
        _ => cl(f[0], 63) | u32::from(f[1]) << 6,
    }
}

#[inline(always)]
fn bucket(v: i32) -> (u16, u32, u32) {
    let m = v.unsigned_abs();
    if m == 0 {
        return (0, 0, 0);
    }
    let n = m.bit_width();
    let b = 2 * n - 1 + u32::from(v < 0);
    (b as u16, n - 1, m & ((1u32 << (n - 1)) - 1))
}

#[derive(Default)]
pub(crate) struct BitW {
    acc: u64,
    n: u32,
    out: Vec<u8>,
}

impl BitW {
    #[inline]
    pub(crate) fn put(&mut self, v: u32, n: u32) {
        self.acc |= u64::from(v) << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

pub(crate) struct BitR<'a> {
    b: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
    /// bits handed out so far
    used: u64,
}

impl<'a> BitR<'a> {
    pub(crate) fn new(b: &'a [u8]) -> Self {
        BitR {
            b,
            pos: 0,
            acc: 0,
            n: 0,
            used: 0,
        }
    }
    #[inline(always)]
    fn refill(&mut self) {
        if let Some(word) = self.b.get(self.pos..).and_then(<[u8]>::first_chunk) {
            let w = u64::from_le_bytes(*word);
            self.acc |= w << self.n;
            let take = (63 - self.n) / 8;
            self.pos += take as usize;
            self.n += take * 8;
        } else {
            while self.n <= 56 {
                let v = u64::from(self.b.get(self.pos).copied().unwrap_or(0));
                self.acc |= v << self.n;
                self.pos += 1;
                self.n += 8;
            }
        }
    }
    /// `n` bits (n <= 31), least significant first; zeros past the end.
    #[inline(always)]
    pub(crate) fn get(&mut self, n: u32) -> u32 {
        if self.n < n {
            self.refill();
        }
        let v = (self.acc & ((1u64 << n) - 1)) as u32;
        self.acc >>= n;
        self.n -= n;
        self.used += u64::from(n);
        v
    }
    /// The bits read end within the last byte (at most 7 padding bits).
    pub(crate) fn finished(&self) -> bool {
        self.used.div_ceil(8) == self.b.len() as u64
    }
}

/// The values of a numeric stream as they are coded: the bucket of each
/// value (its symbol), its context, and the mantissa bits of all of them.
pub(crate) struct NumSyms {
    pub(crate) syms: Vec<u8>,
    pub(crate) ctxs: Vec<u16>,
    pub(crate) extra: Vec<u8>,
}

/// Collects the values of numeric stream `sid` as [`NumSyms`].
pub(crate) struct NumSymW {
    sid: usize,
    syms: Vec<u8>,
    ctxs: Vec<u16>,
    bits: BitW,
}

impl NumSymW {
    pub(crate) fn new(sid: usize) -> Self {
        Self {
            sid,
            syms: Vec::new(),
            ctxs: Vec::new(),
            bits: BitW::default(),
        }
    }
    #[inline(always)]
    pub(crate) fn push(&mut self, c: NCtx, v: i32) {
        let (b, n, m) = bucket(v);
        let ctx = num_ctx(self.sid, &c);
        debug_assert!(
            ctx <= 0xffff,
            "stream {}: context {ctx:#x} over 16 bits",
            self.sid
        );
        self.syms.push(b as u8);
        self.ctxs.push(ctx as u16);
        self.bits.put(m, n);
    }
    pub(crate) fn finish(self) -> NumSyms {
        NumSyms {
            syms: self.syms,
            ctxs: self.ctxs,
            extra: self.bits.finish(),
        }
    }
}

/// A numeric stream coded with its model in the file, `file`.
pub(crate) fn encode_num(num: &NumSyms, file: &Shared) -> Vec<u8> {
    pack_body(&file.model, &num.syms, &num.ctxs, &num.extra)
}

pub(crate) struct NumDec<'a> {
    sid: usize,
    m: Arc<DModel>,
    d: RDec<'a>,
    bits: BitR<'a>,
    n: usize,
    used: usize,
}

impl<'a> NumDec<'a> {
    pub(crate) fn empty() -> Self {
        NumDec {
            sid: 0,
            m: Arc::new(DModel::empty()),
            d: RDec::finished_empty(),
            bits: BitR::new(&[]),
            n: 0,
            used: 0,
        }
    }
    /// `file`: the stream's model in the file.
    pub(crate) fn new(sid: usize, b: &'a [u8], n: usize, file: &Arc<DModel>) -> Result<Self> {
        let u = unpack(b)?;
        Ok(NumDec {
            sid,
            m: file.clone(),
            d: RDec::new(u.rans),
            bits: BitR::new(u.extra),
            n,
            used: 0,
        })
    }
    #[inline(always)]
    pub(crate) fn get(&mut self, c: NCtx) -> Option<i32> {
        self.used += 1;
        let e = self.m.entry(num_ctx(self.sid, &c));
        let b = self.m.decode(&mut self.d, e)?;
        if b > 64 {
            return None;
        }
        // branchless: b = 0 reads no bits and gives 0
        let n = b.div_ceil(2);
        let nb = n.saturating_sub(1);
        let lead = u32::from(n > 0) << nb;
        let m = (lead | self.bits.get(nb)).cast_signed();
        Some(if b & 1 == 0 { m.wrapping_neg() } else { m })
    }
    /// Exactly the declared count read and every byte consumed.
    pub(crate) fn done(&self) -> bool {
        self.used == self.n && self.d.finished() && self.bits.finished()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context as _;

    /// The shared model of `syms` and its decoder model.
    fn models(syms: &[u8], ctxs: &[u16], alpha: usize) -> anyhow::Result<(Shared, Arc<DModel>)> {
        let mut t = Tally::new(alpha);
        t.add(syms, ctxs);
        let sh = t.shared().context("a model")?;
        let dm = read_model(&sh.hdr, alpha, &Budget::unlimited())?;
        Ok((sh, Arc::new(dm)))
    }

    #[test]
    fn count_le_counts_entries_up_to_slot() {
        let table: Vec<u16> = (0..64u16).map(|entry| entry * 70).collect();
        for chunks in 1..=4 {
            for slot in [0, 1, 69, 70, 71, 2000, 4095] {
                let expected = table[..16 * chunks]
                    .iter()
                    .filter(|&&entry| u32::from(entry) <= slot)
                    .count();
                assert_eq!(count_le(&table, chunks, slot), expected, "{chunks} {slot}");
            }
        }
    }

    #[test]
    fn byte_round_trip_and_limits() -> anyhow::Result<()> {
        // three contexts with different distributions: own tables
        let ctxs: Vec<u16> = (0..5000u16).map(|i| i % 3).collect();
        let syms: Vec<u8> = (0..5000u32)
            .map(|i| {
                [(i * 7 % 13) as u8, 100 + (i % 5) as u8, 200 + (i % 2) as u8][(i % 3) as usize]
            })
            .collect();
        let (sh, dm) = models(&syms, &ctxs, 256)?;
        let b = encode_bytes(&syms, &ctxs, &sh);
        let mut d = ByteDec::new(&b, syms.len(), &dm)?;
        for (i, &s) in syms.iter().enumerate() {
            assert_eq!(d.get(u32::from(ctxs[i])), Some(s));
        }
        assert!(d.done());
        // the model of a byte stream is refused as a numeric stream's
        assert!(read_model(&sh.hdr, NALPHA, &Budget::unlimited()).is_err());
        // a body cut short is refused; a symbol short does not finish
        assert!(ByteDec::new(&b[..b.len() - 2], syms.len(), &dm).is_err());
        let mut d = ByteDec::new(&b, syms.len(), &dm)?;
        for i in 0..syms.len() - 1 {
            d.get(u32::from(ctxs[i]));
        }
        assert!(!d.done());
        Ok(())
    }

    /// A model whose header would break the decision limit (thousands of
    /// own tables of one symbol each cost almost no bytes) is made smaller
    /// until it holds, and still codes the stream.
    #[test]
    fn model_over_the_decision_limit_shrinks() -> anyhow::Result<()> {
        let n = 60_000u32;
        let ctxs: Vec<u16> = (0..n).map(|i| (i % 3000) as u16).collect();
        let syms: Vec<u8> = ctxs.iter().map(|&c| (c % 4) as u8).collect();
        let mut t = Tally::new(256);
        t.add(&syms, &ctxs);
        let full = build_model(&select(&t));
        assert!(header(&full).is_none(), "the full model breaks rule M2");
        let (sh, dm) = models(&syms, &ctxs, 256)?;
        assert!(sh.model.own.len() < full.own.len());
        let b = encode_bytes(&syms, &ctxs, &sh);
        let mut d = ByteDec::new(&b, syms.len(), &dm)?;
        for (i, &s) in syms.iter().enumerate() {
            assert_eq!(d.get(u32::from(ctxs[i])), Some(s));
        }
        assert!(d.done());
        Ok(())
    }

    #[test]
    fn numeric_round_trip() -> anyhow::Result<()> {
        let vals: Vec<(NCtx, i32)> = (0..3000i32)
            .map(|i| {
                (
                    NCtx {
                        f: [(i % 5).cast_unsigned() as u8, 0, 0, 0, 0, 0],
                    },
                    (i * 37) % 2001 - 1000,
                )
            })
            .collect();
        let mut w = NumSymW::new(PI_OX);
        for (c, v) in &vals {
            w.push(*c, *v);
        }
        let num = w.finish();
        let (sh, dm) = models(&num.syms, &num.ctxs, NALPHA)?;
        let b = encode_num(&num, &sh);
        let mut d = NumDec::new(PI_OX, &b, vals.len(), &dm)?;
        for &(c, v) in &vals {
            assert_eq!(d.get(c), Some(v));
        }
        assert!(d.done());
        Ok(())
    }
}
