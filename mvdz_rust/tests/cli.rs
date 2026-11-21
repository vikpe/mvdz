//! The command-line tool: inputs that are not demos, output names, and a
//! closed standard output.

use anyhow::Context as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_mvdz");

fn vector(name: &str) -> anyhow::Result<Vec<u8>> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/vectors")
        .join(name);
    Ok(std::fs::read(p)?)
}

/// The raw demo of a valid vector.
fn demo(name: &str) -> anyhow::Result<Vec<u8>> {
    Ok(mvdz::decompress(&vector(name)?, 1)?)
}

/// Data that starts like a gzip file (RFC 1952: magic, DEFLATE, no flags).
fn gzip(b: &[u8]) -> Vec<u8> {
    [&[0x1f, 0x8b, 8, 0][..], b].concat()
}

/// A fresh, empty directory for one test.
fn tmp(test: &str) -> anyhow::Result<PathBuf> {
    let d = std::env::temp_dir().join(format!("mvdz-cli-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

fn run(args: &[&Path]) -> anyhow::Result<std::process::Output> {
    Ok(Command::new(BIN).args(args).output()?)
}

/// Input that is not a demo, or a gzip-compressed one (to be decompressed
/// first): an error, and no output file (not even a temporary one).
#[test]
fn non_demos_are_refused() -> anyhow::Result<()> {
    let d = tmp("notdemo")?;
    let raw = demo("duel.normal.mvdz")?;
    let tiff = b"II*\x00\x08\x00\x00\x00".repeat(200);
    let cases: [(&str, Vec<u8>, &str); 4] = [
        ("empty.mvd", Vec::new(), "not an MVD demo"),
        (
            "text.mvd",
            b"# not a demo\njust some text\n".repeat(100),
            "not an MVD demo",
        ),
        ("tiff.mvd", tiff.clone(), "not an MVD demo"),
        ("duel.mvd.gz", gzip(&raw), "decompress it first"),
    ];
    for (name, bytes, msg) in &cases {
        let (i, o) = (d.join(name), d.join(format!("{name}.mvdz")));
        std::fs::write(&i, bytes)?;
        let r = run(&[Path::new("compress"), &i, &o])?;
        let err = String::from_utf8_lossy(&r.stderr);
        assert_eq!(r.status.code(), Some(1), "{name}: {r:?}");
        assert!(err.contains(msg), "{name}: {err}");
        assert!(!o.exists(), "{name}: wrote an output");
    }
    let left: Vec<_> = std::fs::read_dir(&d)?.collect();
    assert_eq!(left.len(), cases.len(), "temporary files left");
    // --allow-non-demo stores a non-demo anyway, and it round-trips
    let (i, o) = (d.join("tiff.mvd"), d.join("forced.mvdz"));
    let r = run(&[Path::new("compress"), Path::new("--allow-non-demo"), &i, &o])?;
    assert!(r.status.success(), "{r:?}");
    assert_eq!(mvdz::decompress(&std::fs::read(&o)?, 1)?, tiff);
    std::fs::remove_file(&o)?;
    let _ = std::fs::remove_dir_all(&d);
    Ok(())
}

/// Every command that writes to standard output stops quietly when the
/// reader goes away.
#[test]
fn closed_stdout_is_quiet() -> anyhow::Result<()> {
    let vectors = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors");
    let duel = vectors.join("duel.normal.mvdz");
    let d = tmp("pipe")?;
    let raw = d.join("duel.mvd");
    std::fs::write(&raw, demo("duel.normal.mvdz")?)?;
    let infos: Vec<&Path> = Vec::from_fn(2000, |_| duel.as_path());
    let cases: Vec<Vec<&Path>> = vec![
        [&[Path::new("info")][..], &infos].concat(),
        vec![Path::new("decompress"), &duel, Path::new("-")],
        vec![Path::new("compress"), &raw, Path::new("-")],
    ];
    for args in &cases {
        let mut c = Command::new(BIN)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut buf = [0u8; 16];
        let mut stdout = c.stdout.take().context("a piped stdout")?;
        stdout.read_exact(&mut buf).ok();
        // the read end is closed here
        let r = c.wait_with_output()?;
        let err = String::from_utf8_lossy(&r.stderr);
        assert!(r.status.success(), "{:?}: {:?} {err}", args[0], r.status);
        assert!(err.is_empty(), "{:?}: {err}", args[0]);
    }
    let _ = std::fs::remove_dir_all(&d);
    Ok(())
}

/// compress and decompress name each output after its input, keep the
/// inputs, refuse to overwrite without -f, report a bad file and go on.
#[test]
fn outputs_named_after_inputs() -> anyhow::Result<()> {
    let d = tmp("names")?;
    let raw = demo("duel.normal.mvdz")?;
    let (a, b) = (d.join("a.mvd"), d.join("b.MVD"));
    std::fs::write(&a, &raw)?;
    std::fs::write(&b, &raw)?;
    let r = run(&[
        Path::new("compress"),
        Path::new("-j"),
        Path::new("2"),
        &a,
        &b,
    ])?;
    assert!(r.status.success(), "{r:?}");
    let (az, bz) = (d.join("a.mvdz"), d.join("b.mvdz"));
    assert_eq!(std::fs::read(&az)?, std::fs::read(&bz)?);
    assert!(a.exists() && b.exists(), "inputs are kept");

    // a.mvd exists: refused without -f, the other file still written
    let r = run(&[Path::new("decompress"), &az, &bz])?;
    assert_eq!(r.status.code(), Some(1), "{r:?}");
    assert!(String::from_utf8_lossy(&r.stderr).contains("exists (-f overwrites)"));
    assert_eq!(std::fs::read(d.join("b.mvd"))?, raw);
    std::fs::write(&a, b"old")?;
    let r = run(&[Path::new("decompress"), Path::new("-f"), &az])?;
    assert!(r.status.success(), "{r:?}");
    assert_eq!(std::fs::read(&a)?, raw);

    // --rm deletes the input once written
    let r = run(&[
        Path::new("decompress"),
        Path::new("-f"),
        Path::new("--rm"),
        &bz,
    ])?;
    assert!(r.status.success(), "{r:?}");
    assert_eq!(std::fs::read(d.join("b.mvd"))?, raw);
    assert!(!bz.exists(), "--rm deleted the input");

    // -o names the output; the two-name form still works
    let o = d.join("x.bin");
    assert!(
        run(&[Path::new("compress"), Path::new("-o"), &o, &a])?
            .status
            .success()
    );
    let o2 = d.join("y.mvdz");
    assert!(run(&[Path::new("compress"), &a, &o2])?.status.success());
    assert_eq!(std::fs::read(&o)?, std::fs::read(&o2)?);
    // existing outputs need -f in that form too
    let r = run(&[Path::new("compress"), &a, &o2])?;
    assert_eq!(r.status.code(), Some(1), "{r:?}");
    assert!(
        run(&[Path::new("compress"), Path::new("-f"), &a, &o2])?
            .status
            .success()
    );

    // a name decompress cannot derive from, and two inputs with one output
    let r = run(&[Path::new("decompress"), &o, &az])?;
    assert_eq!(r.status.code(), Some(1), "{r:?}");
    assert!(String::from_utf8_lossy(&r.stderr).contains("not named *.mvdz"));
    let c = d.join("a.MVD");
    std::fs::write(&c, &raw)?;
    std::fs::remove_file(&az)?;
    let r = run(&[Path::new("compress"), &a, &c])?;
    assert_eq!(r.status.code(), Some(1), "{r:?}");
    assert!(String::from_utf8_lossy(&r.stderr).contains("is also the output of"));
    assert!(az.exists(), "the first of the two was written");
    let _ = std::fs::remove_dir_all(&d);
    Ok(())
}
