enum { DEFAULT_MAX_MEMORY = 1 << 30 };

typedef struct demo_buffer {
    uint8_t *data;
    size_t len;
    size_t capacity;
} demo_buffer;

static mvdz_error decode_with_max_memory(uint8_t **demo, size_t *demo_size, const uint8_t *file,
                                         size_t file_len, uint64_t max_memory);
static mvdz_error read_models(byte_cursor *file_fields, decoder_model *models);
static mvdz_error decode_segments(byte_cursor *file_fields, const mvdz_header *header,
                                  const decoder_model *models, uint64_t max_memory,
                                  demo_buffer *demo, uint32_t *demo_crc32);
static mvdz_error grow(demo_buffer *demo, uint32_t raw_len, uint64_t demo_size,
                       uint64_t max_memory);
static mvdz_error decode_segment(uint8_t *destination, uint32_t raw_len, const uint8_t *coded,
                                 uint32_t coded_len, const decoder_model *models);

mvdz_error mvdz_decode(uint8_t **demo, size_t *demo_size, const uint8_t *file, size_t file_len) {
    return decode_with_max_memory(demo, demo_size, file, file_len, DEFAULT_MAX_MEMORY);
}

/* max_memory limits the demo; the decoder tables have the default limit. */
static mvdz_error decode_with_max_memory(uint8_t **demo, size_t *demo_size, const uint8_t *file,
                                         size_t file_len, uint64_t max_memory) {
    *demo = NULL;
    *demo_size = 0;
    mvdz_header header;
    mvdz_error error = parse_header(file, file_len, &header);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    byte_cursor file_fields = {file, file_len, HEADER_SIZE, false};
    decoder_model models[FIRST_BLOB_STREAM] = {{false, NULL, 0, 0, NULL, NULL}};
    demo_buffer decoded = {NULL, 0, 0};
    uint32_t demo_crc32 = 0;

    error = read_models(&file_fields, models);
    if (error.kind == MVDZ_OK) {
        error = decode_segments(&file_fields, &header, models, max_memory, &decoded, &demo_crc32);
    }
    /* the whole-demo checks, FORMAT.md 3.4 */
    if (error.kind == MVDZ_OK && !is_at_end(&file_fields)) {
        error = corrupt("trailing bytes");
    }
    if (error.kind == MVDZ_OK && demo_crc32 != header.crc32) {
        error = corrupt("checksum mismatch");
    }

    free_models(models);
    if (error.kind != MVDZ_OK) {
        free(decoded.data);
        return error;
    }
    *demo = decoded.data;
    *demo_size = decoded.len;
    return ok();
}

/* FORMAT.md 2.2 */
static mvdz_error read_models(byte_cursor *file_fields, decoder_model *models) {
    uint32_t models_len = read_uint32(file_fields);
    if (file_fields->is_short) {
        return corrupt("incomplete file");
    }
    if (models_len > MAX_MODELS_LEN) {
        return corrupt("models too large");
    }
    const uint8_t *body = read_bytes(file_fields, models_len);
    if (file_fields->is_short) {
        return corrupt("incomplete file");
    }
    if (models_len == 0) {
        return ok();
    }
    return read_models_block(body, models_len, DEFAULT_MAX_MEMORY, models);
}

/* The segment fields are checked in the order of FORMAT.md 7. */
static mvdz_error decode_segments(byte_cursor *file_fields, const mvdz_header *header,
                                  const decoder_model *models, uint64_t max_memory,
                                  demo_buffer *demo, uint32_t *demo_crc32) {
    uint64_t raw_total = 0;
    while (raw_total < header->size) {
        uint32_t coded_len = read_uint32(file_fields);
        if (file_fields->is_short) {
            return corrupt("incomplete file");
        }
        if (coded_len > MAX_CODED_LEN) {
            return corrupt("segment size");
        }
        uint32_t raw_len = read_uint32(file_fields);
        read_uint32(file_fields); /* duration_ms */
        uint32_t segment_crc32 = read_uint32(file_fields);
        const uint8_t *coded = read_bytes(file_fields, coded_len);
        if (file_fields->is_short) {
            return corrupt("incomplete file");
        }
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

        mvdz_error error = grow(demo, raw_len, header->size, max_memory);
        if (error.kind != MVDZ_OK) {
            return error;
        }
        uint8_t *segment = demo->data + demo->len;
        error = decode_segment(segment, raw_len, coded, coded_len, models);
        if (error.kind != MVDZ_OK) {
            return error;
        }
        if (crc32_update(0, segment, raw_len) != segment_crc32) {
            return corrupt("segment checksum");
        }
        demo->len += raw_len;
        *demo_crc32 = crc32_combine(*demo_crc32, segment_crc32, raw_len);
    }
    return ok();
}

/* grow makes room for a segment. The buffer grows by decoded segment, at most to
 * demo_size: a lying header allocates nothing. */
static mvdz_error grow(demo_buffer *demo, uint32_t raw_len, uint64_t demo_size,
                       uint64_t max_memory) {
    uint64_t needed = (uint64_t)demo->len + raw_len;
    if (needed > max_memory) {
        return memory_limit("demo size");
    }
    if (needed <= demo->capacity) {
        return ok();
    }
    uint64_t capacity = 2 * (uint64_t)demo->capacity;
    if (capacity > demo_size) {
        capacity = demo_size;
    }
    if (capacity < needed) {
        capacity = needed;
    }
    if (capacity > max_memory) {
        capacity = max_memory;
    }
    uint8_t *data = realloc(demo->data, (size_t)capacity);
    if (data == NULL) {
        return out_of_memory();
    }
    demo->data = data;
    demo->capacity = (size_t)capacity;
    return ok();
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
