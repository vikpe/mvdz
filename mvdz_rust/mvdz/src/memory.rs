use crate::error::{Error, Result};

/// The largest demo [`crate::decode`] returns, and the most the decoder tables of a file's
/// models may take.
pub(crate) const DEFAULT_MAX_MEMORY: u64 = 1 << 30;

/// As many as `max_memory` holds beside `base`, at least one, at most `threads`.
pub(crate) fn segments_in_flight(
    max_memory: Option<u64>,
    threads: usize,
    base: u64,
    per_segment: u64,
) -> usize {
    let Some(max_memory) = max_memory else {
        return threads;
    };
    let affordable = max_memory.saturating_sub(base) / per_segment;
    usize::try_from(affordable)
        .unwrap_or(usize::MAX)
        .clamp(1, threads)
}

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

#[cfg(test)]
mod tests {
    use super::segments_in_flight;
    use pretty_assertions::assert_eq;

    #[test]
    fn segments_in_flight_follow_the_memory() {
        let (base, per_segment) = (8, 4);
        let in_flight =
            |max_memory: u64| segments_in_flight(Some(max_memory), 4, base, per_segment);
        assert_eq!(segments_in_flight(None, 4, base, per_segment), 4);
        assert_eq!(in_flight(0), 1);
        assert_eq!(in_flight(base + per_segment), 1);
        assert_eq!(in_flight(base + 3 * per_segment), 3);
        assert_eq!(in_flight(u64::MAX), 4);
    }
}
