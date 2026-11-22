# mvdz benchmarks

Compression ratios, speeds, memory and round trips of mvdz (format version 2), and of
rc.4 (`master`, format version 1) under the same premise: 256 KiB segments
(`-s 256K`), compression with its self-check, decompression with CRC checks only (rc.4's
`--trusted`: this version never recomputes the SHA-256). MB = 10^6
bytes of **raw demo**; speeds are MB/s of raw demo, so a codec's speed does not depend
on how well it compresses.

## Machine

An Intel Core i5-12400F (6 cores, 12 threads; AVX2 and SHA extensions), 62 GB of
memory, Linux 6.8, Rust 1.100, `cargo build --release` (glibc, system allocator,
portable `x86-64`). Speeds ran pinned to four physical cores (`taskset -c 0,2,4,6`)
with an editor using about one core in the background; master and this version ran
in rotated order in every round, so background load falls on both alike. Figures are
the median over rounds, with [min-max].

## Samples

The archive: 50,951 gzip-compressed demos, 72.0 GB as gzip, about 112 GB raw, from
QuakeWorld servers between about 2000 and 2026, named by the SHA-256 of the raw demo.

| sample | n | raw | used for |
|---|---|---|---|
| stratified | 379 of 585 | 969 MB | ratios: stratified by era x gzip-size tercile, weighted to the archive |
| set8 | 8 | 18.4 MB | speeds: 8 demos of sample40 from 0.69 to 6.2 MB |
| sample40 | 32 of 40 | 125 MB | development |

The lists (the SHA-256 of each raw demo; for stratified also the cells and weights)
are in [`benchmarks/samples/`](benchmarks/samples). `scripts/fetch_samples.sh <set>
<dir>` downloads a set from `https://dev.quake.world/mvdz/<set>.tar.gz`, unpacks it
and checks every SHA-256. Not every demo of the original samples could be found again:
stratified and sample40 have 379 and 32 of their demos; set8 is complete (two of its
demos were replaced by the sample40 demos nearest in size).

## Ratios

Ratio = compressed bytes / raw bytes, weighted to the archive (each stratified demo
counts for the archive demos of its cell; the weights are rescaled to the demos
present), with 95 % intervals from a bootstrap within the cells. On the 379 stratified
demos:

| format | size / raw | size / archive gzip |
|---|---|---|
| **mvdz** | **0.193** [0.190-0.197] | 0.326 |
| mvdz rc.4, `-s 256K` | 0.192 | |
| archive gzip | 0.593 [0.581-0.603] | 1 |
| gzip -9 | 0.566 [0.554-0.577] | 0.956 |
| xz -9e | 0.429 [0.422-0.435] | 0.724 |
| zstd -19 | 0.508 [0.498-0.517] | 0.858 |

Projected onto the whole archive (72.0 GB as gzip): 23.5 GB. Unweighted, the 379
demos (968,979,765 bytes raw) compress to 211,736,432 bytes (0.2185); the median demo
to 0.241. mvdz files are 2.6 times smaller than zstd -19 and 2.2 times smaller than xz
-9e.

Against rc.4 at the same segment size: +0.9 % (raw DEFLATE instead of zstd for the blob
streams, fixed-width integers).

**By era** (year of the demo; weighted, 95 % intervals):

| era | n | mvdz | gzip -9 | xz -9e | zstd -19 |
|---|---|---|---|---|---|
| up to 2006 | 127 | 0.262 [0.258-0.266] | 0.611 | 0.508 | 0.554 |
| 2007-2010 | 95 | 0.238 [0.231-0.243] | 0.609 | 0.492 | 0.552 |
| 2011-2020 | 49 | 0.251 [0.241-0.264] | 0.622 | 0.508 | 0.566 |
| 2021-2022 | 19 | 0.180 [0.175-0.184] | 0.610 | 0.442 | 0.546 |
| 2023-2024 | 43 | 0.180 [0.175-0.185] | 0.599 | 0.433 | 0.536 |
| 2025-2026 | 45 | 0.156 [0.147-0.163] | 0.493 | 0.359 | 0.439 |

(One undated demo is not listed.) Current mvdsv records a snapshot every server frame
(`sv_demofps 77`; about 62 a second in practice) where older demos mostly hold 20-27;
the smaller time step makes each snapshot easier to predict.

## Speeds

`mvdz compress` and `mvdz decompress`, one process per file over the 8 demos of set8,
the output written to tmpfs; 5 rounds. `-j 4` encodes and decodes up to 4 segments at
once.

| | rc.4 `-s 256K` | **this version** |
|---|---|---|
| size / raw (set8) | 0.2207 | 0.2218 |
| compress, `-j 1` | 22.1 [21.3-22.5] | **29.5** [28.8-29.6] |
| compress, `-j 4` | 41.1 [38.6-41.5] | **73.0** [66.1-73.8] |
| decompress, `-j 1` | 72.1 [71.3-72.4] | **80.7** [78.4-81.6] |
| decompress, `-j 4` | 142.1 [136.9-142.8] | **222.6** [178.4-224.0] |

Compression is 1.3 times faster on one thread and 1.8 times on four (raw DEFLATE
instead of zstd for the blob streams; segments encode in parallel). Decompression is
1.1 times faster on one thread and 1.6 times on four (segments decode in parallel).

**Against general-purpose decompressors** (`scripts/bench_cli.sh`, same files, one
process per file; "batch" = 4 processes in parallel, one thread each; 3 rounds):

| format | size / raw | decode, 1 process | decode, batch of 4 |
|---|---|---|---|
| mvdz, `-j 1` | 0.222 | 80.5 [79.6-80.8] | 171.6 [170.4-173.8] |
| mvdz, `-j 4` | 0.222 | 211.1 [200.8-219.1] | 251.9 [249.4-253.0] |
| gzip -9 (`gzip -dc`) | 0.603 | 144.2 [138.2-146.0] | 310.2 [309.1-310.4] |
| xz -9 (`xz -dc`) | 0.475 | 37.9 [37.8-38.2] | 80.8 [80.5-81.3] |
| zstd -19 (`zstd -dc`) | 0.545 | 406.1 [397.5-408.0] | 773.3 [767.8-781.0] |

## Memory

Peak resident memory of the command line (`/usr/bin/time -f %M`), rc.4 `-s 256K` ->
this version:

| demo | compress `-j 1` | compress `-j 4` | decompress `-j 1` | decompress `-j 4` |
|---|---|---|---|---|
| 14.2 MB (sample40) | 93 -> **35** MiB | 102 -> **42** MiB | 23 -> **5.6** MiB | 24 -> **7.6** MiB |
| 6.2 MB (set8) | 49 -> **22** MiB | 57 -> **28** MiB | 13 -> **5.2** MiB | 15 -> **7.2** MiB |
| 2.0 MB (set8) | 23 -> **13** MiB | 29 -> **17** MiB | 7.8 -> **4.6** MiB | 8.8 -> **6.6** MiB |

Decompression streams: its memory is a few segments (at most 1 MiB raw each) however
large the demo; rc.4's grows with the demo. Compression holds the demo and its walked
segments: about 2.5 times the demo, 2.1 to 2.7 times less than rc.4.

## Correctness

* Round trips (`examples/sweep.rs`): every demo of stratified (379) and set8 (8), 987
  MB raw, compressed and decoded by both the whole-file decoder and the streaming
  `Decoder`: 774 round trips, 0 failures, byte-identical by SHA-256.
* The test suite (`cargo nextest run --release`, 49 tests) with the set8 demos and the
  FTE float demo in `MVDZ_TEST_DATA`: all pass. The golden vectors in
  `vectors/` are refused or decoded as `MANIFEST.tsv` records.
* Fuzzing (`mvdz_rust/mvdz/fuzz/`: `decode`, `stream_reader`, `compress_rt`,
  `gen_rt`): not yet run on format version 2.

## Reproducing

```
# the demos: set8 for speeds, stratified for ratios
scripts/fetch_samples.sh set8 demos/set8
scripts/fetch_samples.sh stratified demos/stratified

# library speeds over every *.mvd in a directory, 4 threads and 1, with criterion
MVDZ_BENCH_DEMOS=$PWD/demos/set8 cargo bench --manifest-path mvdz_rust/Cargo.toml --bench codec

# command-line decode speed against gzip, xz and zstd (one process per file)
ls demos/set8/*.mvd > demos/set8.list
scripts/bench_cli.sh demos/cli-bench demos/set8.list

# round trips: every demo through the whole-file and the streaming decoder
cargo run --manifest-path mvdz_rust/Cargo.toml --release --example sweep -- demos/stratified/*.mvd
```

For the confidence intervals, draw the sample at random from the whole collection
(stratify by era and size if the collection is skewed), weight each demo by how many it
stands for, and bootstrap within the strata.
