//! Bytes per command, coded bytes per stream and ablations over the `*.mvd` and `*.mvd.gz`
//! files under a directory:
//!
//! ```text
//! cargo run --release --example stats -- DIR [--limit N] [--ablate N] [-j N] > stats.tsv
//! ```
//!
//! `--limit`: the first N files in path order; `--ablate`: the first N of them get every ablation.

use mvdz::stats::Stats;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn main() {
    let mut dir = None;
    let mut limit = usize::MAX;
    let mut ablated_count = 0;
    let mut threads = std::thread::available_parallelism().map_or(1, usize::from);
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut number = || -> usize {
            let value = arguments.next().and_then(|value| value.parse().ok());
            value.unwrap_or_else(|| panic!("{argument} takes a number"))
        };
        match argument.as_str() {
            "--limit" => limit = number(),
            "--ablate" => ablated_count = number(),
            "-j" => threads = number(),
            _ => dir = Some(PathBuf::from(argument)),
        }
    }
    let dir = dir.expect("usage: stats DIR [--limit N] [--ablate N] [-j N]");
    let mut paths = Vec::new();
    collect_demos(&dir, &mut paths);
    paths.sort();
    paths.truncate(limit);
    eprintln!("{} demos, {threads} threads", paths.len());

    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let work = || {
        let mut stats = Stats::default();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(path) = paths.get(index) else {
                break stats;
            };
            let measured = std::panic::catch_unwind(|| {
                let demo = read_demo(path)?;
                Stats::of_demo(&demo, index < ablated_count).map_err(|error| error.to_string())
            });
            match measured {
                Ok(Ok(demo_stats)) => stats.merge(&demo_stats),
                Ok(Err(error)) => eprintln!("skipped {}: {error}", path.display()),
                Err(_) => eprintln!("skipped {}: panic", path.display()),
            }
            if (index + 1).is_multiple_of(1000) {
                eprintln!("{} demos, {} s", index + 1, started.elapsed().as_secs());
            }
        }
    };
    let mut total = Stats::default();
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads).map(|_| scope.spawn(work)).collect();
        for worker in workers {
            total.merge(&worker.join().expect("a worker"));
        }
    });
    eprintln!(
        "{} of {} demos measured, {} s",
        total.demos(),
        paths.len(),
        started.elapsed().as_secs()
    );
    print!("{}", total.report_tsv());
}

fn collect_demos(dir: &Path, paths: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        let name = path.to_string_lossy();
        if path.is_dir() {
            collect_demos(&path, paths);
        } else if name.ends_with(".mvd") || name.ends_with(".mvd.gz") {
            paths.push(path);
        }
    }
}

fn read_demo(path: &Path) -> Result<Vec<u8>, String> {
    if path.extension().is_none_or(|extension| extension != "gz") {
        return std::fs::read(path).map_err(|error| error.to_string());
    }
    let gzip = Command::new("gzip")
        .arg("-dc")
        .arg(path)
        .output()
        .map_err(|error| error.to_string())?;
    if gzip.status.success() {
        Ok(gzip.stdout)
    } else {
        Err(String::from_utf8_lossy(&gzip.stderr).trim().to_owned())
    }
}
