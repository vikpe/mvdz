use anyhow::Context as _;
use pretty_assertions::assert_eq;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_mvdz");

fn vector(name: &str) -> anyhow::Result<Vec<u8>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vectors")
        .join(name);
    Ok(std::fs::read(path)?)
}

fn demo(name: &str) -> anyhow::Result<Vec<u8>> {
    let mut demo = Vec::new();
    mvdz::Decoder::new(&vector(name)?[..])?.read_to_end(&mut demo)?;
    Ok(demo)
}

fn gzip(body: &[u8]) -> Vec<u8> {
    [&[0x1f, 0x8b, 8, 0][..], body].concat()
}

fn temp_dir(test: &str) -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("mvdz-cli-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn run(args: &[&Path]) -> anyhow::Result<std::process::Output> {
    Ok(Command::new(BIN).args(args).output()?)
}

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
        ("tiff.mvd", tiff, "not an MVD demo"),
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
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn closed_stdout_is_quiet() -> anyhow::Result<()> {
    let vectors = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vectors");
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
        let mut buffer = [0u8; 16];
        let mut stdout = child.stdout.take().context("a piped stdout")?;
        stdout.read_exact(&mut buffer).ok();
        drop(stdout);
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

    let outcome = run(&[Path::new("decompress"), &file_a, &file_b])?;
    assert_eq!(outcome.status.code(), Some(1), "{outcome:?}");
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("exists (-f overwrites)"));
    assert_eq!(std::fs::read(dir.join("b.mvd"))?, raw);
    std::fs::write(&demo_a, b"old")?;
    let outcome = run(&[Path::new("decompress"), Path::new("-f"), &file_a])?;
    assert!(outcome.status.success(), "{outcome:?}");
    assert_eq!(std::fs::read(&demo_a)?, raw);

    let outcome = run(&[
        Path::new("decompress"),
        Path::new("-f"),
        Path::new("--rm"),
        &file_b,
    ])?;
    assert!(outcome.status.success(), "{outcome:?}");
    assert_eq!(std::fs::read(dir.join("b.mvd"))?, raw);
    assert!(!file_b.exists(), "--rm deleted the input");

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
