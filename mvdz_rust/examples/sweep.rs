//! Round-trip sweep: every file as the encoder writes it (256 KiB
//! segments) and at one other segment size (-s, default 64K); decoded by
//! the whole-file and the sequential reader and compared by SHA-256.
//! Usage: sweep [-s size] files... (`size` takes K, M, G suffixes, binary).
use anyhow::Context as _;
use sha2::{Digest, Sha256};
use std::io::Read;

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut seg = 1usize << 16;
    if args.first().is_some_and(|a| a == "-s") {
        seg = size(&args[1])?;
        args.drain(..2);
    }
    // None: the format's own segment size
    let configs: Vec<Option<usize>> = vec![None, Some(seg)];
    let res = mvdz::util::par_map(4, args.len(), |i| {
        let src = std::fs::read(&args[i]).with_context(|| args[i].clone())?;
        let want = Sha256::digest(&src);
        let mut fails = Vec::new();
        let mut fb = 0;
        for &s in &configs {
            let cfg = mvdz::Config::default();
            let (c, info) = match s {
                None => mvdz::compress_info(&src, &cfg)?,
                Some(s) => mvdz::encode::compress_segmented(&src, s, &cfg)?,
            };
            fb += info.fallback_segments;
            let ok1 = mvdz::decompress(&c, 1).is_ok_and(|d| Sha256::digest(&d) == want);
            let mut d = Vec::new();
            let ok2 = mvdz::Reader::new(std::io::Cursor::new(&c), 1, &mvdz::Limits::default())
                .is_ok_and(|mut r| r.read_to_end(&mut d).is_ok() && Sha256::digest(&d) == want);
            if !(ok1 && ok2) {
                fails.push(format!("segment {s:?}"));
            }
        }
        anyhow::Ok((args[i].clone(), src.len(), fails, fb))
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
        "files {n}  raw {:.1} MB  configs {}  round trips {}  failed files {bad}  fallback segments {fbs}",
        mvdz::util::approx_f64(raw as u64) / 1e6,
        configs.len(),
        n * configs.len() * 2
    );
    Ok(())
}

/// A size with an optional binary K, M or G suffix.
fn size(s: &str) -> anyhow::Result<usize> {
    let (num, shift) = match s.as_bytes().last() {
        Some(b'k' | b'K') => (&s[..s.len() - 1], 10),
        Some(b'm' | b'M') => (&s[..s.len() - 1], 20),
        Some(b'g' | b'G') => (&s[..s.len() - 1], 30),
        _ => (s, 0),
    };
    let n: usize = num.parse().with_context(|| format!("bad size {s}"))?;
    n.checked_mul(1 << shift)
        .with_context(|| format!("size {s} too large"))
}
