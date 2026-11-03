use crate::{Control, Existing, compat, native_tar, native_zip};
use anyhow::{Result, bail, ensure};
use std::{io::Read, path::Path};

/// The reader for archives that are not `.zipper`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Engine {
    /// Native for ZIP, tar and unrecognized files, with no fallback. 7-Zip
    /// for the other formats whose signature Zipper recognizes.
    #[default]
    Auto,
    /// 7-Zip for every archive that is not `.zipper`.
    Backend,
    /// Native for ZIP and tar, and native ZIP for unrecognized files.
    Native,
    /// Like `Native`, with a shared ZIP entry-buffer budget.
    NativeFast,
}

/// The reader an archive goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reader {
    /// The `.zipper` reader for v1 and v2.
    Zipper,
    /// The native ZIP reader, with the shared entry-buffer path when `fast`.
    Zip { fast: bool },
    /// The native reader for plain, gzip, zstd and xz tar.
    Tar,
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
        _ if compat::is_native(archive)? => Reader::Zipper,
        Engine::Backend => Reader::Backend { format: None },
        _ if starts_with_zip_signature(archive)? => Reader::Zip {
            fast: engine == Engine::NativeFast,
        },
        _ if native_tar::sniff(archive)?.is_some() => Reader::Tar,
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
    /// Exact entry paths or folder subtrees to extract, for `.zipper` v2 and
    /// ZIP. Empty extracts everything.
    pub paths: &'a [String],
    /// Progress reports and cancellation. Every reader checks the cancel flag
    /// before it starts. Only ZIP and tar report progress and check it later.
    pub control: Control<'a>,
    /// Cap on declared output bytes. Without it, `.zipper` v2, ZIP and tar
    /// output must fit in free space. The 7-Zip backend and `.zipper` v1
    /// refuse a cap and check no free space.
    pub max_output_bytes: Option<u64>,
    /// Maximum ZIP workers, or tar writer threads, 1–64.
    pub threads: Option<usize>,
    pub engine: Engine,
    /// Shared ZIP entry-buffer budget for `Engine::NativeFast`.
    pub zip_buffer_bytes: Option<usize>,
}

/// Extract any archive Zipper reads into `destination`, choosing the reader
/// as `zipper extract` does.
pub fn extract_archive(archive: &Path, destination: &Path, options: ExtractOptions) -> Result<()> {
    ensure!(
        options
            .threads
            .is_none_or(|threads| (1..=64).contains(&threads)),
        "threads must be between 1 and 64"
    );
    crate::progress::Tracker::new(options.control, None, None).check()?;
    let reader = choose_reader(options.engine, archive)?;
    match reader {
        Reader::Zipper => {
            crate::extract_with(
                archive,
                destination,
                options.paths,
                options.max_output_bytes,
                options.existing,
            )?;
        }
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
                    control: options.control,
                },
            )?;
        }
        Reader::Tar | Reader::Backend { .. } if !options.paths.is_empty() => {
            bail!("--path needs a .zipper or ZIP archive")
        }
        Reader::Tar => {
            native_tar::read_archive(
                archive,
                Some(destination),
                native_tar::ReadOptions {
                    max_output_bytes: options.max_output_bytes,
                    writers: options.threads,
                    existing: options.existing,
                    control: options.control,
                },
            )?;
        }
        Reader::Backend { format } => {
            ensure!(
                options.max_output_bytes.is_none(),
                "--max-extracted-bytes needs a .zipper archive or a ZIP or tar read in process"
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
