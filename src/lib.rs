pub mod compat;
mod pipeline;
mod v1;
mod v2;
use anyhow::{Result, bail, ensure};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub directory: bool,
}

pub const MAGIC: &[u8; 8] = b"ZIPPER\0\x02";

/// Native creation settings. Larger blocks can improve compression at the cost
/// of memory and selective-extraction work. Defaults preserve the v2 layout.
#[derive(Clone, Copy, Debug)]
pub struct CreateOptions {
    pub level: i32,
    /// Power of two from 64 KiB through 16 MiB.
    pub block_size: usize,
}
impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            level: 1,
            block_size: 1024 * 1024,
        }
    }
}

pub fn create(output: &Path, inputs: &[PathBuf], level: i32) -> Result<()> {
    create_with_options(
        output,
        inputs,
        CreateOptions {
            level,
            ..CreateOptions::default()
        },
    )
}
pub fn create_with_options(
    output: &Path,
    inputs: &[PathBuf],
    options: CreateOptions,
) -> Result<()> {
    v2::create(output, inputs, options)
}
fn version(path: &Path) -> Result<u8> {
    let magic = number::<8>(&mut File::open(path)?)?;
    ensure!(&magic[..7] == b"ZIPPER\0", "not a Zipper archive");
    Ok(magic[7])
}
/// Validate all payloads, optionally extracting into a new directory.
pub fn read_archive(archive: &Path, destination: Option<&Path>) -> Result<Vec<Entry>> {
    match version(archive)? {
        1 => v1::read_archive(archive, destination),
        2 => v2::read_archive(archive, destination, &[]),
        n => bail!("unsupported Zipper version: {n}"),
    }
}
/// v2 listing checks the index only. Use read_archive to validate payloads.
pub fn list_archive(archive: &Path) -> Result<Vec<Entry>> {
    match version(archive)? {
        1 => v1::read_archive(archive, None),
        2 => v2::list(archive),
        n => bail!("unsupported Zipper version: {n}"),
    }
}
/// Extract selected exact paths or directory subtrees from an indexed v2 archive.
pub fn extract_selected(
    archive: &Path,
    destination: &Path,
    paths: &[String],
) -> Result<Vec<Entry>> {
    ensure!(
        version(archive)? == 2,
        "selective extraction requires a v2 archive"
    );
    v2::read_archive(archive, Some(destination), paths)
}
