# mvdz_c

MVDZ decoder in C99 for QuakeWorld clients and servers. Decoder only, whole buffers, one
thread, no dependencies beyond the C library. Files are written with the Rust or Go tool.

## Use

Add the directory to the project and compile `mvdz.c`: it includes the other `.c` files
(unity build) and exports only the names of `mvdz.h`.

```c
#include "mvdz.h"

uint8_t *demo;
size_t demo_size;
mvdz_error error = mvdz_decode(&demo, &demo_size, file, file_len);
if (error.kind != MVDZ_OK) {
    printf("%s\n", error.message);
    return;
}
/* ... */
free(demo);
```

- `*demo` is NULL after an error and for an empty demo.
- Errors: `MVDZ_NOT_MVDZ`, `MVDZ_UNSUPPORTED_VERSION`, `MVDZ_CORRUPT` (`message` is the
  rule of FORMAT.md that was broken), `MVDZ_MEMORY_LIMIT` (a demo or decoder tables of
  more than 1 GiB, or `malloc` failed).
- The CRC-32 of each segment and of the demo is checked; the header's SHA-256 is not
  (FORMAT.md 10).
- No global state: calls from several threads are independent.
- Memory beyond the file and the demo, all from `malloc`: the decoder tables (refused
  past 1 GiB) and, per segment, at most 2 MiB of blob bytes and 193 KiB of walker state.

## Build and test

```
make mvdz.o               # the library: one compiler call
make test                 # unit tests + ../vectors, warnings as errors
make test SANITIZE=1      # ASan + UBSan
make fuzz                 # clang, libFuzzer, corpus seeded from ../vectors
MVDZ_TEST_DATA=$PWD/../testdata \
MVDZ_REFERENCE_CLI=$PWD/../mvdz_rust/target/release/mvdz make test-demos
make mvdz_bench           # for scripts/bench_cli.sh: MVDZ=mvdz_c/mvdz_bench
make format-check         # clang-format
```

`make test-demos` compresses each `sample/*.mvd` with the Rust tool, decodes it here
and compares. `scripts/bench_cli.sh` compresses only the files missing from WORKDIR:
fill it with the Rust tool first; the 4-thread row repeats the 1-thread one.

Without make (MSVC): `cl /W3 tests\test.c /Fe:test.exe`, then `test.exe ..\vectors`.

## Layout

One `.c` per module, the names of `mvdz_rust/mvdz/src/` and `mvdz_go/`, each holding
what decoding reaches: `error fields math streams format protocol numeric crc32 rans lz
segment walker decode`. `tests/test.c` includes `mvdz.c` to reach the statics;
`tests/sha256.c` is for the vectors' digests only.
