use crate::{Directory, parent};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// What extraction does when the destination already exists.
///
/// Every mode but `Refuse` refuses, before decoding, a destination on FAT or
/// exFAT on macOS, or on Linux or macOS a filesystem whose rename cannot
/// refuse to replace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Existing {
    /// Refuse an existing destination. Output appears only when the whole
    /// archive succeeds.
    #[default]
    Refuse,
    /// Extract into an existing directory, or the directory a symlink names.
    /// A failed decode replaces nothing. File and directory conflicts, and
    /// folders on another filesystem, fail before any replacement. Then each
    /// archive file replaces the file of the same name by rename. A rename
    /// that fails, or a new folder's mode that cannot be set, undoes the
    /// renames already made in reverse order, so the destination keeps every
    /// file, link, folder and mode it had, and the error names the path that
    /// failed. A cancel seen during these renames undoes them the same way, as
    /// [`crate::Control::cancel`] describes. Another program that changes the
    /// destination or the staging folder during extraction can defeat the
    /// undo. If an undo fails, the error names each path left changed and the
    /// staging folder, which is kept with everything not moved back. Existing
    /// directories keep their mode, and files the archive does not name are
    /// left alone.
    ///
    /// On Linux and macOS, several `Overwrite` extractions may run at once
    /// into one destination when their archives hold only regular files and
    /// folders. A folder another extraction creates first is merged into. An
    /// undo moves back only this extraction's files and removes only the
    /// folders it created that are then empty. Where two archives share a
    /// file path, the path ends with one archive's whole file, or the
    /// extraction that finds it newly created fails and undoes its renames.
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

#[derive(Clone, Debug)]
enum Step {
    Create(PathBuf),
    Replace(PathBuf),
    ReplaceFolder(PathBuf, Vec<(PathBuf, u32)>),
    CreateAs(PathBuf, PathBuf),
    SetAside(PathBuf, PathBuf),
}

impl Step {
    fn archive_source(&self) -> Option<&Path> {
        match self {
            Step::Create(path)
            | Step::Replace(path)
            | Step::ReplaceFolder(path, _)
            | Step::CreateAs(path, _) => Some(path),
            Step::SetAside(..) => None,
        }
    }

    fn archive_target(&self) -> Option<&Path> {
        match self {
            Step::Create(path) | Step::Replace(path) | Step::ReplaceFolder(path, _) => Some(path),
            Step::CreateAs(_, name) => Some(name),
            Step::SetAside(..) => None,
        }
    }
}

type Identity = Option<(u64, u64)>;

enum Done {
    Moved {
        from: PathBuf,
        to: PathBuf,
        moved: Identity,
    },
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "android", target_vendor = "apple")),
        allow(dead_code)
    )]
    Swapped {
        staged: PathBuf,
        placed: PathBuf,
        new: Identity,
    },
    #[cfg(unix)]
    Folder {
        from: PathBuf,
        to: PathBuf,
        moved: Identity,
        ours: HashSet<Entry>,
    },
    #[cfg(unix)]
    FolderReplaced {
        staged: PathBuf,
        placed: PathBuf,
        new: Identity,
        ours: Contents,
    },
    MadeAgain(PathBuf),
}

#[cfg(unix)]
type Entry = (u64, u64);

#[cfg(unix)]
fn entry_key(root: &Path, entry: &walkdir::DirEntry) -> Entry {
    use walkdir::DirEntryExt;
    key(
        entry.ino(),
        entry.path().strip_prefix(root).unwrap_or(entry.path()),
    )
}

#[cfg(unix)]
fn key(ino: u64, relative: &Path) -> Entry {
    (ino, path_hash(relative))
}

#[cfg(unix)]
fn path_hash(relative: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    relative.hash(&mut hasher);
    hasher.finish()
}

#[cfg(unix)]
struct Held {
    inode: u64,
    directory: bool,
}

#[cfg(unix)]
type Contents = std::collections::HashMap<u64, Held>;

#[cfg(unix)]
fn contents(folder: &Path) -> Result<Contents> {
    use walkdir::DirEntryExt;
    walkdir::WalkDir::new(folder)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry?;
            let held = Held {
                inode: entry.ino(),
                directory: entry.file_type().is_dir(),
            };
            Ok((path_hash(entry.path().strip_prefix(folder)?), held))
        })
        .collect()
}

#[cfg(unix)]
fn ensure_only_replaced_entries(old: &Path, placed: &Path, new: &Contents) -> Result<()> {
    ensure!(
        fs::symlink_metadata(old).is_ok_and(|metadata| metadata.is_dir()),
        "{} stopped being a directory during extraction",
        placed.display()
    );
    for entry in walkdir::WalkDir::new(old).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(old)?;
        match new.get(&path_hash(relative)) {
            Some(held) if held.directory == entry.file_type().is_dir() => {}
            Some(_) => bail!(
                "{} changed between a file and a directory during extraction",
                placed.join(relative).display()
            ),
            None => bail!(
                "{} appeared in the destination during extraction",
                placed.join(relative).display()
            ),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_only_our_entries(new: &Path, placed: &Path, ours: &Contents) -> Result<()> {
    use walkdir::DirEntryExt;
    for entry in walkdir::WalkDir::new(new).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(new)?;
        ensure!(
            ours.get(&path_hash(relative))
                .is_some_and(|held| held.inode == entry.ino()),
            "another program put {} there",
            placed.join(relative).display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn files_left_in_staging(done: &[Done]) -> usize {
    done.iter()
        .map(|step| match step {
            Done::Swapped { .. } => 1,
            Done::FolderReplaced { ours, .. } => ours.len(),
            _ => 0,
        })
        .sum()
}

#[cfg(unix)]
const FEWEST_FILES_REMOVED_ON_THREADS: usize = 1024;

// RECORD(benchmarks/overwrite/full-folder/README.md): four threads remove the old files of
// 280 folders in half the time of one, and eight are slower than four.
#[cfg(unix)]
const REMOVING_THREADS: usize = 4;

#[cfg(unix)]
fn remove_files_on_threads(root: &Path) {
    let folders: Vec<PathBuf> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_dir())
        .map(walkdir::DirEntry::into_path)
        .collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(REMOVING_THREADS)
        .min(folders.len());
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(folder) = folders.get(next.fetch_add(1, Ordering::Relaxed)) {
                    remove_files_in(folder);
                }
            });
        }
    });
}

#[cfg(unix)]
fn remove_files_in(folder: &Path) {
    let (Ok(handle), Ok(entries)) = (fs::File::open(folder), fs::read_dir(folder)) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| !kind.is_dir()) {
            let _ = rustix::fs::unlinkat(&handle, entry.file_name(), rustix::fs::AtFlags::empty());
        }
    }
}

#[cfg(unix)]
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(1);

struct DestinationLock {
    #[cfg(unix)]
    folder: Option<fs::File>,
    alone: bool,
}

impl DestinationLock {
    #[cfg(unix)]
    fn take(
        destination: &Path,
        existing: Existing,
        check: &mut impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        use rustix::fs::{FlockOperation, flock};
        let Ok(folder) = fs::File::open(destination) else {
            return Ok(Self {
                folder: None,
                alone: false,
            });
        };
        let alone = existing == Existing::Overwrite
            && flock(&folder, FlockOperation::NonBlockingLockExclusive).is_ok();
        let mut lock = Self {
            folder: Some(folder),
            alone,
        };
        if !alone {
            lock.share(check)?;
        }
        Ok(lock)
    }

    #[cfg(not(unix))]
    fn take(_: &Path, _: Existing, _: &mut impl FnMut() -> Result<()>) -> Result<Self> {
        Ok(Self { alone: false })
    }

    #[cfg(unix)]
    fn share(&mut self, check: &mut impl FnMut() -> Result<()>) -> Result<()> {
        use rustix::fs::{FlockOperation, flock};
        self.alone = false;
        while let Some(folder) = &self.folder {
            match flock(folder, FlockOperation::NonBlockingLockShared) {
                Ok(()) => break,
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    check()?;
                    std::thread::sleep(LOCK_POLL);
                }
                Err(_) => self.folder = None,
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn share(&mut self, _: &mut impl FnMut() -> Result<()>) -> Result<()> {
        Ok(())
    }

    fn release(self) {}
}

const MOST_TIMES_A_PARENT_IS_MADE_AGAIN: usize = 64;

enum AfterFailedCreate {
    Merge(Vec<Step>),
    TryAgain,
    Fail,
}

enum MadeAgain {
    Made,
    Existed,
    ParentVanished,
}

fn make_folder_again(placed: &Path) -> std::io::Result<MadeAgain> {
    match fs::create_dir(placed) {
        Ok(()) => Ok(MadeAgain::Made),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(MadeAgain::Existed),
        Err(error) if parent_vanished(&error) => Ok(MadeAgain::ParentVanished),
        Err(error) => Err(error),
    }
}

fn parent_vanished(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    let invalid = rustix::io::Errno::from_io_error(error) == Some(rustix::io::Errno::INVAL);
    #[cfg(not(unix))]
    let invalid = false;
    invalid || error.kind() == std::io::ErrorKind::NotFound
}

pub(crate) struct Staging {
    temp: std::sync::Mutex<Option<tempfile::TempDir>>,
    root: PathBuf,
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
    if merge {
        Staging::ensure_exact_merge(destination, temp.path())?;
    }
    Ok(Staging {
        root: temp.path().to_owned(),
        temp: std::sync::Mutex::new(Some(temp)),
        destination: destination.to_owned(),
        merge,
        existing,
        zone: crate::ntfs_streams::archive_mark(archive).filter(|_| metadata.restores()),
    })
}

impl Staging {
    fn ensure_exact_merge(destination: &Path, staging: &Path) -> Result<()> {
        let refused = |filesystem: &str| {
            crate::DestinationRefused(format!(
                "{} is on {filesystem}, where a merge cannot be made exact; \
                 extract into a new folder instead",
                destination.display()
            ))
        };
        #[cfg(target_vendor = "apple")]
        {
            let stats = rustix::fs::statfs(destination).with_context(|| {
                format!("cannot read the filesystem of {}", destination.display())
            })?;
            let name: String = stats
                .f_fstypename
                .iter()
                .take_while(|&&character| character != 0)
                .map(|&character| char::from(character.to_ne_bytes()[0]))
                .collect();
            if ["msdos", "exfat"].contains(&name.as_str()) {
                bail!(refused(&name));
            }
        }
        #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
        {
            let (first, second) = (staging.join("first"), staging.join("second"));
            fs::write(&first, b"").with_context(|| format!("cannot write {}", first.display()))?;
            let renamed = rustix::fs::renameat_with(
                rustix::fs::CWD,
                &first,
                rustix::fs::CWD,
                &second,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(std::io::Error::from);
            let _ = fs::remove_file(&first);
            let _ = fs::remove_file(&second);
            match renamed {
                Err(error) if unsupported(&error) => {
                    bail!(refused("a filesystem that cannot rename without replacing"))
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("cannot rename in {}", staging.display()));
                }
                Ok(()) => {}
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
        let _ = (destination, staging, refused);
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }

    pub(crate) fn publish(
        &self,
        directories: &[Directory],
        cancel: Option<&AtomicBool>,
    ) -> Result<()> {
        self.publish_checking(directories, || match cancel {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(crate::Cancelled.into()),
            _ => Ok(()),
        })
    }

    fn publish_checking(
        &self,
        directories: &[Directory],
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        if let Some(zone) = &self.zone {
            crate::ntfs_streams::mark_extracted_files(&self.root, zone)?;
        }
        if !self.merge {
            let published = crate::publish(&self.root, &self.destination, directories, &mut check);
            let cancelled = published
                .as_ref()
                .is_err_and(|error| error.downcast_ref::<crate::Cancelled>().is_some());
            let appeared_folder = self.existing != Existing::Refuse
                && fs::metadata(&self.destination).is_ok_and(|metadata| metadata.is_dir());
            if published.is_ok() || cancelled || !appeared_folder {
                return published;
            }
            Self::ensure_exact_merge(&self.destination, &self.root)?;
        }
        let mut lock = DestinationLock::take(&self.destination, self.existing, &mut check)?;
        let mut steps = Vec::new();
        plan_folder(
            &self.root,
            &self.destination,
            self.existing,
            Path::new(""),
            &mut HashSet::new(),
            &mut steps,
            lock.alone,
        )?;
        if !steps
            .iter()
            .any(|step| matches!(step, Step::ReplaceFolder(..)))
        {
            lock.share(&mut check)?;
        }
        ensure_links_stay_in_archive_paths(&self.root, &self.destination, &steps)?;
        #[cfg(unix)]
        let default = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&self.root)?.permissions().mode() & 0o777
        };
        let mut done = Vec::new();
        let (steps, mut published) = match self.carry_out(steps, &mut done, &mut check, |_| {}) {
            Ok(steps) => (steps, Ok(())),
            Err(error) => (Vec::new(), Err(error)),
        };
        let created = created_directories(directories, &steps);
        #[cfg(unix)]
        if published.is_ok() {
            published = crate::unix_modes::apply_with_default(&self.destination, &created, default)
                .and_then(|()| check())
                .inspect_err(|_| self.unlock_created(&done));
        }
        #[cfg(not(unix))]
        if published.is_ok() {
            published = check();
        }
        if let Err(error) = published {
            return Err(self.undo(&done, error));
        }
        #[cfg(windows)]
        crate::restore_directory_times(&self.destination, &created);
        lock.release();
        #[cfg(unix)]
        if files_left_in_staging(&done) >= FEWEST_FILES_REMOVED_ON_THREADS {
            remove_files_on_threads(&self.root);
        }
        Ok(())
    }

    fn carry_out(
        &self,
        mut steps: Vec<Step>,
        done: &mut Vec<Done>,
        check: &mut impl FnMut() -> Result<()>,
        mut after_each: impl FnMut(&Step),
    ) -> Result<Vec<Step>> {
        let staged = |path: &Path| self.root.join(path);
        let placed = |path: &Path| self.destination.join(path);
        let mut aside = None;
        let mut spellings = Spellings::new();
        let mut index = 0;
        let mut parents_made_again = Vec::new();
        let mut creates_tried = 0;
        while let Some(step) = steps.get(index) {
            check()?;
            match step {
                Step::Create(path) => {
                    if let Err(error) = Self::create(staged(path), placed(path), done) {
                        if self.make_vanished_parents_again(path, done, &mut parents_made_again)? {
                            continue;
                        }
                        match self.after_failed_create(path)? {
                            AfterFailedCreate::Merge(merged) => {
                                steps.splice(index..=index, merged);
                                continue;
                            }
                            AfterFailedCreate::TryAgain
                                if creates_tried < MOST_TIMES_A_PARENT_IS_MADE_AGAIN =>
                            {
                                creates_tried += 1;
                                continue;
                            }
                            _ => return Err(error),
                        }
                    }
                }
                Step::Replace(path) => {
                    let (aside, swaps) = self.aside(&mut aside, index)?;
                    let placed = placed(path);
                    if swaps {
                        Self::replace_by_swap(staged(path), placed, done)?
                    } else {
                        let old = spelled_on_disk(&placed, &mut spellings);
                        Self::replace_in_two_moves(staged(path), placed, old, aside, done)?
                    }
                }
                Step::ReplaceFolder(path, kept) => {
                    let (_, swaps) = self.aside(&mut aside, index)?;
                    if !swaps || !self.replace_folder(path, kept, done)? {
                        let merged = self.plan_merge_of_each_entry(path)?;
                        steps.splice(index..=index, merged);
                        continue;
                    }
                }
                Step::CreateAs(path, name) => {
                    Self::move_into_place(staged(path), placed(name), done)?
                }
                Step::SetAside(path, name) => {
                    let old = spelled_on_disk(&placed(path), &mut spellings);
                    Self::move_into_place(old, placed(name), done)?
                }
            }
            after_each(step);
            index += 1;
            parents_made_again.clear();
            creates_tried = 0;
        }
        Ok(steps)
    }

    fn make_vanished_parents_again(
        &self,
        path: &Path,
        done: &mut Vec<Done>,
        times_made: &mut Vec<usize>,
    ) -> Result<bool> {
        let mut parents: Vec<&Path> = path
            .ancestors()
            .skip(1)
            .filter(|parent| !parent.as_os_str().is_empty())
            .collect();
        parents.reverse();
        times_made.resize(parents.len(), 0);
        let mut made = false;
        let mut depth = 0;
        while let Some(parent) = parents.get(depth) {
            let placed = self.destination.join(parent);
            match fs::symlink_metadata(&placed) {
                Ok(metadata) if metadata.is_dir() => {
                    depth += 1;
                    continue;
                }
                Ok(_) => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("cannot read {}", placed.display()));
                }
            }
            if times_made[depth] == MOST_TIMES_A_PARENT_IS_MADE_AGAIN {
                return Ok(false);
            }
            times_made[depth] += 1;
            made = true;
            match make_folder_again(&placed)
                .with_context(|| format!("cannot make {}", placed.display()))?
            {
                MadeAgain::Made => done.push(Done::MadeAgain(placed)),
                MadeAgain::Existed => {}
                MadeAgain::ParentVanished => depth = 0,
            }
        }
        Ok(made)
    }

    fn after_failed_create(&self, path: &Path) -> Result<AfterFailedCreate> {
        let placed = match fs::symlink_metadata(self.destination.join(path)) {
            Ok(placed) => placed,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && fs::symlink_metadata(self.root.join(path)).is_ok() =>
            {
                return Ok(AfterFailedCreate::TryAgain);
            }
            Err(_) => return Ok(AfterFailedCreate::Fail),
        };
        Ok(match self.plan_merge_into_appeared_folder(path, &placed) {
            Some(merged) => AfterFailedCreate::Merge(merged?),
            None => AfterFailedCreate::Fail,
        })
    }

    fn plan_merge_into_appeared_folder(
        &self,
        path: &Path,
        placed: &fs::Metadata,
    ) -> Option<Result<Vec<Step>>> {
        let staged = fs::symlink_metadata(self.root.join(path)).ok()?;
        let links = walkdir::WalkDir::new(self.root.join(path))
            .into_iter()
            .any(|entry| entry.is_ok_and(|entry| entry.path_is_symlink()));
        if !staged.is_dir() || !placed.is_dir() || !same_filesystem(&staged, placed) || links {
            return None;
        }
        let mut steps = Vec::new();
        let planned = plan(
            &self.root,
            &self.destination,
            self.existing,
            path,
            &mut HashSet::new(),
            &mut steps,
        );
        Some(planned.map(|()| steps))
    }

    fn plan_merge_of_each_entry(&self, path: &Path) -> Result<Vec<Step>> {
        let placed = self.destination.join(path);
        let mut steps = Vec::new();
        match fs::symlink_metadata(&placed) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                steps.push(Step::Create(path.to_owned()))
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot read {}", placed.display()));
            }
            Ok(existing) if existing.is_dir() => plan(
                &self.root,
                &self.destination,
                self.existing,
                path,
                &mut HashSet::new(),
                &mut steps,
            )?,
            Ok(existing) => bail!(
                "{} is a {} in the destination and a directory in the archive",
                path.display(),
                kind(&existing)
            ),
        }
        Ok(steps)
    }

    #[cfg(unix)]
    fn replace_folder(
        &self,
        path: &Path,
        kept: &[(PathBuf, u32)],
        done: &mut Vec<Done>,
    ) -> Result<bool> {
        use std::os::unix::fs::PermissionsExt;
        let (staged, placed) = (self.root.join(path), self.destination.join(path));
        let ours = contents(&staged)?;
        for (folder, mode) in kept {
            let folder = self.root.join(folder);
            fs::set_permissions(&folder, fs::Permissions::from_mode(*mode))
                .with_context(|| format!("cannot set the mode of {}", folder.display()))?;
        }
        if swap(&staged, &placed).is_err() {
            return Ok(false);
        }
        done.push(Done::FolderReplaced {
            new: identity(&placed),
            staged: staged.clone(),
            placed: placed.clone(),
            ours,
        });
        let Some(Done::FolderReplaced { ours, .. }) = done.last() else {
            unreachable!("the replaced folder was just recorded")
        };
        ensure_only_replaced_entries(&staged, &placed, ours)?;
        Ok(true)
    }

    #[cfg(not(unix))]
    fn replace_folder(&self, _: &Path, _: &[(PathBuf, u32)], _: &mut Vec<Done>) -> Result<bool> {
        Ok(false)
    }

    #[cfg(unix)]
    fn create(staged: PathBuf, placed: PathBuf, done: &mut Vec<Done>) -> Result<()> {
        if !fs::symlink_metadata(&staged).is_ok_and(|metadata| metadata.is_dir()) {
            return Self::move_into_place(staged, placed, done);
        }
        let ours = walkdir::WalkDir::new(&staged)
            .min_depth(1)
            .into_iter()
            .map(|entry| entry.map(|entry| entry_key(&staged, &entry)))
            .collect::<Result<HashSet<Entry>, _>>()?;
        rename_without_replacing(&staged, &placed)?;
        let moved = identity(&placed);
        done.push(Done::Folder {
            from: staged,
            to: placed,
            moved,
            ours,
        });
        Ok(())
    }

    #[cfg(not(unix))]
    fn create(staged: PathBuf, placed: PathBuf, done: &mut Vec<Done>) -> Result<()> {
        Self::move_into_place(staged, placed, done)
    }

    fn move_into_place(from: PathBuf, to: PathBuf, done: &mut Vec<Done>) -> Result<()> {
        rename_without_replacing(&from, &to)?;
        let moved = identity(&to);
        done.push(Done::Moved { from, to, moved });
        Ok(())
    }

    fn replace_by_swap(staged: PathBuf, placed: PathBuf, done: &mut Vec<Done>) -> Result<()> {
        match swap(&staged, &placed) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::move_into_place(staged, placed, done);
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot replace {}", placed.display()));
            }
        }
        ensure!(
            fs::symlink_metadata(&staged).is_ok(),
            "{} was replaced but not swapped, so its old file is lost",
            placed.display()
        );
        done.push(Done::Swapped {
            staged: staged.clone(),
            new: identity(&placed),
            placed: placed.clone(),
        });
        Self::ensure_replaced_no_folder(&staged, &placed)
    }

    fn replace_in_two_moves(
        staged: PathBuf,
        placed: PathBuf,
        old: PathBuf,
        aside: PathBuf,
        done: &mut Vec<Done>,
    ) -> Result<()> {
        Self::move_into_place(old, aside.clone(), done).map_err(|error| {
            anyhow::anyhow!(
                "cannot move {} aside to replace it: {}",
                placed.display(),
                error.root_cause()
            )
        })?;
        Self::ensure_replaced_no_folder(&aside, &placed)?;
        Self::move_into_place(staged, placed, done)
    }

    fn ensure_replaced_no_folder(old: &Path, placed: &Path) -> Result<()> {
        let old = fs::symlink_metadata(old)
            .with_context(|| format!("cannot replace {}", placed.display()))?;
        ensure!(
            !old.is_dir(),
            "{} became a directory during extraction",
            placed.display()
        );
        Ok(())
    }

    fn aside(&self, folder: &mut Option<(PathBuf, bool)>, index: usize) -> Result<(PathBuf, bool)> {
        let (folder, swaps) = match folder {
            Some(folder) => folder,
            None => {
                let created = tempfile::Builder::new()
                    .prefix(".zipper-")
                    .tempdir_in(&self.root)
                    .with_context(|| format!("cannot make a folder in {}", self.root.display()))?
                    .keep();
                let swaps = swap_works(&created)?;
                folder.insert((created, swaps))
            }
        };
        Ok((folder.join(index.to_string()), *swaps))
    }

    #[cfg(unix)]
    fn unlock_created(&self, done: &[Done]) {
        for step in done {
            if let Done::Folder { to, ours, .. } = step {
                unlock_only_ours(to, to, ours);
            }
        }
    }

    fn undo(&self, done: &[Done], error: anyhow::Error) -> anyhow::Error {
        let left: Vec<String> = done
            .iter()
            .rev()
            .filter_map(|step| {
                let undone = match step {
                    Done::Moved { from, to, moved } => {
                        ensure_unchanged(to, *moved).and_then(|()| {
                            rename_without_replacing(to, from).map_err(|error| {
                                anyhow::anyhow!(
                                    "cannot move {} back to {}: {}",
                                    to.display(),
                                    from.display(),
                                    error.root_cause()
                                )
                            })
                        })
                    }
                    Done::Swapped {
                        staged,
                        placed,
                        new,
                    } => ensure_unchanged(placed, *new).and_then(|()| {
                        swap(staged, placed)
                            .with_context(|| format!("cannot swap {} back", placed.display()))
                    }),
                    #[cfg(unix)]
                    Done::FolderReplaced {
                        staged,
                        placed,
                        new,
                        ours,
                    } => ensure_unchanged(placed, *new).and_then(|()| {
                        swap(staged, placed)
                            .with_context(|| format!("cannot swap {} back", placed.display()))?;
                        crate::unix_modes::unlock_tree_for_cleanup(staged);
                        ensure_only_our_entries(staged, placed, ours)
                    }),
                    Done::MadeAgain(folder) => match fs::remove_dir(folder) {
                        Err(error)
                            if [
                                std::io::ErrorKind::DirectoryNotEmpty,
                                std::io::ErrorKind::NotFound,
                            ]
                            .contains(&error.kind()) =>
                        {
                            Ok(())
                        }
                        removed => {
                            removed.with_context(|| format!("cannot remove {}", folder.display()))
                        }
                    },
                    #[cfg(unix)]
                    Done::Folder {
                        from,
                        to,
                        moved,
                        ours,
                    } => ensure_unchanged(to, *moved).and_then(|()| {
                        move_back_only_ours(from, to, ours).map_err(|error| {
                            anyhow::anyhow!(
                                "cannot move {} back to {}: {}",
                                to.display(),
                                from.display(),
                                error.root_cause()
                            )
                        })
                    }),
                };

                let describe = || match step {
                    Done::Moved { from, to, .. } => {
                        format!("{} moved from {}", to.display(), from.display())
                    }
                    #[cfg(unix)]
                    Done::Folder { from, to, .. } => {
                        format!("{} moved from {}", to.display(), from.display())
                    }
                    Done::MadeAgain(folder) => format!("{} made again", folder.display()),
                    Done::Swapped { staged, placed, .. } => {
                        format!("{} swapped with {}", placed.display(), staged.display())
                    }
                    #[cfg(unix)]
                    Done::FolderReplaced { staged, placed, .. } => {
                        format!("{} swapped with {}", placed.display(), staged.display())
                    }
                };
                undone
                    .err()
                    .map(|undo| format!("{} ({undo:#})", describe()))
            })
            .collect();
        if left.is_empty() {
            return error;
        }
        if let Some(temp) = self.temp.lock().ok().and_then(|mut temp| temp.take()) {
            let _ = temp.keep();
        }
        error.context(format!(
            "extraction failed and could not undo {}; {} is kept with everything not moved back",
            left.join(", "),
            self.root.display()
        ))
    }
}

fn created_directories<'a>(directories: &[Directory<'a>], steps: &[Step]) -> Vec<Directory<'a>> {
    let moved: HashSet<&Path> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Create(path) | Step::Replace(path) | Step::ReplaceFolder(path, _) => {
                Some(path.as_path())
            }
            _ => None,
        })
        .collect();
    let kept: HashSet<&Path> = steps
        .iter()
        .filter_map(|step| match step {
            Step::ReplaceFolder(_, kept) => Some(kept),
            _ => None,
        })
        .flatten()
        .map(|(folder, _)| folder.as_path())
        .collect();
    directories
        .iter()
        .filter(|directory| {
            let name = Path::new(directory.name);
            name.ancestors().any(|ancestor| moved.contains(ancestor)) && !kept.contains(name)
        })
        .copied()
        .collect()
}

#[cfg(unix)]
fn unlock_only_ours(root: &Path, folder: &Path, ours: &HashSet<Entry>) {
    use std::os::unix::fs::{DirEntryExt, PermissionsExt};
    if let Ok(metadata) = fs::symlink_metadata(folder) {
        let mode = metadata.permissions().mode() | 0o700;
        let _ = fs::set_permissions(folder, fs::Permissions::from_mode(mode));
    }
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            && ours.contains(&key(entry.ino(), relative))
        {
            unlock_only_ours(root, &path, ours);
        }
    }
}

#[cfg(unix)]
fn only_ours<'a>(
    to: &'a Path,
    ours: &'a HashSet<Entry>,
    contents_first: bool,
) -> impl Iterator<Item = walkdir::Result<walkdir::DirEntry>> + 'a {
    walkdir::WalkDir::new(to)
        .contents_first(contents_first)
        .into_iter()
        .filter_entry(move |entry| entry.depth() == 0 || ours.contains(&entry_key(to, entry)))
}

#[cfg(unix)]
fn move_back_only_ours(from: &Path, to: &Path, ours: &HashSet<Entry>) -> Result<()> {
    let walk = |contents_first| only_ours(to, ours, contents_first);
    for entry in walk(false) {
        let entry = entry?;
        let back = join_rest(from, entry.path().strip_prefix(to)?);
        if entry.file_type().is_dir() {
            fs::create_dir(&back)?;
        } else {
            rename_without_replacing(entry.path(), &back)?;
        }
    }
    for entry in walk(true) {
        let entry = entry?;
        match fs::remove_dir(entry.path()) {
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
            removed => removed?,
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn swap(staged: &Path, placed: &Path) -> std::io::Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staged,
        rustix::fs::CWD,
        placed,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(std::io::Error::from)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn swap(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn swap_works(folder: &Path) -> Result<bool> {
    let (first, second) = (folder.join("first"), folder.join("second"));
    let probe = || -> Result<bool> {
        fs::write(&first, b"first").with_context(|| format!("cannot write {}", first.display()))?;
        fs::write(&second, b"second")
            .with_context(|| format!("cannot write {}", second.display()))?;
        match swap(&first, &second) {
            Err(error) if unsupported(&error) => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot swap names in {}", folder.display()));
            }
            Ok(()) => {}
        }
        Ok(fs::read(&first).is_ok_and(|bytes| bytes == b"second")
            && fs::read(&second).is_ok_and(|bytes| bytes == b"first"))
    };
    let swaps = probe();
    let _ = fs::remove_file(&first);
    let _ = fs::remove_file(&second);
    swaps
}

fn unsupported(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    let unsupported_here = rustix::io::Errno::from_io_error(error).is_some_and(|errno| {
        [
            rustix::io::Errno::INVAL,
            rustix::io::Errno::NOTSUP,
            rustix::io::Errno::OPNOTSUPP,
            rustix::io::Errno::NOSYS,
        ]
        .contains(&errno)
    });
    #[cfg(not(unix))]
    let unsupported_here = false;
    unsupported_here || error.kind() == std::io::ErrorKind::Unsupported
}

#[cfg(unix)]
fn identity(path: &Path) -> Identity {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn identity(_: &Path) -> Identity {
    None
}

fn ensure_unchanged(path: &Path, expected: Identity) -> Result<()> {
    ensure!(
        expected.is_none() || identity(path) == expected,
        "{} changed during extraction",
        path.display()
    );
    Ok(())
}

type Spellings =
    std::collections::HashMap<PathBuf, std::collections::HashMap<u64, Vec<std::ffi::OsString>>>;

#[cfg(unix)]
fn spelled_on_disk(path: &Path, spellings: &mut Spellings) -> PathBuf {
    use std::os::unix::fs::{DirEntryExt, MetadataExt};
    let (Some(parent), Some(name), Ok(metadata)) =
        (path.parent(), path.file_name(), fs::symlink_metadata(path))
    else {
        return path.to_owned();
    };
    let names = spellings.entry(parent.to_owned()).or_insert_with(|| {
        let mut names = std::collections::HashMap::<u64, Vec<std::ffi::OsString>>::new();
        for entry in fs::read_dir(parent).into_iter().flatten().flatten() {
            names
                .entry(entry.ino())
                .or_default()
                .push(entry.file_name());
        }
        names
    });
    let folded = |name: &std::ffi::OsStr| name.to_string_lossy().to_lowercase();
    let candidates = names
        .get(&metadata.ino())
        .map(Vec::as_slice)
        .unwrap_or_default();
    if candidates.iter().any(|candidate| candidate == name) {
        return path.to_owned();
    }
    let matching: Vec<_> = candidates
        .iter()
        .filter(|candidate| folded(candidate) == folded(name))
        .cloned()
        .collect();
    match (candidates, matching.as_slice()) {
        ([only], _) | (_, [only]) => parent.join(only),
        _ => path.to_owned(),
    }
}

#[cfg(not(unix))]
fn spelled_on_disk(path: &Path, _: &mut Spellings) -> PathBuf {
    path.to_owned()
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
    plan_folder(
        staging,
        destination,
        existing_mode,
        relative,
        taken,
        steps,
        false,
    )
    .map(drop)
}

struct Cover {
    replaced: usize,
    kept: Vec<(PathBuf, u32)>,
}

struct CoveredFolder {
    path: PathBuf,
    steps: std::ops::Range<usize>,
    cover: Cover,
}

fn plan_folder(
    staging: &Path,
    destination: &Path,
    existing_mode: Existing,
    relative: &Path,
    taken: &mut HashSet<PathBuf>,
    steps: &mut Vec<Step>,
    replace_covered_folders: bool,
) -> Result<Option<Cover>> {
    let folder = staging.join(relative);
    let mut cover = (replace_covered_folders
        && existing_mode == Existing::Overwrite
        && !relative.as_os_str().is_empty())
    .then(|| mode_a_new_folder_can_keep(&folder, &destination.join(relative)))
    .flatten()
    .map(|mode| Cover {
        replaced: 0,
        kept: vec![(relative.to_owned(), mode)],
    });
    let mut covered_folders = Vec::new();
    let mut names = HashSet::new();
    for entry in
        fs::read_dir(&folder).with_context(|| format!("cannot read {}", folder.display()))?
    {
        let name = entry?.file_name();
        ensure!(
            Some(name.as_os_str()) != staging.file_name(),
            "archive entry has the staging folder's name"
        );
        let path = relative.join(&name);
        let staged = fs::symlink_metadata(staging.join(&path))?;
        match fs::symlink_metadata(destination.join(&path)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                steps.push(Step::Create(path))
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot read {}", destination.join(&path).display()));
            }
            Ok(existing) if staged.is_dir() && existing.is_dir() => {
                ensure!(
                    same_filesystem(&staged, &existing),
                    "{} is on another filesystem than the destination",
                    path.display()
                );
                let start = steps.len();
                let inner = plan_folder(
                    staging,
                    destination,
                    existing_mode,
                    &path,
                    taken,
                    steps,
                    replace_covered_folders,
                )?;
                match inner {
                    Some(inner) => covered_folders.push(CoveredFolder {
                        path,
                        steps: start..steps.len(),
                        cover: inner,
                    }),
                    None => cover = None,
                }
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
                _ => {
                    match &mut cover {
                        Some(cover) if flags(&existing) == 0 => cover.replaced += 1,
                        _ => cover = None,
                    }
                    steps.push(Step::Replace(path))
                }
            },
        }
        if cover.is_some() {
            names.insert(name);
        }
    }
    match cover.filter(|_| names_only(&destination.join(relative), &names)) {
        Some(mut cover) => {
            for inner in covered_folders {
                cover.replaced += inner.cover.replaced;
                cover.kept.extend(inner.cover.kept);
            }
            Ok(Some(cover))
        }
        None => {
            for inner in covered_folders.into_iter().rev() {
                if inner.cover.replaced > 0 {
                    let whole = Step::ReplaceFolder(inner.path, inner.cover.kept);
                    steps.splice(inner.steps, [whole]);
                }
            }
            Ok(None)
        }
    }
}

fn names_only(folder: &Path, names: &HashSet<std::ffi::OsString>) -> bool {
    fs::read_dir(folder).is_ok_and(|mut entries| {
        entries.all(|entry| entry.is_ok_and(|entry| names.contains(&entry.file_name())))
    })
}

#[cfg(target_os = "macos")]
fn flags(metadata: &fs::Metadata) -> u32 {
    std::os::macos::fs::MetadataExt::st_flags(metadata)
}

#[cfg(not(target_os = "macos"))]
fn flags(_: &fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
const OWNER_READS_WRITES_AND_ENTERS: u32 = 0o700;

#[cfg(unix)]
fn mode_a_new_folder_can_keep(staged_path: &Path, existing_path: &Path) -> Option<u32> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let staged = fs::symlink_metadata(staged_path).ok()?;
    let existing = fs::symlink_metadata(existing_path).ok()?;
    let mode = existing.permissions().mode() & 0o7777;
    let stands_in = staged.is_dir()
        && existing.is_dir()
        && staged.uid() == existing.uid()
        && staged.gid() == existing.gid()
        && flags(&staged) == flags(&existing)
        && mode & OWNER_READS_WRITES_AND_ENTERS == OWNER_READS_WRITES_AND_ENTERS
        && extended_attributes(existing_path)
            .is_some_and(|attributes| Some(attributes) == extended_attributes(staged_path))
        && !has_access_control_list(existing_path);
    stands_in.then_some(mode)
}

#[cfg(not(unix))]
fn mode_a_new_folder_can_keep(_: &Path, _: &Path) -> Option<u32> {
    None
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
const FIRST_ATTRIBUTE_BUFFER: usize = 256;

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
const LARGEST_ATTRIBUTE_BUFFER: usize = 1 << 20;

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn extended_attributes(path: &Path) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let filled = |read: &dyn Fn(&mut [u8]) -> rustix::io::Result<usize>| {
        let mut bytes = vec![0; FIRST_ATTRIBUTE_BUFFER];
        loop {
            match read(&mut bytes) {
                Ok(length) => {
                    bytes.truncate(length);
                    return Some(bytes);
                }
                Err(rustix::io::Errno::RANGE) if bytes.len() < LARGEST_ATTRIBUTE_BUFFER => {
                    bytes.resize(bytes.len() * 2, 0)
                }
                Err(_) => return None,
            }
        }
    };
    let names = filled(&|buffer| rustix::fs::llistxattr(path, buffer))?;
    names
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| {
            let value = filled(&|buffer| rustix::fs::lgetxattr(path, name, buffer))?;
            Some((name.to_vec(), value))
        })
        .collect()
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn extended_attributes(_: &Path) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    None
}

#[cfg(target_os = "macos")]
fn has_access_control_list(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    unsafe extern "C" {
        fn acl_get_link_np(
            path: *const std::ffi::c_char,
            kind: std::ffi::c_int,
        ) -> *mut std::ffi::c_void;
        fn acl_free(object: *mut std::ffi::c_void) -> std::ffi::c_int;
    }
    const ACL_TYPE_EXTENDED: std::ffi::c_int = 0x100;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: the path is NUL-terminated and outlives the call, which only reads it.
    let list = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
    if list.is_null() {
        return std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound;
    }
    // SAFETY: the list is the non-null result of acl_get_link_np and is freed once.
    unsafe { acl_free(list) };
    true
}

#[cfg(all(unix, not(target_os = "macos")))]
fn has_access_control_list(_: &Path) -> bool {
    false
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

    #[cfg(unix)]
    type Tree = std::collections::BTreeMap<PathBuf, (u32, Option<Vec<u8>>)>;

    #[cfg(unix)]
    fn tree(root: &Path, leave_out: &Path) -> Tree {
        use std::os::unix::fs::PermissionsExt;
        walkdir::WalkDir::new(root)
            .min_depth(1)
            .into_iter()
            .filter_entry(|entry| entry.path() != leave_out)
            .map(|entry| entry.unwrap())
            .map(|entry| {
                let metadata = entry.path().symlink_metadata().unwrap();
                let content = metadata.is_file().then(|| fs::read(entry.path()).unwrap());
                let path = entry.path().strip_prefix(root).unwrap().to_owned();
                (path, (metadata.permissions().mode(), content))
            })
            .collect()
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    struct Merge {
        _root: tempfile::TempDir,
        destination: PathBuf,
        staging: Staging,
        steps: Vec<Step>,
    }

    #[cfg(unix)]
    fn merge() -> Merge {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("out");
        fs::create_dir_all(destination.join("e")).unwrap();
        fs::write(destination.join("x"), b"old x").unwrap();
        set_mode(&destination.join("x"), 0o440);
        fs::write(destination.join("kept"), b"kept").unwrap();
        let staging = stage(
            &root.path().join("archive"),
            &destination,
            Existing::Overwrite,
            crate::Metadata::Restore,
        )
        .unwrap();
        fs::create_dir_all(staging.path().join("d")).unwrap();
        fs::create_dir_all(staging.path().join("e")).unwrap();
        fs::write(staging.path().join("d/f"), b"new f").unwrap();
        fs::write(staging.path().join("x"), b"new x").unwrap();
        fs::write(staging.path().join("e/g"), b"new g").unwrap();
        let steps = vec![
            Step::Create("d".into()),
            Step::Replace("x".into()),
            Step::Create("e/g".into()),
        ];
        Merge {
            _root: root,
            destination,
            staging,
            steps,
        }
    }

    #[cfg(unix)]
    fn run(merge: &Merge, after_each: impl FnMut(&Step)) -> anyhow::Error {
        let mut done = Vec::new();
        let error = merge
            .staging
            .carry_out(merge.steps.clone(), &mut done, &mut || Ok(()), after_each)
            .unwrap_err();
        merge.staging.undo(&done, error)
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_replaced_by_a_file_during_publish_undoes_every_earlier_rename() {
        let merge = merge();
        let staged = tree(merge.staging.path(), Path::new(""));
        let mut expected = tree(&merge.destination, merge.staging.path());
        expected.retain(|path, _| path != Path::new("e"));
        let error = run(&merge, |step| {
            if matches!(step, Step::Replace(_)) {
                fs::remove_dir(merge.destination.join("e")).unwrap();
                fs::write(merge.destination.join("e"), b"another program").unwrap();
            }
        });
        let message = format!("{error:#}");
        assert!(
            message.contains(&merge.destination.join("e/g").display().to_string()),
            "{message}"
        );
        assert!(!message.contains("could not undo"), "{message}");
        let mut after = tree(&merge.destination, merge.staging.path());
        assert_eq!(
            after.remove(Path::new("e")).unwrap().1.unwrap(),
            b"another program"
        );
        assert_eq!(after, expected);
        assert_eq!(after[Path::new("x")], (0o100440, Some(b"old x".to_vec())));
        assert!(!after.contains_key(Path::new("d")));
        let mut staged_after = tree(merge.staging.path(), Path::new(""));
        staged_after.retain(|path, _| !path.to_string_lossy().starts_with(".zipper-"));
        assert_eq!(staged_after, staged);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_another_extraction_creates_first_is_merged_into() {
        let merge = merge();
        let steps = vec![Step::Replace("x".into()), Step::Create("d".into())];
        let mut done = Vec::new();
        let carried = merge
            .staging
            .carry_out(steps, &mut done, &mut || Ok(()), |step| {
                if matches!(step, Step::Replace(_)) {
                    fs::create_dir(merge.destination.join("d")).unwrap();
                    fs::write(merge.destination.join("d/other"), b"another extraction").unwrap();
                }
            });
        let carried = carried.unwrap_or_else(|error| panic!("{error:#}"));
        assert!(
            !carried
                .iter()
                .any(|step| matches!(step, Step::Create(path) if path == Path::new("d")))
        );
        assert_eq!(fs::read(merge.destination.join("d/f")).unwrap(), b"new f");
        assert_eq!(
            fs::read(merge.destination.join("d/other")).unwrap(),
            b"another extraction"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cancel_before_a_merged_step_stops_there_and_keeps_the_other_extraction_files() {
        let merge = merge();
        let before = tree(&merge.destination, merge.staging.path());
        let steps = vec![
            Step::Replace("x".into()),
            Step::Create("d".into()),
            Step::Create("e/g".into()),
        ];
        let checks = std::cell::Cell::new(0);
        let mut check = || {
            checks.set(checks.get() + 1);
            ensure!(checks.get() < 3, crate::Cancelled);
            Ok(())
        };
        let mut done = Vec::new();
        let error = merge
            .staging
            .carry_out(steps, &mut done, &mut check, |step| {
                if matches!(step, Step::Replace(_)) {
                    fs::create_dir(merge.destination.join("d")).unwrap();
                    fs::write(merge.destination.join("d/other"), b"another extraction").unwrap();
                }
            })
            .unwrap_err();
        assert_eq!(checks.get(), 3);
        let error = merge.staging.undo(&done, error);
        assert!(
            error.downcast_ref::<crate::Cancelled>().is_some(),
            "{error:#}"
        );
        let mut after = tree(&merge.destination, merge.staging.path());
        assert_eq!(
            after.remove(Path::new("d/other")).unwrap().1.unwrap(),
            b"another extraction"
        );
        assert!(after.remove(Path::new("d")).unwrap().1.is_none());
        assert_eq!(after, before);
        assert_eq!(
            fs::read(merge.staging.path().join("d/f")).unwrap(),
            b"new f"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_another_extraction_removes_in_its_undo_is_made_again() {
        let merge = merge();
        let steps = vec![Step::Replace("x".into()), Step::Create("e/g".into())];
        let mut done = Vec::new();
        let carried = merge
            .staging
            .carry_out(steps, &mut done, &mut || Ok(()), |step| {
                if matches!(step, Step::Replace(_)) {
                    fs::remove_dir(merge.destination.join("e")).unwrap();
                }
            });
        carried.unwrap_or_else(|error| panic!("{error:#}"));
        assert_eq!(fs::read(merge.destination.join("e/g")).unwrap(), b"new g");
        let error = merge.staging.undo(&done, anyhow::anyhow!("later step"));
        assert_eq!(format!("{error:#}"), "later step");
        assert!(!merge.destination.join("e").exists());
        assert_eq!(fs::read(merge.destination.join("x")).unwrap(), b"old x");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_create_whose_target_is_gone_again_is_tried_again() {
        let merge = merge();
        let after = |path: &str| merge.staging.after_failed_create(Path::new(path)).unwrap();
        assert!(matches!(after("d"), AfterFailedCreate::TryAgain));
        fs::create_dir(merge.destination.join("d")).unwrap();
        assert!(matches!(after("d"), AfterFailedCreate::Merge(_)));
        fs::remove_dir(merge.destination.join("d")).unwrap();
        fs::write(merge.destination.join("d"), b"another program").unwrap();
        assert!(matches!(after("d"), AfterFailedCreate::Fail));
    }

    #[test]
    fn a_parent_that_vanishes_while_its_child_is_made_is_reported_as_vanished() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("gone/child");
        assert!(matches!(
            make_folder_again(&child),
            Ok(MadeAgain::ParentVanished)
        ));
        assert!(matches!(
            make_folder_again(&root.path().join("here")),
            Ok(MadeAgain::Made)
        ));
        assert!(matches!(
            make_folder_again(&root.path().join("here")),
            Ok(MadeAgain::Existed)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn an_unlock_for_cleanup_leaves_the_modes_of_another_extraction_folders() {
        let merge = merge();
        fs::create_dir_all(merge.staging.path().join("d/h/k")).unwrap();
        let steps = vec![Step::Create("d".into())];
        let mut done = Vec::new();
        merge
            .staging
            .carry_out(steps, &mut done, &mut || Ok(()), |_| {
                fs::create_dir(merge.destination.join("d/other")).unwrap();
                set_mode(&merge.destination.join("d/other"), 0o555);
            })
            .unwrap();
        set_mode(&merge.destination.join("d/h/k"), 0o300);
        set_mode(&merge.destination.join("d/h"), 0o300);
        merge.staging.unlock_created(&done);
        let mode = |path: &str| {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(merge.destination.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("d/h"), 0o700);
        assert_eq!(mode("d/h/k"), 0o700);
        assert_eq!(mode("d/other"), 0o555);
        set_mode(&merge.destination.join("d/other"), 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn an_undo_keeps_what_another_extraction_put_in_a_folder_it_created() {
        let merge = merge();
        fs::create_dir(merge.staging.path().join("d/h")).unwrap();
        fs::write(merge.staging.path().join("d/h/i"), b"new i").unwrap();
        let before = tree(&merge.destination, merge.staging.path());
        let error = run(&merge, |step| match step {
            Step::Create(path) if path == Path::new("d") => {
                fs::write(merge.destination.join("d/h/other"), b"another extraction").unwrap();
            }
            Step::Replace(_) => {
                fs::remove_dir(merge.destination.join("e")).unwrap();
                fs::write(merge.destination.join("e"), b"another program").unwrap();
            }
            _ => {}
        });
        let message = format!("{error:#}");
        assert!(!message.contains("could not undo"), "{message}");
        let mut after = tree(&merge.destination, merge.staging.path());
        assert_eq!(
            after.remove(Path::new("d/h/other")).unwrap().1.unwrap(),
            b"another extraction"
        );
        assert!(after.remove(Path::new("d/h")).unwrap().1.is_none());
        assert!(after.remove(Path::new("d")).unwrap().1.is_none());
        after.remove(Path::new("e"));
        let mut expected = before;
        expected.remove(Path::new("e"));
        assert_eq!(after, expected);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_replaced_by_a_folder_during_publish_is_swapped_back() {
        let merge = merge();
        let before = tree(&merge.destination, merge.staging.path());
        let error = run(&merge, |step| {
            if matches!(step, Step::Create(path) if path == Path::new("d")) {
                fs::remove_file(merge.destination.join("x")).unwrap();
                fs::create_dir(merge.destination.join("x")).unwrap();
                fs::write(merge.destination.join("x/inner"), b"another program").unwrap();
            }
        });
        let message = format!("{error:#}");
        assert!(message.contains("became a directory"), "{message}");
        assert!(!message.contains("could not undo"), "{message}");
        let after = tree(&merge.destination, merge.staging.path());
        assert_eq!(
            after[Path::new("x/inner")].1.as_deref(),
            Some(&b"another program"[..])
        );
        let mut unchanged = after.clone();
        unchanged.retain(|path, _| !path.starts_with("x"));
        let mut expected = before;
        expected.retain(|path, _| !path.starts_with("x"));
        assert_eq!(unchanged, expected);
        assert_eq!(fs::read(merge.staging.path().join("x")).unwrap(), b"new x");
    }

    #[cfg(unix)]
    #[test]
    fn an_undo_that_fails_names_what_it_left_and_keeps_the_staging_folder() {
        let merge = merge();
        let staging = merge.staging.path().to_owned();
        let error = run(&merge, |step| {
            if matches!(step, Step::Create(path) if path == Path::new("d")) {
                set_mode(&merge.destination, 0o555);
            }
        });
        let message = format!("{error:#}");
        set_mode(&merge.destination, 0o755);
        assert!(message.contains("could not undo"), "{message}");
        assert!(
            message.contains(&merge.destination.join("d").display().to_string()),
            "{message}"
        );
        assert!(
            message.contains(&merge.destination.join("x").display().to_string()),
            "{message}"
        );
        let back = format!(
            "cannot move {} back to {}",
            merge.destination.join("d").display(),
            staging.join("d").display()
        );
        assert!(message.contains(&back), "{message}");
        assert!(!message.contains("into place"), "{message}");
        assert!(
            message.contains(&format!(
                "{} is kept with everything not moved back",
                staging.display()
            )),
            "{message}"
        );
        drop(merge.staging);
        assert!(staging.is_dir());
        assert_eq!(fs::read(staging.join("x")).unwrap(), b"new x");
    }

    #[cfg(unix)]
    #[test]
    fn a_replace_in_two_moves_is_undone_with_the_old_file_and_its_mode() {
        let merge = merge();
        let before = tree(&merge.destination, merge.staging.path());
        let mut done = Vec::new();
        let mut aside = None;
        Staging::replace_in_two_moves(
            merge.staging.path().join("x"),
            merge.destination.join("x"),
            merge.destination.join("x"),
            merge.staging.aside(&mut aside, 1).unwrap().0,
            &mut done,
        )
        .unwrap();
        assert_eq!(fs::read(merge.destination.join("x")).unwrap(), b"new x");
        let error = merge.staging.undo(&done, anyhow::anyhow!("later step"));
        assert_eq!(format!("{error:#}"), "later step");
        assert_eq!(tree(&merge.destination, merge.staging.path()), before);
        assert_eq!(fs::read(merge.staging.path().join("x")).unwrap(), b"new x");
        fs::remove_file(merge.destination.join("x")).unwrap();
        fs::create_dir(merge.destination.join("x")).unwrap();
        let mut done = Vec::new();
        let error = Staging::replace_in_two_moves(
            merge.staging.path().join("x"),
            merge.destination.join("x"),
            merge.destination.join("x"),
            merge.staging.aside(&mut aside, 2).unwrap().0,
            &mut done,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("became a directory"),
            "{error:#}"
        );
        let error = merge.staging.undo(&done, error);
        assert!(
            !format!("{error:#}").contains("could not undo"),
            "{error:#}"
        );
        assert!(merge.destination.join("x").is_dir());
        fs::remove_dir(merge.destination.join("x")).unwrap();
        fs::write(merge.destination.join("x"), b"old x").unwrap();
        set_mode(&merge.destination, 0o555);
        let aside_path = merge.staging.aside(&mut aside, 3).unwrap().0;
        let error = Staging::replace_in_two_moves(
            merge.staging.path().join("x"),
            merge.destination.join("x"),
            merge.destination.join("x"),
            aside_path.clone(),
            &mut Vec::new(),
        )
        .unwrap_err();
        set_mode(&merge.destination, 0o755);
        let message = format!("{error:#}");
        let aside_named = format!(
            "cannot move {} aside to replace it",
            merge.destination.join("x").display()
        );
        assert!(message.contains(&aside_named), "{message}");
        assert!(
            !message.contains(&aside_path.display().to_string()),
            "{message}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_undo_never_moves_what_a_swapped_in_link_points_at() {
        let merge = merge();
        let victim = merge.destination.parent().unwrap().join("victim");
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("g"), b"outside").unwrap();
        let steps = vec![Step::Create("e/g".into()), Step::Create("missing".into())];
        let mut done = Vec::new();
        let error = merge
            .staging
            .carry_out(steps, &mut done, &mut || Ok(()), |step| {
                if matches!(step, Step::Create(path) if path == Path::new("e/g")) {
                    fs::rename(
                        merge.destination.join("e"),
                        merge.destination.join("e_moved"),
                    )
                    .unwrap();
                    std::os::unix::fs::symlink(&victim, merge.destination.join("e")).unwrap();
                }
            })
            .unwrap_err();
        let message = format!("{:#}", merge.staging.undo(&done, error));
        assert!(message.contains("changed during extraction"), "{message}");
        assert_eq!(fs::read(victim.join("g")).unwrap(), b"outside");
        assert_eq!(
            fs::read(merge.destination.join("e_moved/g")).unwrap(),
            b"new g"
        );
    }

    #[cfg(unix)]
    #[test]
    fn removal_on_threads_takes_every_file_and_link_and_follows_no_link() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("kept"), b"kept").unwrap();
        let staging = root.path().join("staging");
        for folder in 0..12 {
            let folder = staging.join(format!("a/{folder}/deep"));
            fs::create_dir_all(&folder).unwrap();
            for file in 0..20 {
                fs::write(folder.join(file.to_string()), b"old").unwrap();
            }
            fs::write(folder.parent().unwrap().join("beside"), b"old").unwrap();
            std::os::unix::fs::symlink(&outside, folder.join("link")).unwrap();
            std::os::unix::fs::symlink(outside.join("kept"), folder.join("file link")).unwrap();
        }
        fs::write(staging.join("top"), b"old").unwrap();
        remove_files_on_threads(&staging);
        let left: Vec<PathBuf> = walkdir::WalkDir::new(&staging)
            .into_iter()
            .map(|entry| entry.unwrap())
            .filter(|entry| !entry.file_type().is_dir())
            .map(walkdir::DirEntry::into_path)
            .collect();
        assert_eq!(left, Vec::<PathBuf>::new());
        assert_eq!(
            walkdir::WalkDir::new(&staging).into_iter().count(),
            2 + 12 * 2
        );
        assert_eq!(fs::read(outside.join("kept")).unwrap(), b"kept");
    }

    #[cfg(unix)]
    #[test]
    fn a_swap_probe_matches_the_filesystem() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(
            swap_works(folder.path()).unwrap(),
            cfg!(any(
                target_os = "linux",
                target_os = "android",
                target_vendor = "apple"
            ))
        );
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 0);
        let merge = merge();
        let (aside, swaps) = merge.staging.aside(&mut None, 0).unwrap();
        assert_eq!(swaps, swap_works(folder.path()).unwrap());
        assert_eq!(fs::read_dir(aside.parent().unwrap()).unwrap().count(), 0);
        set_mode(folder.path(), 0o555);
        let unwritable = swap_works(folder.path());
        set_mode(folder.path(), 0o755);
        assert!(unwritable.is_err());
    }

    #[test]
    fn created_directories_are_those_under_a_created_or_replaced_path_on_every_small_plan() {
        let names = [
            "a", "a/", "b", "a/a", "a/b", "a/a/", "b/a", "ab", "a/ab", "a/a/a",
        ];
        let directories: Vec<Directory> = names
            .iter()
            .map(|name| Directory {
                name,
                mode: None,
                modified: None,
            })
            .collect();
        let paths = ["a", "b", "a/a", "a/b", "ab", "b/a"];
        for mask in 0u32..(1 << (paths.len() * 3)) {
            let steps: Vec<Step> = paths
                .iter()
                .enumerate()
                .filter_map(|(index, path)| match (mask >> (index * 3)) & 0b111 {
                    1 => Some(Step::Create(path.into())),
                    2 => Some(Step::Replace(path.into())),
                    3 => Some(Step::CreateAs(path.into(), "x".into())),
                    4 => Some(Step::SetAside(path.into(), "y".into())),
                    5 => Some(Step::ReplaceFolder(path.into(), vec![(path.into(), 0)])),
                    _ => None,
                })
                .collect();
            let expected: Vec<&str> = directories
                .iter()
                .filter(|directory| {
                    let name = Path::new(directory.name);
                    let kept = steps
                        .iter()
                        .any(|step| matches!(step, Step::ReplaceFolder(path, _) if name == path));
                    !kept && steps.iter().any(|step| {
                        matches!(
                            step,
                            Step::Create(path) | Step::Replace(path) | Step::ReplaceFolder(path, _)
                                if name.starts_with(path)
                        )
                    })
                })
                .map(|directory| directory.name)
                .collect();
            let created: Vec<&str> = created_directories(&directories, &steps)
                .iter()
                .map(|directory| directory.name)
                .collect();
            assert_eq!(created, expected, "{steps:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_ordinary_filesystem_allows_an_exact_merge_and_keeps_staging_empty() {
        let merge = merge();
        let folder = tempfile::tempdir().unwrap();
        Staging::ensure_exact_merge(&merge.destination, folder.path()).unwrap();
        assert_eq!(fs::read_dir(folder.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    mod cancel {
        use super::*;
        use rayon::prelude::*;
        use std::{
            cell::Cell,
            collections::BTreeMap,
            os::unix::fs::{PermissionsExt, symlink},
        };

        const MERGING: [Existing; 4] = [
            Existing::Overwrite,
            Existing::Skip,
            Existing::RenameNew,
            Existing::RenameExisting,
        ];

        #[derive(Debug, PartialEq, Eq)]
        enum Node {
            File(u32, Vec<u8>),
            Directory(u32),
            Link(PathBuf),
        }

        type Snapshot = BTreeMap<PathBuf, Node>;

        fn is_staging(name: &std::ffi::OsStr) -> bool {
            name.to_string_lossy().starts_with(".zipper-")
        }

        fn snapshot(root: &Path) -> Snapshot {
            walkdir::WalkDir::new(root)
                .min_depth(1)
                .into_iter()
                .filter_entry(|entry| !is_staging(entry.file_name()))
                .map(|entry| entry.unwrap())
                .map(|entry| {
                    let metadata = entry.path().symlink_metadata().unwrap();
                    let mode = metadata.permissions().mode() & 0o7777;
                    let node = if metadata.file_type().is_symlink() {
                        Node::Link(fs::read_link(entry.path()).unwrap())
                    } else if metadata.is_dir() {
                        Node::Directory(mode)
                    } else {
                        Node::File(mode, fs::read(entry.path()).unwrap())
                    };
                    (entry.path().strip_prefix(root).unwrap().to_owned(), node)
                })
                .collect()
        }

        fn staging_left(root: &Path) -> Vec<PathBuf> {
            walkdir::WalkDir::new(root)
                .min_depth(1)
                .into_iter()
                .map(|entry| entry.unwrap())
                .filter(|entry| is_staging(entry.file_name()))
                .map(|entry| entry.path().to_owned())
                .collect()
        }

        fn cancel_from(call: usize, calls: &Cell<usize>) -> impl FnMut() -> Result<()> + '_ {
            move || {
                let this = calls.get();
                calls.set(this + 1);
                if this >= call {
                    Err(crate::Cancelled.into())
                } else {
                    Ok(())
                }
            }
        }

        struct Scene {
            root: tempfile::TempDir,
            staging: Staging,
        }

        const NEW_FOLDER_MODES: [(&str, u32); 2] = [("ro", 0o555), ("d", 0o750)];

        fn new_folders() -> Vec<Directory<'static>> {
            NEW_FOLDER_MODES
                .iter()
                .map(|&(name, mode)| Directory {
                    name,
                    mode: Some(mode),
                    modified: None,
                })
                .collect()
        }

        fn scene(existing: Existing) -> Scene {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir_all(root.path().join("outside/folder")).unwrap();
            fs::write(root.path().join("outside/file"), b"outside").unwrap();
            let destination = root.path().join("out");
            if existing != Existing::Refuse {
                fs::create_dir_all(destination.join("e")).unwrap();
                fs::write(destination.join("e/kept"), b"kept in e").unwrap();
                fs::write(destination.join("x"), b"old x").unwrap();
                set_mode(&destination.join("x"), 0o440);
                symlink("../outside/file", destination.join("l")).unwrap();
                fs::write(destination.join("kept"), b"kept").unwrap();
            }
            let staging = stage(
                &root.path().join("archive"),
                &destination,
                existing,
                crate::Metadata::Restore,
            )
            .unwrap();
            let staged = staging.path().to_owned();
            fs::create_dir_all(staged.join("d")).unwrap();
            fs::write(staged.join("d/f"), b"new f").unwrap();
            fs::create_dir_all(staged.join("e")).unwrap();
            fs::write(staged.join("e/g"), b"new g").unwrap();
            symlink("../x", staged.join("e/up")).unwrap();
            fs::write(staged.join("x"), b"new x").unwrap();
            fs::write(staged.join("l"), b"new l").unwrap();
            fs::create_dir(staged.join("ro")).unwrap();
            fs::write(staged.join("ro/h"), b"new h").unwrap();
            symlink("d/f", staged.join("al")).unwrap();
            Scene { root, staging }
        }

        fn cancel_points_restoring_the_scene(existing: Existing) -> usize {
            for call in 0.. {
                let Scene { root, staging } = scene(existing);
                let destination = root.path().join("out");
                let before = snapshot(root.path());
                let calls = Cell::new(0);
                let published = staging.publish_checking(&new_folders(), cancel_from(call, &calls));
                drop(staging);
                let case = format!("{existing:?} cancelled at check {call}");
                assert_eq!(staging_left(root.path()), Vec::<PathBuf>::new(), "{case}");
                let Err(error) = published else {
                    assert_eq!(calls.get(), call, "{case}");
                    assert_eq!(fs::read(destination.join("d/f")).unwrap(), b"new f");
                    set_mode(&destination.join("ro"), 0o755);
                    return call;
                };
                assert!(error.is::<crate::Cancelled>(), "{case}: {error:#}");
                assert!(
                    !format!("{error:#}").contains("could not undo"),
                    "{case}: {error:#}"
                );
                assert_eq!(calls.get(), call + 1, "{case}");
                assert_eq!(snapshot(root.path()), before, "{case}");
            }
            unreachable!("publication ends after its last check")
        }

        #[test]
        fn a_cancel_at_any_check_of_publication_leaves_the_destination_as_it_was() {
            for (existing, moves_into_a_merge) in [
                (Existing::Refuse, 0),
                (Existing::Overwrite, 7),
                (Existing::Skip, 5),
                (Existing::RenameNew, 7),
                (Existing::RenameExisting, 9),
            ] {
                assert_eq!(
                    cancel_points_restoring_the_scene(existing),
                    moves_into_a_merge + 1,
                    "{existing:?} checks before each move into a merge, then once more \
                     after the folder modes, which is just before a new destination's one rename"
                );
            }
        }

        #[test]
        fn a_cancel_whose_undo_fails_stays_cancelled_and_names_what_it_left() {
            let Scene { root, staging } = scene(Existing::Overwrite);
            let destination = root.path().join("out");
            let after_folder_modes = 8;
            let calls = Cell::new(0);
            let published = staging.publish_checking(&new_folders(), || {
                calls.set(calls.get() + 1);
                if calls.get() < after_folder_modes {
                    return Ok(());
                }
                set_mode(&destination, 0o555);
                Err(crate::Cancelled.into())
            });
            set_mode(&destination, 0o755);
            let error = published.unwrap_err();
            let message = format!("{error:#}");
            assert!(error.is::<crate::Cancelled>(), "{message}");
            assert!(message.contains("could not undo"), "{message}");
            assert!(
                message.contains(&destination.join("x").display().to_string()),
                "{message}"
            );
            let kept = staging.path().to_owned();
            drop(staging);
            assert!(kept.is_dir());
        }

        const FULL_FOLDER_MODES: [(&str, u32); 3] =
            [("p", 0o555), ("p/sub", 0o555), ("p/fresh", 0o555)];

        fn full_folder_modes() -> Vec<Directory<'static>> {
            FULL_FOLDER_MODES
                .iter()
                .map(|&(name, mode)| Directory {
                    name,
                    mode: Some(mode),
                    modified: None,
                })
                .collect()
        }

        fn full_folder() -> Scene {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("outside")).unwrap();
            fs::write(root.path().join("outside/file"), b"outside").unwrap();
            let destination = root.path().join("out");
            fs::create_dir_all(destination.join("p/sub")).unwrap();
            fs::create_dir_all(destination.join("q")).unwrap();
            for name in ["p/a", "p/b", "p/sub/c", "q/x", "q/extra", "kept"] {
                fs::write(destination.join(name), format!("old {name}")).unwrap();
            }
            symlink("../../outside/file", destination.join("p/link")).unwrap();
            set_mode(&destination.join("p/a"), 0o440);
            set_mode(&destination.join("p"), 0o750);
            set_mode(&destination.join("p/sub"), 0o700);
            let staging = stage(
                &root.path().join("archive"),
                &destination,
                Existing::Overwrite,
                crate::Metadata::Restore,
            )
            .unwrap();
            let staged = staging.path().to_owned();
            fs::create_dir_all(staged.join("p/sub")).unwrap();
            fs::create_dir_all(staged.join("p/fresh")).unwrap();
            fs::create_dir_all(staged.join("q")).unwrap();
            for name in [
                "p/a",
                "p/b",
                "p/link",
                "p/sub/c",
                "p/sub/d",
                "p/fresh/e",
                "q/x",
            ] {
                fs::write(staged.join(name), format!("new {name}")).unwrap();
            }
            Scene { root, staging }
        }

        fn inode(path: &Path) -> u64 {
            std::os::unix::fs::MetadataExt::ino(&fs::symlink_metadata(path).unwrap())
        }

        #[test]
        fn a_folder_the_archive_replaces_entirely_is_published_in_one_step() {
            let Scene { root, staging } = full_folder();
            let destination = root.path().join("out");
            let mut expected = snapshot(root.path());
            let file = |name: &str| Node::File(0o644, format!("new {name}").into_bytes());
            for name in [
                "p/a",
                "p/b",
                "p/link",
                "p/sub/c",
                "p/sub/d",
                "p/fresh/e",
                "q/x",
            ] {
                expected.insert(Path::new("out").join(name), file(name));
            }
            expected.insert("out/p/fresh".into(), Node::Directory(0o555));
            let calls = Cell::new(0);
            let published = staging.publish_checking(&full_folder_modes(), || {
                calls.set(calls.get() + 1);
                Ok(())
            });
            published.unwrap_or_else(|error| panic!("{error:#}"));
            drop(staging);
            assert_eq!(staging_left(root.path()), Vec::<PathBuf>::new());
            assert_eq!(snapshot(root.path()), expected);
            assert_eq!(
                calls.get(),
                3,
                "one check before p, one before q/x and one after the folder modes"
            );
            set_mode(&destination.join("p/fresh"), 0o755);
        }

        #[test]
        fn a_cancel_at_any_check_puts_a_replaced_folder_back_as_it_was() {
            for call in 0..3 {
                let Scene { root, staging } = full_folder();
                let destination = root.path().join("out");
                let before = snapshot(root.path());
                let folders = [
                    inode(&destination.join("p")),
                    inode(&destination.join("p/sub")),
                ];
                let calls = Cell::new(0);
                let published =
                    staging.publish_checking(&full_folder_modes(), cancel_from(call, &calls));
                drop(staging);
                let error = published.unwrap_err();
                assert!(error.is::<crate::Cancelled>(), "check {call}: {error:#}");
                assert!(
                    !format!("{error:#}").contains("could not undo"),
                    "check {call}: {error:#}"
                );
                assert_eq!(
                    staging_left(root.path()),
                    Vec::<PathBuf>::new(),
                    "check {call}"
                );
                assert_eq!(snapshot(root.path()), before, "check {call}");
                assert_eq!(
                    [
                        inode(&destination.join("p")),
                        inode(&destination.join("p/sub"))
                    ],
                    folders,
                    "check {call}"
                );
            }
        }

        fn planned(scene: &Scene, alone: bool) -> Vec<Step> {
            let mut steps = Vec::new();
            plan_folder(
                scene.staging.path(),
                &scene.root.path().join("out"),
                Existing::Overwrite,
                Path::new(""),
                &mut HashSet::new(),
                &mut steps,
                alone,
            )
            .unwrap();
            steps
        }

        fn replaced_whole(steps: &[Step]) -> Vec<&str> {
            let mut folders: Vec<&str> = steps
                .iter()
                .filter_map(|step| match step {
                    Step::ReplaceFolder(path, _) => path.to_str(),
                    _ => None,
                })
                .collect();
            folders.sort();
            folders
        }

        #[test]
        fn a_covered_folder_is_planned_as_one_step_that_keeps_the_modes_of_its_folders() {
            let scene = full_folder();
            let mut steps = planned(&scene, true);
            steps.sort_by_key(|step| step.archive_source().map(Path::to_owned));
            let [Step::ReplaceFolder(folder, kept), Step::Replace(file)] = steps.as_slice() else {
                panic!("{steps:?}")
            };
            assert_eq!(
                (folder.as_path(), file.as_path()),
                (Path::new("p"), Path::new("q/x"))
            );
            let mut kept = kept.clone();
            kept.sort();
            assert_eq!(kept, [("p".into(), 0o750), ("p/sub".into(), 0o700)]);
        }

        fn run(command: &str, arguments: &[&str], path: &Path) -> bool {
            std::process::Command::new(command)
                .args(arguments)
                .arg(path)
                .status()
                .is_ok_and(|status| status.success())
        }

        #[test]
        fn a_folder_that_a_new_one_cannot_stand_in_for_is_merged_entry_by_entry() {
            type Change = fn(&Path) -> bool;
            let cases: [(&str, Change, &[&str]); 9] = [
                ("nothing", |_| true, &["p"]),
                (
                    "a file the archive does not name",
                    |out| fs::write(out.join("p/other"), b"other").is_ok(),
                    &["p/sub"],
                ),
                (
                    "a file the archive does not name, one folder down",
                    |out| fs::write(out.join("p/sub/other"), b"other").is_ok(),
                    &[],
                ),
                (
                    "a name in another case",
                    |out| fs::rename(out.join("p/a"), out.join("p/A")).is_ok(),
                    &["p/sub"],
                ),
                (
                    "a mode that stops its owner writing",
                    |out| {
                        fs::set_permissions(out.join("p"), fs::Permissions::from_mode(0o500))
                            .is_ok()
                    },
                    &["p/sub"],
                ),
                (
                    "an extended attribute",
                    |out| {
                        rustix::fs::lsetxattr(
                            out.join("p"),
                            "user.zipper.test",
                            b"1",
                            rustix::fs::XattrFlags::empty(),
                        )
                        .is_ok()
                    },
                    &["p/sub"],
                ),
                (
                    "a file flag",
                    |out| {
                        cfg!(target_os = "macos") && run("chflags", &["hidden"], &out.join("p/a"))
                    },
                    &["p/sub"],
                ),
                (
                    "a folder flag",
                    |out| cfg!(target_os = "macos") && run("chflags", &["hidden"], &out.join("p")),
                    &["p/sub"],
                ),
                (
                    "an access control list",
                    |out| {
                        cfg!(target_os = "macos")
                            && run("chmod", &["+a", "everyone deny delete"], &out.join("p"))
                    },
                    &["p/sub"],
                ),
            ];
            for (case, change, expected) in cases {
                let scene = full_folder();
                let out = scene.root.path().join("out");
                if !change(&out) {
                    eprintln!("skipped {case}, which this system cannot set up");
                    continue;
                }
                assert_eq!(replaced_whole(&planned(&scene, true)), expected, "{case}");
                assert!(replaced_whole(&planned(&scene, false)).is_empty(), "{case}");
                set_mode(&out.join("p"), 0o755);
            }
        }

        #[test]
        fn an_existing_folder_with_no_file_to_replace_is_merged_into() {
            let scene = full_folder();
            let out = scene.root.path().join("out");
            fs::write(out.join("p/other"), b"other").unwrap();
            fs::remove_file(out.join("p/sub/c")).unwrap();
            assert!(replaced_whole(&planned(&scene, true)).is_empty());
        }

        fn carried_out(scene: &Scene, steps: Vec<Step>) -> (Result<Vec<Step>>, Vec<Done>) {
            let mut done = Vec::new();
            let carried = scene
                .staging
                .carry_out(steps, &mut done, &mut || Ok(()), |_| {});
            (carried, done)
        }

        #[test]
        fn an_entry_that_appears_in_a_folder_before_it_is_replaced_stops_the_extraction() {
            type Change = fn(&Path);
            let cases: [(Change, &str, &str); 3] = [
                (
                    |out| fs::write(out.join("p/sub/late"), b"late").unwrap(),
                    "p/sub/late appeared in the destination",
                    "p/sub/late",
                ),
                (
                    |out| {
                        fs::remove_file(out.join("p/a")).unwrap();
                        fs::create_dir(out.join("p/a")).unwrap();
                    },
                    "p/a changed between a file and a directory",
                    "p/a",
                ),
                (
                    |out| {
                        fs::remove_dir_all(out.join("p")).unwrap();
                        fs::write(out.join("p"), b"late").unwrap();
                    },
                    "p stopped being a directory",
                    "p",
                ),
            ];
            for (change, message, changed) in cases {
                let scene = full_folder();
                let out = scene.root.path().join("out");
                let steps = planned(&scene, true);
                change(&out);
                let before = snapshot(scene.root.path());
                let (carried, done) = carried_out(&scene, steps);
                let error = scene.staging.undo(&done, carried.unwrap_err());
                let text = format!("{error:#}");
                assert!(text.contains(message), "{text}");
                assert!(!text.contains("could not undo"), "{text}");
                assert_eq!(snapshot(scene.root.path()), before, "{changed}");
            }
        }

        #[test]
        fn a_folder_that_vanishes_before_it_is_replaced_is_created() {
            let scene = full_folder();
            let out = scene.root.path().join("out");
            let steps = planned(&scene, true);
            fs::remove_dir_all(out.join("p")).unwrap();
            let (carried, _) = carried_out(&scene, steps);
            let carried = carried.unwrap_or_else(|error| panic!("{error:#}"));
            assert!(replaced_whole(&carried).is_empty(), "{carried:?}");
            assert_eq!(fs::read(out.join("p/sub/d")).unwrap(), b"new p/sub/d");
            assert_eq!(fs::read(out.join("q/extra")).unwrap(), b"old q/extra");
        }

        #[test]
        fn an_undo_keeps_what_another_program_put_in_a_folder_that_replaced_one() {
            let scene = full_folder();
            let out = scene.root.path().join("out");
            let (carried, done) = carried_out(&scene, planned(&scene, true));
            carried.unwrap();
            let folder = inode(&out.join("p"));
            fs::write(out.join("p/sub/late"), b"late").unwrap();
            let error = scene.staging.undo(&done, anyhow::anyhow!("stopped"));
            let text = format!("{error:#}");
            assert!(text.contains("could not undo"), "{text}");
            assert!(
                text.contains(&out.join("p/sub/late").display().to_string()),
                "{text}"
            );
            assert_eq!(fs::read(out.join("p/a")).unwrap(), b"old p/a");
            assert_ne!(inode(&out.join("p")), folder);
            let kept = scene.staging.path().to_owned();
            drop(scene.staging);
            assert_eq!(fs::read(kept.join("p/sub/late")).unwrap(), b"late");
        }

        #[test]
        fn a_folder_is_merged_entry_by_entry_while_another_extraction_shares_the_destination() {
            let Scene { root, staging } = full_folder();
            let out = root.path().join("out");
            let folder = inode(&out.join("p"));
            let other = fs::File::open(&out).unwrap();
            rustix::fs::flock(&other, rustix::fs::FlockOperation::LockShared).unwrap();
            staging.publish_checking(&[], || Ok(())).unwrap();
            assert_eq!(inode(&out.join("p")), folder);
            assert_eq!(fs::read(out.join("p/a")).unwrap(), b"new p/a");
        }

        #[test]
        fn publication_waits_for_an_extraction_that_replaces_folders_and_a_cancel_ends_the_wait() {
            for cancels in [true, false] {
                let Scene { root, staging } = full_folder();
                let out = root.path().join("out");
                let before = snapshot(root.path());
                let folder = inode(&out.join("p"));
                let other = fs::File::open(&out).unwrap();
                rustix::fs::flock(&other, rustix::fs::FlockOperation::LockExclusive).unwrap();
                let calls = Cell::new(0);
                let published = staging.publish_checking(&[], || {
                    calls.set(calls.get() + 1);
                    if calls.get() == 3 && cancels {
                        return Err(crate::Cancelled.into());
                    }
                    if calls.get() == 3 {
                        rustix::fs::flock(&other, rustix::fs::FlockOperation::Unlock).unwrap();
                    }
                    Ok(())
                });
                if cancels {
                    assert!(published.unwrap_err().is::<crate::Cancelled>());
                    assert_eq!(calls.get(), 3);
                    drop(staging);
                    assert_eq!(snapshot(root.path()), before);
                } else {
                    published.unwrap();
                    assert!(calls.get() > 3);
                    assert_eq!(inode(&out.join("p")), folder);
                    assert_eq!(fs::read(out.join("p/a")).unwrap(), b"new p/a");
                }
            }
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Kind {
            File,
            Directory,
            Link,
        }

        type Tree = BTreeMap<String, Kind>;

        fn forest(prefix: &str, depth: usize) -> Vec<Tree> {
            let mut trees = vec![Tree::new()];
            for name in ["a", "b"] {
                let path = format!("{prefix}{name}");
                let mut options = vec![
                    Tree::new(),
                    Tree::from([(path.clone(), Kind::File)]),
                    Tree::from([(path.clone(), Kind::Link)]),
                ];
                let children = if depth > 1 {
                    forest(&format!("{path}/"), depth - 1)
                } else {
                    vec![Tree::new()]
                };
                for mut child in children {
                    child.insert(path.clone(), Kind::Directory);
                    options.push(child);
                }
                trees = trees
                    .iter()
                    .flat_map(|tree| {
                        options.iter().map(move |option| {
                            let mut merged = tree.clone();
                            merged.extend(option.clone());
                            merged
                        })
                    })
                    .collect();
            }
            trees
        }

        fn build(root: &Path, tree: &Tree, link: fn(&str) -> String, contents: &str) {
            fs::create_dir_all(root).unwrap();
            for (name, kind) in tree {
                let path = root.join(name);
                match kind {
                    Kind::Directory => fs::create_dir(&path).unwrap(),
                    Kind::File => fs::write(&path, format!("{contents} {name}")).unwrap(),
                    Kind::Link => symlink(link(name), &path).unwrap(),
                }
            }
        }

        fn outside_folder(name: &str) -> String {
            format!("{}outside", "../".repeat(name.matches('/').count() + 1))
        }

        fn sibling(name: &str) -> String {
            if name.ends_with('a') { "b" } else { "a" }.to_owned()
        }

        fn cancel_points_restoring_the_pair(
            destination: &Tree,
            archive: &Tree,
            existing: Existing,
        ) -> usize {
            for call in 0.. {
                let root = tempfile::tempdir().unwrap();
                fs::create_dir(root.path().join("outside")).unwrap();
                fs::write(root.path().join("outside/inner"), b"outside").unwrap();
                let out = root.path().join("out");
                build(&out, destination, outside_folder, "old");
                let staging = stage(
                    &root.path().join("archive"),
                    &out,
                    existing,
                    crate::Metadata::Restore,
                )
                .unwrap();
                build(staging.path(), archive, sibling, "new");
                let before = snapshot(root.path());
                let calls = Cell::new(0);
                let published = staging.publish_checking(&[], cancel_from(call, &calls));
                drop(staging);
                let case = format!("{existing:?} {destination:?} {archive:?} at check {call}");
                assert_eq!(staging_left(root.path()), Vec::<PathBuf>::new(), "{case}");
                let Err(error) = published else {
                    return call;
                };
                assert!(
                    !format!("{error:#}").contains("could not undo"),
                    "{case}: {error:#}"
                );
                assert_eq!(snapshot(root.path()), before, "{case}: {error:#}");
                if !error.is::<crate::Cancelled>() {
                    return call;
                }
                assert_eq!(calls.get(), call + 1, "{case}");
            }
            unreachable!("publication ends after its last check")
        }

        #[test]
        fn a_cancel_at_any_check_restores_every_small_tree_pair() {
            let stride = if std::env::var_os("ZIPPER_OVERWRITE_EXHAUSTIVE").is_some() {
                std::env::var("ZIPPER_MERGE_STRIDE")
                    .ok()
                    .and_then(|stride| stride.parse().ok())
                    .unwrap_or(37)
            } else {
                1009
            };
            let trees = forest("", 2);
            let pairs: Vec<(&Tree, &Tree)> = trees
                .iter()
                .flat_map(|destination| trees.iter().map(move |archive| (destination, archive)))
                .step_by(stride)
                .collect();
            let cancelled: usize = pairs
                .par_iter()
                .map(|&(destination, archive)| {
                    MERGING
                        .iter()
                        .map(|&existing| {
                            cancel_points_restoring_the_pair(destination, archive, existing)
                        })
                        .sum::<usize>()
                })
                .sum();
            eprintln!(
                "{} of {} tree pairs in 4 modes: {cancelled} cancelled publications restored",
                pairs.len(),
                trees.len() * trees.len()
            );
            assert!(cancelled > pairs.len());
        }
    }
}
