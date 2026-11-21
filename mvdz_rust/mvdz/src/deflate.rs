//! Raw DEFLATE (RFC 1951): the format's general-purpose compressor, through
//! the pure-Rust `miniz_oxide` on every target.
//!
//! Wherever the format stores DEFLATE data it is one raw stream: no zlib or
//! gzip wrapper, no preset dictionary. It ends with its final block, the
//! stored bytes hold nothing after it, and it decodes to exactly the
//! declared length. Empty data is not a stream and is an error.

use crate::budget::zeroed;
use crate::error::{Error, Result};
use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress as inflate, inflate_flags};

/// Compresses `data` at `level` (1-9; 0: stored blocks) into one raw DEFLATE
/// stream. Never empty.
pub(crate) fn compress(data: &[u8], level: u32) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec(data, level as u8)
}

/// Decompresses `stream` to exactly `len` bytes. The caller has charged `len`
/// bytes to its budget.
pub(crate) fn decompress(stream: &[u8], len: usize) -> Result<Vec<u8>> {
    let mut out = zeroed(len)?;
    // one call: with the whole output space at hand the stream either ends
    // or does not fit
    let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    match inflate(&mut DecompressorOxide::new(), stream, &mut out, 0, flags) {
        (TINFLStatus::Done, read, written) if read == stream.len() && written == len => Ok(out),
        _ => Err(Error::Corrupt("deflate data")),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    #[test]
    fn streams() -> anyhow::Result<()> {
        let text = b"hello hello hello hello".repeat(50);
        for level in [0, 1, 6, 9] {
            let packed = super::compress(&text, level);
            assert_eq!(packed.len() < text.len(), level > 0);
            assert_eq!(super::decompress(&packed, text.len())?, text);
            assert!(super::decompress(&packed, text.len() - 1).is_err());
            assert!(super::decompress(&packed, text.len() + 1).is_err());
            // nothing may follow the final block
            let mut more = packed.clone();
            more.push(0);
            assert!(super::decompress(&more, text.len()).is_err());
            // a cut stream
            assert!(super::decompress(&packed[..packed.len() - 1], text.len()).is_err());
        }
        assert!(super::decompress(&[], 0).is_err());
        // incompressible input, larger than one stored block
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let random: Vec<u8> = (0..200_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let packed = super::compress(&random, 9);
        assert!(packed.len() <= random.len() + random.len() / 65535 * 5 + 64);
        assert_eq!(super::decompress(&packed, random.len())?, random);
        // the empty input is a stream of its own
        let empty = super::compress(&[], 9);
        assert!(!empty.is_empty());
        assert_eq!(super::decompress(&empty, 0)?, Vec::<u8>::new());
        Ok(())
    }
}
