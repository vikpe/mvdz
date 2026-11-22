use crate::error::{Error, Result};

/// The largest demo [`crate::decode`] returns, and the most the decoder tables of a file's
/// models may take.
pub(crate) const DEFAULT_MAX_MEMORY: u64 = 1 << 30;

pub(crate) fn try_zeroed(len: usize) -> Result<Vec<u8>> {
    let mut buffer = try_with_capacity(len)?;
    buffer.resize(len, 0);
    Ok(buffer)
}

pub(crate) fn try_with_capacity<T>(capacity: usize) -> Result<Vec<T>> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(capacity)
        .map_err(|_| Error::MemoryLimit("out of memory"))?;
    Ok(buffer)
}
