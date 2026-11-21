//! Numeric groups: predictors and side contexts for the numeric fields.
//!
//! Each group walks one record list (written by the walker) in order,
//! predicts each numeric field from group-local state, and codes the
//! residual through a [`NumIO`]. The same code runs in both directions: on
//! encode it reads the value from the input buffer, on decode it writes the
//! reconstructed value into the output buffer at the recorded offset.
//! The groups are independent of each other except that sounds and temp
//! entities read player and entity origins the others wrote: decoding runs
//! them last ([`GROUPS`]).

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
    /// The segment: the input on encode, the output on decode.
    fn bytes(&self) -> &[u8];
    /// The output segment on decode; empty on encode, which writes nothing.
    fn bytes_mut(&mut self) -> &mut [u8];
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

/// The little-endian u16 at `off` (0 outside `bytes`).
#[inline(always)]
fn rd16(bytes: &[u8], off: u32) -> u16 {
    bytes
        .get(off as usize..)
        .and_then(<[u8]>::first_chunk)
        .map_or(0, |le| u16::from_le_bytes(*le))
}

/// The little-endian u32 at `off` (0 outside `bytes`).
#[inline(always)]
fn rd32(bytes: &[u8], off: u32) -> u32 {
    bytes
        .get(off as usize..)
        .and_then(<[u8]>::first_chunk)
        .map_or(0, |le| u32::from_le_bytes(*le))
}

/// Writes `value` at `off` as little-endian (nothing outside `bytes`).
#[inline(always)]
fn wr16(bytes: &mut [u8], off: u32, value: u16) {
    if let Some(le) = bytes
        .get_mut(off as usize..)
        .and_then(<[u8]>::first_chunk_mut)
    {
        *le = value.to_le_bytes();
    }
}

/// Writes `value` at `off` as little-endian (nothing outside `bytes`).
#[inline(always)]
fn wr32(bytes: &mut [u8], off: u32, value: u32) {
    if let Some(le) = bytes
        .get_mut(off as usize..)
        .and_then(<[u8]>::first_chunk_mut)
    {
        *le = value.to_le_bytes();
    }
}

#[inline(always)]
fn x16<const ENC: bool, IO: NumIO>(
    off: u32,
    pred: u16,
    io: &mut IO,
    k: usize,
    c: NCtx,
) -> (u16, i32) {
    if ENC {
        let v = rd16(io.bytes(), off);
        let r = i32::from(v.wrapping_sub(pred).cast_signed());
        io.code(k, c, r);
        (v, r)
    } else {
        let r = io.code(k, c, 0);
        let v = pred.wrapping_add(r.cast_unsigned() as u16);
        wr16(io.bytes_mut(), off, v);
        (v, i32::from(r as i16))
    }
}

#[inline(always)]
fn x32<const ENC: bool, IO: NumIO>(
    off: u32,
    pred: u32,
    io: &mut IO,
    k: usize,
    c: NCtx,
) -> (u32, i32) {
    if ENC {
        let v = rd32(io.bytes(), off);
        let r = v.wrapping_sub(pred).cast_signed();
        io.code(k, c, r);
        (v, r)
    } else {
        let r = io.code(k, c, 0);
        let v = pred.wrapping_add(r.cast_unsigned());
        wr32(io.bytes_mut(), off, v);
        (v, r)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Group {
    PeO(u8),
    PiA(u8),
    PiO(u8),
    Snd,
    Te,
    /// usercmd angles on the grid
    Uc,
}

/// Every group, in decoding order: sounds and temp entities last, as they
/// read the origins the others write.
pub(crate) const GROUPS: [Group; 12] = [
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
    Group::Snd,
    Group::Te,
];

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
            Self::Uc => &[UC_A],
            Self::Snd => &[SND_O],
            Self::Te => &[TE_O],
        }
    }

    pub(crate) fn run<const ENC: bool, IO: NumIO>(self, recs: &Recs, io: &mut IO) {
        match self {
            Self::PiO(a) => {
                let mut st = vec![Track::default(); 256];
                let mut tm = vec![(0u32, 0u32); 256];
                for r in recs.pi.iter() {
                    let (dn, dp) = times(&mut tm[r.num as usize], r.t);
                    pi_origin_step::<ENC, IO>(
                        a as usize,
                        &mut st[r.num as usize],
                        r,
                        dn,
                        dp,
                        io,
                        0,
                    );
                }
            }
            Self::PiA(a) => {
                let mut st = vec![Track::default(); 256];
                let mut tm = vec![(0u32, 0u32); 256];
                for r in recs.pi.iter() {
                    let (dn, dp) = times(&mut tm[r.num as usize], r.t);
                    pi_angle_step::<ENC, IO>(a as usize, &mut st[r.num as usize], r, dn, dp, io, 0);
                }
            }
            Self::PeO(a) => {
                let mut st = vec![Track::default(); 2048];
                for r in recs.pe.iter() {
                    pe_origin_step::<ENC, IO>(
                        a as usize,
                        &mut st[(r.num & 2047) as usize],
                        r,
                        io,
                        0,
                    );
                }
            }
            Self::Uc => uc::<ENC, IO>(recs, io),
            Self::Snd => snd::<ENC, IO>(recs, io),
            Self::Te => te::<ENC, IO>(recs, io),
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
            let (v, d) = x32::<ENC, IO>(off, pred, io, k + 1, c);
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
            let (v, d) = x16::<ENC, IO>(off, pred, io, k, c);
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
        let (v, d) = x16::<ENC, IO>(off, pred, io, k, c);
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
            let (v, d) = x32::<ENC, IO>(off, pred, io, k + 1, c);
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
            let (v, d) = x16::<ENC, IO>(off, pred, io, k, c);
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
fn uc<const ENC: bool, IO: NumIO>(recs: &Recs, io: &mut IO) {
    let mut st = vec![[Track::default(); 3]; 256];
    for r in recs.uc.iter() {
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
                let v = crate::walk::on_grid(rd32(io.bytes(), off))
                    .expect("the walker saw it on the grid");
                io.code(0, c, i32::from(v.wrapping_sub(pred).cast_signed()));
                v
            } else {
                let v = pred.wrapping_add(io.code(0, c, 0).cast_unsigned() as u16);
                wr32(io.bytes_mut(), off, crate::walk::grid_f32(v));
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
fn rd_ref(bytes: &[u8], r: u32) -> u16 {
    if r == NONE { 0 } else { rd16(bytes, r) }
}

fn snd<const ENC: bool, IO: NumIO>(recs: &Recs, io: &mut IO) {
    for r in recs.snd.iter() {
        for i in 0..3u32 {
            let pred = rd_ref(io.bytes(), r.refs[i as usize]);
            let c = NCtx {
                f: [i as u8 | r.kind << 2, r.num, 0, 0, 0, 0],
            };
            x16::<ENC, IO>(r.off + 2 * i, pred, io, 0, c);
        }
    }
}

fn te<const ENC: bool, IO: NumIO>(recs: &Recs, io: &mut IO) {
    let mut last = [0u16; 3];
    for r in recs.te.iter() {
        if matches!(r.ty, 5 | 6 | 9) {
            let mut start = [0u16; 3];
            for i in 0..3u32 {
                let pred = rd_ref(io.bytes(), r.refs[i as usize]);
                let c = NCtx {
                    f: [i as u8 | 1 << 2, r.ty, 0, 0, 0, 0],
                };
                start[i as usize] = x16::<ENC, IO>(r.off + 2 * i, pred, io, 0, c).0;
            }
            for i in 0..3u32 {
                let c = NCtx {
                    f: [i as u8 | 2 << 2, r.ty, 0, 0, 0, 0],
                };
                last[i as usize] = x16::<ENC, IO>(r.off + 6 + 2 * i, start[i as usize], io, 0, c).0;
            }
        } else {
            for i in 0..3u32 {
                let c = NCtx {
                    f: [i as u8, r.ty, 0, 0, 0, 0],
                };
                last[i as usize] = x16::<ENC, IO>(r.off + 2 * i, last[i as usize], io, 0, c).0;
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
