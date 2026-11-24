enum {
    DEFAULT_MAX_MEMORY = 64 << 20,
    /* What a demo's decoder tables take with room to spare: no lower limit applies. */
    LEAST_MAX_MEMORY = 14 << 20,
    LEN_FIELD_SIZE = 4,
    /* raw_len, duration_ms and crc32, after the coded length. */
    SEGMENT_FIELDS_SIZE = 4 + 4 + 4
};

/* file_reader is the input of a decode. */
typedef struct file_reader {
    mvdz_read read;
    void *context;
} file_reader;

static size_t effective_max_memory(size_t max_memory);
static mvdz_error decode_file(file_reader reader, mvdz_write write, void *write_context,
                              table_memory *memory);
static mvdz_error read_models(file_reader reader, table_memory *memory, decoder_model *models);
static mvdz_error decode_segments(file_reader reader, mvdz_write write, void *write_context,
                                  const mvdz_header *header, const decoder_model *models,
                                  uint8_t *buffers);
static bool read_exact(file_reader reader, uint8_t *buffer, size_t len);
static mvdz_error decode_segment(uint8_t *destination, uint32_t raw_len, const uint8_t *coded,
                                 uint32_t coded_len, const decoder_model *models);

mvdz_error mvdz_decode(mvdz_read read, void *read_context, mvdz_write write, void *write_context,
                       size_t max_memory) {
    file_reader reader = {read, read_context};
    table_memory memory = {0, effective_max_memory(max_memory)};
    return decode_file(reader, write, write_context, &memory);
}

/* 0 is the default; less than the least counts as the least. */
static size_t effective_max_memory(size_t max_memory) {
    if (max_memory == 0) {
        return DEFAULT_MAX_MEMORY;
    }
    return max_memory < LEAST_MAX_MEMORY ? LEAST_MAX_MEMORY : max_memory;
}

/* decode_file reads the file in order and passes the demo to write a segment at a
 * time; memory counts the decoder tables. */
static mvdz_error decode_file(file_reader reader, mvdz_write write, void *write_context,
                              table_memory *memory) {
    uint8_t header_block[HEADER_SIZE];
    size_t header_len = reader.read(reader.context, header_block, HEADER_SIZE);
    mvdz_header header;
    mvdz_error error = parse_header(header_block, header_len, &header);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    decoder_model models[FIRST_BLOB_STREAM] = {{false, NULL, 0, 0, NULL, NULL}};
    error = read_models(reader, memory, models);
    if (error.kind == MVDZ_OK) {
        /* a coded segment with its fields, then the segment it decodes to */
        uint8_t *buffers = allocate(SEGMENT_FIELDS_SIZE + MAX_CODED_LEN + MAX_RAW_LEN);
        error = buffers != NULL
                    ? decode_segments(reader, write, write_context, &header, models, buffers)
                    : out_of_memory();
        free(buffers);
    }
    free_models(models);
    return error;
}

/* FORMAT.md 2.2 */
static mvdz_error read_models(file_reader reader, table_memory *memory, decoder_model *models) {
    uint8_t len_field[LEN_FIELD_SIZE];
    if (!read_exact(reader, len_field, LEN_FIELD_SIZE)) {
        return corrupt("incomplete file");
    }
    uint32_t models_len = le_uint32(len_field);
    if (models_len > MAX_MODELS_LEN) {
        return corrupt("models too large");
    }
    if (models_len == 0) {
        return ok();
    }
    uint8_t *body = allocate(models_len);
    if (body == NULL) {
        return out_of_memory();
    }
    mvdz_error error = read_exact(reader, body, models_len)
                           ? read_models_block(body, models_len, memory, models)
                           : corrupt("incomplete file");
    free(body);
    return error;
}

/* The segment fields are checked in the order of FORMAT.md 7; then the whole-demo
 * checks, FORMAT.md 3.4. */
static mvdz_error decode_segments(file_reader reader, mvdz_write write, void *write_context,
                                  const mvdz_header *header, const decoder_model *models,
                                  uint8_t *buffers) {
    uint8_t *fields = buffers;
    uint8_t *coded = fields + SEGMENT_FIELDS_SIZE;
    uint8_t *segment = coded + MAX_CODED_LEN;
    uint64_t raw_total = 0;
    uint32_t demo_crc32 = 0;
    while (raw_total < header->size) {
        uint8_t len_field[LEN_FIELD_SIZE];
        if (!read_exact(reader, len_field, LEN_FIELD_SIZE)) {
            return corrupt("incomplete file");
        }
        uint32_t coded_len = le_uint32(len_field);
        if (coded_len > MAX_CODED_LEN) {
            return corrupt("segment size");
        }
        if (!read_exact(reader, fields, SEGMENT_FIELDS_SIZE + coded_len)) {
            return corrupt("incomplete file");
        }
        uint32_t raw_len = le_uint32(fields);
        uint32_t segment_crc32 = le_uint32(fields + 8); /* after duration_ms */
        if (raw_len == 0) {
            return corrupt("empty segment");
        }
        if (raw_len > MAX_RAW_LEN) {
            return corrupt("segment too large");
        }
        if (raw_len > header->size - raw_total) {
            return corrupt("segment sizes");
        }
        raw_total += raw_len;

        mvdz_error error = decode_segment(segment, raw_len, coded, coded_len, models);
        if (error.kind != MVDZ_OK) {
            return error;
        }
        if (crc32_update(0, segment, raw_len) != segment_crc32) {
            return corrupt("segment checksum");
        }
        if (!write(write_context, segment, raw_len)) {
            mvdz_error stopped = {MVDZ_STOPPED, "the write callback stopped the decoding"};
            return stopped;
        }
        demo_crc32 = crc32_combine(demo_crc32, segment_crc32, raw_len);
    }
    uint8_t trailing;
    if (reader.read(reader.context, &trailing, 1) != 0) {
        return corrupt("trailing bytes");
    }
    if (demo_crc32 != header->crc32) {
        return corrupt("checksum mismatch");
    }
    return ok();
}

static bool read_exact(file_reader reader, uint8_t *buffer, size_t len) {
    return reader.read(reader.context, buffer, len) == len;
}

/* FORMAT.md 3.3 */
static mvdz_error decode_segment(uint8_t *destination, uint32_t raw_len, const uint8_t *coded,
                                 uint32_t coded_len, const decoder_model *models) {
    transform_header header;
    mvdz_error error = parse_transform_header(coded, coded_len, raw_len, &header);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    return decode_transform(destination, raw_len, coded, &header, models);
}
