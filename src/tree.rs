use crate::{Entry, progress::Tracker, safe_path};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::Path,
    time::SystemTime,
};

/// Options for `read_archive` in each reader module that re-exports this
/// type.
#[derive(Clone, Copy, Default)]
pub struct ReadOptions<'a> {
    /// Cap on declared output bytes. Without it, extraction must fit in the
    /// destination filesystem's free space.
    pub max_output_bytes: Option<u64>,
    /// What to do when the destination exists.
    pub existing: crate::Existing,
    /// Exact entry paths or folder subtrees to read. Empty reads every entry.
    pub paths: &'a [String],
    /// Wildcard patterns to read, written as for
    /// [`crate::ExtractOptions::include`].
    pub include: &'a [String],
    /// Wildcard patterns to leave out, written as for
    /// [`crate::ExtractOptions::include`].
    pub exclude: &'a [String],
    /// Leading path segments to drop before writing, as [`crate::Strip`]
    /// describes. Selection still matches the full archive paths.
    pub strip: crate::Strip<'a>,
    /// Which metadata to restore beside contents, as [`crate::Metadata`]
    /// describes.
    pub metadata: crate::Metadata,
    /// Progress reports and cancellation.
    pub control: crate::Control<'a>,
    /// Accept the NTFS alternate data streams a WIM stores. With a
    /// destination they are written as
    /// [`crate::ExtractOptions::alternate_streams`] describes. Without one,
    /// their names and data are checked on every platform. Other readers
    /// ignore it.
    pub alternate_streams: bool,
}

pub(crate) const MAX_ENTRIES: usize = 1_000_000;
pub(crate) const MAX_NAME: u64 = 8192;
const MAX_METADATA: usize = 64 * 1024 * 1024;
const COPY_BUFFER: usize = 1024 * 1024;
const LINKS_PER_REPORT: usize = 1024;
pub(crate) const BACKEND_HINT: &str = "pass --zip-engine backend to read it with 7-Zip";

pub(crate) trait Holes: Read {
    fn skip_hole(&mut self, most: u64) -> std::io::Result<u64>;
}

type Sink<'s> = &'s mut dyn FnMut(&[u8]) -> Result<()>;

struct NoHoles<'r>(&'r mut dyn Read);

impl Read for NoHoles<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(out)
    }
}

impl Holes for NoHoles<'_> {
    fn skip_hole(&mut self, _most: u64) -> std::io::Result<u64> {
        Ok(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Node {
    ImpliedDirectory,
    Directory,
    File,
    LinkedFile,
    HardLink,
    Symlink,
}

struct DirectoryMetadata {
    name: String,
    mode: u32,
    modified: Option<SystemTime>,
}

struct Budget {
    limit: Option<u64>,
    free: u64,
    declared: u64,
    estimate: u64,
}

impl Budget {
    fn charge(&mut self, size: u64) -> Result<()> {
        self.declared = self
            .declared
            .checked_add(size)
            .context("archive output size overflows")?;
        self.estimate = self
            .estimate
            .saturating_add(size.div_ceil(4096).saturating_mul(4096));
        match self.limit {
            Some(limit) => ensure!(
                self.declared <= limit,
                "archive expands to more than --max-extracted-bytes {limit}"
            ),
            None => ensure!(
                self.estimate <= self.free,
                "archive needs more than the {} bytes free; pass --max-extracted-bytes to override",
                self.free
            ),
        }
        Ok(())
    }
}

pub(crate) struct Tree<'a> {
    format: &'static str,
    root: Option<&'a Path>,
    nodes: HashMap<String, (Node, u32)>,
    metadata_bytes: usize,
    created_directories: HashSet<String>,
    directories: Vec<DirectoryMetadata>,
    directory_index: HashMap<String, usize>,
    hard_links: Vec<(String, String)>,
    symlinks: Vec<(String, String)>,
    streams_kept: Option<bool>,
    entries: Vec<Entry>,
    budget: Budget,
    tracker: &'a Tracker<'a>,
    selection: crate::Selection<'a>,
    placement: crate::placement::Placement<'a>,
    placed: crate::placement::PlacedNames,
    placed_any: bool,
    metadata: crate::Metadata,
    buffer: Vec<u8>,
}

pub(crate) struct Written {
    placed: String,
    size: u64,
    modified: Option<SystemTime>,
    file: Option<fs::File>,
    unlocked: bool,
    locked_permissions: Option<fs::Permissions>,
}

fn leading_dots(raw: &str) -> usize {
    (raw.len() - entry_name(raw).len()) / 2
}

fn entry_name(raw: &str) -> &str {
    let mut name = raw;
    while let Some(rest) = name.strip_prefix("./") {
        name = rest;
    }
    name
}

fn checked_name(raw: &str) -> Result<(String, usize)> {
    let name = entry_name(raw);
    safe_path(name)?;
    Ok((name.to_owned(), leading_dots(raw)))
}

pub(crate) fn extract(
    format: &'static str,
    archive: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
    read: impl FnOnce(&mut Tree) -> Result<()>,
) -> Result<Vec<Entry>> {
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    let placement = crate::placement::Placement::new(options.strip)?;
    let tracker = Tracker::new(options.control, None, None);
    tracker.check()?;
    let staging = destination
        .map(|destination| {
            crate::staging::stage(archive, destination, options.existing, options.metadata)
        })
        .transpose()?;
    let free = match (&staging, options.max_output_bytes) {
        (Some(staging), None) => {
            let folder = staging.path();
            crate::available_bytes(folder)
                .with_context(|| format!("cannot measure free space at {}", folder.display()))?
        }
        _ => u64::MAX,
    };
    let mut tree = Tree {
        format,
        root: staging.as_ref().map(crate::staging::Staging::path),
        nodes: HashMap::new(),
        metadata_bytes: 0,
        created_directories: HashSet::new(),
        directories: Vec::new(),
        directory_index: HashMap::new(),
        hard_links: Vec::new(),
        symlinks: Vec::new(),
        streams_kept: None,
        entries: Vec::new(),
        budget: Budget {
            limit: options.max_output_bytes,
            free,
            declared: 0,
            estimate: 0,
        },
        tracker: &tracker,
        selection,
        placement,
        placed: crate::placement::PlacedNames::default(),
        placed_any: false,
        metadata: options.metadata,
        buffer: vec![0; COPY_BUFFER],
    };
    read(&mut tree)?;
    tree.ensure_each_selected_path_matches()?;
    tracker.finish();
    tracker.check()?;
    if let Some(staging) = &staging {
        tree.create_links()?;
        tree.publish(staging, options.control.cancel)?;
    }
    Ok(tree.entries)
}

impl<'a> Tree<'a> {
    pub(crate) fn check(&self) -> Result<()> {
        self.tracker.check()
    }

    pub(crate) fn check_name(&self, raw: &str) -> Result<()> {
        checked_name(raw).map(|_| ())
    }

    pub(crate) fn keep_for_links(&mut self, raw: &str) -> Result<()> {
        let (name, dots) = checked_name(raw)?;
        if self.placement.strips() {
            if let Some(placed) = self.placement.place_after_dots(&name, dots) {
                self.placed.keep(placed);
            }
        } else if let Some((node @ Node::File, _)) = self.nodes.get_mut(&name) {
            *node = Node::LinkedFile;
        }
        Ok(())
    }

    pub(crate) fn release_name(&mut self, bytes: usize) {
        self.metadata_bytes -= bytes;
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn sort_entries_from(&mut self, first: usize, order: &[usize]) -> Result<()> {
        ensure!(
            order.len() == self.entries.len() - first,
            "{} listing order covers {} of {} entries",
            self.format,
            order.len(),
            self.entries.len() - first
        );
        let mut keyed: Vec<(usize, Entry)> = order
            .iter()
            .copied()
            .zip(self.entries.drain(first..))
            .collect();
        keyed.sort_by_key(|(key, _)| *key);
        self.entries
            .extend(keyed.into_iter().map(|(_, entry)| entry));
        Ok(())
    }

    pub(crate) fn charge_name(&mut self, bytes: usize) -> Result<()> {
        self.metadata_bytes += bytes;
        ensure!(
            self.metadata_bytes <= MAX_METADATA,
            "{} names and link targets exceed 64 MiB",
            self.format
        );
        Ok(())
    }

    fn ensure_each_selected_path_matches(&self) -> Result<()> {
        self.selection
            .ensure_each_path_matches(self.nodes.keys().map(String::as_str))?;
        self.placement.ensure_something_placed(self.placed_any)?;
        self.placement
            .ensure_prefix_is_a_folder(self.nodes.iter().map(|(name, (node, _))| {
                (
                    name.as_str(),
                    matches!(node, Node::Directory | Node::ImpliedDirectory),
                )
            }))
    }

    fn place(
        &mut self,
        name: &str,
        dots: usize,
        directory: bool,
        replaces: bool,
    ) -> Result<Option<(String, bool)>> {
        if !self.selection.selects(name) {
            return Ok(None);
        }
        self.placed_any |= self.placement.is_prefix(name);
        let Some(placed) = self.placement.place_after_dots(name, dots) else {
            return Ok(None);
        };
        self.placed_any = true;
        let replaces = if self.placement.strips() {
            self.placed
                .add_entry(placed, directory, self.nodes[name].1)?
        } else {
            replaces
        };
        Ok(Some((placed.to_owned(), replaces)))
    }

    fn insert_node(&mut self, name: &str, node: Node) -> Result<Option<Node>> {
        ensure!(
            self.nodes.len() < MAX_ENTRIES,
            "{} has more than {MAX_ENTRIES} entries",
            self.format
        );
        self.charge_name(name.len())?;
        let id = self.nodes.len() as u32;
        Ok(match self.nodes.get_mut(name) {
            Some((earlier, _)) => Some(std::mem::replace(earlier, node)),
            None => {
                self.nodes.insert(name.to_owned(), (node, id));
                None
            }
        })
    }

    fn claim(&mut self, name: &str, node: Node) -> Result<bool> {
        let format = self.format;
        let mut ancestor = name;
        while let Some(slash) = ancestor.rfind('/') {
            ancestor = &ancestor[..slash];
            match self.nodes.get(ancestor).map(|(node, _)| node) {
                Some(Node::ImpliedDirectory | Node::Directory) => break,
                Some(_) => bail!("{format} entry {name:?} is inside a file or link"),
                None => {
                    self.insert_node(ancestor, Node::ImpliedDirectory)?;
                }
            }
        }
        match (self.insert_node(name, node)?, node) {
            (None, _) | (Some(Node::ImpliedDirectory), Node::Directory) => Ok(false),
            (Some(Node::Directory), Node::Directory) | (Some(Node::File), Node::File) => Ok(true),
            (Some(Node::ImpliedDirectory), _) => {
                bail!("{format} entry {name:?} is also a directory")
            }
            (Some(Node::LinkedFile), Node::File) => {
                bail!("{format} replaces {name:?}, which an earlier hard link names")
            }
            _ => bail!("{format} has two entries of different kinds named {name:?}"),
        }
    }

    fn create_parent(&mut self, name: &str) -> Result<()> {
        let (Some(root), Some(slash)) = (self.root, name.rfind('/')) else {
            return Ok(());
        };
        let directory = &name[..slash];
        if self.created_directories.insert(directory.to_owned()) {
            fs::create_dir_all(root.join(directory)).with_context(|| {
                format!("cannot create the folder of {} entry {name:?}", self.format)
            })?;
        }
        Ok(())
    }

    fn copy(
        &mut self,
        data: &mut dyn Holes,
        size: u64,
        counted: bool,
        mut sink: Option<Sink>,
    ) -> Result<()> {
        let mut remaining = size;
        while remaining > 0 {
            if sink.is_none() {
                let skipped = data
                    .skip_hole(remaining)
                    .with_context(|| format!("cannot read the {} archive", self.format))?
                    .min(remaining);
                if skipped > 0 {
                    if counted {
                        self.tracker.add_bytes(skipped)?;
                    }
                    remaining -= skipped;
                    continue;
                }
            }
            let want = remaining.min(self.buffer.len() as u64) as usize;
            let read = data
                .read(&mut self.buffer[..want])
                .with_context(|| format!("cannot read the {} archive", self.format))?;
            ensure!(read > 0, "truncated {} entry", self.format);
            if let Some(sink) = sink.as_mut() {
                sink(&self.buffer[..read])?;
            }
            if counted {
                self.tracker.add_bytes(read as u64)?;
            }
            remaining -= read as u64;
        }
        Ok(())
    }

    pub(crate) fn directory(
        &mut self,
        raw: &str,
        mode: u32,
        modified: Option<SystemTime>,
    ) -> Result<()> {
        self.placed_directory(raw, mode, modified).map(drop)
    }

    pub(crate) fn placed_directory(
        &mut self,
        raw: &str,
        mode: u32,
        modified: Option<SystemTime>,
    ) -> Result<Option<String>> {
        let dots = leading_dots(raw);
        let name = entry_name(raw).trim_end_matches('/');
        if name.is_empty() || name == "." {
            return Ok(None);
        }
        safe_path(name)?;
        let name = name.to_owned();
        let replaces = self.claim(&name, Node::Directory)?;
        let Some((placed, _)) = self.place(&name, dots, true, replaces)? else {
            return Ok(None);
        };
        if self.root.is_some() {
            self.create_parent(&format!("{placed}/"))?;
        }
        let metadata = DirectoryMetadata {
            name: placed.clone(),
            mode,
            modified: self.metadata.time(modified),
        };
        match self.directory_index.get(&placed) {
            Some(&index) => self.directories[index] = metadata,
            None => {
                self.directory_index
                    .insert(placed.clone(), self.directories.len());
                self.directories.push(metadata);
            }
        }
        self.entries.push(Entry {
            name: placed.clone(),
            size: 0,
            directory: true,
        });
        Ok(Some(placed))
    }

    pub(crate) fn file(
        &mut self,
        raw: &str,
        size: u64,
        mode: u32,
        modified: Option<SystemTime>,
        data: &mut dyn Read,
    ) -> Result<()> {
        let written = self.write_file(raw, size, mode, modified, data)?;
        self.finish_file(written)
    }

    pub(crate) fn sparse_file(
        &mut self,
        raw: &str,
        size: u64,
        mode: u32,
        modified: Option<SystemTime>,
        data: &mut dyn Holes,
    ) -> Result<()> {
        let written = self.write_data(raw, size, mode, modified, data)?;
        self.finish_file(written)
    }

    pub(crate) fn write_file(
        &mut self,
        raw: &str,
        size: u64,
        mode: u32,
        modified: Option<SystemTime>,
        data: &mut dyn Read,
    ) -> Result<Option<Written>> {
        self.write_data(raw, size, mode, modified, &mut NoHoles(data))
    }

    fn write_data(
        &mut self,
        raw: &str,
        size: u64,
        mode: u32,
        modified: Option<SystemTime>,
        data: &mut dyn Holes,
    ) -> Result<Option<Written>> {
        let (name, dots) = checked_name(raw)?;
        let mode = self.metadata.file_mode(Some(mode)).unwrap_or(mode);
        let replaces = self.claim(&name, Node::File)?;
        let Some((placed, replaces)) = self.place(&name, dots, false, replaces)? else {
            self.copy(data, size, false, None)?;
            return Ok(None);
        };
        self.budget.charge(size)?;
        let format = self.format;
        let file = match self.root {
            None => {
                self.copy(data, size, true, None)?;
                None
            }
            Some(root) => {
                self.create_parent(&placed)?;
                let path = root.join(&placed);
                if replaces {
                    fs::remove_file(&path)?;
                }
                let attributes = crate::windows_attributes(0, Some(mode));
                let mut file = crate::create_new_file(&path, mode, attributes).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        anyhow::anyhow!(
                            "{format} entry {placed:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
                        )
                    } else {
                        anyhow::Error::from(error)
                            .context(format!("cannot create {format} entry {placed:?}"))
                    }
                })?;
                self.copy(
                    data,
                    size,
                    true,
                    Some(&mut |bytes| {
                        file.write_all(bytes)
                            .with_context(|| format!("cannot write {format} entry {placed:?}"))
                    }),
                )?;
                Some(file)
            }
        };
        Ok(Some(Written {
            placed,
            size,
            modified,
            file,
            unlocked: false,
            locked_permissions: None,
        }))
    }

    pub(crate) fn finish_file(&mut self, written: Option<Written>) -> Result<()> {
        let Some(written) = written else {
            return Ok(());
        };
        if let Some(file) = &written.file {
            if let Some(permissions) = written.locked_permissions {
                file.set_permissions(permissions).with_context(|| {
                    format!(
                        "cannot restore the mode of {} entry {:?}",
                        self.format, written.placed
                    )
                })?;
            }
            if let Some(time) = self.metadata.time(written.modified) {
                crate::set_modified_best_effort(file, time);
            }
        }
        self.entries.push(Entry {
            name: written.placed,
            size: written.size,
            directory: false,
        });
        self.tracker.file_done()
    }

    pub(crate) fn writes_to_disk(&self) -> bool {
        self.root.is_some()
    }

    pub(crate) fn stream(
        &mut self,
        written: Option<&mut Written>,
        name: &str,
        size: u64,
        data: &mut dyn Read,
    ) -> Result<()> {
        let skipped = !self.metadata.restores()
            && name.eq_ignore_ascii_case(crate::ntfs_streams::MARK_OF_THE_WEB);
        let Some(written) = written.filter(|_| !skipped) else {
            return self.copy(&mut NoHoles(data), size, false, None);
        };
        self.budget.charge(size)?;
        let format = self.format;
        let (Some(root), Some(file)) = (self.root, &written.file) else {
            return self.copy(&mut NoHoles(data), size, true, None);
        };
        let kept = match self.streams_kept {
            Some(kept) => kept,
            None => *self
                .streams_kept
                .insert(crate::ntfs_streams::destination_keeps_streams(root)?),
        };
        ensure!(
            kept,
            "{format} entry {:?} has alternate data streams, and the file system of {} keeps none",
            written.placed,
            root.display()
        );
        if !written.unlocked {
            written.locked_permissions = crate::ntfs_streams::make_writable(file)?;
            written.unlocked = true;
        }
        let what = format!("stream {name:?} of {format} entry {:?}", written.placed);
        let mut stream = crate::ntfs_streams::create_stream(&root.join(&written.placed), name)
            .with_context(|| format!("cannot create {what}"))?;
        self.copy(
            &mut NoHoles(data),
            size,
            true,
            Some(&mut |bytes| {
                stream
                    .write_all(bytes)
                    .with_context(|| format!("cannot write {what}"))
            }),
        )
    }

    pub(crate) fn symlink(&mut self, raw: &str, target: String) -> Result<()> {
        self.placed_symlink(raw, target).map(drop)
    }

    pub(crate) fn placed_symlink(&mut self, raw: &str, target: String) -> Result<Option<usize>> {
        let (name, dots) = checked_name(raw)?;
        crate::ensure_link_stays_inside(self.format, &name, &target)?;
        self.charge_name(target.len())?;
        self.claim(&name, Node::Symlink)?;
        let Some((placed, _)) = self.place(&name, dots, false, false)? else {
            return Ok(None);
        };
        crate::ensure_link_stays_inside(self.format, &placed, &target)?;
        self.create_parent(&placed)?;
        self.symlinks.push((placed.clone(), target));
        self.entries.push(Entry {
            name: placed,
            size: 0,
            directory: false,
        });
        self.tracker.file_done()?;
        Ok(Some(self.symlinks.len() - 1))
    }

    pub(crate) fn hard_link(&mut self, raw: &str, raw_target: &str) -> Result<()> {
        let format = self.format;
        let (name, dots) = checked_name(raw)?;
        let (target, target_dots) = checked_name(raw_target)?;
        ensure!(
            matches!(
                self.nodes.get(&target).map(|(node, _)| node),
                Some(Node::File | Node::LinkedFile | Node::HardLink)
            ),
            "{format} hard link {name:?} does not name an earlier file"
        );
        self.charge_name(target.len())?;
        if let Some((node @ Node::File, _)) = self.nodes.get_mut(&target) {
            *node = Node::LinkedFile;
        }
        self.claim(&name, Node::HardLink)?;
        let placed_target = self
            .placement
            .place_after_dots(&target, target_dots)
            .filter(|_| self.selection.selects(&target))
            .map(str::to_owned);
        let Some((placed, _)) = self.place(&name, dots, false, false)? else {
            return Ok(());
        };
        let target = placed_target.with_context(|| {
            format!("{format} hard link {placed:?} needs {target:?}, which the selection or strip leaves out")
        })?;
        self.create_parent(&placed)?;
        self.hard_links.push((placed.clone(), target));
        self.entries.push(Entry {
            name: placed,
            size: 0,
            directory: false,
        });
        self.tracker.file_done()
    }

    fn create_links(&self) -> Result<()> {
        let Some(root) = self.root else {
            return Ok(());
        };
        let format = self.format;
        for (index, (name, target)) in self.hard_links.iter().enumerate() {
            if index % LINKS_PER_REPORT == 0 {
                self.tracker.report()?;
            }
            fs::hard_link(root.join(target), root.join(name)).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    anyhow::anyhow!(
                        "{format} hard link {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
                    )
                } else {
                    anyhow::Error::from(error)
                        .context(format!("cannot create {format} hard link {name:?}"))
                }
            })?;
        }
        for (index, (name, target)) in self.symlinks.iter().enumerate() {
            if index % LINKS_PER_REPORT == 0 {
                self.tracker.report()?;
            }
            crate::create_symlink(format, root, name, target)?;
        }
        Ok(())
    }

    fn publish(
        &self,
        staging: &crate::staging::Staging,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        let merged = crate::later_spellings_of_same_directory(
            staging.path(),
            self.directories
                .iter()
                .map(|directory| Some(directory.name.as_str())),
        )?;
        let directories: Vec<crate::Directory> = self
            .directories
            .iter()
            .enumerate()
            .filter(|(index, _)| !merged.contains(index))
            .map(|(_, directory)| crate::Directory {
                name: &directory.name,
                mode: self.metadata.directory_mode(Some(directory.mode)),
                modified: directory.modified,
            })
            .collect();
        staging.publish(&directories, cancel)
    }
}

#[cfg(test)]
mod hole_tests {
    use super::*;

    struct AllHole {
        left: u64,
        skips: u64,
    }

    impl Read for AllHole {
        fn read(&mut self, _out: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("read a hole byte by byte"))
        }
    }

    impl Holes for AllHole {
        fn skip_hole(&mut self, most: u64) -> std::io::Result<u64> {
            let skipped = most.min(self.left).min(1 << 30);
            self.left -= skipped;
            self.skips += 1;
            Ok(skipped)
        }
    }

    #[test]
    fn holes_are_skipped_without_reading_when_no_file_is_written() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join("unused.img");
        let size = 1 << 50;
        let mut hole = AllHole {
            left: size,
            skips: 0,
        };
        let listed = extract("test", &archive, None, ReadOptions::default(), |tree| {
            tree.sparse_file("hole.bin", size, 0o644, None, &mut hole)
        })
        .unwrap();
        assert_eq!(listed[0].size, size);
        assert_eq!((hole.left, hole.skips), (0, size >> 30));
        let skipped = ["other.bin".to_owned()];
        let mut hole = AllHole {
            left: size,
            skips: 0,
        };
        let options = ReadOptions {
            paths: &skipped,
            ..ReadOptions::default()
        };
        let into = work.path().join("out");
        extract("test", &archive, Some(&into), options, |tree| {
            tree.sparse_file("hole.bin", size, 0o644, None, &mut hole)?;
            tree.file("other.bin", 1, 0o644, None, &mut &b"x"[..])
        })
        .unwrap();
        assert_eq!(hole.left, 0);
        assert_eq!(std::fs::read(into.join("other.bin")).unwrap(), b"x");
    }
}
