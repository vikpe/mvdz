use crate::budget::DEFAULT_MAX_MEMORY;
use crate::error::{Error, Result};
use crate::util::thread_count;
use std::io::Write;

/// `bytes`: a raw `.mvd`; other bytes are `Error::NotMvd`.
/// `max_threads`: capped at the logical cores; 0 is one per logical core.
pub fn encode(bytes: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    check_demo(bytes)?;
    crate::encode::compress(bytes, thread_count(max_threads)).map(|(encoded, _)| encoded)
}

/// Collects the bytes written to it; `finish` encodes them and writes the result. The
/// encoder needs all the bytes, so nothing is written before `finish`, and dropping it
/// without `finish` writes nothing.
#[derive(Debug)]
pub struct Encoder<W: Write> {
    writer: W,
    max_threads: u8,
    bytes: Vec<u8>,
}

impl<W: Write> Encoder<W> {
    /// Encodes on all logical cores until `max_threads` says otherwise.
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            max_threads: 0,
            bytes: Vec::new(),
        }
    }

    /// Capped at the logical cores; 0 is all of them.
    #[must_use]
    pub fn max_threads(mut self, max_threads: u8) -> Self {
        self.max_threads = max_threads;
        self
    }

    /// Encodes the bytes, writes the result and flushes; returns the writer.
    pub fn finish(mut self) -> Result<W> {
        self.writer
            .write_all(&encode(&self.bytes, self.max_threads)?)?;
        self.writer.flush()?;
        Ok(self.writer)
    }
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    /// Writes nothing: `finish` writes the result.
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// `bytes`: what `encode` returned. `max_threads`: capped at the logical cores; 0 is one per
/// logical core.
pub fn decode(bytes: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    crate::decode::decompress(bytes, thread_count(max_threads), DEFAULT_MAX_MEMORY)
}

/// The first frame must be whole, carry messages (not only a `dem_set`) and parse without
/// an escape (FORMAT.md 6.2); a damaged demo with an intact start passes. A `.mvd.gz`
/// fails: decompress it first.
pub(crate) fn check_demo(demo: &[u8]) -> Result<()> {
    const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
    if demo.is_empty() {
        return Err(Error::NotMvd("empty input"));
    }
    if demo.starts_with(&GZIP_MAGIC) {
        return Err(Error::NotMvd("gzip-compressed data (decompress it first)"));
    }
    if !crate::encode::starts_like_mvd(demo) {
        return Err(Error::NotMvd("the first frame does not parse"));
    }
    Ok(())
}
