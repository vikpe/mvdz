/* Stream ids, FORMAT.md 3.2: the names there with a stream prefix. */
enum {
    /* Structural streams: rANS symbols, decoded by the walker. */
    STREAM_TIME = 0,
    STREAM_TYPE = 1,
    STREAM_COMMAND = 2,
    STREAM_PLAYER_FLAGS = 3,
    STREAM_PLAYER_FRAME = 4,
    STREAM_PLAYER_WEAPON_FRAME = 5,
    STREAM_ENTITY_BITS = 6,
    STREAM_ENTITY_MORE = 7,
    STREAM_ENTITY_ANGLE = 8,
    STREAM_STAT_INDEX = 9,
    STREAM_STAT_VALUE = 10,
    STREAM_SOUND_CHANNEL = 11,
    STREAM_SOUND_NUMBER = 12,
    STREAM_TEMP_ENTITY_TYPE = 13,
    STREAM_USERCMD = 14,
    STREAM_LZ_LITERAL = 15, /* literal bytes of the blob tokens */

    /* Numeric streams: rANS values, decoded by the walk and the blob tokens.
     * Origins and angles have one stream per axis: the id below plus the axis. */
    STREAM_PLAYER_ORIGIN = 16,
    STREAM_PLAYER_ANGLE = 19,
    STREAM_ENTITY_ORIGIN = 22,
    STREAM_SOUND_ORIGIN = 25,
    STREAM_TEMP_ENTITY_ORIGIN = 26,
    STREAM_PLAYER_FLOAT_ORIGIN = 27,
    STREAM_ENTITY_FLOAT_ORIGIN = 30,
    STREAM_USERCMD_ANGLE = 33,
    STREAM_LZ_LENGTH = 34,   /* match lengths of the blob tokens, 0 for a literal */
    STREAM_LZ_DISTANCE = 35, /* match distances of the blob tokens */

    /* Blob streams: plain bytes from the blob tokens, consumed by the walker. */
    STREAM_MASK = 36,
    STREAM_ESCAPE_LENGTH = 37,
    STREAM_HIDDEN_TAG = 38,
    STREAM_HIDDEN_LENGTH = 39,
    STREAM_HIDDEN_TYPE = 40,
    STREAM_PLAYER_NUMBER = 41,
    STREAM_PLAYER_MODEL = 42,
    STREAM_PLAYER_SKIN = 43,
    STREAM_PLAYER_EFFECTS = 44,
    STREAM_ENTITY_HEADER = 45,
    STREAM_ENTITY_GAP = 46,
    STREAM_ENTITY_MODEL = 47,
    STREAM_ENTITY_FRAME = 48,
    STREAM_ENTITY_MISC = 49,
    STREAM_SOUND_MISC = 50,
    STREAM_TEMP_ENTITY_MISC = 51,
    STREAM_RAW = 52,
    STREAM_TAIL = 53,
    STREAM_SET = 54,
    STREAM_NAILS = 55,
    STREAM_HIDDEN_MISC = 56,
    STREAM_SOUND_FLOAT = 57,
    STREAM_TEMP_ENTITY_FLOAT = 58,
    STREAM_GENERIC = 59, /* the id is 59 plus the command byte */

    FIRST_NUMERIC_STREAM = 16,
    FIRST_BLOB_STREAM = 36,
    STREAM_COUNT = 315
};
