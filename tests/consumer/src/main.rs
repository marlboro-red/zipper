//! A program that depends on Zipper with only the `zip` and `7z` features,
//! or with only `zip-deflate` when built without its default features.
//! `cargo xtask gate` runs each build on corpus archives and their manifests,
//! so a subset build must compile for an outside crate and extract each
//! archive to the tree its manifest records.

use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
enum Kind {
    Folder,
    File { size: u64, crc: u32 },
    Link(String),
}

fn manifest(path: &Path, archive: &str) -> Result<BTreeMap<String, Kind>> {
    let text = fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let mut tree = BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [name, entry, kind, value, ..] = fields[..] else {
            bail!("bad manifest line {line:?}");
        };
        if name != archive {
            continue;
        }
        let kind = match kind {
            "dir" => Kind::Folder,
            "link" => Kind::Link(value.to_owned()),
            size => Kind::File {
                size: size.parse()?,
                crc: u32::from_str_radix(value, 16)?,
            },
        };
        tree.insert(entry.to_owned(), kind);
    }
    ensure!(!tree.is_empty(), "{} lists no {archive}", path.display());
    Ok(tree)
}

fn walk(root: &Path, folder: &Path, tree: &mut BTreeMap<String, Kind>) -> Result<()> {
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        let name = path
            .strip_prefix(root)?
            .iter()
            .map(|part| part.to_str().context("name is not UTF-8"))
            .collect::<Result<Vec<_>>>()?
            .join("/");
        let metadata = fs::symlink_metadata(&path)?;
        let kind = if metadata.is_symlink() {
            Kind::Link(fs::read_link(&path)?.to_string_lossy().replace('\\', "/"))
        } else if metadata.is_dir() {
            walk(root, &path, tree)?;
            Kind::Folder
        } else {
            let bytes = fs::read(&path)?;
            Kind::File {
                size: bytes.len() as u64,
                crc: crc32fast::hash(&bytes),
            }
        };
        tree.insert(name, kind);
    }
    Ok(())
}

fn check(archive: &Path, manifest_path: &Path, destination: &Path) -> Result<()> {
    let name = archive
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("archive has no name")?;
    let wanted = manifest(manifest_path, name)?;
    let into = destination.join(name);
    let in_process = zipper::ExtractOptions::default();
    zipper::extract_archive(archive, &into, in_process)
        .with_context(|| archive.display().to_string())?;
    let mut found = BTreeMap::new();
    walk(&into, &into, &mut found)?;
    ensure!(
        found == wanted,
        "{} extracted to {found:#?}, not the manifest's {wanted:#?}",
        archive.display()
    );
    println!("{}: {} entries match", archive.display(), found.len());
    Ok(())
}

fn main() -> Result<()> {
    let arguments: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let Some((destination, pairs)) = arguments.split_first() else {
        bail!("usage: zipper-consumer DESTINATION (ARCHIVE MANIFEST)...");
    };
    ensure!(
        !pairs.is_empty() && pairs.len() % 2 == 0,
        "pass each archive with its manifest"
    );
    for pair in pairs.chunks(2) {
        check(&pair[0], &pair[1], destination)?;
    }
    Ok(())
}
