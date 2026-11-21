//! Raw DEFLATE (RFC 1951): no wrapper, no preset dictionary, nothing after the final block,
//! exactly the declared length. Empty data is an error.

use crate::budget::zeroed;
use crate::error::{Error, Result};
use miniz_oxide::inflate::TINFLStatus;
use miniz_oxide::inflate::core::{DecompressorOxide, decompress as inflate, inflate_flags};

/// Never empty.
pub(crate) fn compress(bytes: &[u8]) -> Vec<u8> {
    const LEVEL: u8 = 9;
    miniz_oxide::deflate::compress_to_vec(bytes, LEVEL)
}

/// The caller has charged `len` bytes to its budget.
pub(crate) fn decompress(bytes: &[u8], len: usize) -> Result<Vec<u8>> {
    let mut out = zeroed(len)?;
    // one call: with the whole output at hand the DEFLATE stream either ends or does not fit
    let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    match inflate(&mut DecompressorOxide::new(), bytes, &mut out, 0, flags) {
        (TINFLStatus::Done, read, written) if read == bytes.len() && written == len => Ok(out),
        _ => Err(Error::Corrupt("deflate data")),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    #[test]
    fn streams() -> anyhow::Result<()> {
        let text = b"hello hello hello hello".repeat(50);
        // other encoders may write any level: 0 is stored blocks
        for level in [0, 1, 6, 9] {
            let packed = miniz_oxide::deflate::compress_to_vec(&text, level);
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
        let packed = super::compress(&random);
        assert!(packed.len() <= random.len() + random.len() / 65535 * 5 + 64);
        assert_eq!(super::decompress(&packed, random.len())?, random);
        // the empty input is a stream of its own
        let empty = super::compress(&[]);
        assert!(!empty.is_empty());
        assert_eq!(super::decompress(&empty, 0)?, Vec::<u8>::new());
        Ok(())
    }
}
