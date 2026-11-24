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

/* Supplies the file in order: fills buffer with len bytes and returns the count, less
 * than len only at the end of the file. */
typedef size_t (*mvdz_read)(void *context, uint8_t *buffer, size_t len);

/* Receives the demo in order, a segment of at most 1 MiB at a time. segment is valid
 * during the call. Returning false stops the decoding: the error is MVDZ_STOPPED. */
typedef bool (*mvdz_write)(void *context, const uint8_t *segment, size_t segment_len);

/* Decodes an MVDZ file from read to the demo it holds, passed to write. Neither the
 * file nor the demo is held. A segment is written after its CRC-32 is checked; the
 * whole-demo checks come after the last segment, so an error can follow segments
 * already written.
 *
 * max_memory, in bytes and approximate, is the memory to stay within: a file whose
 * decoder tables need more is refused with MVDZ_MEMORY_LIMIT. 0 is the default,
 * 64 MiB; less than 14 MiB counts as 14 MiB. At most 8 MiB of working memory is not
 * counted. */
mvdz_error mvdz_decode(mvdz_read read, void *read_context, mvdz_write write, void *write_context,
                       size_t max_memory);

#ifdef __cplusplus
}
#endif

#endif
