/* Unit tests and the vectors of ../vectors. Includes the unity file to reach the
 * statics. Usage: test [VECTORS_DIRECTORY] */
#include "../mvdz.c"

#include <stdarg.h>
#include <stdio.h>

#include "sha256.c"

enum { MAX_LINE_LEN = 1024, HEX_SHA256_SIZE = 2 * SHA256_SIZE + 1 };

typedef struct file_bytes {
    uint8_t *data;
    size_t len;
} file_bytes;

/* The input of read_bytes_from: a file in memory, read in order. */
typedef struct bytes_input {
    const uint8_t *data;
    size_t len;
    size_t position;
} bytes_input;

static int failure_count;

static void test_crc32_check_value(void);
static void test_sha256(void);
static void test_residual_bucket(void);
static void test_quantized_weight(void);
static void test_normalize(void);
static void test_div_round(void);
static void test_grid_angles(void);
static void test_header(const char *vectors_directory);
static void test_not_mvdz(void);
static void test_vectors(const char *vectors_directory);
static void test_vector(const char *vectors_directory, char *line);
static void test_decode_memory_limit(const char *vectors_directory);
static void test_decode_stops(const char *vectors_directory);
static mvdz_error decode_whole(uint8_t **demo, size_t *demo_size, const uint8_t *file,
                               size_t file_len);
static size_t read_bytes_from(void *context, uint8_t *buffer, size_t len);
static bool collect_segment(void *context, const uint8_t *segment, size_t segment_len);
static bool refuse_segment(void *context, const uint8_t *segment, size_t segment_len);
static void expect(bool is_passing, const char *format, ...);
static file_bytes read_file(const char *directory, const char *name);
static void hex_sha256(const uint8_t *data, size_t len, char *hex);
static const char *kind_prefix(mvdz_error_kind kind);

int main(int argument_count, char **arguments) {
    const char *vectors_directory = argument_count > 1 ? arguments[1] : "../vectors";
    test_crc32_check_value();
    test_sha256();
    test_residual_bucket();
    test_quantized_weight();
    test_normalize();
    test_div_round();
    test_grid_angles();
    test_header(vectors_directory);
    test_not_mvdz();
    test_vectors(vectors_directory);
    test_decode_memory_limit(vectors_directory);
    test_decode_stops(vectors_directory);
    if (failure_count != 0) {
        printf("FAILED: %d\n", failure_count);
        return 1;
    }
    printf("ok\n");
    return 0;
}

/* FORMAT.md 1 */
static void test_crc32_check_value(void) {
    const uint8_t digits[] = "123456789";
    uint32_t whole = crc32_update(0, digits, 9);
    uint32_t in_parts = crc32_update(crc32_update(0, digits, 4), digits + 4, 5);
    expect(whole == 0xCBF43926, "crc32: got %#x", (unsigned)whole);
    expect(in_parts == whole, "crc32 in parts: got %#x", (unsigned)in_parts);
    for (uint32_t first_len = 0; first_len <= 9; first_len++) {
        uint32_t combined =
            crc32_combine(crc32_update(0, digits, first_len),
                          crc32_update(0, digits + first_len, 9 - first_len), 9 - first_len);
        expect(combined == whole, "crc32_combine at %u: got %#x", (unsigned)first_len,
               (unsigned)combined);
    }
}

static void test_sha256(void) {
    /* FIPS 180-4 examples: one block, two blocks; then a rest that fills its block */
    static const struct {
        const char *message;
        const char *expected;
    } cases[] = {
        {"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"},
        {"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"},
        {"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
         "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"},
        {"0123456789012345678901234567890123456789012345678901234567890123",
         "9674d9e078535b7cec43284387a6ee39956188e735a85452b0050b55341cda56"},
    };
    for (size_t index = 0; index < sizeof cases / sizeof cases[0]; index++) {
        char hex[HEX_SHA256_SIZE];
        hex_sha256((const uint8_t *)cases[index].message, strlen(cases[index].message), hex);
        expect(strcmp(hex, cases[index].expected) == 0, "sha256(%s): got %s", cases[index].message,
               hex);
    }
}

static void test_residual_bucket(void) {
    static const struct {
        int32_t value;
        uint32_t expected;
    } cases[] = {
        {0, 0}, {1, 1},     {-1, 2},   {2, 3},          {3, 3},           {-3, 4},
        {4, 5}, {-128, 16}, {127, 13}, {INT32_MAX, 61}, {-INT32_MAX, 62}, {INT32_MIN, 64},
    };
    for (size_t index = 0; index < sizeof cases / sizeof cases[0]; index++) {
        uint32_t bucket = residual_bucket(cases[index].value);
        expect(bucket == cases[index].expected, "residual_bucket(%d) = %u, want %u",
               (int)cases[index].value, (unsigned)bucket, (unsigned)cases[index].expected);
    }
}

static void test_quantized_weight(void) {
    static const struct {
        uint32_t quantized;
        uint64_t expected;
    } cases[] = {
        {0, 0},
        {1, 65536},
        {2, 92682},
        {3, 131072},
        {4, 185364},
        {64, UINT64_C(92682) << 31},
        {MAX_QUANTIZED, UINT64_C(1) << 48},
    };
    for (size_t index = 0; index < sizeof cases / sizeof cases[0]; index++) {
        uint64_t weight = quantized_weight(cases[index].quantized);
        expect(weight == cases[index].expected, "quantized_weight(%u) = %llu",
               (unsigned)cases[index].quantized, (unsigned long long)weight);
    }
}

static void test_normalize(void) {
    static const struct {
        const char *name;
        uint32_t count;
        uint64_t weights[4];
        int32_t expected[4];
    } cases[] = {
        {"absent", 3, {0, 0, 0}, {0, 0, 0}},
        {"single", 3, {0, 7, 0}, {0, 4096, 0}},
        {"exact", 3, {1, 1, 2}, {1024, 1024, 2048}},
        {"remainder to the first largest", 3, {1, 1, 1}, {1366, 1365, 1365}},
        {"largest pays for the floors of 1", 4, {UINT64_C(1) << 40, 1, 1, 0}, {4094, 1, 1, 0}},
        {"largest weights", 2, {UINT64_C(1) << 48, UINT64_C(1) << 48}, {2048, 2048}},
    };
    for (size_t index = 0; index < sizeof cases / sizeof cases[0]; index++) {
        int32_t frequencies[4];
        normalize(cases[index].weights, cases[index].count, FREQUENCY_TOTAL, frequencies);
        bool is_equal = memcmp(frequencies, cases[index].expected,
                               cases[index].count * sizeof frequencies[0]) == 0;
        expect(is_equal, "normalize: %s", cases[index].name);
    }

    /* 256 present symbols with one dominant: the floors of 1 take the sum over 4096. */
    uint64_t weights[STRUCTURAL_ALPHABET_SIZE];
    for (uint32_t symbol = 0; symbol < STRUCTURAL_ALPHABET_SIZE; symbol++) {
        weights[symbol] = 1;
    }
    weights[9] = UINT64_C(1) << 50;
    int32_t frequencies[STRUCTURAL_ALPHABET_SIZE];
    normalize(weights, STRUCTURAL_ALPHABET_SIZE, FREQUENCY_TOTAL, frequencies);
    expect(frequencies[9] == 4096 - 255 && frequencies[0] == 1,
           "normalize: dominant: got %d and %d", (int)frequencies[9], (int)frequencies[0]);
}

static void test_div_round(void) {
    static const struct {
        int32_t dividend;
        int32_t divisor;
        int32_t expected;
    } cases[] = {
        {7, 2, 4},
        {-7, 2, -4},
        {5, 3, 2},
        {-5, 3, -2},
        {4, 3, 1},
        {0, 5, 0},
        {-(1 << 30), 32767, -32769},
    };
    for (size_t index = 0; index < sizeof cases / sizeof cases[0]; index++) {
        int32_t quotient = div_round(cases[index].dividend, cases[index].divisor);
        expect(quotient == cases[index].expected, "div_round(%d, %d) = %d, want %d",
               (int)cases[index].dividend, (int)cases[index].divisor, (int)quotient,
               (int)cases[index].expected);
    }
}

static void test_grid_angles(void) {
    /* 0.0, 45/8192, 91 * 45/8192, 45.0, 90.0, 32767 * 45/8192, -180.0, -45/8192 */
    static const struct {
        uint16_t grid;
        uint32_t expected;
    } cases[] = {
        {0, 0},
        {1, 0x3bb40000},
        {91, 0x3efff000},
        {8192, 0x42340000},
        {16384, 0x42b40000},
        {32767, 0x4333fe98},
        {0x8000, 0xc3340000},
        {0xffff, 0xbbb40000},
    };
    for (size_t index = 0; index < sizeof cases / sizeof cases[0]; index++) {
        uint32_t angle_bits = grid_to_float32_bits(cases[index].grid);
        expect(angle_bits == cases[index].expected, "grid_to_float32_bits(%u) = %#x, want %#x",
               (unsigned)cases[index].grid, (unsigned)angle_bits, (unsigned)cases[index].expected);
    }
}

static void test_header(const char *vectors_directory) {
    file_bytes file = read_file(vectors_directory, "float-mvd1.normal.mvdz");
    mvdz_header header;
    mvdz_error error = parse_header(file.data, file.len, &header);
    expect(error.kind == MVDZ_OK && header.size == 8435, "header: kind %d, size %llu",
           (int)error.kind, (unsigned long long)header.size);
    /* the header's SHA-256 is of the demo */
    uint8_t *demo;
    size_t demo_size;
    error = decode_whole(&demo, &demo_size, file.data, file.len);
    uint8_t digest[SHA256_SIZE];
    sha256(demo, demo_size, digest);
    expect(error.kind == MVDZ_OK && memcmp(digest, header.sha256, SHA256_SIZE) == 0,
           "header: SHA-256");
    free(demo);
    free(file.data);
}

static void test_not_mvdz(void) {
    static const struct {
        const char *file;
        size_t len;
    } cases[] = {{"", 0}, {"MVD", 3}, {"MVDX\x02\x00\x00\x00", 8}, {"\x1f\x8b\x08\x00", 4}};
    for (size_t index = 0; index < sizeof cases / sizeof cases[0]; index++) {
        uint8_t *demo;
        size_t demo_size;
        mvdz_error error =
            decode_whole(&demo, &demo_size, (const uint8_t *)cases[index].file, cases[index].len);
        expect(error.kind == MVDZ_NOT_MVDZ && demo == NULL, "not MVDZ, case %d: kind %d",
               (int)index, (int)error.kind);
    }
}

/* Valid files must decode to the SHA-256 of the manifest; hostile files must be
 * refused with its message. */
static void test_vectors(const char *vectors_directory) {
    file_bytes manifest = read_file(vectors_directory, "MANIFEST.tsv");
    int vector_count = 0;
    size_t line_start = 0;
    for (size_t position = 0; position < manifest.len; position++) {
        if (manifest.data[position] != '\n') {
            continue;
        }
        size_t line_len = position - line_start;
        char line[MAX_LINE_LEN];
        bool is_vector = line_len != 0 && manifest.data[line_start] != '#';
        if (is_vector && line_len < MAX_LINE_LEN) {
            memcpy(line, manifest.data + line_start, line_len);
            line[line_len] = '\0';
            test_vector(vectors_directory, line);
            vector_count++;
        }
        line_start = position + 1;
    }
    expect(vector_count >= 60, "vectors: only %d in the manifest", vector_count);
    free(manifest.data);
}

/* line: file, decoded_sha256 or "error: <message>", decoded_size, how it was made. */
static void test_vector(const char *vectors_directory, char *line) {
    char *name = strtok(line, "\t");
    char *outcome = strtok(NULL, "\t");
    char *size_column = strtok(NULL, "\t");
    if (size_column == NULL) {
        expect(false, "manifest line of %s", name != NULL ? name : "?");
        return;
    }
    file_bytes file = read_file(vectors_directory, name);
    uint8_t *demo;
    size_t demo_size;
    mvdz_error error = decode_whole(&demo, &demo_size, file.data, file.len);

    static const char error_prefix[] = "error: ";
    bool is_hostile = strncmp(outcome, error_prefix, strlen(error_prefix)) == 0;
    if (is_hostile) {
        char message[MAX_LINE_LEN] = "ok";
        if (error.kind != MVDZ_OK) {
            snprintf(message, sizeof message, "%s%s", kind_prefix(error.kind), error.message);
        }
        expect(strcmp(message, outcome + strlen(error_prefix)) == 0 && demo == NULL,
               "%s: got %s, want %s", name, message, outcome);
    } else if (error.kind != MVDZ_OK) {
        expect(false, "%s: %s%s", name, kind_prefix(error.kind), error.message);
    } else {
        char hex[HEX_SHA256_SIZE];
        hex_sha256(demo, demo_size, hex);
        bool is_equal = demo_size == strtoul(size_column, NULL, 10) && strcmp(hex, outcome) == 0;
        expect(is_equal, "%s: got %lu bytes with SHA-256 %s, want %s bytes with %s", name,
               (unsigned long)demo_size, hex, size_column, outcome);
    }
    free(demo);
    free(file.data);
}

/* Decoder tables over the limit are refused; the least limit decodes an ordinary file
 * whatever is asked for. */
static void test_decode_memory_limit(const char *vectors_directory) {
    file_bytes file = read_file(vectors_directory, "float-mvd1.normal.mvdz");
    file_bytes collected = {NULL, 0};
    bytes_input input = {file.data, file.len, 0};
    mvdz_error error = mvdz_decode(read_bytes_from, &input, collect_segment, &collected, 1);
    expect(error.kind == MVDZ_OK && collected.len == 8435, "a limit of 1 byte: kind %d",
           (int)error.kind);
    free(collected.data);

    int call_count = 0;
    table_memory memory = {0, 1000};
    bytes_input again = {file.data, file.len, 0};
    file_reader reader = {read_bytes_from, &again};
    error = decode_file(reader, refuse_segment, &call_count, &memory);
    expect(error.kind == MVDZ_MEMORY_LIMIT && call_count == 0, "tables within 1000 bytes: kind %d",
           (int)error.kind);
    free(file.data);
}

static void test_decode_stops(const char *vectors_directory) {
    file_bytes file = read_file(vectors_directory, "duel.normal.mvdz");
    int call_count = 0;
    bytes_input input = {file.data, file.len, 0};
    mvdz_error error = mvdz_decode(read_bytes_from, &input, refuse_segment, &call_count, 0);
    expect(error.kind == MVDZ_STOPPED && call_count == 1, "stop: kind %d after %d calls",
           (int)error.kind, call_count);
    free(file.data);
}

/* The demo as one buffer, freed with free(); NULL after an error. */
static mvdz_error decode_whole(uint8_t **demo, size_t *demo_size, const uint8_t *file,
                               size_t file_len) {
    file_bytes collected = {NULL, 0};
    bytes_input input = {file, file_len, 0};
    mvdz_error error = mvdz_decode(read_bytes_from, &input, collect_segment, &collected, 0);
    if (error.kind != MVDZ_OK) {
        free(collected.data);
        collected.data = NULL;
        collected.len = 0;
    }
    *demo = collected.data;
    *demo_size = collected.len;
    return error;
}

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

static bool collect_segment(void *context, const uint8_t *segment, size_t segment_len) {
    file_bytes *collected = context;
    uint8_t *data = realloc(collected->data, collected->len + segment_len);
    if (data == NULL) {
        return false;
    }
    memcpy(data + collected->len, segment, segment_len);
    collected->data = data;
    collected->len += segment_len;
    return true;
}

static bool refuse_segment(void *context, const uint8_t *segment, size_t segment_len) {
    int *call_count = context;
    (*call_count)++;
    (void)segment;
    (void)segment_len;
    return false;
}

static void expect(bool is_passing, const char *format, ...) {
    if (is_passing) {
        return;
    }
    failure_count++;
    va_list arguments;
    va_start(arguments, format);
    vprintf(format, arguments);
    va_end(arguments);
    printf("\n");
}

/* Exits if the file cannot be read. */
static file_bytes read_file(const char *directory, const char *name) {
    enum { CHUNK_LEN = 1 << 16 };
    char path[MAX_LINE_LEN];
    snprintf(path, sizeof path, "%s/%s", directory, name);
    FILE *stream = fopen(path, "rb");
    if (stream == NULL) {
        printf("cannot open %s\n", path);
        exit(1);
    }
    file_bytes file = {NULL, 0};
    for (;;) {
        uint8_t *data = realloc(file.data, file.len + CHUNK_LEN);
        if (data == NULL) {
            printf("out of memory\n");
            exit(1);
        }
        file.data = data;
        size_t count = fread(file.data + file.len, 1, CHUNK_LEN, stream);
        file.len += count;
        if (count < CHUNK_LEN) {
            break;
        }
    }
    fclose(stream);
    return file;
}

static void hex_sha256(const uint8_t *data, size_t len, char *hex) {
    uint8_t digest[SHA256_SIZE];
    sha256(data, len, digest);
    for (uint32_t index = 0; index < SHA256_SIZE; index++) {
        snprintf(hex + 2 * index, 3, "%02x", (unsigned)digest[index]);
    }
}

static const char *kind_prefix(mvdz_error_kind kind) {
    switch (kind) {
    case MVDZ_CORRUPT:
        return "corrupt: ";
    case MVDZ_MEMORY_LIMIT:
        return "memory limit: ";
    default:
        return "";
    }
}
