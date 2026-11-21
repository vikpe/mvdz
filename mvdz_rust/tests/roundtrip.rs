//! Round trips through every reader, at the format's segment size and at
//! smaller ones; odd and non-MVD input; determinism.

mod common;
use anyhow::Context as _;
use common::*;
use std::assert_matches;

fn check_all_ways(src: &[u8], what: &str, expect_transform: bool) -> anyhow::Result<()> {
    for seg in [0usize, 65536] {
        let c = cfg(seg);
        let (one, info) = unchecked(src, &c)?;
        if expect_transform {
            assert_eq!(info.fallback_segments, 0, "{what}: seg {seg} fell back");
        }
        let back =
            readers_agree(&one, what).with_context(|| format!("{what}: seg {seg} refused"))?;
        assert!(back == src, "{what}: seg {seg}");
    }
    Ok(())
}

#[test]
fn synthetic_demos() -> anyhow::Result<()> {
    check_all_ways(&synthetic(&[], false), "plain", true)?;
    check_all_ways(
        &synthetic(&[(1, "{\"a\":"), (0, "1}")], false),
        "demoinfo",
        true,
    )?;
    check_all_ways(&synthetic(&[], true), "truncated", true)?;
    Ok(())
}

#[test]
fn sample_demos() -> anyhow::Result<()> {
    for (i, src) in samples(3).iter().enumerate() {
        check_all_ways(src, &format!("sample {i}"), true)?;
    }
    Ok(())
}

/// Input that is not an MVD demo is refused by the encoder.
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
        let refusal = compress(src, &cfg(0)).err();
        assert_matches!(refusal, Some(mvdz::Error::NotMvd(_)), "{refusal:?}");
        // the unchecked encoder still round-trips any bytes
        check_all_ways(src, &format!("{src:?}"), false)?;
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
            let r = compress(src, &cfg(1000));
            assert_matches!(r, Err(mvdz::Error::NotMvd(_)), "input {i}");
            continue;
        }
        for seg in [1000usize, 1 << 20] {
            let c = compress(src, &cfg(seg))?;
            assert!(
                readers_agree(&c, "non-mvd").context("every reader refuses")? == *src,
                "input {i} seg {seg}"
            );
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
    for seg in [0usize, 1 << 20] {
        let (c, info) = compress_info(&src, &cfg(seg))?;
        assert_eq!(info.fallback_segments, 0);
        assert!(readers_agree(&c, "nop").context("every reader refuses")? == src);
    }
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
        let (file, info) = unchecked(&src, &cfg(0))?;
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
        let seg = [0, 4096, 65536][case % 3];
        // an edit that destroys the first frame makes it a non-demo
        if mvdz::check_demo(&d).is_err() {
            refused += 1;
            continue;
        }
        let c = compress(&d, &cfg(seg))?;
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
            assert_matches!(compress(&src, &cfg(0)), Err(mvdz::Error::NotMvd(_)));
            let (c, _) = unchecked(&src, &cfg(50_000))?;
            assert!(mvdz::decompress(&c, 2)? == src, "{name}");
            continue;
        }
        for seg in [0usize, 50_000] {
            let c = compress(&src, &cfg(seg))?;
            assert!(mvdz::decompress(&c, 2)? == src, "{name} seg {seg}");
            assert!(
                mvdz::decompress(&c, 1)? == src,
                "{name} seg {seg}, 1 thread"
            );
        }
    }
    Ok(())
}

#[test]
fn deterministic_output() -> anyhow::Result<()> {
    let src = duel().unwrap_or_else(|| synthetic(&[(0, "{}")], false));
    for seg in [0usize, 30_000] {
        let a = compress(&src, &cfg(seg))?;
        let b = compress(
            &src,
            &Cfg {
                threads: 3,
                ..cfg(seg)
            },
        )?;
        assert!(a == b, "seg {seg}: threads change the output");
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
    let (seg, info) = compress_info(&src, &cfg(8192))?;
    assert!(info.segments > 2);
    assert!(has(&seg)?, "models");
    assert_eq!(info.fallback_segments, 0, "every segment is transformed");
    assert!(readers_agree(&seg, "tables").context("every reader refuses")? == src);
    let one = compress(&src, &cfg(0))?;
    assert_eq!(parts(&one)?.segs.len(), 1);
    assert!(has(&one)?, "a file of one segment has its models there too");
    Ok(())
}
