//! The library interface.

use crate::budget::{Budget, Limits};
use crate::decode::{SeekSrc, scan_layout};
use crate::error::{Error, Result};
use crate::format::{MAGIC, VERSION};
use crate::stream::Reader;
use std::io::{Read, Seek, Write};

pub use crate::encode::{Config, Info};
use crate::format::{Header, Index};

/// Compresses a demo (a raw `.mvd`). Input that is not an MVD demo is
/// `Error::NotMvd` (see `check_demo`).
pub fn compress(src: &[u8], config: &Config) -> Result<Vec<u8>> {
    compress_info(src, config).map(|(packed, _)| packed)
}

/// Compresses a demo (as `compress`) and reports what the encoder did.
pub fn compress_info(src: &[u8], config: &Config) -> Result<(Vec<u8>, Info)> {
    check_demo(src)?;
    crate::encode::compress(src, config)
}

/// As `compress_info`, for any bytes: input that is not an MVD demo is stored
/// (mostly as tail) and still round-trips.
pub fn compress_any(src: &[u8], config: &Config) -> Result<(Vec<u8>, Info)> {
    crate::encode::compress(src, config)
}

/// Checks that `src` is an MVD demo, as the encoders do before any work.
///
/// Its first frame must be whole, carry messages (not only a `dem_set`)
/// and parse without an escape (FORMAT.md §6.2). Real demos start with
/// the server data; a truncated or partly damaged demo passes as long as
/// its start is intact, and what does not parse later is stored raw as
/// always. Empty input and other files fail with `Error::NotMvd`; so does
/// a gzip-compressed demo (`.mvd.gz`), which is to be decompressed first.
pub fn check_demo(src: &[u8]) -> Result<()> {
    if src.is_empty() {
        return Err(Error::NotMvd("empty input"));
    }
    // the gzip magic (RFC 1952), for a clear message: most archives hand
    // out .mvd.gz
    if src.starts_with(&[0x1f, 0x8b]) {
        return Err(Error::NotMvd("gzip-compressed data (decompress it first)"));
    }
    if !crate::encode::starts_like_mvd(src) {
        return Err(Error::NotMvd("the first frame does not parse"));
    }
    Ok(())
}

/// True if `bytes` starts with the MVDZ magic and a version this library
/// reads (needs 8 bytes).
pub fn is_mvdz(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && &bytes[..4] == MAGIC && bytes[4..8] == VERSION.to_le_bytes()
}

/// Decompresses a whole file with up to `threads` threads (0: one per
/// logical core) and the default limits.
pub fn decompress(file: &[u8], threads: usize) -> Result<Vec<u8>> {
    crate::decode::decompress(file, threads, &Limits::default())
}

/// Decompresses a whole file under the caller's limits.
pub fn decompress_with(file: &[u8], threads: usize, limits: &Limits) -> Result<Vec<u8>> {
    crate::decode::decompress(file, threads, limits)
}

/// Decompresses from `reader` into `writer` segment by segment.
///
/// The file is read in order; up to `threads` segments are decoded at once
/// and nothing more is buffered. On an error the bytes already written are
/// a prefix of the demo.
pub fn decompress_stream<R: Read, W: Write>(
    reader: R,
    mut writer: W,
    threads: usize,
    limits: &Limits,
) -> Result<u64> {
    let mut decoder = Reader::new(reader, threads, limits)?;
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let len = decoder.read_demo(&mut buf)?;
        if len == 0 {
            break;
        }
        writer.write_all(&buf[..len])?;
        total += len as u64;
    }
    writer.flush()?;
    Ok(total)
}

// ---------------------------------------------------------------- file information

/// What a file says about itself, read without decoding the demo: the
/// header and the segments.
#[derive(Clone, Debug, Default)]
pub struct FileInfo {
    pub header: Header,
    pub index: Index,
    /// compressed file size
    pub file_size: u64,
}

/// Reads the header of a complete file and the header of every segment,
/// after checking its structure (no segment is decoded). The hashes are
/// what the file claims: only a full decode verifies them.
pub fn read_info<R: Read + Seek>(reader: R) -> Result<FileInfo> {
    read_info_with(reader, &Limits::default())
}

pub fn read_info_with<R: Read + Seek>(reader: R, limits: &Limits) -> Result<FileInfo> {
    let budget = Budget::new(limits);
    let mut src = SeekSrc(reader);
    let len = crate::decode::Src::len(&mut src)?;
    budget.input(len);
    let layout = scan_layout(&mut src, 1, &budget)?;
    Ok(FileInfo {
        header: layout.header,
        index: layout.index(),
        file_size: len,
    })
}
