# MVDZ file format, version 3

An MVDZ file holds one QuakeWorld MVD demo, losslessly. The demo is cut
into segments; each segment is walked frame by frame and command by
command (`svc_*`), every field goes to a stream, numeric fields are coded
as residuals against predictors, structural and numeric streams are coded
with static rANS under models stored once in the file, and every other
byte is coded as LZ77 tokens under the same kind of model.

This document specifies the format precisely enough to write an
independent decoder, and, with section 8 and Appendix A, an encoder whose
files the reference accepts. The reference implementation is the Rust
crate `mvdz_rust/mvdz`; sections cite the code they describe as
`file.rs:item`, so a porter can cross-check. Test vectors are in
`vectors/` (see `MANIFEST.tsv`): every valid vector
must decode to its recorded SHA-256 and every hostile vector must be
refused. The manifest records the reference's error for each hostile
vector; another decoder must refuse the file and should report the same
kind of error (section 7), in its own words.

**Identity and integrity.**

* *Identity*: a demo is identified by the SHA-256 of the original `.mvd`
  bytes, stored in the file header (2.1). Decoding does not recompute it:
  a consumer that must authenticate a demo hashes the decoded bytes.
* *Integrity*: the CRC-32 of every segment's decoded bytes (2.3) and of
  the whole demo (2.1) are checked on every decode, together with the
  demo's size. They detect damage (bit rot, truncation, a bad
  copy), not a deliberately forged file (section 10).

**Version.** The header's version field is 3. A reader refuses any other
value.

---------------------------------------------------------------------------

## 1. Conventions

* **`u32`, `u64`**: unsigned fixed-width integers, little-endian. Every
  integer field of the container and the segment layout is a `u32`,
  except two `u64` fields of the file header (2.1). The only
  variable-length integer of the format is the escape length (4.5).
* **CRC-32**: as in zlib, gzip and PNG (ISO 3309; reflected polynomial
  0xEDB88320, initial value 0xFFFFFFFF, final XOR 0xFFFFFFFF). Check
  value: CRC-32("123456789") = 0xCBF43926.
* **SHA-256**: FIPS 180-4.
* **Integer arithmetic.** Unless stated, values are as wide as their type,
  and arithmetic wraps modulo 2^bits where it says "wrapping"; elsewhere it
  cannot overflow for valid input (section 9 lists the places where it
  could for invalid input). `i16(x)` reinterprets the low 16 bits as
  signed; `x as u16` keeps the low 16 bits; a signed right shift is
  arithmetic; integer division truncates toward zero, and every division
  in this format has non-negative operands. "Rust semantics" means: `as`
  casts truncate (`u32 as u16`), sign-extend (`i16 as i32`) or
  reinterpret (`u32 as i32`).
* **Precedence** of the expressions quoted in this document:
  `* / %` > `+ -` > `<< >>` > `&` > `^` > `|`. For example `a | b << 8` is
  `a | (b << 8)`.
* `popcount(x)` = number of set bits. `min`, `max`, `clamp(x, lo, hi)` as
  usual. `bitlen(m)` = number of bits up to and including the highest set
  bit of `m` (0 for 0).
* **Residual bucket** `residual_bucket(v)` for a 32-bit signed `v`
  (`math.rs:residual_bucket`): 0 if `v = 0`, else
  `2 * bitlen(|v|) - 1 + (v < 0 ? 1 : 0)`, with `|i32::MIN| = 2^31`; so
  it is in 0..=64. For a byte residual,
  `byte_residual_bucket(v: u8) = residual_bucket(v as i8 as i32)`
  (`math.rs:byte_residual_bucket`).
* "Symbol" = one byte of a structural stream; "value" = one `i32` of a
  numeric stream.

---------------------------------------------------------------------------

## 2. File layout (`format.rs`)

```
file    := header | models | segment*
header  := "MVDZ" | version u32 | size u64 | duration_ms u64 | crc32 u32 | sha256[32]   (60 bytes)
models  := len u32 | body[len]
segment := len u32 | raw_len u32 | duration_ms u32 | crc32 u32 | coded[len]
```

### 2.1 Header (`format.rs:parse_header`)

The first 60 bytes. Every field describes the original demo, none
describes the `.mvdz` file.

| Offset | Bytes | Field | Content |
|---|---|---|---|
| 0 | 4 | magic | `MVDZ` |
| 4 | 4 | version | 3 |
| 8 | 8 | size | length of the original demo in bytes |
| 16 | 8 | duration_ms | demo time in milliseconds: the sum of the frame time bytes (4.4) |
| 24 | 4 | crc32 | CRC-32 of the original demo |
| 28 | 32 | sha256 | SHA-256 of the original demo |

* Input whose first 4 bytes are not `MVDZ` (or that is shorter than 4
  bytes) is not an MVDZ file. If bytes 4-7 are present and are not 3, the
  version is unsupported. Otherwise a header shorter than 60 bytes is
  truncated: the file is corrupt (section 7).
* Demo time stands still while the server is paused: `duration_ms` is not
  wall-clock time. Like `sha256`, it is a claim of the encoder: no reader
  checks it (section 7).

### 2.2 Models block (`rans.rs:read_models_block`)

```
models := len u32 | body[len]
body   := meta | n u32 | n x { id u32 | raw_len u32 | len u32 | stream[len] }
```

* `len` 0: the file has no models. Otherwise `len` is at most 2^24
  (16 MiB) and the body is consumed exactly.
* `meta` is the **meta model**: a raw model (section 6.2) of alphabet 256,
  stored as it is. Every other model of the block is coded with it.
* `n >= 1`. Each `id` is a structural or numeric stream id (3.2), the ids
  strictly increasing.
* `stream` is a structural stream body (section 6.1; `extra` must be
  empty). Under the meta model it decodes to exactly `raw_len` bytes, each
  under the context "the byte before it" (0 for the first), and must then
  be complete (section 6.5). The bytes are the raw model (section 6.2) of
  stream `id`, with the alphabet of its kind: 256 for a structural stream,
  65 for a numeric one.
* The length of `meta` and the `raw_len` of all entries sum to at most
  2^24 (16 MiB). A decoder checks this before it decodes any model, so no
  model takes more memory than the format allows.
* The models block holds the only models: every structural or numeric
  stream that a segment codes (3.1) is decoded with the model of its id
  here; a segment that codes such a stream without a model is an error.
  A model no segment uses is allowed and ignored.
* A decoder builds every model when it reads the block, before the first
  segment, and the models do not change while segments are decoded.

### 2.3 Segments

```
segment := len u32 | raw_len u32 | duration_ms u32 | crc32 u32 | coded[len]
```

* `len`: length of `coded`, at most 2^21 (2 MiB). The segment occupies
  `16 + len` bytes.
* `raw_len`: bytes the segment decodes to, 1 to 2^20 (1 MiB).
* `duration_ms`: demo time the segment covers: the sum of the time bytes of
  the whole frames it starts with (4.4). A claim of the encoder, not
  checked (section 7).
* `crc32`: CRC-32 of the segment's decoded bytes.
* `coded`: the segment's transform (section 3).

Segments follow the models block in demo order, without gaps. A segment's
offset in the demo is the sum of the `raw_len` of all earlier segments,
and its start time the sum of their `duration_ms`: a reader finds the
segment of a demo time from the segment fields alone, without decoding.
The running sum of `raw_len` never exceeds the header's `size`
(`format.rs:Layout::add_segment`).

**Completeness.** The file is complete when the segments' `raw_len` sum to
the header's `size`. A file that ends before that is incomplete; bytes
after that are an error ("trailing bytes"). An empty demo (`size` 0) has
no segment.

**Segment boundaries.** Segments are independent: every walker and
predictor state starts fresh in each segment, and the protocol state at
its start is in its transform header (3.1). A reader does not check where
a demo is cut: a segment may start or end inside a frame, and the bytes
after its last whole frame are its tail (3.1).

**Cut points written by the reference encoder** (`segment.rs:cut_points`):
a segment ends at the first frame boundary at or after 256 KiB of its
start. Where a frame, or data that does not frame, runs past 1 MiB from
the segment start, the rest of the demo is cut every 1 MiB, and each piece
is walked afresh. A reader must not depend on these cut points beyond the
rules above.

---------------------------------------------------------------------------

## 3. Segment (`segment.rs`)

### 3.1 Transform header and stream table (`segment.rs:parse_transform_header`)

```
coded := fte u32 | fte2 u32 | mvd1 u32 | tail u32 | n u32
         | n x { id u32 | count u32 | [len u32 if id is not a blob stream] }
         | stream bodies, in table order (body k is len_k bytes)
```

* `fte`, `fte2`, `mvd1`: the protocol extension state at the segment
  start (4.1).
* `tail`: length of the unframed bytes at the end of the segment, at most
  `raw_len`.
* `n`: at most 322; `n = 0` is a stored segment (below). Per entry:
  * `id`: a structural, numeric or blob stream id (3.2), each at most
    once, in any order;
  * `count`: symbols, values or bytes in the stream, at most
    `2 * raw_len + 16`: two per byte of the segment (an entity word of two
    bytes takes up to three `ENTITY_GAP` symbols) plus a fixed margin of
    16 for short segments;
  * `len`: length of the stream body, present only for a structural or
    numeric stream. A blob stream has no body.
* The stream bodies fill `coded` exactly to its end.
* **Stream bodies**: one per structural or numeric entry, coded with the
  model of its id from the models block (2.2): `rans_len u32 | rans | extra`
  (section 6.1).
* **Blob bytes**: the blob streams' counts sum to `blob_total`, at most
  `2 * raw_len`. The blob tokens (section 6.6) give that many bytes; the
  blob streams take them in table order, `count` bytes each.
* A structural or numeric stream absent from the table has no symbols:
  reading from it is an error. An absent blob stream is empty: reading 0
  bytes from it succeeds, reading more is an error.

**Stored segment.** A transform header with `n = 0` is followed by the
segment's bytes as they are: `tail` must equal `raw_len`, and exactly
`raw_len` bytes follow the 20 bytes of the header. It is the one layout
whose size is bounded, for a segment the models cannot code (section 8).

### 3.2 Stream ids (`streams.rs`)

Three kinds of stream, one id range each:

* **Structural** streams (ids 0-16) are decoded one symbol at a time, each
  symbol under a context, with rANS (section 6.4): ids 0-15 by the walker
  (section 4), id 16 by the blob tokens (section 6.6).
* **Numeric** streams (ids 17-36) hold `i32` values, each under a feature
  vector, with rANS (section 6.4): ids 17-34 are the residuals the numeric
  groups decode after the walk (section 5), ids 35 and 36 belong to the
  blob tokens.
* **Blob** streams (ids 37-321) are byte arrays the walker consumes in
  order. They have no body: their bytes come from the blob tokens.

An id of 322 or above is an error.

| id | name | kind | content |
|---|---|---|---|
| 0 | `TIME` | structural | frame time byte |
| 1 | `TYPE` | structural | frame type byte |
| 2 | `COMMAND` | structural | svc command byte; 255 = end of payload, 0 = escape |
| 3 | `PLAYER_FLAGS` | structural | playerinfo flags (1 to 3 symbols) |
| 4 | `PLAYER_FRAME` | structural | playerinfo frame (residual) |
| 5 | `PLAYER_WEAPON_FRAME` | structural | playerinfo weapon frame (residual) |
| 6 | `ENTITY_GAP` | structural | entity-number gap |
| 7 | `ENTITY_BITS` | structural | U_* high bits of the entity word, XOR previous |
| 8 | `ENTITY_MORE` | structural | U_MORE_BITS byte, FTE extension bytes |
| 9 | `ENTITY_ANGLE` | structural | entity angles (residual or raw) |
| 10 | `STAT_INDEX` | structural | svc_updatestat index |
| 11 | `STAT_VALUE` | structural | svc_updatestat value (residual) |
| 12 | `SOUND_CHANNEL` | structural | sound channel word (2 symbols) |
| 13 | `SOUND_NUMBER` | structural | sound number |
| 14 | `TEMP_ENTITY_TYPE` | structural | temp-entity type |
| 15 | `USERCMD` | structural | usercmd blocks: every field but the angles on the grid (4.11) |
| 16 | `LZ_LITERAL` | structural | literal bytes of the blob tokens (6.6) |
| 17, 18, 19 | `PLAYER_ORIGIN` + axis | numeric | player origin, 16-bit coordinates |
| 20, 21, 22 | `PLAYER_ANGLE` + axis | numeric | player angles (16-bit) |
| 23, 24, 25 | `ENTITY_ORIGIN` + axis | numeric | entity origin, 16-bit coordinates |
| 26 | `SOUND_ORIGIN` | numeric | sound origins (16-bit coordinates) |
| 27 | `TEMP_ENTITY_ORIGIN` | numeric | temp-entity origins (16-bit coordinates) |
| 28, 29, 30 | `PLAYER_FLOAT_ORIGIN` + axis | numeric | player origin, 32-bit float coordinates (bit patterns) |
| 31, 32, 33 | `ENTITY_FLOAT_ORIGIN` + axis | numeric | entity origin, 32-bit float coordinates |
| 34 | `USERCMD_ANGLE` | numeric | usercmd angles on the 360/65536 grid (5.6) |
| 35 | `LZ_LENGTH` | numeric | match lengths of the blob tokens, 0 for a literal (6.6) |
| 36 | `LZ_DISTANCE` | numeric | match distances of the blob tokens (6.6) |
| 37 | `MASK` | blob | dem_multiple mask (4 bytes) |
| 38 | `ESCAPE_LENGTH` | blob | escape length bytes |
| 39 | `HIDDEN_TAG` | blob | hidden-block tag (0 escape, 1 block, 2 end) |
| 40 | `HIDDEN_LENGTH` | blob | hidden-block length (4 bytes) |
| 41 | `HIDDEN_TYPE` | blob | hidden-block type (2 bytes) |
| 42 | `HIDDEN_DAMAGE` | blob | hidden damage block (8 bytes) |
| 43 | `PLAYER_NUMBER` | blob | playerinfo player number |
| 44 | `PLAYER_MODEL` | blob | playerinfo model |
| 45 | `PLAYER_SKIN` | blob | playerinfo skin |
| 46 | `PLAYER_EFFECTS` | blob | playerinfo effects |
| 47 | `ENTITY_HEADER` | blob | delta packet-entities "from" byte |
| 48 | `ENTITY_MODEL` | blob | modelindex (1 or 2 bytes) |
| 49 | `ENTITY_FRAME` | blob | frame |
| 50 | `ENTITY_MISC` | blob | colormap, skin, effects, FTE trans/scale/fatness/colormod |
| 51 | `STAT_LONG_INDEX` | blob | svc_updatestatlong index |
| 52 | `SOUND_MISC` | blob | sound volume, attenuation |
| 53 | `TEMP_ENTITY_MISC` | blob | temp-entity entity (2 bytes) or count (1 byte) |
| 54 | `PRINT_LEVEL` | blob | svc_print level |
| 55 | `FRAGS` | blob | svc_updatefrags (3 bytes) |
| 56 | `STAT_LONG_VALUE` | blob | svc_updatestatlong value (4 bytes) |
| 57 | `RAW` | blob | escaped payload bytes |
| 58 | `TAIL` | blob | unframed bytes at the end of the segment |
| 59 | `SET` | blob | dem_set bodies (8 bytes each) |
| 60 | `TEXT` | blob | string bodies of svc 8 (after the level), 9 and 26 |
| 61 | `PING` | blob | bodies of svc 36, 37 and 53 |
| 62 | `NAILS` | blob | bodies of svc 43 and 54 |
| 63 | `HIDDEN_MISC` | blob | hidden-block bodies other than damage |
| 64 | `SOUND_FLOAT` | blob | sound origins with float coordinates (12 bytes) |
| 65 | `TEMP_ENTITY_FLOAT` | blob | temp-entity origins with float coordinates |
| 66 + c | `GENERIC` + c | blob | body of every other svc command c (including 11, serverdata) |

### 3.3 Decoding a segment (`decode.rs:decode_segment`, `segment.rs:decode`)

1. Parse the segment fields (2.3) and the transform header and stream
   table (3.1).
2. A stored segment (`n = 0`) is copied; go to step 7.
3. Construct a rANS decoder for every structural and numeric stream in the
   table, with its model from the models block.
4. Expand the blob tokens (section 6.6) to `blob_total` bytes and split
   them among the blob streams in table order. Afterwards streams 35 and
   36 must be complete.
5. **Walk** (section 4), producing the structural bytes of the segment and
   the record lists of the numeric fields. Afterwards every structural
   stream (stream 16 included) must have yielded exactly `count` symbols
   with its coded data consumed, and every blob stream must be consumed
   exactly.
6. Run the **numeric groups** (section 5) in their fixed order; afterwards
   every numeric stream must be complete.
7. Check `crc32(output) == crc32` of the segment fields.

The reference decodes segments in parallel; the result does not depend on
it.

### 3.4 Whole-demo checks (`decode.rs:Decoder::finish`)

After the last segment a reader checks that no byte follows it and that
the CRC-32 of the concatenated output equals the header's `crc32`. The
output's length equals the header's `size` by the completeness rule (2.3).

The decoded demo is returned only when these pass. A sequential
reader delivers each segment's bytes once that segment passes its own
checks (3.3), and reports the end of the demo only after the whole-demo
checks; a failure after some bytes were delivered is an error, and those
bytes are a prefix of the demo.

## 4. The structural transform (`walker.rs`, `protocol.rs`)

The decoder rebuilds the segment (length `raw_len`) into an output buffer
`dst`. The walker writes every structural byte. For each numeric field it
only *reserves* the bytes (records their offset in a record list); the
numeric groups (section 5) fill them in afterwards. The structure never
depends on a numeric value.

Three kinds of stream feed the walker (stream ids: section 3):

* *structural* streams (ids 0-15): rANS-coded symbols, each
  with a 16-bit context (section 6);
* *blob* streams: plain bytes, read in order from the stream's slice of the
  segment's blob bytes (section 6.6);
* *numeric* streams: read only by section 5.

Walker primitives (decode direction; `p` = output cursor):

* `symbol(s, ctx)`: the next symbol of structural stream `s` under context
  `ctx`. If `s` is exhausted or absent, `symbol` returns 255 and the segment
  fails (`segment.rs:DecodeIo::symbol`); 255 ends every loop below.
* `code_symbol(s, ctx)`: `v = symbol(s, ctx)`; write `v` at `dst[p]`;
  `p += 1`; return `v`.
* `code_symbol_residual(s, ctx, pred)`: `v = (symbol(s, ctx) + pred) mod 256`;
  write and return `v`.
* `blob_byte(s)`: the next byte of blob stream `s`, not written; error if the
  stream has no byte left (`SegmentWalker::blob_byte`).
* `code_blob_byte(s)`: `v = blob_byte(s)`; write `v`; return `v`
  (`SegmentWalker::code_blob_byte`).
* `code_blob_residual(s, pred)`: `v = (blob_byte(s) + pred) mod 256`; write
  and return `v` (`SegmentWalker::code_blob_residual`).
* `code_blob_bytes(s, n)`: `n` times `code_blob_byte(s)`; returns the
  little-endian value
  (`SegmentWalker::code_blob_bytes`).
* `copy_blob(s, n)`: copy the next `n` bytes of blob stream `s` to `dst[p..]`;
  `p += n`. Error if the stream has fewer than `n` bytes left or the output
  would overflow (`SegmentWalker::copy_blob`).
* `reserve(n)`: `off = p`; `p += n`; error if `p > raw_len`; returns `off`.
* `copy_command_body(s, c)`: `n = command_body_size(c, the unread bytes of blob s)`
  (section 4.2); `None` is an error; else `copy_blob(s, n)`.

Writing beyond `raw_len` is an error. Errors are sticky: the walk stops at
the next check and the segment fails.

### 4.1 Protocol state (`protocol.rs:ProtocolState`)

```
FTE_PEXT_TRANS = 1 << 3, FTE_PEXT_FLOAT_COORDS = 1 << 15, FTE_PEXT_COLOR_MOD = 1 << 19
MVD_PEXT_1_FLOAT_COORDS = 1 << 0
has_float_coords     = (fte & FTE_PEXT_FLOAT_COORDS) != 0 || (mvd1 & MVD_PEXT_1_FLOAT_COORDS) != 0
has_fte_float_coords = (fte & FTE_PEXT_FLOAT_COORDS) != 0
coord      = has_float_coords ? 4 : 2       // entity and player origins, sound and TE coords
wide_coord = has_fte_float_coords ? 4 : 2   // SVC_INTERMISSION coords
angle      = has_fte_float_coords ? 2 : 1
```

A segment starts with the state in its transform header (`fte`, `fte2`,
`mvd1`; section 3). `observe_server_data(body)` updates it after an
`SVC_SERVER_DATA` body has been written:

```
i = 0
while i + 4 <= len(body):
    v = le32(body[i..]); i += 4
    if v == PROTOCOL_VERSION (28): break
    if i + 4 > len(body): break
    x = le32(body[i..]); i += 4
    if v == PROTOCOL_VERSION_FTE   ('FTEX', 0x58455446): fte  = x
    if v == PROTOCOL_VERSION_FTE_2 ('FTE2', 0x32455446): fte2 = x
    if v == PROTOCOL_VERSION_MVD_1 ('MVD1', 0x3144564D): mvd1 = x
re-derive has_float_coords, has_fte_float_coords
```

`fte2` is carried but no rule reads it.

### 4.2 Command sizer (`protocol.rs:ProtocolState::command_body_size`)

`command_body_size(c, b)` is the number of body bytes (the command byte
excluded) of command `c` whose body starts at `b[0]`, or `None` if `c` is
unknown or the body does not fit in `b`. Primitives: `skip(n)` (fails if
fewer than `n` bytes are left), `u8`, `u16`, `u32` (little-endian; fail if
short), `str` (to and including the next NUL; fails if there is none).

| c | name | body |
|---|---|---|
| 1, 27, 28, 34, 35 | `SVC_NOP`, `SVC_KILLED_MONSTER`, `SVC_FOUND_SECRET`, `SVC_SMALL_KICK`, `SVC_BIG_KICK` | empty |
| 2, 9, 26, 31 | `SVC_DISCONNECT`, `SVC_STUFF_TEXT`, `SVC_CENTER_PRINT`, `SVC_FINALE` | str |
| 3 | `SVC_UPDATE_STAT` | skip 2 |
| 6 | `SVC_SOUND` | `ch = u16`; if `ch & SND_VOLUME` (1<<15) skip 1; if `ch & SND_ATTENUATION` (1<<14) skip 1; skip `1 + 3*coord` |
| 8 | `SVC_PRINT` | skip 1; str |
| 10 | `SVC_SET_ANGLE` | skip `1 + 3*angle` |
| 11 | `SVC_SERVER_DATA` | loop { `v = u32`; if `v == 28` break; skip 4 }; skip 4; str; skip 4; str; skip 40 |
| 12 | `SVC_LIGHT_STYLE` | skip 1; str |
| 14 | `SVC_UPDATE_FRAGS` | skip 3 |
| 19 | `SVC_DAMAGE` | skip `2 + 3*coord` |
| 20 | `SVC_SPAWN_STATIC` | skip `4 + 3*(coord + angle)` |
| 21, 66 | `SVC_FTE_SPAWN_STATIC_2`, `SVC_FTE_SPAWN_BASELINE_2` | `w = u16`; `delta(w)` |
| 22 | `SVC_SPAWN_BASELINE` | skip `2 + 4 + 3*(coord + angle)` |
| 23 | `SVC_TEMP_ENTITY` | `t = u8`; t in {`TE_LIGHTNING_1` 5, `TE_LIGHTNING_2` 6, `TE_LIGHTNING_3` 9}: skip `2 + 6*coord`; t in {0, 1, 3, 4, 7, 8, 10, 11, 13}: skip `3*coord`; t in {`TE_GUNSHOT` 2, `TE_BLOOD` 12}: skip `1 + 3*coord`; else `None` |
| 24, 32, 44 | `SVC_SET_PAUSE`, `SVC_CD_TRACK`, `SVC_CHOKE_COUNT` | skip 1 |
| 29 | `SVC_SPAWN_STATIC_SOUND` | skip `3*coord + 3` |
| 30 | `SVC_INTERMISSION` | skip `3*wide_coord + 3*angle` |
| 36 | `SVC_UPDATE_PING` | skip 3 |
| 37, 38 | `SVC_UPDATE_ENTER_TIME`, `SVC_UPDATE_STAT_LONG` | skip 5 |
| 39, 53 | `SVC_MUZZLE_FLASH`, `SVC_UPDATE_PL` | skip 2 |
| 40 | `SVC_UPDATE_USER_INFO` | skip 5; str |
| 41 | `SVC_DOWNLOAD` | `n = u16` as i16; skip 1; if `n > 0` skip `n` |
| 42 | `SVC_PLAYER_INFO` | skip 1; `f = u16`; skip 1; for i in 0..3: if `f & (DF_ORIGIN << i)` skip coord; for i in 0..3: if `f & (DF_ANGLES << i)` skip 2; skip 1 for each of `DF_MODEL`, `DF_SKIN_NUM`, `DF_EFFECTS`, `DF_WEAPON_FRAME` that is set |
| 43 | `SVC_NAILS` | `n = u8`; skip `6*n` |
| 45, 46 | `SVC_MODEL_LIST`, `SVC_SOUND_LIST` | skip 1; loop { str; break if it was just the NUL }; skip 1 |
| 47 | `SVC_PACKET_ENTITIES` | loop { `w = u16`; if `w == 0` break; `delta(w)` } |
| 48 | `SVC_DELTA_PACKET_ENTITIES` | skip 1; then as 47 |
| 49, 50 | `SVC_MAX_SPEED`, `SVC_ENT_GRAVITY` | skip 4 |
| 51 | `SVC_SET_INFO` | skip 1; str; str |
| 52 | `SVC_SERVER_INFO` | str; str |
| 54 | `SVC_NAILS_2` | `n = u8`; skip `7*n` |
| any other | | `None` |

A decoder sizes commands only through `copy_command_body` (section 4.6):
with the rows of 9 (for 8, 9 and 26), 11, 36, 37, 53, 43, 54 and every
command without a row of its own in section 4.6. Commands 3, 6, 8, 14, 23,
26, 38, 42, 47 and 48 are sized only by the encoder, to choose between
walking a command and escaping (section 4.5).

Bits (`protocol.rs`): `DF_ORIGIN` 1<<0, `DF_ANGLES` 1<<3, `DF_EFFECTS` 1<<6,
`DF_SKIN_NUM` 1<<7, `DF_WEAPON_FRAME` 1<<10, `DF_MODEL` 1<<11.
`U_ANGLE_1` 1<<0, `U_ANGLE_3` 1<<1, `U_MODEL` 1<<2, `U_COLOR_MAP` 1<<3,
`U_SKIN` 1<<4, `U_EFFECTS` 1<<5, `U_FTE_EVEN_MORE` 1<<7, `U_ORIGIN_1` 1<<9,
`U_ORIGIN_2` 1<<10, `U_ORIGIN_3` 1<<11, `U_ANGLE_2` 1<<12, `U_FRAME` 1<<13,
`U_MORE_BITS` 1<<15. In the extension byte(s): `U_FTE_SCALE` 1<<0,
`U_FTE_TRANS` 1<<1, `U_FTE_FATNESS` 1<<2, `U_FTE_MODEL_DBL` 1<<3,
`U_FTE_ENTITY_DBL` 1<<5, `U_FTE_ENTITY_DBL_2` 1<<6, `U_FTE_YET_MORE` 1<<7,
`U_FTE_COLOR_MOD` 1<<10.

`delta(w)` (`ProtocolState::skip_entity_delta`):

```
bits = w & ~511; more = 0
if bits & U_MORE_BITS: bits |= u8
if bits & U_FTE_EVEN_MORE and fte != 0:
    more = u8
    if more & U_FTE_YET_MORE: more |= u8 << 8
if bits & U_MODEL: skip 1   elif more & U_FTE_MODEL_DBL: skip 2
for m in [U_FRAME, U_COLOR_MAP, U_SKIN, U_EFFECTS]: if bits & m: skip 1
for (o, a) in [(U_ORIGIN_1, U_ANGLE_1), (U_ORIGIN_2, U_ANGLE_2), (U_ORIGIN_3, U_ANGLE_3)]:
    if bits & o: skip coord
    if bits & a: skip angle
if more & U_FTE_TRANS and fte & FTE_PEXT_TRANS: skip 1
if more & U_FTE_SCALE: skip 1
if more & U_FTE_FATNESS: skip 1
if more & U_FTE_COLOR_MOD and fte & FTE_PEXT_COLOR_MOD: skip 3
```

`frame_len(src, p)` (`protocol.rs:frame_len`) is the length of the frame at
`p`, header included, or `None`. With fewer than 2 bytes left: `None`.
By `src[p+1] & 7`: `DEM_SET` (2): 10 bytes; `DEM_MULTIPLE` (3):
`10 + le32(src[p+6..])`; `DEM_READ` (1), `DEM_SINGLE` (4), `DEM_STATS` (5),
`DEM_ALL` (6): `6 + le32(src[p+2..])`; anything else (0, 7): `None`. A
frame that runs past the end of `src` is `None`. The encoder walks frames
while `frame_len` is `Some` and stores the rest of the segment as its tail;
it also cuts the demo and sums `duration_ms` with it (section 8). A decoder
does not need `frame_len`.

### 4.3 Walker state (`walker.rs:SegmentWalker::new`)

Fresh for every segment; zero unless noted:

| state | type | init | updated |
|---|---|---|---|
| `time` | u32 | 0 | += every time byte (wrapping) |
| `target` | u8 | 0 | `type >> 3` of the current frame |
| `last_type`, `last_time_byte` | u8 | 0 | the previous frame's type and time byte |
| `previous_command` | u8 | 0 | reset to 0 at each payload start |
| `first_command_by_type[8]` | u8 | 0 | per frame type (`type & 7`), section 4.5 |
| `stat_previous_index` | u8 | 255 | reset to 255 at each payload start |
| `stat_previous_residual` | u8 | 0 | reset to 0 at each payload start |
| `last_header` | u8 | 0 | per segment |
| `previous_temp_entity` | u8 | 0 | per segment |
| `previous_entity_gap` | u8 | 0 | reset to 0 at each packet-entities command |
| `players[256]` | {flags u16, frame u8, weapon_frame u8} | 0 | per player number |
| `usercmd_previous_players[2]` | u8 | 0 | per segment |
| `usercmds[256]` | {drop, msec, grid, buttons, impulse u8; moves[3] u16} | 0 | per usercmd player number |
| `entities[2048]` | {angles[3] u8, angle_residuals[3] u8, frame u8, model u8, bits u8, last_packet u32} | 0 | per entity number |
| `stats[32][256]` | u8 | 0 | per (target & 31, stat index) |
| `player_origin_offsets[256][3]`, `entity_origin_offsets[2048][3]` | u32 | `NO_OFFSET` (0xFFFFFFFF) | offset of the latest origin coord per axis |
| `previous_packet`, `current_packet` | list of u16 | empty | entity numbers (9-bit) of the previous and current packet |
| `previous_packet_index` | usize | 0 | |
| `packet_number` | u32 | 1 | packet counter |
| `protocol` | ProtocolState | the transform header's | section 4.1 |

Record lists (`walker.rs:RecordLists`), appended in walk order and read by
section 5:

* `PlayerInfoRecord {offset u32, time u32, flags u16, number u8, has_float_coords}`
* `PacketEntityRecord {offset u32, number u16, bits u16}`;
  `bits` holds origin `i` at `1 << 2i` and angle `i` at `2 << 2i`
  (i = 0..2), `PE_IN_PREVIOUS` 64, `PE_FLOAT` 128, `PE_WIDE` 256
* `SoundRecord {offset u32, references[3] u32, kind u8, number u8}`
* `TempEntityRecord {offset u32, references[3] u32, kind u8}`
* `UsercmdRecord {pitch_offset u32, number u8, grid_axes u8}` (section 4.11)

The lists grow as the walk appends. Every record consumes a symbol of its
stream, so the stream table's counts bound them (section 9, note 2).

### 4.4 Segment walk and frames (`SegmentWalker::run`, `SegmentWalker::frame`)

```
stop = max(raw_len - tail, 0)
while no error and p < stop: frame()
if p != stop: error
copy_blob(TAIL, tail)
```

`frame()`:

```
tb = code_symbol(TIME, last_type | (last_time_byte << 8));          last_time_byte = tb; time += tb
ty = code_symbol(TYPE, last_type | (min(tb, 32) << 8));             last_type = ty; target = ty >> 3
switch ty & 7:
  DEM_SET:                          copy_blob(SET, 8)
  DEM_MULTIPLE:                     mask = code_blob_bytes(MASK, 4)
                                    length_prefixed(ty, hidden = (mask == 0))
  DEM_READ, DEM_SINGLE, DEM_STATS, DEM_ALL:
                                    length_prefixed(ty, hidden = false)
  0, 7:                             error
```

Both contexts use `last_type` before it is updated; TIME's uses the
previous frame's time byte.

`length_prefixed(ty, hidden)`: `at = reserve(4)` (stop on error); then
`hidden ? hidden_payload() : payload(ty)`; then write `n = p - at - 4` as
u32le at `dst[at..at+4]`.

### 4.5 Payload command loop (`SegmentWalker::payload`)

```
previous_command = 0
stat_previous_index = 255; stat_previous_residual = 0
ti = ty & 7
first_class = command_class(first_command_by_type[ti])   // the previous payload's
first_command_by_type[ti] = 255
for index = 0, 1, 2, ...:
    if error: return
    if index == 1: first_command_by_type[ti] = previous_command
    class = command_class(previous_command)
    if class == 15:
        ctx = 0x8000 | previous_command | (ti << 8) | (min(index, 7) << 11)
    else:
        ctx = class | (min(index, 15) << 4) | (first_class << 8) | (ti << 12)
    c = symbol(COMMAND, ctx)
    if c == 255: return                // end of payload
    if c == 0:   escape(); return      // the rest of the payload is raw
    write c at dst[p]; p += 1
    command(c)
    previous_command = c
```

The COMMAND symbol 255 and 0 are not written. After a payload with at least
one command `first_command_by_type[ti]` holds its first command; after an
empty one (an immediate 255 or 0) it holds 255. `command_class(255)` is 15;
initially `first_command_by_type` is 0, class 0.

The COMMAND context is split by bit 15:

* class 0-14: bits 0-3 class, bits 4-7 `min(index, 15)`, bits 8-11
  `first_class`, bits 12-14 `ti`, bit 15 = 0.
* class 15: bits 0-7 `previous_command`, bits 8-10 `ti`, bits 11-13
  `min(index, 7)`, bit 14 = 0, bit 15 = 1.

`command_class(c)` (`walker.rs:command_class`):

| c | class |
|---|---|
| 0 (`SVC_BAD`) | 0 |
| 42 `SVC_PLAYER_INFO` | 1 |
| 48 `SVC_DELTA_PACKET_ENTITIES` | 2 |
| 47 `SVC_PACKET_ENTITIES` | 3 |
| 6 `SVC_SOUND` | 4 |
| 23 `SVC_TEMP_ENTITY` | 5 |
| 3 `SVC_UPDATE_STAT` | 6 |
| 38 `SVC_UPDATE_STAT_LONG` | 7 |
| 8 `SVC_PRINT` | 8 |
| 51 `SVC_SET_INFO` | 9 |
| 14 `SVC_UPDATE_FRAGS` | 10 |
| 36 `SVC_UPDATE_PING` | 11 |
| 9 `SVC_STUFF_TEXT` | 12 |
| 43 `SVC_NAILS`, 54 `SVC_NAILS_2` | 13 |
| 19 `SVC_DAMAGE` | 14 |
| any other | 15 |

`escape()` (`SegmentWalker::escape`):

```
n = 0
for k in 0..5:
    b = blob_byte(ESCAPE_LENGTH)
    n |= (b & 0x7f) << (7*k)          // u32; bits beyond 32 are lost
    if b < 0x80: break
    if k == 4: error; return
copy_blob(RAW, n)
```

The encoder escapes when the next command byte is 0, unknown to the sizer,
or its body does not fit before the payload end. It writes `n` in groups of
7 bits, least significant first, bit 7 set on every group but the last.
The escaped part runs to the end of the payload.

### 4.6 Commands (`SegmentWalker::command`)

In decode order; `previous_command` is the payload's previous command (0
for the first), updated after `command()` returns.

| c | handling |
|---|---|
| `SVC_PLAYER_INFO` | section 4.7 |
| `SVC_PACKET_ENTITIES`, `SVC_DELTA_PACKET_ENTITIES` | section 4.8 |
| `SVC_UPDATE_STAT` | `pl = target & 31`; `i = code_symbol(STAT_INDEX, stat_previous_index \| (previous_command << 8))`; `prev = stats[pl][i]`; `v = code_symbol_residual(STAT_VALUE, i \| (stat_previous_residual << 8), prev)`; `stat_previous_index = i`; `stat_previous_residual = v - prev` (mod 256); `stats[pl][i] = v` |
| `SVC_UPDATE_STAT_LONG` | `code_blob_byte(STAT_LONG_INDEX)`; `copy_blob(STAT_LONG_VALUE, 4)` |
| `SVC_SOUND` | section 4.9 |
| `SVC_TEMP_ENTITY` | section 4.10 |
| `SVC_PRINT` | `code_blob_byte(PRINT_LEVEL)`; `copy_command_body(TEXT, SVC_STUFF_TEXT)` (one string) |
| `SVC_STUFF_TEXT`, `SVC_CENTER_PRINT` | `copy_command_body(TEXT, SVC_STUFF_TEXT)` |
| `SVC_UPDATE_FRAGS` | `copy_blob(FRAGS, 3)` |
| `SVC_UPDATE_PING`, `SVC_UPDATE_ENTER_TIME`, `SVC_UPDATE_PL` | `copy_command_body(PING, c)` |
| `SVC_NAILS`, `SVC_NAILS_2` | `copy_command_body(NAILS, c)` |
| `SVC_SERVER_DATA` | `start = p`; `copy_command_body(GENERIC + 11, SVC_SERVER_DATA)`; if no error: `protocol.observe_server_data(dst[start..p])` |
| any other | `copy_command_body(GENERIC + c, c)` |

### 4.7 playerinfo (`SegmentWalker::player_info`, `SegmentWalker::player_info_flags`)

```
number = code_blob_byte(PLAYER_NUMBER)
ps = players[number]
flags = player_info_flags(ps.flags)
frame = code_symbol_residual(PLAYER_FRAME, ps.frame | (flags & (DF_DEAD | DF_GIB)), ps.frame)  // bits 8-9
cw = protocol.coord_size
off = reserve(popcount(flags & 0x7) * cw + popcount(flags & 0x38) * 2)
push PlayerInfoRecord{off, time, flags, number, protocol.has_float_coords}
o = off
for i in 0..3: if flags & (DF_ORIGIN << i): player_origin_offsets[number][i] = o; o += cw
if flags & DF_MODEL:    code_blob_byte(PLAYER_MODEL)
if flags & DF_SKIN_NUM: code_blob_byte(PLAYER_SKIN)
if flags & DF_EFFECTS:  code_blob_byte(PLAYER_EFFECTS)
wf = ps.weapon_frame
if flags & DF_WEAPON_FRAME: wf = code_symbol_residual(PLAYER_WEAPON_FRAME, ps.weapon_frame, ps.weapon_frame)
players[number] = {flags, frame, wf}
```

`DF_DEAD` is 1<<8 and `DF_GIB` 1<<9. The reserved block holds the origin
coords (flag bits 0-2, `cw` bytes each) and then the angles (flag bits 3-5,
2 bytes each); section 5 fills it.

`player_info_flags(prev)` codes the 16-bit flags as `x = flags XOR prev`:

```
a = symbol(PLAYER_FLAGS, 0x8000 | (prev & 0x7fff))
x = (a & 0x3f) | (((a >> 6) & 1) << 10)
if a & 0x80:                                     // a rare bit changed
    b1 = symbol(PLAYER_FLAGS, 0x100 | ((prev >> 6) & 0xff))
    b2 = symbol(PLAYER_FLAGS, 0x200)
    x |= ((b1 & 0xf) << 6) | ((b1 >> 4) << 11) | ((b2 & 1) << 15)
flags = x ^ prev
write flags as 2 bytes, low byte first
```

Symbol `a`: bits 0-5 are `x` bits 0-5, bit 6 is `x` bit 10, bit 7 says a
rare bit changed. `b1`: low nibble `x` bits 6-9, high nibble `x` bits
11-14. `b2`: `x` bit 15 (the encoder writes 0 or 1; the decoder reads bit
0). The encoder sets bit 7 of `a` exactly when `x & ~0x43f != 0`. The three
contexts do not overlap: 0x8000-0xFFFF, 0x100-0x1FF and 0x200.

### 4.8 Packet entities (`SegmentWalker::packet_entities`, `SegmentWalker::entity_delta`)

```
if c == SVC_DELTA_PACKET_ENTITIES: h = code_blob_residual(ENTITY_HEADER, last_header); last_header = h
previous_entity_gap = 0
prev = -1 (i32)
previous_packet_index = 0; current_packet = []
packet_number += 1 (wrapping)
loop:
    if error: break
    pe = predict_entity(prev)
    ectx = min(pe - prev, 255) | (min(previous_entity_gap, 15) << 8)
    d = symbol(ENTITY_GAP, ectx)
    if d == 255: d = symbol(ENTITY_GAP, 1 << 12) | (symbol(ENTITY_GAP, 2 << 12) << 8)
    if d == 0: write 0x00, 0x00; break               // end of packet
    e = (prev + d) & 511
    es = entities[e]
    bits7 = symbol(ENTITY_BITS, es.bits | (es.model << 7)) XOR es.bits
    w = (bits7 << 9) | e                             // u16
    if w == 0: error; break
    write w as 2 bytes, low byte first
    previous_entity_gap = clamp(e - prev, 0, 255)
    prev = e
    current_packet.append(e)
    entities[e].bits = w >> 9
    entity_delta(w)
swap(previous_packet, current_packet)                // also after an error break
```

`predict_entity(prev)`: advance `previous_packet_index` while it is below
`len(previous_packet)` and `previous_packet[previous_packet_index] <=
prev`; return `previous_packet[previous_packet_index]` if the index is
below the length, else `prev + 1`. The index only moves forward within a
packet. The encoder codes `d = ((e - prev - 1) & 511) + 1` (1..512):
directly if `d < 255`, else as 255 followed by the two bytes of `d`.

`entity_delta(w)`:

```
bits = w & ~511; more = 0
number = w & 511
m0 = entities[number].model
if bits & U_MORE_BITS: bits |= code_symbol(ENTITY_MORE, (bits >> 9) | (m0 << 8))
if bits & U_FTE_EVEN_MORE and protocol.fte != 0:
    more = code_symbol(ENTITY_MORE, 0x8000)
    if more & U_FTE_YET_MORE: more |= code_symbol(ENTITY_MORE, 0x8001) << 8
if more & U_FTE_ENTITY_DBL:   number += 512
if more & U_FTE_ENTITY_DBL_2: number += 1024
in_previous = (entities[number].last_packet == packet_number - 1)    // wrapping
entities[number].last_packet = packet_number
es = copy of entities[number]
if bits & U_MODEL:              es.model = code_blob_byte(ENTITY_MODEL)
elif more & U_FTE_MODEL_DBL:    code_blob_bytes(ENTITY_MODEL, 2)                    // model not updated
model = es.model
if bits & U_FRAME:     es.frame = code_blob_residual(ENTITY_FRAME, es.frame)
if bits & U_COLOR_MAP: code_blob_byte(ENTITY_MISC)
if bits & U_SKIN:      code_blob_byte(ENTITY_MISC)
if bits & U_EFFECTS:   code_blob_byte(ENTITY_MISC)
rb = 0
for i in 0..3 over (U_ORIGIN_1, U_ANGLE_1), (U_ORIGIN_2, U_ANGLE_2), (U_ORIGIN_3, U_ANGLE_3):
    if bits & origin_i: rb |= 1 << (2*i)
    if bits & angle_i:  rb |= 2 << (2*i)
if in_previous:                 rb |= PE_IN_PREVIOUS
if protocol.has_float_coords:      rb |= PE_FLOAT
if protocol.has_fte_float_coords:  rb |= PE_WIDE
record = PacketEntityRecord{offset = p, number, bits = rb}
for i in 0..3:
    if bits & origin_i: entity_origin_offsets[number][i] = reserve(protocol.coord_size)
    if bits & angle_i:
        if protocol.has_fte_float_coords:
            code_symbol(ENTITY_ANGLE, 3 | (model << 6))
            code_symbol(ENTITY_ANGLE, 3 | (model << 6) | (1 << 14))
        else:
            q = min(byte_residual_bucket(es.angle_residuals[i]), 15)
            v = code_symbol_residual(ENTITY_ANGLE, i | (q << 2) | (model << 6), es.angles[i])
            es.angle_residuals[i] = v - es.angles[i] (mod 256); es.angles[i] = v
push record
if more & U_FTE_TRANS and protocol.fte & FTE_PEXT_TRANS:          code_blob_byte(ENTITY_MISC)
if more & U_FTE_SCALE:                                         code_blob_byte(ENTITY_MISC)
if more & U_FTE_FATNESS:                                       code_blob_byte(ENTITY_MISC)
if more & U_FTE_COLOR_MOD and protocol.fte & FTE_PEXT_COLOR_MOD:  code_blob_bytes(ENTITY_MISC, 3)
entities[number] = es
```

* `m0` (the `ENTITY_MORE` context) and the `ENTITY_BITS` context use the entity at
  the 9-bit index `w & 511`; the model, frame and angle state use the
  extended `number`. `entities[w & 511].bits` is written before
  `entity_delta`; if `number != w & 511`, the final `entities[number] = es`
  writes back `entities[number]`'s own `bits`.
* `es` is copied after `last_packet` is set, so the write-back keeps it.
* `previous_packet` and `current_packet` hold 9-bit numbers.
* Angles sit between the reserved origin coords in wire order (origin 1,
  angle 1, origin 2, angle 2, origin 3, angle 3); section 5 recomputes the
  origin offsets from `record.offset` and `record.bits`
  (`PacketEntityRecord::origin_offsets`).

### 4.9 sound (`SegmentWalker::sound`)

```
lo = code_symbol(SOUND_CHANNEL, previous_command)
ch = lo | (code_symbol(SOUND_CHANNEL, 0x100 | lo) << 8)
if ch & SND_VOLUME:      code_blob_byte(SOUND_MISC)
if ch & SND_ATTENUATION: code_blob_byte(SOUND_MISC)
e = (ch >> 3) & 1023
number = code_symbol(SOUND_NUMBER, (ch & 7) | (min(e, 33) << 3))
if protocol.coord_size != 2: copy_blob(SOUND_FLOAT, 12); return
if 1 <= e <= 32: references = player_origin_offsets[e - 1]; kind = 1
else:            references = entity_origin_offsets[e];     kind = 2
off = reserve(6)
push SoundRecord{off, references, kind, number}
```

`references` are copied at this moment: the latest origin offsets of that player
or entity in this segment, or `NO_OFFSET`.

### 4.10 temp entity (`SegmentWalker::temp_entity`)

```
t = code_symbol(TEMP_ENTITY_TYPE, previous_temp_entity | ((previous_command == SVC_TEMP_ENTITY) << 8))
previous_temp_entity = t
c = protocol.coord_size
if c != 2:
    if t in {TE_LIGHTNING_1, TE_LIGHTNING_2, TE_LIGHTNING_3}: code_blob_bytes(TEMP_ENTITY_MISC, 2); copy_blob(TEMP_ENTITY_FLOAT, 6*c)
    elif t in {TE_GUNSHOT, TE_BLOOD}: code_blob_byte(TEMP_ENTITY_MISC); copy_blob(TEMP_ENTITY_FLOAT, 3*c)
    else: copy_blob(TEMP_ENTITY_FLOAT, 3*c)
    return
if t in {TE_LIGHTNING_1, TE_LIGHTNING_2, TE_LIGHTNING_3}:       // beams
    e = code_blob_bytes(TEMP_ENTITY_MISC, 2)
    references = (1 <= e <= 32) ? player_origin_offsets[e - 1] : [NO_OFFSET] * 3
    off = reserve(12); push TempEntityRecord{off, references, t}
elif t in {TE_GUNSHOT, TE_BLOOD}:
    code_blob_byte(TEMP_ENTITY_MISC)
    off = reserve(6); push TempEntityRecord{off, [NO_OFFSET] * 3, t}
else:
    off = reserve(6); push TempEntityRecord{off, [NO_OFFSET] * 3, t}
```

The encoder escapes a payload with an unknown temp-entity type (the sizer
returns `None`), so the last branch sees only the types of section 4.2.

### 4.11 Hidden payloads (`SegmentWalker::hidden_payload`)

A `DEM_MULTIPLE` frame with mask 0 carries blocks `[u32 len][u16 type][len
bytes]`:

```
loop:
    if error: return
    tag = blob_byte(HIDDEN_TAG)
    if tag == 0: escape(); return
    if tag == 2: return
    if tag != 1: error; return
    len  = code_blob_bytes(HIDDEN_LENGTH, 4)
    type = code_blob_bytes(HIDDEN_TYPE, 2)
    if type == MVD_HIDDEN_DMG_DONE (7) and len == 8:   copy_blob(HIDDEN_DAMAGE, 8)
    elif type == MVD_HIDDEN_USERCMD (1) and len == 23: usercmd()
    else:                                              copy_blob(HIDDEN_MISC, len)
```

The encoder writes tag 2 at the end of the payload, and tag 0 (an escape of
the rest) when fewer than 6 bytes remain or the block length is 0 or runs
past the payload.

**usercmd blocks** (`SegmentWalker::usercmd`). mvdsv writes one hidden block of
type 1 per player input when usercmd recording is on (`sv_user.c`
`SV_DebugClientCommand`), 23 bytes: player u8, dropnum u8, msec u8, pitch,
yaw and roll as f32le, forward, side and up moves as i16le, buttons u8,
impulse u8. Every block of type 1 and length 23 is coded so, field by field
into stream `USERCMD`, whatever its bytes; its angles on the 360/65536 grid go to
the numeric group of section 5 (stream `USERCMD_ANGLE`). The context's kind is in
bits 12-15:

```
usercmd():
    [p1, p2] = usercmd_previous_players
    player = code_symbol(USERCMD, min(p1, 31) | (min(p2, 31) << 5))            // kind 0
    usercmd_previous_players = [player, p1]
    s = usercmds[player]
    s.drop = code_symbol(USERCMD, (1 << 12) | s.drop)
    s.msec = code_symbol(USERCMD, (2 << 12) | s.msec)
    grid = symbol(USERCMD, (3 << 12) | s.grid)                           // not written
    if grid > 7: error; return
    s.grid = grid
    off = p
    for a in 0..3:
        if grid & (1 << a): reserve(4)                           // angle a, section 5
        else: for k in 0..4: code_symbol(USERCMD, (4 << 12) | (a << 2) | k)
    if grid != 0: push UsercmdRecord{pitch_offset = off, number = player, grid_axes = grid}
    for m in 0..3:
        lo = code_symbol(USERCMD, (5 << 12) | (m << 8) | (s.moves[m] & 0xff))
        hi = code_symbol(USERCMD, (6 << 12) | (m << 8) | lo)
        s.moves[m] = lo | (hi << 8)
    s.buttons = code_symbol(USERCMD, (7 << 12) | s.buttons)
    s.impulse = code_symbol(USERCMD, (8 << 12) | s.impulse)
    usercmds[player] = s
```

`grid` bit `a` says angle `a` is on the grid: there is an integer `n` in
-32768..32767 with `f32(n * 45 / 8192)` bit for bit equal to the angle
(`n * 45 / 8192` is exact in f32: `n * 45` has at most 21 bits and 8192 is a
power of two). The client sends its angles as 16-bit values and the server
stores exactly that value times 360/65536, so in practice every angle is on
the grid; -0.0, NaN, +180.0 and anything else are not, and are kept as their
four bytes. The encoder computes the bits so (`math.rs:on_grid`). A type-1
block of another length goes to `HIDDEN_MISC` like every other block. A segment
without usercmd blocks has neither `USERCMD` nor `USERCMD_ANGLE` in its table.

### 4.12 Contexts of the structural streams

| stream | context(s) |
|---|---|
| `TIME` | `last_type \| (last_time_byte << 8)` |
| `TYPE` | `last_type \| (min(tb, 32) << 8)` |
| `COMMAND` | class 0-14: `class \| (min(index, 15) << 4) \| (first_class << 8) \| (ti << 12)`; class 15: `0x8000 \| previous_command \| (ti << 8) \| (min(index, 7) << 11)` |
| `PLAYER_FLAGS` | `0x8000 \| (prev & 0x7fff)`; `0x100 \| ((prev >> 6) & 0xff)`; `0x200` |
| `PLAYER_FRAME` | `ps.frame \| (flags & 0x300)` |
| `PLAYER_WEAPON_FRAME` | `ps.weapon_frame` |
| `ENTITY_GAP` | `ectx`; `1 << 12`; `2 << 12` |
| `ENTITY_BITS` | `es.bits \| (es.model << 7)` (9-bit index) |
| `ENTITY_MORE` | `(bits >> 9) \| (m0 << 8)`; `0x8000`; `0x8001` |
| `ENTITY_ANGLE` | `i \| (q << 2) \| (model << 6)`; FTE float: `3 \| (model << 6) \| (k << 14)` |
| `STAT_INDEX` | `stat_previous_index \| (previous_command << 8)` |
| `STAT_VALUE` | `i \| (stat_previous_residual << 8)` |
| `SOUND_CHANNEL` | `previous_command`; `0x100 \| lo` |
| `SOUND_NUMBER` | `(ch & 7) \| (min(e, 33) << 3)` |
| `TEMP_ENTITY_TYPE` | `previous_temp_entity \| ((previous_command == SVC_TEMP_ENTITY) << 8)` |
| `USERCMD` | `min(p1, 31) \| (min(p2, 31) << 5)`; `(kind << 12) \| x` for the other fields (section 4.11) |

Every context fits in 16 bits. The blob streams the walker reads byte by
byte (`blob_byte`, `code_blob_byte`, `code_blob_residual`, `code_blob_bytes`)
are `MASK`, `ESCAPE_LENGTH`, `HIDDEN_TAG`, `HIDDEN_LENGTH`, `HIDDEN_TYPE`,
`PLAYER_NUMBER`, `PLAYER_MODEL`, `PLAYER_SKIN`, `PLAYER_EFFECTS`,
`ENTITY_HEADER`, `ENTITY_MODEL`, `ENTITY_FRAME`, `ENTITY_MISC`,
`STAT_LONG_INDEX`, `SOUND_MISC`, `TEMP_ENTITY_MISC` and `PRINT_LEVEL`;
`HIDDEN_DAMAGE`, `FRAGS`, `STAT_LONG_VALUE`, `RAW`, `TAIL`, `SET`, `TEXT`,
`PING`, `NAILS`, `HIDDEN_MISC`, `SOUND_FLOAT`, `TEMP_ENTITY_FLOAT` and the
generic streams `GENERIC + c` are read with `copy_blob`.

## 5. Numeric groups (`numeric.rs`)

The walk (section 4) leaves every numeric field of the segment as a reserved,
unwritten slot and appends a record for it to one of five record lists:
player infos, packet entities, sounds, temp entities and usercmds. After
the walk, the numeric groups fill these slots. A group iterates one record
list in walk order; for each value it computes a prediction `pred` and a
feature vector `f` (up to 4 values, u8 each) from group-local state, takes a residual
`r` from its stream with context `numeric_context(stream, f)` (section 6.4),
writes `pred + r` at the slot's offset and updates its state.

The groups (`numeric.rs:GROUPS`, `Group::streams`):

| Group | Streams | Record list |
|---|---|---|
| `PlayerOrigin(a)`, a = 0..2 | 17 + a (16-bit), 28 + a (float) | player infos |
| `PlayerAngle(a)`, a = 0..2 | 20 + a | player infos |
| `EntityOrigin(a)`, a = 0..2 | 23 + a (16-bit), 31 + a (float) | packet entities |
| `UsercmdAngles` | 34 | usercmds |
| `Sound` | 26 | sounds |
| `TempEntity` | 27 | temp entities |

Every group has its own state, starting from zero, even where two groups
compute the same values (the frame times of the player groups). The
`Sound` and `TempEntity` groups read coordinates that the origin groups
write (section 5.6), so they run after all other groups; the other groups are
independent of each other and may run in any order, also in parallel. The
reference runs them in `GROUPS` order after the walk (`segment.rs:decode`).

### 5.1 Value write rules (`numeric.rs:code_u16`, `code_u32`)

* 16-bit field: `v = (pred + (r as u16)) mod 2^16`, written u16le at the
  slot. The residual kept as state is `(r as u16) as i16 as i32`.
* 32-bit field: `v = (pred + (r as u32)) mod 2^32`, written u32le. The
  residual kept as state is `r`.
* A read outside the segment's output reads 0; a write outside it is
  dropped. Neither happens in a well-formed walk.

The encoder's residual is `(v - pred) as i16 as i32` for a 16-bit field
and `(v - pred) as i32` for a 32-bit field.

### 5.2 Prediction helpers

```
time_deltas(frame_times = (last, previous), t) -> (now, before):
    now    = (t - last) as i32            // u32 wrapping, then reinterpreted
    before = (last - previous) as i32
    frame_times = (t, last)

predict_time_scaled(last u16, previous u16, now, before) -> u16:
    velocity = (last - previous) as i16 as i32
    if now == before or before <= 0 or now < 0:
        return last + (velocity as u16)                              // mod 2^16
    return last + (div_round(velocity * min(now, 32767), min(before, 32767)) as u16)

div_round(a i32, b i32 > 0) -> i32:          // halves round away from zero
    n = |a| + b / 2                          // u32
    q = n / b                                // floor
    return a < 0 ? -q : q
```

The reference divides through a reciprocal table for `b < 256`; it equals
plain integer division for every `n < 2^31` (`math.rs:div_round`, test
`div_round_matches_reference`).

`AxisTrack` (per player or entity and axis, all zero initially): `last,
previous, before_previous: u16; residual: i32; float_last_bits,
float_previous_bits: u32; float_residual_bits: i32`.

### 5.3 Player origins: streams 17-19 and 28-30 (`player_origin_step`)

Group `PlayerOrigin(a)`. State: `AxisTrack` and `frame_times` per player
number (256 each). For each player info record `p` (fields of section 4:
`offset`, `time`, `flags`, `number`, `has_float_coords`), first
`(now, before) = time_deltas(frame_times[p.number], p.time)`, for every
record of that player whatever its flags; then with `s = tracks[p.number]`
and `sent = p.flags & (1 << a)`:

```
if p.has_float_coords:                                  // stream 28 + a
    if sent:
        offset = p.offset + 4 * popcount(p.flags & ((1 << a) - 1))
        pred   = float_last_bits + (float_last_bits - float_previous_bits)   // u32 wrapping
        f      = [residual_bucket(float_residual_bits), a]
        v, r'  = 32-bit field (offset, pred, f)
        float_previous_bits = float_last_bits; float_last_bits = v; float_residual_bits = r'
    else:
        float_previous_bits = float_last_bits; float_residual_bits = 0
    // the 16-bit state is untouched
else:                                                   // stream 17 + a
    if sent:
        pred     = predict_time_scaled(last, previous, now, before)
        offset   = p.offset + 2 * popcount(p.flags & ((1 << a) - 1))
        velocity = (last - previous) as i16 as i32
        f        = [residual_bucket(residual), residual_bucket(velocity)]
        v, r'    = 16-bit field (offset, pred, f); residual = r'
    else:
        residual = 0; v = last
    previous = last; last = v
    // the float state is untouched
```

### 5.4 Player angles: streams 20-22 (`player_angle_step`)

Group `PlayerAngle(a)`. State and `time_deltas` as in section 5.3, separate from
the origin groups'. For each player info record `p`, with `s =
tracks[p.number]`:

```
if p.flags & (8 << a):
    pred   = predict_time_scaled(last, previous, now, before)
    width  = p.has_float_coords ? 4 : 2
    offset = p.offset + width * popcount(p.flags & 7)
                      + 2 * popcount((p.flags >> 3) & ((1 << a) - 1))
    previous_velocity = (previous - before_previous) as i16 as i32
    f      = [residual_bucket(residual), residual_bucket(previous_velocity)]
    v, r'  = 16-bit field (offset, pred, f); residual = r'
else:
    residual = 0; v = last
before_previous = previous; previous = last; last = v
```

Angles are 16-bit whether the coordinates are float or not.

### 5.5 Entity origins: streams 23-25 and 31-33 (`entity_origin_step`)

Group `EntityOrigin(a)`. State: `AxisTrack` per entity, indexed `number &
2047`. For each packet entity record `e` (fields of section 4: `offset`,
`number`, `bits`; `bits` holds `1 << 2i` for
origin `i` sent, `2 << 2i` for angle `i` sent, and `PE_IN_PREVIOUS` (64),
`PE_FLOAT` (128), `PE_WIDE` (256)), with `s = tracks[e.number & 2047]`:

```
if e.bits & (1 << (2 * a)):                                      // origin a sent
    offset = origin_offsets(e)[a]
    if e.bits & PE_FLOAT:                                        // stream 31 + a
        pred  = float_last_bits + (float_last_bits - float_previous_bits)
        f     = [residual_bucket(float_residual_bits), a]
        v, r' = 32-bit field (offset, pred, f)
        float_previous_bits = float_last_bits; float_last_bits = v; float_residual_bits = r'
    else:                                                        // stream 23 + a
        velocity = (last - previous) as i16 as i32
        pred     = last + (velocity as u16)
        in_previous = (e.bits & PE_IN_PREVIOUS) ? 1 : 0
        f     = [residual_bucket(residual), residual_bucket(velocity), in_previous]
        v, r' = 16-bit field (offset, pred, f); residual = r'
        previous = last; last = v
else:
    previous = last; residual = 0
    float_previous_bits = float_last_bits; float_residual_bits = 0
```

`origin_offsets(e)` (`walker.rs:PacketEntityRecord::origin_offsets`):
`coord = PE_FLOAT ? 4 : 2`, `angle = PE_WIDE ? 2 : 1`, `pos = e.offset`;
for `i` in 0..3: if `bits & (1 << 2i)`: `o[i] = pos; pos += coord`; if
`bits & (2 << 2i)`: `pos += angle`.

### 5.6 Usercmd angles (34), sounds (26), temp entities (27)

**Usercmd angles, stream 34** (`numeric.rs:usercmd_angles`). State: three
`AxisTrack`s per player number (256). For each usercmd record `u` (section 4:
`pitch_offset`, `number`, `grid_axes`), for `a` in 0..3 with `u.grid_axes &
(1 << a)` (other axes are skipped and their state is untouched), with `s =
tracks[u.number][a]`:

```
velocity          = (last - previous) as i16 as i32
previous_velocity = (previous - before_previous) as i16 as i32
pred  = last + (velocity as u16)                                 // mod 2^16
f     = [residual_bucket(residual), residual_bucket(velocity),
         residual_bucket(previous_velocity), a]
v     = pred + (r as u16)                                        // the grid value
write grid_f32(v) as u32le at u.pitch_offset + 4a
residual = (v - pred) as i16 as i32
before_previous = previous; previous = last; last = v
```

`grid_f32(g)` (`math.rs:grid_f32`) is the f32 `(g as i16) * 45/8192`,
which is exact. The encoder's residual is `(g - pred) as i16 as i32` for
the grid value `g` of the angle (`math.rs:on_grid`, section 4).

**Sounds, stream 26** (`numeric.rs:sound_origins`). For each sound record
`s` (section 4: `offset`, `references[3]`, `kind`, `number`), for `i` in 0..3:
`pred = references[i] == NO_OFFSET ? 0 : the u16le already decoded at references[i]`,
`f = [i | kind << 2, number]`, 16-bit field at `offset + 2i`.

**Temp entities, stream 27** (`numeric.rs:temp_entity_origins`). State:
`last[3]` u16, zero, across the segment. For each temp entity record `t`
(section 4: `offset`, `references[3]`, `kind`):

```
if t.kind in {TE_LIGHTNING_1, TE_LIGHTNING_2, TE_LIGHTNING_3}:   // 5, 6, 9
    for i in 0..3: start[i] = 16-bit field (offset + 2i,
                       pred = references[i] as for sounds, f = [i | 1 << 2, kind])
    for i in 0..3: last[i]  = 16-bit field (offset + 6 + 2i,
                       pred = start[i], f = [i | 2 << 2, kind])
else:
    for i in 0..3: last[i]  = 16-bit field (offset + 2i,
                       pred = last[i], f = [i, kind])
```

`references` point only at 16-bit player or entity origin slots, written by the
origin groups before these two groups run.

### 5.7 Feature vectors (summary)

| Stream | f0 | f1 | f2 | f3 |
|---|---|---|---|---|
| 17-19 | residual_bucket(residual) | residual_bucket(velocity) | | |
| 20-22 | residual_bucket(residual) | residual_bucket(previous_velocity) | | |
| 23-25 | residual_bucket(residual) | residual_bucket(velocity) | in_previous | |
| 26 | i \| kind << 2 | sound number | | |
| 27 | i \| sel << 2 (sel 0 point, 1 beam start, 2 beam end) | kind | | |
| 28-30, 31-33 | residual_bucket(float_residual_bits) | axis | | |
| 34 | residual_bucket(residual) | residual_bucket(velocity) | residual_bucket(previous_velocity) | axis |

## 6. Entropy coding (`rans.rs`)

Every structural and numeric stream of a segment is coded with static rANS
(Giesen's scheme, one state) under the stream's model from the models
block (section 2): one model per stream id for the whole file, shared by every
segment, never changed by decoding. A segment's stream table entry names
the stream; its kind (structural or numeric, from its id, section 3) decides the
alphabet and the coder. No stream carries a model or coder id of its own. A
structural or numeric stream in a stream table without a model in the
models block is an error (`rans.rs:file_model`). Blob streams have no
body of their own: their bytes are coded as tokens on three of these streams
(section 6.6).

### 6.1 Stream body

```
rans_len u32 | rans[rans_len] | extra (rest of the body)
```

`rans` is the rANS stream (section 6.4). `extra` holds the mantissa
bits of a numeric stream (section 6.4); for a structural stream it must be empty
(`rans.rs:unpack`, `ByteDecoder::new`).

### 6.2 Model (`rans.rs:read_model_header`, `read_model`)

A raw model is these bytes: the meta model as the models block stores it,
every other model as its entry decodes (section 2.2):

```
raw model := own_count u32 | contexts | tables
contexts  := own_count x u16                   // little-endian, strictly increasing
tables    := (own_count + 1) x q[alphabet]     // one byte per symbol
```

* `own_count` is at most 65536: one table per 16-bit context.
* `alphabet` follows from the stream's kind: 256 structural, 65 numeric;
  256 for the meta model.
* A raw model is exactly `4 + 2 * own_count + (own_count + 1) * alphabet`
  bytes long: an entry's `raw_len` must be that, with the `own_count` its
  first four bytes hold.

Each table is a vector of log-quantized counts `q[s]`, 0 meaning absent;
every byte value is valid. The first table is the fallback: it serves
every context that is not in `contexts`. Table `i + 1` serves context
`contexts[i]`.

### 6.3 From quantized counts to frequencies (`math.rs:quantized_weight`, `normalize`; `rans.rs:Table`)

```
STEPS_PER_OCTAVE = 2
STEP_POWERS = [65536, 92682]                  // 2^(k/2) in 16.16 fixed point
quantized_weight(q) -> u64:
    if q == 0: return 0
    step = q - 1; octave = step / 2; k = step % 2
    return (octave >= 40) ? (2^64 - 1) >> 8 : STEP_POWERS[k] << octave

normalize(w[0..alphabet]) -> f[0..alphabet]   (total 4096):
    total = sum of w (u128)
    if total == 0: return all zeros
    sum = 0
    for s: if w[s] > 0: f[s] = max(floor(w[s] * 4096 / total), 1); sum += f[s]
    diff = 4096 - sum
    while diff != 0:
        b = index of the largest f (the first on ties: scan from 0 with strict >)
        if diff > 0: f[b] += diff; diff = 0
        else:
            take = min(-diff, f[b] - 1, (f[b] + 1) / 2)
            if take <= 0: break
            f[b] -= take; diff += take
    return f
```

`freq = normalize(quantized_weight(q[s]))`, `cumulative[0] = 0`,
`cumulative[s + 1] = cumulative[s] + freq[s]`. A table is **empty** if no
symbol has `freq > 0` and **single-symbol** if exactly one has (its `freq`
is then 4096). A symbol is present exactly when `q[s] > 0`.

### 6.4 Symbol decode (`rans.rs:DecoderModel::decode`, `RansDecoder`)

State: `x` u32 and a word cursor over `rans`.

```
init: hi = word(); lo = word(); x = (hi << 16) | lo
word(): the u16le at pos (0 if fewer than 2 bytes remain); pos += 2

decode(context):
    table = the table of context (contexts) or the fallback
    if table is empty: error
    if table is single-symbol: return its symbol         // the state is not used
    slot = x & 4095
    s = the symbol with cumulative[s] <= slot < cumulative[s + 1]
    x = freq[s] * (x >> 12) + slot - cumulative[s]      // u32 wrapping
    if x < 2^16: x = (x << 16) | word()
    return s
```

A symbol of a single-symbol table is not coded. The reference's
decoder-side layout (search tables, slot LUTs, a context map) is an
implementation detail.

**Structural streams** (`ByteDecoder`): `context = walker_context & 0xFFFF`
(every walker context fits 16 bits, section 4); the symbol is the byte.

**Numeric streams** (`NumericDecoder`): `context = numeric_context(stream,
f)`; the symbol is the bucket `b`, 0 to 64. Then

```
n = (b + 1) / 2                         // bit length of |v|
m = ((n > 0 ? 1 : 0) << max(n - 1, 0)) | bits.get(max(n - 1, 0))      // u32
v = (b is even) ? (m as i32).wrapping_neg() : m as i32                  // b = 0 gives 0
```

`bits` reads `extra` as an LSB-first bit stream (bit `k` of the stream is
bit `k % 8` of byte `k / 8`), zeros past the end. `get(n)` reads `n` bits,
the first one read being bit 0 of the result; `get(0) = 0`. Buckets 63
and 64 (n = 32) give `m` up to 2^32 - 1; `m` is reinterpreted as i32
before the sign, and the negation wraps. The encoder's bucket is `2n - 1 +
(v < 0)` with mantissa `|v|` without its leading 1, `n - 1` bits
(`residual_bucket`, section 1).

`numeric_context(stream, f)` (`rans.rs:numeric_context`), with `c(v, m) =
min(v, m)`:

| Stream | Context |
|---|---|
| 17-19 | `c(f0, 19) \| c(f1, 19) << 5` |
| 20-22 | `c(f0, 23) \| c(f1, 23) << 5` |
| 23-25 | `c(f0, 17) \| c(f1, 17) << 5 \| f2 << 10` |
| 26, 27 | `f0 \| f1 << 4` |
| 34 | `c(f0, 23) \| c(f1, 23) << 5 \| c(f2, 15) << 10 \| f3 << 14` |
| 28-33 | `c(f0, 63) \| f1 << 6` |
| 35, 36 | `f0` |

### 6.5 End of a stream

A structural or numeric stream is complete only if exactly `count` symbols
(its stream table entry, section 3; symbols of single-symbol tables count,
though they are not coded) were decoded, the state is back at 2^16 and
every word of `rans` was read (the word cursor equals `rans_len`; an empty
`rans` therefore never completes: the cursor is past its end). A numeric
stream's mantissa bits must also end within the last byte of `extra`:
`ceil(bits_read / 8) == len(extra)`, at most 7 padding bits
(`ByteDecoder::is_done`, `NumericDecoder::is_done`, `BitReader::is_finished`).
An incomplete stream is an error.

### 6.6 Blob tokens (`lz.rs`)

The blob streams of a segment, in stream table order, form one string of
`blob_total` bytes (section 3.1). It is coded as LZ77 tokens: a literal
byte, or a match that copies bytes the tokens before it produced. Three
streams hold the tokens, each coded as any other stream of its kind:

| Stream | Per token | Context |
|---|---|---|
| 35 `LZ_LENGTH` | every token: the match length, 0 for a literal | `f0 = min(residual_bucket(length of the token before), 63)`, 0 for the first token |
| 16 `LZ_LITERAL` | a literal: the byte | the byte produced before it, 0 at the start |
| 36 `LZ_DISTANCE` | a match: the distance | `f0 = min(residual_bucket(length), 63)` |

```
out = []                                   // the blob bytes
previous = 0
while len(out) < blob_total:
    length = value(35, [min(residual_bucket(previous), 63)])
    if length < 0: error
    if length == 0:
        out.push(symbol(16, last byte of out, or 0 if out is empty))
    else:
        distance = value(36, [min(residual_bucket(length), 63)])
        if distance < 1 or distance > len(out): error
        if length > blob_total - len(out): error
        repeat length times: out.push(out[len(out) - distance])    // byte by byte: a match may overlap itself
    previous = length
```

`value` decodes a numeric value (section 6.4) and `symbol` a structural symbol;
a stream that is absent or has no symbol left is an error. Afterwards
streams 35 and 36 must be complete (section 6.5), and stream 16 once the
walk is done (section 3.3). With `blob_total = 0` no token is read. A
decoder accepts any tokens that follow these rules; the tokens an encoder
writes are in section 8.

## 7. Validation rules and limits

The reference has one reader: whole-file decoding (`decode.rs:decompress`) collects what
in-order streaming (`decode.rs:Decoder`) reads, so both accept and refuse the same files.
The reference tests this on every vector and on mutated files.

**Layout rules** (`decode.rs:Decoder`, `format.rs:Layout`), each checked before the
block it covers is decoded; refuse the file if any fails:

1. Header (section 2): the magic `MVDZ` (else *NotMvdz*), version 3 (else
   *UnsupportedVersion*), all 60 bytes present.
2. Models block (section 2): its length field present, the length at most 2^24, the body
   present; if the length is not 0, the body follows section 2.2 (the meta model whole,
   count not 0, ids strictly increasing, each a modelled stream, the raw lengths within
   their limit, consumed exactly, and every model's rules: its stream decodes to exactly
   `raw_len` bytes and is complete, the raw length matches its own count, the own count is
   at most 65536, contexts strictly increasing).
3. Each segment (section 2), in file order, until the running sum of `raw_len` equals the
   header's `size`: its length field present, `len` at most 2^21, the 12 header bytes
   and `len` coded bytes present, `raw_len` at least 1 and at most 2^20, the running
   sum of `raw_len` at most the header's `size`.
4. After the last segment: no byte follows.

**Content rules**, applied when a segment is decoded: the transform header and stream
table (section 3), every stream body consumed exactly (section 6), the walk (section 4), the numeric groups
(section 5), the segment's CRC-32 (section 3). After the last segment, the whole-demo
check (section 3): the CRC-32 of the demo.

A model the models block lists and no segment uses is valid and ignored. A segment that
codes a structural or numeric stream the models block has no model for is corrupt.

**Errors.** The reference reports one of these `Error` variants; another decoder should
report the same category in its own words (the messages in `MANIFEST.tsv` are the
reference's):

| Variant | When |
|---|---|
| `NotMvdz` | the magic is not `MVDZ` |
| `UnsupportedVersion` | the version is not 3 |
| `Corrupt` | every other rule of this document |
| `MemoryLimit` | the file is valid but needs more memory than the reader allows, or a size that does not fit its address space |
| `Io` | the input could not be read |
| `NotMvd` | encoding only: the input does not start like an MVD demo (section 8) |
| `DemoTooLarge` | encoding only: the demo needs more than 65536 segments (section 8) |
| `Internal` | encoding only: the output does not decode to the input (a bug) |

When a file breaks several rules, the one reported depends on the order of the checks:
the reference checks the header, the models block, then the segments in file order, a
few at a time (their headers, then their content rules), then the whole-demo checks.
Each hostile vector breaks one rule only.

**Limits.** All are rules of the format:

| Limit | Value | Rule |
|---|---|---|
| segment raw length (`raw_len`) | 1 to 2^20 bytes | section 2 |
| segment coded length (`len`) | at most 2^21 bytes | section 2 |
| models block | at most 2^24 bytes | section 2 |
| raw models, the meta model included | at most 2^24 bytes together | section 2 |
| stream table | each id once, `count <= 2 * raw_len + 16` | section 3 |
| blob streams of a segment | at most `2 * raw_len` bytes together | section 3 |
| own tables per model | at most 65536 | section 6 |

With these, every size a decoder allocates is bounded: a segment yields at most 2^20
bytes and takes at least its 16 header bytes of the file, its blob bytes are at most
twice its length, and the raw models of a file are at most 16 MiB together, checked from
the models block's fields before any of them is decoded. Every decoder applies these
limits, so all decoders accept the same files.

A decoder may cap its memory on top and refuse a file that needs more with `MemoryLimit`.
The reference (`memory.rs:DEFAULT_MAX_MEMORY`) refuses models whose decoder tables take
more than 1 GiB, and whole-file decoding refuses a demo of more than 1 GiB; in-order
streaming holds a few segments at a time whatever the demo's size. A decoder must never
abort the process on a declared size.

**Not checked.** The header's SHA-256 (section 10) and `duration_ms`, and the segments'
`duration_ms`: no reader recomputes them. Where the demo is cut into segments. The `fte2` field
of a transform header beyond carrying it, and all three protocol fields of a stored
segment. Padding bits after a numeric stream's mantissa bits.

---------------------------------------------------------------------------

## 8. Encoder rules

A decoder does not depend on these. They are exact: every step is integer arithmetic or a
single IEEE operation, so an encoder that follows them writes the same bytes as the
reference for the same demo, in any language. The test vectors are that contract: an
encoder is checked by reproducing them byte for byte, and `vectors/lz-tokens.tsv` checks
its blob parser (rule 10) on its own.

1. **Input** (`encode.rs:check_demo`): an encoder refuses input that does not start like
   an MVD demo (`NotMvd`): empty input, a gzip stream (bytes `1f 8b`), or a first frame
   that is not a whole frame of type 1, 3, 4, 5 or 6 carrying more than its header,
   unless that frame walks without an escape (section 4) or 16 more whole frames of type 1 to 6
   follow it. A damaged demo with an intact start is encoded. The unchecked entry point
   (the fuzz targets' `encode_any`) writes any bytes.
2. **Cut points** (`segment.rs:cut_points`): an empty demo has no segment. Otherwise walk
   the frames from the start (`protocol.rs:frame_len`, section 4) and cut after a frame whenever
   at least 2^18 bytes have accumulated since the last cut and bytes remain. When a
   frame would take the segment past 2^20 bytes, or the bytes no longer frame, the rest
   of the demo is cut every 2^20 bytes from the last cut; the last piece is the last
   segment. At most 65536 segments (`DemoTooLarge`): about 16 GiB of demo.
3. **Protocol state**: the first pass walks the segments in order (section 4). Segment k's
   `fte`, `fte2`, `mvd1` are the state at the end of the last segment before it whose
   walk succeeded (0 for the first).
4. **Models** (`segment.rs:ModelBuilder`, `rans.rs:Tally::file_model`): every
   structural and numeric stream gets one model, built from its symbol counts per
   context summed over every segment whose walk succeeded:
   * A context gets its own table where its entropy plus 48 + 6 bits per present symbol
     is below its cross-entropy against the fallback, or where the fallback lacks one of
     its symbols. Counts here are raw, not quantized. Two passes test every context
     against a fixed fallback: pass 1 against the sum of all contexts, pass 2 against the
     sum of the contexts pass 1 left without a table. The own tables are those of pass 2;
     the fallback table is the sum of the contexts pass 2 left without one. The costs are
     integers, so every encoder selects the same tables:

     ```
     log2_fixed(n) -> u64, n >= 1:             // log2 with 16 fraction bits, rounded down
         int = floor(log2(n))
         x = n shifted to [2^30, 2^31)         // left or right by |30 - int|
         frac = 0
         repeat 16 times:
             x = (x * x) >> 30                 // below 2^62: fits u64
             frac = frac << 1
             if x >= 2^31: x = x >> 1; frac = frac | 1
         return int << 16 | frac

     entropy(counts)    = sum over count > 0 of count * (log2_fixed(total) - log2_fixed(count))
     cross(counts, ref) = sum over count > 0 of count * (log2_fixed(ref_total) - log2_fixed(ref[s]))
     own table if a count > 0 has ref[s] = 0, or
                  entropy(counts) + ((48 + 6 * present) << 16) < cross(counts, fallback)
     ```

     Check values: `log2_fixed(3) = 103872`, `log2_fixed(10) = 217705`,
     `log2_fixed(1000) = 653117`, `log2_fixed(2^32 - 1) = 2097151`.
   * A table stores `quantize(count)` per symbol: the `q` whose `quantized_weight`
     (section 6.3) is nearest to the count in ratio:

     ```
     quantize(c) -> u8:
         if c == 0: return 0
         w = c << 16
         q = 2 * floor(log2(c)) + 1
         if quantized_weight(q + 1) <= w: q = q + 1
         return (w * w > quantized_weight(q) * quantized_weight(q + 1)) ? q + 1 : q   // u128
     ```
   * A segment whose walk fails counts as all tail: only its blob tokens (streams 16, 35
     and 36) are counted. A segment whose walk succeeds is counted as walked, also when
     rule 5 then writes it another way: the models do not depend on pass 2.
   * While the models block of all models would exceed 2^24 bytes, coded or raw
     (section 7), the model with the longest raw model is dropped (ties: highest stream
     id), never those of streams 16, 35 and 36 (`segment.rs:fit_models_block`).
5. **Segments** (`encode.rs:code_segments`): a segment is written the first of these
   ways that works:
   * transform-coded (section 3), if its walk succeeds;
   * all tail: no walker stream, every byte in the `TAIL` blob stream, `tail = raw_len`;
   * stored (section 3.1).

   A way works if every structural and numeric stream it codes has a model with a code
   for each of its symbols under its context, no stream's count and not the blob total
   exceed the limits of section 7, and the coded length is at most 2^21. The protocol
   fields are the segment's start state in all three. Streams are written in id order,
   empty streams omitted; the blob bytes are the non-empty blob streams in id order.
6. **Models block** (`rans.rs:write_models_block`): lists only the models some
   segment uses, in id order; length 0 when none is used. The meta model is built as in
   rule 4 from the bytes of the listed raw models, each byte counted under the byte
   before it in its model (0 for the first), with 16 + 8 x 256 bits as the cost of an
   own table whatever its symbols.
7. **Self-check** (`encode.rs:decodes_to`), always on: the encoder decodes its output,
   each segment against its input, then the whole-demo checks. A file that fails is
   `Internal`: a bug in the encoder.
8. **Determinism**: the output depends only on the input, not on the thread count.
9. **Duration** (`segment.rs:duration_ms`): a segment's `duration_ms` is the sum of the
   time bytes of the whole frames it starts with (`protocol.rs:frame_len`, section 4),
   stopping at the first bytes that do not frame. The header's `duration_ms` is the sum
   of the segments'.
10. **Blob tokens** (`lz.rs:parse`): the canonical parse of the blob bytes. Greedy; at
    each position the longest match among the 32 latest earlier positions with the same
    hash, the nearest of equally long ones; a match is taken from 4 bytes:

    ```
    hash(p) = (u32le(bytes[p .. p + 4]) * 0x9E3779B1 mod 2^32) >> 17        // 15 bits
    latest[2^15] = none; earlier[len] = none
    p = 0
    while p < len:
        best_length = 0
        if p + 4 <= len:
            candidate = latest[hash(p)]; steps = 0
            while candidate != none and steps < 32:
                length = common prefix of bytes[candidate ..] and bytes[p ..]   // up to the end of the bytes
                if length > best_length: best_length = length; best = candidate
                candidate = earlier[candidate]; steps += 1
        if best_length >= 4: emit match(best_length, p - best); advance = best_length
        else:                emit literal(bytes[p]);            advance = 1
        for q in p .. p + advance:
            if q + 4 <= len: earlier[q] = latest[hash(q)]; latest[hash(q)] = q
        p += advance
    ```

    The tokens are coded as section 6.6 reads them. `vectors/lz-tokens.tsv` holds inputs
    with their tokens.

---------------------------------------------------------------------------

## 9. Notes for implementers

1. **Order of work in a segment.** Walk the structure first (section 4), then run the numeric
   groups in `numeric.rs:GROUPS` order (section 5): the sound and temp-entity groups predict
   from origins the player and entity groups wrote, so they run after them.
2. **Record lists are bounded by the stream table.** Every record consumes a symbol from
   its stream (one `PLAYER_NUMBER` per player record, one `ENTITY_BITS` per entity
   record, one `SOUND_NUMBER` per sound, one `TEMP_ENTITY_TYPE` per temp entity), and
   over-reading a stream fails its completeness check.
3. **An exhausted structural stream reads as 255** (`segment.rs:DecodeIo`) and marks
   failure; every loop of the walk stops on it. A port may stop at once: the result is
   an error either way. A stream decoder does not stop at `count` by itself; the
   completeness check after the walk enforces it.
4. **Arithmetic that could overflow on invalid input** wraps in the reference: time
   differences (`numeric.rs:time_deltas`), time-scaled predictions
   (`predict_time_scaled`) and residuals. Use wrapping 32-bit and 16-bit arithmetic
   (in Go: compute in `int32`/`uint16` and let it wrap before any clamp).
5. **The escape length** (`walker.rs:escape`, section 4) is 7 bits per byte, low group first, at
   most 5 bytes; a fifth byte with its top bit set is an error, and its bits above 32
   are lost.
6. **The rANS alphabet follows from the stream kind** (256 structural, 65 numeric); it
   is not stored.
7. **Coordinate offsets may point at 4-byte float fields** when the protocol switched to
   float coordinates earlier in the same segment (an `svc_serverdata` mid-segment) and
   back; sounds and temp entities then read the low 16 bits of that field as their
   prediction. `observe_server_data` takes effect within a payload: later commands of the
   same payload use the new coordinate width.
8. **Temp entity references** use a player's origin only for entities 1 to 32; beams from
   other entities predict their start from 0.
9. **The `TEXT` blob is shared** by `svc_print` (after the level byte), `svc_stufftext`
   and `svc_centerprint`, all sized as a NUL-terminated string. The `PING` and `NAILS`
   blobs are shared by several commands, each sized by its own rule.
10. **Single-symbol tables**: a symbol from a single-symbol table is not coded and leaves
    the rANS state alone. The final-state check (section 6) needs the state read from the
    first 4 bytes even when no symbol is coded.
11. **The TIME context uses the unclamped previous time byte**; TYPE uses
    `min(time byte, 32)`.
12. **Time is segment-relative**: every walker and predictor state starts fresh in each
    segment, time at 0. A player's second record in a segment measures its previous
    time delta from 0.
13. **Per-player timing**: player origins and angles keep the last two frame times per
    player and advance them on every record, sent or not. Entity origins use no time.
14. **The models are read once per file**, when the layout is read, and the same
    read-only models serve every segment (the reference shares them between threads).
    Their memory counts once against a cap, not per segment.
15. **The demo's CRC-32 need not be hashed**: it follows from the segments' checked
    CRC-32s and lengths (zlib's `crc32_combine`), as the reference does
    (`decode.rs:Decoder::decode_batch`). Hashing the output again gives the same value.

---------------------------------------------------------------------------

## 10. Security considerations

**Forged identity.** The SHA-256 in the header is a claim: no reader recomputes it.
Anyone can build a valid file whose header names another demo's hash (with correct
CRC-32s: CRCs are not authentication). A reader that only reads the header (a catalogue,
`mvdz info`) must not treat the hash as proof of the content; a consumer that must
authenticate a demo hashes the decoded bytes. A signature must sign a hash computed over
the decoded demo.

**Hostile input.** Every size a file declares is bounded by the limits of section 7 and a
decoder may cap its memory on top, so a small file cannot make a conforming decoder
allocate without bound. A decoder must fail with an error, never abort, hang or touch
memory out of bounds. The reference is fuzzed on all its readers.

**Commands in demos.** A demo replays what the server sent: `svc_stufftext` (console
commands such as `bind`, `alias`, `cmd`, `exec`), print and centerprint text with control
bytes, model and sound names, download offers. MVDZ carries them losslessly and does not
filter them. A client that plays a demo from an untrusted source must apply the command
filtering it applies to demos and servers; tools that display demo content must escape
it.

**Bit rot.** A CRC-32 covers every segment's decoded bytes and a second one the whole
demo; a full decode also checks the demo's size. Corruption is detected, not repaired:
there is no redundancy, and the reference refuses the whole file. Each segment decodes on
its own given the models block, so a decoder that skips a damaged segment can still
recover the others. Neither CRC detects a file replaced as a whole by another valid file;
only a hash kept elsewhere does.

---------------------------------------------------------------------------

## Appendix A. Encoders

**Raw model** (`rans.rs:raw_model`): `own_count`, the own contexts in ascending
order (u16le each), the fallback table and the own tables in the order of their contexts,
one byte per symbol: its quantized count.

**rANS encoder** (`rans.rs:rans_encode`): state `x = 2^16`. Encode the symbols in
reverse order, skipping those of single-symbol tables: with `freq`, `cum` of the symbol
in its table: if `x >= ((2^16 >> 12) << 16) * freq`, emit the word `x & 0xFFFF` and
`x >>= 16`; then `x = ((x / freq) << 12) + x % freq + cum`. Finally emit `x & 0xFFFF`
then `x >> 16`; reverse the emitted word list and write it as u16le words. With no coded
symbol the body is the 4 bytes of the state 2^16.

**Mantissa bits** (`math.rs:bucket`, `BitWriter`): for a value v != 0, `n = bitlen(|v|)`,
bucket `2n - 1 + (v < 0)`, and the `n - 1` bits of `|v|` below its leading one, appended
least significant first to the bit stream; v = 0 is bucket 0 with no bits. The last byte
is padded with zero bits.
