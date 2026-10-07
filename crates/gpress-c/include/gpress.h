/* gpress: lossless PDF compression. C API.
 *
 * Link with libgpress (shared or static). Every function is thread-safe and never aborts the process:
 * a problem comes back as a non-zero code and a message. Every buffer or message gpress hands out is
 * freed with the matching gpress_free* function, never with free().
 *
 *   gpress_options opt; gpress_options_default(&opt); opt.effort = 70;
 *   gpress_result r = gpress_optimize(pdf, pdf_len, &opt);
 *   if (r.code == 0) { fwrite(r.output.data, 1, r.output.len, out); }
 *   else            { fprintf(stderr, "%s\n", r.message); }
 *   gpress_result_free(&r);
 */
#ifndef GPRESS_H
#define GPRESS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Error codes in gpress_result.code. 0 is success. */
enum {
  GPRESS_OK = 0,
  GPRESS_ERR_TOO_LARGE = 1,           /* input over options.max_input */
  GPRESS_ERR_DAMAGED = 2,             /* not a gpress archive, or a damaged one; nothing restored */
  GPRESS_ERR_UNSUPPORTED_VERSION = 3, /* archive made by a newer gpress */
  GPRESS_ERR_INTERNAL = 4,            /* a bug in gpress; the input is unchanged. Please report it. */
  GPRESS_ERR_BAD_ARGUMENT = 5         /* null input with a non-zero length */
};

/* Outcomes in gpress_result.outcome (optimize and compress). */
enum {
  GPRESS_OUTCOME_SMALLER = 1,  /* output is smaller than the input, content preserved */
  GPRESS_OUTCOME_UNTOUCHED = 2 /* output is the input as it was (optimize: message says why) */
};

typedef struct gpress_options {
  uint8_t effort;                 /* 0..100: 0 quick, 100 everything the engine knows */
  uint64_t max_input;             /* largest accepted input in bytes; 0 = 1 GiB */
  uint64_t max_stream;            /* largest decoded stream kept in memory; 0 = 64 MiB */
  uint32_t threads;               /* worker threads; 0 = one per CPU. Output never depends on it */
  uint64_t time_budget_ms;        /* stop early after this many milliseconds; 0 = no limit */
  uint8_t respect_conformance;    /* 1: leave PDF/A, PDF/X, signed and certified files untouched */
  uint8_t flate_over_jpeg;        /* 1: also try Flate in front of JPEG images */
  uint8_t use_existing_libraries; /* 1: use the optional existing libraries when built in */
} gpress_options;

typedef struct gpress_buffer {
  uint8_t *data;
  size_t len;
  size_t cap_; /* internal */
} gpress_buffer;

typedef struct gpress_result {
  int code;             /* GPRESS_OK or a GPRESS_ERR_* value */
  int outcome;          /* GPRESS_OUTCOME_* for optimize/compress; 0 for expand */
  gpress_buffer output; /* the result bytes on success; empty on error */
  char *message;        /* error text, or the reason a file was left untouched; may be NULL */
} gpress_result;

/* Version string, e.g. "0.1.0". Static; do not free. */
const char *gpress_version(void);

/* Fills *out with the defaults (effort 100). */
void gpress_options_default(gpress_options *out);

/* A smaller PDF that any reader opens, every object's value preserved; or the input untouched. */
gpress_result gpress_optimize(const uint8_t *input, size_t len, const gpress_options *options);

/* The gpress archive of any input. gpress_expand restores it byte for byte. */
gpress_result gpress_compress(const uint8_t *input, size_t len, const gpress_options *options);

/* The original bytes from an archive. A damaged archive is an error, never a wrong file. */
gpress_result gpress_expand(const uint8_t *input, size_t len, const gpress_options *options);

/* Frees the parts of a result and zeroes it. Safe on NULL and on a result already freed. */
void gpress_result_free(gpress_result *result);

/* Lower-level frees, for callers that take the buffer or message out of a result. */
void gpress_free(gpress_buffer *buffer);
void gpress_free_message(char *message);

#ifdef __cplusplus
}
#endif
#endif /* GPRESS_H */
