/* The unread bytes of a blob stream. */
typedef struct blob_stream {
    const uint8_t *data;
    uint32_t len;
} blob_stream;

/* segment_streams are the streams of a segment as the walk reads them. */
typedef struct segment_streams {
    protocol_state protocol; /* at the segment start */
    stream_decoder decoders[FIRST_BLOB_STREAM];
    blob_stream blobs[STREAM_COUNT];
} segment_streams;

typedef struct stream_entry {
    uint32_t id;
    uint32_t len; /* the body of a structural or numeric stream, the bytes of a blob stream */
} stream_entry;

typedef struct transform_header {
    protocol_state protocol;
    stream_entry entries[STREAM_COUNT];
    uint32_t entry_count; /* 0 for a stored segment */
    uint32_t blob_total;
    size_t body_offset;
} transform_header;

static uint64_t max_blob_total(uint32_t raw_len);
static mvdz_error decode_streams(uint8_t *destination, uint32_t raw_len, const uint8_t *bodies,
                                 const transform_header *header, const decoder_model *models,
                                 uint8_t *blob_bytes);
static mvdz_error walk(uint8_t *segment, uint32_t segment_len, segment_streams *streams);

/* FORMAT.md 3.1 */
static mvdz_error parse_transform_header(const uint8_t *coded, uint32_t coded_len, uint32_t raw_len,
                                         transform_header *header) {
    byte_cursor fields = {coded, coded_len, 0, false};
    header->protocol.fte = read_uint32(&fields);
    header->protocol.mvd_1 = read_uint32(&fields);
    uint32_t entry_count = read_uint32(&fields);
    if (fields.is_short) {
        return corrupt("segment header");
    }
    if (entry_count > STREAM_COUNT) {
        return corrupt("stream count");
    }

    uint32_t next_id = 0;
    uint64_t body_total = 0;
    uint64_t blob_total = 0;
    for (uint32_t index = 0; index < entry_count; index++) {
        stream_entry *entry = &header->entries[index];
        entry->id = read_uint32(&fields);
        if (fields.is_short) {
            return corrupt("stream table");
        }
        if (entry->id >= STREAM_COUNT) {
            return corrupt("stream id");
        }
        entry->len = read_uint32(&fields);
        if (fields.is_short) {
            return corrupt("stream table");
        }
        if (entry->id < next_id) {
            return corrupt("stream order");
        }
        next_id = entry->id + 1;
        if (entry->id >= FIRST_BLOB_STREAM) {
            blob_total += entry->len;
        } else {
            body_total += entry->len;
        }
    }
    if (blob_total > max_blob_total(raw_len)) {
        return corrupt("blob streams");
    }

    uint64_t rest_len = coded_len - fields.position;
    bool is_stored = entry_count == 0;
    if (is_stored && rest_len != raw_len) {
        return corrupt("stored segment");
    }
    if (!is_stored && rest_len != body_total) {
        return corrupt("stream bodies");
    }
    header->entry_count = entry_count;
    header->blob_total = (uint32_t)blob_total;
    header->body_offset = fields.position;
    return ok();
}

/* FORMAT.md 3.3: writes the raw_len bytes of the segment to destination. */
static mvdz_error decode_transform(uint8_t *destination, uint32_t raw_len, const uint8_t *coded,
                                   const transform_header *header, const decoder_model *models) {
    const uint8_t *rest = coded + header->body_offset;
    bool is_stored = header->entry_count == 0;
    if (is_stored) {
        memcpy(destination, rest, raw_len);
        return ok();
    }
    uint8_t *blob_bytes = allocate(header->blob_total);
    if (blob_bytes == NULL) {
        return out_of_memory();
    }
    mvdz_error error = decode_streams(destination, raw_len, rest, header, models, blob_bytes);
    free(blob_bytes);
    return error;
}

/* read_blob_bytes fills destination from a blob stream, false if it has fewer bytes. */
static bool read_blob_bytes(segment_streams *streams, uint32_t stream, uint8_t *destination,
                            uint32_t count) {
    blob_stream *blob = &streams->blobs[stream];
    if (count > blob->len) {
        return false;
    }
    if (count == 0) {
        return true; /* a stream not in the table has no data to copy from */
    }
    memcpy(destination, blob->data, count);
    blob->data += count;
    blob->len -= count;
    return true;
}

/* max_blob_total is of the blob streams of a segment together, FORMAT.md 7. */
static uint64_t max_blob_total(uint32_t raw_len) {
    return 2 * (uint64_t)raw_len;
}

static mvdz_error decode_streams(uint8_t *destination, uint32_t raw_len, const uint8_t *bodies,
                                 const transform_header *header, const decoder_model *models,
                                 uint8_t *blob_bytes) {
    segment_streams streams = {
        header->protocol, {{NULL, NULL, 0, 0, 0, {NULL, 0, 0}}}, {{NULL, 0}}};
    for (uint32_t index = 0; index < header->entry_count; index++) {
        const stream_entry *entry = &header->entries[index];
        if (entry->id >= FIRST_BLOB_STREAM) {
            continue;
        }
        if (!models[entry->id].is_listed) {
            return corrupt("stream without a model");
        }
        mvdz_error error = new_stream_decoder(&models[entry->id], bodies, entry->len,
                                              &streams.decoders[entry->id]);
        if (error.kind != MVDZ_OK) {
            return error;
        }
        bodies += entry->len;
    }

    mvdz_error error = lz_expand(blob_bytes, header->blob_total, streams.decoders);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    bool are_blob_tokens_done = is_done(&streams.decoders[STREAM_LZ_LENGTH]) &&
                                is_done(&streams.decoders[STREAM_LZ_DISTANCE]);
    if (!are_blob_tokens_done) {
        return corrupt("blob tokens");
    }
    for (uint32_t index = 0; index < header->entry_count; index++) {
        const stream_entry *entry = &header->entries[index];
        if (entry->id >= FIRST_BLOB_STREAM) {
            streams.blobs[entry->id].data = blob_bytes;
            streams.blobs[entry->id].len = entry->len;
            blob_bytes += entry->len;
        }
    }

    error = walk(destination, raw_len, &streams);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    for (uint32_t stream = FIRST_BLOB_STREAM; stream < STREAM_COUNT; stream++) {
        if (streams.blobs[stream].len != 0) {
            return corrupt("unconsumed stream data");
        }
    }
    for (uint32_t stream = 0; stream < FIRST_BLOB_STREAM; stream++) {
        if (!is_done(&streams.decoders[stream])) {
            return corrupt("unconsumed stream data");
        }
    }
    return ok();
}
