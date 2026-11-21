//! Protocol state and the bounds-checked command sizer.
//!
//! The sizer is pure wire-layout knowledge (mvdsv `sv_demo.c`, `sv_ents.c`,
//! ezquake `cl_parse.c` / `cl_ents.c`): it reports how many body bytes a
//! `svc_*` command occupies, or `None` when the command is unknown or does
//! not fit. A mis-sized command never breaks losslessness: encoder and
//! decoder size the same bytes the same way, the encoder then asserts that
//! the transform consumed exactly the sized length.

pub(crate) const FTE_TRANS: u32 = 1 << 3; // FTE_PEXT_TRANS
pub(crate) const FTE_FLOAT_COORDS: u32 = 1 << 15; // FTE_PEXT_FLOATCOORDS
pub(crate) const FTE_COLOR_MOD: u32 = 1 << 19; // FTE_PEXT_COLOURMOD
pub(crate) const MVD1_FLOAT_COORDS: u32 = 1 << 0; // MVD_PEXT1_FLOATCOORDS
pub(crate) const PROTO_FTE: u32 = 0x5845_5446; // 'FTEX'
pub(crate) const PROTO_FTE_2: u32 = 0x3245_5446; // 'FTE2'
pub(crate) const PROTO_MVD1: u32 = 0x3144_564D; // 'MVD1'
pub(crate) const PROTOCOL_QW: u32 = 28;

pub(crate) const U_MORE_BITS: u32 = 1 << 15;
pub(crate) const U_ORIGIN_1: u32 = 1 << 9;
pub(crate) const U_ORIGIN_2: u32 = 1 << 10;
pub(crate) const U_ORIGIN_3: u32 = 1 << 11;
pub(crate) const U_ANGLE_2: u32 = 1 << 12;
pub(crate) const U_FRAME: u32 = 1 << 13;
pub(crate) const U_ANGLE_1: u32 = 1 << 0;
pub(crate) const U_ANGLE_3: u32 = 1 << 1;
pub(crate) const U_MODEL: u32 = 1 << 2;
pub(crate) const U_COLORMAP: u32 = 1 << 3;
pub(crate) const U_SKIN: u32 = 1 << 4;
pub(crate) const U_EFFECTS: u32 = 1 << 5;
pub(crate) const U_EVEN_MORE: u32 = 1 << 7;
pub(crate) const X_SCALE: u32 = 1 << 0;
pub(crate) const X_TRANS: u32 = 1 << 1;
pub(crate) const X_FATNESS: u32 = 1 << 2;
pub(crate) const X_MODEL_DBL: u32 = 1 << 3;
pub(crate) const X_ENTITY_DBL: u32 = 1 << 5;
pub(crate) const X_ENTITY_DBL_2: u32 = 1 << 6;
pub(crate) const X_YET_MORE: u32 = 1 << 7;
pub(crate) const X_COLOR_MOD: u32 = 1 << 10;

pub(crate) const DF_MODEL: u16 = 1 << 11;
pub(crate) const DF_SKIN: u16 = 1 << 7;
pub(crate) const DF_EFFECTS: u16 = 1 << 6;
pub(crate) const DF_WEAPON_FRAME: u16 = 1 << 10;

/// Per-demo protocol state that changes command widths. It is derived only
/// from bytes already seen, so both directions agree.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Proto {
    pub fte: u32,
    pub(crate) fte2: u32,
    pub(crate) mvd1: u32,
    /// FTE or MVD1 float coords (entity/player origins, sounds, TEs).
    pub(crate) has_float_coords: bool,
    /// FTE float coords: generic coords 4 bytes, angles 2.
    pub(crate) has_wide_messages: bool,
}

impl Proto {
    pub(crate) fn new(fte: u32, fte2: u32, mvd1: u32) -> Self {
        let mut proto = Self {
            fte,
            fte2,
            mvd1,
            ..Default::default()
        };
        proto.derive();
        proto
    }
    pub(crate) fn derive(&mut self) {
        self.has_float_coords =
            self.fte & FTE_FLOAT_COORDS != 0 || self.mvd1 & MVD1_FLOAT_COORDS != 0;
        self.has_wide_messages = self.fte & FTE_FLOAT_COORDS != 0;
    }
    #[inline]
    pub fn coord(&self) -> usize {
        if self.has_float_coords { 4 } else { 2 }
    }
    #[inline]
    pub fn wide_coord(&self) -> usize {
        if self.has_wide_messages { 4 } else { 2 }
    }
    #[inline]
    pub fn angle(&self) -> usize {
        if self.has_wide_messages { 2 } else { 1 }
    }

    /// Updates the protocol state from an `svc_serverdata` body.
    pub fn observe_server_data(&mut self, body: &[u8]) {
        let mut offset = 0;
        while offset + 4 <= body.len() {
            let protocol = le32(&body[offset..]);
            offset += 4;
            if protocol == PROTOCOL_QW {
                break;
            }
            if offset + 4 > body.len() {
                break;
            }
            let extensions = le32(&body[offset..]);
            offset += 4;
            match protocol {
                PROTO_FTE => self.fte = extensions,
                PROTO_FTE_2 => self.fte2 = extensions,
                PROTO_MVD1 => self.mvd1 = extensions,
                _ => {}
            }
        }
        self.derive();
    }

    /// Body length (excluding the command byte) of `command` whose body
    /// starts at `body[0]`; `None` if unknown or it does not fit.
    pub(crate) fn command_size(&self, command: u8, body: &[u8]) -> Option<usize> {
        let mut cursor = Cursor {
            buf: body,
            offset: 0,
        };
        self.size_inner(command, &mut cursor)?;
        Some(cursor.offset)
    }

    #[expect(
        clippy::match_same_arms,
        reason = "one arm per message layout, in svc order"
    )]
    fn size_inner(&self, command: u8, cursor: &mut Cursor) -> Option<()> {
        match command {
            1 | 27 | 28 | 34 | 35 => {}
            2 | 9 | 26 | 31 => cursor.str()?,
            3 => cursor.skip(2)?,
            6 => {
                let channel = cursor.u16()?;
                if channel & 0x8000 != 0 {
                    cursor.skip(1)?;
                }
                if channel & 0x4000 != 0 {
                    cursor.skip(1)?;
                }
                cursor.skip(1 + 3 * self.coord())?;
            }
            8 => {
                cursor.skip(1)?;
                cursor.str()?;
            }
            10 => cursor.skip(1 + 3 * self.angle())?,
            11 => {
                loop {
                    if cursor.u32()? == PROTOCOL_QW {
                        break;
                    }
                    cursor.skip(4)?;
                }
                cursor.skip(4)?;
                cursor.str()?;
                cursor.skip(4)?;
                cursor.str()?;
                cursor.skip(40)?;
            }
            12 => {
                cursor.skip(1)?;
                cursor.str()?;
            }
            14 => cursor.skip(3)?,
            19 => cursor.skip(2 + 3 * self.coord())?,
            20 => cursor.skip(4 + 3 * (self.coord() + self.angle()))?,
            21 | 66 => {
                let word = u32::from(cursor.u16()?);
                self.delta_size(cursor, word)?;
            }
            22 => cursor.skip(2 + 4 + 3 * (self.coord() + self.angle()))?,
            23 => match cursor.u8()? {
                5 | 6 | 9 => cursor.skip(2 + 6 * self.coord())?,
                0 | 1 | 3 | 4 | 7 | 8 | 10 | 11 | 13 => cursor.skip(3 * self.coord())?,
                2 | 12 => cursor.skip(1 + 3 * self.coord())?,
                _ => return None,
            },
            24 | 32 | 44 => cursor.skip(1)?,
            29 => cursor.skip(3 * self.coord() + 3)?,
            30 => cursor.skip(3 * self.wide_coord() + 3 * self.angle())?,
            36 => cursor.skip(3)?,
            37 | 38 => cursor.skip(5)?,
            39 | 53 => cursor.skip(2)?,
            40 => {
                cursor.skip(5)?;
                cursor.str()?;
            }
            41 => {
                let size = cursor.u16()?.cast_signed();
                cursor.skip(1)?;
                if size > 0 {
                    cursor.skip(size.cast_unsigned() as usize)?;
                }
            }
            42 => {
                cursor.skip(1)?;
                let flags = cursor.u16()?;
                cursor.skip(1)?;
                for axis in 0..3 {
                    if flags & (1 << axis) != 0 {
                        cursor.skip(self.coord())?;
                    }
                }
                for axis in 0..3 {
                    if flags & (8 << axis) != 0 {
                        cursor.skip(2)?;
                    }
                }
                for flag in [DF_MODEL, DF_SKIN, DF_EFFECTS, DF_WEAPON_FRAME] {
                    if flags & flag != 0 {
                        cursor.skip(1)?;
                    }
                }
            }
            43 => {
                let count = cursor.u8()? as usize;
                cursor.skip(6 * count)?;
            }
            54 => {
                let count = cursor.u8()? as usize;
                cursor.skip(7 * count)?;
            }
            45 | 46 => {
                cursor.skip(1)?;
                loop {
                    let start = cursor.offset;
                    cursor.str()?;
                    if cursor.offset == start + 1 {
                        break;
                    }
                }
                cursor.skip(1)?;
            }
            47 | 48 => {
                if command == 48 {
                    cursor.skip(1)?;
                }
                loop {
                    let word = u32::from(cursor.u16()?);
                    if word == 0 {
                        break;
                    }
                    self.delta_size(cursor, word)?;
                }
            }
            49 | 50 => cursor.skip(4)?,
            51 => {
                cursor.skip(1)?;
                cursor.str()?;
                cursor.str()?;
            }
            52 => {
                cursor.str()?;
                cursor.str()?;
            }
            _ => return None,
        }
        Some(())
    }

    fn delta_size(&self, cursor: &mut Cursor, word: u32) -> Option<()> {
        let mut bits = word & !511;
        let mut more = 0u32;
        if bits & U_MORE_BITS != 0 {
            bits |= u32::from(cursor.u8()?);
        }
        if bits & U_EVEN_MORE != 0 && self.fte != 0 {
            more = u32::from(cursor.u8()?);
            if more & X_YET_MORE != 0 {
                more |= u32::from(cursor.u8()?) << 8;
            }
        }
        if bits & U_MODEL != 0 {
            cursor.skip(1)?;
        } else if more & X_MODEL_DBL != 0 {
            cursor.skip(2)?;
        }
        for flag in [U_FRAME, U_COLORMAP, U_SKIN, U_EFFECTS] {
            if bits & flag != 0 {
                cursor.skip(1)?;
            }
        }
        for (origin_flag, angle_flag) in [
            (U_ORIGIN_1, U_ANGLE_1),
            (U_ORIGIN_2, U_ANGLE_2),
            (U_ORIGIN_3, U_ANGLE_3),
        ] {
            if bits & origin_flag != 0 {
                cursor.skip(self.coord())?;
            }
            if bits & angle_flag != 0 {
                cursor.skip(self.angle())?;
            }
        }
        if more & X_TRANS != 0 && self.fte & FTE_TRANS != 0 {
            cursor.skip(1)?;
        }
        if more & X_SCALE != 0 {
            cursor.skip(1)?;
        }
        if more & X_FATNESS != 0 {
            cursor.skip(1)?;
        }
        if more & X_COLOR_MOD != 0 && self.fte & FTE_COLOR_MOD != 0 {
            cursor.skip(3)?;
        }
        Some(())
    }
}

/// Bounds-checked cursor used only to measure lengths.
struct Cursor<'a> {
    buf: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    #[inline]
    fn skip(&mut self, count: usize) -> Option<()> {
        if count > self.buf.len() - self.offset {
            return None;
        }
        self.offset += count;
        Some(())
    }
    #[inline]
    fn u8(&mut self) -> Option<u8> {
        let value = *self.buf.get(self.offset)?;
        self.offset += 1;
        Some(value)
    }
    #[inline]
    fn u16(&mut self) -> Option<u16> {
        if self.buf.len() - self.offset < 2 {
            return None;
        }
        let value = u16::from_le_bytes([self.buf[self.offset], self.buf[self.offset + 1]]);
        self.offset += 2;
        Some(value)
    }
    #[inline]
    fn u32(&mut self) -> Option<u32> {
        if self.buf.len() - self.offset < 4 {
            return None;
        }
        let value = le32(&self.buf[self.offset..]);
        self.offset += 4;
        Some(value)
    }
    #[inline]
    fn str(&mut self) -> Option<()> {
        let rest = &self.buf[self.offset..];
        let len = rest.iter().position(|&byte| byte == 0)?;
        self.offset += len + 1;
        Some(())
    }
}

#[inline]
pub(crate) fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(*bytes.first_chunk().expect("caller checked 4 bytes"))
}

/// Total length of the demo frame at `src[start..]`, or `None` if it does
/// not frame (unknown type or truncated).
pub(crate) fn frame_len(src: &[u8], start: usize) -> Option<usize> {
    let remaining = src.len().checked_sub(start)?;
    if remaining < 2 {
        return None;
    }
    let after_time = &src[start + 1..];
    match src[start + 1] & 7 {
        2 => (remaining >= 10).then_some(10),
        3 => {
            if remaining < 10 {
                return None;
            }
            let len = le32(&after_time[5..]) as usize;
            (len <= remaining - 10).then_some(10 + len)
        }
        1 | 4 | 5 | 6 => {
            if remaining < 6 {
                return None;
            }
            let len = le32(&after_time[1..]) as usize;
            (len <= remaining - 6).then_some(6 + len)
        }
        _ => None,
    }
}
