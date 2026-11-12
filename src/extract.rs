#[cfg(feature = "7z")]
use crate::native_7z;
#[cfg(feature = "ar")]
use crate::native_ar;
#[cfg(feature = "arj")]
use crate::native_arj;
#[cfg(feature = "cab")]
use crate::native_cab;
#[cfg(feature = "cpio")]
use crate::native_cpio;
#[cfg(feature = "cramfs")]
use crate::native_cramfs;
#[cfg(feature = "exe")]
use crate::native_exe;
#[cfg(feature = "ext")]
use crate::native_ext;
#[cfg(feature = "flv")]
use crate::native_flv;
#[cfg(feature = "iso")]
use crate::native_iso;
#[cfg(feature = "lp")]
use crate::native_lp;
#[cfg(feature = "lzh")]
use crate::native_lzh;
#[cfg(feature = "rar")]
use crate::native_rar;
#[cfg(feature = "rpm")]
use crate::native_rpm;
#[cfg(feature = "squashfs")]
use crate::native_squashfs;
#[cfg(feature = "swf")]
use crate::native_swf;
#[cfg(feature = "tar")]
use crate::native_tar;
#[cfg(feature = "uefi")]
use crate::native_uefi;
#[cfg(feature = "wim")]
use crate::native_wim;
#[cfg(feature = "xar")]
use crate::native_xar;
#[cfg(feature = "zip-deflate")]
use crate::native_zip;
use crate::{Control, Existing, compat, refused};
use anyhow::{Result, bail, ensure};
use std::{io::Read, path::Path};

/// The reader for an archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Engine {
    /// Native for ZIP, tar, cpio, `ar`, RPM, xar, ISO and UDF disc images,
    /// single compressed files, unrecognized files, CAB files and cabinet
    /// sets, LZH, ARJ, RAR, WIM and ESD images, raw ext2, ext3 and ext4
    /// volumes, FLV and SWF files, PE, COFF, TE, ELF and Mach-O files, raw
    /// SquashFS 4.0 and CramFS images, Android logical partition images, UEFI
    /// firmware volumes and capsules, Intel HEX files, and 7z whose methods it
    /// decodes. 7-Zip, with `-t` naming the
    /// format, for other 7z, a RAR volume, a RAR 4 solid archive or a RAR
    /// archive with encrypted headers, other SquashFS versions and byte orders,
    /// an ext volume that needs journal recovery or uses a feature Zipper does
    /// not read, a Mach-O universal file with the `B9 FA F1 0E` signature, and
    /// a logical partition image or UEFI firmware whose names are not plain
    /// relative paths. A split or pre-1.13 WIM fails. Every engine fails with
    /// [`Refused`] on the formats that type names, disk images among them, so
    /// none reaches 7-Zip.
    #[default]
    Auto,
    /// 7-Zip for every archive whose format Zipper names by its signature, such
    /// as 7z, RAR, ZIP, gzip, tar or ISO, with `-t` naming it, and for Intel
    /// HEX, named by its content. Any other file, and every CAB, WIM, DMG or
    /// disk image, fails with [`Refused`].
    Backend,
    /// Native for ZIP, tar, cpio, `ar`, RPM, xar, CAB, LZH, ARJ, RAR, disc
    /// images, WIM, ext, SquashFS 4.0, CramFS, FLV, SWF,
    /// executables, logical partition images, UEFI firmware, Intel HEX, 7z and
    /// single compressed files, and native ZIP for unrecognized files.
    Native,
}

/// The reader an archive goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reader {
    /// The native ZIP reader.
    Zip,
    /// The native reader for plain, gzip, zstd and xz tar.
    Tar,
    /// The native 7z reader.
    SevenZip,
    /// The native reader for binary, odc and newc cpio.
    Cpio,
    /// The native reader for `ar` archives and Debian packages.
    Ar,
    /// The native reader for the files an RPM package installs.
    Rpm,
    /// The native reader for xar archives and flat macOS packages.
    Xar,
    /// The native reader for CAB files and cabinet sets with stored, MSZIP
    /// and Quantum folders.
    Cab,
    /// The native reader for LZH (LHA) archives.
    Lzh,
    /// The native reader for ARJ archives.
    Arj,
    /// The native reader for RAR 5 and RAR 4 archives.
    Rar,
    /// The native reader for ISO 9660 and UDF disc images.
    Iso,
    /// The native reader for WIM and ESD images.
    Wim,
    /// The native reader for PE images, COFF objects, TE images, ELF files,
    /// Mach-O files and Mach-O universal files.
    Exe,
    /// The native reader for SquashFS 4.0 images.
    Squashfs,
    /// The native reader for CramFS images.
    Cramfs,
    /// The native reader for raw ext2, ext3 and ext4 volumes.
    Ext,
    /// The native reader for Android logical partition images.
    Lp,
    /// The native reader for UEFI firmware volumes and capsules.
    Uefi,
    /// The native reader for Intel HEX files.
    Ihex,
    /// The native reader for Flash Video files.
    Flv,
    /// The native reader for SWF files, uncompressed or compressed with zlib
    /// or LZMA.
    Swf,
    /// The native reader for one compressed stream that holds no tar, in a
    /// format [`crate::native_stream::Format`] names.
    Stream,
    /// 7-Zip, with the format pinned when Zipper recognized its signature.
    Backend { format: Option<String> },
}

fn starts_with_zip_signature(archive: &Path) -> Result<bool> {
    let mut signature = [0; 4];
    let read = crate::volumes::open(archive)?.read(&mut signature)?;
    Ok(read == 4 && matches!(&signature, b"PK\x03\x04" | b"PK\x05\x06"))
}

/// Pick the reader for `archive` from its first bytes. When a file named
/// `.001` is read with its later volumes as one archive, these are the
/// joined archive's first bytes. `docs/reference/zip-reader.md` gives the
/// rules for numbered volumes.
pub fn choose_reader(engine: Engine, archive: &Path) -> Result<Reader> {
    Ok(match engine {
        Engine::Backend => {
            if let Some(format) = refused::format(archive)? {
                bail!(Refused(format));
            }
            match compat::identified_format(archive)? {
                Some(format) => Reader::Backend {
                    format: Some(format),
                },
                None if crate::ihex::sniff(archive)? => Reader::Backend {
                    format: Some("-tihex".to_owned()),
                },
                None => bail!(Refused(refused::UNNAMED_FOR_7ZIP)),
            }
        }
        _ if crate::volumes::has_zip_volumes(archive)? || starts_with_zip_signature(archive)? => {
            Reader::Zip
        }
        #[cfg(feature = "exe")]
        _ if native_exe::sniff(archive)? => Reader::Exe,
        #[cfg(feature = "tar")]
        _ if native_tar::sniff(archive)?.is_some() => Reader::Tar,
        #[cfg(feature = "cpio")]
        _ if native_cpio::sniff(archive)? => Reader::Cpio,
        #[cfg(feature = "ar")]
        _ if native_ar::sniff(archive)? => Reader::Ar,
        #[cfg(feature = "rpm")]
        _ if native_rpm::sniff(archive)? => Reader::Rpm,
        #[cfg(feature = "xar")]
        _ if native_xar::sniff(archive)? => Reader::Xar,
        #[cfg(feature = "cab")]
        _ if native_cab::sniff(archive)? => Reader::Cab,
        #[cfg(feature = "lzh")]
        _ if native_lzh::sniff(archive)? => Reader::Lzh,
        #[cfg(feature = "arj")]
        _ if native_arj::sniff(archive)? => Reader::Arj,
        #[cfg(feature = "rar")]
        Engine::Auto if native_rar::sniff(archive)? && native_rar::needs_backend(archive)? => {
            Reader::Backend {
                format: compat::pinned_format(archive)?,
            }
        }
        #[cfg(feature = "rar")]
        _ if native_rar::sniff(archive)? => Reader::Rar,
        _ if let Some(format) = refused::leading_signature(archive)? => bail!(Refused(format)),
        _ if let Some(format) = refused::disk_container(archive)? => bail!(Refused(format)),
        #[cfg(feature = "wim")]
        _ if native_wim::sniff(archive)? => Reader::Wim,
        #[cfg(feature = "7z")]
        Engine::Auto if native_7z::sniff(archive)? && native_7z::needs_backend(archive)? => {
            Reader::Backend {
                format: Some(compat::pinned_format(archive)?.unwrap_or_else(|| "-t7z".to_owned())),
            }
        }
        #[cfg(feature = "7z")]
        _ if native_7z::sniff(archive)? => Reader::SevenZip,
        #[cfg(feature = "exe")]
        Engine::Auto if native_exe::needs_backend(archive)? => Reader::Backend {
            format: Some("-tmub".to_owned()),
        },
        #[cfg(feature = "exe")]
        _ if native_exe::sniff_damaged(archive)? => Reader::Exe,
        #[cfg(feature = "lp")]
        Engine::Auto if native_lp::sniff(archive)? && native_lp::needs_backend(archive)? => {
            Reader::Backend {
                format: Some("-tlp".to_owned()),
            }
        }
        #[cfg(feature = "lp")]
        _ if native_lp::sniff(archive)? => Reader::Lp,
        #[cfg(feature = "uefi")]
        Engine::Auto if native_uefi::sniff(archive)? && native_uefi::needs_backend(archive)? => {
            Reader::Backend {
                format: Some(native_uefi::backend_type(archive)?.to_owned()),
            }
        }
        #[cfg(feature = "uefi")]
        _ if native_uefi::sniff(archive)? => Reader::Uefi,
        _ if refused::raw_volume(archive)? == Some(refused::NTFS_VOLUMES) => {
            bail!(Refused(refused::NTFS_VOLUMES))
        }
        #[cfg(feature = "iso")]
        _ if native_iso::sniff(archive)? => Reader::Iso,
        _ if let Some(format) = refused::raw_volume(archive)? => bail!(Refused(format)),
        #[cfg(feature = "squashfs")]
        Engine::Auto
            if native_squashfs::sniff(archive)? && native_squashfs::needs_backend(archive)? =>
        {
            Reader::Backend {
                format: Some("-tsquashfs".to_owned()),
            }
        }
        #[cfg(feature = "squashfs")]
        _ if native_squashfs::sniff(archive)? => Reader::Squashfs,
        #[cfg(feature = "cramfs")]
        _ if native_cramfs::sniff(archive)? => Reader::Cramfs,
        #[cfg(feature = "ext")]
        Engine::Auto if native_ext::sniff(archive)? && native_ext::needs_backend(archive)? => {
            Reader::Backend {
                format: Some("-text".to_owned()),
            }
        }
        #[cfg(feature = "ext")]
        _ if native_ext::sniff(archive)? => Reader::Ext,
        _ if let Some(format) = refused::disk(archive)? => bail!(Refused(format)),
        #[cfg(feature = "flv")]
        _ if native_flv::sniff(archive)? => Reader::Flv,
        #[cfg(feature = "swf")]
        _ if native_swf::sniff(archive)? => Reader::Swf,
        Engine::Auto if !cfg!(feature = "ihex") && crate::ihex::opens(archive)? => {
            Reader::Backend {
                format: Some("-tihex".to_owned()),
            }
        }
        _ if crate::ihex::opens(archive)? => Reader::Ihex,
        _ if let Some(format) = refused::dmg(archive)? => bail!(Refused(format)),
        _ if let Some(format) = refused::base64(archive)? => bail!(Refused(format)),
        #[cfg(feature = "tar")]
        _ if crate::native_stream::sniff(archive)?.is_some() => Reader::Stream,
        #[cfg(feature = "exe")]
        _ if native_exe::sniff_object(archive)? => Reader::Exe,
        #[cfg(feature = "uefi")]
        Engine::Auto
            if native_uefi::sniff_later_volume(archive)?
                && native_uefi::needs_backend(archive)? =>
        {
            Reader::Backend {
                format: Some("-tuefif".to_owned()),
            }
        }
        #[cfg(feature = "uefi")]
        _ if native_uefi::sniff_later_volume(archive)? => Reader::Uefi,
        Engine::Auto if !cfg!(feature = "ihex") && crate::ihex::sniff(archive)? => {
            Reader::Backend {
                format: Some("-tihex".to_owned()),
            }
        }
        _ if crate::ihex::sniff(archive)? => Reader::Ihex,
        Engine::Native => native_fallback(archive)?,
        Engine::Auto => match compat::pinned_format(archive)? {
            Some(format) => Reader::Backend {
                format: Some(format),
            },
            None => native_fallback(archive)?,
        },
    })
}

fn native_fallback(archive: &Path) -> Result<Reader> {
    if cfg!(feature = "all-formats") {
        return Ok(Reader::Zip);
    }
    const USTAR: (&[u8], usize) = (b"ustar", 257);
    let mut head = Vec::new();
    crate::volumes::open(archive)?
        .take((USTAR.1 + USTAR.0.len()) as u64)
        .read_to_end(&mut head)?;
    if cfg!(not(feature = "tar")) && head.get(USTAR.1..) == Some(USTAR.0) {
        bail!(NotBuilt("tar".to_owned()));
    }
    if let Some(format) = compat::pinned_format(archive)? {
        bail!(NotBuilt(format.trim_start_matches("-t").to_owned()));
    }
    Ok(Reader::Zip)
}

fn format_name(reader: &Reader) -> String {
    match reader {
        Reader::Zip => "zip".to_owned(),
        Reader::SevenZip => "7z".to_owned(),
        Reader::Stream => "compressed".to_owned(),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Options for [`extract_archive`].
#[derive(Clone, Copy, Default)]
pub struct ExtractOptions<'a> {
    /// What to do when the destination exists.
    pub existing: Existing,
    /// Exact entry paths or folder subtrees to extract. Empty extracts
    /// everything.
    pub paths: &'a [String],
    /// Wildcard patterns to extract. `*` and `?` stay inside one name, `**`
    /// crosses folders, and a pattern without `/` matches a name at any depth.
    /// Matching a folder selects everything under it.
    pub include: &'a [String],
    /// Wildcard patterns to leave out, written as for `include`.
    pub exclude: &'a [String],
    /// Leading path segments to drop before writing: `Strip::Prefix("a")`
    /// writes `a/b/c.txt` as `<dest>/b/c.txt`, and `Strip::Components(1)`
    /// does the same for every top folder. `paths`, `include` and `exclude`
    /// still match the full archive paths. The 7-Zip backend refuses it.
    pub strip: crate::Strip<'a>,
    /// Which metadata to restore beside contents. `Metadata::ContentOnly`
    /// writes contents and the execute bit only, with no times, Windows
    /// attributes or Mark of the Web. The 7-Zip backend refuses it.
    pub metadata: crate::Metadata,
    /// Progress reports and cancellation. Every reader checks the cancel flag
    /// before it starts. The in-process readers report progress and check it
    /// while they write, and the 7-Zip backend stops 7-Zip when it is set.
    pub control: Control<'a>,
    /// Cap on declared output bytes. Without it, the output of every reader
    /// in process, including single compressed files, must fit in free
    /// space. The 7-Zip backend refuses a cap and checks no free space.
    pub max_output_bytes: Option<u64>,
    /// Maximum ZIP workers, tar writer threads or 7z folders decoded at
    /// once, 1–64.
    pub threads: Option<usize>,
    pub engine: Engine,
    /// Extract every file into the destination itself, without its folders,
    /// as `7z e` does. Files that share a name follow `existing` in path
    /// order: `Refuse` fails, `Overwrite` keeps the last, `Skip` keeps the
    /// first, and the rename modes number the others `name_1.ext` and on.
    /// An archive with links is refused.
    pub flat: bool,
    /// Password for ZipCrypto and WinZip AES entries in a ZIP, and for 7z
    /// AES-256, read in process. A selected encrypted entry without it fails
    /// with [`crate::native_zip::PasswordNeeded`] or
    /// [`crate::native_7z::PasswordNeeded`], as does a 7z whose header is
    /// encrypted. The tar and single-file readers and the 7-Zip backend
    /// refuse it.
    pub password: Option<&'a [u8]>,
    /// Write the NTFS alternate data streams of a WIM's files as named
    /// streams `file:name`, after each file's data and before the file is
    /// published. Only the WIM reader on Windows writes them. A destination
    /// whose file system keeps no named streams is refused, and so is a
    /// stream on a folder or link, a name Windows would change or refuse and
    /// two names NTFS compares equal. With `Metadata::ContentOnly` a stored
    /// `Zone.Identifier` stream is not written. Off Windows a WIM with a
    /// named stream is refused, as it is without this option. Every other
    /// reader refuses the option.
    pub alternate_streams: bool,
}

fn flatten(
    tree: &Path,
    into: &Path,
    holder: &Path,
    existing: Existing,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<()> {
    let cancelled = || cancel.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed));
    let moved = |from: &Path, to: std::path::PathBuf| -> Result<()> {
        crate::retried_while_windows_holds_it(|| std::fs::rename(from, &to), cancelled).map_err(
            |error| match cancelled() {
                true => crate::Cancelled.into(),
                false => anyhow::Error::from(error),
            },
        )
    };
    let mut by_name: std::collections::BTreeMap<std::ffi::OsString, Vec<std::path::PathBuf>> =
        std::collections::BTreeMap::new();
    for entry in walkdir::WalkDir::new(tree).min_depth(1).sort_by_file_name() {
        let entry = entry?;
        let relative = entry.path().strip_prefix(tree)?;
        ensure!(
            !entry.file_type().is_symlink(),
            "--flat refuses archives with links, such as {}",
            relative.display()
        );
        if entry.file_type().is_file() {
            by_name
                .entry(entry.file_name().to_owned())
                .or_default()
                .push(entry.path().to_owned());
        }
    }
    let mut taken: std::collections::HashSet<std::ffi::OsString> =
        by_name.keys().cloned().collect();
    taken.extend(holder.file_name().map(ToOwned::to_owned));
    for (name, paths) in &by_name {
        ensure!(
            Some(name.as_os_str()) != holder.file_name(),
            "an archive file has the staging folder's name"
        );
        let (kept, others): (&std::path::Path, &[std::path::PathBuf]) = match existing {
            _ if paths.len() == 1 => (&paths[0], &[]),
            Existing::Refuse => bail!(
                "several archive files are named {}; pass --existing to choose",
                name.display()
            ),
            Existing::Overwrite => (paths.last().unwrap(), &[]),
            Existing::Skip => (&paths[0], &[]),
            Existing::RenameNew | Existing::RenameExisting => (&paths[0], &paths[1..]),
        };
        moved(kept, into.join(name))?;
        let mut number = 0;
        for other in others {
            let renamed = loop {
                number += 1;
                let candidate = crate::staging::numbered(name, number);
                if taken.insert(candidate.clone()) {
                    break candidate;
                }
            };
            moved(other, into.join(renamed))?;
        }
    }
    Ok(())
}

/// Extract any archive Zipper reads into `destination`, choosing the reader
/// as `zipper extract` does. When it returns, after success, failure or a
/// cancel, no handle to the destination or the archive stays open and every
/// thread it started has exited, so the destination can be renamed or
/// deleted at once. Rayon's shared pool keeps its idle threads.
pub fn extract_archive(archive: &Path, destination: &Path, options: ExtractOptions) -> Result<()> {
    if options.flat {
        let staging =
            crate::staging::stage(archive, destination, options.existing, options.metadata)?;
        let tree = tempfile::tempdir_in(staging.path())?;
        let nested = tree.path().join("archive");
        extract_archive(
            archive,
            &nested,
            ExtractOptions {
                flat: false,
                existing: Existing::Refuse,
                ..options
            },
        )?;
        flatten(
            &nested,
            staging.path(),
            tree.path(),
            options.existing,
            options.control.cancel,
        )?;
        drop(tree);
        return staging.publish(&[], options.control.cancel);
    }
    ensure!(
        options
            .threads
            .is_none_or(|threads| (1..=64).contains(&threads)),
        "threads must be between 1 and 64"
    );
    crate::progress::Tracker::new(options.control, None, None).check()?;
    with_volumes(archive, || read_into(archive, destination, options))
}

#[derive(Debug)]
struct VolumeNote {
    error: anyhow::Error,
    note: String,
}

impl std::fmt::Display for VolumeNote {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}\n{}", self.error, self.note)
    }
}

impl std::error::Error for VolumeNote {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.chain().nth(1)
    }
}

fn says_nothing_about_the_volumes(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<crate::Cancelled>()
            || cause.is::<crate::DestinationRefused>()
            || cause.is::<crate::OverLimit>()
            || cause.is::<NotBuilt>()
    })
}

const BACKEND_ADVICE: &str =
    "; if the file is another format or a split volume, pass --zip-engine backend";

fn without_backend_advice(error: anyhow::Error) -> anyhow::Error {
    if let Some(&crate::BadField(message)) = error.downcast_ref::<crate::BadField>() {
        return match message.split_once(BACKEND_ADVICE) {
            Some((kept, _)) => crate::BadField(kept).into(),
            None => error,
        };
    }
    let message = error.to_string();
    match message.split_once(BACKEND_ADVICE) {
        Some((kept, _)) if error.source().is_none() => anyhow::anyhow!("{kept}"),
        _ => error,
    }
}

fn lone_zip_is_another_format(archive: &Path) -> Option<&'static str> {
    if !crate::volumes::has_zip_volumes(archive).unwrap_or(false) {
        return None;
    }
    #[cfg(feature = "7z")]
    if native_7z::sniff(archive).unwrap_or(false) {
        return Some("7z");
    }
    #[cfg(feature = "tar")]
    if native_tar::sniff(archive).is_ok_and(|tar| tar.is_some()) {
        return Some("tar");
    }
    #[cfg(feature = "tar")]
    if crate::native_stream::sniff(archive).is_ok_and(|stream| stream.is_some()) {
        return Some("compressed");
    }
    None
}

fn with_volumes<T>(archive: &Path, read: impl FnOnce() -> Result<T>) -> Result<T> {
    let volumes = crate::volumes::scope(archive)?;
    let error = match read() {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    if says_nothing_about_the_volumes(&error) {
        return Err(error);
    }
    if let (Some(note), Some(format)) = (volumes.note(), lone_zip_is_another_format(archive)) {
        return Err(VolumeNote {
            error: anyhow::anyhow!(
                "{} by itself is a {format} archive, so the .z01 volumes beside it may not belong to it; move them aside to read it alone",
                archive.display()
            ),
            note: format!("{}\n{note}", without_backend_advice(error)),
        }
        .into());
    }
    match volumes.note() {
        Some(note) => Err(VolumeNote {
            error: without_backend_advice(error),
            note,
        }
        .into()),
        None => Err(error),
    }
}

/// Run `read`, which reads `archive`. When `archive` is a numbered volume,
/// its volumes are found once for every reader that `read` opens, by the
/// rules in `docs/reference/zip-reader.md`. When `read` fails, other than by
/// a cancel, a destination problem or a size limit, the error gains a line
/// that names the volumes read as one archive, or says why a numbered file
/// was read alone when there is a reason to give. A damaged set is neither
/// joined nor read alone, and its error names the volume at fault instead.
pub fn read_volumes<T>(archive: &Path, read: impl FnOnce() -> Result<T>) -> Result<T> {
    with_volumes(archive, read)
}

/// The error for an archive whose reader this build leaves out, naming its
/// format, such as `"zip"`, `"tar"` or `"cpio"`. `Engine::Native` returns
/// it, and writes nothing, for every format Zipper recognizes whose feature
/// this build leaves out. `Engine::Auto` returns it for a ZIP or tar archive
/// it cannot read, and hands every other such format to 7-Zip. For a ZIP
/// entry whose method or encryption this build leaves out, it names the
/// feature to enable, such as `"zip-bzip2"`, `"zip-encryption"` or
/// `"zstd"`, and nothing in the archive is written.
#[derive(Debug)]
pub struct NotBuilt(pub String);

impl std::fmt::Display for NotBuilt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "this build of Zipper leaves out the {} reader; enable its Cargo feature",
            self.0
        )
    }
}

impl std::error::Error for NotBuilt {}

/// The error for a format Zipper refuses in every engine, such as CHM, DMG,
/// FAT, NTFS, HFS+, APFS, compound files and Base64. No engine hands such a
/// file to 7-Zip. The field names the format, and nothing is written. `docs/reference/refused.md`
/// lists the formats and their signatures.
#[derive(Debug)]
pub struct Refused(pub &'static str);

impl std::fmt::Display for Refused {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Zipper does not read {}", self.0)
    }
}

impl std::error::Error for Refused {}

type TreeReader = fn(&Path, Option<&Path>, crate::tree::ReadOptions) -> Result<Vec<crate::Entry>>;

fn tree_reader(reader: &Reader) -> Option<TreeReader> {
    match reader {
        #[cfg(feature = "cpio")]
        Reader::Cpio => Some(native_cpio::read_archive),
        #[cfg(feature = "ar")]
        Reader::Ar => Some(native_ar::read_archive),
        #[cfg(feature = "rpm")]
        Reader::Rpm => Some(native_rpm::read_archive),
        #[cfg(feature = "xar")]
        Reader::Xar => Some(native_xar::read_archive),
        #[cfg(feature = "cab")]
        Reader::Cab => Some(native_cab::read_archive),
        #[cfg(feature = "lzh")]
        Reader::Lzh => Some(native_lzh::read_archive),
        #[cfg(feature = "arj")]
        Reader::Arj => Some(native_arj::read_archive),
        #[cfg(feature = "rar")]
        Reader::Rar => Some(native_rar::read_archive),
        #[cfg(feature = "iso")]
        Reader::Iso => Some(native_iso::read_archive),
        #[cfg(feature = "wim")]
        Reader::Wim => Some(native_wim::read_archive),
        #[cfg(feature = "exe")]
        Reader::Exe => Some(native_exe::read_archive),
        #[cfg(feature = "squashfs")]
        Reader::Squashfs => Some(native_squashfs::read_archive),
        #[cfg(feature = "cramfs")]
        Reader::Cramfs => Some(native_cramfs::read_archive),
        #[cfg(feature = "ext")]
        Reader::Ext => Some(native_ext::read_archive),
        #[cfg(feature = "lp")]
        Reader::Lp => Some(native_lp::read_archive),
        #[cfg(feature = "uefi")]
        Reader::Uefi => Some(native_uefi::read_archive),
        #[cfg(feature = "ihex")]
        Reader::Ihex => Some(crate::native_ihex::read_archive),
        #[cfg(feature = "flv")]
        Reader::Flv => Some(native_flv::read_archive),
        #[cfg(feature = "swf")]
        Reader::Swf => Some(native_swf::read_archive),
        _ => None,
    }
}

fn read_into(archive: &Path, destination: &Path, options: ExtractOptions) -> Result<()> {
    let reader = choose_reader(options.engine, archive)?;
    ensure!(
        options.password.is_none()
            || matches!(
                reader,
                Reader::Zip | Reader::SevenZip | Reader::Backend { .. }
            ),
        "--password applies only to ZIP and 7z archives, and Zipper does not decrypt {}",
        archive.display()
    );
    ensure!(
        !options.alternate_streams || matches!(reader, Reader::Wim),
        "--alternate-streams applies only to WIM images read in process, and {} is not one",
        archive.display()
    );
    match reader {
        #[cfg(feature = "zip-deflate")]
        Reader::Zip => {
            native_zip::read_archive_with_options(
                archive,
                Some(destination),
                native_zip::ReadOptions {
                    threads: options.threads,
                    max_output_bytes: options.max_output_bytes,
                    existing: options.existing,
                    paths: options.paths,
                    include: options.include,
                    exclude: options.exclude,
                    strip: options.strip,
                    metadata: options.metadata,
                    control: options.control,
                    password: options.password,
                },
            )?;
        }
        #[cfg(feature = "7z")]
        Reader::SevenZip => {
            native_7z::read_archive(
                archive,
                Some(destination),
                native_7z::ReadOptions {
                    max_output_bytes: options.max_output_bytes,
                    threads: options.threads,
                    existing: options.existing,
                    paths: options.paths,
                    include: options.include,
                    exclude: options.exclude,
                    strip: options.strip,
                    metadata: options.metadata,
                    control: options.control,
                    password: options.password,
                },
            )?;
        }
        #[cfg(feature = "tar")]
        Reader::Stream => {
            crate::native_stream::read_archive(
                archive,
                Some(destination),
                crate::native_stream::ReadOptions {
                    max_output_bytes: options.max_output_bytes,
                    existing: options.existing,
                    paths: options.paths,
                    include: options.include,
                    exclude: options.exclude,
                    strip: options.strip,
                    metadata: options.metadata,
                    control: options.control,
                },
            )?;
        }
        Reader::Backend { .. } if options.password.is_some() => {
            bail!("--password needs a ZIP or 7z archive read in process")
        }
        Reader::Backend { .. }
            if !(options.paths.is_empty()
                && options.include.is_empty()
                && options.exclude.is_empty()
                && options.strip == crate::Strip::None
                && options.metadata.restores()) =>
        {
            bail!(
                "--path, --include, --exclude, --strip-prefix, --strip-components and --content-only need an archive read in process"
            )
        }
        #[cfg(feature = "tar")]
        Reader::Tar => {
            native_tar::read_archive(
                archive,
                Some(destination),
                native_tar::ReadOptions {
                    max_output_bytes: options.max_output_bytes,
                    writers: options.threads,
                    existing: options.existing,
                    paths: options.paths,
                    include: options.include,
                    exclude: options.exclude,
                    strip: options.strip,
                    metadata: options.metadata,
                    control: options.control,
                },
            )?;
        }
        Reader::Backend { format } => {
            ensure!(
                options.max_output_bytes.is_none(),
                "--max-extracted-bytes needs an archive read in process"
            );
            compat::run_until_cancelled(
                "x",
                archive,
                Some(destination),
                format.as_deref(),
                options.existing,
                options.control.cancel,
            )?;
        }
        _ => {
            let read = tree_reader(&reader).ok_or_else(|| NotBuilt(format_name(&reader)))?;
            read(
                archive,
                Some(destination),
                crate::tree::ReadOptions {
                    max_output_bytes: options.max_output_bytes,
                    existing: options.existing,
                    paths: options.paths,
                    include: options.include,
                    exclude: options.exclude,
                    strip: options.strip,
                    metadata: options.metadata,
                    control: options.control,
                    alternate_streams: options.alternate_streams,
                },
            )?;
        }
    }
    Ok(())
}
