#![allow(dead_code)]

use crate::format::*;
use anyhow::Context as _;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};

pub(crate) fn compress(src: &[u8]) -> crate::Result<Vec<u8>> {
    crate::encode(src, 1)
}

pub(crate) fn decompress(file: &[u8], max_threads: u8) -> crate::Result<Vec<u8>> {
    crate::decode(file, max_threads)
}

pub(crate) fn compress_unchecked(src: &[u8]) -> crate::Result<Vec<u8>> {
    crate::encode::compress(src, 1)
}

/// Every segment all tail: the encoder's fallback layout.
pub(crate) fn all_tail(src: &[u8]) -> anyhow::Result<Vec<u8>> {
    use crate::segment::{ModelBuilder, cut_points, models_body, walk_tail};
    let protocol = crate::protocol::ProtocolState::default();
    let walked_segments: Vec<_> = cut_points(src)
        .windows(2)
        .map(|cut| {
            let raw = &src[cut[0]..cut[1]];
            (raw, walk_tail(raw, protocol))
        })
        .collect();
    let mut model_builder = ModelBuilder::default();
    let counted: Vec<_> = walked_segments.iter().map(|(_, walked)| walked).collect();
    model_builder.add(&counted, 1);
    let models = model_builder.models(1);
    let mut model_ids = Vec::new();
    let mut segments = Vec::new();
    for (raw, walked) in &walked_segments {
        let coded =
            crate::segment::encode(walked, &mut model_ids, &models).context("a tail segment")?;
        let mut segment = segment_header_bytes(
            raw.len() as u32,
            crate::segment::duration_ms(raw),
            crc32fast::hash(raw),
        );
        segment.extend_from_slice(&coded);
        segments.push(segment);
    }
    model_ids.sort_unstable();
    model_ids.dedup();
    let header = Header {
        size_bytes: src.len() as u64,
        duration_ms: u64::from(crate::segment::duration_ms(src)),
        crc32: crc32fast::hash(src),
        sha256: Sha256::digest(src).into(),
    };
    Ok(Parts {
        header,
        models: if model_ids.is_empty() {
            Vec::new()
        } else {
            models_body(&models, &model_ids)
        },
        segments,
    }
    .build())
}

/// Every segment stored: the encoder's last fallback, a file without models.
pub(crate) fn all_stored(src: &[u8]) -> Vec<u8> {
    let protocol = crate::protocol::ProtocolState::default();
    let segments = crate::segment::cut_points(src)
        .windows(2)
        .map(|cut| {
            let raw = &src[cut[0]..cut[1]];
            let mut segment = segment_header_bytes(
                raw.len() as u32,
                crate::segment::duration_ms(raw),
                crc32fast::hash(raw),
            );
            segment.extend_from_slice(&crate::segment::stored(raw, protocol));
            segment
        })
        .collect();
    let header = Header {
        size_bytes: src.len() as u64,
        duration_ms: u64::from(crate::segment::duration_ms(src)),
        crc32: crc32fast::hash(src),
        sha256: Sha256::digest(src).into(),
    };
    Parts {
        header,
        models: Vec::new(),
        segments,
    }
    .build()
}

/// The bytes the walk left unstructured: the RAW and TAIL streams of every segment.
pub(crate) fn unstructured_len(file: &[u8]) -> anyhow::Result<usize> {
    use crate::streams::{RAW, TAIL};
    let mut total = 0;
    for segment in parts(file)?.segments {
        let table = StreamTable::parse(&segment[SEGMENT_FIELDS_LEN..])?;
        let unstructured = (table.entries.iter()).filter(|&&(id, _)| id == RAW || id == TAIL);
        total += unstructured.map(|&(_, size)| size).sum::<usize>();
    }
    Ok(total)
}

/// The transform header and bodies of a segment (FORMAT.md 3.1).
#[derive(Clone)]
pub(crate) struct StreamTable {
    /// fte, fte2, mvd1
    pub(crate) fields: [usize; 3],
    pub(crate) stream_count: usize,
    /// (id, size)
    pub(crate) entries: Vec<(usize, usize)>,
    pub(crate) bodies: Vec<u8>,
}

impl StreamTable {
    pub(crate) fn parse(transform: &[u8]) -> anyhow::Result<Self> {
        use crate::fields::read_usize;
        let mut rest = transform;
        let mut next = || read_usize(&mut rest).context("a transform header field");
        let fields = [next()?, next()?, next()?];
        let stream_count = next()?;
        let mut entries = Vec::new();
        for _ in 0..stream_count {
            entries.push((next()?, next()?));
        }
        Ok(Self {
            fields,
            stream_count,
            entries,
            bodies: rest.to_vec(),
        })
    }

    pub(crate) fn build(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for field in self.fields.into_iter().chain([self.stream_count]) {
            crate::fields::put_usize(&mut out, field);
        }
        for &(id, size) in &self.entries {
            crate::fields::put_usize(&mut out, id);
            crate::fields::put_usize(&mut out, size);
        }
        out.extend_from_slice(&self.bodies);
        out
    }
}

pub(crate) fn several_segments(demo: &[u8]) -> Vec<u8> {
    demo.repeat(4 * crate::segment::SEGMENT_TARGET_RAW / demo.len().max(1) + 1)
}

#[derive(Debug)]
pub(crate) struct Rng(pub u64);
impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub(crate) fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

#[derive(Debug)]
pub(crate) struct Demo(pub Vec<u8>);

impl Demo {
    pub(crate) fn frame(&mut self, time_delta: u8, command: u8, payload: &[u8]) {
        self.0.push(time_delta);
        self.0.push(command);
        self.0
            .extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.0.extend_from_slice(payload);
    }
    pub(crate) fn hidden(&mut self, time_delta: u8, blocks: &[(u16, &[u8])]) {
        let mut payload = Vec::new();
        for (block_type, data) in blocks {
            payload.extend_from_slice(&(data.len() as u32).to_le_bytes());
            payload.extend_from_slice(&block_type.to_le_bytes());
            payload.extend_from_slice(data);
        }
        self.0.push(time_delta);
        self.0.push(3);
        self.0.extend_from_slice(&0u32.to_le_bytes());
        self.0
            .extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.0.extend_from_slice(&payload);
    }
}

pub(crate) fn message(command: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut message = vec![command];
    for part in parts {
        message.extend_from_slice(part);
    }
    message
}

pub(crate) fn c_string(text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

pub(crate) fn demoinfo_blocks(parts: &[(u16, &str)]) -> Vec<Vec<u8>> {
    parts
        .iter()
        .map(|(block_number, text)| {
            let mut body = block_number.to_le_bytes().to_vec();
            body.extend_from_slice(text.as_bytes());
            body
        })
        .collect()
}

pub(crate) fn synthetic(demoinfo: &[(u16, &str)], is_truncated: bool) -> Vec<u8> {
    let mut demo = Demo(Vec::new());
    let mut payload = Vec::new();
    payload.extend(message(
        9,
        &[&c_string(
            "fullserverinfo \"\\map\\dm4\\hostname\\test server\\deathmatch\\3\"\n",
        )],
    ));
    payload.extend(message(
        40,
        &[
            &[0],
            &7u32.to_le_bytes(),
            &c_string("\\name\\alice\\team\\red\\*client\\ezQuake 7"),
        ],
    ));
    payload.extend(message(
        40,
        &[
            &[3],
            &9u32.to_le_bytes(),
            &c_string("\\name\\bob\\team\\blue\\*client\\fte"),
        ],
    ));
    payload.extend(message(
        45,
        &[
            &[0],
            &c_string("maps/dm4.bsp"),
            &c_string("progs/player.mdl"),
            &[0],
            &[0],
        ],
    ));
    demo.frame(0, 6, &payload);
    for frame in 0..50u8 {
        let mut payload = Vec::new();
        payload.extend(message(
            8,
            &[&[2], &c_string(&format!("message {frame}\n"))],
        ));
        payload.extend(message(9, &[&c_string("topcolor ")]));
        payload.extend(message(
            9,
            &[&c_string(
                "10\n_cs;wait;_cs\n//ktx took 1 2 3\n//finalscores \"Oct 12\" 3\n",
            )],
        ));
        payload.extend(message(
            52,
            &[&c_string("status"), &c_string(&format!("{frame} min"))],
        ));
        demo.frame(13 + frame % 3, 6, &payload);
    }
    if !demoinfo.is_empty() {
        let bodies = demoinfo_blocks(demoinfo);
        let blocks: Vec<(u16, &[u8])> = bodies.iter().map(|body| (3u16, &body[..])).collect();
        demo.hidden(0, &blocks);
    }
    demo.frame(5, 6, &message(8, &[&[3], &c_string("bye\n")]));
    if is_truncated {
        // a frame cut short: header and part of its length
        demo.0.extend_from_slice(&[7, 6, 40, 0]);
    }
    demo.0
}

/// mvdsv `SV_DebugClientCommand`.
pub(crate) fn usercmd_block(
    player: u8,
    dropnum: u8,
    msec: u8,
    angles: [f32; 3],
    moves: [i16; 3],
    buttons_and_impulse: [u8; 2],
) -> Vec<u8> {
    let mut block = vec![player, dropnum, msec];
    for angle in angles {
        block.extend_from_slice(&angle.to_le_bytes());
    }
    for movement in moves {
        block.extend_from_slice(&movement.to_le_bytes());
    }
    block.extend_from_slice(&buttons_and_impulse);
    block
}

pub(crate) fn usercmd_demo() -> Vec<u8> {
    let mut demo = Demo(synthetic(&[], false));
    let mut rng = Rng(0x00c0_ffee);
    let mut grid_angles = [[0i32; 2]; 4];
    let off_grid = [
        -0.0f32,
        f32::NAN,
        180.0,
        1.2345,
        f32::from_bits(0x7fc0_1234),
    ];
    let grid_angle = |step: i32| f32::from(step as i16) * (45.0 / 8192.0);
    for tick in 0..600u32 {
        let player = (tick % 4) as u8;
        let player_angles = &mut grid_angles[player as usize];
        player_angles[0] =
            (player_angles[0] + i32::from(rng.below(9) as u8) - 4).clamp(-2900, 2900);
        player_angles[1] += 8 * (i32::from(rng.below(7) as u8) - 3);
        let mut angles = [
            grid_angle(player_angles[0]),
            grid_angle(player_angles[1]),
            0.0,
        ];
        if tick % 53 == 7 {
            angles[(tick % 3) as usize] = off_grid[(tick / 53) as usize % off_grid.len()];
        }
        let forward = [0i16, 320, 400, -400][(tick / 40 % 4) as usize];
        let side = if tick % 90 < 30 {
            0
        } else {
            508 * (1 - 2 * ((tick / 90) % 2) as i16)
        };
        let buttons_and_impulse = [
            u8::from(tick / 25 % 4 == 0) | (2 * u8::from(tick % 70 < 3)),
            if tick % 111 == 5 { 7 } else { 0 },
        ];
        let msec = if tick % 97 == 0 { 12 } else { 13 };
        let block = usercmd_block(
            player,
            u8::from(tick % 150 == 149),
            msec,
            angles,
            [forward, side, 0],
            buttons_and_impulse,
        );
        if tick % 61 == 30 {
            let second_block = usercmd_block(player ^ 1, 0, 13, angles, [0, 0, 0], [0, 0]);
            demo.hidden(0, &[(1, &block), (1, &second_block)]);
        } else {
            demo.hidden(if player == 0 { 13 } else { 0 }, &[(1, &block)]);
        }
        if tick % 150 == 75 {
            demo.hidden(
                0,
                &[(1, &block[..3]), (1, &[block.clone(), vec![9]].concat())],
            );
        }
    }
    demo.frame(5, 6, &message(8, &[&[3], &c_string("bye\n")]));
    demo.0
}

pub(crate) fn test_data(subdirectory: &str) -> Option<std::path::PathBuf> {
    if let Some(root) = std::env::var_os("MVDZ_TEST_DATA") {
        Some(std::path::PathBuf::from(root).join(subdirectory))
    } else {
        eprintln!("MVDZ_TEST_DATA is not set: skipping the parts that need {subdirectory}/ demos");
        None
    }
}

pub(crate) fn samples(count: usize) -> Vec<Vec<u8>> {
    let Some(dir) = test_data("sample") else {
        return Vec::new();
    };
    let mut paths: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "mvd"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    paths.sort();
    paths
        .into_iter()
        .take(count)
        .filter_map(|path| std::fs::read(path).ok())
        .collect()
}

pub(crate) fn sample(sha: &str) -> Option<Vec<u8>> {
    std::fs::read(test_data("sample")?.join(format!("{sha}.mvd"))).ok()
}

/// A sample8 demo (1on1, ztndm3); the golden vectors are made from it.
pub(crate) fn duel() -> Option<Vec<u8>> {
    sample("3fe477dc5e4c050fc411d82be87a24c5ec04d6f18b3c6bc0561789a4c4561416")
}

#[derive(Debug)]
pub(crate) struct Dribble<'a>(pub &'a [u8], pub usize);
impl Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let len = buf.len().min(7).min(self.0.len() - self.1);
        buf[..len].copy_from_slice(&self.0[self.1..self.1 + len]);
        self.1 += len;
        Ok(len)
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

pub(crate) fn all_readers(file: &[u8]) -> Vec<(&'static str, Result<Vec<u8>, String>)> {
    let message = |error: crate::Error| error.to_string();
    let mut results = Vec::new();
    results.push(("whole 1t", decompress(file, 1).map_err(message)));
    results.push(("whole 3t", decompress(file, 3).map_err(message)));
    let read_all = |reader: &mut dyn Read, max_threads| -> Result<Vec<u8>, String> {
        let mut decoder = crate::Decoder::new(reader)
            .map_err(message)?
            .max_threads(max_threads);
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .map_err(|error| error.to_string())?;
        Ok(out)
    };
    results.push(("sequential", read_all(&mut Cursor::new(file), 1)));
    results.push(("sequential dribble", read_all(&mut Dribble(file, 0), 2)));
    results
}

pub(crate) fn readers_agree(file: &[u8], what: &str) -> Option<Vec<u8>> {
    let results = all_readers(file);
    let first = &results[0].1;
    for (name, result) in &results {
        match (first, result) {
            (Ok(first_bytes), Ok(bytes)) => {
                assert!(
                    first_bytes == bytes,
                    "{what}: {name} decodes different bytes"
                );
            }
            (Err(_), Err(_)) => {}
            _ => panic!(
                "{what}: readers disagree: {} {:?} vs {name} {:?}",
                results[0].0,
                first.as_ref().map(std::vec::Vec::len),
                result.as_ref().map(std::vec::Vec::len)
            ),
        }
    }
    first.as_ref().ok().cloned()
}

/// One entry of a models block (FORMAT.md 2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelEntry {
    pub stream: usize,
    pub raw_len: usize,
    /// The raw model, coded under the meta model.
    pub coded: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelsParts {
    /// The meta model, raw.
    pub meta: Vec<u8>,
    pub entries: Vec<ModelEntry>,
}

pub(crate) fn models_parts(body: &[u8]) -> anyhow::Result<ModelsParts> {
    use crate::fields::{read_bytes, read_length_prefixed, read_usize};
    let mut rest = body;
    let own_count = read_usize(&mut &*rest).context("the meta model's own count")?;
    let meta_len = 4 + 2 * own_count + (own_count + 1) * 256;
    let meta = read_bytes(&mut rest, meta_len)
        .context("the meta model")?
        .to_vec();
    let count = read_usize(&mut rest).context("the entry count")?;
    let entries = (0..count)
        .map(|_| {
            Some(ModelEntry {
                stream: read_usize(&mut rest)?,
                raw_len: read_usize(&mut rest)?,
                coded: read_length_prefixed(&mut rest)?.to_vec(),
            })
        })
        .collect::<Option<_>>()
        .context("a models entry")?;
    Ok(ModelsParts { meta, entries })
}

impl ModelsParts {
    pub(crate) fn build(&self) -> Vec<u8> {
        use crate::fields::put_usize;
        let mut body = self.meta.clone();
        put_usize(&mut body, self.entries.len());
        for entry in &self.entries {
            for field in [entry.stream, entry.raw_len, entry.coded.len()] {
                put_usize(&mut body, field);
            }
            body.extend_from_slice(&entry.coded);
        }
        body
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parts {
    pub header: Header,
    pub models: Vec<u8>,
    pub segments: Vec<Vec<u8>>,
}

pub(crate) fn parts(file: &[u8]) -> anyhow::Result<Parts> {
    let header = parse_header(file)?;
    let mut offset = HEADER_LEN;
    let mut next_block = |file: &[u8], fields_len: usize| -> anyhow::Result<Vec<u8>> {
        let len =
            crate::fields::read_usize(&mut &file[offset..]).context("a length field")? + fields_len;
        let block = file[offset + LEN_FIELD_LEN..offset + LEN_FIELD_LEN + len].to_vec();
        offset += LEN_FIELD_LEN + len;
        Ok(block)
    };
    let models = next_block(file, 0)?;
    let mut segments = Vec::new();
    let mut raw = 0;
    while raw < header.size_bytes {
        let segment = next_block(file, SEGMENT_FIELDS_LEN)?;
        raw += u64::from(parse_segment_header(&segment[..SEGMENT_FIELDS_LEN])?.raw_len);
        segments.push(segment);
    }
    Ok(Parts {
        header,
        models,
        segments,
    })
}

impl Parts {
    pub(crate) fn build(&self) -> Vec<u8> {
        let mut out = header_bytes(&self.header).to_vec();
        models_block(&mut out, &self.models);
        for segment in &self.segments {
            segment_block(&mut out, segment);
        }
        out
    }
}
