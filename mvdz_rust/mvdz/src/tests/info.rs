use super::common::*;
use crate::format::{HEADER_SIZE, Header, header_bytes, parse_header};
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use std::assert_matches;
use std::io::Cursor;
use std::time::Duration;

#[test]
fn header_describes_the_demo() -> anyhow::Result<()> {
    /// Of `duration_ms` in a segment's fields.
    const DURATION_OFFSET: usize = 4;
    let small = synthetic(&[(0, "{\"x\":1}")], false);
    for demo in [small.clone(), several_segments(&small)] {
        let file = crate::encode(&demo, 1)?;
        let header = parse_header(&file)?;
        assert_eq!(&file[..4], b"MVDZ");
        assert_eq!(header.size(), demo.len() as u64);
        let duration = Duration::from_millis(u64::from(crate::segment::duration_ms(&demo)));
        assert_eq!(header.duration(), duration);
        let segment_durations: u64 = parts(&file)?
            .segments
            .iter()
            .filter_map(|segment| crate::fields::read_u32(&mut &segment[DURATION_OFFSET..]))
            .map(u64::from)
            .sum();
        assert_eq!(Duration::from_millis(segment_durations), header.duration());
        assert_eq!(header.crc32(), crc32fast::hash(&demo));
        assert_eq!(*header.sha256(), <[u8; 32]>::from(Sha256::digest(&demo)));
        assert_eq!(header_bytes(&header), file[..HEADER_SIZE]);
        let decoder = crate::Decoder::new(Cursor::new(&file))?;
        assert_eq!(*decoder.header(), header);
        assert_eq!(
            parts(&file)?.segments.len() > 1,
            demo.len() > crate::segment::SEGMENT_TARGET_LEN
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
    let file = crate::encode(&demo, 1)?;
    let file_parts = parts(&file)?;
    assert!(file_parts.segments.len() >= 3);
    assert_eq!(file_parts.build(), file);
    #[expect(clippy::type_complexity, reason = "a table of named edits")]
    let cases: [(&str, fn(&Header) -> Header); 3] = [
        ("size + 1", |header| with_size(header, header.size() + 1)),
        ("size - 1", |header| with_size(header, header.size() - 1)),
        ("crc", |header| with_crc32(header, header.crc32() ^ 1)),
    ];
    for (what, edit) in cases {
        let mut edited = file_parts.clone();
        edited.header = edit(&edited.header);
        assert!(readers_agree(&edited.build(), what).is_none(), "{what}");
    }
    // the SHA-256 names the demo: decoding does not recompute it
    let mut edited = file_parts.clone();
    let mut other_sha256 = *edited.header.sha256();
    other_sha256[31] ^= 1;
    edited.header = with_sha256(&edited.header, other_sha256);
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
    other_version[4] = 4;
    assert_matches!(
        crate::decode(&other_version, 1),
        Err(crate::Error::UnsupportedVersion)
    );
    assert!(readers_agree(&other_version, "version").is_none());
    let mut other_magic = file.clone();
    other_magic[0] = b'N';
    assert_matches!(crate::decode(&other_magic, 1), Err(crate::Error::NotMvdz));
    for cut in [
        0,
        3,
        7,
        HEADER_SIZE - 1,
        HEADER_SIZE,
        HEADER_SIZE + 1,
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
        let file = crate::encode(&demo, 1)?;
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
        let file = crate::encode(&demo, 1)?;
        assert_eq!(
            readers_agree(&file, "truncated").context("every reader refuses")?,
            demo
        );
    }
    if let Some(duel) = duel() {
        let result = crate::encode(&duel[..1], 1);
        assert_matches!(result, Err(crate::Error::NotMvd(_)), "{result:?}");
        for cut in [1000, 123_457, duel.len() / 2 + 3, duel.len() - 1] {
            let file = crate::encode(&duel[..cut], 1)?;
            assert_eq!(crate::decode(&file, 2)?, &duel[..cut], "cut {cut}");
        }
    }
    Ok(())
}

#[test]
fn sha256_is_not_checked_on_decode() -> anyhow::Result<()> {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let file = crate::encode(&demo, 1)?;
    let mut file_parts = parts(&file)?;
    file_parts.header = with_sha256(&file_parts.header, [0xff; 32]);
    let other = file_parts.build();
    let decoded = crate::decode(&other, 1)?;
    assert_eq!(decoded, demo);
    let mut read = Vec::new();
    std::io::Read::read_to_end(&mut crate::Decoder::new(Cursor::new(&other))?, &mut read)?;
    assert_eq!(read, decoded);
    let demo = synthetic(&[], false);
    let file = crate::encode(&[demo, vec![0; 5000]].concat(), 1)?;
    let intact = crate::decode(&file, 1)?;
    let mut bad = file;
    let at = (0..bad.len())
        .rev()
        .find(|&at| bad[at] != 0 && at > 200)
        .context("a non-zero byte")?;
    bad[at - 100] ^= 0x40;
    assert!(crate::decode(&bad, 1).is_err());
    assert!(!intact.is_empty());
    Ok(())
}
