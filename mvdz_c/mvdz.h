/* Decoder for MVDZ files: losslessly compressed QuakeWorld MVD demos.
 * It follows FORMAT.md, version 3 of the format. */
#ifndef MVDZ_H
#define MVDZ_H

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
    MVDZ_UNSUPPORTED_VERSION
} mvdz_error_kind;

/* message is a static string: the rule that was broken; NULL for MVDZ_OK. */
typedef struct mvdz_error {
    mvdz_error_kind kind;
    const char *message;
} mvdz_error;

/* Decodes a whole MVDZ file to the demo it holds. *demo is freed with free(); it is
 * NULL after an error and for an empty demo. A demo of more than 1 GiB is refused
 * with MVDZ_MEMORY_LIMIT. */
mvdz_error mvdz_decode(uint8_t **demo, size_t *demo_size, const uint8_t *file, size_t file_len);

#ifdef __cplusplus
}
#endif

#endif
