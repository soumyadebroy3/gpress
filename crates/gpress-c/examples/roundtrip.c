/* Builds against include/gpress.h and libgpress_c: optimises a PDF (or reports why it was left as it is),
 * compresses the file to a gpress archive, expands it back and checks the bytes.
 * Usage: roundtrip FILE [effort] */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "gpress.h"

int main(int argc, char **argv) {
  if (argc < 2 || argc > 3) { fprintf(stderr, "usage: roundtrip FILE [effort]\n"); return 2; }
  FILE *f = fopen(argv[1], "rb");
  if (!f) { perror(argv[1]); return 2; }
  fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
  uint8_t *buf = malloc(n > 0 ? n : 1);
  if (fread(buf, 1, n, f) != (size_t)n) { perror("read"); return 2; }
  fclose(f);

  gpress_options opt; gpress_options_default(&opt);
  if (argc == 3) opt.effort = (uint8_t)atoi(argv[2]);
  opt.time_budget_ms = 600000; /* ten minutes, then GPRESS_ERR_TIME_BUDGET instead of an output */

  gpress_result o = gpress_optimize(buf, n, &opt);
  if (o.code != GPRESS_OK) { fprintf(stderr, "optimize: %s (code %d)\n", o.message, o.code); gpress_result_free(&o); free(buf); return 1; }
  if (o.outcome == GPRESS_OUTCOME_SMALLER) printf("optimize: %ld -> %zu bytes\n", n, o.output.len);
  else printf("optimize: left as it is (%s)\n", o.message ? o.message : "");
  gpress_result_free(&o);

  gpress_result c = gpress_compress(buf, n, &opt);
  if (c.code != GPRESS_OK) { fprintf(stderr, "compress: %s (code %d)\n", c.message, c.code); gpress_result_free(&c); free(buf); return 1; }
  gpress_result e = gpress_expand(c.output.data, c.output.len, &opt);
  if (e.code != GPRESS_OK) { fprintf(stderr, "expand: %s (code %d)\n", e.message, e.code); gpress_result_free(&c); gpress_result_free(&e); free(buf); return 1; }
  int same = e.output.len == (size_t)n && memcmp(e.output.data, buf, n) == 0;
  printf("gpress %s: archive %ld -> %zu bytes%s, restore %s\n", gpress_version(), n, c.output.len,
         c.outcome == GPRESS_OUTCOME_UNTOUCHED ? " (stored as it is)" : "", same ? "exact" : "MISMATCH");
  gpress_result_free(&c); gpress_result_free(&e); free(buf);
  return same ? 0 : 1;
}
