//! The command-line tool: inputs that are not demos, output names, and a
//! closed standard output.

use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_mvdz");

fn vector(name: &str) -> anyhow::Result<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mvdz/tests/vectors")
        .join(name);
    Ok(std::fs::read(path)?)
}

/// The raw demo of a valid vector.
fn demo(name: &str) -> anyhow::Result<Vec<u8>> {
    Ok(mvdz::decompress(&vector(name)?, 1)?)
}

/// Data that starts like a gzip file (RFC 1952: magic, DEFLATE, no flags).
fn gzip(body: &[u8]) -> Vec<u8> {
    [&[0x1f, 0x8b, 8, 0][..], body].concat()
}

/// A fresh, empty directory for one test.
fn temp_dir(test: &str) -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("mvdz-cli-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn run(args: &[&Path]) -> anyhow::Result<std::process::Output> {
    Ok(Command::new(BIN).args(args).output()?)
}

/// Input that is not a demo, or a gzip-compressed one (to be decompressed
/// first): an error, and no output file (not even a temporary one).
#[test]
fn non_demos_are_refused() -> anyhow::Result<()> {
    let dir = temp_dir("notdemo")?;
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
    for (name, bytes, message) in &cases {
        let (input, output) = (dir.join(name), dir.join(format!("{name}.mvdz")));
        std::fs::write(&input, bytes)?;
        let outcome = run(&[Path::new("compress"), &input, &output])?;
        let stderr = String::from_utf8_lossy(&outcome.stderr);
        assert_eq!(outcome.status.code(), Some(1), "{name}: {outcome:?}");
        assert!(stderr.contains(message), "{name}: {stderr}");
        assert!(!output.exists(), "{name}: wrote an output");
    }
    let left: Vec<_> = std::fs::read_dir(&dir)?.collect();
    assert_eq!(left.len(), cases.len(), "temporary files left");
    // --allow-non-demo stores a non-demo anyway, and it round-trips
    let (input, output) = (dir.join("tiff.mvd"), dir.join("forced.mvdz"));
    let outcome = run(&[
        Path::new("compress"),
        Path::new("--allow-non-demo"),
        &input,
        &output,
    ])?;
    assert!(outcome.status.success(), "{outcome:?}");
    assert_eq!(mvdz::decompress(&std::fs::read(&output)?, 1)?, tiff);
    std::fs::remove_file(&output)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Every command that writes to standard output stops quietly when the
/// reader goes away.
#[test]
fn closed_stdout_is_quiet() -> anyhow::Result<()> {
    let vectors = Path::new(env!("CARGO_MANIFEST_DIR")).join("../mvdz/tests/vectors");
    let duel = vectors.join("duel.normal.mvdz");
    let dir = temp_dir("pipe")?;
    let raw = dir.join("duel.mvd");
    std::fs::write(&raw, demo("duel.normal.mvdz")?)?;
    let infos: Vec<&Path> = Vec::from_fn(2000, |_| duel.as_path());
    let cases: Vec<Vec<&Path>> = vec![
        [&[Path::new("info")][..], &infos].concat(),
        vec![Path::new("decompress"), &duel, Path::new("-")],
        vec![Path::new("compress"), &raw, Path::new("-")],
    ];
    for args in &cases {
        let mut child = Command::new(BIN)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut buf = [0u8; 16];
        let mut stdout = child.stdout.take().context("a piped stdout")?;
        stdout.read_exact(&mut buf).ok();
        // the read end is closed here
        let outcome = child.wait_with_output()?;
        let stderr = String::from_utf8_lossy(&outcome.stderr);
        assert!(
            outcome.status.success(),
            "{:?}: {:?} {stderr}",
            args[0],
            outcome.status
        );
        assert!(stderr.is_empty(), "{:?}: {stderr}", args[0]);
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// compress and decompress name each output after its input, keep the
/// inputs, refuse to overwrite without -f, report a bad file and go on.
#[test]
fn outputs_named_after_inputs() -> anyhow::Result<()> {
    let dir = temp_dir("names")?;
    let raw = demo("duel.normal.mvdz")?;
    let (demo_a, demo_b) = (dir.join("a.mvd"), dir.join("b.MVD"));
    std::fs::write(&demo_a, &raw)?;
    std::fs::write(&demo_b, &raw)?;
    let outcome = run(&[
        Path::new("compress"),
        Path::new("-j"),
        Path::new("2"),
        &demo_a,
        &demo_b,
    ])?;
    assert!(outcome.status.success(), "{outcome:?}");
    let (file_a, file_b) = (dir.join("a.mvdz"), dir.join("b.mvdz"));
    assert_eq!(std::fs::read(&file_a)?, std::fs::read(&file_b)?);
    assert!(demo_a.exists() && demo_b.exists(), "inputs are kept");

    // a.mvd exists: refused without -f, the other file still written
    let outcome = run(&[Path::new("decompress"), &file_a, &file_b])?;
    assert_eq!(outcome.status.code(), Some(1), "{outcome:?}");
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("exists (-f overwrites)"));
    assert_eq!(std::fs::read(dir.join("b.mvd"))?, raw);
    std::fs::write(&demo_a, b"old")?;
    let outcome = run(&[Path::new("decompress"), Path::new("-f"), &file_a])?;
    assert!(outcome.status.success(), "{outcome:?}");
    assert_eq!(std::fs::read(&demo_a)?, raw);

    // --rm deletes the input once written
    let outcome = run(&[
        Path::new("decompress"),
        Path::new("-f"),
        Path::new("--rm"),
        &file_b,
    ])?;
    assert!(outcome.status.success(), "{outcome:?}");
    assert_eq!(std::fs::read(dir.join("b.mvd"))?, raw);
    assert!(!file_b.exists(), "--rm deleted the input");

    // -o names the output; the two-name form still works
    let bin_output = dir.join("x.bin");
    assert!(
        run(&[Path::new("compress"), Path::new("-o"), &bin_output, &demo_a])?
            .status
            .success()
    );
    let mvdz_output = dir.join("y.mvdz");
    assert!(
        run(&[Path::new("compress"), &demo_a, &mvdz_output])?
            .status
            .success()
    );
    assert_eq!(std::fs::read(&bin_output)?, std::fs::read(&mvdz_output)?);
    // existing outputs need -f in that form too
    let outcome = run(&[Path::new("compress"), &demo_a, &mvdz_output])?;
    assert_eq!(outcome.status.code(), Some(1), "{outcome:?}");
    assert!(
        run(&[
            Path::new("compress"),
            Path::new("-f"),
            &demo_a,
            &mvdz_output
        ])?
        .status
        .success()
    );

    // a name decompress cannot derive from, and two inputs with one output
    let outcome = run(&[Path::new("decompress"), &bin_output, &file_a])?;
    assert_eq!(outcome.status.code(), Some(1), "{outcome:?}");
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("not named *.mvdz"));
    let demo_a_upper = dir.join("a.MVD");
    std::fs::write(&demo_a_upper, &raw)?;
    std::fs::remove_file(&file_a)?;
    let outcome = run(&[Path::new("compress"), &demo_a, &demo_a_upper])?;
    assert_eq!(outcome.status.code(), Some(1), "{outcome:?}");
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("is also the output of"));
    assert!(file_a.exists(), "the first of the two was written");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
