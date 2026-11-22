//! Decoding: the container reader, segment by segment, and the checks over the demo.

use crate::coder::rans::read_models_block;
use crate::error::{Error, Result, bad};
use crate::format::{
    HEADER_LEN, Header, LEN_FIELD_LEN, Layout, MAX_MODELS_LEN, MAX_SEGMENT_RAW, SEGMENT_FIELDS_LEN,
    SegmentHeader, check_coded_len, parse_segment_header,
};
use crate::memory::{DEFAULT_MAX_MEMORY, try_zeroed};
use crate::parallel::{par_map, thread_count};
use crate::segment::{self, FrameScan, frame_scan};
use std::collections::VecDeque;
use std::io::{self, Read};

/// `max_threads` 0 is all logical cores.
///
/// # Errors
///
/// [`Error::NotMvdz`] or [`Error::UnsupportedVersion`] for another file type or version,
/// [`Error::Corrupt`] for damaged input, [`Error::MemoryLimit`] for a demo of more than 1 GiB:
/// read that one with [`Decoder`].
pub fn decode(bytes: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    decompress(bytes, thread_count(max_threads), DEFAULT_MAX_MEMORY)
}

/// Each segment's CRC-32 is checked before its bytes are read; the first error is sticky.
#[derive(Debug)]
pub struct Decoder<R: Read> {
    input: R,
    layout: Layout,
    current: Option<Vec<u8>>,
    /// Into `current`.
    at: usize,
    ready: VecDeque<Vec<u8>>,
    /// Met after the segments in `ready`; reported once they are read.
    deferred_error: Option<Error>,
    is_done: bool,
    failure: Option<Error>,
    threads: usize,
    max_memory: u64,
    check: DemoCheck,
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
        let layout = Layout::new(&header[..filled])?;
        Ok(Self {
            input,
            layout,
            current: None,
            at: 0,
            ready: VecDeque::new(),
            deferred_error: None,
            is_done: false,
            failure: None,
            threads,
            max_memory,
            check: DemoCheck::default(),
        })
    }

    pub(crate) fn read_demo(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.at == self.current_bytes().len() {
            self.current = self.next_segment()?;
            self.at = 0;
        }
        let unread = &self.current_bytes()[self.at..];
        let len = buf.len().min(unread.len());
        buf[..len].copy_from_slice(&unread[..len]);
        self.at += len;
        Ok(len)
    }

    /// `None` at the end of the demo. The first error is sticky.
    pub(crate) fn next_segment(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let next = self.advance();
        if let Err(error) = &next {
            self.failure = Some(*error);
            self.ready.clear();
        }
        next
    }

    fn current_bytes(&self) -> &[u8] {
        self.current.as_deref().unwrap_or_default()
    }

    fn advance(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(segment) = self.ready.pop_front() {
                return Ok(Some(segment));
            }
            if let Some(error) = self.deferred_error.take() {
                return Err(error);
            }
            if self.is_done {
                return Ok(None);
            }
            if !self.layout.has_models() {
                self.read_models()?;
            } else if self.layout.is_complete() {
                self.finish()?;
            } else {
                self.decode_batch()?;
            }
        }
    }

    fn read_models(&mut self) -> Result<()> {
        let Some(len) = self.read_len()? else {
            return bad("incomplete file");
        };
        if len > MAX_MODELS_LEN {
            return bad("models too large");
        }
        let models = if len > 0 {
            let body = self.read_body(len)?;
            Some(read_models_block(&body, self.threads, self.max_memory)?)
        } else {
            None
        };
        self.layout.set_models(models);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let mut one = [0u8; 1];
        if read_up_to(&mut self.input, &mut one)? != 0 {
            return bad("trailing bytes");
        }
        self.layout.finish()?;
        std::mem::take(&mut self.check).finish(&self.layout)?;
        self.is_done = true;
        Ok(())
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

    fn read_body(&mut self, len: u64) -> Result<Vec<u8>> {
        // the data arrives before the buffer grows: a lying length on a
        // short stream allocates nothing
        let mut body = Vec::new();
        (&mut self.input).take(len).read_to_end(&mut body)?;
        if body.len() as u64 != len {
            return bad("incomplete file");
        }
        Ok(body)
    }

    fn read_segment(&mut self) -> Result<(SegmentHeader, Vec<u8>)> {
        // the stream may end here, short of the demo: `decode_batch` delivers the
        // complete segments before it first
        let Some(coded) = self.read_len()? else {
            return bad("incomplete file");
        };
        check_coded_len(coded)?;
        let body = self.read_body(coded + SEGMENT_FIELDS_LEN as u64)?;
        let header = parse_segment_header(&body[..SEGMENT_FIELDS_LEN])?;
        self.layout.add_segment(&header)?;
        Ok((header, body))
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
        let layout = &self.layout;
        let decoded = par_map(self.threads, &segments, |(header, body)| {
            let mut bytes = try_zeroed(header.raw_len as usize)?;
            let scan = decode_segment(layout, body, header, &mut bytes)?;
            Ok((bytes, scan))
        });
        for result in decoded {
            let checked = result.and_then(|(bytes, scan)| {
                self.check.segment(&bytes, scan)?;
                Ok(bytes)
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

fn decode_segment(
    layout: &Layout,
    body: &[u8],
    header: &SegmentHeader,
    dst: &mut [u8],
) -> Result<FrameScan> {
    if dst.len() as u64 != u64::from(header.raw_len) {
        return bad("segment length");
    }
    let coded = body
        .get(SEGMENT_FIELDS_LEN..)
        .ok_or(Error::Corrupt("segment header"))?;
    let transform_header = segment::parse_transform_header(coded, dst.len())?;
    segment::decode(coded, &transform_header, dst, layout.models.as_ref())?;
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
struct DemoCheck {
    crc32: crc32fast::Hasher,
    size_bytes: u64,
    duration_ms: u64,
    is_inside_frame: bool,
}

impl DemoCheck {
    fn segment(&mut self, out: &[u8], scan: FrameScan) -> Result<()> {
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
    fn finish(self, layout: &Layout) -> Result<()> {
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
    let mut decoder = Decoder::with_max_memory(file, threads, max_memory)?;
    let mut demo = Vec::new();
    while let Some(segment) = decoder.next_segment()? {
        // the bytes decoded, not the header's size: a lying header allocates nothing
        if (demo.len() + segment.len()) as u64 > max_memory {
            return Err(Error::MemoryLimit("demo size"));
        }
        demo.try_reserve(segment.len())
            .map_err(|_| Error::MemoryLimit("out of memory"))?;
        demo.extend_from_slice(&segment);
    }
    Ok(demo)
}
