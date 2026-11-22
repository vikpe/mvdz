//! Cross-implementation golden vectors (`tests/vectors`, `MANIFEST.tsv`).
//! `MVDZ_WRITE_VECTORS=1` rewrites them instead of checking them.

use super::common::*;
use crate::fields::put_usize;
use crate::format::*;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors")
}

fn manifest() -> anyhow::Result<Vec<Vec<String>>> {
    Ok(std::fs::read_to_string(dir().join("MANIFEST.tsv"))?
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .map(|line| line.split('\t').map(String::from).collect())
        .collect())
}

#[test]
fn vectors_decode() -> anyhow::Result<()> {
    let rows = manifest()?;
    assert!(rows.len() >= 30, "the vector set is committed");
    for row in rows {
        let (file, expected) = (&row[0], &row[1]);
        let bytes = std::fs::read(dir().join(file)).with_context(|| file.clone())?;
        let got = readers_agree(&bytes, file);
        if let Some(expected_error) = expected.strip_prefix("error: ") {
            assert!(got.is_none(), "{file}: accepted");
            let refusal = decompress(&bytes, 1).err().map(|error| error.to_string());
            assert_eq!(refusal.as_deref(), Some(expected_error), "{file}: error");
        } else {
            let decoded =
                got.with_context(|| format!("{file}: refused: {:?}", decompress(&bytes, 1).err()))?;
            assert_eq!(
                &hex(&Sha256::digest(&decoded)),
                expected,
                "{file}: decoded SHA-256"
            );
            assert_eq!(decoded.len().to_string(), row[2], "{file}: decoded size");
        }
    }
    Ok(())
}

/// (file, bytes, decoded demo or None for a hostile file, how it was made)
type Vector = (String, Vec<u8>, Option<Vec<u8>>, String);

fn escapes() -> Vec<u8> {
    let mut demo = Demo(synthetic(&[], false));
    for index in 0..30u8 {
        let mut payload = message(8, &[&[2], &c_string("before\n")]);
        payload.extend_from_slice(&[0xEE, index, 1, 2, 3]);
        demo.frame(1, 6, &payload);
        demo.frame(0, 6, &[14, 1, 2]);
        let mut hidden = Vec::new();
        hidden.extend_from_slice(&1000u32.to_le_bytes());
        hidden.extend_from_slice(&7u16.to_le_bytes());
        hidden.extend_from_slice(&[index; 9]);
        demo.0.push(2);
        demo.0.push(3);
        demo.0.extend_from_slice(&0u32.to_le_bytes());
        demo.0
            .extend_from_slice(&(hidden.len() as u32).to_le_bytes());
        demo.0.extend_from_slice(&hidden);
    }
    demo.0
}

fn multi_demoinfo() -> Vec<u8> {
    let parts = [
        (1, "{\"a\":1,"),
        (2, "\"b\":2"),
        (0, "}"),
        (1, "{\"a\":3,"),
        (2, "\"b\":4"),
        (0, "}"),
    ];
    let mut demo = synthetic(&[], false);
    demo.extend(synthetic(&parts[..3], false));
    demo.extend(synthetic(&parts[3..], false));
    demo
}

/// Robustness reviews 1 and 5: cheap to write, costly to build under an adaptive coder.
fn amplifying_model(table_count: u32) -> Vec<u8> {
    use crate::coder::rc::{IntModel, RangeEncoder};
    const NQC: usize = 306;
    let mut encoder = RangeEncoder::new();
    let mut int_model = IntModel::new(NQC + 8);
    int_model.put(&mut encoder, NQC, 256);
    int_model.put(&mut encoder, NQC + 1, table_count);
    let mut previous = 0usize;
    for _ in 0..256 {
        int_model.put(&mut encoder, 17 * 17 + previous.min(16), 1);
        previous = 1;
    }
    for table in 0..table_count {
        int_model.put(&mut encoder, NQC + 2, u32::from(table > 0));
        let mut previous = 0usize;
        for _ in 0..256 {
            int_model.put(&mut encoder, 17 + previous.min(16), 1);
            previous = 1;
        }
    }
    let header = encoder.finish();
    let mut body = Vec::new();
    put_usize(&mut body, header.len());
    body.extend_from_slice(&header);
    put_usize(&mut body, 16);
    body.extend_from_slice(&[0; 16]);
    body
}

/// Returns (models block body, transform body).
fn transform_body(
    lazy: &[(usize, &[u8])],
    blobs: &[(usize, &[u8])],
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    use crate::coder::rans;
    let mut models: Vec<Option<rans::FileModel>> = Vec::new();
    let mut ids = Vec::new();
    let mut bodies = Vec::new();
    for (id, symbols) in lazy {
        let contexts = vec![0u16; symbols.len()];
        let mut tally = rans::Tally::new(256);
        tally.add(symbols, &contexts);
        let model = tally.file_model().context("a model")?;
        bodies.push(rans::encode_bytes(symbols, &contexts, &model));
        models.resize_with(models.len().max(id + 1), || None);
        models[*id] = Some(model);
        ids.push(*id);
    }
    let lazy: Vec<(usize, usize, &[u8])> = lazy
        .iter()
        .zip(&bodies)
        .map(|((id, symbols), body)| (*id, symbols.len(), &body[..]))
        .collect();
    let lazy = &lazy;
    let mut body = Vec::new();
    // fte, fte2, mvd1, tail; the number of streams
    for field in [0, 0, 0, 0, lazy.len() + blobs.len()] {
        put_usize(&mut body, field);
    }
    for (id, count, stream_body) in lazy {
        for field in [*id, *count, stream_body.len()] {
            put_usize(&mut body, field);
        }
    }
    let mut blob_bytes = Vec::new();
    for (id, blob) in blobs {
        put_usize(&mut body, *id);
        put_usize(&mut body, blob.len());
        blob_bytes.extend_from_slice(blob);
    }
    let deflated = if blob_bytes.is_empty() {
        Vec::new()
    } else {
        crate::deflate::compress(&blob_bytes)
    };
    put_usize(&mut body, deflated.len());
    for (_, _, stream_body) in lazy {
        body.extend_from_slice(stream_body);
    }
    body.extend_from_slice(&deflated);
    Ok((crate::segment::models_body(&models, &ids), body))
}

fn one_segment(demo: &[u8], (models, body): &(Vec<u8>, Vec<u8>)) -> anyhow::Result<Vec<u8>> {
    let ms = crate::segment::frame_scan(demo).duration_ms;
    let crc = crc32fast::hash(demo);
    let mut segment = segment_header_bytes(u32::try_from(demo.len())?, u32::try_from(ms)?, crc);
    segment.extend_from_slice(body);
    Ok(Parts {
        header: Header {
            size_bytes: demo.len() as u64,
            duration_ms: ms,
            crc32: crc,
            sha256: Sha256::digest(demo).into(),
        },
        models: models.clone(),
        segments: vec![segment],
    }
    .build())
}

fn hostile(vectors: &mut Vec<Vector>) -> anyhow::Result<()> {
    let mut add = |name: &str, file: Vec<u8>, how: &str| {
        vectors.push((format!("hostile-{name}.mvdz"), file, None, how.to_string()));
    };
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let base = compress(&several_segments(&demo))?;
    let file_parts = parts(&base)?;
    assert!(file_parts.segments.len() > 3);
    add(
        "base-cut",
        base[..base.len() - 10].to_vec(),
        "the base file (a synthetic demo repeated to several segments) without its last 10 bytes",
    );
    let mut edited_file = base.clone();
    edited_file.push(0);
    add(
        "trailing-byte",
        edited_file,
        "one byte after the last segment",
    );
    let mut edited = file_parts.clone();
    edited.header.size_bytes += 1;
    add("header-size", edited.build(), "the header's size + 1");
    let mut edited = file_parts.clone();
    edited.header.duration_ms += 1;
    add(
        "header-duration",
        edited.build(),
        "the header's duration_ms + 1",
    );
    let mut edited = file_parts.clone();
    edited.header.crc32 ^= 1;
    add(
        "header-crc",
        edited.build(),
        "one bit of the header's CRC-32 flipped",
    );
    let mut edited_file = base.clone();
    edited_file[30] = 1;
    add(
        "header-checksum-field",
        edited_file,
        "the high half of the header's checksum field set",
    );
    let mut edited_file = base.clone();
    edited_file[4] = 3;
    add("version", edited_file, "version 3");
    let mut edited = file_parts.clone();
    edited.segments.remove(2);
    add(
        "dropped-segment",
        edited.build(),
        "the third segment removed",
    );
    let mut edited = file_parts.clone();
    edited.segments.swap(1, 2);
    add(
        "swapped-segments",
        edited.build(),
        "the second and third segments swapped",
    );
    let mut edited = file_parts.clone();
    edited.segments.insert(1, file_parts.segments[1].clone());
    add(
        "repeated-segment",
        edited.build(),
        "the second segment twice",
    );
    let mut edited = file_parts.clone();
    edited.segments[1][SEGMENT_FIELDS_LEN - 2] ^= 0x10;
    add(
        "segment-crc",
        edited.build(),
        "one bit of the second segment's CRC-32 flipped",
    );
    let mut edited = file_parts.clone();
    edited
        .segments
        .insert(1, segment_header_bytes(0, 0, crc32fast::hash(&[])));
    add("segment-empty", edited.build(), "an empty segment");
    let mut edited = file_parts.clone();
    edited.segments[0].resize(
        SEGMENT_FIELDS_LEN + usize::try_from(MAX_SEGMENT_CODED)? + 1,
        0,
    );
    add(
        "segment-length",
        edited.build(),
        "the first segment padded with zeros to one byte over the largest coded length (2 MiB)",
    );
    let mut edited = parts(&compress(&synthetic(&[], false))?)?;
    assert_eq!(edited.segments.len(), 1);
    let duration_ms = u32::from_le_bytes(edited.segments[0][4..8].try_into()?);
    edited.segments[0][4..8].copy_from_slice(&(duration_ms + 1).to_le_bytes());
    edited.header.duration_ms += 1;
    add(
        "segment-duration",
        edited.build(),
        "a segment whose dur_ms is 1 more than its frame times",
    );
    let mut edited_file = base[..HEADER_LEN].to_vec();
    edited_file.extend_from_slice(&(256u32 << 20).to_le_bytes());
    edited_file.extend_from_slice(&[0; 100]);
    add(
        "models-size",
        edited_file,
        "a models block of 256 MiB declared in a few bytes",
    );
    let mut edited_file = base[..HEADER_LEN].to_vec();
    for segment in &file_parts.segments {
        segment_block(&mut edited_file, segment);
    }
    add(
        "models-missing",
        edited_file,
        "the base file without its models block",
    );
    let mut one = Demo(Vec::new());
    one.hidden(
        0,
        &[(1, &usercmd_block(0, 0, 13, [0.0; 3], [0; 3], [0, 0]))],
    );
    let one = one.0;
    let usercmd_segment = |usercmd: Option<&[u8]>, misc: Option<&[u8]>| {
        use crate::streams::*;
        let mut lazy: Vec<(usize, &[u8])> = vec![(TIME, &[0]), (TYPE, &[3])];
        if let Some(usercmd) = usercmd {
            lazy.push((UC, usercmd));
        }
        let mut blobs: Vec<(usize, &[u8])> = vec![
            (MASK, &[0; 4]),
            (HIDTAG, &[1, 2]),
            (HIDLEN, &[23, 0, 0, 0]),
            (HIDTYPE, &[1, 0]),
        ];
        if let Some(misc) = misc {
            blobs.push((HIDMISC, misc));
        }
        one_segment(&one, &transform_body(&lazy, &blobs)?)
    };
    add(
        "ucmd-grid",
        usercmd_segment(Some(&[0, 0, 13, 9]), None)?,
        "a usercmd block whose grid flags symbol is 9",
    );
    let misc = usercmd_block(0, 0, 13, [0.0; 3], [0; 3], [0, 0]);
    add(
        "ucmd-misc",
        usercmd_segment(None, Some(&misc))?,
        "a usercmd block (type 1, 23 bytes) stored in hid.misc, no uc stream",
    );
    models_block(&mut add)
}

fn models_block(add: &mut impl FnMut(&str, Vec<u8>, &str)) -> anyhow::Result<()> {
    let usercmd = usercmd_demo();
    let file = compress(&several_segments(&usercmd))?;
    let file_parts = parts(&file)?;
    assert!(
        !file_parts.models.is_empty(),
        "a segmented normal file has models"
    );
    let entries = models_entries(&file_parts.models)?;
    let with_entries = |entries: &[(usize, Vec<u8>)]| {
        let mut edited = file_parts.clone();
        edited.models = models_body(entries);
        edited.build()
    };
    let mut edited = entries.clone();
    edited.remove(0);
    add(
        "tables-no-entry",
        with_entries(&edited),
        "the models block without the entry of a stream the segments code with rANS",
    );
    let mut edited = file_parts.clone();
    edited.models.clear();
    add(
        "tables-missing",
        edited.build(),
        "an empty models block: rANS streams without a model",
    );
    let mut edited = entries.clone();
    edited.swap(0, 1);
    add(
        "tables-order",
        with_entries(&edited),
        "two entries of the models block swapped (stream ids not increasing)",
    );
    let mut edited = entries.clone();
    edited.insert(1, edited[0].clone());
    add(
        "tables-dup-id",
        with_entries(&edited),
        "an entry of the models block repeated (the same stream id twice)",
    );
    let lazy_entry = entries
        .iter()
        .find(|entry| crate::streams::is_lazy(entry.0))
        .context("a structural stream's model")?;
    let numeric_at = entries
        .iter()
        .position(|entry| crate::streams::is_numeric(entry.0))
        .context("a numeric stream's model")?;
    let mut edited = entries.clone();
    edited.push((crate::streams::TEXT, lazy_entry.1.clone()));
    add(
        "tables-blob-id",
        with_entries(&edited),
        "a model for a blob stream (text)",
    );
    let mut edited = entries.clone();
    let at = edited
        .iter()
        .position(|entry| entry.0 > crate::streams::PI_NUM)
        .context("a model after pi.num")?;
    edited.insert(at, (crate::streams::PI_NUM, lazy_entry.1.clone()));
    add(
        "tables-plain-id",
        with_entries(&edited),
        "a model for a stream that has none (pi.num)",
    );
    let mut edited = entries.clone();
    edited.insert(numeric_at, (40, lazy_entry.1.clone()));
    add(
        "tables-unassigned-id",
        with_entries(&edited),
        "a model for stream id 40 (unassigned)",
    );
    add(
        "tables-empty",
        with_entries(&[]),
        "a models block with n = 0",
    );
    let mut edited = file_parts.clone();
    edited.models.push(0);
    add(
        "tables-trailing",
        edited.build(),
        "a models block with one byte after its last entry",
    );
    let mut edited = entries.clone();
    edited[0].1.push(0);
    add(
        "tables-padding",
        with_entries(&edited),
        "a model with one byte of padding (not consumed exactly)",
    );
    let mut edited = entries.clone();
    edited[numeric_at].1.clone_from(&lazy_entry.1);
    add(
        "tables-alphabet",
        with_entries(&edited),
        "a numeric stream's entry holding a structural model (alphabet 256)",
    );
    let mut edited = entries.clone();
    edited[0].1 = {
        use crate::coder::rc::{IntModel, RangeEncoder};
        const NQC: usize = 306;
        let mut encoder = RangeEncoder::new();
        let mut int_model = IntModel::new(NQC + 8);
        for (context, value) in [(NQC, 256), (NQC + 1, 65536)] {
            int_model.put(&mut encoder, context, value);
        }
        encoder.finish()
    };
    add(
        "tables-m1",
        with_entries(&edited),
        "a model declaring 65536 own tables (rule M1)",
    );
    let amplifying = amplifying_model(2000);
    let mut edited = entries.clone();
    edited[0].1 = crate::fields::read_length_prefixed(&mut amplifying.as_slice())
        .context("the model's header")?
        .to_vec();
    add(
        "tables-decisions",
        with_entries(&edited),
        "a model breaking the decision limit (rule M2)",
    );
    Ok(())
}

/// Runs without the demo cache: every hostile vector is built from synthetic demos.
#[test]
fn hostile_vectors_are_refused() -> anyhow::Result<()> {
    let mut vectors = Vec::new();
    hostile(&mut vectors)?;
    assert!(vectors.len() > 30);
    for (name, file, _, _) in &vectors {
        assert!(readers_agree(file, name).is_none(), "{name}: accepted");
    }
    Ok(())
}

/// No real demo among set8, sample40 and stratified uses MVD1 float coordinates.
fn mvd1_float_demo() -> Vec<u8> {
    const MVD_PEXT_1_FLOAT_COORDS: u32 = 1 << 0;
    const ORIGIN_FLAGS: u16 = 0b111;
    let coords = |x: f32, y: f32, z: f32| [x, y, z].map(f32::to_le_bytes).concat();
    let mut demo = Demo(Vec::new());
    let server_data = message(
        11,
        &[
            b"MVD1",
            &MVD_PEXT_1_FLOAT_COORDS.to_le_bytes(),
            &28u32.to_le_bytes(),
            &1u32.to_le_bytes(),
            &c_string("qw"),
            &0u32.to_le_bytes(),
            &c_string("float test"),
            &[0; 40],
        ],
    );
    let model_list = message(
        45,
        &[
            &[0],
            &c_string("maps/dm4.bsp"),
            &c_string("progs/player.mdl"),
            &[0],
            &[0],
        ],
    );
    demo.frame(0, 6, &[server_data, model_list].concat());
    for frame in 0..200u16 {
        let step = f32::from(frame);
        let mut payload = Vec::new();
        for player in 0..2u8 {
            let lane = f32::from(player) * 256.0;
            payload.extend(message(
                42,
                &[
                    &[player],
                    &ORIGIN_FLAGS.to_le_bytes(),
                    &[0],
                    &coords(
                        1.5f32.mul_add(step, lane),
                        0.75f32.mul_add(step, -64.25),
                        24.125,
                    ),
                ],
            ));
        }
        if frame % 10 == 0 {
            let channel: u16 = 1 << 3 | 1;
            payload.extend(message(
                6,
                &[
                    &channel.to_le_bytes(),
                    &[1],
                    &coords(1.5 * step, -64.25, 24.125),
                ],
            ));
        }
        demo.frame(13, 6, &payload);
    }
    demo.0
}

#[test]
fn mvd1_float_demo_is_walked() -> anyhow::Result<()> {
    let demo = mvd1_float_demo();
    let (file, stats) = compress_unchecked(&demo)?;
    assert_eq!(stats.unstructured_len, 0);
    assert_eq!(decompress(&file, 1)?, demo);
    Ok(())
}

/// None without the sample demos.
fn all_vectors() -> anyhow::Result<Option<Vec<Vector>>> {
    let sources = || {
        let duel = duel()?;
        let fte = sample("03fc8ec2603c3862cbc64fd746adf4a662921951fbb189c666f6cc84d85c1673")?;
        Some((duel, fte))
    };
    let Some((duel, fte)) = sources() else {
        return Ok(None);
    };
    let mvd1 = mvd1_float_demo();
    let fte = fte[..300_000].to_vec();
    let mut vectors: Vec<Vector> = Vec::new();
    let mut add = |name: &str, file: Vec<u8>, demo: &[u8], how: &str| {
        vectors.push((name.to_string(), file, Some(demo.to_vec()), how.to_string()));
    };
    // without the input check: some vectors hold non-demo input
    let encode = |demo: &[u8]| anyhow::Ok(compress_unchecked(demo)?.0);
    let duel_source = "duel = set8 demo 3fe477dc...1416";
    add(
        "duel.normal.mvdz",
        encode(&duel)?,
        &duel,
        &format!("{duel_source}; compress"),
    );
    add(
        "duel-truncated.normal.mvdz",
        encode(&duel[..150_000])?,
        &duel[..150_000],
        "the first 150000 bytes of duel (ends inside a frame)",
    );
    let mut rng = Rng(0x1234);
    let random: Vec<u8> = Vec::from_fn(3000, |_| rng.next() as u8);
    add(
        "float-mvd1.normal.mvdz",
        encode(&mvd1)?,
        &mvd1,
        "synthetic demo with MVD1 float coordinates (two players, sounds)",
    );
    add(
        "float-fte.normal.mvdz",
        encode(&fte)?,
        &fte,
        "the first 300000 bytes of stratified demo 03fc8ec2 (FTE float coordinates)",
    );
    let escapes = escapes();
    add(
        "escapes.normal.mvdz",
        encode(&escapes)?,
        &escapes,
        "synthetic demo with unknown commands and lying hidden blocks",
    );
    let multi_demoinfo = several_segments(&multi_demoinfo());
    add(
        "demoinfo-multi.normal.mvdz",
        encode(&multi_demoinfo)?,
        &multi_demoinfo,
        "synthetic demo carrying a 3-block KTX demoinfo twice, repeated to several segments",
    );
    add(
        "random.normal.mvdz",
        encode(&random)?,
        &random,
        "3000 random bytes (no frame: all tail)",
    );
    let framed = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    add(
        "framed-as-tail.mvdz",
        all_tail(&framed)?.0,
        &framed,
        "synthetic demo with demoinfo, written as all tail (the encoder's fallback layout)",
    );
    add("empty.normal.mvdz", encode(b"")?, b"", "an empty demo");
    let usercmd = usercmd_demo();
    add(
        "usercmd.normal.mvdz",
        encode(&usercmd)?,
        &usercmd,
        "synthetic demo with usercmd hidden blocks (4 players, angles on and off the grid, other type-1 lengths)",
    );
    let usercmd_segments = several_segments(&usercmd);
    add(
        "usercmd.normal.segments.mvdz",
        encode(&usercmd_segments)?,
        &usercmd_segments,
        "the same, repeated to several segments",
    );
    hostile(&mut vectors)?;
    Ok(Some(vectors))
}

#[test]
fn vectors_reproduce() -> anyhow::Result<()> {
    let Some(vectors) = all_vectors()? else {
        eprintln!("vector sources not cached, skipping");
        return Ok(());
    };
    if std::env::var_os("MVDZ_WRITE_VECTORS").is_some() {
        let _ = std::fs::create_dir_all(dir());
        for entry in std::fs::read_dir(dir())? {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "mvdz")
            {
                std::fs::remove_file(path)?;
            }
        }
        let mut manifest_text = String::from(
            "# file\tdecoded_sha256 or \"error: <message>\"\tdecoded_size\thow it was made\n# Valid files must decode to the SHA-256; hostile files must be refused. The message is the reference's: another decoder should report the same category (the Error variants; FORMAT.md section 7), in its own words.\n",
        );
        for (name, file, demo, how) in &vectors {
            std::fs::write(dir().join(name), file)?;
            if let Some(demo) = demo {
                writeln!(
                    manifest_text,
                    "{name}\t{}\t{}\t{how}",
                    hex(&Sha256::digest(demo)),
                    demo.len()
                )?;
            } else {
                let refusal = decompress(file, 1)
                    .err()
                    .context("a hostile vector decodes")?;
                writeln!(manifest_text, "{name}\terror: {refusal}\t-\t{how}")?;
            }
        }
        std::fs::write(dir().join("MANIFEST.tsv"), manifest_text)?;
        return Ok(());
    }
    let rows = manifest()?;
    assert_eq!(rows.len(), vectors.len(), "the manifest lists every vector");
    for (name, file, _, _) in &vectors {
        let committed = std::fs::read(dir().join(name)).with_context(|| name.clone())?;
        assert!(
            committed == *file,
            "{name}: the encoder no longer reproduces the committed vector"
        );
    }
    Ok(())
}
