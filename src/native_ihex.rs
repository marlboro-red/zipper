//! In-process extraction of Intel HEX files, as 7-Zip extracts them: one
//! file for each run of data records whose addresses follow on. See
//! `docs/reference/ihex-reader.md`.
use crate::Entry;
use crate::ihex::Records;
use crate::output_name::{archive_file_name, default_name, named, safe_output_name};
use crate::tree;
pub use crate::tree::ReadOptions;
use anyhow::{Context, Result, ensure};
use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
};

const MAX_RUNS: usize = tree::MAX_ENTRIES;
const FILE_MODE: u32 = 0o644;
const READ_BUFFER: usize = 1 << 16;
const RECORDS_PER_CHECK: u32 = 1 << 16;

fn opened(path: &Path) -> Result<Records<BufReader<crate::volumes::ArchiveFile>>> {
    Records::new(BufReader::with_capacity(
        READ_BUFFER,
        crate::volumes::open(path)?,
    ))
}

fn run_sizes(path: &Path, check: impl Fn() -> Result<()>) -> Result<Vec<u64>> {
    let mut records = opened(path)?;
    let mut sizes: Vec<u64> = Vec::new();
    let mut since_check = 0u32;
    while let Some((starts_run, length)) = records.next_data()? {
        if starts_run {
            ensure!(
                sizes.len() < MAX_RUNS,
                "the Intel HEX file holds more than {MAX_RUNS} runs of data"
            );
            sizes.push(0);
        }
        if let Some(size) = sizes.last_mut() {
            *size += length as u64;
        }
        since_check += 1;
        if since_check == RECORDS_PER_CHECK {
            since_check = 0;
            check()?;
        }
    }
    records.ensure_nothing_follows()?;
    Ok(sizes)
}

struct RunBytes<R> {
    records: Records<R>,
    at: usize,
    end: usize,
    at_next_run: bool,
    started: bool,
}

impl<R: BufRead> Read for RunBytes<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.at_next_run {
                return Ok(0);
            }
            if self.at < self.end {
                let length = out.len().min(self.end - self.at);
                out[..length].copy_from_slice(&self.records.data[self.at..self.at + length]);
                self.at += length;
                return Ok(length);
            }
            match self.records.next_data() {
                Ok(Some((starts_run, length))) => {
                    self.at = 0;
                    self.end = length;
                    self.at_next_run = starts_run && self.started;
                    self.started = true;
                }
                Ok(None) => return Ok(0),
                Err(error) => {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
                }
            }
        }
    }
}

impl<R: BufRead> RunBytes<R> {
    fn end_run(&mut self) -> Result<()> {
        let mut more = [0u8; 1];
        ensure!(
            self.read(&mut more)? == 0,
            "the Intel HEX file changed while it was read"
        );
        self.at_next_run = false;
        Ok(())
    }
}

/// Returns whether the file starts as 7-Zip finds an Intel HEX file from
/// its content: a colon, then three data records, or fewer ending in an
/// end-of-file record, each with its checksum.
pub fn sniff(path: &Path) -> Result<bool> {
    crate::ihex::sniff(path)
}

/// Verify and optionally extract an Intel HEX file into a new directory, in
/// the tree `7zz x` writes: one file for each run of data records whose
/// addresses follow on. One run takes the archive's name without its
/// extension, and several take the names `0`, `1` and on, in file order.
/// Every check of `docs/reference/ihex-reader.md` holds before anything is
/// published.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let archive_name = archive_file_name(&named(path))?;
    let alone = safe_output_name(&default_name(&archive_name, ""), &archive_name);
    tree::extract("Intel HEX", path, destination, options, |tree| {
        let sizes = run_sizes(path, || tree.check())?;
        if sizes.is_empty() {
            return Ok(());
        }
        let mut bytes = RunBytes {
            records: opened(path)?,
            at: 0,
            end: 0,
            at_next_run: false,
            started: false,
        };
        for (index, &size) in sizes.iter().enumerate() {
            tree.check()?;
            let name = match sizes.len() {
                1 => alone.clone(),
                _ => index.to_string(),
            };
            tree.file(&name, size, FILE_MODE, None, &mut bytes)
                .context("cannot read the Intel HEX file")?;
            bytes.end_run()?;
        }
        ensure!(
            bytes.records.ended,
            "the Intel HEX file changed while it was read"
        );
        bytes.records.ensure_nothing_follows()
    })
}
