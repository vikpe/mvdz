//! Numeric groups: predictors and side contexts for the numeric fields.
//!
//! Each group walks one record list (written by the walker) in order,
//! predicts each numeric field from group-local state, and codes the
//! residual through a [`NumIO`]. The same code runs in both directions: on
//! encode it reads the value from the input buffer, on decode it writes the
//! reconstructed value into the output buffer at the recorded offset.
//! Groups in wave 1 are independent of each other; wave 2 (sounds, temp
//! entities) reads player and entity origins that wave 1 wrote.

use crate::streams::{PE_F0, PE_OX, PI_AP, PI_F0, PI_OX, SND_O, TE_O, UC_A};
use crate::walk::{NONE, PE_FLOAT, PE_INPREV, PeRec, PiRec, Recs};

/// Side features of one numeric value. Backends pick which to use.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
#[repr(C)]
pub(crate) struct NCtx {
    pub(crate) f: [u8; 6],
}

pub(crate) trait NumIO {
    /// Encode: record `v` on local stream `k`, return it. Decode: return the
    /// decoded residual.
    fn code(&mut self, k: usize, ctx: NCtx, v: i32) -> i32;
}

/// Shared view of the segment buffer.
///
/// On encode it is the input (never
/// written); on decode the output. Groups running in parallel write
/// disjoint byte ranges: every offset comes from the walker's monotonic
/// cursor, so no two records share a byte.
#[derive(Clone, Copy)]
pub(crate) struct Buf {
    ptr: *mut u8,
    len: usize,
}
unsafe impl Send for Buf {}
unsafe impl Sync for Buf {}

impl Buf {
    pub(crate) fn new(b: &mut [u8]) -> Self {
        Self {
            ptr: b.as_mut_ptr(),
            len: b.len(),
        }
    }
    pub(crate) fn ro(b: &[u8]) -> Self {
        Self {
            ptr: b.as_ptr().cast_mut(),
            len: b.len(),
        }
    }
    pub(crate) fn ptr(&self) -> *mut u8 {
        self.ptr
    }
    #[allow(clippy::len_without_is_empty)]
    pub(crate) fn len(&self) -> usize {
        self.len
    }
    #[inline(always)]
    pub(crate) fn rd16(&self, off: u32) -> u16 {
        let o = off as usize;
        if o.wrapping_add(2) <= self.len && o < self.len {
            unsafe { u16::from_le_bytes([*self.ptr.add(o), *self.ptr.add(o + 1)]) }
        } else {
            0
        }
    }
    #[inline(always)]
    pub(crate) fn rd32(&self, off: u32) -> u32 {
        let o = off as usize;
        if o.wrapping_add(4) <= self.len && o < self.len {
            unsafe {
                (self.ptr.add(o) as *const [u8; 4])
                    .read_unaligned()
                    .map(|x| x)
                    .into_le()
            }
        } else {
            0
        }
    }
    #[inline(always)]
    pub(crate) fn wr16(&self, off: u32, v: u16) {
        let o = off as usize;
        if o.wrapping_add(2) <= self.len && o < self.len {
            unsafe {
                let b = v.to_le_bytes();
                *self.ptr.add(o) = b[0];
                *self.ptr.add(o + 1) = b[1];
            }
        }
    }
    #[inline(always)]
    pub(crate) fn wr32(&self, off: u32, v: u32) {
        let o = off as usize;
        if o.wrapping_add(4) <= self.len && o < self.len {
            unsafe {
                self.ptr
                    .add(o)
                    .cast::<[u8; 4]>()
                    .write_unaligned(v.to_le_bytes());
            }
        }
    }
}

trait IntoLe {
    fn into_le(self) -> u32;
}
impl IntoLe for [u8; 4] {
    #[inline(always)]
    fn into_le(self) -> u32 {
        u32::from_le_bytes(self)
    }
}

/// Sign and bit-length bucket of a residual: 0 for zero, else
/// 2*bitlen(|v|) - 1 + sign (0..=64).
#[inline(always)]
pub(crate) fn q(v: i32) -> u8 {
    if v == 0 {
        return 0;
    }
    let neg = u32::from(v < 0);
    let m = v.unsigned_abs();
    let n = m.bit_width();
    (2 * n - 1 + neg) as u8
}

/// floor(2^40 / d) + 1: n * RECIP[d] >> 40 == n / d exactly for
/// 0 <= n < 2^31, 1 <= d <= 255 (the error n/2^40 < 2^-9 < 1/d).
static RECIP: [u64; 256] = {
    let mut t = [0u64; 256];
    let mut d = 1;
    while d < 256 {
        t[d] = (1u64 << 40) / d as u64 + 1;
        d += 1;
    }
    t
};

/// Rounded division a / b for b > 0 (halves away from zero).
#[inline(always)]
fn div_round(a: i32, b: i32) -> i32 {
    let n = a.unsigned_abs() + b.cast_unsigned() / 2;
    let q = if b < 256 {
        ((u128::from(n) * u128::from(RECIP[b.cast_unsigned() as usize])) >> 40) as u32
    } else {
        n / b.cast_unsigned()
    }
    .cast_signed();
    if a < 0 { -q } else { q }
}

/// Frame intervals are clamped so the scaled velocity fits 32 bits.
pub(crate) const DT_MAX: i32 = 32767;

/// dt-scaled linear prediction: last + (last - prev) * `dt_now` / `dt_prev`,
/// rounded (wrapping 16-bit arithmetic). Intervals are clamped to `DT_MAX`.
#[inline(always)]
fn pred_dt(last: u16, prev: u16, dt_now: i32, dt_prev: i32) -> u16 {
    let d = i32::from((last.wrapping_sub(prev)).cast_signed());
    if dt_now == dt_prev || dt_prev <= 0 || dt_now < 0 {
        last.wrapping_add(d.cast_unsigned() as u16)
    } else {
        last.wrapping_add(
            div_round(d * dt_now.min(DT_MAX), dt_prev.min(DT_MAX)).cast_unsigned() as u16,
        )
    }
}

#[inline(always)]
fn x16<const ENC: bool, IO: NumIO>(
    buf: Buf,
    off: u32,
    pred: u16,
    io: &mut IO,
    k: usize,
    c: NCtx,
) -> (u16, i32) {
    if ENC {
        let v = buf.rd16(off);
        let r = i32::from(v.wrapping_sub(pred).cast_signed());
        io.code(k, c, r);
        (v, r)
    } else {
        let r = io.code(k, c, 0);
        let v = pred.wrapping_add(r.cast_unsigned() as u16);
        buf.wr16(off, v);
        (v, i32::from(r as i16))
    }
}

#[inline(always)]
fn x32<const ENC: bool, IO: NumIO>(
    buf: Buf,
    off: u32,
    pred: u32,
    io: &mut IO,
    k: usize,
    c: NCtx,
) -> (u32, i32) {
    if ENC {
        let v = buf.rd32(off);
        let r = v.wrapping_sub(pred).cast_signed();
        io.code(k, c, r);
        (v, r)
    } else {
        let r = io.code(k, c, 0);
        let v = pred.wrapping_add(r.cast_unsigned());
        buf.wr32(off, v);
        (v, r)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Group {
    PiO(u8),
    PiA(u8),
    PeO(u8),
    /// the three player origin axes in one pass
    PiOAll,
    /// the three player angle axes in one pass
    PiAAll,
    /// all three entity axes in one pass
    PeAll,
    /// usercmd angles on the grid
    Uc,
    Snd,
    Te,
}

/// Wave 1, split per axis (for multi-threaded decoding).
pub(crate) const WAVE1: [Group; 10] = [
    Group::PiA(1),
    Group::PiA(0),
    Group::PiO(0),
    Group::PiO(1),
    Group::PiO(2),
    Group::PeO(0),
    Group::PeO(1),
    Group::PeO(2),
    Group::PiA(2),
    Group::Uc,
];
/// Wave 1 for pipelined decoding next to the walker (one task per worker).
pub(crate) const WAVE1_PIPE: [Group; 4] = [Group::PiAAll, Group::PiOAll, Group::PeAll, Group::Uc];
pub(crate) const WAVE2: [Group; 2] = [Group::Snd, Group::Te];

impl Group {
    /// Stream ids by local index.
    pub(crate) fn streams(self) -> &'static [usize] {
        match self {
            Self::PiO(0) => &[PI_OX, PI_F0],
            Self::PiO(1) => &[PI_OX + 1, PI_F0 + 1],
            Self::PiO(_) => &[PI_OX + 2, PI_F0 + 2],
            Self::PiA(0) => &[PI_AP],
            Self::PiA(1) => &[PI_AP + 1],
            Self::PiA(_) => &[PI_AP + 2],
            Self::PeO(0) => &[PE_OX, PE_F0],
            Self::PeO(1) => &[PE_OX + 1, PE_F0 + 1],
            Self::PeO(_) => &[PE_OX + 2, PE_F0 + 2],
            Self::PeAll => &[PE_OX, PE_F0, PE_OX + 1, PE_F0 + 1, PE_OX + 2, PE_F0 + 2],
            Self::PiOAll => &[PI_OX, PI_F0, PI_OX + 1, PI_F0 + 1, PI_OX + 2, PI_F0 + 2],
            Self::PiAAll => &[PI_AP, PI_AP + 1, PI_AP + 2],
            Self::Uc => &[UC_A],
            Self::Snd => &[SND_O],
            Self::Te => &[TE_O],
        }
    }

    pub(crate) fn run<const ENC: bool, IO: NumIO>(self, recs: &Recs, buf: Buf, io: &mut IO) {
        match self {
            Self::PiO(a) => {
                let mut st = vec![Track::default(); 256];
                let mut tm = vec![(0u32, 0u32); 256];
                for r in recs.pi.feed() {
                    let (dn, dp) = times(&mut tm[r.num as usize], r.t);
                    pi_origin_step::<ENC, IO>(
                        a as usize,
                        &mut st[r.num as usize],
                        r,
                        dn,
                        dp,
                        buf,
                        io,
                        0,
                    );
                }
            }
            Self::PiA(a) => {
                let mut st = vec![Track::default(); 256];
                let mut tm = vec![(0u32, 0u32); 256];
                for r in recs.pi.feed() {
                    let (dn, dp) = times(&mut tm[r.num as usize], r.t);
                    pi_angle_step::<ENC, IO>(
                        a as usize,
                        &mut st[r.num as usize],
                        r,
                        dn,
                        dp,
                        buf,
                        io,
                        0,
                    );
                }
            }
            Self::PiOAll => {
                let mut st = vec![[Track::default(); 3]; 256];
                let mut tm = vec![(0u32, 0u32); 256];
                for r in recs.pi.feed() {
                    let (dn, dp) = times(&mut tm[r.num as usize], r.t);
                    let s = &mut st[r.num as usize];
                    for a in 0..3 {
                        pi_origin_step::<ENC, IO>(a, &mut s[a], r, dn, dp, buf, io, 2 * a);
                    }
                }
            }
            Self::PiAAll => {
                let mut st = vec![[Track::default(); 3]; 256];
                let mut tm = vec![(0u32, 0u32); 256];
                for r in recs.pi.feed() {
                    let (dn, dp) = times(&mut tm[r.num as usize], r.t);
                    let s = &mut st[r.num as usize];
                    for a in 0..3 {
                        pi_angle_step::<ENC, IO>(a, &mut s[a], r, dn, dp, buf, io, a);
                    }
                }
            }
            Self::PeO(a) => {
                let mut st = vec![Track::default(); 2048];
                for r in recs.pe.feed() {
                    pe_origin_step::<ENC, IO>(
                        a as usize,
                        &mut st[(r.num & 2047) as usize],
                        r,
                        buf,
                        io,
                        0,
                    );
                }
            }
            Self::PeAll => {
                let mut st = vec![[Track::default(); 3]; 2048];
                for r in recs.pe.feed() {
                    let s = &mut st[(r.num & 2047) as usize];
                    for a in 0..3 {
                        pe_origin_step::<ENC, IO>(a, &mut s[a], r, buf, io, 2 * a);
                    }
                }
            }
            Self::Uc => uc::<ENC, IO>(recs, buf, io),
            Self::Snd => snd::<ENC, IO>(recs, buf, io),
            Self::Te => te::<ENC, IO>(recs, buf, io),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Track {
    last: u16,
    prev: u16,
    prev2: u16,
    res: i32,
    // float-coord state (IEEE bit patterns)
    flast: u32,
    fprev: u32,
    fres: i32,
    // entity times
    t_last: u32,
}

/// Per-player frame times: returns (`dt_now`, `dt_prev`) and advances.
#[inline(always)]
fn times(tm: &mut (u32, u32), t: u32) -> (i32, i32) {
    let (t_last, t_prev) = *tm;
    *tm = (t, t_last);
    (
        t.wrapping_sub(t_last).cast_signed(),
        t_last.wrapping_sub(t_prev).cast_signed(),
    )
}

#[inline(always)]
fn dd(now: i32, prev: i32) -> u8 {
    ((now - prev).clamp(-3, 3) + 3).cast_unsigned() as u8
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn pi_origin_step<const ENC: bool, IO: NumIO>(
    axis: usize,
    s: &mut Track,
    r: &PiRec,
    dt_now: i32,
    dt_prev: i32,
    buf: Buf,
    io: &mut IO,
    k: usize,
) {
    let sent = r.flags & (1 << axis) != 0;
    if r.fl & 1 != 0 {
        if sent {
            let off = r.off + 4 * (r.flags & ((1 << axis) - 1)).count_ones();
            let pred = s.flast.wrapping_add(s.flast.wrapping_sub(s.fprev));
            let c = NCtx {
                f: [q(s.fres), axis as u8, 0, 0, 0, 0],
            };
            let (v, d) = x32::<ENC, IO>(buf, off, pred, io, k + 1, c);
            s.fprev = s.flast;
            s.flast = v;
            s.fres = d;
        } else {
            s.fprev = s.flast;
            s.fres = 0;
        }
    } else {
        let v = if sent {
            let pred = pred_dt(s.last, s.prev, dt_now, dt_prev);
            let off = r.off + 2 * (r.flags & ((1 << axis) - 1)).count_ones();
            let vel = i32::from(s.last.wrapping_sub(s.prev).cast_signed());
            let c = NCtx {
                f: [
                    q(s.res),
                    q(vel),
                    dt_now.clamp(0, 63).cast_unsigned() as u8,
                    (r.flags & 7) as u8,
                    dd(dt_now, dt_prev),
                    0,
                ],
            };
            let (v, d) = x16::<ENC, IO>(buf, off, pred, io, k, c);
            s.res = d;
            v
        } else {
            s.res = 0;
            s.last
        };
        s.prev = s.last;
        s.last = v;
    }
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn pi_angle_step<const ENC: bool, IO: NumIO>(
    axis: usize,
    s: &mut Track,
    r: &PiRec,
    dt_now: i32,
    dt_prev: i32,
    buf: Buf,
    io: &mut IO,
    k: usize,
) {
    let v = if r.flags & (8 << axis) != 0 {
        let pred = pred_dt(s.last, s.prev, dt_now, dt_prev);
        let cw = if r.fl & 1 != 0 { 4 } else { 2 };
        let off = r.off
            + cw * (r.flags & 7).count_ones()
            + 2 * ((r.flags >> 3) & ((1 << axis) - 1)).count_ones();
        let dprev = i32::from(s.prev.wrapping_sub(s.prev2).cast_signed());
        let c = NCtx {
            f: [
                q(s.res),
                q(dprev),
                dt_now.clamp(0, 63).cast_unsigned() as u8,
                ((r.flags >> 3) & 7) as u8,
                dd(dt_now, dt_prev),
                0,
            ],
        };
        let (v, d) = x16::<ENC, IO>(buf, off, pred, io, k, c);
        s.res = d;
        v
    } else {
        s.res = 0;
        s.last
    };
    s.prev2 = s.prev;
    s.prev = s.last;
    s.last = v;
}

#[inline(always)]
fn pe_origin_step<const ENC: bool, IO: NumIO>(
    axis: usize,
    s: &mut Track,
    r: &PeRec,
    buf: Buf,
    io: &mut IO,
    k: usize,
) {
    let dt_now = r.t.wrapping_sub(s.t_last).cast_signed();
    s.t_last = r.t;
    let sent = r.bits & (1 << (2 * axis)) != 0;
    if sent {
        let off = r.origin_offs()[axis];
        if r.bits & PE_FLOAT != 0 {
            let pred = s.flast.wrapping_add(s.flast.wrapping_sub(s.fprev));
            let c = NCtx {
                f: [q(s.fres), axis as u8, 0, 0, 0, 0],
            };
            let (v, d) = x32::<ENC, IO>(buf, off, pred, io, k + 1, c);
            s.fprev = s.flast;
            s.flast = v;
            s.fres = d;
        } else {
            let vel = i32::from(s.last.wrapping_sub(s.prev).cast_signed());
            let pred = s.last.wrapping_add(vel.cast_unsigned() as u16);
            let ip = u8::from(r.bits & PE_INPREV != 0);
            let c = NCtx {
                f: [
                    q(s.res),
                    q(vel),
                    r.model,
                    ip | (r.ubits & 7) << 1,
                    dt_now.clamp(0, 63).cast_unsigned() as u8,
                    0,
                ],
            };
            let (v, d) = x16::<ENC, IO>(buf, off, pred, io, k, c);
            s.res = d;
            s.prev = s.last;
            s.last = v;
        }
    } else {
        s.prev = s.last;
        s.res = 0;
        s.fprev = s.flast;
        s.fres = 0;
    }
}

/// usercmd angles (FORMAT.md §7.6): per player and axis, the grid value
/// against the linear prediction from that axis's last two grid values.
fn uc<const ENC: bool, IO: NumIO>(recs: &Recs, buf: Buf, io: &mut IO) {
    let mut st = vec![[Track::default(); 3]; 256];
    for r in recs.uc.feed() {
        let s = &mut st[r.num as usize];
        for (a, t) in s.iter_mut().enumerate() {
            if r.grid & (1 << a) == 0 {
                continue;
            }
            let off = r.off + 4 * a as u32;
            let d = i32::from(t.last.wrapping_sub(t.prev).cast_signed());
            let d2 = i32::from(t.prev.wrapping_sub(t.prev2).cast_signed());
            let pred = t.last.wrapping_add(d.cast_unsigned() as u16);
            let c = NCtx {
                f: [q(t.res), q(d), q(d2), a as u8, 0, 0],
            };
            let v = if ENC {
                let v = crate::walk::on_grid(buf.rd32(off)).expect("the walker saw it on the grid");
                io.code(0, c, i32::from(v.wrapping_sub(pred).cast_signed()));
                v
            } else {
                let v = pred.wrapping_add(io.code(0, c, 0).cast_unsigned() as u16);
                buf.wr32(off, crate::walk::grid_f32(v));
                v
            };
            t.res = i32::from(v.wrapping_sub(pred).cast_signed());
            t.prev2 = t.prev;
            t.prev = t.last;
            t.last = v;
        }
    }
}

#[inline(always)]
fn rd_ref(buf: Buf, r: u32) -> u16 {
    if r == NONE { 0 } else { buf.rd16(r) }
}

fn snd<const ENC: bool, IO: NumIO>(recs: &Recs, buf: Buf, io: &mut IO) {
    for r in recs.snd.feed() {
        for i in 0..3u32 {
            let pred = rd_ref(buf, r.refs[i as usize]);
            let c = NCtx {
                f: [i as u8 | r.kind << 2, r.num, 0, 0, 0, 0],
            };
            x16::<ENC, IO>(buf, r.off + 2 * i, pred, io, 0, c);
        }
    }
}

fn te<const ENC: bool, IO: NumIO>(recs: &Recs, buf: Buf, io: &mut IO) {
    let mut last = [0u16; 3];
    for r in recs.te.feed() {
        if matches!(r.ty, 5 | 6 | 9) {
            let mut start = [0u16; 3];
            for i in 0..3u32 {
                let pred = rd_ref(buf, r.refs[i as usize]);
                let c = NCtx {
                    f: [i as u8 | 1 << 2, r.ty, 0, 0, 0, 0],
                };
                start[i as usize] = x16::<ENC, IO>(buf, r.off + 2 * i, pred, io, 0, c).0;
            }
            for i in 0..3u32 {
                let c = NCtx {
                    f: [i as u8 | 2 << 2, r.ty, 0, 0, 0, 0],
                };
                last[i as usize] =
                    x16::<ENC, IO>(buf, r.off + 6 + 2 * i, start[i as usize], io, 0, c).0;
            }
        } else {
            for i in 0..3u32 {
                let c = NCtx {
                    f: [i as u8, r.ty, 0, 0, 0, 0],
                };
                last[i as usize] = x16::<ENC, IO>(buf, r.off + 2 * i, last[i as usize], io, 0, c).0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn div_round_ref(a: i64, b: i64) -> i64 {
        if (a < 0) == (b < 0) {
            (a + b / 2) / b
        } else {
            -((-a + b / 2) / b)
        }
    }
    #[test]
    fn div_round_matches_reference() {
        let mut x = 12345u64;
        for b in 1..600i32 {
            for _ in 0..2000 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                let a = (x % (2 * 32768 * 32767) as u64).cast_signed() - 32768 * 32767;
                assert_eq!(
                    i64::from(div_round(a as i32, b)),
                    div_round_ref(a, i64::from(b)),
                    "{a} / {b}"
                );
            }
            for a in [
                -32768 * 32767,
                32767 * 32767,
                0,
                1,
                -1,
                b / 2,
                -(b / 2),
                b,
                -b,
            ] {
                assert_eq!(
                    i64::from(div_round(a, b)),
                    div_round_ref(i64::from(a), i64::from(b)),
                    "{a} / {b}"
                );
            }
        }
    }
}
