#![no_main]
//! `Decoder`, fed in small reads, agrees with whole-file decoding at 1 and 3 threads,
//! also when every other read would block.
use libfuzzer_sys::fuzz_target;
use std::io::{ErrorKind, Read};

struct Dribble<'a>(&'a [u8], usize);
impl Read for Dribble<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let len = buffer.len().min(5).min(self.0.len() - self.1);
        buffer[..len].copy_from_slice(&self.0[self.1..self.1 + len]);
        self.1 += len;
        Ok(len)
    }
}

struct Stalling<'a> {
    input: Dribble<'a>,
    is_stalled: bool,
}
impl Read for Stalling<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        const HEADER_SIZE: usize = 60;
        if self.is_stalled {
            self.is_stalled = false;
            return Err(ErrorKind::WouldBlock.into());
        }
        let len = self.input.read(buffer)?;
        self.is_stalled = self.input.1 >= HEADER_SIZE;
        Ok(len)
    }
}

fn decode_until_error(input: impl Read, max_threads: u8) -> (Vec<u8>, Option<mvdz::Error>) {
    let mut out = Vec::new();
    let mut decoder = match mvdz::Decoder::new(input) {
        Ok(decoder) => decoder.max_threads(max_threads),
        Err(error) => return (out, Some(error)),
    };
    loop {
        match decoder.read_to_end(&mut out) {
            Ok(_) => return (out, None),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => return (out, Some(error.into())),
        }
    }
}

fuzz_target!(|bytes: &[u8]| {
    let whole = mvdz::fuzz::decode(bytes, 1).ok();
    let (out, error) = decode_until_error(Dribble(bytes, 0), 1);
    assert_eq!(whole.is_some(), error.is_none());
    if let Some(demo) = &whole {
        assert!(*demo == out);
    }
    for max_threads in [1, 3] {
        let stalling = Stalling {
            input: Dribble(bytes, 0),
            is_stalled: false,
        };
        assert!(decode_until_error(stalling, max_threads) == (out.clone(), error));
    }
    assert!(decode_until_error(Dribble(bytes, 0), 3) == (out, error));
});
