/* axis_track is the 16-bit history of one axis, FORMAT.md 5.2. */
typedef struct axis_track {
    uint16_t last;
    uint16_t previous;
    uint16_t before_previous;
    int16_t residual;
} axis_track;

/* float_track is the history of one float coordinate, as f32 bits. */
typedef struct float_track {
    uint32_t last_bits;
    uint32_t previous_bits;
    int32_t residual;
} float_track;

typedef struct frame_times {
    uint32_t last;
    uint32_t previous;
} frame_times;

static int32_t axis_track_velocity(const axis_track *track) {
    return (int16_t)(track->last - track->previous);
}

static int32_t axis_track_previous_velocity(const axis_track *track) {
    return (int16_t)(track->previous - track->before_previous);
}

static void axis_track_push(axis_track *track, uint16_t value, int32_t residual) {
    track->before_previous = track->previous;
    track->previous = track->last;
    track->last = value;
    track->residual = (int16_t)residual;
}

/* Notes a value left out of its update: it did not change. */
static void axis_track_hold(axis_track *track) {
    axis_track_push(track, track->last, 0);
}

static uint16_t axis_track_predict_linear(const axis_track *track) {
    return (uint16_t)(track->last + (uint32_t)axis_track_velocity(track));
}

/* Scales the last velocity by the time deltas when they differ. */
static uint16_t axis_track_predict_time_scaled(const axis_track *track, int32_t now,
                                               int32_t before) {
    bool is_unscaled = now == before || before <= 0 || now < 0;
    if (is_unscaled) {
        return axis_track_predict_linear(track);
    }
    int32_t scaled =
        div_round(axis_track_velocity(track) * min_int32(now, 32767), min_int32(before, 32767));
    return (uint16_t)(track->last + (uint32_t)scaled);
}

static uint32_t float_track_predict_linear(const float_track *track) {
    return track->last_bits + (track->last_bits - track->previous_bits);
}

static void float_track_push(float_track *track, uint32_t value_bits, int32_t residual) {
    track->previous_bits = track->last_bits;
    track->last_bits = value_bits;
    track->residual = residual;
}

static void float_track_hold(float_track *track) {
    float_track_push(track, track->last_bits, 0);
}

/* deltas takes time as the last, FORMAT.md 5.2. */
static void deltas(frame_times *times, uint32_t time, int32_t *now, int32_t *before) {
    *now = (int32_t)(time - times->last);
    *before = (int32_t)(times->last - times->previous);
    times->previous = times->last;
    times->last = time;
}

static void last_values(const axis_track *tracks, uint16_t *values) {
    for (uint32_t axis = 0; axis < 3; axis++) {
        values[axis] = tracks[axis].last;
    }
}
