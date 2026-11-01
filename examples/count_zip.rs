use anyhow::{Context, Result, ensure};
use std::path::PathBuf;

const USAGE: &str =
    "usage: count_zip ARCHIVE native|native-fast THREADS prints one JSON object of counters";

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let archive = PathBuf::from(args.get(1).context(USAGE)?);
    let mode = args.get(2).context(USAGE)?;
    let threads: usize = args.get(3).context(USAGE)?.parse()?;
    ensure!((1..=64).contains(&threads), "threads must be 1..64");
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build_global()?;
    let root = tempfile::tempdir()?;
    let output = root.path().join("out");
    zipper::counters::take();
    match mode.as_str() {
        "native" => zipper::native_zip::read_archive(&archive, Some(&output))?,
        "native-fast" => zipper::native_zip::read_archive_fast(&archive, Some(&output))?,
        _ => anyhow::bail!(USAGE),
    };
    let fields: Vec<String> = zipper::counters::take()
        .into_iter()
        .map(|(counter, value)| format!("\"{}\": {value}", counter.name()))
        .collect();
    println!("{{{}}}", fields.join(", "));
    Ok(())
}
