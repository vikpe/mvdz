//! Decoding: the layout scan every random-access reader runs, the decode
//! of one segment, and whole-file decompression.

use crate::budget::{Budget, Charge, Limits, zeroed};
use crate::coder::rans::FileTables;
use crate::error::{Error, Result, bad};
use crate::format::{
    HEADER_LEN, LEN, Layout, MAX_MODELS_LEN, MAX_SEGMENT_RAW, SEGMENT_FIELDS_LEN, Section,
    SegmentHeader, check_coded_len, len_field, parse_segment_header,
};
use crate::segment::{self, frame_scan};
use crate::util::{Crc, crc32, par_map, to_usize};
use std::io::{Read, Seek, SeekFrom};

/// Random access to a file's bytes.
pub(crate) trait Src {
    fn len(&mut self) -> Result<u64>;
    /// Fills `buf` from file offset `offset` (exactly).
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()>;
}

/// A file in memory.
pub(crate) struct SliceSrc<'a>(pub(crate) &'a [u8]);

impl Src for SliceSrc<'_> {
    fn len(&mut self) -> Result<u64> {
        Ok(self.0.len() as u64)
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let bytes = to_usize(offset)
            .and_then(|start| self.0.get(start..start.checked_add(buf.len())?))
            .ok_or(Error::Corrupt("truncated"))?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

/// A file behind `Read + Seek`.
pub(crate) struct SeekSrc<R>(pub(crate) R);

impl<R: Read + Seek> Src for SeekSrc<R> {
    fn len(&mut self) -> Result<u64> {
        Ok(self.0.seek(SeekFrom::End(0))?)
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        if self.0.seek(SeekFrom::Start(offset))? != offset {
            return Err(Error::Io(std::io::ErrorKind::InvalidData));
        }
        read_full(&mut self.0, buf)
    }
}

/// `read_exact` that reports a short read as a truncated file.
pub(crate) fn read_full<R: Read + ?Sized>(reader: &mut R, buf: &mut [u8]) -> Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return bad("truncated"),
            Ok(read) if read <= buf.len() - filled => filled += read,
            Ok(_) => return Err(Error::Io(std::io::ErrorKind::InvalidData)),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Reads `len` bytes at `offset` into a buffer charged to `budget` (for as
/// long as the charge lives).
fn read_vec(
    src: &mut impl Src,
    offset: u64,
    len: u64,
    budget: &Budget,
) -> Result<(Vec<u8>, Charge)> {
    let charge = budget.alloc(len)?;
    let mut buffer = zeroed(to_usize(len).ok_or(Error::Limit("section size"))?)?;
    src.read_at(offset, &mut buffer)?;
    Ok((buffer, charge))
}

/// The length field at file offset `offset` of a file of `file_len` bytes
/// (None: the file ends first).
fn len_at(src: &mut impl Src, offset: u64, file_len: u64) -> Result<Option<u64>> {
    if file_len - offset < LEN as u64 {
        return Ok(None);
    }
    let mut field = [0u8; LEN];
    src.read_at(offset, &mut field)?;
    Ok(len_field(&field))
}

/// Reads and checks the whole structure of a complete file.
///
/// That is the header, the models and the header of every segment
/// (FORMAT.md, "Validation").
/// Segment payloads are not decoded. The models are built on up to
/// `threads` threads.
pub(crate) fn scan_layout(src: &mut impl Src, threads: usize, budget: &Budget) -> Result<Layout> {
    let file_len = src.len()?;
    let mut header = [0u8; HEADER_LEN];
    let header_len = to_usize(file_len).map_or(HEADER_LEN, |len| len.min(HEADER_LEN));
    src.read_at(0, &mut header[..header_len])?;
    let mut layout = Layout::new(&header[..header_len], budget)?;
    let mut offset = HEADER_LEN as u64;
    // the models
    let Some(models_len) = len_at(src, offset, file_len)? else {
        return bad("incomplete file");
    };
    offset += LEN as u64;
    if models_len > MAX_MODELS_LEN {
        return bad("models too large");
    }
    if models_len > file_len - offset {
        return bad("incomplete file");
    }
    if models_len > 0 {
        let (body, _charge) = read_vec(src, offset, models_len, budget)?;
        layout.models(Some(FileTables::new(&body, threads, budget)?))?;
    } else {
        layout.models(None)?;
    }
    offset += models_len;
    // the segments, until they hold the whole demo
    while !layout.is_complete() {
        let Some(coded) = len_at(src, offset, file_len)? else {
            return bad("incomplete file");
        };
        check_coded_len(coded)?;
        let section = Section {
            offset,
            len: coded + SEGMENT_FIELDS_LEN as u64,
        };
        if section.len > file_len - section.body_offset() {
            return bad("incomplete file");
        }
        let mut fields = [0u8; SEGMENT_FIELDS_LEN];
        src.read_at(section.body_offset(), &mut fields)?;
        layout.segment(&section, parse_segment_header(&fields)?)?;
        offset = section.end();
    }
    if offset != file_len {
        return bad("trailing bytes");
    }
    layout.finish()?;
    Ok(layout)
}

/// Frames, frame time and whole-frame length of one decoded segment.
pub(crate) type SegmentTiming = (u64, u64, usize);

/// Decodes one segment body into `dst` (its exact raw length) and checks
/// its CRC and its frame time.
pub(crate) fn decode_segment(
    layout: &Layout,
    body: &[u8],
    header: &SegmentHeader,
    dst: &mut [u8],
    budget: &Budget,
) -> Result<SegmentTiming> {
    let tables = layout.tables.as_ref();
    if dst.len() as u64 != u64::from(header.raw_len) {
        return bad("segment length");
    }
    let coded = body
        .get(header.body_offset..)
        .ok_or(Error::Corrupt("segment header"))?;
    let transform_header = segment::parse_header(coded, dst.len())?;
    segment::decode(coded, &transform_header, dst, tables, budget)?;
    if crc32(dst) != header.crc {
        return bad("segment checksum");
    }
    let timing = frame_scan(dst);
    if timing.1 != u64::from(header.duration_ms) {
        return bad("segment duration");
    }
    Ok(timing)
}

/// Whole-demo checks over the segments' timings (all of them, in order).
/// The header's SHA-256 names the demo; decoding does not recompute it.
#[derive(Default)]
pub(crate) struct DemoCheck {
    crc: Crc,
    size: u64,
    duration_ms: u64,
    is_inside_frame: bool,
}

impl DemoCheck {
    /// The next segment, decoded.
    pub(crate) fn segment(&mut self, out: &[u8], timing: SegmentTiming) -> Result<()> {
        if self.is_inside_frame {
            return bad("segment ends inside a frame");
        }
        self.crc.update(out);
        self.size += out.len() as u64;
        self.duration_ms += timing.1;
        let has_tail = timing.2 != out.len();
        let is_cut_at_max = out.len() as u64 == MAX_SEGMENT_RAW;
        self.is_inside_frame = has_tail && !is_cut_at_max;
        Ok(())
    }
    /// After the last segment: the header's size, duration and CRC-32.
    pub(crate) fn finish(self, layout: &Layout) -> Result<()> {
        let header = &layout.header;
        if self.crc.finish() != header.crc || self.size != header.size {
            return bad("checksum mismatch");
        }
        if self.duration_ms != header.duration_ms {
            return bad("demo timing");
        }
        Ok(())
    }
}

/// Decompresses a whole file in memory.
pub(crate) fn decompress(file: &[u8], threads: usize, limits: &Limits) -> Result<Vec<u8>> {
    let budget = Budget::new(limits);
    budget.input(file.len() as u64);
    let threads = crate::util::threads(threads);
    let layout = scan_layout(&mut SliceSrc(file), threads, &budget)?;
    let size = layout.header.size;
    let _charge = budget.alloc(size)?;
    budget.work(size)?;
    let mut out = zeroed(to_usize(size).ok_or(Error::Limit("demo size"))?)?;
    let segment_count = layout.segments.len();
    let mut slices: Vec<std::sync::Mutex<&mut [u8]>> = Vec::with_capacity(segment_count);
    let mut rest: &mut [u8] = &mut out;
    for (_, header) in &layout.segments {
        let (slice, after) = rest.split_at_mut(header.raw_len as usize);
        slices.push(std::sync::Mutex::new(slice));
        rest = after;
    }
    let results = par_map(threads, segment_count, |index| {
        let (section, header) = &layout.segments[index];
        let body = section
            .body_range()
            .and_then(|range| file.get(range))
            .ok_or(Error::Corrupt("truncated"))?;
        let mut dst = slices[index].lock().map_err(|_| Error::Internal("lock"))?;
        decode_segment(&layout, body, header, &mut dst, &budget)
    });
    drop(slices);
    let mut check = DemoCheck::default();
    let mut at = 0usize;
    for (result, (_, header)) in results.into_iter().zip(&layout.segments) {
        let timing = result?;
        let len = header.raw_len as usize;
        check.segment(&out[at..at + len], timing)?;
        at += len;
    }
    check.finish(&layout)?;
    Ok(out)
}
