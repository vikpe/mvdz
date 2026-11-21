//! Test helpers: synthetic demos, a small RNG, the sample demos (skipped when absent) and
//! the every-reader check.
#![allow(dead_code)]

use crate::encode::EncodeStats;
use crate::format::*;
use anyhow::Context as _;
use std::io::{Cursor, Read};

/// The public encoder on one thread: input that is not a demo is refused.
pub(crate) fn compress(src: &[u8]) -> crate::Result<Vec<u8>> {
    crate::encode(src, 1)
}

/// Header, models and every segment header, without decoding a segment.
pub(crate) fn scan(file: &[u8], max_memory: u64) -> crate::Result<Layout> {
    crate::decode::scan_layout(file, 1, &crate::budget::Budget::new(max_memory))
}

/// Decodes the whole file on `threads` threads (0: one per logical core).
pub(crate) fn decompress(file: &[u8], max_threads: u8) -> crate::Result<Vec<u8>> {
    crate::decode(file, max_threads)
}

/// The encoder without the input check (any bytes round-trip).
pub(crate) fn unchecked(src: &[u8]) -> crate::Result<(Vec<u8>, EncodeStats)> {
    crate::encode::compress(src, 1)
}

/// Every segment coded as all tail: no frames, every byte in the tail.
pub(crate) fn all_tail(src: &[u8]) -> crate::Result<(Vec<u8>, EncodeStats)> {
    crate::encode::compress_all_tail(src, 1)
}

/// `demo` repeated past four times [`crate::segment::SEGMENT_TARGET_RAW`]: the encoder
/// cuts only at that size, so a small demo of whole frames gets at least
/// four segments this way.
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

// ---------------------------------------------------------------- synthetic demos

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

/// KTX demoinfo block bodies (u16 block number, text).
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

/// A demo with prints, fragmented stufftext, userinfo, modellist,
/// serverinfo; optionally demoinfo blocks; optionally a truncated tail.
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
        // a command split over two stufftexts, and ';'-separated commands
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

/// One usercmd hidden-block body (mvdsv `SV_DebugClientCommand`, 23
/// bytes): player, dropnum, msec, pitch/yaw/roll, moves, buttons, impulse.
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

/// A demo with usercmd hidden blocks (type 1, 23 bytes) of four players:
/// turning view angles on the 360/65536 grid, a few angles off it (-0.0,
/// NaN, +180, 1.2345, an odd NaN payload), type-1 blocks of other lengths,
/// two usercmds in one payload, after the base synthetic demo.
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

// ---------------------------------------------------------------- sample demos

/// Optional test data, from the directory in `MVDZ_TEST_DATA`:
/// `sample/` (real demos named `<sha256>.mvd`; `scripts/fetch_samples.sh
/// sample40 DIR/sample` downloads sample40), `gen/` (constructed demos,
/// e.g. `float_mvd1.mvd`) and `poc/` (hostile model headers). Tests that
/// need it skip, with a note, when the variable is unset or the file is
/// missing; everything else runs on synthetic demos.
pub(crate) fn test_data(subdirectory: &str) -> Option<std::path::PathBuf> {
    if let Some(root) = std::env::var_os("MVDZ_TEST_DATA") {
        Some(std::path::PathBuf::from(root).join(subdirectory))
    } else {
        eprintln!("MVDZ_TEST_DATA is not set: skipping the parts that need {subdirectory}/ demos");
        None
    }
}

/// The first `count` sample demos (sorted by name), or none.
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

/// A sample demo by its SHA-256.
pub(crate) fn sample(sha: &str) -> Option<Vec<u8>> {
    std::fs::read(test_data("sample")?.join(format!("{sha}.mvd"))).ok()
}

/// The duel the golden vectors are made from (hub demo 3ff1f82b...8d92).
pub(crate) fn duel() -> Option<Vec<u8>> {
    sample("3ff1f82b5f0274da0af3f1744e7660d602159c3edfb0089134b728d3f3408d92")
}

// ---------------------------------------------------------------- every reader

/// A reader that hands out at most 7 bytes per call.
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

/// Decodes `file` with every reader: (reader, result).
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

/// Every reader accepts `file` with the same bytes, or every reader refuses
/// it. Returns the bytes if accepted. Structural refusals are also
/// required of `scan`.
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
    let scanned = scan(file, crate::budget::DEFAULT_MAX_MEMORY);
    if first.is_ok() {
        assert!(
            scanned.is_ok(),
            "{what}: scan refuses a file the decoders accept: {:?}",
            scanned.err()
        );
    }
    if let Err(error) = &scanned {
        assert!(
            first.is_err(),
            "{what}: decoders accept a file scan refuses ({error})"
        );
    }
    first.as_ref().ok().cloned()
}

// ---------------------------------------------------------------- file surgery

/// The entries (stream id, model) of a models block.
pub(crate) fn models_entries(body: &[u8]) -> anyhow::Result<Vec<(usize, Vec<u8>)>> {
    let mut reader = crate::util::ByteReader::new(body);
    let count = reader.u32_len().context("the entry count")?;
    (0..count)
        .map(|_| Some((reader.u32_len()?, reader.length_prefixed()?.to_vec())))
        .collect::<Option<_>>()
        .context("a models entry")
}

/// A models block from its entries.
pub(crate) fn models_body(entries: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    crate::util::put_u32(&mut body, entries.len());
    for (stream, model) in entries {
        crate::util::put_u32(&mut body, *stream);
        crate::util::put_u32(&mut body, model.len());
        body.extend_from_slice(model);
    }
    body
}

/// A file taken apart: its header, the body of its models block and the
/// body of every segment (its header fields after the length, then its
/// coded data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parts {
    pub header: Header,
    pub models: Vec<u8>,
    pub segments: Vec<Vec<u8>>,
}

/// The parts of a well-formed file.
pub(crate) fn parts(file: &[u8]) -> anyhow::Result<Parts> {
    let header = parse_header(file)?;
    let mut offset = HEADER_LEN;
    // the next block: `extra` bytes of fields, then as many as its length
    // field says
    let mut next = |file: &[u8], extra: usize| -> anyhow::Result<Vec<u8>> {
        let len = len_field(&file[offset..]).context("a length field")? as usize + extra;
        let block = file[offset + LEN_FIELD_LEN..offset + LEN_FIELD_LEN + len].to_vec();
        offset += LEN_FIELD_LEN + len;
        Ok(block)
    };
    let models = next(file, 0)?;
    let mut segments = Vec::new();
    let mut raw = 0;
    while raw < header.size_bytes {
        let segment = next(file, SEGMENT_FIELDS_LEN)?;
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
    /// The file these parts make (whatever they hold).
    pub(crate) fn build(&self) -> Vec<u8> {
        let mut out = header_bytes(&self.header).to_vec();
        models_block(&mut out, &self.models);
        for segment in &self.segments {
            segment_block(&mut out, segment);
        }
        out
    }
}
