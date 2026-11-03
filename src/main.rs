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
#[derive(Subcommand)]
enum Command {
    /// Extract into a new directory, or into an existing one with --overwrite.
    Extract {
        archive: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Extract into an existing directory, replacing files of the same name.
        #[arg(long)]
        overwrite: bool,
        /// Extract an exact archive path or folder subtree (ZIP and 7z; repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Refuse archives whose declared output exceeds this many bytes. By
        /// default, ZIP, tar and 7z output read in process must fit in free space.
        #[arg(long)]
        max_extracted_bytes: Option<u64>,
        /// Write the NTFS alternate data streams a 7z archive stores onto
        /// their files (Windows only). Without it they are checked, not written.
        #[arg(long)]
        streams: bool,
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
            paths,
            overwrite,
            max_extracted_bytes,
            streams,
        } => {
            zipper::extract_archive(
                &archive,
                &output,
                zipper::ExtractOptions {
                    existing: if overwrite {
                        zipper::Existing::Overwrite
                    } else {
                        zipper::Existing::Refuse
                    },
                    paths: &paths,
                    max_output_bytes: max_extracted_bytes,
                    threads: cli.threads,
                    engine: cli.zip_engine.into(),
                    zip_buffer_bytes: buffer_bytes,
                    alternate_streams: streams,
                    ..Default::default()
                },
            )?;
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
            match zipper::choose_reader(cli.zip_engine.into(), &archive)? {
                Reader::Zip { .. } => print(zipper::native_zip::list(&archive)?),
                Reader::SevenZip => print(zipper::native_7z::list(&archive)?),
                Reader::Tar => print(zipper::native_tar::read_archive(
                    &archive,
                    None,
                    zipper::native_tar::ReadOptions::default(),
                )?),
                Reader::Backend { format } => zipper::compat::run(
                    "l",
                    &archive,
                    None,
                    format.as_deref(),
                    zipper::Existing::Refuse,
                )?,
            }
        }
        Command::Test { archive } => {
            let report =
                |entries: Vec<zipper::Entry>| println!("Verified {} entries", entries.len());
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
                Reader::Backend { format } => zipper::compat::run(
                    "t",
                    &archive,
                    None,
                    format.as_deref(),
                    zipper::Existing::Refuse,
                )?,
            }
        }
    }
    Ok(())
}
