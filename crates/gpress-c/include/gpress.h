/* gpress: lossless PDF compression. C API.
 *
 * Link with libgpress_c (shared or static). Every function is thread-safe and never aborts the process:
 * a problem comes back as a non-zero code and a message. Every buffer or message gpress hands out is
 * freed with the matching gpress_free* function, never with free().
 *
 *   gpress_options opt; gpress_options_default(&opt); opt.effort = 70;
 *   gpress_result r = gpress_optimize(pdf, pdf_len, &opt);
 *   if (r.code == 0) { fwrite(r.output.data, 1, r.output.len, out); }
 *   else            { fprintf(stderr, "%s\n", r.message); }
 *   gpress_result_free(&r);
 *
 * The output never depends on the thread count, the machine or the time budget: a budget that runs out
 * gives GPRESS_ERR_TIME_BUDGET and no output.
 */
#ifndef GPRESS_H
#define GPRESS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Error codes in gpress_result.code. 0 is success; the others are stable across versions. */
enum {
  GPRESS_OK = 0,
  GPRESS_ERR_TOO_LARGE = 1,              /* input over options.max_input */
  GPRESS_ERR_DAMAGED = 2,                /* not a gpress archive, or a damaged one; nothing restored */
  GPRESS_ERR_UNSUPPORTED_VERSION = 3,    /* an archive format version this build doesn't read */
  GPRESS_ERR_INTERNAL = 4,               /* a bug in gpress; the input is unchanged. Please report it. */
  GPRESS_ERR_BAD_ARGUMENT = 5,           /* null input with a non-zero length, or options the engine refuses
                                            (threads over 64, a zero time budget, max_input or
                                            objects_per_stream of 0) */
  GPRESS_ERR_TIME_BUDGET = 6,            /* the time budget ran out; the input is unchanged */
  GPRESS_ERR_DECLARED_TOO_LARGE = 7,     /* expand: the archive declares more than options.max_len bytes */
  GPRESS_ERR_NEEDS_EXISTING_LIBRARY = 8, /* expand: the archive has parts made with the existing library
                                            (Lepton) and this build doesn't have it */
  GPRESS_ERR_DECODED_TOO_LARGE = 9,      /* expand: the archive declares more decoded bytes than
                                            options.max_decoded; nothing was decoded */
  GPRESS_ERR_NEEDS_FEATURE = 10          /* the call needs a part of gpress this build leaves out (a Cargo
                                            feature; the message names it): compress and expand in a build
                                            without "archive", or expand of an archive with parts that need
                                            "fax" or "encryption". Nothing was done. (optimize never gives
                                            it: a step whose feature is left out is skipped, and an
                                            encrypted file is left untouched, with the reason) */
};

/* Outcomes in gpress_result.outcome (optimize and compress). */
enum {
  GPRESS_OUTCOME_SMALLER = 1,  /* output is smaller than the input, content preserved */
  GPRESS_OUTCOME_UNTOUCHED = 2 /* output is the input as it was (optimize: message says why; compress: the
                                  archive stores the input as it is) */
};

/* The options, the same as the library's. Fill with gpress_options_default and change what you need: a
 * zeroed struct is not the defaults (it means effort 0 and every switch off), though a size left at 0
 * means its default. */
typedef struct gpress_options {
  uint8_t effort;                 /* 0..100: 0 fastest, 100 everything the engine knows (default 100) */
  uint32_t threads;               /* worker threads; 0 = every core the machine offers, at most 64.
                                     The output never depends on it */
  uint64_t max_input;             /* largest accepted input in bytes; 0 = 1 GiB */
  uint64_t max_stream;            /* normal PDF: largest decoded stream re-encoded; 0 = 64 MiB */
  uint64_t max_stream_archive;    /* archive: largest decoded stream stored decoded; 0 = 256 MiB */
  uint64_t max_decoded;           /* archive: most decoded bytes the parts of one archive may claim in all;
                                     compress stores streams past it as they are, expand refuses an archive
                                     declaring more before anything is decoded (its memory follows this
                                     and max_len, see README "Options"); 0 = 256 MiB */
  uint64_t max_len;               /* expand: largest file restored; an archive declaring more is refused
                                     before anything is decoded; 0 = 1 GiB */
  uint32_t objects_per_stream;    /* normal PDF: most objects per object stream; 0 = 200 */
  uint64_t time_budget_ms;        /* give up after this many milliseconds (GPRESS_ERR_TIME_BUDGET);
                                     0 = no limit */
  uint8_t respect_conformance;    /* 1: leave PDF/A, PDF/X, signed and certified files untouched */
  uint8_t flate_over_jpeg;        /* 1: also try Flate in front of JPEG images */
  uint8_t jpeg_tables;            /* 1: give JPEG images optimal Huffman tables (same coefficients) */
  uint8_t jbig2;                  /* 1: write CCITT fax images as JBIG2 when clearly smaller (same pixels) */
  uint8_t rewrite_encrypted;      /* 1: rewrite files the empty password opens, keeping their encryption */
  uint8_t decrypt;                /* 1: archive: store such files' streams decrypted (encrypted again on
                                     expand) */
  uint8_t use_existing_libraries; /* 1: use the optional existing library (Lepton) when built in */
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

/* Fills *out with the defaults (effort 100, every step on). */
void gpress_options_default(gpress_options *out);

/* A smaller PDF that any reader opens, every object's value preserved; or the input untouched. */
gpress_result gpress_optimize(const uint8_t *input, size_t len, const gpress_options *options);

/* The gpress archive of any input. gpress_expand restores it byte for byte. */
gpress_result gpress_compress(const uint8_t *input, size_t len, const gpress_options *options);

/* The original bytes from an archive. A damaged archive is an error, never a wrong file. */
gpress_result gpress_expand(const uint8_t *input, size_t len, const gpress_options *options);

/* Frees the parts of a result and zeroes it. Safe on NULL and on a result already freed. */
void gpress_result_free(gpress_result *result);

/* Lower-level frees, for callers that take the buffer or message out of a result: each takes the field's
 * address, frees what it points to and sets it to NULL (or zeroes the buffer), so freeing twice, or freeing
 * a part and then the whole result, is harmless by construction. Both are safe on NULL and on an empty
 * part. An empty output (an archive of an empty file, say) has data == NULL and len == 0. */
void gpress_free(gpress_buffer *buffer);
void gpress_free_message(char **message);

#ifdef __cplusplus
}
#endif
#endif /* GPRESS_H */
