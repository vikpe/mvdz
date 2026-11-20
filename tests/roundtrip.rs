//! Round trips through every reader, at the format's segment size and at
//! smaller ones; seeking; odd and non-MVD input; determinism.

mod common;
use common::*;
use std::io::{Cursor, Read};

fn check_all_ways(src: &[u8], what: &str, expect_transform: bool) {
    for seg in [0usize, 65536] {
        let c = cfg(seg);
        let (one, info) = unchecked(src, &c).unwrap();
        if expect_transform {
            assert_eq!(
                info.fallback_segments, 0,
                "{what}: seg {seg} fell back"
            );
        }
        let back = readers_agree(&one, what)
            .unwrap_or_else(|| panic!("{what}: seg {seg} refused"));
        assert!(back == src, "{what}: seg {seg}");
    }
}

#[test]
fn synthetic_demos() {
    check_all_ways(&synthetic(&[], false), "plain", true);
    check_all_ways(
        &synthetic(&[(1, "{\"a\":"), (0, "1}")], false),
        "demoinfo",
        true,
    );
    check_all_ways(&synthetic(&[], true), "truncated", true);
}

#[test]
fn sample_demos() {
    for (i, src) in samples(3).iter().enumerate() {
        check_all_ways(src, &format!("sample {i}"), true);
    }
}

/// Input that is not an MVD demo is refused by the encoder.
#[test]
fn empty_and_tiny_inputs() {
    let mut rng = Rng(7);
    let random: Vec<u8> = (0..10_000).map(|_| rng.next() as u8).collect();
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
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
        let e = compress(src, &cfg(0)).unwrap_err();
        assert!(matches!(e, mvdz::Error::NotMvd(_)), "{e}");
        // the unchecked encoder still round-trips any bytes
        check_all_ways(src, &format!("{src:?}"), false);
    }
}

/// Input that does not frame from the first byte, or stops framing early
/// (correctness review F2, robustness 7).
#[test]
fn non_mvd_input_streams() {
    let mut rng = Rng(7);
    let random: Vec<u8> = (0..10_000).map(|_| rng.next() as u8).collect();
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).unwrap();
    let mut inputs = vec![vec![1u8], random, text];
    if let Some(d) = duel() {
        inputs.push(d[..300].to_vec());
        // the first frame cut by one byte
        let n = 6 + u32::from_le_bytes(d[2..6].try_into().unwrap()) as usize;
        let mut cut = d[..n - 1].to_vec();
        cut.extend_from_slice(&d[n..200_000]);
        inputs.push(cut);
    }
    for (i, src) in inputs.iter().enumerate() {
        // the encoder refuses what is not a demo
        if mvdz::check_demo(src).is_err() {
            let r = compress(src, &cfg(1000));
            assert!(matches!(r, Err(mvdz::Error::NotMvd(_))), "input {i}");
            continue;
        }
        for seg in [1000usize, 1 << 20] {
            let c = compress(src, &cfg(seg)).unwrap();
            assert!(
                readers_agree(&c, "non-mvd").unwrap() == *src,
                "input {i} seg {seg}"
            );
        }
    }
}

/// A flood of svc_nop compresses beyond the ratio limit: the encoder must
/// code such segments in a form every reader accepts (correctness F4).
#[test]
fn ratio_limit_fallback() {
    let mut d = Demo(Vec::new());
    for _ in 0..400 {
        d.frame(0, 1, &[1u8; 10_000]);
    }
    let src = d.0;
    for seg in [0usize, 1 << 20] {
        let (c, info) = compress_info(&src, &cfg(seg)).unwrap();
        assert!(info.fallback_segments > 0);
        assert!(readers_agree(&c, "nop").unwrap() == src);
    }
}

#[test]
fn seeking() {
    let Some(src) = duel() else { return };
    for c in [
        compress(&src, &cfg(40_000)).unwrap(),
        compress(&src, &cfg(50_000)).unwrap(),
    ] {
        let mut r = mvdz::Reader::open(Cursor::new(&c), 2, &Default::default()).unwrap();
        let ix = r.index().unwrap().clone();
        assert!(ix.segs.len() > 3);
        for (i, s) in ix.segs.iter().enumerate() {
            assert_eq!(r.seek_segment(i).unwrap(), s.raw_off);
            let mut tail = Vec::new();
            r.read_to_end(&mut tail).unwrap();
            assert!(tail == src[s.raw_off as usize..], "segment {i}");
        }
        assert!(matches!(
            r.seek_segment(ix.segs.len()),
            Err(mvdz::Error::Invalid(_))
        ));
        let dur = ix.duration_ms;
        for ms in [0, 1, 1000, dur / 2, dur, dur + 1] {
            let (i, t, off) = r.seek_time(ms).unwrap();
            assert!(i == 0 || t < ms, "segment {i} starts before {ms}");
            assert!(i + 1 == ix.segs.len() || ix.segs[i + 1].t_start >= ms);
            let mut tail = Vec::new();
            r.read_to_end(&mut tail).unwrap();
            assert!(tail == src[off as usize..]);
        }
    }
}

/// Edited demos (frames dropped, duplicated, bytes changed, cuts) take the
/// escape and tail paths; all of them round-trip.
#[test]
fn edited_demos_round_trip() {
    let base = duel().unwrap_or_else(|| synthetic(&[(1, "{\"a\":"), (0, "1}")], false));
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut refused = 0;
    for case in 0..30 {
        let mut d = base.clone();
        for _ in 0..1 + rng.below(4) {
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
                    for _ in 0..1 + rng.below(16) {
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
        let c = compress(&d, &cfg(seg)).unwrap();
        assert!(mvdz::decompress(&c, 1).unwrap() == d, "case {case}");
        assert!(mvdz::decompress(&c, 2).unwrap() == d, "case {case}, 2 threads");
    }
    // edits within the demo keep it a demo: only cuts into the first frame
    // are refused
    assert!(refused <= 6, "{refused} of 30 refused");
}

/// The constructed demos of the correctness review (float coordinates,
/// demoinfo variants, odd masks, ...), when present.
#[test]
fn constructed_demos() {
    let Some(gdir) = test_data("gen") else { return };
    let Ok(dir) = std::fs::read_dir(gdir) else {
        return;
    };
    let mut files: Vec<_> = dir.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    files.sort();
    for p in files {
        let src = std::fs::read(&p).unwrap();
        if src.len() > 20 << 20 {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        // a few have no MVD start (only dem_set frames, a cut first
        // frame): refused, and still round-trip through the unchecked
        // encoder
        if mvdz::check_demo(&src).is_err() {
            assert!(matches!(
                compress(&src, &cfg(0)),
                Err(mvdz::Error::NotMvd(_))
            ));
            let (c, _) = unchecked(&src, &cfg(50_000)).unwrap();
            assert!(mvdz::decompress(&c, 2).unwrap() == src, "{name}");
            continue;
        }
        for seg in [0usize, 50_000] {
            let c = compress(&src, &cfg(seg)).unwrap();
            assert!(
                mvdz::decompress(&c, 2).unwrap() == src,
                "{name} seg {seg}"
            );
            assert!(
                mvdz::decompress(&c, 1).unwrap() == src,
                "{name} seg {seg}, 1 thread"
            );
        }
    }
}

#[test]
fn deterministic_output() {
    let src = duel().unwrap_or_else(|| synthetic(&[(0, "{}")], false));
    for seg in [0usize, 30_000] {
        let a = compress(&src, &cfg(seg)).unwrap();
        let b = compress(
            &src,
            &Cfg {
                threads: 3,
                ..cfg(seg)
            },
        )
        .unwrap();
        assert!(a == b, "seg {seg}: threads change the output");
    }
}

/// The models block (FORMAT.md §2.6): every file with a segment has its
/// rANS models there, whether it has one segment or several. Every reader
/// handles it.
#[test]
fn file_tables() {
    let src = usercmd_demo();
    let has = |c: &[u8]| !parts(c).models.is_empty();
    let (seg, info) = compress_info(&src, &cfg(8192)).unwrap();
    assert!(info.segments > 2);
    assert!(has(&seg), "models");
    assert_eq!(info.fallback_segments, 0, "every segment is transformed");
    assert!(readers_agree(&seg, "tables").unwrap() == src);
    let one = compress(&src, &cfg(0)).unwrap();
    assert_eq!(parts(&one).segs.len(), 1);
    assert!(has(&one), "a file of one segment has its models there too");
}
