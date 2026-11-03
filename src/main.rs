use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use zipper::Reader;

#[derive(Parser)]
#[command(version, about = "Zipper — a parallel archive engine")]
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
    /// Native for ZIP, tar and unrecognized files, with no fallback; 7-Zip for other recognized formats.
    Auto,
    /// 7-Zip for every archive that is not .zipper.
    Backend,
    Native,
    /// Native ZIP with a shared entry-buffer budget and streaming fallback.
    NativeFast,
}
#[derive(Subcommand)]
enum Command {
    /// Create a native .zipper archive.
    Create {
        archive: PathBuf,
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(short, long, default_value_t = 1, allow_hyphen_values = true)]
        level: i32,
        /// Block size in KiB (power of two, 64–16384); larger uses more memory.
        #[arg(long, default_value_t = 1024, value_parser = clap::value_parser!(u32).range(64..=16384))]
        block_size_kib: u32,
    },
    /// Extract into a new directory, or into an existing one with --overwrite.
    Extract {
        archive: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Extract into an existing directory, replacing files of the same name.
        #[arg(long)]
        overwrite: bool,
        /// Extract an exact archive path or folder subtree (.zipper v2 and ZIP; repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Refuse archives whose declared output exceeds this many bytes. By
        /// default, ZIP (native engines) and v2 output must fit in free space.
        #[arg(long)]
        max_extracted_bytes: Option<u64>,
    },
    /// List entries (v2 validates the index; use test to verify payloads).
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
        Command::Create {
            archive,
            inputs,
            level,
            block_size_kib,
        } => zipper::create_with_options(
            &archive,
            &inputs,
            zipper::CreateOptions {
                level,
                block_size: block_size_kib as usize * 1024,
            },
        )?,
        Command::Extract {
            archive,
            output,
            paths,
            overwrite,
            max_extracted_bytes,
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
                Reader::Zipper => print(zipper::list_archive(&archive)?),
                Reader::Zip { .. } => print(zipper::native_zip::list(&archive)?),
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
                Reader::Zipper => report(zipper::read_archive(&archive, None)?),
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
