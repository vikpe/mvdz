//! The MVDZ container (FORMAT.md, Part I): a fixed header, the shared
//! models and the segments, and the one set of layout rules every reader
//! applies ([`Layout`]).
//!
//! ```text
//! file    := header | models | segment*
//! header  := "MVDZ" | version u32 | size u64 | duration_ms u64
//!            | crc u64 | sha256[32]                                (64 bytes)
//! models  := len u32 | body[len]
//! segment := len u32 | raw_len u32 | dur_ms u32 | crc u32
//!            | coded[len]                              (16 bytes, then coded)
//! ```
//!
//! Every integer is little-endian and of fixed width.
//! Every header field describes the original demo: its length, its total
//! frame time, its CRC-32 (the low 32 bits of the field; the high 32 are
//! 0) and its SHA-256. `models` holds the shared rANS models (`len` 0:
//! none). A segment's raw offset and start time are the sums of `raw_len`
//! and `dur_ms` of the segments before it. The file is complete when the
//! `raw_len` of its segments sum to `size`; it ends there. A segment's
//! coded data is the transform ([`crate::segment`]); a segment the
//! transform cannot code is written as all tail.

use crate::budget::{Budget, Charge};
use crate::coder::rans::FileTables;
use crate::error::{Error, Result, bad};
use crate::util::Rd;

pub(crate) const MAGIC: &[u8; 4] = b"MVDZ";
pub(crate) const VERSION: u32 = 2;
pub const HEADER_LEN: usize = 64;
/// Bytes of a length field (of the models block, of a segment).
pub const LEN: usize = 4;

/// Largest raw length of one segment: what a reader holds at most per
/// segment. The encoder cuts at a frame boundary well below it and at
/// this length only where no boundary comes first.
pub const MAX_SEG_RAW: u64 = 1 << 20;
/// Largest coded length of one segment (its `len` field): what a reader
/// holds at most of a segment's coded data. Twice the largest raw length:
/// no segment the encoder writes comes near it.
pub const MAX_SEG_CODED: u64 = 2 << 20;
/// Largest models block.
pub(crate) const TABLES_MAX: u64 = 1 << 24;
/// Most segments the reference encoder writes into one file (an encoder
/// rule, not a reader rule).
pub const MAX_SEGMENTS: usize = 1 << 16;

/// Checks a segment's `len` field, before any of its coded data is read.
pub(crate) fn check_coded_len(coded: u64) -> Result<()> {
    if coded > MAX_SEG_CODED {
        return bad("segment size");
    }
    Ok(())
}

// ---------------------------------------------------------------- header

/// The fixed header: what the file says about the original demo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Header {
    /// length of the demo
    pub size: u64,
    /// total demo time: the sum of all frame time bytes
    pub duration_ms: u64,
    /// CRC-32 of the demo (as in zlib)
    pub crc: u32,
    /// SHA-256 of the demo
    pub sha256: [u8; 32],
}

impl Header {
    pub fn bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..4].copy_from_slice(MAGIC);
        b[4..8].copy_from_slice(&VERSION.to_le_bytes());
        b[8..16].copy_from_slice(&self.size.to_le_bytes());
        b[16..24].copy_from_slice(&self.duration_ms.to_le_bytes());
        b[24..32].copy_from_slice(&u64::from(self.crc).to_le_bytes());
        b[32..].copy_from_slice(&self.sha256);
        b
    }

    /// Parses the start of a file. Fewer than [`HEADER_LEN`] bytes: "not
    /// an MVDZ file" if the magic is missing or wrong, "unsupported MVDZ
    /// version" if the version is known to be another, else "truncated
    /// header".
    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.first_chunk() != Some(MAGIC) {
            return Err(Error::Format("not an MVDZ file"));
        }
        let version = b.get(4..).and_then(<[u8]>::first_chunk);
        if version.is_some_and(|v| u32::from_le_bytes(*v) != VERSION) {
            return Err(Error::Format("unsupported MVDZ version"));
        }
        let field = |at: usize| {
            let bytes = b.get(at..).and_then(<[u8]>::first_chunk)?;
            Some(u64::from_le_bytes(*bytes))
        };
        let sha256 = b.get(32..).and_then(<[u8]>::first_chunk);
        let (Some(size), Some(duration_ms), Some(crc), Some(sha256)) =
            (field(8), field(16), field(24), sha256)
        else {
            return bad("truncated header");
        };
        let Ok(crc) = u32::try_from(crc) else {
            return bad("header checksum field");
        };
        Ok(Self {
            size,
            duration_ms,
            crc,
            sha256: *sha256,
        })
    }
}

/// Appends the models block: its length, then `body` (empty: no models).
pub fn models_block(out: &mut Vec<u8>, body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
}

/// Appends a segment: the length of its coded data, then `body` (the
/// segment's header fields and its coded data, from [`seg_hdr_bytes`] on).
pub fn segment_block(out: &mut Vec<u8>, body: &[u8]) {
    out.extend_from_slice(&((body.len() - SEG_FIELDS) as u32).to_le_bytes());
    out.extend_from_slice(body);
}

/// The length field at the start of `b`.
pub fn len_field(b: &[u8]) -> Option<u64> {
    Some(u64::from(u32::from_le_bytes(
        b.get(..LEN)?.try_into().ok()?,
    )))
}

/// A segment of a file: where its length field starts, and the length of
/// its body (its header fields after the length, and its coded data).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sec {
    pub(crate) off: u64,
    pub(crate) len: u64,
}

impl Sec {
    pub(crate) fn body_off(&self) -> u64 {
        self.off + LEN as u64
    }
    pub(crate) fn end(&self) -> u64 {
        self.off + LEN as u64 + self.len
    }
    /// Where the body is in a file held in memory.
    pub(crate) fn body_range(&self) -> Option<std::ops::Range<usize>> {
        Some(crate::util::usz(self.body_off())?..crate::util::usz(self.end())?)
    }
}

// ---------------------------------------------------------------- index

/// The segments of a complete file, as a reader that has scanned their
/// headers knows them. Not stored: it is the running sums of the headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub(crate) size: u64,
    pub(crate) crc: u32,
    pub(crate) duration_ms: u64,
    pub segs: Vec<IndexSeg>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexSeg {
    /// file offset of the segment (its length field)
    pub(crate) off: u64,
    /// offset of its first byte in the demo
    pub raw_off: u64,
    pub raw_len: u64,
    /// demo time before its first frame (ms)
    pub t_start: u64,
}

// ---------------------------------------------------------------- segment header

/// The header of a segment body. `raw_off` and `t_start` are not stored:
/// [`Layout::segment`] fills them in from the segments before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegHdr {
    pub(crate) raw_off: u64,
    pub raw_len: u32,
    pub(crate) t_start: u64,
    /// the segment's frame time: the sum of its frame time bytes
    pub(crate) dur_ms: u32,
    pub(crate) crc: u32,
    /// offset of the coded data in the segment body
    pub body_off: usize,
}

/// The header fields of a segment body: `raw_len`, `dur_ms`, `crc`.
pub fn seg_hdr_bytes(raw_len: u32, dur_ms: u32, crc: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(SEG_FIELDS);
    b.extend_from_slice(&raw_len.to_le_bytes());
    b.extend_from_slice(&dur_ms.to_le_bytes());
    b.extend_from_slice(&crc.to_le_bytes());
    b
}

/// Bytes of a segment's header fields after its length field: the coded
/// data starts here in a segment body.
pub const SEG_FIELDS: usize = 4 + 4 + 4;

/// Parses a segment's header fields from the first [`SEG_FIELDS`] bytes of
/// its body.
pub fn parse_seg_hdr(b: &[u8]) -> Result<SegHdr> {
    let e = Error::Corrupt("segment header");
    let mut r = Rd::new(b);
    let raw_len = r.u32le().ok_or(e)?;
    let dur_ms = r.u32le().ok_or(e)?;
    let crc = r.u32le().ok_or(e)?;
    if raw_len == 0 {
        return bad("empty segment");
    }
    if u64::from(raw_len) > MAX_SEG_RAW {
        return bad("segment too large");
    }
    Ok(SegHdr {
        raw_off: 0,
        raw_len,
        t_start: 0,
        dur_ms,
        crc,
        body_off: SEG_FIELDS,
    })
}

// ---------------------------------------------------------------- layout rules

/// What a reader has learned about a file.
///
/// It is fed in file order: the header, the models, then every segment.
/// Every reader (whole-file or in order) applies these rules through this
/// type, so all of them accept and refuse the same files (FORMAT.md,
/// "Validation").
#[derive(Default)]
pub(crate) struct Layout {
    pub(crate) header: Header,
    /// the shared models, once the models block has been read
    pub(crate) tables: Option<FileTables>,
    models: bool,
    /// the segments: (block, header)
    pub(crate) segs: Vec<(Sec, SegHdr)>,
    raw: u64,
    t: u64,
    /// the budget charge for everything above (the models, one record per
    /// segment)
    held: Option<Charge>,
}

/// Bytes charged per recorded segment.
const SEG_BOOK: u64 = std::mem::size_of::<(Sec, SegHdr)>() as u64;

impl Layout {
    /// `header`: the first [`HEADER_LEN`] bytes of the file (or all of a
    /// shorter one).
    pub(crate) fn new(header: &[u8], budget: &Budget) -> Result<Self> {
        Ok(Self {
            header: Header::parse(header)?,
            held: Some(Charge::none(budget)),
            ..Default::default()
        })
    }

    /// Charges `n` more bytes for what the layout keeps.
    fn hold(&mut self, n: u64) -> Result<()> {
        match &mut self.held {
            Some(c) => c.grow(n),
            None => Ok(()),
        }
    }

    /// True once the models block has been read.
    pub(crate) fn has_models(&self) -> bool {
        self.models
    }

    /// The models block (`None`: its length is 0).
    pub(crate) fn models(&mut self, t: Option<FileTables>) -> Result<()> {
        if let Some(t) = &t {
            self.hold(t.len())?;
        }
        self.tables = t;
        self.models = true;
        Ok(())
    }

    /// The next segment with its parsed header; returns the header with
    /// its raw offset and start time.
    pub(crate) fn segment(&mut self, s: &Sec, mut h: SegHdr) -> Result<SegHdr> {
        h.raw_off = self.raw;
        h.t_start = self.t;
        let raw = self.raw.checked_add(u64::from(h.raw_len));
        let t = self.t.checked_add(u64::from(h.dur_ms));
        match (raw, t) {
            (Some(raw), Some(t)) if raw <= self.header.size && t <= self.header.duration_ms => {
                self.raw = raw;
                self.t = t;
            }
            _ => return bad("segment sizes"),
        }
        self.hold(SEG_BOOK)?;
        self.segs.push((*s, h));
        Ok(h)
    }

    /// True once the segments hold the whole demo: the file ends here.
    pub(crate) fn complete(&self) -> bool {
        self.raw == self.header.size
    }

    /// Checks of a complete file, once every segment has been seen.
    pub(crate) fn finish(&self) -> Result<()> {
        if !self.models || !self.complete() {
            return bad("incomplete file");
        }
        if self.t != self.header.duration_ms {
            return bad("demo duration");
        }
        Ok(())
    }

    /// The segments with their offsets and start times.
    pub(crate) fn index(&self) -> Index {
        Index {
            size: self.header.size,
            crc: self.header.crc,
            duration_ms: self.header.duration_ms,
            segs: self
                .segs
                .iter()
                .map(|(s, h)| IndexSeg {
                    off: s.off,
                    raw_off: h.raw_off,
                    raw_len: u64::from(h.raw_len),
                    t_start: h.t_start,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() -> anyhow::Result<()> {
        let h = Header {
            size: 0x0102_0304_0506,
            duration_ms: 1_234_567,
            crc: 0xdead_beef,
            sha256: [7; 32],
        };
        let b = h.bytes();
        assert_eq!(b.len(), HEADER_LEN);
        assert_eq!(Header::parse(&b)?, h);
        assert_eq!(
            Header::parse(b"MVD"),
            Err(Error::Format("not an MVDZ file"))
        );
        assert_eq!(
            Header::parse(b"XXXX\x02\0\0\0"),
            Err(Error::Format("not an MVDZ file"))
        );
        assert_eq!(
            Header::parse(b"MVDZ\x01\0\0\0"),
            Err(Error::Format("unsupported MVDZ version"))
        );
        assert_eq!(
            Header::parse(&b[..HEADER_LEN - 1]),
            Err(Error::Corrupt("truncated header"))
        );
        // the high half of the checksum field is 0
        let mut x = b;
        x[28] = 1;
        assert_eq!(
            Header::parse(&x),
            Err(Error::Corrupt("header checksum field"))
        );
        Ok(())
    }
}
