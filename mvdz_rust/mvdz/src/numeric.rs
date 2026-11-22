//! Numeric groups: each walks one record list from the walker, predicts every field from
//! group-local state and codes the residual; one code path for both directions.

use crate::math::{div_round, grid_f32, on_grid, residual_bucket};
use crate::protocol::{
    DF_ANGLES, DF_ORIGIN, ORIGIN_FLAGS, TE_LIGHTNING_1, TE_LIGHTNING_2, TE_LIGHTNING_3,
};
use crate::streams::{
    ENTITY_FLOAT_ORIGIN, ENTITY_ORIGIN, LZ_DISTANCE, LZ_LENGTH, PLAYER_ANGLE, PLAYER_FLOAT_ORIGIN,
    PLAYER_ORIGIN, SOUND_ORIGIN, TEMP_ENTITY_ORIGIN, USERCMD_ANGLE,
};
use crate::walker::{
    NO_OFFSET, PE_FLOAT, PE_IN_PREVIOUS, PacketEntityRecord, PlayerInfoRecord, RecordLists,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub(crate) struct NumericContext {
    pub(crate) features: [u8; 4],
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

/// The rANS context key of a residual on `stream` (FORMAT.md section 5).
#[inline(always)]
pub(crate) fn numeric_context(stream: usize, context: &NumericContext) -> u32 {
    let features = &context.features;
    let clamped = |value: u8, max: u8| u32::from(value.min(max));
    match stream {
        PLAYER_ORIGIN..PLAYER_ANGLE => clamped(features[0], 19) | clamped(features[1], 19) << 5,
        PLAYER_ANGLE..ENTITY_ORIGIN => clamped(features[0], 23) | clamped(features[1], 23) << 5,
        ENTITY_ORIGIN..SOUND_ORIGIN => {
            clamped(features[0], 17) | clamped(features[1], 17) << 5 | u32::from(features[2]) << 10
        }
        SOUND_ORIGIN | TEMP_ENTITY_ORIGIN => u32::from(features[0]) | u32::from(features[1]) << 4,
        USERCMD_ANGLE => {
            clamped(features[0], 23)
                | clamped(features[1], 23) << 5
                | clamped(features[2], 15) << 10
                | u32::from(features[3]) << 14
        }
        LZ_LENGTH | LZ_DISTANCE => u32::from(features[0]),
        _ => clamped(features[0], 63) | u32::from(features[1]) << 6,
    }
}

/// Frame intervals are clamped so the scaled velocity fits 32 bits.
pub(crate) const TIME_DELTA_MAX: i32 = 32767;

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
            Self::PlayerOrigin(0) => &[PLAYER_ORIGIN, PLAYER_FLOAT_ORIGIN],
            Self::PlayerOrigin(1) => &[PLAYER_ORIGIN + 1, PLAYER_FLOAT_ORIGIN + 1],
            Self::PlayerOrigin(_) => &[PLAYER_ORIGIN + 2, PLAYER_FLOAT_ORIGIN + 2],
            Self::PlayerAngle(0) => &[PLAYER_ANGLE],
            Self::PlayerAngle(1) => &[PLAYER_ANGLE + 1],
            Self::PlayerAngle(_) => &[PLAYER_ANGLE + 2],
            Self::EntityOrigin(0) => &[ENTITY_ORIGIN, ENTITY_FLOAT_ORIGIN],
            Self::EntityOrigin(1) => &[ENTITY_ORIGIN + 1, ENTITY_FLOAT_ORIGIN + 1],
            Self::EntityOrigin(_) => &[ENTITY_ORIGIN + 2, ENTITY_FLOAT_ORIGIN + 2],
            Self::UsercmdAngles => &[USERCMD_ANGLE],
            Self::Sound => &[SOUND_ORIGIN],
            Self::TempEntity => &[TEMP_ENTITY_ORIGIN],
        }
    }

    pub(crate) fn run<const IS_ENCODE: bool, IO: NumericIo>(
        self,
        records: &RecordLists,
        io: &mut IO,
    ) {
        match self {
            Self::PlayerOrigin(axis) => player_steps(records, io, |track, record, deltas, io| {
                player_origin_step::<IS_ENCODE, IO>(axis as usize, track, record, deltas, io, 0);
            }),
            Self::PlayerAngle(axis) => player_steps(records, io, |track, record, deltas, io| {
                player_angle_step::<IS_ENCODE, IO>(axis as usize, track, record, deltas, io, 0);
            }),
            Self::EntityOrigin(axis) => {
                let mut tracks = vec![AxisTrack::default(); 2048];
                for record in &records.packet_entities {
                    entity_origin_step::<IS_ENCODE, IO>(
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

/// One player's or entity's last values on one axis.
#[derive(Debug, Clone, Copy, Default)]
struct AxisTrack {
    last: u16,
    previous: u16,
    before_previous: u16,
    residual: i32,
    float_last_bits: u32,
    float_previous_bits: u32,
    float_residual_bits: i32,
}

impl AxisTrack {
    #[inline(always)]
    fn velocity(&self) -> i32 {
        i32::from(self.last.wrapping_sub(self.previous).cast_signed())
    }
    #[inline(always)]
    fn previous_velocity(&self) -> i32 {
        i32::from(
            self.previous
                .wrapping_sub(self.before_previous)
                .cast_signed(),
        )
    }
    #[inline(always)]
    fn push(&mut self, value: u16) {
        self.before_previous = self.previous;
        self.previous = self.last;
        self.last = value;
    }
    /// last + velocity * `time_delta_now` / `time_delta_previous`, rounded, wrapping at 16 bits.
    #[inline(always)]
    fn predict_time_scaled(&self, time_deltas: TimeDeltas) -> u16 {
        let TimeDeltas { now, previous } = time_deltas;
        let velocity = self.velocity();
        if now == previous || previous <= 0 || now < 0 {
            self.last.wrapping_add(velocity.cast_unsigned() as u16)
        } else {
            self.last.wrapping_add(
                div_round(
                    velocity * now.min(TIME_DELTA_MAX),
                    previous.min(TIME_DELTA_MAX),
                )
                .cast_unsigned() as u16,
            )
        }
    }
    /// The coordinate as f32 bits, predicted linearly from the last two.
    #[inline(always)]
    fn code_float<const IS_ENCODE: bool, IO: NumericIo>(
        &mut self,
        offset: u32,
        axis: usize,
        io: &mut IO,
        stream_index: usize,
    ) {
        let prediction = self
            .float_last_bits
            .wrapping_add(self.float_last_bits.wrapping_sub(self.float_previous_bits));
        let context = NumericContext {
            features: [residual_bucket(self.float_residual_bits), axis as u8, 0, 0],
        };
        let (value, residual) =
            code_u32::<IS_ENCODE, IO>(offset, prediction, io, stream_index, context);
        self.float_previous_bits = self.float_last_bits;
        self.float_last_bits = value;
        self.float_residual_bits = residual;
    }
}

/// Between a player's frames: this one and the one before.
#[derive(Debug, Clone, Copy)]
struct TimeDeltas {
    now: i32,
    previous: i32,
}

#[inline(always)]
fn time_deltas(frame_times: &mut (u32, u32), time: u32) -> TimeDeltas {
    let (last, previous) = *frame_times;
    *frame_times = (time, last);
    TimeDeltas {
        now: time.wrapping_sub(last).cast_signed(),
        previous: last.wrapping_sub(previous).cast_signed(),
    }
}

/// Every player info record, each stepped on its player's track.
fn player_steps<IO: NumericIo>(
    records: &RecordLists,
    io: &mut IO,
    step: impl Fn(&mut AxisTrack, &PlayerInfoRecord, TimeDeltas, &mut IO),
) {
    let mut tracks = vec![AxisTrack::default(); 256];
    let mut frame_times = vec![(0u32, 0u32); 256];
    for record in &records.player_infos {
        let deltas = time_deltas(&mut frame_times[record.number as usize], record.time);
        step(&mut tracks[record.number as usize], record, deltas, io);
    }
}

#[inline(always)]
fn player_origin_step<const IS_ENCODE: bool, IO: NumericIo>(
    axis: usize,
    track: &mut AxisTrack,
    record: &PlayerInfoRecord,
    time_deltas: TimeDeltas,
    io: &mut IO,
    stream_index: usize,
) {
    let is_sent = record.flags & (DF_ORIGIN << axis) != 0;
    if record.has_float_coords {
        if is_sent {
            let offset = record.offset + 4 * (record.flags & ((1 << axis) - 1)).count_ones();
            track.code_float::<IS_ENCODE, IO>(offset, axis, io, stream_index + 1);
        } else {
            track.float_previous_bits = track.float_last_bits;
            track.float_residual_bits = 0;
        }
    } else {
        let value = if is_sent {
            let prediction = track.predict_time_scaled(time_deltas);
            let offset = record.offset + 2 * (record.flags & ((1 << axis) - 1)).count_ones();
            let context = NumericContext {
                features: [
                    residual_bucket(track.residual),
                    residual_bucket(track.velocity()),
                    0,
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
        track.push(value);
    }
}

#[inline(always)]
fn player_angle_step<const IS_ENCODE: bool, IO: NumericIo>(
    axis: usize,
    track: &mut AxisTrack,
    record: &PlayerInfoRecord,
    time_deltas: TimeDeltas,
    io: &mut IO,
    stream_index: usize,
) {
    let value = if record.flags & (DF_ANGLES << axis) != 0 {
        let prediction = track.predict_time_scaled(time_deltas);
        let coord_width = if record.has_float_coords { 4 } else { 2 };
        let offset = record.offset
            + coord_width * (record.flags & ORIGIN_FLAGS).count_ones()
            + 2 * ((record.flags >> 3) & ((1 << axis) - 1)).count_ones();
        let context = NumericContext {
            features: [
                residual_bucket(track.residual),
                residual_bucket(track.previous_velocity()),
                0,
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
    track.push(value);
}

#[inline(always)]
fn entity_origin_step<const IS_ENCODE: bool, IO: NumericIo>(
    axis: usize,
    track: &mut AxisTrack,
    record: &PacketEntityRecord,
    io: &mut IO,
    stream_index: usize,
) {
    let is_sent = record.bits & (1 << (2 * axis)) != 0;
    if is_sent {
        let offset = record.origin_offsets()[axis];
        if record.bits & PE_FLOAT != 0 {
            track.code_float::<IS_ENCODE, IO>(offset, axis, io, stream_index + 1);
        } else {
            let velocity = track.velocity();
            let prediction = track.last.wrapping_add(velocity.cast_unsigned() as u16);
            let in_previous_bit = u8::from(record.bits & PE_IN_PREVIOUS != 0);
            let context = NumericContext {
                features: [
                    residual_bucket(track.residual),
                    residual_bucket(velocity),
                    in_previous_bit,
                    0,
                ],
            };
            let (value, residual) =
                code_u16::<IS_ENCODE, IO>(offset, prediction, io, stream_index, context);
            track.residual = residual;
            track.push(value);
        }
    } else {
        track.push(track.last);
        track.residual = 0;
        track.float_previous_bits = track.float_last_bits;
        track.float_residual_bits = 0;
    }
}

/// FORMAT.md section 5.6: per player and axis, linear prediction from the last two grid values.
fn usercmd_angles<const IS_ENCODE: bool, IO: NumericIo>(records: &RecordLists, io: &mut IO) {
    let mut tracks = vec![[AxisTrack::default(); 3]; 256];
    for record in &records.usercmds {
        let player_tracks = &mut tracks[record.number as usize];
        for (axis, track) in player_tracks.iter_mut().enumerate() {
            if record.grid_axes & (1 << axis) == 0 {
                continue;
            }
            let offset = record.pitch_offset + 4 * axis as u32;
            let velocity = track.velocity();
            let prediction = track.last.wrapping_add(velocity.cast_unsigned() as u16);
            let context = NumericContext {
                features: [
                    residual_bucket(track.residual),
                    residual_bucket(velocity),
                    residual_bucket(track.previous_velocity()),
                    axis as u8,
                ],
            };
            let value = if IS_ENCODE {
                let value =
                    on_grid(read_u32(io.segment(), offset)).expect("the walker saw it on the grid");
                io.code(
                    0,
                    context,
                    i32::from(value.wrapping_sub(prediction).cast_signed()),
                );
                value
            } else {
                let value = prediction.wrapping_add(io.code(0, context, 0).cast_unsigned() as u16);
                write_u32(io.segment_mut(), offset, grid_f32(value));
                value
            };
            track.residual = i32::from(value.wrapping_sub(prediction).cast_signed());
            track.push(value);
        }
    }
}

#[inline(always)]
fn referenced(bytes: &[u8], reference: u32) -> u16 {
    if reference == NO_OFFSET {
        0
    } else {
        read_u16(bytes, reference)
    }
}

fn sound_origins<const IS_ENCODE: bool, IO: NumericIo>(records: &RecordLists, io: &mut IO) {
    for record in &records.sounds {
        for axis in 0..3u32 {
            let prediction = referenced(io.segment(), record.references[axis as usize]);
            let context = NumericContext {
                features: [axis as u8 | record.kind << 2, record.number, 0, 0],
            };
            code_u16::<IS_ENCODE, IO>(record.offset + 2 * axis, prediction, io, 0, context);
        }
    }
}

fn temp_entity_origins<const IS_ENCODE: bool, IO: NumericIo>(records: &RecordLists, io: &mut IO) {
    let mut last = [0u16; 3];
    for record in &records.temp_entities {
        if matches!(
            record.kind,
            TE_LIGHTNING_1 | TE_LIGHTNING_2 | TE_LIGHTNING_3
        ) {
            let mut start = [0u16; 3];
            for axis in 0..3u32 {
                let prediction = referenced(io.segment(), record.references[axis as usize]);
                let context = NumericContext {
                    features: [axis as u8 | 1 << 2, record.kind, 0, 0],
                };
                start[axis as usize] =
                    code_u16::<IS_ENCODE, IO>(record.offset + 2 * axis, prediction, io, 0, context)
                        .0;
            }
            for axis in 0..3u32 {
                let context = NumericContext {
                    features: [axis as u8 | 2 << 2, record.kind, 0, 0],
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
                    features: [axis as u8, record.kind, 0, 0],
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
