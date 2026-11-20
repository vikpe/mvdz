//! Sequential and seekable decompression.
//!
//! [`Reader`] decodes sequentially from any `io::Read` (pipe, socket, VFS
//! callback), or with `io::Seek` by segment and demo time.

use crate::budget::{Budget, Charge, Limits, zeroed};
use crate::coder::rans::FileTables;
use crate::decode::{DemoCheck, SeekSrc, decode_segment, read_full, scan_layout};
use crate::error::{Error, Result, bad};
use crate::format::*;
use crate::util::usz;
use std::io::{self, Read, Seek, SeekFrom};

// ---------------------------------------------------------------- reader

/// The byte source of a [`Reader`].
pub trait Source: Read {
    /// Positions the source at file offset `off` (random access only).
    fn seek_to(&mut self, off: u64) -> Result<()>;
}

/// A source read strictly in order.
pub struct Seq<R>(pub R);

impl<R: Read> Read for Seq<R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}

impl<R: Read> Source for Seq<R> {
    fn seek_to(&mut self, _: u64) -> Result<()> {
        Err(Error::Invalid("not a seekable reader"))
    }
}

/// A source with random access.
pub struct Seekable<R>(pub R);

impl<R: Read> Read for Seekable<R> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}

impl<R: Read + Seek> Source for Seekable<R> {
    fn seek_to(&mut self, off: u64) -> Result<()> {
        if self.0.seek(SeekFrom::Start(off))? != off {
            return Err(Error::Io(io::ErrorKind::InvalidData));
        }
        Ok(())
    }
}

enum Mode {
    /// the file in order, checked as it arrives
    Seq { pos: u64 },
    /// a complete file whose layout was checked when it was opened
    Random { next: usize },
}

/// Sequential (and, over `Seek`, random-access) decoder. Yields the
/// original demo bytes through `io::Read`. The first error is sticky.
pub struct Reader<S: Source> {
    r: S,
    lay: Layout,
    /// the segments of a random-access reader
    index: Option<Index>,
    mode: Mode,
    cur: Vec<u8>,
    _cur_charge: Option<Charge>,
    at: usize,
    done: bool,
    failed: Option<Error>,
    threads: usize,
    budget: Budget,
    /// whole-demo checks while reading from the first segment on
    check: Option<DemoCheck>,
}

impl<R: Read> Reader<Seq<R>> {
    /// Sequential decoding from any reader.
    pub fn new(r: R, threads: usize, limits: &Limits) -> Result<Self> {
        let mut r = Seq(r);
        let mut p = [0u8; HEADER_LEN];
        let mut n = 0;
        while n < HEADER_LEN {
            match r.read(&mut p[n..]) {
                Ok(0) => break,
                Ok(k) => n += k.min(HEADER_LEN - n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        let budget = Budget::new(limits);
        let lay = Layout::new(&p[..n], &budget)?;
        budget.input(HEADER_LEN as u64);
        let check = Some(DemoCheck::default());
        Ok(Reader {
            r,
            lay,
            index: None,
            mode: Mode::Seq {
                pos: HEADER_LEN as u64,
            },
            cur: Vec::new(),
            _cur_charge: None,
            at: 0,
            done: false,
            failed: None,
            threads: threads.max(1),
            budget,
            check,
        })
    }
}

impl<R: Read + Seek> Reader<Seekable<R>> {
    /// Opens a complete file for random access: checks its whole
    /// structure (the same rules as every other reader) and positions at
    /// the first segment.
    pub fn open(r: R, threads: usize, limits: &Limits) -> Result<Self> {
        let budget = Budget::new(limits);
        let mut src = SeekSrc(r);
        let len = crate::decode::Src::len(&mut src)?;
        budget.input(len);
        let lay = scan_layout(&mut src, &budget)?;
        let check = Some(DemoCheck::default());
        Ok(Reader {
            r: Seekable(src.0),
            index: Some(lay.index()),
            lay,
            mode: Mode::Random { next: 0 },
            cur: Vec::new(),
            _cur_charge: None,
            at: 0,
            done: false,
            failed: None,
            threads: threads.max(1),
            budget,
            check,
        })
    }

    /// Positions the reader at the start of segment `i`; returns its raw
    /// byte offset.
    pub fn seek_segment(&mut self, i: usize) -> Result<u64> {
        self.check_failed()?;
        let Some((_, h)) = self.lay.segs.get(i) else {
            return Err(Error::Invalid("segment out of range"));
        };
        let raw_off = h.raw_off;
        self.mode = Mode::Random { next: i };
        self.cur.clear();
        self._cur_charge = None;
        self.at = 0;
        self.done = false;
        // whole-demo checks apply when reading from the start
        self.check = (i == 0).then(DemoCheck::default);
        Ok(raw_off)
    }

    /// Positions the reader at the segment that holds demo time `ms`;
    /// returns (segment, its start time in ms, its raw byte offset).
    pub fn seek_time(&mut self, ms: u64) -> Result<(usize, u64, u64)> {
        self.check_failed()?;
        let ix = self.index().ok_or(Error::Invalid("no index"))?;
        if ix.segs.is_empty() {
            return Err(Error::Invalid("no segments"));
        }
        let i = ix.seg_at_time(ms);
        let (t, o) = (ix.segs[i].t_start, ix.segs[i].raw_off);
        self.seek_segment(i)?;
        Ok((i, t, o))
    }
}

impl<S: Source> Reader<S> {
    /// The file's header: the demo's size, duration and hashes.
    pub fn header(&self) -> &Header {
        &self.lay.header
    }

    /// The segments with their offsets and start times (random-access
    /// readers).
    pub fn index(&self) -> Option<&Index> {
        self.index.as_ref()
    }

    /// Number of segments (seek points); 0 for a sequential reader.
    pub fn segments(&self) -> usize {
        match self.mode {
            Mode::Random { .. } => self.lay.segs.len(),
            Mode::Seq { .. } => 0,
        }
    }

    fn check_failed(&self) -> Result<()> {
        match self.failed {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The next length field of the stream (None: the stream ends before
    /// its first byte).
    fn read_len(&mut self) -> Result<Option<u64>> {
        let mut b = [0u8; LEN];
        let mut n = 0;
        while n < LEN {
            match self.r.read(&mut b[n..]) {
                Ok(0) if n == 0 => return Ok(None),
                Ok(0) => return bad("incomplete file"),
                Ok(k) => n += k.min(LEN - n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(len_field(&b))
    }

    /// The `len` bytes of a block, charged to the budget while the charge
    /// lives.
    fn read_body(&mut self, len: u64) -> Result<(Vec<u8>, Charge)> {
        let c = self.budget.alloc(len)?;
        // the data arrives before the buffer grows: a lying length on a
        // short stream allocates nothing
        let mut v = Vec::new();
        (&mut self.r).take(len).read_to_end(&mut v)?;
        if v.len() as u64 != len {
            return bad("incomplete file");
        }
        self.budget.input(len);
        Ok((v, c))
    }

    fn deliver(&mut self, h: &SegHdr, body: &[u8]) -> Result<()> {
        let n = usz(h.raw_len).ok_or(Error::Limit("segment size"))?;
        self.cur = Vec::new();
        self._cur_charge = None;
        let charge = self.budget.alloc(h.raw_len)?;
        self.budget.work(h.raw_len)?;
        let mut out = zeroed(n)?;
        let tm = decode_segment(&self.lay, body, h, &mut out, self.threads, &self.budget)?;
        if let Some(c) = self.check.as_mut() {
            c.segment(&out, tm)?;
        }
        self.cur = out;
        self._cur_charge = Some(charge);
        self.at = 0;
        Ok(())
    }

    /// Reads on until a segment's bytes are available or the file ends;
    /// returns the offset of what follows.
    fn advance_seq(&mut self, mut pos: u64) -> Result<u64> {
        while !self.done && self.at == self.cur.len() {
            if !self.lay.has_models() {
                let Some(len) = self.read_len()? else {
                    return bad("incomplete file");
                };
                if len > TABLES_MAX {
                    return bad("models too large");
                }
                let t = if len > 0 {
                    let (b, _c) = self.read_body(len)?;
                    Some(FileTables::new(&b, &self.budget)?)
                } else {
                    None
                };
                self.lay.models(t)?;
                pos += LEN as u64 + len;
                continue;
            }
            if self.lay.complete() {
                // the segments hold the whole demo: the file ends here
                let mut one = [0u8; 1];
                loop {
                    match self.r.read(&mut one) {
                        Ok(0) => break,
                        Ok(_) => return bad("trailing bytes"),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                self.lay.finish()?;
                if let Some(c) = self.check.take() {
                    c.finish(&self.lay)?;
                }
                self.done = true;
                break;
            }
            // the stream may end here, short of the demo: every complete
            // segment has been delivered
            let Some(coded) = self.read_len()? else {
                return bad("incomplete file");
            };
            let s = Sec {
                off: pos,
                len: coded + SEG_FIELDS as u64,
            };
            let (b, _c) = self.read_body(s.len)?;
            let h = parse_seg_hdr(&b[..SEG_FIELDS])?;
            let h = self.lay.segment(&s, h)?;
            self.deliver(&h, &b)?;
            pos = s.end();
        }
        Ok(pos)
    }

    fn advance_random(&mut self, next: usize) -> Result<usize> {
        if next >= self.lay.segs.len() {
            if let Some(c) = self.check.take() {
                c.finish(&self.lay)?;
            }
            self.done = true;
            return Ok(next);
        }
        let (s, h) = self.lay.segs[next];
        let _c = self.budget.alloc(s.len)?;
        let mut b = zeroed(usz(s.len).ok_or(Error::Limit("section size"))?)?;
        self.r.seek_to(s.body_off())?;
        read_full(&mut self.r, &mut b)?;
        self.deliver(&h, &b)?;
        Ok(next + 1)
    }

    /// Reads demo bytes; returns 0 at the end. The first error is returned
    /// again by every later call.
    pub fn read_demo(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.check_failed()?;
        while self.at == self.cur.len() && !self.done {
            let r = match self.mode {
                Mode::Seq { pos } => self.advance_seq(pos).map(|p| Mode::Seq { pos: p }),
                Mode::Random { next } => {
                    self.advance_random(next).map(|n| Mode::Random { next: n })
                }
            };
            match r {
                Ok(m) => self.mode = m,
                Err(e) => {
                    self.failed = Some(e);
                    self.cur = Vec::new();
                    self._cur_charge = None;
                    self.at = 0;
                    return Err(e);
                }
            }
        }
        let n = buf.len().min(self.cur.len() - self.at);
        buf[..n].copy_from_slice(&self.cur[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl<S: Source> Read for Reader<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.read_demo(buf)?)
    }
}
