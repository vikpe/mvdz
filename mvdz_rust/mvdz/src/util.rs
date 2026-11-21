use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};

// ----------------------------------------------------------------- fields

/// Every caller's value is bounded by the 1 MiB segment, the 2 MiB coded segment or the
/// 16 MiB models block.
pub(crate) fn put_u32(out: &mut Vec<u8>, value: usize) {
    debug_assert!(
        u32::try_from(value).is_ok(),
        "{value} does not fit in a u32"
    );
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

#[derive(Debug)]
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
    /// `None` where it does not fit in a `usize` (a 16-bit target).
    pub(crate) fn u32_len(&mut self) -> Option<usize> {
        usize::try_from(self.u32le()?).ok()
    }
    pub(crate) fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let taken = self.bytes.get(self.offset..self.offset.checked_add(len)?)?;
        self.offset += len;
        Some(taken)
    }
    /// u32 length, then the bytes.
    pub(crate) fn length_prefixed(&mut self) -> Option<&'a [u8]> {
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

#[expect(clippy::cast_precision_loss, reason = "exact below 2^53")]
pub(crate) fn approx_f64(count: u64) -> f64 {
    count as f64
}

pub(crate) fn to_usize(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

// ---------------------------------------------------------------- crc32

/// As in zlib, gzip and PNG: every platform with DEFLATE has it.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

#[derive(Debug, Clone, Default)]
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

/// At most `max_threads` and the logical cores (1 where unknown or threads cannot be
/// spawned); 0 is one per logical core.
pub(crate) fn thread_count(max_threads: u8) -> usize {
    let available = if HAS_THREADS {
        std::thread::available_parallelism().map_or(1, NonZeroUsize::get)
    } else {
        1
    };
    match max_threads {
        0 => available,
        max_threads => usize::from(max_threads).min(available),
    }
}

pub(crate) const HAS_THREADS: bool = !cfg!(target_family = "wasm");

/// Results in index order.
pub(crate) fn par_map<T: Send, F: Fn(usize) -> T + Sync>(
    threads: usize,
    count: usize,
    job: F,
) -> Vec<T> {
    if threads <= 1 || count <= 1 || !HAS_THREADS {
        return (0..count).map(job).collect();
    }
    let next = AtomicUsize::new(0);
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
    fn thread_count_is_capped_at_the_cores() {
        let cores = super::thread_count(0);
        assert!(cores >= 1);
        assert_eq!(super::thread_count(1), 1);
        assert_eq!(super::thread_count(u8::MAX), cores.min(255));
    }

    #[test]
    fn par_map_order() {
        let doubled = super::par_map(3, 100, |index| index * 2);
        assert_eq!(doubled, (0..100).map(|index| index * 2).collect::<Vec<_>>());
    }
}
