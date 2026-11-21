//! Encode and decode speed over a fixed set of demos, at 4 threads and 1.
//!
//! The demos are every `*.mvd` in the directory named by `MVDZ_BENCH_DEMOS`
//! (none are in the repository; `scripts/fetch_samples.sh set8 DIR` downloads
//! set8, the speed set of BENCHMARKS.md); without it nothing is measured. One
//! iteration codes every demo once, one file at a time, as the library
//! does by default: the encoder with its self-check, the decoder with
//! every check. Throughput is MB/s of raw demo (MB = 10^6 bytes).
//!
//! ```text
//! # from the repository root
//! scripts/fetch_samples.sh set8 demos/set8
//! MVDZ_BENCH_DEMOS=$PWD/demos/set8 cargo bench --manifest-path mvdz_rust/Cargo.toml \
//!     --bench codec -- --save-baseline before
//! MVDZ_BENCH_DEMOS=$PWD/demos/set8 cargo bench --manifest-path mvdz_rust/Cargo.toml \
//!     --bench codec -- --baseline before
//! ```
//!
//! Compressed size and peak memory are not measured here.

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use std::hint::black_box;
use std::path::PathBuf;
use std::time::Duration;

/// Thread counts measured; 4 is the headline figure.
const THREADS: [u8; 2] = [4, 1];

/// The demos, sorted by file name, or none when `MVDZ_BENCH_DEMOS` is unset.
fn demos() -> Vec<Vec<u8>> {
    let Some(dir) = std::env::var_os("MVDZ_BENCH_DEMOS") else {
        eprintln!("MVDZ_BENCH_DEMOS is not set: no demos to measure");
        return Vec::new();
    };
    let dir = PathBuf::from(dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "mvd"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "{}: no .mvd files", dir.display());
    paths
        .iter()
        .map(|path| {
            std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        })
        .collect()
}

fn codec(criterion: &mut Criterion) {
    let raw = demos();
    if raw.is_empty() {
        return;
    }
    let raw_bytes: usize = raw.iter().map(Vec::len).sum();
    let packed: Vec<Vec<u8>> = raw
        .iter()
        .map(|demo| mvdz::encode(demo, 4).expect("the demo compresses"))
        .collect();
    for (demo, file) in raw.iter().zip(&packed) {
        let back = mvdz::decode(file, 4).expect("the file decompresses");
        assert!(back == *demo, "a demo does not round-trip");
    }
    let packed_bytes: usize = packed.iter().map(Vec::len).sum();
    eprintln!(
        "{} demos, {raw_bytes} bytes raw, {packed_bytes} bytes compressed",
        raw.len()
    );

    measure(criterion, "compress", raw_bytes, |threads| {
        for demo in &raw {
            black_box(mvdz::encode(black_box(demo), threads).expect("compress"));
        }
    });
    measure(criterion, "decompress", raw_bytes, |threads| {
        for file in &packed {
            black_box(mvdz::decode(black_box(file), threads).expect("decompress"));
        }
    });
}

/// One group, `name/4t` and `name/1t`: `run(threads)` codes every demo once.
fn measure(criterion: &mut Criterion, name: &str, raw_bytes: usize, run: impl Fn(u8)) {
    let mut group = criterion.benchmark_group(name);
    group
        .throughput(Throughput::BytesDecimal(raw_bytes as u64))
        .sampling_mode(SamplingMode::Flat)
        .sample_size(10)
        .measurement_time(Duration::from_secs(15));
    for threads in THREADS {
        group.bench_function(
            BenchmarkId::from_parameter(format!("{threads}t")),
            |bencher| {
                bencher.iter(|| run(threads));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, codec);
criterion_main!(benches);
