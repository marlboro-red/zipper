use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

pub const MAGIC: &[u8; 8] = b"ZIPPER\0\x01";
const CHUNK: usize = 1024 * 1024;
const BATCH: usize = 8;
fn number<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut b = [0; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
fn safe_path(s: &str) -> Result<()> {
    ensure!(!s.is_empty() && s.len() <= 4096, "invalid path length");
    ensure!(!s.contains(['\\', ':', '\0']), "unsafe archive path: {s:?}");
    ensure!(
        s.split('/').all(|c| !c.is_empty() && c != "." && c != ".."),
        "unsafe archive path: {s:?}"
    );
    Ok(())
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

pub fn create(output: &Path, inputs: &[PathBuf], level: i32) -> Result<()> {
    ensure!(!inputs.is_empty(), "at least one input is required");
    ensure!(
        (-7..=22).contains(&level),
        "level must be between -7 and 22"
    );
    ensure!(!output.exists(), "output already exists");
    let output_abs = parent(output)
        .canonicalize()?
        .join(output.file_name().context("invalid output")?);
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    for input in inputs {
        ensure!(
            !fs::symlink_metadata(input)?.file_type().is_symlink(),
            "symlinks are not supported"
        );
        let input = input.canonicalize()?;
        ensure!(
            !output_abs.starts_with(&input),
            "archive must be outside its input tree"
        );
        let base = input.parent().context("cannot archive filesystem root")?;
        for entry in walkdir::WalkDir::new(&input).sort_by_file_name() {
            let entry = entry?;
            ensure!(
                entry.file_type().is_file() || entry.file_type().is_dir(),
                "unsupported file type: {}",
                entry.path().display()
            );
            let name = entry
                .path()
                .strip_prefix(base)?
                .to_str()
                .context("paths must be UTF-8")?
                .replace(std::path::MAIN_SEPARATOR, "/");
            safe_path(&name)?;
            ensure!(names.insert(name.clone()), "duplicate path: {name}");
            entries.push((entry.path().to_owned(), name, entry.file_type().is_dir()));
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent(output))?;
    {
        let mut w = BufWriter::new(temp.as_file_mut());
        w.write_all(MAGIC)?;
        for (path, name, directory) in entries {
            w.write_all(&[if directory { 1 } else { 2 }])?;
            w.write_all(&(name.len() as u32).to_le_bytes())?;
            w.write_all(name.as_bytes())?;
            if directory {
                continue;
            }
            let mut f = File::open(&path)?;
            let size = f.metadata()?.len();
            w.write_all(&size.to_le_bytes())?;
            let mut remaining = size;
            while remaining > 0 {
                let mut batch = Vec::new();
                for _ in 0..BATCH {
                    if remaining == 0 {
                        break;
                    }
                    let len = remaining.min(CHUNK as u64) as usize;
                    let mut raw = vec![0; len];
                    f.read_exact(&mut raw)?;
                    remaining -= len as u64;
                    batch.push(raw);
                }
                let compressed: Result<Vec<_>> = batch
                    .into_par_iter()
                    .map(|raw| {
                        let encoded = zstd::bulk::compress(&raw, level)?;
                        let checksum = crc32fast::hash(&raw);
                        let len = raw.len();
                        Ok(if encoded.len() < len {
                            (1u8, len, checksum, encoded)
                        } else {
                            (0u8, len, checksum, raw)
                        })
                    })
                    .collect();
                for (codec, len, checksum, bytes) in compressed? {
                    w.write_all(&[codec])?;
                    for n in [len as u32, bytes.len() as u32, checksum] {
                        w.write_all(&n.to_le_bytes())?;
                    }
                    w.write_all(&bytes)?;
                }
            }
            ensure!(
                f.read(&mut [0])? == 0,
                "input grew while archiving: {}",
                path.display()
            );
        }
        w.write_all(&[0])?;
        w.flush()?;
    }
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output)?;
    Ok(())
}

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub directory: bool,
}

/// Validate content, optionally extracting into a new directory via private staging.
pub fn read_archive(archive: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
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
