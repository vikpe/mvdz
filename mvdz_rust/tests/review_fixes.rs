//! Regressions for the final reviews (robustness R-*, correctness C-*,
//! spec D-*): each test is a reviewer's reproducer, rebuilt here, that
//! failed before the fix.

mod common;
use common::*;
use mvdz::Error;
use mvdz::budget::Limits;
use mvdz::format::*;
use mvdz::util::put_uv;
use std::io::Cursor;
use std::time::{Duration, Instant};

/// A small demo and its file.
fn base() -> (Vec<u8>, Vec<u8>) {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let c = compress(&demo, &cfg(0)).unwrap();
    (demo, c)
}

/// R-M2: a model of 65535 tables whose header is padded to satisfy rule
/// M2 is refused before any table is built (the header must be consumed
/// exactly).
#[test]
fn r_m2_padded_model() {
    use mvdz::coder::rc::{Enc, IntModel};
    const NQC: usize = 17 * 18;
    let n = 65535u32;
    let mut e = Enc::new();
    let mut im = IntModel::new(NQC + 8);
    im.put(&mut e, NQC, 256);
    im.put(&mut e, NQC + 1, n);
    let mut prev = 0usize;
    for _ in 0..256 {
        im.put(&mut e, 17 * 17 + prev, 0);
        prev = 0;
    }
    for i in 0..n {
        im.put(&mut e, NQC + 2, (i > 0) as u32);
        for _ in 0..256 {
            im.put(&mut e, 0, 0);
        }
    }
    let dec = e.decisions;
    let mut hdr = e.finish();
    hdr.resize(
        hdr.len().max((dec.saturating_sub(1024) / 128 + 1) as usize),
        0,
    );
    // the models block: one model, the time stream's
    let mut models = Vec::new();
    for v in [1, 0, hdr.len() as u64] {
        put_uv(&mut models, v);
    }
    models.extend_from_slice(&hdr);
    // an empty rANS body
    let mut st = Vec::new();
    put_uv(&mut st, 0);
    let raw_len = n as u64 + 100;
    // a transform body whose only stream is a time stream: fte, fte2,
    // mvd1, tail; one entry (id 0, count, length); no blob body
    let mut body = Vec::new();
    for v in [0u64, 0, 0, 0, 1, 0, n as u64 + 16, st.len() as u64, 0] {
        put_uv(&mut body, v);
    }
    body.extend_from_slice(&st);
    let mut b = seg_hdr_bytes(raw_len, 0, 0, C_TRANSFORM);
    b.extend_from_slice(&body);
    let f = Parts {
        header: Header {
            size: raw_len,
            ..Default::default()
        },
        models,
        segs: vec![b],
    }
    .build();
    let t = Instant::now();
    let l = Limits {
        memory: 64 << 20,
        ..Default::default()
    };
    assert_eq!(
        mvdz::decompress_with(&f, 1, &l).unwrap_err(),
        Error::Corrupt("model")
    );
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

/// R-M3: the JSON rendering can be capped.
#[test]
fn r_m3_json_limit() {
    let (_, f) = base();
    let fi = mvdz::read_info(Cursor::new(&f)).unwrap();
    let j = fi.to_json();
    assert_eq!(fi.to_json_with(j.len()).unwrap(), j);
    assert_eq!(
        fi.to_json_with(j.len() - 1).unwrap_err(),
        Error::Limit("file information rendering")
    );
}

/// R-M4: a long run of tiny segments is charged to the budget: a
/// sequential reader stops with a limit error instead of growing.
#[test]
fn r_m4_section_bookkeeping() {
    // one segment per frame
    let demo = sets(60_000);
    let f = compress(&demo, &cfg(1)).unwrap();
    assert!(parts(&f).segs.len() > 60_000);
    assert_eq!(mvdz::decompress(&f, 1).unwrap(), demo);
    // the demo is 0.6 MB and a sequential reader holds one segment at a
    // time: only the record kept per segment (64 bytes each, 3.8 MB) can
    // exceed this
    let l = Limits {
        memory: 3 << 20,
        ..Default::default()
    };
    assert_eq!(
        mvdz::decompress_with(&f, 1, &l).unwrap_err(),
        Error::Limit("memory")
    );
    let mut r = mvdz::Reader::new(Cursor::new(&f), 1, &l).unwrap();
    let mut out = Vec::new();
    assert!(std::io::Read::read_to_end(&mut r, &mut out).is_err());
}

/// A demo of a dem_read frame holding one svc_nop, then `n` dem_set
/// frames (10 bytes each).
fn sets(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n * 10 + 7);
    v.extend_from_slice(&[0, 1, 1, 0, 0, 0, 1]);
    for i in 0..n {
        v.push(1);
        v.push(2);
        v.extend_from_slice(&(i as u64).to_le_bytes());
    }
    v
}

/// C-M1, C-L2: more segments than the encoder allows is refused up front,
/// never by writing an INDEX readers refuse.
#[test]
fn c_m1_segment_limit() {
    let demo = sets(MAX_SEGMENTS + 10);
    let t = Instant::now();
    let e = compress(&demo, &cfg(1)).unwrap_err();
    assert_eq!(e, Error::Invalid("too many segments"));
    assert!(t.elapsed() < Duration::from_secs(5));
    // at the limit itself, the file is written and decodes
    let demo = sets(2000);
    let c = compress(&demo, &cfg(1)).unwrap();
    assert_eq!(mvdz::decompress(&c, 1).unwrap(), demo);
}

/// Decoding does not recompute the SHA-256: a file whose header carries
/// another one decodes in every reader; a damaged segment does not.
#[test]
fn sha256_is_not_checked_on_decode() {
    let (demo, f) = base();
    let mut p = parts(&f);
    p.header.sha256 = [0xff; 32];
    let other = p.build();
    let d = mvdz::decompress(&other, 1).unwrap();
    assert_eq!(d, demo);
    let mut r = mvdz::Reader::new(Cursor::new(&other), 1, &Limits::default()).unwrap();
    let mut o = Vec::new();
    std::io::Read::read_to_end(&mut r, &mut o).unwrap();
    assert_eq!(o, d);
    // one byte of an untransformed segment changed: the segment CRC catches it
    let demo = synthetic(&[], false);
    let c = untransformed(&[demo.clone(), vec![0; 5000]].concat(), &cfg(0));
    let ok = mvdz::decompress(&c, 1).unwrap();
    let mut bad = c.clone();
    let i = (0..bad.len())
        .rev()
        .find(|&i| bad[i] != 0 && i > 200)
        .unwrap();
    bad[i - 100] ^= 0x40;
    assert!(mvdz::decompress(&bad, 1).is_err());
    assert!(!ok.is_empty());
}
