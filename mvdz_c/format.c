enum {
    FORMAT_VERSION = 3,
    HEADER_SIZE = 60,
    SHA256_SIZE = 32,

    MAX_RAW_LEN = 1 << 20,
    MAX_CODED_LEN = 1 << 21,
    MAX_MODELS_LEN = 1 << 22,
    /* Of the models before they are coded, the meta model included. */
    MAX_MODELS_RAW_LEN = 1 << 22
};

/* mvdz_header describes the original demo. */
typedef struct mvdz_header {
    uint64_t size;
    uint64_t duration_ms;
    uint32_t crc32;
    uint8_t sha256[SHA256_SIZE]; /* a claim of the encoder: not checked by decoding */
} mvdz_header;

/* FORMAT.md 2.1 */
static mvdz_error parse_header(const uint8_t *block, size_t block_len, mvdz_header *header) {
    static const char magic[4] = {'M', 'V', 'D', 'Z'};
    if (block_len < sizeof magic || memcmp(block, magic, sizeof magic) != 0) {
        mvdz_error error = {MVDZ_NOT_MVDZ, "not an MVDZ file"};
        return error;
    }
    if (block_len >= 8 && le_uint32(block + 4) != FORMAT_VERSION) {
        mvdz_error error = {MVDZ_UNSUPPORTED_VERSION, "unsupported MVDZ version"};
        return error;
    }
    if (block_len < HEADER_SIZE) {
        return corrupt("truncated header");
    }
    header->size = le_uint64(block + 8);
    header->duration_ms = le_uint64(block + 16);
    header->crc32 = le_uint32(block + 24);
    memcpy(header->sha256, block + 28, SHA256_SIZE);
    return ok();
}
