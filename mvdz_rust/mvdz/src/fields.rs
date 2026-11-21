/// Every caller's value is bounded by the 1 MiB segment, the 2 MiB coded segment or the
/// 16 MiB models block.
pub(crate) fn put_u32(out: &mut Vec<u8>, value: usize) {
    debug_assert!(
        u32::try_from(value).is_ok(),
        "{value} does not fit in a u32"
    );
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

pub(crate) fn read_u32(bytes: &mut &[u8]) -> Option<u32> {
    let (head, tail) = bytes.split_first_chunk::<4>()?;
    *bytes = tail;
    Some(u32::from_le_bytes(*head))
}

/// `None` where it does not fit in a `usize` (a 16-bit target).
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
