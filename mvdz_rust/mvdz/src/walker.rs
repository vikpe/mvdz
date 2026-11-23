//! The walker, one routine both ways: every field of a segment in demo order, the numeric
//! ones as residuals against the predictions of `numeric.rs`.

use crate::fields::{read_u16, read_u32, read_usize};
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
    frame_len,
};
use crate::streams::{
    COMMAND, ENTITY_ANGLE, ENTITY_BITS, ENTITY_FLOAT_ORIGIN, ENTITY_FRAME, ENTITY_GAP,
    ENTITY_HEADER, ENTITY_MISC, ENTITY_MODEL, ENTITY_MORE, ENTITY_ORIGIN, ESCAPE_LENGTH, GENERIC,
    HIDDEN_LENGTH, HIDDEN_MISC, HIDDEN_TAG, HIDDEN_TYPE, MASK, NAILS, PLAYER_ANGLE, PLAYER_EFFECTS,
    PLAYER_FLAGS, PLAYER_FLOAT_ORIGIN, PLAYER_FRAME, PLAYER_MODEL, PLAYER_NUMBER, PLAYER_ORIGIN,
    PLAYER_SKIN, PLAYER_WEAPON_FRAME, RAW, SET, SOUND_CHANNEL, SOUND_FLOAT, SOUND_MISC,
    SOUND_NUMBER, SOUND_ORIGIN, STAT_INDEX, STAT_VALUE, TAIL, TEMP_ENTITY_FLOAT, TEMP_ENTITY_MISC,
    TEMP_ENTITY_ORIGIN, TEMP_ENTITY_TYPE, TIME, TYPE, USERCMD, USERCMD_ANGLE,
};

/// Every `context` fits in 16 bits, the rANS table key (FORMAT.md section 6.2).
pub(crate) trait SymbolIo {
    /// `None`: the stream is absent or the context has an empty table.
    fn symbol(&mut self, stream: usize, context: u32, value: u8) -> Option<u8>;
    /// As [`Self::symbol`], for a residual of a numeric stream.
    fn residual(&mut self, stream: usize, context: u32, value: i32) -> Option<i32>;
    fn put_blob(&mut self, stream: usize, bytes: &[u8]);
    fn take_blob(&mut self, stream: usize, len: usize) -> Option<&[u8]>;
    fn blob_rest(&self, stream: usize) -> &[u8];
    #[cfg(feature = "stats")]
    fn is_generic(&self, _command: u8) -> bool {
        false
    }
    #[cfg(feature = "stats")]
    fn is_hidden_misc(&self, _block_type: u32) -> bool {
        false
    }
}

/// FORMAT.md sections 5.3 and 5.4.
#[derive(Debug, Clone, Copy, Default)]
struct PlayerTracks {
    origins: [AxisTrack; 3],
    angles: [AxisTrack; 3],
    /// Of the player's last two records.
    frame_times: (u32, u32),
}

#[derive(Debug, Clone, Copy, Default)]
struct PlayerState {
    flags: u16,
    frame: u8,
    weapon_frame: u8,
}

/// The previous usercmd fields of one player (FORMAT.md section 4.11).
#[derive(Debug, Clone, Copy, Default)]
struct UsercmdState {
    drop: u8,
    msec: u8,
    grid: u8,
    buttons: u8,
    impulse: u8,
    moves: [u16; 3],
}

#[derive(Debug, Clone, Copy, Default)]
struct EntityState {
    angles: [u8; 3],
    angle_residuals: [u8; 3],
    frame: u8,
    model: u8,
    bits: u8,
    last_packet: u32,
}

#[derive(Debug)]
pub(crate) struct SegmentWalker<'a, const IS_ENCODE: bool, IO: SymbolIo> {
    src: &'a [u8],
    dst: &'a mut [u8],
    offset: usize,
    pub(crate) has_error: bool,
    pub(crate) io: IO,
    pub(crate) protocol: ProtocolState,
    players: Box<[PlayerState; 256]>,
    entities: Box<[EntityState; 2048]>,
    stats: Box<[[u8; 256]; 32]>,
    player_tracks: Vec<PlayerTracks>,
    player_float_origins: Vec<[FloatTrack; 3]>,
    entity_origins: Vec<[AxisTrack; 3]>,
    entity_float_origins: Vec<[FloatTrack; 3]>,
    usercmd_tracks: Vec<[AxisTrack; 3]>,
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

impl<'a, IO: SymbolIo> SegmentWalker<'a, true, IO> {
    pub(crate) fn encoder(src: &'a [u8], io: IO, protocol: ProtocolState) -> Self {
        Self::new(src, &mut [], io, protocol)
    }
}

impl<'a, IO: SymbolIo> SegmentWalker<'a, false, IO> {
    pub(crate) fn decoder(dst: &'a mut [u8], io: IO, protocol: ProtocolState) -> Self {
        Self::new(&[], dst, io, protocol)
    }
}

impl<'a, const IS_ENCODE: bool, IO: SymbolIo> SegmentWalker<'a, IS_ENCODE, IO> {
    #[expect(
        clippy::large_stack_arrays,
        reason = "fixed-size tables keep byte-indexed lookups free of bounds checks"
    )]
    fn new(src: &'a [u8], dst: &'a mut [u8], io: IO, protocol: ProtocolState) -> Self {
        SegmentWalker {
            src,
            dst,
            offset: 0,
            has_error: false,
            io,
            protocol,
            players: Box::new([PlayerState::default(); 256]),
            entities: Box::new([EntityState::default(); 2048]),
            stats: Box::new([[0u8; 256]; 32]),
            player_tracks: vec![PlayerTracks::default(); 256],
            player_float_origins: vec![[FloatTrack::default(); 3]; 256],
            entity_origins: vec![[AxisTrack::default(); 3]; 2048],
            entity_float_origins: vec![[FloatTrack::default(); 3]; 2048],
            usercmd_tracks: vec![[AxisTrack::default(); 3]; 256],
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

    #[inline(always)]
    fn read_byte(&mut self) -> u8 {
        if let Some(&byte) = self.src.get(self.offset) {
            self.offset += 1;
            byte
        } else {
            self.has_error = true;
            0
        }
    }

    #[inline(always)]
    fn write_byte(&mut self, byte: u8) {
        if let Some(out) = self.dst.get_mut(self.offset) {
            *out = byte;
        } else {
            self.has_error = true;
        }
        self.offset += 1;
    }

    #[inline(always)]
    fn write_u16(&mut self, value: u16) {
        let [low, high] = value.to_le_bytes();
        self.write_byte(low);
        self.write_byte(high);
    }

    #[inline(always)]
    fn read_u32(&mut self) -> u32 {
        u32::from_le_bytes(std::array::from_fn(|_| self.read_byte()))
    }

    #[inline(always)]
    fn write_u32(&mut self, value: u32) {
        for byte in value.to_le_bytes() {
            self.write_byte(byte);
        }
    }

    /// The last 16-bit origin of player `entity` (1-based), if it is one.
    fn player_origin(&self, entity: usize) -> Option<[u16; 3]> {
        (1..=32)
            .contains(&entity)
            .then(|| self.player_tracks[entity - 1].origins.map(AxisTrack::last))
    }

    /// A 16-bit numeric field and its residual (FORMAT.md section 5.1).
    #[inline(always)]
    fn code_u16(&mut self, stream: usize, context: u32, prediction: u16) -> (u16, i32) {
        if IS_ENCODE {
            let value = u16::from_le_bytes([self.read_byte(), self.read_byte()]);
            let residual = i32::from(value.wrapping_sub(prediction).cast_signed());
            self.residual(stream, context, residual);
            (value, residual)
        } else {
            let residual = self.residual(stream, context, 0);
            let value = prediction.wrapping_add(residual.cast_unsigned() as u16);
            self.write_u16(value);
            (value, i32::from(residual as i16))
        }
    }

    #[inline(always)]
    fn code_u32(&mut self, stream: usize, context: u32, prediction: u32) -> (u32, i32) {
        if IS_ENCODE {
            let value = self.read_u32();
            let residual = value.wrapping_sub(prediction).cast_signed();
            self.residual(stream, context, residual);
            (value, residual)
        } else {
            let residual = self.residual(stream, context, 0);
            let value = prediction.wrapping_add(residual.cast_unsigned());
            self.write_u32(value);
            (value, residual)
        }
    }

    /// FORMAT.md section 5.2.
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

    #[inline(always)]
    fn symbol(&mut self, stream: usize, context: u32, value: u8) -> u8 {
        if let Some(symbol) = self.io.symbol(stream, context, value) {
            symbol
        } else {
            self.has_error = true;
            // ends every loop of the walk
            255
        }
    }

    #[inline(always)]
    fn residual(&mut self, stream: usize, context: u32, value: i32) -> i32 {
        if let Some(residual) = self.io.residual(stream, context, value) {
            residual
        } else {
            self.has_error = true;
            0
        }
    }

    /// Encode hands the source byte to `code`; decode writes what `code` returns.
    #[inline(always)]
    fn code_byte(&mut self, code: impl FnOnce(&mut Self, u8) -> u8) -> u8 {
        if IS_ENCODE {
            let byte = self.read_byte();
            code(self, byte);
            byte
        } else {
            let byte = code(self, 0);
            self.write_byte(byte);
            byte
        }
    }

    #[inline(always)]
    fn code_symbol(&mut self, stream: usize, context: u32) -> u8 {
        self.code_byte(|walker, source_byte| walker.symbol(stream, context, source_byte))
    }

    #[inline(always)]
    fn code_symbol_residual(&mut self, stream: usize, context: u32, prediction: u8) -> u8 {
        self.code_byte(|walker, source_byte| {
            let residual = walker.symbol(stream, context, source_byte.wrapping_sub(prediction));
            residual.wrapping_add(prediction)
        })
    }

    #[inline(always)]
    fn code_blob_byte(&mut self, stream: usize) -> u8 {
        self.code_byte(|walker, source_byte| walker.blob_byte(stream, source_byte))
    }

    #[inline(always)]
    fn code_blob_residual(&mut self, stream: usize, prediction: u8) -> u8 {
        self.code_byte(|walker, source_byte| {
            let residual = walker.blob_byte(stream, source_byte.wrapping_sub(prediction));
            residual.wrapping_add(prediction)
        })
    }

    #[inline]
    fn code_blob_bytes(&mut self, stream: usize, count: usize) -> u32 {
        let mut value = 0u32;
        for index in 0..count {
            value |= u32::from(self.code_blob_byte(stream)) << (8 * index);
        }
        value
    }

    /// A byte not in the demo (a tag, a length byte): encode stores `value`, decode returns
    /// the stored byte.
    #[inline(always)]
    fn blob_byte(&mut self, stream: usize, value: u8) -> u8 {
        if IS_ENCODE {
            self.io.put_blob(stream, &[value]);
            value
        } else if let Some(bytes) = self.io.take_blob(stream, 1) {
            bytes[0]
        } else {
            self.has_error = true;
            0
        }
    }

    fn copy_blob(&mut self, stream: usize, len: usize) {
        if IS_ENCODE {
            if len > self.src.len() - self.offset.min(self.src.len()) {
                self.has_error = true;
                return;
            }
            self.io
                .put_blob(stream, &self.src[self.offset..self.offset + len]);
            self.offset += len;
        } else {
            let start = self.offset;
            match (
                self.io.take_blob(stream, len),
                self.dst.get_mut(start..start + len),
            ) {
                (Some(blob), Some(out)) => {
                    out.copy_from_slice(blob);
                    self.offset += len;
                }
                _ => self.has_error = true,
            }
        }
    }

    /// The tail is the bytes after the last whole frame: all of the `TAIL` blob stream.
    pub(crate) fn run(&mut self) {
        if IS_ENCODE {
            while !self.has_error && frame_len(self.src, self.offset).is_some() {
                self.frame();
            }
            if self.has_error {
                return;
            }
            self.copy_blob(TAIL, self.src.len() - self.offset);
        } else {
            let tail = self.io.blob_rest(TAIL).len();
            let stop = self.dst.len().saturating_sub(tail);
            while !self.has_error && self.offset < stop {
                self.frame();
            }
            if self.offset != stop {
                self.has_error = true;
            }
            self.copy_blob(TAIL, tail);
        }
    }

    fn frame(&mut self) {
        let last_type = u32::from(self.last_type);
        let time_byte = self.code_symbol(TIME, last_type | u32::from(self.last_time_byte) << 8);
        self.last_time_byte = time_byte;
        self.time = self.time.wrapping_add(u32::from(time_byte));
        let frame_type = self.code_symbol(TYPE, last_type | u32::from(time_byte.min(32)) << 8);
        self.last_type = frame_type;
        self.target = frame_type >> 3;
        match frame_type & 7 {
            DEM_SET => self.copy_blob(SET, 8),
            DEM_MULTIPLE => {
                let mask = self.code_blob_bytes(MASK, 4);
                self.length_prefixed(frame_type, mask == 0);
            }
            DEM_READ | DEM_SINGLE | DEM_STATS | DEM_ALL => self.length_prefixed(frame_type, false),
            _ => self.has_error = true,
        }
    }

    fn length_prefixed(&mut self, frame_type: u8, is_hidden: bool) {
        if IS_ENCODE {
            let Some(len) = read_usize(&mut &self.src[self.offset..]) else {
                self.has_error = true;
                return;
            };
            self.offset += 4;
            let end = self.offset + len;
            if end > self.src.len() {
                self.has_error = true;
                return;
            }
            if is_hidden {
                self.hidden_payload(end);
            } else {
                self.payload(end, frame_type);
            }
        } else {
            let len_offset = self.offset;
            self.offset += 4;
            if self.offset > self.dst.len() {
                self.has_error = true;
                return;
            }
            if is_hidden {
                self.hidden_payload(0);
            } else {
                self.payload(0, frame_type);
            }
            let len = (self.offset - len_offset - 4) as u32;
            if let Some(out) = self.dst.get_mut(len_offset..len_offset + 4) {
                out.copy_from_slice(&len.to_le_bytes());
            }
        }
    }

    fn escape(&mut self, end: usize) {
        let source_len = if IS_ENCODE {
            (end - self.offset) as u32
        } else {
            0
        };
        let mut len = 0u32;
        for (index, source_byte) in source_len.to_le_bytes().into_iter().enumerate() {
            len |= u32::from(self.blob_byte(ESCAPE_LENGTH, source_byte)) << (8 * index);
        }
        self.copy_blob(RAW, len as usize);
    }

    fn payload(&mut self, end: usize, frame_type: u8) {
        self.previous_command = 0;
        self.stat_previous_index = 255;
        self.stat_previous_residual = 0;
        let mut command_index = 0u32;
        let type_index = (frame_type & 7) as usize;
        let previous_first_class = u32::from(command_class(self.first_command_by_type[type_index]));
        let mut first_command = 255u8;
        loop {
            if self.has_error {
                return;
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
            command_index += 1;
            if command_index == 2 {
                first_command = self.previous_command;
            }
            if command_index >= 2 {
                self.first_command_by_type[type_index] = first_command;
            } else {
                self.first_command_by_type[type_index] = 255;
            }
            if IS_ENCODE {
                if self.offset == end {
                    self.symbol(COMMAND, context, 255);
                    return;
                }
                let command = self.src[self.offset];
                let size = self
                    .protocol
                    .command_body_size(command, &self.src[self.offset + 1..end]);
                let size = match size {
                    Some(size) if command != 0 => size,
                    _ => {
                        self.symbol(COMMAND, context, 0);
                        self.escape(end);
                        return;
                    }
                };
                self.symbol(COMMAND, context, command);
                self.offset += 1;
                let start = self.offset;
                self.command(command);
                if self.offset != start + size {
                    self.has_error = true;
                    return;
                }
                self.previous_command = command;
            } else {
                let command = self.symbol(COMMAND, context, 0);
                match command {
                    255 => return,
                    0 => {
                        self.escape(0);
                        return;
                    }
                    _ => {}
                }
                self.write_byte(command);
                self.command(command);
                self.previous_command = command;
            }
        }
    }

    fn copy_command_body(&mut self, stream: usize, size_command: u8) -> usize {
        let start = self.offset;
        let size = if IS_ENCODE {
            self.protocol
                .command_body_size(size_command, &self.src[self.offset.min(self.src.len())..])
        } else {
            self.protocol
                .command_body_size(size_command, self.io.blob_rest(stream))
        };
        match size {
            Some(size) => self.copy_blob(stream, size),
            None => self.has_error = true,
        }
        start
    }

    fn command(&mut self, command: u8) {
        #[cfg(feature = "stats")]
        if self.io.is_generic(command) {
            self.copy_command_body(GENERIC + command as usize, command);
            return;
        }
        match command {
            SVC_PLAYER_INFO => self.player_info(),
            SVC_PACKET_ENTITIES | SVC_DELTA_PACKET_ENTITIES => {
                self.packet_entities(command == SVC_DELTA_PACKET_ENTITIES);
            }
            SVC_UPDATE_STAT => {
                let player = (self.target & 31) as usize;
                let stat = self.code_symbol(
                    STAT_INDEX,
                    u32::from(self.stat_previous_index) | u32::from(self.previous_command) << 8,
                );
                let previous = self.stats[player][stat as usize];
                let value = self.code_symbol_residual(
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
            SVC_NAILS | SVC_NAILS_2 => {
                self.copy_command_body(NAILS, command);
            }
            SVC_SERVER_DATA => {
                let start =
                    self.copy_command_body(GENERIC + usize::from(SVC_SERVER_DATA), SVC_SERVER_DATA);
                if !self.has_error {
                    let end = self.offset;
                    let body: &[u8] = if IS_ENCODE {
                        &self.src[start..end]
                    } else {
                        self.dst.get(start..end).unwrap_or_default()
                    };
                    let mut protocol = self.protocol;
                    protocol.observe_server_data(body);
                    self.protocol = protocol;
                }
            }
            _ => {
                self.copy_command_body(GENERIC + command as usize, command);
            }
        }
    }

    /// `changed = flags ^ previous`. The main byte: the often-toggling bits (axes 0-5,
    /// weapon frame 10) and an escape bit; only on escape do bits 6-9, 11-14, then 15 follow.
    fn player_info_flags(&mut self, previous: u16) -> u16 {
        // Bit 15 keeps the main contexts clear of rare_byte's (0x100-0x1ff) and bit15_byte's
        // (0x200).
        let context = 0x8000 | u32::from(previous & 0x7fff);
        let changed = if IS_ENCODE {
            let low = u16::from(self.read_byte());
            let changed = (low | u16::from(self.read_byte()) << 8) ^ previous;
            let rare_bits = changed & !0x43f;
            let main_byte = (changed & 0x3f) as u8
                | (((changed >> 10) & 1) as u8) << 6
                | u8::from(rare_bits != 0) << 7;
            self.symbol(PLAYER_FLAGS, context, main_byte);
            if rare_bits != 0 {
                let rare_byte = ((changed >> 6) & 0xf) as u8 | (((changed >> 11) & 0xf) as u8) << 4;
                self.symbol(
                    PLAYER_FLAGS,
                    0x100 | u32::from((previous >> 6) & 0xff),
                    rare_byte,
                );
                self.symbol(PLAYER_FLAGS, 0x200, (changed >> 15) as u8);
            }
            changed
        } else {
            let main_byte = u16::from(self.symbol(PLAYER_FLAGS, context, 0));
            let mut changed = (main_byte & 0x3f) | ((main_byte >> 6) & 1) << 10;
            if main_byte & 0x80 != 0 {
                let rare_byte = u16::from(self.symbol(
                    PLAYER_FLAGS,
                    0x100 | u32::from((previous >> 6) & 0xff),
                    0,
                ));
                let bit15_byte = u16::from(self.symbol(PLAYER_FLAGS, 0x200, 0));
                changed |= (rare_byte & 0xf) << 6 | (rare_byte >> 4) << 11 | (bit15_byte & 1) << 15;
            }
            self.write_u16(changed ^ previous);
            changed
        };
        changed ^ previous
    }

    fn player_info(&mut self) {
        const DF_DEAD: u16 = 1 << 8;
        const DF_GIB: u16 = 1 << 9;
        let number = self.code_blob_byte(PLAYER_NUMBER);
        let previous = self.players[number as usize];
        let flags = self.player_info_flags(previous.flags);
        let frame = self.code_symbol_residual(
            PLAYER_FRAME,
            u32::from(previous.frame) | u32::from(flags & (DF_DEAD | DF_GIB)),
            previous.frame,
        );
        self.player_origins_and_angles(number, flags);
        if flags & DF_MODEL != 0 {
            self.code_blob_byte(PLAYER_MODEL);
        }
        if flags & DF_SKIN_NUM != 0 {
            self.code_blob_byte(PLAYER_SKIN);
        }
        if flags & DF_EFFECTS != 0 {
            self.code_blob_byte(PLAYER_EFFECTS);
        }
        let weapon_frame = if flags & DF_WEAPON_FRAME != 0 {
            self.code_symbol_residual(
                PLAYER_WEAPON_FRAME,
                u32::from(previous.weapon_frame),
                previous.weapon_frame,
            )
        } else {
            previous.weapon_frame
        };
        self.players[number as usize] = PlayerState {
            flags,
            frame,
            weapon_frame,
        };
    }

    /// FORMAT.md sections 5.3 and 5.4.
    fn player_origins_and_angles(&mut self, number: u8, flags: u16) {
        let player = number as usize;
        let mut tracks = self.player_tracks[player];
        let time_deltas = time_deltas(&mut tracks.frame_times, self.time);
        if self.protocol.has_float_coords {
            let mut float_origins = self.player_float_origins[player];
            for (axis, track) in float_origins.iter_mut().enumerate() {
                let is_sent = flags & DF_ORIGIN << axis != 0;
                self.float_origin(PLAYER_FLOAT_ORIGIN + axis, axis, is_sent, track);
            }
            self.player_float_origins[player] = float_origins;
        } else {
            for (axis, track) in tracks.origins.iter_mut().enumerate() {
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
        for (axis, track) in tracks.angles.iter_mut().enumerate() {
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
        self.player_tracks[player] = tracks;
    }

    fn packet_entities(&mut self, is_delta: bool) {
        if is_delta {
            let header = self.code_blob_residual(ENTITY_HEADER, self.last_header);
            self.last_header = header;
        }
        let mut previous_entity: i32 = -1;
        self.packet_number = self.packet_number.wrapping_add(1);
        loop {
            if self.has_error {
                break;
            }
            let word: u16;
            let entity: usize;
            if IS_ENCODE {
                let Some(read_word) = read_u16(&mut &self.src[self.offset..]) else {
                    self.has_error = true;
                    return;
                };
                word = read_word;
                self.offset += 2;
                let gap = if word == 0 {
                    0
                } else {
                    ((i32::from(word & 511) - previous_entity - 1) & 511) + 1
                };
                self.entity_gap(gap.cast_unsigned() as u16);
                if word == 0 {
                    break;
                }
                entity = (word & 511) as usize;
                let state = self.entities[entity];
                self.symbol(
                    ENTITY_BITS,
                    u32::from(state.bits) | u32::from(state.model) << 7,
                    (word >> 9) as u8 ^ state.bits,
                );
            } else {
                let gap = i32::from(self.entity_gap(0));
                if gap == 0 {
                    self.write_u16(0);
                    break;
                }
                entity = ((previous_entity + gap) & 511).cast_unsigned() as usize;
                let state = self.entities[entity];
                let bits = self.symbol(
                    ENTITY_BITS,
                    u32::from(state.bits) | u32::from(state.model) << 7,
                    0,
                ) ^ state.bits;
                word = u16::from(bits) << 9 | entity as u16;
                if word == 0 {
                    // a word of zero would end the packet; the encoder never emits it
                    self.has_error = true;
                    break;
                }
                self.write_u16(word);
            }
            previous_entity = i32::from(word & 511);
            self.entities[entity].bits = (word >> 9) as u8;
            self.entity_delta(u32::from(word));
        }
    }

    fn entity_gap(&mut self, gap: u16) -> u16 {
        let is_escaped = gap >= 255;
        let first = self.blob_byte(ENTITY_GAP, if is_escaped { 255 } else { gap as u8 });
        if first != 255 {
            return u16::from(first);
        }
        let [low, high] = gap.to_le_bytes();
        u16::from_le_bytes([
            self.blob_byte(ENTITY_GAP, low),
            self.blob_byte(ENTITY_GAP, high),
        ])
    }

    fn entity_delta(&mut self, word: u32) {
        let mut bits = word & !511;
        let mut more = 0u32;
        let mut number = (word & 511) as usize;
        let previous_model = u32::from(self.entities[number].model);
        if bits & U_MORE_BITS != 0 {
            bits |= u32::from(self.code_symbol(ENTITY_MORE, (bits >> 9) | previous_model << 8));
        }
        if bits & U_FTE_EVEN_MORE != 0 && self.protocol.fte != 0 {
            more = u32::from(self.code_symbol(ENTITY_MORE, 0x8000));
            if more & U_FTE_YET_MORE != 0 {
                more |= u32::from(self.code_symbol(ENTITY_MORE, 0x8001)) << 8;
            }
        }
        if more & U_FTE_ENTITY_DBL != 0 {
            number += 512;
        }
        if more & U_FTE_ENTITY_DBL_2 != 0 {
            number += 1024;
        }
        let was_in_previous_packet =
            self.entities[number].last_packet == self.packet_number.wrapping_sub(1);
        self.entities[number].last_packet = self.packet_number;
        let mut state = self.entities[number];
        if bits & U_MODEL != 0 {
            state.model = self.code_blob_byte(ENTITY_MODEL);
        } else if more & U_FTE_MODEL_DBL != 0 {
            self.code_blob_bytes(ENTITY_MODEL, 2);
        }
        let model = u32::from(state.model);
        if bits & U_FRAME != 0 {
            state.frame = self.code_blob_residual(ENTITY_FRAME, state.frame);
        }
        if bits & U_COLOR_MAP != 0 {
            self.code_blob_byte(ENTITY_MISC);
        }
        if bits & U_SKIN != 0 {
            self.code_blob_byte(ENTITY_MISC);
        }
        if bits & U_EFFECTS != 0 {
            self.code_blob_byte(ENTITY_MISC);
        }
        for (axis, (origin_flag, angle_flag)) in ORIGIN_ANGLE_FLAGS.into_iter().enumerate() {
            let is_origin_sent = bits & origin_flag != 0;
            if self.protocol.has_float_coords {
                let mut track = self.entity_float_origins[number][axis];
                let stream = ENTITY_FLOAT_ORIGIN + axis;
                self.float_origin(stream, axis, is_origin_sent, &mut track);
                self.entity_float_origins[number][axis] = track;
            } else {
                let mut track = self.entity_origins[number][axis];
                if is_origin_sent {
                    let context = clamped_bucket(track.residual(), 17)
                        | clamped_bucket(track.velocity(), 17) << 5
                        | u32::from(was_in_previous_packet) << 10;
                    let (value, residual) =
                        self.code_u16(ENTITY_ORIGIN + axis, context, track.predict_linear());
                    track.push(value, residual);
                } else {
                    track.hold();
                }
                self.entity_origins[number][axis] = track;
            }
            if bits & angle_flag != 0 {
                if self.protocol.has_fte_float_coords {
                    self.code_symbol(ENTITY_ANGLE, 3 | model << 6);
                    self.code_symbol(ENTITY_ANGLE, 3 | model << 6 | 1 << 14);
                } else {
                    let residual_bucket =
                        u32::from(byte_residual_bucket(state.angle_residuals[axis]).min(15));
                    let angle = self.code_symbol_residual(
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
            self.code_blob_byte(ENTITY_MISC);
        }
        if more & U_FTE_SCALE != 0 {
            self.code_blob_byte(ENTITY_MISC);
        }
        if more & U_FTE_FATNESS != 0 {
            self.code_blob_byte(ENTITY_MISC);
        }
        if more & U_FTE_COLOR_MOD != 0 && self.protocol.fte & FTE_PEXT_COLOR_MOD != 0 {
            self.code_blob_bytes(ENTITY_MISC, 3);
        }
        self.entities[number] = state;
    }

    fn sound(&mut self) {
        let low = u32::from(self.code_symbol(SOUND_CHANNEL, u32::from(self.previous_command)));
        let channel = low | u32::from(self.code_symbol(SOUND_CHANNEL, 1 << 8 | low)) << 8;
        if channel & u32::from(SND_VOLUME) != 0 {
            self.code_blob_byte(SOUND_MISC);
        }
        if channel & u32::from(SND_ATTENUATION) != 0 {
            self.code_blob_byte(SOUND_MISC);
        }
        let entity = ((channel >> 3) & 1023) as usize;
        let sound_number =
            self.code_symbol(SOUND_NUMBER, (channel & 7) | (entity.min(33) as u32) << 3);
        if self.protocol.coord_size() != 2 {
            self.copy_blob(SOUND_FLOAT, 12);
            return;
        }
        let (origin, kind) = if let Some(origin) = self.player_origin(entity) {
            (origin, 1)
        } else {
            (self.entity_origins[entity].map(AxisTrack::last), 2)
        };
        for (axis, prediction) in origin.into_iter().enumerate() {
            let context = axis as u32 | kind << 2 | u32::from(sound_number) << 4;
            self.code_u16(SOUND_ORIGIN, context, prediction);
        }
    }

    fn temp_entity(&mut self) {
        let kind = self.code_symbol(
            TEMP_ENTITY_TYPE,
            u32::from(self.previous_temp_entity)
                | u32::from(self.previous_command == SVC_TEMP_ENTITY) << 8,
        );
        self.previous_temp_entity = kind;
        let coord_width = self.protocol.coord_size();
        if coord_width != 2 {
            match kind {
                TE_LIGHTNING_1 | TE_LIGHTNING_2 | TE_LIGHTNING_3 => {
                    self.code_blob_bytes(TEMP_ENTITY_MISC, 2);
                    self.copy_blob(TEMP_ENTITY_FLOAT, 6 * coord_width);
                }
                TE_GUNSHOT | TE_BLOOD => {
                    self.code_blob_byte(TEMP_ENTITY_MISC);
                    self.copy_blob(TEMP_ENTITY_FLOAT, 3 * coord_width);
                }
                _ => self.copy_blob(TEMP_ENTITY_FLOAT, 3 * coord_width),
            }
            return;
        }
        let kind_context = u32::from(kind) << 4;
        if matches!(kind, TE_LIGHTNING_1 | TE_LIGHTNING_2 | TE_LIGHTNING_3) {
            const BEAM_START: u32 = 1;
            const BEAM_END: u32 = 2;
            let entity = self.code_blob_bytes(TEMP_ENTITY_MISC, 2) as usize;
            let origin = self.player_origin(entity).unwrap_or_default();
            let mut start = [0u16; 3];
            for (axis, prediction) in origin.into_iter().enumerate() {
                let context = axis as u32 | BEAM_START << 2 | kind_context;
                start[axis] = self.code_u16(TEMP_ENTITY_ORIGIN, context, prediction).0;
            }
            for (axis, axis_start) in start.into_iter().enumerate() {
                let context = axis as u32 | BEAM_END << 2 | kind_context;
                self.temp_entity_last[axis] =
                    self.code_u16(TEMP_ENTITY_ORIGIN, context, axis_start).0;
            }
            return;
        }
        if matches!(kind, TE_GUNSHOT | TE_BLOOD) {
            self.code_blob_byte(TEMP_ENTITY_MISC);
        }
        for axis in 0..3 {
            let context = axis as u32 | kind_context;
            self.temp_entity_last[axis] = self
                .code_u16(TEMP_ENTITY_ORIGIN, context, self.temp_entity_last[axis])
                .0;
        }
    }

    /// `dem_multiple` with mask 0: blocks of [u32 len][u16 type][len bytes].
    /// Tags: 1 block, 2 end, 0 escape (rest raw).
    fn hidden_payload(&mut self, end: usize) {
        const MVD_HIDDEN_USERCMD: u32 = 1;
        loop {
            if self.has_error {
                return;
            }
            if IS_ENCODE {
                let remaining = end - self.offset;
                if remaining == 0 {
                    self.blob_byte(HIDDEN_TAG, 2);
                    return;
                }
                let is_block = remaining >= 6
                    && read_usize(&mut &self.src[self.offset..])
                        .is_some_and(|block_len| (1..=remaining - 6).contains(&block_len));
                if !is_block {
                    self.blob_byte(HIDDEN_TAG, 0);
                    self.escape(end);
                    return;
                }
                self.blob_byte(HIDDEN_TAG, 1);
            } else {
                match self.blob_byte(HIDDEN_TAG, 0) {
                    0 => {
                        self.escape(0);
                        return;
                    }
                    1 => {}
                    2 => return,
                    _ => {
                        self.has_error = true;
                        return;
                    }
                }
            }
            let block_len = self.code_blob_bytes(HIDDEN_LENGTH, 4);
            let block_type = self.code_blob_bytes(HIDDEN_TYPE, 2);
            #[cfg(feature = "stats")]
            if self.io.is_hidden_misc(block_type) {
                self.copy_blob(HIDDEN_MISC, block_len as usize);
                continue;
            }
            if block_type == MVD_HIDDEN_USERCMD && block_len == 23 {
                self.usercmd();
            } else {
                self.copy_blob(HIDDEN_MISC, block_len as usize);
            }
        }
    }

    /// mvdsv `SV_DebugClientCommand`: player, drop, msec, 3 x f32 angles, 3 x i16 moves,
    /// buttons, impulse. Context kind in bits 12-15.
    fn usercmd(&mut self) {
        let [last_player, second_last_player] = self.usercmd_previous_players;
        let player = self.code_symbol(
            USERCMD,
            u32::from(last_player.min(31)) | u32::from(second_last_player.min(31)) << 5,
        );
        self.usercmd_previous_players = [player, last_player];
        let mut state = self.usercmds[player as usize];
        state.drop = self.code_symbol(USERCMD, 1 << 12 | u32::from(state.drop));
        state.msec = self.code_symbol(USERCMD, 2 << 12 | u32::from(state.msec));
        // `hidden` checked that all 23 bytes are in the payload
        let mut grid = 0u8;
        if IS_ENCODE {
            for axis in 0..3 {
                let angle_bits = read_u32(&mut &self.src[self.offset + 4 * axis..]);
                if angle_bits.and_then(grid_from_f32_bits).is_some() {
                    grid |= 1 << axis;
                }
            }
        }
        grid = self.symbol(USERCMD, 3 << 12 | u32::from(state.grid), grid);
        if grid > 7 {
            self.has_error = true;
            return;
        }
        state.grid = grid;
        let mut tracks = self.usercmd_tracks[player as usize];
        for (axis, track) in tracks.iter_mut().enumerate() {
            if grid & (1 << axis) != 0 {
                self.usercmd_grid_angle(axis, track);
            } else {
                for byte_index in 0..4 {
                    self.code_symbol(USERCMD, 4 << 12 | (axis as u32) << 2 | byte_index);
                }
            }
        }
        self.usercmd_tracks[player as usize] = tracks;
        for (axis, axis_move) in state.moves.iter_mut().enumerate() {
            let low = self.code_symbol(
                USERCMD,
                5 << 12 | (axis as u32) << 8 | u32::from(*axis_move & 0xff),
            );
            let high = self.code_symbol(USERCMD, 6 << 12 | (axis as u32) << 8 | u32::from(low));
            *axis_move = u16::from_le_bytes([low, high]);
        }
        state.buttons = self.code_symbol(USERCMD, 7 << 12 | u32::from(state.buttons));
        state.impulse = self.code_symbol(USERCMD, 8 << 12 | u32::from(state.impulse));
        self.usercmds[player as usize] = state;
    }

    /// FORMAT.md section 5.6.
    fn usercmd_grid_angle(&mut self, axis: usize, track: &mut AxisTrack) {
        let context = clamped_bucket(track.residual(), 23)
            | clamped_bucket(track.velocity(), 23) << 5
            | clamped_bucket(track.previous_velocity(), 15) << 10
            | (axis as u32) << 14;
        let prediction = track.predict_linear();
        let grid_value = if IS_ENCODE {
            let grid_value =
                grid_from_f32_bits(self.read_u32()).expect("`usercmd` saw it on the grid");
            let residual = i32::from(grid_value.wrapping_sub(prediction).cast_signed());
            self.residual(USERCMD_ANGLE, context, residual);
            grid_value
        } else {
            let residual = self.residual(USERCMD_ANGLE, context, 0);
            let grid_value = prediction.wrapping_add(residual.cast_unsigned() as u16);
            self.write_u32(grid_to_f32_bits(grid_value));
            grid_value
        };
        let residual = i32::from(grid_value.wrapping_sub(prediction).cast_signed());
        track.push(grid_value, residual);
    }
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
