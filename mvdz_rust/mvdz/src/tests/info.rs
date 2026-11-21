use super::common::*;
use crate::format::{HEADER_LEN, Header, header_bytes, parse_header};
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use std::assert_matches;
use std::io::Cursor;

#[test]
fn header_describes_the_demo() -> anyhow::Result<()> {
    let small = synthetic(&[(0, "{\"x\":1}")], false);
    for demo in [small.clone(), several_segments(&small)] {
        let frames = crate::segment::frame_scan(&demo);
        assert_eq!(frames.framed_len, demo.len());
        let file = compress(&demo)?;
        let header = parse_header(&file)?;
        assert_eq!(&file[..4], b"MVDZ");
        assert_eq!(header.size_bytes, demo.len() as u64);
        assert_eq!(header.duration_ms, frames.duration_ms);
        assert_eq!(header.crc32, crc32fast::hash(&demo));
        assert_eq!(header.sha256, <[u8; 32]>::from(Sha256::digest(&demo)));
        assert_eq!(header_bytes(&header), file[..HEADER_LEN]);
        let decoder = crate::Decoder::new(Cursor::new(&file))?;
        assert_eq!(*decoder.header(), header);
        let layout = scan(&file, crate::budget::DEFAULT_MAX_MEMORY)?;
        assert_eq!(layout.header, header);
        assert_eq!(
            layout.segments.len() > 1,
            demo.len() > crate::segment::SEGMENT_TARGET_RAW
        );
        let (mut raw, mut previous_start) = (0, 0);
        for (index, (_, segment)) in layout.segments.iter().enumerate() {
            let size = u64::from(segment.raw_len);
            assert_eq!(segment.raw_offset, raw, "segment {index}");
            assert!(segment.offset_ms >= previous_start, "segment {index}");
            assert!(size > 0, "segment {index}");
            assert_eq!(
                segment.offset_ms,
                crate::segment::frame_scan(&demo[..raw as usize]).duration_ms
            );
            raw += size;
            previous_start = segment.offset_ms;
        }
        assert_eq!(raw, header.size_bytes);
        assert_eq!(
            readers_agree(&file, "header").context("every reader refuses")?,
            demo
        );
    }
    Ok(())
}

#[test]
fn wrong_header_fields_are_refused() -> anyhow::Result<()> {
    let demo = several_segments(&synthetic(&[], false));
    let file = compress(&demo)?;
    let file_parts = parts(&file)?;
    assert!(file_parts.segments.len() >= 3);
    assert_eq!(file_parts.build(), file);
    #[expect(clippy::type_complexity, reason = "a table of named edits")]
    let cases: [(&str, fn(&mut Header)); 5] = [
        ("size + 1", |header| header.size_bytes += 1),
        ("size - 1", |header| header.size_bytes -= 1),
        ("duration + 1", |header| header.duration_ms += 1),
        ("duration - 1", |header| header.duration_ms -= 1),
        ("crc", |header| header.crc32 ^= 1),
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
    let mut trailing = file.clone();
    trailing.push(0);
    assert!(readers_agree(&trailing, "trailing byte").is_none());
    let mut edited = file_parts.clone();
    edited.segments.pop();
    assert!(readers_agree(&edited.build(), "dropped segment").is_none());
    let mut edited = file_parts;
    edited.segments.swap(1, 2);
    assert!(readers_agree(&edited.build(), "swapped segments").is_none());
    let mut other_version = file.clone();
    other_version[4] = 3;
    assert_matches!(
        decompress(&other_version, 1),
        Err(crate::Error::UnsupportedVersion)
    );
    assert!(readers_agree(&other_version, "version").is_none());
    let mut other_magic = file.clone();
    other_magic[0] = b'N';
    assert_matches!(decompress(&other_magic, 1), Err(crate::Error::NotMvdz));
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
        let result = compress(&duel[..1]);
        assert_matches!(result, Err(crate::Error::NotMvd(_)), "{result:?}");
        for cut in [1000, 123_457, duel.len() / 2 + 3, duel.len() - 1] {
            let file = compress(&duel[..cut])?;
            assert_eq!(decompress(&file, 2)?, &duel[..cut], "cut {cut}");
        }
    }
    Ok(())
}
