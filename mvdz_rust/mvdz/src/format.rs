//! The MVDZ container (FORMAT.md section 2) and the layout rules every reader applies.

use crate::budget::{Budget, Charge};
use crate::coder::rans::ModelsBlock;
use crate::error::{Error, Result, bad};
use crate::fields::{put_u32, put_usize, read_array, read_u32, read_u64};

pub(crate) const MAGIC: &[u8; 4] = b"MVDZ";
pub(crate) const VERSION: u32 = 2;
pub(crate) const HEADER_LEN: usize = 64;
pub(crate) const LEN_FIELD_LEN: usize = 4;
pub(crate) const SEGMENT_FIELDS_LEN: usize = 4 + 4 + 4;

pub(crate) const MAX_SEGMENT_RAW: u64 = 1 << 20;
/// Twice [`MAX_SEGMENT_RAW`]: far above any segment the encoder writes.
pub(crate) const MAX_SEGMENT_CODED: u64 = 2 << 20;
pub(crate) const MAX_MODELS_LEN: u64 = 1 << 24;
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
    bytes[24..32].copy_from_slice(&u64::from(header.crc32).to_le_bytes());
    bytes[32..].copy_from_slice(&header.sha256);
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
        read_u64(&mut rest),
        read_array::<32>(&mut rest),
    ) else {
        return bad("truncated header");
    };
    let Ok(crc32) = u32::try_from(crc32) else {
        return bad("header checksum field");
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

pub(crate) fn len_field(mut bytes: &[u8]) -> Option<u64> {
    read_u32(&mut bytes).map(u64::from)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SegmentSpan {
    pub(crate) len_field_offset: u64,
    pub(crate) body_len: u64,
}

impl SegmentSpan {
    pub(crate) fn body_offset(&self) -> u64 {
        self.len_field_offset + LEN_FIELD_LEN as u64
    }
    pub(crate) fn end(&self) -> u64 {
        self.body_offset() + self.body_len
    }
    pub(crate) fn body<'a>(&self, file: &'a [u8]) -> Option<&'a [u8]> {
        file.get(usize::try_from(self.body_offset()).ok()?..usize::try_from(self.end()).ok()?)
    }
}

/// `raw_offset` and `offset_ms` are not stored: [`Layout::add_segment`] fills them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SegmentHeader {
    pub(crate) raw_offset: u64,
    pub(crate) raw_len: u32,
    pub(crate) offset_ms: u64,
    pub(crate) duration_ms: u32,
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
    let duration_ms = read_u32(&mut rest).ok_or(error)?;
    let crc32 = read_u32(&mut rest).ok_or(error)?;
    if raw_len == 0 {
        return bad("empty segment");
    }
    if u64::from(raw_len) > MAX_SEGMENT_RAW {
        return bad("segment too large");
    }
    Ok(SegmentHeader {
        raw_offset: 0,
        raw_len,
        offset_ms: 0,
        duration_ms,
        crc32,
    })
}

/// Every reader applies the layout rules through it, so all accept and refuse the same files.
#[derive(Debug)]
pub(crate) struct Layout {
    pub(crate) header: Header,
    pub(crate) models_block: Option<ModelsBlock>,
    has_models: bool,
    pub(crate) segments: Vec<(SegmentSpan, SegmentHeader)>,
    raw_len: u64,
    duration_ms: u64,
    charge: Charge,
}

impl Layout {
    pub(crate) fn new(header: &[u8], budget: &Budget) -> Result<Self> {
        Ok(Self {
            header: parse_header(header)?,
            models_block: None,
            has_models: false,
            segments: Vec::new(),
            raw_len: 0,
            duration_ms: 0,
            charge: Charge::none(budget),
        })
    }

    pub(crate) fn has_models(&self) -> bool {
        self.has_models
    }

    pub(crate) fn models(&mut self, models_block: Option<ModelsBlock>) -> Result<()> {
        if let Some(models_block) = &models_block {
            self.charge.grow(models_block.len())?;
        }
        self.models_block = models_block;
        self.has_models = true;
        Ok(())
    }

    /// Returns `header` with its raw offset and start time filled in.
    pub(crate) fn add_segment(
        &mut self,
        span: &SegmentSpan,
        mut header: SegmentHeader,
    ) -> Result<SegmentHeader> {
        const SEGMENT_RECORD_LEN: u64 = std::mem::size_of::<(SegmentSpan, SegmentHeader)>() as u64;
        header.raw_offset = self.raw_len;
        header.offset_ms = self.duration_ms;
        let raw_len = self.raw_len.checked_add(u64::from(header.raw_len));
        let duration_ms = self.duration_ms.checked_add(u64::from(header.duration_ms));
        match (raw_len, duration_ms) {
            (Some(raw_len), Some(duration_ms))
                if raw_len <= self.header.size_bytes && duration_ms <= self.header.duration_ms =>
            {
                self.raw_len = raw_len;
                self.duration_ms = duration_ms;
            }
            _ => return bad("segment sizes"),
        }
        self.charge.grow(SEGMENT_RECORD_LEN)?;
        self.segments.push((*span, header));
        Ok(header)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.raw_len == self.header.size_bytes
    }

    pub(crate) fn finish(&self) -> Result<()> {
        if !self.has_models || !self.is_complete() {
            return bad("incomplete file");
        }
        if self.duration_ms != self.header.duration_ms {
            return bad("demo duration");
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
        // the high half of the checksum field is 0
        let mut damaged = bytes;
        damaged[28] = 1;
        assert_eq!(
            parse_header(&damaged),
            Err(Error::Corrupt("header checksum field"))
        );
        Ok(())
    }
}
