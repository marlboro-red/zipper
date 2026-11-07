# In-process RPM reader

Zipper extracts the files an RPM package installs with its own Rust reader. The default engine sends a file here when it starts with the RPM lead signature `ED AB EE DB`. A file that starts with a ZIP, tar, cpio or `ar` signature goes to that reader first. `--zip-engine native` also reads RPM in process, and `--zip-engine backend` hands it to 7-Zip.

## Supported packages

An RPM package is a 96-byte lead, a signature header, a main header and a compressed payload. The payload is a cpio archive of the files the package installs. The reader supports these features:

- Binary and source packages in the version 4 format, with a lead of version 3 or 4 and a signature in header form.
- Payloads compressed with gzip, bzip2, xz, LZMA or zstd. The main header's payload compressor tag names the compressor, and a package without that tag holds gzip, as `rpm` assumes. An xz payload decodes with Zipper's own xz reader, as [tar's xz](tar-reader.md#supported-archives) does.
- A payload in the newc cpio form, `070701`, or its CRC variant `070702`.
- Files, directories, symlinks that stay inside the destination, and hard links, by the rules of the [cpio reader](cpio-reader.md#supported-archives). `rpmbuild` writes each name with a leading `./`, which the reader removes.

The reader refuses these packages:

- A payload in the stripped cpio form `07070X`. `rpm` writes it for a package with a file of 4 GiB or more, and for every package in the version 6 format, which `rpm` 6 builds by default. 7-Zip does not unpack it either.
- A payload format other than `cpio`, such as the `drpm` of a delta package.
- A payload compressor other than the five above. The error names it.

## Metadata

The reader restores what the [cpio reader](cpio-reader.md#metadata) restores, from the cpio headers in the payload. Modes are masked to `0o777`, files get their modification time in whole seconds, and directory modes and times are applied after all files are written. It never restores owners, and it never reads the file list, the scripts or the dependencies in the main header.

## Safety checks

The reader checks the whole package before it publishes anything, and it publishes nothing on failure:

- The lead starts with `ED AB EE DB`, has version 3 or 4, and gives signature type 5, a signature in header form.
- Each header starts with `8E AD E8 01`. Its index count and data size stay under the limits below, which the reader checks before it reads the index. The whole header must lie inside the file.
- Every index entry in both headers has a type from 0 to 9. A string entry has a count of 1. A 16-bit, 32-bit or 64-bit entry starts at an offset aligned to its size. The entry's values, at their least size, end inside the header's data store.
- The signature header is padded to a multiple of 8 bytes.
- The payload format and compressor tags appear at most once, are strings, and end with a NUL within 64 bytes and within the data store.
- The payload starts with the signature of the compressor the header names. gzip starts with `1F 8B`, bzip2 with `BZh` and a level, xz with its six-byte magic and zstd with `28 B5 2F FD`. LZMA has no magic, so its first byte must be a properties byte below 225, the limit the LZMA decoder holds to.
- Nothing follows the payload's compressed stream. The gzip, bzip2, xz and zstd decoders refuse trailing data, and the reader checks that an LZMA stream ends where the file ends, so a second cpio cannot hide after it.
- An error from decoding the payload, such as a cut, a failed checksum or trailing data, says the RPM payload is damaged.
- The payload passes every check of the [cpio reader](cpio-reader.md#safety-checks). Absolute names, `..`, backslashes and NUL bytes are rejected, symlinks must stay inside the destination, and the archive must end with its `TRAILER!!!` entry.

The reader does not check header digests, payload digests or signatures. A damaged gzip, bzip2 or xz payload still fails its compressor's checksum, because the reader decodes the payload to its end. `rpmbuild` writes zstd payloads without a frame checksum, and an LZMA payload has none, so damage there is caught only when it breaks the stream or the cpio.

## Limits

| Limit | Value |
| --- | --- |
| Index entries in each header | at most 65,535 |
| Data store of each header | at most 256 MiB |
| Payload format and compressor tags | at most 64 bytes each |
| Largest xz or LZMA dictionary | the [tar reader's limit](tar-reader.md#limits), 256 MiB |
| Payload | the [cpio limits](cpio-reader.md#limits) |
| Total expanded size | must fit in free space, or `--max-extracted-bytes` |

The reader keeps no header in memory. It reads the index one entry at a time, keeps only the payload format and compressor entries, and seeks over each data store. So a package whose headers fill the limits costs no more memory than a small one. An xz or LZMA payload with a larger dictionary is refused, as the tar reader refuses it. One thread decodes the payload and writes each file in archive order through a 1 MiB buffer. `--threads` has no effect on RPM.

## Differences from 7-Zip

`tests/parity.rs` requires the trees of the RPM packages in the [parity corpus](parity.md), which `rpmbuild` wrote with each compressor, to match 7-Zip's. `tests/hostile_rpm.rs` compares packages with each compressor against 7-Zip where it is installed. The reader differs from 7-Zip 26.05 in these cases:

- 7-Zip writes the decompressed payload as one `.cpio` file, named from the package name, version, release and architecture. Zipper extracts the files that cpio holds, as `rpm2cpio` and `cpio -id` do. The parity corpus compares Zipper's tree with what 7-Zip extracts from the `.cpio` it wrote.
- 7-Zip writes the stripped `07070X` cpio of a version 6 package as a `.cpio` file it cannot open. Zipper refuses the package. `--zip-engine backend` writes that `.cpio`.
- Inside the payload, the [cpio differences from 7-Zip](cpio-reader.md#differences-from-7-zip) apply. A symlink whose target starts with `..` but stays inside is one 7-Zip skips and Zipper creates.

## Extraction options

`Existing`, `Control`, `paths`, `include`, `exclude`, `strip` and `metadata` work as in the [cpio reader](cpio-reader.md#extraction-options), on the payload's paths without their leading `./`. So select `./usr/share/a.txt` with `--path usr/share/a.txt`. A selected path that starts with `./` is refused, and the error gives the spelling to use. The leading `.` counts as one component for `strip`, as it does in the tar reader, so `--strip-components 3` writes `./usr/share/a.txt` as `a.txt`. The cancel flag is checked as the cpio reader checks it.

## Formal checks

Kani proves that the lead, header intro and index entry readers never panic on any bytes. It proves that every index entry they accept lies inside its data store and is aligned, and that every header stays under the limits. It also proves that the window read for a payload tag stays inside the store, that the tag reader stays inside its window, and that signature padding aligns every store. The proofs are under `[rpm-fields]` in `verification/coverage.txt`.

`tests/hostile_rpm.rs` holds one case per rejected layout. Each case must fail and leave nothing in the destination's parent, both when extracting and when only verifying. It cuts packages at every byte, through the lead, both headers and the payload. `tests/data/rpm/rpm6-format6.rpm` is a version 6 package that `rpmbuild` from `rpm` 6.1.0 wrote, with SHA-256 `a04a866c5bde79d5049920a9ce52659edea835335dd80fe1c1f8254b0b6a6ba6`, and the reader must refuse it by name.
