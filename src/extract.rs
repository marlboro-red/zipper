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
use crate::{Control, Existing, refused};
use anyhow::{Result, bail, ensure};
use std::{io::Read, path::Path};

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
}

fn starts_with_zip_signature(archive: &Path) -> Result<bool> {
    let mut signature = [0; 4];
    let read = crate::volumes::open(archive)?.read(&mut signature)?;
    Ok(read == 4 && matches!(&signature, b"PK\x03\x04" | b"PK\x05\x06"))
}

/// Pick the reader for `archive` from its first bytes. When a file named
/// `.001` is read with its later volumes as one archive, these are the
/// joined archive's first bytes. `docs/reference/zip-reader.md` gives the
/// rules for numbered volumes. Every reader runs in process, and none starts
/// 7-Zip. A format `docs/reference/refused.md` lists fails with [`Refused`].
/// A file no reader recognizes goes to the ZIP reader.
pub fn choose_reader(archive: &Path) -> Result<Reader> {
    Ok(match () {
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
        _ if native_rar::sniff(archive)? => Reader::Rar,
        _ if let Some(format) = refused::leading_signature(archive)? => bail!(Refused(format)),
        _ if let Some(format) = refused::disk_container(archive)? => bail!(Refused(format)),
        #[cfg(feature = "wim")]
        _ if native_wim::sniff(archive)? => Reader::Wim,
        #[cfg(feature = "7z")]
        _ if native_7z::sniff(archive)? => Reader::SevenZip,
        _ if let Some(format) = refused::other_universal(archive)? => bail!(Refused(format)),
        #[cfg(feature = "exe")]
        _ if native_exe::sniff_damaged(archive)? => Reader::Exe,
        #[cfg(feature = "lp")]
        _ if native_lp::sniff(archive)? => Reader::Lp,
        #[cfg(feature = "uefi")]
        _ if native_uefi::sniff(archive)? => Reader::Uefi,
        _ if refused::raw_volume(archive)? == Some(refused::NTFS_VOLUMES) => {
            bail!(Refused(refused::NTFS_VOLUMES))
        }
        #[cfg(feature = "iso")]
        _ if native_iso::sniff(archive)? => Reader::Iso,
        _ if let Some(format) = refused::raw_volume(archive)? => bail!(Refused(format)),
        #[cfg(feature = "squashfs")]
        _ if native_squashfs::other_version(archive)? => {
            bail!(Refused(refused::OTHER_SQUASHFS_VERSIONS))
        }
        #[cfg(feature = "squashfs")]
        _ if native_squashfs::sniff(archive)? => Reader::Squashfs,
        #[cfg(feature = "cramfs")]
        _ if native_cramfs::sniff(archive)? => Reader::Cramfs,
        #[cfg(feature = "ext")]
        _ if native_ext::sniff(archive)? => Reader::Ext,
        _ if let Some(format) = refused::disk(archive)? => bail!(Refused(format)),
        #[cfg(feature = "flv")]
        _ if native_flv::sniff(archive)? => Reader::Flv,
        #[cfg(feature = "swf")]
        _ if native_swf::sniff(archive)? => Reader::Swf,
        _ if crate::ihex::opens(archive)? => Reader::Ihex,
        _ if let Some(format) = refused::dmg(archive)? => bail!(Refused(format)),
        _ if let Some(format) = refused::base64(archive)? => bail!(Refused(format)),
        #[cfg(feature = "tar")]
        _ if crate::native_stream::sniff(archive)?.is_some() => Reader::Stream,
        #[cfg(feature = "exe")]
        _ if native_exe::sniff_object(archive)? => Reader::Exe,
        #[cfg(feature = "uefi")]
        _ if native_uefi::sniff_later_volume(archive)? => Reader::Uefi,
        _ if crate::ihex::sniff(archive)? => Reader::Ihex,
        _ => by_signature(archive)?,
    })
}

struct LeadingSignature {
    signature: &'static [u8],
    next_byte_one_of: &'static [u8],
    format: &'static str,
}

impl LeadingSignature {
    fn starts(&self, head: &[u8]) -> bool {
        head.starts_with(self.signature)
            && (self.next_byte_one_of.is_empty()
                || head
                    .get(self.signature.len())
                    .is_some_and(|byte| self.next_byte_one_of.contains(byte)))
    }
}

const READERS_BY_LEADING_SIGNATURE: &[LeadingSignature] = &[
    LeadingSignature {
        signature: b"7z\xBC\xAF\x27\x1C",
        next_byte_one_of: b"",
        format: "7z",
    },
    LeadingSignature {
        signature: b"Rar!\x1A\x07\x00",
        next_byte_one_of: b"",
        format: "rar",
    },
    LeadingSignature {
        signature: b"Rar!\x1A\x07\x01\x00",
        next_byte_one_of: b"",
        format: "rar",
    },
    LeadingSignature {
        signature: b"\x1F\x8B",
        next_byte_one_of: b"",
        format: "gzip",
    },
    LeadingSignature {
        signature: b"BZh",
        next_byte_one_of: b"123456789",
        format: "bzip2",
    },
    LeadingSignature {
        signature: b"\xFD7zXZ\x00",
        next_byte_one_of: b"",
        format: "xz",
    },
    LeadingSignature {
        signature: b"MSCF",
        next_byte_one_of: b"",
        format: "cab",
    },
];
const SIGNATURE_WINDOW: usize = 8;

fn signature_reader(name: &str) -> Option<Reader> {
    match name {
        #[cfg(feature = "7z")]
        "7z" => Some(Reader::SevenZip),
        #[cfg(feature = "rar")]
        "rar" => Some(Reader::Rar),
        #[cfg(feature = "tar")]
        "gzip" | "bzip2" | "xz" => Some(Reader::Stream),
        #[cfg(feature = "cab")]
        "cab" => Some(Reader::Cab),
        _ => None,
    }
}

fn by_signature(archive: &Path) -> Result<Reader> {
    const USTAR: (&[u8], usize) = (b"ustar", 257);
    let mut head = Vec::new();
    crate::volumes::open(archive)?
        .take((USTAR.1 + USTAR.0.len()).max(SIGNATURE_WINDOW) as u64)
        .read_to_end(&mut head)?;
    if cfg!(not(feature = "tar")) && head.get(USTAR.1..) == Some(USTAR.0) {
        bail!(NotBuilt("tar".to_owned()));
    }
    match READERS_BY_LEADING_SIGNATURE
        .iter()
        .find(|known| known.starts(&head))
    {
        Some(known) => {
            signature_reader(known.format).ok_or_else(|| NotBuilt(known.format.to_owned()).into())
        }
        None => Ok(Reader::Zip),
    }
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
    /// still match the full archive paths.
    pub strip: crate::Strip<'a>,
    /// Which metadata to restore beside contents. `Metadata::ContentOnly`
    /// writes contents and the execute bit only, with no times, Windows
    /// attributes or Mark of the Web.
    pub metadata: crate::Metadata,
    /// Progress reports and cancellation. Every reader checks the cancel flag
    /// before it starts, and reports progress and checks it while it writes.
    pub control: Control<'a>,
    /// Cap on declared output bytes. Without it, the output of every reader,
    /// including single compressed files, must fit in free space.
    pub max_output_bytes: Option<u64>,
    /// Maximum ZIP workers, tar writer threads or 7z folders decoded at
    /// once, 1–64.
    pub threads: Option<usize>,
    /// Extract every file into the destination itself, without its folders,
    /// as `7z e` does. Files that share a name follow `existing` in path
    /// order: `Refuse` fails, `Overwrite` keeps the last, `Skip` keeps the
    /// first, and the rename modes number the others `name_1.ext` and on.
    /// An archive with links is refused.
    pub flat: bool,
    /// Password for ZipCrypto and WinZip AES entries in a ZIP, and for 7z
    /// AES-256. A selected encrypted entry without it fails
    /// with [`crate::native_zip::PasswordNeeded`] or
    /// [`crate::native_7z::PasswordNeeded`], as does a 7z whose header is
    /// encrypted. The other readers refuse it.
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

fn removed_with_its_folders(tree: tempfile::TempDir) -> std::io::Result<()> {
    #[cfg(unix)]
    crate::unix_modes::unlock_tree_for_cleanup(tree.path());
    tree.close()
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
        let flattened = extract_archive(
            archive,
            &nested,
            ExtractOptions {
                flat: false,
                existing: Existing::Refuse,
                ..options
            },
        )
        .and_then(|()| {
            flatten(
                &nested,
                staging.path(),
                tree.path(),
                options.existing,
                options.control.cancel,
            )
        });
        let removed = removed_with_its_folders(tree);
        flattened?;
        removed?;
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
            note: format!("{error}\n{note}"),
        }
        .into());
    }
    match volumes.note() {
        Some(note) => Err(VolumeNote { error, note }.into()),
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
/// format, such as `"zip"`, `"tar"`, `"gzip"`, `"7z"` or `"ihex"`. It is
/// returned, and nothing is written, for a ZIP, a tar, an Intel HEX file, or
/// a file that starts with the signature of 7z, RAR, gzip, bzip2, xz or CAB,
/// when this build leaves out its reader. For a ZIP
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

/// The error for a format Zipper does not read, such as CHM, DMG, FAT, NTFS,
/// HFS+, APFS, compound files, Base64 and disk images. The field names the
/// format, and nothing is written. `docs/reference/refused.md` lists the
/// formats and their signatures.
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
    let reader = choose_reader(archive)?;
    ensure!(
        options.password.is_none() || matches!(reader, Reader::Zip | Reader::SevenZip),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_cancel_during_a_failing_flat_rename_returns_cancelled_and_moves_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let set_mode = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        for cancel_during_the_rename in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let (tree, into) = (root.path().join("tree"), root.path().join("into"));
            std::fs::create_dir_all(tree.join("locked")).unwrap();
            std::fs::create_dir(&into).unwrap();
            std::fs::write(tree.join("locked/file"), b"written").unwrap();
            set_mode(&tree.join("locked"), 0o555);
            let cancel = std::sync::atomic::AtomicBool::new(cancel_during_the_rename);
            let error = flatten(&tree, &into, &tree, Existing::Refuse, Some(&cancel)).unwrap_err();
            set_mode(&tree.join("locked"), 0o755);
            assert_eq!(
                error.is::<crate::Cancelled>(),
                cancel_during_the_rename,
                "{error:#}"
            );
            assert_eq!(std::fs::read_dir(&into).unwrap().count(), 0);
            assert_eq!(std::fs::read(tree.join("locked/file")).unwrap(), b"written");
        }
    }
}
