//! Shared helpers: little-endian u32 fields, checked integer conversions,
//! CRC-32, a scoped parallel map.

use std::sync::atomic::{AtomicUsize, Ordering};

// ----------------------------------------------------------------- fields

/// Appends `value` as a little-endian u32, the one integer type inside a
/// segment and the models block. Every caller's value is bounded by the
/// 1 MiB segment, the 2 MiB coded segment or the 16 MiB models block.
pub(crate) fn put_u32(out: &mut Vec<u8>, value: usize) {
    debug_assert!(
        u32::try_from(value).is_ok(),
        "{value} does not fit in a u32"
    );
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

/// Byte cursor over a buffer. Every read is bounds-checked.
pub(crate) struct ByteReader<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) offset: usize,
}

impl<'a> ByteReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        ByteReader { bytes, offset: 0 }
    }
    pub(crate) fn u32le(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(*self.take(4)?.first_chunk()?))
    }
    /// A little-endian u32 as a count or length; `None` where it does not
    /// fit in a `usize` (a 16-bit target).
    pub(crate) fn u32_len(&mut self) -> Option<usize> {
        usize::try_from(self.u32le()?).ok()
    }
    pub(crate) fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let taken = self.bytes.get(self.offset..self.offset.checked_add(len)?)?;
        self.offset += len;
        Some(taken)
    }
    /// A length-prefixed byte string (u32 length, bytes).
    pub(crate) fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.u32_len()?;
        self.take(len)
    }
    pub(crate) fn rest(&self) -> &'a [u8] {
        &self.bytes[self.offset.min(self.bytes.len())..]
    }
    pub(crate) fn is_done(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

/// A count or size as a float, for ratios and estimates.
#[expect(clippy::cast_precision_loss, reason = "exact below 2^53")]
pub(crate) fn approx_f64(count: u64) -> f64 {
    count as f64
}

pub(crate) fn to_usize(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

// ---------------------------------------------------------------- crc32

/// CRC-32 as in zlib, gzip and PNG (ISO 3309; polynomial 0xEDB88320
/// reflected): the checksum every platform with DEFLATE already has.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// Running CRC-32 over consecutive chunks.
#[derive(Clone, Default)]
pub(crate) struct Crc(crc32fast::Hasher);

impl Crc {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    pub(crate) fn finish(&self) -> u32 {
        self.0.clone().finalize()
    }
}

// ---------------------------------------------------------------- parallel helper

/// The thread count a caller asked for: 0 means one per logical core (1
/// where that is unknown or threads cannot be spawned).
pub(crate) fn threads(requested: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    if !HAS_THREADS {
        return 1;
    }
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// True where the standard library can spawn threads.
pub(crate) const HAS_THREADS: bool = !cfg!(target_family = "wasm");

/// Runs `job(index)` for index in 0..count on up to `threads` threads; results
/// in order.
pub(crate) fn par_map<T: Send, F: Fn(usize) -> T + Sync>(
    threads: usize,
    count: usize,
    job: F,
) -> Vec<T> {
    if threads <= 1 || count <= 1 || !HAS_THREADS {
        return (0..count).map(job).collect();
    }
    let next = AtomicUsize::new(0);
    // each thread claims the next index and keeps its results; they are put in
    // order once every thread has finished
    let done: Vec<Vec<(usize, T)>> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads.min(count))
            .map(|_| {
                scope.spawn(|| {
                    let mut results = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= count {
                            break results;
                        }
                        results.push((index, job(index)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    let mut out: Vec<Option<T>> = Vec::from_fn(count, |_| None);
    for (index, value) in done.into_iter().flatten() {
        if let Some(slot) = out.get_mut(index) {
            *slot = Some(value);
        }
    }
    out.into_iter()
        .map(|value| value.expect("every index is filled"))
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    #[test]
    fn crc32_check_value() {
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
        let mut crc = super::Crc::default();
        crc.update(b"1234");
        crc.update(b"56789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    #[test]
    fn threads_zero_is_automatic() {
        assert_eq!(super::threads(3), 3);
        assert!(super::threads(0) >= 1);
    }

    #[test]
    fn par_map_order() {
        let doubled = super::par_map(3, 100, |index| index * 2);
        assert_eq!(doubled, (0..100).map(|index| index * 2).collect::<Vec<_>>());
    }
}
