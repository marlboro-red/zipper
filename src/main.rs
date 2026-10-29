use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Zipper — a parallel archive engine")]
struct Cli {
    /// Maximum worker threads (defaults to available CPUs; tiny ZIPs may use fewer).
    #[arg(long, global = true)]
    threads: Option<usize>,
    /// ZIP reader: native is experimental and preserves bytes/directories only.
    #[arg(long, global = true, value_enum, default_value_t = ZipEngine::Backend)]
    zip_engine: ZipEngine,
    #[command(subcommand)]
    command: Command,
}
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ZipEngine {
    Backend,
    Native,
    /// Native ZIP with up to 64 MiB of entry buffers per worker.
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
    /// Extract into a new directory.
    Extract {
        archive: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Extract an exact archive path or directory subtree (v2 only; repeatable).
        #[arg(long = "path")]
        paths: Vec<String>,
    },
    /// List entries (v2 validates the index; use test to verify payloads).
    List { archive: PathBuf },
    /// Verify archive integrity without extracting.
    Test { archive: PathBuf },
}
fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(n) = cli.threads {
        anyhow::ensure!((1..=64).contains(&n), "threads must be between 1 and 64");
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()?;
    }
    match cli.command {
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
        } => {
            if !paths.is_empty() {
                zipper::extract_selected(&archive, &output, &paths)?;
                return Ok(());
            }
            if zipper::compat::is_native(&archive)? {
                zipper::read_archive(&archive, Some(&output))?;
            } else if cli.zip_engine != ZipEngine::Backend {
                zipper::native_zip::read_archive_with_options(
                    &archive,
                    Some(&output),
                    zipper::native_zip::ReadOptions {
                        fast: cli.zip_engine == ZipEngine::NativeFast,
                        threads: cli.threads,
                    },
                )?;
            } else {
                zipper::compat::run("x", &archive, Some(&output))?;
            }
        }
        Command::List { archive } => {
            let native = zipper::compat::is_native(&archive)?;
            if !native && cli.zip_engine == ZipEngine::Backend {
                return zipper::compat::run("l", &archive, None);
            }
            let entries = if native {
                zipper::list_archive(&archive)?
            } else {
                zipper::native_zip::list(&archive)?
            };
            for e in entries {
                println!(
                    "{}\t{}\t{:?}",
                    if e.directory { "dir" } else { "file" },
                    e.size,
                    e.name
                );
            }
        }
        Command::Test { archive } => {
            let native = zipper::compat::is_native(&archive)?;
            if !native && cli.zip_engine == ZipEngine::Backend {
                return zipper::compat::run("t", &archive, None);
            }
            let entries = if native {
                zipper::read_archive(&archive, None)?
            } else {
                zipper::native_zip::read_archive_with_options(
                    &archive,
                    None,
                    zipper::native_zip::ReadOptions {
                        fast: cli.zip_engine == ZipEngine::NativeFast,
                        threads: cli.threads,
                    },
                )?
            };
            println!("Verified {} entries", entries.len());
        }
    }
    Ok(())
}
