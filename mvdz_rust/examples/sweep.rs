//! Round-trip sweep: every file as the encoder writes it, decoded by the
//! whole-file and the sequential reader and compared by SHA-256.
//! Usage: sweep files...
use anyhow::Context as _;
use sha2::{Digest, Sha256};
use std::io::Read;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = mvdz::util::par_map(4, args.len(), |i| {
        let src = std::fs::read(&args[i]).with_context(|| args[i].clone())?;
        let want = Sha256::digest(&src);
        let (c, info) = mvdz::compress_info(&src, &mvdz::Config::default())?;
        let is_whole_ok = mvdz::decompress(&c, 1).is_ok_and(|d| Sha256::digest(&d) == want);
        let mut d = Vec::new();
        let is_sequential_ok =
            mvdz::Reader::new(std::io::Cursor::new(&c), 1, &mvdz::Limits::default())
                .is_ok_and(|mut r| r.read_to_end(&mut d).is_ok() && Sha256::digest(&d) == want);
        let mut fails = Vec::new();
        if !is_whole_ok {
            fails.push("whole-file reader");
        }
        if !is_sequential_ok {
            fails.push("sequential reader");
        }
        anyhow::Ok((args[i].clone(), src.len(), fails, info.fallback_segments))
    });
    let (mut n, mut raw, mut bad, mut fbs) = (0, 0usize, 0, 0);
    for file in res {
        let (f, len, fails, fb) = file?;
        n += 1;
        raw += len;
        fbs += fb;
        if !fails.is_empty() {
            bad += 1;
            println!("FAIL {f}: {fails:?}");
        }
    }
    println!(
        "files {n}  raw {:.1} MB  round trips {}  failed files {bad}  fallback segments {fbs}",
        mvdz::util::approx_f64(raw as u64) / 1e6,
        n * 2
    );
    Ok(())
}
