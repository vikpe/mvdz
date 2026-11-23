//! Stream ids, one range per kind. Every input byte lands in one stream: structural
//! (decoded symbol by symbol by the walker), numeric (residuals, after the walk) or blob
//! (plain bytes, as LZ77 tokens).

pub(crate) const TIME: usize = 0;
pub(crate) const TYPE: usize = 1;
pub(crate) const COMMAND: usize = 2;
pub(crate) const PLAYER_FLAGS: usize = 3;
pub(crate) const PLAYER_FRAME: usize = 4;
pub(crate) const PLAYER_WEAPON_FRAME: usize = 5;
pub(crate) const ENTITY_GAP: usize = 6;
pub(crate) const ENTITY_BITS: usize = 7;
pub(crate) const ENTITY_MORE: usize = 8;
pub(crate) const ENTITY_ANGLE: usize = 9;
pub(crate) const STAT_INDEX: usize = 10;
pub(crate) const STAT_VALUE: usize = 11;
pub(crate) const SOUND_CHANNEL: usize = 12;
pub(crate) const SOUND_NUMBER: usize = 13;
pub(crate) const TEMP_ENTITY_TYPE: usize = 14;
/// usercmd blocks: every field but the angles on the grid
pub(crate) const USERCMD: usize = 15;
/// The literals of the blob bytes (`lz.rs`).
pub(crate) const LZ_LITERAL: usize = 16;
pub(crate) const FIRST_NUMERIC_STREAM: usize = 17;

/// The first of three, one per axis.
pub(crate) const PLAYER_ORIGIN: usize = 17;
/// The first of three, one per axis.
pub(crate) const PLAYER_ANGLE: usize = 20;
/// The first of three, one per axis.
pub(crate) const ENTITY_ORIGIN: usize = 23;
pub(crate) const SOUND_ORIGIN: usize = 26;
pub(crate) const TEMP_ENTITY_ORIGIN: usize = 27;
/// The first of three, one per axis.
pub(crate) const PLAYER_FLOAT_ORIGIN: usize = 28;
/// The first of three, one per axis.
pub(crate) const ENTITY_FLOAT_ORIGIN: usize = 31;
/// usercmd angles on the 360/65536 grid
pub(crate) const USERCMD_ANGLE: usize = 34;
/// The match lengths of the blob bytes, 0 for a literal (`lz.rs`).
pub(crate) const LZ_LENGTH: usize = 35;
/// The match distances of the blob bytes (`lz.rs`).
pub(crate) const LZ_DISTANCE: usize = 36;

pub(crate) const FIRST_BLOB_STREAM: usize = 37;
pub(crate) const MASK: usize = 37;
pub(crate) const ESCAPE_LENGTH: usize = 38;
pub(crate) const HIDDEN_TAG: usize = 39;
pub(crate) const HIDDEN_LENGTH: usize = 40;
pub(crate) const HIDDEN_TYPE: usize = 41;
pub(crate) const PLAYER_NUMBER: usize = 42;
pub(crate) const PLAYER_MODEL: usize = 43;
pub(crate) const PLAYER_SKIN: usize = 44;
pub(crate) const PLAYER_EFFECTS: usize = 45;
pub(crate) const ENTITY_HEADER: usize = 46;
pub(crate) const ENTITY_MODEL: usize = 47;
pub(crate) const ENTITY_FRAME: usize = 48;
pub(crate) const ENTITY_MISC: usize = 49;
pub(crate) const SOUND_MISC: usize = 50;
pub(crate) const TEMP_ENTITY_MISC: usize = 51;
pub(crate) const RAW: usize = 52;
pub(crate) const TAIL: usize = 53;
pub(crate) const SET: usize = 54;
pub(crate) const NAILS: usize = 55;
pub(crate) const HIDDEN_MISC: usize = 56;
pub(crate) const SOUND_FLOAT: usize = 57;
pub(crate) const TEMP_ENTITY_FLOAT: usize = 58;
/// One stream per command without a layout of its own: `GENERIC` + the command byte.
pub(crate) const GENERIC: usize = 59;
pub(crate) const STREAM_COUNT: usize = GENERIC + 256;

#[inline]
pub(crate) fn is_structural(id: usize) -> bool {
    id < FIRST_NUMERIC_STREAM
}
#[inline]
pub(crate) fn is_numeric(id: usize) -> bool {
    (FIRST_NUMERIC_STREAM..FIRST_BLOB_STREAM).contains(&id)
}
#[inline]
pub(crate) fn is_blob(id: usize) -> bool {
    (FIRST_BLOB_STREAM..STREAM_COUNT).contains(&id)
}
