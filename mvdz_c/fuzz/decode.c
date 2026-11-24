/* libFuzzer target: any input decodes or is refused, without touching memory out of
 * bounds; a decoded demo is the same on a second decode and a segment at a time. */
#include "../mvdz.h"

#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

static bool count_segment(void *context, const uint8_t *segment, size_t segment_len) {
    size_t *total = context;
    (void)segment;
    *total += segment_len;
    return true;
}

int LLVMFuzzerTestOneInput(const uint8_t *file, size_t file_len) {
    enum { MAX_MEMORY = 1 << 30 };
    uint8_t *demo;
    size_t demo_size;
    mvdz_error error = mvdz_decode(&demo, &demo_size, file, file_len, MAX_MEMORY);
    uint8_t *again;
    size_t again_size;
    mvdz_error again_error = mvdz_decode(&again, &again_size, file, file_len, MAX_MEMORY);
    bool is_same = error.kind == again_error.kind && demo_size == again_size &&
                   (demo_size == 0 || memcmp(demo, again, demo_size) == 0);
    size_t segments_size = 0;
    mvdz_error segments_error =
        mvdz_decode_segments(count_segment, &segments_size, file, file_len, MAX_MEMORY);
    bool is_same_in_segments =
        segments_error.kind == error.kind && (error.kind != MVDZ_OK || segments_size == demo_size);
    if (!is_same || !is_same_in_segments) {
        abort();
    }
    free(demo);
    free(again);
    return 0;
}
