//! Raw DEFLATE (RFC 1951): the format's general-purpose compressor, through
//! the pure-Rust `flate2`/`miniz_oxide` on every target.
//!
//! Wherever the format stores DEFLATE data it is one raw stream: no zlib or
//! gzip wrapper, no preset dictionary. It ends with its final block, the
//! stored bytes hold nothing after it, and it decodes to exactly the
//! declared length (or, where the length is only bounded, to at most the
//! bound). Empty data is not a stream and is an error.

use crate::budget::with_capacity;
use crate::error::{Error, Result};
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

/// Compresses `b` at `level` (1-9; 0: stored blocks) into one raw DEFLATE
/// stream. Never empty.
pub fn compress(b: &[u8], level: u32) -> Vec<u8> {
    let mut c = Compress::new(Compression::new(level), false);
    // room for incompressible input as stored blocks (5 bytes per 65535)
    let mut out = Vec::with_capacity(b.len() + b.len() / 65535 * 5 + 64);
    loop {
        let st = c
            .compress_vec(&b[c.total_in() as usize..], &mut out, FlushCompress::Finish)
            .expect("deflate compression");
        if st == Status::StreamEnd {
            return out;
        }
        out.reserve(out.capacity().max(64));
    }
}

/// Decompresses `b` to exactly `n` bytes. The caller has charged `n` bytes
/// to its budget.
pub fn decompress(b: &[u8], n: usize) -> Result<Vec<u8>> {
    let out = decompress_upto(b, n)?;
    if out.len() != n {
        return Err(Error::Corrupt("deflate data"));
    }
    Ok(out)
}

/// Decompresses `b` to at most `max` bytes (more is an error).
pub fn decompress_upto(b: &[u8], max: usize) -> Result<Vec<u8>> {
    let bad = Error::Corrupt("deflate data");
    let mut out = with_capacity(max)?;
    let mut d = Decompress::new(false);
    // one call: with the whole output space at hand the stream either ends
    // or does not fit; it writes only into the reserved capacity
    match d.decompress_vec(b, &mut out, FlushDecompress::Finish) {
        Ok(Status::StreamEnd) if d.total_in() as usize == b.len() => Ok(out),
        _ => Err(bad),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn streams() {
        let a = b"hello hello hello hello".repeat(50);
        for level in [0, 1, 6, 9] {
            let z = super::compress(&a, level);
            assert_eq!(z.len() < a.len(), level > 0);
            assert_eq!(super::decompress(&z, a.len()).unwrap(), a);
            assert_eq!(super::decompress_upto(&z, a.len() + 9).unwrap(), a);
            assert!(super::decompress(&z, a.len() - 1).is_err());
            assert!(super::decompress(&z, a.len() + 1).is_err());
            // nothing may follow the final block
            let mut more = z.clone();
            more.push(0);
            assert!(super::decompress(&more, a.len()).is_err());
            // a cut stream
            assert!(super::decompress(&z[..z.len() - 1], a.len()).is_err());
        }
        assert!(super::decompress(&[], 0).is_err());
        // incompressible input, larger than one stored block
        let mut x = 0x9e3779b97f4a7c15u64;
        let r: Vec<u8> = (0..200_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let z = super::compress(&r, 9);
        assert!(z.len() <= r.len() + r.len() / 65535 * 5 + 64);
        assert_eq!(super::decompress(&z, r.len()).unwrap(), r);
        // the empty input is a stream of its own
        let e = super::compress(&[], 9);
        assert!(!e.is_empty());
        assert_eq!(super::decompress(&e, 0).unwrap(), Vec::<u8>::new());
    }
}
