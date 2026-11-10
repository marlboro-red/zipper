use std::fs::{self, File};
use std::io::{self, BufWriter};
use std::path::Path;

use sevenz_rust2::{ArchiveReader, Password};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, archive, destination, threads] = &args[..] else {
        return Err("usage: sevenz-rust2-extract ARCHIVE DESTINATION THREADS|auto".into());
    };
    let destination = Path::new(destination);
    fs::create_dir(destination)?;
    let mut reader = ArchiveReader::new(File::open(archive)?, Password::empty())?;
    if threads != "auto" {
        reader.set_thread_count(threads.parse()?);
    }
    reader.for_each_entries(|entry, data| {
        let path = destination.join(entry.name());
        if entry.is_directory() {
            fs::create_dir_all(&path)?;
            return Ok(true);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        io::copy(data, &mut BufWriter::with_capacity(1 << 20, File::create(&path)?))?;
        Ok(true)
    })?;
    Ok(())
}
