//! The MVDZ container (FORMAT.md section 2) and the layout rules every reader applies.

use crate::error::{Error, Result, bad};
use crate::fields::{put_u32, put_usize, read_array, read_u32, read_u64};
use crate::rans::Models;

pub(crate) const MAGIC: &[u8; 4] = b"MVDZ";
pub(crate) const VERSION: u32 = 3;
pub(crate) const HEADER_LEN: usize = 60;
pub(crate) const LEN_FIELD_LEN: usize = 4;
pub(crate) const SEGMENT_FIELDS_LEN: usize = 4 + 4 + 4;

pub(crate) const MAX_SEGMENT_RAW: u64 = 1 << 20;
/// Twice [`MAX_SEGMENT_RAW`]: far above any segment the encoder writes.
pub(crate) const MAX_SEGMENT_CODED: u64 = 2 << 20;
pub(crate) const MAX_MODELS_LEN: u64 = 1 << 24;
/// Of the models before they are coded, the meta model included.
pub(crate) const MAX_MODELS_RAW_LEN: u64 = 1 << 24;
/// An encoder rule, not a reader rule.
pub(crate) const MAX_SEGMENTS: usize = 1 << 16;

pub(crate) fn check_coded_len(coded: u64) -> Result<()> {
    if coded > MAX_SEGMENT_CODED {
        return bad("segment size");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub(crate) size_bytes: u64,
    pub(crate) duration_ms: u64,
    pub(crate) crc32: u32,
    pub(crate) sha256: [u8; 32],
}

impl Header {
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn crc32(&self) -> u32 {
        self.crc32
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

pub(crate) fn header_bytes(header: &Header) -> [u8; HEADER_LEN] {
    let mut bytes = [0u8; HEADER_LEN];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4..8].copy_from_slice(&VERSION.to_le_bytes());
    bytes[8..16].copy_from_slice(&header.size_bytes.to_le_bytes());
    bytes[16..24].copy_from_slice(&header.duration_ms.to_le_bytes());
    bytes[24..28].copy_from_slice(&header.crc32.to_le_bytes());
    bytes[28..].copy_from_slice(&header.sha256);
    bytes
}

pub(crate) fn parse_header(bytes: &[u8]) -> Result<Header> {
    let mut rest = bytes;
    if read_array(&mut rest) != Some(MAGIC) {
        return Err(Error::NotMvdz);
    }
    if read_u32(&mut rest).is_some_and(|version| version != VERSION) {
        return Err(Error::UnsupportedVersion);
    }
    let (Some(size_bytes), Some(duration_ms), Some(crc32), Some(sha256)) = (
        read_u64(&mut rest),
        read_u64(&mut rest),
        read_u32(&mut rest),
        read_array::<32>(&mut rest),
    ) else {
        return bad("truncated header");
    };
    Ok(Header {
        size_bytes,
        duration_ms,
        crc32,
        sha256: *sha256,
    })
}

pub(crate) fn models_block(out: &mut Vec<u8>, body: &[u8]) {
    put_usize(out, body.len());
    out.extend_from_slice(body);
}

pub(crate) fn segment_block(out: &mut Vec<u8>, body: &[u8]) {
    put_usize(out, body.len() - SEGMENT_FIELDS_LEN);
    out.extend_from_slice(body);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SegmentHeader {
    pub(crate) raw_len: u32,
    pub(crate) crc32: u32,
}

pub(crate) fn segment_header_bytes(raw_len: u32, duration_ms: u32, crc: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SEGMENT_FIELDS_LEN);
    put_u32(&mut bytes, raw_len);
    put_u32(&mut bytes, duration_ms);
    put_u32(&mut bytes, crc);
    bytes
}

pub(crate) fn parse_segment_header(body: &[u8]) -> Result<SegmentHeader> {
    let error = Error::Corrupt("segment header");
    let mut rest = body;
    let raw_len = read_u32(&mut rest).ok_or(error)?;
    // a claim of the encoder, for readers that seek by time: not checked
    let _duration_ms = read_u32(&mut rest).ok_or(error)?;
    let crc32 = read_u32(&mut rest).ok_or(error)?;
    if raw_len == 0 {
        return bad("empty segment");
    }
    if u64::from(raw_len) > MAX_SEGMENT_RAW {
        return bad("segment too large");
    }
    Ok(SegmentHeader { raw_len, crc32 })
}

/// The layout rules (FORMAT.md section 7) over the blocks read so far.
#[derive(Debug)]
pub(crate) struct Layout {
    pub(crate) header: Header,
    pub(crate) models: Option<Models>,
    has_models: bool,
    raw_len: u64,
}

impl Layout {
    pub(crate) fn new(header: &[u8]) -> Result<Self> {
        Ok(Self {
            header: parse_header(header)?,
            models: None,
            has_models: false,
            raw_len: 0,
        })
    }

    pub(crate) fn has_models(&self) -> bool {
        self.has_models
    }

    /// `None` for a file without models.
    pub(crate) fn set_models(&mut self, models: Option<Models>) {
        self.models = models;
        self.has_models = true;
    }

    pub(crate) fn add_segment(&mut self, header: SegmentHeader) -> Result<()> {
        let raw_len = self.raw_len + u64::from(header.raw_len);
        if raw_len > self.header.size_bytes {
            return bad("segment sizes");
        }
        self.raw_len = raw_len;
        Ok(())
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.raw_len == self.header.size_bytes
    }

    pub(crate) fn finish(&self) -> Result<()> {
        if !self.has_models || !self.is_complete() {
            return bad("incomplete file");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn header_round_trip() -> anyhow::Result<()> {
        let header = Header {
            size_bytes: 0x0102_0304_0506,
            duration_ms: 1_234_567,
            crc32: 0xdead_beef,
            sha256: [7; 32],
        };
        let bytes = header_bytes(&header);
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(parse_header(&bytes)?, header);
        assert_eq!(parse_header(b"MVD"), Err(Error::NotMvdz));
        assert_eq!(parse_header(b"XXXX\x02\0\0\0"), Err(Error::NotMvdz));
        assert_eq!(
            parse_header(b"MVDZ\x01\0\0\0"),
            Err(Error::UnsupportedVersion)
        );
        assert_eq!(
            parse_header(&bytes[..HEADER_LEN - 1]),
            Err(Error::Corrupt("truncated header"))
        );
        Ok(())
    }
}
