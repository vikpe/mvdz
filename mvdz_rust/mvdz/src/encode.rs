//! Encoding in two passes over the demo: count for the models, then code each segment and
//! check that it decodes to its input.

use crate::decode::decode_segment;
use crate::error::{Error, Result};
use crate::format::{
    Header, MAX_CODED_LEN, SEGMENT_FIELDS_SIZE, header_bytes, parse_segment_header,
    segment_header_bytes, write_models, write_segment,
};
use crate::memory::segments_in_flight;
use crate::parallel::{parallel_map, thread_count};
use crate::protocol::{
    DEM_ALL, DEM_MULTIPLE, DEM_READ, DEM_SINGLE, DEM_STATS, FrameRun, ProtocolState, frame_len,
    frame_run,
};
use crate::rans::{FileModel, Models, read_models_block};
use crate::segment::{self, SegmentReader, Walked};
use sha2::{Digest, Sha256};
use std::io::{Cursor, ErrorKind, Read, Seek, Write};

/// `max_threads` 0 is all logical cores.
///
/// # Errors
///
/// As [`Encoder::encode`].
pub fn encode(demo: &[u8], max_threads: u8) -> Result<Vec<u8>> {
    let mut file = Vec::new();
    Encoder::new()
        .max_threads(max_threads)
        .encode(Cursor::new(demo), &mut file)?;
    Ok(file)
}

/// The file written depends on the demo only, not on the threads or the memory.
#[derive(Debug, Clone)]
pub struct Encoder {
    threads: usize,
    max_memory: Option<u64>,
    is_input_checked: bool,
}

impl Default for Encoder {
    fn default() -> Self {
        Self {
            threads: thread_count(0),
            max_memory: None,
            is_input_checked: true,
        }
    }
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Segments coded at once; capped at the logical cores, 0, the default, is all of them.
    #[must_use]
    pub fn max_threads(mut self, max_threads: u8) -> Self {
        self.threads = thread_count(max_threads);
        self
    }

    /// In bytes, approximate: fewer segments are coded at once to stay within it, never
    /// fewer than one (32 MiB). No setting: a segment a thread.
    #[must_use]
    pub fn max_memory(mut self, max_memory: u64) -> Self {
        self.max_memory = Some(max_memory);
        self
    }

    /// Reads `input` twice.
    ///
    /// # Errors
    ///
    /// [`Error::NotMvd`] for bytes that are not an MVD demo, [`Error::DemoTooLarge`] past 65536
    /// segments, [`Error::Io`] when reading or writing fails or the input changes between the
    /// passes, [`Error::Internal`] when a segment fails its self-check (a bug).
    pub fn encode(&self, mut input: impl Read + Seek, mut output: impl Write) -> Result<()> {
        let first_pass = self.first_pass(&mut input)?;
        let mut start = header_bytes(&first_pass.header).to_vec();
        let models_body = segment::models_body(&first_pass.file_models);
        write_models(&mut start, &models_body);
        let decoder_models = if models_body.is_empty() {
            None
        } else {
            let models = read_models_block(&models_body, self.threads, u64::MAX)
                .map_err(|_| Error::Internal("the encoder's models do not read back"))?;
            Some(models)
        };
        output.write_all(&start)?;
        input.rewind()?;
        self.second_pass(
            &first_pass,
            decoder_models.as_ref(),
            &mut input,
            &mut output,
        )?;
        Ok(output.flush()?)
    }

    /// FORMAT.md 8 rule 3: a batch is walked in parallel from the end protocol of the batch
    /// before; the few segments that start in another protocol are walked again.
    fn first_pass(&self, input: &mut impl Read) -> Result<FirstPass> {
        /// An encoder rule, not a reader rule.
        const MAX_SEGMENT_COUNT: usize = 1 << 16;
        let mut segments = SegmentReader::new(input);
        let mut model_builder = segment::ModelBuilder::default();
        let mut protocol = ProtocolState::default();
        let mut plans: Vec<SegmentPlan> = Vec::new();
        let mut header = Header::default();
        let mut crc32 = crc32fast::Hasher::new();
        let mut sha256 = Sha256::new();
        loop {
            // the first segment alone: it sets the protocol of most demos
            let batch_len = if plans.is_empty() {
                1
            } else {
                self.segments_in_flight()
            };
            let mut batch = Vec::with_capacity(batch_len);
            while batch.len() < batch_len
                && let Some(raw) = segments.next_segment()?
            {
                batch.push(raw);
            }
            if plans.is_empty() && self.is_input_checked {
                check_first_segment(batch.first().map_or(&[], Vec::as_slice))?;
            }
            if batch.is_empty() {
                break;
            }
            let batch_protocol = protocol;
            let mut walks = parallel_map(self.threads, &batch, |raw| {
                SegmentWalk::new(raw, batch_protocol)
            });
            for (raw, walk) in batch.iter().zip(walks.iter_mut()) {
                if walk.start_protocol != protocol {
                    *walk = SegmentWalk::new(raw, protocol);
                }
                protocol = walk.end_protocol;
                let segment_crc32 = crc32fast::hash(raw);
                crc32.combine(&crc32fast::Hasher::new_with_initial_len(
                    segment_crc32,
                    raw.len() as u64,
                ));
                sha256.update(raw);
                header.size += raw.len() as u64;
                header.duration_ms += u64::from(segment::duration_ms(raw));
                plans.push(SegmentPlan {
                    len: raw.len(),
                    crc32: segment_crc32,
                    start_protocol: walk.start_protocol,
                    is_walked: walk.is_walked,
                });
            }
            if plans.len() > MAX_SEGMENT_COUNT {
                return Err(Error::DemoTooLarge);
            }
            let walked: Vec<&Walked> = walks.iter().map(|walk| &walk.walked).collect();
            model_builder.add(&walked, self.threads);
        }
        header.crc32 = crc32.finalize();
        header.sha256 = sha256.finalize().into();
        Ok(FirstPass {
            header,
            file_models: model_builder.models(self.threads),
            plans,
        })
    }

    /// The segments as the first pass cut them, each written once it decodes to its input.
    fn second_pass(
        &self,
        first_pass: &FirstPass,
        decoder_models: Option<&Models>,
        input: &mut impl Read,
        output: &mut impl Write,
    ) -> Result<()> {
        for plans in first_pass.plans.chunks(self.segments_in_flight()) {
            let mut batch = Vec::with_capacity(plans.len());
            for plan in plans {
                let mut raw = vec![0u8; plan.len];
                input.read_exact(&mut raw)?;
                batch.push((raw, plan));
            }
            let bodies = parallel_map(self.threads, &batch, |(raw, plan)| {
                encode_segment(raw, plan, &first_pass.file_models, decoder_models)
            });
            for body in bodies {
                let mut block = Vec::new();
                write_segment(&mut block, &body?);
                output.write_all(&block)?;
            }
        }
        Ok(())
    }

    fn segments_in_flight(&self) -> usize {
        // ponytail: measured on sample demos, not a bound; count the buffers if it must hold
        const BASE_MEMORY: u64 = 28 << 20;
        const SEGMENT_MEMORY: u64 = 4 << 20;
        segments_in_flight(self.max_memory, self.threads, BASE_MEMORY, SEGMENT_MEMORY)
    }
}

/// Threads never change the file written. Skips the input check.
#[cfg(any(test, feature = "fuzz"))]
pub(crate) fn encode_any(demo: &[u8], threads: usize) -> Result<Vec<u8>> {
    let encoder = Encoder {
        threads,
        max_memory: None,
        is_input_checked: false,
    };
    let mut file = Vec::new();
    encoder.encode(Cursor::new(demo), &mut file)?;
    Ok(file)
}

/// FORMAT.md 8 rule 1, of a demo held whole.
#[cfg(any(test, feature = "fuzz"))]
pub(crate) fn check_demo(demo: &[u8]) -> Result<()> {
    let first_segment_len = segment::cut_points(demo).get(1).copied().unwrap_or(0);
    check_first_segment(&demo[..first_segment_len])
}

/// FORMAT.md 8 rule 1: a damaged demo with an intact start passes.
fn check_first_segment(first_segment: &[u8]) -> Result<()> {
    const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
    if first_segment.is_empty() {
        return Err(Error::NotMvd("empty input"));
    }
    if first_segment.starts_with(&GZIP_MAGIC) {
        return Err(Error::NotMvd("gzip-compressed data (decompress it first)"));
    }
    if !starts_like_mvd(first_segment) {
        return Err(Error::NotMvd("the first frame does not parse"));
    }
    Ok(())
}

fn starts_like_mvd(bytes: &[u8]) -> bool {
    if bytes.len() < 2 {
        return false;
    }
    let header_size = match bytes[1] & 7 {
        DEM_MULTIPLE => 10,
        DEM_READ | DEM_SINGLE | DEM_STATS | DEM_ALL => 6,
        _ => return false,
    };
    let Some(first_len) = frame_len(bytes, 0) else {
        return false;
    };
    if first_len <= header_size {
        return false;
    }
    if segment::walk(&bytes[..first_len], ProtocolState::default())
        .is_some_and(|(walked, _)| !walked.has_escape())
    {
        return true;
    }
    frame_run(bytes, first_len) == FrameRun::Yes
}

#[derive(Debug)]
struct FirstPass {
    header: Header,
    file_models: Vec<Option<FileModel>>,
    plans: Vec<SegmentPlan>,
}

/// What pass 2 needs to code one segment, and to see that it reads the bytes pass 1 read.
#[derive(Debug)]
struct SegmentPlan {
    len: usize,
    crc32: u32,
    start_protocol: ProtocolState,
    is_walked: bool,
}

/// A pass 1 walk of one segment.
struct SegmentWalk {
    start_protocol: ProtocolState,
    end_protocol: ProtocolState,
    is_walked: bool,
    /// All tail when the segment does not walk.
    walked: Walked,
}

impl SegmentWalk {
    fn new(raw: &[u8], start_protocol: ProtocolState) -> Self {
        let (walked, end_protocol, is_walked) = match segment::walk(raw, start_protocol) {
            Some((walked, end_protocol)) => (walked, end_protocol, true),
            None => (
                segment::walk_tail(raw, start_protocol),
                start_protocol,
                false,
            ),
        };
        Self {
            start_protocol,
            end_protocol,
            is_walked,
            walked,
        }
    }
}

/// The segment fields and the coded segment. FORMAT.md 8 rule 5: transform-coded, else
/// all tail, else stored; rule 7: it decodes to `raw`.
fn encode_segment(
    raw: &[u8],
    plan: &SegmentPlan,
    file_models: &[Option<FileModel>],
    decoder_models: Option<&Models>,
) -> Result<Vec<u8>> {
    let is_input_unchanged = crc32fast::hash(raw) == plan.crc32;
    if !is_input_unchanged {
        return Err(Error::Io(ErrorKind::InvalidData));
    }
    let coded_with_models = |walked: &Walked| {
        segment::encode_transform(walked, file_models)
            .filter(|coded| coded.len() as u64 <= MAX_CODED_LEN)
    };
    let transform_coded = (plan.is_walked)
        .then(|| segment::walk(raw, plan.start_protocol))
        .flatten()
        .and_then(|(walked, _)| coded_with_models(&walked));
    let coded = transform_coded
        .or_else(|| coded_with_models(&segment::walk_tail(raw, plan.start_protocol)))
        .unwrap_or_else(|| segment::stored(raw, plan.start_protocol));
    let mut body = segment_header_bytes(raw.len() as u32, segment::duration_ms(raw), plan.crc32);
    body.extend_from_slice(&coded);
    if !decodes_to(&body, raw, decoder_models) {
        return Err(Error::Internal(
            "the encoder's output does not decode to its input",
        ));
    }
    Ok(body)
}

fn decodes_to(body: &[u8], raw: &[u8], decoder_models: Option<&Models>) -> bool {
    let Ok(header) = parse_segment_header(&body[..SEGMENT_FIELDS_SIZE]) else {
        return false;
    };
    let mut decoded = vec![0u8; raw.len()];
    decode_segment(decoder_models, body, header, &mut decoded).is_ok() && decoded == raw
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without models for the blob tokens only the stored segment is left.
    #[test]
    fn segment_no_model_codes_is_stored() -> anyhow::Result<()> {
        let demo = b"not a demo: neither walked nor coded as tail";
        let plan = SegmentPlan {
            len: demo.len(),
            crc32: crc32fast::hash(demo),
            start_protocol: ProtocolState::default(),
            is_walked: false,
        };
        let body = encode_segment(demo, &plan, &[], None)?;
        let stored_len = demo.len() + 16;
        assert!(body.ends_with(demo) && body.len() == SEGMENT_FIELDS_SIZE + stored_len);
        Ok(())
    }
}
