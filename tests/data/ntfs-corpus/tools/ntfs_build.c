/* Populate an NTFS image made by mkntfs from a script on stdin, with libntfs-3g.
 *
 * Commands, one per line, fields separated by single spaces:
 *   dir PATH                 create a folder
 *   cdir PATH                create a folder whose new files are LZNT1 compressed
 *   file PATH SIZE SEED KIND write SIZE bytes; KIND is r (random) or t (text-like)
 *   sparse PATH SIZE OFF:LEN,... write random data only in the ranges
 *   frag PATH1 PATH2 SIZE CHUNK SEED  write two files in alternating chunks
 *   link OLD NEW             hard link NEW to OLD
 *   dos PATH SHORT           give PATH the DOS name SHORT
 *   ads PATH NAME SIZE SEED  add a named data stream
 *   symlink PATH TARGET      create a symbolic link (reparse point)
 *   reparse PATH HEXBYTES    set raw reparse data on an existing file
 *   time PATH SECONDS        set every time of PATH to SECONDS since 1970
 *   attrib PATH HEX          set the attribute flags (low 16 bits) of PATH
 *   delete PATH              delete PATH
 */
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ntfs-3g/types.h>
#include <ntfs-3g/volume.h>
#include <ntfs-3g/dir.h>
#include <ntfs-3g/attrib.h>
#include <ntfs-3g/inode.h>
#include <ntfs-3g/unistr.h>
#include <ntfs-3g/reparse.h>
#include <ntfs-3g/ntfstime.h>
#include <ntfs-3g/security.h>
#include <sys/stat.h>

static ntfs_volume *vol;

static void die(const char *what, const char *path)
{
	fprintf(stderr, "ntfs_build: %s %s: %s\n", what, path, strerror(errno));
	exit(1);
}

static unsigned long long state;
static unsigned char next_byte(int kind)
{
	state ^= state << 13;
	state ^= state >> 7;
	state ^= state << 17;
	if (kind == 't') {
		static const char words[] = "the quick brown fox jumps over a lazy dog and then rests ";
		if ((state & 31) == 0)
			return (unsigned char)('A' + (state >> 8) % 26);
		return (unsigned char)words[(state >> 16) % 9 == 0 ? (state >> 24) % (sizeof words - 1) : 0];
	}
	return (unsigned char)(state >> 24);
}

static unsigned char *bytes(long long size, unsigned long long seed, int kind)
{
	unsigned char *b = malloc(size ? size : 1);
	long long i;
	state = seed * 2654435761ULL + 1;
	if (kind == 't') {
		static const char text[] = "Zipper reads NTFS volumes. The quick brown fox jumps over the lazy dog. ";
		for (i = 0; i < size; i++) {
			b[i] = (unsigned char)text[i % (sizeof text - 1)];
			if (next_byte('r') < 8)
				b[i] = (unsigned char)('a' + next_byte('r') % 26);
		}
	} else {
		for (i = 0; i < size; i++)
			b[i] = next_byte('r');
	}
	return b;
}

static ntfs_inode *open_path(const char *path)
{
	ntfs_inode *ni = ntfs_pathname_to_inode(vol, NULL, path);
	if (!ni)
		die("open", path);
	return ni;
}

static void split(const char *path, char *parent, const char **name)
{
	const char *slash = strrchr(path, '/');
	if (!slash || slash == path) {
		strcpy(parent, "/");
		*name = slash ? slash + 1 : path;
	} else {
		memcpy(parent, path, slash - path);
		parent[slash - path] = 0;
		*name = slash + 1;
	}
}

static ntfs_inode *create(const char *path, mode_t type)
{
	char parent[4096];
	const char *name;
	ntfschar *uname = NULL;
	int len;
	ntfs_inode *dir, *ni;
	split(path, parent, &name);
	dir = open_path(parent);
	len = ntfs_mbstoucs(name, &uname);
	if (len < 0)
		die("name", path);
	ni = ntfs_create(dir, const_cpu_to_le32(0), uname, len, type);
	if (!ni)
		die("create", path);
	free(uname);
	ntfs_inode_close(dir);
	return ni;
}

static void write_data(ntfs_inode *ni, ntfschar *sname, int slen, long long pos, long long size,
		const unsigned char *b, const char *path)
{
	ntfs_attr *na = ntfs_attr_open(ni, AT_DATA, sname, slen);
	long long done = 0;
	if (!na)
		die("attr open", path);
	while (done < size) {
		s64 w = ntfs_attr_pwrite(na, pos + done, size - done, b + done);
		if (w <= 0)
			die("write", path);
		done += w;
	}
	if (ntfs_attr_pclose(na))
		die("pclose", path);
	ntfs_attr_close(na);
}

static void truncate_to(ntfs_inode *ni, long long size, const char *path)
{
	ntfs_attr *na = ntfs_attr_open(ni, AT_DATA, AT_UNNAMED, 0);
	if (!na)
		die("attr open", path);
	if (ntfs_attr_truncate(na, size))
		die("truncate", path);
	ntfs_attr_close(na);
}

int main(int argc, char **argv)
{
	char line[65536];
	if (argc != 2) {
		fprintf(stderr, "usage: ntfs_build IMAGE < script\n");
		return 2;
	}
	vol = ntfs_mount(argv[1], 0);
	if (!vol)
		die("mount", argv[1]);
	NVolSetCompression(vol);
	while (fgets(line, sizeof line, stdin)) {
		char *argv2[8] = {0};
		int n = 0;
		char *s = strtok(line, " \n");
		while (s && n < 8) {
			argv2[n++] = s;
			s = strtok(NULL, " \n");
		}
		if (n == 0 || argv2[0][0] == '#')
			continue;
		const char *cmd = argv2[0], *path = argv2[1];
		if (!strcmp(cmd, "dir") || !strcmp(cmd, "cdir")) {
			ntfs_inode *ni = create(path, S_IFDIR);
			if (cmd[0] == 'c') {
				ni->flags |= FILE_ATTR_COMPRESSED;
				ntfs_inode_mark_dirty(ni);
			}
			ntfs_inode_close(ni);
		} else if (!strcmp(cmd, "file")) {
			long long size = atoll(argv2[2]);
			unsigned char *b = bytes(size, strtoull(argv2[3], 0, 10), argv2[4][0]);
			ntfs_inode *ni = create(path, S_IFREG);
			if (size)
				write_data(ni, AT_UNNAMED, 0, 0, size, b, path);
			free(b);
			ntfs_inode_close(ni);
		} else if (!strcmp(cmd, "sparse")) {
			long long size = atoll(argv2[2]);
			ntfs_inode *ni = create(path, S_IFREG);
			char *ranges = argv2[3], *r;
			unsigned long long seed = 7;
			for (r = strtok(ranges, ","); r; r = strtok(NULL, ",")) {
				long long off = atoll(r), len = atoll(strchr(r, ':') + 1);
				unsigned char *b = bytes(len, seed++, 'r');
				write_data(ni, AT_UNNAMED, 0, off, len, b, path);
				free(b);
			}
			truncate_to(ni, size, path);
			ntfs_inode_close(ni);
		} else if (!strcmp(cmd, "frag")) {
			long long size = atoll(argv2[3]), chunk = atoll(argv2[4]), off;
			int kind = argv2[6] ? argv2[6][0] : 'r';
			unsigned char *a = bytes(size, strtoull(argv2[5], 0, 10), kind);
			unsigned char *b = bytes(size, strtoull(argv2[5], 0, 10) + 1, kind);
			ntfs_inode *one = create(argv2[1], S_IFREG);
			ntfs_inode *two = create(argv2[2], S_IFREG);
			for (off = 0; off < size; off += chunk) {
				long long len = size - off < chunk ? size - off : chunk;
				write_data(one, AT_UNNAMED, 0, off, len, a + off, argv2[1]);
				write_data(two, AT_UNNAMED, 0, off, len, b + off, argv2[2]);
			}
			free(a);
			free(b);
			ntfs_inode_close(one);
			ntfs_inode_close(two);
		} else if (!strcmp(cmd, "link")) {
			char parent[4096];
			const char *name;
			ntfschar *uname = NULL;
			ntfs_inode *ni = open_path(argv2[1]);
			split(argv2[2], parent, &name);
			ntfs_inode *dir = open_path(parent);
			int len = ntfs_mbstoucs(name, &uname);
			if (ntfs_link(ni, dir, uname, len))
				die("link", argv2[2]);
			free(uname);
			ntfs_inode_close(dir);
			ntfs_inode_close(ni);
		} else if (!strcmp(cmd, "dos")) {
			char parent[4096];
			const char *name;
			split(path, parent, &name);
			ntfs_inode *ni = open_path(path);
			ntfs_inode *dir = open_path(parent);
			if (ntfs_set_ntfs_dos_name(ni, dir, argv2[2], strlen(argv2[2]), 0))
				die("dos name", path);
		} else if (!strcmp(cmd, "ads")) {
			ntfschar *uname = NULL;
			long long size = atoll(argv2[3]);
			int len = ntfs_mbstoucs(argv2[2], &uname);
			unsigned char *b = bytes(size, strtoull(argv2[4], 0, 10), 'r');
			ntfs_inode *ni = open_path(path);
			if (ntfs_attr_add(ni, AT_DATA, uname, len, NULL, 0))
				die("ads add", path);
			if (size)
				write_data(ni, uname, len, 0, size, b, path);
			free(b);
			free(uname);
			ntfs_inode_close(ni);
		} else if (!strcmp(cmd, "symlink")) {
			char parent[4096];
			const char *name;
			ntfschar *uname = NULL, *utarget = NULL;
			split(path, parent, &name);
			ntfs_inode *dir = open_path(parent);
			int len = ntfs_mbstoucs(name, &uname);
			int tlen = ntfs_mbstoucs(argv2[2], &utarget);
			ntfs_inode *ni = ntfs_create_symlink(dir, const_cpu_to_le32(0), uname, len, utarget, tlen);
			if (!ni)
				die("symlink", path);
			ntfs_inode_close(ni);
			ntfs_inode_close(dir);
		} else if (!strcmp(cmd, "reparse")) {
			const char *hex = argv2[2];
			size_t n2 = strlen(hex) / 2, i;
			char *raw = malloc(n2);
			for (i = 0; i < n2; i++) {
				unsigned v;
				sscanf(hex + 2 * i, "%2x", &v);
				raw[i] = (char)v;
			}
			ntfs_inode *ni = open_path(path);
			if (ntfs_set_ntfs_reparse_data(ni, raw, n2, 0))
				die("reparse", path);
			ntfs_inode_close(ni);
			free(raw);
		} else if (!strcmp(cmd, "time")) {
			struct timespec ts = { atoll(argv2[2]), 500 };
			ntfs_inode *ni = open_path(path);
			ntfs_time t = timespec2ntfs(ts);
			u64 times[3] = { t, t, t };
			if (ntfs_inode_set_times(ni, (const char *)times, 24, 0))
				die("times", path);
			ntfs_inode_close(ni);
		} else if (!strcmp(cmd, "attrib")) {
			ntfs_inode *ni = open_path(path);
			ni->flags = (ni->flags & ~const_cpu_to_le32(0xFFFF)) |
				cpu_to_le32(strtoul(argv2[2], 0, 16) & 0xFFFF);
			ntfs_inode_mark_dirty(ni);
			ntfs_inode_close(ni);
		} else if (!strcmp(cmd, "delete")) {
			char parent[4096];
			const char *name;
			ntfschar *uname = NULL;
			split(path, parent, &name);
			ntfs_inode *ni = open_path(path);
			ntfs_inode *dir = open_path(parent);
			int len = ntfs_mbstoucs(name, &uname);
			if (ntfs_delete(vol, path, ni, dir, uname, len))
				die("delete", path);
			free(uname);
		} else {
			fprintf(stderr, "ntfs_build: unknown command %s\n", cmd);
			return 1;
		}
	}
	if (ntfs_umount(vol, FALSE))
		die("umount", argv[1]);
	return 0;
}
