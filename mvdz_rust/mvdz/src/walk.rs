//! The structural walker: one routine that runs in both directions.
//!
//! On encode it reads the demo and appends every structural byte to its
//! stream (with a side context); on decode it pulls the same bytes from the
//! streams and writes them to the output. Numeric fields (origins, angles,
//! sound/TE coordinates, long stats) are not touched: the walker only
//! reserves their bytes and records where they are ([`Records`]); the numeric
//! groups (`numeric.rs`) fill them in after the walk. The structure of a
//! demo never depends on a numeric value, so the walk and the numeric
//! groups are independent.
//!
//! Every read and write is bounds-checked and sets a sticky `err` instead of
//! panicking, so corrupt input ends the walk early and fails the checksum.

use crate::proto::{
    DF_EFFECTS, DF_MODEL, DF_SKIN, DF_WEAPON_FRAME, FTE_COLOR_MOD, FTE_TRANS, Proto, U_ANGLE_1,
    U_ANGLE_2, U_ANGLE_3, U_COLORMAP, U_EFFECTS, U_EVEN_MORE, U_FRAME, U_MODEL, U_MORE_BITS,
    U_ORIGIN_1, U_ORIGIN_2, U_ORIGIN_3, U_SKIN, X_COLOR_MOD, X_ENTITY_DBL, X_ENTITY_DBL_2,
    X_FATNESS, X_MODEL_DBL, X_SCALE, X_TRANS, X_YET_MORE, frame_len, le32,
};
use crate::streams::{
    CMD, ESCLEN, FRAGS, GEN_BASE, HIDDMG, HIDLEN, HIDMISC, HIDTAG, HIDTYPE, MASK, NAILS, PE_A,
    PE_BITS, PE_ENT, PE_FRAME, PE_HDR, PE_MISC, PE_MODEL, PE_MORE, PI_EFF, PI_FLAGS, PI_FRAME,
    PI_MODEL, PI_NUM, PI_SKIN, PI_WFRAME, PING, PR_LEVEL, RAW, SET, SND_CH, SND_MISC, SND_NUM,
    SNDF, ST_IDX, ST_VAL, STL_IDX, STL_VAL, TAIL, TE_MISC, TE_TYPE, TEF, TEXT, TIME, TYPE, UC,
};

pub(crate) const NO_OFFSET: u32 = u32::MAX;

/// Structural symbol I/O.
///
/// Encode: record and return `value`. Decode: return the
/// decoded symbol (`value` is ignored). Every `context` fits in 16 bits, the rans
/// table key (FORMAT.md §8.3); both implementations debug-assert it.
pub(crate) trait SymbolIo {
    fn byte(&mut self, stream: usize, context: u32, value: u8) -> u8;
    fn put_blob(&mut self, stream: usize, bytes: &[u8]);
    fn take_blob(&mut self, stream: usize, len: usize) -> Option<&[u8]>;
    fn blob_rest(&self, stream: usize) -> &[u8];
}

#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub(crate) struct PlayerInfoRecord {
    pub(crate) offset: u32,
    pub(crate) time: u32,
    pub(crate) flags: u16,
    pub(crate) number: u8,
    pub(crate) float_flags: u8, // bit0: float coords
}

pub(crate) const PE_IN_PREVIOUS: u16 = 64;
pub(crate) const PE_FLOAT: u16 = 128;
pub(crate) const PE_WIDE: u16 = 256;

#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub(crate) struct PacketEntityRecord {
    pub(crate) offset: u32,
    pub(crate) time: u32,
    pub(crate) number: u16,
    pub(crate) bits: u16,
    pub(crate) model: u8,
    pub(crate) update_bits: u8, // U_* bits 9..15 of the entity word
}

impl PacketEntityRecord {
    /// Byte offsets of the three origin coords (`NO_OFFSET` if not sent).
    #[inline]
    pub(crate) fn origin_offsets(&self) -> [u32; 3] {
        let coord_width = if self.bits & PE_FLOAT != 0 { 4 } else { 2 };
        let angle_width = if self.bits & PE_WIDE != 0 { 2 } else { 1 };
        let mut offset = self.offset;
        let mut offsets = [NO_OFFSET; 3];
        for (axis, origin_offset) in offsets.iter_mut().enumerate() {
            if self.bits & (1 << (2 * axis)) != 0 {
                *origin_offset = offset;
                offset += coord_width;
            }
            if self.bits & (2 << (2 * axis)) != 0 {
                offset += angle_width;
            }
        }
        offsets
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SoundRecord {
    pub(crate) offset: u32,
    pub(crate) refs: [u32; 3],
    pub(crate) kind: u8,
    pub(crate) number: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TempEntityRecord {
    pub(crate) offset: u32,
    pub(crate) refs: [u32; 3],
    pub(crate) kind: u8,
}

/// A usercmd block with angles on the grid: `off` is the pitch's offset
/// (yaw and roll follow), `grid` bit a set for each axis a on the grid.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UsercmdRecord {
    pub(crate) offset: u32,
    pub(crate) number: u8,
    pub(crate) grid: u8,
}

/// A record list: the walker appends, the numeric groups read it after the walk.
#[derive(Debug, Clone)]
pub(crate) struct RecordList<T> {
    records: Vec<T>,
    /// decode: the count from the stream table
    capacity: usize,
}

impl<T> RecordList<T> {
    /// At most `capacity` records, allocated up front.
    pub(crate) fn fixed(capacity: usize) -> crate::error::Result<Self> {
        Ok(Self {
            records: crate::budget::with_capacity(capacity)?,
            capacity,
        })
    }
    /// No limit; `reserve` records allocated up front.
    pub(crate) fn growable(reserve: usize) -> Self {
        Self {
            records: Vec::with_capacity(reserve),
            capacity: usize::MAX,
        }
    }
    /// Appends `record`; false if the list is full.
    #[inline(always)]
    pub(crate) fn push(&mut self, record: T) -> bool {
        if self.records.len() == self.capacity {
            return false;
        }
        self.records.push(record);
        true
    }
    pub(crate) fn iter(&self) -> std::slice::Iter<'_, T> {
        self.records.iter()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Records {
    pub(crate) player_infos: RecordList<PlayerInfoRecord>,
    pub(crate) packet_entities: RecordList<PacketEntityRecord>,
    pub(crate) sounds: RecordList<SoundRecord>,
    pub(crate) temp_entities: RecordList<TempEntityRecord>,
    pub(crate) usercmds: RecordList<UsercmdRecord>,
}

impl Records {
    /// Encode side: growable, sized by typical densities.
    pub(crate) fn growable(len: usize) -> Self {
        Self {
            player_infos: RecordList::growable(len / 18),
            packet_entities: RecordList::growable(len / 32),
            sounds: RecordList::growable(len / 400),
            temp_entities: RecordList::growable(len / 400),
            usercmds: RecordList::growable(0),
        }
    }
    /// Decode side: exact capacities from the stream table.
    pub(crate) fn fixed(
        pi: usize,
        pe: usize,
        snd: usize,
        te: usize,
        uc: usize,
    ) -> crate::error::Result<Self> {
        Ok(Self {
            player_infos: RecordList::fixed(pi)?,
            packet_entities: RecordList::fixed(pe)?,
            sounds: RecordList::fixed(snd)?,
            temp_entities: RecordList::fixed(te)?,
            usercmds: RecordList::fixed(uc)?,
        })
    }
}

#[derive(Clone, Copy, Default)]
struct PlayerState {
    flags: u16,
    frame: u8,
    weapon_frame: u8,
}

/// The previous usercmd fields of one player (§6.11).
#[derive(Clone, Copy, Default)]
struct UsercmdState {
    drop: u8,
    msec: u8,
    grid: u8,
    buttons: u8,
    impulse: u8,
    moves: [u16; 3],
}

#[derive(Clone, Copy, Default)]
struct EntityState {
    angles: [u8; 3],
    angle_residuals: [u8; 3],
    frame: u8,
    model: u8,
    bits: u8,
    last_packet: u32,
}

pub(crate) struct Walker<'a, const IS_ENCODE: bool, IO: SymbolIo> {
    pub(crate) src: &'a [u8],
    /// the output segment (decode; empty on encode)
    dst: &'a mut [u8],
    pub(crate) offset: usize,
    pub(crate) has_error: bool,
    pub(crate) io: IO,
    pub(crate) proto: Proto,
    pub(crate) recs: &'a mut Records,
    players: Box<[PlayerState; 256]>,
    entities: Box<[EntityState; 2048]>,
    stats: Box<[[u8; 256]; 32]>,
    player_origin_offsets: Box<[[u32; 3]; 256]>,
    entity_origin_offsets: Box<[[u32; 3]; 2048]>,
    time: u32,
    target: u8,
    last_type: u8,
    previous_command: u8,
    previous_packet: Vec<u16>,
    current_packet: Vec<u16>,
    previous_packet_index: usize,
    packet_number: u32,
    last_time_byte: u8,
    first_command_by_type: [u8; 8],
    playerinfo_previous_number: u8,
    stat_previous_index: u8,
    stat_previous_residual: u8,
    last_header: u8,
    previous_temp_entity: u8,
    previous_entity_gap: u8,
    usercmd_previous_players: [u8; 2],
    usercmds: Box<[UsercmdState; 256]>,
    /// demo time at the end of the walk (ms)
    pub(crate) end_time: u32,
}

impl<'a, const IS_ENCODE: bool, IO: SymbolIo> Walker<'a, IS_ENCODE, IO> {
    /// `dst`: the output segment (decode; empty on encode). The walker writes the
    /// structural bytes; the numeric groups fill in the rest after the walk.
    #[expect(
        clippy::large_stack_arrays,
        reason = "fixed-size tables keep byte-indexed lookups free of bounds checks"
    )]
    pub(crate) fn new(
        src: &'a [u8],
        dst: &'a mut [u8],
        recs: &'a mut Records,
        io: IO,
        proto: Proto,
    ) -> Self {
        Walker {
            src,
            dst,
            offset: 0,
            has_error: false,
            io,
            proto,
            recs,
            players: Box::new([PlayerState::default(); 256]),
            entities: Box::new([EntityState::default(); 2048]),
            stats: Box::new([[0u8; 256]; 32]),
            player_origin_offsets: Box::new([[NO_OFFSET; 3]; 256]),
            entity_origin_offsets: Box::new([[NO_OFFSET; 3]; 2048]),
            time: 0,
            target: 0,
            last_type: 0,
            previous_command: 0,
            previous_packet: Vec::new(),
            current_packet: Vec::new(),
            previous_packet_index: 0,
            packet_number: 1,
            last_time_byte: 0,
            first_command_by_type: [0; 8],
            playerinfo_previous_number: 255,
            stat_previous_index: 255,
            stat_previous_residual: 0,
            last_header: 0,
            previous_temp_entity: 0,
            previous_entity_gap: 0,
            usercmd_previous_players: [0; 2],
            usercmds: Box::new([UsercmdState::default(); 256]),
            end_time: 0,
        }
    }

    #[inline(always)]
    fn limit(&self) -> usize {
        if IS_ENCODE {
            self.src.len()
        } else {
            self.dst.len()
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

    /// A plain byte.
    #[inline(always)]
    fn coded_byte(&mut self, stream: usize, context: u32) -> u8 {
        if IS_ENCODE {
            let byte = self.read_byte();
            self.io.byte(stream, context, byte)
        } else {
            let byte = self.io.byte(stream, context, 0);
            self.write_byte(byte);
            byte
        }
    }

    /// A byte coded as (byte - prediction) mod 256.
    #[inline(always)]
    fn coded_residual(&mut self, stream: usize, context: u32, prediction: u8) -> u8 {
        if IS_ENCODE {
            let byte = self.read_byte();
            self.io.byte(stream, context, byte.wrapping_sub(prediction));
            byte
        } else {
            let byte = self.io.byte(stream, context, 0).wrapping_add(prediction);
            self.write_byte(byte);
            byte
        }
    }

    /// A byte of the demo kept in blob stream `stream`.
    #[inline(always)]
    fn blob_byte(&mut self, stream: usize) -> u8 {
        if IS_ENCODE {
            let byte = self.read_byte();
            self.io.put_blob(stream, &[byte]);
            byte
        } else {
            let byte = self.tag(stream, 0);
            self.write_byte(byte);
            byte
        }
    }

    /// A byte of the demo kept in blob stream `stream` as (byte - prediction) mod 256.
    #[inline(always)]
    fn blob_residual(&mut self, stream: usize, prediction: u8) -> u8 {
        if IS_ENCODE {
            let byte = self.read_byte();
            self.io.put_blob(stream, &[byte.wrapping_sub(prediction)]);
            byte
        } else {
            let byte = self.tag(stream, 0).wrapping_add(prediction);
            self.write_byte(byte);
            byte
        }
    }

    /// `count` little-endian bytes of the demo kept in blob stream `stream`.
    #[inline]
    fn blob_le(&mut self, stream: usize, count: usize) -> u32 {
        let mut value = 0u32;
        for index in 0..count {
            value |= u32::from(self.blob_byte(stream)) << (8 * index);
        }
        value
    }

    /// A byte that is not part of the demo (a tag, a length byte), kept in
    /// blob stream `stream`: encode stores `value`, decode returns the stored byte.
    #[inline(always)]
    fn tag(&mut self, stream: usize, value: u8) -> u8 {
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

    /// `count` little-endian bytes, the byte index in context bits 14-15.
    #[inline]
    fn coded_le(&mut self, stream: usize, context: u32, count: usize) -> u32 {
        let mut value = 0u32;
        for index in 0..count {
            value |=
                u32::from(self.coded_byte(stream, context | (index as u32) << 14)) << (8 * index);
        }
        value
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

    /// Reserve `len` bytes for a numeric field; returns their offset.
    #[inline(always)]
    fn reserve(&mut self, len: usize) -> u32 {
        let offset = self.offset;
        self.offset += len;
        if self.offset > self.limit() {
            self.has_error = true;
        }
        offset as u32
    }

    /// Walks the whole segment. Encode: returns the tail length. Decode:
    /// `tail` is the stored tail length.
    pub(crate) fn run(&mut self, tail: usize) -> usize {
        if IS_ENCODE {
            while !self.has_error && frame_len(self.src, self.offset).is_some() {
                self.frame();
            }
            if self.has_error {
                return 0;
            }
            let tail_len = self.src.len() - self.offset;
            self.copy_blob(TAIL, tail_len);
            self.end_time = self.time;
            tail_len
        } else {
            let stop = self.dst.len().saturating_sub(tail);
            while !self.has_error && self.offset < stop {
                self.frame();
            }
            if self.offset != stop {
                self.has_error = true;
            }
            self.copy_blob(TAIL, tail);
            self.end_time = self.time;
            tail
        }
    }

    fn frame(&mut self) {
        let last_type = u32::from(self.last_type);
        let time_byte = self.coded_byte(TIME, last_type | u32::from(self.last_time_byte) << 8);
        self.last_time_byte = time_byte;
        self.time = self.time.wrapping_add(u32::from(time_byte));
        let frame_type = self.coded_byte(TYPE, last_type | u32::from(time_byte.min(32)) << 8);
        self.last_type = frame_type;
        self.target = frame_type >> 3;
        match frame_type & 7 {
            2 => self.copy_blob(SET, 8),
            3 => {
                let mask = self.blob_le(MASK, 4);
                self.length_prefixed(frame_type, mask == 0);
            }
            1 | 4 | 5 | 6 => self.length_prefixed(frame_type, false),
            _ => self.has_error = true,
        }
    }

    fn length_prefixed(&mut self, frame_type: u8, is_hidden: bool) {
        if IS_ENCODE {
            if self.src.len() - self.offset < 4 {
                self.has_error = true;
                return;
            }
            let len = le32(&self.src[self.offset..]) as usize;
            self.offset += 4;
            let end = self.offset + len;
            if end > self.src.len() {
                self.has_error = true;
                return;
            }
            if is_hidden {
                self.hidden(end);
            } else {
                self.payload(end, frame_type);
            }
        } else {
            let len_offset = self.offset;
            self.reserve(4);
            if self.has_error {
                return;
            }
            if is_hidden {
                self.hidden(0);
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
        if IS_ENCODE {
            let mut len = (end - self.offset) as u32;
            loop {
                let byte = (len & 0x7f) as u8 | if len >= 0x80 { 0x80 } else { 0 };
                self.tag(ESCLEN, byte);
                len >>= 7;
                if len == 0 {
                    break;
                }
            }
            self.copy_blob(RAW, end - self.offset);
        } else {
            let mut len = 0u32;
            for group in 0..5u32 {
                let byte = self.tag(ESCLEN, 0);
                len |= u32::from(byte & 0x7f) << (7 * group);
                if byte < 0x80 {
                    break;
                }
                if group == 4 {
                    self.has_error = true;
                    return;
                }
            }
            self.copy_blob(RAW, len as usize);
        }
    }

    fn payload(&mut self, end: usize, frame_type: u8) {
        self.previous_command = 0;
        self.playerinfo_previous_number = 255;
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
                    self.io.byte(CMD, context, 255);
                    return;
                }
                let command = self.src[self.offset];
                let size = self
                    .proto
                    .command_size(command, &self.src[self.offset + 1..end]);
                let size = match size {
                    Some(size) if command != 0 => size,
                    _ => {
                        self.io.byte(CMD, context, 0);
                        self.escape(end);
                        return;
                    }
                };
                self.io.byte(CMD, context, command);
                self.offset += 1;
                let start = self.offset;
                self.command(command);
                if self.offset != start + size {
                    self.has_error = true;
                    return;
                }
                self.previous_command = command;
            } else {
                let command = self.io.byte(CMD, context, 0);
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

    fn generic(&mut self, stream: usize, size_command: u8) -> usize {
        let start = self.offset;
        let size = if IS_ENCODE {
            self.proto
                .command_size(size_command, &self.src[self.offset.min(self.src.len())..])
        } else {
            self.proto
                .command_size(size_command, self.io.blob_rest(stream))
        };
        match size {
            Some(size) => self.copy_blob(stream, size),
            None => self.has_error = true,
        }
        start
    }

    fn command(&mut self, command: u8) {
        match command {
            42 => self.playerinfo(),
            47 | 48 => self.packet_entities(command == 48),
            3 => {
                let player = (self.target & 31) as usize;
                let stat = self.coded_byte(
                    ST_IDX,
                    u32::from(self.stat_previous_index) | u32::from(self.previous_command) << 8,
                );
                let previous = self.stats[player][stat as usize];
                let value = self.coded_residual(
                    ST_VAL,
                    u32::from(stat) | u32::from(self.stat_previous_residual) << 8,
                    previous,
                );
                self.stat_previous_index = stat;
                self.stat_previous_residual = value.wrapping_sub(previous);
                self.stats[player][stat as usize] = value;
            }
            38 => {
                self.blob_byte(STL_IDX);
                self.copy_blob(STL_VAL, 4);
            }
            6 => self.sound(),
            23 => self.temp_entity(),
            8 => {
                self.blob_byte(PR_LEVEL);
                self.generic(TEXT, 9);
            }
            9 | 26 => {
                self.generic(TEXT, 9);
            }
            14 => {
                self.copy_blob(FRAGS, 3);
            }
            36 | 37 | 53 => {
                self.generic(PING, command);
            }
            43 | 54 => {
                self.generic(NAILS, command);
            }
            11 => {
                let start = self.generic(GEN_BASE + 11, 11);
                if !self.has_error {
                    let end = self.offset;
                    // decode: the serverdata body the walker just wrote
                    let body: &[u8] = if IS_ENCODE {
                        &self.src[start..end]
                    } else {
                        self.dst.get(start..end).unwrap_or_default()
                    };
                    let mut proto = self.proto;
                    proto.observe_server_data(body);
                    self.proto = proto;
                }
            }
            _ => {
                self.generic(GEN_BASE + command as usize, command);
            }
        }
    }

    /// The DF_* flags as one symbol in the common case: changed = flags ^ previous;
    /// the often-toggling bits (sent axes 0-5, weaponframe 10) plus an
    /// escape bit for "a rare bit changed" form one byte; only on escape do
    /// bits 6-9, 11-14 and then bit 15 follow.
    fn playerinfo_flags(&mut self, previous: u16) -> u16 {
        // Bit 15 keeps the main symbol's contexts clear of rare_byte's (0x100-0x1ff)
        // and bit15_byte's (0x200); previous's own bit 15 is left out.
        let context = 0x8000 | u32::from(previous & 0x7fff);
        let changed = if IS_ENCODE {
            let low = u16::from(self.read_byte());
            let changed = (low | u16::from(self.read_byte()) << 8) ^ previous;
            let rare_bits = changed & !0x43f;
            let main_byte = (changed & 0x3f) as u8
                | (((changed >> 10) & 1) as u8) << 6
                | u8::from(rare_bits != 0) << 7;
            self.io.byte(PI_FLAGS, context, main_byte);
            if rare_bits != 0 {
                let rare_byte = ((changed >> 6) & 0xf) as u8 | (((changed >> 11) & 0xf) as u8) << 4;
                self.io.byte(
                    PI_FLAGS,
                    0x100 | u32::from((previous >> 6) & 0xff),
                    rare_byte,
                );
                self.io.byte(PI_FLAGS, 0x200, (changed >> 15) as u8);
            }
            changed
        } else {
            let main_byte = u16::from(self.io.byte(PI_FLAGS, context, 0));
            let mut changed = (main_byte & 0x3f) | ((main_byte >> 6) & 1) << 10;
            if main_byte & 0x80 != 0 {
                let rare_byte = u16::from(self.io.byte(
                    PI_FLAGS,
                    0x100 | u32::from((previous >> 6) & 0xff),
                    0,
                ));
                let bit15_byte = u16::from(self.io.byte(PI_FLAGS, 0x200, 0));
                changed |= (rare_byte & 0xf) << 6 | (rare_byte >> 4) << 11 | (bit15_byte & 1) << 15;
            }
            let flags = changed ^ previous;
            let [low, high] = flags.to_le_bytes();
            self.write_byte(low);
            self.write_byte(high);
            changed
        };
        changed ^ previous
    }

    fn playerinfo(&mut self) {
        let number = self.blob_byte(PI_NUM);
        self.playerinfo_previous_number = number;
        let previous = self.players[number as usize];
        let flags = self.playerinfo_flags(previous.flags);
        let frame = self.coded_residual(
            PI_FRAME,
            u32::from(previous.frame) | u32::from(flags & 0x300),
            previous.frame,
        );
        let coord_width = self.proto.coord();
        let origin_count = (flags & 7).count_ones() as usize;
        let angle_count = (flags & 0x38).count_ones() as usize;
        let offset = self.reserve(origin_count * coord_width + angle_count * 2);
        if !self.recs.player_infos.push(PlayerInfoRecord {
            offset,
            time: self.time,
            flags,
            number,
            float_flags: u8::from(self.proto.has_float_coords),
        }) {
            self.has_error = true;
        }
        let mut origin_offset = offset;
        for axis in 0..3 {
            if flags & (1 << axis) != 0 {
                self.player_origin_offsets[number as usize][axis] = origin_offset;
                origin_offset += coord_width as u32;
            }
        }
        if flags & DF_MODEL != 0 {
            self.blob_byte(PI_MODEL);
        }
        if flags & DF_SKIN != 0 {
            self.blob_byte(PI_SKIN);
        }
        if flags & DF_EFFECTS != 0 {
            self.blob_byte(PI_EFF);
        }
        let weapon_frame = if flags & DF_WEAPON_FRAME != 0 {
            self.coded_residual(
                PI_WFRAME,
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

    /// Next entity number in this packet as predicted from the previous
    /// packet's list.
    fn predict_entity(&mut self, previous: i32) -> i32 {
        while self.previous_packet_index < self.previous_packet.len()
            && i32::from(self.previous_packet[self.previous_packet_index]) <= previous
        {
            self.previous_packet_index += 1;
        }
        if self.previous_packet_index < self.previous_packet.len() {
            i32::from(self.previous_packet[self.previous_packet_index])
        } else {
            previous + 1
        }
    }

    fn packet_entities(&mut self, is_delta: bool) {
        if is_delta {
            let header = self.blob_residual(PE_HDR, self.last_header);
            self.last_header = header;
        }
        self.previous_entity_gap = 0;
        let mut previous_entity: i32 = -1;
        self.previous_packet_index = 0;
        self.current_packet.clear();
        self.packet_number = self.packet_number.wrapping_add(1);
        loop {
            if self.has_error {
                break;
            }
            let predicted = self.predict_entity(previous_entity);
            let entity_context = (predicted - previous_entity).min(255).cast_unsigned()
                | u32::from(self.previous_entity_gap.min(15)) << 8;
            let word: u16;
            let entity: usize;
            if IS_ENCODE {
                if self.src.len() - self.offset < 2 {
                    self.has_error = true;
                    return;
                }
                word = u16::from_le_bytes([self.src[self.offset], self.src[self.offset + 1]]);
                self.offset += 2;
                if word == 0 {
                    self.io.byte(PE_ENT, entity_context, 0);
                    break;
                }
                entity = (word & 511) as usize;
                let gap = ((i32::from(word & 511) - previous_entity - 1) & 511) + 1;
                if gap < 255 {
                    self.io
                        .byte(PE_ENT, entity_context, gap.cast_unsigned() as u8);
                } else {
                    self.io.byte(PE_ENT, entity_context, 255);
                    self.io.byte(PE_ENT, 1 << 12, gap.cast_unsigned() as u8);
                    self.io
                        .byte(PE_ENT, 2 << 12, (gap >> 8).cast_unsigned() as u8);
                }
                let state = self.entities[entity];
                self.io.byte(
                    PE_BITS,
                    u32::from(state.bits) | u32::from(state.model) << 7,
                    (word >> 9) as u8 ^ state.bits,
                );
            } else {
                let mut gap = i32::from(self.io.byte(PE_ENT, entity_context, 0));
                if gap == 255 {
                    gap = i32::from(self.io.byte(PE_ENT, 1 << 12, 0));
                    gap |= i32::from(self.io.byte(PE_ENT, 2 << 12, 0)) << 8;
                }
                if gap == 0 {
                    self.write_byte(0);
                    self.write_byte(0);
                    break;
                }
                entity = ((previous_entity + gap) & 511).cast_unsigned() as usize;
                let state = self.entities[entity];
                let bits = self.io.byte(
                    PE_BITS,
                    u32::from(state.bits) | u32::from(state.model) << 7,
                    0,
                ) ^ state.bits;
                word = u16::from(bits) << 9 | entity as u16;
                if word == 0 {
                    // a word of zero would end the packet; the encoder never emits it
                    self.has_error = true;
                    break;
                }
                let [low, high] = word.to_le_bytes();
                self.write_byte(low);
                self.write_byte(high);
            }
            let entity_number = i32::from(word & 511);
            self.previous_entity_gap = (entity_number - previous_entity)
                .clamp(0, 255)
                .cast_unsigned() as u8;
            previous_entity = entity_number;
            self.current_packet.push(entity as u16);
            self.entities[entity].bits = (word >> 9) as u8;
            self.entity_delta(u32::from(word));
        }
        std::mem::swap(&mut self.previous_packet, &mut self.current_packet);
    }

    fn entity_delta(&mut self, word: u32) {
        let mut bits = word & !511;
        let mut more = 0u32;
        let mut number = (word & 511) as usize;
        let previous_model = u32::from(self.entities[number].model);
        if bits & U_MORE_BITS != 0 {
            bits |= u32::from(self.coded_byte(PE_MORE, (bits >> 9) | previous_model << 8));
        }
        if bits & U_EVEN_MORE != 0 && self.proto.fte != 0 {
            more = u32::from(self.coded_byte(PE_MORE, 0x8000));
            if more & X_YET_MORE != 0 {
                more |= u32::from(self.coded_byte(PE_MORE, 0x8001)) << 8;
            }
        }
        if more & X_ENTITY_DBL != 0 {
            number += 512;
        }
        if more & X_ENTITY_DBL_2 != 0 {
            number += 1024;
        }
        let was_in_previous_packet =
            self.entities[number].last_packet == self.packet_number.wrapping_sub(1);
        self.entities[number].last_packet = self.packet_number;
        let mut state = self.entities[number];
        if bits & U_MODEL != 0 {
            state.model = self.blob_byte(PE_MODEL);
        } else if more & X_MODEL_DBL != 0 {
            self.blob_le(PE_MODEL, 2);
        }
        let model = u32::from(state.model);
        if bits & U_FRAME != 0 {
            state.frame = self.blob_residual(PE_FRAME, state.frame);
        }
        if bits & U_COLORMAP != 0 {
            self.blob_byte(PE_MISC);
        }
        if bits & U_SKIN != 0 {
            self.blob_byte(PE_MISC);
        }
        if bits & U_EFFECTS != 0 {
            self.blob_byte(PE_MISC);
        }
        let mut record_bits = 0u16;
        for (axis, (origin_flag, angle_flag)) in [
            (U_ORIGIN_1, U_ANGLE_1),
            (U_ORIGIN_2, U_ANGLE_2),
            (U_ORIGIN_3, U_ANGLE_3),
        ]
        .into_iter()
        .enumerate()
        {
            if bits & origin_flag != 0 {
                record_bits |= 1 << (2 * axis);
            }
            if bits & angle_flag != 0 {
                record_bits |= 2 << (2 * axis);
            }
        }
        if was_in_previous_packet {
            record_bits |= PE_IN_PREVIOUS;
        }
        if self.proto.has_float_coords {
            record_bits |= PE_FLOAT;
        }
        if self.proto.has_wide_messages {
            record_bits |= PE_WIDE;
        }
        let record = PacketEntityRecord {
            offset: self.offset as u32,
            time: self.time,
            number: number as u16,
            bits: record_bits,
            model: state.model,
            update_bits: (word >> 9) as u8,
        };
        let coord_width = self.proto.coord();
        for (axis, (origin_flag, angle_flag)) in [
            (U_ORIGIN_1, U_ANGLE_1),
            (U_ORIGIN_2, U_ANGLE_2),
            (U_ORIGIN_3, U_ANGLE_3),
        ]
        .into_iter()
        .enumerate()
        {
            if bits & origin_flag != 0 {
                let offset = self.reserve(coord_width);
                self.entity_origin_offsets[number][axis] = offset;
            }
            if bits & angle_flag != 0 {
                if self.proto.has_wide_messages {
                    self.coded_le(PE_A, 3 | model << 6, 2);
                } else {
                    let residual_bucket = u32::from(qb(state.angle_residuals[axis]).min(15));
                    let angle = self.coded_residual(
                        PE_A,
                        axis as u32 | residual_bucket << 2 | model << 6,
                        state.angles[axis],
                    );
                    state.angle_residuals[axis] = angle.wrapping_sub(state.angles[axis]);
                    state.angles[axis] = angle;
                }
            }
        }
        if !self.recs.packet_entities.push(record) {
            self.has_error = true;
        }
        if more & X_TRANS != 0 && self.proto.fte & FTE_TRANS != 0 {
            self.blob_byte(PE_MISC);
        }
        if more & X_SCALE != 0 {
            self.blob_byte(PE_MISC);
        }
        if more & X_FATNESS != 0 {
            self.blob_byte(PE_MISC);
        }
        if more & X_COLOR_MOD != 0 && self.proto.fte & FTE_COLOR_MOD != 0 {
            self.blob_le(PE_MISC, 3);
        }
        self.entities[number] = state;
    }

    fn sound(&mut self) {
        let low = u32::from(self.coded_byte(SND_CH, u32::from(self.previous_command)));
        let channel = low | u32::from(self.coded_byte(SND_CH, 1 << 8 | low)) << 8;
        if channel & 0x8000 != 0 {
            self.blob_byte(SND_MISC);
        }
        if channel & 0x4000 != 0 {
            self.blob_byte(SND_MISC);
        }
        let entity = ((channel >> 3) & 1023) as usize;
        let sound_number = self.coded_byte(SND_NUM, (channel & 7) | (entity.min(33) as u32) << 3);
        if self.proto.coord() != 2 {
            self.copy_blob(SNDF, 12);
            return;
        }
        let (refs, kind) = if (1..=32).contains(&entity) {
            (self.player_origin_offsets[entity - 1], 1)
        } else if entity < 2048 {
            (self.entity_origin_offsets[entity], 2)
        } else {
            ([NO_OFFSET; 3], 0)
        };
        let offset = self.reserve(6);
        if !self.recs.sounds.push(SoundRecord {
            offset,
            refs,
            kind,
            number: sound_number,
        }) {
            self.has_error = true;
        }
    }

    fn temp_entity(&mut self) {
        let kind = self.coded_byte(
            TE_TYPE,
            u32::from(self.previous_temp_entity) | u32::from(self.previous_command == 23) << 8,
        );
        self.previous_temp_entity = kind;
        let coord_width = self.proto.coord();
        if coord_width != 2 {
            match kind {
                5 | 6 | 9 => {
                    self.blob_le(TE_MISC, 2);
                    self.copy_blob(TEF, 6 * coord_width);
                }
                2 | 12 => {
                    self.blob_byte(TE_MISC);
                    self.copy_blob(TEF, 3 * coord_width);
                }
                _ => self.copy_blob(TEF, 3 * coord_width),
            }
            return;
        }
        match kind {
            5 | 6 | 9 => {
                let entity = self.blob_le(TE_MISC, 2) as usize;
                let refs = if (1..=32).contains(&entity) {
                    self.player_origin_offsets[entity - 1]
                } else {
                    [NO_OFFSET; 3]
                };
                let offset = self.reserve(12);
                if !self
                    .recs
                    .temp_entities
                    .push(TempEntityRecord { offset, refs, kind })
                {
                    self.has_error = true;
                }
            }
            2 | 12 => {
                self.blob_byte(TE_MISC);
                let offset = self.reserve(6);
                if !self.recs.temp_entities.push(TempEntityRecord {
                    offset,
                    refs: [NO_OFFSET; 3],
                    kind,
                }) {
                    self.has_error = true;
                }
            }
            _ => {
                let offset = self.reserve(6);
                if !self.recs.temp_entities.push(TempEntityRecord {
                    offset,
                    refs: [NO_OFFSET; 3],
                    kind,
                }) {
                    self.has_error = true;
                }
            }
        }
    }

    /// `dem_multiple` with mask 0: blocks of [u32 len][u16 type][len bytes].
    /// Tags: 1 block, 2 end, 0 escape (rest raw).
    fn hidden(&mut self, end: usize) {
        loop {
            if self.has_error {
                return;
            }
            if IS_ENCODE {
                let remaining = end - self.offset;
                if remaining == 0 {
                    self.tag(HIDTAG, 2);
                    return;
                }
                let mut is_block = remaining >= 6;
                if is_block {
                    let block_len = u64::from(le32(&self.src[self.offset..]));
                    is_block = block_len >= 1 && block_len <= (remaining - 6) as u64;
                }
                if !is_block {
                    self.tag(HIDTAG, 0);
                    self.escape(end);
                    return;
                }
                self.tag(HIDTAG, 1);
            } else {
                match self.tag(HIDTAG, 0) {
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
            let block_len = self.blob_le(HIDLEN, 4);
            let block_type = self.blob_le(HIDTYPE, 2);
            if block_type == 7 && block_len == 8 {
                self.copy_blob(HIDDMG, 8);
            } else if block_type == 1 && block_len == 23 {
                self.usercmd();
            } else {
                self.copy_blob(HIDMISC, block_len as usize);
            }
        }
    }

    /// One usercmd block (mvdsv `SV_DebugClientCommand`): player, dropnum,
    /// msec, pitch/yaw/roll as f32, forward/side/up as i16, buttons,
    /// impulse. Angles on the 360/65536 grid are left to the numeric group
    /// (`UsercmdRecord`); the rest goes to UC, context kind in bits 12-15.
    fn usercmd(&mut self) {
        let [last_player, second_last_player] = self.usercmd_previous_players;
        let player = self.coded_byte(
            UC,
            u32::from(last_player.min(31)) | u32::from(second_last_player.min(31)) << 5,
        );
        self.usercmd_previous_players = [player, last_player];
        let mut state = self.usercmds[player as usize];
        state.drop = self.coded_byte(UC, 1 << 12 | u32::from(state.drop));
        state.msec = self.coded_byte(UC, 2 << 12 | u32::from(state.msec));
        // hidden() saw all 23 bytes inside the payload: the angles follow
        let mut grid = 0u8;
        if IS_ENCODE {
            for axis in 0..3 {
                if on_grid(le32(&self.src[self.offset + 4 * axis..])).is_some() {
                    grid |= 1 << axis;
                }
            }
        }
        grid = self.io.byte(UC, 3 << 12 | u32::from(state.grid), grid);
        if grid > 7 {
            self.has_error = true;
            return;
        }
        state.grid = grid;
        let offset = self.offset as u32;
        for axis in 0..3u32 {
            if grid & (1 << axis) != 0 {
                self.reserve(4);
            } else {
                for byte_index in 0..4 {
                    self.coded_byte(UC, 4 << 12 | axis << 2 | byte_index);
                }
            }
        }
        if grid != 0
            && !self.recs.usercmds.push(UsercmdRecord {
                offset,
                number: player,
                grid,
            })
        {
            self.has_error = true;
        }
        for axis in 0..3 {
            let low = self.coded_byte(
                UC,
                5 << 12 | (axis as u32) << 8 | u32::from(state.moves[axis] & 0xff),
            );
            let high = self.coded_byte(UC, 6 << 12 | (axis as u32) << 8 | u32::from(low));
            state.moves[axis] = u16::from_le_bytes([low, high]);
        }
        state.buttons = self.coded_byte(UC, 7 << 12 | u32::from(state.buttons));
        state.impulse = self.coded_byte(UC, 8 << 12 | u32::from(state.impulse));
        self.usercmds[player as usize] = state;
    }
}

/// Small class of an svc number for the command context.
#[inline(always)]
fn command_class(command: u8) -> u8 {
    match command {
        0 => 0,
        42 => 1,
        48 => 2,
        47 => 3,
        6 => 4,
        23 => 5,
        3 => 6,
        38 => 7,
        8 => 8,
        51 => 9,
        14 => 10,
        36 => 11,
        9 => 12,
        43 | 54 => 13,
        19 => 14,
        _ => 15,
    }
}

/// Sign/magnitude bucket of a byte residual (as i8).
#[inline(always)]
pub(crate) fn qb(residual: u8) -> u8 {
    crate::numeric::residual_bucket(i32::from(residual.cast_signed()))
}

/// The grid value of a usercmd angle: the `grid` in -32768..=32767 whose
/// f32(grid * 45/8192) has exactly these bits (grid * 45/8192 is exact in f32),
/// or None (-0.0, NaN, +180 and anything else off the grid).
#[inline]
pub(crate) fn on_grid(bits: u32) -> Option<u16> {
    // f * 8192 is exact and the division correctly rounded: an integer
    // quotient comes out exactly
    let scaled = f64::from(f32::from_bits(bits)) * 8192.0 / 45.0;
    if scaled.fract() != 0.0 || !(-32768.0..=32767.0).contains(&scaled) {
        return None;
    }
    let grid = scaled as i32 as i16;
    (grid_f32(grid.cast_unsigned()) == bits).then_some(grid.cast_unsigned())
}

/// The f32 bits of grid value `grid` (as i16): grid * 45/8192, exact.
#[inline]
pub(crate) fn grid_f32(grid: u16) -> u32 {
    (f32::from(grid.cast_signed()) * (45.0 / 8192.0)).to_bits()
}
