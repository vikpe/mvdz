//! Protocol state and the bounds-checked command sizer.
//!
//! The sizer is pure wire-layout knowledge (mvdsv `sv_demo.c`, `sv_ents.c`,
//! ezquake `cl_parse.c` / `cl_ents.c`): it reports how many body bytes a
//! `svc_*` command occupies, or `None` when the command is unknown or does
//! not fit. A mis-sized command never breaks losslessness: encoder and
//! decoder size the same bytes the same way, the encoder then asserts that
//! the transform consumed exactly the sized length.

pub(crate) const FTE_TRANS: u32 = 0x0000_0008;
pub(crate) const FTE_FLOAT: u32 = 0x0000_8000;
pub(crate) const FTE_COLOURMOD: u32 = 0x0008_0000;
pub(crate) const MVD1_FLOAT: u32 = 1;
pub(crate) const PROTO_FTE: u32 = 0x5845_5446; // 'FTEX'
pub(crate) const PROTO_FTE2: u32 = 0x3245_5446; // 'FTE2'
pub(crate) const PROTO_MVD1: u32 = 0x3144_564D; // 'MVD1'
pub(crate) const PROTOCOL_QW: u32 = 28;

pub(crate) const U_MOREBITS: u32 = 1 << 15;
pub(crate) const U_ORIGIN1: u32 = 1 << 9;
pub(crate) const U_ORIGIN2: u32 = 1 << 10;
pub(crate) const U_ORIGIN3: u32 = 1 << 11;
pub(crate) const U_ANGLE2: u32 = 1 << 12;
pub(crate) const U_FRAME: u32 = 1 << 13;
pub(crate) const U_ANGLE1: u32 = 1 << 0;
pub(crate) const U_ANGLE3: u32 = 1 << 1;
pub(crate) const U_MODEL: u32 = 1 << 2;
pub(crate) const U_COLORMAP: u32 = 1 << 3;
pub(crate) const U_SKIN: u32 = 1 << 4;
pub(crate) const U_EFFECTS: u32 = 1 << 5;
pub(crate) const U_EVENMORE: u32 = 1 << 7;
pub(crate) const X_SCALE: u32 = 1 << 0;
pub(crate) const X_TRANS: u32 = 1 << 1;
pub(crate) const X_FATNESS: u32 = 1 << 2;
pub(crate) const X_MODELDBL: u32 = 1 << 3;
pub(crate) const X_ENTITYDBL: u32 = 1 << 5;
pub(crate) const X_ENTITYDBL2: u32 = 1 << 6;
pub(crate) const X_YETMORE: u32 = 1 << 7;
pub(crate) const X_COLOURMOD: u32 = 1 << 10;

pub(crate) const DF_MODEL: u16 = 1 << 11;
pub(crate) const DF_SKIN: u16 = 1 << 7;
pub(crate) const DF_EFFECTS: u16 = 1 << 6;
pub(crate) const DF_WEAPONFRAME: u16 = 1 << 10;

/// Per-demo protocol state that changes command widths. It is derived only
/// from bytes already seen, so both directions agree.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Proto {
    pub fte: u32,
    pub(crate) fte2: u32,
    pub(crate) mvd1: u32,
    /// FTE or MVD1 float coords (entity/player origins, sounds, TEs).
    pub(crate) float_coords: bool,
    /// FTE float coords: generic coords 4 bytes, angles 2.
    pub(crate) msg_wide: bool,
}

impl Proto {
    pub(crate) fn new(fte: u32, fte2: u32, mvd1: u32) -> Self {
        let mut p = Self {
            fte,
            fte2,
            mvd1,
            ..Default::default()
        };
        p.derive();
        p
    }
    pub(crate) fn derive(&mut self) {
        self.float_coords = self.fte & FTE_FLOAT != 0 || self.mvd1 & MVD1_FLOAT != 0;
        self.msg_wide = self.fte & FTE_FLOAT != 0;
    }
    #[inline]
    pub fn coord(&self) -> usize {
        if self.float_coords { 4 } else { 2 }
    }
    #[inline]
    pub fn wcoord(&self) -> usize {
        if self.msg_wide { 4 } else { 2 }
    }
    #[inline]
    pub fn angle(&self) -> usize {
        if self.msg_wide { 2 } else { 1 }
    }

    /// Updates the protocol state from an `svc_serverdata` body.
    pub fn observe_server_data(&mut self, b: &[u8]) {
        let mut i = 0;
        while i + 4 <= b.len() {
            let v = le32(&b[i..]);
            i += 4;
            if v == PROTOCOL_QW {
                break;
            }
            if i + 4 > b.len() {
                break;
            }
            let f = le32(&b[i..]);
            i += 4;
            match v {
                PROTO_FTE => self.fte = f,
                PROTO_FTE2 => self.fte2 = f,
                PROTO_MVD1 => self.mvd1 = f,
                _ => {}
            }
        }
        self.derive();
    }

    /// Body length (excluding the command byte) of `cmd` whose body starts
    /// at `b[0]`; `None` if unknown or it does not fit.
    pub(crate) fn cmd_size(&self, cmd: u8, b: &[u8]) -> Option<usize> {
        let mut c = Cur { b, i: 0 };
        self.size_inner(cmd, &mut c)?;
        Some(c.i)
    }

    #[expect(
        clippy::match_same_arms,
        reason = "one arm per message layout, in svc order"
    )]
    fn size_inner(&self, cmd: u8, c: &mut Cur) -> Option<()> {
        match cmd {
            1 | 27 | 28 | 34 | 35 => {}
            2 | 9 | 26 | 31 => c.str()?,
            3 => c.skip(2)?,
            6 => {
                let ch = c.u16()?;
                if ch & 0x8000 != 0 {
                    c.skip(1)?;
                }
                if ch & 0x4000 != 0 {
                    c.skip(1)?;
                }
                c.skip(1 + 3 * self.coord())?;
            }
            8 => {
                c.skip(1)?;
                c.str()?;
            }
            10 => c.skip(1 + 3 * self.angle())?,
            11 => {
                loop {
                    if c.u32()? == PROTOCOL_QW {
                        break;
                    }
                    c.skip(4)?;
                }
                c.skip(4)?;
                c.str()?;
                c.skip(4)?;
                c.str()?;
                c.skip(40)?;
            }
            12 => {
                c.skip(1)?;
                c.str()?;
            }
            14 => c.skip(3)?,
            19 => c.skip(2 + 3 * self.coord())?,
            20 => c.skip(4 + 3 * (self.coord() + self.angle()))?,
            21 | 66 => {
                let w = u32::from(c.u16()?);
                self.delta_size(c, w)?;
            }
            22 => c.skip(2 + 4 + 3 * (self.coord() + self.angle()))?,
            23 => match c.u8()? {
                5 | 6 | 9 => c.skip(2 + 6 * self.coord())?,
                0 | 1 | 3 | 4 | 7 | 8 | 10 | 11 | 13 => c.skip(3 * self.coord())?,
                2 | 12 => c.skip(1 + 3 * self.coord())?,
                _ => return None,
            },
            24 | 32 | 44 => c.skip(1)?,
            29 => c.skip(3 * self.coord() + 3)?,
            30 => c.skip(3 * self.wcoord() + 3 * self.angle())?,
            36 => c.skip(3)?,
            37 | 38 => c.skip(5)?,
            39 | 53 => c.skip(2)?,
            40 => {
                c.skip(5)?;
                c.str()?;
            }
            41 => {
                let sz = c.u16()?.cast_signed();
                c.skip(1)?;
                if sz > 0 {
                    c.skip(sz.cast_unsigned() as usize)?;
                }
            }
            42 => {
                c.skip(1)?;
                let f = c.u16()?;
                c.skip(1)?;
                for i in 0..3 {
                    if f & (1 << i) != 0 {
                        c.skip(self.coord())?;
                    }
                }
                for i in 0..3 {
                    if f & (8 << i) != 0 {
                        c.skip(2)?;
                    }
                }
                for m in [DF_MODEL, DF_SKIN, DF_EFFECTS, DF_WEAPONFRAME] {
                    if f & m != 0 {
                        c.skip(1)?;
                    }
                }
            }
            43 => {
                let n = c.u8()? as usize;
                c.skip(6 * n)?;
            }
            54 => {
                let n = c.u8()? as usize;
                c.skip(7 * n)?;
            }
            45 | 46 => {
                c.skip(1)?;
                loop {
                    let s = c.i;
                    c.str()?;
                    if c.i == s + 1 {
                        break;
                    }
                }
                c.skip(1)?;
            }
            47 | 48 => {
                if cmd == 48 {
                    c.skip(1)?;
                }
                loop {
                    let w = u32::from(c.u16()?);
                    if w == 0 {
                        break;
                    }
                    self.delta_size(c, w)?;
                }
            }
            49 | 50 => c.skip(4)?,
            51 => {
                c.skip(1)?;
                c.str()?;
                c.str()?;
            }
            52 => {
                c.str()?;
                c.str()?;
            }
            _ => return None,
        }
        Some(())
    }

    fn delta_size(&self, c: &mut Cur, w: u32) -> Option<()> {
        let mut bits = w & !511;
        let mut more = 0u32;
        if bits & U_MOREBITS != 0 {
            bits |= u32::from(c.u8()?);
        }
        if bits & U_EVENMORE != 0 && self.fte != 0 {
            more = u32::from(c.u8()?);
            if more & X_YETMORE != 0 {
                more |= u32::from(c.u8()?) << 8;
            }
        }
        if bits & U_MODEL != 0 {
            c.skip(1)?;
        } else if more & X_MODELDBL != 0 {
            c.skip(2)?;
        }
        for m in [U_FRAME, U_COLORMAP, U_SKIN, U_EFFECTS] {
            if bits & m != 0 {
                c.skip(1)?;
            }
        }
        for (o, a) in [
            (U_ORIGIN1, U_ANGLE1),
            (U_ORIGIN2, U_ANGLE2),
            (U_ORIGIN3, U_ANGLE3),
        ] {
            if bits & o != 0 {
                c.skip(self.coord())?;
            }
            if bits & a != 0 {
                c.skip(self.angle())?;
            }
        }
        if more & X_TRANS != 0 && self.fte & FTE_TRANS != 0 {
            c.skip(1)?;
        }
        if more & X_SCALE != 0 {
            c.skip(1)?;
        }
        if more & X_FATNESS != 0 {
            c.skip(1)?;
        }
        if more & X_COLOURMOD != 0 && self.fte & FTE_COLOURMOD != 0 {
            c.skip(3)?;
        }
        Some(())
    }
}

/// Bounds-checked cursor used only to measure lengths.
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cur<'_> {
    #[inline]
    fn skip(&mut self, n: usize) -> Option<()> {
        if n > self.b.len() - self.i {
            return None;
        }
        self.i += n;
        Some(())
    }
    #[inline]
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    #[inline]
    fn u16(&mut self) -> Option<u16> {
        if self.b.len() - self.i < 2 {
            return None;
        }
        let v = u16::from_le_bytes([self.b[self.i], self.b[self.i + 1]]);
        self.i += 2;
        Some(v)
    }
    #[inline]
    fn u32(&mut self) -> Option<u32> {
        if self.b.len() - self.i < 4 {
            return None;
        }
        let v = le32(&self.b[self.i..]);
        self.i += 4;
        Some(v)
    }
    #[inline]
    fn str(&mut self) -> Option<()> {
        let rest = &self.b[self.i..];
        let n = rest.iter().position(|&x| x == 0)?;
        self.i += n + 1;
        Some(())
    }
}

#[inline]
pub(crate) fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Total length of the demo frame at `src[p..]`, or `None` if it does not
/// frame (unknown type or truncated).
pub(crate) fn frame_len(src: &[u8], p: usize) -> Option<usize> {
    let rem = src.len().checked_sub(p)?;
    if rem < 2 {
        return None;
    }
    let h = &src[p + 1..];
    match src[p + 1] & 7 {
        2 => (rem >= 10).then_some(10),
        3 => {
            if rem < 10 {
                return None;
            }
            let n = le32(&h[5..]) as usize;
            (n <= rem - 10).then_some(10 + n)
        }
        1 | 4 | 5 | 6 => {
            if rem < 6 {
                return None;
            }
            let n = le32(&h[1..]) as usize;
            (n <= rem - 6).then_some(6 + n)
        }
        _ => None,
    }
}
