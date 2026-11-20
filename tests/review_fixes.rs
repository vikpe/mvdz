//! Regressions for the final reviews (robustness R-*, correctness C-*,
//! spec D-*): each test is a reviewer's reproducer, rebuilt here, that
//! failed before the fix.

mod common;
use common::*;
use mvdz::budget::{Budget, Limits, ReadOptions};
use mvdz::capi::*;
use mvdz::format::*;
use mvdz::util::{crc32, put_uv};
use mvdz::{Error, Level};
use std::io::Cursor;
use std::time::{Duration, Instant};

fn base() -> (Vec<u8>, Vec<(u8, Vec<u8>)>) {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let c = mvdz::compress(&demo, &cfg(Level::Normal, 0)).unwrap();
    (demo, sections(&c))
}

fn key(i: usize, w: usize) -> String {
    const A: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut x = i;
    (0..w)
        .map(|_| {
            let c = A[x % 62] as char;
            x /= 62;
            c
        })
        .collect()
}

fn meta_fields(body: &[u8]) -> Vec<u8> {
    let mut r = mvdz::util::Rd::new(body);
    let (_, codec, n, _) = (
        r.u8().unwrap(),
        r.u8().unwrap(),
        r.uz().unwrap(),
        r.u32le().unwrap(),
    );
    if codec == 1 {
        mvdz::deflate::decompress(r.rest(), n).unwrap()
    } else {
        r.rest().to_vec()
    }
}

fn zstd_meta(p: &[u8]) -> Vec<u8> {
    let mut out = vec![1u8, 1];
    put_uv(&mut out, p.len() as u64);
    out.extend_from_slice(&crc32(p).to_le_bytes());
    out.extend_from_slice(&mvdz::deflate::compress(p, 9));
    out
}

/// R-H1: 300 000 extension keys in META: linear, not hours.
#[test]
fn r_h1_meta_ext_keys_linear() {
    let (demo, mut s) = base();
    let mut p = meta_fields(&s[0].1);
    let n = 300_000;
    let mut v = Vec::new();
    put_uv(&mut v, n as u64);
    for i in 0..n {
        let k = key(i, 4);
        put_uv(&mut v, 4);
        v.extend_from_slice(k.as_bytes());
        put_uv(&mut v, 0);
    }
    put_uv(&mut p, T_EXT);
    put_uv(&mut p, v.len() as u64);
    p.extend_from_slice(&v);
    s[0].1 = zstd_meta(&p);
    let f = build(0, true, &s, true);
    let t = Instant::now();
    let fi = mvdz::read_metadata(Cursor::new(&f)).unwrap();
    assert_eq!(fi.meta.ext.len(), n);
    assert_eq!(mvdz::decompress(&f, 1).unwrap(), demo);
    assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
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
    im.put(&mut e, NQC + 1, 2);
    im.put(&mut e, NQC + 2, n);
    let mut prev = 0usize;
    for _ in 0..256 {
        im.put(&mut e, 17 * 17 + prev, 0);
        prev = 0;
    }
    for i in 0..n {
        im.put(&mut e, NQC + 3, (i > 0) as u32);
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
    let mut st = Vec::new();
    put_uv(&mut st, hdr.len() as u64);
    st.extend_from_slice(&hdr);
    put_uv(&mut st, 0);
    let raw_len = n as u64 + 100;
    let mut body = Vec::new();
    for v in [
        0u64,
        0,
        0,
        0,
        0,
        0,
        0,
        1,
        0,
        2,
        n as u64 + 16,
        st.len() as u64,
    ] {
        put_uv(&mut body, v);
    }
    body.extend_from_slice(&st);
    let mut b = seg_hdr_bytes(0, raw_len, 0, 0, C_TRANSFORM);
    b.extend_from_slice(&body);
    let (_, s0) = base();
    let mut ix = Index {
        size: raw_len,
        ..Default::default()
    };
    ix.segs.push(IndexSeg {
        raw_len,
        ..Default::default()
    });
    let mut m = Metadata::decode(&s0[0].1, &Budget::unlimited()).unwrap();
    m.size = raw_len;
    m.demoinfo.clear();
    let f = build(
        0,
        true,
        &[
            (K_META, m.encode().unwrap()),
            (K_SEGMENT, b),
            (K_INDEX, ix.encode()),
        ],
        true,
    );
    let t = Instant::now();
    let l: ReadOptions = Limits {
        memory: 64 << 20,
        ..Default::default()
    }
    .into();
    assert_eq!(
        mvdz::decompress_with(&f, 1, &l).unwrap_err(),
        Error::Corrupt("model")
    );
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
}

/// R-M3: the JSON rendering can be capped.
#[test]
fn r_m3_json_limit() {
    let (_, s) = base();
    let f = build(0, true, &s, true);
    let fi = mvdz::read_metadata(Cursor::new(&f)).unwrap();
    let j = fi.to_json();
    assert_eq!(fi.to_json_with(j.len()).unwrap(), j);
    assert_eq!(
        fi.to_json_with(j.len() - 1).unwrap_err(),
        Error::Limit("metadata rendering")
    );
    let (mut out, mut n) = (std::ptr::null_mut(), 0usize);
    assert_eq!(
        unsafe { mvdz_info_json_memory(f.as_ptr() as *const _, f.len(), 64, &mut out, &mut n) },
        MVDZ_E_LIMIT
    );
}

/// R-M4: a long run of empty segments is charged to the budget: a
/// sequential reader stops with a limit error instead of growing.
#[test]
fn r_m4_section_bookkeeping() {
    let (demo, s0) = base();
    let n = 100_000;
    let mut s = vec![s0[0].clone()];
    for _ in 0..n {
        s.push((K_SEGMENT, seg_hdr_bytes(0, 0, 0, crc32(&[]), C_STORE)));
    }
    for x in &s0[1..] {
        if x.0 == K_INDEX {
            let mut ix = Index::decode(&x.1).unwrap();
            let mut segs = vec![IndexSeg::default(); n];
            segs.extend(ix.segs);
            ix.segs = segs;
            s.push((K_INDEX, ix.encode()));
        } else {
            s.push(x.clone());
        }
    }
    let f = build(0, true, &s, true);
    assert_eq!(mvdz::decompress(&f, 1).unwrap(), demo);
    let l: ReadOptions = Limits {
        memory: 4 << 20,
        ..Default::default()
    }
    .into();
    assert_eq!(
        mvdz::decompress_with(&f, 1, &l).unwrap_err(),
        Error::Limit("memory")
    );
    let mut r = mvdz::Reader::new(Cursor::new(&f), 1, &l).unwrap();
    let mut out = Vec::new();
    assert!(std::io::Read::read_to_end(&mut r, &mut out).is_err());
}

struct Reent {
    c: Cursor<Vec<u8>>,
    handle: *mut std::ffi::c_void,
    inner: Vec<i64>,
}

unsafe extern "C" fn re_read(
    user: *mut std::ffi::c_void,
    buf: *mut std::ffi::c_void,
    n: usize,
) -> std::ffi::c_longlong {
    let u = unsafe { &mut *(user as *mut Reent) };
    if !u.handle.is_null() {
        // calls on the handle from inside its own callback
        u.inner.push(unsafe { mvdz_raw_size(u.handle) });
        unsafe { mvdz_close(u.handle) };
    }
    let b = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, n) };
    std::io::Read::read(&mut u.c, b).map_or(-1, |k| k as i64)
}

/// R-M6: a callback that calls back into its own handle gets an error
/// instead of a deadlock; closing from inside is ignored.
#[test]
fn r_m6_reentrant_callbacks() {
    let (demo, s) = base();
    let f = build(0, true, &s, true);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut u = Box::new(Reent {
            c: Cursor::new(f),
            handle: std::ptr::null_mut(),
            inner: Vec::new(),
        });
        let up = &mut *u as *mut Reent as *mut std::ffi::c_void;
        let mut err = 0;
        let r = unsafe { mvdz_open_callbacks(Some(re_read), None, up, 1, 0, 0, &mut err) };
        assert!(!r.is_null());
        u.handle = r;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 4096];
        loop {
            let n = unsafe { mvdz_read(r, buf.as_mut_ptr() as *mut _, buf.len()) };
            assert!(n >= 0, "read {n}");
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        unsafe { mvdz_close(r) };
        tx.send((out, u.inner.clone())).unwrap();
    });
    let (out, inner) = rx.recv_timeout(Duration::from_secs(60)).expect("deadlock");
    assert_eq!(out, demo);
    assert!(
        !inner.is_empty() && inner.iter().all(|&x| x == MVDZ_E_INVALID as i64),
        "{inner:?}"
    );
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
    let e = mvdz::compress(&demo, &cfg(Level::Fast, 1)).unwrap_err();
    assert_eq!(
        e,
        Error::Invalid("too many segments: use a larger segment size")
    );
    assert!(t.elapsed() < Duration::from_secs(5));
    // at the limit itself, the file is written and decodes
    let demo = sets(2000);
    let c = mvdz::compress(&demo, &cfg(Level::Fast, 1)).unwrap();
    assert_eq!(mvdz::decompress(&c, 1).unwrap(), demo);
}

/// D3, D9: the transform flags are checked by every reader at open.
#[test]
fn d3_flags_at_open() {
    let b = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/hostile-segment-flags.mvdz"
    ))
    .unwrap();
    assert_eq!(
        mvdz::read_metadata(Cursor::new(&b)).unwrap_err(),
        Error::Corrupt("segment flags")
    );
    let b = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/vectors/hostile-share-empty.mvdz"
    ))
    .unwrap();
    assert_eq!(
        mvdz::read_metadata(Cursor::new(&b)).unwrap_err(),
        Error::Corrupt("demoinfo share")
    );
}

/// Trusted decoding skips the SHA-256 only: a forged identity passes, a
/// damaged segment does not, in every reader.
#[test]
fn trusted_mode() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    let forged = std::fs::read(dir.join("hostile-forged-identity.mvdz")).unwrap();
    let t = ReadOptions {
        trusted: true,
        ..Default::default()
    };
    assert_eq!(
        mvdz::decompress(&forged, 1).unwrap_err(),
        Error::Corrupt("identity mismatch")
    );
    let d = mvdz::decompress_with(&forged, 1, &t).unwrap();
    let mut r = mvdz::Reader::new(Cursor::new(&forged), 1, &t).unwrap();
    let mut o = Vec::new();
    std::io::Read::read_to_end(&mut r, &mut o).unwrap();
    assert_eq!(o, d);
    let (mut out, mut n) = (std::ptr::null_mut(), 0usize);
    assert_eq!(
        unsafe {
            mvdz_decompress(
                forged.as_ptr() as *const _,
                forged.len(),
                1,
                0,
                MVDZ_TRUSTED,
                &mut out,
                &mut n,
            )
        },
        MVDZ_OK
    );
    assert_eq!(unsafe { std::slice::from_raw_parts(out, n) }, &d[..]);
    unsafe { mvdz_free(out as *mut _, n) };
    // one byte of a stored segment changed: the segment CRC catches it
    let demo = synthetic(&[], false);
    let (c, _) = mvdz::encode::compress_untransformed(
        &[demo.clone(), vec![0; 5000]].concat(),
        &cfg(Level::Normal, 0),
    )
    .unwrap();
    let ok = mvdz::decompress_with(&c, 1, &t).unwrap();
    let mut bad = c.clone();
    let i = (0..bad.len())
        .rev()
        .find(|&i| bad[i] != 0 && i > 200)
        .unwrap();
    bad[i - 100] ^= 0x40;
    assert!(mvdz::decompress_with(&bad, 1, &t).is_err());
    assert!(!ok.is_empty());
}

/// C decode calls refuse flag bits they do not define.
#[test]
fn c_unknown_flags() {
    let (_, s) = base();
    let f = build(0, true, &s, true);
    let (mut out, mut n) = (std::ptr::null_mut(), 0usize);
    assert_eq!(
        unsafe { mvdz_decompress(f.as_ptr() as *const _, f.len(), 1, 0, 2, &mut out, &mut n) },
        MVDZ_E_INVALID
    );
    assert_eq!(
        unsafe {
            mvdz_decompress(
                f.as_ptr() as *const _,
                f.len(),
                1,
                0,
                MVDZ_TRUSTED,
                &mut out,
                &mut n,
            )
        },
        MVDZ_OK
    );
    unsafe { mvdz_free(out as *mut _, n) };
    let mut err = 0;
    assert!(
        unsafe { mvdz_open_memory(f.as_ptr() as *const _, f.len(), 1, 0, 1 << 31, &mut err) }
            .is_null()
    );
    assert_eq!(err, MVDZ_E_INVALID);
}
