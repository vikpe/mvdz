//! `svc_*` body sizes as mvdsv `sv_ents.c` and ezquake `cl_parse.c` write them. A mis-sized
//! command stays lossless: both directions size it alike.

use crate::fields::{read_u8, read_u16, read_u32, read_usize, skip, skip_cstr};

pub(crate) const PROTOCOL_VERSION: u32 = 28;

pub(crate) const FTE_PEXT_COLOR_MOD: u32 = 1 << 19;
pub(crate) const FTE_PEXT_TRANS: u32 = 1 << 3;

pub(crate) const DEM_ALL: u8 = 6;
pub(crate) const DEM_MULTIPLE: u8 = 3;
pub(crate) const DEM_READ: u8 = 1;
pub(crate) const DEM_SET: u8 = 2;
pub(crate) const DEM_SINGLE: u8 = 4;
pub(crate) const DEM_STATS: u8 = 5;

pub(crate) const SVC_CENTER_PRINT: u8 = 26;
pub(crate) const SVC_DAMAGE: u8 = 19;
pub(crate) const SVC_DELTA_PACKET_ENTITIES: u8 = 48;
pub(crate) const SVC_NAILS: u8 = 43;
pub(crate) const SVC_NAILS_2: u8 = 54;
pub(crate) const SVC_PACKET_ENTITIES: u8 = 47;
pub(crate) const SVC_PLAYER_INFO: u8 = 42;
pub(crate) const SVC_PRINT: u8 = 8;
pub(crate) const SVC_SERVER_DATA: u8 = 11;
pub(crate) const SVC_SET_INFO: u8 = 51;
pub(crate) const SVC_SOUND: u8 = 6;
pub(crate) const SVC_STUFF_TEXT: u8 = 9;
pub(crate) const SVC_TEMP_ENTITY: u8 = 23;
pub(crate) const SVC_UPDATE_ENTER_TIME: u8 = 37;
pub(crate) const SVC_UPDATE_FRAGS: u8 = 14;
pub(crate) const SVC_UPDATE_PING: u8 = 36;
pub(crate) const SVC_UPDATE_PL: u8 = 53;
pub(crate) const SVC_UPDATE_STAT: u8 = 3;
pub(crate) const SVC_UPDATE_STAT_LONG: u8 = 38;

pub(crate) const SND_ATTENUATION: u16 = 1 << 14;
pub(crate) const SND_VOLUME: u16 = 1 << 15;

pub(crate) const TE_BLOOD: u8 = 12;
pub(crate) const TE_GUNSHOT: u8 = 2;
pub(crate) const TE_LIGHTNING_1: u8 = 5;
pub(crate) const TE_LIGHTNING_2: u8 = 6;
pub(crate) const TE_LIGHTNING_3: u8 = 9;

const U_ANGLE_1: u32 = 1 << 0;
const U_ANGLE_2: u32 = 1 << 12;
const U_ANGLE_3: u32 = 1 << 1;
pub(crate) const U_COLOR_MAP: u32 = 1 << 3;
pub(crate) const U_EFFECTS: u32 = 1 << 5;
pub(crate) const U_FRAME: u32 = 1 << 13;
pub(crate) const U_MODEL: u32 = 1 << 2;
pub(crate) const U_MORE_BITS: u32 = 1 << 15;
const U_ORIGIN_1: u32 = 1 << 9;
const U_ORIGIN_2: u32 = 1 << 10;
const U_ORIGIN_3: u32 = 1 << 11;
pub(crate) const U_SKIN: u32 = 1 << 4;
/// By axis.
pub(crate) const ORIGIN_ANGLE_FLAGS: [(u32, u32); 3] = [
    (U_ORIGIN_1, U_ANGLE_1),
    (U_ORIGIN_2, U_ANGLE_2),
    (U_ORIGIN_3, U_ANGLE_3),
];

pub(crate) const U_FTE_COLOR_MOD: u32 = 1 << 10;
pub(crate) const U_FTE_ENTITY_DBL: u32 = 1 << 5;
pub(crate) const U_FTE_ENTITY_DBL_2: u32 = 1 << 6;
pub(crate) const U_FTE_EVEN_MORE: u32 = 1 << 7;
pub(crate) const U_FTE_FATNESS: u32 = 1 << 2;
pub(crate) const U_FTE_MODEL_DBL: u32 = 1 << 3;
pub(crate) const U_FTE_SCALE: u32 = 1 << 0;
pub(crate) const U_FTE_TRANS: u32 = 1 << 1;
pub(crate) const U_FTE_YET_MORE: u32 = 1 << 7;

pub(crate) const DF_ANGLES: u16 = 1 << 3;
pub(crate) const DF_EFFECTS: u16 = 1 << 6;
pub(crate) const DF_MODEL: u16 = 1 << 11;
pub(crate) const DF_ORIGIN: u16 = 1 << 0;
pub(crate) const DF_SKIN_NUM: u16 = 1 << 7;
pub(crate) const DF_WEAPON_FRAME: u16 = 1 << 10;
pub(crate) const ORIGIN_FLAGS: u16 = DF_ORIGIN | DF_ORIGIN << 1 | DF_ORIGIN << 2;

/// Derived only from bytes already seen, so both directions agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ProtocolState {
    pub(crate) fte: u32,
    pub(crate) fte2: u32,
    pub(crate) mvd1: u32,
    pub(crate) has_float_coords: bool,
    pub(crate) has_fte_float_coords: bool,
}

impl ProtocolState {
    pub(crate) fn new(fte: u32, fte2: u32, mvd1: u32) -> Self {
        let mut protocol = Self {
            fte,
            fte2,
            mvd1,
            ..Default::default()
        };
        protocol.derive();
        protocol
    }
    pub(crate) fn derive(&mut self) {
        const FTE_PEXT_FLOAT_COORDS: u32 = 1 << 15;
        const MVD_PEXT_1_FLOAT_COORDS: u32 = 1 << 0;
        self.has_float_coords =
            self.fte & FTE_PEXT_FLOAT_COORDS != 0 || self.mvd1 & MVD_PEXT_1_FLOAT_COORDS != 0;
        self.has_fte_float_coords = self.fte & FTE_PEXT_FLOAT_COORDS != 0;
    }
    #[inline]
    pub(crate) fn coord_size(&self) -> usize {
        if self.has_float_coords { 4 } else { 2 }
    }
    #[inline]
    pub(crate) fn wide_coord_size(&self) -> usize {
        if self.has_fte_float_coords { 4 } else { 2 }
    }
    #[inline]
    pub(crate) fn angle_size(&self) -> usize {
        if self.has_fte_float_coords { 2 } else { 1 }
    }

    pub(crate) fn observe_server_data(&mut self, body: &[u8]) {
        const PROTOCOL_VERSION_FTE: u32 = u32::from_le_bytes(*b"FTEX");
        const PROTOCOL_VERSION_FTE_2: u32 = u32::from_le_bytes(*b"FTE2");
        const PROTOCOL_VERSION_MVD_1: u32 = u32::from_le_bytes(*b"MVD1");
        let mut rest = body;
        while let Some(protocol) = read_u32(&mut rest) {
            if protocol == PROTOCOL_VERSION {
                break;
            }
            let Some(extensions) = read_u32(&mut rest) else {
                break;
            };
            match protocol {
                PROTOCOL_VERSION_FTE => self.fte = extensions,
                PROTOCOL_VERSION_FTE_2 => self.fte2 = extensions,
                PROTOCOL_VERSION_MVD_1 => self.mvd1 = extensions,
                _ => {}
            }
        }
        self.derive();
    }

    /// Excludes the command byte.
    pub(crate) fn command_body_size(&self, command: u8, body: &[u8]) -> Option<usize> {
        let mut rest = body;
        self.size_inner(command, &mut rest)?;
        Some(body.len() - rest.len())
    }

    #[expect(
        clippy::match_same_arms,
        reason = "one arm per message layout, in svc order"
    )]
    fn size_inner(&self, command: u8, rest: &mut &[u8]) -> Option<()> {
        const SVC_BIG_KICK: u8 = 35;
        const SVC_CD_TRACK: u8 = 32;
        const SVC_CHOKE_COUNT: u8 = 44;
        const SVC_DISCONNECT: u8 = 2;
        const SVC_DOWNLOAD: u8 = 41;
        const SVC_ENT_GRAVITY: u8 = 50;
        const SVC_FINALE: u8 = 31;
        const SVC_FOUND_SECRET: u8 = 28;
        const SVC_FTE_SPAWN_BASELINE_2: u8 = 66;
        const SVC_FTE_SPAWN_STATIC_2: u8 = 21;
        const SVC_INTERMISSION: u8 = 30;
        const SVC_KILLED_MONSTER: u8 = 27;
        const SVC_LIGHT_STYLE: u8 = 12;
        const SVC_MAX_SPEED: u8 = 49;
        const SVC_MODEL_LIST: u8 = 45;
        const SVC_MUZZLE_FLASH: u8 = 39;
        const SVC_NOP: u8 = 1;
        const SVC_SERVER_INFO: u8 = 52;
        const SVC_SET_ANGLE: u8 = 10;
        const SVC_SET_PAUSE: u8 = 24;
        const SVC_SMALL_KICK: u8 = 34;
        const SVC_SOUND_LIST: u8 = 46;
        const SVC_SPAWN_BASELINE: u8 = 22;
        const SVC_SPAWN_STATIC: u8 = 20;
        const SVC_SPAWN_STATIC_SOUND: u8 = 29;
        const SVC_UPDATE_USER_INFO: u8 = 40;

        const TE_EXPLOSION: u8 = 3;
        const TE_KNIGHT_SPIKE: u8 = 8;
        const TE_LAVA_SPLASH: u8 = 10;
        const TE_LIGHTNING_BLOOD: u8 = 13;
        const TE_SPIKE: u8 = 0;
        const TE_SUPER_SPIKE: u8 = 1;
        const TE_TAR_EXPLOSION: u8 = 4;
        const TE_TELEPORT: u8 = 11;
        const TE_WIZ_SPIKE: u8 = 7;

        match command {
            SVC_NOP | SVC_KILLED_MONSTER | SVC_FOUND_SECRET | SVC_SMALL_KICK | SVC_BIG_KICK => {}
            SVC_DISCONNECT | SVC_STUFF_TEXT | SVC_CENTER_PRINT | SVC_FINALE => skip_cstr(rest)?,
            SVC_UPDATE_STAT => skip(rest, 2)?,
            SVC_SOUND => {
                let channel = read_u16(rest)?;
                if channel & SND_VOLUME != 0 {
                    skip(rest, 1)?;
                }
                if channel & SND_ATTENUATION != 0 {
                    skip(rest, 1)?;
                }
                skip(rest, 1 + 3 * self.coord_size())?;
            }
            SVC_PRINT => {
                skip(rest, 1)?;
                skip_cstr(rest)?;
            }
            SVC_SET_ANGLE => skip(rest, 1 + 3 * self.angle_size())?,
            SVC_SERVER_DATA => {
                loop {
                    if read_u32(rest)? == PROTOCOL_VERSION {
                        break;
                    }
                    skip(rest, 4)?;
                }
                skip(rest, 4)?;
                skip_cstr(rest)?;
                skip(rest, 4)?;
                skip_cstr(rest)?;
                skip(rest, 40)?;
            }
            SVC_LIGHT_STYLE => {
                skip(rest, 1)?;
                skip_cstr(rest)?;
            }
            SVC_UPDATE_FRAGS => skip(rest, 3)?,
            SVC_DAMAGE => skip(rest, 2 + 3 * self.coord_size())?,
            SVC_SPAWN_STATIC => skip(rest, 4 + 3 * (self.coord_size() + self.angle_size()))?,
            SVC_FTE_SPAWN_STATIC_2 | SVC_FTE_SPAWN_BASELINE_2 => {
                let word = u32::from(read_u16(rest)?);
                self.skip_entity_delta(rest, word)?;
            }
            SVC_SPAWN_BASELINE => skip(rest, 2 + 4 + 3 * (self.coord_size() + self.angle_size()))?,
            SVC_TEMP_ENTITY => match read_u8(rest)? {
                TE_LIGHTNING_1 | TE_LIGHTNING_2 | TE_LIGHTNING_3 => {
                    skip(rest, 2 + 6 * self.coord_size())?;
                }
                TE_SPIKE | TE_SUPER_SPIKE | TE_EXPLOSION | TE_TAR_EXPLOSION | TE_WIZ_SPIKE
                | TE_KNIGHT_SPIKE | TE_LAVA_SPLASH | TE_TELEPORT | TE_LIGHTNING_BLOOD => {
                    skip(rest, 3 * self.coord_size())?;
                }
                TE_GUNSHOT | TE_BLOOD => skip(rest, 1 + 3 * self.coord_size())?,
                _ => return None,
            },
            SVC_SET_PAUSE | SVC_CD_TRACK | SVC_CHOKE_COUNT => skip(rest, 1)?,
            SVC_SPAWN_STATIC_SOUND => skip(rest, 3 * self.coord_size() + 3)?,
            SVC_INTERMISSION => skip(rest, 3 * self.wide_coord_size() + 3 * self.angle_size())?,
            SVC_UPDATE_PING => skip(rest, 3)?,
            SVC_UPDATE_ENTER_TIME | SVC_UPDATE_STAT_LONG => skip(rest, 5)?,
            SVC_MUZZLE_FLASH | SVC_UPDATE_PL => skip(rest, 2)?,
            SVC_UPDATE_USER_INFO => {
                skip(rest, 5)?;
                skip_cstr(rest)?;
            }
            SVC_DOWNLOAD => {
                let size = read_u16(rest)?.cast_signed();
                skip(rest, 1)?;
                if size > 0 {
                    skip(rest, size.cast_unsigned() as usize)?;
                }
            }
            SVC_PLAYER_INFO => {
                skip(rest, 1)?;
                let flags = read_u16(rest)?;
                skip(rest, 1)?;
                for axis in 0..3 {
                    if flags & DF_ORIGIN << axis != 0 {
                        skip(rest, self.coord_size())?;
                    }
                }
                for axis in 0..3 {
                    if flags & DF_ANGLES << axis != 0 {
                        skip(rest, 2)?;
                    }
                }
                for flag in [DF_MODEL, DF_SKIN_NUM, DF_EFFECTS, DF_WEAPON_FRAME] {
                    if flags & flag != 0 {
                        skip(rest, 1)?;
                    }
                }
            }
            SVC_NAILS => {
                let count = read_u8(rest)? as usize;
                skip(rest, 6 * count)?;
            }
            SVC_NAILS_2 => {
                let count = read_u8(rest)? as usize;
                skip(rest, 7 * count)?;
            }
            SVC_MODEL_LIST | SVC_SOUND_LIST => {
                skip(rest, 1)?;
                loop {
                    let before = rest.len();
                    skip_cstr(rest)?;
                    if before - rest.len() == 1 {
                        break;
                    }
                }
                skip(rest, 1)?;
            }
            SVC_PACKET_ENTITIES | SVC_DELTA_PACKET_ENTITIES => {
                if command == SVC_DELTA_PACKET_ENTITIES {
                    skip(rest, 1)?;
                }
                loop {
                    let word = u32::from(read_u16(rest)?);
                    if word == 0 {
                        break;
                    }
                    self.skip_entity_delta(rest, word)?;
                }
            }
            SVC_MAX_SPEED | SVC_ENT_GRAVITY => skip(rest, 4)?,
            SVC_SET_INFO => {
                skip(rest, 1)?;
                skip_cstr(rest)?;
                skip_cstr(rest)?;
            }
            SVC_SERVER_INFO => {
                skip_cstr(rest)?;
                skip_cstr(rest)?;
            }
            _ => return None,
        }
        Some(())
    }

    fn skip_entity_delta(&self, rest: &mut &[u8], word: u32) -> Option<()> {
        let mut bits = word & !511;
        let mut more = 0u32;
        if bits & U_MORE_BITS != 0 {
            bits |= u32::from(read_u8(rest)?);
        }
        if bits & U_FTE_EVEN_MORE != 0 && self.fte != 0 {
            more = u32::from(read_u8(rest)?);
            if more & U_FTE_YET_MORE != 0 {
                more |= u32::from(read_u8(rest)?) << 8;
            }
        }
        if bits & U_MODEL != 0 {
            skip(rest, 1)?;
        } else if more & U_FTE_MODEL_DBL != 0 {
            skip(rest, 2)?;
        }
        for flag in [U_FRAME, U_COLOR_MAP, U_SKIN, U_EFFECTS] {
            if bits & flag != 0 {
                skip(rest, 1)?;
            }
        }
        for (origin_flag, angle_flag) in ORIGIN_ANGLE_FLAGS {
            if bits & origin_flag != 0 {
                skip(rest, self.coord_size())?;
            }
            if bits & angle_flag != 0 {
                skip(rest, self.angle_size())?;
            }
        }
        if more & U_FTE_TRANS != 0 && self.fte & FTE_PEXT_TRANS != 0 {
            skip(rest, 1)?;
        }
        if more & U_FTE_SCALE != 0 {
            skip(rest, 1)?;
        }
        if more & U_FTE_FATNESS != 0 {
            skip(rest, 1)?;
        }
        if more & U_FTE_COLOR_MOD != 0 && self.fte & FTE_PEXT_COLOR_MOD != 0 {
            skip(rest, 3)?;
        }
        Some(())
    }
}

/// Includes the frame header.
pub(crate) fn frame_len(src: &[u8], start: usize) -> Option<usize> {
    let remaining = src.len().checked_sub(start)?;
    let payload_len_at = |offset: usize| read_usize(&mut src.get(start + offset..)?);
    match src.get(start + 1)? & 7 {
        DEM_SET => (remaining >= 10).then_some(10),
        DEM_MULTIPLE => {
            let len = payload_len_at(6)?;
            (len <= remaining - 10).then_some(10 + len)
        }
        DEM_READ | DEM_SINGLE | DEM_STATS | DEM_ALL => {
            let len = payload_len_at(2)?;
            (len <= remaining - 6).then_some(6 + len)
        }
        _ => None,
    }
}
