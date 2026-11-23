//! Cross-implementation golden vectors (`vectors/` at the repo root, `MANIFEST.tsv`).
//! `MVDZ_WRITE_VECTORS=1` rewrites them instead of checking them.

use super::common::*;
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
            let refusal = crate::decode(&bytes, 1)
                .err()
                .map(|error| error.to_string());
            assert_eq!(refusal.as_deref(), Some(expected_error), "{file}: error");
        } else {
            let decoded = got.with_context(|| {
                format!("{file}: refused: {:?}", crate::decode(&bytes, 1).err())
            })?;
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
    structural: &[(usize, &[u8])],
    blobs: &[(usize, &[u8])],
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let blob_bytes: Vec<u8> = (blobs.iter())
        .flat_map(|(_, blob)| blob.iter().copied())
        .collect();
    let blob_counts: Vec<(usize, usize)> =
        (blobs.iter()).map(|&(id, blob)| (id, blob.len())).collect();
    let tokens = crate::lz::tokens(&blob_bytes);
    token_body(structural, &blob_counts, &tokens)
}

/// As [`transform_body`], with the blob streams' counts and their tokens given.
fn token_body(
    structural: &[(usize, &[u8])],
    blob_counts: &[(usize, usize)],
    tokens: &crate::lz::Tokens,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let (models, body) = token_models(structural, blob_counts, tokens)?;
    Ok((crate::segment::models_body(&models)?, body))
}

/// The models by stream id before they are coded, and the transform body.
type TokenModels = (Vec<Option<crate::rans::FileModel>>, Vec<u8>);

/// As [`token_body`], with the models not yet coded.
fn token_models(
    structural: &[(usize, &[u8])],
    blob_counts: &[(usize, usize)],
    tokens: &crate::lz::Tokens,
) -> anyhow::Result<TokenModels> {
    use crate::rans;
    use crate::streams::{LZ_DISTANCE, LZ_LENGTH, LZ_LITERAL, is_structural};
    fn file_model(alphabet_size: usize, symbols: &[u8], contexts: &[u16]) -> rans::FileModel {
        let mut tally = rans::Tally::new(alphabet_size);
        tally.add(symbols, contexts);
        tally.file_model()
    }
    const UNCODABLE: &str = "every symbol has a code";
    let structural_streams: Vec<(usize, rans::StreamSymbols)> = (structural.iter())
        .map(|&(id, symbols)| {
            let stream = rans::StreamSymbols {
                symbols: symbols.to_vec(),
                contexts: vec![0; symbols.len()],
                mantissas: Vec::new(),
            };
            (id, stream)
        })
        .collect();
    let token_streams = [
        (LZ_LITERAL, &tokens.literals),
        (LZ_LENGTH, &tokens.lengths),
        (LZ_DISTANCE, &tokens.distances),
    ];
    let streams = (structural_streams.iter())
        .map(|(id, stream)| (*id, stream))
        .chain(token_streams);
    // (id, model, body)
    let mut coded: Vec<(usize, rans::FileModel, Vec<u8>)> = Vec::new();
    for (id, stream) in streams {
        if !stream.symbols.is_empty() {
            let alphabet_size = if is_structural(id) {
                rans::STRUCTURAL_ALPHABET_SIZE
            } else {
                rans::NUMERIC_ALPHABET_SIZE
            };
            let model = file_model(alphabet_size, &stream.symbols, &stream.contexts);
            let body = rans::encode_stream(stream, &model).context(UNCODABLE)?;
            coded.push((id, model, body));
        }
    }
    coded.sort_by_key(|&(id, ..)| id);
    let coded_entries = (coded.iter()).map(|(id, _, stream_body)| (*id, stream_body.len()));
    let entries: Vec<(usize, usize)> = coded_entries.chain(blob_counts.iter().copied()).collect();
    let mut bodies = Vec::new();
    let mut models: Vec<Option<rans::FileModel>> = Vec::new();
    for (id, model, stream_body) in coded {
        bodies.extend_from_slice(&stream_body);
        models.resize_with(models.len().max(id + 1), || None);
        models[id] = Some(model);
    }
    let table = StreamTable {
        fields: [0; 3],
        stream_count: entries.len(),
        entries,
        bodies,
    };
    Ok((models, table.build()))
}

/// A file whose only command loop escapes one command and walks the next, and its demo.
fn escape_walks_on() -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    use crate::streams::{COMMAND, ESCAPE_LENGTH, RAW, TIME, TYPE};
    const DEM_ALL: u8 = 6;
    const SVC_NOP: u8 = 1;
    const ESCAPE: u8 = 0;
    const END: u8 = 255;
    let payload = [SVC_NOP, SVC_NOP];
    let demo = [&[0, DEM_ALL][..], &2u32.to_le_bytes(), &payload].concat();
    let body = transform_body(
        &[
            (TIME, &[0]),
            (TYPE, &[DEM_ALL]),
            (COMMAND, &[ESCAPE, SVC_NOP, 0, 0, 0, END]),
        ],
        &[(ESCAPE_LENGTH, &1u32.to_le_bytes()), (RAW, &[SVC_NOP])],
    )?;
    Ok((one_segment(&demo, &body)?, demo))
}

fn one_segment(demo: &[u8], (models, body): &(Vec<u8>, Vec<u8>)) -> anyhow::Result<Vec<u8>> {
    let crc = crc32fast::hash(demo);
    let duration_ms = crate::segment::duration_ms(demo);
    let mut segment = segment_header_bytes(u32::try_from(demo.len())?, duration_ms, crc);
    segment.extend_from_slice(body);
    Ok(Parts {
        header: Header::new(
            demo.len() as u64,
            u64::from(duration_ms),
            crc,
            Sha256::digest(demo).into(),
        ),
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
    let base = crate::encode(&several_segments(&demo), 1)?;
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
    edited.header = with_size(&edited.header, edited.header.size() + 1);
    add("header-size", edited.build(), "the header's size + 1");
    let mut edited = file_parts.clone();
    edited.header = with_crc32(&edited.header, edited.header.crc32() ^ 1);
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
    edited.segments[1][SEGMENT_FIELDS_SIZE - 2] ^= 0x10;
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
    edited.segments[0].resize(SEGMENT_FIELDS_SIZE + usize::try_from(MAX_CODED_LEN)? + 1, 0);
    add(
        "segment-length",
        edited.build(),
        "the first segment padded with zeros to one byte over the largest coded length (2 MiB)",
    );
    let mut edited_file = base[..HEADER_SIZE].to_vec();
    edited_file.extend_from_slice(&(256u32 << 20).to_le_bytes());
    edited_file.extend_from_slice(&[0; 100]);
    add(
        "models-size",
        edited_file,
        "a models block of 256 MiB declared in a few bytes",
    );
    let mut edited_file = base[..HEADER_SIZE].to_vec();
    for segment in &file_parts.segments {
        write_segment(&mut edited_file, segment);
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
        let mut structural: Vec<(usize, &[u8])> = vec![(TIME, &[0]), (TYPE, &[3])];
        if let Some(usercmd) = usercmd {
            structural.push((USERCMD, usercmd));
        }
        let mut blobs: Vec<(usize, &[u8])> = vec![
            (MASK, &[0; 4]),
            (HIDDEN_TAG, &[1, 2]),
            (HIDDEN_LENGTH, &[23, 0, 0, 0]),
            (HIDDEN_TYPE, &[1, 0]),
        ];
        if let Some(misc) = misc {
            blobs.push((HIDDEN_MISC, misc));
        }
        one_segment(&one, &transform_body(&structural, &blobs)?)
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
        "a usercmd block (type 1, 23 bytes) stored in HIDDEN_MISC, no USERCMD stream",
    );
    let mut edited = file_parts.clone();
    let mut segment = segment_header_bytes(u32::try_from(MAX_RAW_LEN)?, 0, 0);
    segment.extend_from_slice(&[0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0, 0, 0]);
    edited.header = with_size(&edited.header, MAX_RAW_LEN);
    edited.segments = vec![segment];
    add(
        "segment-declared",
        edited.build(),
        "a segment declaring 1 MiB of demo in 9 bytes",
    );
    add(
        "header-short",
        base[..30].to_vec(),
        "the first 30 bytes of the header",
    );
    add("header-tiny", b"MVDZ\x03".to_vec(), "magic and one byte");
    let mut edited_file = base;
    edited_file[3] = b'X';
    add("magic", edited_file, "the magic `MVDX`");
    blob_tokens(&mut add)?;
    stream_table(&mut add)?;
    models_block(&mut add)
}

/// One-segment files whose stream table breaks a rule of FORMAT.md 3.1.
fn stream_table(add: &mut impl FnMut(&str, Vec<u8>, &str)) -> anyhow::Result<()> {
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let file_parts = parts(&crate::encode(&demo, 1)?)?;
    anyhow::ensure!(file_parts.segments.len() == 1, "one segment");
    let segment = &file_parts.segments[0];
    let table = StreamTable::parse(&segment[SEGMENT_FIELDS_SIZE..])?;
    let raw_len = demo.len();
    let with_table = |table: &StreamTable| {
        let mut edited = file_parts.clone();
        edited.segments[0] = [&segment[..SEGMENT_FIELDS_SIZE], &table.build()].concat();
        edited.build()
    };
    let blob_at: Vec<usize> = (0..table.entries.len())
        .filter(|&index| crate::streams::is_blob(table.entries[index].0))
        .collect();
    let last_blob = *blob_at.last().context("a blob stream")?;
    let mut edited = table.clone();
    edited.stream_count = 323;
    add(
        "table-count",
        with_table(&edited),
        "a stream table declaring 323 streams",
    );
    let mut edited = table.clone();
    edited.entries[last_blob].0 = crate::streams::STREAM_COUNT;
    add(
        "table-id",
        with_table(&edited),
        "a blob stream with id 315, past the last",
    );
    let mut edited = table.clone();
    edited.entries[last_blob].0 = table.entries[blob_at[blob_at.len() - 2]].0;
    add(
        "table-dup-id",
        with_table(&edited),
        "the last blob stream's id set to the one before it (the same id twice)",
    );
    let mut edited = table.clone();
    let (first_len, second_len) = (table.entries[0].1, table.entries[1].1);
    anyhow::ensure!(blob_at[0] >= 2, "two coded streams");
    edited.entries.swap(0, 1);
    edited.bodies[..first_len + second_len].rotate_left(first_len);
    add(
        "table-order",
        with_table(&edited),
        "the first two streams swapped, entries and bodies (stream ids not increasing)",
    );
    let blob_total: usize = blob_at.iter().map(|&index| table.entries[index].1).sum();
    let mut edited = table.clone();
    edited.entries[last_blob].1 += 2 * raw_len + 1 - blob_total;
    add(
        "table-blob-total",
        with_table(&edited),
        "the last blob stream's size raised so the blob sizes sum to 2 * raw_len + 1",
    );
    let mut edited = table.clone();
    edited.bodies.push(0);
    add(
        "table-trailing",
        with_table(&edited),
        "one byte after the last stream body",
    );
    let mut edited = table.clone();
    let (first, second) = (blob_at[0], blob_at[1]);
    edited.entries[first].1 -= 1;
    edited.entries[second].1 += 1;
    add(
        "blob-split",
        with_table(&edited),
        "the first two blob streams' sizes moved by one byte: the same blob bytes, split differently",
    );
    let rans_len = u32::from_le_bytes(table.bodies[..4].try_into()?);
    let mut edited = table.clone();
    let rans_end = 4 + rans_len as usize;
    edited.bodies.drain(rans_end - 2..rans_end);
    edited.bodies[..4].copy_from_slice(&(rans_len - 2).to_le_bytes());
    edited.entries[0].1 -= 2;
    add(
        "rans-short",
        with_table(&edited),
        "the first stream body without its last rANS word",
    );
    let mut edited = table.clone();
    edited.bodies[4..6].fill(0);
    add(
        "rans-low-state",
        with_table(&edited),
        "the first stream body's first rANS word zeroed: an initial state below 2^16",
    );
    let mut edited = table;
    edited.bodies[..4].copy_from_slice(&(rans_len + 1).to_le_bytes());
    add(
        "rans-odd",
        with_table(&edited),
        "the first stream body's rans_len raised by one, to an odd length",
    );
    Ok(())
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
            vec![
                literal,
                Token::Match {
                    len: 3,
                    distance: 2,
                },
            ],
            "a match reaching 2 bytes back after 1 byte",
        ),
        (
            "lz-overrun",
            vec![
                literal,
                Token::Match {
                    len: 9,
                    distance: 1,
                },
            ],
            "a match of 9 bytes where 3 are left",
        ),
    ] {
        let tokens = crate::lz::code(demo, &tokens);
        let body = token_body(&[], &[(TAIL, demo.len())], &tokens)?;
        add(name, one_segment(demo, &body)?, how);
    }
    let longer = b"aaaaa";
    let body = token_body(&[], &[(TAIL, longer.len())], &crate::lz::tokens(longer))?;
    add(
        "tail-long",
        one_segment(demo, &body)?,
        "a `TAIL` stream of raw_len + 1 bytes",
    );
    let stored = StreamTable {
        fields: [0; 3],
        stream_count: 0,
        entries: Vec::new(),
        bodies: demo[..3].to_vec(),
    };
    add(
        "stored-len",
        one_segment(demo, &(Vec::new(), stored.build()))?,
        "a stored segment of 4 bytes holding 3",
    );
    Ok(())
}

fn models_block(add: &mut impl FnMut(&str, Vec<u8>, &str)) -> anyhow::Result<()> {
    use crate::streams::{LZ_LENGTH, TAIL};
    const OVER_ONE_PER_CONTEXT: u32 = (1 << 16) + 1;
    let usercmd = usercmd_demo();
    let file = crate::encode(&several_segments(&usercmd), 1)?;
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
    let structural_entry = entries
        .iter()
        .find(|entry| crate::streams::is_structural(entry.stream))
        .context("a structural stream's model")?;
    let numeric_at = entries
        .iter()
        .position(|entry| crate::streams::is_numeric(entry.stream))
        .context("a numeric stream's model")?;
    let as_stream = |stream: usize| ModelEntry {
        stream,
        ..structural_entry.clone()
    };
    let mut edited = entries.clone();
    edited.push(as_stream(crate::streams::TAIL));
    add(
        "tables-blob-id",
        with_entries(&edited),
        "a model for a blob stream (the tail)",
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
    edited[0].raw_len = 1 << 22;
    add(
        "tables-raw-size",
        with_entries(&edited),
        "a model declaring 4 MiB of raw bytes: the models together over the limit",
    );
    // three literals: the lengths' model has one table with one symbol, which has every
    // slot whatever its count, so only the limit refuses the file
    let literals = b"abc";
    let tokens = crate::lz::tokens(literals);
    let (literal_models, body) = token_models(&[], &[(TAIL, literals.len())], &tokens)?;
    let mut raw_models: Vec<(usize, Vec<u8>)> = (literal_models.iter().enumerate())
        .filter_map(|(stream, model)| Some((stream, model.as_ref()?.raw().to_vec())))
        .collect();
    let (_, lengths) = (raw_models.iter_mut())
        .find(|(stream, _)| *stream == LZ_LENGTH)
        .context("a model of the lengths")?;
    let no_own_contexts = 0u32.to_le_bytes();
    assert!(lengths.starts_with(&no_own_contexts) && lengths[4] > 0);
    lengths[4] = crate::math::MAX_QUANTIZED as u8 + 1;
    let listed: Vec<(usize, &[u8])> = (raw_models.iter())
        .map(|(stream, raw)| (*stream, &raw[..]))
        .collect();
    let models_body = crate::rans::write_models_block(&listed)?.body;
    add(
        "tables-quantized",
        one_segment(literals, &(models_body, body))?,
        "a quantized count of 66, over the largest, for the only symbol of a table",
    );
    let own_count_only = OVER_ONE_PER_CONTEXT.to_le_bytes();
    let block = crate::rans::write_models_block(&[(entries[0].stream, &own_count_only)])?;
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

/// No real demo among sample8, sample40 and stratified uses MVD1 float coordinates.
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
    let file = crate::encode(&demo, 1)?;
    assert_eq!(unstructured_len(&file)?, 0);
    assert_eq!(crate::decode(&file, 1)?, demo);
    Ok(())
}

/// Every frame type, every command the sizer knows (FORMAT.md 8
/// rule 12) and the playerinfo, packet-entity, sound and temp-entity forms, first without
/// and then with FTE extensions. The walk escapes nothing.
fn commands_demo() -> Vec<u8> {
    const FTE_EXTENSIONS: u32 = 1 << 3 | 1 << 19; // FTE_PEXT_TRANS, FTE_PEXT_COLOR_MOD
    let coords = |x: i16, y: i16, z: i16| [x, y, z].map(i16::to_le_bytes).concat();
    let entity_word = |bits: u16, number: u16| (bits | number).to_le_bytes();
    let server_data = |extensions: &[&[u8]]| {
        let mut parts: Vec<&[u8]> = extensions.to_vec();
        let tail = [
            28u32.to_le_bytes().to_vec(),
            1u32.to_le_bytes().to_vec(),
            c_string("qw"),
            0u32.to_le_bytes().to_vec(),
            c_string("commands"),
            vec![0; 40],
        ];
        parts.extend(tail.iter().map(Vec::as_slice));
        message(11, &parts)
    };
    let list = |command: u8, names: &[&str]| {
        let mut parts = vec![vec![0u8]];
        parts.extend(names.iter().map(|name| c_string(name)));
        parts.push(vec![0]);
        parts.push(vec![0]);
        message(
            command,
            &parts.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        )
    };
    let mut demo = Demo(Vec::new());
    demo.frame(
        0,
        6,
        &[
            server_data(&[]),
            list(
                45,
                &["maps/dm4.bsp", "progs/player.mdl", "progs/rocket.mdl"],
            ),
            list(46, &["weapons/r_exp3.wav"]),
            message(22, &[&[2, 0, 2, 0, 0, 0, 0], &[0; 8]]),
            message(20, &[&[2, 0, 0, 0], &[0; 9]]),
            message(29, &[&[0; 6], &[1, 255, 1]]),
            message(12, &[&[0], &c_string("abcdefg")]),
            message(32, &[&[4]]),
            message(40, &[&[0], &7u32.to_le_bytes(), &c_string("\\name\\alice")]),
            message(51, &[&[0], &c_string("team"), &c_string("red")]),
            message(52, &[&c_string("maxfps"), &c_string("77")]),
            message(49, &[&320f32.to_le_bytes()]),
            message(50, &[&1f32.to_le_bytes()]),
        ]
        .concat(),
    );
    // DEM_SET, DEM_MULTIPLE with a player mask, DEM_SINGLE, DEM_STATS, DEM_READ
    demo.0.extend_from_slice(&[0, 2]);
    demo.0.extend_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
    let multiple = message(8, &[&[2], &c_string("to players 0 and 2\n")]);
    demo.0.extend_from_slice(&[0, 3]);
    demo.0.extend_from_slice(&0b101u32.to_le_bytes());
    demo.0
        .extend_from_slice(&(multiple.len() as u32).to_le_bytes());
    demo.0.extend_from_slice(&multiple);
    demo.frame(0, 1 << 3 | 4, &message(26, &[&c_string("single\n")]));
    demo.frame(
        0,
        2 << 3 | 5,
        &[
            message(3, &[&[6, 100]]),
            message(38, &[&[7], &200u32.to_le_bytes()]),
        ]
        .concat(),
    );
    demo.frame(
        0,
        1,
        &[message(1, &[]), message(10, &[&[0, 1, 2, 3]])].concat(),
    );
    for frame in 0..60u8 {
        let step = i16::from(frame);
        let mut payload = Vec::new();
        for player in 0..3u8 {
            let mut flags: u16 = 0b111 | 0b111 << 3; // DF_ORIGIN, DF_ANGLES
            flags |= [1 << 11, 1 << 7, 1 << 6, 1 << 10][usize::from(frame % 4)];
            if frame % 17 == 3 {
                flags |= 1 << 8 | 1 << 9 | 1 << 12 | 1 << 15; // dead, gib, rare bits
            }
            let mut fields = vec![player];
            fields.extend_from_slice(&flags.to_le_bytes());
            fields.push(frame % 6);
            fields.extend(coords(step * 8 + i16::from(player) * 100, -step * 3, 24));
            for angle in [step * 91, step * 7, 0i16] {
                fields.extend_from_slice(&angle.to_le_bytes());
            }
            fields.push(frame);
            payload.extend(message(42, &[&fields]));
        }
        let command = if frame % 2 == 0 { 47 } else { 48 };
        let mut entities = if command == 48 {
            vec![frame]
        } else {
            Vec::new()
        };
        // entity 1: every field; 2: frame only; 300 (a gap over 255): origins; 511: removed
        let all_bits: u16 = 1 << 15 | 1 << 13 | 1 << 12 | 0b111 << 9;
        entities.extend_from_slice(&entity_word(all_bits, 1));
        entities.push(0b11_1111); // U_ANGLE_1, U_ANGLE_3, U_MODEL, U_COLOR_MAP, U_SKIN, U_EFFECTS
        entities.extend_from_slice(&[2, frame % 8, 1, 2, 3]);
        for axis in 0..3u8 {
            entities.extend_from_slice(&(step * i16::from(axis + 1)).to_le_bytes());
            entities.push(frame.wrapping_mul(axis + 3));
        }
        entities.extend_from_slice(&entity_word(1 << 13, 2));
        entities.push(frame % 3);
        entities.extend_from_slice(&entity_word(0b111 << 9, 300));
        entities.extend(coords(step, step * 2, 16));
        entities.extend_from_slice(&entity_word(1 << 14, 511));
        entities.extend_from_slice(&[0, 0]);
        payload.push(command);
        payload.extend(entities);
        let sound_entity: u16 = [1, 100, 600][usize::from(frame % 3)];
        let channel = sound_entity << 3
            | u16::from(frame % 8)
            | u16::from(frame % 2) << 15
            | u16::from(frame % 3 == 0) << 14;
        let mut sound = channel.to_le_bytes().to_vec();
        if channel & 1 << 15 != 0 {
            sound.push(200);
        }
        if channel & 1 << 14 != 0 {
            sound.push(64);
        }
        sound.push(frame % 4);
        sound.extend(coords(step * 8, 0, 24));
        payload.extend(message(6, &[&sound]));
        let kind = frame % 14;
        let mut temp_entity = vec![kind];
        match kind {
            5 | 6 | 9 => {
                let entity: u16 = if frame % 28 < 14 { 2 } else { 400 };
                temp_entity.extend_from_slice(&entity.to_le_bytes());
                temp_entity.extend(coords(step, 0, 24));
                temp_entity.extend(coords(step + 300, 40, 24));
            }
            2 | 12 => {
                temp_entity.push(frame % 5 + 1);
                temp_entity.extend(coords(step, step, 0));
            }
            _ => temp_entity.extend(coords(step, -step, 8)),
        }
        payload.extend(message(23, &[&temp_entity]));
        payload.extend(message(3, &[&[frame % 4, frame]]));
        demo.frame(13, 6, &payload);
    }
    demo.frame(
        0,
        6,
        &[
            message(14, &[&[0, 5, 0]]),
            message(36, &[&[0, 40, 0]]),
            message(37, &[&[0], &1f32.to_le_bytes()]),
            message(53, &[&[0, 3]]),
            message(39, &[&[1, 0]]),
            message(19, &[&[10, 20], &coords(1, 2, 3)]),
            message(43, &[&[2], &[1; 12]]),
            message(54, &[&[1], &[2; 7]]),
            message(44, &[&[3]]),
            message(41, &[&4i16.to_le_bytes(), &[50], &[9; 4]]),
            message(41, &[&(-1i16).to_le_bytes(), &[0]]),
            message(24, &[&[1]]),
            message(27, &[]),
            message(28, &[]),
            message(34, &[]),
            message(35, &[]),
            message(8, &[&[1], &c_string("print\n")]),
            message(9, &[&c_string("stuff\n")]),
            message(31, &[&c_string("finale")]),
            message(30, &[&coords(1, 2, 3), &[4, 5, 6]]),
        ]
        .concat(),
    );
    let fte = FTE_EXTENSIONS.to_le_bytes();
    demo.frame(0, 6, &server_data(&[b"FTEX", &fte]));
    for frame in 0..20u8 {
        let mut entities = vec![47];
        // U_MORE_BITS, U_ORIGIN_1; more: U_FTE_EVEN_MORE; even more: every FTE field, the
        // entity number + 512 + 1024, and a yet-more byte with U_FTE_COLOR_MOD
        entities.extend_from_slice(&entity_word(1 << 15 | 1 << 9, 7));
        entities.push(1 << 7);
        entities.push(0b1110_1111);
        entities.push(1 << 2);
        entities.extend_from_slice(&[3, 1]);
        entities.extend_from_slice(&i16::from(frame).to_le_bytes());
        entities.extend_from_slice(&[128, 16, 4, 255, 128, 0]);
        entities.extend_from_slice(&[0, 0]);
        let mut fte_static = vec![21];
        fte_static.extend_from_slice(&entity_word(1 << 15 | 1 << 13, 9));
        fte_static.push(1 << 7);
        fte_static.push(0b0000_0110);
        fte_static.extend_from_slice(&[frame, 128, 16]);
        let mut fte_baseline = vec![66];
        fte_baseline.extend_from_slice(&entity_word(1 << 15, 10));
        fte_baseline.push(1 << 7 | 1 << 2);
        fte_baseline.push(1 << 3);
        fte_baseline.push(1);
        demo.frame(13, 6, &[entities, fte_static, fte_baseline].concat());
    }
    demo.frame(5, 6, &message(2, &[&c_string("EndOfDemo")]));
    demo.0
}

fn protocol_change_demo() -> Vec<u8> {
    let small = synthetic(&[(0, "{}")], false);
    let copies = 3 * crate::segment::SEGMENT_TARGET_LEN / (2 * small.len());
    let mut demo = Demo(small.repeat(copies));
    let server_data = message(
        crate::protocol::SVC_SERVER_DATA,
        &[
            b"FTE2",
            &(1u32 << 30).to_le_bytes(),
            &crate::protocol::PROTOCOL_VERSION.to_le_bytes(),
            &1u32.to_le_bytes(),
            &c_string("qw"),
            &0u32.to_le_bytes(),
            &c_string("changed"),
            &[0; 40],
        ],
    );
    demo.frame(0, 6, &server_data);
    demo.0.extend_from_slice(&small.repeat(2 * copies));
    demo.0
}

fn oversized_frame_demo() -> Vec<u8> {
    let max_segment = MAX_RAW_LEN as usize;
    let mut demo = Demo(synthetic(&[], false));
    demo.frame(0, 1, &vec![1u8; max_segment + max_segment / 2]);
    demo.0.extend_from_slice(&synthetic(&[], false));
    demo.0
}

fn entity_words_demo() -> Vec<u8> {
    let mut demo = Demo(synthetic(&[], false));
    let mut packet = vec![crate::protocol::SVC_PACKET_ENTITIES];
    for index in 0..120_000u32 {
        packet.extend_from_slice(&((511 - index % 511) as u16).to_le_bytes());
    }
    packet.extend_from_slice(&[0, 0]);
    demo.frame(5, crate::protocol::DEM_ALL, &packet);
    demo.0.extend_from_slice(&synthetic(&[], false));
    demo.0
}

#[test]
fn commands_demo_is_walked() -> anyhow::Result<()> {
    let demo = commands_demo();
    let file = crate::encode(&demo, 1)?;
    assert_eq!(unstructured_len(&file)?, 0);
    assert_eq!(crate::decode(&file, 1)?, demo);
    Ok(())
}

/// A match with float coordinates, then one whose `svc_serverdata` announces no extension.
fn two_matches_demo() -> Vec<u8> {
    [mvd1_float_demo(), commands_demo()].concat()
}

#[test]
fn second_match_starts_from_its_own_server_data() -> anyhow::Result<()> {
    let demo = two_matches_demo();
    let file = crate::encode(&demo, 1)?;
    assert_eq!(unstructured_len(&file)?, 0);
    assert_eq!(crate::decode(&file, 1)?, demo);
    Ok(())
}

/// Three matches in one segment: 16-bit coordinates, float (MVD1), 16-bit again, then a
/// sound before the player's next origin.
fn width_switch_demo() -> Vec<u8> {
    const MVD_PEXT_1_FLOAT_COORDS: u32 = 1 << 0;
    const ALL_ORIGINS: u16 = 0b111;
    const FIRST_ORIGIN: u16 = 0b001;
    const PLAYER_1_VOICE: u16 = 1 << 3 | 1;
    let server_data = |extensions: &[&[u8]]| {
        let mut parts: Vec<&[u8]> = extensions.to_vec();
        let tail = [
            28u32.to_le_bytes().to_vec(),
            1u32.to_le_bytes().to_vec(),
            c_string("qw"),
            0u32.to_le_bytes().to_vec(),
            c_string("width switch"),
            vec![0; 40],
        ];
        parts.extend(tail.iter().map(Vec::as_slice));
        message(11, &parts)
    };
    let position = |player: u8, step: i16| [10 * step + 256 * i16::from(player), 3 * step, 24];
    let coords = |origin: [i16; 3]| origin.map(i16::to_le_bytes).concat();
    let float_coords =
        |origin: [i16; 3]| origin.map(|coord| f32::from(coord).to_le_bytes()).concat();
    let player_info = |player: u8, flags: u16, origin: &[u8]| {
        message(42, &[&[player], &flags.to_le_bytes(), &[0], origin])
    };
    let mut demo = Demo(Vec::new());
    demo.frame(0, 6, &server_data(&[]));
    for step in 0..40 {
        let mut payload = Vec::new();
        for player in 0..2 {
            let origin = coords(position(player, step));
            payload.extend(player_info(player, ALL_ORIGINS, &origin));
        }
        demo.frame(13, 6, &payload);
    }
    let mvd1_float_coords = MVD_PEXT_1_FLOAT_COORDS.to_le_bytes();
    demo.frame(0, 6, &server_data(&[b"MVD1", &mvd1_float_coords]));
    for step in 40..60 {
        let mut payload = Vec::new();
        for player in 0..2 {
            let origin = position(player, step);
            payload.extend(if step % 2 == 0 {
                player_info(player, ALL_ORIGINS, &float_coords(origin))
            } else {
                player_info(player, FIRST_ORIGIN, &f32::from(origin[0]).to_le_bytes())
            });
        }
        demo.frame(13, 6, &payload);
    }
    demo.frame(0, 6, &server_data(&[]));
    let sound = message(
        6,
        &[
            &PLAYER_1_VOICE.to_le_bytes(),
            &[1],
            &coords(position(0, 39)),
        ],
    );
    demo.frame(13, 6, &sound);
    for step in 40..60 {
        let mut payload = Vec::new();
        for player in 0..2 {
            let origin = coords(position(player, step));
            payload.extend(player_info(player, ALL_ORIGINS, &origin));
        }
        demo.frame(13, 6, &payload);
    }
    demo.0
}

#[test]
fn width_switch_demo_is_walked() -> anyhow::Result<()> {
    let demo = width_switch_demo();
    let file = crate::encode(&demo, 1)?;
    assert_eq!(unstructured_len(&file)?, 0);
    assert_eq!(crate::decode(&file, 1)?, demo);
    Ok(())
}

fn truncated_match_demo() -> Vec<u8> {
    let small = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let mut demo = small[..small.len() - 3].to_vec();
    demo.extend(several_segments(&small));
    demo
}

#[test]
fn frames_are_walked_again_after_a_truncated_match() -> anyhow::Result<()> {
    let demo = truncated_match_demo();
    let file = crate::encode(&demo, 1)?;
    let unframed_segment_or_two = 2 * crate::segment::SEGMENT_TARGET_LEN;
    assert!(unstructured_len(&file)? < unframed_segment_or_two);
    assert_eq!(crate::decode(&file, 1)?, demo);
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
    let encode_any = |demo: &[u8]| anyhow::Ok(crate::encode::encode_any(demo, 1)?);
    let duel_source = "duel = sample8 demo 3fe477dc...1416";
    add(
        "duel.normal.mvdz",
        encode_any(&duel)?,
        &duel,
        &format!("{duel_source}; compress"),
    );
    add(
        "duel-truncated.normal.mvdz",
        encode_any(&duel[..150_000])?,
        &duel[..150_000],
        "the first 150000 bytes of duel (ends inside a frame)",
    );
    let mut rng = Rng(0x1234);
    let random: Vec<u8> = Vec::from_fn(3000, |_| rng.next() as u8);
    add(
        "float-mvd1.normal.mvdz",
        encode_any(&mvd1)?,
        &mvd1,
        "synthetic demo with MVD1 float coordinates (two players, sounds)",
    );
    add(
        "float-fte.normal.mvdz",
        encode_any(&fte)?,
        &fte,
        "the first 300000 bytes of stratified demo 03fc8ec2 (FTE float coordinates)",
    );
    let escapes = escapes();
    add(
        "escapes.normal.mvdz",
        encode_any(&escapes)?,
        &escapes,
        "synthetic demo with unknown commands and lying hidden blocks",
    );
    let multi_demoinfo = several_segments(&multi_demoinfo());
    add(
        "demoinfo-multi.normal.mvdz",
        encode_any(&multi_demoinfo)?,
        &multi_demoinfo,
        "synthetic demo carrying a 3-block KTX demoinfo twice, repeated to several segments",
    );
    add(
        "random.normal.mvdz",
        encode_any(&random)?,
        &random,
        "3000 random bytes (no frame: all tail), written by the unchecked entry point: rule 1 refuses them",
    );
    let framed = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    add(
        "framed-as-tail.mvdz",
        all_tail(&framed)?,
        &framed,
        "synthetic demo with demoinfo, forced to all tail (the encoder's fallback layout), models counted from the tail tokens only",
    );
    add(
        "framed-as-stored.mvdz",
        all_stored(&framed),
        &framed,
        "the same demo, forced to a stored segment (the encoder's last fallback), no models",
    );
    let (escape_walks_on, two_nops) = escape_walks_on()?;
    add(
        "escape-walks-on.mvdz",
        escape_walks_on,
        &two_nops,
        "one frame of two svc_nop, the first as an escape of 1 byte, the second walked with its length of 0 (FORMAT.md 4.5): no encoder here writes it",
    );
    add("empty.normal.mvdz", encode_any(b"")?, b"", "an empty demo");
    let usercmd = usercmd_demo();
    add(
        "usercmd.normal.mvdz",
        encode_any(&usercmd)?,
        &usercmd,
        "synthetic demo with usercmd hidden blocks (4 players, angles on and off the grid, other type-1 lengths)",
    );
    let usercmd_segments = several_segments(&usercmd);
    add(
        "usercmd.normal.segments.mvdz",
        encode_any(&usercmd_segments)?,
        &usercmd_segments,
        "the same, repeated to several segments",
    );
    let commands = commands_demo();
    add(
        "commands.normal.mvdz",
        encode_any(&commands)?,
        &commands,
        "synthetic demo with every frame type, every command of the sizer, the playerinfo, packet-entity, sound and temp-entity forms, then FTE entity extensions",
    );
    let protocol_change = protocol_change_demo();
    add(
        "protocol-change.normal.mvdz",
        encode_any(&protocol_change)?,
        &protocol_change,
        "synthetic demo whose second segment announces FTE2 extensions: later segments start from the changed protocol (rule 3)",
    );
    let two_matches = two_matches_demo();
    add(
        "two-matches.normal.mvdz",
        encode_any(&two_matches)?,
        &two_matches,
        "synthetic demo of two matches: float coordinates (MVD1), then a server data without extensions, which resets the protocol state",
    );
    let width_switch = width_switch_demo();
    add(
        "width-switch.normal.mvdz",
        encode_any(&width_switch)?,
        &width_switch,
        "synthetic demo of three matches in one segment: 16-bit coordinates, float (MVD1), 16-bit again, then a sound before the player's next origin",
    );
    let truncated_match = truncated_match_demo();
    add(
        "truncated-match.normal.mvdz",
        encode_any(&truncated_match)?,
        &truncated_match,
        "synthetic demo of a match cut off inside its last frame, then whole matches: the frames are walked again after the break (rule 2)",
    );
    let oversized = oversized_frame_demo();
    add(
        "oversized-frame.normal.mvdz",
        encode_any(&oversized)?,
        &oversized,
        "synthetic demo with a 1.5 MiB frame of nops: it is cut every 2^20 bytes and the frames go on after it (rule 2)",
    );
    let entity_words = entity_words_demo();
    add(
        "entity-words.normal.mvdz",
        encode_any(&entity_words)?,
        &entity_words,
        "synthetic demo with a packet of 120000 entity words, three ENTITY_GAP blob bytes per word",
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
            "# file\tdecoded_sha256 or \"error: <message>\"\tdecoded_size\thow it was made\n# Valid files must decode to the SHA-256; hostile files must be refused. The message is the reference's: another decoder should report the same category (the Error variants; FORMAT.md 7), in its own words.\n",
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
                let refusal = crate::decode(file, 1)
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

/// The canonical parse (FORMAT.md 8) on fixed inputs, for an encoder in another
/// language to check its parser before it compares whole files.
fn lz_tokens_text() -> String {
    use crate::lz::Token;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut noise = |bound: u64| (rng.next() % bound) as u8;
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
                Token::Match { len, distance } => format!("M{len},{distance}"),
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
