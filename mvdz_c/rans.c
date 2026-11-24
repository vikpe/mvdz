enum {
    STRUCTURAL_ALPHABET_SIZE = 256,
    NUMERIC_ALPHABET_SIZE = 65,
    OWN_COUNT_SIZE = 4,
    MAX_OWN_COUNT = 1 << 16,
    SCALE_BITS = 12,
    FREQUENCY_TOTAL = 1 << SCALE_BITS,
    RANS_LOWER_BOUND = 1 << 16
};

/* An entry of a decoder_model is ENTRY_EMPTY for a table without symbols,
 * ENTRY_LOOKUP with an offset into lookup_tables, or an offset into pool. */
static const uint32_t ENTRY_EMPTY = UINT32_C(1) << 31;
static const uint32_t ENTRY_LOOKUP = UINT32_C(1) << 30;

/* decoder_model is the model of one stream as the decoder reads symbols with
 * it, FORMAT.md 6.4. A symbol's range is packed as the symbol, its frequency
 * less one from bit 8 (a lone symbol's 4096 needs the 13th bit), and from
 * bit 20 the slot the range starts at (pool) or the slot's offset into the
 * range (lookup_tables). */
typedef struct decoder_model {
    bool is_listed;     /* false where the models block has none */
    uint32_t *entry_of; /* by context; contexts past its end use fallback. Owns the tables. */
    uint32_t entry_count;
    uint32_t fallback;
    /* The search tables: the ranges of a table's symbols, the most frequent first. */
    const uint32_t *pool;
    /* FREQUENCY_TOTAL slots per table: the range each slot is in. */
    const uint32_t *lookup_tables;
} decoder_model;

/* table_memory is the decoder tables of a file's models, against the memory limit. */
typedef struct table_memory {
    uint64_t used;
    uint64_t max;
} table_memory;

/* bit_reader reads mantissa bits, FORMAT.md 6.4: least significant bit
 * first, zeros past the end. */
typedef struct bit_reader {
    const uint8_t *data;
    uint32_t len;
    uint32_t position;
} bit_reader;

/* stream_decoder decodes one structural or numeric stream of a segment,
 * FORMAT.md 6.1, 6.4 and 6.5. */
typedef struct stream_decoder {
    const decoder_model *model; /* NULL for a stream not in the table: no symbols, done */
    const uint8_t *rans;
    uint32_t rans_len;
    uint32_t word_position;
    uint32_t state;
    bit_reader mantissas;
} stream_decoder;

static mvdz_error read_listed_models(byte_cursor *fields, const decoder_model *meta,
                                     uint32_t meta_raw_len, table_memory *memory,
                                     decoder_model *models);
static mvdz_error new_decoder_model(const uint8_t *raw, uint32_t raw_len, uint32_t alphabet_size,
                                    table_memory *memory, decoder_model *model);
static mvdz_error read_model_header(const uint8_t *raw, uint32_t raw_len, uint32_t alphabet_size,
                                    uint32_t *own_count, const uint8_t **quantized_tables);
static uint32_t raw_model_len(uint32_t own_count, uint32_t alphabet_size);
static uint32_t present_count(const uint8_t *quantized, uint32_t alphabet_size);
static mvdz_error reserve(table_memory *memory, uint64_t size);
static void new_rans_table(const uint8_t *quantized, uint32_t alphabet_size, uint16_t *table);
static uint32_t write_search_table(const uint16_t *table, uint32_t alphabet_size, uint32_t *ranges);
static int compare_ranges(const void *a, const void *b);
static uint32_t write_lookup_table(const uint16_t *table, uint32_t alphabet_size, uint32_t *ranges);
static mvdz_error decode_raw_model(uint8_t *raw, uint32_t raw_len, const uint8_t *coded,
                                   uint32_t coded_len, const decoder_model *meta);
static uint32_t stream_alphabet_size(uint32_t stream);
static uint32_t read_word(stream_decoder *decoder);
static uint32_t read_bits(bit_reader *reader, uint32_t count);
static bool bit_reader_is_done(const bit_reader *reader);

/* FORMAT.md 2.2. models has FIRST_BLOB_STREAM entries, by stream id; they are freed
 * with free_models, after an error too. A file whose decoder tables take more than
 * memory allows is refused. */
static mvdz_error read_models_block(const uint8_t *body, uint32_t body_len, table_memory *memory,
                                    decoder_model *models) {
    if (body_len < OWN_COUNT_SIZE) {
        return corrupt("tables");
    }
    uint32_t meta_own_count = le_uint32(body);
    if (meta_own_count > MAX_OWN_COUNT) {
        return corrupt("tables");
    }
    byte_cursor fields = {body, body_len, 0, false};
    uint32_t meta_raw_len = raw_model_len(meta_own_count, STRUCTURAL_ALPHABET_SIZE);
    const uint8_t *meta_raw = read_bytes(&fields, meta_raw_len);
    if (fields.is_short) {
        return corrupt("tables");
    }

    decoder_model meta = {false, NULL, 0, 0, NULL, NULL};
    mvdz_error error =
        new_decoder_model(meta_raw, meta_raw_len, STRUCTURAL_ALPHABET_SIZE, memory, &meta);
    if (error.kind != MVDZ_OK) {
        return error;
    }

    error = read_listed_models(&fields, &meta, meta_raw_len, memory, models);
    free(meta.entry_of);
    return error;
}

static void free_models(decoder_model *models) {
    for (uint32_t stream = 0; stream < FIRST_BLOB_STREAM; stream++) {
        free(models[stream].entry_of);
    }
}

static mvdz_error new_stream_decoder(const decoder_model *model, const uint8_t *body,
                                     uint32_t body_len, stream_decoder *decoder) {
    byte_cursor fields = {body, body_len, 0, false};
    uint32_t rans_len = read_uint32(&fields);
    const uint8_t *rans = read_bytes(&fields, rans_len);
    if (fields.is_short) {
        return corrupt("rans stream");
    }
    decoder->model = model;
    decoder->rans = rans;
    decoder->rans_len = rans_len;
    decoder->word_position = 0;
    decoder->mantissas.data = body + fields.position;
    decoder->mantissas.len = body_len - (uint32_t)fields.position;
    decoder->mantissas.position = 0;
    uint32_t high = read_word(decoder);
    decoder->state = high << 16 | read_word(decoder);
    if (decoder->state < RANS_LOWER_BOUND) {
        return corrupt("rans stream");
    }
    return ok();
}

/* decode_symbol is false if the context's table is empty. */
static bool decode_symbol(stream_decoder *decoder, uint32_t context, uint8_t *symbol) {
    const decoder_model *model = decoder->model;
    if (model == NULL) {
        return false;
    }
    uint32_t entry = context < model->entry_count ? model->entry_of[context] : model->fallback;
    if (entry == ENTRY_EMPTY) {
        return false;
    }
    uint32_t slot = decoder->state & (FREQUENCY_TOTAL - 1);
    uint32_t symbol_range;
    uint32_t offset;
    if ((entry & ENTRY_LOOKUP) != 0) {
        symbol_range = model->lookup_tables[(entry & ~ENTRY_LOOKUP) + slot];
        offset = symbol_range >> 20;
    } else {
        /* the ranges of a table cover every slot: the search ends within it;
         * a slot below the range wraps to an offset past it */
        for (uint32_t position = entry;; position++) {
            symbol_range = model->pool[position];
            offset = slot - (symbol_range >> 20);
            if (offset <= (symbol_range >> 8 & 0xfff)) {
                break;
            }
        }
    }
    uint32_t state = ((symbol_range >> 8 & 0xfff) + 1) * (decoder->state >> SCALE_BITS) + offset;
    if (state < RANS_LOWER_BOUND) {
        state = state << 16 | read_word(decoder);
    }
    decoder->state = state;
    *symbol = (uint8_t)symbol_range;
    return true;
}

/* decode_value gives the next residual of a numeric stream: a bucket symbol, then
 * the mantissa bits below the leading one. */
static bool decode_value(stream_decoder *decoder, uint32_t context, int32_t *value) {
    uint8_t bucket;
    if (!decode_symbol(decoder, context, &bucket)) {
        return false;
    }
    uint32_t bit_len = ((uint32_t)bucket + 1) / 2;
    if (bit_len == 0) {
        *value = 0;
        return true;
    }
    uint32_t magnitude = UINT32_C(1) << (bit_len - 1) | read_bits(&decoder->mantissas, bit_len - 1);
    bool is_negative = bucket % 2 == 0;
    *value = (int32_t)(is_negative ? 0 - magnitude : magnitude);
    return true;
}

static bool is_done(const stream_decoder *decoder) {
    if (decoder->model == NULL) {
        return true;
    }
    return decoder->state == RANS_LOWER_BOUND && decoder->word_position == decoder->rans_len &&
           bit_reader_is_done(&decoder->mantissas);
}

/* read_listed_models reads the entries after the meta model, each a raw model coded
 * under it. */
static mvdz_error read_listed_models(byte_cursor *fields, const decoder_model *meta,
                                     uint32_t meta_raw_len, table_memory *memory,
                                     decoder_model *models) {
    uint32_t entry_count = read_uint32(fields);
    if (fields->is_short || entry_count == 0 || entry_count > FIRST_BLOB_STREAM) {
        return corrupt("tables");
    }
    /* before any model is decoded: no model allocates more than the format allows */
    byte_cursor checked = *fields;
    uint32_t next_id = 0;
    uint64_t raw_total = meta_raw_len;
    for (uint32_t index = 0; index < entry_count; index++) {
        uint32_t id = read_uint32(&checked);
        raw_total += read_uint32(&checked);
        read_bytes(&checked, read_uint32(&checked));
        if (checked.is_short || id < next_id || id >= FIRST_BLOB_STREAM) {
            return corrupt("tables");
        }
        next_id = id + 1;
    }
    if (!is_at_end(&checked)) {
        return corrupt("tables");
    }
    if (raw_total > MAX_MODELS_RAW_LEN) {
        return corrupt("models size");
    }

    for (uint32_t index = 0; index < entry_count; index++) {
        uint32_t id = read_uint32(fields);
        uint32_t raw_len = read_uint32(fields);
        uint32_t coded_len = read_uint32(fields);
        const uint8_t *coded = read_bytes(fields, coded_len);
        uint8_t *raw = allocate(raw_len);
        if (raw == NULL) {
            return out_of_memory();
        }
        mvdz_error error = decode_raw_model(raw, raw_len, coded, coded_len, meta);
        if (error.kind == MVDZ_OK) {
            error = new_decoder_model(raw, raw_len, stream_alphabet_size(id), memory, &models[id]);
        }
        free(raw);
        if (error.kind != MVDZ_OK) {
            return error;
        }
    }
    return ok();
}

/* new_decoder_model reads a raw model. A wide table decodes with a slot lookup
 * table: a search of it could take as many steps as it has symbols. */
static mvdz_error new_decoder_model(const uint8_t *raw, uint32_t raw_len, uint32_t alphabet_size,
                                    table_memory *memory, decoder_model *model) {
    /* Rust also gives a model's 16 most used tables a lookup table; measured here,
     * that took 8 % more cycles than the search that starts at the most frequent symbol. */
    enum { SEARCH_MAX = 64 };
    uint32_t own_count;
    const uint8_t *quantized_tables;
    mvdz_error error =
        read_model_header(raw, raw_len, alphabet_size, &own_count, &quantized_tables);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    const uint8_t *own_contexts = raw + OWN_COUNT_SIZE;
    uint32_t table_count = own_count + 1;

    /* present (frequency >= 1) exactly when the quantized count is non-zero
     * (normalize): the layout is known before any table is built */
    uint32_t pool_count = 0;
    uint32_t lookup_count = 0;
    for (uint32_t index = 0; index < table_count; index++) {
        uint32_t present = present_count(quantized_tables + index * alphabet_size, alphabet_size);
        if (present > SEARCH_MAX) {
            lookup_count += FREQUENCY_TOTAL;
        } else {
            pool_count += present;
        }
    }
    uint32_t entry_count = 0;
    if (own_count > 0) {
        entry_count = (uint32_t)le_uint16(own_contexts + 2 * (own_count - 1)) + 1;
    }
    uint64_t range_count = (uint64_t)entry_count + pool_count + lookup_count;
    error = reserve(memory, sizeof(uint32_t) * range_count);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    uint32_t *entry_of = allocate(sizeof(uint32_t) * (size_t)range_count);
    if (entry_of == NULL) {
        return out_of_memory();
    }
    uint32_t *pool = entry_of + entry_count;
    uint32_t *lookup_tables = pool + pool_count;

    /* at most MAX_OWN_COUNT + 1 tables: every offset is below ENTRY_LOOKUP */
    uint32_t pool_position = 0;
    uint32_t lookup_position = 0;
    for (uint32_t index = 0; index < table_count; index++) {
        const uint8_t *quantized = quantized_tables + index * alphabet_size;
        uint16_t table[STRUCTURAL_ALPHABET_SIZE + 1];
        new_rans_table(quantized, alphabet_size, table);
        uint32_t present = present_count(quantized, alphabet_size);
        uint32_t entry;
        if (present == 0) {
            entry = ENTRY_EMPTY;
        } else if (present > SEARCH_MAX) {
            entry = ENTRY_LOOKUP | lookup_position;
            lookup_position +=
                write_lookup_table(table, alphabet_size, lookup_tables + lookup_position);
        } else {
            entry = pool_position;
            pool_position += write_search_table(table, alphabet_size, pool + pool_position);
        }
        bool is_fallback = index == 0;
        if (is_fallback) {
            model->fallback = entry;
            for (uint32_t context = 0; context < entry_count; context++) {
                entry_of[context] = entry;
            }
        } else {
            entry_of[le_uint16(own_contexts + 2 * (index - 1))] = entry;
        }
    }
    model->is_listed = true;
    model->entry_of = entry_of;
    model->entry_count = entry_count;
    model->pool = pool;
    model->lookup_tables = lookup_tables;
    return ok();
}

/* read_model_header gives the own count and the quantized counts of every
 * table, the fallback first; the own contexts are between them. FORMAT.md 6.2. */
static mvdz_error read_model_header(const uint8_t *raw, uint32_t raw_len, uint32_t alphabet_size,
                                    uint32_t *own_count, const uint8_t **quantized_tables) {
    if (raw_len < OWN_COUNT_SIZE) {
        return corrupt("model");
    }
    uint32_t declared_own_count = le_uint32(raw);
    bool is_len_valid = declared_own_count <= MAX_OWN_COUNT &&
                        raw_len == raw_model_len(declared_own_count, alphabet_size);
    if (!is_len_valid) {
        return corrupt("model");
    }
    const uint8_t *own_contexts = raw + OWN_COUNT_SIZE;
    for (uint32_t index = 1; index < declared_own_count; index++) {
        bool is_ascending =
            le_uint16(own_contexts + 2 * index) > le_uint16(own_contexts + 2 * (index - 1));
        if (!is_ascending) {
            return corrupt("model");
        }
    }
    const uint8_t *tables = own_contexts + 2 * declared_own_count;
    uint32_t tables_len = (declared_own_count + 1) * alphabet_size;
    for (uint32_t index = 0; index < tables_len; index++) {
        if (tables[index] > MAX_QUANTIZED) {
            return corrupt("model");
        }
    }
    *own_count = declared_own_count;
    *quantized_tables = tables;
    return ok();
}

/* FORMAT.md 6.2 */
static uint32_t raw_model_len(uint32_t own_count, uint32_t alphabet_size) {
    return OWN_COUNT_SIZE + 2 * own_count + (own_count + 1) * alphabet_size;
}

static uint32_t present_count(const uint8_t *quantized, uint32_t alphabet_size) {
    uint32_t count = 0;
    for (uint32_t symbol = 0; symbol < alphabet_size; symbol++) {
        if (quantized[symbol] > 0) {
            count++;
        }
    }
    return count;
}

static mvdz_error reserve(table_memory *memory, uint64_t size) {
    memory->used += size;
    if (memory->used > memory->max) {
        return memory_limit("models");
    }
    return ok();
}

/* table is by symbol: a symbol owns the slots table[symbol] to
 * table[symbol + 1], FORMAT.md 6.3. */
static void new_rans_table(const uint8_t *quantized, uint32_t alphabet_size, uint16_t *table) {
    uint64_t weights[STRUCTURAL_ALPHABET_SIZE];
    for (uint32_t symbol = 0; symbol < alphabet_size; symbol++) {
        weights[symbol] = quantized_weight(quantized[symbol]);
    }
    int32_t frequencies[STRUCTURAL_ALPHABET_SIZE];
    normalize(weights, alphabet_size, FREQUENCY_TOTAL, frequencies);
    table[0] = 0;
    for (uint32_t symbol = 0; symbol < alphabet_size; symbol++) {
        table[symbol + 1] = (uint16_t)(table[symbol] + frequencies[symbol]);
    }
}

/* write_search_table returns the count of ranges written: one per present symbol. */
static uint32_t write_search_table(const uint16_t *table, uint32_t alphabet_size,
                                   uint32_t *ranges) {
    uint32_t count = 0;
    for (uint32_t symbol = 0; symbol < alphabet_size; symbol++) {
        uint32_t start = table[symbol];
        uint32_t end = table[symbol + 1];
        if (end > start) {
            ranges[count] = symbol | (end - start - 1) << 8 | start << 20;
            count++;
        }
    }
    qsort(ranges, count, sizeof ranges[0], compare_ranges);
    return count;
}

/* The most frequent first; a total order, as no two ranges are equal. */
static int compare_ranges(const void *a, const void *b) {
    uint32_t left = *(const uint32_t *)a;
    uint32_t right = *(const uint32_t *)b;
    uint32_t left_frequency = left >> 8 & 0xfff;
    uint32_t right_frequency = right >> 8 & 0xfff;
    if (left_frequency != right_frequency) {
        return left_frequency > right_frequency ? -1 : 1;
    }
    return (left > right) - (left < right);
}

/* write_lookup_table returns the count of ranges written: FREQUENCY_TOTAL. */
static uint32_t write_lookup_table(const uint16_t *table, uint32_t alphabet_size,
                                   uint32_t *ranges) {
    uint32_t count = 0;
    for (uint32_t symbol = 0; symbol < alphabet_size; symbol++) {
        uint32_t start = table[symbol];
        uint32_t end = table[symbol + 1];
        for (uint32_t slot = start; slot < end; slot++) {
            ranges[count] = symbol | (end - start - 1) << 8 | (slot - start) << 20;
            count++;
        }
    }
    return count;
}

static mvdz_error decode_raw_model(uint8_t *raw, uint32_t raw_len, const uint8_t *coded,
                                   uint32_t coded_len, const decoder_model *meta) {
    stream_decoder decoder;
    mvdz_error error = new_stream_decoder(meta, coded, coded_len, &decoder);
    if (error.kind != MVDZ_OK) {
        return error;
    }
    uint8_t previous = 0;
    for (uint32_t index = 0; index < raw_len; index++) {
        if (!decode_symbol(&decoder, previous, &raw[index])) {
            return corrupt("model");
        }
        previous = raw[index];
    }
    if (!is_done(&decoder)) {
        return corrupt("model");
    }
    return ok();
}

static uint32_t stream_alphabet_size(uint32_t stream) {
    return stream >= FIRST_NUMERIC_STREAM ? NUMERIC_ALPHABET_SIZE : STRUCTURAL_ALPHABET_SIZE;
}

/* read_word gives 0 past the end. */
static uint32_t read_word(stream_decoder *decoder) {
    uint32_t word = 0;
    bool is_inside = decoder->word_position <= decoder->rans_len &&
                     decoder->rans_len - decoder->word_position >= 2;
    if (is_inside) {
        word = le_uint16(decoder->rans + decoder->word_position);
    }
    decoder->word_position += 2;
    return word;
}

/* read_bits takes at most 31 bits: the bytes they span, then the bits in them. */
static uint32_t read_bits(bit_reader *reader, uint32_t count) {
    uint32_t first_byte = reader->position / 8;
    uint32_t bit_offset = reader->position % 8;
    uint64_t window = 0;
    for (uint32_t index = 0; 8 * index < bit_offset + count; index++) {
        if (first_byte + index < reader->len) {
            window |= (uint64_t)reader->data[first_byte + index] << (8 * index);
        }
    }
    reader->position += count;
    return (uint32_t)(window >> bit_offset) & ((UINT32_C(1) << count) - 1);
}

/* FORMAT.md 6.5: the bits end within the last byte. */
static bool bit_reader_is_done(const bit_reader *reader) {
    return (reader->position + 7) / 8 == reader->len;
}
