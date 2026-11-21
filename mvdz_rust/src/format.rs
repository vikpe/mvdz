//! The MVDZ container (FORMAT.md, Part I): a fixed header, the shared
//! models and the segments, and the one set of layout rules every reader
//! applies ([`Layout`]).
//!
//! ```text
//! file    := header | models | segment*
//! header  := "MVDZ" | version u32 | size u64 | duration_ms u64
//!            | crc u64 | sha256[32]                                (64 bytes)
//! models  := len u32 | body[len]
//! segment := len u32 | raw_len u32 | dur_ms u32 | crc u32 | coding u8
//!            | coded[len]                              (17 bytes, then coded)
//! ```
//!
//! Every integer is little-endian and of fixed width.
//! Every header field describes the original demo: its length, its total
//! frame time, its CRC-32 (the low 32 bits of the field; the high 32 are
//! 0) and its SHA-256. `models` holds the shared rANS models (`len` 0:
//! none). A segment's raw offset and start time are the sums of `raw_len`
//! and `dur_ms` of the segments before it. The file is complete when the
//! `raw_len` of its segments sum to `size`; it ends there.

use crate::budget::{Budget, Charge};
use crate::coder::rans::FileTables;
use crate::error::{Error, Result, bad};
use crate::util::Rd;

pub const MAGIC: &[u8; 4] = b"MVDZ";
pub const VERSION: u32 = 2;
pub const HEADER_LEN: usize = 64;
/// Bytes of a length field (of the models block, of a segment).
pub const LEN: usize = 4;

/// Segment codings.
pub const C_TRANSFORM: u8 = 0;
/// The whole segment as one raw DEFLATE stream: the encoder's fallback.
pub const C_DEFLATE: u8 = 1;

/// Largest raw length of one segment. Below 2^32 so that every offset
/// into a segment (plus the walker's small look-ahead) fits in a `u32`.
pub const MAX_SEG_RAW: u64 = 0xFFFF_0000;
/// A segment decodes to at most `SEG_RATIO * len + SEG_SLACK` bytes.
pub const SEG_RATIO: u64 = 1024;
pub const SEG_SLACK: u64 = 4096;
/// Largest models block.
pub const TABLES_MAX: u64 = 1 << 24;
/// Most segments the reference encoder writes into one file (an encoder
/// rule, not a reader rule).
pub const MAX_SEGMENTS: usize = 1 << 16;

/// The ratio limit: `raw` bytes may be coded in `len` bytes.
pub fn ratio_ok(raw: u64, len: u64) -> bool {
    raw <= len.saturating_mul(SEG_RATIO).saturating_add(SEG_SLACK)
}

/// True if a segment of `raw_len` bytes may be stored in a body of `len`
/// bytes.
pub fn seg_ratio_ok(raw_len: u64, len: u64) -> bool {
    raw_len <= MAX_SEG_RAW && ratio_ok(raw_len, len)
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
        b[24..32].copy_from_slice(&(self.crc as u64).to_le_bytes());
        b[32..].copy_from_slice(&self.sha256);
        b
    }

    /// Parses the start of a file. Fewer than [`HEADER_LEN`] bytes: "not
    /// an MVDZ file" if the magic is missing or wrong, "unsupported MVDZ
    /// version" if the version is known to be another, else "truncated
    /// header".
    pub fn parse(b: &[u8]) -> Result<Header> {
        if b.len() < 4 || &b[..4] != MAGIC {
            return Err(Error::Format("not an MVDZ file"));
        }
        if b.len() >= 8 && u32::from_le_bytes(b[4..8].try_into().unwrap()) != VERSION {
            return Err(Error::Format("unsupported MVDZ version"));
        }
        if b.len() < HEADER_LEN {
            return bad("truncated header");
        }
        let u = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let crc = u(24);
        if crc >> 32 != 0 {
            return bad("header checksum field");
        }
        Ok(Header {
            size: u(8),
            duration_ms: u(16),
            crc: crc as u32,
            sha256: b[32..HEADER_LEN].try_into().unwrap(),
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
    Some(u32::from_le_bytes(b.get(..LEN)?.try_into().ok()?) as u64)
}

/// A segment of a file: where its length field starts, and the length of
/// its body (its header fields after the length, and its coded data).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sec {
    pub off: u64,
    pub len: u64,
}

impl Sec {
    pub fn body_off(&self) -> u64 {
        self.off + LEN as u64
    }
    pub fn end(&self) -> u64 {
        self.off + LEN as u64 + self.len
    }
}

// ---------------------------------------------------------------- index

/// The segments of a complete file, as a reader that has scanned their
/// headers knows them. Not stored: it is the running sums of the headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    pub size: u64,
    pub crc: u32,
    pub duration_ms: u64,
    pub segs: Vec<IndexSeg>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexSeg {
    /// file offset of the segment (its length field)
    pub off: u64,
    /// offset of its first byte in the demo
    pub raw_off: u64,
    pub raw_len: u64,
    /// demo time before its first frame (ms)
    pub t_start: u64,
}

impl Index {
    /// The segment a seek to demo time `ms` starts at: the last one whose
    /// start time is before `ms` (so every frame at `ms` or later is
    /// decoded), or the first.
    pub fn seg_at_time(&self, ms: u64) -> usize {
        self.segs
            .partition_point(|s| s.t_start < ms)
            .saturating_sub(1)
    }
}

// ---------------------------------------------------------------- segment header

/// The header of a segment body. `raw_off` and `t_start` are not stored:
/// [`Layout::segment`] fills them in from the segments before.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegHdr {
    pub raw_off: u64,
    pub raw_len: u64,
    pub t_start: u64,
    /// the segment's frame time: the sum of its frame time bytes
    pub dur_ms: u64,
    pub crc: u32,
    pub coding: u8,
    /// offset of the coded data in the segment body
    pub body_off: usize,
}

/// The header fields of a segment body: `raw_len`, `dur_ms` (both must fit
/// in 32 bits), `crc`, `coding`.
pub fn seg_hdr_bytes(raw_len: u64, dur_ms: u64, crc: u32, coding: u8) -> Vec<u8> {
    let mut b = Vec::with_capacity(SEG_FIELDS);
    b.extend_from_slice(&(raw_len as u32).to_le_bytes());
    b.extend_from_slice(&(dur_ms as u32).to_le_bytes());
    b.extend_from_slice(&crc.to_le_bytes());
    b.push(coding);
    b
}

/// Bytes of a segment's header fields after its length field: the coded
/// data starts here in a segment body.
pub const SEG_FIELDS: usize = 4 + 4 + 4 + 1;

/// Parses a segment's header fields from the first [`SEG_FIELDS`] bytes of
/// its body.
pub fn parse_seg_hdr(b: &[u8]) -> Result<SegHdr> {
    let e = Error::Corrupt("segment header");
    let mut r = Rd::new(b);
    let raw_len = r.u32le().ok_or(e)? as u64;
    let dur_ms = r.u32le().ok_or(e)? as u64;
    let crc = r.u32le().ok_or(e)?;
    let coding = r.u8().ok_or(e)?;
    if !matches!(coding, C_TRANSFORM | C_DEFLATE) {
        return Err(Error::Format("unknown segment coding"));
    }
    if raw_len == 0 {
        return bad("empty segment");
    }
    if raw_len > MAX_SEG_RAW {
        return bad("segment too large");
    }
    Ok(SegHdr {
        raw_off: 0,
        raw_len,
        t_start: 0,
        dur_ms,
        crc,
        coding,
        body_off: SEG_FIELDS,
    })
}

// ---------------------------------------------------------------- layout rules

/// What a reader has learned about a file, fed in file order: the header,
/// the models, then every segment. Every reader (whole-file, sequential,
/// seekable) applies these rules through this type, so all of them accept
/// and refuse the same files (FORMAT.md, "Validation").
#[derive(Default)]
pub struct Layout {
    pub header: Header,
    /// the shared models, once the models block has been read
    pub tables: Option<FileTables>,
    models: bool,
    /// the segments: (block, header)
    pub segs: Vec<(Sec, SegHdr)>,
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
    pub fn new(header: &[u8], budget: &Budget) -> Result<Layout> {
        Ok(Layout {
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
    pub fn has_models(&self) -> bool {
        self.models
    }

    /// The models block (`None`: its length is 0).
    pub fn models(&mut self, t: Option<FileTables>) -> Result<()> {
        if let Some(t) = &t {
            self.hold(t.len())?;
        }
        self.tables = t;
        self.models = true;
        Ok(())
    }

    /// The next segment with its parsed header; returns the header with
    /// its raw offset and start time.
    pub fn segment(&mut self, s: &Sec, mut h: SegHdr) -> Result<SegHdr> {
        if !seg_ratio_ok(h.raw_len, s.len) {
            return bad("segment size");
        }
        h.raw_off = self.raw;
        h.t_start = self.t;
        let raw = self.raw.checked_add(h.raw_len);
        let t = self.t.checked_add(h.dur_ms);
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
    pub fn complete(&self) -> bool {
        self.raw == self.header.size
    }

    /// Checks of a complete file, once every segment has been seen.
    pub fn finish(&self) -> Result<()> {
        if !self.models || !self.complete() {
            return bad("incomplete file");
        }
        if self.t != self.header.duration_ms {
            return bad("demo duration");
        }
        Ok(())
    }

    /// The segments with their offsets and start times.
    pub fn index(&self) -> Index {
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
                    raw_len: h.raw_len,
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
    fn header_round_trip() {
        let h = Header {
            size: 0x0102_0304_0506,
            duration_ms: 1_234_567,
            crc: 0xdead_beef,
            sha256: [7; 32],
        };
        let b = h.bytes();
        assert_eq!(b.len(), HEADER_LEN);
        assert_eq!(Header::parse(&b).unwrap(), h);
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
    }
}
