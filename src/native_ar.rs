//! In-process extraction of `ar` archives in the System V, GNU and BSD
//! forms, and of the files a Debian package installs. See
//! `docs/reference/ar-reader.md` for what it accepts and restores.
pub use crate::tree::ReadOptions;
use crate::tree::{self, MAX_ENTRIES, MAX_NAME};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check, progress::Tracker};
use anyhow::{Context, Result, ensure};
use std::{
    io::{BufReader, Read, Seek},
    path::Path,
};

const MAGIC: &[u8; 8] = b"!<arch>\n";
const THIN_MAGIC: &[u8; 8] = b"!<thin>\n";
const HEADER: usize = 60;
const HEADER_END: &[u8; 2] = b"`\n";
const COPY_BUFFER: usize = 1024 * 1024;
const MAX_LONG_NAMES: u64 = 16 * 1024 * 1024;
const DEBIAN_BINARY: &str = "debian-binary";
const DEBIAN_DATA: &str = "data.tar.";
const DEBIAN_PLAIN_DATA: &str = "data.tar";
const SPECIAL_MEMBERS: [&[u8]; 3] = [b"/", b"/SYM64/", b"/<ECSYMBOLS>/"];
const BSD_SYMBOL_TABLES: [&str; 4] = [
    "__.SYMDEF",
    "__.SYMDEF SORTED",
    "__.SYMDEF_64",
    "__.SYMDEF_64 SORTED",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    name: [u8; 16],
    modified: Option<u64>,
    mode: Option<u64>,
    size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShortName<'a> {
    Plain(&'a [u8]),
    Slashed(&'a [u8]),
    SymbolTable,
    LongNames,
    GnuLong(u64),
    BsdLong(u64),
}

fn number(field: &[u8], radix: u64, what: &'static str) -> Field<Option<u64>> {
    let end = field
        .iter()
        .rposition(|&byte| byte != b' ')
        .map_or(0, |last| last + 1);
    if end == 0 {
        return Ok(None);
    }
    field[..end]
        .iter()
        .try_fold(0u64, |value, &byte| {
            let digit = u64::from(byte.wrapping_sub(b'0'));
            field_check(digit < radix, what)?;
            value
                .checked_mul(radix)
                .and_then(|value| value.checked_add(digit))
                .ok_or(BadField("ar numeric field overflows"))
        })
        .map(Some)
}

fn decimal(field: &[u8]) -> Field<Option<u64>> {
    number(field, 10, "ar numeric field is not decimal")
}

fn header(bytes: &[u8; HEADER]) -> Field<Header> {
    field_check(
        &bytes[58..60] == HEADER_END,
        "ar member header does not end with its marker",
    )?;
    Ok(Header {
        name: bytes[..16].try_into().unwrap(),
        modified: decimal(&bytes[16..28])?,
        mode: number(&bytes[40..48], 8, "ar mode is not octal")?,
        size: decimal(&bytes[48..58])?.ok_or(BadField("ar member has no size"))?,
    })
}

fn short_name(field: &[u8; 16]) -> Field<ShortName<'_>> {
    let end = field
        .iter()
        .rposition(|&byte| byte != b' ')
        .map_or(0, |last| last + 1);
    let name = &field[..end];
    Ok(match name {
        b"//" => ShortName::LongNames,
        _ if SPECIAL_MEMBERS.contains(&name) => ShortName::SymbolTable,
        [b'#', b'1', b'/', length @ ..] => {
            ShortName::BsdLong(decimal(length)?.ok_or(BadField("ar BSD name has no length"))?)
        }
        [b'/', offset @ ..] => ShortName::GnuLong(
            decimal(offset)?.ok_or(BadField("ar special member is not supported"))?,
        ),
        [] => return Err(BadField("ar member has an empty name")),
        [rest @ .., b'/'] => ShortName::Slashed(rest),
        _ => ShortName::Plain(name),
    })
}

/// Returns whether the file starts with the `ar` signature, plain or thin.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(MAGIC.len());
    crate::volumes::open(path)?
        .take(MAGIC.len() as u64)
        .read_to_end(&mut magic)?;
    Ok(magic == MAGIC || magic == THIN_MAGIC)
}

struct Member {
    name: String,
    slashed: bool,
    data: u64,
    size: u64,
    mode: Option<u64>,
    modified: Option<u64>,
}

struct Members {
    reader: BufReader<ArchiveFile>,
    length: u64,
    next: u64,
    long_names: Option<Vec<u8>>,
    count: usize,
}

impl Members {
    fn open(path: &Path) -> Result<Self> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        let mut reader = BufReader::with_capacity(COPY_BUFFER, file);
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic).context("not an ar archive")?;
        ensure!(
            &magic != THIN_MAGIC,
            "thin ar archives name files outside the archive, which Zipper does not read"
        );
        ensure!(&magic == MAGIC, "not an ar archive");
        Ok(Members {
            reader,
            length,
            next: MAGIC.len() as u64,
            long_names: None,
            count: 0,
        })
    }

    fn exact(&mut self, buffer: &mut [u8], what: &str) -> Result<()> {
        self.reader
            .read_exact(buffer)
            .with_context(|| format!("truncated ar {what}"))
    }

    fn long_name(&self, offset: u64) -> Result<Vec<u8>> {
        let table = self
            .long_names
            .as_deref()
            .context("ar member names a long name before the long name table")?;
        let start = usize::try_from(offset)
            .ok()
            .filter(|&start| start < table.len())
            .context("ar long name lies outside the long name table")?;
        let window = &table[start..table.len().min(start + MAX_NAME as usize + 2)];
        let end = window
            .iter()
            .position(|&byte| byte == b'\n' || byte == 0)
            .with_context(|| {
                if window.len() < table.len() - start {
                    format!("ar long name is longer than {MAX_NAME} bytes")
                } else {
                    "ar long name has no terminator".to_owned()
                }
            })?;
        let name = &table[start..start + end];
        Ok(name.strip_suffix(b"/").unwrap_or(name).to_vec())
    }

    fn next(&mut self) -> Result<Option<Member>> {
        loop {
            if self.next == self.length {
                return Ok(None);
            }
            ensure!(
                self.length - self.next >= HEADER as u64,
                "truncated ar header"
            );
            self.count += 1;
            ensure!(
                self.count <= MAX_ENTRIES,
                "ar has more than {MAX_ENTRIES} members"
            );
            self.seek_to(self.next)?;
            let mut bytes = [0u8; HEADER];
            self.exact(&mut bytes, "header")?;
            let header = header(&bytes)?;
            let mut data = self.next + HEADER as u64;
            let end = data
                .checked_add(header.size)
                .filter(|&end| end <= self.length)
                .context("truncated ar member")?;
            self.next = end + (end % 2).min(self.length - end);
            let mut size = header.size;
            let short = short_name(&header.name)?;
            let name = match short {
                ShortName::SymbolTable => continue,
                ShortName::LongNames => {
                    ensure!(self.long_names.is_none(), "ar has two long name tables");
                    ensure!(
                        size <= MAX_LONG_NAMES,
                        "ar long name table is larger than 16 MiB"
                    );
                    let mut table = vec![0; size as usize];
                    self.exact(&mut table, "long name table")?;
                    self.long_names = Some(table);
                    continue;
                }
                ShortName::GnuLong(offset) => self.long_name(offset)?,
                ShortName::BsdLong(length) => {
                    ensure!(
                        length <= size && length <= MAX_NAME,
                        "ar BSD name length {length} does not fit its member"
                    );
                    let mut name = vec![0; length as usize];
                    self.exact(&mut name, "name")?;
                    while name.pop_if(|byte| *byte == 0).is_some() {}
                    data += length;
                    size -= length;
                    name
                }
                ShortName::Plain(name) | ShortName::Slashed(name) => name.to_vec(),
            };
            let name = String::from_utf8(name)
                .map_err(|_| anyhow::anyhow!("ar member name is not UTF-8"))?;
            if BSD_SYMBOL_TABLES.contains(&name.as_str()) {
                continue;
            }
            return Ok(Some(Member {
                name,
                slashed: matches!(short, ShortName::Slashed(_)),
                data,
                size,
                mode: header.mode,
                modified: header.modified,
            }));
        }
    }

    fn seek_to(&mut self, offset: u64) -> Result<()> {
        let here = self.reader.stream_position()?;
        let step = i64::try_from(i128::from(offset) - i128::from(here))
            .context("ar member offset overflows")?;
        self.reader.seek_relative(step)?;
        Ok(())
    }

    fn data(&mut self, member: &Member) -> Result<std::io::Take<&mut BufReader<ArchiveFile>>> {
        self.seek_to(member.data)?;
        Ok((&mut self.reader).take(member.size))
    }
}

struct Package {
    debian: bool,
    data: Option<Member>,
}

// EXTERNAL(7-Zip ArHandler): a first member named `debian-binary`, without GNU's slash, makes a
// Debian package. 7-Zip hides that member, and opens the one `data.tar.*` member when there is one.
fn package(path: &Path, tracker: &Tracker) -> Result<Package> {
    let mut members = Members::open(path)?;
    let mut debian = false;
    let mut data = Vec::new();
    let mut first = true;
    while let Some(member) = members.next()? {
        tracker.check()?;
        debian |= first && member.name == DEBIAN_BINARY && !member.slashed;
        first = false;
        let holds_data = member.name == DEBIAN_PLAIN_DATA || member.name.starts_with(DEBIAN_DATA);
        if holds_data && data.len() < 2 {
            data.push(member);
        }
    }
    Ok(Package {
        debian,
        data: (debian && data.len() == 1).then(|| data.remove(0)),
    })
}

fn extract_debian_data(
    path: &Path,
    member: &Member,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let data = crate::native_tar::Part::new(path, member.data, member.size);
    ensure!(
        crate::native_tar::sniff_part(&data)?.is_some(),
        "Debian package member {:?} holds no tar Zipper reads",
        member.name
    );
    crate::native_tar::read_part(
        path,
        &data,
        destination,
        crate::native_tar::ReadOptions {
            max_output_bytes: options.max_output_bytes,
            writers: None,
            existing: options.existing,
            paths: options.paths,
            include: options.include,
            exclude: options.exclude,
            strip: options.strip,
            metadata: options.metadata,
            control: options.control,
        },
    )
}

fn parse(tree: &mut tree::Tree, members: &mut Members, debian: bool) -> Result<()> {
    let mut first = true;
    while let Some(member) = members.next()? {
        tree.check()?;
        let skip = debian && first;
        first = false;
        if skip {
            continue;
        }
        let mode = member.mode.unwrap_or(0o644) as u32 & 0o777;
        let modified = member.modified.and_then(|seconds| {
            std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(seconds))
        });
        let mut data = members.data(&member)?;
        tree.file(&member.name, member.size, mode, modified, &mut data)?;
    }
    Ok(())
}

/// Verify and optionally extract an `ar` archive into a new directory. It
/// restores member files with their Unix modes and modification times. A
/// Debian package, whose first member is `debian-binary`, extracts the tar
/// in its one `data.tar.*` member instead, as 7-Zip opens it, through the
/// tar reader.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let package = package(path, &Tracker::new(options.control, None, None))?;
    if let Some(data) = &package.data {
        return extract_debian_data(path, data, destination, options);
    }
    let mut members = Members::open(path)?;
    tree::extract("ar", path, destination, options, |tree| {
        parse(tree, &mut members, package.debian)
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn padded<const N: usize>(text: &[u8]) -> [u8; N] {
        let mut field = [b' '; N];
        field[..text.len()].copy_from_slice(text);
        field
    }

    #[test]
    fn names_resolve_as_each_writer_spells_them() {
        for (field, expected) in [
            (&b"hello.txt/"[..], ShortName::Slashed(b"hello.txt")),
            (b"hello.txt", ShortName::Plain(b"hello.txt")),
            (b"a b/", ShortName::Slashed(b"a b")),
            (b"/", ShortName::SymbolTable),
            (b"/SYM64/", ShortName::SymbolTable),
            (b"//", ShortName::LongNames),
            (b"/0", ShortName::GnuLong(0)),
            (b"/1234", ShortName::GnuLong(1234)),
            (b"#1/20", ShortName::BsdLong(20)),
        ] {
            assert_eq!(short_name(&padded(field)).unwrap(), expected, "{field:?}");
        }
        for field in [&b""[..], b"/x", b"#1/", b"#1/x", b"/12 3", b"/-1"] {
            assert!(short_name(&padded(field)).is_err(), "{field:?}");
        }
    }

    #[test]
    fn numeric_fields_read_back_and_refuse_other_bytes() {
        for value in (0..2000u64).chain([9_999_999_999, 999_999_999_999]) {
            assert_eq!(
                decimal(&padded::<12>(value.to_string().as_bytes())).unwrap(),
                Some(value)
            );
            if value < 8u64.pow(8) {
                assert_eq!(
                    number(&padded::<8>(format!("{value:o}").as_bytes()), 8, "").unwrap(),
                    Some(value)
                );
            }
        }
        assert_eq!(decimal(b"          ").unwrap(), None);
        for bad in [
            &b" 1        "[..],
            b"1 2       ",
            b"-1        ",
            b"+1        ",
            b"1\0        ",
        ] {
            assert!(decimal(bad).is_err(), "{bad:?}");
        }
        assert!(number(b"8       ", 8, "").is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(13)]
    fn headers_never_panic_and_their_sizes_fit_ten_digits() {
        let bytes: [u8; HEADER] = kani::any();
        if let Ok(header) = header(&bytes) {
            assert!(header.size < 10u64.pow(10));
            assert!(header.modified.is_none_or(|time| time < 10u64.pow(12)));
            assert!(header.mode.is_none_or(|mode| mode < 8u64.pow(8)));
        }
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn names_never_panic_and_long_name_lengths_fit_their_field() {
        let field: [u8; 16] = kani::any();
        match short_name(&field) {
            Ok(ShortName::BsdLong(length)) => assert!(length < 10u64.pow(13)),
            Ok(ShortName::GnuLong(offset)) => assert!(offset < 10u64.pow(15)),
            Ok(ShortName::Plain(name) | ShortName::Slashed(name)) => {
                assert!(!name.is_empty() && name.len() <= 16)
            }
            _ => {}
        }
    }

    #[kani::proof]
    #[kani::unwind(11)]
    fn every_size_reads_back_from_its_decimal_field() {
        let value: u64 = kani::any();
        kani::assume(value < 10u64.pow(10));
        let mut field = [b' '; 10];
        let mut rest = value;
        let mut digits = [0u8; 10];
        let mut count = 0;
        loop {
            digits[count] = b'0' + (rest % 10) as u8;
            count += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        for index in 0..count {
            field[index] = digits[count - 1 - index];
        }
        assert!(matches!(decimal(&field), Ok(Some(read)) if read == value));
    }
}
