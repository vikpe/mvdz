#![no_main]
//! The reader (in small reads) agrees with whole-file decoding, and at 3
//! threads it yields the same bytes before the same error as at 1.
use libfuzzer_sys::fuzz_target;
use std::io::Read;

struct Dribble<'a>(&'a [u8], usize);
impl Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = buf.len().min(5).min(self.0.len() - self.1);
        buf[..len].copy_from_slice(&self.0[self.1..self.1 + len]);
        self.1 += len;
        Ok(len)
    }
}

/// The bytes the reader yields before its first error, and that error.
fn read_until_error(
    data: &[u8],
    threads: usize,
    limits: &mvdz::Limits,
) -> (Vec<u8>, Option<mvdz::Error>) {
    let mut out = Vec::new();
    let mut reader = match mvdz::Reader::new(Dribble(data, 0), threads, limits) {
        Ok(reader) => reader,
        Err(error) => return (out, Some(error)),
    };
    let mut buf = [0u8; 4096];
    loop {
        match reader.read_demo(&mut buf) {
            Ok(0) => return (out, None),
            Ok(len) => out.extend_from_slice(&buf[..len]),
            Err(error) => return (out, Some(error)),
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let limits = mvdz::Limits {
        memory: 256 << 20,
        ..Default::default()
    };
    let whole = mvdz::decompress_with(data, 1, &limits).ok();
    let (out, error) = read_until_error(data, 1, &limits);
    assert_eq!(whole.is_some(), error.is_none());
    if let Some(demo) = &whole {
        assert!(*demo == out);
    }
    assert!(read_until_error(data, 3, &limits) == (out, error));
});
