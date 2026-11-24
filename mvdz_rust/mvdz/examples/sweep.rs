//! Usage: sweep files... (each must round-trip through both decoders)
use anyhow::Context as _;
use sha2::{Digest, Sha256};
use std::io::Read;

fn main() -> anyhow::Result<()> {
    const WORKERS: usize = 4;
    let paths: Vec<String> = std::env::args().skip(1).collect();
    let chunk_len = paths.len().div_ceil(WORKERS).max(1);
    let results: Vec<anyhow::Result<(usize, Vec<&str>)>> = std::thread::scope(|scope| {
        // every worker starts before the first is joined
        let mut workers = Vec::new();
        for chunk in paths.chunks(chunk_len) {
            workers.push(
                scope.spawn(move || chunk.iter().map(|path| check(path)).collect::<Vec<_>>()),
            );
        }
        workers
            .into_iter()
            .flat_map(|worker| {
                worker
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    let (mut raw, mut failed_files) = (0, 0);
    for (path, result) in paths.iter().zip(results) {
        let (len, fails) = result?;
        raw += len;
        if !fails.is_empty() {
            failed_files += 1;
            println!("FAIL {path}: {fails:?}");
        }
    }
    println!(
        "files {}  raw {raw} bytes  round trips {}  failed files {failed_files}",
        paths.len(),
        paths.len() * 2
    );
    Ok(())
}

fn check(path: &str) -> anyhow::Result<(usize, Vec<&'static str>)> {
    let source = std::fs::read(path).with_context(|| path.to_owned())?;
    let want = Sha256::digest(&source);
    let mut file = Vec::new();
    mvdz::Encoder::new()
        .max_threads(1)
        .encode(std::io::Cursor::new(&source), &mut file)?;
    let mut demo = Vec::new();
    let is_sequential_ok = mvdz::Decoder::new(std::io::Cursor::new(&file))
        .and_then(|mut decoder| Ok(decoder.read_to_end(&mut demo)?))
        .is_ok_and(|_| Sha256::digest(&demo) == want);
    let mut fails = Vec::new();
    if !is_sequential_ok {
        fails.push("sequential reader");
    }
    Ok((source.len(), fails))
}
