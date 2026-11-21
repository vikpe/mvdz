//! Decompression in order.
//!
//! [`Reader`] decodes a file as it arrives from any `io::Read` (file,
//! pipe, socket, VFS callback), as many segments at a time as it has
//! threads.

use crate::budget::{Budget, Charge, Limits, zeroed};
use crate::coder::rans::FileTables;
use crate::decode::{DemoCheck, decode_segment};
use crate::error::{Error, Result, bad};
use crate::format::{
    HEADER_LEN, Header, LEN, Layout, SEG_FIELDS, Sec, SegHdr, TABLES_MAX, check_coded_len,
    len_field, parse_seg_hdr,
};
use crate::util::par_map;
use std::collections::VecDeque;
use std::io::{self, Read};

// ---------------------------------------------------------------- reader

/// Decoder of a file read in order.
///
/// Yields the original demo bytes
/// through `io::Read`, checked as they arrive: every segment's CRC-32
/// before its bytes are returned, and the whole demo's size, duration and
/// CRC-32 before the end is reported. The first error is sticky.
pub struct Reader<R: Read> {
    r: R,
    lay: Layout,
    /// the file offset of what is read next
    pos: u64,
    cur: Vec<u8>,
    /// held so the current segment stays charged to the budget
    cur_charge: Option<Charge>,
    at: usize,
    /// decoded and checked segments after the current one
    ready: VecDeque<(Vec<u8>, Charge)>,
    /// an error met after the segments in `ready`, reported once they
    /// are read
    later: Option<Error>,
    done: bool,
    failed: Option<Error>,
    threads: usize,
    budget: Budget,
    /// the whole-demo checks, until the end has passed them
    check: Option<DemoCheck>,
}

impl<R: Read> Reader<R> {
    /// Decoding from any reader; reads the file's header.
    pub fn new(mut r: R, threads: usize, limits: &Limits) -> Result<Self> {
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
        Ok(Self {
            r,
            lay,
            pos: HEADER_LEN as u64,
            cur: Vec::new(),
            cur_charge: None,
            at: 0,
            ready: VecDeque::new(),
            later: None,
            done: false,
            failed: None,
            threads: crate::util::threads(threads),
            budget,
            check,
        })
    }

    /// The file's header: the demo's size, duration and hashes.
    pub fn header(&self) -> &Header {
        &self.lay.header
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

    /// The next segment of the stream: its header and its bytes, charged
    /// to the budget while the charge lives.
    fn read_segment(&mut self) -> Result<(SegHdr, Vec<u8>, Charge)> {
        // the stream may end here, short of the demo: the complete
        // segments before this point are delivered first (`fill`)
        let Some(coded) = self.read_len()? else {
            return bad("incomplete file");
        };
        check_coded_len(coded)?;
        let s = Sec {
            off: self.pos,
            len: coded + SEG_FIELDS as u64,
        };
        let (b, c) = self.read_body(s.len)?;
        let h = parse_seg_hdr(&b[..SEG_FIELDS])?;
        let h = self.lay.segment(&s, h)?;
        self.pos = s.end();
        Ok((h, b, c))
    }

    /// Reads up to `threads` segments, decodes them in parallel and queues
    /// them in order, each checked. An error after the first segment waits
    /// in `later` until the segments before it have been read.
    fn fill(&mut self) -> Result<()> {
        let mut segs = Vec::new();
        while segs.len() < self.threads && !self.lay.complete() {
            match self.read_segment() {
                Ok(seg) => segs.push(seg),
                Err(e) if !segs.is_empty() => {
                    self.later = Some(e);
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        let (lay, budget) = (&self.lay, &self.budget);
        let decoded = par_map(self.threads, segs.len(), |i| {
            let (h, body, _) = &segs[i];
            let charge = budget.alloc(u64::from(h.raw_len))?;
            budget.work(u64::from(h.raw_len))?;
            let mut out = zeroed(h.raw_len as usize)?;
            let tm = decode_segment(lay, body, h, &mut out, budget)?;
            Ok((out, charge, tm))
        });
        for r in decoded {
            let checked = r.and_then(|(out, charge, tm)| {
                if let Some(c) = self.check.as_mut() {
                    c.segment(&out, tm)?;
                }
                Ok((out, charge))
            });
            match checked {
                Ok(seg) => self.ready.push_back(seg),
                Err(e) => {
                    // an earlier error than any met while reading on
                    self.later = Some(e);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Reads on until a segment's bytes are available or the file ends.
    fn advance(&mut self) -> Result<()> {
        while !self.done && self.at == self.cur.len() {
            if let Some((out, charge)) = self.ready.pop_front() {
                self.cur = out;
                self.cur_charge = Some(charge);
                self.at = 0;
                continue;
            }
            self.cur = Vec::new();
            self.cur_charge = None;
            self.at = 0;
            if let Some(e) = self.later.take() {
                return Err(e);
            }
            if !self.lay.has_models() {
                let Some(len) = self.read_len()? else {
                    return bad("incomplete file");
                };
                if len > TABLES_MAX {
                    return bad("models too large");
                }
                let t = if len > 0 {
                    let (b, _c) = self.read_body(len)?;
                    Some(FileTables::new(&b, self.threads, &self.budget)?)
                } else {
                    None
                };
                self.lay.models(t)?;
                self.pos += LEN as u64 + len;
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
            self.fill()?;
        }
        Ok(())
    }

    /// Reads demo bytes; returns 0 at the end. The first error is returned
    /// again by every later call.
    pub fn read_demo(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.check_failed()?;
        while self.at == self.cur.len() && !self.done {
            if let Err(e) = self.advance() {
                self.failed = Some(e);
                self.cur = Vec::new();
                self.cur_charge = None;
                self.ready.clear();
                self.at = 0;
                return Err(e);
            }
        }
        let n = buf.len().min(self.cur.len() - self.at);
        buf[..n].copy_from_slice(&self.cur[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl<R: Read> Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.read_demo(buf)?)
    }
}
