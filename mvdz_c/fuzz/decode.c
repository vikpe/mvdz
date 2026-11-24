/* libFuzzer target: any input decodes or is refused, without touching memory out of
 * bounds; a second decode gives the same demo. */
#include "../mvdz.h"

#include <stdbool.h>
#include <stdlib.h>
#include <string.h>

/* A running FNV-1a of the demo. */
static const uint64_t FNV_OFFSET_BASIS = UINT64_C(0xcbf29ce484222325);

typedef struct demo_digest {
    size_t size;
    uint64_t hash;
} demo_digest;

/* The fuzz input, read in order. */
typedef struct bytes_input {
    const uint8_t *data;
    size_t len;
    size_t position;
} bytes_input;

static size_t read_bytes_from(void *context, uint8_t *buffer, size_t len) {
    bytes_input *input = context;
    size_t unread = input->len - input->position;
    size_t count = len < unread ? len : unread;
    if (count != 0) {
        memcpy(buffer, input->data + input->position, count);
    }
    input->position += count;
    return count;
}

static bool digest_segment(void *context, const uint8_t *segment, size_t segment_len) {
    demo_digest *digest = context;
    for (size_t index = 0; index < segment_len; index++) {
        digest->hash = (digest->hash ^ segment[index]) * UINT64_C(0x100000001b3);
    }
    digest->size += segment_len;
    return true;
}

int LLVMFuzzerTestOneInput(const uint8_t *file, size_t file_len) {
    enum { MAX_MEMORY = 1 << 30 };
    demo_digest first = {0, FNV_OFFSET_BASIS};
    bytes_input input = {file, file_len, 0};
    mvdz_error error = mvdz_decode(read_bytes_from, &input, digest_segment, &first, MAX_MEMORY);
    demo_digest second = {0, FNV_OFFSET_BASIS};
    bytes_input again = {file, file_len, 0};
    mvdz_error second_error =
        mvdz_decode(read_bytes_from, &again, digest_segment, &second, MAX_MEMORY);
    bool is_same =
        error.kind == second_error.kind && first.size == second.size && first.hash == second.hash;
    if (!is_same) {
        abort();
    }
    return 0;
}
