//! The MVDZ container (FORMAT.md, Part I) and the layout rules every reader applies
//! ([`Layout`]).
//!
//! ```text
//! file    := header | models | segment*
//! header  := "MVDZ" | version u32 | size u64 | duration_ms u64
//!            | crc u64 | sha256[32]                                (64 bytes)
//! models  := len u32 | body[len]
//! segment := len u32 | raw_len u32 | duration_ms u32 | crc u32
//!            | coded[len]                              (16 bytes, then coded)
//! ```
//!
//! Integers are little-endian. Header fields describe the original demo; `crc` is
//! CRC-32 in the low 32 bits, the high 32 are 0. The file ends when `raw_len` sums
//! to `size`.

use crate::budget::{Budget, Charge};
use crate::coder::rans::ModelsBlock;
use crate::error::{Error, Result, bad};
use crate::util::ByteReader;

pub(crate) const MAGIC: &[u8; 4] = b"MVDZ";
pub(crate) const VERSION: u32 = 2;
pub(crate) const HEADER_LEN: usize = 64;
/// Bytes of a length field.
pub(crate) const LEN_FIELD_LEN: usize = 4;

/// What a reader holds at most per segment. The encoder cuts here only where no frame
/// boundary comes first.
pub(crate) const MAX_SEGMENT_RAW: u64 = 1 << 20;
/// Twice [`MAX_SEGMENT_RAW`]: no segment the encoder writes comes near it.
pub(crate) const MAX_SEGMENT_CODED: u64 = 2 << 20;
pub(crate) const MAX_MODELS_LEN: u64 = 1 << 24;
/// An encoder rule, not a reader rule.
pub(crate) const MAX_SEGMENTS: usize = 1 << 16;

/// Before any coded data is read.
pub(crate) fn check_coded_len(coded: u64) -> Result<()> {
    if coded > MAX_SEGMENT_CODED {
        return bad("segment size");
    }
    Ok(())
}

// ---------------------------------------------------------------- header

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

/// Magic and version, then the fields of `header`.
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

/// Short input still gets "not an MVDZ file" or "unsupported MVDZ version" when the
/// bytes present show it; otherwise "truncated header".
pub(crate) fn parse_header(bytes: &[u8]) -> Result<Header> {
    if bytes.first_chunk() != Some(MAGIC) {
        return Err(Error::Format("not an MVDZ file"));
    }
    let version = bytes.get(4..).and_then(<[u8]>::first_chunk);
    if version.is_some_and(|version| u32::from_le_bytes(*version) != VERSION) {
        return Err(Error::Format("unsupported MVDZ version"));
    }
    let field = |at: usize| {
        let value = bytes.get(at..).and_then(<[u8]>::first_chunk)?;
        Some(u64::from_le_bytes(*value))
    };
    let sha256 = bytes.get(32..).and_then(<[u8]>::first_chunk);
    let (Some(size_bytes), Some(duration_ms), Some(crc32), Some(sha256)) =
        (field(8), field(16), field(24), sha256)
    else {
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

/// `body` empty: no models.
pub(crate) fn models_block(out: &mut Vec<u8>, body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
}

/// `body`: the header fields from [`segment_header_bytes`], then the coded data.
pub(crate) fn segment_block(out: &mut Vec<u8>, body: &[u8]) {
    out.extend_from_slice(&((body.len() - SEGMENT_FIELDS_LEN) as u32).to_le_bytes());
    out.extend_from_slice(body);
}

pub(crate) fn len_field(bytes: &[u8]) -> Option<u64> {
    Some(u64::from(u32::from_le_bytes(
        bytes.get(..LEN_FIELD_LEN)?.try_into().ok()?,
    )))
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
    pub(crate) fn body_range(&self) -> Option<std::ops::Range<usize>> {
        Some(crate::util::to_usize(self.body_offset())?..crate::util::to_usize(self.end())?)
    }
}

// ---------------------------------------------------------------- segment header

/// `raw_offset` and `offset_ms` are not stored: [`Layout::segment`] fills them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SegmentHeader {
    pub(crate) raw_offset: u64,
    pub(crate) raw_len: u32,
    pub(crate) offset_ms: u64,
    /// The sum of its frame time bytes.
    pub(crate) duration_ms: u32,
    pub(crate) crc32: u32,
    /// Of the coded data in the segment body.
    pub(crate) body_offset: usize,
}

pub(crate) fn segment_header_bytes(raw_len: u32, duration_ms: u32, crc: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(SEGMENT_FIELDS_LEN);
    bytes.extend_from_slice(&raw_len.to_le_bytes());
    bytes.extend_from_slice(&duration_ms.to_le_bytes());
    bytes.extend_from_slice(&crc.to_le_bytes());
    bytes
}

/// Header fields after the length field; the coded data starts here.
pub(crate) const SEGMENT_FIELDS_LEN: usize = 4 + 4 + 4;

pub(crate) fn parse_segment_header(body: &[u8]) -> Result<SegmentHeader> {
    let error = Error::Corrupt("segment header");
    let mut reader = ByteReader::new(body);
    let raw_len = reader.u32le().ok_or(error)?;
    let duration_ms = reader.u32le().ok_or(error)?;
    let crc32 = reader.u32le().ok_or(error)?;
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
        body_offset: SEGMENT_FIELDS_LEN,
    })
}

// ---------------------------------------------------------------- layout rules

/// Fed in file order: header, models, segments. Every reader applies the layout rules
/// through it, so all accept and refuse the same files (FORMAT.md, "Validation").
#[derive(Debug, Default)]
pub(crate) struct Layout {
    pub(crate) header: Header,
    pub(crate) models_block: Option<ModelsBlock>,
    has_models: bool,
    pub(crate) segments: Vec<(SegmentSpan, SegmentHeader)>,
    raw_len: u64,
    duration_ms: u64,
    /// For the models and one record per segment.
    charge: Option<Charge>,
}

impl Layout {
    /// `header`: the first [`HEADER_LEN`] bytes, or all of a shorter file.
    pub(crate) fn new(header: &[u8], budget: &Budget) -> Result<Self> {
        Ok(Self {
            header: parse_header(header)?,
            charge: Some(Charge::none(budget)),
            ..Default::default()
        })
    }

    fn charge(&mut self, len: u64) -> Result<()> {
        match &mut self.charge {
            Some(charge) => charge.grow(len),
            None => Ok(()),
        }
    }

    pub(crate) fn has_models(&self) -> bool {
        self.has_models
    }

    /// `None`: the models block is empty.
    pub(crate) fn models(&mut self, models_block: Option<ModelsBlock>) -> Result<()> {
        if let Some(models_block) = &models_block {
            self.charge(models_block.len())?;
        }
        self.models_block = models_block;
        self.has_models = true;
        Ok(())
    }

    /// Returns `header` with its raw offset and start time filled in.
    pub(crate) fn segment(
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
        self.charge(SEGMENT_RECORD_LEN)?;
        self.segments.push((*span, header));
        Ok(header)
    }

    /// The file ends here.
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
        assert_eq!(parse_header(b"MVD"), Err(Error::Format("not an MVDZ file")));
        assert_eq!(
            parse_header(b"XXXX\x02\0\0\0"),
            Err(Error::Format("not an MVDZ file"))
        );
        assert_eq!(
            parse_header(b"MVDZ\x01\0\0\0"),
            Err(Error::Format("unsupported MVDZ version"))
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
