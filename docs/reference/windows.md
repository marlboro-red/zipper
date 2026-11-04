# Zipper for Windows (early release)

Zipper for Windows is a portable command-line program for 64-bit Intel and AMD
Windows. It is not a graphical installer.

## Install

1. Download the `windows-x86_64.zip` asset from the GitHub prerelease.
2. Extract it into a folder.
3. Open PowerShell in that folder.

## Verify the download

`SHA256SUMS.txt` contains the package checksum. To verify the downloaded ZIP, run:

```powershell
Get-FileHash .\zipper-0.1.0-alpha.1-windows-x86_64.zip -Algorithm SHA256
```

## Run it

```powershell
.\zipper.exe --version
.\zipper.exe test files.zip
.\zipper.exe extract files.zip --output .\unzipped
.\zipper.exe extract source.tar.gz --output .\source
.\zipper.exe --zip-engine native-fast --threads 4 extract files.zip --output .\unzipped
```

Extraction needs a new destination folder whose parent already exists, or
`--overwrite` to merge into an existing one.

The native ZIP, tar and 7z readers work without Rust or 7-Zip.
The build statically links the MSVC runtime.

## Use 7-Zip for other formats

Other formats, and the default ZIP compatibility reader, need 7-Zip. If 7-Zip is
not on PATH, set `ZIPPER_7Z` to its location:

```powershell
$env:ZIPPER_7Z = 'C:\Program Files\7-Zip\7z.exe'
.\zipper.exe extract archive.7z --output .\restored7z
```

## What this release supports

This list describes the published early release. The current source tree no longer creates or reads `.zipper` archives, and it reads tar and 7z in process.


- Native archives preserve regular-file bytes and directories with UTF-8 paths.
  They do not restore permissions, timestamps, links, or extended attributes on Windows.
- Encryption is not supported.
- Native ZIP supports stored and Deflate ZIP, and conventional ZIP64.
- Native-fast can use up to 64 MiB of entry buffers per worker. For lower memory
  use, choose `--zip-engine native`.
- Expanded-size limits apply to native archives and the native ZIP engines, not to the 7-Zip backend.

This release has no GUI, no Explorer integration, no automatic update mechanism,
and no Windows ARM64 build.

## Mark of the Web

Windows marks a downloaded file with a `Zone.Identifier` stream, and warns before such a file runs. The current source tree copies the archive's stream onto every file it extracts, with every reader and the 7-Zip backend, as 7-Zip's `-snz` does. The mark is written in the hidden staging folder before anything is published, and each file keeps its modification time. Folders get no mark. A destination on a volume without named streams, such as FAT, gets no mark, as Windows itself drops it there. `tests/zone.rs` checks ZIP, tar, gzip tar and 7z. `--content-only` writes no mark, as [usage](../usage.md#extract-contents-only) describes.

## File attributes

The current source tree restores the read-only, hidden and system attributes of extracted files, as 7-Zip does. ZIP and 7z entries carry them in their attribute byte. An entry with a Unix mode that has no write bit, including every tar entry, becomes read-only. Overwriting replaces a read-only file in the destination. Folders keep default attributes. `tests/attributes.rs` compares each attribute with 7-Zip's own extraction of 7z, ZIP and tar archives. `--content-only` sets no attribute.

## Testing status

This unsigned alpha is tested on Windows Server 2022 in GitHub Actions. It has not
been benchmarked on Windows or validated across consumer Windows versions. Windows
can show an unknown-publisher warning. Treat Zipper as experimental software, and
keep your original data.

## Development builds

The measurements below come from source builds, not from the downloadable alpha.
The alpha asset has not been rebuilt as part of these experiments.

- [Preliminary Windows extraction and memory measurements](https://github.com/marlboro-red/zipper/blob/main/benchmarks/extraction-matrix/README.md)
  found a whole-buffer extraction regression. The regression prompted a
  conservative shared-buffer default.
- A [write-size experiment](https://github.com/marlboro-red/zipper/blob/main/benchmarks/native-zip/windows-write-size/README.md)
  then removed most of that penalty. It limits buffered output writes to 256 KiB.
  The default memory budget is unchanged.
