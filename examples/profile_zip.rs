use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

const USAGE: &str =
    "usage: profile_zip ARCHIVE [ROUNDS] [THREADS] repeats extraction for external CPU sampling";

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let archive = PathBuf::from(args.get(1).context(USAGE)?);
    let rounds: usize = args.get(2).map_or(Ok(20), |n| n.parse())?;
    let threads: usize = args.get(3).map_or(Ok(4), |n| n.parse())?;
    ensure!((1..=64).contains(&threads), "threads must be 1..64");
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()?;
    let root = tempfile::tempdir()?;
    for round in 0..rounds {
        let output = root.path().join(round.to_string());
        zipper::native_zip::read_archive(&archive, Some(&output))?;
    }
    Ok(())
}
