# Byte-identical encoders (C, Rust, Go)

Goal: an encoder in any language writes the same `.mvdz` bytes for the same demo, so one
vector set checks every port. Status: format version 3 is implemented in Rust and in Go
(encoders, decoders, vectors, `FORMAT.md`). The two encoders write the same bytes for all
387 local demos (set8 and the stratified sample), the vectors and the edge cases tried.
Open: timing against version 2.

Measured on 387 demos (set8 and the stratified sample, 987 MB raw): version 2 217,616,648
bytes, version 3 216,586,623 bytes (-0.47 %). Of that, the LZ stage against DEFLATE is
-0.26 %, the meta-coded models block +0.08 %, and the own-table cost of 48 + 6 bits per
symbol (was 16 + 5; the best of 22 points swept) -0.29 %.

## What is already pinned

Section 8 of `FORMAT.md` fixes cut points, protocol state carry, context selection order,
model drop order, tail fallback, stream order and thread independence. These are
integer-exact: `quantize`, `normalize` (u128), the rANS encoder, CRC-32, SHA-256,
`duration_ms`, every sort (total keys).

The only float arithmetic is the usercmd angle grid (`math.rs:on_grid`, `grid_f32`). It
reproduces the protocol's 16-bit angle (`grid * 45/8192`, `SHORT2ANGLE` in qwprot
`protocol.h`) with single IEEE operations: f32 to f64 (exact), `* 8192` (exact), one
division, one f32 multiply. Portable in Go (no `a*b + c` to fuse) and in C on SSE
targets (64-bit, or `-mfpmath=sse`; never x87). No value gets more precision than the
wire format carries.

## What blocks identity today

1. **DEFLATE** (models block entries, section 6.2; blob body, section 6.6). Pinned as
   "`miniz_oxide` level 9", not as an algorithm. Go `compress/flate` emits other bytes;
   miniz C vs `miniz_oxide` parity is unverified. Any difference moves the models block
   and every blob body.
2. **f64 `log2` in model selection** (`rans.rs:entropy_bits`, `cross_bits`,
   `select`). libm last-ulp differences (glibc, musl, MSVC, Go `math.Log2`) can flip
   `own_cost < cross_bits` on a near-tie. One flip changes a table, which changes every
   segment's rANS bytes. `mul_add` is fused in all three languages and is not a problem.

## Decisions

- Replace DEFLATE with an LZ77 stage coded by the existing static rANS coder. Byte-identical
  by construction, no third-party codec in any port. Format version 3.
- Integer (fixed-point) cost in model selection. Encoder rule only; decoders unchanged.
- Vectors regenerate once; the Go decoder must agree on every one.

Alternatives considered: pin miniz `tdefl` (Go port of ~1500 lines, parity test needed);
a canonical fixed-Huffman DEFLATE encoder in the spec (~150 lines, keeps stdlib inflate,
but a third coder in the format and a worse ratio on blobs); identity outside the blobs
only (no file-level identity). Rejected for the version-3 route: token expansion is ~30
lines on top of the coder every port already has.

Kept for robustness, not up for simplification: the stored segment (the only hard size
bound once DEFLATE's stored blocks are gone), the distance/length checks, the `raw_len`
check before any model allocation, the encoder self-check, the fuzz targets, and the Go
decoder agreeing on every vector.

## Against master (format 1, rc.4)

Master's bytes depended on libzstd 1.5.7 at four levels across segments, blobs, META,
attachments and untransformed segments; on store-vs-zstd choices decided by zstd's output
size; on the same f64 `log2`; on a runtime knob (`TABLE_BITS_PER_SYM`); on two encoder
paths (one-shot with shared tables, streaming with per-segment models); and on census JSON
and metadata rendering. No libzstd-identical encoder exists in Go. Format 2 removed all of
that except DEFLATE and the f64 `log2`; format 3 removes those.

## Format changes

### Stream ids (section 3.2)

One range per kind; the three LZ streams join their kind and every later id shifts.

| Id | Name | Kind | Alphabet |
|---|---|---|---|
| 0-15 | as today | structural | 256 |
| 16 | `lz.lit` | structural | 256 |
| 17-34 | the numeric streams, shifted by +1 (`PI_OX` 17 .. `UC_A` 34) | numeric | 65 |
| 35 | `lz.len` | numeric | 65 |
| 36 | `lz.dist` | numeric | 65 |
| 37.. | blobs, shifted by +3 (`MASK` 37 .. `GEN_BASE` 66, `STREAM_COUNT` 322) | blob | - |

Kind by range: 0-16 structural, 17-36 numeric, 37-321 blob.

### LZ token streams replace the blob body (section 6.6)

The blob streams of a segment, in table order, still form one byte string of
`blob_total` bytes (sum of blob counts). It is produced from tokens before the walk:

```
produced = 0
while produced < blob_total:
    len = lz.len.get(ctx_len)                      // numeric value, >= 0
    if len == 0:
        byte = lz.lit.get(ctx_lit); out.push(byte); produced += 1
    else:
        dist = lz.dist.get(ctx_dist)               // 1 <= dist <= produced, len <= blob_total - produced
        copy len bytes from out[produced - dist ..], byte by byte (overlap allowed)
        produced += len
```

Contexts (16-bit):

| Stream | Context |
|---|---|
| `lz.lit` | previous output byte, 0 at start |
| `lz.len` | `min(bucket(previous len value), 63)`, 0 at start |
| `lz.dist` | `min(bucket(len), 63)` |

`bucket` is `residual_bucket` (section 5.2). `len < 0`, `dist < 1`, `dist > produced` or
`len > blob_total - produced` is Corrupt. With `blob_total == 0` the three streams are
absent. Completeness as for any stream (section 6.5): every token consumes a symbol, so
over-reading fails.

Transform header: the `blob_len u32` field is removed. Blob entries stay `{id, count}`;
the LZ streams are ordinary coded entries `{id, count, len}` and sort before the blobs.
One count rule for every stream: `count <= 2 * raw_len + 16` (the `lz.lit` and `lz.len`
counts are bounded by `blob_total <= 2 * raw_len`).

### Stored segment (section 3.1, new)

A transform header with `n = 0` is a stored segment: `tail = raw_len` and exactly
`raw_len` raw bytes follow the 20-byte header. Needed because an LZ-coded all-tail segment
has no size bound (a literal can cost up to 12 + 12 bits); DEFLATE's stored blocks gave one.
The encoder uses it only when all-tail would exceed 2^21.

### Models block (sections 2.2, 6.2)

```
body      := meta | n u32 | n x { id u32 | raw_len u32 | len u32 | stream[len] }
raw_model := own_count u32 | own_count x u16le (strictly increasing) | (own_count + 1) x alphabet bytes
meta      := raw_model, alphabet 256, read as is
stream    := rans_len u32 | rans            // a structural stream body, extra must be empty
```

Entry `i`'s stream decodes, under `meta`, exactly `raw_len` symbols with context = the
previous decoded byte of that model (0 first). The bytes are that model's `raw_model`,
alphabet from its id kind. `raw_len` must equal `4 + 2 * own_count + (own_count + 1) *
alphabet`; `own_count` is the first 4 decoded bytes, so a reader can bound the rest before
allocating. `own_count <= 65536` as before. Ids strictly increasing, structural or numeric
(the LZ streams included). Per-model streams keep the parallel model build.

The `raw_len` of all entries and the meta model's raw length sum to at most 2^24 (16 MiB):
a format rule, so every decoder accepts the same files. The encoder drops models until
this and the coded block limit both hold (`fit_models_block`).

### Validation (section 7)

DEFLATE rules replaced by: the LZ token rules, the stored-segment length rule, the models
block raw/coded length rules. Limits, each a "must": `count <= 2 * raw_len + 16`; models
block at most 2^24 bytes coded and 2^24 bytes raw; blob total at most `2 x raw_len`. Header: `version = 3`, 60 bytes unchanged.

## Encoder rules (section 8)

### Rule 4: integer own-table criterion

```
log2_fixed(n) -> u64, n >= 1, 16 fraction bits:
    int = ilog2(n)
    x = n normalized to [2^30, 2^31)          // shift left or right by |30 - int|; u64
    frac = 0
    repeat 16 times:
        x = (x * x) >> 30                     // x * x < 2^62: fits u64, no u128
        frac <<= 1
        if x >= 2^31: x >>= 1; frac |= 1
    return int << 16 | frac

entropy(counts) = sum over count > 0 of count * (log2_fixed(total) - log2_fixed(count))
cross(counts, ref) = sum over count > 0 of count * (log2_fixed(ref_total) - log2_fixed(ref[s]))
                     ref[s] == 0 with count > 0: infinite, always own (the fallback cannot code it)
own table if entropy + ((16 + 5 * present) << 16) < cross
```

`log2_fixed` is monotone, so every term is non-negative. Same shape as the f64 code, so
vectors move only on near-ties. The meta model uses table cost `(16 + 8 * 256) << 16`
(raw tables); `select` takes the table cost as a parameter.

`lz.*` models are never dropped by `fit_models_block` (bounded: at most 257 + 2 x 65
tables).

The models are built from every segment whose walk succeeds, also one that pass 2 then
writes all tail or stored: pass 1 does not depend on pass 2. A stream is coded only if
every symbol has a code under its model; otherwise the segment takes the next fallback of
rule 5.

### Rule 5: segments

Transform-coded unless the walk fails, a model is missing, or the coded length exceeds
2^21; then all-tail (the `TAIL` blob through the LZ stage); if that also exceeds 2^21,
stored.

### Rule 10: canonical LZ parse

Encoder only; a decoder accepts any token sequence.

```
MIN_MATCH = 4, HASH_BITS = 15, MAX_CHAIN = 32          // freeze after the size sweep; MAX_CHAIN = 1 drops prev[]
hash(pos) = (u32le(bytes[pos..pos+4]) * 0x9E3779B1) >> (32 - HASH_BITS)
head[2^HASH_BITS] = NONE; prev[len]
pos = 0
while pos < len:
    best_len = 0; best_pos = NONE
    if pos + MIN_MATCH <= len:
        cand = head[hash(pos)]; steps = 0
        while cand != NONE and steps < MAX_CHAIN:
            l = common prefix length of bytes[cand..] and bytes[pos..], at most len - pos
            if l > best_len: best_len = l; best_pos = cand       // strict: nearest wins ties
            cand = prev[cand]; steps += 1
    if best_len >= MIN_MATCH:
        emit match(best_len, pos - best_pos); insert pos .. pos + best_len - 1; pos += best_len
    else:
        emit literal bytes[pos]; insert pos; pos += 1
insert(p): if p + 4 <= len: h = hash(p); prev[p] = head[h]; head[h] = p
```

Greedy, no lazy match, no length cap. `ponytail:` lazy matching and chain length are
ratio upgrades; they move every vector, so they change only with a version bump.

Appendix A: `write_model` writes a raw model; the models block writer tallies every raw
model's bytes (previous-byte contexts) into the meta model, then codes each.

## Implementation order (Rust, `mvdz_rust/mvdz/src`)

1. `rans.rs`: `log2_fixed`; `entropy_bits`, `cross_bits` to u64; `select` with a
   table-cost parameter; `write_model` raw; `read_model` from raw bytes; models block
   writer/reader with the meta model (`Tally`, `encode_bytes`, `ByteDecoder` reused).
2. `streams.rs`: `LZ_LIT = 16`, numerics 17-34, `LZ_LEN = 35`, `LZ_DIST = 36`,
   `BLOB_BASE = 37`, `STREAM_COUNT = 322`; kind predicates stay three range checks.
3. New `lz.rs`: `parse(bytes) -> tokens`, `expand(decoders, blob_total) -> bytes`. Tests:
   round trips (text, zero runs, random), one fixed input with its expected token list.
4. `segment.rs`: encode through `lz::parse` into the LZ streams; drop `blob_len`; stored
   path on `n = 0`; unified count rule; `decode` expands before the walk;
   `ModelBuilder::add` tallies LZ symbols of walked and tail segments; `fit_models_block`
   skips the LZ streams; `tail_body` takes the file models; stored fallback above 2^21.
5. `encode.rs`: pass 1 parses blobs for the tally; pass 2 re-parses (deterministic).
6. Delete `deflate.rs`; drop `miniz_oxide` from both `Cargo.toml` and `fuzz/Cargo.lock`;
   `format.rs:VERSION = 3`.
7. Tests: `tests/vectors.rs:transform_body` builds blobs through `lz`; replace the DEFLATE
   hostile cases (`hostile-tables-padding` -> byte after a model stream,
   `hostile-tables-alphabet` -> raw_len mismatch, `hostile-version` -> version 4); add
   `hostile-lz-distance`, `hostile-lz-overrun`, `hostile-lz-short`, `hostile-stored-len`.
   `tests/hostile.rs:wide_model` built raw plus meta.
   Add a language-neutral parse vector, `vectors/lz-tokens.tsv` (input as hex, expected
   token list), written by the Rust test. A C or Go encoder checks its parser against it
   before any file-level comparison: the cheapest guard against an ambiguous parse rule.
8. Sweep on the benchmark samples. Done ahead of the implementation with a throwaway
   coder (120 stratified demos, 334 MB raw; set8 agrees):

   | Blob coding | Blob bytes coded | File size against version 2 |
   |---|---|---|
   | DEFLATE level 9 (version 2) | 5,511,649 (7.4 % of the file) | 0 |
   | literals only, previous-byte context | 12,218,388 | +9.00 % |
   | `MIN_MATCH` 4, `MAX_CHAIN` 1 | 5,737,707 | +0.30 % |
   | `MIN_MATCH` 4, `MAX_CHAIN` 16 | 5,356,483 | -0.21 % |
   | `MIN_MATCH` 4, `MAX_CHAIN` 32 | 5,333,673 | -0.24 % |
   | `MIN_MATCH` 4, `MAX_CHAIN` 64 | 5,321,972 | -0.25 % |
   | `MIN_MATCH` 5, `MAX_CHAIN` 32 | 5,343,834 | -0.23 % |
   | `MIN_MATCH` 8, `MAX_CHAIN` 32 | 5,431,613 | -0.11 % |

   Blob bytes are 12.9 % of the raw demo. Matches are worth 9 % of the file, so `lz.len`
   and `lz.dist` stay. `MIN_MATCH` 3 equals 4 (the hash covers 4 bytes). The models in
   these numbers are still DEFLATE-coded.
   Still to do on the real implementation: the own-table cost (`16 + 5 x present`, tuned
   for a model coding that no longer exists), and encode and decode time against version
   2 for the chosen point, since decode speed is fixed by the format.
   Freeze the constants in `FORMAT.md`, then regenerate vectors
   (`MVDZ_WRITE_VECTORS=1 MVDZ_TEST_DATA=... cargo nextest run vectors_reproduce`).
9. Docs: `FORMAT.md` sections 1, 2.2, 3.1, 3.2, 3.3, 6.2, 6.6, 7, 8, 9, Appendix A, title
   and version rule; `README.md`; `BENCHMARKS.md` (ratios stale until re-run).

## Go (`mvdz_go`)

Done: decoder and encoder at version 3, standard library only. `go test ./...` decodes
every vector and reproduces the compressed ones byte for byte; with `MVDZ_REFERENCE_CLI`
and `MVDZ_TEST_DEMOS` set it compares its files with the Rust tool's for every demo in a
directory. File names follow the Rust modules (`walker`, `protocol`, `rans`, `lz`, `math`,
`fields`, `segment`, `streams`, `encode`; `reader.go` for `decode.rs`).

## Verification

- `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run`
  (vectors reproduce byte for byte), `cargo +nightly fuzz build`.
- `go test ./...` on the same vectors: every file and error category agrees.
- Sizes on the benchmark samples against the version-2 numbers in `BENCHMARKS.md`.
- Determinism: encode the samples with 1 and N threads; compare output SHA-256.
- Cross-language identity becomes testable with the first C or Go encoder: same vectors,
  same file hashes. Until then the spec text is the contract and the Go decoder agreeing
  on every vector proves the decoder side.
