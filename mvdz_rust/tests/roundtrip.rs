//! Round trips through every reader, in one segment and in several; odd
//! and non-MVD input; determinism.

mod common;
use anyhow::Context as _;
use common::*;
use std::assert_matches;

fn check_all_ways(src: &[u8], what: &str) -> anyhow::Result<()> {
    for (how, src) in [("as is", src.to_vec()), ("repeated", several_segments(src))] {
        let (one, _) = unchecked(&src)?;
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
    for (i, src) in samples(3).iter().enumerate() {
        check_all_ways(src, &format!("sample {i}"))?;
    }
    Ok(())
}

/// Input that is not an MVD demo is refused by the encoder.
/// The bytes a sequential reader at `threads` threads yields before its
/// first error, and that error.
fn read_until_error(f: &[u8], threads: usize) -> (Vec<u8>, Option<mvdz::Error>) {
    let mut out = Vec::new();
    let l = mvdz::Limits::default();
    let mut r = match mvdz::Reader::new(std::io::Cursor::new(f), threads, &l) {
        Ok(r) => r,
        Err(e) => return (out, Some(e)),
    };
    let mut buf = [0u8; 4096];
    loop {
        match r.read_demo(&mut buf) {
            Ok(0) => return (out, None),
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => return (out, Some(e)),
        }
    }
}

/// A sequential reader that decodes several segments at once yields what
/// one decoding a segment at a time does: the segments before a cut or a
/// damaged segment, then the same error.
#[test]
fn sequential_reader_threads_agree() -> anyhow::Result<()> {
    let demo = several_segments(&synthetic(&[], false));
    let f = compress(&demo)?;
    let p = parts(&f)?;
    anyhow::ensure!(p.segs.len() >= 4, "{} segments", p.segs.len());
    let mut cases = Vec::new();
    for k in 1..8 {
        cases.push(f[..f.len() * k / 8].to_vec());
    }
    for i in 0..p.segs.len() {
        let mut q = p.clone();
        let s = q.segs[i].last_mut().context("a segment body")?;
        *s ^= 0x55;
        cases.push(q.build());
    }
    let mut partial = 0;
    for (k, c) in cases.iter().enumerate() {
        let one = read_until_error(c, 1);
        assert!(demo.starts_with(&one.0), "case {k}");
        if k < 7 {
            assert!(one.1.is_some(), "cut {k} is refused");
        }
        if one.1.is_some() && !one.0.is_empty() {
            partial += 1;
        }
        for threads in [2, 4] {
            assert!(
                read_until_error(c, threads) == one,
                "case {k}, {threads} threads"
            );
        }
    }
    // the cases include errors after some segments were delivered
    assert!(partial >= 2, "{partial}");
    Ok(())
}

#[test]
fn empty_and_tiny_inputs() -> anyhow::Result<()> {
    let mut rng = Rng(7);
    let random: Vec<u8> = Vec::from_fn(10_000, |_| rng.next() as u8);
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md"))?;
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
        assert_matches!(refusal, Some(mvdz::Error::NotMvd(_)), "{refusal:?}");
        // the unchecked encoder still round-trips any bytes
        check_all_ways(src, &format!("{src:?}"))?;
    }
    Ok(())
}

/// Input that does not frame from the first byte, or stops framing early
/// (correctness review F2, robustness 7).
#[test]
fn non_mvd_input_streams() -> anyhow::Result<()> {
    let mut rng = Rng(7);
    let random: Vec<u8> = Vec::from_fn(10_000, |_| rng.next() as u8);
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md"))?;
    let mut inputs = vec![vec![1u8], random, text];
    if let Some(d) = duel() {
        inputs.push(d[..300].to_vec());
        // the first frame cut by one byte
        let n = 6 + u32::from_le_bytes(d[2..6].try_into()?) as usize;
        let mut cut = d[..n - 1].to_vec();
        cut.extend_from_slice(&d[n..200_000]);
        inputs.push(cut);
    }
    for (i, src) in inputs.iter().enumerate() {
        // the encoder refuses what is not a demo
        if mvdz::check_demo(src).is_err() {
            let r = compress(src);
            assert_matches!(r, Err(mvdz::Error::NotMvd(_)), "input {i}");
            continue;
        }
        for src in [src.clone(), several_segments(src)] {
            let c = compress(&src)?;
            assert!(
                readers_agree(&c, "non-mvd").context("every reader refuses")? == src,
                "input {i}, {} bytes",
                src.len()
            );
        }
    }
    Ok(())
}

/// Bytes that frame, written as all tail (the encoder's fallback for a
/// segment the transform cannot code): every reader decodes them, in one
/// segment and in several.
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
            let (c, info) = all_tail(&src)?;
            assert_eq!(
                info.escaped,
                src.len(),
                "{what} {how}: every byte in the tail"
            );
            assert_eq!(info.segments, parts(&c)?.segs.len());
            let back = readers_agree(&c, what).with_context(|| format!("{what} {how}: refused"))?;
            assert!(back == src, "{what} {how}");
        }
    }
    Ok(())
}

/// A flood of `svc_nop` codes in very few bytes. No rule ties a segment's
/// coded length to its raw length from below: the transform codes it and
/// every reader accepts it.
#[test]
fn nop_flood_round_trips() -> anyhow::Result<()> {
    let mut d = Demo(Vec::new());
    for _ in 0..400 {
        d.frame(0, 1, &[1u8; 10_000]);
    }
    let src = d.0;
    let (c, info) = compress_info(&src)?;
    assert!(info.segments > 1);
    assert!(readers_agree(&c, "nop").context("every reader refuses")? == src);
    Ok(())
}

/// Data that does not frame, and a frame larger than a segment may be, are
/// cut at the largest segment length; every reader decodes them.
#[test]
fn segments_are_capped() -> anyhow::Result<()> {
    let max_segment = mvdz::format::MAX_SEG_RAW as usize;
    let mut rng = Rng(0x00ca_99ed);
    let unframed: Vec<u8> = Vec::from_fn(3 * max_segment, |_| rng.next() as u8);
    let mut oversized = Demo(synthetic(&[], false));
    oversized.frame(0, 1, &vec![1u8; max_segment + max_segment / 2]);
    oversized.0.extend_from_slice(&synthetic(&[], false));
    for (what, src, segments) in [
        ("unframed", unframed, 3),
        ("oversized frame", oversized.0, 2),
    ] {
        let (file, info) = unchecked(&src)?;
        assert!(
            info.segments >= segments,
            "{what}: {} segments",
            info.segments
        );
        let index = mvdz::read_info(std::io::Cursor::new(&file))?.index;
        assert!(
            index
                .segs
                .iter()
                .all(|seg| seg.raw_len as usize <= max_segment),
            "{what}: a segment over the largest length"
        );
        assert!(
            readers_agree(&file, what).context("every reader refuses")? == src,
            "{what}"
        );
    }
    Ok(())
}

/// Edited demos (frames dropped, duplicated, bytes changed, cuts) take the
/// escape and tail paths; all of them round-trip.
#[test]
fn edited_demos_round_trip() -> anyhow::Result<()> {
    let base = duel().unwrap_or_else(|| synthetic(&[(1, "{\"a\":"), (0, "1}")], false));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut refused = 0;
    for case in 0..30 {
        let mut d = base.clone();
        for _ in 0..=rng.below(4) {
            let i = rng.below(d.len());
            match rng.below(4) {
                0 => {
                    let j = (i + rng.below(2000)).min(d.len());
                    d.drain(i..j);
                }
                1 => {
                    let j = (i + rng.below(2000)).min(d.len());
                    let chunk = d[i..j].to_vec();
                    let k = rng.below(d.len());
                    d.splice(k..k, chunk);
                }
                2 => {
                    for _ in 0..=rng.below(16) {
                        let k = rng.below(d.len());
                        d[k] = rng.next() as u8;
                    }
                }
                _ => d.truncate(i),
            }
        }
        // an edit that destroys the first frame makes it a non-demo
        if mvdz::check_demo(&d).is_err() {
            refused += 1;
            continue;
        }
        let c = compress(&d)?;
        assert!(mvdz::decompress(&c, 1)? == d, "case {case}");
        assert!(mvdz::decompress(&c, 2)? == d, "case {case}, 2 threads");
    }
    // edits within the demo keep it a demo: only cuts into the first frame
    // are refused
    assert!(refused <= 6, "{refused} of 30 refused");
    Ok(())
}

/// The constructed demos of the correctness review (float coordinates,
/// demoinfo variants, odd masks, ...), when present.
#[test]
fn constructed_demos() -> anyhow::Result<()> {
    let Some(gdir) = test_data("gen") else {
        return Ok(());
    };
    let Ok(dir) = std::fs::read_dir(gdir) else {
        return Ok(());
    };
    let mut files: Vec<_> = dir
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .collect();
    files.sort();
    for p in files {
        let src = std::fs::read(&p)?;
        if src.len() > 20 << 20 {
            continue;
        }
        let name = p
            .file_name()
            .context("a file name")?
            .to_string_lossy()
            .to_string();
        // a few have no MVD start (only dem_set frames, a cut first
        // frame): refused, and still round-trip through the unchecked
        // encoder
        if mvdz::check_demo(&src).is_err() {
            assert_matches!(compress(&src), Err(mvdz::Error::NotMvd(_)));
            let (c, _) = unchecked(&src)?;
            assert!(mvdz::decompress(&c, 2)? == src, "{name}");
            continue;
        }
        let c = compress(&src)?;
        assert!(mvdz::decompress(&c, 2)? == src, "{name}");
        assert!(mvdz::decompress(&c, 1)? == src, "{name}, 1 thread");
    }
    Ok(())
}

#[test]
fn deterministic_output() -> anyhow::Result<()> {
    let small = synthetic(&[(0, "{}")], false);
    let src = duel().unwrap_or_else(|| several_segments(&small));
    for src in [small, src] {
        let threads = |threads| mvdz::Config { threads };
        let a = mvdz::compress(&src, &threads(1))?;
        let b = mvdz::compress(&src, &threads(3))?;
        assert!(a == b, "{} bytes: threads change the output", src.len());
    }
    Ok(())
}

/// The models block (FORMAT.md §2.6): every file with a segment has its
/// rANS models there, whether it has one segment or several. Every reader
/// handles it.
#[test]
fn file_tables() -> anyhow::Result<()> {
    let src = usercmd_demo();
    let has = |c: &[u8]| anyhow::Ok(!parts(c)?.models.is_empty());
    let (seg, info) = compress_info(&several_segments(&src))?;
    assert!(info.segments > 2);
    assert!(has(&seg)?, "models");
    assert!(
        readers_agree(&seg, "tables").context("every reader refuses")? == several_segments(&src)
    );
    let one = compress(&src)?;
    assert_eq!(parts(&one)?.segs.len(), 1);
    assert!(has(&one)?, "a file of one segment has its models there too");
    Ok(())
}
