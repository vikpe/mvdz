/* Decodes a file with the library, a segment at a time, for scripts/bench_cli.sh and
 * make test-demos. Usage: mvdz_bench decompress [-j N] INPUT OUTPUT (-j is ignored:
 * one thread) */
#include "../mvdz.h"

#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static bool read_file(const char *path, uint8_t **data, size_t *len);
static bool write_segment(void *context, const uint8_t *segment, size_t segment_len);

int main(int argument_count, char **arguments) {
    int first_path = 2;
    bool has_threads = argument_count > 3 && strcmp(arguments[2], "-j") == 0;
    if (has_threads) {
        first_path = 4;
    }
    bool is_usage_valid =
        argument_count == first_path + 2 && strcmp(arguments[1], "decompress") == 0;
    if (!is_usage_valid) {
        fprintf(stderr, "usage: mvdz_bench decompress [-j N] INPUT OUTPUT\n");
        return 2;
    }
    const char *input = arguments[first_path];
    const char *output = arguments[first_path + 1];

    uint8_t *file;
    size_t file_len;
    if (!read_file(input, &file, &file_len)) {
        fprintf(stderr, "mvdz_bench: cannot read %s\n", input);
        return 1;
    }
    FILE *stream = fopen(output, "wb");
    if (stream == NULL) {
        fprintf(stderr, "mvdz_bench: cannot write %s\n", output);
        free(file);
        return 1;
    }
    mvdz_error error = mvdz_decode_segments(write_segment, stream, file, file_len, 0);
    free(file);
    bool is_closed = fclose(stream) == 0;
    if (error.kind != MVDZ_OK) {
        fprintf(stderr, "mvdz_bench: %s: error %d: %s\n", input, (int)error.kind, error.message);
        return 1;
    }
    if (!is_closed) {
        fprintf(stderr, "mvdz_bench: cannot write %s\n", output);
        return 1;
    }
    return 0;
}

static bool read_file(const char *path, uint8_t **data, size_t *len) {
    enum { CHUNK_LEN = 1 << 20 };
    FILE *stream = fopen(path, "rb");
    if (stream == NULL) {
        return false;
    }
    *data = NULL;
    *len = 0;
    bool is_read = false;
    for (;;) {
        uint8_t *grown = realloc(*data, *len + CHUNK_LEN);
        if (grown == NULL) {
            break;
        }
        *data = grown;
        size_t count = fread(*data + *len, 1, CHUNK_LEN, stream);
        *len += count;
        if (count < CHUNK_LEN) {
            is_read = ferror(stream) == 0;
            break;
        }
    }
    fclose(stream);
    if (!is_read) {
        free(*data);
    }
    return is_read;
}

static bool write_segment(void *context, const uint8_t *segment, size_t segment_len) {
    return fwrite(segment, 1, segment_len, context) == segment_len;
}
