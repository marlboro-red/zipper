use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Zipper — a parallel archive engine")]
struct Cli {
    /// Worker threads (defaults to available CPUs).
    #[arg(long, global = true)]
    threads: Option<usize>,
    #[command(subcommand)]
    command: Command,
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
        } => zipper::create(&archive, &inputs, level)?,
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
            } else {
                zipper::compat::run("x", &archive, Some(&output))?;
            }
        }
        Command::List { archive } => {
            if !zipper::compat::is_native(&archive)? {
                return zipper::compat::run("l", &archive, None);
            }
            for e in zipper::list_archive(&archive)? {
                println!(
                    "{}\t{}\t{:?}",
                    if e.directory { "dir" } else { "file" },
                    e.size,
                    e.name
                );
            }
        }
        Command::Test { archive } => {
            if !zipper::compat::is_native(&archive)? {
                return zipper::compat::run("t", &archive, None);
            }
            let entries = zipper::read_archive(&archive, None)?;
            println!("Verified {} entries", entries.len());
        }
    }
    Ok(())
}
