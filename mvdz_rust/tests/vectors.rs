//! Golden vectors (tests/vectors, MANIFEST.tsv): every valid file must
//! decode to the recorded SHA-256 through every reader; every hostile file
//! must be refused by every reader with the recorded error. These are the
//! cross-implementation vectors (FORMAT.md).
//!
//! `vectors_reproduce` rebuilds every vector from its source (the sample
//! demos of the local cache, or the synthetic demos below) and requires the
//! encoder's output to equal the committed file byte for byte. With
//! `MVDZ_WRITE_VECTORS=1` it writes the files and the manifest instead.

mod common;
use anyhow::Context as _;
use common::*;
use mvdz::format::*;
use mvdz::util::put_uv;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

fn dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors")
}

fn manifest() -> anyhow::Result<Vec<Vec<String>>> {
    Ok(std::fs::read_to_string(dir().join("MANIFEST.tsv"))?
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| l.split('\t').map(String::from).collect())
        .collect())
}

#[test]
fn vectors_decode() -> anyhow::Result<()> {
    let m = manifest()?;
    assert!(m.len() >= 30, "the vector set is committed");
    for row in m {
        let (file, expect) = (&row[0], &row[1]);
        let b = std::fs::read(dir().join(file)).with_context(|| file.clone())?;
        let got = readers_agree(&b, file);
        if let Some(err) = expect.strip_prefix("error: ") {
            assert!(got.is_none(), "{file}: accepted");
            let refusal = mvdz::decompress(&b, 1).err().map(|e| e.to_string());
            assert_eq!(refusal.as_deref(), Some(err), "{file}: error");
        } else {
            let d = got
                .with_context(|| format!("{file}: refused: {:?}", mvdz::decompress(&b, 1).err()))?;
            assert_eq!(&hex(&Sha256::digest(&d)), expect, "{file}: decoded SHA-256");
            assert_eq!(d.len().to_string(), row[2], "{file}: decoded size");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- the vector set

/// (file, bytes, decoded demo or None for a hostile file, how it was made)
type Vector = (String, Vec<u8>, Option<Vec<u8>>, String);

/// A demo with escapes: unknown commands, a command that does not fit, and
/// hidden blocks with lying lengths.
fn escapes() -> Vec<u8> {
    let mut d = Demo(synthetic(&[], false));
    for i in 0..30u8 {
        let mut p = s(8, &[&[2], &z("before\n")]);
        p.extend_from_slice(&[0xEE, i, 1, 2, 3]);
        d.frame(1, 6, &p);
        d.frame(0, 6, &[14, 1, 2]);
        let mut h = Vec::new();
        h.extend_from_slice(&1000u32.to_le_bytes());
        h.extend_from_slice(&7u16.to_le_bytes());
        h.extend_from_slice(&[i; 9]);
        d.0.push(2);
        d.0.push(3);
        d.0.extend_from_slice(&0u32.to_le_bytes());
        d.0.extend_from_slice(&(h.len() as u32).to_le_bytes());
        d.0.extend_from_slice(&h);
    }
    d.0
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
    let mut d = synthetic(&[], false);
    d.extend(synthetic(&parts[..3], false));
    d.extend(synthetic(&parts[3..], false));
    d
}

/// A model header of `n` tables with every symbol present: amplifies
/// cheaply under an adaptive coder (robustness review 1 and 5).
fn amplifying_model(n: u32) -> Vec<u8> {
    use mvdz::coder::rc::{Enc, IntModel};
    const NQC: usize = 306;
    let mut e = Enc::new();
    let mut im = IntModel::new(NQC + 8);
    im.put(&mut e, NQC, 256);
    im.put(&mut e, NQC + 1, n);
    let mut prev = 0usize;
    for _ in 0..256 {
        im.put(&mut e, 17 * 17 + prev.min(16), 1);
        prev = 1;
    }
    for t in 0..n {
        im.put(&mut e, NQC + 2, u32::from(t > 0));
        let mut prev = 0usize;
        for _ in 0..256 {
            im.put(&mut e, 17 + prev.min(16), 1);
            prev = 1;
        }
    }
    let hdr = e.finish();
    let mut b = Vec::new();
    put_uv(&mut b, hdr.len() as u64);
    b.extend_from_slice(&hdr);
    put_uv(&mut b, 16);
    b.extend_from_slice(&[0; 16]);
    b
}

/// A transform body made by hand: the structural streams `lazy` (id, back
/// end, count, body) and the blob streams `blobs` (id, bytes), in that
/// order.
/// Returns the models block's body and the transform body. Each structural
/// stream is coded with a model of its own symbols (so few that the model
/// is its fallback table alone: the contexts do not matter).
fn transform_body(
    lazy: &[(usize, &[u8])],
    blobs: &[(usize, &[u8])],
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    use mvdz::coder::rans;
    let mut models: Vec<Option<rans::Shared>> = Vec::new();
    let mut ids = Vec::new();
    let mut bodies = Vec::new();
    for (id, syms) in lazy {
        let ctxs = vec![0u16; syms.len()];
        let mut t = rans::Tally::new(256);
        t.add(syms, &ctxs);
        let m = t.shared().context("a model")?;
        bodies.push(rans::encode_bytes(syms, &ctxs, &m));
        models.resize_with(models.len().max(id + 1), || None);
        models[*id] = Some(m);
        ids.push(*id);
    }
    let lazy: Vec<(usize, u64, &[u8])> = lazy
        .iter()
        .zip(&bodies)
        .map(|((id, syms), b)| (*id, syms.len() as u64, &b[..]))
        .collect();
    let lazy = &lazy;
    let mut body = Vec::new();
    // fte, fte2, mvd1, tail; the number of streams
    for x in [0, 0, 0, 0, (lazy.len() + blobs.len()) as u64] {
        put_uv(&mut body, x);
    }
    for (id, count, b) in lazy {
        for x in [*id as u64, *count, b.len() as u64] {
            put_uv(&mut body, x);
        }
    }
    let mut all = Vec::new();
    for (id, b) in blobs {
        put_uv(&mut body, *id as u64);
        put_uv(&mut body, b.len() as u64);
        all.extend_from_slice(b);
    }
    let z = if all.is_empty() {
        Vec::new()
    } else {
        mvdz::deflate::compress(&all, 9)
    };
    put_uv(&mut body, z.len() as u64);
    for (_, _, b) in lazy {
        body.extend_from_slice(b);
    }
    body.extend_from_slice(&z);
    Ok((mvdz::segment::tables_body(&models, &ids), body))
}

/// The file of `demo` as one transform segment with these models and body.
fn one_segment(demo: &[u8], (models, body): &(Vec<u8>, Vec<u8>)) -> anyhow::Result<Vec<u8>> {
    let (_, ms, _) = mvdz::segment::frame_scan(demo);
    let crc = mvdz::util::crc32(demo);
    let mut b = seg_hdr_bytes(u32::try_from(demo.len())?, u32::try_from(ms)?, crc);
    b.extend_from_slice(body);
    Ok(Parts {
        header: Header {
            size: demo.len() as u64,
            duration_ms: ms,
            crc,
            sha256: Sha256::digest(demo).into(),
        },
        models: models.clone(),
        segs: vec![b],
    }
    .build())
}

/// Hostile files: refused by every reader.
fn hostile(v: &mut Vec<Vector>) -> anyhow::Result<()> {
    let mut add = |name: &str, b: Vec<u8>, how: &str| {
        v.push((format!("hostile-{name}.mvdz"), b, None, how.to_string()));
    };
    let demo = synthetic(&[(1, "{\"a\":"), (0, "1}")], false);
    let base = compress(&several_segments(&demo))?;
    let p = parts(&base)?;
    assert!(p.segs.len() > 3);
    add(
        "base-cut",
        base[..base.len() - 10].to_vec(),
        "the base file (a synthetic demo repeated to several segments) without its last 10 bytes",
    );
    let mut x = base.clone();
    x.push(0);
    add("trailing-byte", x, "one byte after the last segment");
    let mut q = p.clone();
    q.header.size += 1;
    add("header-size", q.build(), "the header's size + 1");
    let mut q = p.clone();
    q.header.duration_ms += 1;
    add("header-duration", q.build(), "the header's duration_ms + 1");
    let mut q = p.clone();
    q.header.crc ^= 1;
    add(
        "header-crc",
        q.build(),
        "one bit of the header's CRC-32 flipped",
    );
    let mut x = base.clone();
    x[30] = 1;
    add(
        "header-checksum-field",
        x,
        "the high half of the header's checksum field set",
    );
    let mut x = base.clone();
    x[4] = 3;
    add("version", x, "version 3");
    let mut q = p.clone();
    q.segs.remove(2);
    add("dropped-segment", q.build(), "the third segment removed");
    let mut q = p.clone();
    q.segs.swap(1, 2);
    add(
        "swapped-segments",
        q.build(),
        "the second and third segments swapped",
    );
    let mut q = p.clone();
    q.segs.insert(1, p.segs[1].clone());
    add("repeated-segment", q.build(), "the second segment twice");
    let h = parse_seg_hdr(&p.segs[1])?;
    let mut q = p.clone();
    q.segs[1][h.body_off - 2] ^= 0x10;
    add(
        "segment-crc",
        q.build(),
        "one bit of the second segment's CRC-32 flipped",
    );
    let mut q = p.clone();
    q.segs
        .insert(1, seg_hdr_bytes(0, 0, mvdz::util::crc32(&[])));
    add("segment-empty", q.build(), "an empty segment");
    let mut q = p.clone();
    q.segs[0].resize(SEG_FIELDS + usize::try_from(MAX_SEG_CODED)? + 1, 0);
    add(
        "segment-length",
        q.build(),
        "the first segment padded with zeros to one byte over the largest coded length (2 MiB)",
    );
    // a segment whose header claims 1 ms more than its frames hold (the
    // header's duration raised with it)
    let mut q = parts(&compress(&synthetic(&[], false))?)?;
    assert_eq!(q.segs.len(), 1);
    let dur = u32::from_le_bytes(q.segs[0][4..8].try_into()?);
    q.segs[0][4..8].copy_from_slice(&(dur + 1).to_le_bytes());
    q.header.duration_ms += 1;
    add(
        "segment-duration",
        q.build(),
        "a segment whose dur_ms is 1 more than its frame times",
    );
    let mut x = base[..HEADER_LEN].to_vec();
    x.extend_from_slice(&(256u32 << 20).to_le_bytes());
    x.extend_from_slice(&[0; 100]);
    add(
        "models-size",
        x,
        "a models block of 256 MiB declared in a few bytes",
    );
    let mut x = base[..HEADER_LEN].to_vec();
    for s in &p.segs {
        segment_block(&mut x, s);
    }
    add(
        "models-missing",
        x,
        "the base file without its models block",
    );
    // one usercmd block in a segment built by hand
    let mut one = Demo(Vec::new());
    one.hidden(
        0,
        &[(1, &usercmd_block(0, 0, 13, [0.0; 3], [0; 3], [0, 0]))],
    );
    let one = one.0;
    let ucmd_seg = |uc: Option<&[u8]>, misc: Option<&[u8]>| {
        use mvdz::streams::*;
        let mut lazy: Vec<(usize, &[u8])> = vec![(TIME, &[0]), (TYPE, &[3])];
        if let Some(u) = uc {
            lazy.push((UC, u));
        }
        let mut blobs: Vec<(usize, &[u8])> = vec![
            (MASK, &[0; 4]),
            (HIDTAG, &[1, 2]),
            (HIDLEN, &[23, 0, 0, 0]),
            (HIDTYPE, &[1, 0]),
        ];
        if let Some(m) = misc {
            blobs.push((HIDMISC, m));
        }
        one_segment(&one, &transform_body(&lazy, &blobs)?)
    };
    add(
        "ucmd-grid",
        ucmd_seg(Some(&[0, 0, 13, 9]), None)?,
        "a usercmd block whose grid flags symbol is 9",
    );
    let misc = usercmd_block(0, 0, 13, [0.0; 3], [0; 3], [0, 0]);
    add(
        "ucmd-misc",
        ucmd_seg(None, Some(&misc))?,
        "a usercmd block (type 1, 23 bytes) stored in hid.misc, no uc stream",
    );
    tables(&mut add)
}

/// Hostile models blocks (FORMAT.md §2.6, §8.4), from the usercmd demo
/// repeated to several segments.
fn tables(add: &mut impl FnMut(&str, Vec<u8>, &str)) -> anyhow::Result<()> {
    let uc = usercmd_demo();
    let tc = compress(&several_segments(&uc))?;
    let p = parts(&tc)?;
    assert!(!p.models.is_empty(), "a segmented normal file has models");
    let ents = tables_entries(&p.models)?;
    let with = |e: &[(usize, Vec<u8>)]| {
        let mut q = p.clone();
        q.models = tables_body(e);
        q.build()
    };
    let mut e = ents.clone();
    e.remove(0);
    add(
        "tables-no-entry",
        with(&e),
        "the models block without the entry of a stream the segments code with rANS",
    );
    let mut q = p.clone();
    q.models.clear();
    add(
        "tables-missing",
        q.build(),
        "an empty models block: rANS streams without a model",
    );
    let mut e = ents.clone();
    e.swap(0, 1);
    add(
        "tables-order",
        with(&e),
        "two entries of the models block swapped (stream ids not increasing)",
    );
    let mut e = ents.clone();
    e.insert(1, e[0].clone());
    add(
        "tables-dup-id",
        with(&e),
        "an entry of the models block repeated (the same stream id twice)",
    );
    let lazy = ents
        .iter()
        .find(|e| mvdz::streams::is_lazy(e.0))
        .context("a structural stream's model")?;
    let num = ents
        .iter()
        .position(|e| mvdz::streams::is_num(e.0))
        .context("a numeric stream's model")?;
    let mut e = ents.clone();
    e.push((mvdz::streams::TEXT, lazy.1.clone()));
    add(
        "tables-blob-id",
        with(&e),
        "a model for a blob stream (text)",
    );
    let mut e = ents.clone();
    let at = e
        .iter()
        .position(|x| x.0 > mvdz::streams::PI_NUM)
        .context("a model after pi.num")?;
    e.insert(at, (mvdz::streams::PI_NUM, lazy.1.clone()));
    add(
        "tables-plain-id",
        with(&e),
        "a model for a stream that has none (pi.num)",
    );
    let mut e = ents.clone();
    e.insert(num, (40, lazy.1.clone()));
    add(
        "tables-unassigned-id",
        with(&e),
        "a model for stream id 40 (unassigned)",
    );
    add("tables-empty", with(&[]), "a models block with n = 0");
    let mut q = p.clone();
    q.models.push(0);
    add(
        "tables-trailing",
        q.build(),
        "a models block with one byte after its last entry",
    );
    let mut e = ents.clone();
    e[0].1.push(0);
    add(
        "tables-padding",
        with(&e),
        "a model with one byte of padding (not consumed exactly)",
    );
    let mut e = ents.clone();
    e[num].1 = lazy.1.clone();
    add(
        "tables-alphabet",
        with(&e),
        "a numeric stream's entry holding a structural model (alphabet 256)",
    );
    let mut e = ents.clone();
    e[0].1 = {
        use mvdz::coder::rc::{Enc, IntModel};
        const NQC: usize = 306;
        let mut enc = Enc::new();
        let mut im = IntModel::new(NQC + 8);
        for (ctx, v) in [(NQC, 256), (NQC + 1, 65536)] {
            im.put(&mut enc, ctx, v);
        }
        enc.finish()
    };
    add(
        "tables-m1",
        with(&e),
        "a model declaring 65536 own tables (rule M1)",
    );
    let amp = amplifying_model(2000);
    let mut e = ents.clone();
    e[0].1 = mvdz::util::Rd::new(&amp)
        .bytes()
        .context("the model's header")?
        .to_vec();
    add(
        "tables-decisions",
        with(&e),
        "a model breaking the decision limit (rule M2)",
    );
    Ok(())
}

/// The entries (stream id, model) of a models block.
fn tables_entries(b: &[u8]) -> anyhow::Result<Vec<(usize, Vec<u8>)>> {
    let mut r = mvdz::util::Rd::new(b);
    let n = r.uz().context("the entry count")?;
    (0..n)
        .map(|_| Some((r.uz()?, r.bytes()?.to_vec())))
        .collect::<Option<_>>()
        .context("a models entry")
}

/// A models block from its entries.
fn tables_body(e: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut b = Vec::new();
    put_uv(&mut b, e.len() as u64);
    for (sid, m) in e {
        put_uv(&mut b, *sid as u64);
        put_uv(&mut b, m.len() as u64);
        b.extend_from_slice(m);
    }
    b
}

/// Every hostile vector is refused by every reader (they are built from
/// synthetic demos, so this runs without the demo cache).
#[test]
fn hostile_vectors_are_refused() -> anyhow::Result<()> {
    let mut v = Vec::new();
    hostile(&mut v)?;
    assert!(v.len() > 30);
    for (name, b, _, _) in &v {
        assert!(readers_agree(b, name).is_none(), "{name}: accepted");
    }
    Ok(())
}

/// Every vector; None without the demos they are made from.
fn all_vectors() -> anyhow::Result<Option<Vec<Vector>>> {
    let sources = || {
        let duel = duel()?;
        let mvd1 = std::fs::read(test_data("gen")?.join("float_mvd1.mvd")).ok()?;
        let fte = sample("3fdd80182a39f773710e4468c509106926450a296c0c9e4a9d4d821f4a783ddb")?;
        Some((duel, mvd1, fte))
    };
    let Some((duel, mvd1, fte)) = sources() else {
        return Ok(None);
    };
    let fte = fte[..300_000].to_vec();
    let mut v: Vec<Vector> = Vec::new();
    let mut add = |name: &str, b: Vec<u8>, d: &[u8], how: &str| {
        v.push((name.to_string(), b, Some(d.to_vec()), how.to_string()));
    };
    // without the input check: some vectors hold non-demo input
    let one = |d: &[u8]| anyhow::Ok(unchecked(d)?.0);
    let src = "duel = sample 3ff1f82b...8d92";
    add(
        "duel.normal.mvdz",
        one(&duel)?,
        &duel,
        &format!("{src}; compress"),
    );
    add(
        "duel-truncated.normal.mvdz",
        one(&duel[..150_000])?,
        &duel[..150_000],
        "the first 150000 bytes of duel (ends inside a frame)",
    );
    let mut rng = Rng(0x1234);
    let random: Vec<u8> = Vec::from_fn(3000, |_| rng.next() as u8);
    add(
        "float-mvd1.normal.mvdz",
        one(&mvd1)?,
        &mvd1,
        "rev-correct gen1/float_mvd1.mvd (MVD1 float coordinates, 3 escapes)",
    );
    add(
        "float-fte.normal.mvdz",
        one(&fte)?,
        &fte,
        "the first 300000 bytes of sample 3fdd8018 (FTE float coordinates)",
    );
    let esc = escapes();
    add(
        "escapes.normal.mvdz",
        one(&esc)?,
        &esc,
        "synthetic demo with unknown commands and lying hidden blocks",
    );
    let md = several_segments(&multi_demoinfo());
    add(
        "demoinfo-multi.normal.mvdz",
        one(&md)?,
        &md,
        "synthetic demo carrying a 3-block KTX demoinfo twice, repeated to several segments",
    );
    add(
        "random.normal.mvdz",
        one(&random)?,
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
    add("empty.normal.mvdz", one(b"")?, b"", "an empty demo");
    let uc = usercmd_demo();
    add(
        "usercmd.normal.mvdz",
        one(&uc)?,
        &uc,
        "synthetic demo with usercmd hidden blocks (4 players, angles on and off the grid, other type-1 lengths)",
    );
    let ucs = several_segments(&uc);
    add(
        "usercmd.normal.segments.mvdz",
        one(&ucs)?,
        &ucs,
        "the same, repeated to several segments",
    );
    hostile(&mut v)?;
    Ok(Some(v))
}

#[test]
fn vectors_reproduce() -> anyhow::Result<()> {
    let Some(v) = all_vectors()? else {
        eprintln!("vector sources not cached, skipping");
        return Ok(());
    };
    if std::env::var_os("MVDZ_WRITE_VECTORS").is_some() {
        let _ = std::fs::create_dir_all(dir());
        for f in std::fs::read_dir(dir())? {
            let p = f?.path();
            if p.extension().is_some_and(|x| x == "mvdz") {
                std::fs::remove_file(p)?;
            }
        }
        let mut m = String::from(
            "# file\tdecoded_sha256 or \"error: <message>\"\tdecoded_size\thow it was made\n# Valid files must decode to the SHA-256; hostile files must be refused. The message is the reference's: another decoder should report the same category (format, corrupt, limit; FORMAT.md 9), in its own words.\n",
        );
        for (name, b, d, how) in &v {
            std::fs::write(dir().join(name), b)?;
            if let Some(d) = d {
                writeln!(m, "{name}\t{}\t{}\t{how}", hex(&Sha256::digest(d)), d.len())?;
            } else {
                let refusal = mvdz::decompress(b, 1)
                    .err()
                    .context("a hostile vector decodes")?;
                writeln!(m, "{name}\terror: {refusal}\t-\t{how}")?;
            }
        }
        std::fs::write(dir().join("MANIFEST.tsv"), m)?;
        return Ok(());
    }
    let rows = manifest()?;
    assert_eq!(rows.len(), v.len(), "the manifest lists every vector");
    for (name, b, _, _) in &v {
        let committed = std::fs::read(dir().join(name)).with_context(|| name.clone())?;
        assert!(
            committed == *b,
            "{name}: the encoder no longer reproduces the committed vector"
        );
    }
    Ok(())
}
