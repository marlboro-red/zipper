//! Read-only duplicate-content discovery. Hashes filter candidates; byte
//! comparison establishes identity. Inputs must stay unchanged during scanning.
use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Read,
    path::PathBuf,
    time::SystemTime,
};

const BUFFER: usize = 64 * 1024;

#[derive(Debug)]
pub struct DuplicateGroup {
    pub size: u64,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct DuplicateReport {
    pub scanned_files: usize,
    pub skipped_entries: usize,
    pub groups: Vec<DuplicateGroup>,
    /// Redundant logical payload bytes, not filesystem space savings.
    pub duplicate_bytes: u64,
}

#[derive(Clone)]
struct Input {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

fn check(file: &File, input: &Input) -> Result<()> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.len() == input.size
            && metadata.modified().ok() == input.modified,
        "file changed during duplicate scan: {:?}",
        input.path
    );
    Ok(())
}

fn fingerprint(input: &Input, prefix_only: bool) -> Result<u32> {
    let mut file = File::open(&input.path).with_context(|| format!("open {:?}", input.path))?;
    check(&file, input)?;
    let mut remaining = if prefix_only {
        input.size.min(BUFFER as u64)
    } else {
        input.size
    };
    let mut buffer = vec![0; BUFFER];
    let mut hash = crc32fast::Hasher::new();
    while remaining > 0 {
        let count = remaining.min(BUFFER as u64) as usize;
        file.read_exact(&mut buffer[..count])
            .with_context(|| format!("read {:?}", input.path))?;
        hash.update(&buffer[..count]);
        remaining -= count as u64;
    }
    check(&file, input)?;
    Ok(hash.finalize())
}

fn equal(a: &Input, b: &Input) -> Result<bool> {
    ensure!(
        a.size == b.size,
        "internal duplicate candidate size mismatch"
    );
    let mut left = File::open(&a.path)?;
    let mut right = File::open(&b.path)?;
    check(&left, a)?;
    check(&right, b)?;
    let mut remaining = a.size;
    let mut lhs = vec![0; BUFFER];
    let mut rhs = vec![0; BUFFER];
    let mut same = true;
    while remaining > 0 {
        let count = remaining.min(BUFFER as u64) as usize;
        left.read_exact(&mut lhs[..count])?;
        right.read_exact(&mut rhs[..count])?;
        if lhs[..count] != rhs[..count] {
            same = false;
            break;
        }
        remaining -= count as u64;
    }
    check(&left, a)?;
    check(&right, b)?;
    Ok(same)
}

// A hash collision only adds comparisons: it can never declare unequal bytes
// duplicates. Retain multiple representatives within each fingerprint bucket.
fn partition(candidates: Vec<Input>) -> Result<Vec<DuplicateGroup>> {
    let mut groups: Vec<Vec<Input>> = Vec::new();
    for candidate in candidates {
        let mut matching = None;
        for (index, group) in groups.iter().enumerate() {
            if equal(&group[0], &candidate)? {
                matching = Some(index);
                break;
            }
        }
        if let Some(index) = matching {
            groups[index].push(candidate);
        } else {
            groups.push(vec![candidate]);
        }
    }
    Ok(groups
        .into_iter()
        .filter(|group| group.len() > 1)
        .map(|group| DuplicateGroup {
            size: group[0].size,
            paths: group.into_iter().map(|input| input.path).collect(),
        })
        .collect())
}

/// Find byte-identical regular files across input files/directories. Overlapping
/// paths are counted once; symlinks and special files are skipped, not followed.
/// Different hard-link paths are reported, so duplicate_bytes is not disk savings.
pub fn find(inputs: &[PathBuf]) -> Result<DuplicateReport> {
    ensure!(!inputs.is_empty(), "at least one input is required");
    let mut files = BTreeMap::new();
    let mut skipped_entries = 0;
    for input in inputs {
        for entry in walkdir::WalkDir::new(input)
            .follow_links(false)
            .follow_root_links(false)
        {
            let entry = entry?;
            if entry.file_type().is_dir() {
                continue;
            }
            if !entry.file_type().is_file() {
                skipped_entries += 1;
                continue;
            }
            let path = fs::canonicalize(entry.path())?;
            let metadata = entry.metadata()?;
            files.entry(path.clone()).or_insert(Input {
                path,
                size: metadata.len(),
                modified: metadata.modified().ok(),
            });
        }
    }
    let scanned_files = files.len();
    let mut sizes: BTreeMap<u64, Vec<Input>> = BTreeMap::new();
    for input in files.into_values() {
        sizes.entry(input.size).or_default().push(input);
    }
    // Unique sizes need no payload I/O. Prefix fingerprints cheaply exclude
    // same-size files before full hashing, and tiny files are hashed only once.
    let candidates: Vec<_> = sizes
        .into_values()
        .filter(|group| group.len() > 1)
        .flatten()
        .collect();
    let prefixes: Vec<_> = candidates
        .into_par_iter()
        .map(|input| {
            let hash = fingerprint(&input, true)?;
            Ok((input, hash))
        })
        .collect::<Result<_>>()?;
    let mut buckets: BTreeMap<(u64, u32), Vec<Input>> = BTreeMap::new();
    for (input, hash) in prefixes {
        buckets.entry((input.size, hash)).or_default().push(input);
    }
    let candidates: Vec<_> = buckets
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .flat_map(|((_, prefix), group)| group.into_iter().map(move |input| (input, prefix)))
        .collect();
    let hashes: Vec<_> = candidates
        .into_par_iter()
        .map(|(input, prefix)| {
            let hash = if input.size <= BUFFER as u64 {
                prefix
            } else {
                fingerprint(&input, false)?
            };
            Ok((input, hash))
        })
        .collect::<Result<_>>()?;
    let mut buckets: BTreeMap<(u64, u32), Vec<Input>> = BTreeMap::new();
    for (input, hash) in hashes {
        buckets.entry((input.size, hash)).or_default().push(input);
    }
    let mut groups: Vec<_> = buckets
        .into_values()
        .filter(|group| group.len() > 1)
        .map(partition)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    for group in &mut groups {
        group.paths.sort();
    }
    groups.sort_by(|a, b| a.paths.cmp(&b.paths));
    let mut duplicate_bytes = 0u64;
    for group in &groups {
        duplicate_bytes = group
            .size
            .checked_mul((group.paths.len() - 1) as u64)
            .and_then(|bytes| duplicate_bytes.checked_add(bytes))
            .context("duplicate byte count overflow")?;
    }
    Ok(DuplicateReport {
        scanned_files,
        skipped_entries,
        groups,
        duplicate_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_collisions_require_byte_comparison() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut candidates = Vec::new();
        for (name, bytes) in [("a", b"aaa"), ("b", b"bbb"), ("c", b"aaa"), ("d", b"bbb")] {
            let path = root.path().join(name);
            fs::write(&path, bytes)?;
            candidates.push(Input {
                modified: fs::metadata(&path)?.modified().ok(),
                path,
                size: 3,
            });
        }
        let groups = partition(candidates)?;
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0]
                .paths
                .iter()
                .map(|p| p.file_name().unwrap())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(
            groups[1]
                .paths
                .iter()
                .map(|p| p.file_name().unwrap())
                .collect::<Vec<_>>(),
            ["b", "d"]
        );
        Ok(())
    }
}
