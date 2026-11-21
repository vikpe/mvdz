//! The header and what a file says about itself; demos with KTX demoinfo
//! and truncated demos. Synthetic demos are built here, so most of these
//! run without the demo cache.

mod common;
use anyhow::Context as _;
use common::*;
use mvdz::format::{HEADER_LEN, Header};
use sha2::{Digest, Sha256};
use std::assert_matches;
use std::io::Cursor;

/// Every header field describes the original demo; the segments' raw
/// lengths and start times are running sums.
#[test]
fn header_describes_the_demo() -> anyhow::Result<()> {
    let small = synthetic(&[(0, "{\"x\":1}")], false);
    for demo in [small.clone(), several_segments(&small)] {
        let (_, ms, whole) = mvdz::segment::frame_scan(&demo);
        assert_eq!(whole, demo.len());
        let c = compress(&demo)?;
        let h = Header::parse(&c)?;
        assert_eq!(&c[..4], b"MVDZ");
        assert_eq!(h.size, demo.len() as u64);
        assert_eq!(h.duration_ms, ms);
        assert_eq!(h.crc, mvdz::util::crc32(&demo));
        assert_eq!(h.sha256, <[u8; 32]>::from(Sha256::digest(&demo)));
        assert_eq!(h.bytes(), c[..HEADER_LEN]);
        let fi = mvdz::read_info(Cursor::new(&c))?;
        assert_eq!(fi.header, h);
        assert_eq!(fi.file_size, c.len() as u64);
        assert_eq!(fi.index.segs.len() > 1, demo.len() > mvdz::segment::SEGMENT);
        let (mut raw, mut t) = (0, 0);
        for (i, s) in fi.index.segs.iter().enumerate() {
            assert_eq!(s.raw_off, raw, "segment {i}");
            assert!(s.t_start >= t, "segment {i}");
            assert!(s.raw_len > 0, "segment {i}");
            let part = &demo[raw as usize..(raw + s.raw_len) as usize];
            assert_eq!(
                s.t_start,
                mvdz::segment::frame_scan(&demo[..raw as usize]).1
            );
            raw += s.raw_len;
            t = s.t_start;
            assert!(!part.is_empty());
        }
        assert_eq!(raw, h.size);
        assert_eq!(
            readers_agree(&c, "header").context("every reader refuses")?,
            demo
        );
    }
    Ok(())
}

/// A wrong header field makes every reader refuse the file.
#[test]
fn wrong_header_fields_are_refused() -> anyhow::Result<()> {
    let demo = several_segments(&synthetic(&[], false));
    let c = compress(&demo)?;
    let p = parts(&c)?;
    assert!(p.segs.len() >= 3);
    assert_eq!(p.build(), c);
    #[expect(clippy::type_complexity, reason = "a table of named edits")]
    let cases: [(&str, fn(&mut Header)); 5] = [
        ("size + 1", |h| h.size += 1),
        ("size - 1", |h| h.size -= 1),
        ("duration + 1", |h| h.duration_ms += 1),
        ("duration - 1", |h| h.duration_ms -= 1),
        ("crc", |h| h.crc ^= 1),
    ];
    for (what, f) in cases {
        let mut q = p.clone();
        f(&mut q.header);
        assert!(readers_agree(&q.build(), what).is_none(), "{what}");
    }
    // the SHA-256 names the demo: decoding does not recompute it
    let mut q = p.clone();
    q.header.sha256[31] ^= 1;
    assert_eq!(
        readers_agree(&q.build(), "sha256").context("every reader refuses")?,
        demo
    );
    // a byte after the last segment
    let mut more = c.clone();
    more.push(0);
    assert!(readers_agree(&more, "trailing byte").is_none());
    // a dropped segment, two segments swapped
    let mut q = p.clone();
    q.segs.pop();
    assert!(readers_agree(&q.build(), "dropped segment").is_none());
    let mut q = p;
    q.segs.swap(1, 2);
    assert!(readers_agree(&q.build(), "swapped segments").is_none());
    // an unknown version, another magic
    let mut v = c.clone();
    v[4] = 3;
    assert_matches!(mvdz::decompress(&v, 1), Err(mvdz::Error::Format(_)));
    assert!(readers_agree(&v, "version").is_none());
    let mut m = c.clone();
    m[0] = b'N';
    assert_matches!(mvdz::decompress(&m, 1), Err(mvdz::Error::Format(_)));
    // every cut of the file
    for cut in [
        0,
        3,
        7,
        HEADER_LEN - 1,
        HEADER_LEN,
        HEADER_LEN + 1,
        c.len() / 2,
        c.len() - 1,
    ] {
        assert!(readers_agree(&c[..cut], "cut").is_none(), "cut at {cut}");
    }
    Ok(())
}

#[test]
fn demoinfo_multi_block_round_trip() -> anyhow::Result<()> {
    // blocks 1, 2, 0; then a second, newer copy
    let parts = [
        (1, "{\"a\":1,"),
        (2, "\"b\":2"),
        (0, "}"),
        (1, "{\"a\":3,"),
        (2, "\"b\":4"),
        (0, "}"),
    ];
    let small = synthetic(&parts, false);
    for demo in [small.clone(), several_segments(&small)] {
        let c = compress(&demo)?;
        assert_eq!(
            readers_agree(&c, "demoinfo").context("every reader refuses")?,
            demo
        );
    }
    Ok(())
}

#[test]
fn truncated_demo_round_trips() -> anyhow::Result<()> {
    // the truncated frame in the last segment
    let small = synthetic(&[], true);
    let long = [
        several_segments(&synthetic(&[], false)),
        small[small.len() - 4..].to_vec(),
    ]
    .concat();
    for demo in [small, long] {
        let c = compress(&demo)?;
        assert_eq!(
            readers_agree(&c, "truncated").context("every reader refuses")?,
            demo
        );
    }
    if let Some(d) = duel() {
        // one byte holds no frame: refused as a non-demo
        let r = compress(&d[..1]);
        assert_matches!(r, Err(mvdz::Error::NotMvd(_)), "{r:?}");
        for cut in [1000, 123_457, d.len() / 2 + 3, d.len() - 1] {
            let c = compress(&d[..cut])?;
            assert_eq!(mvdz::decompress(&c, 2)?, &d[..cut], "cut {cut}");
        }
    }
    Ok(())
}
