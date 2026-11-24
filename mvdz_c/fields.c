/* byte_cursor reads little-endian fields from a block. A read past the end sets
 * is_short and yields zero; is_short is checked once, after a group of reads. */
typedef struct byte_cursor {
    const uint8_t *data;
    size_t len;
    size_t position;
    bool is_short;
} byte_cursor;

static uint16_t le_uint16(const uint8_t *field) {
    return (uint16_t)(field[0] | field[1] << 8);
}

static uint32_t le_uint32(const uint8_t *field) {
    return (uint32_t)field[0] | (uint32_t)field[1] << 8 | (uint32_t)field[2] << 16 |
           (uint32_t)field[3] << 24;
}

static uint64_t le_uint64(const uint8_t *field) {
    return (uint64_t)le_uint32(field) | (uint64_t)le_uint32(field + 4) << 32;
}

static void put_le_uint32(uint8_t *field, uint32_t value) {
    field[0] = (uint8_t)value;
    field[1] = (uint8_t)(value >> 8);
    field[2] = (uint8_t)(value >> 16);
    field[3] = (uint8_t)(value >> 24);
}

/* NULL if fewer than count bytes are left. */
static const uint8_t *read_bytes(byte_cursor *cursor, uint32_t count) {
    if (cursor->is_short || count > cursor->len - cursor->position) {
        cursor->is_short = true;
        return NULL;
    }
    const uint8_t *field = cursor->data + cursor->position;
    cursor->position += count;
    return field;
}

static uint32_t read_uint32(byte_cursor *cursor) {
    const uint8_t *field = read_bytes(cursor, 4);
    return field != NULL ? le_uint32(field) : 0;
}

static bool is_at_end(const byte_cursor *cursor) {
    return !cursor->is_short && cursor->position == cursor->len;
}
