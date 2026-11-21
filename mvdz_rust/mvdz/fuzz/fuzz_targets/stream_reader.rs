#![no_main]
//! `Decoder`, fed in small reads, agrees with whole-file decoding at 1 and 3 threads.
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

fn decode_until_error(bytes: &[u8], max_threads: u8) -> (Vec<u8>, Option<mvdz::Error>) {
    let mut out = Vec::new();
    let error = match mvdz::Decoder::new(Dribble(bytes, 0)) {
        Ok(decoder) => decoder
            .max_threads(max_threads)
            .read_to_end(&mut out)
            .err()
            .map(mvdz::Error::from),
        Err(error) => Some(error),
    };
    (out, error)
}

fuzz_target!(|bytes: &[u8]| {
    let whole = mvdz::decode(bytes, 1).ok();
    let (out, error) = decode_until_error(bytes, 1);
    assert_eq!(whole.is_some(), error.is_none());
    if let Some(demo) = &whole {
        assert!(*demo == out);
    }
    assert!(decode_until_error(bytes, 3) == (out, error));
});
