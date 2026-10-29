//! Separate ZIP work from file materialization; not a theoretical lower bound.
//! Usage: small_zip_costs ARCHIVE SOURCE [ROUNDS] [THREADS]
use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use std::{fs, io::Write, path::PathBuf, time::Instant};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let archive = PathBuf::from(args.get(1).context("archive required")?);
    let source = PathBuf::from(args.get(2).context("source required")?);
    let rounds: usize = args.get(3).map_or(Ok(7), |s| s.parse())?;
    let threads: usize = args.get(4).map_or(Ok(4), |s| s.parse())?;
    ensure!(
        rounds > 0 && (1..=64).contains(&threads),
        "invalid run/thread count"
    );
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()?;
    let prefix = source.file_name().context("source basename required")?;
    let mut files = Vec::new();
    let mut directories = vec![PathBuf::from(prefix)];
    for entry in walkdir::WalkDir::new(&source)
        .min_depth(1)
        .sort_by_file_name()
    {
        let entry = entry?;
        let path = PathBuf::from(prefix).join(entry.path().strip_prefix(&source)?);
        if entry.file_type().is_dir() {
            directories.push(path);
        } else {
            ensure!(entry.file_type().is_file(), "regular files only");
            files.push((path, fs::read(entry.path())?));
        }
    }
    let root = tempfile::tempdir()?;
    println!("round,operation,seconds,verified_bytes");
    for round in 0..=rounds {
        for slot in 0..3 {
            let operation = ["extract", "verify", "materialize"][(round + slot) % 3];
            let destination = root.path().join("out");
            let start = Instant::now();
            match operation {
                "extract" => {
                    zipper::native_zip::read_archive(&archive, Some(&destination))?;
                }
                "verify" => {
                    zipper::native_zip::read_archive(&archive, None)?;
                }
                _ => {
                    let stage = tempfile::tempdir_in(root.path())?;
                    for directory in &directories {
                        fs::create_dir_all(stage.path().join(directory))?;
                    }
                    files
                        .par_iter()
                        .try_for_each(|(path, bytes)| -> Result<()> {
                            fs::File::options()
                                .write(true)
                                .create_new(true)
                                .open(stage.path().join(path))?
                                .write_all(bytes)?;
                            Ok(())
                        })?;
                    fs::rename(stage.path(), &destination)?;
                }
            }
            let elapsed = start.elapsed().as_secs_f64();
            let mut verified = 0usize;
            if operation != "verify" {
                let count = walkdir::WalkDir::new(&destination)
                    .min_depth(1)
                    .into_iter()
                    .collect::<std::result::Result<Vec<_>, _>>()?
                    .len();
                ensure!(
                    count == files.len() + directories.len(),
                    "tree membership mismatch"
                );
                for (path, bytes) in &files {
                    ensure!(fs::read(destination.join(path))? == *bytes, "byte mismatch");
                    verified += bytes.len();
                }
                fs::remove_dir_all(&destination)?;
            }
            if round > 0 {
                println!("{round},{operation},{elapsed:.9},{verified}");
            }
        }
    }
    Ok(())
}
