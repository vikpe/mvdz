/* Example: compress a demo, decode the .mvdz through stdio callbacks (as
 * an engine would through its VFS), seek by time and read the metadata.
 * Usage: example file.mvd  (any QuakeWorld MVD demo). */
#define _FILE_OFFSET_BITS 64
#include "mvdz.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static long long f_read(void *u, void *buf, size_t n) {
    size_t k = fread(buf, 1, n, (FILE *)u);
    return ferror((FILE *)u) ? -1 : (long long)k;
}
static long long f_seek(void *u, long long off, int whence) {
    int w = whence == 0 ? SEEK_SET : whence == 1 ? SEEK_CUR : SEEK_END;
    /* 64-bit offsets (fseeko/ftello; _fseeki64 on Windows) */
    if (fseeko((FILE *)u, (off_t)off, w) != 0) return -1;
    return (long long)ftello((FILE *)u);
}

static int fail(const char *what, int rc) {
    fprintf(stderr, "%s: %s (%d)\n", what, mvdz_strerror(rc), rc);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 2) { fprintf(stderr, "usage: %s demo.mvd\n", argv[0]); return 2; }
    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror(argv[1]); return 1; }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *demo = malloc(n);
    if (fread(demo, 1, n, f) != (size_t)n) return 1;
    fclose(f);
    printf("mvdz %s\n", mvdz_version());

    /* 1. compress the demo */
    const char *packed = "example_out.mvdz";
    unsigned char *c; size_t c_n;
    int err = 0;
    int rc = mvdz_compress(demo, (size_t)n, 1, &c, &c_n);
    if (rc) return fail("compress", rc);
    FILE *o = fopen(packed, "wb");
    if (!o || fwrite(c, 1, c_n, o) != c_n) { perror(packed); return 1; }
    fclose(o);
    long packed_n = (long)c_n;
    mvdz_free(c, c_n);
    printf("compressed %ld -> %ld bytes\n", n, packed_n);

    /* 2. the header and the segments, without decoding */
    FILE *p = fopen(packed, "rb");
    char *json; size_t json_n;
    rc = mvdz_info_json(f_read, f_seek, p, 0, &json, &json_n);
    if (rc) return fail("info_json", rc);
    if (!strstr(json, "\"sha256\":")) { fprintf(stderr, "sha256 missing\n"); return 1; }
    printf("info: %.120s...\n", json);
    mvdz_free(json, json_n);

    /* 3. decode through callbacks and compare */
    fseek(p, 0, SEEK_SET);
    mvdz_reader *r = mvdz_open_callbacks(f_read, f_seek, p, 2, 0, &err);
    if (!r) return fail("open_callbacks", err);
    printf("raw size %lld, segments %lld\n", mvdz_raw_size(r), mvdz_segment_count(r));
    unsigned char *back = malloc(n + 1);
    long long got = 0, k;
    while ((k = mvdz_read(r, back + got, 7777 < n + 1 - got ? 7777 : (size_t)(n + 1 - got))) > 0) got += k;
    if (k < 0) return fail("read", (int)k);
    if (got != n || memcmp(back, demo, n) != 0) { fprintf(stderr, "MISMATCH\n"); return 1; }
    printf("decoded %lld bytes, identical\n", got);

    /* 4. seek to the middle of the demo by time */
    uint64_t t0, off;
    rc = mvdz_seek_time(r, 60000, &t0, &off);
    if (rc) return fail("seek_time", rc);
    got = 0;
    while ((k = mvdz_read(r, back + got, 65536)) > 0) got += k;
    if (k < 0 || (uint64_t)got != (uint64_t)n - off || memcmp(back, demo + off, got) != 0) {
        fprintf(stderr, "SEEK MISMATCH\n");
        return 1;
    }
    printf("seek to 60 s: segment starts at %llu ms, byte %llu; tail identical\n",
           (unsigned long long)t0, (unsigned long long)off);
    mvdz_close(r);
    mvdz_close(r); /* a closed handle is invalid, not a crash */
    if (mvdz_read(r, back, 10) != MVDZ_E_INVALID) { fprintf(stderr, "closed handle accepted\n"); return 1; }
    fclose(p);

    /* 5. corrupt input never crashes */
    unsigned char junk[64] = "MVDZ\x01";
    r = mvdz_open_memory(junk, sizeof junk, 1, 0, &err);
    printf("garbage: %s\n", r ? "opened?!" : mvdz_strerror(err));
    if (r) mvdz_close(r);

    /* 6. whole-buffer API (raw .mvd input); input that is not a
     * demo is refused */
    unsigned char *z; size_t z_n;
    rc = mvdz_compress(demo, (size_t)n, 4, &z, &z_n);
    if (rc) return fail("compress", rc);
    unsigned char *e; size_t e_n;
    rc = mvdz_decompress(z, z_n, 1, 0, &e, &e_n);
    if (rc || e_n != (size_t)n || memcmp(e, demo, n) != 0) return fail("decompress", rc);
    mvdz_free(z, z_n);
    mvdz_free(e, e_n);
    rc = mvdz_compress("hello world", 11, 1, &z, &z_n);
    if (rc != MVDZ_E_NOTMVD) return fail("compress of a non-demo", rc);
    printf("non-demo: %s\n", mvdz_strerror(rc));
    free(back);
    free(demo);
    remove(packed);
    printf("OK\n");
    return 0;
}
