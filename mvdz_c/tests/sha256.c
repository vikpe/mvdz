/* SHA-256 (FIPS 180-4) for the digests of the vectors; the library needs none. */

static uint32_t rotate_right(uint32_t value, uint32_t count);
static void sha256_block(uint32_t *state, const uint8_t *block);

static void sha256(const uint8_t *data, size_t len, uint8_t *digest) {
    enum { BLOCK_SIZE = 64, LEN_SIZE = 8 };
    uint32_t state[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                         0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    size_t position = 0;
    for (; len - position >= BLOCK_SIZE; position += BLOCK_SIZE) {
        sha256_block(state, data + position);
    }
    /* the rest, 0x80, zeros, then the bit length: one or two blocks */
    uint8_t padded[2 * BLOCK_SIZE] = {0};
    size_t rest_len = len - position;
    if (rest_len != 0) {
        memcpy(padded, data + position, rest_len);
    }
    padded[rest_len] = 0x80;
    size_t padded_len = rest_len + 1 + LEN_SIZE <= BLOCK_SIZE ? BLOCK_SIZE : 2 * BLOCK_SIZE;
    uint64_t bit_len = (uint64_t)len * 8;
    for (uint32_t index = 0; index < LEN_SIZE; index++) {
        padded[padded_len - 1 - index] = (uint8_t)(bit_len >> (8 * index));
    }
    for (size_t offset = 0; offset < padded_len; offset += BLOCK_SIZE) {
        sha256_block(state, padded + offset);
    }
    for (uint32_t index = 0; index < 8; index++) {
        digest[4 * index] = (uint8_t)(state[index] >> 24);
        digest[4 * index + 1] = (uint8_t)(state[index] >> 16);
        digest[4 * index + 2] = (uint8_t)(state[index] >> 8);
        digest[4 * index + 3] = (uint8_t)state[index];
    }
}

static uint32_t rotate_right(uint32_t value, uint32_t count) {
    return value >> count | value << (32 - count);
}

static void sha256_block(uint32_t *state, const uint8_t *block) {
    static const uint32_t round_constants[64] = {
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2};
    uint32_t schedule[64];
    for (uint32_t index = 0; index < 16; index++) {
        const uint8_t *word = block + 4 * index;
        schedule[index] = (uint32_t)word[0] << 24 | (uint32_t)word[1] << 16 |
                          (uint32_t)word[2] << 8 | (uint32_t)word[3];
    }
    for (uint32_t index = 16; index < 64; index++) {
        uint32_t early = schedule[index - 15];
        uint32_t late = schedule[index - 2];
        uint32_t early_sigma = rotate_right(early, 7) ^ rotate_right(early, 18) ^ early >> 3;
        uint32_t late_sigma = rotate_right(late, 17) ^ rotate_right(late, 19) ^ late >> 10;
        schedule[index] = schedule[index - 16] + early_sigma + schedule[index - 7] + late_sigma;
    }
    uint32_t working[8];
    memcpy(working, state, sizeof working);
    for (uint32_t index = 0; index < 64; index++) {
        uint32_t first = working[0];
        uint32_t fifth = working[4];
        uint32_t fifth_sigma =
            rotate_right(fifth, 6) ^ rotate_right(fifth, 11) ^ rotate_right(fifth, 25);
        uint32_t choice = (fifth & working[5]) ^ (~fifth & working[6]);
        uint32_t first_sigma =
            rotate_right(first, 2) ^ rotate_right(first, 13) ^ rotate_right(first, 22);
        uint32_t majority = (first & working[1]) ^ (first & working[2]) ^ (working[1] & working[2]);
        uint32_t mixed =
            working[7] + fifth_sigma + choice + round_constants[index] + schedule[index];
        memmove(working + 1, working, 7 * sizeof working[0]);
        working[4] += mixed;
        working[0] = mixed + first_sigma + majority;
    }
    for (uint32_t index = 0; index < 8; index++) {
        state[index] += working[index];
    }
}
