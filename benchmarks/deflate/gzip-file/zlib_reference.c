// The fastest zlib reference of this record: libdeflate's zlib API over a whole file in memory,
// as libdeflate-gunzip does for gzip. It decodes the zlib stream that starts SKIP bytes into IN,
// checks its Adler-32, and writes the first SKIP bytes with their first byte replaced by 'F',
// then the decoded bytes, to stdout. With SKIP 8 that turns a zlib-compressed SWF into the
// uncompressed SWF.
// Build: cc -O2 -I INCLUDE -o zlib-reference zlib_reference.c LIB/libdeflate.a
// Use:   zlib-reference SKIP IN > OUT
#include <fcntl.h>
#include <libdeflate.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc != 3) return fprintf(stderr, "usage: zlib-reference SKIP IN > OUT\n"), 2;
    size_t skip = strtoul(argv[1], NULL, 10);
    int in = open(argv[2], O_RDONLY);
    struct stat status;
    if (in < 0 || fstat(in, &status) != 0 || (size_t)status.st_size < skip) return perror(argv[2]), 1;
    size_t packed_size = status.st_size;
    unsigned char *packed = mmap(NULL, packed_size, PROT_READ, MAP_PRIVATE, in, 0);
    if (packed == MAP_FAILED) return perror("mmap"), 1;
    size_t room = skip >= 8 ? (size_t)packed[4] | (size_t)packed[5] << 8 | (size_t)packed[6] << 16 | (size_t)packed[7] << 24
                            : packed_size * 40;
    unsigned char *plain = malloc(room);
    if (!plain) return perror("malloc"), 1;
    memcpy(plain, packed, skip);
    if (skip) plain[0] = 'F';
    struct libdeflate_decompressor *decoder = libdeflate_alloc_decompressor();
    size_t written;
    enum libdeflate_result result = libdeflate_zlib_decompress(decoder, packed + skip, packed_size - skip,
                                                              plain + skip, room - skip, &written);
    if (result != LIBDEFLATE_SUCCESS) return fprintf(stderr, "libdeflate refused the stream: %d\n", result), 1;
    size_t total = skip + written, done = 0;
    while (done < total) {
        ssize_t count = write(STDOUT_FILENO, plain + done, total - done);
        if (count < 0) return perror("write"), 1;
        done += count;
    }
    return 0;
}
