# Simplifying MVDZ

MVDZ is a proposal with no files in the wild, so there is no compatibility burden.

## Status (2026-11-20, evening)

Branch `feature/simplify`, 15 commits on `master` (rc.4, a9ac8fc), last commit 4e297d1.
The working tree is clean; this file is untracked. Viktor commits each change himself.

Done: the new container, DEFLATE in place of zstd, the 20 low-value stream models
dropped, attachments, metadata, index, trailer, streaming writer, levels, chunk-size
option, trusted mode and the SHA-256 pass on decode removed, one model path, fixed-width
container fields, five speed changes. See Decisions and Results per change.

Also done: one fallback (ad53e50; see "One fallback" under Results per change), and
measured at 1, 4 and 12 threads and in WebAssembly (see "Threads and WebAssembly").

Tests: 45 pass, 9 fail, all nine on the list under "Known failing tests".

Whole session against `master`: see "Whole session" under Results per change.

Next, in this order (nothing below is started):

1. The open questions that can still change the file format: 4, 9, 12, 13 (and 14, to
   specify).
2. The other open questions (5 to 7, 10, 11).
3. Finishing work, once the format no longer changes: regenerate the test vectors
   (clears 8 of the 9 failing tests), update the Go decoder (`interop/specdec`), rewrite
   FORMAT.md and README, update BENCHMARKS.md.

How changes are measured from the fallback change on: `mvdz bench -r 3 -j 4` on the 500
demos listed in `~/dev/mvdz-bench/rand500.txt` (1,786,252,992 bytes raw; drawn by
`scripts/pick.py` there from the 82,980 files of `~/dev/qwdemos`, names sorted, Python
`random.Random(20261120)`, all 500 MVD by `qworld file info`); results in
`~/dev/mvdz-bench/results/`. This sample is not the earlier one: its figures do not
compare with those above it. The earlier harness is lost (`/tmp/blobplan` did not survive
a reboot); it was:

`/tmp/blobplan/v2/measure_fast.sh <name>` (first 500 demos;
`ROUNDS=3 PROBE=tmp_probe4`) for each change, `/tmp/blobplan/v2/measure.sh <name>` (all
2,000) at milestones; results in `/tmp/blobplan/v2/<name>/`. The sample list is
`/tmp/blobplan/mvd2000.txt`. `/tmp` does not survive a reboot: the harness sources are
`bench2_a9.rs` and `bench3.rs` there, the probe `tmp_probe4.rs`.

## Goal

A format simple enough to stay unchanged for 10+ years.

- An MVD is a plain series of frames. It need not be a match, and may hold several
  matches or maps. The format does not model a match.
- Compress and decompress take no options. Parameters are tuned once and fixed.
- A decoder has as few paths as possible. Anything optional is frozen forever.
- The Rust code is an example of implementing the specification.

## Decisions

All implemented unless marked.

| Topic | Decision | Reason |
|---|---|---|
| Attachments (ATTACH, ADIR, bundle hash) | Removed | Out of scope for a demo codec |
| Media attachments proposal | Dropped | Same |
| META | Removed entirely | Match-shaped; derivable by decoding; belongs in a catalogue |
| INDEX | Removed | Duplicates the chunk headers; readers already scan them |
| Trailer | Removed | Only located INDEX, META and ADIR |
| Chunk size | Always 256K raw, cut at the next frame boundary | Measured best; one setting |
| Live QTV streams | Not compressed | 4-11 KB/s raw does not justify it |
| QWD demos | Not supported; MVD only | Needs a second frame walker and a second playerinfo model in every port; 75 of 2,200 archive files (3.4 %) |
| Streaming writer | Removed; finished `.mvd` only | One encoder path; shared models always available |
| Structural transform | Keep | Most of the compression (zstd -19 alone: 0.553 of raw) |
| rANS | Keep | Best ratio (0.218 of raw at 256K) |
| General-purpose compressor | Raw DEFLATE (RFC 1951) replaces zstd | +0.41 % size and +0.24 % decode time; zstd was 26 % of compression time; DEFLATE is in the Go, Python, Java and Node standard libraries and in browsers |
| Blob layout | One DEFLATE stream per chunk holds all blob streams | Blob decode 1.7 times faster than one stream each; one inflate call per chunk; 0.08 % larger |
| Low-value stream models | The 20 listed under Open questions lost their model; their bytes join the chunk's blob | Each model must match bit for bit in every port; together they were worth 0.078 % of the output |
| Fallback | One: a chunk the transform cannot code is one raw DEFLATE stream. A modelled stream is always rANS with the file's model; the per-stream back-end id, the store and DEFLATE back ends and the `stored` chunk coding are gone | Two chunk codings, one coder per stream kind; -0.056 % size |
| Model over the decision limit (rule M2) | The encoder makes it smaller (own contexts with the fewest symbols join the fallback table) instead of dropping it | Without this a stream with no model sends every chunk of its file to DEFLATE: one demo in 500 grew 3.6 times, the output 1.9 % |
| Thread count | The tool defaults to all logical cores, no cap, `-j` overrides; the library's default stays 1 and 0 means all logical cores (also in the C interface and `-j 0`) | The usual convention; a library should not take every core unasked; inside one file 12 threads give nothing over 4, so a cap would buy nothing |
| Built-in profiler | Removed, with the `sampler` feature and the optional `libc` dependency | `perf` does the same; 92 lines, all of them around `unsafe` blocks |
| Levels | One | No options |
| Model step size | One value, 2 | Trying 4 as well was worth 77 bytes in 1.68 GB |
| Model stored in a chunk | Removed; models only in the models block, also for one-chunk files | Costs 0.004 %; one model path |
| The next ten stream models | Keep | Dropping all ten costs 2.70 % |
| Container fields | Fixed width: 17-byte chunk header, 4-byte models length | Costs 0.005 %; no variable-length integers to decode there |
| Encoder self-check | On by default; the switch that turns it off stays | Every file written is proven to decode; it is a third of compression time, so experiments and benchmarks can skip it |
| Per-chunk checksum | Keep | Subsets of a demo are decoded for analytics |
| Checksum algorithm | The CRC-32 of zlib (73bd49b) | In every standard library that has DEFLATE; `crc32fast` was already in the build; 80 lines of hand-written CRC-32C gone, one `unsafe` function with them |
| Checks on decode | Both stay: each chunk's CRC-32 and the whole demo's (decided 2026-11-20) | The whole-demo check is the only one that catches chunks in the wrong order; the chunk check serves subset decodes and reports an error at the chunk |
| SHA-256 on decode | Never recomputed; the trusted mode, `ReadOptions` and the C `flags` argument are gone | 9.1 % of a 4-thread decode with SHA instructions, 19.7 % of a batch decode without; a decoder needs no SHA-256 |
| Size, duration, hashes | Fixed 64-byte header at the head | One read; completeness without an end record |
| Checksum and SHA-256 in the header | Both stay (decided 2026-11-20) | Cheap to write: for a 10 MB demo the checksum is 0.8 ms and SHA-256 4.5 ms (28 ms without SHA instructions), once, at compress time |
| Dependencies | `serde` and `serde_json` dropped from the library (JSON written by hand); `sha2` and `flate2` remain | 13 crates in the default build, from 24 |
| rayon | Not used; the 28-line `par_map` stays | No gain that repeats; 6 more crates; one parallel call costs 59 us |
| Speed changes | Models block read once; models built on all threads; symbols counted per stream in parallel; predictors run once | See "Speed changes" |
| Test vectors, Go decoder, FORMAT.md | Updated once, after the last format change (not done) | Each step would otherwise regenerate them |

Why the index could go: not because chunks are a fixed size (compressed offsets and demo
time cannot be computed from a chunk number), but because scanning the chunk headers is
cheap. A typical demo is about 10 chunks, a 4on4 a few dozen.

## The format as implemented (version 2)

```
file   := header | models | chunk*
header := "MVDZ" | version u32 | size u64 | duration_ms u64 | crc u64 | sha256[32]   (64 bytes)
models := len u32 | body[len]
chunk  := len u32 | raw_len u32 | dur_ms u32 | crc u32 | coding u8 | coded[len]      (17 bytes, then coded)
```

Every integer is little-endian and of fixed width. The code calls a chunk a segment.

| Offset | Bytes | Field | Content |
|---|---|---|---|
| 0 | 4 | magic | `MVDZ` |
| 4 | 4 | version | 2; a reader refuses any value it does not know |
| 8 | 8 | size | length of the original `.mvd` |
| 16 | 8 | duration_ms | total demo time: the sum of all frame time bytes |
| 24 | 8 | crc | CRC-32 (as in zlib) of the original `.mvd` in the low 4 bytes; the high 4 are 0 |
| 32 | 32 | sha256 | SHA-256 of the original `.mvd` |

Rules:

- Every header field describes the original demo, none describes the `.mvdz`.
- A chunk's raw offset and start time are the running sums of `raw_len` and `dur_ms`.
  The sums over all chunks equal the header's `size` and `duration_ms`.
- The file is complete when `raw_len` sums to `size`. Bytes after that are an error. An
  empty demo has no chunk.
- A full decode verifies each chunk's CRC-32 and frame time, and the whole demo's
  CRC-32, size and duration. A subset decode verifies the chunks it reads.
- SHA-256 is the demo's identity, computed at compress time. Decoding does not check
  it; a consumer that must authenticate it hashes the output.
- `models` holds one rANS model per modelled stream that some chunk codes with it
  (`len` 0: none). It is read once, when the file is opened, and built on all threads.
- Chunk `coding`: 0 transform, 1 the whole chunk as raw DEFLATE (the encoder's only
  fallback). Any other value is refused.
- A transformed chunk: protocol state, a stream table, the stream bodies, then one raw
  DEFLATE stream holding all blob streams in table order (no zlib or gzip wrapper, no
  preset dictionary; it decodes to exactly the sum of their byte counts and nothing
  follows its final block). A modelled stream is rANS with the file's model; its table
  entry is id, count, length (a blob stream's: id, count). A modelled stream without a
  model in the models block is an error. The stream table inside a chunk still uses
  variable-length integers.
- Each chunk decodes on its own, given the models block. Protocol state at the chunk
  start is in the chunk, so map changes decode as before.
- KTX demoinfo blocks stay in their chunk; the shared demoinfo store is gone.
- Limits: a chunk is at most 0xFFFF0000 raw bytes and decodes to at most 1024 times its
  coded length plus 4096; the models block is at most 16 MiB.

What consumers do without META:

- Size, duration, identity: the 64-byte header.
- Map, serverinfo, initial players, start time: decode the first chunk.
- Final scores, KTX demoinfo: decode the last chunk.
- Anything else: full decode. Caller-supplied keys (e.g. `hub_game_id`) live in the
  catalogue, not in the file.
- A chunk gives exact bytes, not full game state: baselines, userinfo and delta-coded
  entities depend on earlier frames.
- `duration_ms` is demo time, which stands still while the server is paused.

## Open questions

All Viktor's to decide. Of those left, 4, 9, 12 and 13 can change the file format.

1. Fallback paths: decided and implemented, see "One fallback".
2. The whole-demo CRC pass: decided, it stays (see Decisions).
3. Checksum algorithm: decided, the CRC-32 of zlib (see "Checksum: CRC-32").
4. The stream table inside a chunk: fixed width too? Not measured.
5. The C interface: keep it here, move it to its own crate, or remove it. 659 lines and
   60 of the 85 `unsafe` occurrences; whether any engine links it is not known.
6. Encoder self-check default: on (as now) or off like `flac -V`. Recommended: on.
7. Gzip input (`.mvd.gz`) accepted by compress?
8. Thread count: decided and implemented (see Decisions).
9. Bits that can be flipped without changing the decoded demo: 784 of 30,696 in the
   probe's small file (256 are the header's SHA-256, 80 in the models block, 448 in the
   chunks); never with a different demo. Accept, or require every flip to be refused.
10. Optional features: the built-in profiler is removed (`sampler.rs`, the `sampler`
    feature and the optional `libc` dependency; `perf` does its job). Left: `mimalloc`,
    which the static musl release builds use.
11. Compression speed further: the walk is single-threaded (17.7 % of compression before
    the speed changes; not profiled since). Peak memory rose with changes 4 and 5.
12. `pi.ar` (-0.002 %) is low value, but it is the third axis of the angle predictor, so
    dropping it removes no code. `uc` and `uc.a` are not measured (none of the 2,000
    demos has usercmd blocks).
13. `te.f` and `snd.f` (temp-entity and sound origins with float coordinates) are blobs
    that no general-purpose coder compresses: 2.4 % of the output, all of it in the 457
    of 2,000 demos with float coordinates. Estimated gain from modelling them: 0.31 % of
    the output, for two more streams to specify. Recommended: leave them as blobs.
14. Edge inputs to specify: a demo ending in a partial frame; input that is not an MVD.

The 20 streams that lost their model: `snd.misc`, `pi.model`, `pi.eff`, `pi.skin`,
`pr.level`, `esclen`, `mask`, `pe.hdr`, `pe.misc`, `pe.frame`, `pe.model`, `te.misc`,
`pi.num`, `frags`, `stl.idx`, `stl.val`, `hid.tag`, `hid.len`, `hid.type`, `hid.dmg`.
The stat-long predictor went with `stl.val`.

## Baseline

rc.4 unmodified (commit a9ac8fc, release build, glibc allocator), the 2,000-demo sample
(6,910,564,804 bytes raw), normal level, 256K chunks, encoder self-check and every
decode check on. AMD Ryzen 5 5600X (6 cores, 12 threads), 4 threads. Median [min-max]
of 3 rounds, MB/s of raw demo. Every change is measured against this.

| Measure | Baseline |
|---|---|
| Output | 1,676,084,713 bytes, 0.2425 of raw |
| Round trip | 2,000 of 2,000 |
| Compress, one file at a time, 4 threads | 136.0 s, 50.8 [50.1-51.1] MB/s |
| Compress, files in parallel on 4 threads | 76.3 s, 90.6 [90.4-92.8] MB/s |
| Decompress, one file at a time, 4 threads | 35.9 s, 192.4 [191.7-198.0] MB/s |
| Decompress, files in parallel on 4 threads | 20.9 s, 330.9 [328.6-331.7] MB/s |

At rc.4's default whole-file setting the output is 1,617,122,442 bytes (0.2340); 256K
chunks cost 3.65 %.

## Results per change

Same sample, settings, machine and harness as the baseline. Speeds are the median of 3
rounds in MB/s of raw demo; "4t" is one file at a time with 4 threads, "batch" is files
in parallel on 4 threads. Each column includes the changes before it.

| Measure | Baseline | A1: DEFLATE, one blob per chunk (85cb9a4) | A2: 20 stream models dropped (0e83d14) | A3: attachments removed (9da9a0a) |
|---|---|---|---|---|
| Output, bytes | 1,676,084,713 | 1,682,457,715 (+0.38 %) | 1,682,973,334 (+0.41 %) | 1,682,973,334 (+0.41 %) |
| Ratio | 0.2425 | 0.2435 | 0.2435 | 0.2435 |
| Round trip | 2,000 of 2,000 | 2,000 of 2,000 | 2,000 of 2,000 | 2,000 of 2,000 |
| Compress 4t | 50.8 | 59.4 (+16.9 %) | 60.9 (+19.9 %) | 61.1 (+20.3 %) |
| Compress batch | 90.6 | 118.9 (+31.2 %) | 119.4 (+31.8 %) | 121.3 (+33.9 %) |
| Decompress 4t | 192.4 | 197.0 (+2.4 %) | 197.7 (+2.8 %) | 196.1 (+1.9 %) |
| Decompress batch | 330.9 | 330.9 (0.0 %) | 330.3 (-0.2 %) | 331.8 (+0.3 %) |
| Output at the whole-file setting | 1,617,122,442 | 1,630,656,576 (+0.84 %) | 1,634,224,656 (+1.06 %) | 1,634,224,656 (+1.06 %) |

Percentages are against the baseline. A3 is cdd4a50 plus the build fix 9da9a0a; cdd4a50
alone does not compile.

### New harness (from the container change on)

Size and round trip on all 2,000 demos; speeds on the first 500 of them (1,813,240,608
bytes raw), 4 threads, median of 3 rounds, MB/s of raw demo. About 5 minutes a run. Not
comparable with the speeds in the table above, which time all 2,000 demos.

| Measure | rc.4 | A4: writer removed (6cb7849) | A5+A6: new container (7e9efcb) | A9: one level, 256K (a464a29) |
|---|---|---|---|---|
| Output, bytes | 1,676,084,713 | 1,682,973,334 (+0.41 %) | 1,677,158,353 (+0.06 %) | 1,677,158,353 (+0.06 %) |
| Round trip | 2,000 of 2,000 | 2,000 of 2,000 | 2,000 of 2,000 | 2,000 of 2,000 |
| Compress 4t | 49.6 [48.8-49.8] | 62.5 [62.1-62.6] | 66.1 [64.5-66.9] | 66.1 [65.9-66.9] |
| Compress batch | 88.4 [87.7-91.3] | 121.9 [119.7-122.1] | 125.5 [120.5-127.1] | 124.8 [121.8-126.6] |
| Decompress 4t | 195.1 [194.8-200.6] | 203.6 [200.5-204.4] | 200.9 [195.0-202.1] | 199.6 [196.7-201.1] |
| Decompress batch | 321.9 [320.8-331.4] | 334.0 [332.1-335.0] | 322.7 [317.5-328.3] | 326.4 [325.4-332.9] |

| Measure | Step 1: no model in a chunk | Steps 1 + 2: one step size (5c03bf6) | Step 3: fixed-width fields (4a20f89) |
|---|---|---|---|
| Output, bytes | 1,677,219,823 (+0.068 %) | 1,677,220,481 (+0.068 %) | 1,677,306,737 (+0.073 %) |
| Round trip | 2,000 of 2,000 | 2,000 of 2,000 | 2,000 of 2,000 |
| Compress 4t | 65.6 | 66.2 | 66.1 [65.1-66.5] |
| Compress batch | 123.5 | 124.7 | 125.8 [122.9-127.2] |
| Decompress 4t | 201.9 | 196.7 | 201.4 [200.9-203.1] |
| Decompress batch | 331.2 | 322.0 | 322.3 [314.9-326.5] |

Step 3 adds 86,256 bytes to steps 1 + 2 (+0.005 %). Sizes in percent are against rc.4.

From the next change on, a change is measured on the first 500 demos with one round (about 2
minutes): the size change against rc.4 on those 500 agrees with all 2,000 within 0.014
points (A4 +0.411 % on both; container +0.078 % against +0.064 %), rounds within a run
spread 1 to 5 %, and medians of three for identical code differ by about 3 % between
runs. The full 2,000 demos with three rounds are run at milestones.

A3 (9da9a0a) and A4 on the old harness: identical in size to A2 and within the spread
in speed. The A5+A6 run was disturbed by other commands on the machine.

Share of a full decode spent on checksums (A4, the 500 demos, batch on 4 threads, full
decode 5.40 s): CRC-32C with the CPU instruction 0.88 % a pass, 1.75 % for the two
passes a decode makes; with the portable table 3.5 % a pass, 7.0 % for two; SHA-256
3.8 % a pass (2.9 % measured as decode minus trusted decode). This CPU has SHA
instructions; BENCHMARKS.md's 20 % was measured on one without.

With SHA-256 forced to its portable code (`sha2` feature `force-soft`, same harness and
decoder): one pass 1.272 s, 19.7 % of a full decode of 6.47 s; decode minus trusted
decode 17.9 %. A decode with the check is 280 MB/s, without it 341 MB/s.

### Where compression time goes (4a20f89)

The 500 demos, one file at a time, wall clock from timers in a scratch copy
(`/tmp/blobplan/prof_stage`, results in `/tmp/blobplan/v2/prof/`).

| Stage | 4 threads, s | Share | 1 thread, s | Share | Parallel |
|---|---|---|---|---|---|
| Cut | 0.12 | 0.4 % | 0.12 | 0.2 % | no |
| Walk | 4.80 | 17.7 % | 4.73 | 8.8 % | no |
| Hash (CRC-32C, SHA-256) | 0.94 | 3.5 % | 0.94 | 1.8 % | no |
| Train: numeric predictors | 1.77 | 6.6 % | 4.64 | 8.6 % | within a chunk |
| Train: count symbols | 4.95 | 18.3 % | 4.78 | 8.9 % | no |
| Train: build models | 0.99 | 3.7 % | 3.31 | 6.2 % | yes |
| Encode and assemble | 4.01 | 14.8 % | 13.90 | 25.9 % | across chunks |
| Self-check | 9.25 | 34.2 % | 21.05 | 39.2 % | yes |
| Total | 27.06 | | 53.65 | | |

- 4 threads are 1.98 times faster than 1. The stages that run on one thread (cut, walk,
  hash, count symbols) are 39.9 % of the time at 4 threads; `perf stat` on the first 200
  demos: 2.2 of 4 cores busy on average (23.3 s of CPU in 10.6 s).
- The numeric predictors run twice: in training and again in encoding (encoding: 4.87 s
  summed over threads; rANS of structural streams 2.51 s, of numeric streams 4.07 s,
  DEFLATE of the blobs 2.87 s).
- rayon instead of the scoped-thread `par_map` (scratch copy, two alternating runs of
  three rounds): compress 4t 68.5 and 67.2 MB/s against 64.7 and 66.5; the other three
  measures inside the spread of the current code; under `perf stat` 65.4 against 65.8.
  No gain that repeats. It adds 6 crates (13 to 19) and triples the context switches
  (26,824 against 9,199).

### Where decompression time goes (4a20f89)

Same scratch copy and demos, one file at a time on 4 threads, three passes: 27.47 s
(198.0 MB/s); on 1 thread 63.02 s (86.3 MB/s). Seconds for the three passes.

| Stage | 4 threads, s | Share | 1 thread, s | Share | Parallel |
|---|---|---|---|---|---|
| Scan layout (header, chunk list, first parse of the models block) | 3.01 | 11.0 % | 2.87 | 4.6 % | no |
| Allocate output | 0.27 | 1.0 % | 0.27 | 0.4 % | no |
| Build models (second parse; the other threads wait) | 6.13 | 22.3 % | 5.59 | 8.9 % | no |
| Decode chunks, rest | 15.07 | 54.9 % | 51.46 | 81.6 % | across chunks |
| Whole-demo CRC-32C | 0.48 | 1.7 % | 0.45 | 0.7 % | no |
| Whole-demo SHA-256 | 2.49 | 9.1 % | 2.37 | 3.8 % | no |

Inside "decode chunks", summed over threads at 4 threads: walk (structural rANS and
writing the bytes) 25.05 s, numeric groups wave 1 27.64 s, wave 2 0.89 s, inflate of the
blobs 1.13 s, chunk CRC-32C 0.46 s, frame scan 0.22 s.

- 45.1 % of the time at 4 threads runs on one thread. The models block is parsed twice
  (`FileTables::new` to check it, `models()` to build): together 33.3 %.
- SHA-256 is 9.1 % of a 4-thread decode on a CPU with SHA instructions, because it runs
  on one thread after the parallel part (2.9 % in batch, one thread a file).

### Speed changes (decided 2026-11-20: all five)

1. Parse the models block once. 2. No SHA-256 pass on decode (removes the trusted
mode). 3. Build the models on all threads. 4. Count symbols in parallel across chunks.
5. Run the numeric predictors once. One at a time, each measured on the first 500 demos
with three rounds (MB/s, median [min-max]); output bytes on those 500: 439,724,555.

| Measure | Before (4a20f89, two runs) | 1: models read once (76b5653) | 2: no SHA-256 on decode (1ccd654) | 3: models built on all threads (a72af2e) | 4: symbols counted in parallel (649932a) | 5: predictors run once |
|---|---|---|---|---|---|---|
| Output, bytes (500 demos) | 439,724,555 | 439,724,555 | 439,724,555 | 439,724,555 | 439,724,555 | 439,724,555 |
| Round trip | 500 of 500 | 500 of 500 | 500 of 500 | 500 of 500 | 500 of 500 | 500 of 500 |
| Compress 4t | 64.7, 66.5 | 69.3 [69.3-70.4] | 70.3 [70.3-70.3] | 75.0 [74.9-76.0] | 91.3 [91.2-92.6] | 96.2 [95.8-97.0] |
| Compress batch | 123.2, 128.3 | 126.5 [125.9-130.4] | 127.2 [126.8-127.5] | 127.8 [126.8-128.5] | 130.2 [129.6-130.8] | 139.2 [135.8-141.9] |
| Decompress 4t | 199.2, 203.6 | 227.1 [226.9-227.7] | 250.4 [249.6-251.4] | 313.5 [313.2-313.6] | 315.0 [311.1-315.3] | 310.6 [304.9-310.8] |
| Decompress batch | 323.1, 333.6 | 340.8 [340.7-343.2] | 353.0 [352.3-353.2] | 353.8 [352.6-357.6] | 358.8 [350.4-359.5] | 341.8 [336.9-351.9] |

Change 1: the models are built when the file is opened (no check-only pass, no lazy
build). Reading the file information now builds them too: 3.87 ms a file against 1.93
(500 files), and a models block too large for the memory limit is refused at open.
Bit-flip probe unchanged (528 accepted with identical output).

Change 2: decoding checks the CRC-32C of every chunk and of the whole demo, the size and
the timing; the header's SHA-256 is not recomputed. Removed with it: the `trusted`
option, the `ReadOptions` type (decoding takes `Limits`), `--trusted`, `MVDZ_TRUSTED` and
the `flags` argument of the three C decoding calls. Bit-flip probe: the 256 SHA-256 bits
of the header are now accepted, 784 accepted in all (identical output), 0 with different
output.

Change 3: the models block's entries are listed on one thread, then each model is read
and built by `par_map` (the models do not depend on each other). While building, each
thread holds one model's temporary tables, so a memory limit within a few of those of the
models' total can refuse a file at 4 threads that passes at 1; not measured how close
that is for real files.

Change 4: training computes the numeric values of every chunk first (chunks in
parallel) and then counts each stream's symbols and builds its model in one parallel
pass over the streams (a stream's counts do not depend on another stream's, and each
stream still sees the chunks in order, so the models are the same). The numeric values
of the whole file are held during training: peak memory of `mvdz compress` on the
largest of the 2,000 demos (18.1 MB) 143.1 MB against 119.9 MB at 4 threads, 140.6
against 110.9 MB at 1 thread; on a median demo (1.3 MB) 18.2 against 19.5 MB.

Change 5: the predictors run once, after the walk (chunks in parallel); their values are
kept with each chunk's plan, the models are trained on them and the chunks coded from
them. The decoder is not touched by this change: its two figures in this column are
run-to-run variation. Peak memory on the largest demo 168.2 MB at 4 threads and 165.3 MB
at 1 (the values now live until the file is written); 18.1 MB on the median demo.

### One fallback (2026-11-20, ad53e50)

`mvdz bench -r 3 -j 4` on the 500 demos of `~/dev/mvdz-bench/rand500.txt`, self-check
on, MB/s median [min-max]. The machine was not idle (load average 3 to 5).

| Measure | Before (3857a1c) | One fallback, model dropped at the limit | One fallback, model made smaller (final) |
|---|---|---|---|
| Output, bytes | 341,362,438 | 347,875,709 (+1.91 %) | 341,169,945 (-0.056 %) |
| Round trip | 500 of 500 | 500 of 500 | 500 of 500 |
| Chunks coded as DEFLATE | 0 | 56 | 0 |
| Compress 1t | 36.6 [36.5-37.2] | 35.3 [34.6-35.8] | 36.6 [36.5-36.6] |
| Compress 4t | 94.5 [93.0-94.8] | 93.0 [89.2-93.7] | 93.0 [92.7-94.6] |
| Compress batch | 141.5 [138.7-142.1] | 137.0 [131.6-140.3] | 141.8 [138.7-142.5] |
| Decompress 1t | 87.4 [87.4-89.0] | 85.4 [85.1-85.5] | 86.8 [83.5-87.6] |
| Decompress 4t | 303.6 [289.5-303.9] | 291.2 [282.3-304.8] | 299.3 [298.0-304.2] |
| Decompress batch | 336.2 [333.6-343.2] | 332.6 [325.3-338.3] | 342.2 [323.8-344.6] |

- What changed in the format: no back-end id in a stream table entry; chunk coding 2
  (stored) is gone. In the code: `LazyDec`, `NumDec`, the two decoder traits, the store
  and DEFLATE stream coders and the back-end search in the encoder are gone.
- The streams without a model are identified: in this sample it is `time` in one demo
  (file 422 of the list, 14.5 MB, 56 chunks). Its model broke the decision limit (rule
  M2: a header may take at most 128 decisions a byte plus 1,024), so it was dropped and
  `time` was coded with DEFLATE in every chunk. With one fallback that sends all 56
  chunks to DEFLATE: the file goes from 2,593,286 to 9,281,592 bytes.
- The fix is in the encoder only (`Tally::shared`): a model over the limit loses the
  eighth of its own contexts with the fewest symbols, again until it holds. That file is
  then 2,575,828 bytes, smaller than before the change. A stream gets no model only if
  its fallback table alone breaks the limit or the models block would pass 16 MiB; its
  chunks are then DEFLATE. Neither happened in the 500 demos.
- A fallback chunk whose DEFLATE stream would break the ratio limit (1024 times) is
  written as stored DEFLATE blocks.
- Every file is smaller, by the back-end byte of each stream entry: 192,493 bytes in
  all.
- Vectors: `tests/vectors.rs` builds its hand-made chunks with rANS streams now;
  `random.stored.mvdz` is renamed `random.deflate.mvdz`, the `segment-coding` vector
  uses coding 2. The files in `tests/vectors/` are not regenerated.
- Not done: the Go decoder and FORMAT.md (with the other finishing work).

### Threads and WebAssembly (2026-11-20, ad53e50)

The 500 demos of `~/dev/mvdz-bench/rand500.txt`, `mvdz bench -r 3`, self-check on, MB/s
median [min-max]. Results in `~/dev/mvdz-bench/results/ad53e50.*`.

| Measure | 1 thread | 4 threads | 12 threads |
|---|---|---|---|
| Compress, one file at a time | 36.2 [35.9-36.7] | 91.3 [90.0-94.8] | 98.7 [97.9-100.5] |
| Compress, batch | as 1 thread | 137.4 [133.4-139.2] | 241.0 [233.6-242.4] |
| Decompress, one file at a time | 84.9 [83.7-88.0] | 289.2 [286.7-305.8] | 277.4 [274.5-281.1] |
| Decompress, batch | as 1 thread | 338.0 [322.1-342.8] | 612.0 [583.1-620.6] |

- Inside one file 12 threads give little over 4: compress +8 %, decompress -4 %. In
  batch they give 6.7 times one thread for compress and 7.2 times for decompress (6
  cores, 12 threads).
- The 4-thread run started with the load average at 5 (the 12-thread run had just
  ended); the 12-thread run started at 0.8.

WebAssembly: `wasm32-unknown-unknown`, one thread, node 26.13, the same 500 demos, 3
rounds. A wrapper crate (`~/dev/mvdz-bench/wasm`, `bench.mjs` there) exports decode and
encode; release, fat LTO, `panic = "abort"`, stripped.

| Measure | No SIMD | SIMD (`+simd128`) | Native, 1 thread |
|---|---|---|---|
| Decode | 54.3 [53.8-54.5] | 73.5 [72.9-73.5] | 84.9 |
| Encode, self-check on | 23.7 [23.6-24.0] | 27.2 [26.7-27.5] | 36.2 |
| Encode, self-check off | 43.9 [43.4-44.2] | 44.4 [44.3-44.4] | not measured |
| Module, bytes | 435,890 | 442,577 | |
| Module, gzip -9 | 160,204 | 163,522 | |

- All 500 decode to the original bytes; the files the WebAssembly encoder writes are
  identical to the native ones (341,169,945 bytes).
- The module is the whole library: the exported functions of the C interface keep the
  encoder in. Without the encode export it is 662 bytes smaller. A decoder-only size
  needs the C interface behind a feature (open question 5).
- One checksum pass as a share of a decode (speeds from "Checksum candidates"), no SIMD
  and SIMD: CRC-32C table 2.4 % and 3.2 %, CRC-32 of zlib 2.3 % and 3.1 %, XXH64 0.32 %
  and 0.43 %, XXH3-64 0.38 % and 0.35 %. A decode makes two passes (each chunk, the
  whole demo): about 5 to 6 % with either CRC.
- The first decode rounds without SIMD started with the load average at 10; their
  spread is 1 %.

### Checksum: CRC-32 (2026-11-20, 73bd49b)

The 500 demos, `mvdz bench -r 3 -j 4`, MB/s median [min-max]; WebAssembly as under
"Threads and WebAssembly". Results in `~/dev/mvdz-bench/results/crc32.txt`.

| Measure | CRC-32C (ad53e50) | CRC-32 of zlib (73bd49b) |
|---|---|---|
| Output, bytes | 341,169,945 | 341,169,945 |
| Round trip | 500 of 500 | 500 of 500 |
| Compress 1t | 36.2 [35.9-36.7] | 36.3 [36.1-37.1] |
| Compress 4t | 91.3 [90.0-94.8] | 93.1 [92.3-97.0] |
| Compress batch | 137.4 [133.4-139.2] | 140.2 [138.6-140.6] |
| Decompress 1t | 84.9 [83.7-88.0] | 87.7 [86.5-88.0] |
| Decompress 4t | 289.2 [286.7-305.8] | 312.2 [296.4-313.1] |
| Decompress batch | 338.0 [322.1-342.8] | 343.0 [318.0-347.1] |
| WebAssembly decode, no SIMD | 54.3 [53.8-54.5] | 56.0 [55.1-56.0] |
| WebAssembly decode, SIMD | 73.5 [72.9-73.5] | 74.5 [74.4-74.9] |
| WebAssembly module, no SIMD, bytes | 435,890 | 434,258 |

- The differences are inside the spread between runs except Decompress 4t (+8 %), whose
  earlier run started on a loaded machine. No measure is slower.
- `mvdz info` writes the key `crc32` (was `crc32c`). Tests: 44 pass, 9 fail, the same
  nine. README still says CRC-32C in two places (with the README rewrite).

### Checksum candidates (2026-11-20)

One thread, whole demos, best of 5 passes, MB/s. Native: the first 500 demos, Rust
crates built for plain x86_64 (`/tmp/blobplan/hashbench`). WebAssembly: the first 150
demos in node 26 (`/tmp/blobplan/hashwasm`), without and with SIMD.

| Checksum | Native | WebAssembly | WebAssembly, SIMD |
|---|---|---|---|
| CRC-32C, CPU instruction (mvdz) | 11,914 | - | - |
| CRC-32C, table (mvdz) | 2,484 | 2,277 | 2,279 |
| CRC-32 of zlib (crc32fast) | 25,938; portable code 5,468 | 2,336 | 2,332 |
| Adler-32 (adler2) | 13,624 | 4,043 | 13,435 |
| XXH32 | 9,023 | 9,026 | 8,995 |
| XXH64 | 17,152 | 17,104 | 17,139 |
| XXH3-64 | 22,716 (SSE2) | 14,127 | 20,819 |
| SHA-256 (sha2) | - | 375 | 374 |

One thread decodes 89.7 MB/s natively (batch, 358.8 MB/s on 4 threads), so one pass is:
CRC-32C instruction 0.75 %, CRC-32C table 3.6 %, XXH64 0.52 %, XXH3-64 0.39 %. Decode
speed in WebAssembly is not measured. Checked on this machine: Node has `zlib.crc32` and
no CRC-32C, Adler-32 or xxHash; Python has `zlib.crc32` and `zlib.adler32` and no CRC-32C
or xxHash; Go has `hash/crc32`, `hash/crc64`, `hash/adler32` and no xxHash. In the
`xxhash-rust` crate XXH64 is 300 lines and XXH3 1,790 (with its 128-bit variant and SIMD
paths).

### Open questions measured (A9, a464a29)

All 2,000 demos, 4 threads; every variant round-trips every demo.

- In use today: 27,301 chunks, none fell back; no modelled stream on the store or
  DEFLATE fallback; 674,330 modelled streams with a shared model, 367 with a model in
  the chunk (150 in the 6 one-chunk files, 217 structural streams without a shared
  model in other files; which stream is not identified). Models blocks: 33.7 MB,
  2.0 % of the output.
- Model in a chunk at step 2 only (no step 4): +77 bytes in total.
- Shared models also for one-chunk files: +162 bytes in total (+0.056 % on those 6
  files).
- No model in a chunk at all (back end 2 gone; the 217 streams go to store/DEFLATE):
  +61,470 bytes, +0.004 %.
- The next ten stream models, their bytes joining the chunk's blob: all ten dropped
  +2.70 % (the earlier +1.91 % was with separate DEFLATE streams). Alone: `te.type`
  +0.10 %, `pe.more` +0.13 %, `pi.wframe` +0.20 %, `pe.ent` +0.20 %, `st.idx` +0.21 %,
  `pe.a` +0.25 %, `st.val` +0.37 %, `type` +0.40 %, `snd.num` +0.43 %, `snd.ch`
  +0.45 %.
- Chunk header: `len`, `raw_len` and `dur_ms` average 8.96 bytes a chunk as varints;
  as three u32 they are 12: +82,986 bytes, +0.005 %.
- Encoder self-check (500 demos): compression with it 127.5 MB/s in batch and
  67.2 MB/s one file at a time; without it 196.4 and 101.5. It is 35 % of
  compression time.
- Not measured: `uc` and `uc.a` (no demo has usercmd blocks).

### Known failing tests

Rule: between commits, no test may fail that is not on this list. On rc.4, 79 pass and
1 fails. After A9: 44 pass, 9 fail. Since 1ccd654: 43 pass, 9 fail (the test of unknown C
flags went with the flags argument). With one fallback: 44 pass, 9 fail (one new test,
of a model made smaller at the decision limit). Tests of removed features were deleted with them
(attachments, the zstd frame rules, the streaming writer, metadata, census, extension
keys, unknown sections, demoinfo sharing).

| Tests | Passed on rc.4 | Why they fail | Resolved by |
|---|---|---|---|
| `c_compress_gzip`, `closed_stdout_is_quiet`, `gunzip_limit`, `gzip_input_is_the_raw_demo`, `gzip_output`, `non_demos_are_refused`, `outputs_named_after_inputs` (cli), `vectors_decode` | yes | read the vectors in `tests/vectors/`, written by rc.4 in the old format | regenerating the vectors after the last format change |
| `c_example_builds_and_runs` | no | fails on unmodified rc.4 | outside this work |

Off the list since: `r_m2_padded_model` and `trusted_mode` (rewritten for the new
container), `ratio_limit_fallback` (passes since the fast level went), `c_handles`
(since A2).

Bits of one small test file that can be flipped without changing the decoded demo
(never with a different demo):

| Where | rc.4 | A1 | A2 |
|---|---|---|---|
| File size, bytes | 5,598 | 5,091 | 4,782 |
| Prefix (128 bits) | 25 | 25 | 25 |
| META | 19 | 2 | 2 |
| Models block (TABLES) | 104 of 776 bits | 104 of 776 | 80 of 680 |
| Chunks (SEGMENT) | 522 | 452 | 448 |
| Total | 670 of 44,784 | 583 of 40,728 | 555 of 38,256 |

A3 and A4: identical to A2 (555 of 38,256). New container (A5+A6) and A9: 528 of 29,648 on a 3,706-byte file; none of the header's 512 bits, 80 of 616 in the models block, 448 of 28,520 in the chunks.

## Measurements

Speeds: four threads (`-j 4`) is the primary figure, one thread the secondary one, as
in BENCHMARKS.md. Sizes do not depend on the thread count. Steam
Hardware Survey, October 2026, physical cores per PC: under 4 cores 2.9 %, 4 cores
12.4 %, 6 cores 27.3 %, 8 cores 28.7 %, more than 8 cores 28.5 %.

### Done: blob coder

Sample: 2,000 MVDs from `demos.quake.world` (seeded shuffle of its 14,028 files; format
checked with `qworld file info`), 6.91 GB raw, 27,301 chunks. rc.4 at 256K chunks:
1,676.1 MB, 0.2425 of raw; none refused, no chunk fell back. Blobs are 604.5 MB raw
(text 457.7 MB), 8.33 blob streams per chunk, 7.5 % of the output. Release build,
4 threads. Blob coders were run on the blob streams alone, not in a rebuilt codec.

Size:

| Blob coder | Blob bytes | Change in file size |
|---|---|---|
| zstd 17 per stream (rc.4) | 125.67 MB | baseline |
| Raw DEFLATE 9 per stream | 131.20 MB | +0.33 % |
| Raw DEFLATE 9, one blob per chunk | 132.47 MB | +0.41 % |
| Raw DEFLATE 9, two blobs per chunk (text, the rest) | 129.72 MB | +0.24 % |
| zstd 17 with a per-file shared dictionary | 109.61 MB | -0.96 % |
| zstd 17 over the whole file (bound for cross-chunk sharing) | 103.84 MB | -1.30 % |
| Adaptive order-2 byte-context model | 188.03 MB | +3.72 % |
| Static order-1 rANS with model cost | 242.50 MB | +6.97 % |
| Stored | 604.49 MB | +28.57 % |

Speed, files in parallel on 4 threads (one thread in brackets). The whole rc.4 codec
on the same sample (Baseline): compress 76.3 s, decompress 20.9 s.

| Blob coder | Blob bytes | Encode | Decode |
|---|---|---|---|
| zstd 17 per stream (rc.4) | 125.67 MB | 19.78 s | 0.153 s (0.586 s) |
| DEFLATE 1 per stream | 153.15 MB | 0.78 s | 0.482 s (1.891 s) |
| DEFLATE 1 one blob per chunk | 157.24 MB | 0.45 s | 0.255 s (1.003 s) |
| DEFLATE 6 per stream | 131.33 MB | 1.85 s | 0.449 s (1.746 s) |
| DEFLATE 6 one blob per chunk | 132.61 MB | 1.65 s | 0.218 s (0.850 s) |
| DEFLATE 9 per stream | 131.20 MB | 2.04 s | 0.451 s (1.740 s) |
| DEFLATE 9 one blob per chunk | 132.47 MB | 2.09 s | 0.220 s (0.857 s) |

- zstd 17 on the blobs is 25.9 % of rc.4's compression time; DEFLATE 9 is 2.7 %.
- The decode column creates a decoder and buffer per stream. With one decoder and
  buffer reused: per stream 0.344 s (1.346 s), one blob per chunk 0.204 s (0.782 s).
  Against zstd that is +0.91 % and +0.24 % of the whole decode.
- Chunks per file: median 6, maximum 69; 66 files (3.3 %) have fewer than 4.
- Fallbacks: none of the 1,082,936 structural and numeric streams used zstd or store.
  444 streams carried their model in the chunk.
- No `store` path for blobs: 1.7 KB in 3.46 MB of blob output (first 60 files).
- Size of what a decoder carries (source lines, `wc -l`): Go standard library, inflate
  1,015 against `internal/zstd` 2,277; Rust, miniz_oxide inflate 3,404 against the
  ruzstd decoder 7,141. Specifications: RFC 1951 4,715 words, RFC 8878 12,917.
- Platform support, checked 2026-11-20: `DecompressionStream("deflate-raw")` since
  Chrome 103, Firefox 113, Safari 16.4 (caniuse). Raw DEFLATE in the standard library
  of Go (`compress/flate`), Python (`zlib`, negative `wbits`), Java (`Inflater(true)`)
  and Node (`zlib`). zstd: no public Go package, none in `java.util.zip` (JDK 25),
  Python from 3.14, Node from 22.15 (experimental); MDN lists `"zstd"` for
  `DecompressionStream`, browser versions not checked.
- Largest blob streams after text (52.3 MB with zstd): `te.f` 23.6 MB, `snd.f`
  16.2 MB, `gen.19` 12.1 MB, `ping` 5.1 MB, `nails` 3.8 MB.
- The first 1,000 of these demos gave the same figures within 0.02 points; the 379
  stratified demos on this machine gave DEFLATE +0.33 %.
- QWD, for the record: all 75 files `qworld` reports as QWD are refused by
  `mvdz compress`. 8 are plain QWD (75.4 MB; 0.288 of raw with `--allow-non-demo`,
  gzip 0.327); 67 are QWZ, already compressed (0.994).

DEFLATE figures are miniz_oxide through `flate2`. The wasm build is not measured. The
experiment code is not in the repository.

### Done: what each stream model is worth

Same 2,000 demos. Each modelled stream coded with rANS and the file's shared model
(body plus model) against the same bytes as a raw DEFLATE blob per chunk. Per-stream
estimates, not a rebuilt codec. Change in file size if the model is dropped:

| Streams | Each | Together |
|---|---|---|
| `snd.misc`, `pi.model`, `pi.eff`, `hid.tag`, `pi.skin`, `pi.ar`, `pr.level`, `hid.type`, `esclen` | below 0 | -0.07 % |
| `mask`, `hid.len`, `pe.hdr`, `pe.misc`, `stl.val`, `frags`, `pe.frame`, `te.misc` | up to 0.014 % | +0.06 % |
| `pi.num`, `stl.idx`, `pe.model`, `hid.dmg` | 0.03 to 0.05 % | +0.14 % |
| `te.type`, `pe.a`, `pe.ent`, `pe.more`, `st.idx`, `pi.wframe` | 0.08 to 0.16 % | +0.71 % |
| `type`, `snd.num`, `st.val`, `snd.ch` | 0.23 to 0.37 % | +1.20 % |
| the other 21 (origins, angles, `cmd`, `time`, `pi.flags`, `pi.frame`, `pe.bits`, float origins) | 0.5 to 17.4 % | +94.9 % |

- The first row loses to DEFLATE on fixed cost: a rANS stream with a shared model is
  17 bytes for 1 to 16 symbols (measured on `pr.level` and `frags`); DEFLATE is 3
  bytes for 1 symbol and 9 to 18 for 16. The stream's model in the models block
  comes on top.
- Code behind the first three rows: 13 of the 21 streams are referenced once in
  `walk.rs`; `pe.misc` 7 times, `te.misc` and `hid.tag` 4. The stat-long predictor
  (`stl.val`) is one 13-line function; the hidden-block parser is 60 lines.
- With the chosen layout (one DEFLATE stream per chunk): the 20 low-value streams
  cost 17.21 MB with their rANS models; with their bytes joined to the chunk's blob
  the blob grows from 132.47 to 150.99 MB. Dropping the 20 models costs 1.32 MB,
  +0.078 % of the output (as separate DEFLATE streams: +0.130 %).
- `uc` and `uc.a` (usercmd blocks) do not occur in the sample: not measured.
- Float origins (`pi.f*`, `pe.f*`) occur in 457 demos and are worth 7.5 % of the whole
  sample's output.

### To do

Not measured yet:

1. The size of a decoder-only wasm build (needs the C interface behind a feature).
2. `uc` and `uc.a` against blobs, on demos with usercmd blocks.
3. XXH3 without SIMD from the reference C code (a download was blocked; the WebAssembly
   run without SIMD stands in for it).
4. rayon on the final code (optional).
5. The count of bits that can be flipped unnoticed, after the fallback change (the
   probe was lost with `/tmp/blobplan`).

## Remaining implementation

After the format decisions (open questions 4, 9, 12, 13):

1. Regenerate the vectors in `tests/vectors/`; hostile vectors for the header, checksums
   and chunk sums are already generated by `tests/vectors.rs`.
2. Go conformance decoder (`interop/specdec`): the new container, `compress/flate` in
   place of `klauspost/compress`, the 20 streams as blobs, no model in a chunk.
3. FORMAT.md: rewrite the container (§2); delete metadata, index, demoinfo sharing,
   attachments and streaming (§3, §4, §5.3, §10, §11); adjust the rest. README and
   `include/mvdz.h` comments that still point at removed sections.
4. BENCHMARKS.md from the final measurements.

## Verification

- Every benchmark sample round-trips byte for byte (2,000 of 2,000 in every run so far).
- The Rust and Go decoders agree on every vector (after the Go decoder is updated).
- Refused: truncated file, dropped or swapped chunk, trailing bytes, a flipped bit in
  a chunk, wrong header size, duration or hash.
- Refused: a chunk's blob data that decodes to the wrong length, has bytes after its
  final block, or is not valid DEFLATE.
- Ratio compared with rc.4 at `-s 256K` (0.218 of raw on the stratified sample).
