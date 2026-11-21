//! Stream ids. Every input byte lands in one stream: lazy (structural, decoded symbol by
//! symbol by the walker), blob (plain bytes, DEFLATE) or numeric (residuals, after the walk).

pub(crate) const TIME: usize = 0;
pub(crate) const TYPE: usize = 1;
pub(crate) const MASK: usize = 2;
pub(crate) const CMD: usize = 3;
pub(crate) const ESCLEN: usize = 4;
pub(crate) const HIDTAG: usize = 5;
pub(crate) const HIDLEN: usize = 6;
pub(crate) const HIDTYPE: usize = 7;
pub(crate) const HIDDMG: usize = 8;
pub(crate) const PI_NUM: usize = 9;
pub(crate) const PI_FLAGS: usize = 10;
pub(crate) const PI_FRAME: usize = 11;
pub(crate) const PI_MODEL: usize = 12;
pub(crate) const PI_SKIN: usize = 13;
pub(crate) const PI_EFF: usize = 14;
pub(crate) const PI_WFRAME: usize = 15;
pub(crate) const PE_HDR: usize = 16;
pub(crate) const PE_ENT: usize = 17;
pub(crate) const PE_BITS: usize = 18;
pub(crate) const PE_MORE: usize = 19;
pub(crate) const PE_MODEL: usize = 20;
pub(crate) const PE_FRAME: usize = 21;
pub(crate) const PE_MISC: usize = 22;
pub(crate) const PE_A: usize = 23;
pub(crate) const ST_IDX: usize = 24;
pub(crate) const ST_VAL: usize = 25;
pub(crate) const STL_IDX: usize = 26;
pub(crate) const SND_CH: usize = 27;
pub(crate) const SND_MISC: usize = 28;
pub(crate) const SND_NUM: usize = 29;
pub(crate) const TE_TYPE: usize = 30;
pub(crate) const TE_MISC: usize = 31;
pub(crate) const PR_LEVEL: usize = 32;
pub(crate) const FRAGS: usize = 33;
/// usercmd blocks: every field but the angles on the grid
pub(crate) const UC: usize = 34;
/// Player, dropnum, msec, grid, three 2-byte moves, buttons, impulse. Bounds the usercmd
/// record list (FORMAT.md section 6.3).
pub(crate) const UC_MIN_SYMBOLS: usize = 12;
pub(crate) const LAZY_STREAM_COUNT: usize = 35;

// numeric streams
pub(crate) const PI_OX: usize = 48;
pub(crate) const PI_AP: usize = 51;
pub(crate) const PE_OX: usize = 54;
pub(crate) const SND_O: usize = 57;
pub(crate) const TE_O: usize = 58;
pub(crate) const STL_VAL: usize = 59;
pub(crate) const PI_F0: usize = 60;
pub(crate) const PE_F0: usize = 63;
/// usercmd angles on the 360/65536 grid
pub(crate) const UC_A: usize = 66;

// blob streams
pub(crate) const RAW: usize = 80;
pub(crate) const TAIL: usize = 81;
pub(crate) const SET: usize = 82;
pub(crate) const TEXT: usize = 83;
pub(crate) const PING: usize = 84;
pub(crate) const NAILS: usize = 85;
pub(crate) const HIDMISC: usize = 86;
pub(crate) const SNDF: usize = 87;
pub(crate) const TEF: usize = 88;
/// Not stored: the id under which statistics report a segment's DEFLATE blob body.
pub(crate) const BLOB_BODY: usize = 78;
pub(crate) const GEN_BASE: usize = 128;
pub(crate) const STREAM_COUNT: usize = GEN_BASE + 256;

/// Blob streams below id 80: their models saved too little to specify (SIMPLIFY.md).
pub(crate) const PLAIN: [usize; 20] = [
    MASK, ESCLEN, HIDTAG, HIDLEN, HIDTYPE, HIDDMG, PI_NUM, PI_MODEL, PI_SKIN, PI_EFF, PE_HDR,
    PE_MODEL, PE_FRAME, PE_MISC, STL_IDX, SND_MISC, TE_MISC, PR_LEVEL, FRAGS, STL_VAL,
];
#[inline]
fn is_plain(id: usize) -> bool {
    const PLAIN_MASK: u64 = {
        let (mut mask, mut index) = (0u64, 0);
        while index < PLAIN.len() {
            mask |= 1 << PLAIN[index];
            index += 1;
        }
        mask
    };
    id < 64 && PLAIN_MASK >> id & 1 != 0
}
#[inline]
pub(crate) fn is_lazy(id: usize) -> bool {
    id < LAZY_STREAM_COUNT && !is_plain(id)
}
#[inline]
pub(crate) fn is_numeric(id: usize) -> bool {
    (PI_OX..=UC_A).contains(&id) && !is_plain(id)
}
#[inline]
pub(crate) fn is_blob(id: usize) -> bool {
    is_plain(id) || (RAW..STREAM_COUNT).contains(&id) && !(TEF + 1..GEN_BASE).contains(&id)
}
