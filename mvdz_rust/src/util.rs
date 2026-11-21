//! Shared helpers: LEB128 varints, checked integer conversions, CRC-32,
//! a scoped parallel map.

use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------- varints

pub fn put_uv(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
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
    /// Unsigned LEB128, at most 10 bytes. A value that does not fit in 64
    /// bits (bits set above bit 63 in the 10th byte) is an error.
    pub(crate) fn uv(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for k in 0..10 {
            let b = *self.b.get(self.i)?;
            self.i += 1;
            if k == 9 && b > 1 {
                return None;
            }
            v |= u64::from(b & 0x7f) << (7 * k);
            if b < 0x80 {
                return Some(v);
            }
        }
        None
    }
    /// A `uv` that must fit in a `usize`.
    pub fn uz(&mut self) -> Option<usize> {
        usize::try_from(self.uv()?).ok()
    }
    /// A `uv` that must fit in a `u32`.
    pub(crate) fn u32v(&mut self) -> Option<u32> {
        u32::try_from(self.uv()?).ok()
    }
    pub(crate) fn u32le(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    /// A length-prefixed byte string (`uv` length, bytes).
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.uz()?;
        self.take(n)
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.b[self.i.min(self.b.len())..]
    }
    pub(crate) fn done(&self) -> bool {
        self.i == self.b.len()
    }
}

/// `u64` to `usize`, or `None` on a 32-bit target when it does not fit.
#[inline]
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
    fn uv_limits() {
        let mut v = Vec::new();
        super::put_uv(&mut v, u64::MAX);
        assert_eq!(v.len(), 10);
        assert_eq!(super::Rd::new(&v).uv(), Some(u64::MAX));
        // a 10th byte with bits above bit 63
        let bad = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02];
        assert_eq!(super::Rd::new(&bad).uv(), None);
        // 11 bytes
        let long = [0x80; 11];
        assert_eq!(super::Rd::new(&long).uv(), None);
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
