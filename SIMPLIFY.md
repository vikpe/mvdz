# Simplifying MVDZ

MVDZ is a proposal with no files in the wild, so there is no compatibility burden.

## Status (2026-11-21, night)

Resume point: "After steps B and C" in the Handoff below. Five format decisions on
2026-11-21; steps A (149c415), B (d1a8c06) and C (3fedb9d, every integer a u32) are
done. Open questions 6, 13 and 14 decided later the same day (see Decisions); the
self-check switch is removed (f0c2019). 12 decided: every stream model stays (see
Measurements and Decisions). Open: 9 (needs a new probe). Then "Suggestions (2026-11-21)" near the end of this file.
No unsafe code since the same night (3f737db; see Decisions); the encoder threads
across segments only (not committed at the time of writing). Tests (`cargo nextest run --release` in `mvdz_rust/`): 41 pass, 4 fail (the
known four).
`cargo clippy --all-targets -- -D warnings` is clean.

Done on 2026-11-21: two-pass encoder and the memory work, CLI decompress streams,
gzip, mimalloc, the seeking reader, the C interface and the Go decoder removed, crate
moved to `mvdz_rust/`, slipgate lint set, visibility, anyhow in tests, Rust 1.100,
segments capped at 1 MiB. See Decisions.

The paragraphs below are the status of 2026-11-20 and are partly superseded.

Since 2026-11-21 the Rust implementation is in `mvdz_rust/` (the crate is still named
`mvdz`): paths in this file such as `src/...`, `tests/...` and `fuzz/...` are relative to
that directory. The specification, this file and the benchmarks stay at the root.

Branch `feature/simplify` on `master` (rc.4, a9ac8fc). Viktor commits each change
himself.

Done: the new container, DEFLATE in place of zstd, the 20 low-value stream models
dropped, attachments, metadata, index, trailer, streaming writer, levels, chunk-size
option, trusted mode and the SHA-256 pass on decode removed, one model path, fixed-width
container fields, five speed changes. See Decisions and Results per change.

Also done: one fallback (ad53e50; see "One fallback" under Results per change), and
measured at 1, 4 and 12 threads and in WebAssembly (see "Threads and WebAssembly").

Tests: the four failures are listed under "Known failing tests".

Whole session against `master`: see "Whole session" under Results per change.

Next, in this order (nothing below is started):

1. The open question that can still change the file format: 9 (4, 12, 13 and 14 are
   decided).
2. The other open question, 11 (5, 6, 7 and 10 are decided).
3. Finishing work, once the format no longer changes: regenerate the test vectors
   (clears the 4 failing tests), rewrite FORMAT.md and README, update BENCHMARKS.md.
   Implementations in C, Go and other languages come after that (see Decisions).

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

## Handoff (written 2026-11-21 for the next session)

Read this section first. Steps B and C are done (their plans below are kept as the
record); the next work is under "After steps B and C". The rest of the file is history
and measurements.

### Working rules

- Viktor commits every change himself. Never commit, never `git add`. Stop after each
  step, list the changed files, suggest a commit message, and wait.
- Do not edit code until Viktor says so for the step at hand (he has said so for steps A,
  B and C: "2. Record and implement", 2026-11-21). Do not start the Suggestions without
  being asked.
- Between commits no test may fail that is not under "Known failing tests" (the four
  that read old vectors). Run from `mvdz_rust/`: `cargo nextest run --release
  --no-fail-fast`, `cargo clippy --all-targets --release -- -D warnings`, `cargo fmt`.
- Every claim needs a measurement or a checked fact; otherwise say it is unmeasured.
- Decisions are Viktor's: recommend, number the options, and wait. Each suggestion in
  a table with the current and the proposed solution side by side.
- Limits: a thing repeated per segment or per stream gets about 100 times the real
  maximum (what mvdsv can write; see the table below), never 1,000. A field that occurs
  once per file may be generous (u64) if it costs little. Where the simple choice still
  clears the limits by a wide margin, take the simple choice (fixed width over
  variable-length).

### Where things are

- Code: `mvdz_rust/` (crate `mvdz`). Paths below are relative to it.
- Steps A, B and C are done on branch `feature/simplify`: 149c415 "Replace the ratio
  rule with a 2 MiB cap on a segments coded length", d1a8c06 "One segment coding: drop
  coding 1 and the coding byte", then step C.
- The 500-demo list (`~/dev/mvdz-bench/rand500.txt`) and `~/dev/qwdemos` are not on
  this machine. Step C was measured on `stratified` instead (379 demos, in
  `/tmp/mvdz-strat/demos`, with `/tmp/mvdz-strat/size.sh BIN DIR`, which sums the
  `mvdz compress` output; `/tmp` does not survive a reboot).
- Demos for measuring: set8, sample40 and stratified of `benchmarks/samples/`, unpacked
  (`<sha256>.mvd`), from `https://dev.quake.world/mvdz/<set>.tar.gz`;
  `scripts/fetch_samples.sh <set> <dir>` downloads one and checks every SHA-256. They
  hold 6/8, 32/40 and 379/585 (the rest could not be found again), without the
  golden-vector duel (`3ff1f82b…`) and the FTE demo (`3fdd8018…`) of `tests/`.
- Reference sources: `~/dev/reference/mvdsv` (demo writer in `src/sv_demo.c`,
  `src/sv_demo_misc.c`; constants in `src/bothdefs.h`, `src/server.h`) and
  `~/dev/reference/qwprot/src/protocol.h` (message types `dem_*`, hidden block ids).

### The target format after steps B and C

```
file    := header | models | segment*
header  := "MVDZ" | version u32 | size u64 | duration_ms u64 | crc u64 | sha256[32]  (64 bytes)
models  := len u32 | body[len]
body    := n u32 | n x { id u32 | len u32 | model[len] }           (ids strictly increasing)
segment := len u32 | raw_len u32 | dur_ms u32 | crc u32 | coded[len]   (16 bytes, then coded)
coded   := fte u32 | fte2 u32 | mvd1 u32 | tail u32 | n u32
           | n x { id u32 | count u32 | [len u32 if id is not a blob stream] }
           | blob_len u32 | stream bodies in table order | one raw DEFLATE stream of blob_len bytes
stream body := rans_len u32 | rans[rans_len] | extra (rest of the body)
```

Every integer is little-endian; every integer is a u32 except the three u64 fields of
the file header. There is one segment coding, the transform. A segment the transform
cannot code is written with `tail = raw_len`: no frames, every byte in the `TAIL` blob
stream (id 81). Limits: `raw_len` 1 to 1 MiB (`MAX_SEG_RAW`); `len` at most 2 MiB
(`MAX_SEG_CODED`, step A); models block at most 16 MiB (`TABLES_MAX`). Every segment
but the last is whole frames or exactly 1 MiB. `version` stays 2 until the vectors are
regenerated (the old vectors fail anyway).

### Step B: one segment coding (done, d1a8c06)

Result: implemented as planned (`segment::walk_tail`, `encode::compress_all_tail`). The
500-demo size check below was not run: the sample is not on this machine. Measured on
stratified at ebe25b3 (after B): 210,244,777 bytes, the baseline for step C.

Goal: delete coding 1 (whole segment as DEFLATE) and the `coding` byte. Write the test
first: a segment of bytes that *do* frame, written as all tail, must round-trip through
every reader (`readers_agree` in `tests/common/mod.rs`). Today an all-tail segment is
only known to work for bytes that do not frame (`random.normal.mvdz`, 3,000 random
bytes, and 1 MiB of appended junk in a manual run).

Library:

1. `src/format.rs`: remove `C_TRANSFORM`, `C_DEFLATE`, the `coding` field of `SegHdr`,
   the `coding` parameter of `seg_hdr_bytes`, the coding check in `parse_seg_hdr`;
   `SEG_FIELDS` becomes `4 + 4 + 4` (12). Update the module doc grammar.
2. `src/decode.rs` `decode_segment`: drop the `match h.coding`; always
   `segment::parse_header` + `segment::decode`. Imports of `C_*` go.
3. `src/encode.rs`: delete `raw_body`, `Body::raw`, `Body::fallback`,
   `Info::fallback_segments`, `compress_untransformed` and the `force_raw` parameter of
   `compress_inner`; import of `DEFLATE_LEVEL` goes. Where a segment is not walkable
   (`t.walked[i]` false, or `segment::walk` returns None in pass 2, or
   `transform_body` returns None) code it as all tail (point 4). The self-check loop
   (`for _round in 0..2`, `failing_segments`): keep it, but a failing segment is
   replaced by its all-tail body instead of `Body::raw`; a segment that is already all
   tail and still fails is `Error::Internal`. `Info::escaped` already counts tail bytes.
4. `src/segment.rs`: add `pub(crate) fn walk_tail(src: &[u8], proto: Proto) -> Walked`
   next to `walk` (line 227): `io = EncIO::new()` with `io.blobs[TAIL] =
   src.to_vec()`, `recs = Recs::growable(0)` then `recs.finish()`, `tail = src.len()`,
   `proto0 = proto`; the protocol state after it is `proto` unchanged (nothing was
   parsed). Then `numerics(src, &wk, threads)` must give empty streams for it (check:
   `Group::run` over empty `recs`), and `segment::encode` must write a table with the
   one blob entry (`TAIL`, count = len) and the DEFLATE blob body. Decode side needs no
   change: `Walker::run` in decode direction with `tail == dst_len` runs no frame and
   copies the tail (walk.rs line 578). Untested until the test in the first paragraph
   passes.
5. `src/main.rs`: remove the fallback warning (lines 315-318). (`bench` and its
   `fallback_segments` column are gone with the criterion change.)
6. `src/deflate.rs` stays: the blob body is DEFLATE. `compress` with level 0 (stored)
   is then unused: remove that branch if nothing else calls it.

Tests:

7. `tests/common/mod.rs`: delete `untransformed`.
8. `tests/hostile.rs` lines 84-95: the "unknown coding" case goes; the "empty segment"
   case builds its header with the 3-argument `seg_hdr_bytes`. Lines 128-136 (the
   "1 MiB segment in 9 bytes" case) build a transform body instead: any 9 bytes are
   refused by `parse_header`. Line 202-206 and `tests/review_fixes.rs` line 68: drop the
   `C_TRANSFORM` argument. `tests/review_fixes.rs` lines 174-187 (an untransformed
   segment's CRC): use a normally compressed file.
9. `tests/vectors.rs`: remove vectors `segment-coding` (line 272), rebuild
   `segment-empty` (275) and `segment-duration` (288-310) without coding 1 (the latter
   can use `one(&plain, ...)` and raise `dur_ms` and the header's duration by 1); remove
   `duel.untransformed.mvdz` (555-566) and `random.deflate.mvdz` (600-613). Add
   `framed-as-tail.mvdz`: a synthetic demo written as all tail (needs a test entry
   point; `compress_untransformed` becomes `compress_all_tail` with the same
   `#[doc(hidden)]` status, or the vector is built by hand from `parts`).
10. `tests/roundtrip.rs` lines 14, 113, 275 and `tests/corrupt.rs` line 52: the
    `fallback_segments` asserts go. `fuzz/fuzz_targets/gen_rt.rs` line 4 comment.
11. FORMAT.md is rewritten later (Remaining implementation); do not touch it now.

Measure after B: the 500-demo output size must be unchanged to the byte except for the
missing coding byte (5,649 segments: 278,553,572 - 5,649 = 278,547,923 bytes). Report.

### Step C: every integer a u32 (done)

Result: every integer in a segment, the models block and a stream body is a
little-endian u32. Beyond the list below, `rans_len` at the head of each structural and
numeric stream body is a u32 too (decided by Viktor, 2026-11-21: no variable-length
reader left in the file). `put_uv`, `Rd::uv`, `Rd::uz`, `Rd::u32v` and `uv_limits` are
gone; `util::put_u32` and `Rd::u32z` are new; `Rd::bytes` takes a u32 length. The test
`model_amplification` reads the reviewer's `poc/v2model_*.bin` header length with a
local LEB128 reader (their format); it skips without `MVDZ_TEST_DATA`. Size on
stratified (379 demos, 968,979,765 bytes raw), self-check on:

| Build | Output, bytes | Change |
|---|---|---|
| ebe25b3 (step B) | 210,244,777 | - |
| Step C, `rans_len` left variable-length | 211,524,887 | +0.61 % |
| Step C | 211,736,432 | +0.71 % |

`rans_len` as a u32 costs 211,545 bytes (0.10 %). All 379 round-trip through both
readers (`examples/sweep`). Speed not measured. The escape lengths inside the `ESCLEN`
stream (`walk.rs`, `escape`) are still 7-bit groups: they are coded symbols, not file
fields.

Goal: no variable-length integers in the file. Expected cost: +0.66 % of output on the
500 (arithmetic from the entry counts; measure the real figure).

1. `src/segment.rs` `encode` (lines 482-507): each `put_uv(&mut out, v)` becomes
   `out.extend_from_slice(&(v as u32).to_le_bytes())` (ids, counts and lengths all fit:
   count at most 1 MiB + 16, length at most 2 MiB; use `u32::try_from` with an
   `Internal` error, or a checked cast under the crate's cast rule).
2. `src/segment.rs` `parse_header` (579-640): `r.u32v()`/`r.uz()` become `r.u32le()`
   (then `usize::from`/`as usize` per the cast rule). Keep every check (tail <=
   raw_len, n <= N_STREAMS, stream id valid, no duplicate, count bound, bodies sum to
   the body length).
3. `src/segment.rs` `tables_body` (389-395) and `src/coder/rans.rs` `read_tables`
   (885-900): count, id and length as u32.
4. `src/util.rs`: delete `put_uv`, `Rd::uv`, `Rd::uz`, `Rd::u32v` and the test
   `uv_limits` (174-185) once nothing uses them. `Rd::u32le` stays.
5. Tests that write these integers by hand: `tests/hostile.rs` (192, 199, 293-296),
   `tests/review_fixes.rs` (54-65), `tests/vectors.rs` (119-177, 508-511): write u32s.
6. Re-measure the flippable-bit count afterwards if asked (open question 9): padded
   variable-length encodings (`80 00` for 0) were one source of flippable bits.

Measure after C: output size on the same 500 against the step-B figure; report the
percentage. The Decisions table row "Integers inside a chunk" has the estimate.

### mvdsv's limits against ours (2026-11-21)

Theoretical maxima are from mvdsv's code (70f1135); headroom is ours divided by mvdsv's.

| Thing | mvdsv maximum | Ours (after A to C) | Headroom |
|---|---|---|---|
| Frame payload | 8,092 bytes (`MAX_MVD_SIZE`) | no rule; a frame up to 786,432 bytes is always kept whole in one segment | 97x |
| Whole frame | 8,102 bytes | same | 97x |
| Bytes per server tick | 40,506 | no rule; bounded by the 1 MiB segment | 26x |
| Frame time | 255 ms, one byte | `dur_ms` u32 per segment; a 1 MiB segment holds at most 44,564,310 ms | 96x |
| Segment raw length | 270,245 (256 KiB target + one largest frame) | 1 MiB | 3.9x |
| Segment coded length | about 270 KiB for mvdsv data; about 1 MiB + 0.5 KiB for junk | 2 MiB | 7.7x / 2x |
| Demo size | 20 MiB default (`sv_demoMaxSize`; 0 = unlimited) | u64; `MAX_SEGMENTS` 65,536, about 16 GiB | 819x |
| Segments per demo | 80 | 65,536 | 819x |
| Demo duration | about 10 days at the default size cap | u64 | 2 x 10^10 x |
| Symbol count per stream | 1,048,592 | u32 (step C; u16 is too small) | 4,096x |
| Stream body length | 2 MiB | u32 (step C) | 2,048x |
| Models block | none; largest measured 47,608 | 16 MiB | 352x (measured) |
| Frame types | writes 3, 4, 5, 6 | accepts 1 to 6 (1 and 2 occur in the archive) | - |

### After steps B and C

Decided 2026-11-21 (night): 6 (self-check always on, switch removed), 13 (`te.f`,
`snd.f` stay blobs), 14 (edge inputs: specify the code as is). Left: 9, 12 and 11.

For 12 Viktor asked for a re-measurement of every stream model before deciding. Done
2026-11-21 (see "what each stream model is worth, rebuilt codec" under Measurements):
34 modelled streams (16 structural, 18 numeric), one run each. Note for whoever
repeats it: the `PLAIN` list alone does not switch a stream, the walker's calls per
stream are hard-coded (`p8`/`pn`/`copy_blob` for plain, `b8`/`r8`/`bn` for modelled),
so the experiment diverts at the symbol sinks (`EncIO::byte`, `DecIO::byte`,
`NumEnc`/`NumDecIO::code`) with a run-time id list. Result: every model pays except
`pi.ar`. Viktor first chose to drop `pi.ar`, then reverted it the same day for
simplicity (see Decisions). Question 12 closed: every stream model stays.

Then the open questions 9 and 11; then the Suggestions in the
order given there (out-of-the-library 1 and 2, tests-and-experiments 1 and 3, naming),
then Remaining implementation (vectors, FORMAT.md, README, BENCHMARKS.md). Note for
FORMAT.md: demoinfo is not always in the last segment (25 of 367 files), so the
consumer note "decode the last chunk" must become "scan back from the last segment".

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
| Largest segment | 1 MiB raw (`MAX_SEG_RAW`, was 4 GiB minus 64 KiB), 2026-11-21. The encoder still cuts at the first frame boundary after 256 KiB; where a frame or data that does not frame runs past 1 MiB, the rest of the demo is cut every 1 MiB. Reader rule: every segment but the last is whole frames or exactly 1 MiB | A reader holds at most 1 MiB per segment, by the format and not by a budget; a coded segment cannot approach its 32-bit length field. Normal demos are byte-identical |
| C interface and Go decoder | Removed (2026-11-21): `src/capi.rs`, `include/mvdz.h`, `examples/c`, `interop/specdec` and their tests and fuzz target; the crate builds as a Rust library only | The work is the specification and one very good Rust implementation; C, Go and other implementations are written once the format is settled, from the specification and a shared test suite |
| Seeking reader | Removed (2026-11-21): `Reader::open`, `seek_time`, `seek_segment`, `mvdz_seek_time`, `mvdz_seek_segment`, `mvdz_segment_count`, the seek callback of `mvdz_open_callbacks`. One reader, in order | A demo has no keyframes, so playback cannot start mid-file; part of a file is read by listing the segments and decoding the ones wanted; one reading mode for every port |
| Gzip input and output | Removed (2026-11-21): library, C interface and tool take and give raw demos; a gzip file is refused with a message | Not part of the format; every user has gzip (`gzip -dc x.mvd.gz \| mvdz compress - x.mvdz`); one input type for every port |
| Structural transform | Keep | Most of the compression (zstd -19 alone: 0.553 of raw) |
| rANS | Keep | Best ratio (0.218 of raw at 256K) |
| General-purpose compressor | Raw DEFLATE (RFC 1951) replaces zstd | +0.41 % size and +0.24 % decode time; zstd was 26 % of compression time; DEFLATE is in the Go, Python, Java and Node standard libraries and in browsers |
| Blob layout | One DEFLATE stream per chunk holds all blob streams | Blob decode 1.7 times faster than one stream each; one inflate call per chunk; 0.08 % larger |
| Low-value stream models | The 20 listed under Open questions lost their model; their bytes join the chunk's blob | Each model must match bit for bit in every port; together they were worth 0.078 % of the output |
| Stream models, open question 12 (2026-11-21) | Every one of the 34 stays, `pi.ar` included | Measured with a rebuilt codec per stream: every model pays except `pi.ar`, -0.004 % (8,951 bytes on 379 demos) without it. Dropping it was implemented and reverted the same day: it saved no function, and made roll an exception to "the three angles are coded alike" in the walker's byte layout and in the specification. The uniform rule is the simpler one |
| No unsafe code (2026-11-21, 3f737db) | `unsafe_code = "forbid"` in `Cargo.toml`. Decoding is sequential inside a segment and parallel across segments; the streaming reader decodes `threads` segments at a time; the rANS table search is one safe loop on every target (`scalar` feature gone) | Viktor: "unsafe is horrible choice". Accepted cost: single-thread decode +7 %, whole-file decode at 4 threads +6 %, demos of 1 segment at `-j 4` +0.4 to 0.9 ms. The CLI at `-j 4` on a 16 MB demo is 30 % faster. 20 lines of `unsafe` and 240 lines of library code gone |
| Encoder threads (2026-11-21, not committed) | Across segments only, as the decoder: no threads inside a segment (`outer`/`inner` gone; `numerics` and `segment::encode` run in sequence) | Viktor: demos are typically 1 MB or more. Measured cost: a 1-segment demo (237 KB) compresses 1.5 ms slower at `-j 4` (9.6 to 11.2 ms); 3 and 62 segments and the 34-demo library set unchanged. Output identical on all 379 stratified demos |
| Chunk codings (2026-11-21, step B, d1a8c06) | One: the transform. A chunk the transform cannot code is written as all tail; the `coding` byte and coding 1 (raw DEFLATE) go; chunk header 16 bytes | Coding 1 was used in 0 of 9,109 segments of 525 archive demos and 0 of 5 damaged copies; a 1 MiB segment of junk already codes as all tail and round-trips |
| Ratio rule (2026-11-21, step A, 149c415) | Removed (`raw_len <= 1024 * len + 4096`, the two constants, the reader check, the "stored blocks" fallback). In its place a reader rule: a chunk's `len` is at most 2 MiB (`MAX_SEG_CODED`), checked at the length field before anything is read | Largest `raw_len / len` in 9,109 segments is 118.39, so the rule had 8.7 times headroom; the 1 MiB raw cap already bounds output. The largest coded segment seen is 110,888 bytes; 1 MiB of junk codes to about 1 MiB + 0.5 KiB, so 2 MiB is 2 times the possible maximum |
| Models block limit | Stays 16 MiB | Once per file; largest seen 47,608 bytes (352 times headroom); the cost is reader memory, not file bytes |
| Integers inside a chunk and in the models index (2026-11-21, step C, done) | Every number a little-endian u32: the chunk's protocol flags, tail, stream count, each stream entry (id, count, length), the blob body length, the models block index (count, id, length) and the rANS length at the head of each stream body. No variable-length integers anywhere | Simplicity over the margin (Viktor, 2026-11-21): one integer type in the file, no variable-length reader and its three edge rules in any port, no padded encodings. Estimated +0.66 % of output on 500 random archive demos, +1.31 % on the 25 largest and longest (arithmetic from the entry counts, without the rANS length). Measured after the change: +0.71 % on stratified (379 demos), of which the rANS length is 0.10 %. Open question 4 closed |
| Header fields | Stay u64 (`size`, `duration_ms`, `crc`), 64-byte header; `MAX_SEGMENTS` stays | Once per file, 12 bytes; the rule of 2026-11-21: a single-place field may be generous if cheap, a repeated one gets about 100 times the real maximum |
| Fallback (2026-11-20, superseded by "Chunk codings" above) | One: a chunk the transform cannot code is one raw DEFLATE stream. A modelled stream is always rANS with the file's model; the per-stream back-end id, the store and DEFLATE back ends and the `stored` chunk coding are gone | Two chunk codings, one coder per stream kind; -0.056 % size |
| Model over the decision limit (rule M2) | The encoder makes it smaller (own contexts with the fewest symbols join the fallback table) instead of dropping it | Without this a stream with no model sends every chunk of its file to DEFLATE: one demo in 500 grew 3.6 times, the output 1.9 % |
| Thread count | The tool defaults to all logical cores, no cap, `-j` overrides; the library's default stays 1 and 0 means all logical cores (also in the C interface and `-j 0`) | The usual convention; a library should not take every core unasked; inside one file 12 threads give nothing over 4, so a cap would buy nothing |
| Built-in profiler | Removed, with the `sampler` feature and the optional `libc` dependency | `perf` does the same; 92 lines, all of them around `unsafe` blocks |
| Levels | One | No options |
| Model step size | One value, 2 | Trying 4 as well was worth 77 bytes in 1.68 GB |
| Model stored in a chunk | Removed; models only in the models block, also for one-chunk files | Costs 0.004 %; one model path |
| The next ten stream models | Keep | Dropping all ten costs 2.70 % |
| Container fields | Fixed width: 16-byte chunk header (17 until step B), 4-byte models length | Costs 0.005 %; no variable-length integers to decode there |
| Encoder self-check | Always on; the switch is removed (2026-11-21: `Config::verify`, the tool's undocumented `--no-verify`; `Config` is a thread count). Supersedes "the switch stays" of 2026-11-20 | Every file written is proven to decode; since the criterion change nothing but the undocumented flag read the switch, and the benchmark times compress with the self-check on; compress takes no options |
| Float origins `te.f`, `snd.f` (open question 13, 2026-11-21) | Stay blobs | 2.4 % of the output, all in the 457 of 2,000 demos with float coordinates; modelling them is estimated at 0.31 % of the output for two more streams every port must match bit for bit |
| Edge inputs (open question 14, 2026-11-21) | Specified as the code is. A demo ending inside a frame: the partial bytes are the `TAIL` of its segment, the rest of that segment is transform-coded. Input that does not start like an MVD: refused by the library (`check_demo`, `NotMvd`) and the tool; the unchecked entry point and `--allow-non-demo` write it. Bytes that do not frame are tail; where they run past 1 MiB the rest is cut every 1 MiB and each piece is walked afresh, so frames found later are coded | Both already round-trip and are tested (`truncated_demo_round_trips`, `empty_and_tiny_inputs`, `non_mvd_input_streams`, `non_demos_are_refused`, vectors `duel-truncated`, `random.normal`); the alternative, no frames after the first unframeable byte, needs a code change for a slightly simpler sentence |
| Per-chunk checksum | Keep | Subsets of a demo are decoded for analytics |
| Checksum algorithm | The CRC-32 of zlib (73bd49b) | In every standard library that has DEFLATE; `crc32fast` was already in the build; 80 lines of hand-written CRC-32C gone, one `unsafe` function with them |
| Checks on decode | Both stay: each chunk's CRC-32 and the whole demo's (decided 2026-11-20) | The whole-demo check is the only one that catches chunks in the wrong order; the chunk check serves subset decodes and reports an error at the chunk |
| SHA-256 on decode | Never recomputed; the trusted mode, `ReadOptions` and the C `flags` argument are gone | 9.1 % of a 4-thread decode with SHA instructions, 19.7 % of a batch decode without; a decoder needs no SHA-256 |
| Size, duration, hashes | Fixed 64-byte header at the head | One read; completeness without an end record |
| Checksum and SHA-256 in the header | Both stay (decided 2026-11-20) | Cheap to write: for a 10 MB demo the checksum is 0.8 ms and SHA-256 4.5 ms (28 ms without SHA instructions), once, at compress time |
| Dependencies | `serde` and `serde_json` dropped from the library (JSON written by hand); `sha2`, `crc32fast` and `miniz_oxide` remain (`flate2`, a wrapper around `miniz_oxide`, replaced by it, 2026-11-21) | 11 crates in the default build, from 24 |
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
chunk  := len u32 | raw_len u32 | dur_ms u32 | crc u32 | coded[len]                 (16 bytes, then coded)
coded  := fte u32 | fte2 u32 | mvd1 u32 | tail u32 | n u32
          | n x { id u32 | count u32 | [len u32 if id is not a blob stream] }
          | blob_len u32 | stream bodies in table order | blob body (blob_len bytes)
stream body := rans_len u32 | rans[rans_len] | extra (rest of the body)
models body := n u32 | n x { id u32 | len u32 | model[len] }             (ids strictly increasing)
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
- One chunk coding, the transform. A chunk the transform cannot code is written with
  `tail = raw_len`: no frames, every byte in the `TAIL` blob stream.
- A chunk: protocol state, a stream table, the stream bodies, then one raw
  DEFLATE stream holding all blob streams in table order (no zlib or gzip wrapper, no
  preset dictionary; it decodes to exactly the sum of their byte counts and nothing
  follows its final block). A modelled stream is rANS with the file's model; its table
  entry is id, count, length (a blob stream's: id, count). A modelled stream without a
  model in the models block is an error.
- Each chunk decodes on its own, given the models block. Protocol state at the chunk
  start is in the chunk, so map changes decode as before.
- KTX demoinfo blocks stay in their chunk; the shared demoinfo store is gone.
- Limits: a chunk is at most 1 MiB raw and its coded data (`len`) at most 2 MiB (step A;
  the ratio rule of 1024 times plus 4096 is gone); the models block is at most 16 MiB.

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

All Viktor's to decide. Of those left, 9 and 12 can change the file format.

1. Fallback paths: decided and implemented, see "One fallback".
2. The whole-demo CRC pass: decided, it stays (see Decisions).
3. Checksum algorithm: decided, the CRC-32 of zlib (see "Checksum: CRC-32").
4. The stream table inside a chunk: decided 2026-11-21, every integer a u32 (see
   Decisions, "Integers inside a chunk"; measured under "Format decisions of
   2026-11-21").
5. The C interface: decided, removed (see Decisions).
6. Encoder self-check: decided 2026-11-21, always on, the switch removed (see
   Decisions).
7. Gzip input (`.mvd.gz`) accepted by compress? Decided: no (see Decisions).
8. Thread count: decided and implemented (see Decisions).
9. Bits that can be flipped without changing the decoded demo: 784 of 30,696 in the
   probe's small file (256 are the header's SHA-256, 80 in the models block, 448 in the
   chunks); never with a different demo. Accept, or require every flip to be refused.
10. Optional features: the built-in profiler is removed (`sampler.rs`, the `sampler`
    feature and the optional `libc` dependency; `perf` does its job). The `mimalloc`
    feature is removed too (2026-11-21): no speed gain on glibc and 2-4 times the
    memory; the static musl release builds now use musl's allocator (slower: not
    measured on this branch).
11. Compression speed further: the walk is single-threaded (17.7 % of compression before
    the speed changes; not profiled since). Peak memory rose with changes 4 and 5.
12. Decided 2026-11-21: every stream model stays (see Decisions and "what each
    stream model is worth, rebuilt codec" under Measurements). `uc` and `uc.a` cannot
    be measured on the archive (no usercmd blocks) and stay.
13. Decided 2026-11-21: `te.f` and `snd.f` stay blobs (see Decisions).
14. Decided 2026-11-21: specified as the code is (see Decisions); the FORMAT.md text
    is written with the rewrite (Remaining implementation 2).

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
  encoder in. Without the encode export it is 662 bytes smaller. (The C interface has
  since been removed: a decoder-only size can now be measured from a wrapper that
  exports decoding alone.)
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

### Profile and the table search (2026-11-21, on dbc533d, not committed)

`perf` on the first 120 demos of the sample (399 MB), one thread, the command-line tool.

Decompress, share of CPU time before the change: numeric groups (`Group::run`, with the
rANS decoder inlined) 41.9 %, walk (`Walker::command`) 24.5 %, the walk's closure in
`segment::decode` 9.4 %, building the models (`IntModel::get`, `read_tables`,
`Table::from_q`) 8.0 %, inflate 0.8 %, CRC-32 0.5 %. Compress: the self-check's decode
31 %, `pack_body` 8.9 %, `numerics` 7.9 %, `num_syms` 6.6 %, the encode walk 6.3 %,
DEFLATE 4.1 %, training (`train`, `Tally::add`, `Table::from_q`) 7.9 %, SHA-256 1.7 %.

Found: by source line, 11.1 % of decompression was `count_ones` in the rANS decoder's
table search (`count_le`). Plain x86-64 has no population-count instruction, so it
compiled to bit arithmetic. Now the compare results are summed as vector lanes (SSE2
only), with no population count. Same output.

| Measure (500 demos, `bench -r 3 -j 4`) | CRC-32 build (73bd49b) | Table search without a population count |
|---|---|---|
| Compress 1t | 36.3 [36.1-37.1] | 38.5 [38.3-38.8] |
| Compress 4t | 93.1 [92.3-97.0] | 97.5 [95.0-98.2] |
| Compress batch | 140.2 [138.6-140.6] | 142.7 [141.5-144.8] |
| Decompress 1t | 87.7 [86.5-88.0] | 99.8 [99.4-100.8] |
| Decompress 4t | 312.2 [296.4-313.1] | 347.6 [346.5-349.3] |
| Decompress batch | 343.0 [318.0-347.1] | 386.9 [359.2-390.2] |

- Decode CPU time on the 120 demos: 4.55 s before, 3.95 s after; 3.94 s for the old code
  built with `-C target-feature=+popcnt`, so the whole gain is had without the
  instruction.
- Candidates not done: `num_syms` runs twice for each numeric stream (training and
  coding; 6.6 % of compression together); building the models is 8 to 9 % of a
  one-thread decode, a fixed cost per file; the decode profile is otherwise flat (no
  source line above 4.4 %).

### Where the time goes at 4 threads (2026-11-21, table-search change included)

The command-line tool with `-j 4`, one file at a time, the first 120 demos (399 MB):
compress 5.41 s with 2.3 of 4 cores busy, decompress 1.93 s with 2.4. Wall-clock share
from `perf` samples in 2 ms slots (`~/dev/mvdz-bench/scripts/stages.py`; compress
recorded with `--call-graph dwarf`). File reading and writing and process start are in
these figures; `mvdz bench` works in memory and leaves them out.

| Compress | Wall | CPU | Threads |
|---|---|---|---|
| Walk | 20.8 % | 5.6 % | one |
| Self-check (numeric 10.1, walk 8.5, build models 4.6, rest 1.1) | 24.3 % | 34.0 % | all |
| Memory (page faults, allocator), process start and exit | 14.6 % | 10.1 % | mixed |
| Encode (rANS 6.2, DEFLATE 3.7, making symbols 2.2) | 12.1 % | 18.4 % | all |
| Train (build models 4.6, count 2.4, making symbols 2.1) | 9.1 % | 15.3 % | all |
| Numeric predictors | 7.1 % | 11.9 % | all |
| File reading and writing | 5.1 % | 1.8 % | one |
| SHA-256 | 2.6 % | 0.9 % | one |
| CRC-32, other, idle | 4.3 % | 2.0 % | |

| Decompress | Wall | CPU | Threads |
|---|---|---|---|
| Numeric streams | 26.4 % | 29.7 % | all |
| Walk (structural streams, writing the bytes) | 21.2 % | 31.1 % | all |
| File reading and writing | 20.5 % | 5.2 % | one |
| Memory, process start and exit | 12.7 % | 9.0 % | mixed |
| Build models | 10.8 % | 18.7 % | all |
| Inflate of the blobs | 1.8 % | 3.1 % | all |
| CRC-32 (whole demo 1.0, chunks 0.1) | 1.1 % | 0.6 % | |
| Other, idle | 5.6 % | 2.5 % | |

### Cheap wins tried (2026-11-21, not committed)

On the 120 demos with `-j 4` (wall, median) and the 500 with `bench -r 3 -j 4`.

- Done: the demo's CRC-32 and SHA-256 are computed on a second thread while the walk
  runs (only with more than one thread). Compress on the 120 demos 5.50 s to 5.25 s;
  on the 500 demos Compress 4t 97.5 [95.0-98.2] to 101.5 [100.0-103.7] MB/s. Same
  files. One thread and batch are untouched.
- No gain: the `mimalloc` allocator on glibc. Compress 5.50 s to 5.71 s, decompress
  1.92 s to 2.45 s.
- Found, a behaviour question: the tool calls `fsync` twice for each output file (the
  file, its directory): 2.9 ms a file on this NVMe disk, 0.34 s of the 1.92 s of
  decompression (18 %) and 0.32 s of the 5.5 s of compression (6 %). gzip and zstd do
  not sync by default; xz does since 5.8, with `--no-sync` to turn it off.
- Not done, larger: run the numeric predictors of a chunk while the next chunk is
  walked (the predictors are 7.1 % of compression wall time, the walk 20.8 % on one
  thread); a symbols-only pass for training (`num_syms` also packs the mantissa bits
  there, unused; about 1 %).

### No unsafe code (2026-11-21, 3f737db)

The plan, measured 2026-11-21 on f0c2019 with a temporary `MVDZ_NOPIPE` switch (since
reverted). In-process: mean of 30 runs after 3 warm-up runs. CLI: hyperfine, 20
runs. One machine (12 cores), one demo per size from `/tmp/mvdz-strat/demos`.
"Split" means no pipeline, with segments spread over threads even when there are
fewer segments than threads (`outer = min(nseg, threads)`), which is what the code
would do without the pipeline:

| Demo, path | j1 | j4 now | j4 without pipeline |
|---|---|---|---|
| 16.1 MB (62 seg), whole-file | 145.4 ms | 40.3 ms | 41.2 ms |
| 16.1 MB, streaming (`Reader`) | 145.0 ms | 75.0 ms | 144.0 ms |
| 16.1 MB, CLI `mvdz decompress` | 171.0 ms | 103.9 ms | 175.9 ms |
| 664 KB (3 seg), whole-file | 7.7 ms | 4.0 ms | 6.2 ms; split 3.0 ms |
| 237 KB (1 seg), whole-file | 3.2 ms | 1.4 ms | 2.3 ms; split 2.4 ms |

- The pipeline is worth 1.9 times on one segment at 4 threads (145 to 75 ms
  streaming). Removing it alone makes the CLI as slow as `-j 1`.
- Segment parallelism beats it: whole-file at 4 threads (40 ms) is 1.9 times
  faster than the pipelined streaming path (75 ms) on the same file.
- Split beats the pipeline from 3 segments (3.0 against 4.0 ms). Only demos of
  1 or 2 segments (under 512 KiB) lose, about 1 ms at 1 segment (1.4 to 2.4 ms).
  How many archive demos are that small: unmeasured (the stratified sample is
  not representative of sizes).

What went (all 20 lines of `unsafe`):

| What | Before | Now |
|---|---|---|
| Inside a segment | Walker and numeric groups at once: spin-wait `RecBuf`, raw-pointer `Buf` shared by all | Walker, then the groups in order (`GROUPS`), each with the output as `&mut [u8]`; record lists are plain `Vec`s |
| Combined groups (`PiOAll`, `PiAAll`, `PeAll`) | Used by the pipeline | Removed: in sequence they decode no faster than the per-axis groups (at most 0.5 %, within noise; 32 demos, 1 thread, 3 runs) |
| Whole-file decoder | Segments over threads, or the pipeline when fewer segments than threads | Segments over threads |
| Streaming reader (CLI) | One segment at a time, threads in the pipeline | Reads up to `threads` segments, decodes them at once, hands them out in order; an error waits until the segments before it are read |
| `par_map` | Raw-pointer result slots | Each thread returns its results on join |
| rANS table search | SSE2 (`unsafe`) plus a `scalar` path | One loop over 16 counters that the compiler vectorizes |
| rANS table slice | `get_unchecked` | Checked slice |

Table search alternatives, decode of 32 demos (108 MB) at 1 thread, best of 10, two
runs each: SSE2 1017.8 / 1013.4 ms; 16 `u16` counters 1075.6 / 1076.7 ms (+6 %); the
old `scalar` path 1246.9 / 1255.0 ms (+23 %); 8 `i16` counters 1220.9 / 1216.4 ms
(+20 %). The SSE2 intrinsics need `unsafe` even though SSE2 is always on for x86_64.
Checked slice for `get_unchecked`: 1018.9 / 1016.6 against 1016.6 / 1012.8 ms (within
noise).

Result, old and new build alternated twice, cores 0, 2, 4, 6, silent machine, 104 s in
all (`/tmp/seqdec/harness/quick.sh`). Library figures over 32 demos of the stratified
set (108,606,042 bytes raw; plus a 1-segment and a 3-segment demo); CLI on single demos:
62 segments (16,107,150 bytes raw, 3,108,929 coded), 3 (663,865; 167,896) and 1
(237,314; 65,619). Medians of each run:

| Measure | Before | After | Change |
|---|---|---|---|
| Library decode, 1 thread | 1017 / 1020 ms | 1090 / 1087 ms | +7 % |
| Library whole-file decode, 4 threads | 368 / 363 ms | 386 / 387 ms | +6 % |
| Library streaming decode, 4 threads | 638 / 576 ms | 470 / 471 ms | -22 % |
| CLI, 62 segments, `-j 1` | 162 / 165 ms | 180 / 179 ms | +10 % |
| CLI, 62 segments, `-j 4` | 102 / 124 ms | 79 / 80 ms | -30 % |
| CLI, 3 segments, `-j 4` | 7.9 / 8.1 ms | 7.2 / 5.8 ms | faster |
| CLI, 1 segment, `-j 4` | 4.0 / 4.1 ms | 4.9 / 4.5 ms | +0.4 to 0.9 ms |
| CLI batch, 32 files, `-j 4` | 424 / 447 ms | 452 / 409 ms | no clear change |
| Compress, 4 threads | 1449 / 1493 ms | 1408 / 1440 ms | -3 %, within run spread |
| Peak memory, CLI `-j 4`, 62 segments | 6.6 / 6.7 MiB | 8.1 / 8.3 MiB | +1.5 MiB |
| wasm decode, 1 thread, node | 43.5 / 60.5 MB/s | 55.6 / 55.3 MB/s | not conclusive: the old build's runs differ by 39 % |

- Correctness: all 379 stratified demos compress to the same bytes as before and decode
  to their SHA-256 at `-j 1` and `-j 4`. New test `sequential_reader_threads_agree`:
  at 2 and 4 threads the reader yields the same bytes and error as at 1 on cut and
  damaged files (it fails when the waiting error is reported early).
- The pre-implementation timings above ran while the machine was in use; the result
  table did not.

### Encoder threads across segments only (2026-11-21, not committed)

Follow-up of "No unsafe code". Measured first with a temporary switch (`inner = 1`
always), old and new alternated twice, `-j 4`, cores 0, 2, 4, 6, silent machine, 32 s
(`/tmp/seqdec/harness/quick_enc.sh`). Medians of each run:

| Measure | Threads inside a segment when segments are few | Across segments only |
|---|---|---|
| Library compress, 34 demos | 1412 / 1416 ms | 1417 / 1400 ms |
| CLI compress, 62 segments (16.1 MB) | 213.5 / 213.3 ms | 213.3 / 214.9 ms |
| CLI compress, 3 segments (664 KB) | 17.9 / 18.1 ms | 18.0 / 17.7 ms |
| CLI compress, 1 segment (237 KB) | 9.6 / 9.8 ms | 11.1 / 11.2 ms |

- Gone: the `outer`/`inner` split, the `inner` argument of `train`, `Body::new` and
  `transform_body`, the `threads` argument of `segment::numerics` and
  `segment::encode`, and the biggest-first job order that balanced the stream packing
  over threads. The models are still built in parallel over streams (whole file).
- The 379 stratified demos compress to the same bytes as before the change.
- With it, not part of the change: the blob buffers of the segment decoder are plain
  slices (`Cow` was only ever borrowed); `compress_rt` and `gen_rt` (fuzz) compile
  again. Their check that the transform alone is lossless is gone with
  `Config::verify` (f0c2019): a transform bug now costs size, as the self-check codes
  the segment as all tail, but does not fail the fuzzer. Open: restore it through a
  hidden count of self-check fallbacks, or accept the loss. `cargo fuzz` is not
  installed here: the targets compile but did not run.
- Not done: sharing a helper between the encoder's self-check (`failing_segments`)
  and `decompress`. They share only the lookup of a segment body; the rest differs
  (one output buffer and the first error against a compare per segment).

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

### Format decisions of 2026-11-21 (mvdsv's writer and the archive)

What mvdsv can write (checkout 70f1135 of `~/dev/reference/mvdsv`, `src/sv_demo.c`,
`src/sv_demo_misc.c`, `src/bothdefs.h`, `src/server.h`; message types from
`~/dev/reference/qwprot`, the submodule directory in the mvdsv checkout being empty):

| Thing | mvdsv | Ours |
|---|---|---|
| Frame payload | 8,092 bytes (`MAX_MVD_SIZE` = 8192 - 100); a larger write stops recording | any 32-bit length |
| Whole frame | 8,102 bytes (10-byte `dem_multiple` header + payload) | no limit; a segment is cut at 1 MiB |
| Bytes per server frame | 32,408 accumulated (`_buf__data`) plus one player message of up to 8,098 | - |
| Frame time | one byte, capped at 255 ms; under 2 ms written as 0 | `dur_ms` u32 per segment |
| Demo size | `sv_demoMaxSize` default 20,480 KiB, 0 = unlimited | u64 `size`; `MAX_SEGMENTS` 65,536 |
| Frame types written | 3, 4, 5, 6 only (`dem_read` and `dem_set` sit in commented-out code) | 1 to 6 accepted |
| Demoinfo | the file read up to 200 KiB, split into blocks of 8,084 content bytes; more than four blocks in one server frame overflows the buffer (by reading, not run) | stays in its segment |

The archive (`/media/vikpe/qw_storage/demos_storage/all_demos/index.jsonl`, 293,929 MVD
files; `~/dev/mvdz-bench` and `~/dev/qwdemos` are not on this machine): median
2,032,160 bytes, p99 14,010,690, largest 67,109,197; longest 6,986,474 ms; 122 files
over 20 MiB, 1 over 64 MiB. Two samples, compressed with the encoder at 97bea21 (scripts
in `/tmp/mvdz-framescan`, not kept): random 500 (`random.Random(20261121)` over the
sorted MVD list; 1,418,792,446 bytes, 5,649 segments, output 278,553,572, 0.1963) and
extreme 25 (the 20 largest and 5 longest; 904,801,169 bytes, 3,460 segments, 0.0827).

| Measure | Random 500 | Extreme 25 |
|---|---|---|
| Largest frame | 8,102 | 8,102 |
| Largest segment raw | 270,004 | 265,008 |
| Largest segment coded (`len`) | 110,888 | 92,469 |
| Largest `raw_len / len` | 17.69 | 118.39 |
| Largest `dur_ms` | 267,395 | 499,384 |
| Largest models block | 36,006 | 47,608 |
| Segments coded as DEFLATE (coding 1) | 0 | 0 |
| Segments with an unframed tail | 0 | 0 |
| Stream table, share of output | 0.399 % | 0.728 % |
| Stream table fixed width, id u16 and the rest u32 | +0.457 % | +0.930 % |
| Stream table, every number u32 | +0.66 % | +1.31 % |
| Frame types 1 and 2 | present (488,560 and 27 frames) | - |

Also: of 367 files with demoinfo in the random 500, it starts before the last 256 KiB
segment in 25 and ends before it in 21, so "decode the last chunk" does not always find
it. Five damaged copies of one demo (truncated mid-frame, 100 bytes overwritten, 100
inserted, 2 MiB appended): none used coding 1; the overwritten and inserted copies did
not frame again, so the rest of the demo (911,582 bytes) became one tail in a
1,036,762-byte segment, and the output doubled. The "or exactly 1 MiB" exception in the
segment rule is therefore live for damaged demos and unreachable for intact mvdsv ones.

Decisions (Viktor, 2026-11-21): A. ratio rule out, `len <= 2 MiB` in; B. one chunk
coding; C. every integer in a chunk and in the models index a u32; the models limit
stays 16 MiB. Rule for limits: a repeated thing gets about 100 times the real maximum,
a single-place field may be generous if cheap, and where the simple choice clears the
limits with a wide margin, take the simple choice.

Step A (149c415): `SEG_RATIO`, `SEG_SLACK`, `ratio_ok`,
`seg_ratio_ok` gone; `MAX_SEG_CODED` (2 MiB) and `check_coded_len` in `format.rs`,
applied by both readers at the length field; the encoder's "stored blocks" second try
gone; `transform_body` refuses a body over the limit. Tests: `ratio_limit_fallback`
became `nop_flood_round_trips` (no fallback expected), the "4 GiB segment" hostile case
is now refused by DEFLATE rather than the ratio rule, a new case pads a segment to one
byte over 2 MiB, vector `segment-ratio` became `segment-length` (vectors not
regenerated). 41 pass, the known 4 fail; clippy clean.

Step B (d1a8c06) and step C: see their sections in the Handoff. After C: 40 pass (the
test `uv_limits` went with the variable-length integers), the known 4 fail; clippy clean.

### Known failing tests

Rule: between commits, no test may fail that is not on this list. On rc.4, 79 pass and
1 fails. After A9: 44 pass, 9 fail. Since 1ccd654: 43 pass, 9 fail (the test of unknown C
flags went with the flags argument). With one fallback: 44 pass, 9 fail (one new test,
of a model made smaller at the decision limit). Tests of removed features were deleted with them
(attachments, the zstd frame rules, the streaming writer, metadata, census, extension
keys, unknown sections, demoinfo sharing).

| Tests | Passed on rc.4 | Why they fail | Resolved by |
|---|---|---|---|
| `closed_stdout_is_quiet`, `non_demos_are_refused`, `outputs_named_after_inputs` (cli), `vectors_decode` | yes | read the vectors in `tests/vectors/`, written by rc.4 in the old format | regenerating the vectors after the last format change |

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

DEFLATE figures are miniz_oxide through `flate2` (since replaced by `miniz_oxide` directly: the same bytes). The wasm build is not measured. The
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

### Done: what each stream model is worth, rebuilt codec (2026-11-21, f0c2019)

Open question 12, at Viktor's request. Not the estimate above: a real build per stream.
The stream loses its model and its bytes join the segment's DEFLATE blob; encoder and
decoder agree, the self-check is on in every run, and the baseline binary refuses the
files (the stream id changed kind). Sample: stratified, 379 demos, 968,979,765 bytes
raw; baseline 211,736,432 bytes (step C figure, re-run first: identical). Done with a
temporary patch, not committed (`/tmp/mvdz-strat/plain-exp.patch`; `MVDZ_PLAIN=<ids>`
names the streams at run time, one binary for all runs; results
`/tmp/mvdz-strat/per_stream.tsv`, `per_stream_num.tsv`; `/tmp` does not survive a
reboot). A structural byte goes to the blob as itself; a numeric residual as 4 bytes
little-endian, so a numeric row says "model against a plain blob of its residuals", not
against a cleverer blob. Change in output if the model is dropped:

| Stream | Bytes | Change |
|---|---|---|
| `pi.ar` | -8,727 | -0.004 % |
| `uc`, `uc.a` | 0 | no usercmd block in the sample: unmeasurable here |
| `pe.fy`, `pe.fz`, `pe.fx` | +36,491 to +38,873 | +0.017 to +0.018 % |
| `pi.fz` | +43,392 | +0.020 % |
| `pi.fy`, `pi.fx` | +108,158, +110,546 | +0.051 %, +0.052 % |
| `te.type` | +177,961 | +0.084 % |
| `pe.more` | +264,432 | +0.125 % |
| `pe.ent` | +335,153 | +0.158 % |
| `pi.wframe` | +423,900 | +0.200 % |
| `st.idx` | +430,227 | +0.203 % |
| `pe.a` | +466,698 | +0.220 % |
| `st.val` | +813,076 | +0.384 % |
| `snd.num` | +977,023 | +0.461 % |
| `type` | +996,013 | +0.470 % |
| `snd.ch` | +1,025,640 | +0.484 % |
| `pe.bits` | +1,555,026 | +0.734 % |
| `time` | +2,374,790 | +1.122 % |
| `snd.o` | +3,161,697 | +1.493 % |
| `pi.frame` | +3,289,888 | +1.554 % |
| `cmd` | +3,629,005 | +1.714 % |
| `te.o` | +4,349,487 | +2.054 % |
| `pi.flags` | +4,717,916 | +2.228 % |
| `pe.oy`, `pe.oz` | +6,927,806, +7,016,983 | +3.27 %, +3.31 % |
| `pe.ox` | +8,284,492 | +3.91 % |
| `pi.oz` | +9,061,010 | +4.28 % |
| `pi.oy`, `pi.ox` | +16,759,962, +16,845,469 | +7.92 %, +7.96 % |
| `pi.ap` | +18,260,513 | +8.62 % |
| `pi.ay` | +22,240,480 | +10.50 % |

- Only `pi.ar` is at or below zero, so the "every stream at or below zero together"
  build is that row. It is the roll axis of the player angle: rarely sent, so its
  stream is a fixed cost per segment plus a model per file.
- The float origins (`pi.f*`, `pe.f*`) are worth 0.017 to 0.052 % each on this
  sample; how many of the 379 demos use float coordinates was not counted. The
  earlier estimate gave 7.5 % for all six on the 2,000 (457 float demos), so the
  figure depends on the share of float demos.
- `uc` and `uc.a`: output identical to the baseline in both runs, which confirms no
  demo of the sample has usercmd blocks.
- First attempt failed silently: the diverted numeric bytes sat in a blob the walker
  never read, the end-of-walk "every blob consumed" check failed, and the self-check
  wrote every segment as all tail (output 2.8 times the baseline, decodable by the
  baseline binary). Caught by the identical sizes for `pi.ox`, `pi.oy`, `pi.oz`. The
  numeric rows above are from the corrected binary; the structural rows from the
  first, whose structural path was the same (unset, both give the baseline bytes).

### To do

Not measured yet:

1. The size of a decoder-only wasm build (the C interface is gone: measurable now).
2. `uc` and `uc.a` against blobs, on demos with usercmd blocks.
3. XXH3 without SIMD from the reference C code (a download was blocked; the WebAssembly
   run without SIMD stands in for it).
4. rayon on the final code (optional).
5. The count of bits that can be flipped unnoticed, after the fallback change (the
   probe was lost with `/tmp/blobplan`).

## Suggestions (2026-11-21)

Not started. Each was checked to exist in the code; effects are estimates unless a
number is given.

### Guideline steps left (slipgate `AGENTS.md`, `rust/AGENTS.md`)

1. Naming, module by module. The code should be obvious: spell names out unless the
   abbreviation is well known (`crc`, `id`, `ms`). Examples: `dur` to `duration`,
   `durs` to `durations`, `Sec`/`sec` to `Section`/`section`; the same goes for `seg`,
   `hdr`, `wk`, `st`, `gi`, `sid`, `ctx`, `cfg`, `tm`, `nums`, `recs`, `lay`, `chk` and
   their kind, in variables, fields, functions and types. Also: about 620
   single-letter names, about 20 booleans without an `is_`/`has_` prefix, 7 bit
   constants written as hex. Delete each comment the new
   name makes redundant (about 800 of 7,680 source lines are comments). Removes the
   last temporary allow, `many_single_char_names`. No logic changes in the same commit.
2. `unsafe`: 20 occurrences (walker 9, predictors 6, parallel map 3, rANS 2). Not linted
   yet. Decide: hard rule, or allowed where a benchmark shows it pays. One site at a
   time, measured.
3. `main.rs` allows `clippy::exit` for the whole file: return `ExitCode` from `main`.
4. `pretty_assertions` in tests.
5. Truncating casts: 86 in the library under one stated allow. About 37 are positions
   narrowed from `usize` to `u32`; a walker cursor held as `u32` would remove them. Hot
   loop: measure.

### Out of the library (not the codec)

1. Atomic file writing (`write_file_atomic*`, `replace_locked`, `temp_name`, `gid` in
   `api.rs`, about 110 lines, Unix-specific parts): move to `main.rs`.
2. JSON rendering (`FileInfo::to_json*`): only `mvdz info` uses it. Return the struct,
   print in the tool.
3. Done (2026-11-21, not committed): `mvdz bench` removed from `main.rs` (its
   options `-r`, `--files`, `--streams`, `--skip-compress` with it; `--no-verify`
   stays, `compress` reads it). In its place `benches/codec.rs`, criterion 0.8 as a
   dev-dependency without default features (no plotters, no rayon). Groups
   `compress` and `decompress`, each `4t` and `1t`; one iteration codes every demo
   once, one file at a time, self-check and every decode check on; throughput in MB/s
   (10^6 bytes); flat sampling, 10 samples, 15 s. Demos: every `*.mvd` in the
   directory `MVDZ_BENCH_DEMOS` (like the tests' `MVDZ_TEST_DATA`); unset, it measures
   nothing. Before timing it compresses each demo and checks the round trip. Compare
   builds with `cargo bench --bench codec -- --save-baseline a` and `-- --baseline a`.
   Size and peak memory stay outside criterion. Still open: which demos (set8 of
   `benchmarks/samples/` is 18.9 MB, the old speed-table set; 6 of its 8 are in the
   download); `Info::streams`, `util::par_map`, `streams::name` and `approx_f64`
   no longer need to be public for the tool (`Info::streams` has no reader left).
   First run (11 random archive demos, 36,183,640 bytes raw, in `/tmp/mvdz-bench-demos`,
   not kept; MB/s, criterion's estimate): compress 4t 88.2, 1t 33.6; decompress 4t
   341.4, 1t 104.5. A second run against that baseline: all four within 1 %, flagged
   "no change" or "within noise". One run takes about 2 minutes.

### Paths that exist for tests or experiments

1. Done (2026-11-21, decided by Viktor): `Config::verify` and `--no-verify` removed;
   `Config` is a thread count; the self-check always runs. Output unchanged (one demo
   checked byte for byte; the switch never changed the file).
2. Done (2026-11-21, not committed; Viktor chose "one segment size everywhere"):
   `compress_segmented` is gone, `compress_untransformed` and `cut_points` lose
   their size parameter, `cut_points` cuts at `SEGMENT` only. Tests that need
   several segments repeat a synthetic demo past 4 x 256 KiB (`several_segments` in
   `tests/common`); the `Cfg` test type is gone. `examples/sweep.rs` drops `-s` and
   checks the 256 KiB file only; the fuzz target `gen_rt` loses its segment byte
   (old corpus inputs read differently). Vectors: `duel.normal.seg64k` removed,
   `usercmd.normal.seg8k` is now `usercmd.normal.segments`; `duel.untransformed`,
   `demoinfo-multi` and the hostile bases now use 256 KiB segments (to regenerate
   with the others). `r_m4_section_bookkeeping` builds its 60,000-segment file by
   hand (one encoded one-frame segment repeated). `c_m1_segment_limit` removed: the
   encoder's `MAX_SEGMENTS` refusal needs a 16 GiB demo now, so it is untested (the
   check stays in `compress_inner`). Tests: 40 pass, the known 4 fail (44 tests, was
   45); with 11 archive demos as `MVDZ_TEST_DATA/sample` the same, and `sweep` round
   trips all 11. The tests that need the duel sample still skip on this machine.
3. `TABLE_BITS_PER_SYM` (`rans.rs`): a global atomic nothing writes. Make it a constant.
4. Done 2026-11-21: the `scalar` feature is removed with the SSE2 table search (see
   "No unsafe code" under Results per change).

### Structure

1. Eight `#[doc(hidden)] pub` modules (`coder`, `deflate`, `encode`, `format`, `proto`,
   `segment`, `streams`, `util`) are public only for tests that build files by hand.
   Moving those tests into the crate as unit tests lets every module be private.
2. Done 2026-11-21 (not committed): decode-side concurrency removed with every
   `unsafe`. See "No unsafe code" under Results per change and Decisions.
3. The work budget (`Limits::work_per_byte` and its accounting): with segments at
   1 MiB and models at 16 MiB the work a file can demand is bounded by its size. Maybe
   only the memory limit is needed. Check what the hostile-file tests rely on first.
4. Pass 2 of the encoder is an inline closure with block comments; pass 1 is a named
   function (`train`). Name pass 2.

### Format, enabled by the 1 MiB cap

1. `MAX_SEGMENTS` (65,536) is now only a demo-size limit: state it as 16 GiB.
2. The fallback tries DEFLATE level 9 and falls back to stored blocks when the ratio
   rule breaks. With the cap one rule may do; whether the ratio rule is still needed
   for a 1 MiB segment is not worked out.
3. FORMAT.md still gives the old segment limit (4 GiB) and the old rule (only the last
   segment may end inside a frame). New rule: a segment is at most 1 MiB; every
   segment but the last is whole frames or exactly 1 MiB.
4. Open since the two-pass encoder: may the models block list a model no segment
   uses? The decoder has no check against it (not tested). Saying yes in FORMAT.md
   keeps an encoder free to write segments as it codes them.

### API gaps found on the way

1. Part of a file (a time range, the last segment for ktxstats) cannot be decoded from
   outside the crate since `decode` became private. If wanted: one public function,
   and each segment's starting protocol state exposed for parsers.
2. `Reader::header` is public and has no test.
3. No regression test for the `Budget::unlimited()` bug (work limit of 2^20 with no
   input recorded; a demo with large models failed the self-check). Needs a demo whose
   models take more than 2^20 decisions to build.

### Possible later, larger

1. Streaming input for the encoder (read the demo twice instead of holding it): peak
   drops by the raw size. 250 MB works today at about 410 MiB.
2. Done 2026-11-21 with Structure 2: the streaming reader the CLI uses decodes
   `threads` segments at a time. (Was: single-file decode at several threads slower since
   the CLI streams, 282 MB demo at 4 threads 0.80 s to 1.40 s; that demo is not
   re-measured.)

### Measuring

`~/dev/mvdz-bench/scripts/compare4.sh` (not in the repository): 100 demos from
`rand500`, gzip, zstd and mvdz, speed, ratio, peak memory, output hash. Use
`CPUS=0,2,4,6` (four physical cores; `0-3` is two cores with hyperthreads) and
`CODECS=mvdz MVDZ=<binary>` for one build. The machine flips between two speed levels
about 6 % apart (about 123/378 and 117/357 MB/s encode/decode): compare two builds by
alternating runs, never by one run each. Output hash of the 100 files so far:
`6b17ab94f6e5c4c2`. Criterion (`benches/codec.rs`, see "Out of the library", 3) now
does the library speed part; this script still gives size, peak memory and the
command-line speed.

Order suggested: out-of-the-library 1 and 2, then tests-and-experiments 1 and 3, then
naming.

## Remaining implementation

After the format decisions (open questions 4, 9, 12, 13):

1. Regenerate the vectors in `tests/vectors/`; hostile vectors for the header, checksums
   and chunk sums are already generated by `tests/vectors.rs`.
2. FORMAT.md: rewrite the container (§2); delete metadata, index, demoinfo sharing,
   attachments and streaming (§3, §4, §5.3, §10, §11) and what it says of the C
   interface; adjust the rest. README passages that still point at removed sections.
   Also, from open question 14: §12 rule 1 ("once a frame does not parse, the rest of
   the demo is the last segment") becomes the 1 MiB cut with each piece walked afresh;
   §12 rule 5 (the zstd/stored fallback) becomes all tail; state that input not
   starting like an MVD is refused by encoders; the consumer note "decode the last
   chunk" becomes "scan back from the last segment".
3. BENCHMARKS.md from the final measurements.
4. Later, from the settled specification: implementations in C, Go and other
   languages, with a test suite they share.

## Verification

- Every benchmark sample round-trips byte for byte (2,000 of 2,000 in every run so far).
- A second, independent decoder agrees on every vector (once one is written from the
  settled specification; the Go decoder that did this for rc.4 is removed).
- Refused: truncated file, dropped or swapped chunk, trailing bytes, a flipped bit in
  a chunk, wrong header size, duration or hash.
- Refused: a chunk's blob data that decodes to the wrong length, has bytes after its
  final block, or is not valid DEFLATE.
- Ratio compared with rc.4 at `-s 256K` (0.218 of raw on the stratified sample).
