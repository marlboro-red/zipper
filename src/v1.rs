use crate::{Entry, number, parent, safe_path};
use anyhow::{Result, ensure};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
};
const MAGIC: &[u8; 8] = b"ZIPPER\0\x01";
const CHUNK: usize = 1024 * 1024;
const BATCH: usize = 8;
/// Validate content, optionally extracting into a new directory via private staging.
pub(super) fn read_archive(archive: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
    let staging = if let Some(dst) = destination {
        ensure!(
            fs::symlink_metadata(dst).is_err(),
            "extraction destination must not exist"
        );
        Some(tempfile::tempdir_in(parent(dst))?)
    } else {
        None
    };
    let mut r = BufReader::new(File::open(archive)?);
    ensure!(
        &number::<8>(&mut r)? == MAGIC,
        "not a supported Zipper archive"
    );
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    loop {
        let kind = number::<1>(&mut r)?[0];
        if kind == 0 {
            break;
        }
        ensure!(kind == 1 || kind == 2, "invalid entry type");
        let len = u32::from_le_bytes(number(&mut r)?) as usize;
        ensure!(len > 0 && len <= 4096, "invalid path length");
        let mut bytes = vec![0; len];
        r.read_exact(&mut bytes)?;
        let name = String::from_utf8(bytes)?;
        safe_path(&name)?;
        ensure!(names.insert(name.clone()), "duplicate entry: {name}");
        let directory = kind == 1;
        let size = if directory {
            0
        } else {
            u64::from_le_bytes(number(&mut r)?)
        };
        let mut output = None;
        if let Some(temp) = &staging {
            let path = temp.path().join(&name);
            if directory {
                fs::create_dir_all(&path)?;
            } else {
                fs::create_dir_all(parent(&path))?;
                output = Some(BufWriter::new(
                    File::options().write(true).create_new(true).open(path)?,
                ));
            }
        }
        let mut remaining = size;
        while remaining > 0 {
            let mut batch = Vec::new();
            for _ in 0..BATCH {
                if remaining == 0 {
                    break;
                }
                let codec = number::<1>(&mut r)?[0];
                let raw = u32::from_le_bytes(number(&mut r)?) as usize;
                let stored = u32::from_le_bytes(number(&mut r)?) as usize;
                let checksum = u32::from_le_bytes(number(&mut r)?);
                ensure!(
                    raw > 0 && raw <= CHUNK && raw as u64 <= remaining,
                    "invalid chunk size"
                );
                ensure!(stored > 0 && stored <= CHUNK, "invalid stored size");
                ensure!(codec <= 1, "unknown codec");
                let mut data = vec![0; stored];
                r.read_exact(&mut data)?;
                remaining -= raw as u64;
                batch.push((codec, raw, checksum, data));
            }
            let decoded: Result<Vec<_>> = batch
                .into_par_iter()
                .map(|(codec, raw, checksum, data)| {
                    let data = if codec == 0 {
                        data
                    } else {
                        zstd::bulk::decompress(&data, raw)?
                    };
                    ensure!(data.len() == raw, "chunk length mismatch");
                    ensure!(
                        crc32fast::hash(&data) == checksum,
                        "chunk checksum mismatch"
                    );
                    Ok(data)
                })
                .collect();
            for data in decoded? {
                if let Some(w) = &mut output {
                    w.write_all(&data)?;
                }
            }
        }
        if let Some(mut w) = output {
            w.flush()?;
        }
        entries.push(Entry {
            name,
            size,
            directory,
        });
    }
    ensure!(r.read(&mut [0])? == 0, "unexpected trailing data");
    if let (Some(temp), Some(dst)) = (staging, destination) {
        ensure!(
            fs::symlink_metadata(dst).is_err(),
            "destination appeared during extraction"
        );
        fs::rename(temp.path(), dst)?;
    }
    Ok(entries)
}
