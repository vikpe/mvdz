/* Decoder for MVDZ files: losslessly compressed QuakeWorld MVD demos.
 * It follows FORMAT.md, version 3 of the format. */
#ifndef MVDZ_H
#define MVDZ_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Error categories, FORMAT.md 7. */
typedef enum mvdz_error_kind {
    MVDZ_OK,
    MVDZ_CORRUPT,
    MVDZ_MEMORY_LIMIT,
    MVDZ_NOT_MVDZ,
    MVDZ_STOPPED,
    MVDZ_UNSUPPORTED_VERSION
} mvdz_error_kind;

/* message is a static string: the rule that was broken; NULL for MVDZ_OK. */
typedef struct mvdz_error {
    mvdz_error_kind kind;
    const char *message;
} mvdz_error;

/* Decodes a whole MVDZ file to the demo it holds. *demo is freed with free(); it is
 * NULL after an error and for an empty demo.
 *
 * max_memory, in bytes and approximate, is the memory to stay within: a file whose
 * demo and decoder tables need more is refused with MVDZ_MEMORY_LIMIT. 0 is the
 * default, 64 MiB; less than 14 MiB counts as 14 MiB. About 5 MiB of working memory is
 * not counted. */
mvdz_error mvdz_decode(uint8_t **demo, size_t *demo_size, const uint8_t *file, size_t file_len,
                       size_t max_memory);

/* Receives the demo in order, a segment of at most 1 MiB at a time. segment is valid
 * during the call. Returning false stops the decoding: the error is MVDZ_STOPPED. */
typedef bool (*mvdz_write)(void *context, const uint8_t *segment, size_t segment_len);

/* Decodes a whole MVDZ file without holding the demo: write receives each segment
 * after its CRC-32 is checked. The whole-demo checks come after the last segment, so
 * an error can follow segments already written.
 *
 * max_memory is as for mvdz_decode; with no demo to hold, only the decoder tables count. */
mvdz_error mvdz_decode_segments(mvdz_write write, void *context, const uint8_t *file,
                                size_t file_len, size_t max_memory);

#ifdef __cplusplus
}
#endif

#endif
