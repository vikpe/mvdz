# C

Extends the root `AGENTS.md`. C-specific additions only. The rules of
`mvdz_rust/AGENTS.md` that are not about Rust tooling apply too (visibility, constant
scope, variable lifespan, `1 << N` bit constants, `_` between words of constant names).

## Coding standards

- Before declaring a change done: `make format`, `make test CC=gcc`, `make test CC=clang`,
  `make test SANITIZE=1`. A change to decoding also runs `make fuzz` and `make test-demos`.
- C99. No VLAs, no compiler extensions, no macros beyond include guards, no dependencies
  beyond the C library. No platform code in the library.
- Unity build: `mvdz.c` includes the modules in dependency order. Every function and
  object outside `mvdz.h` is `static`; `nm mvdz.o` shows `mvdz_decode` only.
- File order: constants and types, prototypes of the file's private functions, then the
  functions, callers before callees. A function of a later module is declared where it
  is called (`walk` in `segment.c`).
- Naming: the Go name in `snake_case`; constants `UPPER_SNAKE` in an `enum` (`static
  const` when the value does not fit `int`). A function takes its struct first. The
  flat namespace prefixes the type word where the bare word is ambiguous:
  `walker_write`, `axis_track_push`, `bit_reader_is_done`.
- Errors: every fallible function returns `mvdz_error` by value; `message` is a static
  string. Results go through out parameters, written only on success.
- No global mutable state.

## Safety

- No undefined behavior on any input. Wrapping arithmetic in unsigned types only
  (FORMAT.md 9.1): no signed overflow, no shift by the width, no shift of a promoted
  `uint8_t` into the sign bit (cast to `uint32_t` first), no type punning. Bytes are read
  and written with shifts (`le_uint32`, `put_le_uint32`).
- A cast from an unsigned to a narrower or signed type wraps (two's complement) on every
  supported compiler; write it as an explicit cast.
- Every index that comes from the file is checked before use. Reads go through
  `byte_cursor`, writes through `walker_write` and `copy_blob`, which fail sticky.
- Format quantities in `uint32_t`/`uint64_t`, memory lengths in `size_t`; sums a file
  declares are compared in 64 bits (FORMAT.md 9.2). 32-bit targets must work.
- Every allocation goes through `allocate`, `calloc` or `realloc` and is checked:
  `out_of_memory()`. One exit path per function that owns memory.
- No `memcpy` with a NULL pointer, also for a length of 0.
- `qsort` only with a total order, so the result does not depend on the libc.
