use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::{self, BufRead, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const FIRST_VOLUME: u64 = 1;
const MIN_NUMBER_DIGITS: usize = 3;
const MAX_NUMBER_DIGITS: usize = 19;
const REPEAT_MIN_BYTES: u64 = 64;
const REPEAT_SCAN_BYTES: u64 = 1 << 30;

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

struct Volume {
    path: PathBuf,
    identity: Identity,
}

fn name(path: &Path) -> std::path::Display<'_> {
    Path::new(path.file_name().unwrap_or(path.as_os_str())).display()
}

pub(crate) struct VolumeSet {
    volumes: Vec<Volume>,
    layout: Layout,
}

impl VolumeSet {
    fn open_volume(&self, index: u64) -> io::Result<File> {
        let volume = &self.volumes[index as usize];
        let file = File::open(&volume.path)?;
        if Identity::of(&file.metadata()?) != volume.identity {
            return Err(invalid(format!(
                "split volume {} changed while it was read",
                name(&volume.path)
            )));
        }
        Ok(file)
    }
}

struct Numbered {
    folder: PathBuf,
    stem: OsString,
    digits: usize,
    number: u64,
}

impl Numbered {
    fn parse(path: &Path) -> Option<Self> {
        let digits = path.extension()?.to_str()?;
        if !(MIN_NUMBER_DIGITS..=MAX_NUMBER_DIGITS).contains(&digits.len())
            || !digits.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            folder: path.parent().unwrap_or(Path::new("")).to_owned(),
            stem: path.file_stem()?.to_owned(),
            digits: digits.len(),
            number: digits.parse().ok()?,
        })
    }

    fn path(&self, number: u64) -> PathBuf {
        let mut name = self.stem.clone();
        name.push(format!(".{number:0width$}", width = self.digits));
        self.folder.join(name)
    }

    fn highest_present(&self) -> io::Result<u64> {
        let folder = if self.folder.as_os_str().is_empty() {
            Path::new(".")
        } else {
            &self.folder
        };
        let mut highest = 0;
        for entry in std::fs::read_dir(folder)? {
            let file_name = entry?.file_name();
            if let Some(other) = Numbered::parse(Path::new(&file_name))
                && other.stem == self.stem
                && other.digits >= self.digits
            {
                highest = highest.max(other.number);
            }
        }
        Ok(highest)
    }
}

#[derive(Clone)]
pub(crate) enum Source {
    File(PathBuf),
    Volumes(Arc<VolumeSet>),
}

impl Source {
    pub(crate) fn new(path: &Path) -> io::Result<Self> {
        let Some(numbered) = Numbered::parse(path).filter(|numbered| numbered.number > 0) else {
            return Ok(Self::File(path.to_owned()));
        };
        if numbered.number != FIRST_VOLUME {
            let first = numbered.path(FIRST_VOLUME);
            if first.is_file() {
                return Err(invalid(format!(
                    "{} is a later volume of a split archive; open its first volume, {}",
                    name(path),
                    name(&first)
                )));
            }
            return Ok(Self::File(path.to_owned()));
        }
        let mut volumes = Vec::new();
        loop {
            let number = FIRST_VOLUME + volumes.len() as u64;
            let path = numbered.path(number);
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound && number > FIRST_VOLUME => {
                    break;
                }
                Err(error) => return Err(error),
            };
            if !metadata.is_file() {
                return Err(invalid(format!(
                    "split volume {} is not a file",
                    name(&path)
                )));
            }
            volumes.push(Volume {
                path,
                identity: Identity::of(&metadata),
            });
        }
        let count = volumes.len() as u64;
        let highest = numbered.highest_present()?;
        if highest > count {
            return Err(invalid(format!(
                "split volume {} is missing, but {} is present",
                name(&numbered.path(count + 1)),
                name(&numbered.path(highest))
            )));
        }
        let first = &volumes[0];
        let volume = first.identity.length;
        if volume == 0 {
            return Err(invalid(format!(
                "split volume {} is empty",
                name(&first.path)
            )));
        }
        for (index, later) in volumes.iter().enumerate().skip(1) {
            let length = later.identity.length;
            let last = index + 1 == volumes.len();
            if length > volume {
                return Err(invalid(format!(
                    "split volume {} holds {length} bytes, more than the {volume} of {}; every volume after the first must match its size, and only the last may be shorter",
                    name(&later.path),
                    name(&first.path)
                )));
            }
            if length == 0 {
                return Err(invalid(format!(
                    "split volume {} is empty",
                    name(&later.path)
                )));
            }
            if !last && length < volume {
                let next = name(&volumes[index + 1].path);
                return Err(invalid(format!(
                    "split volume {} holds {length} bytes, fewer than the {volume} of {}, but {next} follows it; only the last volume may be shorter, so {} is truncated or {next} does not belong to the set",
                    name(&later.path),
                    name(&first.path),
                    name(&later.path),
                )));
            }
        }
        let last = volumes[volumes.len() - 1].identity.length;
        let layout = Layout::new(volume, count, last)
            .ok_or_else(|| invalid("split archive is larger than 16 EiB".to_owned()))?;
        Ok(Self::Volumes(Arc::new(VolumeSet { volumes, layout })))
    }

    pub(crate) fn open(&self) -> io::Result<ArchiveFile> {
        Ok(match self {
            Self::File(path) => ArchiveFile::One(File::open(path)?),
            Self::Volumes(set) => ArchiveFile::Joined(Joined {
                set: Arc::clone(set),
                position: 0,
                open: Mutex::new(None),
            }),
        })
    }
}

pub(crate) fn open(path: &Path) -> io::Result<ArchiveFile> {
    Source::new(path)?.open()
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

fn repeated_volume(set: &VolumeSet) -> io::Result<Option<(usize, usize)>> {
    if set.layout.total() > REPEAT_SCAN_BYTES {
        return Ok(None);
    }
    let mut seen: std::collections::HashMap<(u64, u32), Vec<usize>> = Default::default();
    for (index, volume) in set.volumes.iter().enumerate() {
        if volume.identity.length < REPEAT_MIN_BYTES {
            continue;
        }
        let mut hasher = crc32fast::Hasher::new();
        io::copy(&mut File::open(&volume.path)?, &mut HashWriter(&mut hasher))?;
        let earlier = seen
            .entry((volume.identity.length, hasher.finalize()))
            .or_default();
        for &other in earlier.iter() {
            if same_bytes(&set.volumes[other].path, &volume.path)? {
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

pub(crate) fn failure_hint(path: &Path) -> Option<String> {
    let Ok(Source::Volumes(set)) = Source::new(path) else {
        return None;
    };
    let numbered = Numbered::parse(path)?;
    let count = set.volumes.len();
    let first = name(&set.volumes[0].path);
    let last = name(&set.volumes[count - 1].path);
    let mut hint = format!("{first} is the first of {count} split volumes, up to {last}");
    if let Ok(Some((earlier, later))) = repeated_volume(&set) {
        hint.push_str(&format!(
            "; {} holds the same bytes as {}",
            name(&set.volumes[later].path),
            name(&set.volumes[earlier].path)
        ));
    }
    if set.layout.last == set.layout.volume {
        hint.push_str(&format!(
            "; if they do not form one archive, {} may be missing",
            name(&numbered.path(count as u64 + 1))
        ));
    } else {
        hint.push_str(&format!(
            "; if they do not form one archive, {last} may be truncated or not belong to the set"
        ));
    }
    hint.push_str(", or a volume may be repeated, out of order or damaged");
    Some(hint)
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
                name(&self.set.volumes[index as usize].path)
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
                name(&joined.set.volumes[index as usize].path)
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
