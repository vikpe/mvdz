//! Decoding: the layout scan every random-access reader runs, the decode
//! of one segment, and whole-file decompression.

use crate::budget::{Budget, zeroed};
use crate::coder::rans::ModelsBlock;
use crate::error::{Error, Result, bad};
use crate::format::{
    HEADER_LEN, LEN_FIELD_LEN, Layout, MAX_MODELS_LEN, MAX_SEGMENT_RAW, SEGMENT_FIELDS_LEN,
    SegmentHeader, SegmentSpan, check_coded_len, len_field, parse_segment_header,
};
use crate::segment::{self, FrameScan, frame_scan};
use crate::util::{Crc, crc32, par_map};

/// `None`: `file` ends first.
fn bytes_at(file: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    file.get(start..start.checked_add(usize::try_from(len).ok()?)?)
}

/// `None`: `file` ends first.
fn len_at(file: &[u8], offset: u64) -> Option<u64> {
    len_field(bytes_at(file, offset, LEN_FIELD_LEN as u64)?)
}

/// Header, models and every segment header (FORMAT.md, "Validation"); segment payloads
/// are not decoded.
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
        layout.segment(&span, parse_segment_header(fields)?)?;
        offset = span.end();
    }
    if offset != file_len {
        return bad("trailing bytes");
    }
    layout.finish()?;
    Ok(layout)
}

/// Frames, frame time and whole-frame length of one decoded segment.
/// `dst`: exactly the segment's raw length.
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
        .get(header.body_offset..)
        .ok_or(Error::Corrupt("segment header"))?;
    let transform_header = segment::parse_transform_header(coded, dst.len())?;
    segment::decode(coded, &transform_header, dst, models_block, budget)?;
    if crc32(dst) != header.crc32 {
        return bad("segment checksum");
    }
    let scan = frame_scan(dst);
    if scan.duration_ms != u64::from(header.duration_ms) {
        return bad("segment duration");
    }
    Ok(scan)
}

/// Over every segment, in order. The header's SHA-256 names the demo; decoding does not
/// recompute it.
#[derive(Debug, Default)]
pub(crate) struct DemoCheck {
    crc: Crc,
    size_bytes: u64,
    duration_ms: u64,
    is_inside_frame: bool,
}

impl DemoCheck {
    pub(crate) fn segment(&mut self, out: &[u8], scan: FrameScan) -> Result<()> {
        if self.is_inside_frame {
            return bad("segment ends inside a frame");
        }
        self.crc.update(out);
        self.size_bytes += out.len() as u64;
        self.duration_ms += scan.duration_ms;
        let has_tail = scan.framed_len != out.len();
        let is_cut_at_max = out.len() as u64 == MAX_SEGMENT_RAW;
        self.is_inside_frame = has_tail && !is_cut_at_max;
        Ok(())
    }
    /// After the last segment.
    pub(crate) fn finish(self, layout: &Layout) -> Result<()> {
        let header = &layout.header;
        if self.crc.finish() != header.crc32 || self.size_bytes != header.size_bytes {
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
    let mut out = zeroed(usize::try_from(size).map_err(|_| Error::Limit("demo size"))?)?;
    let segment_count = layout.segments.len();
    let mut slices: Vec<std::sync::Mutex<&mut [u8]>> = Vec::with_capacity(segment_count);
    let mut rest: &mut [u8] = &mut out;
    for (_, header) in &layout.segments {
        let (slice, after) = rest.split_at_mut(header.raw_len as usize);
        slices.push(std::sync::Mutex::new(slice));
        rest = after;
    }
    let results = par_map(threads, segment_count, |index| {
        let (span, header) = &layout.segments[index];
        let body = span
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
        let scan = result?;
        let len = header.raw_len as usize;
        check.segment(&out[at..at + len], scan)?;
        at += len;
    }
    check.finish(&layout)?;
    Ok(out)
}
