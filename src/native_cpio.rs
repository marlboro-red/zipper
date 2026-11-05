//! In-process extraction of cpio archives in the binary, odc and newc forms,
//! published only after the whole archive reads cleanly. See
//! `docs/reference/cpio-reader.md` for what it accepts and restores.
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read},
    path::Path,
    time::SystemTime,
};

const COPY_BUFFER: usize = 1024 * 1024;
const TRAILER: &[u8] = b"TRAILER!!!";
const TYPE_MASK: u32 = 0o170000;
const REGULAR: u32 = 0o100000;
const DIRECTORY: u32 = 0o040000;
const SYMLINK: u32 = 0o120000;
const FIFO: u32 = 0o010000;
const CHARACTER_DEVICE: u32 = 0o020000;
const BLOCK_DEVICE: u32 = 0o060000;
const SOCKET: u32 = 0o140000;
const BINARY_MAGIC: u16 = 0o070707;
const BINARY_HEADER: usize = 26;
const ODC_HEADER: usize = 76;
const NEWC_HEADER: usize = 110;
const LONGEST_HEADER: usize = NEWC_HEADER;
const LINK_MEMORY: usize = 128 * 1024 * 1024;
const NAME_COST: usize = 32;
const GROUP_COST: usize = 2 * (size_of::<Inode>() + size_of::<Group>());
const DEFERRED_COST: usize = 2 * size_of::<Deferred>() + NAME_COST;
// SPEC(cpio(5) Portable ASCII Format): device, inode, mode, owner, group, links, rdev, mtime, name size, file size.
const ODC_FIELDS: [(usize, usize); 10] = [
    (6, 12),
    (12, 18),
    (18, 24),
    (24, 30),
    (30, 36),
    (36, 42),
    (42, 48),
    (48, 59),
    (59, 65),
    (65, 76),
];

/// Options for [`read_archive`] and [`crate::native_ar::read_archive`].
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Binary { big_endian: bool },
    Odc,
    Newc { crc: bool },
}

impl Form {
    fn of(magic: &[u8]) -> Option<Form> {
        match magic {
            [b'0', b'7', b'0', b'7', b'0', b'1', ..] => Some(Form::Newc { crc: false }),
            [b'0', b'7', b'0', b'7', b'0', b'2', ..] => Some(Form::Newc { crc: true }),
            [b'0', b'7', b'0', b'7', b'0', b'7', ..] => Some(Form::Odc),
            [first, second, ..] if u16::from_le_bytes([*first, *second]) == BINARY_MAGIC => {
                Some(Form::Binary { big_endian: false })
            }
            [first, second, ..] if u16::from_be_bytes([*first, *second]) == BINARY_MAGIC => {
                Some(Form::Binary { big_endian: true })
            }
            _ => None,
        }
    }

    fn header_length(self) -> usize {
        match self {
            Form::Binary { .. } => BINARY_HEADER,
            Form::Odc => ODC_HEADER,
            Form::Newc { .. } => NEWC_HEADER,
        }
    }

    fn alignment(self) -> u64 {
        match self {
            Form::Binary { .. } => 2,
            Form::Odc => 1,
            Form::Newc { .. } => 4,
        }
    }

    fn header(self, bytes: &[u8; LONGEST_HEADER]) -> Field<Header> {
        match self {
            Form::Binary { big_endian } => {
                binary_header(bytes[..BINARY_HEADER].try_into().unwrap(), big_endian)
            }
            Form::Odc => odc_header(bytes[..ODC_HEADER].try_into().unwrap()),
            Form::Newc { crc } => newc_header(bytes, crc),
        }
    }
}

fn padding(length: u64, alignment: u64) -> u64 {
    (alignment - length % alignment) % alignment
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Inode {
    number: u64,
    device: (u64, u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    inode: Inode,
    mode: u32,
    links: u64,
    modified: u64,
    size: u64,
    name_size: u64,
    check: u32,
}

fn hex(field: &[u8]) -> Field<u64> {
    field_check(!field.is_empty(), "cpio numeric field is empty")?;
    field.iter().try_fold(0u64, |value, &byte| {
        let digit = char::from(byte)
            .to_digit(16)
            .ok_or(BadField("cpio newc field is not hexadecimal"))?;
        value
            .checked_mul(16)
            .and_then(|value| value.checked_add(u64::from(digit)))
            .ok_or(BadField("cpio numeric field overflows"))
    })
}

fn octal(field: &[u8]) -> Field<u64> {
    field_check(!field.is_empty(), "cpio numeric field is empty")?;
    field.iter().try_fold(0u64, |value, &byte| {
        field_check((b'0'..=b'7').contains(&byte), "cpio odc field is not octal")?;
        value
            .checked_mul(8)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or(BadField("cpio numeric field overflows"))
    })
}

fn newc_header(bytes: &[u8; NEWC_HEADER], crc: bool) -> Field<Header> {
    let magic: &[u8] = if crc { b"070702" } else { b"070701" };
    field_check(&bytes[..6] == magic, "cpio header changes form")?;
    let mut fields = [0u64; 13];
    for (index, field) in fields.iter_mut().enumerate() {
        *field = hex(&bytes[6 + 8 * index..14 + 8 * index])?;
    }
    let [
        number,
        mode,
        _,
        _,
        links,
        modified,
        size,
        major,
        minor,
        _,
        _,
        name_size,
        check,
    ] = fields;
    Ok(Header {
        inode: Inode {
            number,
            device: (major, minor),
        },
        mode: u32::try_from(mode).map_err(|_| BadField("cpio mode overflows"))?,
        links,
        modified,
        size,
        name_size,
        check: u32::try_from(check).map_err(|_| BadField("cpio check overflows"))?,
    })
}

fn odc_header(bytes: &[u8; ODC_HEADER]) -> Field<Header> {
    field_check(&bytes[..6] == b"070707", "cpio header changes form")?;
    let mut fields = [0u64; 10];
    for (index, field) in fields.iter_mut().enumerate() {
        let (start, end) = ODC_FIELDS[index];
        *field = octal(&bytes[start..end])?;
    }
    let [
        device,
        number,
        mode,
        _,
        _,
        links,
        _,
        modified,
        name_size,
        size,
    ] = fields;
    Ok(Header {
        inode: Inode {
            number,
            device: (device, 0),
        },
        mode: u32::try_from(mode).map_err(|_| BadField("cpio mode overflows"))?,
        links,
        modified,
        name_size,
        size,
        check: 0,
    })
}

fn binary_header(bytes: &[u8; BINARY_HEADER], big_endian: bool) -> Field<Header> {
    let word = |index: usize| {
        let pair = [bytes[2 * index], bytes[2 * index + 1]];
        u64::from(if big_endian {
            u16::from_be_bytes(pair)
        } else {
            u16::from_le_bytes(pair)
        })
    };
    field_check(
        word(0) == u64::from(BINARY_MAGIC),
        "cpio header changes form",
    )?;
    Ok(Header {
        inode: Inode {
            number: word(2),
            device: (word(1), 0),
        },
        mode: word(3) as u32,
        links: word(6),
        modified: word(8) << 16 | word(9),
        name_size: word(10),
        size: word(11) << 16 | word(12),
        check: 0,
    })
}

fn unix_time(seconds: u64) -> Option<SystemTime> {
    std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))
}

/// Returns whether the file starts with a cpio header in the binary, odc
/// or newc form.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(6);
    File::open(path)?.take(6).read_to_end(&mut magic)?;
    Ok(Form::of(&magic).is_some())
}

struct Input {
    reader: BufReader<File>,
}

impl Input {
    fn exact(&mut self, buffer: &mut [u8], what: &str) -> Result<()> {
        let mut filled = 0;
        while filled < buffer.len() {
            let read = self
                .reader
                .read(&mut buffer[filled..])
                .context("cannot read the cpio archive")?;
            ensure!(read > 0, "truncated cpio {what}");
            filled += read;
        }
        Ok(())
    }

    fn skip(&mut self, count: u64, what: &str) -> Result<()> {
        let skipped = std::io::copy(&mut (&mut self.reader).take(count), &mut std::io::sink())
            .context("cannot read the cpio archive")?;
        ensure!(skipped == count, "truncated cpio {what}");
        Ok(())
    }

    fn header(&mut self, form: Form) -> Result<Header> {
        let mut bytes = [0u8; LONGEST_HEADER];
        let length = form.header_length();
        let mut filled = 0;
        while filled < length {
            let read = self
                .reader
                .read(&mut bytes[filled..length])
                .context("cannot read the cpio archive")?;
            ensure!(
                read > 0 || filled > 0,
                "cpio ends without its TRAILER!!! entry"
            );
            ensure!(read > 0, "truncated cpio header");
            filled += read;
        }
        Ok(form.header(&bytes)?)
    }

    fn only_zeros_remain(&mut self) -> Result<()> {
        let mut buffer = [0u8; 8192];
        loop {
            let read = self
                .reader
                .read(&mut buffer)
                .context("cannot read the cpio archive")?;
            if read == 0 {
                return Ok(());
            }
            ensure!(
                buffer[..read].iter().all(|&byte| byte == 0),
                "cpio has data after its TRAILER!!! entry"
            );
        }
    }
}

struct Summed<R> {
    inner: R,
    sum: u32,
}

impl<R: Read> Read for Summed<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        for &byte in &buffer[..read] {
            self.sum = self.sum.wrapping_add(u32::from(byte));
        }
        Ok(read)
    }
}

struct Deferred {
    name: String,
    mode: u32,
    modified: Option<SystemTime>,
}

struct Group {
    first_seen: u32,
    names_left: u32,
    written: Option<Box<str>>,
    waiting: Vec<Deferred>,
}

#[derive(Default)]
struct HardLinks {
    groups: HashMap<Inode, Group>,
    seen: u32,
    memory: usize,
}

fn deferred_cost(name: &str) -> usize {
    DEFERRED_COST + name.len()
}

fn name_cost(name: &str) -> usize {
    NAME_COST + name.len()
}

impl HardLinks {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.memory += bytes;
        ensure!(
            self.memory <= LINK_MEMORY,
            "cpio hard links take more than {} MiB to keep track of",
            LINK_MEMORY >> 20
        );
        Ok(())
    }

    fn group(&mut self, inode: Inode, links: u64) -> Result<&mut Group> {
        if !self.groups.contains_key(&inode) {
            self.charge(GROUP_COST)?;
            self.seen += 1;
            self.groups.insert(
                inode,
                Group {
                    first_seen: self.seen,
                    names_left: u32::try_from(links).unwrap_or(u32::MAX),
                    written: None,
                    waiting: Vec::new(),
                },
            );
        }
        let group = self.groups.get_mut(&inode).expect("a group just inserted");
        group.names_left = group.names_left.saturating_sub(1);
        Ok(group)
    }

    fn forget_if_complete(&mut self, inode: Inode) -> bool {
        let complete = self
            .groups
            .get(&inode)
            .is_some_and(|group| group.names_left == 0 && group.written.is_some());
        if complete && let Some(group) = self.groups.remove(&inode) {
            self.memory -= GROUP_COST + group.written.as_deref().map_or(0, name_cost);
        }
        complete
    }

    fn defer(
        &mut self,
        tree: &mut tree::Tree,
        inode: Inode,
        links: u64,
        entry: Deferred,
    ) -> Result<()> {
        tree.check_name(&entry.name)?;
        tree.charge_name(entry.name.len())?;
        if let Some(target) = self.group(inode, links)?.written.clone() {
            tree.hard_link(&entry.name, &target)?;
        } else {
            self.charge(deferred_cost(&entry.name))?;
            let waiting = &mut self.group_mut(inode).waiting;
            if waiting.len() == waiting.capacity() {
                waiting.reserve_exact(waiting.len().max(1));
            }
            waiting.push(entry);
        }
        self.forget_if_complete(inode);
        Ok(())
    }

    fn group_mut(&mut self, inode: Inode) -> &mut Group {
        self.groups.get_mut(&inode).expect("a group of this inode")
    }

    fn written(
        &mut self,
        tree: &mut tree::Tree,
        inode: Inode,
        links: u64,
        name: &str,
    ) -> Result<()> {
        let group = self.group(inode, links)?;
        let waiting = std::mem::take(&mut group.waiting);
        let first = group.written.is_none();
        if first {
            group.written = Some(name.into());
            self.charge(name_cost(name))?;
        }
        for entry in waiting {
            self.memory -= deferred_cost(&entry.name);
            tree.hard_link(&entry.name, name)?;
        }
        if first && !self.forget_if_complete(inode) {
            tree.keep_for_links(name)?;
        }
        Ok(())
    }

    fn finish(self, tree: &mut tree::Tree) -> Result<()> {
        let mut unwritten: Vec<Group> = self
            .groups
            .into_values()
            .filter(|group| group.written.is_none() && !group.waiting.is_empty())
            .collect();
        unwritten.sort_unstable_by_key(|group| group.first_seen);
        for group in unwritten {
            let mut entries = group.waiting.into_iter();
            let Some(first) = entries.next() else {
                continue;
            };
            tree.file(
                &first.name,
                0,
                first.mode,
                first.modified,
                &mut std::io::empty(),
            )?;
            for entry in entries {
                tree.hard_link(&entry.name, &first.name)?;
            }
        }
        Ok(())
    }
}

fn utf8(bytes: Vec<u8>, what: &str) -> Result<String> {
    String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("cpio {what} is not UTF-8; {}", tree::BACKEND_HINT))
}

fn entry(
    tree: &mut tree::Tree,
    input: &mut Input,
    form: Form,
    header: &Header,
    name: &str,
    links: &mut HardLinks,
) -> Result<()> {
    let mode = header.mode & 0o777;
    let modified = unix_time(header.modified);
    let size = header.size;
    match header.mode & TYPE_MASK {
        DIRECTORY => {
            ensure!(size == 0, "cpio directory {name:?} has data");
            tree.directory(name, mode, modified)
        }
        REGULAR if matches!(form, Form::Newc { .. }) && header.links > 1 && size == 0 => {
            let deferred = Deferred {
                name: name.to_owned(),
                mode,
                modified,
            };
            links.defer(tree, header.inode, header.links, deferred)
        }
        REGULAR => {
            let mut data = Summed {
                inner: (&mut input.reader).take(size),
                sum: 0,
            };
            tree.file(name, size, mode, modified, &mut data)?;
            if form == (Form::Newc { crc: true }) {
                ensure!(
                    data.sum == header.check,
                    "cpio entry {name:?} fails its checksum"
                );
            }
            if matches!(form, Form::Newc { .. }) && header.links > 1 {
                links.written(tree, header.inode, header.links, name)?;
            }
            Ok(())
        }
        SYMLINK => {
            ensure!(
                size <= crate::MAX_LINK_TARGET,
                "cpio symlink {name:?} has a target longer than {} bytes",
                crate::MAX_LINK_TARGET
            );
            let mut target = vec![0; size as usize];
            input.exact(&mut target, "symlink target")?;
            tree.symlink(name, utf8(target, "symlink target")?)
        }
        FIFO | CHARACTER_DEVICE | BLOCK_DEVICE | SOCKET => {
            bail!("cpio entry {name:?} is a device, FIFO or socket, which is not extracted")
        }
        other => bail!("cpio entry {name:?} has the unknown type {other:o}"),
    }
}

fn parse(tree: &mut tree::Tree, input: &mut Input, form: Form) -> Result<()> {
    let mut links = HardLinks::default();
    let mut headers = 0usize;
    loop {
        tree.check()?;
        let header = input.header(form)?;
        headers += 1;
        ensure!(
            headers <= tree::MAX_ENTRIES,
            "cpio has more than {} entries",
            tree::MAX_ENTRIES
        );
        ensure!(
            (1..=tree::MAX_NAME).contains(&header.name_size),
            "cpio name size {} is not between 1 and {}",
            header.name_size,
            tree::MAX_NAME
        );
        let mut name = vec![0; header.name_size as usize];
        input.exact(&mut name, "name")?;
        input.skip(
            padding(
                form.header_length() as u64 + header.name_size,
                form.alignment(),
            ),
            "name",
        )?;
        ensure!(name.pop() == Some(0), "cpio name has no NUL terminator");
        if name == TRAILER {
            ensure!(header.size == 0, "cpio TRAILER!!! entry has data");
            links.finish(tree)?;
            return input.only_zeros_remain();
        }
        let name = utf8(name, "name")?;
        entry(tree, input, form, &header, &name, &mut links)?;
        input.skip(padding(header.size, form.alignment()), "entry")?;
    }
}

/// Verify and optionally extract a cpio archive into a new directory. It
/// restores files, directories, hard links, Unix modes, modification times
/// and symlinks that stay inside the destination.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut input = Input {
        reader: BufReader::with_capacity(COPY_BUFFER, File::open(path)?),
    };
    let mut magic = [0u8; 6];
    input.exact(&mut magic, "header")?;
    let form = Form::of(&magic).context("not a cpio archive")?;
    input.reader = BufReader::with_capacity(COPY_BUFFER, File::open(path)?);
    tree::extract("cpio", path, destination, options, |tree| {
        parse(tree, &mut input, form)
    })
}

pub(crate) mod tree {
    use super::ReadOptions;
    use crate::{Entry, progress::Tracker, safe_path};
    use anyhow::{Context, Result, bail, ensure};
    use std::{
        collections::{HashMap, HashSet},
        fs,
        io::{Read, Write},
        path::Path,
        time::SystemTime,
    };

    pub(crate) const MAX_ENTRIES: usize = 1_000_000;
    pub(crate) const MAX_NAME: u64 = 8192;
    const MAX_METADATA: usize = 64 * 1024 * 1024;
    const COPY_BUFFER: usize = 1024 * 1024;
    const LINKS_PER_REPORT: usize = 1024;
    pub(crate) const BACKEND_HINT: &str = "pass --zip-engine backend to read it with 7-Zip";

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
            tree.publish(staging)?;
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
            let (name, _) = checked_name(raw)?;
            if let Some((node @ Node::File, _)) = self.nodes.get_mut(&name) {
                *node = Node::LinkedFile;
            }
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
                (Some(Node::Directory), Node::Directory) | (Some(Node::File), Node::File) => {
                    Ok(true)
                }
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
            data: &mut dyn Read,
            size: u64,
            counted: bool,
            mut sink: impl FnMut(&[u8]) -> Result<()>,
        ) -> Result<()> {
            let mut remaining = size;
            while remaining > 0 {
                let want = remaining.min(self.buffer.len() as u64) as usize;
                let read = data
                    .read(&mut self.buffer[..want])
                    .with_context(|| format!("cannot read the {} archive", self.format))?;
                ensure!(read > 0, "truncated {} entry", self.format);
                sink(&self.buffer[..read])?;
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
            let dots = leading_dots(raw);
            let name = entry_name(raw).trim_end_matches('/');
            if name.is_empty() || name == "." {
                return Ok(());
            }
            safe_path(name)?;
            let name = name.to_owned();
            let replaces = self.claim(&name, Node::Directory)?;
            let Some((placed, _)) = self.place(&name, dots, true, replaces)? else {
                return Ok(());
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
                name: placed,
                size: 0,
                directory: true,
            });
            Ok(())
        }

        pub(crate) fn file(
            &mut self,
            raw: &str,
            size: u64,
            mode: u32,
            modified: Option<SystemTime>,
            data: &mut dyn Read,
        ) -> Result<()> {
            let (name, dots) = checked_name(raw)?;
            let mode = self.metadata.file_mode(Some(mode)).unwrap_or(mode);
            let replaces = self.claim(&name, Node::File)?;
            let Some((placed, replaces)) = self.place(&name, dots, false, replaces)? else {
                return self.copy(data, size, false, |_| Ok(()));
            };
            self.budget.charge(size)?;
            let format = self.format;
            match self.root {
                None => self.copy(data, size, true, |_| Ok(()))?,
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
                    self.copy(data, size, true, |bytes| {
                        file.write_all(bytes)
                            .with_context(|| format!("cannot write {format} entry {placed:?}"))
                    })?;
                    if let Some(time) = self.metadata.time(modified) {
                        crate::set_modified_best_effort(&file, time);
                    }
                }
            }
            self.entries.push(Entry {
                name: placed,
                size,
                directory: false,
            });
            self.tracker.file_done()
        }

        pub(crate) fn symlink(&mut self, raw: &str, target: String) -> Result<()> {
            let (name, dots) = checked_name(raw)?;
            crate::ensure_link_stays_inside(self.format, &name, &target)?;
            self.charge_name(target.len())?;
            self.claim(&name, Node::Symlink)?;
            let Some((placed, _)) = self.place(&name, dots, false, false)? else {
                return Ok(());
            };
            crate::ensure_link_stays_inside(self.format, &placed, &target)?;
            self.create_parent(&placed)?;
            self.symlinks.push((placed.clone(), target));
            self.entries.push(Entry {
                name: placed,
                size: 0,
                directory: false,
            });
            self.tracker.file_done()
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

        fn publish(&self, staging: &crate::staging::Staging) -> Result<()> {
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
            staging.publish(&directories)
        }
    }
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn every_field_value_reads_back_from_its_spelling() {
        for value in (0..=0xFFFFu64).chain([0xFFFF_FFFF, 0x1234_5678, 0o77_777_777_777]) {
            if value <= 0xFFFF_FFFF {
                assert_eq!(hex(format!("{value:08X}").as_bytes()).unwrap(), value);
                assert_eq!(hex(format!("{value:08x}").as_bytes()).unwrap(), value);
            }
            assert_eq!(octal(format!("{value:011o}").as_bytes()).unwrap(), value);
        }
        for bad in [&b""[..], b" 0000001", b"0000000g", b"-0000001", b"+0000001"] {
            assert!(hex(bad).is_err(), "{bad:?}");
        }
        for bad in [&b""[..], b"000008", b" 00001", b"00001 "] {
            assert!(octal(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn every_name_and_data_length_pads_to_its_form() {
        for length in 0..64u64 {
            for alignment in [1, 2, 4] {
                let padded = length + padding(length, alignment);
                assert_eq!(padded % alignment, 0);
                assert!(padded - length < alignment);
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(10)]
    fn hex_and_octal_fields_never_panic_and_read_back_their_values() {
        let bytes: [u8; 8] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= 8);
        let _ = hex(&bytes[..length]);
        let _ = octal(&bytes[..length]);
        let value: u32 = kani::any();
        let mut field = [0u8; 8];
        for (index, byte) in field.iter_mut().enumerate() {
            let digit = (value >> (4 * (7 - index))) & 0xF;
            *byte = if digit < 10 {
                b'0' + digit as u8
            } else {
                b'A' + (digit - 10) as u8
            };
        }
        assert!(matches!(hex(&field), Ok(read) if read == u64::from(value)));
    }

    #[kani::proof]
    #[kani::unwind(12)]
    fn eleven_octal_digits_read_back_every_value_below_their_limit() {
        let value: u64 = kani::any();
        kani::assume(value < 8u64.pow(11));
        let mut field = [0u8; 11];
        for (index, byte) in field.iter_mut().enumerate() {
            *byte = b'0' + ((value >> (3 * (10 - index))) & 7) as u8;
        }
        assert!(matches!(octal(&field), Ok(read) if read == value));
    }

    #[kani::proof]
    fn padding_aligns_every_length_of_every_form() {
        let length: u64 = kani::any();
        kani::assume(length <= u64::from(u32::MAX) + NEWC_HEADER as u64);
        for form in [
            Form::Binary { big_endian: false },
            Form::Odc,
            Form::Newc { crc: false },
        ] {
            let alignment = form.alignment();
            let padded = length + padding(length, alignment);
            assert!(padded % alignment == 0 && padded - length < alignment);
        }
    }

    #[kani::proof]
    #[kani::unwind(14)]
    fn newc_headers_never_panic_and_fit_their_fields() {
        let bytes: [u8; NEWC_HEADER] = kani::any();
        let crc: bool = kani::any();
        if let Ok(header) = newc_header(&bytes, crc) {
            assert!(header.size <= u64::from(u32::MAX));
            assert!(header.name_size <= u64::from(u32::MAX));
            assert!(padding(NEWC_HEADER as u64 + header.name_size, 4) < 4);
            assert!(padding(header.size, 4) < 4);
        }
    }

    #[kani::proof]
    #[kani::unwind(12)]
    fn odc_headers_never_panic_and_fit_their_fields() {
        let bytes: [u8; ODC_HEADER] = kani::any();
        if let Ok(header) = odc_header(&bytes) {
            assert!(header.size < 8u64.pow(11));
            assert!(header.modified < 8u64.pow(11));
            assert!(header.name_size < 8u64.pow(6));
        }
    }

    #[kani::proof]
    fn binary_headers_never_panic_and_fit_their_fields() {
        let bytes: [u8; BINARY_HEADER] = kani::any();
        let big_endian: bool = kani::any();
        if let Ok(header) = binary_header(&bytes, big_endian) {
            assert!(header.size <= u64::from(u32::MAX));
            assert!(header.modified <= u64::from(u32::MAX));
            assert!(header.name_size <= u64::from(u16::MAX));
            assert!(header.mode <= u32::from(u16::MAX));
        }
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;

    fn deferred(name: &str) -> Deferred {
        Deferred {
            name: name.to_owned(),
            mode: 0o644,
            modified: None,
        }
    }

    fn inode(number: u64) -> Inode {
        Inode {
            number,
            device: (0, 0),
        }
    }

    #[test]
    fn a_group_that_only_carries_data_costs_at_most_200_bytes() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join("unused.cpio");
        tree::extract("cpio", &archive, None, ReadOptions::default(), |tree| {
            let mut links = HardLinks::default();
            for number in 0..1_000u64 {
                let name = format!("f{number:07}");
                tree.file(&name, 1, 0o644, None, &mut &b"x"[..])?;
                links.written(tree, inode(number), 2, &name)?;
            }
            assert!(links.memory <= 1_000 * 200, "{}", links.memory);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn complete_pairs_give_their_link_memory_back() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join("unused.cpio");
        tree::extract("cpio", &archive, None, ReadOptions::default(), |tree| {
            let mut links = HardLinks {
                memory: LINK_MEMORY - 64 * 1024,
                ..HardLinks::default()
            };
            for number in 0..20_000u64 {
                let first = format!("{number:06}a");
                let second = format!("{number:06}b");
                links.defer(tree, inode(number), 2, deferred(&first))?;
                tree.file(&second, 1, 0o644, None, &mut &b"x"[..])?;
                links.written(tree, inode(number), 2, &second)?;
            }
            links.finish(tree)
        })
        .unwrap();
    }
}
