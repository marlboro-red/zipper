use crate::blake2sp::Blake2sp;
use crate::rar5_crypto::Derived;
use crate::volumes::{ArchiveFile, Unchanged};
use anyhow::{Context, Result, bail};
use std::{
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    rc::Rc,
};

pub(crate) const MOST_VOLUMES: u64 = 65_536;
const PART_BUFFER: usize = 1024 * 1024;
const OLD_STYLE_LETTERS: u64 = (b'z' - b'r' + 1) as u64;
const OLD_STYLE_NUMBERS: u64 = 100;
const HASH_BYTES: usize = 32;

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Names {
    Parts {
        folder: PathBuf,
        before: String,
        first: u64,
        width: usize,
        after: String,
    },
    OldStyle {
        folder: PathBuf,
        stem: String,
        upper: bool,
    },
}

fn split_extension(path: &Path) -> Option<(PathBuf, &str, &str)> {
    let name = path.file_name()?.to_str()?;
    let (stem, extension) = name.rsplit_once('.')?;
    Some((
        path.parent().unwrap_or(Path::new("")).to_owned(),
        stem,
        extension,
    ))
}

fn is_old_style_later(extension: &str) -> bool {
    let bytes = extension.to_ascii_lowercase().into_bytes();
    matches!(bytes.as_slice(), [b'r'..=b'z', b'0'..=b'9', b'0'..=b'9'])
}

impl Names {
    pub(crate) fn new_style(path: &Path) -> Option<Self> {
        let (folder, stem, extension) = split_extension(path)?;
        if !extension.eq_ignore_ascii_case("rar") {
            return None;
        }
        let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let (before, number) = stem.split_at(stem.len() - digits);
        let marked = before.len() >= ".part".len()
            && before[before.len() - ".part".len()..].eq_ignore_ascii_case(".part");
        let first: u64 = number.parse().ok()?;
        (marked && first > 0).then(|| Names::Parts {
            folder,
            before: before.to_owned(),
            first,
            width: digits,
            after: format!(".{extension}"),
        })
    }

    pub(crate) fn old_style(path: &Path) -> Option<Self> {
        let (folder, stem, extension) = split_extension(path)?;
        let named = extension.eq_ignore_ascii_case("rar") || is_old_style_later(extension);
        (named && !stem.is_empty()).then(|| Names::OldStyle {
            folder,
            stem: stem.to_owned(),
            upper: extension.starts_with(|c: char| c.is_ascii_uppercase()),
        })
    }

    fn name(&self, volume: u64) -> Option<String> {
        match self {
            Names::Parts {
                before,
                first,
                width,
                after,
                ..
            } => Some(format!(
                "{before}{:0width$}{after}",
                first.checked_add(volume)?
            )),
            Names::OldStyle { stem, upper, .. } if volume == 0 => {
                Some(format!("{stem}.{}", if *upper { "RAR" } else { "rar" }))
            }
            Names::OldStyle { stem, upper, .. } => {
                let later = volume - 1;
                if later >= OLD_STYLE_LETTERS * OLD_STYLE_NUMBERS {
                    return None;
                }
                let letter = char::from(b'r' + (later / OLD_STYLE_NUMBERS) as u8);
                let letter = match upper {
                    true => letter.to_ascii_uppercase(),
                    false => letter,
                };
                Some(format!("{stem}.{letter}{:02}", later % OLD_STYLE_NUMBERS))
            }
        }
    }

    fn folder(&self) -> &Path {
        match self {
            Names::Parts { folder, .. } | Names::OldStyle { folder, .. } => folder,
        }
    }

    pub(crate) fn path(&self, volume: u64) -> Result<PathBuf> {
        if volume >= MOST_VOLUMES {
            bail!("RAR set has more than {MOST_VOLUMES} volumes, Zipper's limit");
        }
        let name = self.name(volume).with_context(|| {
            format!("RAR set has more than {volume} volumes, more than its names can number")
        })?;
        Ok(self.folder().join(name))
    }

    pub(crate) fn first_name(&self) -> String {
        match self {
            Names::Parts {
                before,
                width,
                after,
                ..
            } => format!("{before}{:0width$}{after}", 1),
            Names::OldStyle { .. } => self.name(0).expect("volume 0 has a name"),
        }
    }
}

pub(crate) fn later_volume(path: &Path, names: Option<&Names>, number: Option<u64>) -> String {
    let which = match number {
        Some(number) => format!("volume {} of a RAR set", number.saturating_add(1)),
        None => "a later volume of a RAR set".to_owned(),
    };
    match names {
        Some(names) => format!(
            "{} is {which}; open its first volume, {}, to read the set",
            path.display(),
            names.first_name()
        ),
        None => format!(
            "{} is {which}; open its first volume to read the set",
            path.display()
        ),
    }
}

pub(crate) fn open(
    names: &Names,
    volume: u64,
    before: &Path,
) -> Result<(PathBuf, ArchiveFile, Unchanged)> {
    let path = names.path(volume)?;
    let (file, unchanged) = crate::volumes::open_alone(&path).with_context(|| {
        format!(
            "RAR volume {} is missing or cannot be read, and {} says more volumes follow",
            file_name(&path),
            file_name(before)
        )
    })?;
    Ok((path, file, unchanged))
}

#[derive(Default)]
pub(crate) struct PartCheck {
    pub(crate) crc: Option<u32>,
    pub(crate) blake: Option<[u8; HASH_BYTES]>,
    pub(crate) tweak: Option<Rc<Derived>>,
}

pub(crate) struct Part {
    pub(crate) path: Option<PathBuf>,
    pub(crate) reopen: Option<Unchanged>,
    pub(crate) offset: u64,
    pub(crate) size: u64,
    pub(crate) check: Option<PartCheck>,
}

pub(crate) struct Span {
    parts: Vec<Part>,
    pub(crate) packed: u64,
}

impl Span {
    pub(crate) fn one(size: u64) -> Self {
        Span {
            parts: vec![Part {
                path: None,
                reopen: None,
                offset: 0,
                size,
                check: None,
            }],
            packed: size,
        }
    }

    pub(crate) fn starting(part: Part) -> Self {
        Span {
            packed: part.size,
            parts: vec![part],
        }
    }

    pub(crate) fn push(&mut self, part: Part) -> Result<()> {
        self.packed = self
            .packed
            .checked_add(part.size)
            .context("RAR entry packs more than 16 EiB across its volumes")?;
        self.parts.push(part);
        Ok(())
    }
}

enum Source<'a> {
    Open(io::Take<&'a mut BufReader<ArchiveFile>>),
    Reopened(io::Take<Box<BufReader<ArchiveFile>>>),
}

impl Source<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Source::Open(area) => area.read(buffer),
            Source::Reopened(area) => area.read(buffer),
        }
    }

    fn left(&self) -> u64 {
        match self {
            Source::Open(area) => area.limit(),
            Source::Reopened(area) => area.limit(),
        }
    }
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

struct Split {
    rest: std::vec::IntoIter<Part>,
    path: Option<PathBuf>,
    check: Option<PartCheck>,
    crc: crc32fast::Hasher,
    blake: Option<Blake2sp>,
    entry: String,
}

impl Split {
    fn volume(&self) -> String {
        self.path.as_deref().map(file_name).unwrap_or_default()
    }

    fn update(&mut self, bytes: &[u8]) {
        if self.check.is_some() {
            self.crc.update(bytes);
            if let Some(blake) = &mut self.blake {
                blake.update(bytes);
            }
        }
    }

    fn finish_part(&mut self, format: &str) -> io::Result<()> {
        let Some(check) = self.check.take() else {
            return Ok(());
        };
        let crc = std::mem::take(&mut self.crc).finalize();
        let crc = match &check.tweak {
            Some(derived) => derived.tweaked_crc(crc),
            None => crc,
        };
        if check.crc.is_some_and(|expected| expected != crc) {
            return Err(invalid(format!(
                "{format} entry {:?} fails the CRC-32 of its part in volume {}",
                self.entry,
                self.volume()
            )));
        }
        if let (Some(blake), Some(expected)) = (self.blake.take(), check.blake) {
            let hash = blake.finish();
            let hash = match &check.tweak {
                Some(derived) => derived.tweaked_hash(&hash),
                None => hash,
            };
            if hash != expected {
                return Err(invalid(format!(
                    "{format} entry {:?} fails the BLAKE2sp of its part in volume {}",
                    self.entry,
                    self.volume()
                )));
            }
        }
        Ok(())
    }

    fn start(&mut self, part: Part) -> io::Result<Source<'static>> {
        let path = part.path.expect("a later part names its volume");
        let reopen = part
            .reopen
            .expect("a later part comes from a volume opened alone");
        let mut file = crate::volumes::reopen_unchanged(&path, reopen)?;
        file.seek(SeekFrom::Start(part.offset))?;
        let reader = Box::new(BufReader::with_capacity(PART_BUFFER, file));
        self.path = Some(path);
        self.blake = part
            .check
            .as_ref()
            .and_then(|check| check.blake.map(|_| Blake2sp::new()));
        self.check = part.check;
        Ok(Source::Reopened(reader.take(part.size)))
    }
}

pub(crate) struct Parts<'a> {
    source: Source<'a>,
    format: &'static str,
    split: Option<Box<Split>>,
}

impl<'a> Parts<'a> {
    pub(crate) fn new(
        open: &'a mut BufReader<ArchiveFile>,
        span: Span,
        format: &'static str,
        entry: &str,
    ) -> Self {
        let split = span.parts.len() > 1;
        let mut rest = span.parts.into_iter();
        let first = rest.next().expect("a span has its first part");
        Parts {
            source: Source::Open(open.take(first.size)),
            format,
            split: split.then(|| {
                Box::new(Split {
                    rest,
                    path: first.path,
                    blake: first
                        .check
                        .as_ref()
                        .and_then(|check| check.blake.map(|_| Blake2sp::new())),
                    check: first.check,
                    crc: crc32fast::Hasher::new(),
                    entry: entry.to_owned(),
                })
            }),
        }
    }
}

impl Read for Parts<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            let read = self.source.read(buffer)?;
            if let Some(split) = &mut self.split {
                split.update(&buffer[..read]);
            }
            if read > 0 {
                return Ok(read);
            }
            let Some(split) = &mut self.split else {
                return match self.source.left() {
                    0 => Ok(0),
                    _ => Err(invalid(format!("{} entry data is cut short", self.format))),
                };
            };
            if self.source.left() > 0 {
                return Err(invalid(format!(
                    "{} entry {:?} is cut short in volume {}",
                    self.format,
                    split.entry,
                    split.volume()
                )));
            }
            split.finish_part(self.format)?;
            match split.rest.next() {
                Some(part) => self.source = split.start(part)?,
                None => return Ok(0),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_of(names: &Names, volumes: impl IntoIterator<Item = u64>) -> Vec<String> {
        volumes
            .into_iter()
            .map(|volume| file_name(&names.path(volume).unwrap()))
            .collect()
    }

    #[test]
    fn new_style_names_keep_the_width_and_spelling_of_the_first() {
        let names = Names::new_style(Path::new("dir/set.part01.rar")).unwrap();
        assert_eq!(
            names_of(&names, [0, 1, 8, 9, 98, 99]),
            [
                "set.part01.rar",
                "set.part02.rar",
                "set.part09.rar",
                "set.part10.rar",
                "set.part99.rar",
                "set.part100.rar"
            ]
        );
        assert_eq!(names.path(1).unwrap().parent(), Some(Path::new("dir")));
        let upper = Names::new_style(Path::new("A.B.PART1.RAR")).unwrap();
        assert_eq!(names_of(&upper, [1]), ["A.B.PART2.RAR"]);
        assert_eq!(upper.first_name(), "A.B.PART1.RAR");
        let third = Names::new_style(Path::new("set.part003.rar")).unwrap();
        assert_eq!(third.first_name(), "set.part001.rar");
        for refused in [
            "set.rar",
            "set.part.rar",
            "set.part0.rar",
            "set.part1.zip",
            "set1.rar",
            "set.part99999999999999999999.rar",
        ] {
            assert_eq!(Names::new_style(Path::new(refused)), None, "{refused}");
        }
    }

    #[test]
    fn old_style_names_run_from_r00_to_z99() {
        let names = Names::old_style(Path::new("set.rar")).unwrap();
        assert_eq!(
            names_of(&names, [0, 1, 2, 100, 101, 900]),
            [
                "set.rar", "set.r00", "set.r01", "set.r99", "set.s00", "set.z99"
            ]
        );
        assert!(names.path(901).is_err());
        let upper = Names::old_style(Path::new("SET.RAR")).unwrap();
        assert_eq!(
            names_of(&upper, [0, 1, 101]),
            ["SET.RAR", "SET.R00", "SET.S00"]
        );
        assert_eq!(Names::old_style(Path::new(".rar")), None);
        assert_eq!(Names::old_style(Path::new("set.zip")), None);
        assert_eq!(Names::old_style(Path::new("set.r0")), None);
        let later = Names::old_style(Path::new("set.s07")).unwrap();
        assert_eq!(later.first_name(), "set.rar");
    }

    #[test]
    fn no_set_is_named_past_the_volume_limit() {
        let names = Names::new_style(Path::new("set.part1.rar")).unwrap();
        assert!(names.path(MOST_VOLUMES - 1).is_ok());
        let error = names.path(MOST_VOLUMES).unwrap_err();
        assert!(format!("{error}").contains("65536 volumes"), "{error}");
    }
}
