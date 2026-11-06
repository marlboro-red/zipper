use crate::{
    Control, Existing, compat, native_7z, native_ar, native_cpio, native_rpm, native_tar,
    native_zip,
};
use anyhow::{Result, bail, ensure};
use std::{io::Read, path::Path};

/// The reader for an archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Engine {
    /// Native for ZIP, tar, cpio, `ar`, RPM, single compressed files,
    /// unrecognized files and 7z whose methods it decodes. 7-Zip for other
    /// 7z and the other formats whose signature Zipper recognizes.
    #[default]
    Auto,
    /// 7-Zip for every archive.
    Backend,
    /// Native for ZIP, tar, cpio, `ar`, RPM, 7z and single compressed files,
    /// and native ZIP for unrecognized files.
    Native,
    /// Like `Native`, with a shared ZIP entry-buffer budget.
    NativeFast,
}

/// The reader an archive goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reader {
    /// The native ZIP reader, with the shared entry-buffer path when `fast`.
    Zip { fast: bool },
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
    /// The native reader for one gzip, bzip2, xz, zstd or LZMA stream that
    /// holds no tar.
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
        Engine::Backend => Reader::Backend { format: None },
        _ if starts_with_zip_signature(archive)? => Reader::Zip {
            fast: engine == Engine::NativeFast,
        },
        _ if native_tar::sniff(archive)?.is_some() => Reader::Tar,
        _ if native_cpio::sniff(archive)? => Reader::Cpio,
        _ if native_ar::sniff(archive)? => Reader::Ar,
        _ if native_rpm::sniff(archive)? => Reader::Rpm,
        Engine::Auto if native_7z::sniff(archive)? && native_7z::needs_backend(archive)? => {
            Reader::Backend {
                format: compat::pinned_format(archive)?,
            }
        }
        _ if native_7z::sniff(archive)? => Reader::SevenZip,
        _ if crate::native_stream::sniff(archive)?.is_some() => Reader::Stream,
        Engine::Native => Reader::Zip { fast: false },
        Engine::NativeFast => Reader::Zip { fast: true },
        Engine::Auto => match compat::pinned_format(archive)? {
            Some(format) => Reader::Backend {
                format: Some(format),
            },
            None => Reader::Zip { fast: false },
        },
    })
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
    /// before it starts. Every reader in process reports progress and checks
    /// it later. The 7-Zip backend does neither.
    pub control: Control<'a>,
    /// Cap on declared output bytes. Without it, the output of every reader
    /// in process, including single compressed files, must fit in free
    /// space. The 7-Zip backend refuses a cap and checks no free space.
    pub max_output_bytes: Option<u64>,
    /// Maximum ZIP workers, tar writer threads or 7z folders decoded at
    /// once, 1–64.
    pub threads: Option<usize>,
    pub engine: Engine,
    /// Shared ZIP entry-buffer budget for `Engine::NativeFast`.
    pub zip_buffer_bytes: Option<usize>,
    /// Extract every file into the destination itself, without its folders,
    /// as `7z e` does. Files that share a name follow `existing` in path
    /// order: `Refuse` fails, `Overwrite` keeps the last, `Skip` keeps the
    /// first, and the rename modes number the others `name_1.ext` and on.
    /// An archive with links is refused.
    pub flat: bool,
}

fn flatten(tree: &Path, into: &Path, holder: &Path, existing: Existing) -> Result<()> {
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
        std::fs::rename(kept, into.join(name))?;
        let mut number = 0;
        for other in others {
            let renamed = loop {
                number += 1;
                let candidate = crate::staging::numbered(name, number);
                if taken.insert(candidate.clone()) {
                    break candidate;
                }
            };
            std::fs::rename(other, into.join(renamed))?;
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
        flatten(&nested, staging.path(), tree.path(), options.existing)?;
        drop(tree);
        return staging.publish(&[]);
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

fn with_volumes<T>(archive: &Path, read: impl FnOnce() -> Result<T>) -> Result<T> {
    let volumes = crate::volumes::scope(archive)?;
    let error = match read() {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    if says_nothing_about_the_volumes(&error) {
        return Err(error);
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

fn read_into(archive: &Path, destination: &Path, options: ExtractOptions) -> Result<()> {
    let reader = choose_reader(options.engine, archive)?;
    match reader {
        Reader::Zip { fast } => {
            native_zip::read_archive_with_options(
                archive,
                Some(destination),
                native_zip::ReadOptions {
                    fast,
                    threads: options.threads,
                    buffer_bytes: options.zip_buffer_bytes,
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
                },
            )?;
        }
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
        Reader::Cpio | Reader::Ar | Reader::Rpm => {
            let read = match reader {
                Reader::Cpio => native_cpio::read_archive,
                Reader::Ar => native_ar::read_archive,
                _ => native_rpm::read_archive,
            };
            read(
                archive,
                Some(destination),
                native_cpio::ReadOptions {
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
            compat::run(
                "x",
                archive,
                Some(destination),
                format.as_deref(),
                options.existing,
            )?;
        }
    }
    Ok(())
}
