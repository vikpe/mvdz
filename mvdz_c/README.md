# mvdz_c

MVDZ decoder in C99 for QuakeWorld clients and servers. Decoder only, one thread, no
dependencies beyond the C library. The file is given as one buffer; the demo comes back
as one buffer or a segment at a time. Files are written with the Rust or Go tool.

## API

```c
#include "mvdz.h"

/* *demo is freed with free(); it is NULL after an error and for an empty demo.
   max_memory: bytes to stay within, approximate; a file that needs more is refused;
   0: the default, 64 MiB; less than 14 MiB counts as 14 MiB */
mvdz_error mvdz_decode(uint8_t **demo, size_t *demo_size, const uint8_t *file, size_t file_len,
                       size_t max_memory);

/* receives the demo in order, a segment of at most 1 MiB at a time; segment is valid
   during the call; returning false stops the decoding (MVDZ_STOPPED) */
typedef bool (*mvdz_write)(void *context, const uint8_t *segment, size_t segment_len);

/* decodes without holding the demo: the way to decode a large demo in little memory */
mvdz_error mvdz_decode_segments(mvdz_write write, void *context, const uint8_t *file,
                                size_t file_len, size_t max_memory);

typedef struct mvdz_error {
    mvdz_error_kind kind;
    const char *message; /* static; NULL for MVDZ_OK */
} mvdz_error;

typedef enum mvdz_error_kind {
    MVDZ_OK, MVDZ_CORRUPT, MVDZ_MEMORY_LIMIT, MVDZ_NOT_MVDZ, MVDZ_STOPPED,
    MVDZ_UNSUPPORTED_VERSION
} mvdz_error_kind;
```

## Use

Add the directory to the project and compile `mvdz.c`: it includes the other `.c` files
(unity build) and exports only the names of `mvdz.h`.

```c
#include "mvdz.h"

uint8_t *demo;
size_t demo_size;
mvdz_error error = mvdz_decode(&demo, &demo_size, file, file_len, 0);
if (error.kind != MVDZ_OK) {
    printf("%s\n", error.message);
    return;
}
/* ... */
free(demo);
```

A segment at a time, here to a file:

```c
static bool write_segment(void *context, const uint8_t *segment, size_t segment_len) {
    return fwrite(segment, 1, segment_len, context) == segment_len;
}

mvdz_error error = mvdz_decode_segments(write_segment, stream, file, file_len, 0);
```

`write` receives a segment after its CRC-32 is checked. The whole-demo checks come after
the last segment, so an error can follow segments already written.

- `*demo` is NULL after an error and for an empty demo.
- Errors: `MVDZ_NOT_MVDZ`, `MVDZ_UNSUPPORTED_VERSION`, `MVDZ_CORRUPT` (`message` is the
  rule of FORMAT.md that was broken), `MVDZ_MEMORY_LIMIT` (the demo and the decoder
  tables together need more than `max_memory`, or `malloc` failed), `MVDZ_STOPPED`
  (`write` returned false).
- The CRC-32 of each segment and of the demo is checked; the header's SHA-256 is not
  (FORMAT.md 10).
- No global state: calls from several threads are independent.
- Memory beyond the file, all from `malloc`: the decoder tables, the demo for
  `mvdz_decode`, and about 5 MiB of working memory. `max_memory` is approximate, as in
  Rust and Go: it counts the tables and the demo, not the working memory. With the
  default of 64 MiB, `mvdz_decode` refuses a demo of about 60 MB or more: pass a larger
  limit or use `mvdz_decode_segments`. With
  `mvdz_decode_segments` a 199.5 MB demo decodes in 9 MiB beside its 33 MiB file.

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
