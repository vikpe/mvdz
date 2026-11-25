use super::common::*;
use crate::format::{HEADER_SIZE, LEN_FIELD_SIZE, SEGMENT_FIELDS_SIZE, parse_segment_header};
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::assert_matches;
use std::cell::Cell;
use std::io::{Cursor, ErrorKind, Read, Seek, SeekFrom};

fn check_all_ways(source: &[u8], what: &str) -> anyhow::Result<()> {
    for (how, source) in [
        ("as is", source.to_vec()),
        ("repeated", several_segments(source)),
    ] {
        let one = crate::encode::encode_any(&source, 1)?;
        let back = readers_agree(&one, what).with_context(|| format!("{what} {how}: refused"))?;
        assert!(back == source, "{what} {how}");
    }
    Ok(())
}

#[test]
fn a_packet_of_120000_entity_words_round_trips() -> anyhow::Result<()> {
    let mut demo = Demo(synthetic(&[], false));
    let mut packet = vec![crate::protocol::SVC_PACKET_ENTITIES];
    for index in 0..120_000u32 {
        packet.extend_from_slice(&((511 - index % 511) as u16).to_le_bytes());
    }
    packet.extend_from_slice(&[0, 0]);
    demo.frame(5, crate::protocol::DEM_ALL, &packet);
    demo.0.extend_from_slice(&synthetic(&[], false));
    let file = crate::encode(&demo.0, 1)?;
    assert_eq!(
        readers_agree(&file, "count limit").context("every reader refuses")?,
        demo.0
    );
    assert!(unstructured_len(&file)? < packet.len());
    Ok(())
}

#[test]
fn synthetic_demos() -> anyhow::Result<()> {
    check_all_ways(&synthetic(&[], false), "plain")?;
    check_all_ways(&synthetic(&[(1, "{\"a\":"), (0, "1}")], false), "demoinfo")?;
    check_all_ways(&synthetic(&[], true), "truncated")?;
    Ok(())
}

#[test]
fn sample_demos() -> anyhow::Result<()> {
    for (index, source) in samples(3).iter().enumerate() {
        check_all_ways(source, &format!("sample {index}"))?;
    }
    Ok(())
}

fn read_until_error(file: &[u8], max_threads: u8) -> (Vec<u8>, Option<crate::Error>) {
    let mut out = Vec::new();
    let mut reader = match crate::Decoder::new(Cursor::new(file)) {
        Ok(reader) => reader.max_threads(max_threads),
        Err(error) => return (out, Some(error)),
    };
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read_demo(&mut buffer) {
            Ok(0) => return (out, None),
            Ok(len) => out.extend_from_slice(&buffer[..len]),
            Err(error) => return (out, Some(error)),
        }
    }
}

#[test]
fn segments_are_read_as_the_input_arrives() -> anyhow::Result<()> {
    struct Arriving<'a> {
        file: &'a [u8],
        position: usize,
        arrived_len: &'a Cell<usize>,
    }
    impl Read for Arriving<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let arrived = &self.file[self.position..self.arrived_len.get()];
            if arrived.is_empty() && self.position < self.file.len() {
                return Err(ErrorKind::WouldBlock.into());
            }
            let len = buffer.len().min(arrived.len());
            buffer[..len].copy_from_slice(&arrived[..len]);
            self.position += len;
            Ok(len)
        }
    }
    let demo = several_segments(&synthetic(&[], false));
    let file = crate::encode(&demo, 1)?;
    let file_parts = parts(&file)?;
    let first_segment = file_parts.segments.first().context("a segment")?;
    let first_segment_end =
        HEADER_SIZE + 2 * LEN_FIELD_SIZE + file_parts.models.len() + first_segment.len();
    for max_threads in [1, 3] {
        let mid_len_field = first_segment_end + 1;
        let arrived_len = Cell::new(mid_len_field);
        let input = Arriving {
            file: &file,
            position: 0,
            arrived_len: &arrived_len,
        };
        let mut decoder = crate::Decoder::new(input)?.max_threads(max_threads);
        let would_block = Err(crate::Error::Io(ErrorKind::WouldBlock));
        let mut out = decoder.next_segment()?.context("the first segment")?;
        assert_eq!(decoder.next_segment(), would_block);
        assert_eq!(decoder.next_segment(), would_block);
        arrived_len.set(file.len());
        decoder.read_to_end(&mut out)?;
        assert!(out == demo, "{max_threads} threads");
    }
    Ok(())
}

#[test]
fn sequential_reader_threads_agree() -> anyhow::Result<()> {
    const CUTS: usize = 7;
    let demo = several_segments(&synthetic(&[], false));
    let file = crate::encode(&demo, 1)?;
    let file_parts = parts(&file)?;
    anyhow::ensure!(
        file_parts.segments.len() >= 4,
        "{} segments",
        file_parts.segments.len()
    );
    let mut cases = Vec::new();
    for eighth in 1..=CUTS {
        cases.push(
            file.get(..file.len() * eighth / 8)
                .context("a cut")?
                .to_vec(),
        );
    }
    for segment in 0..file_parts.segments.len() {
        let mut damaged = file_parts.clone();
        let last_byte = damaged
            .segments
            .get_mut(segment)
            .and_then(|body| body.last_mut())
            .context("a segment body")?;
        *last_byte ^= 0x55;
        cases.push(damaged.build());
    }
    let mut partial_reads = 0;
    for (case, bytes) in cases.iter().enumerate() {
        let (out, error) = read_until_error(bytes, 1);
        assert!(demo.starts_with(&out), "case {case}");
        let is_cut = case < CUTS;
        if is_cut {
            assert!(error.is_some(), "cut {case} is refused");
        }
        if error.is_some() && !out.is_empty() {
            partial_reads += 1;
        }
        for threads in [2, 4] {
            let is_same = read_until_error(bytes, threads) == (out.clone(), error);
            assert!(is_same, "case {case}, {threads} threads");
        }
    }
    assert!(partial_reads >= 2, "{partial_reads}");
    Ok(())
}

#[test]
fn unused_model_is_ignored() -> anyhow::Result<()> {
    let demo = several_segments(&synthetic(&[], false));
    let file = crate::encode(&demo, 1)?;
    let mut file_parts = parts(&file)?;
    let mut models = models_parts(&file_parts.models)?;
    anyhow::ensure!(
        (models.entries.iter()).all(|entry| entry.stream != crate::streams::USERCMD),
        "the demo has no usercmd blocks"
    );
    let structural = (models.entries.iter())
        .find(|entry| crate::streams::is_structural(entry.stream))
        .context("a structural stream's model")?
        .clone();
    models.entries.push(ModelEntry {
        stream: crate::streams::USERCMD,
        ..structural
    });
    models.entries.sort_by_key(|entry| entry.stream);
    file_parts.models = models.build();
    let with_unused = file_parts.build();
    assert!(with_unused.len() > file.len());
    let decoded = readers_agree(&with_unused, "unused model").context("a reader refuses it")?;
    assert!(decoded == demo);
    Ok(())
}

#[test]
fn empty_and_tiny_inputs() -> anyhow::Result<()> {
    let mut rng = Rng(7);
    let random: Vec<u8> = Vec::from_fn(10_000, |_| rng.next() as u8);
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))?;
    let inputs: [&[u8]; 7] = [
        b"",
        b"\x00",
        b"MVDZ\x01",
        b"hello world",
        b"II*\x00\x08\x00\x00\x00\x00\x00\x00\x00", // a TIFF header
        &random,
        &text,
    ];
    for source in inputs {
        let refusal = crate::encode(source, 1).err();
        assert_matches!(refusal, Some(crate::Error::NotMvd(_)), "{refusal:?}");
        check_all_ways(source, &format!("{source:?}"))?;
    }
    Ok(())
}

/// Correctness review F2, robustness 7.
#[test]
fn non_mvd_input_streams() -> anyhow::Result<()> {
    let mut rng = Rng(7);
    let random: Vec<u8> = Vec::from_fn(10_000, |_| rng.next() as u8);
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))?;
    let mut inputs = vec![vec![1u8], random, text];
    if let Some(duel) = duel() {
        inputs.push(duel[..300].to_vec());
        let first_frame_len = 6 + u32::from_le_bytes(duel[2..6].try_into()?) as usize;
        let mut first_frame_cut_short = duel[..first_frame_len - 1].to_vec();
        first_frame_cut_short.extend_from_slice(&duel[first_frame_len..200_000]);
        inputs.push(first_frame_cut_short);
    }
    for (index, source) in inputs.iter().enumerate() {
        if crate::encode::check_demo(source).is_err() {
            let refusal = crate::encode(source, 1);
            assert_matches!(refusal, Err(crate::Error::NotMvd(_)), "input {index}");
            continue;
        }
        for source in [source.clone(), several_segments(source)] {
            let file = crate::encode(&source, 1)?;
            assert!(
                readers_agree(&file, "non-mvd").context("every reader refuses")? == source,
                "input {index}, {} bytes",
                source.len()
            );
        }
    }
    Ok(())
}

#[test]
fn framed_demo_as_all_tail_round_trips() -> anyhow::Result<()> {
    let demos = [
        ("synthetic", synthetic(&[(1, "{\"a\":"), (0, "1}")], false)),
        ("usercmd", usercmd_demo()),
    ];
    for (what, demo) in demos {
        for (how, source) in [
            ("as is", demo.clone()),
            ("repeated", several_segments(&demo)),
        ] {
            let file = all_tail(&source)?;
            assert_eq!(
                unstructured_len(&file)?,
                source.len(),
                "{what} {how}: every byte in the tail"
            );
            let back =
                readers_agree(&file, what).with_context(|| format!("{what} {how}: refused"))?;
            assert!(back == source, "{what} {how}");
            let stored = readers_agree(&all_stored(&source), what)
                .with_context(|| format!("{what} {how}: stored, refused"))?;
            assert!(stored == source, "{what} {how}: stored");
        }
    }
    Ok(())
}

/// No rule bounds a segment's coded length from below by its raw length.
#[test]
fn nop_flood_round_trips() -> anyhow::Result<()> {
    let mut demo = Demo(Vec::new());
    for _ in 0..400 {
        demo.frame(0, 1, &[1u8; 10_000]);
    }
    let source = demo.0;
    let file = crate::encode(&source, 1)?;
    assert!(parts(&file)?.segments.len() > 1);
    assert!(readers_agree(&file, "nop").context("every reader refuses")? == source);
    Ok(())
}

#[test]
fn segments_are_capped() -> anyhow::Result<()> {
    let max_segment = crate::format::MAX_RAW_LEN as usize;
    let mut rng = Rng(0x00ca_99ed);
    let unframed: Vec<u8> = Vec::from_fn(3 * max_segment, |_| rng.next() as u8);
    let mut oversized = Demo(synthetic(&[], false));
    oversized.frame(0, 1, &vec![1u8; max_segment + max_segment / 2]);
    oversized.0.extend_from_slice(&synthetic(&[], false));
    for (what, source, segments) in [
        ("unframed", unframed, 3),
        ("oversized frame", oversized.0, 2),
    ] {
        let file = crate::encode::encode_any(&source, 1)?;
        let file_segments = parts(&file)?.segments;
        let count = file_segments.len();
        assert!(count >= segments, "{what}: {count} segments");
        for segment in &file_segments {
            let header = parse_segment_header(&segment[..SEGMENT_FIELDS_SIZE])?;
            assert!(
                header.raw_len() as usize <= max_segment,
                "{what}: a segment over the largest length"
            );
        }
        assert!(
            readers_agree(&file, what).context("every reader refuses")? == source,
            "{what}"
        );
    }
    Ok(())
}

#[test]
fn edited_demos_round_trip() -> anyhow::Result<()> {
    let base = duel().unwrap_or_else(|| synthetic(&[(1, "{\"a\":"), (0, "1}")], false));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut refused = 0;
    for case in 0..30 {
        let mut demo = base.clone();
        for _ in 0..=rng.below(4) {
            let start = rng.below(demo.len());
            match rng.below(4) {
                0 => {
                    let end = (start + rng.below(2000)).min(demo.len());
                    demo.drain(start..end);
                }
                1 => {
                    let end = (start + rng.below(2000)).min(demo.len());
                    let chunk = demo[start..end].to_vec();
                    let at = rng.below(demo.len());
                    demo.splice(at..at, chunk);
                }
                2 => {
                    for _ in 0..=rng.below(16) {
                        let at = rng.below(demo.len());
                        demo[at] = rng.next() as u8;
                    }
                }
                _ => demo.truncate(start),
            }
        }
        if crate::encode::check_demo(&demo).is_err() {
            refused += 1;
            continue;
        }
        let file = crate::encode(&demo, 1)?;
        assert!(crate::decode(&file, 1)? == demo, "case {case}");
        assert!(crate::decode(&file, 2)? == demo, "case {case}, 2 threads");
    }
    // only cuts into the first frame make a non-demo
    assert!(refused <= 6, "{refused} of 30 refused");
    Ok(())
}

/// The correctness review's constructed demos, from `MVDZ_TEST_DATA/gen`.
#[test]
fn constructed_demos() -> anyhow::Result<()> {
    let Some(gen_dir) = test_data("gen") else {
        return Ok(());
    };
    let Ok(entries) = std::fs::read_dir(gen_dir) else {
        return Ok(());
    };
    let mut files: Vec<_> = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .collect();
    files.sort();
    for path in files {
        let source = std::fs::read(&path)?;
        if source.len() > 20 << 20 {
            continue;
        }
        let name = path
            .file_name()
            .context("a file name")?
            .to_string_lossy()
            .to_string();
        if crate::encode::check_demo(&source).is_err() {
            assert_matches!(crate::encode(&source, 1), Err(crate::Error::NotMvd(_)));
            let file = crate::encode::encode_any(&source, 1)?;
            assert!(crate::decode(&file, 2)? == source, "{name}");
            continue;
        }
        let file = crate::encode(&source, 1)?;
        assert!(crate::decode(&file, 2)? == source, "{name}");
        assert!(crate::decode(&file, 1)? == source, "{name}, 1 thread");
    }
    Ok(())
}

/// Hands out `read_len` bytes a call.
struct SlowInput<'a> {
    demo: Cursor<&'a [u8]>,
    read_len: usize,
}

impl Read for SlowInput<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let len = buffer.len().min(self.read_len);
        self.demo.read(&mut buffer[..len])
    }
}

impl Seek for SlowInput<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.demo.seek(position)
    }
}

#[test]
fn deterministic_output() -> anyhow::Result<()> {
    const LEAST_MEMORY: u64 = 0;
    let small = synthetic(&[(0, "{}")], false);
    let source = duel().unwrap_or_else(|| several_segments(&small));
    for source in [small, source] {
        let one_thread = crate::encode(&source, 1)?;
        for (max_threads, read_len) in [(3, 1), (1, 7), (4, 65536)] {
            for encoder in [
                crate::Encoder::new().max_threads(max_threads),
                crate::Encoder::new()
                    .max_threads(max_threads)
                    .max_memory(LEAST_MEMORY),
            ] {
                let input = SlowInput {
                    demo: Cursor::new(&source),
                    read_len,
                };
                let mut file = Vec::new();
                encoder.encode(input, &mut file)?;
                assert!(
                    file == one_thread,
                    "{} bytes, {max_threads} threads, {read_len} bytes a read: another output",
                    source.len()
                );
            }
        }
    }
    Ok(())
}

/// Pass 2 reads other bytes than pass 1 counted.
#[test]
fn input_that_changes_between_the_passes_is_refused() {
    struct ChangingInput {
        passes: [Cursor<Vec<u8>>; 2],
        pass: usize,
    }
    impl Read for ChangingInput {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.passes[self.pass].read(buffer)
        }
    }
    impl Seek for ChangingInput {
        fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
            self.pass = 1;
            Ok(0)
        }
    }
    let demo = synthetic(&[], false);
    let mut changed = demo.clone();
    changed[demo.len() / 2] ^= 1;
    let input = ChangingInput {
        passes: [Cursor::new(demo), Cursor::new(changed)],
        pass: 0,
    };
    assert_eq!(
        crate::Encoder::new().encode(input, std::io::sink()),
        Err(crate::Error::Io(std::io::ErrorKind::InvalidData))
    );
}

/// A server data message in the second segment changes the protocol, so the segments after
/// it, walked in parallel from the protocol before, are walked again (`Encoder::count`).
#[test]
fn protocol_change_mid_demo() -> anyhow::Result<()> {
    let small = synthetic(&[(0, "{}")], false);
    let copies = 3 * crate::segment::SEGMENT_TARGET_LEN / (2 * small.len());
    let mut demo = Demo(small.repeat(copies));
    let server_data = message(
        crate::protocol::SVC_SERVER_DATA,
        &[
            b"MVD1",
            &(1u32 << 30).to_le_bytes(),
            &crate::protocol::PROTOCOL_VERSION.to_le_bytes(),
            &1u32.to_le_bytes(),
            &c_string("qw"),
            &0u32.to_le_bytes(),
            &c_string("changed"),
            &[0; 40],
        ],
    );
    demo.frame(0, 6, &server_data);
    demo.0.extend_from_slice(&small.repeat(2 * copies));
    let file = crate::encode(&demo.0, 1)?;
    assert!(
        file == crate::encode(&demo.0, 4)?,
        "threads change the output"
    );
    assert!(crate::decode(&file, 4)? == demo.0);
    Ok(())
}

#[test]
fn models_block() -> anyhow::Result<()> {
    let source = usercmd_demo();
    let has_models = |file: &[u8]| anyhow::Ok(!parts(file)?.models.is_empty());
    let segmented = crate::encode(&several_segments(&source), 1)?;
    assert!(parts(&segmented)?.segments.len() > 2);
    assert!(has_models(&segmented)?, "models");
    assert!(
        readers_agree(&segmented, "tables").context("every reader refuses")?
            == several_segments(&source)
    );
    let one = crate::encode(&source, 1)?;
    assert_eq!(parts(&one)?.segments.len(), 1);
    assert!(
        has_models(&one)?,
        "a file of one segment has its models there too"
    );
    Ok(())
}
