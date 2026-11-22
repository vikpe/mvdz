//! Cross-implementation golden vectors (`vectors/` at the repo root, `MANIFEST.tsv`).
//! `MVDZ_WRITE_VECTORS=1` rewrites them instead of checking them.

use super::common::*;
use crate::fields::put_usize;
use crate::format::*;
use anyhow::Context as _;
use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vectors")
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

/// Returns (models block body, transform body). The structural streams are coded under
/// context 0; the blob bytes go through the tokens as the encoder's do.
fn transform_body(
    lazy: &[(usize, &[u8])],
    blobs: &[(usize, &[u8])],
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let blob_bytes: Vec<u8> = (blobs.iter())
        .flat_map(|(_, blob)| blob.iter().copied())
        .collect();
    let blob_counts: Vec<(usize, usize)> =
        (blobs.iter()).map(|&(id, blob)| (id, blob.len())).collect();
    let tokens = crate::lz::tokens(&blob_bytes);
    token_body(lazy, &blob_counts, &tokens, 0)
}

/// As [`transform_body`], with the blob streams' counts and their tokens given.
fn token_body(
    lazy: &[(usize, &[u8])],
    blob_counts: &[(usize, usize)],
    tokens: &crate::lz::Tokens,
    tail: usize,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    use crate::rans;
    use crate::streams::{LZ_DIST, LZ_LEN, LZ_LIT};
    fn file_model(alphabet: usize, symbols: &[u8], contexts: &[u16]) -> rans::FileModel {
        let mut tally = rans::Tally::new(alphabet);
        tally.add(symbols, contexts);
        tally.file_model()
    }
    const UNCODABLE: &str = "every symbol has a code";
    // (id, count, model, body)
    let mut coded: Vec<(usize, usize, rans::FileModel, Vec<u8>)> = Vec::new();
    let structural = (lazy.iter())
        .map(|&(id, symbols)| (id, symbols, vec![0u16; symbols.len()]))
        .chain([(
            LZ_LIT,
            &tokens.literals[..],
            tokens.literal_contexts.clone(),
        )]);
    for (id, symbols, contexts) in structural {
        if !symbols.is_empty() {
            let model = file_model(256, symbols, &contexts);
            let body = rans::encode_bytes(symbols, &contexts, &model).context(UNCODABLE)?;
            coded.push((id, symbols.len(), model, body));
        }
    }
    for (id, numeric) in [(LZ_LEN, &tokens.lens), (LZ_DIST, &tokens.dists)] {
        if !numeric.symbols.is_empty() {
            let model = file_model(rans::NUMERIC_ALPHABET, &numeric.symbols, &numeric.contexts);
            let body = rans::encode_num(numeric, &model).context(UNCODABLE)?;
            coded.push((id, numeric.symbols.len(), model, body));
        }
    }
    coded.sort_by_key(|&(id, ..)| id);
    let mut body = Vec::new();
    // fte, fte2, mvd1, tail; the number of streams
    for field in [0, 0, 0, tail, coded.len() + blob_counts.len()] {
        put_usize(&mut body, field);
    }
    for (id, count, _, stream_body) in &coded {
        for field in [*id, *count, stream_body.len()] {
            put_usize(&mut body, field);
        }
    }
    for &(id, count) in blob_counts {
        put_usize(&mut body, id);
        put_usize(&mut body, count);
    }
    let mut ids = Vec::new();
    let mut models: Vec<Option<rans::FileModel>> = Vec::new();
    for (id, _, model, stream_body) in coded {
        body.extend_from_slice(&stream_body);
        models.resize_with(models.len().max(id + 1), || None);
        models[id] = Some(model);
        ids.push(id);
    }
    Ok((crate::segment::models_body(&models, &ids), body))
}

fn one_segment(demo: &[u8], (models, body): &(Vec<u8>, Vec<u8>)) -> anyhow::Result<Vec<u8>> {
    let crc = crc32fast::hash(demo);
    let duration_ms = crate::segment::duration_ms(demo);
    let mut segment = segment_header_bytes(u32::try_from(demo.len())?, duration_ms, crc);
    segment.extend_from_slice(body);
    Ok(Parts {
        header: Header {
            size_bytes: demo.len() as u64,
            duration_ms: u64::from(duration_ms),
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
    edited.header.crc32 ^= 1;
    add(
        "header-crc",
        edited.build(),
        "one bit of the header's CRC-32 flipped",
    );
    let mut edited_file = base.clone();
    edited_file[4] = 4;
    add("version", edited_file, "version 4");
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
    blob_tokens(&mut add)?;
    models_block(&mut add)
}

/// Four bytes of tail, with tokens no encoder writes.
fn blob_tokens(add: &mut impl FnMut(&str, Vec<u8>, &str)) -> anyhow::Result<()> {
    use crate::lz::Token;
    use crate::streams::TAIL;
    let demo = b"aaaa";
    let literal = Token::Literal(b'a');
    for (name, tokens, how) in [
        (
            "lz-distance",
            vec![literal, Token::Match { len: 3, dist: 2 }],
            "a match reaching 2 bytes back after 1 byte",
        ),
        (
            "lz-overrun",
            vec![literal, Token::Match { len: 9, dist: 1 }],
            "a match of 9 bytes where 3 are left",
        ),
        (
            "lz-short",
            vec![literal, literal],
            "tokens for 2 of the 4 blob bytes",
        ),
    ] {
        let tokens = crate::lz::code(demo, &tokens);
        let body = token_body(&[], &[(TAIL, demo.len())], &tokens, demo.len())?;
        add(name, one_segment(demo, &body)?, how);
    }
    let stored = |raw: &[u8]| {
        let mut body = Vec::new();
        // fte, fte2, mvd1, tail; no stream
        for field in [0, 0, 0, demo.len(), 0] {
            put_usize(&mut body, field);
        }
        body.extend_from_slice(raw);
        one_segment(demo, &(Vec::new(), body))
    };
    add(
        "stored-len",
        stored(&demo[..3])?,
        "a stored segment of 4 bytes holding 3",
    );
    Ok(())
}

fn models_block(add: &mut impl FnMut(&str, Vec<u8>, &str)) -> anyhow::Result<()> {
    const OVER_ONE_PER_CONTEXT: u32 = (1 << 16) + 1;
    let usercmd = usercmd_demo();
    let file = compress(&several_segments(&usercmd))?;
    let file_parts = parts(&file)?;
    assert!(
        !file_parts.models.is_empty(),
        "a segmented normal file has models"
    );
    let models = models_parts(&file_parts.models)?;
    let entries = models.entries.clone();
    let with_models = |models_body: Vec<u8>| {
        let mut edited = file_parts.clone();
        edited.models = models_body;
        edited.build()
    };
    let with_entries = |entries: &[ModelEntry]| {
        let edited = ModelsParts {
            meta: models.meta.clone(),
            entries: entries.to_vec(),
        };
        with_models(edited.build())
    };
    let mut edited = entries.clone();
    edited.remove(0);
    add(
        "tables-no-entry",
        with_entries(&edited),
        "the models block without the entry of a stream the segments code with rANS",
    );
    add(
        "tables-missing",
        with_models(Vec::new()),
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
        .find(|entry| crate::streams::is_lazy(entry.stream))
        .context("a structural stream's model")?;
    let numeric_at = entries
        .iter()
        .position(|entry| crate::streams::is_numeric(entry.stream))
        .context("a numeric stream's model")?;
    let as_stream = |stream: usize| ModelEntry {
        stream,
        ..lazy_entry.clone()
    };
    let mut edited = entries.clone();
    edited.push(as_stream(crate::streams::TEXT));
    add(
        "tables-blob-id",
        with_entries(&edited),
        "a model for a blob stream (text)",
    );
    let mut edited = entries.clone();
    edited.push(as_stream(crate::streams::STREAM_COUNT));
    add(
        "tables-unassigned-id",
        with_entries(&edited),
        "a model for a stream id past the last",
    );
    add(
        "tables-empty",
        with_entries(&[]),
        "a models block with n = 0",
    );
    let mut edited = file_parts.models.clone();
    edited.push(0);
    add(
        "tables-trailing",
        with_models(edited),
        "a models block with one byte after its last entry",
    );
    let mut edited = entries.clone();
    edited[0].coded.push(0);
    add(
        "tables-padding",
        with_entries(&edited),
        "a model whose coded stream has one byte of padding (not consumed exactly)",
    );
    let mut edited = entries.clone();
    edited[numeric_at] = as_stream(entries[numeric_at].stream);
    add(
        "tables-alphabet",
        with_entries(&edited),
        "a numeric stream's entry holding a structural model (the raw length of alphabet 256)",
    );
    let mut edited = entries.clone();
    edited[0].raw_len = 1 << 24;
    add(
        "tables-raw-size",
        with_entries(&edited),
        "a model declaring 16 MiB of raw bytes: the models together over the limit",
    );
    let own_count_only = OVER_ONE_PER_CONTEXT.to_le_bytes();
    let block = crate::rans::write_models_block(&[(entries[0].stream, &own_count_only)]);
    add(
        "tables-own-count",
        with_models(block.body),
        "a model declaring 65537 own tables: more than there are contexts",
    );
    Ok(())
}

/// Runs without the demo cache: every hostile vector is built from synthetic demos.
#[test]
fn hostile_vectors_are_refused() -> anyhow::Result<()> {
    let mut vectors = Vec::new();
    hostile(&mut vectors)?;
    assert!(vectors.len() > 20);
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
    let file = compress(&demo)?;
    assert_eq!(unstructured_len(&file)?, 0);
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
    let encode = |demo: &[u8]| anyhow::Ok(compress_unchecked(demo)?);
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
        all_tail(&framed)?,
        &framed,
        "synthetic demo with demoinfo, written as all tail (the encoder's fallback layout)",
    );
    add(
        "framed-as-stored.mvdz",
        all_stored(&framed),
        &framed,
        "the same demo, written as a stored segment (the encoder's last fallback)",
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

/// The canonical parse (FORMAT.md section 8) on fixed inputs, for an encoder in another
/// language to check its parser before it compares whole files.
fn lz_tokens_text() -> String {
    use crate::lz::Token;
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut noise = |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % bound) as u8
    };
    // the fifth byte differs each time: only a chain longer than the limit finds the first
    let mut over_the_chain_limit = Vec::new();
    for occurrence in 0..40u8 {
        over_the_chain_limit.extend_from_slice(b"wxyz");
        over_the_chain_limit.push(b'0' + occurrence);
    }
    over_the_chain_limit.extend_from_slice(b"wxyz0");
    let few_symbols: Vec<u8> = (0..400).map(|_| b'a' + noise(3)).collect();
    let mut inputs: Vec<Vec<u8>> = [
        &b"abc"[..],
        b"abcXabc",
        b"abcdXabcdYabcd",
        b"aaaaaaaaaa",
        b"\\name\\alice\\team\\red\\name\\bob\\team\\red\\name\\alice\\team\\blue",
    ]
    .map(<[u8]>::to_vec)
    .to_vec();
    inputs.push(over_the_chain_limit);
    inputs.push(few_symbols);
    let mut text = String::from(
        "# input as hex\ttokens: L<byte as hex> for a literal, M<length>,<distance> for a match\n",
    );
    for input in inputs {
        let tokens: Vec<String> = crate::lz::parse(&input)
            .into_iter()
            .map(|token| match token {
                Token::Literal(byte) => format!("L{byte:02x}"),
                Token::Match { len, dist } => format!("M{len},{dist}"),
            })
            .collect();
        let _ = writeln!(text, "{}\t{}", hex(&input), tokens.join(" "));
    }
    text
}

#[test]
fn lz_tokens_reproduce() -> anyhow::Result<()> {
    let path = dir().join("lz-tokens.tsv");
    if std::env::var_os("MVDZ_WRITE_VECTORS").is_some() {
        std::fs::write(&path, lz_tokens_text())?;
        return Ok(());
    }
    assert_eq!(std::fs::read_to_string(&path)?, lz_tokens_text());
    Ok(())
}
