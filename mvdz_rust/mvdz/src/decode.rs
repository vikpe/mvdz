//! Decoding: whole-file and streaming, the layout scan, and the checks over the demo.

use crate::budget::{Budget, Charge, DEFAULT_MAX_MEMORY, try_zeroed};
use crate::coder::rans::ModelsBlock;
use crate::error::{Error, Result, bad};
use crate::format::{
    HEADER_LEN, Header, LEN_FIELD_LEN, Layout, MAX_MODELS_LEN, MAX_SEGMENT_RAW, SEGMENT_FIELDS_LEN,
    SegmentHeader, SegmentSpan, check_coded_len, len_field, parse_segment_header,
};
use crate::parallel::{par_map, thread_count};
use crate::segment::{self, FrameScan, frame_scan};
use std::collections::VecDeque;
use std::io::{self, Read};

/// `max_threads` 0 is all logical cores.
///
/// # Errors
///
/// [`Error::NotMvdz`] or [`Error::UnsupportedVersion`] for another file type or version,
/// [`Error::Corrupt`] for damaged input, [`Error::MemoryLimit`] past the memory limit.
pub fn decode(bytes: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    decompress(bytes, thread_count(max_threads), DEFAULT_MAX_MEMORY)
}

/// Each segment's CRC-32 is checked before its bytes are read; the first error is sticky.
#[derive(Debug)]
pub struct Decoder<R: Read> {
    input: R,
    layout: Layout,
    next_offset: u64,
    current: Option<Decoded>,
    /// Into `current`.
    at: usize,
    ready: VecDeque<Decoded>,
    /// Met after the segments in `ready`; reported once they are read.
    deferred_error: Option<Error>,
    is_done: bool,
    failure: Option<Error>,
    threads: usize,
    budget: Budget,
    check: DemoCheck,
}

#[derive(Debug)]
struct Decoded {
    bytes: Vec<u8>,
    _charge: Charge,
}

impl<R: Read> Decoder<R> {
    /// Reads the header only; the segments are read by [`Read::read`].
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when reading fails, [`Error::NotMvdz`] or [`Error::UnsupportedVersion`] for
    /// another file type or version, [`Error::Corrupt`] for a truncated header.
    pub fn new(input: R) -> Result<Self> {
        Self::with_max_memory(input, thread_count(0), DEFAULT_MAX_MEMORY)
    }

    /// Segments decoded at once; capped at the logical cores, 0, the default, is all of them.
    #[must_use]
    pub fn max_threads(mut self, max_threads: u8) -> Self {
        self.threads = thread_count(max_threads);
        self
    }

    pub fn header(&self) -> &Header {
        &self.layout.header
    }

    pub(crate) fn with_max_memory(mut input: R, threads: usize, max_memory: u64) -> Result<Self> {
        let mut header = [0u8; HEADER_LEN];
        let filled = read_up_to(&mut input, &mut header)?;
        let budget = Budget::new(max_memory);
        let layout = Layout::new(&header[..filled], &budget)?;
        Ok(Self {
            input,
            layout,
            next_offset: HEADER_LEN as u64,
            current: None,
            at: 0,
            ready: VecDeque::new(),
            deferred_error: None,
            is_done: false,
            failure: None,
            threads,
            budget,
            check: DemoCheck::default(),
        })
    }

    pub(crate) fn read_demo(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.check_failed()?;
        if let Err(error) = self.advance() {
            self.failure = Some(error);
            self.current = None;
            self.ready.clear();
            self.at = 0;
            return Err(error);
        }
        let current = self.current_bytes();
        let len = buf.len().min(current.len() - self.at);
        buf[..len].copy_from_slice(&current[self.at..self.at + len]);
        self.at += len;
        Ok(len)
    }

    fn current_bytes(&self) -> &[u8] {
        self.current.as_ref().map_or(&[], |decoded| &decoded.bytes)
    }

    fn check_failed(&self) -> Result<()> {
        match self.failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// `None` at the end of the input.
    fn read_len(&mut self) -> Result<Option<u64>> {
        let mut field = [0u8; LEN_FIELD_LEN];
        match read_up_to(&mut self.input, &mut field)? {
            0 => Ok(None),
            LEN_FIELD_LEN => Ok(Some(u64::from(u32::from_le_bytes(field)))),
            _ => bad("incomplete file"),
        }
    }

    fn read_body(&mut self, len: u64) -> Result<(Vec<u8>, Charge)> {
        let charge = self.budget.alloc(len)?;
        // the data arrives before the buffer grows: a lying length on a
        // short stream allocates nothing
        let mut body = Vec::new();
        (&mut self.input).take(len).read_to_end(&mut body)?;
        if body.len() as u64 != len {
            return bad("incomplete file");
        }
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
        let header = self.layout.add_segment(&span, header)?;
        self.next_offset = span.end();
        Ok((header, body, charge))
    }

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
        let decoded = par_map(self.threads, &segments, |(header, body, _)| {
            let charge = budget.alloc(u64::from(header.raw_len))?;
            let mut bytes = try_zeroed(header.raw_len as usize)?;
            let scan = decode_segment(layout, body, header, &mut bytes, budget)?;
            Ok((bytes, charge, scan))
        });
        for result in decoded {
            let checked = result.and_then(|(bytes, charge, scan)| {
                self.check.segment(&bytes, scan)?;
                Ok(Decoded {
                    bytes,
                    _charge: charge,
                })
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

    /// Until `current` has unread bytes or the demo is done.
    fn advance(&mut self) -> Result<()> {
        while !self.is_done && self.at == self.current_bytes().len() {
            self.current = self.ready.pop_front();
            self.at = 0;
            if self.current.is_some() {
                continue;
            }
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
                let mut one = [0u8; 1];
                if read_up_to(&mut self.input, &mut one)? != 0 {
                    return bad("trailing bytes");
                }
                self.layout.finish()?;
                std::mem::take(&mut self.check).finish(&self.layout)?;
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

/// Fills `buf` until it is full or the input ends; returns the count.
fn read_up_to(input: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read.min(buf.len() - filled),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn bytes_at(file: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    file.get(start..start.checked_add(usize::try_from(len).ok()?)?)
}

fn len_at(file: &[u8], offset: u64) -> Option<u64> {
    len_field(bytes_at(file, offset, LEN_FIELD_LEN as u64)?)
}

/// FORMAT.md section 7; segment payloads are not decoded.
pub(crate) fn scan_layout(file: &[u8], threads: usize, budget: &Budget) -> Result<Layout> {
    let file_len = file.len() as u64;
    let mut layout = Layout::new(&file[..file.len().min(HEADER_LEN)], budget)?;
    let mut offset = HEADER_LEN as u64;
    let Some(models_len) = len_at(file, offset) else {
        return bad("incomplete file");
    };
    offset += LEN_FIELD_LEN as u64;
    if models_len > MAX_MODELS_LEN {
        return bad("models too large");
    }
    if models_len > file_len - offset {
        return bad("incomplete file");
    }
    if models_len > 0 {
        let body = bytes_at(file, offset, models_len).ok_or(Error::Corrupt("truncated"))?;
        layout.models(Some(ModelsBlock::new(body, threads, budget)?))?;
    } else {
        layout.models(None)?;
    }
    offset += models_len;
    while !layout.is_complete() {
        let Some(coded) = len_at(file, offset) else {
            return bad("incomplete file");
        };
        check_coded_len(coded)?;
        let span = SegmentSpan {
            len_field_offset: offset,
            body_len: coded + SEGMENT_FIELDS_LEN as u64,
        };
        if span.body_len > file_len - span.body_offset() {
            return bad("incomplete file");
        }
        let fields = bytes_at(file, span.body_offset(), SEGMENT_FIELDS_LEN as u64)
            .ok_or(Error::Corrupt("truncated"))?;
        layout.add_segment(&span, parse_segment_header(fields)?)?;
        offset = span.end();
    }
    if offset != file_len {
        return bad("trailing bytes");
    }
    layout.finish()?;
    Ok(layout)
}

pub(crate) fn decode_segment(
    layout: &Layout,
    body: &[u8],
    header: &SegmentHeader,
    dst: &mut [u8],
    budget: &Budget,
) -> Result<FrameScan> {
    let models_block = layout.models_block.as_ref();
    if dst.len() as u64 != u64::from(header.raw_len) {
        return bad("segment length");
    }
    let coded = body
        .get(SEGMENT_FIELDS_LEN..)
        .ok_or(Error::Corrupt("segment header"))?;
    let transform_header = segment::parse_transform_header(coded, dst.len())?;
    segment::decode(coded, &transform_header, dst, models_block, budget)?;
    if crc32fast::hash(dst) != header.crc32 {
        return bad("segment checksum");
    }
    let scan = frame_scan(dst);
    if scan.duration_ms != u64::from(header.duration_ms) {
        return bad("segment duration");
    }
    Ok(scan)
}

/// Decoding does not recompute the header's SHA-256.
#[derive(Debug, Default)]
pub(crate) struct DemoCheck {
    crc32: crc32fast::Hasher,
    size_bytes: u64,
    duration_ms: u64,
    is_inside_frame: bool,
}

impl DemoCheck {
    pub(crate) fn segment(&mut self, out: &[u8], scan: FrameScan) -> Result<()> {
        if self.is_inside_frame {
            return bad("segment ends inside a frame");
        }
        self.crc32.update(out);
        self.size_bytes += out.len() as u64;
        self.duration_ms += scan.duration_ms;
        let has_tail = scan.framed_len != out.len();
        let is_cut_at_max = out.len() as u64 == MAX_SEGMENT_RAW;
        self.is_inside_frame = has_tail && !is_cut_at_max;
        Ok(())
    }
    pub(crate) fn finish(self, layout: &Layout) -> Result<()> {
        let header = &layout.header;
        if self.crc32.finalize() != header.crc32 || self.size_bytes != header.size_bytes {
            return bad("checksum mismatch");
        }
        if self.duration_ms != header.duration_ms {
            return bad("demo timing");
        }
        Ok(())
    }
}

pub(crate) fn decompress(file: &[u8], threads: usize, max_memory: u64) -> Result<Vec<u8>> {
    let budget = Budget::new(max_memory);
    let layout = scan_layout(file, threads, &budget)?;
    let size = layout.header.size_bytes;
    let _charge = budget.alloc(size)?;
    let mut out = try_zeroed(usize::try_from(size).map_err(|_| Error::MemoryLimit("demo size"))?)?;
    let mut slices: Vec<&mut [u8]> = Vec::with_capacity(layout.segments.len());
    let mut rest: &mut [u8] = &mut out;
    for (_, header) in &layout.segments {
        let (slice, after) = rest.split_at_mut(header.raw_len as usize);
        slices.push(slice);
        rest = after;
    }
    let results = par_map(
        threads,
        layout.segments.iter().zip(slices),
        |((span, header), dst)| {
            let body = span.body(file).ok_or(Error::Corrupt("truncated"))?;
            decode_segment(&layout, body, header, dst, &budget)
        },
    );
    let mut check = DemoCheck::default();
    let mut at = 0usize;
    for (result, (_, header)) in results.into_iter().zip(&layout.segments) {
        let scan = result?;
        let len = header.raw_len as usize;
        check.segment(&out[at..at + len], scan)?;
        at += len;
    }
    check.finish(&layout)?;
    Ok(out)
}
