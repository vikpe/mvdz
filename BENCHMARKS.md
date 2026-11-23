# mvdz benchmarks

Compression ratios, speeds, memory and round trips of mvdz (format version 3), and of
rc.4 (`master`, format version 1) under the same premise: 256 KiB segments
(`-s 256K`), compression with its self-check, decompression with CRC checks only (rc.4's
`--trusted`: this version never recomputes the SHA-256). MB = 10^6
bytes of **raw demo**; speeds are MB/s of raw demo, so a codec's speed does not depend
on how well it compresses.

## Machine

An Intel Core i5-12400F (6 cores, 12 threads; AVX2 and SHA extensions), 62 GB of
memory, Linux 6.8, Rust 1.100, `cargo build --release` (glibc, system allocator,
portable `x86-64`). Speeds ran pinned to four physical cores (`taskset -c 0,2,4,6`)
with an editor using about one core in the background. The rc.4 figures are from an
earlier session on the same machine. Figures are the median over rounds, with
[min-max].

## Samples

The archive: 50,951 gzip-compressed demos, 72.0 GB as gzip, about 112 GB raw, from
QuakeWorld servers between about 2000 and 2026, named by the SHA-256 of the raw demo.

| sample | n | raw | used for |
|---|---|---|---|
| stratified | 379 of 585 | 969 MB | ratios: stratified by era x gzip-size tercile, weighted to the archive |
| sample8 | 8 | 53.2 MB | speeds: 7 demos of sample40 from 0.79 to 6.2 MB and one usercmd demo of 35.5 MB |
| sample40 | 32 of 40 | 200 MB | development; includes two usercmd demos |

The lists (the SHA-256 of each raw demo; for stratified also the cells and weights)
are in [`benchmarks/samples/`](benchmarks/samples). `scripts/fetch_samples.sh <set>
<dir>` downloads a set from `https://dev.quake.world/mvdz/<set>.tar.gz`, unpacks it
and checks every SHA-256. Not every demo of the original samples could be found again:
stratified and sample40 have 379 and 32 of their demos; sample8 is complete (two of its
demos were replaced by the sample40 demos nearest in size).

## Ratios

Ratio = compressed bytes / raw bytes, weighted to the archive (each stratified demo
counts for the archive demos of its cell; the weights are rescaled to the demos
present), with 95 % intervals from a bootstrap within the cells. On the 379 stratified
demos:

| format | size / raw | size / archive gzip |
|---|---|---|
| **mvdz** | **0.194** [0.191-0.197] | 0.327 |
| mvdz rc.4, `-s 256K` | 0.192 | |
| archive gzip | 0.593 [0.581-0.603] | 1 |
| gzip -9 | 0.566 [0.554-0.577] | 0.956 |
| xz -9e | 0.429 [0.422-0.435] | 0.724 |
| zstd -19 | 0.508 [0.498-0.517] | 0.858 |

Projected onto the whole archive (72.0 GB as gzip): 23.5 GB. Unweighted, the 379
demos (968,979,765 bytes raw) compress to 212,044,254 bytes (0.2188); the median demo
to 0.243. mvdz files are 2.6 times smaller than zstd -19 and 2.2 times smaller than xz
-9e.

Against rc.4 at the same segment size: +1.1 % (the blob streams as LZ77 tokens coded
with rANS instead of zstd, fixed-width integers).

**By era** (year of the demo; weighted, 95 % intervals):

| era | n | mvdz | gzip -9 | xz -9e | zstd -19 |
|---|---|---|---|---|---|
| up to 2006 | 127 | 0.264 [0.260-0.269] | 0.611 | 0.508 | 0.554 |
| 2007-2010 | 95 | 0.239 [0.232-0.244] | 0.609 | 0.492 | 0.552 |
| 2011-2020 | 49 | 0.252 [0.242-0.264] | 0.622 | 0.508 | 0.566 |
| 2021-2022 | 19 | 0.180 [0.176-0.185] | 0.610 | 0.442 | 0.546 |
| 2023-2024 | 43 | 0.180 [0.175-0.185] | 0.599 | 0.433 | 0.536 |
| 2025-2026 | 45 | 0.156 [0.147-0.164] | 0.493 | 0.359 | 0.439 |

(One undated demo is not listed.) Current mvdsv records a snapshot every server frame
(`sv_demofps 77`; about 62 a second in practice) where older demos mostly hold 20-27;
the smaller time step makes each snapshot easier to predict.

**Demos with usercmd blocks.** mvdsv can record every player input as a hidden block
(type 1, 23 bytes, `SV_DebugClientCommand`; about 77 a second per player). Such demos
are rare: none in the archive, none on the public QTV servers checked in November 2026,
but one server records them. On 10 such 4on4 demos of October 2026 (36 MB raw each, the
blocks 60 % of it; two of them are in sample40, one of those in sample8) the blocks are
coded field by field (FORMAT.md 4.11, 5.6):

| | size / raw | / gzip |
|---|---|---|
| gzip (the hub's `.mvd.gz`) | 0.402 | 1 |
| mvdz | 0.107 (0.104 to 0.111 per demo) | 0.266 |

The usercmd layout codes the blocks at 0.058 of their raw bytes; with the blocks stored
as generic hidden blocks instead, the files are 90 % larger (0.203 of raw).

## Speeds

`mvdz compress` and `mvdz decompress`, one process per file over the 8 demos of set8
(sample8 before the usercmd demo replaced its smallest), the output written to tmpfs; 5 rounds. `-j 4` encodes and decodes up to 4 segments at
once.

| | rc.4 `-s 256K` | **this version** |
|---|---|---|
| size / raw (sample8) | 0.2207 | 0.2225 |
| compress, `-j 1` | 22.1 [21.3-22.5] | **28.1** [27.8-28.5] |
| compress, `-j 4` | 41.1 [38.6-41.5] | **71.1** [70.2-71.4] |
| decompress, `-j 1` | 72.1 [71.3-72.4] | **82.9** [82.2-83.8] |
| decompress, `-j 4` | 142.1 [136.9-142.8] | **226.7** [225.8-227.3] |

Compression is 1.3 times faster on one thread and 1.7 times on four (segments encode
in parallel). Decompression is 1.1 times faster on one thread and 1.6 times on four
(segments decode in parallel).

**Against general-purpose decompressors** (`scripts/bench_cli.sh`, same files, one
process per file; "batch" = 4 processes in parallel, one thread each; 3 rounds):

| format | size / raw | decode, 1 process | decode, batch of 4 |
|---|---|---|---|
| mvdz, `-j 1` | 0.223 | 80.9 [79.6-81.9] | 178.4 [168.3-179.8] |
| mvdz, `-j 4` | 0.223 | 221.7 [214.2-221.7] | 256.8 [243.8-260.9] |
| gzip -9 (`gzip -dc`) | 0.603 | 142.8 [142.7-143.3] | 307.9 [306.8-309.1] |
| xz -9 (`xz -dc`) | 0.475 | 37.8 [37.7-38.0] | 80.2 [80.1-80.3] |
| zstd -19 (`zstd -dc`) | 0.545 | 403.4 [402.8-404.7] | 750.2 [748.3-776.4] |

## Memory

Peak resident memory of the command line (`/usr/bin/time -f %M`), rc.4 `-s 256K` ->
this version:

| demo | compress `-j 1` | compress `-j 4` | decompress `-j 1` | decompress `-j 4` |
|---|---|---|---|---|
| 14.2 MB (sample40) | 93 -> **16** MiB | 102 -> **21** MiB | 23 -> **6.9** MiB | 24 -> **9.2** MiB |
| 6.2 MB (sample8) | 49 -> **14** MiB | 57 -> **19** MiB | 13 -> **5.7** MiB | 15 -> **7.5** MiB |
| 2.0 MB (sample8) | 23 -> **9.8** MiB | 29 -> **14** MiB | 7.8 -> **4.7** MiB | 8.8 -> **6.5** MiB |

Neither direction holds the demo: compression reads the file twice (counts, then codes)
and writes each segment as it is coded; decompression streams. The memory is the models
plus the segments worked on at once, so it follows the models a demo needs, not its
length. One 14.2 MB demo repeated, this version:

| demo | compress `-j 1` | compress `-j 4` | decompress `-j 1` | decompress `-j 4` |
|---|---|---|---|---|
| 14 MB | 16 MiB | 21 MiB | 6.9 MiB | 9.3 MiB |
| 57 MB (4 times) | 22 MiB | 29 MiB | 9.9 MiB | 13 MiB |
| 227 MB (16 times) | 27 MiB | 36 MiB | 12 MiB | 14 MiB |
| 567 MB (40 times) | 29 MiB | 38 MiB | 12 MiB | 15 MiB |

A longer demo earns more tables (FORMAT.md 8 rule 4), up to the contexts it has: the
figures level off. 50 different 4on4 demos joined (575 MB), the most models seen: 41 and
53 MiB to compress, 20 and 23 MiB to decompress. The encoder that held the demo took 836
MiB for it. rc.4's memory grows with the demo in both directions.

`-M` (`Encoder::max_memory`, `Decoder::max_memory`) lowers the segments worked on at
once to stay within a figure, approximately; the least is one segment (32 MiB to
compress, 14 MiB to decompress). The 36 MB usercmd demo at `-j 8`: 18, 25 and 30 MiB to
compress with `-M` 32, 48 and none; 7.7, 11 and 13 MiB to decompress with `-M` 14, 24
and none. Compression from standard input holds the demo, since it is read twice.

## Correctness

* Round trips (`examples/sweep.rs`): every demo of stratified (379) and sample8 (8), 987
  MB raw, compressed and decoded by both the whole-file decoder and the streaming
  `Decoder`: 774 round trips, 0 failures, byte-identical by SHA-256.
* The Go package (`mvdz_go/`) is behind this version: it wrote the same bytes as the Rust
  encoder for all 387 before the walk went on after an escape (FORMAT.md 4.5), and its
  decoder does not read a file with one.
* The test suite (`cargo nextest run`, 66 tests) with the sample8 demos and the FTE float
  demo in `MVDZ_TEST_DATA`: all pass. The golden vectors in
  `vectors/` are refused or decoded as `MANIFEST.tsv` records.
* Fuzzing, one hour per target, no failure, before the two-pass encoder and the escape
  rule of this version (not repeated since): Rust (`mvdz_rust/mvdz/fuzz/`) `decode`
  (78k runs, seeded with the vectors), `stream_reader` (4.5M), `compress_rt` (438k),
  `gen_rt` (290k); Go `FuzzDecode` (2.5M) and `FuzzEncode` (816k).

## Reproducing

```
# the demos: sample8 for speeds, stratified for ratios
scripts/fetch_samples.sh sample8 demos/sample8
scripts/fetch_samples.sh stratified demos/stratified

# library speeds over every *.mvd in a directory, 4 threads and 1, with criterion
MVDZ_BENCH_DEMOS=$PWD/demos/sample8 cargo bench --manifest-path mvdz_rust/Cargo.toml --bench codec

# command-line decode speed against gzip, xz and zstd (one process per file)
ls demos/sample8/*.mvd > demos/sample8.list
scripts/bench_cli.sh demos/cli-bench demos/sample8.list

# round trips: every demo through the whole-file and the streaming decoder
cargo run --manifest-path mvdz_rust/Cargo.toml --release --example sweep -- demos/stratified/*.mvd
```

For the confidence intervals, draw the sample at random from the whole collection
(stratify by era and size if the collection is skewed), weight each demo by how many it
stands for, and bootstrap within the strata.
