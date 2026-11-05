use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::{self, BufRead, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
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
        if format!("{number:03}") != digits || matches!(stem.to_str(), Some("" | "." | "..")) {
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
        let path = self.path(index);
        let file = File::open(&path)?;
        if Identity::of(&file.metadata()?) != self.identities[index as usize] {
            return Err(invalid(format!(
                "split volume {} changed while it was read",
                name(&path)
            )));
        }
        Ok(file)
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

static DISCOVERED: Mutex<Vec<Weak<Discovery>>> = Mutex::new(Vec::new());

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

fn discover(path: &Path) -> io::Result<Arc<Discovery>> {
    let mut discovered = DISCOVERED
        .lock()
        .map_err(|_| io::Error::other("split volume registry poisoned"))?;
    discovered.retain(|entry| entry.strong_count() > 0);
    if let Some(known) = discovered
        .iter()
        .filter_map(Weak::upgrade)
        .find(|known| known.path == path)
    {
        return Ok(known);
    }
    let found = match Numbered::parse(path) {
        None => Found::Alone { why: None },
        Some(numbered) if numbered.number == FIRST_VOLUME => find_set(numbered),
        Some(numbered) => {
            let first = numbered.path(FIRST_VOLUME);
            if first.is_file() {
                return Err(invalid(format!(
                    "{} is a later volume of a split archive; open its first volume, {}",
                    name(path),
                    name(&first)
                )));
            }
            Found::Alone { why: None }
        }
    };
    let discovery = Arc::new(Discovery {
        path: path.to_owned(),
        found,
    });
    discovered.push(Arc::downgrade(&discovery));
    Ok(discovery)
}

pub(crate) struct Scope(Arc<Discovery>);

pub(crate) fn scope(path: &Path) -> io::Result<Scope> {
    discover(path).map(Scope)
}

pub(crate) fn open(path: &Path) -> io::Result<ArchiveFile> {
    let discovery = discover(path)?;
    Ok(match &discovery.found {
        Found::Alone { .. } => ArchiveFile::One(File::open(path)?),
        Found::Set(set) => ArchiveFile::Joined(Joined {
            set: Arc::clone(set),
            _discovery: Arc::clone(&discovery),
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

fn same_bytes(left: &Path, right: &Path) -> io::Result<bool> {
    let (mut left, mut right) = (
        io::BufReader::new(File::open(left)?),
        io::BufReader::new(File::open(right)?),
    );
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

fn repeated_volume(set: &VolumeSet) -> io::Result<Option<(u64, u64)>> {
    if set.layout.total() > REPEAT_SCAN_BYTES {
        return Ok(None);
    }
    let mut seen: std::collections::HashMap<(u64, u32), Vec<u64>> = Default::default();
    for (index, identity) in (0..).zip(&set.identities) {
        if identity.length < REPEAT_MIN_BYTES {
            continue;
        }
        let mut hasher = crc32fast::Hasher::new();
        io::copy(
            &mut File::open(set.path(index))?,
            &mut HashWriter(&mut hasher),
        )?;
        let earlier = seen
            .entry((identity.length, hasher.finalize()))
            .or_default();
        for &other in earlier.iter() {
            if same_bytes(&set.path(other), &set.path(index))? {
                return Ok(Some((other, index)));
            }
        }
        earlier.push(index);
    }
    Ok(None)
}

struct HashWriter<'a>(&'a mut crc32fast::Hasher);

impl io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
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
        if let Ok(Some((earlier, later))) = repeated_volume(set) {
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
    _discovery: Arc<Discovery>,
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
                _discovery: Arc::clone(&joined._discovery),
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
    fn a_set_is_found_once_while_anything_holds_it() -> io::Result<()> {
        let folder = tempfile::tempdir()?;
        let first = write_set(folder.path(), "once.bin", &[5; 30], 10);
        let joined_set = |file: ArchiveFile| match file {
            ArchiveFile::Joined(joined) => joined.set,
            ArchiveFile::One(_) => panic!("read alone"),
        };
        let scope = scope(&first)?;
        let held = joined_set(open(&first)?);
        assert!(Arc::ptr_eq(&held, &joined_set(open(&first)?)));
        drop(scope);
        assert!(!Arc::ptr_eq(&held, &joined_set(open(&first)?)));
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
