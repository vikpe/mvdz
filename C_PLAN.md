# C implementation plan (`mvdz_c/`)

## Context

Third implementation, in pure C, for QuakeWorld clients and servers (ezQuake, FTE,
MVDSV). Scope: a decoder only, whole buffers, one thread: `mvdz_decode`. Same error
categories as Rust and Go. No encoder (admins convert with the Rust or Go tool), no CLI.

Source to port: the decode path of the Go package (`mvdz_go/`, flat names), minus
`Reader`, goroutines and everything only `Encode` reaches.

## Decisions

| topic | pick | rejected |
|---|---|---|
| language | C99, no VLAs, no compiler extensions | C11 |
| dependencies | none: own CRC-32 | zlib (a client then has to link it) |
| linkage | unity build: `mvdz.c` includes the module files, every internal function `static`; only `mvdz.h` names are exported | one object per module with `mvdz_`-prefixed internals |
| walker | decode direction only: no `is_encode`, no source peeks; the `code_*` step names stay (FORMAT.md cites them) | both directions behind a flag, as Go |
| errors | `mvdz_error { kind, message }` by value from every fallible function, internal and public; `message` a static string | `int` code + `strerror` |
| allocation | `malloc`/`free`; the returned buffer freed with `free` | allocator hooks |
| platforms | Linux, macOS, Windows build and pass the tests; no platform code in the library | |
| build | `Makefile`; the library is one compiler call | CMake |

## Public API (`mvdz_c/mvdz.h`)

```c
typedef enum mvdz_error_kind {
    MVDZ_OK, MVDZ_CORRUPT, MVDZ_MEMORY_LIMIT, MVDZ_NOT_MVDZ, MVDZ_UNSUPPORTED_VERSION
} mvdz_error_kind;
typedef struct mvdz_error { mvdz_error_kind kind; const char *message; } mvdz_error;

/* *demo is freed with free() */
mvdz_error mvdz_decode(const uint8_t *file, size_t file_len,
                       uint8_t **demo, size_t *demo_size);
```

## Files

All in `mvdz_c/`, same module names as Rust and Go:

- `mvdz.h`, `mvdz.c` (unity file: includes the modules in dependency order)
- `error.c fields.c math.c streams.c format.c protocol.c numeric.c crc32.c`
- `rans.c lz.c segment.c walker.c decode.c`
- `tests/test.c` (includes `../mvdz.c`: reaches the statics, as Go in-package tests),
  `tests/sha256.c` (the vectors' digests; no reader checks the header's)
- `fuzz/decode.c` (libFuzzer)
- `bench/mvdz_bench.c`: reads a file, calls the library, writes a file. Arguments
  `decompress [-j N] INPUT OUTPUT`, `-j` ignored. `scripts/bench_cli.sh` compresses
  only the files missing from WORKDIR: fill it with the Rust tool, then run it with
  `MVDZ=mvdz_c/mvdz_bench` (its 4-thread row repeats the 1-thread one).
- `Makefile`, `.clang-format`, `README.md`, `AGENTS.md`

Each `.c` holds what `Decode` reaches in the Go file of the same name:

| file | kept | not ported |
|---|---|---|
| `format.c` | `parse_header`, the layout constants | `append*` |
| `math.c` | `residual_bucket`, `div_round`, `grid_to_float32_bits`, `quantized_weight`, `normalize` | `quantize`, `log2_fixed`, `grid_from_float32_bits` |
| `protocol.c` | `protocol_state`, `observe_server_data`, the coordinate and angle sizes, the `svc` and `dem` constants | `command_body_size`, `skip_entity_delta`, `frame_at`, `frame_run`, `frame_len` |
| `rans.c` | `read_models_block`, `decoder_model` (dense context array, search tables, lookup tables), `table_memory`, `stream_decoder`, `bit_reader` | tally, table costs, `encode_stream`, `bit_writer`, `write_models_block` |
| `lz.c` | `lz_expand`, `lz_bucket_context` | `lz_parse`, `lz_code` |
| `segment.c` | `segment_streams` (decoders and blobs), `parse_transform_header`, `decode_transform` | `walk_segment`, `model_builder`, `fit_models_block`, `encode_transform`, `segment_reader` |
| `walker.c` | every step, writing only | `peek*`, `payload_rest`, `payload_end`, `source_command`, `source_hidden_tag` |
| `decode.c` | header, models block, then one segment at a time, whole-demo checks (FORMAT.md 3.4) | `Reader`, `decodeBatch`, `segmentsInFlight` |

`decode.c`: the demo buffer grows by decoded segment (a lying header allocates nothing)
and is refused past 1 GiB (`MVDZ_MEMORY_LIMIT "demo size"`); decoder tables past 1 GiB
are refused (`MVDZ_MEMORY_LIMIT "models"`). Both as Rust `decode` and Go `Decode`.
No `parallel`, no `memory` module.

Estimated size: about 2200 lines of library, 500 of tests.

## C-specific rules (go into `mvdz_c/AGENTS.md`)

- No undefined behavior on any input: wrapping arithmetic in unsigned types only
  (FORMAT.md 9.1), no signed overflow, no shift by the width, no type punning; bytes
  are read and written with shifts (endian-neutral).
- Every index that comes from the file is checked before use; reads go through the
  cursor in `fields.c`, writes through the walker's `write`, which fails sticky as in Go.
- Format quantities in `uint32_t`/`uint64_t`, memory lengths in `size_t`; sums a file
  declares are compared in 64 bits (FORMAT.md 9.2). 32-bit targets must work.
- Every `malloc` checked: `MVDZ_MEMORY_LIMIT "out of memory"`. One exit path per
  function that owns memory.
- No global mutable state: two threads of a client may call the library at once.
- `qsort` only with a total order, so the result does not depend on the libc.

## Known ceilings (`ponytail:` in the code)

- CRC-32 a byte at a time from a 256-entry constant table, and the demo's CRC-32 by
  hashing the output again. Upgrade: slicing-by-8 and `crc32_combine` (FORMAT.md 9.6).
  Expected to show in the decode profile.
- Decoder tables as Go: a lookup table only for a table of more than 64 symbols.
  Upgrade: Rust also gives a model's 16 most used tables one.

## Order of work

1. Scaffold, `fields`, `math`, `streams`, `format`, `crc32`, `error`, `protocol`,
   `numeric`, `tests/sha256.c`. Check: unit tests (grid angles, `div_round`,
   `quantized_weight`, `normalize`, CRC-32 and SHA-256 known values, `parse_header`).
2. Decode: `rans`, `lz_expand`, `segment`, `walker`, `mvdz_decode`. Check: every file
   of `vectors/MANIFEST.tsv` (valid: size and SHA-256; hostile: error kind), under
   ASan + UBSan; fuzz target `decode`, corpus seeded from the vectors.
3. Real demos and `mvdz_bench`: `make test-demos` compresses each `sample/*.mvd` with
   the Rust tool, decodes it with `mvdz_bench` and compares with `cmp`.
4. Docs and CI: `mvdz_c/README.md`, `mvdz_c/AGENTS.md`, root `AGENTS.md` (project
   list, naming section covers C), root `README.md` (C section: decoder only, whole
   buffers, one thread), `ci.yml` job `c`: gcc and clang on Linux with `-Wall -Wextra
   -Wpedantic -Wconversion -Wshadow -Werror`, the tests under ASan + UBSan,
   `clang-format --dry-run -Werror`, a 60 s fuzz run; MSVC and macOS jobs build and
   run the tests.

## Not in this plan

- An encoder.
- Of the Rust library: streaming decoder, `max_threads`, `max_memory`. A 250 MB demo
  is held in memory with its file.
- A function that reads the header without decoding. `parse_header` exists
  internally; exporting it is one declaration.
- `gen_rt`, `stream_reader` and `compress_rt` fuzz targets, the synthetic-demo tests
  of `src/tests/`. The vectors are the language-neutral suite.
- Speed work. Measure `mvdz_bench` with `scripts/bench_cli.sh` first, then profile.

## Verification

```
cd mvdz_c
make test                 # unit tests + vectors, warnings as errors
make test SANITIZE=1      # ASan + UBSan
make fuzz                 # clang, libFuzzer, corpus seeded from ../vectors
MVDZ_TEST_DATA=$PWD/../testdata \
MVDZ_REFERENCE_CLI=$PWD/../mvdz_rust/target/release/mvdz make test-demos
make mvdz_bench           # -O2, for scripts/bench_cli.sh
```
