use super::common::*;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::assert_matches;
use std::io::Cursor;

fn check_all_ways(src: &[u8], what: &str) -> anyhow::Result<()> {
    for (how, src) in [("as is", src.to_vec()), ("repeated", several_segments(src))] {
        let (one, info) = compress_unchecked(&src)?;
        assert_eq!(
            info.self_check_fallbacks, 0,
            "{what} {how}: the transform is lossy"
        );
        let back = readers_agree(&one, what).with_context(|| format!("{what} {how}: refused"))?;
        assert!(back == src, "{what} {how}");
    }
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
    for (index, src) in samples(3).iter().enumerate() {
        check_all_ways(src, &format!("sample {index}"))?;
    }
    Ok(())
}

fn read_until_error(file: &[u8], threads: usize) -> (Vec<u8>, Option<crate::Error>) {
    let mut out = Vec::new();
    let max_memory = crate::budget::DEFAULT_MAX_MEMORY;
    let mut reader =
        match crate::decode::Decoder::with_max_memory(Cursor::new(file), threads, max_memory) {
            Ok(reader) => reader,
            Err(error) => return (out, Some(error)),
        };
    let mut buf = [0u8; 4096];
    loop {
        match reader.read_demo(&mut buf) {
            Ok(0) => return (out, None),
            Ok(len) => out.extend_from_slice(&buf[..len]),
            Err(error) => return (out, Some(error)),
        }
    }
}

#[test]
fn sequential_reader_threads_agree() -> anyhow::Result<()> {
    const CUTS: usize = 7;
    let demo = several_segments(&synthetic(&[], false));
    let file = compress(&demo)?;
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
    let file = compress(&demo)?;
    let mut file_parts = parts(&file)?;
    let mut entries = models_entries(&file_parts.models)?;
    anyhow::ensure!(
        entries.iter().all(|entry| entry.0 != crate::streams::UC),
        "the demo has no usercmd blocks"
    );
    let structural = entries
        .iter()
        .find(|entry| crate::streams::is_lazy(entry.0))
        .context("a structural stream's model")?
        .1
        .clone();
    entries.push((crate::streams::UC, structural));
    entries.sort_by_key(|entry| entry.0);
    file_parts.models = models_body(&entries);
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
    for src in inputs {
        let refusal = compress(src).err();
        assert_matches!(refusal, Some(crate::Error::NotMvd(_)), "{refusal:?}");
        check_all_ways(src, &format!("{src:?}"))?;
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
    for (index, src) in inputs.iter().enumerate() {
        if crate::encode::check_demo(src).is_err() {
            let refusal = compress(src);
            assert_matches!(refusal, Err(crate::Error::NotMvd(_)), "input {index}");
            continue;
        }
        for src in [src.clone(), several_segments(src)] {
            let file = compress(&src)?;
            assert!(
                readers_agree(&file, "non-mvd").context("every reader refuses")? == src,
                "input {index}, {} bytes",
                src.len()
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
        for (how, src) in [
            ("as is", demo.clone()),
            ("repeated", several_segments(&demo)),
        ] {
            let (file, info) = all_tail(&src)?;
            assert_eq!(
                info.unstructured_len,
                src.len(),
                "{what} {how}: every byte in the tail"
            );
            let back =
                readers_agree(&file, what).with_context(|| format!("{what} {how}: refused"))?;
            assert!(back == src, "{what} {how}");
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
    let src = demo.0;
    let file = compress(&src)?;
    assert!(parts(&file)?.segments.len() > 1);
    assert!(readers_agree(&file, "nop").context("every reader refuses")? == src);
    Ok(())
}

#[test]
fn segments_are_capped() -> anyhow::Result<()> {
    let max_segment = crate::format::MAX_SEGMENT_RAW as usize;
    let mut rng = Rng(0x00ca_99ed);
    let unframed: Vec<u8> = Vec::from_fn(3 * max_segment, |_| rng.next() as u8);
    let mut oversized = Demo(synthetic(&[], false));
    oversized.frame(0, 1, &vec![1u8; max_segment + max_segment / 2]);
    oversized.0.extend_from_slice(&synthetic(&[], false));
    for (what, src, segments) in [
        ("unframed", unframed, 3),
        ("oversized frame", oversized.0, 2),
    ] {
        let (file, _) = compress_unchecked(&src)?;
        let layout = scan(&file, crate::budget::DEFAULT_MAX_MEMORY)?;
        let count = layout.segments.len();
        assert!(count >= segments, "{what}: {count} segments");
        assert!(
            layout
                .segments
                .iter()
                .all(|(_, segment)| segment.raw_len as usize <= max_segment),
            "{what}: a segment over the largest length"
        );
        assert!(
            readers_agree(&file, what).context("every reader refuses")? == src,
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
        let file = compress(&demo)?;
        assert!(decompress(&file, 1)? == demo, "case {case}");
        assert!(decompress(&file, 2)? == demo, "case {case}, 2 threads");
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
        let src = std::fs::read(&path)?;
        if src.len() > 20 << 20 {
            continue;
        }
        let name = path
            .file_name()
            .context("a file name")?
            .to_string_lossy()
            .to_string();
        if crate::encode::check_demo(&src).is_err() {
            assert_matches!(compress(&src), Err(crate::Error::NotMvd(_)));
            let (file, _) = compress_unchecked(&src)?;
            assert!(decompress(&file, 2)? == src, "{name}");
            continue;
        }
        let file = compress(&src)?;
        assert!(decompress(&file, 2)? == src, "{name}");
        assert!(decompress(&file, 1)? == src, "{name}, 1 thread");
    }
    Ok(())
}

#[test]
fn deterministic_output() -> anyhow::Result<()> {
    let small = synthetic(&[(0, "{}")], false);
    let src = duel().unwrap_or_else(|| several_segments(&small));
    for src in [small, src] {
        let one_thread = crate::encode(&src, 1)?;
        let three_threads = crate::encode(&src, 3)?;
        assert!(
            one_thread == three_threads,
            "{} bytes: threads change the output",
            src.len()
        );
    }
    Ok(())
}

/// A server data message in the second segment changes the proto, so the segments after
/// it, walked in parallel from the proto before, are walked again (`encode::run_first_pass`).
#[test]
fn proto_change_mid_demo() -> anyhow::Result<()> {
    let small = synthetic(&[(0, "{}")], false);
    let copies = 3 * crate::segment::SEGMENT_TARGET_RAW / (2 * small.len());
    let mut demo = Demo(small.repeat(copies));
    let server_data = message(
        crate::proto::SVC_SERVER_DATA,
        &[
            b"FTE2",
            &(1u32 << 30).to_le_bytes(),
            &crate::proto::PROTOCOL_VERSION.to_le_bytes(),
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
    assert!(decompress(&file, 4)? == demo.0);
    Ok(())
}

#[test]
fn encoder_matches_encode() -> anyhow::Result<()> {
    let src = several_segments(&synthetic(&[(0, "{}")], false));
    let mut encoder = crate::Encoder::new(Vec::new()).max_threads(2);
    for chunk in src.chunks(1000) {
        std::io::Write::write_all(&mut encoder, chunk)?;
    }
    let file = encoder.finish()?;
    assert!(file == crate::encode(&src, 2)?);
    assert!(decompress(&file, 1)? == src);
    assert_matches!(
        crate::Encoder::new(Vec::new()).finish(),
        Err(crate::Error::NotMvd(_))
    );
    Ok(())
}

#[test]
fn models_block() -> anyhow::Result<()> {
    let src = usercmd_demo();
    let has_models = |file: &[u8]| anyhow::Ok(!parts(file)?.models.is_empty());
    let segmented = compress(&several_segments(&src))?;
    assert!(parts(&segmented)?.segments.len() > 2);
    assert!(has_models(&segmented)?, "models");
    assert!(
        readers_agree(&segmented, "tables").context("every reader refuses")?
            == several_segments(&src)
    );
    let one = compress(&src)?;
    assert_eq!(parts(&one)?.segments.len(), 1);
    assert!(
        has_models(&one)?,
        "a file of one segment has its models there too"
    );
    Ok(())
}
