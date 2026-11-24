# Benchmarks

mvdz (format version 3) against rc.4 (`master`, format version 1) and general-purpose
compressors, with the Rust tool; then the Rust, Go and C implementations side by side.

- Ratio: compressed size / raw `.mvd` size.
- Speed: MB/s of raw demo, MB = 10^6 bytes.
- Memory: peak resident memory, MiB (`/usr/bin/time -f %M`).

## Setup

| | |
|---|---|
| CPU | AMD Ryzen 5 5600X, 6 cores, 12 threads |
| Memory | 62 GB |
| OS | Linux 7.0 |
| Build | Rust 1.100, `cargo build --release`, baseline `x86-64`; Go 1.27; C: gcc 13.3 `-O2` |
| Pinning | four physical cores, `taskset -c 2,3,4,5` |
| Date | 2026-11-24 |

One process per file, files on tmpfs, the median of 5 rounds (3 for
`scripts/bench_cli.sh`). An editor took about half a core during the runs (load 0.9 to
1.7).

rc.4 is `master` at 0bc8817, measured in the same session with the same feature set:
normal level, `-s 256K` segments, the compress self-check, and `--trusted` (CRC checks
only, as this version).

## Data

The archive: 50,951 demos from QuakeWorld servers, about 2000 to 2026, 72.0 GB as gzip,
about 112 GB raw.

| sample | demos | raw | used for |
|---|---|---|---|
| stratified | 379 | 969 MB | ratios; by era and size, weighted to the archive |
| sample8 | 8 | 53.2 MB | speeds; 0.79 to 6.2 MB, and one of 35.5 MB with usercmd blocks |
| sample40 | 32 | 200 MB | single demos by size |

The lists are in [`benchmarks/samples/`](benchmarks/samples);
`scripts/fetch_samples.sh <set> <dir>` downloads a set and checks every SHA-256.

## Ratio

The 379 stratified demos, weighted to the archive:

| format | size / raw |
|---|---|
| **mvdz** | **0.19** |
| gzip -9 | 0.57 |
| xz -9e | 0.43 |
| zstd -19 | 0.51 |
| the archive's gzip | 0.59 |

The whole archive would take about 23.6 GB as mvdz.

By year of the demo:

| era | demos | mvdz | gzip -9 | xz -9e | zstd -19 |
|---|---|---|---|---|---|
| up to 2006 | 127 | 0.27 | 0.61 | 0.51 | 0.55 |
| 2007-2010 | 95 | 0.24 | 0.61 | 0.49 | 0.55 |
| 2011-2020 | 49 | 0.25 | 0.62 | 0.51 | 0.57 |
| 2021-2022 | 19 | 0.18 | 0.61 | 0.44 | 0.55 |
| 2023-2024 | 43 | 0.18 | 0.60 | 0.43 | 0.54 |
| 2025-2026 | 45 | 0.16 | 0.49 | 0.36 | 0.44 |

Recent demos hold about 62 snapshots a second where older ones hold 20 to 27: a smaller
time step is easier to predict.

Demos that record player inputs (usercmd blocks; 10 demos of 36 MB): mvdz 0.11, gzip
0.40.

## Speed

sample8, decoding against general-purpose tools (`scripts/bench_cli.sh`; batch: 4
processes at once, one thread each):

| decoder | size / raw | 1 process | batch of 4 |
|---|---|---|---|
| mvdz, `-j 1` | 0.15 | 96.5 | 156.1 |
| mvdz, `-j 4` | 0.15 | 236.8 | 258.2 |
| gzip -9 | 0.47 | 193.5 | 316.8 |
| xz -9 | 0.36 | 56.1 | 100.5 |
| zstd -19 | 0.42 | 508.4 | 782.6 |

Single demos by size. The 199.5 MB file is the 32 demos of sample40 joined; the 2.0 MB
figures include starting the process.

| | 2.0 MB | 14.2 MB | 199.5 MB |
|---|---|---|---|
| compress, `-j 1` | 29.5 | 33.5 | 35.7 |
| compress, `-j 4` | 58.4 | 86.0 | 100.1 |
| decompress, `-j 1` | 82.1 | 89.4 | 93.0 |
| decompress, `-j 4` | 155.5 | 224.5 | 228.0 |

## Memory

The same three files:

| | 2.0 MB | 14.2 MB | 199.5 MB |
|---|---|---|---|
| compress, `-j 1` | 10.0 | 15.8 | 37.9 |
| compress, `-j 4` | 14.2 | 21.2 | 45.6 |
| decompress, `-j 1` | 4.8 | 6.8 | 15.9 |
| decompress, `-j 4` | 6.7 | 8.9 | 18.9 |

The tool holds the models and the segments in flight, not the demo.

## Against rc.4

Size: mvdz files are 1.4 % larger than rc.4's on the stratified sample (both 0.19 of
raw).

Speed on sample8:

| | rc.4 | **mvdz** | change |
|---|---|---|---|
| compress, `-j 1` | 28.6 | **35.7** | +25 % |
| compress, `-j 4` | 48.1 | **79.1** | +64 % |
| decompress, `-j 1` | 91.0 | **96.7** | +6 % |
| decompress, `-j 4` | 189.8 | **229.0** | +21 % |

Memory on the 14.2 MB demo:

| | rc.4 | **mvdz** | change |
|---|---|---|---|
| compress, `-j 1` | 93.5 | **15.6** | -83 % |
| compress, `-j 4` | 103.2 | **21.1** | -80 % |
| decompress, `-j 1` | 22.4 | **6.9** | -69 % |
| decompress, `-j 4` | 24.2 | **9.1** | -62 % |

Memory on the 199.5 MB file:

| | rc.4 | **mvdz** | change |
|---|---|---|---|
| compress, `-j 1` | 1,225.9 | **37.5** | -97 % |
| compress, `-j 4` | 1,236.8 | **46.5** | -96 % |
| decompress, `-j 1` | 236.7 | **16.0** | -93 % |
| decompress, `-j 4` | 238.9 | **19.0** | -92 % |

rc.4's memory grows with the demo; mvdz holds the models and the segments in flight.

## Implementations

Rust (`mvdz`), Go (`Encoder`, `Reader`, file to file) and C (`mvdz_decode`, whole
buffers, one thread), measured in one run. Speed on sample8; memory on the 14.2 MB
demo. The Go and Rust encoders write the same bytes.

Speed:

| | Rust | Go | C |
|---|---|---|---|
| compress, 1 thread | 36.1 | 19.9 | - |
| compress, 4 threads | 77.7 | 44.4 | - |
| decompress, 1 thread | 95.4 | 61.9 | 87.6 |
| decompress, 4 threads | 211.9 | 138.2 | - |

Memory:

| | Rust | Go | C |
|---|---|---|---|
| compress, 1 thread | 15.8 | 27.2 | - |
| compress, 4 threads | 20.4 | 40.9 | - |
| decompress, 1 thread | 6.9 | 12.9 | 19.7 |
| decompress, 4 threads | 9.1 | 19.0 | - |

The C figure includes the file and the demo, which it holds in memory.

## Reproducing

```
scripts/fetch_samples.sh sample8 demos/sample8
scripts/fetch_samples.sh stratified demos/stratified

# decode speed against gzip, xz and zstd
ls demos/sample8/*.mvd > demos/sample8.list
scripts/bench_cli.sh demos/cli-bench demos/sample8.list

# the C decoder, on the files the run above compressed
make -C mvdz_c mvdz_bench
MVDZ=mvdz_c/mvdz_bench scripts/bench_cli.sh demos/cli-bench demos/sample8.list

# library speeds with criterion
MVDZ_BENCH_DEMOS=$PWD/demos/sample8 cargo bench --manifest-path mvdz_rust/Cargo.toml --bench codec

# round trips of every demo
cargo run --manifest-path mvdz_rust/Cargo.toml --release --example sweep -- demos/stratified/*.mvd
```
