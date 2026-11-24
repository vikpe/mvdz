enum {
    DEFAULT_MAX_MEMORY = 64 << 20,
    /* What a demo's decoder tables take with room to spare: no lower limit applies. */
    LEAST_MAX_MEMORY = 14 << 20
};

typedef struct demo_buffer {
    uint8_t *data;
    size_t len;
    size_t capacity;
} demo_buffer;

/* demo_writer collects the segments of mvdz_decode. */
typedef struct demo_writer {
    demo_buffer demo;
    uint64_t demo_size;         /* the header's */
    const table_memory *memory; /* what the tables took of the limit */
    mvdz_error error;           /* why it stopped */
} demo_writer;

static size_t effective_max_memory(size_t max_memory);
static mvdz_error decode_with_max_memory(uint8_t **demo, size_t *demo_size, const uint8_t *file,
                                         size_t file_len, size_t max_memory);
static bool write_to_demo(void *context, const uint8_t *segment, size_t segment_len);
static mvdz_error decode_file(mvdz_write write, void *context, const uint8_t *file, size_t file_len,
                              table_memory *memory);
static mvdz_error read_models(byte_cursor *file_fields, table_memory *memory,
                              decoder_model *models);
static mvdz_error decode_segments(mvdz_write write, void *context, byte_cursor *file_fields,
                                  const mvdz_header *header, const decoder_model *models,
                                  uint8_t *segment);
static mvdz_error grow(demo_buffer *demo, size_t segment_len, uint64_t demo_size,
                       uint64_t max_demo_size);
static mvdz_error decode_segment(uint8_t *destination, uint32_t raw_len, const uint8_t *coded,
                                 uint32_t coded_len, const decoder_model *models);

mvdz_error mvdz_decode(uint8_t **demo, size_t *demo_size, const uint8_t *file, size_t file_len,
                       size_t max_memory) {
    return decode_with_max_memory(demo, demo_size, file, file_len,
                                  effective_max_memory(max_memory));
}

mvdz_error mvdz_decode_segments(mvdz_write write, void *context, const uint8_t *file,
                                size_t file_len, size_t max_memory) {
    table_memory memory = {0, effective_max_memory(max_memory)};
    return decode_file(write, context, file, file_len, &memory);
}

/* 0 is the default; less than the least counts as the least. */
static size_t effective_max_memory(size_t max_memory) {
    if (max_memory == 0) {
        return DEFAULT_MAX_MEMORY;
    }
    return max_memory < LEAST_MAX_MEMORY ? LEAST_MAX_MEMORY : max_memory;
}

/* max_memory as it is given: the demo and the decoder tables together. */
static mvdz_error decode_with_max_memory(uint8_t **demo, size_t *demo_size, const uint8_t *file,
                                         size_t file_len, size_t max_memory) {
    *demo = NULL;
    *demo_size = 0;
    mvdz_header header;
    mvdz_error error = parse_header(file, file_len, &header);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    table_memory memory = {0, max_memory};
    demo_writer writer = {{NULL, 0, 0}, header.size, &memory, ok()};
    error = decode_file(write_to_demo, &writer, file, file_len, &memory);
    if (error.kind == MVDZ_STOPPED) {
        error = writer.error;
    }
    if (error.kind != MVDZ_OK) {
        free(writer.demo.data);
        return error;
    }
    *demo = writer.demo.data;
    *demo_size = writer.demo.len;
    return ok();
}

/* The demo may take what the decoder tables left of the limit. */
static bool write_to_demo(void *context, const uint8_t *segment, size_t segment_len) {
    demo_writer *writer = context;
    demo_buffer *demo = &writer->demo;
    uint64_t max_demo_size = writer->memory->max - writer->memory->used;
    writer->error = grow(demo, segment_len, writer->demo_size, max_demo_size);
    if (writer->error.kind != MVDZ_OK) {
        return false;
    }
    memcpy(demo->data + demo->len, segment, segment_len);
    demo->len += segment_len;
    return true;
}

/* decode_file passes the demo to write a segment at a time; memory counts the decoder
 * tables. */
static mvdz_error decode_file(mvdz_write write, void *context, const uint8_t *file, size_t file_len,
                              table_memory *memory) {
    mvdz_header header;
    mvdz_error error = parse_header(file, file_len, &header);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    uint8_t *segment = allocate(MAX_RAW_LEN);
    if (segment == NULL) {
        return out_of_memory();
    }
    byte_cursor file_fields = {file, file_len, HEADER_SIZE, false};
    decoder_model models[FIRST_BLOB_STREAM] = {{false, NULL, 0, 0, NULL, NULL}};

    error = read_models(&file_fields, memory, models);
    if (error.kind == MVDZ_OK) {
        error = decode_segments(write, context, &file_fields, &header, models, segment);
    }
    free_models(models);
    free(segment);
    return error;
}

/* FORMAT.md 2.2 */
static mvdz_error read_models(byte_cursor *file_fields, table_memory *memory,
                              decoder_model *models) {
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
    return read_models_block(body, models_len, memory, models);
}

/* The segment fields are checked in the order of FORMAT.md 7; then the whole-demo
 * checks, FORMAT.md 3.4. segment has room for MAX_RAW_LEN bytes. */
static mvdz_error decode_segments(mvdz_write write, void *context, byte_cursor *file_fields,
                                  const mvdz_header *header, const decoder_model *models,
                                  uint8_t *segment) {
    uint64_t raw_total = 0;
    uint32_t demo_crc32 = 0;
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

        mvdz_error error = decode_segment(segment, raw_len, coded, coded_len, models);
        if (error.kind != MVDZ_OK) {
            return error;
        }
        if (crc32_update(0, segment, raw_len) != segment_crc32) {
            return corrupt("segment checksum");
        }
        if (!write(context, segment, raw_len)) {
            mvdz_error stopped = {MVDZ_STOPPED, "the write callback stopped the decoding"};
            return stopped;
        }
        demo_crc32 = crc32_combine(demo_crc32, segment_crc32, raw_len);
    }
    if (!is_at_end(file_fields)) {
        return corrupt("trailing bytes");
    }
    if (demo_crc32 != header->crc32) {
        return corrupt("checksum mismatch");
    }
    return ok();
}

/* grow makes room for a segment. The buffer grows by decoded segment, at most to
 * demo_size: a lying header allocates nothing. */
static mvdz_error grow(demo_buffer *demo, size_t segment_len, uint64_t demo_size,
                       uint64_t max_demo_size) {
    uint64_t needed = (uint64_t)demo->len + segment_len;
    if (needed > max_demo_size) {
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
    if (capacity > max_demo_size) {
        capacity = max_demo_size;
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
