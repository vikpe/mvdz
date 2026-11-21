//! Shared helpers of the integration tests: synthetic demos, a small RNG,
//! the sample demos (skipped when absent) and the every-reader check.
#![allow(dead_code)]

use anyhow::Context as _;
use mvdz::Config;
use mvdz::format::*;
use std::io::{Cursor, Read};

/// Compresses a demo as the library does (input that is not a demo is
/// refused), on one thread.
pub(crate) fn compress(src: &[u8]) -> mvdz::Result<Vec<u8>> {
    compress_info(src).map(|x| x.0)
}

pub(crate) fn compress_info(src: &[u8]) -> mvdz::Result<(Vec<u8>, mvdz::Info)> {
    mvdz::compress_info(src, &Config::default())
}

/// The encoder without the input check (any bytes round-trip).
pub(crate) fn unchecked(src: &[u8]) -> mvdz::Result<(Vec<u8>, mvdz::Info)> {
    mvdz::encode::compress(src, &Config::default())
}

/// Every segment coded as all tail: no frames, every byte in the tail.
pub(crate) fn all_tail(src: &[u8]) -> mvdz::Result<(Vec<u8>, mvdz::Info)> {
    mvdz::encode::compress_all_tail(src, &Config::default())
}

/// `demo` repeated past four times [`mvdz::segment::SEGMENT`]: the encoder
/// cuts only at that size, so a small demo of whole frames gets at least
/// four segments this way.
pub(crate) fn several_segments(demo: &[u8]) -> Vec<u8> {
    demo.repeat(4 * mvdz::segment::SEGMENT / demo.len().max(1) + 1)
}

pub(crate) struct Rng(pub u64);
impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

// ---------------------------------------------------------------- synthetic demos

pub(crate) struct Demo(pub Vec<u8>);

impl Demo {
    pub(crate) fn frame(&mut self, tb: u8, ty: u8, payload: &[u8]) {
        self.0.push(tb);
        self.0.push(ty);
        self.0
            .extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.0.extend_from_slice(payload);
    }
    pub(crate) fn hidden(&mut self, tb: u8, blocks: &[(u16, &[u8])]) {
        let mut p = Vec::new();
        for (ty, data) in blocks {
            p.extend_from_slice(&(data.len() as u32).to_le_bytes());
            p.extend_from_slice(&ty.to_le_bytes());
            p.extend_from_slice(data);
        }
        self.0.push(tb);
        self.0.push(3);
        self.0.extend_from_slice(&0u32.to_le_bytes());
        self.0.extend_from_slice(&(p.len() as u32).to_le_bytes());
        self.0.extend_from_slice(&p);
    }
}

pub(crate) fn s(cmd: u8, parts: &[&[u8]]) -> Vec<u8> {
    let mut v = vec![cmd];
    for p in parts {
        v.extend_from_slice(p);
    }
    v
}

pub(crate) fn z(t: &str) -> Vec<u8> {
    let mut v = t.as_bytes().to_vec();
    v.push(0);
    v
}

/// KTX demoinfo block bodies (u16 block number, text).
pub(crate) fn dinfo_blocks(parts: &[(u16, &str)]) -> Vec<Vec<u8>> {
    parts
        .iter()
        .map(|(n, t)| {
            let mut b = n.to_le_bytes().to_vec();
            b.extend_from_slice(t.as_bytes());
            b
        })
        .collect()
}

/// A demo with prints, fragmented stufftext, userinfo, modellist,
/// serverinfo; optionally demoinfo blocks; optionally a truncated tail.
pub(crate) fn synthetic(demoinfo: &[(u16, &str)], truncate: bool) -> Vec<u8> {
    let mut d = Demo(Vec::new());
    let mut p = Vec::new();
    p.extend(s(
        9,
        &[&z(
            "fullserverinfo \"\\map\\dm4\\hostname\\test server\\deathmatch\\3\"\n",
        )],
    ));
    p.extend(s(
        40,
        &[
            &[0],
            &7u32.to_le_bytes(),
            &z("\\name\\alice\\team\\red\\*client\\ezQuake 7"),
        ],
    ));
    p.extend(s(
        40,
        &[
            &[3],
            &9u32.to_le_bytes(),
            &z("\\name\\bob\\team\\blue\\*client\\fte"),
        ],
    ));
    p.extend(s(
        45,
        &[&[0], &z("maps/dm4.bsp"), &z("progs/player.mdl"), &[0], &[0]],
    ));
    d.frame(0, 6, &p);
    for i in 0..50u8 {
        let mut p = Vec::new();
        p.extend(s(8, &[&[2], &z(&format!("message {i}\n"))]));
        // a command split over two stufftexts, and ';'-separated commands
        p.extend(s(9, &[&z("topcolor ")]));
        p.extend(s(
            9,
            &[&z(
                "10\n_cs;wait;_cs\n//ktx took 1 2 3\n//finalscores \"Oct 12\" 3\n",
            )],
        ));
        p.extend(s(52, &[&z("status"), &z(&format!("{i} min"))]));
        d.frame(13 + i % 3, 6, &p);
    }
    if !demoinfo.is_empty() {
        let bodies = dinfo_blocks(demoinfo);
        let refs: Vec<(u16, &[u8])> = bodies.iter().map(|b| (3u16, &b[..])).collect();
        d.hidden(0, &refs);
    }
    d.frame(5, 6, &s(8, &[&[3], &z("bye\n")]));
    if truncate {
        // a frame cut short: header and part of its length
        d.0.extend_from_slice(&[7, 6, 40, 0]);
    }
    d.0
}

/// One usercmd hidden-block body (mvdsv `SV_DebugClientCommand`, 23
/// bytes): player, dropnum, msec, pitch/yaw/roll, moves, buttons, impulse.
pub(crate) fn usercmd_block(
    pl: u8,
    drop: u8,
    msec: u8,
    a: [f32; 3],
    mv: [i16; 3],
    bi: [u8; 2],
) -> Vec<u8> {
    let mut b = vec![pl, drop, msec];
    for x in a {
        b.extend_from_slice(&x.to_le_bytes());
    }
    for m in mv {
        b.extend_from_slice(&m.to_le_bytes());
    }
    b.extend_from_slice(&bi);
    b
}

/// A demo with usercmd hidden blocks (type 1, 23 bytes) of four players:
/// turning view angles on the 360/65536 grid, a few angles off it (-0.0,
/// NaN, +180, 1.2345, an odd NaN payload), type-1 blocks of other lengths,
/// two usercmds in one payload, after the base synthetic demo.
pub(crate) fn usercmd_demo() -> Vec<u8> {
    let mut d = Demo(synthetic(&[], false));
    let mut rng = Rng(0x00c0_ffee);
    let mut ang = [[0i32; 2]; 4];
    let odd = [
        -0.0f32,
        f32::NAN,
        180.0,
        1.2345,
        f32::from_bits(0x7fc0_1234),
    ];
    let grid = |s: i32| f32::from(s as i16) * (45.0 / 8192.0);
    for i in 0..600u32 {
        let pl = (i % 4) as u8;
        let a = &mut ang[pl as usize];
        a[0] = (a[0] + i32::from(rng.below(9) as u8) - 4).clamp(-2900, 2900);
        a[1] += 8 * (i32::from(rng.below(7) as u8) - 3);
        let mut angles = [grid(a[0]), grid(a[1]), 0.0];
        if i % 53 == 7 {
            angles[(i % 3) as usize] = odd[(i / 53) as usize % odd.len()];
        }
        let fwd = [0i16, 320, 400, -400][(i / 40 % 4) as usize];
        let side = if i % 90 < 30 {
            0
        } else {
            508 * (1 - 2 * ((i / 90) % 2) as i16)
        };
        let bi = [
            u8::from(i / 25 % 4 == 0) | (2 * u8::from(i % 70 < 3)),
            if i % 111 == 5 { 7 } else { 0 },
        ];
        let msec = if i % 97 == 0 { 12 } else { 13 };
        let b = usercmd_block(
            pl,
            u8::from(i % 150 == 149),
            msec,
            angles,
            [fwd, side, 0],
            bi,
        );
        if i % 61 == 30 {
            let b2 = usercmd_block(pl ^ 1, 0, 13, angles, [0, 0, 0], [0, 0]);
            d.hidden(0, &[(1, &b), (1, &b2)]);
        } else {
            d.hidden(if pl == 0 { 13 } else { 0 }, &[(1, &b)]);
        }
        if i % 150 == 75 {
            d.hidden(0, &[(1, &b[..3]), (1, &[b.clone(), vec![9]].concat())]);
        }
    }
    d.frame(5, 6, &s(8, &[&[3], &z("bye\n")]));
    d.0
}

// ---------------------------------------------------------------- sample demos

/// Optional test data, from the directory in `MVDZ_TEST_DATA`:
/// `sample/` (real demos named `<sha256>.mvd`; `scripts/fetch_samples.sh
/// sample40 DIR/sample` downloads sample40), `gen/` (constructed demos,
/// e.g. `float_mvd1.mvd`) and `poc/` (hostile model headers). Tests that
/// need it skip, with a note, when the variable is unset or the file is
/// missing; everything else runs on synthetic demos.
pub(crate) fn test_data(sub: &str) -> Option<std::path::PathBuf> {
    if let Some(d) = std::env::var_os("MVDZ_TEST_DATA") {
        Some(std::path::PathBuf::from(d).join(sub))
    } else {
        eprintln!("MVDZ_TEST_DATA is not set: skipping the parts that need {sub}/ demos");
        None
    }
}

/// The first `n` sample demos (sorted by name), or none.
pub(crate) fn samples(n: usize) -> Vec<Vec<u8>> {
    let Some(dir) = test_data("sample") else {
        return Vec::new();
    };
    let mut v: Vec<_> = match std::fs::read_dir(dir) {
        Ok(d) => d
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mvd"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    v.sort();
    v.into_iter()
        .take(n)
        .filter_map(|p| std::fs::read(p).ok())
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
pub(crate) struct Dribble<'a>(pub &'a [u8], pub usize);
impl Read for Dribble<'_> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        let n = b.len().min(7).min(self.0.len() - self.1);
        b[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
        self.1 += n;
        Ok(n)
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// Decodes `b` with every reader: (reader, result).
pub(crate) fn all_readers(b: &[u8]) -> Vec<(&'static str, Result<Vec<u8>, String>)> {
    let e = |x: mvdz::Error| x.to_string();
    let mut v = Vec::new();
    v.push(("whole 1t", mvdz::decompress(b, 1).map_err(e)));
    v.push(("whole 3t", mvdz::decompress(b, 3).map_err(e)));
    let seq = |r: Result<mvdz::Reader<_>, mvdz::Error>| -> Result<Vec<u8>, String> {
        let mut r = r.map_err(e)?;
        let mut out = Vec::new();
        r.read_to_end(&mut out).map_err(|x| x.to_string())?;
        Ok(out)
    };
    v.push((
        "sequential",
        seq(mvdz::Reader::new(
            Cursor::new(b),
            1,
            &mvdz::Limits::default(),
        )),
    ));
    let mut out = Vec::new();
    let r = mvdz::Reader::new(Dribble(b, 0), 2, &mvdz::Limits::default())
        .map_err(e)
        .and_then(|mut r| {
            r.read_to_end(&mut out).map_err(|x| x.to_string())?;
            Ok(std::mem::take(&mut out))
        });
    v.push(("sequential dribble", r));
    v
}

/// Every reader accepts `b` with the same bytes, or every reader refuses
/// it. Returns the bytes if accepted. Structural refusals are also
/// required of `read_info`.
pub(crate) fn readers_agree(b: &[u8], what: &str) -> Option<Vec<u8>> {
    let rs = all_readers(b);
    let first = &rs[0].1;
    for (name, r) in &rs {
        match (first, r) {
            (Ok(a), Ok(x)) => assert!(a == x, "{what}: {name} decodes different bytes"),
            (Err(_), Err(_)) => {}
            _ => panic!(
                "{what}: readers disagree: {} {:?} vs {name} {:?}",
                rs[0].0,
                first.as_ref().map(std::vec::Vec::len),
                r.as_ref().map(std::vec::Vec::len)
            ),
        }
    }
    let info = mvdz::read_info(Cursor::new(b));
    if first.is_ok() {
        assert!(
            info.is_ok(),
            "{what}: read_info refuses a file the decoders accept: {:?}",
            info.err()
        );
    }
    if let Err(m) = &info {
        assert!(
            first.is_err(),
            "{what}: decoders accept a file read_info refuses ({m})"
        );
    }
    first.as_ref().ok().cloned()
}

// ---------------------------------------------------------------- file surgery

/// A file taken apart: its header, the body of its models block and the
/// body of every segment (its header fields after the length, then its
/// coded data).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parts {
    pub header: Header,
    pub models: Vec<u8>,
    pub segs: Vec<Vec<u8>>,
}

/// The parts of a well-formed file.
pub(crate) fn parts(c: &[u8]) -> anyhow::Result<Parts> {
    let header = Header::parse(c)?;
    let mut off = HEADER_LEN;
    // the next block: `extra` bytes of fields, then as many as its length
    // field says
    let mut next = |c: &[u8], extra: usize| -> anyhow::Result<Vec<u8>> {
        let len = len_field(&c[off..]).context("a length field")? as usize + extra;
        let b = c[off + LEN..off + LEN + len].to_vec();
        off += LEN + len;
        Ok(b)
    };
    let models = next(c, 0)?;
    let mut segs = Vec::new();
    let mut raw = 0;
    while raw < header.size {
        let b = next(c, SEG_FIELDS)?;
        raw += u64::from(parse_seg_hdr(&b[..SEG_FIELDS])?.raw_len);
        segs.push(b);
    }
    Ok(Parts {
        header,
        models,
        segs,
    })
}

impl Parts {
    /// The file these parts make (whatever they hold).
    pub(crate) fn build(&self) -> Vec<u8> {
        let mut out = self.header.bytes().to_vec();
        models_block(&mut out, &self.models);
        for s in &self.segs {
            segment_block(&mut out, s);
        }
        out
    }
}
