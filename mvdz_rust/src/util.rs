//! Shared helpers: little-endian u32 fields, checked integer conversions,
//! CRC-32, a scoped parallel map.

use std::sync::atomic::{AtomicUsize, Ordering};

// ----------------------------------------------------------------- fields

/// Appends `v` as a little-endian u32, the one integer type inside a
/// segment and the models block. Every caller's value is bounded by the
/// 1 MiB segment, the 2 MiB coded segment or the 16 MiB models block.
pub fn put_u32(out: &mut Vec<u8>, v: usize) {
    debug_assert!(u32::try_from(v).is_ok(), "{v} does not fit in a u32");
    out.extend_from_slice(&(v as u32).to_le_bytes());
}

/// Byte cursor over a buffer. Every read is bounds-checked.
pub struct Rd<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) i: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Rd { b, i: 0 }
    }
    pub(crate) fn u32le(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(*self.take(4)?.first_chunk()?))
    }
    /// A little-endian u32 as a count or length; `None` where it does not
    /// fit in a `usize` (a 16-bit target).
    pub fn u32z(&mut self) -> Option<usize> {
        usize::try_from(self.u32le()?).ok()
    }
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    /// A length-prefixed byte string (u32 length, bytes).
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32z()?;
        self.take(n)
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.b[self.i.min(self.b.len())..]
    }
    pub(crate) fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

/// A count or size as a float, for ratios and estimates.
#[expect(clippy::cast_precision_loss, reason = "exact below 2^53")]
pub fn approx_f64(count: u64) -> f64 {
    count as f64
}

pub(crate) fn usz(v: u64) -> Option<usize> {
    usize::try_from(v).ok()
}

// ---------------------------------------------------------------- crc32

/// CRC-32 as in zlib, gzip and PNG (ISO 3309; polynomial 0xEDB88320
/// reflected): the checksum every platform with DEFLATE already has.
pub fn crc32(b: &[u8]) -> u32 {
    crc32fast::hash(b)
}

/// Running CRC-32 over consecutive chunks.
#[derive(Clone, Default)]
pub(crate) struct Crc(crc32fast::Hasher);

impl Crc {
    pub(crate) fn update(&mut self, b: &[u8]) {
        self.0.update(b);
    }
    pub(crate) fn finish(&self) -> u32 {
        self.0.clone().finalize()
    }
}

// ---------------------------------------------------------------- parallel helper

/// The thread count a caller asked for: 0 means one per logical core (1
/// where that is unknown or threads cannot be spawned).
pub fn threads(n: usize) -> usize {
    if n > 0 {
        return n;
    }
    if !HAS_THREADS {
        return 1;
    }
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// True where the standard library can spawn threads.
pub(crate) const HAS_THREADS: bool = !cfg!(target_family = "wasm");

/// Runs `f(i)` for i in 0..n on up to `threads` threads; results in order.
pub fn par_map<T: Send, F: Fn(usize) -> T + Sync>(threads: usize, n: usize, f: F) -> Vec<T> {
    if threads <= 1 || n <= 1 || !HAS_THREADS {
        return (0..n).map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut out: Vec<Option<T>> = Vec::from_fn(n, |_| None);
    let slots = SyncPtr(out.as_mut_ptr());
    std::thread::scope(|s| {
        for _ in 0..threads.min(n) {
            s.spawn(|| {
                let slots = &slots;
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let v = f(i);
                    // SAFETY: each index is claimed by exactly one thread,
                    // and `out` outlives the scope.
                    unsafe { *slots.0.add(i) = Some(v) };
                }
            });
        }
    });
    out.into_iter()
        .map(|v| v.expect("every index is filled"))
        .collect()
}

struct SyncPtr<T>(*mut T);
unsafe impl<T> Sync for SyncPtr<T> {}
unsafe impl<T> Send for SyncPtr<T> {}

#[cfg(test)]
mod tests {
    #[test]
    fn crc32_check_value() {
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
        let mut c = super::Crc::default();
        c.update(b"1234");
        c.update(b"56789");
        assert_eq!(c.finish(), 0xCBF4_3926);
    }

    #[test]
    fn threads_zero_is_automatic() {
        assert_eq!(super::threads(3), 3);
        assert!(super::threads(0) >= 1);
    }

    #[test]
    fn par_map_order() {
        let v = super::par_map(3, 100, |i| i * 2);
        assert_eq!(v, (0..100).map(|i| i * 2).collect::<Vec<_>>());
    }
}
