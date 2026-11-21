//! Hostile files: each one is refused (or accepted) by every reader alike,
//! with the expected error, quickly and within the memory budget. These
//! are the reviewers' proofs of concept rebuilt for this format.

mod common;
use common::*;
use mvdz::format::*;
use mvdz::util::put_uv;
use mvdz::Error;
use std::io::Cursor;
use std::time::Instant;

fn base(seg: usize) -> (Vec<u8>, Vec<u8>) {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let c = compress(&demo, &cfg(seg)).unwrap();
    (demo, c)
}

/// Refused by every reader within `secs`, with `want` matching the error.
fn refused(b: &[u8], what: &str, want: impl Fn(&Error) -> bool) {
    let t = Instant::now();
    assert!(readers_agree(b, what).is_none(), "{what}: accepted");
    let e = mvdz::decompress(b, 2).unwrap_err();
    assert!(want(&e), "{what}: unexpected error {e}");
    assert!(
        t.elapsed().as_secs_f64() < 5.0,
        "{what}: took {:?}",
        t.elapsed()
    );
}

fn corrupt(e: &Error) -> bool {
    matches!(e, Error::Corrupt(_))
}

#[test]
fn structure_rules() {
    let (demo, c) = base(300);
    assert_eq!(readers_agree(&c, "base").unwrap(), demo);
    let p = parts(&c);
    assert!(p.segs.len() > 3);
    // rebuilt unchanged: the same file
    assert_eq!(p.build(), c);
    // another SHA-256 in the header: the SHA-256 names the demo and
    // decoding does not recompute it, so the file decodes
    let mut q = p.clone();
    q.header.sha256 = [0xff; 32];
    let b = q.build();
    assert_eq!(readers_agree(&b, "another SHA-256").unwrap(), demo);
    assert_eq!(
        mvdz::read_info(Cursor::new(&b)).unwrap().header.sha256,
        [0xff; 32]
    );
    // a segment dropped
    let mut q = p.clone();
    q.segs.remove(2);
    refused(&q.build(), "dropped segment", corrupt);
    // segments swapped
    let mut q = p.clone();
    q.segs.swap(1, 2);
    refused(&q.build(), "swapped segments", corrupt);
    // a segment twice
    let mut q = p.clone();
    q.segs.insert(1, p.segs[1].clone());
    refused(&q.build(), "repeated segment", corrupt);
    // the last segment cut short
    refused(&c[..c.len() - 3], "cut", |e| {
        *e == Error::Corrupt("incomplete file")
    });
    // data after the last segment
    let mut b = c.clone();
    b.push(0);
    refused(&b, "trailing byte", |e| {
        *e == Error::Corrupt("trailing bytes")
    });
    // a segment of a coding this version does not know
    let mut q = p.clone();
    let h = parse_seg_hdr(&q.segs[1]).unwrap();
    q.segs[1][h.body_off - 1] = 9;
    refused(&q.build(), "unknown coding", |e| {
        matches!(e, Error::Format(_))
    });
    // an empty segment in between
    let mut q = p.clone();
    q.segs
        .insert(1, seg_hdr_bytes(0, 0, mvdz::util::crc32(&[]), C_DEFLATE));
    refused(&q.build(), "empty segment", corrupt);
    // the models block missing
    let mut b = c[..HEADER_LEN].to_vec();
    for s in &p.segs {
        segment_block(&mut b, s);
    }
    refused(&b, "no models block", corrupt);
    // the high half of the header's checksum field set
    let mut b = c.clone();
    b[30] = 1;
    refused(&b, "checksum field", |e| {
        *e == Error::Corrupt("header checksum field")
    });
    // empty file, magic only, other versions, a cut header
    for (b, what) in [
        (&b""[..], "empty"),
        (b"MVDZ", "magic"),
        (b"MVDZ\x03\x00\x00\x00", "v3"),
        (b"MVDZ\x01\x00\x00\x00", "v1"),
        (b"MVDZ\x02\x00", "header cut"),
        (&c[..HEADER_LEN], "header only"),
    ] {
        assert!(readers_agree(b, what).is_none(), "{what}");
    }
}

/// Declared sizes never reach the allocator unbounded (robustness 3, 4,
/// 12; correctness F4): each is refused by a format rule or the budget.
#[test]
fn declared_sizes() {
    let (_, c) = base(0);
    let p = parts(&c);
    // a 30-byte segment that claims 4 GiB: DEFLATE coding, raw_len
    // 2^32 - 2^16
    let mut q = p.clone();
    let mut b = seg_hdr_bytes(MAX_SEG_RAW, 0, 0, C_DEFLATE);
    b.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0, 0]);
    q.header.size = MAX_SEG_RAW;
    q.segs = vec![b];
    refused(&q.build(), "4 GiB segment", |e| {
        *e == Error::Corrupt("segment size")
    });
    // a header claiming 60 GB
    let mut q = p.clone();
    q.header.size = 60 << 30;
    refused(&q.build(), "60 GB header", corrupt);
    // a models block of 256 MiB declared in a few bytes
    let mut b = c[..HEADER_LEN].to_vec();
    b.extend_from_slice(&(256u32 << 20).to_le_bytes());
    b.extend_from_slice(&[0; 100]);
    refused(&b, "256 MiB models", |e| {
        *e == Error::Corrupt("models too large")
    });
    // a segment whose length field runs past the end of the file
    let mut b = c[..HEADER_LEN].to_vec();
    models_block(&mut b, &p.models);
    b.extend_from_slice(&u32::MAX.to_le_bytes());
    b.extend_from_slice(&p.segs[0]);
    refused(&b, "segment length", corrupt);
    // the caller's memory budget
    let (demo, c) = base(0);
    let small = mvdz::Limits {
        memory: demo.len() as u64 / 2,
        ..Default::default()
    };
    assert!(matches!(
        mvdz::decompress_with(&c, 1, &small),
        Err(Error::Limit(_))
    ));
    assert!(
        mvdz::Reader::new(Cursor::new(&c), 1, &small)
            .and_then(|mut r| r.read_demo(&mut [0; 10]))
            .is_err()
    );
}

/// The reviewer's amplifying v2 model headers (robustness 5: a 2.3 KB
/// header took 1.3 GB and 3.6 s) in the models block: refused at once by
/// the model rules (decisions <= 128 per header byte + 1024).
#[test]
fn model_amplification() {
    let Some(dir) = test_data("poc") else { return };
    let demo = synthetic(&[], false);
    let c = compress(&demo, &cfg(0)).unwrap();
    let p = parts(&c);
    for name in ["v2model_1000.bin", "v2model_16000.bin", "v2model_65535.bin"] {
        // a model and a rANS stream body: header length, header, rANS data
        let Ok(st) = std::fs::read(dir.join(name)) else {
            continue;
        };
        let mut r = mvdz::util::Rd::new(&st);
        let hdr = r.bytes().unwrap();
        let rest = r.rest();
        // the models block: one model, the time stream's
        let mut models = Vec::new();
        for v in [1, 0, hdr.len() as u64] {
            put_uv(&mut models, v);
        }
        models.extend_from_slice(hdr);
        // a segment whose only stream is a time stream: fte, fte2, mvd1,
        // tail; one entry (id 0, count, length); no blob body
        let mut body = Vec::new();
        for v in [0u64, 0, 0, 0, 1, 0, demo.len() as u64, rest.len() as u64, 0] {
            put_uv(&mut body, v);
        }
        body.extend_from_slice(rest);
        let mut b = seg_hdr_bytes(
            demo.len() as u64,
            p.header.duration_ms,
            mvdz::util::crc32(&demo),
            C_TRANSFORM,
        );
        b.extend_from_slice(&body);
        let mut q = p.clone();
        q.models = models;
        q.segs = vec![b];
        refused(&q.build(), name, |e| *e == Error::Corrupt("model"));
    }
}

/// Mutated files: every reader decides the same way (spec review items
/// 2-4, robustness 13).
#[test]
fn mutations_agree() {
    let mut rng = Rng(0x5eed);
    for seg in [0, 300] {
        let (_, c) = base(seg);
        for _ in 0..150 {
            let mut b = c.clone();
            match rng.below(3) {
                0 => {
                    for _ in 0..1 + rng.below(3) {
                        let k = rng.below(b.len());
                        b[k] ^= 1 << rng.below(8);
                    }
                }
                1 => b.truncate(rng.below(b.len())),
                _ => {
                    let k = rng.below(b.len());
                    b.insert(k, rng.next() as u8);
                }
            }
            readers_agree(&b, "mutation");
        }
    }
}

/// A model header (FORMAT.md §8.3) of `n` own tables and the fallback,
/// each with all 256 symbols present and pseudo-random q values: legal
/// (rule M2 holds), and every table takes a 16 KiB slot LUT once built.
fn wide_model(n: u32) -> Vec<u8> {
    use mvdz::coder::rc::{Enc, IntModel};
    const NQC: usize = 306;
    let mut e = Enc::new();
    let mut im = IntModel::new(NQC + 8);
    let mut x = 1u32;
    let mut q = || {
        x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        1 + (x >> 16) % 12
    };
    im.put(&mut e, NQC, 256);
    im.put(&mut e, NQC + 1, n);
    let fb: Vec<u32> = (0..256).map(|_| q()).collect();
    let mut prev = 0;
    for &v in &fb {
        im.put(&mut e, 17 * 17 + prev.min(16) as usize, v);
        prev = v;
    }
    for t in 0..n {
        im.put(&mut e, NQC + 2, (t > 0) as u32);
        let mut prev = 0u32;
        for &f in &fb {
            let v = q();
            im.put(&mut e, f.min(16) as usize * 17 + prev.min(16) as usize, v);
            prev = v;
        }
    }
    e.finish()
}

/// A large, legal models block: its models are built when the file is
/// opened, under the reader's budget, which refuses them, for the file
/// information as for a decode. With room for them the file opens, quickly.
#[test]
fn large_tables() {
    let demo = usercmd_demo();
    let c = compress(&demo, &cfg(8192)).unwrap();
    let mut p = parts(&c);
    assert!(!p.models.is_empty());
    let m = wide_model(512);
    let mut t = Vec::new();
    // a model for every structural stream
    let lazy: Vec<usize> = (0..mvdz::streams::N_LAZY)
        .filter(|&sid| mvdz::streams::is_lazy(sid))
        .collect();
    put_uv(&mut t, lazy.len() as u64);
    for sid in lazy {
        put_uv(&mut t, sid as u64);
        put_uv(&mut t, m.len() as u64);
        t.extend_from_slice(&m);
    }
    p.models = t;
    let b = p.build();
    // the models: 16 x 513 LUTs of 16 KiB, just over 128 MiB
    let lim = mvdz::Limits {
        memory: 128 << 20,
        ..Default::default()
    };
    let e = mvdz::read_info_with(Cursor::new(&b), &lim).err();
    assert_eq!(e, Some(Error::Limit("memory")));
    let e = mvdz::decompress_with(&b, 1, &lim).unwrap_err();
    assert_eq!(e, Error::Limit("memory"));
    let t0 = Instant::now();
    mvdz::read_info_with(Cursor::new(&b), &mvdz::Limits::default()).unwrap();
    assert!(
        t0.elapsed().as_secs_f64() < 10.0,
        "reading the models took {:?}",
        t0.elapsed()
    );
}
