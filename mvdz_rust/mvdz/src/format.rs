//! The MVDZ container (FORMAT.md 2) and the layout rules every reader applies.

use crate::error::{Error, Result, corrupt};
use crate::fields::{read_array, read_u32, read_u64, write_u32, write_usize};
use crate::rans::Models;
use std::time::Duration;

pub(crate) const MAGIC: &[u8; 4] = b"MVDZ";
pub(crate) const FORMAT_VERSION: u32 = 3;
pub(crate) const HEADER_SIZE: usize = 60;
pub(crate) const LEN_FIELD_SIZE: usize = 4;
pub(crate) const SEGMENT_FIELDS_SIZE: usize = 4 + 4 + 4;

pub(crate) const MAX_RAW_LEN: u64 = 1 << 20;
/// Twice [`MAX_RAW_LEN`]: far above any segment the encoder writes.
pub(crate) const MAX_CODED_LEN: u64 = 2 << 20;
pub(crate) const MAX_MODELS_LEN: u64 = 1 << 22;
/// Of the models before they are coded, the meta model included.
pub(crate) const MAX_MODELS_RAW_LEN: u64 = 1 << 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    size: u64,
    duration_ms: u64,
    crc32: u32,
    sha256: [u8; 32],
}

impl Header {
    pub(crate) fn new(size: u64, duration_ms: u64, crc32: u32, sha256: [u8; 32]) -> Self {
        Self {
            size,
            duration_ms,
            crc32,
            sha256,
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// In whole milliseconds.
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms)
    }

    pub fn crc32(&self) -> u32 {
        self.crc32
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

pub(crate) fn header_bytes(header: &Header) -> [u8; HEADER_SIZE] {
    let mut bytes = [0u8; HEADER_SIZE];
    bytes[..4].copy_from_slice(MAGIC);
    bytes[4..8].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    bytes[8..16].copy_from_slice(&header.size.to_le_bytes());
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
    if read_u32(&mut rest).is_some_and(|version| version != FORMAT_VERSION) {
        return Err(Error::UnsupportedVersion);
    }
    let (Some(size), Some(duration_ms), Some(crc32), Some(sha256)) = (
        read_u64(&mut rest),
        read_u64(&mut rest),
        read_u32(&mut rest),
        read_array::<32>(&mut rest),
    ) else {
        return corrupt("truncated header");
    };
    Ok(Header {
        size,
        duration_ms,
        crc32,
        sha256: *sha256,
    })
}

pub(crate) fn write_models(out: &mut Vec<u8>, body: &[u8]) {
    write_usize(out, body.len());
    out.extend_from_slice(body);
}

pub(crate) fn write_segment(out: &mut Vec<u8>, body: &[u8]) {
    write_usize(out, body.len() - SEGMENT_FIELDS_SIZE);
    out.extend_from_slice(body);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SegmentHeader {
    raw_len: u32,
    crc32: u32,
}

impl SegmentHeader {
    pub(crate) fn raw_len(self) -> u32 {
        self.raw_len
    }

    pub(crate) fn crc32(self) -> u32 {
        self.crc32
    }
}

pub(crate) fn segment_header_bytes(raw_len: u32, duration_ms: u32, crc32: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SEGMENT_FIELDS_SIZE);
    write_u32(&mut bytes, raw_len);
    write_u32(&mut bytes, duration_ms);
    write_u32(&mut bytes, crc32);
    bytes
}

pub(crate) fn check_coded_len(coded_len: u64) -> Result<()> {
    if coded_len > MAX_CODED_LEN {
        return corrupt("segment size");
    }
    Ok(())
}

pub(crate) fn parse_segment_header(body: &[u8]) -> Result<SegmentHeader> {
    let error = Error::Corrupt("segment header");
    let mut rest = body;
    let raw_len = read_u32(&mut rest).ok_or(error)?;
    // a claim of the encoder, for readers that seek by time: not checked
    let _duration_ms = read_u32(&mut rest).ok_or(error)?;
    let crc32 = read_u32(&mut rest).ok_or(error)?;
    if raw_len == 0 {
        return corrupt("empty segment");
    }
    if u64::from(raw_len) > MAX_RAW_LEN {
        return corrupt("segment too large");
    }
    Ok(SegmentHeader { raw_len, crc32 })
}

/// The layout rules (FORMAT.md 7) over the blocks read so far.
#[derive(Debug)]
pub(crate) struct Layout {
    header: Header,
    models: Option<Models>,
    has_models: bool,
    raw_total: u64,
}

impl Layout {
    pub(crate) fn new(header: &[u8]) -> Result<Self> {
        Ok(Self {
            header: parse_header(header)?,
            models: None,
            has_models: false,
            raw_total: 0,
        })
    }

    pub(crate) fn header(&self) -> &Header {
        &self.header
    }

    pub(crate) fn models(&self) -> Option<&Models> {
        self.models.as_ref()
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
        let raw_total = self.raw_total + u64::from(header.raw_len);
        if raw_total > self.header.size {
            return corrupt("segment sizes");
        }
        self.raw_total = raw_total;
        Ok(())
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.raw_total == self.header.size
    }

    pub(crate) fn finish(&self) -> Result<()> {
        if !self.has_models || !self.is_complete() {
            return corrupt("incomplete file");
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
            size: 0x0102_0304_0506,
            duration_ms: 1_234_567,
            crc32: 0xdead_beef,
            sha256: [7; 32],
        };
        let bytes = header_bytes(&header);
        assert_eq!(bytes.len(), HEADER_SIZE);
        assert_eq!(parse_header(&bytes)?, header);
        assert_eq!(parse_header(b"MVD"), Err(Error::NotMvdz));
        assert_eq!(parse_header(b"XXXX\x02\0\0\0"), Err(Error::NotMvdz));
        assert_eq!(
            parse_header(b"MVDZ\x01\0\0\0"),
            Err(Error::UnsupportedVersion)
        );
        assert_eq!(
            parse_header(&bytes[..HEADER_SIZE - 1]),
            Err(Error::Corrupt("truncated header"))
        );
        Ok(())
    }
}
