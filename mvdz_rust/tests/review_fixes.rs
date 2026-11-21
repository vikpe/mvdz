//! Regressions for the final reviews (robustness R-*, correctness C-*,
//! spec D-*): each test is a reviewer's reproducer, rebuilt here, that
//! failed before the fix.

mod common;
use anyhow::Context as _;
use common::*;
use mvdz::Error;
use mvdz::Limits;
use mvdz::format::*;
use mvdz::util::put_u32;
use std::io::Cursor;
use std::time::{Duration, Instant};

/// A small demo and its file.
fn base() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let c = compress(&demo)?;
    Ok((demo, c))
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
        im.put(&mut e, NQC + 2, u32::from(i > 0));
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
    for v in [1, 0, hdr.len()] {
        put_u32(&mut models, v);
    }
    models.extend_from_slice(&hdr);
    // an empty rANS body
    let mut st = Vec::new();
    put_u32(&mut st, 0);
    let raw_len = n + 100;
    // a transform body whose only stream is a time stream: fte, fte2,
    // mvd1, tail; one entry (id 0, count, length); no blob body
    let mut body = Vec::new();
    for v in [0, 0, 0, 0, 1, 0, n as usize + 16, st.len(), 0] {
        put_u32(&mut body, v);
    }
    body.extend_from_slice(&st);
    let mut b = seg_hdr_bytes(raw_len, 0, 0);
    b.extend_from_slice(&body);
    let f = Parts {
        header: Header {
            size: u64::from(raw_len),
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
        mvdz::decompress_with(&f, 1, &l).err(),
        Some(Error::Corrupt("model"))
    );
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

/// R-M4: a long run of tiny segments is charged to the budget: a
/// sequential reader stops with a limit error instead of growing.
#[test]
fn r_m4_section_bookkeeping() -> anyhow::Result<()> {
    // one segment per frame. The encoder cuts at 256 KiB, so the file is
    // built by hand: the segment of a one-frame demo, 60,000 times, under
    // the header of the whole demo
    let frame = [0, 1, 1, 0, 0, 0, 1]; // dem_read holding one svc_nop
    let n = 60_000;
    let demo = frame.repeat(n);
    let one = parts(&compress(&frame)?)?;
    assert_eq!(one.segs.len(), 1);
    let f = Parts {
        header: parts(&compress(&demo)?)?.header,
        models: one.models,
        segs: vec![one.segs[0].clone(); n],
    }
    .build();
    assert_eq!(mvdz::decompress(&f, 1)?, demo);
    // the demo is 0.4 MB and a sequential reader holds one segment at a
    // time: only the record kept per segment (64 bytes each, 3.8 MB) can
    // exceed this
    let l = Limits {
        memory: 3 << 20,
        ..Default::default()
    };
    assert_eq!(
        mvdz::decompress_with(&f, 1, &l).err(),
        Some(Error::Limit("memory"))
    );
    let mut r = mvdz::Reader::new(Cursor::new(&f), 1, &l)?;
    let mut out = Vec::new();
    assert!(std::io::Read::read_to_end(&mut r, &mut out).is_err());
    Ok(())
}

/// Decoding does not recompute the SHA-256: a file whose header carries
/// another one decodes in every reader; a damaged segment does not.
#[test]
fn sha256_is_not_checked_on_decode() -> anyhow::Result<()> {
    let (demo, f) = base()?;
    let mut p = parts(&f)?;
    p.header.sha256 = [0xff; 32];
    let other = p.build();
    let d = mvdz::decompress(&other, 1)?;
    assert_eq!(d, demo);
    let mut r = mvdz::Reader::new(Cursor::new(&other), 1, &Limits::default())?;
    let mut o = Vec::new();
    std::io::Read::read_to_end(&mut r, &mut o)?;
    assert_eq!(o, d);
    // one byte of a segment changed: decoding refuses it
    let demo = synthetic(&[], false);
    let c = compress(&[demo, vec![0; 5000]].concat())?;
    let ok = mvdz::decompress(&c, 1)?;
    let mut bad = c;
    let i = (0..bad.len())
        .rev()
        .find(|&i| bad[i] != 0 && i > 200)
        .context("a non-zero byte")?;
    bad[i - 100] ^= 0x40;
    assert!(mvdz::decompress(&bad, 1).is_err());
    assert!(!ok.is_empty());
    Ok(())
}
