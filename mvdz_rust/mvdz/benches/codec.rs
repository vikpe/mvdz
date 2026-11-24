//! Encode and decode speed of every `*.mvd` in `MVDZ_BENCH_DEMOS`, at 4 threads and 1:
//!
//! ```text
//! # from the repository root
//! scripts/fetch_samples.sh sample8 demos/sample8
//! MVDZ_BENCH_DEMOS=$PWD/demos/sample8 cargo bench --manifest-path mvdz_rust/Cargo.toml \
//!     --bench codec -- --save-baseline before
//! MVDZ_BENCH_DEMOS=$PWD/demos/sample8 cargo bench --manifest-path mvdz_rust/Cargo.toml \
//!     --bench codec -- --baseline before
//! ```

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use std::hint::black_box;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::time::Duration;

/// 4 is the headline figure.
const THREADS: [u8; 2] = [4, 1];

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
        .map(|demo| encode(demo, 4).expect("the demo compresses"))
        .collect();
    for (demo, file) in raw.iter().zip(&packed) {
        let back = decode(file, 4).expect("the file decompresses");
        assert!(back == *demo, "a demo does not round-trip");
    }
    let packed_bytes: usize = packed.iter().map(Vec::len).sum();
    eprintln!(
        "{} demos, {raw_bytes} bytes raw, {packed_bytes} bytes compressed",
        raw.len()
    );

    measure(criterion, "compress", raw_bytes, |threads| {
        for demo in &raw {
            black_box(encode(black_box(demo), threads).expect("compress"));
        }
    });
    measure(criterion, "decompress", raw_bytes, |threads| {
        for file in &packed {
            black_box(decode(black_box(file), threads).expect("decompress"));
        }
    });
}

fn encode(demo: &[u8], max_threads: u8) -> mvdz::Result<Vec<u8>> {
    let mut file = Vec::new();
    mvdz::Encoder::new()
        .max_threads(max_threads)
        .encode(Cursor::new(demo), &mut file)?;
    Ok(file)
}

fn decode(file: &[u8], max_threads: u8) -> mvdz::Result<Vec<u8>> {
    let mut demo = Vec::new();
    mvdz::Decoder::new(file)?
        .max_threads(max_threads)
        .read_to_end(&mut demo)?;
    Ok(demo)
}

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
