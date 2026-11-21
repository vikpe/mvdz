//! In-order decompression: [`Decoder`] decodes a file as it arrives from any `io::Read`,
//! as many segments at a time as it has threads.

use crate::budget::{Budget, Charge, Limits, zeroed};
use crate::coder::rans::ModelsBlock;
use crate::decode::{DemoCheck, decode_segment};
use crate::error::{Error, Result, bad};
use crate::format::{
    HEADER_LEN, Header, LEN_FIELD_LEN, Layout, MAX_MODELS_LEN, SEGMENT_FIELDS_LEN, SegmentHeader,
    SegmentSpan, check_coded_len, len_field, parse_segment_header,
};
use crate::util::{par_map, thread_count};
use std::collections::VecDeque;
use std::io::{self, Read};

/// Yields the decoded bytes through `io::Read`, checked as they arrive: each segment's
/// CRC-32 before its bytes, the whole CRC-32 before the end. The first error is sticky.
#[derive(Debug)]
pub struct Decoder<R: Read> {
    input: R,
    layout: Layout,
    next_offset: u64,
    current: Vec<u8>,
    current_charge: Option<Charge>,
    at: usize,
    /// Decoded and checked, after `current`.
    ready: VecDeque<(Vec<u8>, Charge)>,
    /// Met after the segments in `ready`; reported once they are read.
    deferred_error: Option<Error>,
    is_done: bool,
    failure: Option<Error>,
    threads: usize,
    budget: Budget,
    check: Option<DemoCheck>,
}

impl<R: Read> Decoder<R> {
    /// Reads the header; decodes on all logical cores until `max_threads` says otherwise.
    pub fn new(input: R) -> Result<Self> {
        Self::with_limits(input, thread_count(0), &Limits::default())
    }

    /// Up to this many segments are decoded at once, nothing more is buffered; capped at the
    /// logical cores, 0 is all of them.
    #[must_use]
    pub fn max_threads(mut self, max_threads: u8) -> Self {
        self.threads = thread_count(max_threads);
        self
    }

    pub fn header(&self) -> &Header {
        &self.layout.header
    }

    pub(crate) fn with_limits(mut input: R, threads: usize, limits: &Limits) -> Result<Self> {
        let mut header = [0u8; HEADER_LEN];
        let mut filled = 0;
        while filled < HEADER_LEN {
            match input.read(&mut header[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read.min(HEADER_LEN - filled),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        let budget = Budget::new(limits);
        let layout = Layout::new(&header[..filled], &budget)?;
        budget.input(HEADER_LEN as u64);
        let check = Some(DemoCheck::default());
        Ok(Self {
            input,
            layout,
            next_offset: HEADER_LEN as u64,
            current: Vec::new(),
            current_charge: None,
            at: 0,
            ready: VecDeque::new(),
            deferred_error: None,
            is_done: false,
            failure: None,
            threads,
            budget,
            check,
        })
    }

    /// Returns 0 at the end. The first error is returned again by every later call.
    pub(crate) fn read_demo(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.check_failed()?;
        while self.at == self.current.len() && !self.is_done {
            if let Err(error) = self.advance() {
                self.failure = Some(error);
                self.current = Vec::new();
                self.current_charge = None;
                self.ready.clear();
                self.at = 0;
                return Err(error);
            }
        }
        let len = buf.len().min(self.current.len() - self.at);
        buf[..len].copy_from_slice(&self.current[self.at..self.at + len]);
        self.at += len;
        Ok(len)
    }

    fn check_failed(&self) -> Result<()> {
        match self.failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// `None`: the stream ends before its first byte.
    fn read_len(&mut self) -> Result<Option<u64>> {
        let mut field = [0u8; LEN_FIELD_LEN];
        let mut filled = 0;
        while filled < LEN_FIELD_LEN {
            match self.input.read(&mut field[filled..]) {
                Ok(0) if filled == 0 => return Ok(None),
                Ok(0) => return bad("incomplete file"),
                Ok(read) => filled += read.min(LEN_FIELD_LEN - filled),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(len_field(&field))
    }

    /// Charged to the budget while the returned charge lives.
    fn read_body(&mut self, len: u64) -> Result<(Vec<u8>, Charge)> {
        let charge = self.budget.alloc(len)?;
        // the data arrives before the buffer grows: a lying length on a
        // short stream allocates nothing
        let mut body = Vec::new();
        (&mut self.input).take(len).read_to_end(&mut body)?;
        if body.len() as u64 != len {
            return bad("incomplete file");
        }
        self.budget.input(len);
        Ok((body, charge))
    }

    fn read_segment(&mut self) -> Result<(SegmentHeader, Vec<u8>, Charge)> {
        // the stream may end here, short of the demo: `decode_batch` delivers the
        // complete segments before it first
        let Some(coded) = self.read_len()? else {
            return bad("incomplete file");
        };
        check_coded_len(coded)?;
        let span = SegmentSpan {
            len_field_offset: self.next_offset,
            body_len: coded + SEGMENT_FIELDS_LEN as u64,
        };
        let (body, charge) = self.read_body(span.body_len)?;
        let header = parse_segment_header(&body[..SEGMENT_FIELDS_LEN])?;
        let header = self.layout.segment(&span, header)?;
        self.next_offset = span.end();
        Ok((header, body, charge))
    }

    /// Up to `threads` segments, decoded in parallel, queued in order.
    /// ponytail: reading and decoding take turns and a batch waits for its
    /// slowest segment; a rolling window would overlap them.
    fn decode_batch(&mut self) -> Result<()> {
        let mut segments = Vec::new();
        while segments.len() < self.threads && !self.layout.is_complete() {
            match self.read_segment() {
                Ok(segment) => segments.push(segment),
                Err(error) if !segments.is_empty() => {
                    self.deferred_error = Some(error);
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        let (layout, budget) = (&self.layout, &self.budget);
        let decoded = par_map(self.threads, segments.len(), |index| {
            let (header, body, _) = &segments[index];
            let charge = budget.alloc(u64::from(header.raw_len))?;
            budget.work(u64::from(header.raw_len))?;
            let mut out = zeroed(header.raw_len as usize)?;
            let scan = decode_segment(layout, body, header, &mut out, budget)?;
            Ok((out, charge, scan))
        });
        for result in decoded {
            let checked = result.and_then(|(out, charge, scan)| {
                if let Some(check) = self.check.as_mut() {
                    check.segment(&out, scan)?;
                }
                Ok((out, charge))
            });
            match checked {
                Ok(segment) => self.ready.push_back(segment),
                Err(error) => {
                    // earlier in the demo than any error met while reading on
                    self.deferred_error = Some(error);
                    break;
                }
            }
        }
        Ok(())
    }

    /// Until a segment's bytes are available or the file ends.
    fn advance(&mut self) -> Result<()> {
        while !self.is_done && self.at == self.current.len() {
            if let Some((out, charge)) = self.ready.pop_front() {
                self.current = out;
                self.current_charge = Some(charge);
                self.at = 0;
                continue;
            }
            self.current = Vec::new();
            self.current_charge = None;
            self.at = 0;
            if let Some(error) = self.deferred_error.take() {
                return Err(error);
            }
            if !self.layout.has_models() {
                let Some(len) = self.read_len()? else {
                    return bad("incomplete file");
                };
                if len > MAX_MODELS_LEN {
                    return bad("models too large");
                }
                let models_block = if len > 0 {
                    let (body, _charge) = self.read_body(len)?;
                    Some(ModelsBlock::new(&body, self.threads, &self.budget)?)
                } else {
                    None
                };
                self.layout.models(models_block)?;
                self.next_offset += LEN_FIELD_LEN as u64 + len;
                continue;
            }
            if self.layout.is_complete() {
                // the file ends here
                let mut one = [0u8; 1];
                loop {
                    match self.input.read(&mut one) {
                        Ok(0) => break,
                        Ok(_) => return bad("trailing bytes"),
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                self.layout.finish()?;
                if let Some(check) = self.check.take() {
                    check.finish(&self.layout)?;
                }
                self.is_done = true;
                break;
            }
            self.decode_batch()?;
        }
        Ok(())
    }
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Ok(self.read_demo(buf)?)
    }
}
