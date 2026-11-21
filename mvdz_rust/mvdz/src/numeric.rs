//! Numeric groups: each walks one record list from the walker, predicts every field from
//! group-local state and codes the residual; one code path for both directions.

use crate::proto::{TE_LIGHTNING_1, TE_LIGHTNING_2, TE_LIGHTNING_3};
use crate::streams::{PE_F0, PE_OX, PI_AP, PI_F0, PI_OX, SND_O, TE_O, UC_A};
use crate::walk::{
    NO_OFFSET, PE_FLOAT, PE_IN_PREVIOUS, PacketEntityRecord, PlayerInfoRecord, Records,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(crate) struct NumericContext {
    pub(crate) features: [u8; 6],
}

pub(crate) trait NumericIo {
    /// Encode: record `value` on local stream `stream_index`, return it.
    /// Decode: return the decoded residual.
    fn code(&mut self, stream_index: usize, context: NumericContext, value: i32) -> i32;
    /// The segment: the input on encode, the output on decode.
    fn segment(&self) -> &[u8];
    /// Empty on encode, which writes nothing.
    fn segment_mut(&mut self) -> &mut [u8];
}

/// 0 for zero, else 2 * bitlen(|residual|) - 1 + sign (0..=64).
#[inline(always)]
pub(crate) fn residual_bucket(residual: i32) -> u8 {
    if residual == 0 {
        return 0;
    }
    let sign = u32::from(residual < 0);
    let magnitude = residual.unsigned_abs();
    let bit_length = magnitude.bit_width();
    (2 * bit_length - 1 + sign) as u8
}

/// floor(2^40 / d) + 1: n * RECIPROCALS[d] >> 40 == n / d exactly for
/// 0 <= n < 2^31, 1 <= d <= 255 (the error n/2^40 < 2^-9 < 1/d).
static RECIPROCALS: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut divisor = 1;
    while divisor < 256 {
        table[divisor] = (1u64 << 40) / divisor as u64 + 1;
        divisor += 1;
    }
    table
};

/// `divisor` > 0; halves round away from zero.
#[inline(always)]
fn div_round(dividend: i32, divisor: i32) -> i32 {
    let magnitude = dividend.unsigned_abs() + divisor.cast_unsigned() / 2;
    let quotient = if divisor < 256 {
        ((u128::from(magnitude) * u128::from(RECIPROCALS[divisor.cast_unsigned() as usize])) >> 40)
            as u32
    } else {
        magnitude / divisor.cast_unsigned()
    }
    .cast_signed();
    if dividend < 0 { -quotient } else { quotient }
}

/// Frame intervals are clamped so the scaled velocity fits 32 bits.
pub(crate) const TIME_DELTA_MAX: i32 = 32767;

/// last + (last - previous) * `time_delta_now` / `time_delta_previous`, rounded, wrapping
/// at 16 bits.
#[inline(always)]
fn predict_time_scaled(
    last: u16,
    previous: u16,
    time_delta_now: i32,
    time_delta_previous: i32,
) -> u16 {
    let velocity = i32::from((last.wrapping_sub(previous)).cast_signed());
    if time_delta_now == time_delta_previous || time_delta_previous <= 0 || time_delta_now < 0 {
        last.wrapping_add(velocity.cast_unsigned() as u16)
    } else {
        last.wrapping_add(
            div_round(
                velocity * time_delta_now.min(TIME_DELTA_MAX),
                time_delta_previous.min(TIME_DELTA_MAX),
            )
            .cast_unsigned() as u16,
        )
    }
}

/// 0 outside `bytes`.
#[inline(always)]
fn read_u16(bytes: &[u8], offset: u32) -> u16 {
    bytes
        .get(offset as usize..)
        .and_then(<[u8]>::first_chunk)
        .map_or(0, |le_bytes| u16::from_le_bytes(*le_bytes))
}

/// 0 outside `bytes`.
#[inline(always)]
fn read_u32(bytes: &[u8], offset: u32) -> u32 {
    bytes
        .get(offset as usize..)
        .and_then(<[u8]>::first_chunk)
        .map_or(0, |le_bytes| u32::from_le_bytes(*le_bytes))
}

/// Nothing outside `bytes`.
#[inline(always)]
fn write_u16(bytes: &mut [u8], offset: u32, value: u16) {
    if let Some(le_bytes) = bytes
        .get_mut(offset as usize..)
        .and_then(<[u8]>::first_chunk_mut)
    {
        *le_bytes = value.to_le_bytes();
    }
}

/// Nothing outside `bytes`.
#[inline(always)]
fn write_u32(bytes: &mut [u8], offset: u32, value: u32) {
    if let Some(le_bytes) = bytes
        .get_mut(offset as usize..)
        .and_then(<[u8]>::first_chunk_mut)
    {
        *le_bytes = value.to_le_bytes();
    }
}

#[inline(always)]
fn code_u16<const IS_ENCODE: bool, IO: NumericIo>(
    offset: u32,
    prediction: u16,
    io: &mut IO,
    stream_index: usize,
    context: NumericContext,
) -> (u16, i32) {
    if IS_ENCODE {
        let value = read_u16(io.segment(), offset);
        let residual = i32::from(value.wrapping_sub(prediction).cast_signed());
        io.code(stream_index, context, residual);
        (value, residual)
    } else {
        let residual = io.code(stream_index, context, 0);
        let value = prediction.wrapping_add(residual.cast_unsigned() as u16);
        write_u16(io.segment_mut(), offset, value);
        (value, i32::from(residual as i16))
    }
}

#[inline(always)]
fn code_u32<const IS_ENCODE: bool, IO: NumericIo>(
    offset: u32,
    prediction: u32,
    io: &mut IO,
    stream_index: usize,
    context: NumericContext,
) -> (u32, i32) {
    if IS_ENCODE {
        let value = read_u32(io.segment(), offset);
        let residual = value.wrapping_sub(prediction).cast_signed();
        io.code(stream_index, context, residual);
        (value, residual)
    } else {
        let residual = io.code(stream_index, context, 0);
        let value = prediction.wrapping_add(residual.cast_unsigned());
        write_u32(io.segment_mut(), offset, value);
        (value, residual)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Group {
    EntityOrigin(u8),
    PlayerAngle(u8),
    PlayerOrigin(u8),
    Sound,
    TempEntity,
    UsercmdAngles,
}

/// In decoding order: sounds and temp entities last, as they read the origins the others
/// write.
pub(crate) const GROUPS: [Group; 12] = [
    Group::PlayerAngle(1),
    Group::PlayerAngle(0),
    Group::PlayerOrigin(0),
    Group::PlayerOrigin(1),
    Group::PlayerOrigin(2),
    Group::EntityOrigin(0),
    Group::EntityOrigin(1),
    Group::EntityOrigin(2),
    Group::PlayerAngle(2),
    Group::UsercmdAngles,
    Group::Sound,
    Group::TempEntity,
];

impl Group {
    pub(crate) fn streams(self) -> &'static [usize] {
        match self {
            Self::PlayerOrigin(0) => &[PI_OX, PI_F0],
            Self::PlayerOrigin(1) => &[PI_OX + 1, PI_F0 + 1],
            Self::PlayerOrigin(_) => &[PI_OX + 2, PI_F0 + 2],
            Self::PlayerAngle(0) => &[PI_AP],
            Self::PlayerAngle(1) => &[PI_AP + 1],
            Self::PlayerAngle(_) => &[PI_AP + 2],
            Self::EntityOrigin(0) => &[PE_OX, PE_F0],
            Self::EntityOrigin(1) => &[PE_OX + 1, PE_F0 + 1],
            Self::EntityOrigin(_) => &[PE_OX + 2, PE_F0 + 2],
            Self::UsercmdAngles => &[UC_A],
            Self::Sound => &[SND_O],
            Self::TempEntity => &[TE_O],
        }
    }

    pub(crate) fn run<const IS_ENCODE: bool, IO: NumericIo>(self, records: &Records, io: &mut IO) {
        match self {
            Self::PlayerOrigin(axis) => {
                let mut tracks = vec![Track::default(); 256];
                let mut frame_times = vec![(0u32, 0u32); 256];
                for record in records.player_infos.iter() {
                    let (time_delta_now, time_delta_previous) =
                        time_deltas(&mut frame_times[record.number as usize], record.time);
                    pi_origin_step::<IS_ENCODE, IO>(
                        axis as usize,
                        &mut tracks[record.number as usize],
                        record,
                        time_delta_now,
                        time_delta_previous,
                        io,
                        0,
                    );
                }
            }
            Self::PlayerAngle(axis) => {
                let mut tracks = vec![Track::default(); 256];
                let mut frame_times = vec![(0u32, 0u32); 256];
                for record in records.player_infos.iter() {
                    let (time_delta_now, time_delta_previous) =
                        time_deltas(&mut frame_times[record.number as usize], record.time);
                    pi_angle_step::<IS_ENCODE, IO>(
                        axis as usize,
                        &mut tracks[record.number as usize],
                        record,
                        time_delta_now,
                        time_delta_previous,
                        io,
                        0,
                    );
                }
            }
            Self::EntityOrigin(axis) => {
                let mut tracks = vec![Track::default(); 2048];
                for record in records.packet_entities.iter() {
                    pe_origin_step::<IS_ENCODE, IO>(
                        axis as usize,
                        &mut tracks[(record.number & 2047) as usize],
                        record,
                        io,
                        0,
                    );
                }
            }
            Self::UsercmdAngles => usercmd_angles::<IS_ENCODE, IO>(records, io),
            Self::Sound => sound_origins::<IS_ENCODE, IO>(records, io),
            Self::TempEntity => temp_entity_origins::<IS_ENCODE, IO>(records, io),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Track {
    last: u16,
    previous: u16,
    before_previous: u16,
    residual: i32,
    float_last_bits: u32,
    float_previous_bits: u32,
    float_residual_bits: i32,
    last_time: u32,
}

#[inline(always)]
fn time_deltas(frame_times: &mut (u32, u32), time: u32) -> (i32, i32) {
    let (last, previous) = *frame_times;
    *frame_times = (time, last);
    (
        time.wrapping_sub(last).cast_signed(),
        last.wrapping_sub(previous).cast_signed(),
    )
}

#[inline(always)]
fn time_delta_change(now: i32, previous: i32) -> u8 {
    ((now - previous).clamp(-3, 3) + 3).cast_unsigned() as u8
}

#[inline(always)]
fn pi_origin_step<const IS_ENCODE: bool, IO: NumericIo>(
    axis: usize,
    track: &mut Track,
    record: &PlayerInfoRecord,
    time_delta_now: i32,
    time_delta_previous: i32,
    io: &mut IO,
    stream_index: usize,
) {
    let is_sent = record.flags & (1 << axis) != 0;
    if record.has_float_coords {
        if is_sent {
            let offset = record.offset + 4 * (record.flags & ((1 << axis) - 1)).count_ones();
            let prediction = track.float_last_bits.wrapping_add(
                track
                    .float_last_bits
                    .wrapping_sub(track.float_previous_bits),
            );
            let context = NumericContext {
                features: [
                    residual_bucket(track.float_residual_bits),
                    axis as u8,
                    0,
                    0,
                    0,
                    0,
                ],
            };
            let (value, residual) =
                code_u32::<IS_ENCODE, IO>(offset, prediction, io, stream_index + 1, context);
            track.float_previous_bits = track.float_last_bits;
            track.float_last_bits = value;
            track.float_residual_bits = residual;
        } else {
            track.float_previous_bits = track.float_last_bits;
            track.float_residual_bits = 0;
        }
    } else {
        let value = if is_sent {
            let prediction = predict_time_scaled(
                track.last,
                track.previous,
                time_delta_now,
                time_delta_previous,
            );
            let offset = record.offset + 2 * (record.flags & ((1 << axis) - 1)).count_ones();
            let velocity = i32::from(track.last.wrapping_sub(track.previous).cast_signed());
            let context = NumericContext {
                features: [
                    residual_bucket(track.residual),
                    residual_bucket(velocity),
                    time_delta_now.clamp(0, 63).cast_unsigned() as u8,
                    (record.flags & 7) as u8,
                    time_delta_change(time_delta_now, time_delta_previous),
                    0,
                ],
            };
            let (value, residual) =
                code_u16::<IS_ENCODE, IO>(offset, prediction, io, stream_index, context);
            track.residual = residual;
            value
        } else {
            track.residual = 0;
            track.last
        };
        track.previous = track.last;
        track.last = value;
    }
}

#[inline(always)]
fn pi_angle_step<const IS_ENCODE: bool, IO: NumericIo>(
    axis: usize,
    track: &mut Track,
    record: &PlayerInfoRecord,
    time_delta_now: i32,
    time_delta_previous: i32,
    io: &mut IO,
    stream_index: usize,
) {
    let value = if record.flags & (8 << axis) != 0 {
        let prediction = predict_time_scaled(
            track.last,
            track.previous,
            time_delta_now,
            time_delta_previous,
        );
        let coord_width = if record.has_float_coords { 4 } else { 2 };
        let offset = record.offset
            + coord_width * (record.flags & 7).count_ones()
            + 2 * ((record.flags >> 3) & ((1 << axis) - 1)).count_ones();
        let previous_velocity = i32::from(
            track
                .previous
                .wrapping_sub(track.before_previous)
                .cast_signed(),
        );
        let context = NumericContext {
            features: [
                residual_bucket(track.residual),
                residual_bucket(previous_velocity),
                time_delta_now.clamp(0, 63).cast_unsigned() as u8,
                ((record.flags >> 3) & 7) as u8,
                time_delta_change(time_delta_now, time_delta_previous),
                0,
            ],
        };
        let (value, residual) =
            code_u16::<IS_ENCODE, IO>(offset, prediction, io, stream_index, context);
        track.residual = residual;
        value
    } else {
        track.residual = 0;
        track.last
    };
    track.before_previous = track.previous;
    track.previous = track.last;
    track.last = value;
}

#[inline(always)]
fn pe_origin_step<const IS_ENCODE: bool, IO: NumericIo>(
    axis: usize,
    track: &mut Track,
    record: &PacketEntityRecord,
    io: &mut IO,
    stream_index: usize,
) {
    let time_delta_now = record.time.wrapping_sub(track.last_time).cast_signed();
    track.last_time = record.time;
    let is_sent = record.bits & (1 << (2 * axis)) != 0;
    if is_sent {
        let offset = record.origin_offsets()[axis];
        if record.bits & PE_FLOAT != 0 {
            let prediction = track.float_last_bits.wrapping_add(
                track
                    .float_last_bits
                    .wrapping_sub(track.float_previous_bits),
            );
            let context = NumericContext {
                features: [
                    residual_bucket(track.float_residual_bits),
                    axis as u8,
                    0,
                    0,
                    0,
                    0,
                ],
            };
            let (value, residual) =
                code_u32::<IS_ENCODE, IO>(offset, prediction, io, stream_index + 1, context);
            track.float_previous_bits = track.float_last_bits;
            track.float_last_bits = value;
            track.float_residual_bits = residual;
        } else {
            let velocity = i32::from(track.last.wrapping_sub(track.previous).cast_signed());
            let prediction = track.last.wrapping_add(velocity.cast_unsigned() as u16);
            let in_previous_bit = u8::from(record.bits & PE_IN_PREVIOUS != 0);
            let context = NumericContext {
                features: [
                    residual_bucket(track.residual),
                    residual_bucket(velocity),
                    record.model,
                    in_previous_bit | (record.update_bits & 7) << 1,
                    time_delta_now.clamp(0, 63).cast_unsigned() as u8,
                    0,
                ],
            };
            let (value, residual) =
                code_u16::<IS_ENCODE, IO>(offset, prediction, io, stream_index, context);
            track.residual = residual;
            track.previous = track.last;
            track.last = value;
        }
    } else {
        track.previous = track.last;
        track.residual = 0;
        track.float_previous_bits = track.float_last_bits;
        track.float_residual_bits = 0;
    }
}

/// FORMAT.md section 7.6: per player and axis, linear prediction from the last two grid values.
fn usercmd_angles<const IS_ENCODE: bool, IO: NumericIo>(records: &Records, io: &mut IO) {
    let mut tracks = vec![[Track::default(); 3]; 256];
    for record in records.usercmds.iter() {
        let player_tracks = &mut tracks[record.number as usize];
        for (axis, track) in player_tracks.iter_mut().enumerate() {
            if record.grid_axes & (1 << axis) == 0 {
                continue;
            }
            let offset = record.pitch_offset + 4 * axis as u32;
            let velocity = i32::from(track.last.wrapping_sub(track.previous).cast_signed());
            let previous_velocity = i32::from(
                track
                    .previous
                    .wrapping_sub(track.before_previous)
                    .cast_signed(),
            );
            let prediction = track.last.wrapping_add(velocity.cast_unsigned() as u16);
            let context = NumericContext {
                features: [
                    residual_bucket(track.residual),
                    residual_bucket(velocity),
                    residual_bucket(previous_velocity),
                    axis as u8,
                    0,
                    0,
                ],
            };
            let value = if IS_ENCODE {
                let value = crate::walk::on_grid(read_u32(io.segment(), offset))
                    .expect("the walker saw it on the grid");
                io.code(
                    0,
                    context,
                    i32::from(value.wrapping_sub(prediction).cast_signed()),
                );
                value
            } else {
                let value = prediction.wrapping_add(io.code(0, context, 0).cast_unsigned() as u16);
                write_u32(io.segment_mut(), offset, crate::walk::grid_f32(value));
                value
            };
            track.residual = i32::from(value.wrapping_sub(prediction).cast_signed());
            track.before_previous = track.previous;
            track.previous = track.last;
            track.last = value;
        }
    }
}

#[inline(always)]
fn read_reference(bytes: &[u8], reference: u32) -> u16 {
    if reference == NO_OFFSET {
        0
    } else {
        read_u16(bytes, reference)
    }
}

fn sound_origins<const IS_ENCODE: bool, IO: NumericIo>(records: &Records, io: &mut IO) {
    for record in records.sounds.iter() {
        for axis in 0..3u32 {
            let prediction = read_reference(io.segment(), record.refs[axis as usize]);
            let context = NumericContext {
                features: [axis as u8 | record.kind << 2, record.number, 0, 0, 0, 0],
            };
            code_u16::<IS_ENCODE, IO>(record.offset + 2 * axis, prediction, io, 0, context);
        }
    }
}

fn temp_entity_origins<const IS_ENCODE: bool, IO: NumericIo>(records: &Records, io: &mut IO) {
    let mut last = [0u16; 3];
    for record in records.temp_entities.iter() {
        if matches!(
            record.kind,
            TE_LIGHTNING_1 | TE_LIGHTNING_2 | TE_LIGHTNING_3
        ) {
            let mut start = [0u16; 3];
            for axis in 0..3u32 {
                let prediction = read_reference(io.segment(), record.refs[axis as usize]);
                let context = NumericContext {
                    features: [axis as u8 | 1 << 2, record.kind, 0, 0, 0, 0],
                };
                start[axis as usize] =
                    code_u16::<IS_ENCODE, IO>(record.offset + 2 * axis, prediction, io, 0, context)
                        .0;
            }
            for axis in 0..3u32 {
                let context = NumericContext {
                    features: [axis as u8 | 2 << 2, record.kind, 0, 0, 0, 0],
                };
                last[axis as usize] = code_u16::<IS_ENCODE, IO>(
                    record.offset + 6 + 2 * axis,
                    start[axis as usize],
                    io,
                    0,
                    context,
                )
                .0;
            }
        } else {
            for axis in 0..3u32 {
                let context = NumericContext {
                    features: [axis as u8, record.kind, 0, 0, 0, 0],
                };
                last[axis as usize] = code_u16::<IS_ENCODE, IO>(
                    record.offset + 2 * axis,
                    last[axis as usize],
                    io,
                    0,
                    context,
                )
                .0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn div_round_ref(dividend: i64, divisor: i64) -> i64 {
        if (dividend < 0) == (divisor < 0) {
            (dividend + divisor / 2) / divisor
        } else {
            -((-dividend + divisor / 2) / divisor)
        }
    }
    #[test]
    fn div_round_matches_reference() {
        let mut rng_state = 12345u64;
        for divisor in 1..600i32 {
            for _ in 0..2000 {
                rng_state ^= rng_state << 13;
                rng_state ^= rng_state >> 7;
                rng_state ^= rng_state << 17;
                let dividend =
                    (rng_state % (2 * 32768 * 32767) as u64).cast_signed() - 32768 * 32767;
                assert_eq!(
                    i64::from(div_round(dividend as i32, divisor)),
                    div_round_ref(dividend, i64::from(divisor)),
                    "{dividend} / {divisor}"
                );
            }
            for dividend in [
                -32768 * 32767,
                32767 * 32767,
                0,
                1,
                -1,
                divisor / 2,
                -(divisor / 2),
                divisor,
                -divisor,
            ] {
                assert_eq!(
                    i64::from(div_round(dividend, divisor)),
                    div_round_ref(i64::from(dividend), i64::from(divisor)),
                    "{dividend} / {divisor}"
                );
            }
        }
    }
}
