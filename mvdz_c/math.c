/* The exact arithmetic of the format, FORMAT.md: every encoder and decoder
 * computes these values bit for bit, in any language. Integers throughout. */

enum {
    STEPS_PER_OCTAVE = 2,
    /* The largest quantized count. */
    MAX_QUANTIZED = 65
};

static uint32_t min_uint32(uint32_t first, uint32_t second) {
    return first < second ? first : second;
}

static int32_t min_int32(int32_t first, int32_t second) {
    return first < second ? first : second;
}

static uint32_t bit_len32(uint32_t value) {
    uint32_t len = 0;
    while (value != 0) {
        value >>= 1;
        len++;
    }
    return len;
}

/* residual_bucket is q(v) of FORMAT.md 1: 0 for zero, else twice the
 * bit length of the magnitude, less one, plus one for a negative value. */
static uint32_t residual_bucket(int32_t value) {
    if (value == 0) {
        return 0;
    }
    uint32_t magnitude = (uint32_t)value;
    uint32_t sign = 0;
    if (value < 0) {
        magnitude = 0 - magnitude;
        sign = 1;
    }
    return 2 * bit_len32(magnitude) - 1 + sign;
}

/* FORMAT.md 1 */
static uint32_t byte_residual_bucket(uint8_t residual) {
    return residual_bucket((int8_t)residual);
}

/* div_round divides by a positive divisor; halves round away from zero. */
static int32_t div_round(int32_t dividend, int32_t divisor) {
    uint32_t magnitude = (uint32_t)dividend;
    if (dividend < 0) {
        magnitude = 0 - magnitude;
    }
    int32_t quotient = (int32_t)((magnitude + (uint32_t)divisor / 2) / (uint32_t)divisor);
    return dividend < 0 ? -quotient : quotient;
}

/* grid_to_float32_bits is the float bits of grid * 45/8192, FORMAT.md 5.6. It is
 * exact: |grid| * 45 has at most 21 bits. */
static uint32_t grid_to_float32_bits(uint16_t grid) {
    int32_t value = (int16_t)grid;
    if (value == 0) {
        return 0;
    }
    uint32_t sign = 0;
    if (value < 0) {
        sign = 1;
        value = -value;
    }
    uint32_t magnitude = (uint32_t)value * 45;
    uint32_t width = bit_len32(magnitude);
    /* magnitude * 2^-13 with its highest bit as the implicit one: 127 + width - 1 - 13 */
    uint32_t exponent = width + 113;
    uint32_t fraction = (magnitude << (24 - width)) & 0x7fffff;
    return sign << 31 | exponent << 23 | fraction;
}

/* quantized_weight is 2^((quantized-1)/2) in 16.16 fixed point, FORMAT.md 6.3. */
static uint64_t quantized_weight(uint32_t quantized) {
    static const uint64_t step_powers[STEPS_PER_OCTAVE] = {65536, 92682};
    if (quantized == 0) {
        return 0;
    }
    uint32_t step = quantized - 1;
    return step_powers[step % STEPS_PER_OCTAVE] << (step / STEPS_PER_OCTAVE);
}

/* normalize scales the weights to frequencies that sum to total, every
 * present symbol getting at least 1, FORMAT.md 6.3. */
static void normalize(const uint64_t *weights, uint32_t count, int32_t total,
                      int32_t *frequencies) {
    uint64_t weight_total = 0; /* at most 256 weights of at most 2^48 */
    for (uint32_t symbol = 0; symbol < count; symbol++) {
        weight_total += weights[symbol];
        frequencies[symbol] = 0;
    }
    if (weight_total == 0) {
        return;
    }
    int32_t sum = 0;
    for (uint32_t symbol = 0; symbol < count; symbol++) {
        if (weights[symbol] == 0) {
            continue;
        }
        int32_t scaled = (int32_t)(weights[symbol] * (uint64_t)total / weight_total);
        frequencies[symbol] = scaled > 1 ? scaled : 1;
        sum += frequencies[symbol];
    }
    int32_t difference = total - sum;
    while (difference != 0) {
        uint32_t largest = 0;
        for (uint32_t symbol = 0; symbol < count; symbol++) {
            if (frequencies[symbol] > frequencies[largest]) {
                largest = symbol;
            }
        }
        if (difference > 0) {
            frequencies[largest] += difference;
            break;
        }
        int32_t take = min_int32(
            -difference, min_int32(frequencies[largest] - 1, (frequencies[largest] + 1) / 2));
        if (take <= 0) {
            break;
        }
        frequencies[largest] -= take;
        difference += take;
    }
}
