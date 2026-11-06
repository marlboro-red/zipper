use crate::{Directory, parent};
use anyhow::{Result, bail, ensure};
use std::{
    collections::HashSet,
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
    /// Extract into an existing directory, leaving out every archive file
    /// whose path already exists there, as 7-Zip's `-aos` does. An archive
    /// folder merges into an existing folder. Where something else is in the
    /// way, the folder and everything under it is left out.
    Skip,
    /// Extract into an existing directory, writing an archive file or folder
    /// whose path already exists under the first free name `name_1.ext`,
    /// `name_2.ext` and on, as 7-Zip's `-aou` does. Folders merge.
    RenameNew,
    /// Extract into an existing directory, first renaming whatever already
    /// holds an archive file's path to the first free name `name_1.ext`, as
    /// 7-Zip's `-aot` does. A link is renamed, never followed. Folders merge.
    RenameExisting,
}

enum Step {
    Create(PathBuf),
    Replace(PathBuf),
    CreateAs(PathBuf, PathBuf),
    SetAside(PathBuf, PathBuf),
}

impl Step {
    fn archive_source(&self) -> Option<&Path> {
        match self {
            Step::Create(path) | Step::Replace(path) | Step::CreateAs(path, _) => Some(path),
            Step::SetAside(..) => None,
        }
    }

    fn archive_target(&self) -> Option<&Path> {
        match self {
            Step::Create(path) | Step::Replace(path) => Some(path),
            Step::CreateAs(_, name) => Some(name),
            Step::SetAside(..) => None,
        }
    }
}

pub(crate) struct Staging {
    temp: tempfile::TempDir,
    destination: PathBuf,
    merge: bool,
    existing: Existing,
    zone: Option<Vec<u8>>,
}

pub(crate) fn stage(
    archive: &Path,
    destination: &Path,
    existing: Existing,
    metadata: crate::Metadata,
) -> Result<Staging> {
    let refused = |message: String| crate::DestinationRefused(message);
    let merge = match fs::metadata(destination) {
        Ok(metadata) if existing != Existing::Refuse => {
            ensure!(
                metadata.is_dir(),
                refused("extraction destination is not a directory".to_owned())
            );
            true
        }
        Ok(_) => bail!(refused("extraction destination must not exist".to_owned())),
        Err(_) => {
            ensure!(
                fs::symlink_metadata(destination).is_err(),
                refused("extraction destination is a broken symlink".to_owned())
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
        .tempdir_in(folder)
        .map_err(|error| {
            refused(format!(
                "cannot create a staging folder in {}: {error}",
                folder.display()
            ))
        })?;
    Ok(Staging {
        temp,
        destination: destination.to_owned(),
        merge,
        existing,
        zone: crate::ntfs_streams::archive_mark(archive).filter(|_| metadata.restores()),
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
        let mut steps = Vec::new();
        plan(
            self.temp.path(),
            &self.destination,
            self.existing,
            Path::new(""),
            &mut HashSet::new(),
            &mut steps,
        )?;
        ensure_links_stay_in_archive_paths(self.temp.path(), &self.destination, &steps)?;
        let created: Vec<Directory> = directories
            .iter()
            .filter(|directory| {
                steps.iter().any(|step| {
                    matches!(step, Step::Create(path) | Step::Replace(path) if Path::new(directory.name).starts_with(path))
                })
            })
            .copied()
            .collect();
        #[cfg(unix)]
        let default = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(self.temp.path())?.permissions().mode() & 0o777
        };
        let staged = |path: &Path| self.temp.path().join(path);
        let placed = |path: &Path| self.destination.join(path);
        for step in &steps {
            match step {
                Step::Create(path) => rename_without_replacing(&staged(path), &placed(path))?,
                Step::Replace(path) => {
                    #[cfg(windows)]
                    allow_replacing(&placed(path))?;
                    fs::rename(staged(path), placed(path))?;
                }
                Step::CreateAs(path, name) => {
                    rename_without_replacing(&staged(path), &placed(name))?
                }
                Step::SetAside(path, name) => {
                    rename_without_replacing(&placed(path), &placed(name))?
                }
            }
        }
        #[cfg(unix)]
        crate::unix_modes::apply_with_default(&self.destination, &created, default)?;
        #[cfg(windows)]
        crate::restore_directory_times(&self.destination, &created);
        Ok(())
    }
}

fn rename_without_replacing(from: &Path, to: &Path) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    let renamed = rustix::fs::renameat_with(
        rustix::fs::CWD,
        from,
        rustix::fs::CWD,
        to,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from);
    #[cfg(windows)]
    let renamed = {
        use std::os::windows::ffi::OsStrExt;
        let wide = |path: &Path| {
            path.as_os_str()
                .encode_wide()
                .chain([0])
                .collect::<Vec<u16>>()
        };
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both paths are NUL-terminated UTF-16 buffers that outlive the call.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
        } != 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        windows
    )))]
    let renamed = if fs::symlink_metadata(to).is_ok() {
        Err(std::io::ErrorKind::AlreadyExists.into())
    } else {
        fs::rename(from, to)
    };
    renamed.map_err(|error| {
        if fs::symlink_metadata(to).is_ok() {
            anyhow::anyhow!(
                "{} appeared in the destination during extraction",
                to.display()
            )
        } else {
            anyhow::Error::from(error).context(format!("cannot move {} into place", to.display()))
        }
    })
}

fn join_rest(base: &Path, rest: &Path) -> PathBuf {
    if rest.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

enum Owner {
    ArchiveLink(PathBuf),
    DestinationLink,
    Other,
}

fn owner_after_publish(
    staging: &Path,
    destination: &Path,
    steps: &[Step],
    path: &Path,
) -> Result<Owner> {
    let placed = steps.iter().find_map(|step| {
        let rest = path.strip_prefix(step.archive_target()?).ok()?;
        Some(staging.join(join_rest(step.archive_source()?, rest)))
    });
    let set_aside = || {
        steps.iter().find_map(|step| match step {
            Step::SetAside(original, name) => {
                Some(join_rest(original, path.strip_prefix(name).ok()?))
            }
            _ => None,
        })
    };
    let (on_disk, archive) = match placed {
        Some(staged) => (staged, true),
        None => (
            destination.join(set_aside().unwrap_or_else(|| path.to_path_buf())),
            false,
        ),
    };
    Ok(match fs::symlink_metadata(&on_disk) {
        Ok(metadata) if metadata.file_type().is_symlink() && archive => {
            Owner::ArchiveLink(fs::read_link(on_disk)?)
        }
        Ok(metadata) if metadata.file_type().is_symlink() => Owner::DestinationLink,
        _ => Owner::Other,
    })
}

const MOST_LINKS_FOLLOWED: usize = 40;

fn ensure_links_stay_in_archive_paths(
    staging: &Path,
    destination: &Path,
    steps: &[Step],
) -> Result<()> {
    for entry in walkdir::WalkDir::new(staging).min_depth(1) {
        let entry = entry?;
        if !entry.file_type().is_symlink() {
            continue;
        }
        let relative = entry.path().strip_prefix(staging)?;
        let Some(location) = steps.iter().find_map(|step| {
            let source = step.archive_source()?;
            let rest = relative.strip_prefix(source).ok()?;
            Some(join_rest(step.archive_target()?, rest))
        }) else {
            continue;
        };
        let mut resolved = location.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut remaining: Vec<std::ffi::OsString> = Vec::new();
        let push_target = |target: &Path, remaining: &mut Vec<std::ffi::OsString>| -> Result<()> {
            for component in target.components().rev() {
                match component {
                    std::path::Component::Normal(part) => remaining.push(part.to_owned()),
                    std::path::Component::ParentDir => remaining.push("..".into()),
                    std::path::Component::CurDir => {}
                    _ => bail!("archive link {} has an absolute target", relative.display()),
                }
            }
            Ok(())
        };
        push_target(&fs::read_link(entry.path())?, &mut remaining)?;
        let mut followed = 0;
        while let Some(part) = remaining.pop() {
            if part == ".." {
                resolved.pop();
                continue;
            }
            resolved.push(&part);
            match owner_after_publish(staging, destination, steps, &resolved)? {
                Owner::DestinationLink => bail!(
                    "archive link {} points through {}, a link the destination keeps",
                    relative.display(),
                    resolved.display()
                ),
                Owner::ArchiveLink(target) => {
                    followed += 1;
                    ensure!(
                        followed <= MOST_LINKS_FOLLOWED,
                        "archive link {} goes through more than {MOST_LINKS_FOLLOWED} links",
                        relative.display()
                    );
                    resolved.pop();
                    push_target(&target, &mut remaining)?;
                }
                Owner::Other => {}
            }
        }
    }
    Ok(())
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

const LONGEST_NAME: usize = 255;

pub(crate) fn numbered(name: &std::ffi::OsStr, number: u64) -> std::ffi::OsString {
    let path = Path::new(name);
    let mut numbered = path.file_stem().unwrap_or_default().to_owned();
    numbered.push(format!("_{number}"));
    if let Some(extension) = path.extension() {
        numbered.push(".");
        numbered.push(extension);
    }
    numbered
}

fn free_name(
    staging: &Path,
    destination: &Path,
    path: &Path,
    taken: &mut HashSet<PathBuf>,
) -> Result<PathBuf> {
    for number in 1.. {
        let name = numbered(path.file_name().unwrap_or_default(), number);
        #[cfg(windows)]
        let length = std::os::windows::ffi::OsStrExt::encode_wide(name.as_os_str()).count();
        #[cfg(not(windows))]
        let length = name.len();
        ensure!(
            length <= LONGEST_NAME,
            "cannot rename {}: {} is longer than a file name may be",
            path.display(),
            name.display()
        );
        let candidate = path.with_file_name(name);
        let free = |root: &Path| match fs::symlink_metadata(root.join(&candidate)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error),
            Ok(_) => Ok(false),
        };
        if !taken.contains(&candidate) && free(destination)? && free(staging)? {
            taken.insert(candidate.clone());
            return Ok(candidate);
        }
    }
    unreachable!("some number is free")
}

fn plan(
    staging: &Path,
    destination: &Path,
    existing_mode: Existing,
    relative: &Path,
    taken: &mut HashSet<PathBuf>,
    steps: &mut Vec<Step>,
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                steps.push(Step::Create(path))
            }
            Err(error) => return Err(error.into()),
            Ok(existing) if staged.is_dir() && existing.is_dir() => {
                ensure!(
                    same_filesystem(&staged, &existing),
                    "{} is on another filesystem than the destination",
                    path.display()
                );
                plan(staging, destination, existing_mode, &path, taken, steps)?
            }
            Ok(existing) => match existing_mode {
                Existing::Skip => {}
                Existing::RenameNew => {
                    let name = free_name(staging, destination, &path, taken)?;
                    steps.push(Step::CreateAs(path, name));
                }
                Existing::RenameExisting => {
                    let name = free_name(staging, destination, &path, taken)?;
                    steps.push(Step::SetAside(path.clone(), name));
                    steps.push(Step::Create(path));
                }
                _ if staged.is_dir() || existing.is_dir() => bail!(
                    "{} is a {} in the destination and a {} in the archive",
                    path.display(),
                    kind(&existing),
                    kind(&staged)
                ),
                _ => steps.push(Step::Replace(path)),
            },
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

fn kind(metadata: &fs::Metadata) -> &'static str {
    if metadata.file_type().is_symlink() {
        "link"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_names_are_new_distinct_and_keep_their_extension_on_every_short_name() {
        let mut names = vec![String::new()];
        for _ in 0..5 {
            names = names
                .iter()
                .flat_map(|name| ['a', '.', '_', '1'].map(|character| format!("{name}{character}")))
                .collect();
            for name in names.iter().filter(|name| {
                Path::new(name.as_str()).file_name() == Some(std::ffi::OsStr::new(name.as_str()))
            }) {
                let original = Path::new(name.as_str());
                let mut seen = HashSet::new();
                for number in 1..=12 {
                    let renamed = numbered(original.as_os_str(), number);
                    let renamed_path = Path::new(&renamed);
                    assert_ne!(
                        renamed.as_os_str(),
                        original.as_os_str(),
                        "{name:?} {number}"
                    );
                    assert!(seen.insert(renamed.clone()), "{name:?} repeats at {number}");
                    assert_eq!(renamed_path.extension(), original.extension(), "{name:?}");
                    assert_eq!(
                        renamed_path.file_name(),
                        Some(renamed.as_os_str()),
                        "{name:?}"
                    );
                    assert_eq!(
                        renamed.len(),
                        name.len() + format!("_{number}").len(),
                        "{name:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_rename_into_place_never_replaces_what_appeared_there() {
        let root = tempfile::tempdir().unwrap();
        let (from, to) = (root.path().join("from"), root.path().join("to"));
        fs::write(&from, b"archive").unwrap();
        fs::write(&to, b"written by another program").unwrap();
        let error = rename_without_replacing(&from, &to).unwrap_err();
        assert!(format!("{error:#}").contains("appeared in the destination"));
        assert_eq!(fs::read(&to).unwrap(), b"written by another program");
        assert_eq!(fs::read(&from).unwrap(), b"archive");
        fs::remove_file(&to).unwrap();
        rename_without_replacing(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"archive");
    }
}
