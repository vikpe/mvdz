//! Stream ids.
//!
//! Every byte of the input lands in exactly one
//! stream: a lazy structural byte stream (coded with a side context, decoded
//! one symbol at a time by the walker), a blob stream (plain bytes, decoded
//! up front), or a numeric stream (residuals coded by the numeric groups
//! after the walk).
//!
//! Ids below 80 were all structural or numeric streams once; the ones in
//! [`PLAIN`] have no model and are blob streams.

pub const TIME: usize = 0;
pub const TYPE: usize = 1;
pub const MASK: usize = 2;
pub(crate) const CMD: usize = 3;
pub(crate) const ESCLEN: usize = 4;
pub const HIDTAG: usize = 5;
pub const HIDLEN: usize = 6;
pub const HIDTYPE: usize = 7;
pub(crate) const HIDDMG: usize = 8;
pub const PI_NUM: usize = 9;
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
/// usercmd hidden blocks: every field but the angles on the grid
pub const UC: usize = 34;
/// The fewest UC symbols one usercmd block codes (its angles all on the
/// grid): player, dropnum, msec, grid, three moves of two bytes, buttons,
/// impulse. Bounds the usercmd record list (FORMAT.md §6.3).
pub(crate) const UC_MIN_SYMS: usize = 12;
pub const N_LAZY: usize = 35;

// numeric streams
pub(crate) const NUM_BASE: usize = 48;
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
pub(crate) const N_NUM_END: usize = 67;

// blob streams
pub(crate) const RAW: usize = 80;
pub(crate) const TAIL: usize = 81;
pub(crate) const SET: usize = 82;
pub const TEXT: usize = 83;
pub(crate) const PING: usize = 84;
pub(crate) const NAILS: usize = 85;
pub const HIDMISC: usize = 86;
pub(crate) const SNDF: usize = 87;
pub(crate) const TEF: usize = 88;
/// Not a stored stream: the id under which the blob body of a segment (the
/// DEFLATE stream of all its blob streams) is reported in statistics.
pub(crate) const BLOB_BODY: usize = 78;
pub(crate) const GEN_BASE: usize = 128;
pub(crate) const N_STREAMS: usize = GEN_BASE + 256;

/// Blob streams with an id below 80: fields whose model saved too little to
/// be worth specifying (SIMPLIFY.md).
pub(crate) const PLAIN: [usize; 20] = [
    MASK, ESCLEN, HIDTAG, HIDLEN, HIDTYPE, HIDDMG, PI_NUM, PI_MODEL, PI_SKIN, PI_EFF, PE_HDR,
    PE_MODEL, PE_FRAME, PE_MISC, STL_IDX, SND_MISC, TE_MISC, PR_LEVEL, FRAGS, STL_VAL,
];
/// Bit `id` set: `id` is in [`PLAIN`].
const PLAIN_MASK: u64 = {
    let (mut m, mut i) = (0u64, 0);
    while i < PLAIN.len() {
        m |= 1 << PLAIN[i];
        i += 1;
    }
    m
};
#[inline]
fn is_plain(id: usize) -> bool {
    id < 64 && PLAIN_MASK >> id & 1 != 0
}
#[inline]
pub fn is_lazy(id: usize) -> bool {
    id < N_LAZY && !is_plain(id)
}
#[inline]
pub fn is_num(id: usize) -> bool {
    (NUM_BASE..N_NUM_END).contains(&id) && !is_plain(id)
}
#[inline]
pub(crate) fn is_blob(id: usize) -> bool {
    is_plain(id) || (RAW..N_STREAMS).contains(&id) && !(TEF + 1..GEN_BASE).contains(&id)
}

pub fn name(id: usize) -> String {
    const LAZY: [&str; N_LAZY] = [
        "time",
        "type",
        "mask",
        "cmd",
        "esclen",
        "hid.tag",
        "hid.len",
        "hid.type",
        "hid.dmg",
        "pi.num",
        "pi.flags",
        "pi.frame",
        "pi.model",
        "pi.skin",
        "pi.eff",
        "pi.wframe",
        "pe.hdr",
        "pe.ent",
        "pe.bits",
        "pe.more",
        "pe.model",
        "pe.frame",
        "pe.misc",
        "pe.a",
        "st.idx",
        "st.val",
        "stl.idx",
        "snd.ch",
        "snd.misc",
        "snd.num",
        "te.type",
        "te.misc",
        "pr.level",
        "frags",
        "uc",
    ];
    const NUM: [&str; N_NUM_END - NUM_BASE] = [
        "pi.ox", "pi.oy", "pi.oz", "pi.ap", "pi.ay", "pi.ar", "pe.ox", "pe.oy", "pe.oz", "snd.o",
        "te.o", "stl.val", "pi.fx", "pi.fy", "pi.fz", "pe.fx", "pe.fy", "pe.fz", "uc.a",
    ];
    const BLOB: [&str; 9] = [
        "raw", "tail", "set", "text", "ping", "nails", "hid.misc", "snd.f", "te.f",
    ];
    if id < N_LAZY {
        LAZY[id].into()
    } else if is_num(id) {
        NUM[id - NUM_BASE].into()
    } else if (RAW..=TEF).contains(&id) {
        BLOB[id - RAW].into()
    } else if id == BLOB_BODY {
        "blob".into()
    } else if (GEN_BASE..N_STREAMS).contains(&id) {
        format!("gen.{}", id - GEN_BASE)
    } else {
        format!("?{id}")
    }
}
