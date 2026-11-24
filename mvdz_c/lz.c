/* The blob bytes of a segment as LZ77 tokens, FORMAT.md 6.6: a
 * literal byte, or a match that copies bytes already produced. Three streams
 * code them, with the models every other stream uses. */

static uint32_t lz_bucket_context(uint32_t value);

/* lz_expand decodes the blob_total blob bytes of a segment from its tokens;
 * decoders are the segment's, by stream id. */
static mvdz_error lz_expand(stream_decoder *decoders, uint8_t *blob_bytes, uint32_t blob_total) {
    uint32_t position = 0;
    uint32_t previous_len = 0;
    while (position < blob_total) {
        int32_t coded_len;
        bool is_decoded =
            decode_value(&decoders[STREAM_LZ_LENGTH], lz_bucket_context(previous_len), &coded_len);
        if (!is_decoded || coded_len < 0) {
            return corrupt("blob tokens");
        }
        uint32_t length = (uint32_t)coded_len;
        if (length == 0) {
            uint8_t previous_byte = position > 0 ? blob_bytes[position - 1] : 0;
            if (!decode_symbol(&decoders[STREAM_LZ_LITERAL], previous_byte,
                               &blob_bytes[position])) {
                return corrupt("blob tokens");
            }
            position++;
        } else {
            int32_t distance;
            is_decoded =
                decode_value(&decoders[STREAM_LZ_DISTANCE], lz_bucket_context(length), &distance);
            bool is_inside = is_decoded && distance >= 1 && (uint32_t)distance <= position &&
                             length <= blob_total - position;
            if (!is_inside) {
                return corrupt("blob tokens");
            }
            /* byte by byte: a match may run into the bytes it produces */
            for (uint32_t end = position + length; position < end; position++) {
                blob_bytes[position] = blob_bytes[position - (uint32_t)distance];
            }
        }
        previous_len = length;
    }
    return ok();
}

static uint32_t lz_bucket_context(uint32_t value) {
    enum { MAX_BUCKET = 63 };
    return min_uint32(residual_bucket((int32_t)value), MAX_BUCKET);
}
