#![no_main]
//! The reader (in small reads) agrees with whole-file decoding.
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

fuzz_target!(|data: &[u8]| {
    let l = mvdz::Limits {
        memory: 256 << 20,
        ..Default::default()
    };
    let whole = mvdz::decompress_with(data, 1, &l).ok();
    let mut out = Vec::new();
    let seq = mvdz::Reader::new(Dribble(data, 0), 1, &l)
        .ok()
        .and_then(|mut r| r.read_to_end(&mut out).ok().map(|_| out));
    assert_eq!(whole.is_some(), seq.is_some());
    if let (Some(a), Some(b)) = (&whole, &seq) {
        assert!(a == b);
    }
});
