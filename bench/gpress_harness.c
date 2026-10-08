/* gpress_harness: runs one file through the gpress C library, so the security benchmark can send hostile
 * input through the C API as well as through the command-line tool. The point of going through C: the
 * library promises never to abort the host process (a panic is caught by its guard and comes back as
 * GPRESS_ERR_INTERNAL), so a panic that escapes the guard aborts this program (SIGABRT), which the benchmark
 * counts as a crash, and a caught one shows up as exit code 4, which it counts as a bug too.
 *
 *   gpress_harness optimize  IN OUT [options]   smaller PDF or untouched -> OUT
 *   gpress_harness compress  IN OUT [options]   gpress archive of IN -> OUT
 *   gpress_harness expand    IN OUT [options]   the bytes an archive restores -> OUT
 *   gpress_harness roundtrip IN OUT [options]   compress, expand, compare; archive -> OUT
 *
 * Options: --effort N, --threads N, --max-decoded BYTES (the archive's decoded-bytes budget, both ways).
 *
 * Exit codes: 0 success (OUT written), 1 a clean error from the library (message on stderr, no OUT),
 * 2 usage or file error, 4 GPRESS_ERR_INTERNAL (the library's guard caught a panic: a bug, but no crash),
 * 5 roundtrip restored different bytes (data loss). optimize prints "IN<TAB>SMALLER|UNTOUCHED<TAB>in<TAB>out"
 * on stdout, the same first fields the command-line tool prints.
 *
 * Built by bench/security.py only when crates/gpress-c (and its include/gpress.h) is in the tree, against
 * the static library: cc -O2 -I crates/gpress-c/include bench/gpress_harness.c target/release/libgpress_c.a
 * -lpthread -ldl -lm. The benchmark skips this part with a message when the crate is absent. */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "gpress.h"

static uint8_t *read_all(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return NULL; }
    long n = ftell(f);
    if (n < 0) { fclose(f); return NULL; }
    rewind(f);
    uint8_t *buf = (uint8_t *)malloc((size_t)n ? (size_t)n : 1);
    if (!buf) { fclose(f); return NULL; }
    size_t got = fread(buf, 1, (size_t)n, f);
    fclose(f);
    if (got != (size_t)n) { free(buf); return NULL; }
    *len = got;
    return buf;
}

static int write_all(const char *path, const uint8_t *buf, size_t len) {
    FILE *f = fopen(path, "wb");
    if (!f) return 2;
    size_t wrote = len ? fwrite(buf, 1, len, f) : 0;
    int bad = fclose(f) != 0;
    return (wrote == len && !bad) ? 0 : 2;
}

/* The library's code as this program's exit code (see the top of the file). */
static int exit_code_for(const gpress_result *r, const char *what) {
    if (r->code == GPRESS_OK) return 0;
    fprintf(stderr, "%s: %s (code %d)\n", what, r->message ? r->message : "error", r->code);
    return r->code == GPRESS_ERR_INTERNAL ? 4 : 1;
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: gpress_harness optimize|compress|expand|roundtrip IN OUT [--effort N] [--threads N] [--max-decoded BYTES]\n");
        return 2;
    }
    gpress_options opt;
    gpress_options_default(&opt);
    opt.threads = 1;
    for (int i = 4; i + 1 < argc; i += 2) {
        if (strcmp(argv[i], "--effort") == 0) opt.effort = (uint8_t)strtoul(argv[i + 1], NULL, 10);
        else if (strcmp(argv[i], "--threads") == 0) opt.threads = (uint32_t)strtoul(argv[i + 1], NULL, 10);
        else if (strcmp(argv[i], "--max-decoded") == 0) opt.max_decoded = (uint64_t)strtoull(argv[i + 1], NULL, 10);
        else { fprintf(stderr, "unknown option %s\n", argv[i]); return 2; }
    }
    const char *mode = argv[1], *in_path = argv[2], *out_path = argv[3];
    size_t in_len = 0;
    uint8_t *in = read_all(in_path, &in_len);
    if (!in) { fprintf(stderr, "cannot read %s\n", in_path); return 2; }

    int code = 2;
    if (strcmp(mode, "optimize") == 0) {
        gpress_result r = gpress_optimize(in, in_len, &opt);
        code = exit_code_for(&r, "optimize");
        if (code == 0) {
            printf("%s\t%s\t%zu\t%zu\n", in_path, r.outcome == GPRESS_OUTCOME_SMALLER ? "SMALLER" : "UNTOUCHED", in_len, r.output.len);
            code = write_all(out_path, r.output.data, r.output.len);
        }
        gpress_result_free(&r);
    } else if (strcmp(mode, "compress") == 0 || strcmp(mode, "expand") == 0) {
        gpress_result r = mode[0] == 'c' ? gpress_compress(in, in_len, &opt) : gpress_expand(in, in_len, &opt);
        code = exit_code_for(&r, mode);
        if (code == 0) code = write_all(out_path, r.output.data, r.output.len);
        gpress_result_free(&r);
    } else if (strcmp(mode, "roundtrip") == 0) {
        gpress_result c = gpress_compress(in, in_len, &opt);
        code = exit_code_for(&c, "compress");
        if (code == 0) {
            gpress_result e = gpress_expand(c.output.data, c.output.len, &opt);
            code = exit_code_for(&e, "expand");
            if (code == 0) {
                int same = e.output.len == in_len && (in_len == 0 || memcmp(e.output.data, in, in_len) == 0);
                printf("%s\t%s\t%zu\t%zu\n", in_path, same ? "EXACT" : "MISMATCH", in_len, c.output.len);
                code = same ? write_all(out_path, c.output.data, c.output.len) : 5;
            }
            gpress_result_free(&e);
        }
        gpress_result_free(&c);
    } else {
        fprintf(stderr, "unknown mode %s\n", mode);
    }
    free(in);
    return code;
}
