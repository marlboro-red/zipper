# Windows early release

Download the `windows-x86_64.zip` asset from the GitHub prerelease and extract it
into a folder. This is a portable command-line program for 64-bit Intel/AMD
Windows, not a graphical installer. Open PowerShell in that folder:

```powershell
.\zipper.exe --version
.\zipper.exe create backup.zipper .\data
.\zipper.exe test backup.zipper
.\zipper.exe extract backup.zipper --output .\restored
.\zipper.exe --zip-engine native-fast --threads 4 extract files.zip --output .\unzipped
```

Put the archive outside the input folder. Extraction requires a new destination
folder whose parent already exists. The native archive and native ZIP readers
work without installing Rust or 7-Zip. The build statically links the MSVC runtime.

For other formats, or the default ZIP compatibility reader, install 7-Zip and
point Zipper to it if it is not on PATH:

```powershell
$env:ZIPPER_7Z = 'C:\Program Files\7-Zip\7z.exe'
.\zipper.exe extract archive.7z --output .\restored7z
```

This unsigned alpha is tested on Windows Server 2022 in GitHub Actions. It has
not been benchmarked on Windows or validated across consumer Windows versions.
Windows may show an unknown-publisher warning. There is no GUI, Explorer
integration, automatic update mechanism, or Windows ARM64 build in this release.

Native archives preserve regular-file bytes and directories with UTF-8 paths;
they do not preserve permissions, timestamps, links, or extended attributes.
Encryption is not supported. Native ZIP supports stored/Deflate ZIP and
conventional ZIP64. Native-fast can use up to 64 MiB of entry buffers per worker;
use `--zip-engine native` for lower memory use. Expanded-size quotas are not
implemented. Treat this as experimental software and retain original data.

`SHA256SUMS.txt` contains the package checksum. Verify the downloaded ZIP with:

```powershell
Get-FileHash .\zipper-0.1.0-alpha.1-windows-x86_64.zip -Algorithm SHA256
```
