typedef struct player_state {
    uint16_t flags;
    uint8_t frame;
    uint8_t weapon_frame;
    axis_track origins[3]; /* FORMAT.md 5.3 */
    axis_track angles[3];  /* FORMAT.md 5.4 */
    frame_times frame_times;
    float_track float_origins[3]; /* FORMAT.md 5.2 */
} player_state;

typedef struct usercmd_state {
    uint8_t drop;
    uint8_t msec;
    uint8_t grid;
    uint8_t buttons;
    uint8_t impulse;
    uint16_t moves[3];
    axis_track angles[3]; /* FORMAT.md 5.6 */
} usercmd_state;

typedef struct entity_state {
    uint8_t angles[3];
    uint8_t angle_residuals[3];
    uint8_t frame;
    uint8_t model;
    uint8_t bits;
    uint32_t last_packet;
    axis_track origins[3];        /* FORMAT.md 5.5 */
    float_track float_origins[3]; /* FORMAT.md 5.2 */
} entity_state;

/* segment_walker walks a segment, FORMAT.md 4 and 5, writing it. */
typedef struct segment_walker {
    uint8_t *segment;
    uint32_t segment_len;
    uint32_t position;
    segment_streams *streams;
    protocol_state protocol;
    bool is_failed; /* after it every primitive does nothing and every loop stops */

    /* FORMAT.md 4.3 */
    uint32_t time;
    uint8_t target;
    uint8_t last_type;
    uint8_t last_time_byte;
    uint8_t previous_command;
    uint8_t first_command_by_type[8];
    uint8_t stat_previous_index;
    uint8_t stat_previous_residual;
    uint8_t last_header;
    uint8_t previous_temp_entity;
    player_state players[256];
    uint8_t usercmd_previous_players[2];
    usercmd_state usercmds[256];
    entity_state entities[2048];
    uint8_t stats[32][256];
    uint32_t packet_number;
    uint16_t temp_entity_last[3];
} segment_walker;

/* A value and the residual to keep as state, FORMAT.md 5.1. */
typedef struct coded_uint16 {
    uint16_t value;
    int32_t residual;
} coded_uint16;

typedef struct coded_uint32 {
    uint32_t value;
    int32_t residual;
} coded_uint32;

static void walker_run(segment_walker *walker, uint32_t tail);
static void walker_frame(segment_walker *walker);
static void length_prefixed(segment_walker *walker, uint8_t frame_type, bool is_hidden);
static void walker_escape(segment_walker *walker);
static void walker_payload(segment_walker *walker, uint8_t frame_type);
static void copy_command_body(segment_walker *walker, uint32_t stream, uint8_t command);
static void walker_command(segment_walker *walker, uint8_t command);
static void update_stat(segment_walker *walker);
static void player_info(segment_walker *walker);
static void packet_entities(segment_walker *walker, bool is_delta);
static uint32_t entity_gap(segment_walker *walker);
static void entity_delta(segment_walker *walker, uint32_t word);
static void entity_origin(segment_walker *walker, entity_state *entity, uint32_t axis, bool is_sent,
                          bool was_in_previous_packet);
static void entity_angle(segment_walker *walker, entity_state *entity, uint32_t axis);
static void sound(segment_walker *walker);
static void sound_origin(segment_walker *walker, const axis_track *origins, uint32_t kind,
                         uint8_t sound_number);
static void temp_entity(segment_walker *walker);
static void temp_entity_origins(segment_walker *walker, uint8_t kind, bool is_beam,
                                const uint16_t *beam_origin);
static void hidden_payload(segment_walker *walker);
static void usercmd(segment_walker *walker);
static void walker_fail(segment_walker *walker);
static void walker_write(segment_walker *walker, uint8_t value);
static void walker_write_uint16(segment_walker *walker, uint16_t value);
static void walker_write_uint32(segment_walker *walker, uint32_t value);
static uint8_t walker_symbol(segment_walker *walker, uint32_t stream, uint32_t context);
static int32_t walker_residual(segment_walker *walker, uint32_t stream, uint32_t context);
static uint8_t walker_blob_byte(segment_walker *walker, uint32_t stream);
static uint8_t code_symbol(segment_walker *walker, uint32_t stream, uint32_t context);
static uint8_t code_symbol_residual(segment_walker *walker, uint32_t stream, uint32_t context,
                                    uint8_t prediction);
static uint8_t code_blob_byte(segment_walker *walker, uint32_t stream);
static uint8_t code_blob_residual(segment_walker *walker, uint32_t stream, uint8_t prediction);
static uint32_t code_blob_bytes(segment_walker *walker, uint32_t stream, uint32_t count);
static void copy_blob(segment_walker *walker, uint32_t stream, uint32_t count);
static coded_uint16 code_uint16(segment_walker *walker, uint32_t stream, uint32_t context,
                                uint16_t prediction);
static coded_uint32 code_uint32(segment_walker *walker, uint32_t stream, uint32_t context,
                                uint32_t prediction);
static void float_origin(segment_walker *walker, uint32_t stream, uint32_t axis, bool is_sent,
                         float_track *track);
static uint16_t player_info_flags(segment_walker *walker, uint16_t previous);
static void player_origins_and_angles(segment_walker *walker, player_state *player, uint16_t flags);
static void usercmd_grid_angle(segment_walker *walker, uint32_t axis, axis_track *track);
static uint32_t command_class(uint8_t command);

/* walk writes the segment from its streams; the tail is what the TAIL stream holds. */
static mvdz_error walk(uint8_t *segment, uint32_t segment_len, segment_streams *streams) {
    segment_walker *walker = calloc(1, sizeof *walker);
    if (walker == NULL) {
        return out_of_memory();
    }
    walker->segment = segment;
    walker->segment_len = segment_len;
    walker->streams = streams;
    walker->protocol = streams->protocol;
    walker->stat_previous_index = 255;
    walker->packet_number = 1;
    walker_run(walker, streams->blobs[STREAM_TAIL].len);
    bool is_failed = walker->is_failed;
    free(walker);
    return is_failed ? corrupt("structure") : ok();
}

/* FORMAT.md 4.4 */
static void walker_run(segment_walker *walker, uint32_t tail) {
    if (tail > walker->segment_len) {
        walker_fail(walker);
        return;
    }
    uint32_t stop = walker->segment_len - tail;
    while (!walker->is_failed && walker->position < stop) {
        walker_frame(walker);
    }
    if (walker->position != stop) {
        walker_fail(walker);
    }
    copy_blob(walker, STREAM_TAIL, tail);
}

static void walker_frame(segment_walker *walker) {
    uint32_t last_type = walker->last_type;
    uint8_t time_byte =
        code_symbol(walker, STREAM_TIME, last_type | (uint32_t)walker->last_time_byte << 8);
    walker->last_time_byte = time_byte;
    walker->time += time_byte;
    uint8_t frame_type =
        code_symbol(walker, STREAM_TYPE, last_type | min_uint32(time_byte, 32) << 8);
    walker->last_type = frame_type;
    walker->target = frame_type >> 3;

    switch (frame_type & 7) {
    case DEM_SET:
        copy_blob(walker, STREAM_SET, 8);
        break;
    case DEM_MULTIPLE: {
        uint32_t mask = code_blob_bytes(walker, STREAM_MASK, 4);
        length_prefixed(walker, frame_type, mask == 0);
        break;
    }
    case DEM_READ:
    case DEM_SINGLE:
    case DEM_STATS:
    case DEM_ALL:
        length_prefixed(walker, frame_type, false);
        break;
    default:
        walker_fail(walker);
    }
}

/* The payload length is written once the payload is. */
static void length_prefixed(segment_walker *walker, uint8_t frame_type, bool is_hidden) {
    if (walker->is_failed) {
        return;
    }
    uint32_t offset = walker->position;
    if (walker->segment_len - offset < 4) {
        walker_fail(walker);
        return;
    }
    walker->position += 4;
    if (is_hidden) {
        hidden_payload(walker);
    } else {
        walker_payload(walker, frame_type);
    }
    if (walker->is_failed) {
        return;
    }
    put_le_uint32(walker->segment + offset, walker->position - offset - 4);
}

/* walker_escape copies the rest of a payload, which the walk does not model. */
static void walker_escape(segment_walker *walker) {
    uint32_t escape_len = 0;
    for (uint32_t index = 0; index < 4; index++) {
        escape_len |= (uint32_t)walker_blob_byte(walker, STREAM_ESCAPE_LENGTH) << (8 * index);
    }
    copy_blob(walker, STREAM_RAW, escape_len);
}

/* FORMAT.md 4.5 */
static void walker_payload(segment_walker *walker, uint8_t frame_type) {
    walker->previous_command = 0;
    walker->stat_previous_index = 255;
    walker->stat_previous_residual = 0;
    uint32_t type_index = frame_type & 7;
    uint32_t previous_first_class = command_class(walker->first_command_by_type[type_index]);
    walker->first_command_by_type[type_index] = 255; /* stays for an empty payload */

    for (uint32_t command_index = 0; !walker->is_failed; command_index++) {
        if (command_index == 1) {
            walker->first_command_by_type[type_index] = walker->previous_command;
        }
        uint32_t context;
        uint32_t previous_class = command_class(walker->previous_command);
        if (previous_class == 15) {
            context = 0x8000 | walker->previous_command | type_index << 8 |
                      min_uint32(command_index, 7) << 11;
        } else {
            context = previous_class | min_uint32(command_index, 15) << 4 |
                      previous_first_class << 8 | type_index << 12;
        }
        uint8_t command = walker_symbol(walker, STREAM_COMMAND, context);
        if (command == 255) {
            return;
        }
        if (command == 0) {
            walker_escape(walker);
        } else {
            walker_write(walker, command);
            walker_command(walker, command);
        }
        walker->previous_command = command;
    }
}

/* FORMAT.md 4.2 */
static void copy_command_body(segment_walker *walker, uint32_t stream, uint8_t command) {
    enum { LENGTH_CONTEXTS = 0xc000 };
    uint32_t body_len = 0;
    for (uint32_t index = 0; index < 3; index++) {
        uint32_t context = LENGTH_CONTEXTS | index << 8 | command;
        body_len |= (uint32_t)walker_symbol(walker, STREAM_COMMAND, context) << (8 * index);
    }
    copy_blob(walker, stream, body_len);
}

/* FORMAT.md 4.6 */
static void walker_command(segment_walker *walker, uint8_t command) {
    switch (command) {
    case SVC_PLAYER_INFO:
        player_info(walker);
        break;
    case SVC_PACKET_ENTITIES:
    case SVC_DELTA_PACKET_ENTITIES:
        packet_entities(walker, command == SVC_DELTA_PACKET_ENTITIES);
        break;
    case SVC_UPDATE_STAT:
        update_stat(walker);
        break;
    case SVC_SOUND:
        sound(walker);
        break;
    case SVC_TEMP_ENTITY:
        temp_entity(walker);
        break;
    case SVC_NAILS:
    case SVC_NAILS_2:
        copy_command_body(walker, STREAM_NAILS, command);
        break;
    case SVC_SERVER_DATA: {
        uint32_t start = walker->position;
        copy_command_body(walker, STREAM_GENERIC + SVC_SERVER_DATA, SVC_SERVER_DATA);
        observe_server_data(&walker->protocol, walker->segment + start, walker->position - start);
        break;
    }
    default:
        copy_command_body(walker, STREAM_GENERIC + (uint32_t)command, command);
    }
}

static void update_stat(segment_walker *walker) {
    uint8_t *stats = walker->stats[walker->target & 31];
    uint8_t index =
        code_symbol(walker, STREAM_STAT_INDEX,
                    walker->stat_previous_index | (uint32_t)walker->previous_command << 8);
    uint8_t previous = stats[index];
    uint8_t value = code_symbol_residual(
        walker, STREAM_STAT_VALUE, index | (uint32_t)walker->stat_previous_residual << 8, previous);
    walker->stat_previous_index = index;
    walker->stat_previous_residual = (uint8_t)(value - previous);
    stats[index] = value;
}

/* FORMAT.md 4.7 */
static void player_info(segment_walker *walker) {
    uint8_t number = code_blob_byte(walker, STREAM_PLAYER_NUMBER);
    player_state *player = &walker->players[number];
    uint16_t flags = player_info_flags(walker, player->flags);
    uint32_t frame_context = player->frame | (flags & (DF_DEAD | DF_GIB));
    uint8_t frame = code_symbol_residual(walker, STREAM_PLAYER_FRAME, frame_context, player->frame);

    player_origins_and_angles(walker, player, flags);

    if ((flags & DF_MODEL) != 0) {
        code_blob_byte(walker, STREAM_PLAYER_MODEL);
    }
    if ((flags & DF_SKIN_NUM) != 0) {
        code_blob_byte(walker, STREAM_PLAYER_SKIN);
    }
    if ((flags & DF_EFFECTS) != 0) {
        code_blob_byte(walker, STREAM_PLAYER_EFFECTS);
    }
    if ((flags & DF_WEAPON_FRAME) != 0) {
        player->weapon_frame = code_symbol_residual(walker, STREAM_PLAYER_WEAPON_FRAME,
                                                    player->weapon_frame, player->weapon_frame);
    }
    player->flags = flags;
    player->frame = frame;
}

/* FORMAT.md 4.8 */
static void packet_entities(segment_walker *walker, bool is_delta) {
    if (is_delta) {
        walker->last_header = code_blob_residual(walker, STREAM_ENTITY_HEADER, walker->last_header);
    }
    walker->packet_number++;

    uint32_t previous_number = 511; /* the number before entity 0 */
    while (!walker->is_failed) {
        uint32_t gap = entity_gap(walker);
        if (gap == 0) {
            walker_write_uint16(walker, 0);
            break;
        }
        uint32_t number = (previous_number + gap) & 511;
        entity_state *entity = &walker->entities[number];
        uint32_t bits_context = entity->bits | (uint32_t)entity->model << 7;
        uint32_t bits = walker_symbol(walker, STREAM_ENTITY_BITS, bits_context) ^ entity->bits;
        uint32_t word = bits << 9 | number;
        if (word == 0) {
            walker_fail(walker);
            break;
        }
        walker_write_uint16(walker, (uint16_t)word);
        previous_number = number;
        entity->bits = (uint8_t)bits;
        entity_delta(walker, word);
    }
}

static uint32_t entity_gap(segment_walker *walker) {
    uint32_t first = walker_blob_byte(walker, STREAM_ENTITY_GAP);
    if (first != 255) {
        return first;
    }
    uint32_t low = walker_blob_byte(walker, STREAM_ENTITY_GAP);
    uint32_t high = walker_blob_byte(walker, STREAM_ENTITY_GAP);
    return low | high << 8;
}

static void entity_delta(segment_walker *walker, uint32_t word) {
    uint32_t bits = word & ~UINT32_C(511);
    uint32_t number = word & 511;
    if ((bits & U_MORE_BITS) != 0) {
        uint32_t more_context = bits >> 9 | (uint32_t)walker->entities[number].model << 8;
        bits |= code_symbol(walker, STREAM_ENTITY_MORE, more_context);
    }
    uint32_t more = 0;
    if ((bits & U_FTE_EVEN_MORE) != 0 && walker->protocol.fte != 0) {
        more = code_symbol(walker, STREAM_ENTITY_MORE, 0x8000);
        if ((more & U_FTE_YET_MORE) != 0) {
            more |= (uint32_t)code_symbol(walker, STREAM_ENTITY_MORE, 0x8001) << 8;
        }
    }
    if ((more & U_FTE_ENTITY_DBL) != 0) {
        number += 512;
    }
    if ((more & U_FTE_ENTITY_DBL_2) != 0) {
        number += 1024;
    }
    entity_state *entity = &walker->entities[number];
    bool was_in_previous_packet = entity->last_packet == walker->packet_number - 1;
    entity->last_packet = walker->packet_number;

    if ((bits & U_MODEL) != 0) {
        entity->model = code_blob_byte(walker, STREAM_ENTITY_MODEL);
    } else if ((more & U_FTE_MODEL_DBL) != 0) {
        code_blob_bytes(walker, STREAM_ENTITY_MODEL, 2);
    }
    if ((bits & U_FRAME) != 0) {
        entity->frame = code_blob_residual(walker, STREAM_ENTITY_FRAME, entity->frame);
    }
    static const uint32_t misc_bits[3] = {U_COLOR_MAP, U_SKIN, U_EFFECTS};
    for (uint32_t index = 0; index < 3; index++) {
        if ((bits & misc_bits[index]) != 0) {
            code_blob_byte(walker, STREAM_ENTITY_MISC);
        }
    }
    for (uint32_t axis = 0; axis < 3; axis++) {
        bool is_origin_sent = (bits & entity_origin_bits[axis]) != 0;
        entity_origin(walker, entity, axis, is_origin_sent, was_in_previous_packet);
        if ((bits & entity_angle_bits[axis]) != 0) {
            entity_angle(walker, entity, axis);
        }
    }

    if ((more & U_FTE_TRANS) != 0 && (walker->protocol.fte & FTE_PEXT_TRANS) != 0) {
        code_blob_byte(walker, STREAM_ENTITY_MISC);
    }
    if ((more & U_FTE_SCALE) != 0) {
        code_blob_byte(walker, STREAM_ENTITY_MISC);
    }
    if ((more & U_FTE_FATNESS) != 0) {
        code_blob_byte(walker, STREAM_ENTITY_MISC);
    }
    if ((more & U_FTE_COLOR_MOD) != 0 && (walker->protocol.fte & FTE_PEXT_COLOR_MOD) != 0) {
        code_blob_bytes(walker, STREAM_ENTITY_MISC, 3);
    }
}

/* FORMAT.md 5.5 */
static void entity_origin(segment_walker *walker, entity_state *entity, uint32_t axis, bool is_sent,
                          bool was_in_previous_packet) {
    if (has_float_coords(&walker->protocol)) {
        float_origin(walker, STREAM_ENTITY_FLOAT_ORIGIN + axis, axis, is_sent,
                     &entity->float_origins[axis]);
        return;
    }
    axis_track *track = &entity->origins[axis];
    if (!is_sent) {
        axis_track_hold(track);
        return;
    }
    uint32_t context = min_uint32(residual_bucket(track->residual), 17) |
                       min_uint32(residual_bucket(axis_track_velocity(track)), 17) << 5;
    if (was_in_previous_packet) {
        context |= 1 << 10;
    }
    coded_uint16 coded =
        code_uint16(walker, STREAM_ENTITY_ORIGIN + axis, context, axis_track_predict_linear(track));
    axis_track_push(track, coded.value, coded.residual);
}

static void entity_angle(segment_walker *walker, entity_state *entity, uint32_t axis) {
    uint32_t model_context = (uint32_t)entity->model << 6;
    if (has_fte_float_coords(&walker->protocol)) {
        code_symbol(walker, STREAM_ENTITY_ANGLE, 3 | model_context);
        code_symbol(walker, STREAM_ENTITY_ANGLE, 3 | model_context | 1 << 14);
        return;
    }
    uint32_t bucket = min_uint32(byte_residual_bucket(entity->angle_residuals[axis]), 15);
    uint8_t previous = entity->angles[axis];
    uint8_t angle = code_symbol_residual(walker, STREAM_ENTITY_ANGLE,
                                         axis | bucket << 2 | model_context, previous);
    entity->angle_residuals[axis] = (uint8_t)(angle - previous);
    entity->angles[axis] = angle;
}

/* FORMAT.md 4.9 */
static void sound(segment_walker *walker) {
    uint32_t low = code_symbol(walker, STREAM_SOUND_CHANNEL, walker->previous_command);
    uint32_t high = code_symbol(walker, STREAM_SOUND_CHANNEL, 0x100 | low);
    uint32_t channel = low | high << 8;
    if ((channel & SND_VOLUME) != 0) {
        code_blob_byte(walker, STREAM_SOUND_MISC);
    }
    if ((channel & SND_ATTENUATION) != 0) {
        code_blob_byte(walker, STREAM_SOUND_MISC);
    }
    uint32_t entity = (channel >> 3) & 1023;
    uint8_t sound_number =
        code_symbol(walker, STREAM_SOUND_NUMBER, (channel & 7) | min_uint32(entity, 33) << 3);
    if (has_float_coords(&walker->protocol)) {
        copy_blob(walker, STREAM_SOUND_FLOAT, 12);
        return;
    }
    bool is_player = 1 <= entity && entity <= 32;
    if (is_player) {
        sound_origin(walker, walker->players[entity - 1].origins, 1, sound_number);
    } else {
        sound_origin(walker, walker->entities[entity].origins, 2, sound_number);
    }
}

/* kind: 1 for a player's origin, 2 for an entity's. */
static void sound_origin(segment_walker *walker, const axis_track *origins, uint32_t kind,
                         uint8_t sound_number) {
    uint16_t origin[3];
    last_values(origins, origin);
    for (uint32_t axis = 0; axis < 3; axis++) {
        uint32_t context = axis | kind << 2 | (uint32_t)sound_number << 4;
        code_uint16(walker, STREAM_SOUND_ORIGIN, context, origin[axis]);
    }
}

/* FORMAT.md 4.10 */
static void temp_entity(segment_walker *walker) {
    uint32_t context = walker->previous_temp_entity;
    if (walker->previous_command == SVC_TEMP_ENTITY) {
        context |= 1 << 8;
    }
    uint8_t kind = code_symbol(walker, STREAM_TEMP_ENTITY_TYPE, context);
    walker->previous_temp_entity = kind;

    uint16_t beam_origin[3] = {0, 0, 0}; /* of the player a beam starts at */
    bool is_beam = false;
    switch (kind) {
    case TE_LIGHTNING_1:
    case TE_LIGHTNING_2:
    case TE_LIGHTNING_3: {
        uint32_t entity = code_blob_bytes(walker, STREAM_TEMP_ENTITY_MISC, 2);
        bool is_player = 1 <= entity && entity <= 32;
        if (is_player) {
            last_values(walker->players[entity - 1].origins, beam_origin);
        }
        is_beam = true;
        break;
    }
    case TE_GUNSHOT:
    case TE_BLOOD:
        code_blob_byte(walker, STREAM_TEMP_ENTITY_MISC);
        break;
    default:
        break;
    }
    if (has_float_coords(&walker->protocol)) {
        uint32_t coord_count = is_beam ? 6 : 3;
        copy_blob(walker, STREAM_TEMP_ENTITY_FLOAT, coord_count * 4);
        return;
    }
    temp_entity_origins(walker, kind, is_beam, beam_origin);
}

/* FORMAT.md 5.6 */
static void temp_entity_origins(segment_walker *walker, uint8_t kind, bool is_beam,
                                const uint16_t *beam_origin) {
    enum { POINT = 0, BEAM_START = 1, BEAM_END = 2 };
    uint32_t kind_context = (uint32_t)kind << 4;
    uint16_t *last = walker->temp_entity_last;
    if (!is_beam) {
        for (uint32_t axis = 0; axis < 3; axis++) {
            uint32_t context = axis | POINT << 2 | kind_context;
            last[axis] = code_uint16(walker, STREAM_TEMP_ENTITY_ORIGIN, context, last[axis]).value;
        }
        return;
    }
    uint16_t start[3];
    for (uint32_t axis = 0; axis < 3; axis++) {
        uint32_t context = axis | BEAM_START << 2 | kind_context;
        start[axis] =
            code_uint16(walker, STREAM_TEMP_ENTITY_ORIGIN, context, beam_origin[axis]).value;
    }
    for (uint32_t axis = 0; axis < 3; axis++) {
        uint32_t context = axis | BEAM_END << 2 | kind_context;
        last[axis] = code_uint16(walker, STREAM_TEMP_ENTITY_ORIGIN, context, start[axis]).value;
    }
}

/* FORMAT.md 4.11 */
static void hidden_payload(segment_walker *walker) {
    enum { MVD_HIDDEN_USERCMD = 1, HIDDEN_USERCMD_SIZE = 23 };
    enum { TAG_ESCAPE = 0, TAG_BLOCK = 1, TAG_END = 2 };
    while (!walker->is_failed) {
        uint8_t tag = walker_blob_byte(walker, STREAM_HIDDEN_TAG);
        if (tag == TAG_ESCAPE) {
            walker_escape(walker);
            continue;
        }
        if (tag == TAG_END) {
            return;
        }
        if (tag != TAG_BLOCK) {
            walker_fail(walker);
            return;
        }
        uint32_t block_len = code_blob_bytes(walker, STREAM_HIDDEN_LENGTH, 4);
        uint32_t block_type = code_blob_bytes(walker, STREAM_HIDDEN_TYPE, 2);
        if (block_type == MVD_HIDDEN_USERCMD && block_len == HIDDEN_USERCMD_SIZE) {
            usercmd(walker);
        } else {
            copy_blob(walker, STREAM_HIDDEN_MISC, block_len);
        }
    }
}

static void usercmd(segment_walker *walker) {
    uint8_t *previous_players = walker->usercmd_previous_players;
    uint32_t player_context =
        min_uint32(previous_players[0], 31) | min_uint32(previous_players[1], 31) << 5;
    uint8_t player = code_symbol(walker, STREAM_USERCMD, player_context);
    previous_players[1] = previous_players[0];
    previous_players[0] = player;

    usercmd_state *state = &walker->usercmds[player];
    state->drop = code_symbol(walker, STREAM_USERCMD, 1 << 12 | state->drop);
    state->msec = code_symbol(walker, STREAM_USERCMD, 2 << 12 | state->msec);
    uint8_t grid = walker_symbol(walker, STREAM_USERCMD, 3 << 12 | state->grid);
    if (grid > 7) {
        walker_fail(walker);
        return;
    }
    state->grid = grid;

    for (uint32_t axis = 0; axis < 3; axis++) {
        bool is_on_grid = (grid & (1 << axis)) != 0;
        if (is_on_grid) {
            usercmd_grid_angle(walker, axis, &state->angles[axis]);
            continue;
        }
        for (uint32_t byte_index = 0; byte_index < 4; byte_index++) {
            code_symbol(walker, STREAM_USERCMD, 4 << 12 | axis << 2 | byte_index);
        }
    }

    for (uint32_t axis = 0; axis < 3; axis++) {
        uint32_t low =
            code_symbol(walker, STREAM_USERCMD, 5 << 12 | axis << 8 | (state->moves[axis] & 0xff));
        uint32_t high = code_symbol(walker, STREAM_USERCMD, 6 << 12 | axis << 8 | low);
        state->moves[axis] = (uint16_t)(low | high << 8);
    }
    state->buttons = code_symbol(walker, STREAM_USERCMD, 7 << 12 | state->buttons);
    state->impulse = code_symbol(walker, STREAM_USERCMD, 8 << 12 | state->impulse);
}

/* Walker primitives, FORMAT.md 4. */

static void walker_fail(segment_walker *walker) {
    walker->is_failed = true;
}

static void walker_write(segment_walker *walker, uint8_t value) {
    if (walker->is_failed) {
        return;
    }
    if (walker->position >= walker->segment_len) {
        walker_fail(walker);
        return;
    }
    walker->segment[walker->position] = value;
    walker->position++;
}

static void walker_write_uint16(segment_walker *walker, uint16_t value) {
    walker_write(walker, (uint8_t)value);
    walker_write(walker, (uint8_t)(value >> 8));
}

static void walker_write_uint32(segment_walker *walker, uint32_t value) {
    walker_write_uint16(walker, (uint16_t)value);
    walker_write_uint16(walker, (uint16_t)(value >> 16));
}

/* walker_symbol is 255 after a failure. */
static uint8_t walker_symbol(segment_walker *walker, uint32_t stream, uint32_t context) {
    if (walker->is_failed) {
        return 255;
    }
    uint8_t symbol;
    if (!decode_symbol(&walker->streams->decoders[stream], context, &symbol)) {
        walker_fail(walker);
        return 255;
    }
    return symbol;
}

static int32_t walker_residual(segment_walker *walker, uint32_t stream, uint32_t context) {
    if (walker->is_failed) {
        return 0;
    }
    int32_t residual;
    if (!decode_value(&walker->streams->decoders[stream], context, &residual)) {
        walker_fail(walker);
        return 0;
    }
    return residual;
}

static uint8_t walker_blob_byte(segment_walker *walker, uint32_t stream) {
    if (walker->is_failed) {
        return 0;
    }
    uint8_t value;
    if (!read_blob_bytes(walker->streams, stream, &value, 1)) {
        walker_fail(walker);
        return 0;
    }
    return value;
}

static uint8_t code_symbol(segment_walker *walker, uint32_t stream, uint32_t context) {
    uint8_t value = walker_symbol(walker, stream, context);
    walker_write(walker, value);
    return value;
}

static uint8_t code_symbol_residual(segment_walker *walker, uint32_t stream, uint32_t context,
                                    uint8_t prediction) {
    uint8_t value = (uint8_t)(walker_symbol(walker, stream, context) + prediction);
    walker_write(walker, value);
    return value;
}

static uint8_t code_blob_byte(segment_walker *walker, uint32_t stream) {
    uint8_t value = walker_blob_byte(walker, stream);
    walker_write(walker, value);
    return value;
}

static uint8_t code_blob_residual(segment_walker *walker, uint32_t stream, uint8_t prediction) {
    uint8_t value = (uint8_t)(walker_blob_byte(walker, stream) + prediction);
    walker_write(walker, value);
    return value;
}

/* code_blob_bytes returns the little-endian value of the bytes. */
static uint32_t code_blob_bytes(segment_walker *walker, uint32_t stream, uint32_t count) {
    uint32_t value = 0;
    for (uint32_t index = 0; index < count; index++) {
        value |= (uint32_t)code_blob_byte(walker, stream) << (8 * index);
    }
    return value;
}

static void copy_blob(segment_walker *walker, uint32_t stream, uint32_t count) {
    if (walker->is_failed) {
        return;
    }
    if (count > walker->segment_len - walker->position) {
        walker_fail(walker);
        return;
    }
    if (!read_blob_bytes(walker->streams, stream, walker->segment + walker->position, count)) {
        walker_fail(walker);
        return;
    }
    walker->position += count;
}

static coded_uint16 code_uint16(segment_walker *walker, uint32_t stream, uint32_t context,
                                uint16_t prediction) {
    coded_uint16 coded;
    coded.residual = (int16_t)walker_residual(walker, stream, context);
    coded.value = (uint16_t)(prediction + (uint32_t)coded.residual);
    walker_write_uint16(walker, coded.value);
    return coded;
}

static coded_uint32 code_uint32(segment_walker *walker, uint32_t stream, uint32_t context,
                                uint32_t prediction) {
    coded_uint32 coded;
    coded.residual = walker_residual(walker, stream, context);
    coded.value = prediction + (uint32_t)coded.residual;
    walker_write_uint32(walker, coded.value);
    return coded;
}

/* FORMAT.md 5.2 */
static void float_origin(segment_walker *walker, uint32_t stream, uint32_t axis, bool is_sent,
                         float_track *track) {
    if (!is_sent) {
        float_track_hold(track);
        return;
    }
    uint32_t context = min_uint32(residual_bucket(track->residual), 63) | axis << 6;
    coded_uint32 coded = code_uint32(walker, stream, context, float_track_predict_linear(track));
    float_track_push(track, coded.value, coded.residual);
}

/* player_info_flags codes the flags as the bits that changed since previous. */
static uint16_t player_info_flags(segment_walker *walker, uint16_t previous) {
    uint32_t main_byte = walker_symbol(walker, STREAM_PLAYER_FLAGS, 0x8000 | (previous & 0x7fff));
    uint32_t changed = (main_byte & 0x3f) | ((main_byte >> 6) & 1) << 10;
    bool has_rare_change = (main_byte & 0x80) != 0;
    if (has_rare_change) {
        uint32_t rare_byte =
            walker_symbol(walker, STREAM_PLAYER_FLAGS, 0x100 | ((previous >> 6) & 0xff));
        uint32_t top_byte = walker_symbol(walker, STREAM_PLAYER_FLAGS, 0x200);
        changed |= (rare_byte & 0xf) << 6 | (rare_byte >> 4) << 11 | (top_byte & 1) << 15;
    }
    uint16_t flags = (uint16_t)(changed ^ previous);
    walker_write_uint16(walker, flags);
    return flags;
}

/* FORMAT.md 5.3 and 5.4 */
static void player_origins_and_angles(segment_walker *walker, player_state *player,
                                      uint16_t flags) {
    int32_t now;
    int32_t before;
    deltas(&player->frame_times, walker->time, &now, &before);
    bool has_float = has_float_coords(&walker->protocol);
    for (uint32_t axis = 0; axis < 3; axis++) {
        bool is_sent = (flags & (DF_ORIGIN << axis)) != 0;
        if (has_float) {
            float_origin(walker, STREAM_PLAYER_FLOAT_ORIGIN + axis, axis, is_sent,
                         &player->float_origins[axis]);
            continue;
        }
        axis_track *track = &player->origins[axis];
        if (!is_sent) {
            axis_track_hold(track);
            continue;
        }
        uint32_t context = min_uint32(residual_bucket(track->residual), 19) |
                           min_uint32(residual_bucket(axis_track_velocity(track)), 19) << 5;
        coded_uint16 coded = code_uint16(walker, STREAM_PLAYER_ORIGIN + axis, context,
                                         axis_track_predict_time_scaled(track, now, before));
        axis_track_push(track, coded.value, coded.residual);
    }
    for (uint32_t axis = 0; axis < 3; axis++) {
        axis_track *track = &player->angles[axis];
        bool is_sent = (flags & (DF_ANGLES << axis)) != 0;
        if (!is_sent) {
            axis_track_hold(track);
            continue;
        }
        uint32_t context = min_uint32(residual_bucket(track->residual), 23) |
                           min_uint32(residual_bucket(axis_track_previous_velocity(track)), 23)
                               << 5;
        coded_uint16 coded = code_uint16(walker, STREAM_PLAYER_ANGLE + axis, context,
                                         axis_track_predict_time_scaled(track, now, before));
        axis_track_push(track, coded.value, coded.residual);
    }
}

/* FORMAT.md 5.6 */
static void usercmd_grid_angle(segment_walker *walker, uint32_t axis, axis_track *track) {
    uint32_t context = min_uint32(residual_bucket(track->residual), 23) |
                       min_uint32(residual_bucket(axis_track_velocity(track)), 23) << 5 |
                       min_uint32(residual_bucket(axis_track_previous_velocity(track)), 15) << 10 |
                       axis << 14;
    uint16_t prediction = axis_track_predict_linear(track);
    int32_t residual = (int16_t)walker_residual(walker, STREAM_USERCMD_ANGLE, context);
    uint16_t value = (uint16_t)(prediction + (uint32_t)residual);
    walker_write_uint32(walker, grid_to_float32_bits(value));
    axis_track_push(track, value, residual);
}

static uint32_t command_class(uint8_t command) {
    switch (command) {
    case 0:
        return 0;
    case SVC_PLAYER_INFO:
        return 1;
    case SVC_DELTA_PACKET_ENTITIES:
        return 2;
    case SVC_PACKET_ENTITIES:
        return 3;
    case SVC_SOUND:
        return 4;
    case SVC_TEMP_ENTITY:
        return 5;
    case SVC_UPDATE_STAT:
        return 6;
    case SVC_UPDATE_STAT_LONG:
        return 7;
    case SVC_PRINT:
        return 8;
    case SVC_SET_INFO:
        return 9;
    case SVC_UPDATE_FRAGS:
        return 10;
    case SVC_UPDATE_PING:
        return 11;
    case SVC_STUFF_TEXT:
        return 12;
    case SVC_NAILS:
    case SVC_NAILS_2:
        return 13;
    case SVC_DAMAGE:
        return 14;
    default:
        return 15;
    }
}
