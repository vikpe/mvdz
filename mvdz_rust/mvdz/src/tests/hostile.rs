//! Hostile files: each one is refused (or accepted) by every reader alike,
//! with the expected error, quickly and within the memory budget. These
//! are the reviewers' proofs of concept rebuilt for this format.

use super::common::*;
use crate::Error;
use crate::format::*;
use crate::util::put_u32;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::assert_matches;
use std::io::Cursor;
use std::time::Instant;

/// `is_long`: the demo repeated to several segments.
fn small_demo_and_file(is_long: bool) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    if is_long {
        demo = several_segments(&demo);
    }
    let file = compress(&demo)?;
    Ok((demo, file))
}

/// Refused by every reader within `secs`, with `want` matching the error.
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
    // rebuilt unchanged: the same file
    assert_eq!(file_parts.build(), file);
    // another SHA-256 in the header: the SHA-256 names the demo and
    // decoding does not recompute it, so the file decodes
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
    // a segment dropped
    let mut edited = file_parts.clone();
    edited.segments.remove(2);
    refused(&edited.build(), "dropped segment", is_corrupt);
    // segments swapped
    let mut edited = file_parts.clone();
    edited.segments.swap(1, 2);
    refused(&edited.build(), "swapped segments", is_corrupt);
    // a segment twice
    let mut edited = file_parts.clone();
    edited.segments.insert(1, file_parts.segments[1].clone());
    refused(&edited.build(), "repeated segment", is_corrupt);
    // the last segment cut short
    refused(&file[..file.len() - 3], "cut", |error| {
        *error == Error::Corrupt("incomplete file")
    });
    // data after the last segment
    let mut trailing = file.clone();
    trailing.push(0);
    refused(&trailing, "trailing byte", |error| {
        *error == Error::Corrupt("trailing bytes")
    });
    // an empty segment in between
    let mut edited = file_parts.clone();
    edited
        .segments
        .insert(1, segment_header_bytes(0, 0, crate::util::crc32(&[])));
    refused(&edited.build(), "empty segment", is_corrupt);
    // the models block missing
    let mut no_models = file[..HEADER_LEN].to_vec();
    for segment in &file_parts.segments {
        segment_block(&mut no_models, segment);
    }
    refused(&no_models, "no models block", is_corrupt);
    // the high half of the header's checksum field set
    let mut checksum_set = file.clone();
    checksum_set[30] = 1;
    refused(&checksum_set, "checksum field", |error| {
        *error == Error::Corrupt("header checksum field")
    });
    // empty file, magic only, other versions, a cut header
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

/// Declared sizes never reach the allocator unbounded (robustness 3, 4,
/// 12; correctness F4): each is refused by a format rule or the budget.
#[test]
fn declared_sizes() -> anyhow::Result<()> {
    let (_, file) = small_demo_and_file(false)?;
    let file_parts = parts(&file)?;
    // a segment that claims the largest raw length in 9 bytes that are
    // not a transform body: its output buffer is within the budget, and
    // decoding refuses it
    let mut edited = file_parts.clone();
    let mut segment = segment_header_bytes(u32::try_from(MAX_SEGMENT_RAW)?, 0, 0);
    segment.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0, 0]);
    edited.header.size_bytes = MAX_SEGMENT_RAW;
    edited.segments = vec![segment];
    refused(&edited.build(), "1 MiB segment in 9 bytes", is_corrupt);
    // a segment one byte over the largest coded length: refused at its
    // length field
    let mut edited = file_parts.clone();
    edited.segments[0].resize(
        SEGMENT_FIELDS_LEN + usize::try_from(MAX_SEGMENT_CODED)? + 1,
        0,
    );
    refused(&edited.build(), "coded length", |error| {
        *error == Error::Corrupt("segment size")
    });
    // a header claiming 60 GB
    let mut edited = file_parts.clone();
    edited.header.size_bytes = 60 << 30;
    refused(&edited.build(), "60 GB header", is_corrupt);
    // a models block of 256 MiB declared in a few bytes
    let mut huge_models = file[..HEADER_LEN].to_vec();
    huge_models.extend_from_slice(&(256u32 << 20).to_le_bytes());
    huge_models.extend_from_slice(&[0; 100]);
    refused(&huge_models, "256 MiB models", |error| {
        *error == Error::Corrupt("models too large")
    });
    // a segment whose length field runs past the end of the file
    let mut past_end = file[..HEADER_LEN].to_vec();
    models_block(&mut past_end, &file_parts.models);
    past_end.extend_from_slice(&u32::MAX.to_le_bytes());
    past_end.extend_from_slice(&file_parts.segments[0]);
    refused(&past_end, "segment length", is_corrupt);
    // the caller's memory budget
    let (demo, file) = small_demo_and_file(false)?;
    let small = crate::budget::Limits {
        memory: demo.len() as u64 / 2,
        ..Default::default()
    };
    assert_matches!(
        crate::decode::decompress(&file, 1, &small),
        Err(Error::Limit(_))
    );
    assert!(
        crate::stream::Decoder::with_limits(Cursor::new(&file), 1, &small)
            .and_then(|mut reader| reader.read_demo(&mut [0; 10]))
            .is_err()
    );
    Ok(())
}

/// An unsigned LEB128 at the start of `bytes` and its length in bytes.
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

/// The reviewer's amplifying v2 model headers (robustness 5: a 2.3 KB
/// header took 1.3 GB and 3.6 s) in the models block: refused at once by
/// the model rules (decisions <= 128 per header byte + 1024).
#[test]
fn model_amplification() -> anyhow::Result<()> {
    let Some(dir) = test_data("poc") else {
        return Ok(());
    };
    let demo = synthetic(&[], false);
    let file = compress(&demo)?;
    let file_parts = parts(&file)?;
    for name in ["v2model_1000.bin", "v2model_16000.bin", "v2model_65535.bin"] {
        // a model and a rANS stream body: header length (LEB128, the
        // reviewer's format), header, rANS data
        let Ok(proof_of_concept) = std::fs::read(dir.join(name)) else {
            continue;
        };
        let (header_len, at) = leb128(&proof_of_concept).context("the model's header length")?;
        let header = proof_of_concept
            .get(at..at + header_len)
            .context("the model's header")?;
        let rest = &proof_of_concept[at + header_len..];
        // the models block: one model, the time stream's
        let mut models = Vec::new();
        for field in [1, 0, header.len()] {
            put_u32(&mut models, field);
        }
        models.extend_from_slice(header);
        // a segment whose only stream is a time stream: fte, fte2, mvd1,
        // tail; one entry (id 0, count, length); no blob body
        let mut body = Vec::new();
        for field in [0, 0, 0, 0, 1, 0, demo.len(), rest.len(), 0] {
            put_u32(&mut body, field);
        }
        body.extend_from_slice(rest);
        let mut segment = segment_header_bytes(
            u32::try_from(demo.len())?,
            u32::try_from(file_parts.header.duration_ms)?,
            crate::util::crc32(&demo),
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

/// Mutated files: every reader decides the same way (spec review items
/// 2-4, robustness 13).
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

/// A model header (FORMAT.md 8.3) of `table_count` own tables and the
/// fallback, each with all 256 symbols present and pseudo-random q values:
/// legal (rule M2 holds), and every table takes a 16 KiB slot LUT once built.
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

/// A large, legal models block: its models are built when the file is
/// opened, under the reader's budget, which refuses them, for the file
/// information as for a decode. With room for them the file opens, quickly.
#[test]
fn large_models_block() -> anyhow::Result<()> {
    let demo = usercmd_demo();
    let file = compress(&several_segments(&demo))?;
    let mut file_parts = parts(&file)?;
    assert!(!file_parts.models.is_empty());
    let model = wide_model(512);
    let mut models = Vec::new();
    // a model for every structural stream
    let lazy: Vec<usize> = (0..crate::streams::LAZY_STREAM_COUNT)
        .filter(|&stream| crate::streams::is_lazy(stream))
        .collect();
    put_u32(&mut models, lazy.len());
    for stream in lazy {
        put_u32(&mut models, stream);
        put_u32(&mut models, model.len());
        models.extend_from_slice(&model);
    }
    file_parts.models = models;
    let wide = file_parts.build();
    // the models: 16 x 513 LUTs of 16 KiB, just over 128 MiB
    let limits = crate::budget::Limits {
        memory: 128 << 20,
        ..Default::default()
    };
    let error = scan(&wide, &limits).err();
    assert_eq!(error, Some(Error::Limit("memory")));
    let error = crate::decode::decompress(&wide, 1, &limits).err();
    assert_eq!(error, Some(Error::Limit("memory")));
    let start = Instant::now();
    scan(&wide, &crate::budget::Limits::default())?;
    assert!(
        start.elapsed().as_secs_f64() < 10.0,
        "reading the models took {:?}",
        start.elapsed()
    );
    Ok(())
}
