# Simplifying MVDZ

MVDZ is a proposal with no files in the wild, so there is no compatibility burden.
Earlier versions of this file (plans of done steps, superseded results) are in git.

## Status (2026-11-21)

Branch `feature/simplify` on `master` (rc.4, a9ac8fc). Every open question is decided
(see Decisions). Code is in `mvdz_rust/`, a virtual workspace: the library in `mvdz/`, the tool in `mvdz_cli/`.
Paths below are relative to `mvdz_rust/mvdz/` unless they start with `mvdz_cli/`.
Tests (`cargo nextest run --release`): 49 pass, none fail.
`cargo clippy --all-targets -- -D warnings` is clean, with no lint allowed for naming. Next: "Open tasks".

## Working rules

- Viktor commits every change himself. Never commit, never `git add`. Stop after each
  step, list the changed files, suggest a commit message, and wait. Exception: all
  refactoring (names, scope, order, comments; no change in behavior) goes in one commit,
  so continue through it without stopping.
- Plan until Viktor says to implement. Decisions are his: recommend, number the options,
  each suggestion as a table with the current and the proposed solution side by side.
- Every change follows `~/dev/slipgate/AGENTS.md` and `~/dev/slipgate/rust/AGENTS.md`.
  Before declaring done: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo nextest run --release --no-fail-fast`, `cargo +nightly fuzz build`.
- No `unsafe` (`unsafe_code = "forbid"`). Simplicity is a high priority.
- Every claim needs a measurement or a checked fact; otherwise say it is unmeasured.
- Timing runs: ask Viktor for a silent machine first; at most 3 minutes a run; compare
  two builds by alternating runs (the machine flips between two speed levels about 6 %
  apart); cores 0, 2, 4, 6 (four physical cores). `-j 4` is the headline figure, one
  thread the secondary one.
- Limits: a thing repeated per segment or per stream gets about 100 times the real
  maximum (what mvdsv can write), never 1,000. A field that occurs once per file may be
  generous (u64) if it costs little. Where the simple choice clears the limits by a
  wide margin, take it (fixed width over variable-length).

## Where things are

- Demos: set8, sample40 and stratified of `benchmarks/samples/`, from
  `https://dev.quake.world/mvdz/<set>.tar.gz`; `scripts/fetch_samples.sh <set> <dir>`
  downloads one and checks every SHA-256 (8/8,
  32/40 and 379/585 present). Single demos: `https://d.quake.world/<first 3 hex>/<sha256>.mvd.gz`
  (some only). Stratified
  is unpacked in `/tmp/mvdz-strat/demos` (`/tmp` does not survive a reboot).
- Library speed: `benches/codec.rs` (criterion; demos from `MVDZ_BENCH_DEMOS`).
  Session harness in `/tmp/seqdec/harness`: `quick.sh` (library, CLI, memory, wasm),
  `quick_enc.sh`, `roundtrip.sh`; bit-flip probe `/tmp/seqdec/flip/zz_flip.rs`.
- Not on this machine: `~/dev/mvdz-bench` (500-demo list `rand500.txt`, `compare4.sh`)
  and `~/dev/qwdemos`.
- Reference sources: `~/dev/slipgate/reference/mvdsv` (demo writer in `src/sv_demo.c`,
  `src/sv_demo_misc.c`; constants in `src/bothdefs.h`, `src/server.h`) and
  `~/dev/qw-group/qwprot/src/protocol.h` (message types `dem_*`, hidden block ids,
  the protocol constants of `proto.rs`).
  Archive index: `/media/vikpe/qw_storage/demos_storage/all_demos/index.jsonl`.

## Goal

A format simple enough to stay unchanged for 10+ years.

- An MVD is a plain series of frames. It need not be a match, and may hold several
  matches or maps. The format does not model a match.
- Compress and decompress take no options. Parameters are tuned once and fixed.
- A decoder has as few paths as possible. Anything optional is frozen forever.
- The Rust code is an example of implementing the specification.

## Decisions

| Topic | Decision | Reason |
|---|---|---|
| Attachments, media attachments | Removed | Out of scope for a demo codec |
| META, INDEX, trailer | Removed | Match-shaped and derivable by decoding; the index duplicates the segment headers, which readers scan cheaply (a typical demo is about 10 segments) |
| Segment size | Cut at the first frame boundary after 256 KiB | Measured best; one setting |
| Largest segment | 1 MiB raw (`MAX_SEG_RAW`); where a frame or unframed data runs past 1 MiB the rest is cut every 1 MiB. Reader rule: every segment but the last is whole frames or exactly 1 MiB | A reader holds at most 1 MiB per segment by the format; normal demos are byte-identical |
| Live QTV streams | Not compressed | 4-11 KB/s raw does not justify it |
| QWD demos | Not supported; MVD only | A second frame walker and playerinfo model in every port; 75 of 2,200 archive files (3.4 %) |
| Streaming writer | Removed; finished `.mvd` only | One encoder path; shared models always available |
| C interface, Go decoder | Removed | One very good Rust implementation now; C, Go and others from the settled specification and a shared test suite |
| Seeking reader | Removed; one reader, in order | A demo has no keyframes; part of a file is read by listing the segments and decoding the ones wanted |
| Gzip input and output | Removed; a gzip file is refused with a message | Not part of the format; `gzip -dc x.mvd.gz \| mvdz compress - x.mvdz` |
| Structural transform, rANS | Keep | Most of the compression (zstd -19 alone: 0.553 of raw); rANS gives the best ratio |
| General-purpose compressor | Raw DEFLATE (RFC 1951) replaces zstd; one DEFLATE stream per segment holds all blob streams | +0.41 % size, +0.24 % decode time; zstd was 26 % of compression time; DEFLATE is in the Go, Python, Java and Node standard libraries and in browsers. One stream per segment: blob decode 1.7 times faster, 0.08 % larger |
| Stream models | 34 modelled streams, `pi.ar` included; 20 low-value streams are blobs: `snd.misc`, `pi.model`, `pi.eff`, `pi.skin`, `pr.level`, `esclen`, `mask`, `pe.hdr`, `pe.misc`, `pe.frame`, `pe.model`, `te.misc`, `pi.num`, `frags`, `stl.idx`, `stl.val`, `hid.tag`, `hid.len`, `hid.type`, `hid.dmg` (the stat-long predictor went with `stl.val`) | The 20 were worth 0.078 % together and each must match bit for bit in every port. Rebuilt codec per stream: every remaining model pays except `pi.ar` (-0.004 %), kept so the three angles are coded alike |
| Float origins `te.f`, `snd.f` | Stay blobs | Modelling them: estimated 0.31 % for two more streams every port must match |
| Segment coding | One: the transform. A segment it cannot code is written with `tail = raw_len` (every byte in the `TAIL` blob stream) | Coding 1 (raw DEFLATE) was used in 0 of 9,109 segments of 525 archive demos and 0 of 5 damaged copies |
| Coded length limit | `len` at most 2 MiB (`MAX_SEG_CODED`), checked at the length field; the ratio rule is gone | Largest coded segment seen 110,888 bytes; 1 MiB of junk codes to about 1 MiB + 0.5 KiB |
| Integers | Every integer a little-endian u32, except the three u64 header fields; no variable-length integers | One integer type, no variable-length reader in any port. +0.71 % on stratified (the rANS length at the head of each stream body is 0.10 % of it) |
| Header | 64 bytes: magic, version, size, duration, CRC-32, SHA-256; u64 fields | Once per file; one read; completeness without an end record |
| Models block | Models only here (none in a segment, also for one-segment files); at most 16 MiB | One model path (+0.004 %); largest seen 47,608 bytes |
| Model over the decision limit (rule M2) | The encoder makes it smaller (own contexts with the fewest symbols join the fallback table) | Otherwise a stream with no model sends every segment to the fallback: one demo grew 3.6 times (measured with the former DEFLATE fallback) |
| Model step size, levels | One step size (2), one level | Trying 4 as well was worth 77 bytes in 1.68 GB; no options |
| Encoder self-check | Always on | Every file written is proven to decode; about a third of compression time |
| Self-check fallbacks | `EncodeStats::self_check_fallbacks` counts segments that failed the self-check and were written as all tail; fuzz targets (through `fuzz::encode_any`) (with `REQUIRE_TRANSFORM_LOSSLESS`) and round-trip tests require 0 | A transform bug otherwise only costs size |
| Checksums | CRC-32 of zlib per segment and for the whole demo, both checked on decode; SHA-256 in the header, never recomputed on decode | CRC-32 is in every standard library that has DEFLATE. The whole-demo check catches segments in the wrong order; the segment check serves subset decodes. SHA-256 on decode cost 9.1 % of a 4-thread decode with SHA instructions and 19.7 % of a batch decode without; a consumer that must authenticate hashes the output |
| Edge inputs | A demo ending inside a frame: the partial bytes are the `TAIL` of its segment. Input not starting like an MVD: refused (`NotMvd`); only the fuzz targets' `fuzz::encode_any` writes it. Unframed bytes are tail; past 1 MiB the rest is cut every 1 MiB and each piece walked afresh | Already round-trips and is tested |
| Unused models | A models block may list a model no segment uses; readers ignore it. The encoder writes only used models (`assemble` recomputes them after every self-check round). Test: `unused_model_is_ignored` | No decoder rule to implement in every port; a streaming encoder can write the models block before it knows which segments fall back; the cost is bounded by the 16 MiB limit |
| Flippable bits (open question 9) | Kept as is | See "Bit-flip probe" for the counts and the options if revisited |
| Thread count | `max_threads: u8`: an argument of `encode`/`decode`, a setter on `Encoder`/`Decoder` (default 0; std builder shape, room for later options); capped at the logical cores; 0 is all logical cores. Tool: `-j` the same, default 0 | Viktor, 2026-11-21; replaces "library default 1" |
| Threads | Across segments only, encoder and decoder; nothing inside a segment runs in parallel. The streaming reader decodes up to `threads` segments at once | No `unsafe`, one rule. Costs: 1-thread decode +7 %, demos of 1 segment at `-j 4` up to 0.9 ms (decode) and 1.5 ms (encode) slower; the CLI at `-j 4` on a 16 MB demo decodes 30 % faster. Demos are typically 1 MB or more |
| No `unsafe` | `unsafe_code = "forbid"`; the rANS table search is one safe loop on every target | Viktor: "unsafe is horrible choice". The safe search costs about 6 % of decode against SSE2 |
| Compression passes (open question 11) | Pass 1 keeps only counts; pass 2 walks and predicts again | Keeping pass 1's results would save up to about 19 % of 1-thread compression CPU for about twice the peak memory |
| Speed | Models block read once and built on all threads; symbols counted per stream in parallel; the demo's CRC-32 and SHA-256 computed on a second thread during the walk | Measured gains at the time; see git history |
| Output files (tool) | Written directly with `File::create`, removed when the write fails; no temporary file, no fsync | As gzip and zstd; fsync cost 2.9 ms a file (18 % of a batch decompression). Ceiling: a failed `-f` overwrite loses the old file, a killed process leaves a partial one |
| Crates | A virtual workspace (`mvdz_rust/Cargo.toml`: members, shared dependency pins, lints, profiles) with the library `mvdz` in `mvdz/` and the tool `mvdz_cli` (binary `mvdz`) in `mvdz_cli/` | Not every user needs a CLI: a library user builds no tool code. Directory names match crate names (rust AGENTS rule) |
| Library scope | No file writing, no JSON: `mvdz info`'s JSON is built in `mvdz_cli/src/main.rs` | Not the codec's job |
| Public API | `encode`, `decode`, `Encoder` (`Encoder::new(writer)`, `.max_threads(n)`, implements `io::Write`, buffers the demo; `finish` encodes and writes, since the encoder needs the whole demo), `Decoder` (`Decoder::new(reader)?`, `.max_threads(n)`, implements `io::Read`; bounded memory); the pair as gzip libraries have it, Viktor 2026-11-21, `Decoder::header()` (`Header`, the file header's fields, which all describe the demo: getters only, no format version; as gzip's decoder exposes its header). No `info`: the file size is a file-system call, the segment list waits for part-of-file decoding (task 4), `Error`, `Result`. Everything else is `pub(crate)`. The opt-in feature `fuzz` adds a hidden `fuzz` module (`encode_any`, `check_demo`) for the fuzz targets; the fuzz generator tracks the protocol state itself. Tests that build files by hand are unit tests in `src/tests/` | Information hiding: what a crate exposes is its contract |
| Memory limit | Fixed: 1 GiB on 64-bit, 256 MiB on 32-bit (`DEFAULT_MAX_MEMORY`); no option, no `--max-memory` | Viktor, 2026-11-21 |
| Test vectors | Regenerated 2026-11-21 (`MVDZ_WRITE_VECTORS=1`, `MVDZ_TEST_DATA` with `sample/`): duel = set8 `3fe477dc`; FTE float = the first 300,000 bytes of stratified `03fc8ec2` (4 of 379 stratified demos use FTE float, none of set8 or sample40); MVD1 float = a synthetic demo in `vectors.rs` (no real one among 411 demos), checked by `mvd1_float_demo_is_walked` | The old sources (`3ff1f82b`, `3fdd8018`, `gen/float_mvd1.mvd`) could not be found again |
| Truncating casts | One workspace `allow` (`cast_possible_truncation`), reason in `Cargo.toml` | 83 casts: 30 positions bounded by the 1 MiB segment, 2 MiB coded segment and 16 MiB models block; 53 intended bit and byte extraction. Every alternative adds code or error paths that cannot fail (Viktor, 2026-11-21) |
| Work limit | Removed (2026-11-21); only the memory limit stays | Rule M2 and at most 34 models bound model decoding far below the old 4096 units per input byte; output is at most 1 MiB per segment (about 10^4 times the input, estimated), and whole-file `decode` is capped by memory. The most compressible demos measured encode at 1,471:1. gzip and zstd have no work limit either |
| Non-demo input | Refused; `--allow-non-demo` and `compress_any` removed | Viktor, 2026-11-21 |
| Tool exit status | `main` returns `ExitCode` (2 bad command line, 1 a failed file, 0 when standard output closes early); no `process::exit` | Errors return to `main` instead of ending the process where they occur |
| Names | Spelled out (`SegmentHeader`); protocol constants are qwprot's names with words split by `_` (`U_FTE_MODEL_DBL`), no comments. Stream-id constants (`PI_OX`, ...) stay short | Stream-id constants mirror the stream names in FORMAT.md |
| Dependencies | Library: `sha2`, `crc32fast`, `miniz_oxide`. No rayon (the scoped-thread `par_map` stays), no `mimalloc`, no built-in profiler | rayon: no gain that repeats, 6 more crates. mimalloc: no gain on glibc, 2-4 times the memory |

## The format as implemented (version 2)

```
file    := header | models | segment*
header  := "MVDZ" | version u32 | size u64 | duration_ms u64 | crc u64 | sha256[32]   (64 bytes)
models  := len u32 | body[len]
body    := n u32 | n x { id u32 | len u32 | model[len] }             (ids strictly increasing)
segment := len u32 | raw_len u32 | dur_ms u32 | crc u32 | coded[len]  (16 bytes, then coded)
coded   := fte u32 | fte2 u32 | mvd1 u32 | tail u32 | n u32
           | n x { id u32 | count u32 | [len u32 if id is not a blob stream] }
           | blob_len u32 | stream bodies in table order | blob body (blob_len bytes)
stream body := rans_len u32 | rans[rans_len] | extra (rest of the body)
```

Every integer is little-endian and of fixed width. FORMAT.md calls a segment a chunk.

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
- A segment's raw offset and start time are the running sums of `raw_len` and `dur_ms`;
  the sums over all segments equal `size` and `duration_ms`.
- The file is complete when `raw_len` sums to `size`; bytes after that are an error. An
  empty demo has no segment.
- A full decode verifies each segment's CRC-32 and frame time, and the whole demo's
  CRC-32, size and duration. A subset decode verifies the segments it reads.
- `models` holds one rANS model per modelled stream that some segment codes with it
  (`len` 0: none). A modelled stream without a model there is an error.
- A segment: protocol state at its start, a stream table, the stream bodies, then one
  raw DEFLATE stream holding all blob streams in table order (no wrapper, no preset
  dictionary; it decodes to exactly the sum of their counts and nothing follows its
  final block). Each segment decodes on its own, given the models block.
- Limits: `raw_len` 1 to 1 MiB; `len` at most 2 MiB; models block at most 16 MiB.

What consumers do without META:

- Size, duration, identity: the 64-byte header.
- Map, serverinfo, initial players, start time: decode the first segment.
- Final scores, KTX demoinfo: scan back from the last segment (demoinfo starts before
  the last segment in 25 of 367 files of a random 500).
- Anything else: full decode. A segment gives exact bytes, not full game state.
- `duration_ms` is demo time, which stands still while the server is paused.

mvdsv's limits against ours (mvdsv 70f1135; headroom is ours divided by mvdsv's):

| Thing | mvdsv maximum | Ours | Headroom |
|---|---|---|---|
| Frame payload | 8,092 bytes (`MAX_MVD_SIZE`) | no rule; a frame up to 786,432 bytes is kept whole in one segment | 97x |
| Bytes per server tick | 40,506 | bounded by the 1 MiB segment | 26x |
| Frame time | 255 ms, one byte | `dur_ms` u32 per segment | 96x |
| Segment raw length | 270,245 (256 KiB + one largest frame) | 1 MiB | 3.9x |
| Segment coded length | about 270 KiB for mvdsv data; about 1 MiB + 0.5 KiB for junk | 2 MiB | 7.7x / 2x |
| Demo size | 20 MiB default (`sv_demoMaxSize`; 0 = unlimited) | u64; `MAX_SEGMENTS` 65,536, about 16 GiB | 819x |
| Symbol count per stream | 1,048,592 | u32 | 4,096x |
| Models block | none; largest measured 47,608 | 16 MiB | 352x (measured) |
| Frame types | writes 3, 4, 5, 6 | accepts 1 to 6 (1 and 2 occur in the archive) | - |

The archive (293,929 MVD files): median 2,032,160 bytes, p99 14,010,690, largest
67,109,197; 122 files over 20 MiB.

## Measurements

### Current speed and memory (3f737db, 72d5175)

32 demos of stratified (108,606,042 bytes raw), cores 0, 2, 4, 6, silent machine;
medians of two alternated runs (`/tmp/seqdec/harness/quick.sh`).

| Measure | Result |
|---|---|
| Library decode, 1 thread | 1,090 / 1,087 ms |
| Library whole-file decode, 4 threads | 386 / 387 ms |
| Library streaming decode, 4 threads | 470 / 471 ms |
| Library compress, 1 thread | 3,265 ms (33.5 MB/s) |
| Library compress, 4 threads | 1,413 ms (77.5 MB/s) |
| CLI decompress, 62 segments (16.1 MB), `-j 1` / `-j 4` | 180 / 79 ms |
| CLI decompress, 1 segment (237 KB), `-j 4` | 4.5 to 4.9 ms |
| CLI compress, 62 / 3 / 1 segments, `-j 4` | 213 / 18 / 11.2 ms |
| Peak memory, CLI decompress `-j 4`, 62 segments | 8.1 to 8.3 MiB |
| Peak memory, CLI compress, 16.1 MB demo | 37.2 MiB (`-j 1`), 46.8 MiB (`-j 4`) |
| wasm decode, 1 thread, node | 55.3 to 55.6 MB/s (old build's runs not conclusive) |

Size: stratified (379 demos, 968,979,765 bytes raw) compresses to 211,736,432 bytes
(0.2185 of raw; rc.4 at 256 KiB segments: 0.218).

### Where compression time goes (6b4b32f)

Same 32 demos, `perf record -F 999`, flat profile at 1 thread (LTO inlines heavily, so
stages are grouped by the function that holds them):

| Stage | Functions | Share |
|---|---|---|
| Self-check decode | `Group::run<false>`, `Walker<false>`, `decode_segment`, `read_tables`, `Table::from_q`, `IntModel::get`, inflate | about 31 % |
| Numeric predictors, pass 1 and again pass 2 | `segment::numerics` | 22.6 % |
| Walk, pass 1 and again pass 2 | `Walker<true>::command`, `segment::walk`, `Proto::cmd_size`, `delta_size` | 15.3 % |
| rANS and DEFLATE encode | `Body::new` | 10.9 % |
| Training | `Tally::add`, `Trainer::models`, `IntModel::put` | 9.6 % |
| Allocation, copies | | about 2.5 % |
| SHA-256, CRC-32 | | 2.1 % |

- 4 threads are 2.31 times 1 thread; the pass-1 walk stays on one thread (the
  protocol state carries from segment to segment).
- Keeping pass 1's walks and values for pass 2 (measured with a temporary test, a
  lower bound): 3.49 times the raw demo (2.42 to 4.06 per demo); 56.2 MB for the
  16.1 MB demo. Not done (open question 11).

### Threads and the table search (3f737db)

Before removing the in-segment pipeline (f0c2019, machine in use): segment
parallelism beat the pipeline from 3 segments up; whole-file decode at 4 threads
(40 ms) was 1.9 times faster than pipelined streaming (75 ms) on the 16.1 MB demo.

Table search, decode of the 32 demos at 1 thread, best of 10, two runs: SSE2
1,017.8 / 1,013.4 ms; 16 `u16` counters (chosen) 1,075.6 / 1,076.7 ms (+6 %); the old
`scalar` path +23 %; 8 `i16` counters +20 %. The SSE2 intrinsics need `unsafe` even
though SSE2 is always on for x86_64. A checked slice instead of `get_unchecked`: within
noise.

### Bit-flip probe (open question 9)

Every bit of the file flipped, decoded with `mvdz::decompress` (312f6ae).
No flip ever decodes to a different demo. Flips that decode to the same demo:

| Region | 20 KB prefix (7,364-byte file) | 237 KB demo (65,619-byte file) | Cause |
|---|---|---|---|
| Header SHA-256 | 256 of 256 | 256 of 256 | not checked on decode, by decision |
| Segment `fte` / `fte2` / `mvd1` | 31 / 32 / 31 of 32 each | same | decoding reads only `FTE_PEXT_FLOAT_COORDS`, `FTE_PEXT_TRANS`, `FTE_PEXT_COLOR_MOD`, `fte != 0` and `MVD_PEXT_1_FLOAT_COORDS`; `fte2` is never read |
| Models block | 572 of 7,040 | 587 of 31,664 | per model: the range coder's first byte (always 0, shifted out unchecked) and a few bits of its flush bytes |
| Numeric rANS bodies | 32 of 21,624 | 31 of 383,544 | padding after the mantissa bits, not checked to be 0 |
| DEFLATE blob body | 23 of 19,320 | 69 of 30,880 | final padding and bits the inflater ignores |
| Everything else | 0 | 0 | |

Options if revisited: (1) reader checks, no format change: range coder's first byte 0,
numeric padding 0; (2) format change: one u32 with the 5 flags decoding reads, other
bits 0, `fte2` gone (8 bytes less per segment). Not practical: SHA-256 check on decode,
an exact range-coder flush, DEFLATE slack.

### What each stream model is worth (f0c2019)

Rebuilt codec per stream: the stream loses its model and its bytes join the segment's
blob (a numeric residual as 4 bytes). Stratified, baseline 211,736,432 bytes. Change in
output if the model is dropped:

| Stream | Change |
|---|---|
| `pi.ar` | -0.004 % |
| `uc`, `uc.a` | unmeasurable: no usercmd block in the sample |
| `pe.fy`, `pe.fz`, `pe.fx`, `pi.fz` | +0.017 to +0.020 % |
| `pi.fy`, `pi.fx` | +0.051 %, +0.052 % |
| `te.type`, `pe.more`, `pe.ent` | +0.084 to +0.158 % |
| `pi.wframe`, `st.idx`, `pe.a` | +0.200 to +0.220 % |
| `st.val`, `snd.num`, `type`, `snd.ch` | +0.384 to +0.484 % |
| `pe.bits`, `time`, `snd.o`, `pi.frame`, `cmd` | +0.734 to +1.714 % |
| `te.o`, `pi.flags` | +2.054 %, +2.228 % |
| `pe.oy`, `pe.oz`, `pe.ox`, `pi.oz` | +3.27 to +4.28 % |
| `pi.oy`, `pi.ox`, `pi.ap`, `pi.ay` | +7.92 to +10.50 % |

### Blob coder (rc.4, 2,000 demos)

Blob coders on the blob streams alone (604.5 MB raw, 7.5 % of the output):

| Blob coder | Change in file size |
|---|---|
| zstd 17 per stream (rc.4) | baseline |
| Raw DEFLATE 9, one blob per segment (chosen) | +0.41 % |
| Raw DEFLATE 9, two blobs per segment | +0.24 % |
| zstd 17, per-file dictionary | -0.96 % |
| Adaptive order-2 model | +3.72 % |
| Stored | +28.57 % |

zstd was 25.9 % of rc.4's compression time, DEFLATE 9 2.7 %. `DecompressionStream
("deflate-raw")` since Chrome 103, Firefox 113, Safari 16.4. Decoder size: Go inflate
1,015 lines against `internal/zstd` 2,277; RFC 1951 4,715 words, RFC 8878 12,917.

## Known failing tests

Rule: between commits no test may fail that is not on this list.

| Tests | Why they fail | Resolved by |
|---|---|---|
| (none) | | |

## Open tasks

Code:

1. Streaming input for the encoder (read the demo twice instead of holding it): peak
   drops by the raw size; 250 MB works today at about 410 MiB.

API gaps:

2. Part of a file (a time range, the last segment) cannot be decoded from outside the
   crate. If wanted: one public function, and each segment's starting protocol state.

Format and specification:

3. FORMAT.md: rewrite the container (section 2); delete metadata, index, demoinfo
   sharing, attachments, streaming (sections 3, 4, 5.3, 10, 11) and the C interface.
   State: the 1 MiB segment rule (it still gives 4 GiB and "only the last segment may
   end inside a frame"); `MAX_SEGMENTS` as a 16 GiB demo limit; section 12 rule 1 as
   the 1 MiB cut with each piece walked afresh, rule 5 as all tail; encoders refuse
   input not starting like an MVD; "scan back from the last segment" for demoinfo;
   a model no segment uses is allowed and ignored; the protocol constant names of
   `proto.rs`; no work limit, only memory (the resource rules section).
   Stale: code references `api.rs:…` and `stream.rs:…` (now `encode.rs`, and
   `decode.rs:Decoder`), line 382 (`info` JSON: now the header only: `size_bytes`, `duration_ms`,
   `crc32`, `sha256`), line 668 (decode waves), line 357 (`FileInfo::to_json`, now `header_json` in
   `mvdz_cli/src/main.rs`).
4. README: Quickstart, Command line, Hashes and trust, Rust, WebAssembly and the fuzz
   list are current (2026-11-21). Still rc.4: the intro (levels, metadata, attachments,
   C), Install (the AVX2 speed claim), Ratios and speeds (needs task 5's numbers,
   mentions `--trusted` and the fast level), the FAQ (metadata, attachments) and the
   Tests paragraph's `MVDZ_WRITE_VECTORS` note (updated 2026-11-21).
5. BENCHMARKS.md from the final measurements, on the new set8 (18.4 MB; README and
   BENCHMARKS.md still say 18.9 MB); drop Miri (no `unsafe` left).
6. Later: implementations in C, Go and other languages from the settled
   specification, with a shared test suite.

Not measured:

7. Size of a decoder-only wasm build.
8. `uc` and `uc.a` against blobs, on demos with usercmd blocks.
9. rayon on the final code (optional).

## Verification

- Every benchmark sample round-trips byte for byte (all 379 stratified at `-j 1` and
  `-j 4` after the latest changes).
- A second, independent decoder agrees on every vector (once one is written from the
  settled specification).
- Refused: truncated file, dropped or swapped segment, trailing bytes, wrong header
  size, duration or CRC, a flipped bit outside the regions under "Bit-flip probe".
- Refused: blob data that decodes to the wrong length, has bytes after its final block,
  or is not valid DEFLATE.
