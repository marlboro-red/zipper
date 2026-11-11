// Counts and times the file system calls of one process, for a profile outside timed runs.
// Build: cc -O2 -dynamiclib -o syscount.dylib syscount.c
// Use:   DYLD_INSERT_LIBRARIES=syscount.dylib SYSCOUNT_OUT=file program ...
#include <fcntl.h>
#include <mach/mach_time.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <unistd.h>

enum { READ, WRITE, OPEN, CLOSE, MKDIR, RENAME, RENAMEX, STAT, LSTAT, FSTAT, STATFS, UNLINK, RMDIR, FSYNC,
       MMAP, MUNMAP, FTRUNCATE, FCNTL, UTIMENSAT, FUTIMENS, FCHMOD, CHMOD, OPENAT, FSTATAT, PWRITE, PREAD, MADVISE, KINDS };
static const char *names[KINDS] = {"read", "write", "open", "close", "mkdir", "rename", "renamex_np", "stat", "lstat",
    "fstat", "statfs", "unlink", "rmdir", "fsync", "mmap", "munmap", "ftruncate", "fcntl", "utimensat", "futimens",
    "fchmod", "chmod", "openat", "fstatat", "pwrite", "pread", "madvise"};
static unsigned long long calls[KINDS], ticks[KINDS], bytes[KINDS];
static unsigned long long started;

#define TIMED(kind, expr, size) do { unsigned long long t0_ = mach_absolute_time(); __typeof__(expr) r_ = (expr); \
    ticks[kind] += mach_absolute_time() - t0_; calls[kind]++; if ((long long)(size) > 0) bytes[kind] += (size); return r_; } while (0)
#define TIMEDR(kind, expr) do { unsigned long long t0_ = mach_absolute_time(); ssize_t r_ = (expr); \
    ticks[kind] += mach_absolute_time() - t0_; calls[kind]++; if (r_ > 0) bytes[kind] += r_; return r_; } while (0)

static ssize_t my_read(int fd, void *b, size_t n) { TIMEDR(READ, read(fd, b, n)); }
static ssize_t my_write(int fd, const void *b, size_t n) { TIMEDR(WRITE, write(fd, b, n)); }
static ssize_t my_pread(int fd, void *b, size_t n, off_t o) { TIMEDR(PREAD, pread(fd, b, n, o)); }
static ssize_t my_pwrite(int fd, const void *b, size_t n, off_t o) { TIMEDR(PWRITE, pwrite(fd, b, n, o)); }
static int my_open(const char *p, int f, ...) { va_list a; va_start(a, f); int m = va_arg(a, int); va_end(a); TIMED(OPEN, open(p, f, m), 0); }
static int my_openat(int d, const char *p, int f, ...) { va_list a; va_start(a, f); int m = va_arg(a, int); va_end(a); TIMED(OPENAT, openat(d, p, f, m), 0); }
static int my_close(int fd) { TIMED(CLOSE, close(fd), 0); }
static int my_mkdir(const char *p, mode_t m) { TIMED(MKDIR, mkdir(p, m), 0); }
static int my_rename(const char *a, const char *b) { TIMED(RENAME, rename(a, b), 0); }
static int my_renamex(const char *a, const char *b, unsigned f) { TIMED(RENAMEX, renamex_np(a, b, f), 0); }
static int my_stat(const char *p, struct stat *s) { TIMED(STAT, stat(p, s), 0); }
static int my_lstat(const char *p, struct stat *s) { TIMED(LSTAT, lstat(p, s), 0); }
static int my_fstat(int fd, struct stat *s) { TIMED(FSTAT, fstat(fd, s), 0); }
static int my_fstatat(int d, const char *p, struct stat *s, int f) { TIMED(FSTATAT, fstatat(d, p, s, f), 0); }
static int my_statfs(const char *p, struct statfs *s) { TIMED(STATFS, statfs(p, s), 0); }
static int my_unlink(const char *p) { TIMED(UNLINK, unlink(p), 0); }
static int my_rmdir(const char *p) { TIMED(RMDIR, rmdir(p), 0); }
static int my_fsync(int fd) { TIMED(FSYNC, fsync(fd), 0); }
static void *my_mmap(void *a, size_t n, int p, int f, int fd, off_t o) { TIMED(MMAP, mmap(a, n, p, f, fd, o), n); }
static int my_munmap(void *a, size_t n) { TIMED(MUNMAP, munmap(a, n), n); }
static int my_madvise(void *a, size_t n, int w) { TIMED(MADVISE, madvise(a, n, w), n); }
static int my_ftruncate(int fd, off_t n) { TIMED(FTRUNCATE, ftruncate(fd, n), 0); }
static int my_fcntl(int fd, int c, ...) { va_list a; va_start(a, c); void *x = va_arg(a, void *); va_end(a); TIMED(FCNTL, fcntl(fd, c, x), 0); }
static int my_utimensat(int d, const char *p, const struct timespec t[2], int f) { TIMED(UTIMENSAT, utimensat(d, p, t, f), 0); }
static int my_futimens(int fd, const struct timespec t[2]) { TIMED(FUTIMENS, futimens(fd, t), 0); }
static int my_fchmod(int fd, mode_t m) { TIMED(FCHMOD, fchmod(fd, m), 0); }
static int my_chmod(const char *p, mode_t m) { TIMED(CHMOD, chmod(p, m), 0); }

#define INTERPOSE(mine, real) __attribute__((used)) static struct { const void *a, *b; } interpose_##real \
    __attribute__((section("__DATA,__interpose"))) = {(const void *)mine, (const void *)real}
INTERPOSE(my_read, read); INTERPOSE(my_write, write); INTERPOSE(my_pread, pread); INTERPOSE(my_pwrite, pwrite);
INTERPOSE(my_open, open); INTERPOSE(my_openat, openat); INTERPOSE(my_close, close); INTERPOSE(my_mkdir, mkdir);
INTERPOSE(my_rename, rename); INTERPOSE(my_renamex, renamex_np); INTERPOSE(my_stat, stat); INTERPOSE(my_lstat, lstat);
INTERPOSE(my_fstat, fstat); INTERPOSE(my_fstatat, fstatat); INTERPOSE(my_statfs, statfs); INTERPOSE(my_unlink, unlink);
INTERPOSE(my_rmdir, rmdir); INTERPOSE(my_fsync, fsync); INTERPOSE(my_mmap, mmap); INTERPOSE(my_munmap, munmap);
INTERPOSE(my_madvise, madvise); INTERPOSE(my_ftruncate, ftruncate); INTERPOSE(my_fcntl, fcntl);
INTERPOSE(my_utimensat, utimensat); INTERPOSE(my_futimens, futimens); INTERPOSE(my_fchmod, fchmod); INTERPOSE(my_chmod, chmod);

__attribute__((constructor)) static void begin(void) { started = mach_absolute_time(); }
__attribute__((destructor)) static void report(void) {
    const char *path = getenv("SYSCOUNT_OUT");
    FILE *out = path ? fopen(path, "a") : stderr;
    if (!out) return;
    mach_timebase_info_data_t tb; mach_timebase_info(&tb);
    unsigned long long total = mach_absolute_time() - started;
    fprintf(out, "process_ms %.3f\n", total * tb.numer / tb.denom / 1e6);
    for (int k = 0; k < KINDS; k++) if (calls[k])
        fprintf(out, "%-11s calls %6llu  ms %8.3f  bytes %llu\n", names[k], calls[k], ticks[k] * tb.numer / tb.denom / 1e6, bytes[k]);
    if (path) fclose(out);
}
