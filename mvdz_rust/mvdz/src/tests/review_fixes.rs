//! Regressions for the final reviews (robustness R-*, correctness C-*,
//! spec D-*): each test is a reviewer's reproducer, rebuilt here, that
//! failed before the fix.

use super::common::*;
use crate::Error;
use crate::format::*;
use crate::util::put_u32;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::io::Cursor;
use std::time::{Duration, Instant};

fn small_demo_and_file() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let file = compress(&demo)?;
    Ok((demo, file))
}

/// R-M2: a model of 65535 tables whose header is padded to satisfy rule
/// M2 is refused before any table is built (the header must be consumed
/// exactly).
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
    // the models block: one model, the time stream's
    let mut models = Vec::new();
    for field in [1, 0, header.len()] {
        put_u32(&mut models, field);
    }
    models.extend_from_slice(&header);
    // an empty rANS body
    let mut rans_body = Vec::new();
    put_u32(&mut rans_body, 0);
    let raw_len = table_count + 100;
    // a transform body whose only stream is a time stream: fte, fte2,
    // mvd1, tail; one entry (id 0, count, length); no blob body
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
        put_u32(&mut body, field);
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

/// R-M4: a long run of tiny segments is charged to the budget: a
/// sequential reader stops with a limit error instead of growing.
#[test]
fn r_m4_section_bookkeeping() -> anyhow::Result<()> {
    // one segment per frame. The encoder cuts at 256 KiB, so the file is
    // built by hand: the segment of a one-frame demo, 60,000 times, under
    // the header of the whole demo
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
    // the demo is 0.4 MB and a sequential reader holds one segment at a
    // time: only the record kept per segment (64 bytes each, 3.8 MB) can
    // exceed this
    let max_memory: u64 = 3 << 20;
    assert_eq!(
        crate::decode::decompress(&file, 1, max_memory).err(),
        Some(Error::Limit("memory"))
    );
    let mut reader = crate::stream::Decoder::with_max_memory(Cursor::new(&file), 1, max_memory)?;
    let mut read_all = || -> crate::Result<()> {
        while reader.read_demo(&mut [0; 4096])? > 0 {}
        Ok(())
    };
    assert_eq!(read_all().err(), Some(Error::Limit("memory")));
    Ok(())
}

/// Decoding does not recompute the SHA-256: a file whose header carries
/// another one decodes in every reader; a damaged segment does not.
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
    // one byte of a segment changed: decoding refuses it
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
