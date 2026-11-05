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
        if volume == 0 || count == 0 || last == 0 || last > volume {
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
        (offset < self.total()).then(|| (offset / self.volume, offset % self.volume))
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
    #[cfg(unix)]
    changed: (i64, i64),
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
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
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

fn open_volume_file(path: &Path, identity: Identity) -> io::Result<File> {
    let file = open_without_blocking(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || Identity::of(&metadata) != identity {
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

pub(crate) struct VolumeSet {
    numbered: Numbered,
    identities: Vec<Identity>,
    layout: Layout,
}

impl VolumeSet {
    fn path(&self, index: u64) -> PathBuf {
        self.numbered.path(FIRST_VOLUME + index)
    }

    fn open_volume(&self, index: u64) -> io::Result<File> {
        open_volume_file(&self.path(index), self.identities[index as usize])
    }
}

enum Found {
    Alone { why: Option<String> },
    Set(Arc<VolumeSet>),
}

struct Discovery {
    path: PathBuf,
    found: Found,
}

fn first_present_after(numbered: &Numbered, missing: u64) -> Option<u64> {
    (missing + 1..=missing + GAP_PROBE).find(|&number| numbered.path(number).exists())
}

fn volume_identities(numbered: &Numbered) -> (Vec<Identity>, Option<String>) {
    let mut identities = Vec::new();
    loop {
        let path = numbered.path(FIRST_VOLUME + identities.len() as u64);
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => identities.push(Identity::of(&metadata)),
            Ok(_) => return (identities, Some(format!("{} is not a file", name(&path)))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return (identities, None),
            Err(error) => {
                return (
                    identities,
                    Some(format!("{} cannot be read: {error}", name(&path))),
                );
            }
        }
    }
}

fn sizes_disagree(numbered: &Numbered, identities: &[Identity]) -> Option<String> {
    let first = numbered.path(FIRST_VOLUME);
    let volume = identities[0].length;
    for (index, identity) in (0..).zip(identities) {
        let path = numbered.path(FIRST_VOLUME + index);
        let length = identity.length;
        if length == 0 {
            return Some(format!("{} is empty", name(&path)));
        }
        if length > volume {
            return Some(format!(
                "{} holds {length} bytes, more than the {volume} of {}",
                name(&path),
                name(&first)
            ));
        }
        if index + 1 < identities.len() as u64 && length < volume {
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

fn find_set(numbered: Numbered) -> Found {
    let (identities, unreadable) = volume_identities(&numbered);
    let count = identities.len() as u64;
    let next = numbered.path(count + 1);
    let why = unreadable
        .or_else(|| {
            first_present_after(&numbered, count + 1).map(|present| {
                format!(
                    "{} is missing, but {} is present",
                    name(&next),
                    name(&numbered.path(present))
                )
            })
        })
        .or_else(|| {
            (count == 1).then(|| {
                format!(
                    "no {} is present; if it is the first volume of a split archive, {} is missing",
                    name(&next),
                    name(&next)
                )
            })
        })
        .or_else(|| sizes_disagree(&numbered, &identities));
    if why.is_some() || count == 0 {
        return Found::Alone { why };
    }
    let (volume, last) = (
        identities[0].length,
        identities[identities.len() - 1].length,
    );
    match Layout::new(volume, count, last) {
        Some(layout) => Found::Set(Arc::new(VolumeSet {
            numbered,
            identities,
            layout,
        })),
        None => Found::Alone {
            why: Some("the volumes add up past 16 EiB".to_owned()),
        },
    }
}

fn discover(path: &Path) -> Discovery {
    let found = match Numbered::parse(path) {
        None => Found::Alone { why: None },
        Some(numbered) if numbered.number == FIRST_VOLUME => find_set(numbered),
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
    };
    Discovery {
        path: path.to_owned(),
        found,
    }
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

    pub(crate) fn is_set(&self) -> bool {
        matches!(self.0.found, Found::Set(_))
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

pub(crate) fn scope(path: &Path) -> Scope {
    Scope::enter(discover(path))
}

pub(crate) fn scope_alone(path: &Path) -> Scope {
    Scope::enter(Discovery {
        path: path.to_owned(),
        found: Found::Alone { why: None },
    })
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

pub(crate) fn open(path: &Path) -> io::Result<ArchiveFile> {
    let discovery = in_use(path).unwrap_or_else(|| Arc::new(discover(path)));
    Ok(match &discovery.found {
        Found::Alone { .. } => ArchiveFile::One(File::open(path)?),
        Found::Set(set) => ArchiveFile::Joined(Joined {
            set: Arc::clone(set),
            position: 0,
            open: Mutex::new(None),
        }),
    })
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
    pub(crate) fn hint(&self) -> Option<String> {
        let first = name(&self.0.path);
        let set = match &self.0.found {
            Found::Alone { why: None } => return None,
            Found::Alone { why: Some(why) } => {
                return Some(format!("{first} was read alone, because {why}"));
            }
            Found::Set(set) => set,
        };
        let count = set.identities.len() as u64;
        let last_path = set.path(count - 1);
        let last = name(&last_path);
        let mut hint = format!("{first} is the first of {count} split volumes, up to {last}");
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
        if set.layout.last == set.layout.volume {
            hint.push_str(&format!(
                "; if they do not form one archive, {} may be missing",
                name(&set.path(count))
            ));
        } else {
            hint.push_str(&format!(
                "; if they do not form one archive, {last} may be truncated or not belong to the set"
            ));
        }
        hint.push_str(", or a volume may be repeated, out of order or damaged");
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
        let layout = &self.set.layout;
        let Some((index, within)) = layout.locate(offset) else {
            return Ok(0);
        };
        let room = layout.size(index) - within;
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

fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::read_at(file, buffer, offset);
    #[cfg(windows)]
    return std::os::windows::fs::FileExt::seek_read(file, buffer, offset);
}

pub(crate) enum ArchiveFile {
    One(File),
    Joined(Joined),
}

impl From<File> for ArchiveFile {
    fn from(file: File) -> Self {
        Self::One(file)
    }
}

impl ArchiveFile {
    pub(crate) fn len(&self) -> io::Result<u64> {
        match self {
            Self::One(file) => Ok(file.metadata()?.len()),
            Self::Joined(joined) => Ok(joined.set.layout.total()),
        }
    }

    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        Ok(match self {
            Self::One(file) => Self::One(file.try_clone()?),
            Self::Joined(joined) => Self::Joined(Joined {
                set: Arc::clone(&joined.set),
                position: joined.position,
                open: Mutex::new(None),
            }),
        })
    }

    pub(crate) fn ensure_ends_at(&self, end: u64) -> io::Result<()> {
        let Self::Joined(joined) = self else {
            return Ok(());
        };
        match joined.set.layout.locate(end) {
            None => Ok(()),
            Some((index, _)) => Err(invalid(format!(
                "split volume {} holds bytes past the archive's end, so it does not belong to the set",
                name(&joined.set.path(index))
            ))),
        }
    }

    pub(crate) fn is_joined(&self) -> bool {
        matches!(self, Self::Joined(_))
    }

    pub(crate) fn reopen(&self, path: &Path) -> io::Result<Self> {
        match self {
            Self::One(_) => Ok(Self::One(File::open(path)?)),
            Self::Joined(_) => self.try_clone(),
        }
    }

    pub(crate) fn read_at(&self, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
        match self {
            Self::One(file) => read_file_at(file, buffer, offset),
            Self::Joined(joined) => joined.read_at(buffer, offset),
        }
    }
}

impl Read for ArchiveFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::One(file) => file.read(buffer),
            Self::Joined(joined) => {
                let read = joined.read_at(buffer, joined.position)?;
                joined.position += read as u64;
                Ok(read)
            }
        }
    }
}

impl Seek for ArchiveFile {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        match self {
            Self::One(file) => file.seek(seek),
            Self::Joined(joined) => {
                let target = match seek {
                    SeekFrom::Start(position) => i128::from(position),
                    SeekFrom::Current(delta) => i128::from(joined.position) + i128::from(delta),
                    SeekFrom::End(delta) => {
                        i128::from(joined.set.layout.total()) + i128::from(delta)
                    }
                };
                joined.position = u64::try_from(target).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid split archive seek")
                })?;
                Ok(joined.position)
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
    fn every_layout_locates_every_offset_as_a_linear_scan_does() {
        for volume in 1..=6u64 {
            for count in 1..=5u64 {
                for last in 1..=volume {
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
        assert_eq!(Layout::new(u64::MAX, 3, 1), None);
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
    fn a_set_is_found_once_per_scope_and_never_shared_across_scopes() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let first = write_set(folder.path(), "once.bin", &[5; 30], 10);
        let joined_set = |file: ArchiveFile| match file {
            ArchiveFile::Joined(joined) => joined.set,
            ArchiveFile::One(_) => panic!("read alone"),
        };
        let outer = scope(&first);
        let held = joined_set(open(&first)?);
        assert!(Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        let inner = scope(&first);
        assert!(!Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        drop(inner);
        assert!(Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        let alone = scope_alone(&first);
        assert!(matches!(open(&first)?, ArchiveFile::One(_)));
        drop(alone);
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
        let Found::Set(set) = discover(&first).found else {
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
        let Found::Set(set) = discover(&first).found else {
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
        let valid = volume > 0 && count > 0 && last > 0 && last <= volume;
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
