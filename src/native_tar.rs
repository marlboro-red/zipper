//! In-process extraction of plain, gzip, zstd and xz tar archives, published
//! only after the whole archive reads cleanly. See
//! `docs/reference/tar-reader.md` for what it accepts and restores.
use crate::{
    Entry,
    counters::{self, Counter},
    parent, safe_path,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::Path,
};

const BLOCK: usize = 512;
const COPY_BUFFER: usize = 1024 * 1024;
const MAX_EXTENSION: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_METADATA: usize = 64 * 1024 * 1024;
const READ_ERROR: &str = "cannot read the tar stream";
const BACKEND_HINT: &str = "pass --zip-engine backend to read it with 7-Zip";

/// The compression around a tar stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Zstd,
    Xz,
}

/// Tar extraction limits.
#[derive(Clone, Copy, Default)]
pub struct ReadOptions {
    /// Cap on declared output bytes. Without it, extraction must fit in the
    /// destination filesystem's free space.
    pub max_output_bytes: Option<u64>,
}

/// Returns the compression of a tar archive, or `None` when the file does not
/// start with a tar header, plain or inside gzip, zstd or xz.
pub fn sniff(path: &Path) -> Result<Option<Compression>> {
    let mut magic = [0u8; 6];
    let read = File::open(path)?.read(&mut magic)?;
    let compression = match &magic[..read] {
        [0x1f, 0x8b, ..] => Compression::Gzip,
        [0x28, 0xb5, 0x2f, 0xfd, ..] | [0x50..=0x5f, 0x2a, 0x4d, 0x18, ..] => Compression::Zstd,
        [0xfd, b'7', b'z', b'X', b'Z', 0x00] => Compression::Xz,
        _ => Compression::None,
    };
    let Ok(mut stream) = decoder(path, compression) else {
        return Ok(None);
    };
    let mut first = [0u8; BLOCK];
    if stream.read_exact(&mut first).is_err() {
        return Ok(None);
    }
    let tar = first != [0; BLOCK] && (checksum_matches(&first) || &first[257..262] == b"ustar");
    Ok(tar.then_some(compression))
}

fn decoder(path: &Path, compression: Compression) -> Result<Box<dyn Read + Send>> {
    let file = BufReader::with_capacity(COPY_BUFFER, File::open(path)?);
    Ok(match compression {
        Compression::None => Box::new(file),
        Compression::Gzip => Box::new(flate2::bufread::MultiGzDecoder::new(file)),
        Compression::Zstd => Box::new(zstd::stream::read::Decoder::with_buffer(file)?),
        Compression::Xz => Box::new(liblzma::bufread::XzDecoder::new_multi_decoder(file)),
    })
}

fn checksum_matches(block: &[u8; BLOCK]) -> bool {
    let Ok(stored) = octal(&block[148..156]) else {
        return false;
    };
    let field = 148..156;
    let (mut unsigned, mut signed) = (0u64, 0i64);
    for (index, &byte) in block.iter().enumerate() {
        let byte = if field.contains(&index) { b' ' } else { byte };
        unsigned += u64::from(byte);
        signed += i64::from(byte as i8);
    }
    stored == unsigned || i64::try_from(stored) == Ok(signed)
}

fn octal(field: &[u8]) -> Result<u64> {
    let digits = field.trim_ascii_start();
    let end = digits
        .iter()
        .position(|&byte| byte == 0 || byte == b' ')
        .unwrap_or(digits.len());
    ensure!(
        digits[end..].iter().all(|&byte| byte == 0 || byte == b' '),
        "tar numeric field has bytes after its terminator"
    );
    digits[..end].iter().try_fold(0u64, |value, &digit| {
        ensure!(
            (b'0'..=b'7').contains(&digit),
            "tar numeric field is not octal"
        );
        value
            .checked_mul(8)
            .map(|value| value + u64::from(digit - b'0'))
            .context("tar numeric field overflows")
    })
}

fn number(field: &[u8]) -> Result<i64> {
    match field[0] {
        0x80 => field[1..].iter().try_fold(0i64, |value, &byte| {
            value
                .checked_mul(256)
                .map(|value| value | i64::from(byte))
                .context("tar base-256 field overflows")
        }),
        0xff => {
            let mut value = -1i64;
            for &byte in &field[1..] {
                ensure!(value >= i64::MIN >> 8, "tar base-256 field overflows");
                value = (value << 8) | i64::from(byte);
            }
            Ok(value)
        }
        _ => i64::try_from(octal(field)?).context("tar numeric field overflows"),
    }
}

fn size_field(field: &[u8]) -> Result<u64> {
    u64::try_from(number(field)?).context("tar entry size is negative")
}

fn text(field: &[u8]) -> &[u8] {
    &field[..field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len())]
}

fn utf8<'a>(bytes: &'a [u8], what: &str) -> Result<&'a str> {
    std::str::from_utf8(bytes).with_context(|| format!("tar {what} is not UTF-8; {BACKEND_HINT}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    V7,
    Ustar,
    Gnu,
}

fn format(block: &[u8; BLOCK]) -> Result<Format> {
    match (&block[257..263], &block[263..265]) {
        (b"ustar\0", _) => Ok(Format::Ustar),
        (b"ustar ", b" \0") => Ok(Format::Gnu),
        (magic, version) if magic.iter().chain(version).all(|&byte| byte == 0) => Ok(Format::V7),
        _ => bail!("tar header has an unknown magic"),
    }
}

#[derive(Clone, Copy)]
struct Time {
    seconds: i64,
    nanos: u32,
}

impl Time {
    fn system_time(self) -> Option<std::time::SystemTime> {
        let whole = std::time::Duration::from_secs(self.seconds.unsigned_abs());
        let epoch = std::time::UNIX_EPOCH;
        let seconds = if self.seconds >= 0 {
            epoch.checked_add(whole)
        } else {
            epoch.checked_sub(whole)
        };
        seconds?.checked_add(std::time::Duration::from_nanos(self.nanos.into()))
    }
}

fn pax_time(value: &str) -> Result<Time> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let seconds: i64 = whole.parse().context("tar pax mtime is not a number")?;
    ensure!(
        fraction.bytes().all(|byte| byte.is_ascii_digit()),
        "tar pax mtime is not a number"
    );
    let digits: String = fraction
        .chars()
        .chain(std::iter::repeat('0'))
        .take(9)
        .collect();
    let mut nanos: u32 = digits.parse()?;
    let mut seconds = seconds;
    if whole.starts_with('-') && nanos > 0 {
        seconds = seconds.checked_sub(1).context("tar pax mtime overflows")?;
        nanos = 1_000_000_000 - nanos;
    }
    Ok(Time { seconds, nanos })
}

#[derive(Default)]
struct Extensions {
    pax: bool,
    path: Option<String>,
    linkpath: Option<String>,
    size: Option<u64>,
    mtime: Option<Time>,
    long_name: Option<Vec<u8>>,
    long_link: Option<Vec<u8>>,
}

impl Extensions {
    fn is_empty(&self) -> bool {
        !self.pax && self.long_name.is_none() && self.long_link.is_none()
    }
}

fn pax_records(data: &[u8]) -> Result<Vec<(&str, &[u8])>> {
    let mut records = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .position(|&byte| byte == b' ')
            .context("tar pax record has no length")?;
        let length: usize = utf8(&rest[..space], "pax record length")?
            .parse()
            .context("tar pax record length is not a number")?;
        ensure!(
            length > space + 1 && length <= rest.len() && rest[length - 1] == b'\n',
            "tar pax record has a wrong length"
        );
        let record = &rest[space + 1..length - 1];
        let equals = record
            .iter()
            .position(|&byte| byte == b'=')
            .context("tar pax record has no key")?;
        let key = utf8(&record[..equals], "pax key")?;
        ensure!(!key.is_empty(), "tar pax record has an empty key");
        records.push((key, &record[equals + 1..]));
        rest = &rest[length..];
    }
    Ok(records)
}

fn ignored_pax_key(key: &str) -> Result<bool> {
    ensure!(
        !key.starts_with("GNU.sparse."),
        "tar sparse files are not supported; {BACKEND_HINT}"
    );
    Ok(matches!(
        key,
        "atime" | "ctime" | "uid" | "gid" | "uname" | "gname" | "comment" | "charset"
    ) || key.contains('.'))
}

fn apply_pax(extensions: &mut Extensions, data: &[u8]) -> Result<()> {
    ensure!(!extensions.pax, "tar has two pax headers for one entry");
    extensions.pax = true;
    let mut seen = HashSet::new();
    for (key, value) in pax_records(data)? {
        ensure!(seen.insert(key), "tar pax header repeats {key:?}");
        match key {
            "path" => extensions.path = Some(utf8(value, "pax path")?.to_owned()),
            "linkpath" => extensions.linkpath = Some(utf8(value, "pax linkpath")?.to_owned()),
            "size" => {
                let value = utf8(value, "pax size")?;
                ensure!(
                    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
                    "tar pax size is not a number"
                );
                extensions.size = Some(value.parse().context("tar pax size overflows")?);
            }
            "mtime" => extensions.mtime = Some(pax_time(utf8(value, "pax mtime")?)?),
            "hdrcharset" => ensure!(
                value == b"ISO-IR 10646 2000 UTF-8",
                "tar pax header declares binary names; {BACKEND_HINT}"
            ),
            _ => ensure!(
                ignored_pax_key(key)?,
                "tar pax key {key:?} is not supported; {BACKEND_HINT}"
            ),
        }
    }
    Ok(())
}

fn check_global_pax(data: &[u8]) -> Result<()> {
    for (key, _) in pax_records(data)? {
        ensure!(
            ignored_pax_key(key)?,
            "tar global pax key {key:?} is not supported; {BACKEND_HINT}"
        );
    }
    Ok(())
}

struct Input {
    reader: Box<dyn Read + Send>,
    buffer: Vec<u8>,
}

impl Input {
    fn block(&mut self) -> Result<Option<[u8; BLOCK]>> {
        let mut block = [0u8; BLOCK];
        let mut filled = 0;
        while filled < BLOCK {
            let read = self.reader.read(&mut block[filled..]).context(READ_ERROR)?;
            if read == 0 {
                ensure!(filled == 0, "truncated tar header");
                return Ok(None);
            }
            filled += read;
        }
        counters::add(Counter::ArchiveBytesRead, BLOCK as u64);
        Ok(Some(block))
    }

    fn copy(&mut self, size: u64, mut sink: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        let mut remaining = padded;
        while remaining > 0 {
            let want = remaining.min(self.buffer.len() as u64) as usize;
            let read = self
                .reader
                .read(&mut self.buffer[..want])
                .context(READ_ERROR)?;
            ensure!(read > 0, "truncated tar entry");
            counters::add(Counter::ArchiveReads, 1);
            counters::add(Counter::ArchiveBytesRead, read as u64);
            let data_end = size.saturating_sub(padded - remaining).min(read as u64) as usize;
            if data_end > 0 {
                sink(&self.buffer[..data_end])?;
            }
            remaining -= read as u64;
        }
        Ok(())
    }

    fn data(&mut self, size: u64, what: &str) -> Result<Vec<u8>> {
        ensure!(size <= MAX_EXTENSION, "tar {what} is larger than 1 MiB");
        let mut data = Vec::with_capacity(size as usize);
        self.copy(size, |bytes| {
            data.extend_from_slice(bytes);
            Ok(())
        })?;
        Ok(data)
    }

    fn finish(&mut self) -> Result<()> {
        loop {
            let read = self.reader.read(&mut self.buffer).context(READ_ERROR)?;
            if read == 0 {
                return Ok(());
            }
            ensure!(
                self.buffer[..read].iter().all(|&byte| byte == 0),
                "tar has data after its end-of-archive marker"
            );
        }
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
    modified: Option<Time>,
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

struct Extraction<'a> {
    root: Option<&'a Path>,
    nodes: HashMap<String, Node>,
    metadata_bytes: usize,
    created_directories: HashSet<String>,
    directories: Vec<DirectoryMetadata>,
    directory_index: HashMap<String, usize>,
    hard_links: Vec<(String, String)>,
    symlinks: Vec<(String, String)>,
    entries: Vec<Entry>,
    budget: Budget,
}

fn entry_name(raw: &str) -> &str {
    let mut name = raw;
    while let Some(rest) = name.strip_prefix("./") {
        name = rest;
    }
    name
}

impl Extraction<'_> {
    fn charge_name(&mut self, bytes: usize) -> Result<()> {
        self.metadata_bytes += bytes;
        ensure!(
            self.metadata_bytes <= MAX_METADATA,
            "tar names and link targets exceed 64 MiB"
        );
        Ok(())
    }

    fn insert_node(&mut self, name: &str, node: Node) -> Result<Option<Node>> {
        ensure!(
            self.nodes.len() < MAX_ENTRIES,
            "tar has more than {MAX_ENTRIES} entries"
        );
        self.charge_name(name.len())?;
        Ok(self.nodes.insert(name.to_owned(), node))
    }

    fn claim(&mut self, name: &str, node: Node) -> Result<bool> {
        let mut ancestor = name;
        while let Some(slash) = ancestor.rfind('/') {
            ancestor = &ancestor[..slash];
            match self.nodes.get(ancestor) {
                Some(Node::ImpliedDirectory | Node::Directory) => break,
                Some(_) => bail!("tar entry {name:?} is inside a file or link"),
                None => {
                    self.insert_node(ancestor, Node::ImpliedDirectory)?;
                }
            }
        }
        match (self.insert_node(name, node)?, node) {
            (None, _) | (Some(Node::ImpliedDirectory), Node::Directory) => Ok(false),
            (Some(Node::Directory), Node::Directory) | (Some(Node::File), Node::File) => Ok(true),
            (Some(Node::ImpliedDirectory), _) => bail!("tar entry {name:?} is also a directory"),
            (Some(Node::LinkedFile), Node::File) => {
                bail!("tar replaces {name:?}, which an earlier hard link names")
            }
            _ => bail!("tar has two entries of different kinds named {name:?}"),
        }
    }

    fn create_parent(&mut self, name: &str) -> Result<()> {
        let (Some(root), Some(slash)) = (self.root, name.rfind('/')) else {
            return Ok(());
        };
        let directory = &name[..slash];
        if self.created_directories.insert(directory.to_owned()) {
            fs::create_dir_all(root.join(directory))?;
            counters::add(Counter::DirectoryCreateCalls, 1);
        }
        Ok(())
    }

    fn write_file(
        &mut self,
        input: &mut Input,
        name: &str,
        size: u64,
        mode: u32,
        modified: Option<Time>,
        replaces: bool,
    ) -> Result<()> {
        self.budget.charge(size)?;
        let Some(root) = self.root else {
            return input.copy(size, |_| Ok(()));
        };
        self.create_parent(name)?;
        if replaces {
            fs::remove_file(root.join(name))?;
        }
        let mut file = crate::create_new_file(&root.join(name), mode).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow::anyhow!(
                    "tar entry {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
                )
            } else {
                anyhow::Error::from(error).context(format!("cannot create tar entry {name:?}"))
            }
        })?;
        counters::add(Counter::FilesCreated, 1);
        input.copy(size, |bytes| {
            file.write_all(bytes)?;
            counters::add(Counter::OutputWrites, 1);
            counters::add(Counter::OutputBytesWritten, bytes.len() as u64);
            Ok(())
        })?;
        if let Some(time) = modified.and_then(Time::system_time) {
            crate::set_modified_best_effort(&file, time);
        }
        Ok(())
    }

    fn create_links(&self) -> Result<()> {
        let Some(root) = self.root else {
            return Ok(());
        };
        for (name, target) in &self.hard_links {
            fs::hard_link(root.join(target), root.join(name)).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    anyhow::anyhow!(
                        "tar hard link {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
                    )
                } else {
                    anyhow::Error::from(error).context(format!("cannot create tar hard link {name:?}"))
                }
            })?;
        }
        #[cfg(not(unix))]
        ensure!(
            self.symlinks.is_empty(),
            "tar symlinks are not supported on this platform"
        );
        #[cfg(unix)]
        for (name, target) in &self.symlinks {
            std::os::unix::fs::symlink(target, root.join(name)).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    anyhow::anyhow!(
                        "tar symlink {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
                    )
                } else {
                    anyhow::Error::from(error).context(format!("cannot create tar symlink {name:?}"))
                }
            })?;
            counters::add(Counter::SymlinksCreated, 1);
        }
        Ok(())
    }

    fn member(
        &mut self,
        input: &mut Input,
        block: &[u8; BLOCK],
        extensions: Extensions,
    ) -> Result<()> {
        let format = format(block)?;
        let kind = block[156];
        let raw_name = match (&extensions.path, &extensions.long_name) {
            (Some(_), Some(_)) => bail!("tar entry has both a pax path and a GNU long name"),
            (Some(path), None) => path.clone(),
            (None, Some(long)) => utf8(text(long), "long name")?.to_owned(),
            (None, None) => {
                let name = text(&block[..100]);
                let prefix = text(&block[345..500]);
                if format == Format::Ustar && !prefix.is_empty() {
                    format!("{}/{}", utf8(prefix, "name")?, utf8(name, "name")?)
                } else {
                    utf8(name, "name")?.to_owned()
                }
            }
        };
        let raw_target = match (&extensions.linkpath, &extensions.long_link) {
            (Some(_), Some(_)) => bail!("tar entry has both a pax linkpath and a GNU long link"),
            (Some(path), None) => path.clone(),
            (None, Some(long)) => utf8(text(long), "long link")?.to_owned(),
            (None, None) => utf8(text(&block[157..257]), "link target")?.to_owned(),
        };
        let size = match extensions.size {
            Some(size) => size,
            None => size_field(&block[124..136])?,
        };
        let mode = (octal(&block[100..108])? & 0o777) as u32;
        let modified = match extensions.mtime {
            Some(time) => Some(time),
            None => Some(Time {
                seconds: number(&block[136..148])?,
                nanos: 0,
            }),
        };
        let mut name = entry_name(&raw_name);
        let directory = match kind {
            b'5' => true,
            b'0' | 0 | b'7' if name.ends_with('/') => {
                ensure!(size == 0, "tar file entry {name:?} ends with a slash");
                true
            }
            _ => false,
        };
        if directory {
            name = name.trim_end_matches('/');
            if name.is_empty() || name == "." {
                ensure!(size == 0, "tar directory entry has data");
                return Ok(());
            }
        }
        safe_path(name)?;
        let name = name.to_owned();
        match kind {
            _ if directory => {
                ensure!(size == 0, "tar directory {name:?} has data");
                self.claim(&name, Node::Directory)?;
                if self.root.is_some() {
                    self.create_parent(&format!("{name}/"))?;
                }
                let metadata = DirectoryMetadata {
                    name: name.clone(),
                    mode,
                    modified,
                };
                match self.directory_index.get(&name) {
                    Some(&index) => self.directories[index] = metadata,
                    None => {
                        self.directory_index
                            .insert(name.clone(), self.directories.len());
                        self.directories.push(metadata);
                    }
                }
            }
            b'0' | 0 | b'7' => {
                let replaces = self.claim(&name, Node::File)?;
                self.write_file(input, &name, size, mode, modified, replaces)?;
                self.entries.push(Entry {
                    name,
                    size,
                    directory: false,
                });
                return Ok(());
            }
            b'1' => {
                ensure!(size == 0, "tar hard link {name:?} has data");
                let target = entry_name(&raw_target).to_owned();
                safe_path(&target)?;
                ensure!(
                    matches!(
                        self.nodes.get(&target),
                        Some(Node::File | Node::LinkedFile | Node::HardLink)
                    ),
                    "tar hard link {name:?} does not name an earlier file"
                );
                self.charge_name(target.len())?;
                if let Some(node @ Node::File) = self.nodes.get_mut(&target) {
                    *node = Node::LinkedFile;
                }
                self.claim(&name, Node::HardLink)?;
                self.create_parent(&name)?;
                self.hard_links.push((name.clone(), target));
            }
            b'2' => {
                ensure!(size == 0, "tar symlink {name:?} has data");
                ensure!(
                    cfg!(unix),
                    "tar symlinks are not supported on this platform; {BACKEND_HINT}"
                );
                crate::ensure_link_stays_inside("tar", &name, &raw_target)?;
                self.charge_name(raw_target.len())?;
                self.claim(&name, Node::Symlink)?;
                self.create_parent(&name)?;
                self.symlinks.push((name.clone(), raw_target));
            }
            b'3' | b'4' | b'6' => {
                bail!("tar entry {name:?} is a device or FIFO, which is not extracted")
            }
            other => bail!(
                "tar entry type {:?} is not supported; {BACKEND_HINT}",
                char::from(other)
            ),
        }
        self.entries.push(Entry {
            name,
            size: 0,
            directory,
        });
        Ok(())
    }
}

/// Verify and optionally extract a plain or compressed tar archive into a new
/// directory. It restores files, directories, hard links, Unix modes,
/// modification times and symlinks that stay inside the destination.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let compression = sniff(path)?.context("not a tar archive")?;
    let staging = match destination {
        Some(destination) => {
            ensure!(
                fs::symlink_metadata(destination).is_err(),
                "extraction destination must not exist"
            );
            Some(tempfile::tempdir_in(parent(destination))?)
        }
        None => None,
    };
    let free = match (destination, options.max_output_bytes) {
        (Some(destination), None) => {
            let folder = parent(destination);
            crate::available_bytes(folder)
                .with_context(|| format!("cannot measure free space at {}", folder.display()))?
        }
        _ => u64::MAX,
    };
    let mut extraction = Extraction {
        root: staging.as_ref().map(|temp| temp.path()),
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
    };
    let mut input = Input {
        reader: decoder(path, compression)?,
        buffer: vec![0; COPY_BUFFER],
    };
    let mut extensions = Extensions::default();
    loop {
        let block = input
            .block()?
            .context("tar ends without its end-of-archive marker")?;
        if block == [0; BLOCK] {
            ensure!(extensions.is_empty(), "tar ends after an extended header");
            if let Some(next) = input.block()? {
                ensure!(next == [0; BLOCK], "tar has a header after a zero block");
            }
            input.finish()?;
            break;
        }
        ensure!(checksum_matches(&block), "tar header checksum mismatch");
        let size = size_field(&block[124..136])?;
        match block[156] {
            b'x' => {
                let data = input.data(size, "pax header")?;
                apply_pax(&mut extensions, &data)?;
            }
            b'g' => {
                ensure!(
                    extensions.is_empty(),
                    "tar global header follows an extended header"
                );
                check_global_pax(&input.data(size, "global pax header")?)?;
            }
            b'L' => {
                ensure!(
                    extensions.long_name.is_none(),
                    "tar has two GNU long names for one entry"
                );
                extensions.long_name = Some(input.data(size, "GNU long name")?);
            }
            b'K' => {
                ensure!(
                    extensions.long_link.is_none(),
                    "tar has two GNU long links for one entry"
                );
                extensions.long_link = Some(input.data(size, "GNU long link")?);
            }
            _ => extraction.member(&mut input, &block, std::mem::take(&mut extensions))?,
        }
    }
    extraction.create_links()?;
    if let (Some(temp), Some(destination)) = (&staging, destination) {
        let merged = crate::later_spellings_of_same_directory(
            temp.path(),
            extraction
                .directories
                .iter()
                .map(|directory| Some(directory.name.as_str())),
        )?;
        let directories: Vec<crate::Directory> = extraction
            .directories
            .iter()
            .enumerate()
            .filter(|(index, _)| !merged.contains(index))
            .map(|(_, directory)| crate::Directory {
                name: &directory.name,
                mode: Some(directory.mode),
                modified: directory.modified.and_then(Time::system_time),
            })
            .collect();
        crate::publish(temp.path(), destination, &directories)?;
    }
    Ok(extraction.entries)
}
