//! The library interface.

use crate::budget::{Budget, Limits};
use crate::decode::{SeekSrc, scan_layout};
use crate::error::{Error, Result};
use crate::format::*;
use crate::stream::{Reader, Seq};
use std::io::{Read, Seek, Write};

pub use crate::encode::{Config, Info};
pub use crate::format::{Header, Index};

/// Compresses a demo: a raw `.mvd`, or a gzip-compressed one (`.mvd.gz`),
/// which is decompressed first. The file holds the raw demo: its identity
/// is the raw demo's SHA-256 and decompressing gives the raw `.mvd` back.
/// Input that is not an MVD demo is `Error::NotMvd` (see `check_demo`).
pub fn compress(src: &[u8], cfg: &Config) -> Result<Vec<u8>> {
    compress_info(src, cfg).map(|r| r.0)
}

/// Compresses a demo (as `compress`) and reports what the encoder did.
pub fn compress_info(src: &[u8], cfg: &Config) -> Result<(Vec<u8>, Info)> {
    let raw;
    let src = if is_gzip(src) {
        raw = gunzip(src, Limits::default().memory)?;
        &raw[..]
    } else {
        src
    };
    check_demo(src)?;
    crate::encode::compress(src, cfg)
}

/// Checks that `src` is an MVD demo, as the encoders do before any work:
/// its first frame must be whole, carry messages (not only a `dem_set`)
/// and parse without an escape (FORMAT.md §6.2). Real demos start with
/// the server data; a truncated or partly damaged demo passes as long as
/// its start is intact, and what does not parse later is stored raw as
/// always. Empty input, gzip data and other files fail with
/// `Error::NotMvd`.
pub fn check_demo(src: &[u8]) -> Result<()> {
    if src.is_empty() {
        return Err(Error::NotMvd("empty input"));
    }
    if is_gzip(src) {
        return Err(Error::NotMvd("gzip-compressed data"));
    }
    if !crate::encode::starts_like_mvd(src) {
        return Err(Error::NotMvd("the first frame does not parse"));
    }
    Ok(())
}

/// True if `b` starts with the MVDZ magic and a version this library
/// reads (needs 8 bytes).
pub fn is_mvdz(b: &[u8]) -> bool {
    b.len() >= 8 && &b[..4] == MAGIC && b[4..8] == VERSION.to_le_bytes()
}

/// True if `b` starts with the gzip magic (RFC 1952: `1f 8b`), as the
/// `.mvd.gz` files of most demo archives do.
pub fn is_gzip(b: &[u8]) -> bool {
    b.starts_with(&[0x1f, 0x8b])
}

/// Decompresses gzip data (every member, as `gzip -d` does), refusing
/// output over `max` bytes. A bad CRC, a truncated stream or anything
/// else gzip refuses is `Error::NotMvd`, never a partial result.
pub fn gunzip(b: &[u8], max: u64) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(b)
        .take(max.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|_| Error::NotMvd("corrupt gzip data (bad CRC, truncated or damaged)"))?;
    if out.len() as u64 > max {
        return Err(Error::Limit(
            "gzip data decompresses to more than the limit",
        ));
    }
    Ok(out)
}

/// Decompresses a whole file with up to `threads` threads and the default
/// limits.
pub fn decompress(b: &[u8], threads: usize) -> Result<Vec<u8>> {
    crate::decode::decompress(b, threads, &Limits::default())
}

/// Decompresses a whole file under the caller's limits.
pub fn decompress_with(b: &[u8], threads: usize, limits: &Limits) -> Result<Vec<u8>> {
    crate::decode::decompress(b, threads, limits)
}

/// Decompresses from `r` into `w` segment by segment (sequential reading;
/// nothing is buffered beyond one segment). On an error the bytes already
/// written are a prefix of the demo.
pub fn decompress_stream<R: Read, W: Write>(
    r: R,
    mut w: W,
    threads: usize,
    limits: &Limits,
) -> Result<u64> {
    let mut rd = Reader::new(r, threads, limits)?;
    let mut buf = vec![0u8; 1 << 16];
    let mut n = 0u64;
    loop {
        let k = rd.read_demo(&mut buf)?;
        if k == 0 {
            break;
        }
        w.write_all(&buf[..k])?;
        n += k as u64;
    }
    w.flush()?;
    Ok(n)
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
pub fn read_info<R: Read + Seek>(r: R) -> Result<FileInfo> {
    read_info_with(r, &Limits::default())
}

pub fn read_info_with<R: Read + Seek>(r: R, limits: &Limits) -> Result<FileInfo> {
    let budget = Budget::new(limits);
    let mut src = SeekSrc(r);
    let len = crate::decode::Src::len(&mut src)?;
    budget.input(len);
    let lay = scan_layout(&mut src, 1, &budget)?;
    Ok(FileInfo {
        header: lay.header,
        index: lay.index(),
        file_size: len,
    })
}

impl FileInfo {
    /// JSON rendering (see [`FileInfo::to_json_with`]), without a size
    /// limit.
    pub fn to_json(&self) -> String {
        self.to_json_with(usize::MAX).expect("no limit")
    }

    /// JSON rendering, refused with a limit error once it would exceed
    /// `max` bytes: one object with `version`, `file_size`, `size`,
    /// `duration_ms`, `crc32` and `sha256` (hexadecimal strings) and
    /// `segments`, a list of `{t_start_ms, raw_offset, raw_len}`.
    pub fn to_json_with(&self, max: usize) -> Result<String> {
        use std::fmt::Write as _;
        let over = Error::Limit("file information rendering");
        let h = &self.header;
        // numbers and hexadecimal strings only: nothing needs escaping
        let mut s = String::new();
        let _ = write!(
            s,
            "{{\"version\":{VERSION},\"file_size\":{},\"size\":{},\"duration_ms\":{},\"crc32\":\"{:08x}\",\"sha256\":\"{}\",\"segments\":[",
            self.file_size,
            h.size,
            h.duration_ms,
            h.crc,
            hex(&h.sha256)
        );
        for (i, g) in self.index.segs.iter().enumerate() {
            if s.len() > max {
                return Err(over);
            }
            let _ = write!(
                s,
                "{}{{\"t_start_ms\":{},\"raw_offset\":{},\"raw_len\":{}}}",
                if i > 0 { "," } else { "" },
                g.t_start,
                g.raw_off,
                g.raw_len
            );
        }
        s.push_str("]}");
        if s.len() > max {
            return Err(over);
        }
        Ok(s)
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(unix)]
fn gid(m: &std::fs::Metadata) -> Option<u32> {
    Some(std::os::unix::fs::MetadataExt::gid(m))
}

#[cfg(not(unix))]
fn gid(_: &std::fs::Metadata) -> Option<u32> {
    None
}

/// A temporary name nobody else uses: process, time, counter.
fn temp_name(name: &str, attempt: u32) -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(".{name}.{}.{t:x}.{n}.{attempt}.tmp", std::process::id())
}

/// Writes `data` to a new file next to `path` and renames it over `path`;
/// the new file gets permissions `perm` and (Unix, when allowed) group
/// `group`.
fn replace_locked(
    path: &std::path::Path,
    fill: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    perm: std::fs::Permissions,
    group: Option<u32>,
) -> Result<()> {
    #[cfg(not(unix))]
    let _ = group;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let name = path
        .file_name()
        .ok_or(Error::Invalid("path"))?
        .to_string_lossy()
        .into_owned();
    let mut attempt = 0;
    let (tmp, mut f) = loop {
        let tmp = dir.join(temp_name(&name, attempt));
        // create_new: never follows or reuses an existing path. Created
        // owner-only (0600 on Unix), then given the final permissions
        // (and group) before a byte is written: no other user can open it
        // while it holds data the original's permissions would not show
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        match o.open(&tmp) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 16 => attempt += 1,
            Err(e) => return Err(e.into()),
        }
    };
    let res = (|| -> std::io::Result<()> {
        #[cfg(unix)]
        if let Some(gid) = group {
            // best effort: only the owner's groups are allowed
            let _ = std::os::unix::fs::fchown(&f, None, Some(gid));
        }
        f.set_permissions(perm)?;
        fill(&mut f)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            d.sync_all()?;
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(res?)
}

/// Writes `data` to `path` atomically (temporary file in the same
/// directory, fsync, rename, fsync of the directory). New files get mode
/// 0644 subject to the umask; an existing file keeps its permissions.
pub fn write_file_atomic(path: &std::path::Path, data: &[u8]) -> Result<()> {
    write_file_atomic_with(path, |f| f.write_all(data))
}

/// As `write_file_atomic`, with the contents written by `fill` (e.g.
/// through an encoder, without building them in memory first).
pub fn write_file_atomic_with(
    path: &std::path::Path,
    fill: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<()> {
    let (perm, group) = match std::fs::metadata(path) {
        Ok(m) => (m.permissions(), gid(&m)),
        Err(_) => {
            let dir = path
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));
            let probe = dir.join(temp_name("probe", 0));
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&probe)?;
            let p = f.metadata()?.permissions();
            drop(f);
            let _ = std::fs::remove_file(&probe);
            (p, None)
        }
    };
    replace_locked(path, fill, perm, group)
}

/// Writes `data` gzip-compressed (level 6) to `w`, streamed. The gzip
/// bytes differ from other gzip tools' output; the demo inside is the
/// same.
pub fn write_gzip<W: Write>(data: &[u8], w: W) -> std::io::Result<W> {
    let mut e = flate2::write::GzEncoder::new(w, flate2::Compression::new(6));
    e.write_all(data)?;
    e.finish()
}

/// The original demo, read sequentially (any reader).
pub fn reader<R: Read>(r: R, threads: usize) -> Result<Reader<Seq<R>>> {
    Reader::new(r, threads, &Limits::default())
}
