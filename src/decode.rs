//! Decoding: the layout scan every random-access reader runs, the decode
//! of one segment, and whole-file decompression.

use crate::budget::{Budget, Charge, Limits, zeroed};
use crate::coder::rans::FileTables;
use crate::error::{Error, Result, bad};
use crate::format::*;
use crate::segment::{self, frame_scan};
use crate::util::{Crc, crc32, par_map, usz};
use std::io::{Read, Seek, SeekFrom};

/// Random access to a file's bytes.
pub trait Src {
    fn len(&mut self) -> Result<u64>;
    /// Fills `buf` from file offset `off` (exactly).
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()>;
}

/// A file in memory.
pub struct SliceSrc<'a>(pub &'a [u8]);

impl Src for SliceSrc<'_> {
    fn len(&mut self) -> Result<u64> {
        Ok(self.0.len() as u64)
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        let s = usz(off)
            .and_then(|o| self.0.get(o..o.checked_add(buf.len())?))
            .ok_or(Error::Corrupt("truncated"))?;
        buf.copy_from_slice(s);
        Ok(())
    }
}

/// A file behind `Read + Seek`.
pub struct SeekSrc<R>(pub R);

impl<R: Read + Seek> Src for SeekSrc<R> {
    fn len(&mut self) -> Result<u64> {
        Ok(self.0.seek(SeekFrom::End(0))?)
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        if self.0.seek(SeekFrom::Start(off))? != off {
            return Err(Error::Io(std::io::ErrorKind::InvalidData));
        }
        read_full(&mut self.0, buf)
    }
}

/// `read_exact` that reports a short read as a truncated file.
pub fn read_full<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> Result<()> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => return bad("truncated"),
            Ok(k) if k <= buf.len() - n => n += k,
            Ok(_) => return Err(Error::Io(std::io::ErrorKind::InvalidData)),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Reads `n` bytes at `off` into a buffer charged to `budget` (for as long
/// as the charge lives).
fn read_vec(src: &mut impl Src, off: u64, n: u64, budget: &Budget) -> Result<(Vec<u8>, Charge)> {
    let c = budget.alloc(n)?;
    let mut v = zeroed(usz(n).ok_or(Error::Limit("section size"))?)?;
    src.read_at(off, &mut v)?;
    Ok((v, c))
}

/// The length field at file offset `off` of a file of `len` bytes (None:
/// the file ends first).
fn len_at(src: &mut impl Src, off: u64, len: u64) -> Result<Option<u64>> {
    if len - off < LEN as u64 {
        return Ok(None);
    }
    let mut b = [0u8; LEN];
    src.read_at(off, &mut b)?;
    Ok(len_field(&b))
}

/// Reads and checks the whole structure of a complete file: the header,
/// the models and the header of every segment (FORMAT.md, "Validation").
/// Segment payloads are not decoded.
pub fn scan_layout(src: &mut impl Src, budget: &Budget) -> Result<Layout> {
    let len = src.len()?;
    let mut h = [0u8; HEADER_LEN];
    let n = len.min(HEADER_LEN as u64) as usize;
    src.read_at(0, &mut h[..n])?;
    let mut lay = Layout::new(&h[..n], budget)?;
    let mut off = HEADER_LEN as u64;
    // the models
    let Some(mlen) = len_at(src, off, len)? else {
        return bad("incomplete file");
    };
    off += LEN as u64;
    if mlen > TABLES_MAX {
        return bad("models too large");
    }
    if mlen > len - off {
        return bad("incomplete file");
    }
    if mlen > 0 {
        let (b, _c) = read_vec(src, off, mlen, budget)?;
        lay.models(Some(FileTables::new(&b, budget)?))?;
    } else {
        lay.models(None)?;
    }
    off += mlen;
    // the segments, until they hold the whole demo
    while !lay.complete() {
        let Some(coded) = len_at(src, off, len)? else {
            return bad("incomplete file");
        };
        let s = Sec {
            off,
            len: coded + SEG_FIELDS as u64,
        };
        if s.len > len - s.body_off() {
            return bad("incomplete file");
        }
        let mut b = [0u8; SEG_FIELDS];
        src.read_at(s.body_off(), &mut b)?;
        lay.segment(&s, parse_seg_hdr(&b)?)?;
        off = s.end();
    }
    if off != len {
        return bad("trailing bytes");
    }
    lay.finish()?;
    Ok(lay)
}

/// Frames, frame time and whole-frame length of one decoded segment.
pub type SegTiming = (u64, u64, usize);

/// Decodes one segment body into `dst` (its exact raw length) and checks
/// its CRC and its frame time.
pub fn decode_segment(
    lay: &Layout,
    body: &[u8],
    h: &SegHdr,
    dst: &mut [u8],
    threads: usize,
    budget: &Budget,
) -> Result<SegTiming> {
    let tables = lay.tables.as_ref();
    if dst.len() as u64 != h.raw_len {
        return bad("segment length");
    }
    let coded = body
        .get(h.body_off..)
        .ok_or(Error::Corrupt("segment header"))?;
    match h.coding {
        C_TRANSFORM => {
            let th = segment::parse_header(coded, dst.len())?;
            segment::decode(coded, &th, dst, tables, threads, budget)?;
        }
        C_DEFLATE => {
            let _c = budget.alloc(h.raw_len)?;
            dst.copy_from_slice(&crate::deflate::decompress(coded, dst.len())?);
        }
        C_STORE if coded.len() == dst.len() => dst.copy_from_slice(coded),
        C_STORE => return bad("stored segment length"),
        _ => return Err(Error::Format("unknown segment coding")),
    }
    if crc32(dst) != h.crc {
        return bad("segment checksum");
    }
    let tm = frame_scan(dst);
    if tm.1 != h.dur_ms {
        return bad("segment duration");
    }
    Ok(tm)
}

/// Whole-demo checks over the segments' timings (all of them, in order).
/// The header's SHA-256 names the demo; decoding does not recompute it.
#[derive(Default)]
pub struct DemoCheck {
    crc: Crc,
    size: u64,
    t: u64,
    /// the previous segment ended inside a frame
    open: bool,
}

impl DemoCheck {
    /// The next segment, decoded.
    pub fn segment(&mut self, out: &[u8], tm: SegTiming) -> Result<()> {
        if self.open {
            return bad("segment ends inside a frame");
        }
        self.crc.update(out);
        self.size += out.len() as u64;
        self.t += tm.1;
        self.open = tm.2 != out.len();
        Ok(())
    }
    /// After the last segment: the header's size, duration and CRC-32C.
    pub fn finish(self, lay: &Layout) -> Result<()> {
        let h = &lay.header;
        if self.crc.finish() != h.crc || self.size != h.size {
            return bad("checksum mismatch");
        }
        if self.t != h.duration_ms {
            return bad("demo timing");
        }
        Ok(())
    }
}

/// Decompresses a whole file in memory.
pub fn decompress(b: &[u8], threads: usize, limits: &Limits) -> Result<Vec<u8>> {
    let budget = Budget::new(limits);
    budget.input(b.len() as u64);
    let lay = scan_layout(&mut SliceSrc(b), &budget)?;
    let size = lay.header.size;
    let _c = budget.alloc(size)?;
    budget.work(size)?;
    let mut out = zeroed(usz(size).ok_or(Error::Limit("demo size"))?)?;
    let nseg = lay.segs.len();
    let threads = threads.max(1);
    let (outer, inner) = if nseg >= threads {
        (threads, 1)
    } else {
        (1, threads)
    };
    let mut slices: Vec<std::sync::Mutex<&mut [u8]>> = Vec::with_capacity(nseg);
    let mut rest: &mut [u8] = &mut out;
    for (_, h) in &lay.segs {
        let (a, r) = rest.split_at_mut(h.raw_len as usize);
        slices.push(std::sync::Mutex::new(a));
        rest = r;
    }
    let res = par_map(outer, nseg, |i| {
        let (s, h) = &lay.segs[i];
        let body = &b[s.body_off() as usize..s.end() as usize];
        let mut dst = slices[i].lock().map_err(|_| Error::Internal("lock"))?;
        decode_segment(&lay, body, h, &mut dst, inner, &budget)
    });
    drop(slices);
    let mut chk = DemoCheck::default();
    let mut at = 0usize;
    for (r, (_, h)) in res.into_iter().zip(&lay.segs) {
        let tm = r?;
        let n = h.raw_len as usize;
        chk.segment(&out[at..at + n], tm)?;
        at += n;
    }
    chk.finish(&lay)?;
    Ok(out)
}
