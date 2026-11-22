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
        assert_eq!(
            parts(&file)?.segments.len() > 1,
            demo.len() > crate::segment::SEGMENT_TARGET_RAW
        );
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

#[test]
fn sha256_is_not_checked_on_decode() -> anyhow::Result<()> {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let file = compress(&demo)?;
    let mut file_parts = parts(&file)?;
    file_parts.header.sha256 = [0xff; 32];
    let other = file_parts.build();
    let decoded = decompress(&other, 1)?;
    assert_eq!(decoded, demo);
    let mut read = Vec::new();
    std::io::Read::read_to_end(&mut crate::Decoder::new(Cursor::new(&other))?, &mut read)?;
    assert_eq!(read, decoded);
    let demo = synthetic(&[], false);
    let file = compress(&[demo, vec![0; 5000]].concat())?;
    let intact = decompress(&file, 1)?;
    let mut bad = file;
    let at = (0..bad.len())
        .rev()
        .find(|&at| bad[at] != 0 && at > 200)
        .context("a non-zero byte")?;
    bad[at - 100] ^= 0x40;
    assert!(decompress(&bad, 1).is_err());
    assert!(!intact.is_empty());
    Ok(())
}
