// Counts and times the file system calls of an unmodified program on macOS.
// Build:  clang -O2 -dynamiclib -o syscount.dylib trace_calls.c
// Use:    SYSCOUNT_OUT=/path/out.json DYLD_INSERT_LIBRARIES=syscount.dylib program ...
// It reads no source of the traced program. It wraps libc entry points with
// dyld interposing, so time inside a call is wall time of the calling thread,
// summed over threads. Calls made inside libSystem itself are not seen.
#include <fcntl.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/attr.h>
#include <sys/clonefile.h>
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/uio.h>
#include <sys/xattr.h>
#include <unistd.h>

#define NAMES(X) \
  X(open_create) X(open_other) X(openat_create) X(openat_other) X(close) X(write) X(pwrite) X(writev) \
  X(read) X(pread) X(lseek) X(fstat) X(stat) X(lstat) X(fstatat) X(mkdir) X(mkdirat) X(rename) \
  X(renameat) X(renamex_np) X(renameatx_np) X(unlink) X(unlinkat) X(rmdir) X(chmod) X(fchmod) X(fchmodat) \
  X(chown) X(fchown) X(lchown) X(fchownat) X(utimes) X(futimes) X(lutimes) X(futimens) X(utimensat) \
  X(setattrlist) X(fsetattrlist) X(setattrlistat) X(getattrlist) X(fgetattrlist) X(getattrlistat) \
  X(getattrlistbulk) X(fcntl) X(ftruncate) X(fsync) X(symlink) X(symlinkat) X(link) X(readlink) \
  X(access) X(faccessat) X(chdir) X(fchdir) X(setxattr) X(fsetxattr) X(getxattr) X(fgetxattr) \
  X(clonefile) X(mmap) X(munmap) X(cond_wait) X(cond_timedwait) X(fchflags) X(chflags) X(lchflags) \
  X(statfs) X(fstatfs) X(pathconf) X(fpathconf) X(umask) X(dup) X(pipe)

enum {
#define X(name) K_##name,
  NAMES(X)
#undef X
  K_COUNT
};
static const char *const names[] = {
#define X(name) #name,
    NAMES(X)
#undef X
};
static _Atomic unsigned long long calls[K_COUNT], ticks[K_COUNT], bytes[K_COUNT];
static unsigned long long started;

#define INTERPOSE(replacement, original) \
  __attribute__((used)) static struct { const void *r, *o; } interpose_##replacement \
      __attribute__((section("__DATA,__interpose"))) = {(const void *)replacement, (const void *)original}
#define TIMED(kind, type, call) \
  unsigned long long before = mach_absolute_time(); \
  type result = call; \
  atomic_fetch_add_explicit(&ticks[kind], mach_absolute_time() - before, memory_order_relaxed); \
  atomic_fetch_add_explicit(&calls[kind], 1, memory_order_relaxed)

static int my_open(const char *path, int flags, ...) {
  va_list list; va_start(list, flags);
  int mode = (flags & O_CREAT) ? va_arg(list, int) : 0;
  va_end(list);
  int kind = (flags & O_CREAT) ? K_open_create : K_open_other;
  TIMED(kind, int, open(path, flags, mode));
  return result;
}
INTERPOSE(my_open, open);
static int my_openat(int at, const char *path, int flags, ...) {
  va_list list; va_start(list, flags);
  int mode = (flags & O_CREAT) ? va_arg(list, int) : 0;
  va_end(list);
  int kind = (flags & O_CREAT) ? K_openat_create : K_openat_other;
  TIMED(kind, int, openat(at, path, flags, mode));
  return result;
}
INTERPOSE(my_openat, openat);
extern int open_nocancel(const char *, int, ...) __asm("_open$NOCANCEL");
static int my_open_nocancel(const char *path, int flags, ...) {
  va_list list; va_start(list, flags);
  int mode = (flags & O_CREAT) ? va_arg(list, int) : 0;
  va_end(list);
  int kind = (flags & O_CREAT) ? K_open_create : K_open_other;
  TIMED(kind, int, open_nocancel(path, flags, mode));
  return result;
}
INTERPOSE(my_open_nocancel, open_nocancel);
extern int openat_nocancel(int, const char *, int, ...) __asm("_openat$NOCANCEL");
static int my_openat_nocancel(int at, const char *path, int flags, ...) {
  va_list list; va_start(list, flags);
  int mode = (flags & O_CREAT) ? va_arg(list, int) : 0;
  va_end(list);
  int kind = (flags & O_CREAT) ? K_openat_create : K_openat_other;
  TIMED(kind, int, openat_nocancel(at, path, flags, mode));
  return result;
}
INTERPOSE(my_openat_nocancel, openat_nocancel);
static int my_fcntl(int fd, int command, ...) {
  va_list list; va_start(list, command);
  void *argument = va_arg(list, void *);
  va_end(list);
  TIMED(K_fcntl, int, fcntl(fd, command, argument));
  return result;
}
INTERPOSE(my_fcntl, fcntl);

#define WRAP(kind, type, name, parameters, arguments) \
  static type my_##kind parameters { TIMED(K_##kind, type, name arguments); return result; } \
  INTERPOSE(my_##kind, name)
#define WRAP_BYTES(kind, name, parameters, arguments) \
  static ssize_t my_##kind parameters { \
    TIMED(K_##kind, ssize_t, name arguments); \
    if (result > 0) atomic_fetch_add_explicit(&bytes[K_##kind], result, memory_order_relaxed); \
    return result; } \
  INTERPOSE(my_##kind, name)

WRAP(close, int, close, (int fd), (fd));
extern int close_nocancel(int) __asm("_close$NOCANCEL");
static int my_close_nocancel(int fd) { TIMED(K_close, int, close_nocancel(fd)); return result; }
INTERPOSE(my_close_nocancel, close_nocancel);
WRAP_BYTES(write, write, (int fd, const void *data, size_t size), (fd, data, size));
extern ssize_t write_nocancel(int, const void *, size_t) __asm("_write$NOCANCEL");
static ssize_t my_write_nocancel(int fd, const void *data, size_t size) {
  TIMED(K_write, ssize_t, write_nocancel(fd, data, size));
  if (result > 0) atomic_fetch_add_explicit(&bytes[K_write], result, memory_order_relaxed);
  return result;
}
INTERPOSE(my_write_nocancel, write_nocancel);
WRAP_BYTES(pwrite, pwrite, (int fd, const void *data, size_t size, off_t at), (fd, data, size, at));
WRAP_BYTES(writev, writev, (int fd, const struct iovec *parts, int count), (fd, parts, count));
WRAP_BYTES(read, read, (int fd, void *data, size_t size), (fd, data, size));
extern ssize_t read_nocancel(int, void *, size_t) __asm("_read$NOCANCEL");
static ssize_t my_read_nocancel(int fd, void *data, size_t size) {
  TIMED(K_read, ssize_t, read_nocancel(fd, data, size));
  if (result > 0) atomic_fetch_add_explicit(&bytes[K_read], result, memory_order_relaxed);
  return result;
}
INTERPOSE(my_read_nocancel, read_nocancel);
WRAP_BYTES(pread, pread, (int fd, void *data, size_t size, off_t at), (fd, data, size, at));
WRAP(lseek, off_t, lseek, (int fd, off_t at, int whence), (fd, at, whence));
WRAP(fstat, int, fstat, (int fd, struct stat *out), (fd, out));
WRAP(stat, int, stat, (const char *path, struct stat *out), (path, out));
WRAP(lstat, int, lstat, (const char *path, struct stat *out), (path, out));
WRAP(fstatat, int, fstatat, (int at, const char *path, struct stat *out, int flags), (at, path, out, flags));
WRAP(mkdir, int, mkdir, (const char *path, mode_t mode), (path, mode));
WRAP(mkdirat, int, mkdirat, (int at, const char *path, mode_t mode), (at, path, mode));
WRAP(rename, int, rename, (const char *from, const char *to), (from, to));
WRAP(renameat, int, renameat, (int a, const char *from, int b, const char *to), (a, from, b, to));
WRAP(renamex_np, int, renamex_np, (const char *from, const char *to, unsigned flags), (from, to, flags));
WRAP(renameatx_np, int, renameatx_np, (int a, const char *from, int b, const char *to, unsigned flags),
     (a, from, b, to, flags));
WRAP(unlink, int, unlink, (const char *path), (path));
WRAP(unlinkat, int, unlinkat, (int at, const char *path, int flags), (at, path, flags));
WRAP(rmdir, int, rmdir, (const char *path), (path));
WRAP(chmod, int, chmod, (const char *path, mode_t mode), (path, mode));
WRAP(fchmod, int, fchmod, (int fd, mode_t mode), (fd, mode));
WRAP(fchmodat, int, fchmodat, (int at, const char *path, mode_t mode, int flags), (at, path, mode, flags));
WRAP(chown, int, chown, (const char *path, uid_t user, gid_t group), (path, user, group));
WRAP(fchown, int, fchown, (int fd, uid_t user, gid_t group), (fd, user, group));
WRAP(lchown, int, lchown, (const char *path, uid_t user, gid_t group), (path, user, group));
WRAP(fchownat, int, fchownat, (int at, const char *path, uid_t user, gid_t group, int flags),
     (at, path, user, group, flags));
WRAP(utimes, int, utimes, (const char *path, const struct timeval *times), (path, times));
WRAP(futimes, int, futimes, (int fd, const struct timeval *times), (fd, times));
WRAP(lutimes, int, lutimes, (const char *path, const struct timeval *times), (path, times));
WRAP(futimens, int, futimens, (int fd, const struct timespec *times), (fd, times));
WRAP(utimensat, int, utimensat, (int at, const char *path, const struct timespec *times, int flags),
     (at, path, times, flags));
WRAP(setattrlist, int, setattrlist,
     (const char *path, void *list, void *buffer, size_t size, unsigned int options),
     (path, list, buffer, size, options));
WRAP(fsetattrlist, int, fsetattrlist, (int fd, void *list, void *buffer, size_t size, unsigned int options),
     (fd, list, buffer, size, options));
WRAP(setattrlistat, int, setattrlistat,
     (int at, const char *path, void *list, void *buffer, size_t size, uint32_t options),
     (at, path, list, buffer, size, options));
WRAP(getattrlist, int, getattrlist,
     (const char *path, void *list, void *buffer, size_t size, unsigned int options),
     (path, list, buffer, size, options));
WRAP(fgetattrlist, int, fgetattrlist, (int fd, void *list, void *buffer, size_t size, unsigned int options),
     (fd, list, buffer, size, options));
WRAP(getattrlistat, int, getattrlistat,
     (int at, const char *path, void *list, void *buffer, size_t size, unsigned long options),
     (at, path, list, buffer, size, options));
WRAP(getattrlistbulk, int, getattrlistbulk,
     (int fd, void *list, void *buffer, size_t size, uint64_t options), (fd, list, buffer, size, options));
WRAP(ftruncate, int, ftruncate, (int fd, off_t size), (fd, size));
WRAP(fsync, int, fsync, (int fd), (fd));
WRAP(symlink, int, symlink, (const char *target, const char *path), (target, path));
WRAP(symlinkat, int, symlinkat, (const char *target, int at, const char *path), (target, at, path));
WRAP(link, int, link, (const char *from, const char *to), (from, to));
WRAP(readlink, ssize_t, readlink, (const char *path, char *out, size_t size), (path, out, size));
WRAP(access, int, access, (const char *path, int mode), (path, mode));
WRAP(faccessat, int, faccessat, (int at, const char *path, int mode, int flags), (at, path, mode, flags));
WRAP(chdir, int, chdir, (const char *path), (path));
WRAP(fchdir, int, fchdir, (int fd), (fd));
WRAP(setxattr, int, setxattr,
     (const char *path, const char *name, const void *value, size_t size, u_int32_t at, int options),
     (path, name, value, size, at, options));
WRAP(fsetxattr, int, fsetxattr,
     (int fd, const char *name, const void *value, size_t size, u_int32_t at, int options),
     (fd, name, value, size, at, options));
WRAP(getxattr, ssize_t, getxattr,
     (const char *path, const char *name, void *value, size_t size, u_int32_t at, int options),
     (path, name, value, size, at, options));
WRAP(fgetxattr, ssize_t, fgetxattr,
     (int fd, const char *name, void *value, size_t size, u_int32_t at, int options),
     (fd, name, value, size, at, options));
WRAP(clonefile, int, clonefile, (const char *from, const char *to, uint32_t flags), (from, to, flags));
WRAP(mmap, void *, mmap, (void *at, size_t size, int protection, int flags, int fd, off_t offset),
     (at, size, protection, flags, fd, offset));
WRAP(munmap, int, munmap, (void *at, size_t size), (at, size));
WRAP(cond_wait, int, pthread_cond_wait, (pthread_cond_t *condition, pthread_mutex_t *mutex), (condition, mutex));
WRAP(cond_timedwait, int, pthread_cond_timedwait,
     (pthread_cond_t *condition, pthread_mutex_t *mutex, const struct timespec *until),
     (condition, mutex, until));
WRAP(fchflags, int, fchflags, (int fd, unsigned int flags), (fd, flags));
WRAP(chflags, int, chflags, (const char *path, unsigned int flags), (path, flags));
WRAP(lchflags, int, lchflags, (const char *path, unsigned int flags), (path, flags));
WRAP(statfs, int, statfs, (const char *path, struct statfs *out), (path, out));
WRAP(fstatfs, int, fstatfs, (int fd, struct statfs *out), (fd, out));
WRAP(pathconf, long, pathconf, (const char *path, int name), (path, name));
WRAP(fpathconf, long, fpathconf, (int fd, int name), (fd, name));
WRAP(umask, mode_t, umask, (mode_t mask), (mask));
WRAP(dup, int, dup, (int fd), (fd));
WRAP(pipe, int, pipe, (int fds[2]), (fds));

__attribute__((constructor)) static void begin(void) { started = mach_absolute_time(); }

__attribute__((destructor)) static void report(void) {
  unsigned long long ended = mach_absolute_time();
  const char *path = getenv("SYSCOUNT_OUT");
  if (!path) return;
  mach_timebase_info_data_t base;
  mach_timebase_info(&base);
  struct rusage usage;
  getrusage(RUSAGE_SELF, &usage);
  char text[16384];
  int used = snprintf(text, sizeof text,
      "{\"wall_us\": %llu, \"user_us\": %lld, \"system_us\": %lld, \"calls\": {",
      (ended - started) * base.numer / base.denom / 1000,
      (long long)usage.ru_utime.tv_sec * 1000000 + usage.ru_utime.tv_usec,
      (long long)usage.ru_stime.tv_sec * 1000000 + usage.ru_stime.tv_usec);
  int first = 1;
  for (int kind = 0; kind < K_COUNT; kind++) {
    unsigned long long count = atomic_load(&calls[kind]);
    if (!count) continue;
    used += snprintf(text + used, sizeof text - used, "%s\"%s\": {\"count\": %llu, \"us\": %llu, \"bytes\": %llu}",
        first ? "" : ", ", names[kind], count,
        atomic_load(&ticks[kind]) * base.numer / base.denom / 1000, atomic_load(&bytes[kind]));
    first = 0;
  }
  used += snprintf(text + used, sizeof text - used, "}}\n");
  int fd = open(path, O_WRONLY | O_CREAT | O_APPEND, 0644);
  if (fd >= 0) { (void)write(fd, text, used); close(fd); }
}
