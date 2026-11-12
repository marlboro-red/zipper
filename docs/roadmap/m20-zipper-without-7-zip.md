# M20. Zipper without 7-Zip

No code path in the library or the CLI runs 7-Zip. A format with a reader always goes to that reader, and a format with none fails with `zipper::Refused`.

On 13 November 2026 the user set this goal, in the words "we should be aiming to remove all 7-zip delegation". It is not 7-Zip parity, which the user set aside on 12 November 2026. 7-Zip stays only as a test oracle that `tests/common/sevenzip.rs` finds. The rows that [M32](m32-every-reader-under-zippers-licence.md) refuses for licence reasons need no reader here.

Scope:

- Send every file that has a reader to it, including the archives the reader refuses for a feature it lacks.
- Refuse through `src/refused.rs` every file that has no reader, with `Zipper does not read`, followed by the format's name.
- Remove the backend. That is `--zip-engine`, `zipper::Engine`, `Reader::Backend`, `zipper::compat` and every 7-Zip lookup outside the tests. Zipper is alpha, so the library loses them with no deprecation.
- Ship the Windows package without 7-Zip.
- Describe the result in `docs/usage.md`, the reader references and the Windows notes.

## Every path that reached 7-Zip

Each row is a case that started 7-Zip before this milestone, and where it goes now.

| Case | Why it reached 7-Zip | Reader now |
| --- | --- | --- |
| `--zip-engine backend` | It sent every file with a known signature to 7-Zip | Removed. clap rejects the option as unknown |
| A file in `PINNED_FORMATS` that no reader's sniff took | The default engine fell back to 7-Zip for 7z, RAR, gzip, bzip2, xz and CAB signatures | That format's reader, which refuses the damaged file |
| A RAR whose first headers fail to open | `native_rar::needs_backend` took any open error, so volumes, encrypted headers, RAR 4 solid archives and damaged headers all went to 7-Zip | The RAR reader, which refuses each by name |
| A 7z with a method the reader does not decode | `native_7z::needs_backend`, for a coder layout, BCJ2 or PPMd memory over the 2 GiB budget, or properties | The 7z reader, which refuses it by the method's name |
| A Mach-O universal file with the `B9 FA F1 0E` signature | The exe reader has no reader for it | Refused with `Zipper does not read Mach-O universal files with the B9 FA F1 0E signature` |
| SquashFS versions 1 to 3, big-endian, or swapped vendor signatures | The SquashFS reader reads only 4.0 in little-endian order | Refused with `Zipper does not read SquashFS images other than version 4.0 in little-endian order` |
| An ext volume with a feature Zipper does not read | `native_ext::needs_backend`, for journal recovery, encryption, compression and other flags | The ext reader, which refuses it by the feature's name |
| An LP image whose partition names are not plain relative paths | `native_lp::needs_backend`, since 7-Zip rewrites such names | The LP reader, which refuses the unsafe path |
| UEFI firmware whose names are not plain relative paths, or longer than 4,096 bytes | `native_uefi::needs_backend`, the `uefic` and `uefif` rows of `ROUTED_TYPES` | The UEFI reader, which refuses the unsafe path |
| Intel HEX in a build without the `ihex` feature | The `ihex` row of `ROUTED_TYPES` | `zipper::NotBuilt` naming `ihex` |
| A format whose feature the build leaves out | The default engine sent a pinned signature to 7-Zip | `zipper::NotBuilt` naming the format |
| A split `*.001` chain | `pinned_format` added `.split` for a set Zipper did not read | Already joined by `src/volumes.rs` for every reader, so no reader needed 7-Zip for it |

The `lp`, `uefic`, `uefif` and `ihex` rows of `ROUTED_TYPES` each had a reader already. Before this milestone a fake 7-Zip on `PATH` started for 28 files in `tests/data`, every one a RAR in `tests/data/rar-corpus`. Now it starts for none.

## Exit checks

1. `tests/no_7zip.rs` runs `zipper test` on every file in `tests/data`, and on each `.xz` file unpacked. A fake 7-Zip first on `PATH` and named by `ZIPPER_7Z` must never start. It also refuses each case in the table above by its reader's reason or by `zipper::Refused`, and checks that `--zip-engine` is an unknown option.
2. No code under `src/` starts a process outside its tests, since the backend module `compat.rs` is gone.
3. `scripts/package_windows.py` packages `zipper.exe` with no 7-Zip, and its `BUILD.json` says so.
4. The release checks run the full M10 corpus on all three operating systems, on machines with no 7-Zip installed.

## Done so far, by 13 November 2026

- Exit checks 1 to 3 pass on the branch that removed the backend.
- `tests/refused.rs`, `tests/parity.rs` and `tests/cli.rs` test the one reader each file gets, with no engine to choose.
- Error messages no longer suggest `--zip-engine backend`.
- Exit check 4 remains. It needs the release checks run on Linux and Windows machines without 7-Zip.
