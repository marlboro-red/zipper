// Apple's libcompression as a test oracle: apple_compression enc|dec lzfse|lzvn IN OUT [capacity]
// writes the encoded or decoded IN to OUT. 0x900 is libcompression's private id for raw LZVN.
#include <compression.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned char *slurp(const char *path, size_t *length) {
  FILE *file = fopen(path, "rb");
  if (!file) {
    perror(path);
    exit(2);
  }
  fseek(file, 0, SEEK_END);
  long size = ftell(file);
  fseek(file, 0, SEEK_SET);
  unsigned char *bytes = malloc(size + 1);
  *length = fread(bytes, 1, size, file);
  fclose(file);
  return bytes;
}

int main(int argc, char **argv) {
  if (argc < 5) return 2;
  compression_algorithm algorithm =
      strcmp(argv[2], "lzvn") == 0 ? (compression_algorithm)0x900 : COMPRESSION_LZFSE;
  size_t length;
  unsigned char *input = slurp(argv[3], &length);
  int encode = strcmp(argv[1], "enc") == 0;
  size_t capacity = encode ? length * 2 + 4096 : length * 64 + (1 << 20);
  if (argc > 5) capacity = strtoull(argv[5], 0, 10);
  unsigned char *output = malloc(capacity + 1);
  size_t written = encode
      ? compression_encode_buffer(output, capacity, input, length, NULL, algorithm)
      : compression_decode_buffer(output, capacity, input, length, NULL, algorithm);
  FILE *file = fopen(argv[4], "wb");
  fwrite(output, 1, written, file);
  fclose(file);
  return written == 0 && length != 0;
}
