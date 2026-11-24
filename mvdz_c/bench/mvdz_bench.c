/* Decodes a file with the library, file to file, for scripts/bench_cli.sh and
 * make test-demos. Usage: mvdz_bench decompress [-j N] INPUT OUTPUT (-j is ignored:
 * one thread) */
#include "../mvdz.h"

#include <stdbool.h>
#include <stdio.h>
#include <string.h>

static size_t read_file(void *context, uint8_t *buffer, size_t len);
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
    const char *input_path = arguments[first_path];
    const char *output_path = arguments[first_path + 1];

    FILE *input = fopen(input_path, "rb");
    if (input == NULL) {
        fprintf(stderr, "mvdz_bench: cannot read %s\n", input_path);
        return 1;
    }
    FILE *output = fopen(output_path, "wb");
    if (output == NULL) {
        fprintf(stderr, "mvdz_bench: cannot write %s\n", output_path);
        fclose(input);
        return 1;
    }
    mvdz_error error = mvdz_decode(read_file, input, write_segment, output, 0);
    bool is_read = ferror(input) == 0;
    fclose(input);
    bool is_closed = fclose(output) == 0;
    if (!is_read) {
        fprintf(stderr, "mvdz_bench: cannot read %s\n", input_path);
        return 1;
    }
    if (error.kind != MVDZ_OK) {
        fprintf(stderr, "mvdz_bench: %s: error %d: %s\n", input_path, (int)error.kind,
                error.message);
        return 1;
    }
    if (!is_closed) {
        fprintf(stderr, "mvdz_bench: cannot write %s\n", output_path);
        return 1;
    }
    return 0;
}

static size_t read_file(void *context, uint8_t *buffer, size_t len) {
    return fread(buffer, 1, len, context);
}

static bool write_segment(void *context, const uint8_t *segment, size_t segment_len) {
    return fwrite(segment, 1, segment_len, context) == segment_len;
}
