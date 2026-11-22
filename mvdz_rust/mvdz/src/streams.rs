//! Stream ids, one range per kind. Every input byte lands in one stream: lazy (structural,
//! decoded symbol by symbol by the walker), numeric (residuals, after the walk) or blob
//! (plain bytes, DEFLATE).

pub(crate) const TIME: usize = 0;
pub(crate) const TYPE: usize = 1;
pub(crate) const CMD: usize = 2;
pub(crate) const PI_FLAGS: usize = 3;
pub(crate) const PI_FRAME: usize = 4;
pub(crate) const PI_WFRAME: usize = 5;
pub(crate) const PE_ENT: usize = 6;
pub(crate) const PE_BITS: usize = 7;
pub(crate) const PE_MORE: usize = 8;
pub(crate) const PE_A: usize = 9;
pub(crate) const ST_IDX: usize = 10;
pub(crate) const ST_VAL: usize = 11;
pub(crate) const SND_CH: usize = 12;
pub(crate) const SND_NUM: usize = 13;
pub(crate) const TE_TYPE: usize = 14;
/// usercmd blocks: every field but the angles on the grid
pub(crate) const UC: usize = 15;
pub(crate) const LAZY_STREAM_COUNT: usize = 16;

/// The first of three, one per axis.
pub(crate) const PI_OX: usize = 16;
/// The first of three, one per axis.
pub(crate) const PI_AP: usize = 19;
/// The first of three, one per axis.
pub(crate) const PE_OX: usize = 22;
pub(crate) const SND_O: usize = 25;
pub(crate) const TE_O: usize = 26;
/// The first of three, one per axis.
pub(crate) const PI_F0: usize = 27;
/// The first of three, one per axis.
pub(crate) const PE_F0: usize = 30;
/// usercmd angles on the 360/65536 grid
pub(crate) const UC_A: usize = 33;

const BLOB_BASE: usize = 34;
pub(crate) const MASK: usize = 34;
pub(crate) const ESCLEN: usize = 35;
pub(crate) const HIDTAG: usize = 36;
pub(crate) const HIDLEN: usize = 37;
pub(crate) const HIDTYPE: usize = 38;
pub(crate) const HIDDMG: usize = 39;
pub(crate) const PI_NUM: usize = 40;
pub(crate) const PI_MODEL: usize = 41;
pub(crate) const PI_SKIN: usize = 42;
pub(crate) const PI_EFF: usize = 43;
pub(crate) const PE_HDR: usize = 44;
pub(crate) const PE_MODEL: usize = 45;
pub(crate) const PE_FRAME: usize = 46;
pub(crate) const PE_MISC: usize = 47;
pub(crate) const STL_IDX: usize = 48;
pub(crate) const SND_MISC: usize = 49;
pub(crate) const TE_MISC: usize = 50;
pub(crate) const PR_LEVEL: usize = 51;
pub(crate) const FRAGS: usize = 52;
pub(crate) const STL_VAL: usize = 53;
pub(crate) const RAW: usize = 54;
pub(crate) const TAIL: usize = 55;
pub(crate) const SET: usize = 56;
pub(crate) const TEXT: usize = 57;
pub(crate) const PING: usize = 58;
pub(crate) const NAILS: usize = 59;
pub(crate) const HIDMISC: usize = 60;
pub(crate) const SNDF: usize = 61;
pub(crate) const TEF: usize = 62;
/// One stream per command without a layout of its own: `GEN_BASE` + the command byte.
pub(crate) const GEN_BASE: usize = 63;
pub(crate) const STREAM_COUNT: usize = GEN_BASE + 256;

#[inline]
pub(crate) fn is_lazy(id: usize) -> bool {
    id < LAZY_STREAM_COUNT
}
#[inline]
pub(crate) fn is_numeric(id: usize) -> bool {
    (LAZY_STREAM_COUNT..BLOB_BASE).contains(&id)
}
#[inline]
pub(crate) fn is_blob(id: usize) -> bool {
    (BLOB_BASE..STREAM_COUNT).contains(&id)
}
