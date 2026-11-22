//! Reviewers' reproducers (robustness R-*, correctness C-*, spec D-*), each failing before its fix.

use super::common::*;
use crate::Error;
use crate::fields::put_usize;
use crate::format::*;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::io::Cursor;
use std::time::{Duration, Instant};

fn small_demo_and_file() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let file = compress(&demo)?;
    Ok((demo, file))
}

/// R-M2: a padded header of 65535 tables is refused before any table is built.
#[test]
fn r_m2_padded_model() {
    use crate::coder::rc::{IntModel, RangeEncoder};
    const NQC: usize = 17 * 18;
    let table_count = 65535u32;
    let mut encoder = RangeEncoder::new();
    let mut int_model = IntModel::new(NQC + 8);
    int_model.put(&mut encoder, NQC, 256);
    int_model.put(&mut encoder, NQC + 1, table_count);
    let mut previous = 0usize;
    for _ in 0..256 {
        int_model.put(&mut encoder, 17 * 17 + previous, 0);
        previous = 0;
    }
    for table in 0..table_count {
        int_model.put(&mut encoder, NQC + 2, u32::from(table > 0));
        for _ in 0..256 {
            int_model.put(&mut encoder, 0, 0);
        }
    }
    let decisions = encoder.decisions;
    let mut header = encoder.finish();
    header.resize(
        header
            .len()
            .max((decisions.saturating_sub(1024) / 128 + 1) as usize),
        0,
    );
    let mut models = Vec::new();
    for field in [1, 0, header.len()] {
        put_usize(&mut models, field);
    }
    models.extend_from_slice(&header);
    let mut rans_body = Vec::new();
    put_usize(&mut rans_body, 0);
    let raw_len = table_count + 100;
    // fte, fte2, mvd1, tail; one entry (id 0, count, length); no blob body
    let mut body = Vec::new();
    for field in [
        0,
        0,
        0,
        0,
        1,
        0,
        table_count as usize + 16,
        rans_body.len(),
        0,
    ] {
        put_usize(&mut body, field);
    }
    body.extend_from_slice(&rans_body);
    let mut segment = segment_header_bytes(raw_len, 0, 0);
    segment.extend_from_slice(&body);
    let file = Parts {
        header: Header {
            size_bytes: u64::from(raw_len),
            ..Default::default()
        },
        models,
        segments: vec![segment],
    }
    .build();
    let start = Instant::now();
    let max_memory: u64 = 64 << 20;
    assert_eq!(
        crate::decode::decompress(&file, 1, max_memory).err(),
        Some(Error::Corrupt("model"))
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
}

/// R-M4: every segment's record is charged, so many tiny segments hit the memory limit.
#[test]
fn r_m4_segment_records_are_charged() -> anyhow::Result<()> {
    // the encoder cuts at 256 KiB, so one segment per frame is built by hand
    let frame = [0, 1, 1, 0, 0, 0, 1]; // dem_read holding one svc_nop
    let segment_count = 60_000;
    let demo = frame.repeat(segment_count);
    let one = parts(&compress(&frame)?)?;
    assert_eq!(one.segments.len(), 1);
    let file = Parts {
        header: parts(&compress(&demo)?)?.header,
        models: one.models,
        segments: vec![one.segments[0].clone(); segment_count],
    }
    .build();
    assert_eq!(decompress(&file, 1)?, demo);
    // only the 60,000 segment records (2.9 MB) exceed it, not the 0.4 MB demo
    let max_memory: u64 = 2 << 20;
    assert_eq!(
        crate::decode::decompress(&file, 1, max_memory).err(),
        Some(Error::MemoryLimit("memory"))
    );
    let mut reader = crate::decode::Decoder::with_max_memory(Cursor::new(&file), 1, max_memory)?;
    let mut read_all = || -> crate::Result<()> {
        while reader.read_demo(&mut [0; 4096])? > 0 {}
        Ok(())
    };
    assert_eq!(read_all().err(), Some(Error::MemoryLimit("memory")));
    Ok(())
}

#[test]
fn sha256_is_not_checked_on_decode() -> anyhow::Result<()> {
    let (demo, file) = small_demo_and_file()?;
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
