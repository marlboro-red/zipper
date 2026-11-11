#!/usr/bin/env python3
"""Copy libarchive's RAR test archives into tests/data/rar-corpus and record
the tree each one extracts to.

Usage: record_rar_corpus.py <folder of decoded libarchive test archives>

libarchive keeps its test archives uuencoded in libarchive/test/. Decode
them into a folder first. 7-Zip as Homebrew builds it has no RAR codec, so
the tree comes from `7zz x` where 7-Zip extracts the archive, and from
bsdtar, libarchive's own extractor used as a black box, where it does not.
RAR 4 keeps DOS times in local time, which each tool converts by its own
rule, so RAR 4 trees record no times. bsdtar on macOS writes names in
NFD, and the tree keeps them in NFC, as the archives store them. A RAR 5 version record names a file
`name;N` in bsdtar and `[VER]/N/name` in 7-Zip, and the tree keeps 7-Zip's
name.
"""
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unicodedata

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS = os.path.join(ROOT, "tests", "data", "rar-corpus")

GOOD = [
    "test_read_format_rar5_arm.rar",
    "test_read_format_rar5_blake2.rar",
    "test_read_format_rar5_compressed.rar",
    "test_read_format_rar5_extra_field_version.rar",
    "test_read_format_rar5_fileattr.rar",
    "test_read_format_rar5_hardlink.rar",
    "test_read_format_rar5_multiple_files.rar",
    "test_read_format_rar5_multiple_files_solid.rar",
    "test_read_format_rar5_owner.rar",
    "test_read_format_rar5_solid.rar",
    "test_read_format_rar5_stored.rar",
    "test_read_format_rar5_stored_manyfiles.rar",
    "test_read_format_rar5_symlink.rar",
    "test_read_format_rar5_unicode.rar",
    "test_read_format_rar5_win32.rar",
    "test_read_format_rar5_zip_in_rar.rar",
    "test_read_format_rar.rar",
    "test_read_format_rar_compress_best.rar",
    "test_read_format_rar_compress_normal.rar",
    "test_read_format_rar_filter.rar",
    "test_read_format_rar_multi_lzss_blocks.rar",
    "test_read_format_rar_noeof.rar",
    "test_read_format_rar_subblock.rar",
    "test_read_format_rar_unicode.rar",
    "test_read_format_rar_windows.rar",
]

# The bsdtar on this machine predates libarchive's fix for this archive, so
# its tree is the one libarchive's test expects.
FROM_LIBARCHIVE_TESTS = {
    "test_read_format_rar5_main_block_extra_bytes.rar": {
        "helloworld.txt": {
            "sha256": hashlib.sha256(b"hello libarchive test suite!\n").hexdigest()
        }
    },
    "test_read_format_rar3_lowdist_reset.rar": {
        "lowdist-reset.bin": {
            "sha256": hashlib.sha256(
                bytes(range(0x28))
                + bytes([2, 3, 4, 5, 6, 7, 0x0D, 0x0E, 0x0F])
                + bytes(range(0x28, 0x37))
            ).hexdigest()
        }
    },
}

BAD = [
    "test_read_format_rar5_arm_filter_on_window_boundary.rar",
    "test_read_format_rar5_bad_tables.rar",
    "test_read_format_rar5_bad_window_sz_in_mltarc_file.rar",
    "test_read_format_rar5_block_size_is_too_small.rar",
    "test_read_format_rar5_bytes_remaining_underflow.rar",
    "test_read_format_rar5_decode_number_out_of_bounds_read.rar",
    "test_read_format_rar5_different_solid_window_size.rar",
    "test_read_format_rar5_different_window_size.rar",
    "test_read_format_rar5_different_winsize_on_merge.rar",
    "test_read_format_rar5_distance_overflow.rar",
    "test_read_format_rar5_extra_field_underflow.rar",
    "test_read_format_rar5_group_name_toolong.rar",
    "test_read_format_rar5_invalid_dict_reference.rar",
    "test_read_format_rar5_leftshift1.rar",
    "test_read_format_rar5_leftshift2.rar",
    "test_read_format_rar5_loop_bug.rar",
    "test_read_format_rar5_nonempty_dir_stream.rar",
    "test_read_format_rar5_owner_name_toolong.rar",
    "test_read_format_rar5_readtables_overflow.rar",
    "test_read_format_rar5_redir_overflow.rar",
    "test_read_format_rar5_redir_varint_2byte.rar",
    "test_read_format_rar5_skip_block_extra_bytes.rar",
    "test_read_format_rar5_truncated_huff.rar",
    "test_read_format_rar5_unpacked_size_exceeds_declared.rar",
    "test_read_format_rar5_window_buf_and_size_desync.rar",
    "test_read_format_rar_endarc_huge.rar",
    "test_read_format_rar_invalid1.rar",
    "test_read_format_rar_overflow.rar",
    "test_read_format_rar_ppmd_use_after_free.rar",
    "test_read_format_rar_ppmd_use_after_free2.rar",
    "test_read_format_rar_seek_data_cursor0.rar",
    "test_read_format_rar_symlink_huge.rar",
    "test_read_format_rar_unbound_staticdata.rar",
]

UNSUPPORTED = [
    "test_read_format_rar5_encrypted.rar",
    "test_read_format_rar5_encrypted_filenames.rar",
    "test_read_format_rar5_multiarchive.part01.rar",
    "test_read_format_rar_encryption_data.rar",
    "test_read_format_rar_encryption_header.rar",
    "test_read_format_rar_encryption_partially.rar",
    "test_read_format_rar4_encrypted.rar",
    "test_read_format_rar_multivolume.part0001.rar",
    "test_read_format_rar_ppmd_lzss_conversion.rar",
]

# Neither oracle here extracts these, and each file's CRC-32 is the only
# check of what Zipper extracts.
CHECKED_BY_CRC_ONLY = [
    "test_read_format_rar_filter_incomplete_block.rar",
]


def seven_zip_name(name):
    base, sep, version = name.rpartition(";")
    if sep and version.isdigit():
        return f"[VER]/{version}/{base}"
    return name


def tree(root):
    found = {}
    for base, folders, files in os.walk(root):
        for name in folders + files:
            path = os.path.join(base, name)
            key = unicodedata.normalize("NFC", seven_zip_name(os.path.relpath(path, root)))
            info = os.lstat(path)
            if stat.S_ISLNK(info.st_mode):
                found[key] = {"link": os.readlink(path)}
            elif stat.S_ISDIR(info.st_mode):
                found[key] = {"dir": True}
            else:
                with open(path, "rb") as file:
                    digest = hashlib.sha256(file.read()).hexdigest()
                found[key] = {
                    "sha256": digest,
                    "mode": oct(info.st_mode & 0o777),
                    "mtime": int(info.st_mtime),
                }
    return found


def main():
    source = sys.argv[1]
    os.makedirs(CORPUS, exist_ok=True)
    trees = {
        "good": {},
        "bad": BAD,
        "unsupported": UNSUPPORTED,
        "crc_only": CHECKED_BY_CRC_ONLY,
    }
    trees["good"].update(FROM_LIBARCHIVE_TESTS)
    for name in GOOD + BAD + UNSUPPORTED + CHECKED_BY_CRC_ONLY + list(FROM_LIBARCHIVE_TESTS):
        shutil.copyfile(os.path.join(source, name), os.path.join(CORPUS, name))
    for name in GOOD:
        with tempfile.TemporaryDirectory() as work:
            archive = os.path.join(CORPUS, name)
            seven_zip = subprocess.run(
                ["7zz", "x", "-y", "-o" + work, archive], capture_output=True
            )
            if seven_zip.returncode != 0:
                shutil.rmtree(work)
                os.mkdir(work)
                subprocess.run(["bsdtar", "-xpf", archive, "-C", work], check=True)
            found = tree(work)
            if "rar5" not in name:
                for node in found.values():
                    node.pop("mtime", None)
            trees["good"][name] = found
            for base, folders, _ in os.walk(work):
                for folder in folders:
                    os.chmod(os.path.join(base, folder), 0o755)
    with open(os.path.join(CORPUS, "trees.json"), "w") as out:
        json.dump(trees, out, indent=1, sort_keys=True)
        out.write("\n")


if __name__ == "__main__":
    main()
