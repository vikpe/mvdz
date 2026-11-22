//! Reviewers' hostile files, rebuilt for this format.

use super::common::*;
use crate::Error;
use crate::fields::put_usize;
use crate::format::*;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::assert_matches;
use std::io::Cursor;
use std::time::Instant;

fn small_demo_and_file(has_several_segments: bool) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    if has_several_segments {
        demo = several_segments(&demo);
    }
    let file = compress(&demo)?;
    Ok((demo, file))
}

fn refused(file: &[u8], what: &str, want: impl Fn(&Error) -> bool) {
    let start = Instant::now();
    assert!(readers_agree(file, what).is_none(), "{what}: accepted");
    let refusal = decompress(file, 2).err();
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
    edited.header.sha256 = [0xff; 32];
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
    let mut no_models = file[..HEADER_LEN].to_vec();
    for segment in &file_parts.segments {
        segment_block(&mut no_models, segment);
    }
    refused(&no_models, "no models block", is_corrupt);
    let mut checksum_set = file.clone();
    checksum_set[30] = 1;
    refused(&checksum_set, "checksum field", |error| {
        *error == Error::Corrupt("header checksum field")
    });
    for (bytes, what) in [
        (&b""[..], "empty"),
        (b"MVDZ", "magic"),
        (b"MVDZ\x03\x00\x00\x00", "v3"),
        (b"MVDZ\x01\x00\x00\x00", "v1"),
        (b"MVDZ\x02\x00", "header cut"),
        (&file[..HEADER_LEN], "header only"),
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
    let mut segment = segment_header_bytes(u32::try_from(MAX_SEGMENT_RAW)?, 0, 0);
    segment.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0, 0]);
    edited.header.size_bytes = MAX_SEGMENT_RAW;
    edited.segments = vec![segment];
    refused(&edited.build(), "1 MiB segment in 9 bytes", is_corrupt);
    let mut edited = file_parts.clone();
    edited.segments[0].resize(
        SEGMENT_FIELDS_LEN + usize::try_from(MAX_SEGMENT_CODED)? + 1,
        0,
    );
    refused(&edited.build(), "coded length", |error| {
        *error == Error::Corrupt("segment size")
    });
    let mut edited = file_parts.clone();
    edited.header.size_bytes = 60 << 30;
    refused(&edited.build(), "60 GB header", is_corrupt);
    let mut huge_models = file[..HEADER_LEN].to_vec();
    huge_models.extend_from_slice(&(256u32 << 20).to_le_bytes());
    huge_models.extend_from_slice(&[0; 100]);
    refused(&huge_models, "256 MiB models", |error| {
        *error == Error::Corrupt("models too large")
    });
    let mut past_end = file[..HEADER_LEN].to_vec();
    models_block(&mut past_end, &file_parts.models);
    past_end.extend_from_slice(&u32::MAX.to_le_bytes());
    past_end.extend_from_slice(&file_parts.segments[0]);
    refused(&past_end, "segment length", is_corrupt);
    let (demo, file) = small_demo_and_file(false)?;
    let max_memory = demo.len() as u64 / 2;
    assert_matches!(
        crate::decode::decompress(&file, 1, max_memory),
        Err(Error::MemoryLimit(_))
    );
    assert!(
        crate::decode::Decoder::with_max_memory(Cursor::new(&file), 1, max_memory)
            .and_then(|mut reader| reader.read_demo(&mut [0; 10]))
            .is_err()
    );
    Ok(())
}

/// (value, length in bytes)
fn leb128(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0usize;
    for (index, &byte) in bytes.iter().enumerate().take(5) {
        value |= usize::from(byte & 0x7f) << (7 * index);
        if byte < 0x80 {
            return Some((value, index + 1));
        }
    }
    None
}

/// Robustness 5: a 2.3 KB model header took 1.3 GB and 3.6 s before rule M2.
#[test]
fn model_amplification() -> anyhow::Result<()> {
    let Some(dir) = test_data("poc") else {
        return Ok(());
    };
    let demo = synthetic(&[], false);
    let file = compress(&demo)?;
    let file_parts = parts(&file)?;
    for name in ["v2model_1000.bin", "v2model_16000.bin", "v2model_65535.bin"] {
        // the reviewer's layout: LEB128 header length, header, rANS data
        let Ok(proof_of_concept) = std::fs::read(dir.join(name)) else {
            continue;
        };
        let (header_len, at) = leb128(&proof_of_concept).context("the model's header length")?;
        let header = proof_of_concept
            .get(at..at + header_len)
            .context("the model's header")?;
        let rest = &proof_of_concept[at + header_len..];
        let mut models = Vec::new();
        for field in [1, 0, header.len()] {
            put_usize(&mut models, field);
        }
        models.extend_from_slice(header);
        // fte, fte2, mvd1, tail; one entry (id 0, count, length); no blob body
        let mut body = Vec::new();
        for field in [0, 0, 0, 0, 1, 0, demo.len(), rest.len(), 0] {
            put_usize(&mut body, field);
        }
        body.extend_from_slice(rest);
        let mut segment = segment_header_bytes(
            u32::try_from(demo.len())?,
            u32::try_from(file_parts.header.duration_ms)?,
            crc32fast::hash(&demo),
        );
        segment.extend_from_slice(&body);
        let mut edited = file_parts.clone();
        edited.models = models;
        edited.segments = vec![segment];
        refused(&edited.build(), name, |error| {
            *error == Error::Corrupt("model")
        });
    }
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

/// Legal under rule M2; once built, every table takes a 16 KiB LUT.
fn wide_model(table_count: u32) -> Vec<u8> {
    use crate::coder::rc::{IntModel, RangeEncoder};
    const NQC: usize = 306;
    let mut encoder = RangeEncoder::new();
    let mut int_model = IntModel::new(NQC + 8);
    let mut state = 1u32;
    let mut next_q = || {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        1 + (state >> 16) % 12
    };
    int_model.put(&mut encoder, NQC, 256);
    int_model.put(&mut encoder, NQC + 1, table_count);
    let fallback: Vec<u32> = Vec::from_fn(256, |_| next_q());
    let mut previous = 0;
    for &q_value in &fallback {
        int_model.put(&mut encoder, 17 * 17 + previous.min(16) as usize, q_value);
        previous = q_value;
    }
    for table in 0..table_count {
        int_model.put(&mut encoder, NQC + 2, u32::from(table > 0));
        let mut previous = 0u32;
        for &fallback_q in &fallback {
            let q_value = next_q();
            int_model.put(
                &mut encoder,
                fallback_q.min(16) as usize * 17 + previous.min(16) as usize,
                q_value,
            );
            previous = q_value;
        }
    }
    encoder.finish()
}

#[test]
fn large_models_block() -> anyhow::Result<()> {
    let demo = usercmd_demo();
    let file = compress(&several_segments(&demo))?;
    let mut file_parts = parts(&file)?;
    assert!(!file_parts.models.is_empty());
    let model = wide_model(512);
    let mut models = Vec::new();
    let lazy: Vec<usize> = (0..crate::streams::LAZY_STREAM_COUNT)
        .filter(|&stream| crate::streams::is_lazy(stream))
        .collect();
    put_usize(&mut models, lazy.len());
    for stream in lazy {
        put_usize(&mut models, stream);
        put_usize(&mut models, model.len());
        models.extend_from_slice(&model);
    }
    file_parts.models = models;
    let wide = file_parts.build();
    // the models: 16 x 513 LUTs of 16 KiB, just over 128 MiB
    let max_memory: u64 = 128 << 20;
    let error = scan(&wide, max_memory).err();
    assert_eq!(error, Some(Error::MemoryLimit("memory")));
    let error = crate::decode::decompress(&wide, 1, max_memory).err();
    assert_eq!(error, Some(Error::MemoryLimit("memory")));
    let start = Instant::now();
    scan(&wide, crate::budget::DEFAULT_MAX_MEMORY)?;
    assert!(
        start.elapsed().as_secs_f64() < 10.0,
        "reading the models took {:?}",
        start.elapsed()
    );
    Ok(())
}
