//! Measurements for deciding which commands earn a layout of their own.
//!
//! Bytes per command, coded bytes per stream, and the file size with a layout or a model
//! taken away. Not part of the codec: an ablated walk does not decode.

#![expect(
    clippy::cast_precision_loss,
    reason = "byte counts as shares and ratios: far under 2^52"
)]

use super::{EncodeIo, ModelBuilder, Walked, cut_points, encode, stored, walk_tail};
use crate::fields::{read_u16, read_usize};
use crate::format::{HEADER_LEN, LEN_FIELD_LEN, MAX_SEGMENT_CODED, SEGMENT_FIELDS_LEN};
use crate::protocol::{
    DEM_MULTIPLE, DEM_SET, ProtocolState, SVC_DELTA_PACKET_ENTITIES, SVC_NAILS, SVC_NAILS_2,
    SVC_PACKET_ENTITIES, SVC_PLAYER_INFO, SVC_SERVER_DATA, SVC_SOUND, SVC_TEMP_ENTITY,
    SVC_UPDATE_STAT, frame_len,
};
use crate::rans::{BitReader, FileModel, StreamSymbols};
use crate::streams::{
    COMMAND, ENTITY_ANGLE, ENTITY_BITS, ENTITY_FLOAT_ORIGIN, ENTITY_FRAME, ENTITY_GAP,
    ENTITY_HEADER, ENTITY_MISC, ENTITY_MODEL, ENTITY_MORE, ENTITY_ORIGIN, ESCAPE_LENGTH, GENERIC,
    HIDDEN_LENGTH, HIDDEN_MISC, HIDDEN_TAG, HIDDEN_TYPE, LZ_DISTANCE, LZ_LENGTH, LZ_LITERAL, MASK,
    NAILS, PLAYER_ANGLE, PLAYER_EFFECTS, PLAYER_FLAGS, PLAYER_FLOAT_ORIGIN, PLAYER_FRAME,
    PLAYER_MODEL, PLAYER_NUMBER, PLAYER_ORIGIN, PLAYER_SKIN, PLAYER_WEAPON_FRAME, RAW, SET,
    SOUND_CHANNEL, SOUND_FLOAT, SOUND_MISC, SOUND_NUMBER, SOUND_ORIGIN, STAT_INDEX, STAT_VALUE,
    STREAM_COUNT, TAIL, TEMP_ENTITY_FLOAT, TEMP_ENTITY_MISC, TEMP_ENTITY_ORIGIN, TEMP_ENTITY_TYPE,
    TIME, TYPE, USERCMD, USERCMD_ANGLE, is_blob,
};
use crate::walker::{SegmentWalker, SymbolIo};
use std::fmt::Write;

/// Sums over demos; one per thread, merged at the end.
#[derive(Debug, Clone)]
pub struct Stats {
    demos: u64,
    raw_bytes: u64,
    coded_bytes: u64,
    census: Vec<CensusRow>,
    streams: Vec<StreamRow>,
    /// The file overhead no stream owns: header, length fields, segment fields.
    container_bytes: f64,
    ablated_demos: u64,
    ablated_raw_bytes: u64,
    ablated_coded_bytes: u64,
    /// By variant: the bytes its files grow by.
    growth: Vec<i64>,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            demos: 0,
            raw_bytes: 0,
            coded_bytes: 0,
            census: vec![CensusRow::default(); CENSUS_ROWS],
            streams: vec![StreamRow::default(); STREAM_COUNT],
            container_bytes: 0.0,
            ablated_demos: 0,
            ablated_raw_bytes: 0,
            ablated_coded_bytes: 0,
            growth: vec![0; VARIANTS.len()],
        }
    }
}

impl Stats {
    /// `should_ablate`: also code the demo once per variant (about 40 times the work).
    ///
    /// # Errors
    ///
    /// [`crate::Error::NotMvd`] for bytes the encoder refuses.
    pub fn of_demo(demo: &[u8], should_ablate: bool) -> crate::Result<Self> {
        crate::encode::check_demo(demo)?;
        let mut stats = Self::default();
        stats.add_demo(demo, should_ablate);
        Ok(stats)
    }

    fn add_demo(&mut self, demo: &[u8], should_ablate: bool) {
        let mut census = vec![CensusRow::default(); CENSUS_ROWS];
        let segments = walk_demo(demo, Ablation::None, Some(&mut census));
        for (total, row) in self.census.iter_mut().zip(&census) {
            total.demos += u64::from(row.count > 0);
            total.count += row.count;
            total.bytes += row.bytes;
        }
        let coded = code(&segments);
        self.demos += 1;
        self.raw_bytes += demo.len() as u64;
        self.coded_bytes += coded.len;
        self.attribute(&segments, &coded);
        if !should_ablate {
            return;
        }
        self.ablated_demos += 1;
        self.ablated_raw_bytes += demo.len() as u64;
        self.ablated_coded_bytes += coded.len;
        for (growth, &(_, ablation)) in self.growth.iter_mut().zip(VARIANTS) {
            let ablated = code(&walk_demo(demo, ablation, None));
            *growth += ablated.len.cast_signed() - coded.len.cast_signed();
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.demos += other.demos;
        self.raw_bytes += other.raw_bytes;
        self.coded_bytes += other.coded_bytes;
        for (total, row) in self.census.iter_mut().zip(&other.census) {
            total.demos += row.demos;
            total.count += row.count;
            total.bytes += row.bytes;
        }
        for (total, row) in self.streams.iter_mut().zip(&other.streams) {
            total.demos += row.demos;
            total.symbols += row.symbols;
            total.coded_bytes += row.coded_bytes;
        }
        self.container_bytes += other.container_bytes;
        self.ablated_demos += other.ablated_demos;
        self.ablated_raw_bytes += other.ablated_raw_bytes;
        self.ablated_coded_bytes += other.ablated_coded_bytes;
        for (total, growth) in self.growth.iter_mut().zip(&other.growth) {
            *total += growth;
        }
    }

    pub fn demos(&self) -> u64 {
        self.demos
    }

    /// Tab-separated tables.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let raw = self.raw_bytes as f64;
        let coded = self.coded_bytes as f64;
        let _ = writeln!(
            out,
            "demos\t{}\nraw bytes\t{}\nmvdz bytes\t{}\nratio\t{:.4}\n",
            self.demos,
            self.raw_bytes,
            self.coded_bytes,
            coded / raw
        );

        let messages: u64 = self.census[..256].iter().map(|row| row.count).sum();
        let _ = writeln!(
            out,
            "# census: what the demos hold\nrow\tdemos\tcount\tcount %\tbytes\tbytes %\tbytes/count"
        );
        let mut rows: Vec<usize> = (0..CENSUS_ROWS)
            .filter(|&row| self.census[row].bytes > 0)
            .collect();
        rows.sort_by_key(|&row| std::cmp::Reverse(self.census[row].bytes));
        for row in rows {
            let CensusRow {
                demos,
                count,
                bytes,
            } = self.census[row];
            let count_share = if row < 256 {
                format!("{:.4}", percent(count as f64, messages as f64))
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "{}\t{demos}\t{count}\t{count_share}\t{bytes}\t{:.4}\t{:.1}",
                census_name(row),
                percent(bytes as f64, raw),
                bytes as f64 / count as f64
            );
        }

        let _ = writeln!(
            out,
            "\n# groups: raw bytes against the coded bytes of the group's streams\ngroup\traw bytes\traw %\tcoded bytes\tcoded %\tcoded/raw"
        );
        let mut group_raw = vec![0u64; GROUPS.len()];
        let mut group_coded = vec![0f64; GROUPS.len()];
        for (row, tally) in self.census.iter().enumerate() {
            group_raw[census_group(row)] += tally.bytes;
        }
        // the command bytes are raw bytes of the COMMAND stream, not of the command's group
        let command_bytes: u64 = self.census[..256].iter().map(|row| row.count).sum();
        for (row, tally) in self.census[..256].iter().enumerate() {
            group_raw[census_group(row)] -= tally.count;
        }
        group_raw[GROUP_FRAMING] += command_bytes;
        for (stream, tally) in self.streams.iter().enumerate() {
            group_coded[stream_group(stream)] += tally.coded_bytes;
        }
        group_coded[GROUP_FRAMING] += self.container_bytes;
        let mut groups: Vec<usize> = (0..GROUPS.len()).collect();
        groups.sort_by(|&a, &b| group_coded[b].total_cmp(&group_coded[a]));
        for group in groups {
            if group_raw[group] == 0 && group_coded[group] == 0.0 {
                continue;
            }
            let _ = writeln!(
                out,
                "{}\t{}\t{:.4}\t{:.0}\t{:.4}\t{:.4}",
                GROUPS[group],
                group_raw[group],
                percent(group_raw[group] as f64, raw),
                group_coded[group],
                percent(group_coded[group], coded),
                group_coded[group] / group_raw[group] as f64
            );
        }

        let _ = writeln!(
            out,
            "\n# streams: coded bytes with stream table entries and models; blob streams share the LZ token streams by token cost\nstream\tkind\tgroup\tdemos\tsymbols\tcoded bytes\tcoded %\tbits/symbol"
        );
        let mut streams: Vec<usize> = (0..STREAM_COUNT)
            .filter(|&stream| self.streams[stream].symbols > 0)
            .collect();
        streams.sort_by(|&a, &b| {
            (self.streams[b].coded_bytes).total_cmp(&self.streams[a].coded_bytes)
        });
        for stream in streams {
            let StreamRow {
                demos,
                symbols,
                coded_bytes,
            } = self.streams[stream];
            let kind = if is_blob(stream) {
                "blob"
            } else if crate::streams::is_numeric(stream) {
                "numeric"
            } else {
                "structural"
            };
            let _ = writeln!(
                out,
                "{}\t{kind}\t{}\t{demos}\t{symbols}\t{coded_bytes:.0}\t{:.4}\t{:.3}",
                stream_name(stream),
                GROUPS[stream_group(stream)],
                percent(coded_bytes, coded),
                8.0 * coded_bytes / symbols as f64
            );
        }
        let _ = writeln!(
            out,
            "(container)\t\tframing\t{}\t\t{:.0}\t{:.4}\t",
            self.demos,
            self.container_bytes,
            percent(self.container_bytes, coded)
        );

        if self.ablated_demos == 0 {
            return out;
        }
        let ablated_coded = self.ablated_coded_bytes as f64;
        let _ = writeln!(
            out,
            "\n# ablations: file growth with a layout or a model taken away\ndemos\t{}\nraw bytes\t{}\nmvdz bytes\t{}\n\nvariant\tgrowth bytes\tgrowth % of mvdz\tgrowth % of raw",
            self.ablated_demos, self.ablated_raw_bytes, self.ablated_coded_bytes
        );
        let mut variants: Vec<usize> = (0..VARIANTS.len()).collect();
        variants.sort_by_key(|&variant| std::cmp::Reverse(self.growth[variant]));
        for variant in variants {
            let growth = self.growth[variant];
            let _ = writeln!(
                out,
                "{}\t{growth}\t{:.4}\t{:.4}",
                VARIANTS[variant].0,
                percent(growth as f64, ablated_coded),
                percent(growth as f64, self.ablated_raw_bytes as f64)
            );
        }
        out
    }

    fn attribute(&mut self, segments: &[Segment], coded: &Coded) {
        const TRANSFORM_FIELDS_LEN: usize = 4 * 4;
        const TABLE_ENTRY_LEN: usize = 8;
        let lz_costs = LzCosts::new(segments);
        let mut is_seen = vec![false; STREAM_COUNT];
        let mut coded_bytes = vec![0f64; STREAM_COUNT];
        let mut blob_weights = vec![0f64; STREAM_COUNT];
        let is_token_stream =
            |stream: usize| [LZ_LITERAL, LZ_LENGTH, LZ_DISTANCE].contains(&stream);
        let mut container = (HEADER_LEN + LEN_FIELD_LEN) as f64;
        for (segment, body) in segments.iter().zip(&coded.bodies) {
            container += (LEN_FIELD_LEN + SEGMENT_FIELDS_LEN) as f64;
            let Body::Transform(body) = body else {
                // all tail or stored: every byte is the tail's
                coded_bytes[TAIL] += body.len() as f64;
                self.streams[TAIL].symbols += segment.raw.len() as u64;
                is_seen[TAIL] = true;
                continue;
            };
            let walked = &segment.walked;
            let weights = lz_costs.blob_weights(walked);
            let weight_total: f64 = weights.iter().sum();
            container += TRANSFORM_FIELDS_LEN as f64;
            let mut table = &body[TRANSFORM_FIELDS_LEN - 4..];
            let entry_count = read_usize(&mut table).unwrap_or(0);
            for _ in 0..entry_count {
                let (Some(stream), Some(size)) = (read_usize(&mut table), read_usize(&mut table))
                else {
                    break;
                };
                is_seen[stream] = true;
                if is_blob(stream) {
                    self.streams[stream].symbols += size as u64;
                    coded_bytes[stream] += TABLE_ENTRY_LEN as f64;
                } else if is_token_stream(stream) && weight_total > 0.0 {
                    let pool = (size + TABLE_ENTRY_LEN) as f64;
                    for (blob, weight) in weights.iter().enumerate() {
                        coded_bytes[blob] += pool * weight / weight_total;
                    }
                } else {
                    self.streams[stream].symbols += walked.streams[stream].symbols.len() as u64;
                    coded_bytes[stream] += (size + TABLE_ENTRY_LEN) as f64;
                }
            }
            for (total, weight) in blob_weights.iter_mut().zip(&weights) {
                *total += weight;
            }
        }
        // the models block, by raw model length; the token models go to the blobs they code
        let raw_models_len: usize = coded.model_lens.iter().sum();
        let blob_weight_total: f64 = blob_weights.iter().sum();
        for (stream, &model_len) in coded.model_lens.iter().enumerate() {
            if model_len == 0 {
                continue;
            }
            let share = coded.models_block_len as f64 * model_len as f64 / raw_models_len as f64;
            if is_token_stream(stream) && blob_weight_total > 0.0 {
                for (blob, weight) in blob_weights.iter().enumerate() {
                    coded_bytes[blob] += share * weight / blob_weight_total;
                }
            } else {
                coded_bytes[stream] += share;
            }
        }
        self.container_bytes += container;
        for (stream, row) in self.streams.iter_mut().enumerate() {
            row.demos += u64::from(is_seen[stream]);
            row.coded_bytes += coded_bytes[stream];
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CensusRow {
    demos: u64,
    count: u64,
    bytes: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct StreamRow {
    demos: u64,
    /// Coded symbols of a structural or numeric stream, bytes of a blob stream.
    symbols: u64,
    coded_bytes: f64,
}

/// Census rows 0-255 are the commands, by their byte.
const ROW_FRAME_HEADERS: usize = 256;
const ROW_SET_FRAMES: usize = 257;
const ROW_ESCAPED: usize = 258;
const ROW_TAIL: usize = 259;
/// The first of `HIDDEN_TYPES + 1`: hidden blocks by type, the last for every other type.
const ROW_HIDDEN: usize = 260;
const HIDDEN_TYPES: usize = 12;
const CENSUS_ROWS: usize = ROW_HIDDEN + HIDDEN_TYPES + 1;
const HIDDEN_BLOCK_HEADER_LEN: usize = 6;

const MVD_HIDDEN_USERCMD: u32 = 1;

fn percent(part: f64, whole: f64) -> f64 {
    100.0 * part / whole
}

/// What an ablated walk codes differently.
#[derive(Debug, Clone, Copy)]
enum Ablation {
    None,
    /// These commands as generic blobs, one per command.
    Generic(&'static [u8]),
    /// Hidden blocks of this type as other hidden blocks.
    HiddenMisc(u32),
    /// These structural or numeric streams as blob bytes, without a model.
    Blob(&'static [usize]),
}

/// Every command with a layout of its own but `svc_playerinfo` and the packet entities.
const MINOR_COMMANDS: &[u8] = &[
    SVC_UPDATE_STAT,
    SVC_SOUND,
    SVC_TEMP_ENTITY,
    SVC_NAILS,
    SVC_NAILS_2,
];

const VARIANTS: &[(&str, Ablation)] = &[
    (
        "generic: svc_playerinfo",
        Ablation::Generic(&[SVC_PLAYER_INFO]),
    ),
    (
        "generic: svc_packetentities + svc_deltapacketentities",
        Ablation::Generic(&[SVC_PACKET_ENTITIES, SVC_DELTA_PACKET_ENTITIES]),
    ),
    (
        "generic: svc_updatestat",
        Ablation::Generic(&[SVC_UPDATE_STAT]),
    ),
    ("generic: svc_sound", Ablation::Generic(&[SVC_SOUND])),
    (
        "generic: svc_temp_entity",
        Ablation::Generic(&[SVC_TEMP_ENTITY]),
    ),
    (
        "generic: svc_nails + svc_nails2",
        Ablation::Generic(&[SVC_NAILS, SVC_NAILS_2]),
    ),
    (
        "generic: every command but svc_playerinfo and the packet entities",
        Ablation::Generic(MINOR_COMMANDS),
    ),
    (
        "hidden misc: usercmd blocks",
        Ablation::HiddenMisc(MVD_HIDDEN_USERCMD),
    ),
    ("no model: TIME", Ablation::Blob(&[TIME])),
    ("no model: TYPE", Ablation::Blob(&[TYPE])),
    ("no model: COMMAND", Ablation::Blob(&[COMMAND])),
    ("no model: PLAYER_FLAGS", Ablation::Blob(&[PLAYER_FLAGS])),
    ("no model: PLAYER_FRAME", Ablation::Blob(&[PLAYER_FRAME])),
    (
        "no model: PLAYER_WEAPON_FRAME",
        Ablation::Blob(&[PLAYER_WEAPON_FRAME]),
    ),
    ("no model: ENTITY_GAP", Ablation::Blob(&[ENTITY_GAP])),
    ("no model: ENTITY_BITS", Ablation::Blob(&[ENTITY_BITS])),
    ("no model: ENTITY_MORE", Ablation::Blob(&[ENTITY_MORE])),
    ("no model: ENTITY_ANGLE", Ablation::Blob(&[ENTITY_ANGLE])),
    ("no model: STAT_INDEX", Ablation::Blob(&[STAT_INDEX])),
    ("no model: STAT_VALUE", Ablation::Blob(&[STAT_VALUE])),
    ("no model: SOUND_CHANNEL", Ablation::Blob(&[SOUND_CHANNEL])),
    ("no model: SOUND_NUMBER", Ablation::Blob(&[SOUND_NUMBER])),
    (
        "no model: TEMP_ENTITY_TYPE",
        Ablation::Blob(&[TEMP_ENTITY_TYPE]),
    ),
    ("no model: USERCMD", Ablation::Blob(&[USERCMD])),
    (
        "no model: PLAYER_ORIGIN x3",
        Ablation::Blob(&[PLAYER_ORIGIN, PLAYER_ORIGIN + 1, PLAYER_ORIGIN + 2]),
    ),
    (
        "no model: PLAYER_ANGLE x3",
        Ablation::Blob(&[PLAYER_ANGLE, PLAYER_ANGLE + 1, PLAYER_ANGLE + 2]),
    ),
    (
        "no model: ENTITY_ORIGIN x3",
        Ablation::Blob(&[ENTITY_ORIGIN, ENTITY_ORIGIN + 1, ENTITY_ORIGIN + 2]),
    ),
    ("no model: SOUND_ORIGIN", Ablation::Blob(&[SOUND_ORIGIN])),
    (
        "no model: TEMP_ENTITY_ORIGIN",
        Ablation::Blob(&[TEMP_ENTITY_ORIGIN]),
    ),
    (
        "no model: PLAYER_FLOAT_ORIGIN x3",
        Ablation::Blob(&[
            PLAYER_FLOAT_ORIGIN,
            PLAYER_FLOAT_ORIGIN + 1,
            PLAYER_FLOAT_ORIGIN + 2,
        ]),
    ),
    (
        "no model: ENTITY_FLOAT_ORIGIN x3",
        Ablation::Blob(&[
            ENTITY_FLOAT_ORIGIN,
            ENTITY_FLOAT_ORIGIN + 1,
            ENTITY_FLOAT_ORIGIN + 2,
        ]),
    ),
    ("no model: USERCMD_ANGLE", Ablation::Blob(&[USERCMD_ANGLE])),
];

/// The encoder's streams, with the ablated ones redirected.
#[derive(Debug)]
struct AblatedIo {
    inner: EncodeIo,
    ablation: Ablation,
}

impl AblatedIo {
    /// The blob stream that takes the place of structural or numeric stream `stream`: no
    /// command has a byte this high, so the id is free.
    fn blob_of(&self, stream: usize) -> Option<usize> {
        const UNUSED_GENERIC: usize = GENERIC + 128;
        match self.ablation {
            Ablation::Blob(streams) if streams.contains(&stream) => Some(UNUSED_GENERIC + stream),
            _ => None,
        }
    }
}

impl SymbolIo for AblatedIo {
    #[inline(always)]
    fn symbol(&mut self, stream: usize, context: u32, value: u8) -> Option<u8> {
        match self.blob_of(stream) {
            Some(blob) => self.inner.blobs[blob].push(value),
            None => return self.inner.symbol(stream, context, value),
        }
        Some(value)
    }
    #[inline(always)]
    fn residual(&mut self, stream: usize, context: u32, value: i32) -> Option<i32> {
        let Some(blob) = self.blob_of(stream) else {
            return self.inner.residual(stream, context, value);
        };
        let is_float = (PLAYER_FLOAT_ORIGIN..PLAYER_FLOAT_ORIGIN + 3).contains(&stream)
            || (ENTITY_FLOAT_ORIGIN..ENTITY_FLOAT_ORIGIN + 3).contains(&stream);
        if is_float {
            self.inner.blobs[blob].extend_from_slice(&value.to_le_bytes());
        } else {
            self.inner.blobs[blob].extend_from_slice(&(value as i16).to_le_bytes());
        }
        Some(value)
    }
    fn put_blob(&mut self, stream: usize, bytes: &[u8]) {
        self.inner.put_blob(stream, bytes);
    }
    fn take_blob(&mut self, _: usize, _: usize) -> Option<&[u8]> {
        None
    }
    fn blob_rest(&self, _: usize) -> &[u8] {
        &[]
    }
    fn is_generic(&self, command: u8) -> bool {
        matches!(self.ablation, Ablation::Generic(commands) if commands.contains(&command))
    }
    fn is_hidden_misc(&self, block_type: u32) -> bool {
        matches!(self.ablation, Ablation::HiddenMisc(ablated_type) if ablated_type == block_type)
    }
}

#[derive(Debug)]
struct Segment<'a> {
    raw: &'a [u8],
    walked: Walked,
}

/// The encoder's pass 1, in one thread.
fn walk_demo<'a>(
    demo: &'a [u8],
    ablation: Ablation,
    mut census: Option<&mut [CensusRow]>,
) -> Vec<Segment<'a>> {
    let cuts = cut_points(demo);
    let mut protocol = ProtocolState::default();
    let mut segments = Vec::with_capacity(cuts.len());
    for (&start, &end) in cuts.iter().zip(&cuts[1..]) {
        let raw = &demo[start..end];
        let io = AblatedIo {
            inner: EncodeIo::new(),
            ablation,
        };
        let mut walker = SegmentWalker::encoder(raw, io, protocol);
        walker.run();
        let start_protocol = protocol;
        let is_walked = !walker.has_error;
        if let Some(census) = census.as_deref_mut() {
            if is_walked {
                count_segment(raw, start_protocol, census);
            } else {
                add(&mut census[ROW_TAIL], raw.len());
            }
        }
        let segment = if is_walked {
            protocol = walker.protocol;
            Walked::new(walker.io.inner, raw.len(), start_protocol)
        } else {
            walk_tail(raw, start_protocol)
        };
        segments.push(Segment {
            raw,
            walked: segment,
        });
    }
    segments
}

#[derive(Debug)]
enum Body {
    Transform(Vec<u8>),
    /// All tail, or stored.
    Fallback(Vec<u8>),
}

impl Body {
    fn len(&self) -> usize {
        match self {
            Self::Transform(body) | Self::Fallback(body) => body.len(),
        }
    }
}

#[derive(Debug)]
struct Coded {
    /// Of the whole file.
    len: u64,
    bodies: Vec<Body>,
    models_block_len: usize,
    /// By stream id: the raw model, 0 for a stream without a used model.
    model_lens: Vec<usize>,
}

/// The encoder's pass 2 and the file layout, in one thread.
fn code(segments: &[Segment]) -> Coded {
    let mut builder = ModelBuilder::default();
    let walked: Vec<&Walked> = segments.iter().map(|segment| &segment.walked).collect();
    builder.add(&walked, 1);
    let models = builder.models(1);
    let mut used_ids = Vec::new();
    let code_with = |walked: &Walked, used_ids: &mut Vec<usize>| -> Option<Vec<u8>> {
        let mut ids = Vec::new();
        let coded = encode(walked, &mut ids, &models)
            .filter(|coded| coded.len() as u64 <= MAX_SEGMENT_CODED)?;
        used_ids.extend(ids);
        Some(coded)
    };
    let bodies: Vec<Body> = segments
        .iter()
        .map(|segment| {
            if let Some(coded) = code_with(&segment.walked, &mut used_ids) {
                return Body::Transform(coded);
            }
            let protocol = segment.walked.start_protocol;
            let all_tail = walk_tail(segment.raw, protocol);
            Body::Fallback(
                code_with(&all_tail, &mut used_ids)
                    .unwrap_or_else(|| stored(segment.raw, protocol)),
            )
        })
        .collect();
    used_ids.sort_unstable();
    used_ids.dedup();
    let mut model_lens = vec![0; STREAM_COUNT];
    for &stream in &used_ids {
        model_lens[stream] = models[stream].as_ref().map_or(0, model_len);
    }
    let models_block_len = if used_ids.is_empty() {
        0
    } else {
        super::models_body(&models, &used_ids).len()
    };
    let bodies_len: usize = bodies
        .iter()
        .map(|body| LEN_FIELD_LEN + SEGMENT_FIELDS_LEN + body.len())
        .sum();
    Coded {
        len: (HEADER_LEN + LEN_FIELD_LEN + models_block_len + bodies_len) as u64,
        bodies,
        models_block_len,
        model_lens,
    }
}

fn model_len(model: &FileModel) -> usize {
    model.bytes.len()
}

/// What the tokens of the blob bytes cost, from their counts over the demo.
struct LzCosts {
    literals: ContextCosts,
    lens: ContextCosts,
    dists: ContextCosts,
}

impl LzCosts {
    fn new(segments: &[Segment]) -> Self {
        let costs_of = |stream: usize, alphabet: usize| {
            let mut costs = ContextCosts::new(alphabet);
            for segment in segments {
                costs.count(&segment.walked.streams[stream]);
            }
            costs
        };
        Self {
            literals: costs_of(LZ_LITERAL, 256),
            lens: costs_of(LZ_LENGTH, crate::rans::NUMERIC_ALPHABET),
            dists: costs_of(LZ_DISTANCE, crate::rans::NUMERIC_ALPHABET),
        }
    }

    /// By stream id: the bits of the tokens that produce the bytes of each blob stream.
    fn blob_weights(&self, walked: &Walked) -> Vec<f64> {
        let mut weights = vec![0f64; STREAM_COUNT];
        let literals = &walked.streams[LZ_LITERAL];
        let lens = &walked.streams[LZ_LENGTH];
        let dists = &walked.streams[LZ_DISTANCE];
        let mut len_mantissas = BitReader::new(&lens.extra);
        let mut blobs = (0..STREAM_COUNT)
            .filter(|&stream| !walked.blobs[stream].is_empty())
            .map(|stream| (stream, walked.blobs[stream].len()));
        let Some((mut blob, mut blob_rest)) = blobs.next() else {
            return weights;
        };
        let (mut literal_index, mut dist_index) = (0, 0);
        for (token_index, &len_symbol) in lens.symbols.iter().enumerate() {
            let mut bits = self.lens.bits(lens.contexts[token_index], len_symbol)
                + f64::from(mantissa_bit_count(len_symbol));
            let len = value_of(len_symbol, &mut len_mantissas);
            let produced = if len == 0 {
                bits += (self.literals).bits(
                    literals.contexts[literal_index],
                    literals.symbols[literal_index],
                );
                literal_index += 1;
                1
            } else {
                let dist_symbol = dists.symbols[dist_index];
                bits += self.dists.bits(dists.contexts[dist_index], dist_symbol)
                    + f64::from(mantissa_bit_count(dist_symbol));
                dist_index += 1;
                len
            };
            // a match over the end of a blob stream is shared by the bytes it produces
            let mut left = produced;
            while left > 0 {
                if blob_rest == 0 {
                    let Some(next) = blobs.next() else {
                        return weights;
                    };
                    (blob, blob_rest) = next;
                }
                let here = left.min(blob_rest);
                weights[blob] += bits * here as f64 / produced as f64;
                left -= here;
                blob_rest -= here;
            }
        }
        weights
    }
}

/// -log2 of each symbol's share of its context.
struct ContextCosts {
    alphabet: usize,
    /// By context, then symbol.
    counts: Vec<u32>,
    totals: Vec<u32>,
}

impl ContextCosts {
    const CONTEXTS: usize = 256;

    fn new(alphabet: usize) -> Self {
        Self {
            alphabet,
            counts: vec![0; Self::CONTEXTS * alphabet],
            totals: vec![0; Self::CONTEXTS],
        }
    }

    fn count(&mut self, stream: &StreamSymbols) {
        for (&symbol, &context) in stream.symbols.iter().zip(&stream.contexts) {
            self.counts[context as usize * self.alphabet + symbol as usize] += 1;
            self.totals[context as usize] += 1;
        }
    }

    fn bits(&self, context: u16, symbol: u8) -> f64 {
        let count = self.counts[context as usize * self.alphabet + symbol as usize];
        (f64::from(self.totals[context as usize]) / f64::from(count)).log2()
    }
}

/// `math::bucket`, backwards.
fn mantissa_bit_count(symbol: u8) -> u32 {
    u32::from(symbol).div_ceil(2).saturating_sub(1)
}

/// A length: never negative.
fn value_of(symbol: u8, mantissas: &mut BitReader) -> usize {
    if symbol == 0 {
        return 0;
    }
    let bit_count = mantissa_bit_count(symbol);
    ((1 << bit_count) | mantissas.get(bit_count)) as usize
}

fn add(row: &mut CensusRow, bytes: usize) {
    row.count += 1;
    row.bytes += bytes as u64;
}

/// The frames of a segment the walker walked, as it frames them.
fn count_segment(raw: &[u8], mut protocol: ProtocolState, census: &mut [CensusRow]) {
    let mut offset = 0;
    while let Some(len) = frame_len(raw, offset) {
        let frame = &raw[offset..offset + len];
        offset += len;
        match frame[1] & 7 {
            DEM_SET => add(&mut census[ROW_SET_FRAMES], len),
            DEM_MULTIPLE => {
                add(&mut census[ROW_FRAME_HEADERS], 10);
                let is_hidden = frame[2..6] == [0; 4];
                if is_hidden {
                    count_hidden(&frame[10..], census);
                } else {
                    count_commands(&frame[10..], &mut protocol, census);
                }
            }
            _ => {
                add(&mut census[ROW_FRAME_HEADERS], 6);
                count_commands(&frame[6..], &mut protocol, census);
            }
        }
    }
    if offset < raw.len() {
        add(&mut census[ROW_TAIL], raw.len() - offset);
    }
}

fn count_commands(payload: &[u8], protocol: &mut ProtocolState, census: &mut [CensusRow]) {
    let mut rest = payload;
    while let Some((&command, body)) = rest.split_first() {
        let size = protocol
            .command_body_size(command, body)
            .filter(|_| command != 0);
        let Some(size) = size else {
            add(&mut census[ROW_ESCAPED], rest.len());
            return;
        };
        add(&mut census[command as usize], 1 + size);
        if command == SVC_SERVER_DATA {
            protocol.observe_server_data(&body[..size]);
        }
        rest = &body[size..];
    }
}

fn count_hidden(payload: &[u8], census: &mut [CensusRow]) {
    let mut rest = payload;
    while !rest.is_empty() {
        let mut block = rest;
        let block_len = read_usize(&mut block).filter(|block_len| {
            (1..=rest.len().saturating_sub(HIDDEN_BLOCK_HEADER_LEN)).contains(block_len)
        });
        let (Some(block_len), Some(block_type)) = (block_len, read_u16(&mut block)) else {
            add(&mut census[ROW_ESCAPED], rest.len());
            return;
        };
        let len = HIDDEN_BLOCK_HEADER_LEN + block_len;
        add(
            &mut census[ROW_HIDDEN + (block_type as usize).min(HIDDEN_TYPES)],
            len,
        );
        rest = &rest[len..];
    }
}

const GROUP_FRAMING: usize = 0;
const GROUP_PLAYER_INFO: usize = 1;
const GROUP_PACKET_ENTITIES: usize = 2;
const GROUP_UPDATE_STAT: usize = 3;
const GROUP_SOUND: usize = 4;
const GROUP_TEMP_ENTITY: usize = 5;
const GROUP_NAILS: usize = 6;
const GROUP_HIDDEN_USERCMD: usize = 7;
const GROUP_HIDDEN_OTHER: usize = 8;
const GROUP_UNPARSED: usize = 9;
const GROUP_GENERIC: usize = 10;
const GROUPS: [&str; 11] = [
    "framing (frame headers, set frames, command bytes, container)",
    "svc_playerinfo",
    "svc_packetentities + svc_deltapacketentities",
    "svc_updatestat",
    "svc_sound",
    "svc_temp_entity",
    "svc_nails + svc_nails2",
    "hidden usercmd",
    "hidden other (block headers of every hidden block)",
    "unparsed (escaped payloads, tails)",
    "generic commands",
];

fn census_group(row: usize) -> usize {
    const USERCMD_ROW: usize = ROW_HIDDEN + MVD_HIDDEN_USERCMD as usize;
    match row {
        ROW_FRAME_HEADERS | ROW_SET_FRAMES => GROUP_FRAMING,
        ROW_ESCAPED | ROW_TAIL => GROUP_UNPARSED,
        USERCMD_ROW => GROUP_HIDDEN_USERCMD,
        ROW_HIDDEN.. => GROUP_HIDDEN_OTHER,
        _ => match row as u8 {
            SVC_PLAYER_INFO => GROUP_PLAYER_INFO,
            SVC_PACKET_ENTITIES | SVC_DELTA_PACKET_ENTITIES => GROUP_PACKET_ENTITIES,
            SVC_UPDATE_STAT => GROUP_UPDATE_STAT,
            SVC_SOUND => GROUP_SOUND,
            SVC_TEMP_ENTITY => GROUP_TEMP_ENTITY,
            SVC_NAILS | SVC_NAILS_2 => GROUP_NAILS,
            _ => GROUP_GENERIC,
        },
    }
}

/// The hidden block headers (`HIDDEN_TAG`, `HIDDEN_LENGTH`, `HIDDEN_TYPE`) are with "hidden
/// other": usercmd blocks carry 6 of their 29 raw bytes there.
fn stream_group(stream: usize) -> usize {
    match stream {
        TIME | TYPE | COMMAND | MASK | SET => GROUP_FRAMING,
        PLAYER_FLAGS | PLAYER_FRAME | PLAYER_WEAPON_FRAME | PLAYER_NUMBER | PLAYER_MODEL
        | PLAYER_SKIN | PLAYER_EFFECTS => GROUP_PLAYER_INFO,
        stream
            if (PLAYER_ORIGIN..ENTITY_ORIGIN).contains(&stream)
                || (PLAYER_FLOAT_ORIGIN..ENTITY_FLOAT_ORIGIN).contains(&stream) =>
        {
            GROUP_PLAYER_INFO
        }
        ENTITY_GAP | ENTITY_BITS | ENTITY_MORE | ENTITY_ANGLE | ENTITY_HEADER | ENTITY_MODEL
        | ENTITY_FRAME | ENTITY_MISC => GROUP_PACKET_ENTITIES,
        stream
            if (ENTITY_ORIGIN..SOUND_ORIGIN).contains(&stream)
                || (ENTITY_FLOAT_ORIGIN..USERCMD_ANGLE).contains(&stream) =>
        {
            GROUP_PACKET_ENTITIES
        }
        STAT_INDEX | STAT_VALUE => GROUP_UPDATE_STAT,
        SOUND_CHANNEL | SOUND_NUMBER | SOUND_ORIGIN | SOUND_MISC | SOUND_FLOAT => GROUP_SOUND,
        TEMP_ENTITY_TYPE | TEMP_ENTITY_ORIGIN | TEMP_ENTITY_MISC | TEMP_ENTITY_FLOAT => {
            GROUP_TEMP_ENTITY
        }
        NAILS => GROUP_NAILS,
        USERCMD | USERCMD_ANGLE => GROUP_HIDDEN_USERCMD,
        HIDDEN_TAG | HIDDEN_LENGTH | HIDDEN_TYPE | HIDDEN_MISC => GROUP_HIDDEN_OTHER,
        ESCAPE_LENGTH | RAW | TAIL => GROUP_UNPARSED,
        _ => GROUP_GENERIC,
    }
}

fn census_name(row: usize) -> String {
    const HIDDEN_NAMES: [&str; HIDDEN_TYPES] = [
        "antilag_position",
        "usercmd",
        "usercmd_weapons",
        "demoinfo",
        "commentary_track",
        "commentary_data",
        "commentary_text_segment",
        "dmgdone",
        "usercmd_weapons_ss",
        "usercmd_weapon_instruction",
        "paused_duration",
        "demo_start_timestamp",
    ];
    match row {
        ROW_FRAME_HEADERS => "(frame headers)".to_owned(),
        ROW_SET_FRAMES => "(dem_set frames)".to_owned(),
        ROW_ESCAPED => "(escaped: payload rest that does not parse)".to_owned(),
        ROW_TAIL => "(tail: bytes that do not frame)".to_owned(),
        ROW_HIDDEN.. => match HIDDEN_NAMES.get(row - ROW_HIDDEN) {
            Some(name) => format!("hidden {} {name}", row - ROW_HIDDEN),
            None => "hidden, other types".to_owned(),
        },
        _ => format!("{row} {}", command_name(row as u8)),
    }
}

fn stream_name(stream: usize) -> String {
    const NAMES: [(usize, &str); GENERIC] = [
        (TIME, "TIME"),
        (TYPE, "TYPE"),
        (COMMAND, "COMMAND"),
        (PLAYER_FLAGS, "PLAYER_FLAGS"),
        (PLAYER_FRAME, "PLAYER_FRAME"),
        (PLAYER_WEAPON_FRAME, "PLAYER_WEAPON_FRAME"),
        (ENTITY_GAP, "ENTITY_GAP"),
        (ENTITY_BITS, "ENTITY_BITS"),
        (ENTITY_MORE, "ENTITY_MORE"),
        (ENTITY_ANGLE, "ENTITY_ANGLE"),
        (STAT_INDEX, "STAT_INDEX"),
        (STAT_VALUE, "STAT_VALUE"),
        (SOUND_CHANNEL, "SOUND_CHANNEL"),
        (SOUND_NUMBER, "SOUND_NUMBER"),
        (TEMP_ENTITY_TYPE, "TEMP_ENTITY_TYPE"),
        (USERCMD, "USERCMD"),
        (LZ_LITERAL, "LZ_LITERAL"),
        (PLAYER_ORIGIN, "PLAYER_ORIGIN.x"),
        (PLAYER_ORIGIN + 1, "PLAYER_ORIGIN.y"),
        (PLAYER_ORIGIN + 2, "PLAYER_ORIGIN.z"),
        (PLAYER_ANGLE, "PLAYER_ANGLE.x"),
        (PLAYER_ANGLE + 1, "PLAYER_ANGLE.y"),
        (PLAYER_ANGLE + 2, "PLAYER_ANGLE.z"),
        (ENTITY_ORIGIN, "ENTITY_ORIGIN.x"),
        (ENTITY_ORIGIN + 1, "ENTITY_ORIGIN.y"),
        (ENTITY_ORIGIN + 2, "ENTITY_ORIGIN.z"),
        (SOUND_ORIGIN, "SOUND_ORIGIN"),
        (TEMP_ENTITY_ORIGIN, "TEMP_ENTITY_ORIGIN"),
        (PLAYER_FLOAT_ORIGIN, "PLAYER_FLOAT_ORIGIN.x"),
        (PLAYER_FLOAT_ORIGIN + 1, "PLAYER_FLOAT_ORIGIN.y"),
        (PLAYER_FLOAT_ORIGIN + 2, "PLAYER_FLOAT_ORIGIN.z"),
        (ENTITY_FLOAT_ORIGIN, "ENTITY_FLOAT_ORIGIN.x"),
        (ENTITY_FLOAT_ORIGIN + 1, "ENTITY_FLOAT_ORIGIN.y"),
        (ENTITY_FLOAT_ORIGIN + 2, "ENTITY_FLOAT_ORIGIN.z"),
        (USERCMD_ANGLE, "USERCMD_ANGLE"),
        (LZ_LENGTH, "LZ_LENGTH"),
        (LZ_DISTANCE, "LZ_DISTANCE"),
        (MASK, "MASK"),
        (ESCAPE_LENGTH, "ESCAPE_LENGTH"),
        (HIDDEN_TAG, "HIDDEN_TAG"),
        (HIDDEN_LENGTH, "HIDDEN_LENGTH"),
        (HIDDEN_TYPE, "HIDDEN_TYPE"),
        (PLAYER_NUMBER, "PLAYER_NUMBER"),
        (PLAYER_MODEL, "PLAYER_MODEL"),
        (PLAYER_SKIN, "PLAYER_SKIN"),
        (PLAYER_EFFECTS, "PLAYER_EFFECTS"),
        (ENTITY_HEADER, "ENTITY_HEADER"),
        (ENTITY_MODEL, "ENTITY_MODEL"),
        (ENTITY_FRAME, "ENTITY_FRAME"),
        (ENTITY_MISC, "ENTITY_MISC"),
        (SOUND_MISC, "SOUND_MISC"),
        (TEMP_ENTITY_MISC, "TEMP_ENTITY_MISC"),
        (RAW, "RAW"),
        (TAIL, "TAIL"),
        (SET, "SET"),
        (NAILS, "NAILS"),
        (HIDDEN_MISC, "HIDDEN_MISC"),
        (SOUND_FLOAT, "SOUND_FLOAT"),
        (TEMP_ENTITY_FLOAT, "TEMP_ENTITY_FLOAT"),
    ];
    let named = NAMES.iter().find(|(id, _)| *id == stream);
    match named {
        Some((_, name)) => (*name).to_owned(),
        None => format!("GENERIC {}", command_name((stream - GENERIC) as u8)),
    }
}

fn command_name(command: u8) -> &'static str {
    match command {
        1 => "svc_nop",
        2 => "svc_disconnect",
        3 => "svc_updatestat",
        6 => "svc_sound",
        8 => "svc_print",
        9 => "svc_stufftext",
        10 => "svc_setangle",
        11 => "svc_serverdata",
        12 => "svc_lightstyle",
        14 => "svc_updatefrags",
        19 => "svc_damage",
        20 => "svc_spawnstatic",
        21 => "svc_fte_spawnstatic2",
        22 => "svc_spawnbaseline",
        23 => "svc_temp_entity",
        24 => "svc_setpause",
        26 => "svc_centerprint",
        27 => "svc_killedmonster",
        28 => "svc_foundsecret",
        29 => "svc_spawnstaticsound",
        30 => "svc_intermission",
        31 => "svc_finale",
        32 => "svc_cdtrack",
        34 => "svc_smallkick",
        35 => "svc_bigkick",
        36 => "svc_updateping",
        37 => "svc_updateentertime",
        38 => "svc_updatestatlong",
        39 => "svc_muzzleflash",
        40 => "svc_updateuserinfo",
        41 => "svc_download",
        42 => "svc_playerinfo",
        43 => "svc_nails",
        44 => "svc_chokecount",
        45 => "svc_modellist",
        46 => "svc_soundlist",
        47 => "svc_packetentities",
        48 => "svc_deltapacketentities",
        49 => "svc_maxspeed",
        50 => "svc_entgravity",
        51 => "svc_setinfo",
        52 => "svc_serverinfo",
        53 => "svc_updatepl",
        54 => "svc_nails2",
        66 => "svc_fte_spawnbaseline2",
        _ => "svc ?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{SVC_PRINT, SVC_UPDATE_PING};
    use pretty_assertions::assert_eq;

    /// A frame for everyone: stat 0 of the target set to 100, a ping, a print.
    fn demo() -> Vec<u8> {
        const DEM_ALL: u8 = 6;
        let payload: &[u8] = &[
            SVC_UPDATE_STAT,
            0,
            100,
            SVC_UPDATE_PING,
            1,
            25,
            0,
            SVC_PRINT,
            2,
            b'h',
            b'i',
            0,
        ];
        let mut frame = vec![10, DEM_ALL];
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(payload);
        frame.repeat(50)
    }

    #[test]
    fn census_covers_every_byte_and_sizes_match_the_encoder() -> anyhow::Result<()> {
        let demo = demo();
        let mut stats = Stats::default();
        stats.add_demo(&demo, true);
        let census_bytes: u64 = stats.census.iter().map(|row| row.bytes).sum();
        assert_eq!(census_bytes, demo.len() as u64);
        assert_eq!(stats.census[SVC_UPDATE_STAT as usize].count, 50);
        assert_eq!(stats.census[SVC_PRINT as usize].bytes, 250);
        let file = crate::encode(&demo, 1)?;
        assert_eq!(stats.coded_bytes, file.len() as u64);
        let attributed: f64 = stats.streams.iter().map(|row| row.coded_bytes).sum();
        assert!((attributed + stats.container_bytes - file.len() as f64).abs() < 1e-6);
        Ok(())
    }
}
