use crate::{Directory, parent};
use anyhow::{Result, bail, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// What extraction does when the destination already exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Existing {
    /// Refuse an existing destination. Output appears only when the whole
    /// archive succeeds.
    #[default]
    Refuse,
    /// Extract into an existing directory, or the directory a symlink names.
    /// A failed decode replaces nothing. File and directory conflicts, and
    /// folders on another filesystem, fail before any replacement. Then each
    /// archive file replaces the file of the same name by rename, so it is
    /// wholly old or wholly new. A rename that fails partway leaves the
    /// earlier files replaced. Existing directories keep their mode, and files
    /// the archive does not name are left alone.
    Overwrite,
}

pub(crate) struct Staging {
    temp: tempfile::TempDir,
    destination: PathBuf,
    merge: bool,
    zone: Option<Vec<u8>>,
}

pub(crate) fn stage(archive: &Path, destination: &Path, existing: Existing) -> Result<Staging> {
    let merge = match fs::metadata(destination) {
        Ok(metadata) if existing == Existing::Overwrite => {
            ensure!(
                metadata.is_dir(),
                "extraction destination is not a directory"
            );
            true
        }
        Ok(_) => bail!("extraction destination must not exist"),
        Err(_) => {
            ensure!(
                fs::symlink_metadata(destination).is_err(),
                "extraction destination is a broken symlink"
            );
            false
        }
    };
    let folder = if merge {
        destination
    } else {
        parent(destination)
    };
    let temp = tempfile::Builder::new()
        .prefix(".zipper-")
        .tempdir_in(folder)?;
    Ok(Staging {
        temp,
        destination: destination.to_owned(),
        merge,
        zone: crate::ntfs_streams::archive_mark(archive),
    })
}

impl Staging {
    pub(crate) fn path(&self) -> &Path {
        self.temp.path()
    }

    pub(crate) fn publish(&self, directories: &[Directory]) -> Result<()> {
        if let Some(zone) = &self.zone {
            crate::ntfs_streams::mark_extracted_files(self.temp.path(), zone)?;
        }
        if !self.merge {
            return crate::publish(self.temp.path(), &self.destination, directories);
        }
        let mut moves = Vec::new();
        plan(
            self.temp.path(),
            &self.destination,
            Path::new(""),
            &mut moves,
        )?;
        let moved = |name: &str| moves.iter().any(|path| Path::new(name).starts_with(path));
        let created: Vec<Directory> = directories
            .iter()
            .filter(|directory| moved(directory.name))
            .copied()
            .collect();
        #[cfg(unix)]
        let default = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(self.temp.path())?.permissions().mode() & 0o777
        };
        for path in &moves {
            #[cfg(windows)]
            allow_replacing(&self.destination.join(path))?;
            fs::rename(self.temp.path().join(path), self.destination.join(path))?;
        }
        #[cfg(unix)]
        crate::unix_modes::apply_with_default(&self.destination, &created, default)?;
        #[cfg(windows)]
        crate::restore_directory_times(&self.destination, &created);
        Ok(())
    }
}

#[cfg(windows)]
#[expect(
    clippy::permissions_set_readonly_false,
    reason = "Windows only, where it clears FILE_ATTRIBUTE_READONLY"
)]
fn allow_replacing(existing: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(existing)
        && metadata.is_file()
        && metadata.permissions().readonly()
    {
        let mut permissions = metadata.permissions();
        permissions.set_readonly(false);
        fs::set_permissions(existing, permissions)?;
    }
    Ok(())
}

fn plan(
    staging: &Path,
    destination: &Path,
    relative: &Path,
    moves: &mut Vec<PathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(staging.join(relative))? {
        let name = entry?.file_name();
        ensure!(
            Some(name.as_os_str()) != staging.file_name(),
            "archive entry has the staging folder's name"
        );
        let path = relative.join(name);
        let staged = fs::symlink_metadata(staging.join(&path))?;
        match fs::symlink_metadata(destination.join(&path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => moves.push(path),
            Err(error) => return Err(error.into()),
            Ok(existing) if staged.is_dir() && existing.is_dir() => {
                ensure!(
                    same_filesystem(&staged, &existing),
                    "{} is on another filesystem than the destination",
                    path.display()
                );
                plan(staging, destination, &path, moves)?
            }
            Ok(existing) if staged.is_dir() || existing.is_dir() => bail!(
                "{} is a {} in the destination and a {} in the archive",
                path.display(),
                kind(existing.is_dir()),
                kind(staged.is_dir())
            ),
            Ok(_) => moves.push(path),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn same_filesystem(staged: &fs::Metadata, existing: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    staged.dev() == existing.dev()
}

#[cfg(not(unix))]
fn same_filesystem(_: &fs::Metadata, _: &fs::Metadata) -> bool {
    true
}

fn kind(directory: bool) -> &'static str {
    if directory { "directory" } else { "file" }
}
