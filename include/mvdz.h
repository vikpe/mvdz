/*
 * mvdz.h - C interface to mvdz, a lossless compressor for QuakeWorld MVD
 * demos (file format: FORMAT.md). Link against libmvdz.a (static; also
 * needs -lpthread -ldl -lm on Linux) or libmvdz.so.
 *
 * Conventions
 *  - Functions return MVDZ_OK (0) or a negative MVDZ_E_* code, unless
 *    noted.
 *  - Buffers the library returns are owned by the caller and freed with
 *    mvdz_free(ptr, len). Only mvdz_free may free them (not free()). An
 *    empty result is returned as NULL with length 0; mvdz_free(NULL, 0) is
 *    a no-op. The strings of mvdz_version and mvdz_strerror are static:
 *    never free them.
 *  - No function panics or unwinds into C; internal errors become
 *    MVDZ_E_PANIC.
 *  - Handles (mvdz_reader) are opaque ids, not pointers. A
 *    closed or unknown handle makes a call fail with
 *    MVDZ_E_INVALID; closing twice is harmless. Calls on one handle are
 *    serialised internally, so a handle may be passed between threads
 *    (calls from several threads at once wait for each other). Different
 *    handles work independently, from any threads, and the functions that
 *    take no handle may be called from several threads at once. A handle
 *    whose call hit an internal error (MVDZ_E_PANIC) fails every later
 *    call with MVDZ_E_PANIC; close it with mvdz_close.
 *  - Callbacks are called only from the thread that is inside an mvdz_*
 *    call, and only during that call. They must return normally: they must
 *    not longjmp (as Quake's Sys_Error / Host_Error do) or throw a C++
 *    exception out of the callback; report errors through the return
 *    value instead. Unwinding through the library is undefined behaviour.
 *    A callback must not call the library on its own handle: such a call
 *    (mvdz_read, mvdz_raw_size, ...) fails with MVDZ_E_INVALID and
 *    mvdz_close does nothing; return an error from the callback and close
 *    the handle after the outer call has returned.
 *  - `threads` arguments are worker threads (1 = none).
 *  - `max_memory` arguments cap the memory one call or reader may hold, in
 *    bytes (0 = the default, 1 GiB on 64-bit targets). Exceeding it gives
 *    MVDZ_E_LIMIT.
 *  - `flags` arguments of the decoding calls: 0, or MVDZ_TRUSTED. A bit
 *    this version does not define gives MVDZ_E_INVALID (so a later flag is
 *    never silently ignored by an older library).
 *  - Errors are sticky: after a reader call fails, every later call on
 *    that handle returns the same error.
 *  - Shared library: a source build makes libmvdz.so without a SONAME;
 *    packagers who want one can build with RUSTFLAGS="-C
 *    link-arg=-Wl,-soname,libmvdz.so.1". Release archives carry the static
 *    library only.
 *
 * MIT licence.
 */
#ifndef MVDZ_H
#define MVDZ_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MVDZ_OK          0
#define MVDZ_E_FORMAT   -1  /* not an MVDZ file, or unsupported version */
#define MVDZ_E_CORRUPT  -2  /* damaged or truncated file */
#define MVDZ_E_INVALID  -3  /* bad argument or handle, or operation not possible */
#define MVDZ_E_IO       -4  /* a callback or file operation failed */
#define MVDZ_E_PANIC    -6  /* internal error */
#define MVDZ_E_LIMIT    -7  /* memory or work limit exceeded */
#define MVDZ_E_NOTMVD   -8  /* input to compress is not an MVD demo, or corrupt gzip */

/* Decoding flags.
 * MVDZ_TRUSTED: the input is trusted (a cache that checked the demo's
 * SHA-256 once, at ingest): skip the SHA-256 identity check of a full
 * decode. The CRC-32C of every segment and of the whole demo are still
 * checked, so damage is still detected, but a file built to carry another
 * demo's identity is not. Without it (the default) a full decode also
 * verifies the SHA-256: use the default for files from anywhere else. */
#define MVDZ_TRUSTED 1u

/* Compression levels. */
#define MVDZ_LEVEL_NORMAL 0  /* static rANS (default) */
#define MVDZ_LEVEL_FAST   1  /* per-stream zstd: larger, faster to compress and decode */

/* Callbacks for reading through the engine's own VFS.
 * read: fill buf with up to n bytes; return the count, 0 at end of file,
 *       negative on error.
 * seek: move to offset relative to whence (0 SET, 1 CUR, 2 END); return
 *       the new absolute position, negative on error. Offsets are 64-bit. */
typedef long long (*mvdz_read_fn)(void *user, void *buf, size_t n);
typedef long long (*mvdz_seek_fn)(void *user, long long offset, int whence);

/* Opaque handle type. Never dereferenced (handles are ids). */
typedef struct mvdz_reader mvdz_reader;

/* The library version, e.g. "1.0.0-rc.4"; a static string, do not free. */
const char *mvdz_version(void);
/* A message for an MVDZ_* code; a static string, do not free. */
const char *mvdz_strerror(int code);
void mvdz_free(void *ptr, size_t len);

/* 1 if buf (at least 5 bytes) starts like an MVDZ file this library reads. */
int mvdz_is_mvdz(const void *buf, size_t n);

/* Whole-buffer operations. mvdz_compress takes a raw .mvd or a
 * gzip-compressed one (.mvd.gz, decompressed first, every member); the
 * file holds the raw demo (its identity is the raw demo's SHA-256, and
 * decoding gives the raw .mvd). Input that is not an MVD demo (empty, or
 * no whole first frame that parses; see check_demo in the Rust API), and
 * corrupt gzip, give MVDZ_E_NOTMVD. level: MVDZ_LEVEL_*. segment: raw bytes per
 * independently decodable segment, 0 = one segment (the default). A file holds at most 65536
 * segments: a smaller segment size than that allows is MVDZ_E_INVALID. */
int mvdz_decompress(const void *in, size_t n, int threads, uint64_t max_memory, unsigned flags,
                    unsigned char **out, size_t *out_n);
int mvdz_compress(const void *in, size_t n, int level, size_t segment, int threads,
                  unsigned char **out, size_t *out_n);

/* Incremental decoding. mvdz_open_memory copies the buffer.
 * mvdz_open_callbacks: with seek == NULL the file is read strictly
 * sequentially (pipes, network streams) and the seek functions fail with
 * MVDZ_E_INVALID; with seek, the file must be complete: its whole section
 * structure is checked when it is opened. *err (may be NULL) receives the
 * error code on failure. */
mvdz_reader *mvdz_open_memory(const void *buf, size_t n, int threads, uint64_t max_memory, unsigned flags,
                              int *err);
mvdz_reader *mvdz_open_callbacks(mvdz_read_fn read, mvdz_seek_fn seek, void *user, int threads,
                                 uint64_t max_memory, unsigned flags, int *err);
/* Reads up to n bytes of the original demo: returns the count, 0 at the
 * end, or a negative MVDZ_E_* code. Every segment's CRC-32C is checked
 * before its bytes are returned; reading from the start, the end is
 * reported only after the whole-demo checks (CRC-32C, size, timing and,
 * without MVDZ_TRUSTED, the SHA-256 identity) have passed. Reading after a
 * seek checks each segment only. */
long long mvdz_read(mvdz_reader *r, void *out, size_t n);
/* Size of the original demo in bytes, -1 if not known (yet). */
long long mvdz_raw_size(const mvdz_reader *r);
/* Number of segments (seek points), negative if the reader cannot seek. */
long long mvdz_segment_count(const mvdz_reader *r);
/* Seek to the segment that holds demo time ms (milliseconds from the
 * start): the last segment that starts before ms, so every frame at ms or
 * later is decoded. Reading resumes at that segment's first byte, which is
 * a demo frame boundary; *seg_start_ms and *raw_offset (may be NULL)
 * receive its start time and byte offset in the original demo. A player
 * still needs the game state at that point (the serverdata and baselines
 * are at the start of the demo). */
int mvdz_seek_time(mvdz_reader *r, uint64_t ms, uint64_t *seg_start_ms, uint64_t *raw_offset);
/* Seek to segment `segment` (0 <= segment < mvdz_segment_count). */
int mvdz_seek_segment(mvdz_reader *r, size_t segment, uint64_t *raw_offset);
void mvdz_close(mvdz_reader *r);

/* Metadata (identity, server info, roster, census, index) as a
 * NUL-terminated JSON string (FORMAT.md §3.2), read without decoding the
 * demo; the section structure is checked. The identity is what the file
 * claims: only a full decode verifies it. *json_n excludes the NUL; free
 * with mvdz_free(json, *json_n). Byte strings are rendered as Latin-1
 * (each byte is the code point of the same value). The JSON counts
 * against max_memory. */
int mvdz_info_json(mvdz_read_fn read, mvdz_seek_fn seek, void *user, uint64_t max_memory, char **json,
                   size_t *json_n);
int mvdz_info_json_memory(const void *buf, size_t n, uint64_t max_memory, char **json, size_t *json_n);

#ifdef __cplusplus
}
#endif
#endif
