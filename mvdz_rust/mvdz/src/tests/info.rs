//! The header and what a file says about itself; demos with KTX demoinfo
//! and truncated demos. Synthetic demos are built here, so most of these
//! run without the demo cache.

use super::common::*;
use crate::format::{HEADER_LEN, Header};
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use std::assert_matches;
use std::io::Cursor;

/// Every header field describes the original demo; the segments' raw
/// lengths and start times are running sums.
#[test]
fn header_describes_the_demo() -> anyhow::Result<()> {
    let small = synthetic(&[(0, "{\"x\":1}")], false);
    for demo in [small.clone(), several_segments(&small)] {
        let (_, ms, whole) = crate::segment::frame_scan(&demo);
        assert_eq!(whole, demo.len());
        let file = compress(&demo)?;
        let header = Header::parse(&file)?;
        assert_eq!(&file[..4], b"MVDZ");
        assert_eq!(header.size, demo.len() as u64);
        assert_eq!(header.duration_ms, ms);
        assert_eq!(header.crc, crate::util::crc32(&demo));
        assert_eq!(header.sha256, <[u8; 32]>::from(Sha256::digest(&demo)));
        assert_eq!(header.bytes(), file[..HEADER_LEN]);
        let file_info = crate::read_info(Cursor::new(&file))?;
        assert_eq!(file_info.header, header);
        assert_eq!(file_info.file_size, file.len() as u64);
        assert_eq!(
            file_info.index.segments.len() > 1,
            demo.len() > crate::segment::SEGMENT
        );
        let (mut raw, mut previous_start) = (0, 0);
        for (index, segment) in file_info.index.segments.iter().enumerate() {
            assert_eq!(segment.raw_offset, raw, "segment {index}");
            assert!(segment.start_ms >= previous_start, "segment {index}");
            assert!(segment.raw_len > 0, "segment {index}");
            let part = &demo[raw as usize..(raw + segment.raw_len) as usize];
            assert_eq!(
                segment.start_ms,
                crate::segment::frame_scan(&demo[..raw as usize]).1
            );
            raw += segment.raw_len;
            previous_start = segment.start_ms;
            assert!(!part.is_empty());
        }
        assert_eq!(raw, header.size);
        assert_eq!(
            readers_agree(&file, "header").context("every reader refuses")?,
            demo
        );
    }
    Ok(())
}

/// A wrong header field makes every reader refuse the file.
#[test]
fn wrong_header_fields_are_refused() -> anyhow::Result<()> {
    let demo = several_segments(&synthetic(&[], false));
    let file = compress(&demo)?;
    let file_parts = parts(&file)?;
    assert!(file_parts.segments.len() >= 3);
    assert_eq!(file_parts.build(), file);
    #[expect(clippy::type_complexity, reason = "a table of named edits")]
    let cases: [(&str, fn(&mut Header)); 5] = [
        ("size + 1", |header| header.size += 1),
        ("size - 1", |header| header.size -= 1),
        ("duration + 1", |header| header.duration_ms += 1),
        ("duration - 1", |header| header.duration_ms -= 1),
        ("crc", |header| header.crc ^= 1),
    ];
    for (what, edit) in cases {
        let mut edited = file_parts.clone();
        edit(&mut edited.header);
        assert!(readers_agree(&edited.build(), what).is_none(), "{what}");
    }
    // the SHA-256 names the demo: decoding does not recompute it
    let mut edited = file_parts.clone();
    edited.header.sha256[31] ^= 1;
    assert_eq!(
        readers_agree(&edited.build(), "sha256").context("every reader refuses")?,
        demo
    );
    // a byte after the last segment
    let mut trailing = file.clone();
    trailing.push(0);
    assert!(readers_agree(&trailing, "trailing byte").is_none());
    // a dropped segment, two segments swapped
    let mut edited = file_parts.clone();
    edited.segments.pop();
    assert!(readers_agree(&edited.build(), "dropped segment").is_none());
    let mut edited = file_parts;
    edited.segments.swap(1, 2);
    assert!(readers_agree(&edited.build(), "swapped segments").is_none());
    // an unknown version, another magic
    let mut other_version = file.clone();
    other_version[4] = 3;
    assert_matches!(
        crate::decompress(&other_version, 1),
        Err(crate::Error::Format(_))
    );
    assert!(readers_agree(&other_version, "version").is_none());
    let mut other_magic = file.clone();
    other_magic[0] = b'N';
    assert_matches!(
        crate::decompress(&other_magic, 1),
        Err(crate::Error::Format(_))
    );
    // every cut of the file
    for cut in [
        0,
        3,
        7,
        HEADER_LEN - 1,
        HEADER_LEN,
        HEADER_LEN + 1,
        file.len() / 2,
        file.len() - 1,
    ] {
        assert!(readers_agree(&file[..cut], "cut").is_none(), "cut at {cut}");
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
        let file = compress(&demo)?;
        assert_eq!(
            readers_agree(&file, "demoinfo").context("every reader refuses")?,
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
        let file = compress(&demo)?;
        assert_eq!(
            readers_agree(&file, "truncated").context("every reader refuses")?,
            demo
        );
    }
    if let Some(duel) = duel() {
        // one byte holds no frame: refused as a non-demo
        let result = compress(&duel[..1]);
        assert_matches!(result, Err(crate::Error::NotMvd(_)), "{result:?}");
        for cut in [1000, 123_457, duel.len() / 2 + 3, duel.len() - 1] {
            let file = compress(&duel[..cut])?;
            assert_eq!(crate::decompress(&file, 2)?, &duel[..cut], "cut {cut}");
        }
    }
    Ok(())
}
