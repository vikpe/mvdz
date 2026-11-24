enum {
    PROTOCOL_VERSION = 28,
    PROTOCOL_VERSION_FTE = 0x58455446,   /* "FTEX" */
    PROTOCOL_VERSION_FTE_2 = 0x32455446, /* "FTE2" */
    PROTOCOL_VERSION_MVD_1 = 0x3144564D, /* "MVD1" */

    FTE_PEXT_TRANS = 1 << 3,
    FTE_PEXT_FLOAT_COORDS = 1 << 15,
    FTE_PEXT_COLOR_MOD = 1 << 19,
    MVD_PEXT_1_FLOAT_COORDS = 1 << 0,

    DEM_READ = 1,
    DEM_SET = 2,
    DEM_MULTIPLE = 3,
    DEM_SINGLE = 4,
    DEM_STATS = 5,
    DEM_ALL = 6,

    SVC_UPDATE_STAT = 3,
    SVC_SOUND = 6,
    SVC_PRINT = 8,
    SVC_STUFF_TEXT = 9,
    SVC_SERVER_DATA = 11,
    SVC_UPDATE_FRAGS = 14,
    SVC_DAMAGE = 19,
    SVC_TEMP_ENTITY = 23,
    SVC_UPDATE_PING = 36,
    SVC_UPDATE_STAT_LONG = 38,
    SVC_PLAYER_INFO = 42,
    SVC_NAILS = 43,
    SVC_PACKET_ENTITIES = 47,
    SVC_DELTA_PACKET_ENTITIES = 48,
    SVC_SET_INFO = 51,
    SVC_NAILS_2 = 54,

    SND_ATTENUATION = 1 << 14,
    SND_VOLUME = 1 << 15,

    TE_GUNSHOT = 2,
    TE_LIGHTNING_1 = 5,
    TE_LIGHTNING_2 = 6,
    TE_LIGHTNING_3 = 9,
    TE_BLOOD = 12,

    /* Bits of the entity word and its U_MORE_BITS byte. */
    U_ANGLE_1 = 1 << 0,
    U_ANGLE_3 = 1 << 1,
    U_MODEL = 1 << 2,
    U_COLOR_MAP = 1 << 3,
    U_SKIN = 1 << 4,
    U_EFFECTS = 1 << 5,
    U_FTE_EVEN_MORE = 1 << 7,
    U_ORIGIN_1 = 1 << 9,
    U_ORIGIN_2 = 1 << 10,
    U_ORIGIN_3 = 1 << 11,
    U_ANGLE_2 = 1 << 12,
    U_FRAME = 1 << 13,
    U_MORE_BITS = 1 << 15,

    /* Bits of the FTE extension bytes. */
    U_FTE_SCALE = 1 << 0,
    U_FTE_TRANS = 1 << 1,
    U_FTE_FATNESS = 1 << 2,
    U_FTE_MODEL_DBL = 1 << 3,
    U_FTE_ENTITY_DBL = 1 << 5,
    U_FTE_ENTITY_DBL_2 = 1 << 6,
    U_FTE_YET_MORE = 1 << 7,
    U_FTE_COLOR_MOD = 1 << 10,

    DF_ORIGIN = 1 << 0,
    DF_ANGLES = 1 << 3,
    DF_EFFECTS = 1 << 6,
    DF_SKIN_NUM = 1 << 7,
    DF_DEAD = 1 << 8,
    DF_GIB = 1 << 9,
    DF_WEAPON_FRAME = 1 << 10,
    DF_MODEL = 1 << 11
};

static const uint32_t entity_origin_bits[3] = {U_ORIGIN_1, U_ORIGIN_2, U_ORIGIN_3};
static const uint32_t entity_angle_bits[3] = {U_ANGLE_1, U_ANGLE_2, U_ANGLE_3};

/* protocol_state is the protocol extension state, FORMAT.md 4.1. */
typedef struct protocol_state {
    uint32_t fte;
    uint32_t fte_2; /* carried in the transform header; no rule reads it */
    uint32_t mvd_1;
} protocol_state;

static bool has_fte_float_coords(const protocol_state *protocol) {
    return (protocol->fte & FTE_PEXT_FLOAT_COORDS) != 0;
}

static bool has_float_coords(const protocol_state *protocol) {
    return has_fte_float_coords(protocol) || (protocol->mvd_1 & MVD_PEXT_1_FLOAT_COORDS) != 0;
}

/* A server data starts a match: an extension it does not announce is off. */
static void observe_server_data(protocol_state *protocol, const uint8_t *body, uint32_t body_len) {
    protocol->fte = 0;
    protocol->fte_2 = 0;
    protocol->mvd_1 = 0;
    byte_cursor fields = {body, body_len, 0, false};
    for (;;) {
        uint32_t version = read_uint32(&fields);
        if (fields.is_short || version == PROTOCOL_VERSION) {
            return;
        }
        uint32_t extensions = read_uint32(&fields);
        if (fields.is_short) {
            return;
        }
        if (version == PROTOCOL_VERSION_FTE) {
            protocol->fte = extensions;
        } else if (version == PROTOCOL_VERSION_FTE_2) {
            protocol->fte_2 = extensions;
        } else if (version == PROTOCOL_VERSION_MVD_1) {
            protocol->mvd_1 = extensions;
        }
    }
}
