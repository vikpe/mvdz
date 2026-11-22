//! Little-endian fields on a `&mut &[u8]` cursor: `None` past the end, nothing consumed.

pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// As a u32: callers' values are bounded by the 1 MiB segment, 2 MiB coded segment or 16 MiB
/// models block.
pub(crate) fn put_usize(out: &mut Vec<u8>, value: usize) {
    debug_assert!(
        u32::try_from(value).is_ok(),
        "{value} does not fit in a u32"
    );
    put_u32(out, value as u32);
}

pub(crate) fn read_array<'a, const N: usize>(bytes: &mut &'a [u8]) -> Option<&'a [u8; N]> {
    let (head, tail) = bytes.split_first_chunk::<N>()?;
    *bytes = tail;
    Some(head)
}

pub(crate) fn read_u8(bytes: &mut &[u8]) -> Option<u8> {
    let (&head, tail) = bytes.split_first()?;
    *bytes = tail;
    Some(head)
}

pub(crate) fn read_u16(bytes: &mut &[u8]) -> Option<u16> {
    read_array(bytes).map(|le_bytes| u16::from_le_bytes(*le_bytes))
}

pub(crate) fn read_u32(bytes: &mut &[u8]) -> Option<u32> {
    read_array(bytes).map(|le_bytes| u32::from_le_bytes(*le_bytes))
}

pub(crate) fn read_u64(bytes: &mut &[u8]) -> Option<u64> {
    read_array(bytes).map(|le_bytes| u64::from_le_bytes(*le_bytes))
}

pub(crate) fn read_usize(bytes: &mut &[u8]) -> Option<usize> {
    usize::try_from(read_u32(bytes)?).ok()
}

pub(crate) fn read_bytes<'a>(bytes: &mut &'a [u8], len: usize) -> Option<&'a [u8]> {
    let (head, tail) = bytes.split_at_checked(len)?;
    *bytes = tail;
    Some(head)
}

pub(crate) fn read_length_prefixed<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = read_usize(bytes)?;
    read_bytes(bytes, len)
}

pub(crate) fn skip(bytes: &mut &[u8], len: usize) -> Option<()> {
    read_bytes(bytes, len).map(|_| ())
}

/// Past the NUL too.
pub(crate) fn skip_cstr(bytes: &mut &[u8]) -> Option<()> {
    let len = bytes.iter().position(|&byte| byte == 0)?;
    skip(bytes, len + 1)
}

/// The caller checked for 2 bytes.
#[inline]
pub(crate) fn le16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(*bytes.first_chunk().expect("caller checked 2 bytes"))
}

/// The caller checked for 4 bytes.
#[inline]
pub(crate) fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(*bytes.first_chunk().expect("caller checked 4 bytes"))
}
