/* Builds against include/gpress.h and libgpress_c: compresses a file to a gpress archive, expands it
 * back and checks the bytes. Usage: roundtrip FILE */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "gpress.h"

int main(int argc, char **argv) {
  if (argc != 2) { fprintf(stderr, "usage: roundtrip FILE\n"); return 2; }
  FILE *f = fopen(argv[1], "rb");
  if (!f) { perror(argv[1]); return 2; }
  fseek(f, 0, SEEK_END); long n = ftell(f); fseek(f, 0, SEEK_SET);
  uint8_t *buf = malloc(n > 0 ? n : 1);
  if (fread(buf, 1, n, f) != (size_t)n) { perror("read"); return 2; }
  fclose(f);

  gpress_options opt; gpress_options_default(&opt);
  gpress_result c = gpress_compress(buf, n, &opt);
  if (c.code != GPRESS_OK) { fprintf(stderr, "compress: %s\n", c.message); gpress_result_free(&c); return 1; }
  gpress_result e = gpress_expand(c.output.data, c.output.len, &opt);
  if (e.code != GPRESS_OK) { fprintf(stderr, "expand: %s\n", e.message); return 1; }
  int same = e.output.len == (size_t)n && memcmp(e.output.data, buf, n) == 0;
  printf("gpress %s: %ld -> %zu bytes, restore %s\n", gpress_version(), n, c.output.len, same ? "exact" : "MISMATCH");
  gpress_result_free(&c); gpress_result_free(&e); free(buf);
  return same ? 0 : 1;
}
