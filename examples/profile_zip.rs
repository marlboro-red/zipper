//! Repeat extraction for external CPU sampling; cleanup occurs after all reads.
//! Usage: profile_zip ARCHIVE native|native-fast [ROUNDS] [THREADS]
use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let archive = PathBuf::from(args.get(1).context("missing archive")?);
    let mode = args.get(2).context("missing native or native-fast mode")?;
    ensure!(
        matches!(mode.as_str(), "native" | "native-fast"),
        "invalid mode"
    );
    let rounds: usize = args.get(3).map_or(Ok(20), |n| n.parse())?;
    let threads: usize = args.get(4).map_or(Ok(4), |n| n.parse())?;
    ensure!((1..=64).contains(&threads), "threads must be 1..64");
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()?;
    let root = tempfile::tempdir()?;
    for round in 0..rounds {
        let output = root.path().join(round.to_string());
        if mode == "native-fast" {
            zipper::native_zip::read_archive_fast(&archive, Some(&output))?;
        } else {
            zipper::native_zip::read_archive(&archive, Some(&output))?;
        }
    }
    Ok(())
}
