static mvdz_error ok(void) {
    mvdz_error error = {MVDZ_OK, NULL};
    return error;
}

static mvdz_error corrupt(const char *rule) {
    mvdz_error error = {MVDZ_CORRUPT, rule};
    return error;
}

static mvdz_error memory_limit(const char *message) {
    mvdz_error error = {MVDZ_MEMORY_LIMIT, message};
    return error;
}

static mvdz_error out_of_memory(void) {
    return memory_limit("out of memory");
}

/* A size of 0 allocates a byte: NULL is out of memory only. */
static void *allocate(size_t size) {
    return malloc(size != 0 ? size : 1);
}
