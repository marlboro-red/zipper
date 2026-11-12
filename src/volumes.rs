use std::{
    cell::RefCell,
    ffi::OsString,
    fs::{File, Metadata},
    io::{self, BufRead, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const FIRST_VOLUME: u64 = 1;
const GAP_PROBE: u64 = 16;
const REPEAT_MIN_BYTES: u64 = 64;
const REPEAT_SCAN_BYTES: u64 = 256 << 20;
const ZIP64_DISK_PLACEHOLDER: u64 = u16::MAX as u64;

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    volume: u64,
    count: u64,
    last: u64,
}

impl Layout {
    fn new(volume: u64, count: u64, last: u64) -> Option<Self> {
        if volume == 0
            || count == 0
            || last == 0
            || last.checked_sub(volume).is_some_and(|extra| extra >= count)
        {
            return None;
        }
        volume.checked_mul(count - 1)?.checked_add(last)?;
        Some(Self {
            volume,
            count,
            last,
        })
    }

    fn total(&self) -> u64 {
        self.volume * (self.count - 1) + self.last
    }

    fn size(&self, index: u64) -> u64 {
        if index + 1 == self.count {
            self.last
        } else {
            self.volume
        }
    }

    fn locate(&self, offset: u64) -> Option<(u64, u64)> {
        (offset < self.total()).then(|| {
            let index = (offset / self.volume).min(self.count - 1);
            (index, offset - index * self.volume)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

fn name(path: &Path) -> std::path::Display<'_> {
    Path::new(path.file_name().unwrap_or(path.as_os_str())).display()
}

fn open_without_blocking(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
    }
    #[cfg(not(unix))]
    File::open(path)
}

fn open_regular_file(path: &Path, identity: Option<Identity>) -> io::Result<File> {
    let file = open_without_blocking(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot open {}: {error}", path.display()),
        )
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if identity.is_some_and(|identity| Identity::of(&metadata) != identity) {
        return Err(invalid(format!(
            "split volume {} changed while it was read",
            name(path)
        )));
    }
    #[cfg(unix)]
    {
        let flags = rustix::fs::fcntl_getfl(&file)?;
        rustix::fs::fcntl_setfl(&file, flags - rustix::fs::OFlags::NONBLOCK)?;
    }
    Ok(file)
}

struct Numbered {
    folder: PathBuf,
    stem: OsString,
    number: u64,
}

impl Numbered {
    fn parse(path: &Path) -> Option<Self> {
        let digits = path.extension()?.to_str()?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let number: u64 = digits.parse().ok()?;
        let stem = path.file_stem()?;
        if number < FIRST_VOLUME
            || format!("{number:03}") != digits
            || matches!(stem.to_str(), Some("" | "." | ".."))
        {
            return None;
        }
        Some(Self {
            folder: path.parent().unwrap_or(Path::new("")).to_owned(),
            stem: stem.to_owned(),
            number,
        })
    }

    fn path(&self, number: u64) -> PathBuf {
        let mut name = self.stem.clone();
        name.push(format!(".{number:03}"));
        self.folder.join(name)
    }
}

struct ZipNames {
    folder: PathBuf,
    stem: OsString,
    upper: bool,
}

impl ZipNames {
    fn of_last(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if !extension.eq_ignore_ascii_case("zip") {
            return None;
        }
        Self::new(path, extension.starts_with('Z'))
    }

    fn of_volume(path: &Path) -> Option<(Self, u64)> {
        let extension = path.extension()?.to_str()?;
        let digits = extension.strip_prefix(['z', 'Z'])?;
        if digits.len() < 2 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let number: u64 = digits.parse().ok()?;
        if number < FIRST_VOLUME || format!("{number:02}") != digits {
            return None;
        }
        Some((Self::new(path, extension.starts_with('Z'))?, number))
    }

    fn new(path: &Path, upper: bool) -> Option<Self> {
        let stem = path.file_stem()?;
        if matches!(stem.to_str(), Some("" | "." | "..")) {
            return None;
        }
        Some(Self {
            folder: path.parent().unwrap_or(Path::new("")).to_owned(),
            stem: stem.to_owned(),
            upper,
        })
    }

    fn named(&self, extension: &str) -> PathBuf {
        let mut name = self.stem.clone();
        name.push(".");
        name.push(if self.upper {
            extension.to_ascii_uppercase()
        } else {
            extension.to_owned()
        });
        self.folder.join(name)
    }

    fn volume(&self, number: u64) -> PathBuf {
        self.named(&format!("z{number:02}"))
    }

    fn last(&self) -> PathBuf {
        self.named("zip")
    }
}

enum Names {
    Numbered(Numbered),
    Zip(ZipNames),
}

struct Uneven {
    starts: Vec<u64>,
}

impl Uneven {
    fn new(lengths: impl Iterator<Item = u64>) -> Option<Self> {
        let mut starts = vec![0u64];
        for length in lengths {
            let end = starts.last()?.checked_add(length)?;
            starts.push(end);
        }
        Some(Self { starts })
    }

    fn total(&self) -> u64 {
        self.starts.last().copied().unwrap_or(0)
    }

    fn size(&self, index: u64) -> u64 {
        let index = index as usize;
        self.starts[index + 1] - self.starts[index]
    }

    fn locate(&self, offset: u64) -> Option<(u64, u64)> {
        (offset < self.total()).then(|| {
            let index = self.starts.partition_point(|&start| start <= offset) - 1;
            (index as u64, offset - self.starts[index])
        })
    }
}

enum Spans {
    Even(Layout),
    Uneven(Uneven),
}

impl Spans {
    fn total(&self) -> u64 {
        match self {
            Self::Even(layout) => layout.total(),
            Self::Uneven(uneven) => uneven.total(),
        }
    }

    fn size(&self, index: u64) -> u64 {
        match self {
            Self::Even(layout) => layout.size(index),
            Self::Uneven(uneven) => uneven.size(index),
        }
    }

    fn locate(&self, offset: u64) -> Option<(u64, u64)> {
        match self {
            Self::Even(layout) => layout.locate(offset),
            Self::Uneven(uneven) => uneven.locate(offset),
        }
    }
}

pub(crate) struct VolumeSet {
    names: Names,
    identities: Vec<Identity>,
    spans: Spans,
}

impl VolumeSet {
    fn count(&self) -> u64 {
        self.identities.len() as u64
    }

    fn path(&self, index: u64) -> PathBuf {
        match &self.names {
            Names::Numbered(numbered) => numbered.path(FIRST_VOLUME + index),
            Names::Zip(zip) if index + 1 == self.count() => zip.last(),
            Names::Zip(zip) => zip.volume(FIRST_VOLUME + index),
        }
    }

    fn even(&self) -> Option<&Layout> {
        match &self.spans {
            Spans::Even(layout) => Some(layout),
            Spans::Uneven(_) => None,
        }
    }

    fn open_volume(&self, index: u64) -> io::Result<File> {
        open_regular_file(&self.path(index), Some(self.identities[index as usize]))
    }
}

enum Found {
    Alone { why: Option<String> },
    Broken { why: String },
    Refused { why: String },
    Set(Arc<VolumeSet>),
}

struct Discovery {
    path: PathBuf,
    found: Found,
    zip: Option<Found>,
}

impl Discovery {
    fn for_zip(&self) -> &Found {
        self.zip.as_ref().unwrap_or(&self.found)
    }
}

fn first_present_after(numbered: &Numbered, missing: u64) -> Option<u64> {
    (missing + 1..=missing + GAP_PROBE).find(|&number| numbered.path(number).exists())
}

struct Identities {
    first: Identity,
    later: Vec<Identity>,
}

impl Identities {
    fn count(&self) -> u64 {
        1 + self.later.len() as u64
    }

    fn last(&self) -> Identity {
        *self.later.last().unwrap_or(&self.first)
    }

    fn iter(&self) -> impl Iterator<Item = &Identity> {
        std::iter::once(&self.first).chain(&self.later)
    }
}

fn regular_file_identity(path: &Path) -> io::Result<Identity> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot open {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(Identity::of(&metadata))
}

fn later_identities(numbered: &Numbered, first: Identity) -> (Identities, Option<String>) {
    let mut identities = Identities {
        first,
        later: Vec::new(),
    };
    loop {
        let path = numbered.path(FIRST_VOLUME + identities.count());
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => identities.later.push(Identity::of(&metadata)),
            Ok(_) => {
                return (
                    identities,
                    Some(format!("{} is not a file", path.display())),
                );
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return (identities, None),
            Err(error) => {
                return (
                    identities,
                    Some(format!("{} cannot be read: {error}", path.display())),
                );
            }
        }
    }
}

fn sizes_disagree(numbered: &Numbered, identities: &Identities) -> Option<String> {
    let first = numbered.path(FIRST_VOLUME);
    let volume = identities.first.length;
    let count = identities.count();
    for (index, identity) in (0..).zip(identities.iter()) {
        let path = numbered.path(FIRST_VOLUME + index);
        let length = identity.length;
        let last = index + 1 == count;
        if length == 0 {
            return Some(format!("{} is empty", name(&path)));
        }
        if last
            && length
                .checked_sub(volume)
                .is_some_and(|extra| extra >= count)
        {
            return Some(format!(
                "{} holds {length} bytes, more than the {volume} of {} by {count} or more",
                name(&path),
                name(&first)
            ));
        }
        if last && Layout::new(volume, count, length).is_none() {
            return Some(format!(
                "{count} volumes of {volume} bytes add up to more than the 16 EiB a split archive can hold"
            ));
        }
        if !last && length > volume {
            return Some(format!(
                "{} holds {length} bytes, more than the {volume} of {}, but more volumes follow it",
                name(&path),
                name(&first)
            ));
        }
        if !last && length < volume {
            let next = numbered.path(FIRST_VOLUME + index + 1);
            return Some(format!(
                "{} holds {length} bytes, fewer than the {volume} of {}, but {} follows it, so {} is truncated or {} does not belong",
                name(&path),
                name(&first),
                name(&next),
                name(&path),
                name(&next)
            ));
        }
    }
    None
}

fn find_set(numbered: Numbered, first: Identity) -> Found {
    let (identities, unreadable) = later_identities(&numbered, first);
    let count = identities.count();
    let next = numbered.path(count + 1);
    let broken = unreadable.or_else(|| {
        first_present_after(&numbered, count + 1).map(|present| {
            format!(
                "{} is missing, but {} is present",
                name(&next),
                name(&numbered.path(present))
            )
        })
    });
    if let Some(why) = broken {
        return Found::Broken { why };
    }
    if count == 1 {
        return Found::Alone {
            why: Some(format!(
                "no {} is present; if it is the first volume of a split archive, {} is missing",
                name(&next),
                name(&next)
            )),
        };
    }
    if let Some(why) = sizes_disagree(&numbered, &identities) {
        let only_a_larger_sibling = count == 2 && identities.last().length > first.length;
        return if only_a_larger_sibling {
            Found::Alone { why: Some(why) }
        } else {
            Found::Broken { why }
        };
    }
    match Layout::new(first.length, count, identities.last().length) {
        Some(layout) => Found::Set(Arc::new(VolumeSet {
            names: Names::Numbered(numbered),
            identities: identities.iter().copied().collect(),
            spans: Spans::Even(layout),
        })),
        None => Found::Broken {
            why: "the volumes add up past 16 EiB".to_owned(),
        },
    }
}

fn zip_volume_identities(names: &ZipNames) -> Result<Vec<Identity>, String> {
    let mut identities = Vec::new();
    loop {
        let path = names.volume(FIRST_VOLUME + identities.len() as u64);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => identities.push(Identity::of(&metadata)),
            Ok(_) => return Err(format!("{} is not a file", path.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(identities),
            Err(error) => return Err(format!("{} cannot be read: {error}", path.display())),
        }
    }
}

fn find_zip_set(names: ZipNames, last: &Path) -> io::Result<Found> {
    let mut identities = match zip_volume_identities(&names) {
        Ok(identities) => identities,
        Err(why) => return Ok(Found::Broken { why }),
    };
    if identities.is_empty() {
        return Ok(Found::Alone { why: None });
    }
    let last_identity = regular_file_identity(last)?;
    let missing = FIRST_VOLUME + identities.len() as u64;
    let present = (missing + 1..=missing + GAP_PROBE).find(|&number| names.volume(number).exists());
    if let Some(present) = present {
        let why = format!(
            "{} is missing, but {} is present",
            name(&names.volume(missing)),
            name(&names.volume(present))
        );
        return Ok(Found::Broken { why });
    }
    identities.push(last_identity);
    let Some(uneven) = Uneven::new(identities.iter().map(|identity| identity.length)) else {
        return Ok(Found::Broken {
            why: "the volumes add up past 16 EiB".to_owned(),
        });
    };
    let set = VolumeSet {
        names: Names::Zip(names),
        identities,
        spans: Spans::Uneven(uneven),
    };
    if let Some(empty) = (0..set.count()).find(|&index| set.spans.size(index) == 0) {
        return Ok(Found::Broken {
            why: format!("{} is empty", name(&set.path(empty))),
        });
    }
    Ok(Found::Set(Arc::new(set)))
}

fn zip_volume_alone(names: &ZipNames, number: u64, path: &Path) -> Found {
    let last = names.last();
    let why = if last.exists() {
        format!(
            "{} is volume {number} of a split ZIP; open its last volume, {}, to read the set",
            path.display(),
            name(&last)
        )
    } else {
        format!(
            "{} is volume {number} of a split ZIP whose last volume, {}, is missing",
            path.display(),
            last.display()
        )
    };
    Found::Refused { why }
}

fn discover(path: &Path) -> io::Result<Discovery> {
    let (found, zip) = if let Some(names) = ZipNames::of_last(path) {
        let zip = Some(find_zip_set(names, path)?)
            .filter(|found| !matches!(found, Found::Alone { why: None }));
        (Found::Alone { why: None }, zip)
    } else if let Some((names, number)) = ZipNames::of_volume(path) {
        (zip_volume_alone(&names, number, path), None)
    } else {
        (discover_numbered(path)?, None)
    };
    Ok(Discovery {
        path: path.to_owned(),
        found,
        zip,
    })
}

fn discover_numbered(path: &Path) -> io::Result<Found> {
    Ok(match Numbered::parse(path) {
        None => Found::Alone { why: None },
        Some(numbered) if numbered.number == FIRST_VOLUME => {
            let first = regular_file_identity(path)?;
            find_set(numbered, first)
        }
        Some(numbered) => {
            let first = numbered.path(FIRST_VOLUME);
            Found::Alone {
                why: first.is_file().then(|| {
                    format!(
                        "it is volume {} of a split archive; open its first volume, {}, to read the set",
                        numbered.number,
                        name(&first)
                    )
                }),
            }
        }
    })
}

thread_local! {
    static IN_USE: RefCell<Vec<Arc<Discovery>>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct Scope(Arc<Discovery>);

impl Scope {
    fn enter(discovery: Discovery) -> Self {
        let discovery = Arc::new(discovery);
        IN_USE.with_borrow_mut(|in_use| in_use.push(Arc::clone(&discovery)));
        Self(discovery)
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        IN_USE.with_borrow_mut(|in_use| {
            if let Some(at) = in_use.iter().rposition(|held| Arc::ptr_eq(held, &self.0)) {
                in_use.remove(at);
            }
        });
    }
}

pub(crate) fn scope(path: &Path) -> io::Result<Scope> {
    Ok(Scope::enter(discover(path)?))
}

fn in_use(path: &Path) -> Option<Arc<Discovery>> {
    IN_USE.with_borrow(|in_use| {
        in_use
            .iter()
            .rev()
            .find(|held| held.path == path)
            .map(Arc::clone)
    })
}

fn discovered(path: &Path) -> io::Result<Arc<Discovery>> {
    match in_use(path) {
        Some(discovery) => Ok(discovery),
        None => Ok(Arc::new(discover(path)?)),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Unchanged(Identity);

pub(crate) fn open_alone(path: &Path) -> io::Result<(ArchiveFile, Unchanged)> {
    let file = open_regular_file(path, None)?;
    let identity = Identity::of(&file.metadata()?);
    Ok((ArchiveFile::from(file), Unchanged(identity)))
}

pub(crate) fn reopen_unchanged(path: &Path, before: Unchanged) -> io::Result<ArchiveFile> {
    Ok(ArchiveFile::from(open_regular_file(path, Some(before.0))?))
}

pub(crate) fn open(path: &Path) -> io::Result<ArchiveFile> {
    open_found(path, &discovered(path)?.found)
}

pub(crate) fn open_zip(path: &Path) -> io::Result<ArchiveFile> {
    open_found(path, discovered(path)?.for_zip())
}

pub(crate) fn has_zip_volumes(path: &Path) -> io::Result<bool> {
    Ok(discovered(path)?.zip.is_some())
}

fn open_found(path: &Path, found: &Found) -> io::Result<ArchiveFile> {
    Ok(match found {
        Found::Alone { .. } => ArchiveFile::from(open_regular_file(path, None)?),
        Found::Broken { why } => {
            return Err(invalid(format!(
                "{why}, so {} is neither joined with its later volumes nor read alone",
                path.display()
            )));
        }
        Found::Refused { why } => return Err(invalid(why.clone())),
        Found::Set(set) => ArchiveFile::Joined(Joined {
            set: Arc::clone(set),
            position: 0,
            open: Mutex::new(None),
        }),
    })
}

pub(crate) fn ensure_stream_set_ends_short(path: &Path) -> io::Result<()> {
    let set = match &discovered(path)?.found {
        Found::Set(set) => Arc::clone(set),
        Found::Alone { .. } if joined_name(path).is_some() => {
            return Err(invalid(format!(
                "{} would be read alone, but a single compressed file has no end of its own to show it is complete, so later volumes may be missing; open the file under a name without .001, or join the volumes into one file with cat",
                path.display()
            )));
        }
        _ => return Ok(()),
    };
    if set.even().is_some_and(|layout| layout.last < layout.volume) {
        return Ok(());
    }
    let count = set.count();
    Err(invalid(format!(
        "{} is not smaller than {}, so later volumes such as {} may be missing or it may not belong to the set, and a single compressed file has no end of its own to show it is complete; join the volumes into one file with cat to read them",
        name(&set.path(count - 1)),
        name(&set.path(0)),
        name(&set.path(count))
    )))
}

pub(crate) fn ensure_not_split(path: &Path, format: &str) -> io::Result<()> {
    let Found::Set(set) = &discovered(path)?.found else {
        return Ok(());
    };
    let count = set.identities.len() as u64;
    Err(invalid(format!(
        "{} to {} would be read as one {format} stream, but {format} has no checksum or end mark to show that no volume is truncated or missing; join the volumes into one file with cat to read them",
        name(&set.path(0)),
        name(&set.path(count - 1))
    )))
}

pub(crate) fn joined_name(path: &Path) -> Option<PathBuf> {
    Numbered::parse(path)
        .filter(|numbered| numbered.number == FIRST_VOLUME)
        .map(|numbered| numbered.folder.join(numbered.stem))
}

fn same_bytes(left: File, right: File) -> io::Result<bool> {
    let (mut left, mut right) = (io::BufReader::new(left), io::BufReader::new(right));
    loop {
        let (left_bytes, right_bytes) = (left.fill_buf()?, right.fill_buf()?);
        let length = left_bytes.len().min(right_bytes.len());
        if left_bytes[..length] != right_bytes[..length] {
            return Ok(false);
        }
        if length == 0 {
            return Ok(left_bytes.is_empty() && right_bytes.is_empty());
        }
        left.consume(length);
        right.consume(length);
    }
}

fn crc32_of(file: File) -> io::Result<u32> {
    let mut hasher = crc32fast::Hasher::new();
    let mut reader = io::BufReader::new(file);
    loop {
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return Ok(hasher.finalize());
        }
        hasher.update(bytes);
        let length = bytes.len();
        reader.consume(length);
    }
}

struct RepeatScan {
    found: Option<(u64, u64)>,
    bytes_read: u64,
}

fn repeated_volume(
    set: &VolumeSet,
    fingerprint: fn(File) -> io::Result<u32>,
    budget: u64,
) -> io::Result<RepeatScan> {
    let mut first_of_group = std::collections::HashMap::new();
    let mut scan = RepeatScan {
        found: None,
        bytes_read: 0,
    };
    for (index, identity) in (0..).zip(&set.identities) {
        let length = identity.length;
        if length < REPEAT_MIN_BYTES {
            continue;
        }
        if scan.bytes_read + length > budget {
            break;
        }
        scan.bytes_read += length;
        let key = (length, fingerprint(set.open_volume(index)?)?);
        let Some(&earlier) = first_of_group.get(&key) else {
            first_of_group.insert(key, index);
            continue;
        };
        if scan.bytes_read + 2 * length > budget {
            break;
        }
        scan.bytes_read += 2 * length;
        if same_bytes(set.open_volume(earlier)?, set.open_volume(index)?)? {
            scan.found = Some((earlier, index));
            break;
        }
    }
    Ok(scan)
}

impl Scope {
    pub(crate) fn note(&self) -> Option<String> {
        let first = name(&self.0.path);
        let set = match self.0.for_zip() {
            Found::Alone { why: None } | Found::Broken { .. } | Found::Refused { .. } => {
                return None;
            }
            Found::Alone { why: Some(why) } => {
                return Some(format!("{first} was read alone, because {why}"));
            }
            Found::Set(set) => set,
        };
        let count = set.count();
        let first_path = set.path(0);
        let first = name(&first_path);
        let last_path = set.path(count - 1);
        let last = name(&last_path);
        let mut hint = match set.names {
            Names::Numbered(_) => format!(
                "read as split volumes {first} to {last}; if {first} is a whole archive by itself, rename it or open it by another name"
            ),
            Names::Zip(_) => format!("read as split ZIP volumes {first} to {last}"),
        };
        if let Ok(RepeatScan {
            found: Some((earlier, later)),
            ..
        }) = repeated_volume(set, crc32_of, REPEAT_SCAN_BYTES)
        {
            hint.push_str(&format!(
                "; {} holds the same bytes as {}",
                name(&set.path(later)),
                name(&set.path(earlier))
            ));
        }
        if set.even().is_none() {
            hint.push_str(
                "; if this archive is damaged, a volume may be missing, truncated, repeated or out of order",
            );
        } else if set
            .even()
            .is_some_and(|layout| layout.last >= layout.volume)
        {
            hint.push_str(&format!(
                "; if this archive is damaged, {} may be missing, or a volume may be truncated, repeated or out of order",
                name(&set.path(count))
            ));
        } else {
            hint.push_str(&format!(
                "; if this archive is damaged, {last} may be truncated or not belong to the set, or a volume may be missing, repeated or out of order"
            ));
        }
        Some(hint)
    }
}

pub(crate) struct Joined {
    set: Arc<VolumeSet>,
    position: u64,
    open: Mutex<Option<(u64, File)>>,
}

impl Joined {
    fn read_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        let mut filled = 0;
        while filled < buffer.len() {
            let read = self.read_in_one_volume(&mut buffer[filled..], offset + filled as u64)?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        Ok(filled)
    }

    fn read_in_one_volume(&self, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        let spans = &self.set.spans;
        let Some((index, within)) = spans.locate(offset) else {
            return Ok(0);
        };
        let room = spans.size(index) - within;
        let wanted = buffer
            .len()
            .min(usize::try_from(room).unwrap_or(usize::MAX));
        let mut open = self
            .open
            .lock()
            .map_err(|_| io::Error::other("split volume lock poisoned"))?;
        let file = match &mut *open {
            Some((open_index, file)) if *open_index == index => file,
            slot => &slot.insert((index, self.set.open_volume(index)?)).1,
        };
        let read = read_file_at(file, &mut buffer[..wanted], within)?;
        if read == 0 && wanted > 0 {
            return Err(invalid(format!(
                "split volume {} ended early while it was read",
                name(&self.set.path(index))
            )));
        }
        Ok(read)
    }
}

pub(crate) enum Stub {
    Accepted,
    NotAnExecutable,
    NotAtTheOverlay {
        end: u64,
    },
    TooManyRanges,
    RangeReaches {
        what: &'static str,
        start: u64,
        end: Option<u64>,
    },
    Holds {
        at: u64,
        what: &'static str,
    },
}

impl Stub {
    pub(crate) fn refusal(&self) -> Option<String> {
        match self {
            Self::Accepted => None,
            Self::NotAnExecutable => Some(
                "start with no whole PE, ELF or Mach-O header and no #! line, so another reader may read them as an archive".to_owned(),
            ),
            Self::NotAtTheOverlay { end } => Some(format!(
                "form an executable whose own data ends at offset {end}, not where the archive starts, so 7-Zip reads whatever starts at offset {end} instead"
            )),
            Self::TooManyRanges => Some(format!(
                "form an executable whose headers name more than {STUB_RANGES_MAX} file ranges, more than any real stub"
            )),
            Self::RangeReaches { what, start, end } => Some(format!(
                "name {what} at offset {start} that ends {}, past where the archive starts, so 7-Zip may count the archive's first bytes as part of the executable",
                end.map_or("beyond 16 EiB".to_owned(), |end| format!("at offset {end}"))
            )),
            Self::Holds { at, what } => Some(format!(
                "hold {what} at offset {at}, so another reader may see a different archive"
            )),
        }
    }
}

const PE_HEADER_POINTER: usize = 0x3C;
const PE_SIGNATURE: &[u8; 4] = b"PE\0\0";
const MACHO_THIN: [[u8; 4]; 4] = [
    [0xFE, 0xED, 0xFA, 0xCE],
    [0xFE, 0xED, 0xFA, 0xCF],
    [0xCE, 0xFA, 0xED, 0xFE],
    [0xCF, 0xFA, 0xED, 0xFE],
];
const MACHO_FAT: [u8; 4] = [0xCA, 0xFE, 0xBA, 0xBE];
const MACHO_FAT_ARCH: u64 = 20;
const FAT_ARCHES_BELOW_ANY_JAVA_CLASS_VERSION: std::ops::RangeInclusive<u64> = 1..=30;
const SCRIPT_LINE: &[u8] = b"#!";
const STUB_SCAN_CHUNK: usize = 64 * 1024;
const ARCHIVE_SIGNATURES: [(&[u8], &str, bool); 10] = [
    (b"PK\x03\x04", "a ZIP local header", true),
    (b"PK\x01\x02", "a ZIP central record", true),
    (b"PK\x05\x06", "a ZIP end record", true),
    (b"PK\x06\x06", "a ZIP64 end record", true),
    (b"PK\x06\x07", "a ZIP64 locator", true),
    (b"7z\xBC\xAF\x27\x1C", "a 7z signature", false),
    (b"MSCF\0\0\0\0", "a CAB signature", false),
    (b"Rar!\x1A\x07\x00", "a RAR 4 signature", false),
    (b"Rar!\x1A\x07\x01\x00", "a RAR 5 signature", false),
    (
        b"\xEF\xBE\xAD\xDENullsoftInst",
        "an NSIS installer signature",
        false,
    ),
];
const LONGEST_SIGNATURE: usize = 16;
const VOLUME_DESCRIPTOR_SECTORS: std::ops::RangeInclusive<u64> = 16..=31;
const VOLUME_DESCRIPTOR_TYPE: u64 = 1;
const SECTOR: u64 = 2048;
const VOLUME_IDENTIFIERS: [&[u8; 5]; 6] =
    [b"CD001", b"BEA01", b"NSR02", b"NSR03", b"TEA01", b"BOOT2"];

fn read_up_to(
    read_at: &mut dyn FnMut(&mut [u8], u64) -> io::Result<usize>,
    buffer: &mut [u8],
    offset: u64,
) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = read_at(&mut buffer[filled..], offset + filled as u64)?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

struct Header<'r> {
    read_at: &'r mut dyn FnMut(&mut [u8], u64) -> io::Result<usize>,
    prefix: u64,
    big_endian: bool,
}

impl Header<'_> {
    fn field(&mut self, at: u64, width: usize) -> io::Result<Option<u64>> {
        if at
            .checked_add(width as u64)
            .is_none_or(|end| end > self.prefix)
        {
            return Ok(None);
        }
        let mut bytes = [0u8; 8];
        if read_up_to(self.read_at, &mut bytes[..width], at)? < width {
            return Ok(None);
        }
        let mut value = [0u8; 8];
        if self.big_endian {
            value[8 - width..].copy_from_slice(&bytes[..width]);
            Ok(Some(u64::from_be_bytes(value)))
        } else {
            value[..width].copy_from_slice(&bytes[..width]);
            Ok(Some(u64::from_le_bytes(value)))
        }
    }

    fn fields<const N: usize>(
        &mut self,
        at: u64,
        layout: [(u64, usize); N],
    ) -> io::Result<Option<[u64; N]>> {
        let mut values = [0u64; N];
        for (value, (offset, width)) in values.iter_mut().zip(layout) {
            match at.checked_add(offset) {
                Some(place) => match self.field(place, width)? {
                    Some(read) => *value = read,
                    None => return Ok(None),
                },
                None => return Ok(None),
            }
        }
        Ok(Some(values))
    }
}

struct FileRange {
    what: &'static str,
    start: u64,
    end: Option<u64>,
}

impl FileRange {
    fn new(what: &'static str, start: u64, size: u64) -> Self {
        Self {
            what,
            start,
            end: start.checked_add(size),
        }
    }

    fn table(what: &'static str, start: u64, entry: u64, count: u64) -> Self {
        Self {
            what,
            start,
            end: entry
                .checked_mul(count)
                .and_then(|size| start.checked_add(size)),
        }
    }
}

enum Executable {
    Script,
    Walked,
    Malformed,
}

const STUB_RANGES_MAX: u64 = 65_536;

enum Stop {
    Reaches(FileRange),
    TooMany,
}

struct Walk {
    prefix: u64,
    furthest: u64,
    walked: u64,
    stop: Option<Stop>,
}

impl Walk {
    fn push(&mut self, range: FileRange) {
        if self.stop.is_some() {
            return;
        }
        self.walked += 1;
        if self.walked > STUB_RANGES_MAX {
            self.stop = Some(Stop::TooMany);
            return;
        }
        match range.end {
            Some(end) if end <= self.prefix => self.furthest = self.furthest.max(end),
            _ => self.stop = Some(Stop::Reaches(range)),
        }
    }

    fn stopped(&self) -> bool {
        self.stop.is_some()
    }
}

const PE_SECTIONS_MAX: u64 = 96;
const PE_COFF_HEADER: u64 = 24;
const PE_SECTION: u64 = 40;
const PE32_MAGIC: u64 = 0x10B;
const PE32_PLUS_MAGIC: u64 = 0x20B;
const PE_DIRECTORIES_MAX: u64 = 16;
const PE_CERTIFICATE_DIRECTORY: u64 = 4;
const PE_DEBUG_DIRECTORY: u64 = 6;
const PE_DEBUG_ENTRY: u64 = 28;
const PE_DEBUG_ENTRIES_MAX: u64 = 1024;
const ELF_SECTION_NO_BITS: u64 = 8;
const MACHO_COMMANDS_MAX: u64 = 4096;
const MACHO_SECTIONS_MAX: u64 = 4096;
const MACHO_FAT_64: [u8; 4] = [0xCA, 0xFE, 0xBA, 0xBF];

struct PeSection {
    virtual_start: u64,
    virtual_size: u64,
    raw_size: u64,
    raw_start: u64,
}

impl PeSection {
    fn file_offset(&self, rva: u64) -> Option<u64> {
        let span = self.virtual_size.max(self.raw_size);
        (rva >= self.virtual_start && rva - self.virtual_start < span)
            .then(|| self.raw_start + (rva - self.virtual_start))
    }
}

fn pe_ranges(header: &mut Header, ranges: &mut Walk) -> io::Result<Executable> {
    let Some([pointer]) = header.fields(0, [(PE_HEADER_POINTER as u64, 4)])? else {
        return Ok(Executable::Malformed);
    };
    let mut signature = [0u8; 4];
    if pointer.checked_add(4).is_none_or(|end| end > header.prefix)
        || read_up_to(header.read_at, &mut signature, pointer)? != 4
        || signature != *PE_SIGNATURE
    {
        return Ok(Executable::Malformed);
    }
    let Some([count, optional_size]) = header.fields(pointer, [(6, 2), (20, 2)])? else {
        return Ok(Executable::Malformed);
    };
    if count == 0 || count > PE_SECTIONS_MAX {
        return Ok(Executable::Malformed);
    }
    let optional = pointer + PE_COFF_HEADER;
    let table = optional + optional_size;
    ranges.push(FileRange::table(
        "a PE section table",
        table,
        PE_SECTION,
        count,
    ));
    let mut sections = Vec::new();
    for index in 0..count {
        if ranges.stopped() {
            break;
        }
        let Some([virtual_size, virtual_start, raw_size, raw_start]) = header.fields(
            table + index * PE_SECTION,
            [(8, 4), (12, 4), (16, 4), (20, 4)],
        )?
        else {
            return Ok(Executable::Malformed);
        };
        if raw_size > 0 {
            ranges.push(FileRange::new(
                "a PE section's raw data",
                raw_start,
                raw_size,
            ));
        }
        sections.push(PeSection {
            virtual_start,
            virtual_size,
            raw_size,
            raw_start,
        });
    }
    let mut raw: Vec<(u64, u64)> = sections
        .iter()
        .filter(|section| section.raw_size > 0)
        .map(|section| {
            (
                section.raw_start,
                section.raw_start.saturating_add(section.raw_size),
            )
        })
        .collect();
    raw.sort_unstable();
    if raw.windows(2).any(|pair| pair[1].0 < pair[0].1) {
        return Ok(Executable::Malformed);
    }
    let Some([magic]) = header.fields(optional, [(0, 2)])? else {
        return Ok(Executable::Malformed);
    };
    let directories = match magic {
        PE32_MAGIC => optional + 96,
        PE32_PLUS_MAGIC => optional + 112,
        _ => return Ok(Executable::Malformed),
    };
    let Some([declared]) = header.fields(directories - 4, [(0, 4)])? else {
        return Ok(Executable::Malformed);
    };
    let fitting = (optional + optional_size).saturating_sub(directories) / 8;
    let file_offset = |rva: u64| {
        sections
            .iter()
            .find_map(|section| section.file_offset(rva))
            .unwrap_or(rva)
    };
    for directory in 0..declared.min(PE_DIRECTORIES_MAX).min(fitting) {
        if ranges.stopped() {
            break;
        }
        let Some([address, size]) = header.fields(directories + directory * 8, [(0, 4), (4, 4)])?
        else {
            return Ok(Executable::Malformed);
        };
        if size == 0 {
            continue;
        }
        if directory == PE_CERTIFICATE_DIRECTORY {
            ranges.push(FileRange::new("a PE certificate table", address, size));
            continue;
        }
        let start = file_offset(address);
        ranges.push(FileRange::new("a PE data directory", start, size));
        if directory == PE_DEBUG_DIRECTORY {
            if size % PE_DEBUG_ENTRY != 0 {
                return Ok(Executable::Malformed);
            }
            let entries = size / PE_DEBUG_ENTRY;
            if entries > PE_DEBUG_ENTRIES_MAX {
                return Ok(Executable::Malformed);
            }
            for entry in 0..entries {
                if ranges.stopped() {
                    break;
                }
                let Some([data_size, data_start]) =
                    header.fields(start + entry * PE_DEBUG_ENTRY, [(16, 4), (24, 4)])?
                else {
                    return Ok(Executable::Malformed);
                };
                if data_size > 0 {
                    ranges.push(FileRange::new(
                        "a PE debug entry's raw data",
                        data_start,
                        data_size,
                    ));
                }
            }
        }
    }
    Ok(Executable::Walked)
}

fn elf_ranges(header: &mut Header, wide: bool, ranges: &mut Walk) -> io::Result<Executable> {
    let word = if wide { 8 } else { 4 };
    let layout = if wide {
        [(32, 8), (40, 8), (54, 2), (56, 2), (58, 2), (60, 2)]
    } else {
        [(28, 4), (32, 4), (42, 2), (44, 2), (46, 2), (48, 2)]
    };
    let Some(
        [
            programs,
            sections,
            program_size,
            program_count,
            section_size,
            section_count,
        ],
    ) = header.fields(0, layout)?
    else {
        return Ok(Executable::Malformed);
    };
    ranges.push(FileRange::new(
        "an ELF header",
        0,
        if wide { 64 } else { 52 },
    ));
    ranges.push(FileRange::table(
        "an ELF program header table",
        programs,
        program_size,
        program_count,
    ));
    ranges.push(FileRange::table(
        "an ELF section header table",
        sections,
        section_size,
        section_count,
    ));
    let (program_entry, section_entry) = if wide { (56, 64) } else { (32, 40) };
    if (program_count > 0 && program_size != program_entry)
        || (section_count > 0 && section_size != section_entry)
    {
        return Ok(Executable::Malformed);
    }
    let program_fields = if wide {
        [(8, 8), (32, 8)]
    } else {
        [(4, 4), (16, 4)]
    };
    for index in 0..program_count {
        if ranges.stopped() {
            break;
        }
        let Some([offset, size]) =
            header.fields(programs + index * program_size, program_fields)?
        else {
            return Ok(Executable::Malformed);
        };
        if size > 0 {
            ranges.push(FileRange::new("an ELF segment", offset, size));
        }
    }
    let section_fields = [
        (4, 4),
        (8 + 2 * word as u64, word),
        (8 + 3 * word as u64, word),
    ];
    for index in 0..section_count {
        if ranges.stopped() {
            break;
        }
        let Some([kind, offset, size]) =
            header.fields(sections + index * section_size, section_fields)?
        else {
            return Ok(Executable::Malformed);
        };
        if kind != ELF_SECTION_NO_BITS && size > 0 {
            ranges.push(FileRange::new("an ELF section", offset, size));
        }
    }
    Ok(Executable::Walked)
}

enum MachoRanges {
    Pairs(&'static str, &'static [(u64, u64)]),
    Tables(&'static str, &'static [(u64, u64, u64)]),
}

const MACHO_LINKEDIT_DATA: &[(u64, u64)] = &[(8, 12)];

fn macho_command_ranges(command: u64, wide: bool) -> Option<MachoRanges> {
    Some(match command {
        0x02 if wide => MachoRanges::Tables("a Mach-O symbol table", &[(8, 12, 16), (16, 20, 1)]),
        0x02 => MachoRanges::Tables("a Mach-O symbol table", &[(8, 12, 12), (16, 20, 1)]),
        0x0B if wide => MachoRanges::Tables("a Mach-O dynamic symbol table", DYSYMTAB_64),
        0x0B => MachoRanges::Tables("a Mach-O dynamic symbol table", DYSYMTAB_32),
        0x16 => MachoRanges::Tables("a Mach-O two-level hint table", &[(8, 12, 4)]),
        0x1D | 0x1E | 0x26 | 0x29 | 0x2B | 0x2E | 0x36 | 0x8000_0033 | 0x8000_0034 => {
            MachoRanges::Pairs("a Mach-O link-edit data range", MACHO_LINKEDIT_DATA)
        }
        0x22 | 0x8000_0022 => MachoRanges::Pairs(
            "a Mach-O dyld information range",
            &[(8, 12), (16, 20), (24, 28), (32, 36), (40, 44)],
        ),
        0x21 | 0x2C => MachoRanges::Pairs("a Mach-O encrypted range", &[(8, 12)]),
        _ => return None,
    })
}

const DYSYMTAB_32: &[(u64, u64, u64)] = &[
    (32, 36, 8),
    (40, 44, 52),
    (48, 52, 4),
    (56, 60, 4),
    (64, 68, 8),
    (72, 76, 8),
];
const DYSYMTAB_64: &[(u64, u64, u64)] = &[
    (32, 36, 8),
    (40, 44, 56),
    (48, 52, 4),
    (56, 60, 4),
    (64, 68, 8),
    (72, 76, 8),
];

fn macho_segment(
    header: &mut Header,
    at: u64,
    command_size: u64,
    wide: bool,
    ranges: &mut Walk,
) -> io::Result<bool> {
    let (fields, first, section, section_fields) = if wide {
        (
            [(40, 8), (48, 8), (64, 4)],
            72,
            80,
            [(40, 8), (48, 4), (64, 4)],
        )
    } else {
        (
            [(32, 4), (36, 4), (48, 4)],
            56,
            68,
            [(36, 4), (40, 4), (56, 4)],
        )
    };
    let Some([offset, size, count]) = header.fields(at, fields)? else {
        return Ok(false);
    };
    if count > MACHO_SECTIONS_MAX || first + count * section > command_size {
        return Ok(false);
    }
    if size > 0 {
        ranges.push(FileRange::new("a Mach-O segment", offset, size));
    }
    for index in 0..count {
        if ranges.stopped() {
            break;
        }
        let Some([size, offset, flags]) =
            header.fields(at + first + index * section, section_fields)?
        else {
            return Ok(false);
        };
        let zero_fill = matches!(flags & 0xFF, 0x01 | 0x0C | 0x12);
        if size > 0 && !zero_fill {
            ranges.push(FileRange::new("a Mach-O section", offset, size));
        }
    }
    Ok(true)
}

fn macho_ranges(header: &mut Header, wide: bool, ranges: &mut Walk) -> io::Result<Executable> {
    let Some([commands, commands_size]) = header.fields(0, [(16, 4), (20, 4)])? else {
        return Ok(Executable::Malformed);
    };
    if commands > MACHO_COMMANDS_MAX {
        return Ok(Executable::Malformed);
    }
    let first: u64 = if wide { 32 } else { 28 };
    ranges.push(FileRange::new(
        "the Mach-O load commands",
        0,
        first + commands_size,
    ));
    let mut at = first;
    for _ in 0..commands {
        if ranges.stopped() {
            break;
        }
        let Some([command, size]) = header.fields(at, [(0, 4), (4, 4)])? else {
            return Ok(Executable::Malformed);
        };
        if size < 8 || at + size > first + commands_size {
            return Ok(Executable::Malformed);
        }
        match command {
            0x01 | 0x19 => {
                if !macho_segment(header, at, size, command == 0x19, ranges)? {
                    return Ok(Executable::Malformed);
                }
            }
            0x31 => {
                let Some([offset, length]) = header.fields(at, [(24, 8), (32, 8)])? else {
                    return Ok(Executable::Malformed);
                };
                ranges.push(FileRange::new("a Mach-O note", offset, length));
            }
            _ => match macho_command_ranges(command, wide) {
                Some(MachoRanges::Pairs(what, pairs)) => {
                    for &(offset, length) in pairs {
                        if ranges.stopped() {
                            break;
                        }
                        let Some([offset, length]) =
                            header.fields(at, [(offset, 4), (length, 4)])?
                        else {
                            return Ok(Executable::Malformed);
                        };
                        if length > 0 {
                            ranges.push(FileRange::new(what, offset, length));
                        }
                    }
                }
                Some(MachoRanges::Tables(what, tables)) => {
                    for &(offset, count, entry) in tables {
                        if ranges.stopped() {
                            break;
                        }
                        let Some([offset, count]) = header.fields(at, [(offset, 4), (count, 4)])?
                        else {
                            return Ok(Executable::Malformed);
                        };
                        if count > 0 {
                            ranges.push(FileRange::table(what, offset, entry, count));
                        }
                    }
                }
                None => {}
            },
        }
        at += size;
    }
    Ok(Executable::Walked)
}

fn fat_ranges(header: &mut Header, wide: bool, ranges: &mut Walk) -> io::Result<Executable> {
    let Some([arches]) = header.fields(0, [(4, 4)])? else {
        return Ok(Executable::Malformed);
    };
    if !FAT_ARCHES_BELOW_ANY_JAVA_CLASS_VERSION.contains(&arches) {
        return Ok(Executable::Malformed);
    }
    let (entry, layout) = if wide {
        (32, [(8, 8), (16, 8)])
    } else {
        (MACHO_FAT_ARCH, [(8, 4), (12, 4)])
    };
    ranges.push(FileRange::table(
        "a fat Mach-O header",
        0,
        1,
        8 + arches * entry,
    ));
    for index in 0..arches {
        if ranges.stopped() {
            break;
        }
        let Some([offset, size]) = header.fields(8 + index * entry, layout)? else {
            return Ok(Executable::Malformed);
        };
        ranges.push(FileRange::new("a fat Mach-O slice", offset, size));
    }
    Ok(Executable::Walked)
}

fn executable_end(
    read_at: &mut dyn FnMut(&mut [u8], u64) -> io::Result<usize>,
    prefix: u64,
    walk: &mut Walk,
) -> io::Result<Executable> {
    let mut head = [0u8; 8];
    let read = read_up_to(read_at, &mut head, 0)?.min(usize::try_from(prefix).unwrap_or(8));
    let head = &head[..read];
    if head.starts_with(SCRIPT_LINE) {
        return Ok(Executable::Script);
    }
    let mut header = Header {
        read_at,
        prefix,
        big_endian: false,
    };
    if head.starts_with(b"MZ") {
        return pe_ranges(&mut header, walk);
    }
    if head.starts_with(b"\x7FELF") && head.len() >= 7 && head[6] == 1 {
        header.big_endian = match head[5] {
            1 => false,
            2 => true,
            _ => return Ok(Executable::Malformed),
        };
        return match head[4] {
            1 => elf_ranges(&mut header, false, walk),
            2 => elf_ranges(&mut header, true, walk),
            _ => Ok(Executable::Malformed),
        };
    }
    if let Some(index) = MACHO_THIN.iter().position(|magic| head.starts_with(magic)) {
        header.big_endian = index < 2;
        return macho_ranges(&mut header, index % 2 == 1, walk);
    }
    if head.starts_with(&MACHO_FAT) || head.starts_with(&MACHO_FAT_64) {
        header.big_endian = true;
        return fat_ranges(&mut header, head.starts_with(&MACHO_FAT_64), walk);
    }
    Ok(Executable::Malformed)
}

fn archive_signature_in(
    read_at: &mut dyn FnMut(&mut [u8], u64) -> io::Result<usize>,
    prefix: u64,
    zip_records_count: bool,
) -> io::Result<Option<(u64, &'static str, bool)>> {
    for sector in VOLUME_DESCRIPTOR_SECTORS {
        let at = sector * SECTOR + VOLUME_DESCRIPTOR_TYPE;
        let mut identifier = [0u8; 5];
        if at + 5 <= prefix
            && read_up_to(read_at, &mut identifier, at)? == 5
            && VOLUME_IDENTIFIERS.contains(&&identifier)
        {
            return Ok(Some((at, "an ISO 9660 or UDF volume descriptor", false)));
        }
    }
    let mut chunk = vec![0u8; STUB_SCAN_CHUNK];
    let mut at = 0u64;
    let scanned = prefix.saturating_add(LONGEST_SIGNATURE as u64 - 1);
    while at < prefix {
        let wanted =
            usize::try_from(scanned - at).map_or(chunk.len(), |left| left.min(chunk.len()));
        let read = read_up_to(read_at, &mut chunk[..wanted], at)?;
        let window = &chunk[..read];
        let found = (0..window.len()).find_map(|index| {
            ARCHIVE_SIGNATURES
                .iter()
                .filter(|&&(_, _, zip)| zip_records_count || !zip)
                .find(|(signature, _, _)| window[index..].starts_with(signature))
                .map(|&(_, what, zip)| (at + index as u64, what, zip))
                .filter(|&(start, _, _)| start < prefix)
        });
        if found.is_some() || read < LONGEST_SIGNATURE {
            return Ok(found);
        }
        at += (read - (LONGEST_SIGNATURE - 1)) as u64;
    }
    Ok(None)
}

pub(crate) fn check_stub(
    read_at: &mut dyn FnMut(&mut [u8], u64) -> io::Result<usize>,
    prefix: u64,
    zip_records_count: bool,
) -> io::Result<Stub> {
    if let Some((at, what, _)) = archive_signature_in(read_at, prefix, zip_records_count)? {
        return Ok(Stub::Holds { at, what });
    }
    let mut walk = Walk {
        prefix,
        furthest: 0,
        walked: 0,
        stop: None,
    };
    match executable_end(read_at, prefix, &mut walk)? {
        Executable::Script => return Ok(Stub::Accepted),
        Executable::Malformed => return Ok(Stub::NotAnExecutable),
        Executable::Walked => {}
    }
    Ok(match walk.stop {
        Some(Stop::Reaches(range)) => Stub::RangeReaches {
            what: range.what,
            start: range.start,
            end: range.end,
        },
        Some(Stop::TooMany) => Stub::TooManyRanges,
        None if walk.furthest == prefix => Stub::Accepted,
        None => Stub::NotAtTheOverlay { end: walk.furthest },
    })
}

fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::read_at(file, buffer, offset);
    #[cfg(windows)]
    return std::os::windows::fs::FileExt::seek_read(file, buffer, offset);
}

pub(crate) enum ArchiveFile {
    One { file: File, position: u64 },
    Joined(Joined),
    After { inner: Box<ArchiveFile>, base: u64 },
}

pub(crate) struct ZipDisks(Arc<VolumeSet>);

impl ZipDisks {
    pub(crate) fn count(&self) -> u64 {
        self.0.count()
    }

    pub(crate) fn place(&self, disk: u64, offset: u64) -> Option<u64> {
        let Spans::Uneven(uneven) = &self.0.spans else {
            return None;
        };
        (disk < self.count() && offset < uneven.size(disk))
            .then(|| uneven.starts[disk as usize] + offset)
    }

    pub(crate) fn name(&self, disk: u64) -> String {
        match disk {
            _ if disk < self.count() => name(&self.0.path(disk)).to_string(),
            ZIP64_DISK_PLACEHOLDER => {
                "disk number 0xFFFF, which leaves the number to a ZIP64 field".to_owned()
            }
            _ => format!("disk {}", disk + 1),
        }
    }

    pub(crate) fn earlier_names(&self) -> String {
        match self.count() {
            0 | 1 => String::new(),
            2 => self.name(0),
            count => format!("{} to {}", self.name(0), self.name(count - 2)),
        }
    }

    pub(crate) fn names(&self) -> String {
        format!("{} to {}", self.name(0), self.name(self.count() - 1))
    }
}

impl From<File> for ArchiveFile {
    fn from(file: File) -> Self {
        Self::One { file, position: 0 }
    }
}

impl ArchiveFile {
    pub(crate) fn len(&self) -> io::Result<u64> {
        match self {
            Self::One { file, .. } => Ok(file.metadata()?.len()),
            Self::Joined(joined) => Ok(joined.set.spans.total()),
            Self::After { inner, base } => Ok(inner.len()?.saturating_sub(*base)),
        }
    }

    pub(crate) fn after(mut self, base: u64) -> io::Result<Self> {
        if base == 0 {
            return Ok(self);
        }
        self.seek(SeekFrom::Start(base))?;
        Ok(Self::After {
            inner: Box::new(self),
            base,
        })
    }

    pub(crate) fn zip_disks(&self) -> Option<ZipDisks> {
        match self {
            Self::Joined(joined) if matches!(joined.set.names, Names::Zip(_)) => {
                Some(ZipDisks(Arc::clone(&joined.set)))
            }
            _ => None,
        }
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(match self {
            Self::One { file, position } => Self::One {
                file: file.try_clone()?,
                position: *position,
            },
            Self::Joined(joined) => Self::Joined(Joined {
                set: Arc::clone(&joined.set),
                position: joined.position,
                open: Mutex::new(None),
            }),
            Self::After { inner, base } => Self::After {
                inner: Box::new(inner.try_clone()?),
                base: *base,
            },
        })
    }

    pub(crate) fn ensure_ends_at(&self, end: u64) -> io::Result<()> {
        let joined = match self {
            Self::Joined(joined) => joined,
            Self::After { inner, base } => return inner.ensure_ends_at(base.saturating_add(end)),
            Self::One { .. } => return Ok(()),
        };
        match joined.set.spans.locate(end) {
            None => Ok(()),
            Some((index, _)) => Err(invalid(format!(
                "split volume {} holds bytes past the archive's end, so it does not belong to the set",
                name(&joined.set.path(index))
            ))),
        }
    }

    pub(crate) fn reopen(&self, path: &Path) -> io::Result<Self> {
        match self {
            Self::One { file, .. } => Ok(Self::from(open_regular_file(
                path,
                Some(Identity::of(&file.metadata()?)),
            )?)),
            Self::Joined(_) => self.try_clone(),
            Self::After { inner, base } => inner.reopen(path)?.after(*base),
        }
    }

    pub(crate) fn read_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        match self {
            Self::One { file, .. } => read_file_at(file, buffer, offset),
            Self::Joined(joined) => joined.read_at(buffer, offset),
            Self::After { inner, base } => match base.checked_add(offset) {
                Some(offset) => inner.read_at(buffer, offset),
                None => Ok(0),
            },
        }
    }
}

impl Read for ArchiveFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::One { file, position } => {
                let read = read_file_at(file, buffer, *position)?;
                *position += read as u64;
                Ok(read)
            }
            Self::Joined(joined) => {
                let read = joined.read_at(buffer, joined.position)?;
                joined.position += read as u64;
                Ok(read)
            }
            Self::After { inner, .. } => inner.read(buffer),
        }
    }
}

impl Seek for ArchiveFile {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        match self {
            Self::One { file, position } => {
                let target = match seek {
                    SeekFrom::Start(target) => i128::from(target),
                    SeekFrom::Current(delta) => i128::from(*position) + i128::from(delta),
                    SeekFrom::End(delta) => i128::from(file.metadata()?.len()) + i128::from(delta),
                };
                *position = i64::try_from(target).and_then(u64::try_from).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid archive seek")
                })?;
                Ok(*position)
            }
            Self::Joined(joined) => {
                let target = match seek {
                    SeekFrom::Start(position) => i128::from(position),
                    SeekFrom::Current(delta) => i128::from(joined.position) + i128::from(delta),
                    SeekFrom::End(delta) => {
                        i128::from(joined.set.spans.total()) + i128::from(delta)
                    }
                };
                joined.position = i64::try_from(target).and_then(u64::try_from).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid split archive seek")
                })?;
                Ok(joined.position)
            }
            Self::After { inner, base } => {
                let base = *base;
                let target = match seek {
                    SeekFrom::Start(position) => i128::from(position),
                    SeekFrom::Current(delta) => {
                        i128::from(inner.stream_position()?) - i128::from(base) + i128::from(delta)
                    }
                    SeekFrom::End(delta) => {
                        i128::from(inner.len()?) - i128::from(base) + i128::from(delta)
                    }
                };
                let position = u64::try_from(target)
                    .ok()
                    .and_then(|position| position.checked_add(base))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "invalid archive seek")
                    })?;
                Ok(inner.seek(SeekFrom::Start(position))? - base)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_set(folder: &Path, stem: &str, data: &[u8], volume: usize) -> PathBuf {
        for (index, chunk) in data.chunks(volume).enumerate() {
            std::fs::write(folder.join(format!("{stem}.{:03}", index + 1)), chunk).unwrap();
        }
        folder.join(format!("{stem}.001"))
    }

    #[test]
    fn sizes_that_overflow_get_their_own_message() {
        let identity = |length| Identity {
            length,
            modified: None,
            #[cfg(unix)]
            device: 0,
            #[cfg(unix)]
            inode: 0,
        };
        let numbered = Numbered::parse(Path::new("huge.bin.001")).unwrap();
        let half = u64::MAX / 2;
        let identities = Identities {
            first: identity(half),
            later: vec![identity(half), identity(half)],
        };
        let why = sizes_disagree(&numbered, &identities).unwrap();
        assert!(why.contains("16 EiB"), "{why}");
        let larger = Identities {
            first: identity(10),
            later: vec![identity(10), identity(13)],
        };
        let why = sizes_disagree(&numbered, &larger).unwrap();
        assert!(why.contains("by 3 or more"), "{why}");
    }

    #[test]
    fn every_layout_locates_every_offset_as_a_linear_scan_does() {
        for volume in 1..=6u64 {
            for count in 1..=5u64 {
                for last in 1..volume + count {
                    let layout = Layout::new(volume, count, last).unwrap();
                    let mut expected = Vec::new();
                    for index in 0..count {
                        for within in 0..layout.size(index) {
                            expected.push((index, within));
                        }
                    }
                    assert_eq!(layout.total(), expected.len() as u64);
                    for offset in 0..expected.len() as u64 + 3 {
                        assert_eq!(
                            layout.locate(offset),
                            expected.get(offset as usize).copied()
                        );
                    }
                }
            }
        }
        assert_eq!(Layout::new(0, 1, 1), None);
        assert_eq!(Layout::new(2, 1, 3), None);
        assert_eq!(Layout::new(2, 3, 5), None);
        assert_eq!(Layout::new(u64::MAX, 3, 1), None);
    }

    #[test]
    fn uneven_volumes_locate_every_offset_as_a_linear_scan_does() {
        let mut layouts = vec![vec![]];
        for _ in 0..4 {
            layouts = layouts
                .into_iter()
                .flat_map(|sizes: Vec<u64>| {
                    (1..=4).map(move |size| [sizes.clone(), vec![size]].concat())
                })
                .collect();
            for sizes in &layouts {
                let uneven = Uneven::new(sizes.iter().copied()).unwrap();
                let mut expected = Vec::new();
                for (index, &size) in sizes.iter().enumerate() {
                    assert_eq!(uneven.size(index as u64), size);
                    for within in 0..size {
                        expected.push((index as u64, within));
                    }
                }
                assert_eq!(uneven.total(), expected.len() as u64);
                for offset in 0..expected.len() as u64 + 3 {
                    assert_eq!(
                        uneven.locate(offset),
                        expected.get(offset as usize).copied()
                    );
                }
            }
        }
        assert!(Uneven::new([u64::MAX, 1].into_iter()).is_none());
    }

    #[test]
    fn joined_reads_and_seeks_match_the_whole_at_every_volume_size() -> io::Result<()> {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 % 251) as u8).collect();
        for volume in [1, 2, 7, 64, 299, 300, 512] {
            let folder = tempfile::tempdir()?;
            let first = write_set(folder.path(), "a.bin", &data, volume);
            let mut joined = open(&first)?;
            assert_eq!(joined.len()?, data.len() as u64);
            let mut whole = Vec::new();
            joined.read_to_end(&mut whole)?;
            assert_eq!(whole, data, "volume {volume}");
            for start in [0, 1, 63, 64, 65, 150, 299, 300, 301] {
                joined.seek(SeekFrom::Start(start))?;
                let mut tail = Vec::new();
                joined.try_clone()?.read_to_end(&mut tail)?;
                assert_eq!(tail, data[(start as usize).min(data.len())..]);
                let mut buffer = [0; 5];
                let read = joined.read_at(&mut buffer, start)?;
                assert_eq!(
                    buffer[..read],
                    data[(start as usize).min(data.len())..][..read]
                );
            }
            assert_eq!(joined.seek(SeekFrom::End(-1))?, 299);
            assert!(joined.seek(SeekFrom::Current(-300)).is_err());
        }
        Ok(())
    }

    #[test]
    fn a_positional_read_leaves_the_sequential_position_where_it_was() -> io::Result<()> {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 7 % 251) as u8).collect();
        for volume in [7, 300] {
            let folder = tempfile::tempdir()?;
            let first = write_set(folder.path(), "a.bin", &data, volume);
            let mut file = open(&first)?;
            let mut probe = [0; 16];
            file.read_at(&mut probe, 250)?;
            let mut head = [0; 10];
            file.read_exact(&mut head)?;
            assert_eq!(head, data[..10], "volume {volume}");
            file.read_at(&mut probe, 200)?;
            let mut clone = file.try_clone()?;
            file.read_exact(&mut head)?;
            assert_eq!(head, data[10..20], "volume {volume}");
            clone.read_exact(&mut head)?;
            assert_eq!(head, data[10..20], "volume {volume} clone");
            assert!(
                file.seek(SeekFrom::Start(1 << 63)).is_err(),
                "volume {volume}"
            );
        }
        Ok(())
    }

    #[test]
    fn a_file_read_after_a_base_reads_and_seeks_as_the_bytes_past_it() -> io::Result<()> {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 13 % 251) as u8).collect();
        for volume in [7, 300] {
            let folder = tempfile::tempdir()?;
            let first = write_set(folder.path(), "a.bin", &data, volume);
            for base in [1, 7, 100, 297] {
                let rest = &data[base..];
                let mut after = open(&first)?.after(base as u64)?;
                assert_eq!(after.len()?, rest.len() as u64);
                let mut whole = Vec::new();
                after.read_to_end(&mut whole)?;
                assert_eq!(whole, rest, "volume {volume} base {base}");
                assert_eq!(after.seek(SeekFrom::End(-1))?, rest.len() as u64 - 1);
                assert_eq!(after.seek(SeekFrom::Current(-1))?, rest.len() as u64 - 2);
                assert!(after.seek(SeekFrom::Current(-(rest.len() as i64))).is_err());
                after.seek(SeekFrom::Start(0))?;
                let mut again = Vec::new();
                after.try_clone()?.read_to_end(&mut again)?;
                assert_eq!(again, rest);
                let mut buffer = [0; 5];
                let read = after.read_at(&mut buffer, 1)?;
                assert_eq!(buffer[..read], rest[1..][..read]);
                let mut reopened = Vec::new();
                after.reopen(&first)?.read_to_end(&mut reopened)?;
                assert_eq!(reopened, rest);
            }
        }
        Ok(())
    }

    #[test]
    fn a_set_is_found_once_per_scope_and_never_shared_across_scopes() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let first = write_set(folder.path(), "once.bin", &[5; 30], 10);
        let joined_set = |file: ArchiveFile| match file {
            ArchiveFile::Joined(joined) => joined.set,
            _ => panic!("read alone"),
        };
        let outer = scope(&first);
        let held = joined_set(open(&first)?);
        assert!(Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        let inner = scope(&first);
        assert!(!Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        drop(inner);
        assert!(Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        drop(outer);
        assert!(!Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        let elsewhere = std::thread::spawn(move || {
            let _scope = scope(&first);
            joined_set(open(&first).unwrap())
        })
        .join()
        .unwrap();
        assert!(!Arc::ptr_eq(&held, &elsewhere));
        Ok(())
    }

    #[test]
    fn the_repeat_scan_reads_each_volume_at_most_three_times_whatever_collides() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let volumes = 300u64;
        let data: Vec<u8> = (0..volumes * 64)
            .map(|i| ((i * 2_654_435_761) >> 7) as u8 ^ (i / 64) as u8)
            .collect();
        let first = write_set(folder.path(), "c.bin", &data, 64);
        let Found::Set(set) = discover(&first)?.found else {
            panic!("not a set")
        };
        let everything_collides: fn(File) -> io::Result<u32> = |_| Ok(0);
        let scan = repeated_volume(&set, everything_collides, u64::MAX)?;
        assert_eq!(scan.found, None);
        assert!(
            scan.bytes_read <= 3 * data.len() as u64,
            "{}",
            scan.bytes_read
        );
        let capped = repeated_volume(&set, everything_collides, 1000)?;
        assert!(capped.bytes_read <= 1000, "{}", capped.bytes_read);
        std::fs::copy(
            folder.path().join("c.bin.002"),
            folder.path().join("c.bin.007"),
        )?;
        let Found::Set(set) = discover(&first)?.found else {
            panic!("not a set")
        };
        let repeat = repeated_volume(&set, crc32_of, u64::MAX)?;
        assert_eq!(repeat.found, Some((1, 6)));
        Ok(())
    }

    #[test]
    fn a_volume_replaced_after_opening_is_refused_by_name() -> io::Result<()> {
        let data = [7u8; 30];
        let mut lengths = vec![9, 11];
        if cfg!(unix) {
            lengths.push(10);
        }
        for length in lengths {
            let folder = tempfile::tempdir()?;
            let first = write_set(folder.path(), "a.bin", &data, 10);
            let joined = open(&first)?;
            let second = folder.path().join("a.bin.002");
            std::fs::remove_file(&second)?;
            std::fs::write(&second, vec![1; length])?;
            let error = joined.read_at(&mut [0; 4], 12).unwrap_err();
            assert!(error.to_string().contains("a.bin.002"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn a_volume_rewritten_in_place_at_the_same_length_is_refused() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let first = write_set(folder.path(), "a.bin", &[7u8; 30], 10);
        let joined = open(&first)?;
        let second = folder.path().join("a.bin.002");
        let rewritten = std::fs::OpenOptions::new().write(true).open(&second)?;
        std::io::Write::write_all(&mut &rewritten, &[1; 10])?;
        rewritten
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(10))?;
        drop(rewritten);
        let error = joined.read_at(&mut [0; 4], 12).unwrap_err();
        assert!(error.to_string().contains("a.bin.002"), "{error}");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn reopening_a_file_swapped_for_a_fifo_fails_instead_of_blocking() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let path = folder.path().join("a.7z");
        std::fs::write(&path, [7u8; 30])?;
        let opened = open(&path)?;
        std::fs::remove_file(&path)?;
        let made = std::process::Command::new("mkfifo").arg(&path).status()?;
        assert!(made.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(opened.reopen(&path).map(drop).map_err(|e| e.to_string()));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("reopening a FIFO blocked");
        assert!(result.unwrap_err().contains("a.7z"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_opened_as_an_archive_fails_instead_of_blocking() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let fifo = folder.path().join("a.zip");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
        assert!(made.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(open(&fifo).map(drop).map_err(|e| e.to_string()));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("opening a FIFO blocked");
        assert!(result.unwrap_err().contains("a.zip"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_volume_swapped_for_a_fifo_fails_instead_of_blocking() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let first = write_set(folder.path(), "a.bin", &[7u8; 30], 10);
        let joined = open(&first)?;
        let second = folder.path().join("a.bin.002");
        std::fs::remove_file(&second)?;
        let made = std::process::Command::new("mkfifo").arg(&second).status()?;
        assert!(made.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(joined.read_at(&mut [0; 4], 12).map_err(|e| e.to_string()));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("reading a FIFO volume blocked");
        let error = result.unwrap_err();
        assert!(error.contains("a.bin.002"), "{error}");
        Ok(())
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn a_layout_exists_exactly_when_its_total_fits() {
        let (volume, count, last): (u64, u64, u64) = kani::any();
        let valid =
            volume > 0 && count > 0 && last > 0 && (last <= volume || last - volume < count);
        let fits = valid
            && (u128::from(volume) * u128::from(count - 1) + u128::from(last))
                <= u128::from(u64::MAX);
        match Layout::new(volume, count, last) {
            Some(layout) => {
                assert!(fits);
                assert!(
                    u128::from(layout.total())
                        == u128::from(volume) * u128::from(count - 1) + u128::from(last)
                );
            }
            None => assert!(!fits),
        }
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn every_offset_in_uneven_volumes_maps_to_the_volume_that_holds_it() {
        let sizes: [u64; 3] = kani::any();
        let Some(uneven) = Uneven::new(sizes.into_iter()) else {
            return;
        };
        let offset: u64 = kani::any();
        match uneven.locate(offset) {
            None => assert!(offset >= uneven.total()),
            Some((index, within)) => {
                assert!(index < 3);
                assert!(within < uneven.size(index));
                assert!(uneven.starts[index as usize] + within == offset);
            }
        }
    }

    #[kani::proof]
    fn every_offset_maps_to_the_one_volume_and_position_that_hold_it() {
        let (volume, count, last, offset): (u8, u8, u8, u16) = kani::any();
        let Some(layout) = Layout::new(volume.into(), count.into(), last.into()) else {
            return;
        };
        let offset = u64::from(offset);
        match layout.locate(offset) {
            None => assert!(offset >= layout.total()),
            Some((index, within)) => {
                assert!(offset < layout.total());
                assert!(index < u64::from(count));
                assert!(within < layout.size(index));
                assert!(index * u64::from(volume) + within == offset);
            }
        }
    }
}
