/* libFuzzer target: any input decodes or is refused, without touching memory out of
 * bounds; a decoded demo is the same on a second decode. */
#include "../mvdz.h"

#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

int LLVMFuzzerTestOneInput(const uint8_t *file, size_t file_len) {
    uint8_t *demo;
    size_t demo_size;
    mvdz_error error = mvdz_decode(file, file_len, &demo, &demo_size);
    uint8_t *again;
    size_t again_size;
    mvdz_error again_error = mvdz_decode(file, file_len, &again, &again_size);
    bool is_same = error.kind == again_error.kind && demo_size == again_size &&
                   (demo_size == 0 || memcmp(demo, again, demo_size) == 0);
    if (!is_same) {
        abort();
    }
    free(demo);
    free(again);
    return 0;
}
