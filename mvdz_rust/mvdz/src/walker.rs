//! The walker, one routine both ways: every field of a segment in demo order, the numeric
//! ones as residuals against the predictions of `numeric.rs`. Decode writes the segment from
//! the streams. Encode reads the segment: every symbol is proposed from the bytes ahead of
//! the cursor, and a payload the walk does not end where its length says fails.

use crate::fields::read_usize;
use crate::math::{byte_residual_bucket, clamped_bucket, grid_from_f32_bits, grid_to_f32_bits};
use crate::numeric::{AxisTrack, FloatTrack, time_deltas};
use crate::protocol::{
    DEM_ALL, DEM_MULTIPLE, DEM_READ, DEM_SET, DEM_SINGLE, DEM_STATS, DF_ANGLES, DF_EFFECTS,
    DF_MODEL, DF_ORIGIN, DF_SKIN_NUM, DF_WEAPON_FRAME, FTE_PEXT_COLOR_MOD, FTE_PEXT_TRANS,
    ORIGIN_ANGLE_FLAGS, ProtocolState, SND_ATTENUATION, SND_VOLUME, SVC_DAMAGE,
    SVC_DELTA_PACKET_ENTITIES, SVC_NAILS, SVC_NAILS_2, SVC_PACKET_ENTITIES, SVC_PLAYER_INFO,
    SVC_PRINT, SVC_SERVER_DATA, SVC_SET_INFO, SVC_SOUND, SVC_STUFF_TEXT, SVC_TEMP_ENTITY,
    SVC_UPDATE_FRAGS, SVC_UPDATE_PING, SVC_UPDATE_STAT, SVC_UPDATE_STAT_LONG, TE_BLOOD, TE_GUNSHOT,
    TE_LIGHTNING_1, TE_LIGHTNING_2, TE_LIGHTNING_3, U_COLOR_MAP, U_EFFECTS, U_FRAME,
    U_FTE_COLOR_MOD, U_FTE_ENTITY_DBL, U_FTE_ENTITY_DBL_2, U_FTE_EVEN_MORE, U_FTE_FATNESS,
    U_FTE_MODEL_DBL, U_FTE_SCALE, U_FTE_TRANS, U_FTE_YET_MORE, U_MODEL, U_MORE_BITS, U_SKIN,
};
use crate::segment::Streams;
use crate::streams::{
    COMMAND, ENTITY_ANGLE, ENTITY_BITS, ENTITY_FLOAT_ORIGIN, ENTITY_FRAME, ENTITY_GAP,
    ENTITY_HEADER, ENTITY_MISC, ENTITY_MODEL, ENTITY_MORE, ENTITY_ORIGIN, ESCAPE_LENGTH, GENERIC,
    HIDDEN_LENGTH, HIDDEN_MISC, HIDDEN_TAG, HIDDEN_TYPE, MASK, NAILS, PLAYER_ANGLE, PLAYER_EFFECTS,
    PLAYER_FLAGS, PLAYER_FLOAT_ORIGIN, PLAYER_FRAME, PLAYER_MODEL, PLAYER_NUMBER, PLAYER_ORIGIN,
    PLAYER_SKIN, PLAYER_WEAPON_FRAME, RAW, SET, SOUND_CHANNEL, SOUND_FLOAT, SOUND_MISC,
    SOUND_NUMBER, SOUND_ORIGIN, STAT_INDEX, STAT_VALUE, TAIL, TEMP_ENTITY_FLOAT, TEMP_ENTITY_MISC,
    TEMP_ENTITY_ORIGIN, TEMP_ENTITY_TYPE, TIME, TYPE, USERCMD, USERCMD_ANGLE,
};

#[derive(Debug)]
pub(crate) struct SegmentWalker<'a, const IS_ENCODE: bool> {
    io: SegmentIo<'a, IS_ENCODE>,
    protocol: ProtocolState,
    players: Box<[PlayerState; 256]>,
    entities: Box<[EntityState; 2048]>,
    stats: Box<[[u8; 256]; 32]>,
    temp_entity_last: [u16; 3],
    time: u32,
    target: u8,
    last_type: u8,
    previous_command: u8,
    packet_number: u32,
    last_time_byte: u8,
    first_command_by_type: [u8; 8],
    stat_previous_index: u8,
    stat_previous_residual: u8,
    last_header: u8,
    previous_temp_entity: u8,
    usercmd_previous_players: [u8; 2],
    usercmds: Box<[UsercmdState; 256]>,
}

impl<'a, const IS_ENCODE: bool> SegmentWalker<'a, IS_ENCODE> {
    #[expect(
        clippy::large_stack_arrays,
        reason = "fixed-size tables keep byte-indexed lookups free of bounds checks"
    )]
    pub(crate) fn new(io: SegmentIo<'a, IS_ENCODE>, protocol: ProtocolState) -> Self {
        SegmentWalker {
            io,
            protocol,
            players: Box::new([PlayerState::default(); 256]),
            entities: Box::new([EntityState::default(); 2048]),
            stats: Box::new([[0u8; 256]; 32]),
            temp_entity_last: [0; 3],
            time: 0,
            target: 0,
            last_type: 0,
            previous_command: 0,
            packet_number: 1,
            last_time_byte: 0,
            first_command_by_type: [0; 8],
            stat_previous_index: 255,
            stat_previous_residual: 0,
            last_header: 0,
            previous_temp_entity: 0,
            usercmd_previous_players: [0; 2],
            usercmds: Box::new([UsercmdState::default(); 256]),
        }
    }

    /// `tail`: the bytes after the last whole frame, all of the `TAIL` blob stream.
    pub(crate) fn run(&mut self, tail: usize) {
        let Some(stop) = self.io.segment().len().checked_sub(tail) else {
            self.io.has_error = true;
            return;
        };
        while !self.io.has_error && self.io.offset < stop {
            self.frame();
        }
        if self.io.offset != stop {
            self.io.has_error = true;
        }
        self.io.copy_blob(TAIL, tail);
    }

    /// The streams and the protocol at the end of the segment; `None` after an error.
    pub(crate) fn finish(self) -> Option<(Streams<'a, IS_ENCODE>, ProtocolState)> {
        if self.io.has_error {
            return None;
        }
        Some((self.io.streams, self.protocol))
    }

    fn frame(&mut self) {
        let last_type = u32::from(self.last_type);
        let time_byte = self
            .io
            .code_symbol(TIME, last_type | u32::from(self.last_time_byte) << 8);
        self.last_time_byte = time_byte;
        self.time = self.time.wrapping_add(u32::from(time_byte));
        let frame_type = self
            .io
            .code_symbol(TYPE, last_type | u32::from(time_byte.min(32)) << 8);
        self.last_type = frame_type;
        self.target = frame_type >> 3;
        match frame_type & 7 {
            DEM_SET => self.io.copy_blob(SET, 8),
            DEM_MULTIPLE => {
                let mask = self.io.code_blob_bytes(MASK, 4);
                self.length_prefixed(frame_type, mask == 0);
            }
            DEM_READ | DEM_SINGLE | DEM_STATS | DEM_ALL => self.length_prefixed(frame_type, false),
            _ => self.io.has_error = true,
        }
    }

    fn length_prefixed(&mut self, frame_type: u8, is_hidden: bool) {
        let Some(len_offset) = self.io.start_payload() else {
            return;
        };
        if is_hidden {
            self.hidden_payload();
        } else {
            self.payload(frame_type);
        }
        self.io.end_payload(len_offset);
    }

    /// Bytes the walk does not model: for the encoder the rest of the payload.
    fn escape(&mut self) {
        let source_len = self.io.payload_rest().len() as u32;
        let mut len = 0u32;
        for (index, source_byte) in source_len.to_le_bytes().into_iter().enumerate() {
            len |= u32::from(self.io.blob_byte(ESCAPE_LENGTH, source_byte)) << (8 * index);
        }
        self.io.copy_blob(RAW, len as usize);
    }

    fn payload(&mut self, frame_type: u8) {
        self.previous_command = 0;
        self.stat_previous_index = 255;
        self.stat_previous_residual = 0;
        let type_index = (frame_type & 7) as usize;
        let previous_first_class = u32::from(command_class(self.first_command_by_type[type_index]));
        self.first_command_by_type[type_index] = 255;
        let mut command_index = 0u32;
        while !self.io.has_error {
            if command_index == 1 {
                self.first_command_by_type[type_index] = self.previous_command;
            }
            // Bit 15 splits the space: a known previous command is named by
            // its class, an "other" one (class 15) by its own number.
            let class = u32::from(command_class(self.previous_command));
            let context = if class == 15 {
                0x8000
                    | u32::from(self.previous_command)
                    | (type_index as u32) << 8
                    | command_index.min(7) << 11
            } else {
                class
                    | command_index.min(15) << 4
                    | previous_first_class << 8
                    | (type_index as u32) << 12
            };
            let source_command = self.io.source_command(&self.protocol);
            let command = self.io.symbol(COMMAND, context, source_command);
            match command {
                255 => return,
                0 => self.escape(),
                _ => {
                    self.io.write(command);
                    self.command(command);
                }
            }
            self.previous_command = command;
            command_index += 1;
        }
    }

    /// A command without a layout of its own: the length of its body in three bytes, then
    /// the body. Only the encoder sizes it.
    fn copy_command_body(&mut self, stream: usize, command: u8) {
        const LENGTH_CONTEXTS: u32 = 0xc000;
        let source_len = self.io.source_command_body_len(&self.protocol, command);
        let mut len = 0u32;
        for (index, source_byte) in source_len.to_le_bytes().into_iter().take(3).enumerate() {
            let context = LENGTH_CONTEXTS | (index as u32) << 8 | u32::from(command);
            len |= u32::from(self.io.symbol(COMMAND, context, source_byte)) << (8 * index);
        }
        self.io.copy_blob(stream, len as usize);
    }

    fn command(&mut self, command: u8) {
        match command {
            SVC_PLAYER_INFO => self.player_info(),
            SVC_PACKET_ENTITIES | SVC_DELTA_PACKET_ENTITIES => {
                self.packet_entities(command == SVC_DELTA_PACKET_ENTITIES);
            }
            SVC_UPDATE_STAT => {
                let player = (self.target & 31) as usize;
                let stat = self.io.code_symbol(
                    STAT_INDEX,
                    u32::from(self.stat_previous_index) | u32::from(self.previous_command) << 8,
                );
                let previous = self.stats[player][stat as usize];
                let value = self.io.code_symbol_residual(
                    STAT_VALUE,
                    u32::from(stat) | u32::from(self.stat_previous_residual) << 8,
                    previous,
                );
                self.stat_previous_index = stat;
                self.stat_previous_residual = value.wrapping_sub(previous);
                self.stats[player][stat as usize] = value;
            }
            SVC_SOUND => self.sound(),
            SVC_TEMP_ENTITY => self.temp_entity(),
            SVC_NAILS | SVC_NAILS_2 => self.copy_command_body(NAILS, command),
            SVC_SERVER_DATA => {
                let start = self.io.offset;
                self.copy_command_body(GENERIC + usize::from(SVC_SERVER_DATA), SVC_SERVER_DATA);
                if !self.io.has_error {
                    self.protocol
                        .observe_server_data(self.io.walked_since(start));
                }
            }
            _ => self.copy_command_body(GENERIC + command as usize, command),
        }
    }

    fn player_info(&mut self) {
        const DF_DEAD: u16 = 1 << 8;
        const DF_GIB: u16 = 1 << 9;
        let number = self.io.code_blob_byte(PLAYER_NUMBER);
        let player = &mut self.players[number as usize];
        let flags = self.io.player_info_flags(player.flags);
        let frame = self.io.code_symbol_residual(
            PLAYER_FRAME,
            u32::from(player.frame) | u32::from(flags & (DF_DEAD | DF_GIB)),
            player.frame,
        );
        self.io
            .player_origins_and_angles(player, flags, self.time, self.protocol.has_float_coords);
        if flags & DF_MODEL != 0 {
            self.io.code_blob_byte(PLAYER_MODEL);
        }
        if flags & DF_SKIN_NUM != 0 {
            self.io.code_blob_byte(PLAYER_SKIN);
        }
        if flags & DF_EFFECTS != 0 {
            self.io.code_blob_byte(PLAYER_EFFECTS);
        }
        if flags & DF_WEAPON_FRAME != 0 {
            player.weapon_frame = self.io.code_symbol_residual(
                PLAYER_WEAPON_FRAME,
                u32::from(player.weapon_frame),
                player.weapon_frame,
            );
        }
        player.flags = flags;
        player.frame = frame;
    }

    fn packet_entities(&mut self, is_delta: bool) {
        if is_delta {
            let header = self.io.code_blob_residual(ENTITY_HEADER, self.last_header);
            self.last_header = header;
        }
        let mut previous_entity: i32 = -1;
        self.packet_number = self.packet_number.wrapping_add(1);
        while !self.io.has_error {
            let source_word = self.io.peek_u16();
            // a word of 0 ends the packet
            let source_gap = if source_word == 0 {
                0
            } else {
                ((i32::from(source_word & 511) - previous_entity - 1) & 511) + 1
            };
            let gap = i32::from(self.entity_gap(source_gap.cast_unsigned() as u16));
            if gap == 0 {
                self.io.write_u16(0);
                break;
            }
            let entity = ((previous_entity + gap) & 511).cast_unsigned() as usize;
            let state = &self.entities[entity];
            let bits = self.io.symbol(
                ENTITY_BITS,
                u32::from(state.bits) | u32::from(state.model) << 7,
                (source_word >> 9) as u8 ^ state.bits,
            ) ^ state.bits;
            let word = u16::from(bits) << 9 | entity as u16;
            if word == 0 {
                // a word of zero would end the packet; the encoder never emits it
                self.io.has_error = true;
                break;
            }
            self.io.write_u16(word);
            previous_entity = i32::from(word & 511);
            self.entities[entity].bits = (word >> 9) as u8;
            self.entity_delta(u32::from(word));
        }
    }

    fn entity_gap(&mut self, gap: u16) -> u16 {
        let is_escaped = gap >= 255;
        let first = self
            .io
            .blob_byte(ENTITY_GAP, if is_escaped { 255 } else { gap as u8 });
        if first != 255 {
            return u16::from(first);
        }
        let [low, high] = gap.to_le_bytes();
        u16::from_le_bytes([
            self.io.blob_byte(ENTITY_GAP, low),
            self.io.blob_byte(ENTITY_GAP, high),
        ])
    }

    fn entity_delta(&mut self, word: u32) {
        let mut bits = word & !511;
        let mut more = 0u32;
        let mut number = (word & 511) as usize;
        let previous_model = u32::from(self.entities[number].model);
        if bits & U_MORE_BITS != 0 {
            bits |= u32::from(
                self.io
                    .code_symbol(ENTITY_MORE, (bits >> 9) | previous_model << 8),
            );
        }
        if bits & U_FTE_EVEN_MORE != 0 && self.protocol.fte != 0 {
            more = u32::from(self.io.code_symbol(ENTITY_MORE, 0x8000));
            if more & U_FTE_YET_MORE != 0 {
                more |= u32::from(self.io.code_symbol(ENTITY_MORE, 0x8001)) << 8;
            }
        }
        if more & U_FTE_ENTITY_DBL != 0 {
            number += 512;
        }
        if more & U_FTE_ENTITY_DBL_2 != 0 {
            number += 1024;
        }
        let state = &mut self.entities[number];
        let was_in_previous_packet = state.last_packet == self.packet_number.wrapping_sub(1);
        state.last_packet = self.packet_number;
        if bits & U_MODEL != 0 {
            state.model = self.io.code_blob_byte(ENTITY_MODEL);
        } else if more & U_FTE_MODEL_DBL != 0 {
            self.io.code_blob_bytes(ENTITY_MODEL, 2);
        }
        let model = u32::from(state.model);
        if bits & U_FRAME != 0 {
            state.frame = self.io.code_blob_residual(ENTITY_FRAME, state.frame);
        }
        if bits & U_COLOR_MAP != 0 {
            self.io.code_blob_byte(ENTITY_MISC);
        }
        if bits & U_SKIN != 0 {
            self.io.code_blob_byte(ENTITY_MISC);
        }
        if bits & U_EFFECTS != 0 {
            self.io.code_blob_byte(ENTITY_MISC);
        }
        for (axis, (origin_flag, angle_flag)) in ORIGIN_ANGLE_FLAGS.into_iter().enumerate() {
            let is_origin_sent = bits & origin_flag != 0;
            if self.protocol.has_float_coords {
                let stream = ENTITY_FLOAT_ORIGIN + axis;
                let track = &mut state.float_origins[axis];
                self.io.float_origin(stream, axis, is_origin_sent, track);
            } else {
                let track = &mut state.origins[axis];
                if is_origin_sent {
                    let context = clamped_bucket(track.residual(), 17)
                        | clamped_bucket(track.velocity(), 17) << 5
                        | u32::from(was_in_previous_packet) << 10;
                    let (value, residual) =
                        self.io
                            .code_u16(ENTITY_ORIGIN + axis, context, track.predict_linear());
                    track.push(value, residual);
                } else {
                    track.hold();
                }
            }
            if bits & angle_flag != 0 {
                if self.protocol.has_fte_float_coords {
                    self.io.code_symbol(ENTITY_ANGLE, 3 | model << 6);
                    self.io.code_symbol(ENTITY_ANGLE, 3 | model << 6 | 1 << 14);
                } else {
                    let residual_bucket =
                        u32::from(byte_residual_bucket(state.angle_residuals[axis]).min(15));
                    let angle = self.io.code_symbol_residual(
                        ENTITY_ANGLE,
                        axis as u32 | residual_bucket << 2 | model << 6,
                        state.angles[axis],
                    );
                    state.angle_residuals[axis] = angle.wrapping_sub(state.angles[axis]);
                    state.angles[axis] = angle;
                }
            }
        }
        if more & U_FTE_TRANS != 0 && self.protocol.fte & FTE_PEXT_TRANS != 0 {
            self.io.code_blob_byte(ENTITY_MISC);
        }
        if more & U_FTE_SCALE != 0 {
            self.io.code_blob_byte(ENTITY_MISC);
        }
        if more & U_FTE_FATNESS != 0 {
            self.io.code_blob_byte(ENTITY_MISC);
        }
        if more & U_FTE_COLOR_MOD != 0 && self.protocol.fte & FTE_PEXT_COLOR_MOD != 0 {
            self.io.code_blob_bytes(ENTITY_MISC, 3);
        }
    }

    fn sound(&mut self) {
        let low = u32::from(
            self.io
                .code_symbol(SOUND_CHANNEL, u32::from(self.previous_command)),
        );
        let channel = low | u32::from(self.io.code_symbol(SOUND_CHANNEL, 1 << 8 | low)) << 8;
        if channel & u32::from(SND_VOLUME) != 0 {
            self.io.code_blob_byte(SOUND_MISC);
        }
        if channel & u32::from(SND_ATTENUATION) != 0 {
            self.io.code_blob_byte(SOUND_MISC);
        }
        let entity = ((channel >> 3) & 1023) as usize;
        let sound_number = self
            .io
            .code_symbol(SOUND_NUMBER, (channel & 7) | (entity.min(33) as u32) << 3);
        if self.protocol.coord_size() != 2 {
            self.io.copy_blob(SOUND_FLOAT, 12);
            return;
        }
        let (origin, kind) = if let Some(origin) = self.player_origin(entity) {
            (origin, 1)
        } else {
            (self.entities[entity].origins.map(AxisTrack::last), 2)
        };
        for (axis, prediction) in origin.into_iter().enumerate() {
            let context = axis as u32 | kind << 2 | u32::from(sound_number) << 4;
            self.io.code_u16(SOUND_ORIGIN, context, prediction);
        }
    }

    /// The last 16-bit origin of player `entity` (1-based), if it is one.
    fn player_origin(&self, entity: usize) -> Option<[u16; 3]> {
        (1..=32)
            .contains(&entity)
            .then(|| self.players[entity - 1].origins.map(AxisTrack::last))
    }

    fn temp_entity(&mut self) {
        let kind = self.io.code_symbol(
            TEMP_ENTITY_TYPE,
            u32::from(self.previous_temp_entity)
                | u32::from(self.previous_command == SVC_TEMP_ENTITY) << 8,
        );
        self.previous_temp_entity = kind;
        let coord_width = self.protocol.coord_size();
        if coord_width != 2 {
            match kind {
                TE_LIGHTNING_1 | TE_LIGHTNING_2 | TE_LIGHTNING_3 => {
                    self.io.code_blob_bytes(TEMP_ENTITY_MISC, 2);
                    self.io.copy_blob(TEMP_ENTITY_FLOAT, 6 * coord_width);
                }
                TE_GUNSHOT | TE_BLOOD => {
                    self.io.code_blob_byte(TEMP_ENTITY_MISC);
                    self.io.copy_blob(TEMP_ENTITY_FLOAT, 3 * coord_width);
                }
                _ => self.io.copy_blob(TEMP_ENTITY_FLOAT, 3 * coord_width),
            }
            return;
        }
        let kind_context = u32::from(kind) << 4;
        if matches!(kind, TE_LIGHTNING_1 | TE_LIGHTNING_2 | TE_LIGHTNING_3) {
            const BEAM_START: u32 = 1;
            const BEAM_END: u32 = 2;
            let entity = self.io.code_blob_bytes(TEMP_ENTITY_MISC, 2) as usize;
            let origin = self.player_origin(entity).unwrap_or_default();
            let mut start = [0u16; 3];
            for (axis, prediction) in origin.into_iter().enumerate() {
                let context = axis as u32 | BEAM_START << 2 | kind_context;
                start[axis] = self.io.code_u16(TEMP_ENTITY_ORIGIN, context, prediction).0;
            }
            for (axis, axis_start) in start.into_iter().enumerate() {
                let context = axis as u32 | BEAM_END << 2 | kind_context;
                self.temp_entity_last[axis] =
                    self.io.code_u16(TEMP_ENTITY_ORIGIN, context, axis_start).0;
            }
            return;
        }
        if matches!(kind, TE_GUNSHOT | TE_BLOOD) {
            self.io.code_blob_byte(TEMP_ENTITY_MISC);
        }
        for axis in 0..3 {
            let context = axis as u32 | kind_context;
            self.temp_entity_last[axis] = self
                .io
                .code_u16(TEMP_ENTITY_ORIGIN, context, self.temp_entity_last[axis])
                .0;
        }
    }

    /// `dem_multiple` with mask 0: blocks of [u32 len][u16 type][len bytes].
    /// Tags: 1 block, 2 end, 0 escape (raw bytes).
    fn hidden_payload(&mut self) {
        const MVD_HIDDEN_USERCMD: u32 = 1;
        while !self.io.has_error {
            let source_tag = self.io.source_hidden_tag();
            match self.io.blob_byte(HIDDEN_TAG, source_tag) {
                0 => {
                    self.escape();
                    continue;
                }
                1 => {}
                2 => return,
                _ => {
                    self.io.has_error = true;
                    return;
                }
            }
            let block_len = self.io.code_blob_bytes(HIDDEN_LENGTH, 4);
            let block_type = self.io.code_blob_bytes(HIDDEN_TYPE, 2);
            if block_type == MVD_HIDDEN_USERCMD && block_len == 23 {
                self.usercmd();
            } else {
                self.io.copy_blob(HIDDEN_MISC, block_len as usize);
            }
        }
    }

    /// mvdsv `SV_DebugClientCommand`: player, drop, msec, 3 x f32 angles, 3 x i16 moves,
    /// buttons, impulse. Context kind in bits 12-15.
    fn usercmd(&mut self) {
        let [last_player, second_last_player] = self.usercmd_previous_players;
        let player = self.io.code_symbol(
            USERCMD,
            u32::from(last_player.min(31)) | u32::from(second_last_player.min(31)) << 5,
        );
        self.usercmd_previous_players = [player, last_player];
        let state = &mut self.usercmds[player as usize];
        state.drop = self
            .io
            .code_symbol(USERCMD, 1 << 12 | u32::from(state.drop));
        state.msec = self
            .io
            .code_symbol(USERCMD, 2 << 12 | u32::from(state.msec));
        // `hidden_payload` saw all 23 bytes in the payload
        let mut source_grid = 0u8;
        for axis in 0..3 {
            if grid_from_f32_bits(self.io.peek_u32(4 * axis)).is_some() {
                source_grid |= 1 << axis;
            }
        }
        let grid = self
            .io
            .symbol(USERCMD, 3 << 12 | u32::from(state.grid), source_grid);
        if grid > 7 {
            self.io.has_error = true;
            return;
        }
        state.grid = grid;
        for (axis, track) in state.angles.iter_mut().enumerate() {
            if grid & (1 << axis) != 0 {
                self.io.usercmd_grid_angle(axis, track);
            } else {
                for byte_index in 0..4 {
                    self.io
                        .code_symbol(USERCMD, 4 << 12 | (axis as u32) << 2 | byte_index);
                }
            }
        }
        for (axis, axis_move) in state.moves.iter_mut().enumerate() {
            let low = self.io.code_symbol(
                USERCMD,
                5 << 12 | (axis as u32) << 8 | u32::from(*axis_move & 0xff),
            );
            let high = self
                .io
                .code_symbol(USERCMD, 6 << 12 | (axis as u32) << 8 | u32::from(low));
            *axis_move = u16::from_le_bytes([low, high]);
        }
        state.buttons = self
            .io
            .code_symbol(USERCMD, 7 << 12 | u32::from(state.buttons));
        state.impulse = self
            .io
            .code_symbol(USERCMD, 8 << 12 | u32::from(state.impulse));
    }
}

/// The segment, the cursor over it and the streams: everything the walker's primitives
/// touch, apart from its state, so a player or an entity can be coded in place. The
/// direction lives here and in [`Streams`]; the walk has one path. Every `context` fits in
/// 16 bits, the rANS table key (FORMAT.md 6.2).
#[derive(Debug)]
pub(crate) struct SegmentIo<'a, const IS_ENCODE: bool> {
    /// Encode: the segment.
    source: &'a [u8],
    /// Decode: the segment as it is written.
    output: &'a mut [u8],
    offset: usize,
    /// Encode: where the payload being walked ends.
    payload_end: usize,
    has_error: bool,
    streams: Streams<'a, IS_ENCODE>,
}

impl<'a> SegmentIo<'a, true> {
    pub(crate) fn encoder(source: &'a [u8], streams: Streams<'a, true>) -> Self {
        SegmentIo {
            source,
            output: &mut [],
            offset: 0,
            payload_end: 0,
            has_error: false,
            streams,
        }
    }
}

impl<'a> SegmentIo<'a, false> {
    pub(crate) fn decoder(output: &'a mut [u8], streams: Streams<'a, false>) -> Self {
        SegmentIo {
            source: &[],
            output,
            offset: 0,
            payload_end: 0,
            has_error: false,
            streams,
        }
    }
}

impl<const IS_ENCODE: bool> SegmentIo<'_, IS_ENCODE> {
    /// The source for encode, the output for decode.
    fn segment(&self) -> &[u8] {
        if IS_ENCODE { self.source } else { self.output }
    }

    /// The source byte `ahead` of the cursor: 0 past the end, so always 0 for decode.
    #[inline(always)]
    fn peek(&self, ahead: usize) -> u8 {
        self.source.get(self.offset + ahead).copied().unwrap_or(0)
    }

    #[inline(always)]
    fn peek_u16(&self) -> u16 {
        u16::from_le_bytes([self.peek(0), self.peek(1)])
    }

    #[inline(always)]
    fn peek_u32(&self, ahead: usize) -> u32 {
        u32::from_le_bytes(std::array::from_fn(|index| self.peek(ahead + index)))
    }

    /// The source from the cursor to the end of the payload; empty for decode.
    fn payload_rest(&self) -> &[u8] {
        self.source
            .get(self.offset..self.payload_end)
            .unwrap_or_default()
    }

    fn walked_since(&self, start: usize) -> &[u8] {
        self.segment().get(start..self.offset).unwrap_or_default()
    }

    /// Encode steps over the source byte, which is `byte`.
    #[inline(always)]
    fn write(&mut self, byte: u8) {
        if IS_ENCODE {
            self.offset += 1;
            return;
        }
        if let Some(out) = self.output.get_mut(self.offset) {
            *out = byte;
        } else {
            self.has_error = true;
        }
        self.offset += 1;
    }

    #[inline(always)]
    fn write_u16(&mut self, value: u16) {
        for byte in value.to_le_bytes() {
            self.write(byte);
        }
    }

    #[inline(always)]
    fn write_u32(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.write(byte);
        }
    }

    /// The symbol under `context`; `source` is the one encode proposes.
    #[inline(always)]
    fn symbol(&mut self, stream: usize, context: u32, source: u8) -> u8 {
        if let Some(symbol) = self.streams.symbol(stream, context, source) {
            symbol
        } else {
            self.has_error = true;
            // ends every loop of the walk
            255
        }
    }

    #[inline(always)]
    fn residual(&mut self, stream: usize, context: u32, source: i32) -> i32 {
        if let Some(residual) = self.streams.residual(stream, context, source) {
            residual
        } else {
            self.has_error = true;
            0
        }
    }

    /// A byte not in the demo (a tag, a length byte): `source` is the one encode stores.
    #[inline(always)]
    fn blob_byte(&mut self, stream: usize, source: u8) -> u8 {
        if IS_ENCODE {
            self.streams.collect_blob(stream, &[source]);
            return source;
        }
        let mut byte = [0];
        if !self.streams.take_blob(stream, &mut byte) {
            self.has_error = true;
        }
        byte[0]
    }

    #[inline(always)]
    fn code_symbol(&mut self, stream: usize, context: u32) -> u8 {
        let value = self.symbol(stream, context, self.peek(0));
        self.write(value);
        value
    }

    #[inline(always)]
    fn code_symbol_residual(&mut self, stream: usize, context: u32, prediction: u8) -> u8 {
        let source_residual = self.peek(0).wrapping_sub(prediction);
        let value = self
            .symbol(stream, context, source_residual)
            .wrapping_add(prediction);
        self.write(value);
        value
    }

    #[inline(always)]
    fn code_blob_byte(&mut self, stream: usize) -> u8 {
        let value = self.blob_byte(stream, self.peek(0));
        self.write(value);
        value
    }

    #[inline(always)]
    fn code_blob_residual(&mut self, stream: usize, prediction: u8) -> u8 {
        let source_residual = self.peek(0).wrapping_sub(prediction);
        let value = self
            .blob_byte(stream, source_residual)
            .wrapping_add(prediction);
        self.write(value);
        value
    }

    /// The little-endian value of the bytes.
    #[inline]
    fn code_blob_bytes(&mut self, stream: usize, count: usize) -> u32 {
        let mut value = 0u32;
        for index in 0..count {
            value |= u32::from(self.code_blob_byte(stream)) << (8 * index);
        }
        value
    }

    fn copy_blob(&mut self, stream: usize, len: usize) {
        let Some(end) = self.offset.checked_add(len) else {
            self.has_error = true;
            return;
        };
        let is_copied = if IS_ENCODE {
            match self.source.get(self.offset..end) {
                Some(bytes) => {
                    self.streams.collect_blob(stream, bytes);
                    true
                }
                None => false,
            }
        } else {
            match self.output.get_mut(self.offset..end) {
                Some(out) => self.streams.take_blob(stream, out),
                None => false,
            }
        };
        if is_copied {
            self.offset = end;
        } else {
            self.has_error = true;
        }
    }

    /// A 16-bit numeric field and its residual (FORMAT.md 5.1).
    #[inline(always)]
    fn code_u16(&mut self, stream: usize, context: u32, prediction: u16) -> (u16, i32) {
        let source_residual = self.peek_u16().wrapping_sub(prediction).cast_signed();
        let residual = self.residual(stream, context, i32::from(source_residual)) as i16;
        let value = prediction.wrapping_add(residual.cast_unsigned());
        self.write_u16(value);
        (value, i32::from(residual))
    }

    #[inline(always)]
    fn code_u32(&mut self, stream: usize, context: u32, prediction: u32) -> (u32, i32) {
        let source_residual = self.peek_u32(0).wrapping_sub(prediction).cast_signed();
        let residual = self.residual(stream, context, source_residual);
        let value = prediction.wrapping_add(residual.cast_unsigned());
        self.write_u32(value);
        (value, residual)
    }

    /// FORMAT.md 5.2.
    #[inline(always)]
    fn float_origin(&mut self, stream: usize, axis: usize, is_sent: bool, track: &mut FloatTrack) {
        if !is_sent {
            track.hold();
            return;
        }
        let context = clamped_bucket(track.residual(), 63) | (axis as u32) << 6;
        let (bits, residual) = self.code_u32(stream, context, track.predict_linear());
        track.push(bits, residual);
    }

    /// Steps over a payload's length field, the end of the payload known from the source:
    /// the offset of the field, `None` when it does not fit.
    fn start_payload(&mut self) -> Option<usize> {
        let source_len = self.peek_u32(0) as usize;
        let len_offset = self.offset;
        if self.segment().len().saturating_sub(len_offset) < 4 {
            self.has_error = true;
            return None;
        }
        self.offset += 4;
        self.payload_end = self.offset.saturating_add(source_len);
        Some(len_offset)
    }

    /// Encode: the walk must end where the source payload does. Decode: writes the length of
    /// the payload walked since `start_payload`.
    fn end_payload(&mut self, len_offset: usize) {
        if IS_ENCODE {
            if self.offset != self.payload_end {
                self.has_error = true;
            }
            return;
        }
        let len = (self.offset - len_offset - 4) as u32;
        if let Some(out) = self.output.get_mut(len_offset..len_offset + 4) {
            out.copy_from_slice(&len.to_le_bytes());
        }
    }

    /// The command symbol of the source (FORMAT.md 4.5): 255 at the payload end, 0 (an
    /// escape) before a command that cannot be walked.
    fn source_command(&self, protocol: &ProtocolState) -> u8 {
        let Some((&command, body)) = self.payload_rest().split_first() else {
            return 255;
        };
        if command == 0 || protocol.command_body_size(command, body).is_none() {
            return 0;
        }
        command
    }

    /// The hidden-block tag of the source (FORMAT.md 4.11): 2 at the payload end, 0 (an
    /// escape) before a block that cannot be walked, else 1.
    fn source_hidden_tag(&self) -> u8 {
        let rest = self.payload_rest();
        if rest.is_empty() {
            return 2;
        }
        let is_block = rest.len() >= 6
            && read_usize(&mut &rest[..])
                .is_some_and(|block_len| (1..=rest.len() - 6).contains(&block_len));
        u8::from(is_block)
    }

    /// Of the source command whose body is at the cursor; 0 for decode.
    fn source_command_body_len(&self, protocol: &ProtocolState, command: u8) -> u32 {
        let len = protocol.command_body_size(command, self.payload_rest());
        len.unwrap_or(0) as u32
    }

    /// `changed = flags ^ previous`. The main byte: the often-toggling bits (axes 0-5,
    /// weapon frame 10) and an escape bit; only on escape do bits 6-9, 11-14, then 15 follow.
    fn player_info_flags(&mut self, previous: u16) -> u16 {
        let source_changed = self.peek_u16() ^ previous;
        let source_rare_bits = source_changed & !0x43f;
        let source_main = (source_changed & 0x3f) as u8
            | (((source_changed >> 10) & 1) as u8) << 6
            | u8::from(source_rare_bits != 0) << 7;
        // Bit 15 keeps the main contexts clear of rare_byte's (0x100-0x1ff) and bit15_byte's
        // (0x200).
        let context = 0x8000 | u32::from(previous & 0x7fff);
        let main_byte = u16::from(self.symbol(PLAYER_FLAGS, context, source_main));
        let mut changed = (main_byte & 0x3f) | ((main_byte >> 6) & 1) << 10;
        if main_byte & 0x80 != 0 {
            let source_rare =
                ((source_changed >> 6) & 0xf) as u8 | (((source_changed >> 11) & 0xf) as u8) << 4;
            let rare_byte = u16::from(self.symbol(
                PLAYER_FLAGS,
                0x100 | u32::from((previous >> 6) & 0xff),
                source_rare,
            ));
            let bit15_byte =
                u16::from(self.symbol(PLAYER_FLAGS, 0x200, (source_changed >> 15) as u8));
            changed |= (rare_byte & 0xf) << 6 | (rare_byte >> 4) << 11 | (bit15_byte & 1) << 15;
        }
        let flags = changed ^ previous;
        self.write_u16(flags);
        flags
    }

    /// FORMAT.md 5.3 and 5.4.
    fn player_origins_and_angles(
        &mut self,
        player: &mut PlayerState,
        flags: u16,
        time: u32,
        has_float_coords: bool,
    ) {
        let time_deltas = time_deltas(&mut player.frame_times, time);
        if has_float_coords {
            for (axis, track) in player.float_origins.iter_mut().enumerate() {
                let is_sent = flags & DF_ORIGIN << axis != 0;
                self.float_origin(PLAYER_FLOAT_ORIGIN + axis, axis, is_sent, track);
            }
        } else {
            for (axis, track) in player.origins.iter_mut().enumerate() {
                if flags & DF_ORIGIN << axis == 0 {
                    track.hold();
                    continue;
                }
                let context = clamped_bucket(track.residual(), 19)
                    | clamped_bucket(track.velocity(), 19) << 5;
                let prediction = track.predict_time_scaled(time_deltas);
                let (value, residual) = self.code_u16(PLAYER_ORIGIN + axis, context, prediction);
                track.push(value, residual);
            }
        }
        for (axis, track) in player.angles.iter_mut().enumerate() {
            if flags & DF_ANGLES << axis == 0 {
                track.hold();
                continue;
            }
            let context = clamped_bucket(track.residual(), 23)
                | clamped_bucket(track.previous_velocity(), 23) << 5;
            let prediction = track.predict_time_scaled(time_deltas);
            let (value, residual) = self.code_u16(PLAYER_ANGLE + axis, context, prediction);
            track.push(value, residual);
        }
    }

    /// FORMAT.md 5.6.
    fn usercmd_grid_angle(&mut self, axis: usize, track: &mut AxisTrack) {
        let context = clamped_bucket(track.residual(), 23)
            | clamped_bucket(track.velocity(), 23) << 5
            | clamped_bucket(track.previous_velocity(), 15) << 10
            | (axis as u32) << 14;
        let prediction = track.predict_linear();
        // `usercmd` saw the source on the grid
        let source_value = grid_from_f32_bits(self.peek_u32(0)).unwrap_or_default();
        let source_residual = source_value.wrapping_sub(prediction).cast_signed();
        let residual = self.residual(USERCMD_ANGLE, context, i32::from(source_residual)) as i16;
        let grid_value = prediction.wrapping_add(residual.cast_unsigned());
        self.write_u32(grid_to_f32_bits(grid_value));
        track.push(grid_value, i32::from(residual));
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PlayerState {
    flags: u16,
    frame: u8,
    weapon_frame: u8,
    origins: [AxisTrack; 3], // FORMAT.md 5.3
    angles: [AxisTrack; 3],  // FORMAT.md 5.4
    frame_times: (u32, u32),
    float_origins: [FloatTrack; 3], // FORMAT.md 5.2
}

/// The previous usercmd fields of one player (FORMAT.md 4.11).
#[derive(Debug, Clone, Copy, Default)]
struct UsercmdState {
    drop: u8,
    msec: u8,
    grid: u8,
    buttons: u8,
    impulse: u8,
    moves: [u16; 3],
    angles: [AxisTrack; 3], // FORMAT.md 5.6
}

#[derive(Debug, Clone, Copy, Default)]
struct EntityState {
    angles: [u8; 3],
    angle_residuals: [u8; 3],
    frame: u8,
    model: u8,
    bits: u8,
    last_packet: u32,
    origins: [AxisTrack; 3],        // FORMAT.md 5.5
    float_origins: [FloatTrack; 3], // FORMAT.md 5.2
}

#[inline(always)]
fn command_class(command: u8) -> u8 {
    const SVC_BAD: u8 = 0;
    match command {
        SVC_BAD => 0,
        SVC_PLAYER_INFO => 1,
        SVC_DELTA_PACKET_ENTITIES => 2,
        SVC_PACKET_ENTITIES => 3,
        SVC_SOUND => 4,
        SVC_TEMP_ENTITY => 5,
        SVC_UPDATE_STAT => 6,
        SVC_UPDATE_STAT_LONG => 7,
        SVC_PRINT => 8,
        SVC_SET_INFO => 9,
        SVC_UPDATE_FRAGS => 10,
        SVC_UPDATE_PING => 11,
        SVC_STUFF_TEXT => 12,
        SVC_NAILS | SVC_NAILS_2 => 13,
        SVC_DAMAGE => 14,
        _ => 15,
    }
}
