# mvdz_c

MVDZ decoder in C99 for QuakeWorld clients and servers. Decoder only, one thread, no
dependencies beyond the C library. It reads the file and writes the demo through two
callbacks and holds neither. Files are written with the Rust or Go tool.

## API

```c
#include "mvdz.h"

/* supplies the file in order: fills buffer with len bytes and returns the count, less
   than len only at the end of the file */
typedef size_t (*mvdz_read)(void *context, uint8_t *buffer, size_t len);

/* receives the demo in order, a segment of at most 1 MiB at a time; segment is valid
   during the call; returning false stops the decoding (MVDZ_STOPPED) */
typedef bool (*mvdz_write)(void *context, const uint8_t *segment, size_t segment_len);

/* max_memory: bytes to stay within, approximate; a file whose decoder tables need
   more is refused; 0: the default, 64 MiB; less than 14 MiB counts as 14 MiB */
mvdz_error mvdz_decode(mvdz_read read, void *read_context, mvdz_write write,
                       void *write_context, size_t max_memory);

typedef struct mvdz_error {
    mvdz_error_kind kind;
    const char *message; /* static; NULL for MVDZ_OK */
} mvdz_error;

typedef enum mvdz_error_kind {
    MVDZ_OK, MVDZ_CORRUPT, MVDZ_MEMORY_LIMIT, MVDZ_NOT_MVDZ, MVDZ_STOPPED,
    MVDZ_UNSUPPORTED_VERSION
} mvdz_error_kind;
```

## Example

`hello.mvdz` to `hello.mvd`:

```c
#include "mvdz.h"

#include <stdio.h>

static size_t read_file(void *context, uint8_t *buffer, size_t len) {
    return fread(buffer, 1, len, context);
}

static bool write_segment(void *context, const uint8_t *segment, size_t segment_len) {
    return fwrite(segment, 1, segment_len, context) == segment_len;
}

int main(void) {
    FILE *input = fopen("hello.mvdz", "rb");
    FILE *output = fopen("hello.mvd", "wb");
    if (input == NULL || output == NULL) {
        return 1;
    }
    mvdz_error error = mvdz_decode(read_file, input, write_segment, output, 0);
    fclose(input);
    fclose(output);
    if (error.kind != MVDZ_OK) {
        fprintf(stderr, "hello.mvdz: %s\n", error.message);
        remove("hello.mvd"); /* segments written before the error are not a demo */
        return 1;
    }
    return 0;
}
```

Build it with the library: `cc -std=c99 -Imvdz_c hello.c mvdz_c/mvdz.c -o hello`.
`mvdz.c` includes the other `.c` files (unity build) and exports only the names of
`mvdz.h`.

In memory: a `read` that copies from a buffer, a `write` that appends to one.
`tests/test.c` has both (`read_bytes_from`, `collect_segment`).

- `write` receives a segment after its CRC-32 is checked. The whole-demo checks come
  after the last segment, so an error can follow segments already written: discard them.
- Errors: `MVDZ_NOT_MVDZ`, `MVDZ_UNSUPPORTED_VERSION`, `MVDZ_CORRUPT` (`message` is the
  rule of FORMAT.md that was broken; a read that ends early is "incomplete file"),
  `MVDZ_MEMORY_LIMIT` (the decoder tables need more than `max_memory`, or `malloc`
  failed), `MVDZ_STOPPED` (`write` returned false).
- The header's SHA-256 is not checked (FORMAT.md 10).
- No global state: calls from several threads are independent.
- Memory, all from `malloc`: the decoder tables, which `max_memory` limits, and at most
  8 MiB of working memory. A 199.5 MB demo decodes in 8.3 MiB.

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
