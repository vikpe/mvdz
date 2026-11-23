//! Reviewers' hostile files, rebuilt for this format.

use super::common::*;
use crate::Error;
use crate::fields::write_usize;
use crate::format::*;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::io::{Cursor, Read as _};
use std::time::Instant;

fn small_demo_and_file(has_several_segments: bool) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    if has_several_segments {
        demo = several_segments(&demo);
    }
    let file = crate::encode(&demo, 1)?;
    Ok((demo, file))
}

fn refused(file: &[u8], what: &str, want: impl Fn(&Error) -> bool) {
    let start = Instant::now();
    assert!(readers_agree(file, what).is_none(), "{what}: accepted");
    let refusal = crate::decode(file, 2).err();
    assert!(
        refusal.as_ref().is_some_and(&want),
        "{what}: unexpected result {refusal:?}"
    );
    assert!(
        start.elapsed().as_secs_f64() < 5.0,
        "{what}: took {:?}",
        start.elapsed()
    );
}

fn is_corrupt(error: &Error) -> bool {
    matches!(error, Error::Corrupt(_))
}

#[test]
fn structure_rules() -> anyhow::Result<()> {
    let (demo, file) = small_demo_and_file(true)?;
    assert_eq!(
        readers_agree(&file, "base").context("every reader refuses")?,
        demo
    );
    let file_parts = parts(&file)?;
    assert!(file_parts.segments.len() > 3);
    assert_eq!(file_parts.build(), file);
    // decoding does not recompute the SHA-256, so the file decodes
    let mut edited = file_parts.clone();
    edited.header = with_sha256(&edited.header, [0xff; 32]);
    let other_sha256 = edited.build();
    assert_eq!(
        readers_agree(&other_sha256, "another SHA-256").context("every reader refuses")?,
        demo
    );
    assert_eq!(
        *crate::Decoder::new(Cursor::new(&other_sha256))?
            .header()
            .sha256(),
        [0xff; 32]
    );
    let mut edited = file_parts.clone();
    edited.segments.remove(2);
    refused(&edited.build(), "dropped segment", is_corrupt);
    let mut edited = file_parts.clone();
    edited.segments.swap(1, 2);
    refused(&edited.build(), "swapped segments", is_corrupt);
    let mut edited = file_parts.clone();
    edited.segments.insert(1, file_parts.segments[1].clone());
    refused(&edited.build(), "repeated segment", is_corrupt);
    refused(&file[..file.len() - 3], "cut", |error| {
        *error == Error::Corrupt("incomplete file")
    });
    let mut trailing = file.clone();
    trailing.push(0);
    refused(&trailing, "trailing byte", |error| {
        *error == Error::Corrupt("trailing bytes")
    });
    let mut edited = file_parts.clone();
    edited
        .segments
        .insert(1, segment_header_bytes(0, 0, crc32fast::hash(&[])));
    refused(&edited.build(), "empty segment", is_corrupt);
    let mut no_models = file[..HEADER_SIZE].to_vec();
    for segment in &file_parts.segments {
        write_segment(&mut no_models, segment);
    }
    refused(&no_models, "no models block", is_corrupt);
    for (bytes, what) in [
        (&b""[..], "empty"),
        (b"MVDZ", "magic"),
        (b"MVDZ\x03\x00\x00\x00", "v3"),
        (b"MVDZ\x01\x00\x00\x00", "v1"),
        (b"MVDZ\x02\x00", "header cut"),
        (&file[..HEADER_SIZE], "header only"),
    ] {
        assert!(readers_agree(bytes, what).is_none(), "{what}");
    }
    Ok(())
}

/// Robustness 3, 4, 12 and correctness F4.
#[test]
fn declared_sizes() -> anyhow::Result<()> {
    let (_, file) = small_demo_and_file(false)?;
    let file_parts = parts(&file)?;
    let mut edited = file_parts.clone();
    let mut segment = segment_header_bytes(u32::try_from(MAX_RAW_LEN)?, 0, 0);
    segment.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0, 0]);
    edited.header = with_size(&edited.header, MAX_RAW_LEN);
    edited.segments = vec![segment];
    refused(&edited.build(), "1 MiB segment in 9 bytes", is_corrupt);
    let mut edited = file_parts.clone();
    edited.segments[0].resize(SEGMENT_FIELDS_SIZE + usize::try_from(MAX_CODED_LEN)? + 1, 0);
    refused(&edited.build(), "coded length", |error| {
        *error == Error::Corrupt("segment size")
    });
    let mut edited = file_parts.clone();
    edited.header = with_size(&edited.header, 60 << 30);
    refused(&edited.build(), "60 GB header", is_corrupt);
    let mut huge_models = file[..HEADER_SIZE].to_vec();
    huge_models.extend_from_slice(&(256u32 << 20).to_le_bytes());
    huge_models.extend_from_slice(&[0; 100]);
    refused(&huge_models, "256 MiB models", |error| {
        *error == Error::Corrupt("models too large")
    });
    let mut past_end = file[..HEADER_SIZE].to_vec();
    write_models(&mut past_end, &file_parts.models);
    past_end.extend_from_slice(&u32::MAX.to_le_bytes());
    past_end.extend_from_slice(&file_parts.segments[0]);
    refused(&past_end, "segment length", is_corrupt);
    // whole-file decoding stops at the limit; streaming holds a few segments only
    let (demo, file) = small_demo_and_file(true)?;
    let max_memory = demo.len() as u64 - 1;
    assert_eq!(
        crate::decode::decode_with_max_memory(&file, 1, max_memory).err(),
        Some(Error::MemoryLimit("demo size"))
    );
    let mut streamed = Vec::new();
    crate::Decoder::new(Cursor::new(&file))?
        .max_threads(1)
        .max_memory(max_memory)
        .read_to_end(&mut streamed)?;
    assert!(streamed == demo);
    Ok(())
}

/// Spec review items 2-4, robustness 13.
#[test]
fn mutations_agree() -> anyhow::Result<()> {
    let mut rng = Rng(0x5eed);
    for is_long in [false, true] {
        let (_, file) = small_demo_and_file(is_long)?;
        for _ in 0..150 {
            let mut mutated = file.clone();
            match rng.below(3) {
                0 => {
                    for _ in 0..=rng.below(3) {
                        let at = rng.below(mutated.len());
                        mutated[at] ^= 1 << rng.below(8);
                    }
                }
                1 => mutated.truncate(rng.below(mutated.len())),
                _ => {
                    let at = rng.below(mutated.len());
                    mutated.insert(at, rng.next() as u8);
                }
            }
            readers_agree(&mutated, "mutation");
        }
    }
    Ok(())
}

/// A raw structural model; once built, every table takes a 16 KiB LUT.
fn wide_model(table_count: u16) -> Vec<u8> {
    let mut state = 1u32;
    let mut next_quantized = || {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (1 + (state >> 16) % 12) as u8
    };
    let mut model = Vec::new();
    write_usize(&mut model, usize::from(table_count));
    for context in 0..table_count {
        model.extend_from_slice(&context.to_le_bytes());
    }
    for _ in 0..(usize::from(table_count) + 1) * 256 {
        model.push(next_quantized());
    }
    model
}

#[test]
fn large_models_block() -> anyhow::Result<()> {
    let demo = usercmd_demo();
    let file = crate::encode(&several_segments(&demo), 1)?;
    let mut file_parts = parts(&file)?;
    assert!(!file_parts.models.is_empty());
    let model = wide_model(512);
    let raw_models: Vec<(usize, &[u8])> = (0..crate::streams::FIRST_NUMERIC_STREAM)
        .map(|stream| (stream, &model[..]))
        .collect();
    file_parts.models = crate::rans::write_models_block(&raw_models)?.body;
    let read_models = |max_memory: u64| {
        crate::rans::read_models_block(&file_parts.models, 1, max_memory).map(|_| ())
    };
    // the models: 17 x 513 LUTs of 16 KiB, over 128 MiB
    let max_memory: u64 = 128 << 20;
    assert_eq!(
        read_models(max_memory).err(),
        Some(Error::MemoryLimit("models"))
    );
    let error = crate::decode::decode_with_max_memory(&file_parts.build(), 1, max_memory).err();
    assert_eq!(error, Some(Error::MemoryLimit("models")));
    let start = Instant::now();
    read_models(crate::memory::DEFAULT_MAX_MEMORY)?;
    assert!(
        start.elapsed().as_secs_f64() < 10.0,
        "reading the models took {:?}",
        start.elapsed()
    );
    Ok(())
}
