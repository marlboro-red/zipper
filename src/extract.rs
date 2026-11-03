use crate::{Control, Existing, compat, native_7z, native_tar, native_zip};
use anyhow::{Result, bail, ensure};
use std::{io::Read, path::Path};

/// The reader for an archive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Engine {
    /// Native for ZIP, tar, unrecognized files and 7z whose methods it
    /// decodes. 7-Zip for other 7z and the other formats whose signature
    /// Zipper recognizes.
    #[default]
    Auto,
    /// 7-Zip for every archive.
    Backend,
    /// Native for ZIP and tar, and native ZIP for unrecognized files.
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
    /// 7-Zip, with the format pinned when Zipper recognized its signature.
    Backend { format: Option<String> },
}

fn starts_with_zip_signature(archive: &Path) -> Result<bool> {
    let mut signature = [0; 4];
    let read = std::fs::File::open(archive)?.read(&mut signature)?;
    Ok(read == 4 && matches!(&signature, b"PK\x03\x04" | b"PK\x05\x06"))
}

/// Pick the reader for `archive` from its first bytes. A `.001` name only
/// marks a recognized 7-Zip format as split.
pub fn choose_reader(engine: Engine, archive: &Path) -> Result<Reader> {
    Ok(match engine {
        Engine::Backend => Reader::Backend { format: None },
        _ if starts_with_zip_signature(archive)? => Reader::Zip {
            fast: engine == Engine::NativeFast,
        },
        _ if native_tar::sniff(archive)?.is_some() => Reader::Tar,
        Engine::Auto if native_7z::sniff(archive)? && native_7z::needs_backend(archive)? => {
            Reader::Backend {
                format: compat::pinned_format(archive)?,
            }
        }
        _ if native_7z::sniff(archive)? => Reader::SevenZip,
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
    /// Progress reports and cancellation. Every reader checks the cancel flag
    /// before it starts. Only ZIP, tar and 7z report progress and check it
    /// later.
    pub control: Control<'a>,
    /// Cap on declared output bytes. Without it, ZIP, tar and 7z output must
    /// fit in free space. The 7-Zip backend refuses a cap and checks no free
    /// space.
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
        let path = std::path::Path::new(name);
        let (stem, extension) = (path.file_stem().unwrap_or_default(), path.extension());
        let mut number = 0;
        for other in others {
            let renamed = loop {
                number += 1;
                let mut candidate = stem.to_owned();
                candidate.push(format!("_{number}"));
                if let Some(extension) = extension {
                    candidate.push(".");
                    candidate.push(extension);
                }
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
/// as `zipper extract` does.
pub fn extract_archive(archive: &Path, destination: &Path, options: ExtractOptions) -> Result<()> {
    if options.flat {
        let staging = crate::staging::stage(archive, destination, options.existing)?;
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
                    control: options.control,
                },
            )?;
        }
        Reader::Backend { .. }
            if !(options.paths.is_empty()
                && options.include.is_empty()
                && options.exclude.is_empty()) =>
        {
            bail!("--path, --include and --exclude need an archive read in process")
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
                    control: options.control,
                },
            )?;
        }
        Reader::Backend { format } => {
            ensure!(
                options.max_output_bytes.is_none(),
                "--max-extracted-bytes needs a ZIP, tar or 7z archive read in process"
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
