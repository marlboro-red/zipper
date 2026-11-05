use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use zipper::Reader;

#[derive(Parser)]
#[command(
    version,
    about = "Zipper — fast, safe extraction of ZIP, tar and 7z archives"
)]
struct Cli {
    /// Maximum worker threads (defaults to available CPUs; tiny ZIPs may use fewer; tar uses 4 writers).
    #[arg(long, global = true)]
    threads: Option<usize>,
    /// Reader for ZIP, tar and other foreign archives. Native readers restore files, directories, modes, times and safe links.
    #[arg(long, global = true, value_enum, default_value_t = ZipEngine::Auto)]
    zip_engine: ZipEngine,
    /// Shared native-fast ZIP entry-buffer MiB (128 on ARM64 macOS, 64 elsewhere; 0 streams).
    /// Metadata, worker state and verification archive cache are additional.
    #[arg(long, global = true, value_parser = clap::value_parser!(u32).range(0..=4096))]
    zip_buffer_mib: Option<u32>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ZipEngine {
    /// Native for ZIP, tar, unrecognized files and 7z it decodes; 7-Zip for other recognized formats.
    Auto,
    /// 7-Zip for every archive.
    Backend,
    Native,
    /// Native ZIP with a shared entry-buffer budget and streaming fallback.
    NativeFast,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExistingArg {
    /// Refuse an existing destination.
    Refuse,
    /// Replace each file of the same name (7-Zip -aoa).
    Overwrite,
    /// Keep each existing file and leave the archive's copy out (7-Zip -aos).
    Skip,
    /// Write the archive's copy as name_1.ext (7-Zip -aou).
    RenameNew,
    /// Rename the existing file to name_1.ext first (7-Zip -aot).
    RenameExisting,
}
impl From<ExistingArg> for zipper::Existing {
    fn from(existing: ExistingArg) -> Self {
        match existing {
            ExistingArg::Refuse => zipper::Existing::Refuse,
            ExistingArg::Overwrite => zipper::Existing::Overwrite,
            ExistingArg::Skip => zipper::Existing::Skip,
            ExistingArg::RenameNew => zipper::Existing::RenameNew,
            ExistingArg::RenameExisting => zipper::Existing::RenameExisting,
        }
    }
}
#[derive(Subcommand)]
enum Command {
    /// Extract into a new directory, or into an existing one with --overwrite.
    Extract {
        /// The archive, or `-` to read it from standard input.
        archive: PathBuf,
        #[arg(short, long, required_unless_present = "stdout")]
        output: Option<PathBuf>,
        /// Write the one selected file to standard output instead of a folder (7-Zip -so).
        #[arg(long, conflicts_with_all = ["output", "flat", "overwrite", "existing"])]
        stdout: bool,
        /// Extract into an existing directory, replacing files of the same name.
        #[arg(long, conflicts_with = "existing")]
        overwrite: bool,
        /// What to do with paths the existing destination already holds.
        #[arg(long, value_enum)]
        existing: Option<ExistingArg>,
        /// Extract an exact archive path or folder subtree (repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Extract entries matching a wildcard pattern (repeatable). `*` and `?`
        /// stay in one name, `**` crosses folders, and a pattern without `/`
        /// matches a name at any depth.
        #[arg(long)]
        include: Vec<String>,
        /// Leave out entries matching a wildcard pattern (repeatable).
        #[arg(long)]
        exclude: Vec<String>,
        /// Extract every file into the output folder itself, without its
        /// folders (7-Zip e). Files that share a name follow --existing.
        #[arg(long)]
        flat: bool,
        /// Extract only this archive folder, with its contents at the output
        /// root: with `a`, `a/b/c.txt` lands at `<output>/b/c.txt`.
        #[arg(long, value_name = "PATH", conflicts_with = "strip_components")]
        strip_prefix: Option<String>,
        /// Drop this many leading folders from every path, as tar does. An
        /// entry with no more folders than that is not extracted.
        #[arg(long, value_name = "N")]
        strip_components: Option<usize>,
        /// Write file contents only: no modification times, no Windows
        /// attributes and no Mark of the Web. Files keep only the execute
        /// bit, and folders get the default mode.
        #[arg(long)]
        content_only: bool,
        /// Refuse archives whose declared output exceeds this many bytes. By
        /// default, ZIP, tar and 7z output read in process must fit in free space.
        #[arg(long)]
        max_extracted_bytes: Option<u64>,
    },
    /// List entries (checks the index only; use test to verify payloads).
    List { archive: PathBuf },
    /// Verify archive integrity without extracting.
    Test { archive: PathBuf },
    /// Find byte-identical files in input paths (read-only; skips symlinks).
    Duplicates {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Print totals without listing individual paths.
        #[arg(long)]
        summary: bool,
    },
}
impl From<ZipEngine> for zipper::Engine {
    fn from(engine: ZipEngine) -> Self {
        match engine {
            ZipEngine::Auto => zipper::Engine::Auto,
            ZipEngine::Backend => zipper::Engine::Backend,
            ZipEngine::Native => zipper::Engine::Native,
            ZipEngine::NativeFast => zipper::Engine::NativeFast,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(n) = cli.threads {
        anyhow::ensure!((1..=64).contains(&n), "threads must be between 1 and 64");
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()?;
    }
    anyhow::ensure!(
        cli.zip_buffer_mib.is_none() || cli.zip_engine == ZipEngine::NativeFast,
        "--zip-buffer-mib requires --zip-engine native-fast"
    );
    let buffer_bytes = cli
        .zip_buffer_mib
        .map(|mib| {
            usize::try_from(u64::from(mib) * 1024 * 1024)
                .map_err(|_| anyhow::anyhow!("ZIP buffer budget exceeds platform address space"))
        })
        .transpose()?;
    match cli.command {
        Command::Duplicates { inputs, summary } => {
            let report = zipper::duplicates::find(&inputs)?;
            for (index, group) in report.groups.iter().enumerate().filter(|_| !summary) {
                println!(
                    "Group {}: {} files, {} bytes each",
                    index + 1,
                    group.paths.len(),
                    group.size
                );
                for path in &group.paths {
                    println!("  {:?}", path);
                }
            }
            println!(
                "Scanned {} files; {} duplicate groups; {} duplicate payload bytes; {} non-regular entries skipped",
                report.scanned_files,
                report.groups.len(),
                report.duplicate_bytes,
                report.skipped_entries
            );
        }
        Command::Extract {
            archive,
            output,
            stdout,
            paths,
            include,
            exclude,
            flat,
            strip_prefix,
            strip_components,
            content_only,
            overwrite,
            existing,
            max_extracted_bytes,
        } => {
            let to_stdout = stdout.then(tempfile::tempdir).transpose()?;
            let output = match (&to_stdout, output) {
                (Some(work), _) => work.path().join("out"),
                (None, output) => output.expect("clap requires --output without --stdout"),
            };
            let spooled = if archive.as_os_str() == "-" {
                let folder = output
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty());
                let mut spool = tempfile::Builder::new()
                    .prefix(".zipper-stdin-")
                    .tempfile_in(folder.unwrap_or(std::path::Path::new(".")))?;
                std::io::copy(&mut std::io::stdin().lock(), &mut spool)?;
                Some(spool)
            } else {
                None
            };
            let archive = spooled
                .as_ref()
                .map_or(archive, |spool| spool.path().to_owned());
            zipper::extract_archive(
                &archive,
                &output,
                zipper::ExtractOptions {
                    existing: match (overwrite, existing) {
                        (true, _) => zipper::Existing::Overwrite,
                        (false, existing) => existing.unwrap_or(ExistingArg::Refuse).into(),
                    },
                    paths: &paths,
                    include: &include,
                    exclude: &exclude,
                    flat: flat || stdout,
                    strip: match (&strip_prefix, strip_components) {
                        (Some(prefix), _) => zipper::Strip::Prefix(prefix),
                        (None, Some(count)) => zipper::Strip::Components(count),
                        (None, None) => zipper::Strip::None,
                    },
                    metadata: if content_only {
                        zipper::Metadata::ContentOnly
                    } else {
                        zipper::Metadata::Restore
                    },
                    max_output_bytes: max_extracted_bytes,
                    threads: cli.threads,
                    engine: cli.zip_engine.into(),
                    zip_buffer_bytes: buffer_bytes,
                    ..Default::default()
                },
            )?;
            if to_stdout.is_some() {
                let files: Vec<PathBuf> = std::fs::read_dir(&output)?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<std::io::Result<_>>()?;
                let [file] = &files[..] else {
                    anyhow::bail!(
                        "--stdout needs a selection of exactly one file; it matched {}",
                        files.len()
                    );
                };
                std::io::copy(
                    &mut std::fs::File::open(file)?,
                    &mut std::io::stdout().lock(),
                )?;
            }
        }
        Command::List { archive } => {
            let print = |entries: Vec<zipper::Entry>| {
                for e in entries {
                    println!(
                        "{}\t{}\t{:?}",
                        if e.directory { "dir" } else { "file" },
                        e.size,
                        e.name
                    );
                }
            };
            zipper::read_volumes(&archive, || {
                match zipper::choose_reader(cli.zip_engine.into(), &archive)? {
                    Reader::Zip { .. } => print(zipper::native_zip::list(&archive)?),
                    Reader::SevenZip => print(zipper::native_7z::list(&archive)?),
                    Reader::Tar => print(zipper::native_tar::read_archive(
                        &archive,
                        None,
                        zipper::native_tar::ReadOptions::default(),
                    )?),
                    Reader::Stream => print(zipper::native_stream::read_archive(
                        &archive,
                        None,
                        zipper::native_stream::ReadOptions::default(),
                    )?),
                    Reader::Cpio => print(zipper::native_cpio::read_archive(
                        &archive,
                        None,
                        zipper::native_cpio::ReadOptions::default(),
                    )?),
                    Reader::Ar => print(zipper::native_ar::read_archive(
                        &archive,
                        None,
                        zipper::native_ar::ReadOptions::default(),
                    )?),
                    Reader::Backend { format } => zipper::compat::run(
                        "l",
                        &archive,
                        None,
                        format.as_deref(),
                        zipper::Existing::Refuse,
                    )?,
                }
                Ok::<_, anyhow::Error>(())
            })?;
        }
        Command::Test { archive } => {
            let report =
                |entries: Vec<zipper::Entry>| println!("Verified {} entries", entries.len());
            zipper::read_volumes(&archive, || {
                match zipper::choose_reader(cli.zip_engine.into(), &archive)? {
                    Reader::Zip { fast } => report(zipper::native_zip::read_archive_with_options(
                        &archive,
                        None,
                        zipper::native_zip::ReadOptions {
                            fast,
                            threads: cli.threads,
                            buffer_bytes,
                            max_output_bytes: None,
                            ..Default::default()
                        },
                    )?),
                    Reader::SevenZip => report(zipper::native_7z::read_archive(
                        &archive,
                        None,
                        zipper::native_7z::ReadOptions {
                            threads: cli.threads,
                            ..Default::default()
                        },
                    )?),
                    Reader::Tar => report(zipper::native_tar::read_archive(
                        &archive,
                        None,
                        zipper::native_tar::ReadOptions::default(),
                    )?),
                    Reader::Stream => report(zipper::native_stream::read_archive(
                        &archive,
                        None,
                        zipper::native_stream::ReadOptions::default(),
                    )?),
                    Reader::Cpio => report(zipper::native_cpio::read_archive(
                        &archive,
                        None,
                        zipper::native_cpio::ReadOptions::default(),
                    )?),
                    Reader::Ar => report(zipper::native_ar::read_archive(
                        &archive,
                        None,
                        zipper::native_ar::ReadOptions::default(),
                    )?),
                    Reader::Backend { format } => zipper::compat::run(
                        "t",
                        &archive,
                        None,
                        format.as_deref(),
                        zipper::Existing::Refuse,
                    )?,
                }
                Ok::<_, anyhow::Error>(())
            })?;
        }
    }
    Ok(())
}
