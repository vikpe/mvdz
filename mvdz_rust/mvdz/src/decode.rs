//! Decoding: the container reader, segment by segment, and the checks over the demo.

use crate::error::{Error, Result, corrupt};
use crate::format::{
    HEADER_SIZE, Header, LEN_FIELD_SIZE, Layout, MAX_MODELS_LEN, SEGMENT_FIELDS_SIZE,
    SegmentHeader, check_coded_len, parse_segment_header,
};
use crate::memory::{DEFAULT_MAX_MEMORY, segments_in_flight, try_zeroed};
use crate::parallel::{parallel_map, thread_count};
use crate::rans::{Models, read_models_block};
use crate::segment;
use std::collections::VecDeque;
use std::io::{self, Read};

/// `max_threads` 0 is all logical cores.
///
/// # Errors
///
/// [`Error::NotMvdz`] or [`Error::UnsupportedVersion`] for another file type or version,
/// [`Error::Corrupt`] for damaged input, [`Error::MemoryLimit`] for a demo of more than 1 GiB:
/// read that one with [`Decoder`].
pub fn decode(file: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    decode_with_max_memory(file, max_threads, DEFAULT_MAX_MEMORY)
}

/// Each segment's CRC-32 is checked before its bytes are read; the first error is sticky.
#[derive(Debug)]
pub struct Decoder<R: Read> {
    input: R,
    layout: Layout,
    current: Option<Vec<u8>>,
    /// Into `current`.
    position: usize,
    ready: VecDeque<Vec<u8>>,
    /// Met after the segments in `ready`; reported once they are read.
    deferred_error: Option<Error>,
    is_done: bool,
    failure: Option<Error>,
    threads: usize,
    max_memory: u64,
    demo_crc32: crc32fast::Hasher,
}

impl<R: Read> Decoder<R> {
    /// Reads the header only; the segments are read by [`Read::read`].
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when reading fails, [`Error::NotMvdz`] or [`Error::UnsupportedVersion`] for
    /// another file type or version, [`Error::Corrupt`] for a truncated header.
    pub fn new(mut input: R) -> Result<Self> {
        let mut header = [0u8; HEADER_SIZE];
        let filled = read_up_to(&mut input, &mut header)?;
        let layout = Layout::new(&header[..filled])?;
        Ok(Self {
            input,
            layout,
            current: None,
            position: 0,
            ready: VecDeque::new(),
            deferred_error: None,
            is_done: false,
            failure: None,
            threads: thread_count(0),
            max_memory: DEFAULT_MAX_MEMORY,
            demo_crc32: crc32fast::Hasher::new(),
        })
    }

    /// Segments decoded at once; capped at the logical cores, 0, the default, is all of them.
    #[must_use]
    pub fn max_threads(mut self, max_threads: u8) -> Self {
        self.threads = thread_count(max_threads);
        self
    }

    /// In bytes, approximate: fewer segments are decoded at once to stay within it, never
    /// fewer than one (14 MiB). A file whose models need more is refused
    /// ([`Error::MemoryLimit`]).
    #[must_use]
    pub fn max_memory(mut self, max_memory: u64) -> Self {
        self.max_memory = max_memory;
        self
    }

    pub fn header(&self) -> &Header {
        self.layout.header()
    }

    pub(crate) fn read_demo(&mut self, buffer: &mut [u8]) -> Result<usize> {
        if self.position == self.current_bytes().len() {
            self.current = self.next_segment()?;
            self.position = 0;
        }
        let unread = &self.current_bytes()[self.position..];
        let len = buffer.len().min(unread.len());
        buffer[..len].copy_from_slice(&unread[..len]);
        self.position += len;
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
            return corrupt("incomplete file");
        };
        if len > MAX_MODELS_LEN {
            return corrupt("models too large");
        }
        let models = if len > 0 {
            let body = self.read_body(len)?;
            let max_models_memory = self.max_memory.max(BASE_MEMORY + SEGMENT_MEMORY);
            Some(read_models_block(&body, self.threads, max_models_memory)?)
        } else {
            None
        };
        self.layout.set_models(models);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        let mut trailing = [0u8; 1];
        if read_up_to(&mut self.input, &mut trailing)? != 0 {
            return corrupt("trailing bytes");
        }
        self.layout.finish()?;
        if self.demo_crc32.clone().finalize() != self.layout.header().crc32 {
            return corrupt("checksum mismatch");
        }
        self.is_done = true;
        Ok(())
    }

    /// `None` at the end of the input.
    fn read_len(&mut self) -> Result<Option<u64>> {
        let mut field = [0u8; LEN_FIELD_SIZE];
        match read_up_to(&mut self.input, &mut field)? {
            0 => Ok(None),
            LEN_FIELD_SIZE => Ok(Some(u64::from(u32::from_le_bytes(field)))),
            _ => corrupt("incomplete file"),
        }
    }

    fn read_body(&mut self, len: u64) -> Result<Vec<u8>> {
        // the data arrives before the buffer grows: a lying length on a
        // short stream allocates nothing
        let mut body = Vec::new();
        (&mut self.input).take(len).read_to_end(&mut body)?;
        if body.len() as u64 != len {
            return corrupt("incomplete file");
        }
        Ok(body)
    }

    fn read_segment(&mut self) -> Result<(SegmentHeader, Vec<u8>)> {
        // the stream may end here, short of the demo: `decode_batch` delivers the
        // complete segments before it first
        let Some(coded_len) = self.read_len()? else {
            return corrupt("incomplete file");
        };
        check_coded_len(coded_len)?;
        let body = self.read_body(coded_len + SEGMENT_FIELDS_SIZE as u64)?;
        let header = parse_segment_header(&body[..SEGMENT_FIELDS_SIZE])?;
        self.layout.add_segment(header)?;
        Ok((header, body))
    }

    /// ponytail: reading and decoding take turns and a batch waits for its
    /// slowest segment; a rolling window would overlap them.
    fn decode_batch(&mut self) -> Result<()> {
        let batch_len = segments_in_flight(
            Some(self.max_memory),
            self.threads,
            BASE_MEMORY,
            SEGMENT_MEMORY,
        );
        let mut segments = Vec::new();
        while segments.len() < batch_len && !self.layout.is_complete() {
            match self.read_segment() {
                Ok(segment) => segments.push(segment),
                Err(error) if !segments.is_empty() => {
                    self.deferred_error = Some(error);
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        let models = self.layout.models();
        let decoded = parallel_map(self.threads, &segments, |(header, body)| {
            let mut bytes = try_zeroed(header.raw_len as usize)?;
            decode_segment(models, body, *header, &mut bytes)?;
            Ok(bytes)
        });
        for ((header, _), result) in segments.iter().zip(decoded) {
            match result {
                Ok(segment) => {
                    // checked against the segment's bytes: the demo's CRC-32 follows
                    // from the segments' without hashing the bytes again
                    self.demo_crc32
                        .combine(&crc32fast::Hasher::new_with_initial_len(
                            header.crc32,
                            u64::from(header.raw_len),
                        ));
                    self.ready.push_back(segment);
                }
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

// ponytail: measured on sample demos, not a bound; count the buffers if it must hold
const BASE_MEMORY: u64 = 12 << 20;
const SEGMENT_MEMORY: u64 = 2 << 20;

/// Fills `buffer` until it is full or the input ends; returns the count.
fn read_up_to(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read.min(buffer.len() - filled),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

pub(crate) fn decode_segment(
    models: Option<&Models>,
    body: &[u8],
    header: SegmentHeader,
    destination: &mut [u8],
) -> Result<()> {
    if destination.len() as u64 != u64::from(header.raw_len) {
        return corrupt("segment length");
    }
    let coded = body
        .get(SEGMENT_FIELDS_SIZE..)
        .ok_or(Error::Corrupt("segment header"))?;
    let transform_header = segment::parse_transform_header(coded, destination.len())?;
    segment::decode_transform(coded, &transform_header, destination, models)?;
    if crc32fast::hash(destination) != header.crc32 {
        return corrupt("segment checksum");
    }
    Ok(())
}

pub(crate) fn decode_with_max_memory(
    file: &[u8],
    max_threads: u8,
    max_memory: u64,
) -> Result<Vec<u8>> {
    let mut decoder = Decoder::new(file)?
        .max_threads(max_threads)
        .max_memory(max_memory);
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
