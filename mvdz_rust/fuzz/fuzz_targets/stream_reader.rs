#![no_main]
//! The reader (in small reads) agrees with whole-file decoding, and at 3
//! threads it yields the same bytes before the same error as at 1.
use libfuzzer_sys::fuzz_target;
use std::io::Read;

struct Dribble<'a>(&'a [u8], usize);
impl Read for Dribble<'_> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        let n = b.len().min(5).min(self.0.len() - self.1);
        b[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
        self.1 += n;
        Ok(n)
    }
}

/// The bytes the reader yields before its first error, and that error.
fn read(data: &[u8], threads: usize, l: &mvdz::Limits) -> (Vec<u8>, Option<mvdz::Error>) {
    let mut out = Vec::new();
    let mut r = match mvdz::Reader::new(Dribble(data, 0), threads, l) {
        Ok(r) => r,
        Err(e) => return (out, Some(e)),
    };
    let mut buf = [0u8; 4096];
    loop {
        match r.read_demo(&mut buf) {
            Ok(0) => return (out, None),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => return (out, Some(e)),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let l = mvdz::Limits {
        memory: 256 << 20,
        ..Default::default()
    };
    let whole = mvdz::decompress_with(data, 1, &l).ok();
    let one = read(data, 1, &l);
    assert_eq!(whole.is_some(), one.1.is_none());
    if let Some(a) = &whole {
        assert!(*a == one.0);
    }
    assert!(read(data, 3, &l) == one);
});
