//! In-process extraction of RAR 5 archives, stored and compressed, solid or
//! not, and of RAR 4 archives that are not solid. See
//! `docs/reference/rar-reader.md` for what it accepts and restores.
use crate::blake2sp::Blake2sp;
use crate::rar_volumes::{self, Names, Part, PartCheck, Parts, Span};
use crate::rar5_crypto::{
    self, BLOCK, CHECK_BYTES, CbcReader, CheckValue, Derived, IV_BYTES, KeyError, Keys,
    MOST_KDF_COUNT, SALT_BYTES,
};
use crate::rar5_lz::{self, Stream};
pub use crate::tree::ReadOptions;
use crate::tree::{self, MAX_NAME};
use crate::volumes::{ArchiveFile, Unchanged};
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{self, BufReader, Read, Seek},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, SystemTime},
};

const RAR5_SIGNATURE: [u8; 8] = *b"Rar!\x1A\x07\x01\x00";
const COPY_BUFFER: usize = 1024 * 1024;
const LONGEST_NAME: u64 = 8192;
const LONGEST_VINT: usize = 10;
const LONGEST_SIZE_VINT: usize = 3;
const LARGEST_HEADER: u64 = (1 << 21) - 1;
const HASH_BYTES: usize = 32;

const MAIN_HEADER: u64 = 1;
const FILE_HEADER: u64 = 2;
const SERVICE_HEADER: u64 = 3;
const ENCRYPTION_HEADER: u64 = 4;
const END_HEADER: u64 = 5;

const HAS_EXTRA: u64 = 0x0001;
const HAS_DATA: u64 = 0x0002;
const SPLIT_BEFORE: u64 = 0x0008;
const SPLIT_AFTER: u64 = 0x0010;

const VOLUME: u64 = 0x0001;
const VOLUME_NUMBER: u64 = 0x0002;
const SOLID_ARCHIVE: u64 = 0x0004;
const NOT_LAST_VOLUME: u64 = 0x0001;

const DIRECTORY: u64 = 0x0001;
const UNIX_MTIME: u64 = 0x0002;
const HAS_CRC: u64 = 0x0004;
const UNKNOWN_SIZE: u64 = 0x0008;

const SOLID_FILE: u64 = 0x0040;
const VERSION_MASK: u64 = 0x3F;
const METHOD_SHIFT: u32 = 7;
const METHOD_MASK: u64 = 7;
const DICTIONARY_SHIFT: u32 = 10;
const DICTIONARY_MASK: u64 = 0x1F;
const FRACTION_SHIFT: u32 = 15;
const FRACTION_MASK: u64 = 0x1F;
const OLD_ALGORITHM_IN_NEW_FORMAT: u64 = 0x10_0000;
const SMALLEST_DICTIONARY: u64 = 128 * 1024;

const HOST_WINDOWS: u64 = 0;
const HOST_UNIX: u64 = 1;
const READ_ONLY: u64 = 0x01;
const FILE_MODE: u32 = 0o644;
const READ_ONLY_MODE: u32 = 0o444;
const DIRECTORY_MODE: u32 = 0o755;

const RECORD_ENCRYPTION: u64 = 1;
const RECORD_HASH: u64 = 2;
const RECORD_TIME: u64 = 3;
const RECORD_VERSION: u64 = 4;
const RECORD_REDIRECT: u64 = 5;
const HASH_BLAKE2SP: u64 = 0;

const AES_256: u64 = 0;
const PASSWORD_CHECK: u64 = 0x0001;
const TWEAKED_CHECKSUMS: u64 = 0x0002;

const TIME_UNIX: u64 = 0x01;
const TIME_MODIFIED: u64 = 0x02;
const TIME_CREATED: u64 = 0x04;
const TIME_ACCESSED: u64 = 0x08;
const TIME_NANOSECONDS: u64 = 0x10;

const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
const FILETIME_TO_UNIX_SECONDS: u64 = 11_644_473_600;

/// The error when a RAR 5 archive needs a password and none was given.
#[derive(Debug)]
pub struct PasswordNeeded {
    /// Whether the headers are encrypted, so that even the names need it.
    pub header: bool,
}

impl std::fmt::Display for PasswordNeeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str(if self.header {
            "RAR archive has encrypted headers; give its password with --password"
        } else {
            "RAR archive is encrypted; give its password with --password"
        })
    }
}

impl std::error::Error for PasswordNeeded {}

struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Fields { bytes, at: 0 }
    }

    // SPEC(RAR 5 technote, vint): 7 bits a byte, low bits first, with the
    // high bit set on every byte but the last, in at most 10 bytes.
    fn vint(&mut self) -> Field<u64> {
        let mut value = 0u64;
        for index in 0..LONGEST_VINT {
            let byte = *self
                .bytes
                .get(self.at)
                .ok_or(BadField("truncated RAR 5 number"))?;
            self.at += 1;
            let bits = u64::from(byte & 0x7F);
            let shift = 7 * index as u32;
            field_check(
                shift < 63 || bits >> (64 - shift) == 0,
                "RAR 5 number does not fit 64 bits",
            )?;
            value |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(BadField("RAR 5 number is longer than 10 bytes"))
    }

    fn take(&mut self, count: u64) -> Field<&'a [u8]> {
        let end = usize::try_from(count)
            .ok()
            .and_then(|count| self.at.checked_add(count))
            .filter(|&end| end <= self.bytes.len())
            .ok_or(BadField("RAR 5 field runs past its header"))?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn range(&mut self, count: u64) -> Field<Range<usize>> {
        let start = self.at;
        self.take(count)?;
        Ok(start..self.at)
    }

    fn u32(&mut self) -> Field<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Field<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn rest(&self) -> usize {
        self.bytes.len() - self.at
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Base {
    kind: u64,
    flags: u64,
    data_size: u64,
    fields: Range<usize>,
    extra: Range<usize>,
}

fn base_header(body: &[u8]) -> Field<Base> {
    let mut fields = Fields::new(body);
    let kind = fields.vint()?;
    let flags = fields.vint()?;
    let extra_size = if flags & HAS_EXTRA != 0 {
        fields.vint()?
    } else {
        0
    };
    let data_size = if flags & HAS_DATA != 0 {
        fields.vint()?
    } else {
        0
    };
    field_check(
        extra_size <= fields.rest() as u64,
        "RAR 5 extra area runs past its header",
    )?;
    let extra_start = body.len() - extra_size as usize;
    Ok(Base {
        kind,
        flags,
        data_size,
        fields: fields.at..extra_start,
        extra: extra_start..body.len(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Main {
    flags: u64,
    number: Option<u64>,
}

// SPEC(RAR 5 technote, main archive header): archive flags, then a volume
// number when its flag is set, absent in the first volume.
fn main_header(fields: &[u8]) -> Field<Main> {
    let mut fields = Fields::new(fields);
    let flags = fields.vint()?;
    let number = if flags & VOLUME_NUMBER != 0 {
        Some(fields.vint()?)
    } else {
        None
    };
    Ok(Main { flags, number })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Redirect {
    UnixSymlink,
    WindowsSymlink,
    Junction,
    HardLink,
    FileCopy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Encryption {
    count: u8,
    salt: [u8; SALT_BYTES],
    iv: [u8; IV_BYTES],
    check: Option<[u8; CHECK_BYTES]>,
    tweaked: bool,
}

// SPEC(RAR 5 technote, file encryption record and archive encryption header): version, flags,
// KDF count, salt, an IV in the file record only, then the check value when flag 0x0001 is set.
fn encryption(fields: &mut Fields, with_iv: bool) -> Field<Encryption> {
    field_check(
        fields.vint()? == AES_256,
        "RAR 5 encryption version is unknown",
    )?;
    let flags = fields.vint()?;
    let count = fields.take(1)?[0];
    field_check(
        count <= MOST_KDF_COUNT,
        "RAR 5 key derivation count is above 24, Zipper's limit",
    )?;
    let salt = fields.take(SALT_BYTES as u64)?.try_into().unwrap();
    let iv = if with_iv {
        fields.take(IV_BYTES as u64)?.try_into().unwrap()
    } else {
        [0; IV_BYTES]
    };
    let check = if flags & PASSWORD_CHECK != 0 {
        Some(fields.take(CHECK_BYTES as u64)?.try_into().unwrap())
    } else {
        None
    };
    Ok(Encryption {
        count,
        salt,
        iv,
        check,
        tweaked: flags & TWEAKED_CHECKSUMS != 0,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Extras {
    encryption: Option<Encryption>,
    hash: Option<[u8; HASH_BYTES]>,
    modified: Option<Time>,
    version: Option<u64>,
    redirect: Option<(Redirect, Range<usize>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Time {
    Unix { seconds: u32, nanoseconds: u32 },
    Windows(u64),
}

fn time_record(fields: &mut Fields) -> Field<Option<Time>> {
    let flags = fields.vint()?;
    let unix = flags & TIME_UNIX != 0;
    let mut modified = None;
    for (flag, keep) in [
        (TIME_MODIFIED, true),
        (TIME_CREATED, false),
        (TIME_ACCESSED, false),
    ] {
        if flags & flag == 0 {
            continue;
        }
        let time = if unix {
            Time::Unix {
                seconds: fields.u32()?,
                nanoseconds: 0,
            }
        } else {
            Time::Windows(fields.u64()?)
        };
        if keep {
            modified = Some(time);
        }
    }
    if unix && flags & TIME_NANOSECONDS != 0 {
        for (flag, keep) in [
            (TIME_MODIFIED, true),
            (TIME_CREATED, false),
            (TIME_ACCESSED, false),
        ] {
            if flags & flag == 0 {
                continue;
            }
            let nanoseconds = fields.u32()?;
            if let (
                true,
                Some(Time::Unix {
                    nanoseconds: kept, ..
                }),
            ) = (keep, modified.as_mut())
            {
                *kept = nanoseconds;
            }
        }
    }
    Ok(modified)
}

// SPEC(RAR 5 technote, extra area format): records of a size, which counts
// the type and data, a type and data. Unknown types are skipped.
fn extras(header: &[u8], area: Range<usize>) -> Field<Extras> {
    let mut found = Extras::default();
    let mut outer = Fields {
        bytes: header
            .get(..area.end)
            .ok_or(BadField("RAR 5 extra area runs past its header"))?,
        at: area.start,
    };
    while outer.rest() > 0 {
        let size = outer.vint()?;
        let record = outer.range(size)?;
        let mut fields = Fields {
            bytes: &header[..record.end],
            at: record.start,
        };
        let kind = fields.vint()?;
        match kind {
            RECORD_ENCRYPTION => found.encryption = Some(encryption(&mut fields, true)?),
            RECORD_HASH => {
                let hash = fields.vint()?;
                field_check(hash == HASH_BLAKE2SP, "RAR 5 hash type is not BLAKE2sp")?;
                found.hash = Some(fields.take(HASH_BYTES as u64)?.try_into().unwrap());
            }
            RECORD_TIME => {
                if let Some(time) = time_record(&mut fields)? {
                    found.modified = Some(time);
                }
            }
            RECORD_VERSION => {
                fields.vint()?;
                found.version = Some(fields.vint()?);
            }
            RECORD_REDIRECT => {
                let redirect = match fields.vint()? {
                    1 => Redirect::UnixSymlink,
                    2 => Redirect::WindowsSymlink,
                    3 => Redirect::Junction,
                    4 => Redirect::HardLink,
                    5 => Redirect::FileCopy,
                    _ => return Err(BadField("RAR 5 redirection type is unknown")),
                };
                fields.vint()?;
                let length = fields.vint()?;
                field_check(length > 0, "RAR 5 link has an empty target")?;
                found.redirect = Some((redirect, fields.range(length)?));
            }
            _ => {}
        }
    }
    Ok(found)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileHeader {
    directory: bool,
    unpacked: u64,
    attributes: u64,
    mtime: Option<u32>,
    crc: Option<u32>,
    compression: u64,
    host: u64,
    name: Range<usize>,
    extras: Extras,
}

fn file_header(body: &[u8], base: &Base) -> Field<FileHeader> {
    let mut fields = Fields {
        bytes: &body[..base.fields.end],
        at: base.fields.start,
    };
    let flags = fields.vint()?;
    let unpacked = fields.vint()?;
    field_check(
        flags & UNKNOWN_SIZE == 0,
        "RAR 5 file of unknown size is not supported",
    )?;
    let attributes = fields.vint()?;
    let mtime = if flags & UNIX_MTIME != 0 {
        Some(fields.u32()?)
    } else {
        None
    };
    let crc = if flags & HAS_CRC != 0 {
        Some(fields.u32()?)
    } else {
        None
    };
    let compression = fields.vint()?;
    let host = fields.vint()?;
    let length = fields.vint()?;
    field_check(
        length > 0 && length <= LONGEST_NAME,
        "RAR 5 name is empty or longer than 8192 bytes",
    )?;
    let name = fields.range(length)?;
    Ok(FileHeader {
        directory: flags & DIRECTORY != 0,
        unpacked,
        attributes,
        mtime,
        crc,
        compression,
        host,
        name,
        extras: extras(body, base.extra.clone())?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Stored,
    Compressed { dictionary: u64, solid: bool },
}

fn method(information: u64) -> Field<Method> {
    let version = information & VERSION_MASK;
    let kind = (information >> METHOD_SHIFT) & METHOD_MASK;
    let power = (information >> DICTIONARY_SHIFT) & DICTIONARY_MASK;
    let fraction = (information >> FRACTION_SHIFT) & FRACTION_MASK;
    let solid = information & SOLID_FILE != 0;
    let dictionary = match version {
        0 => {
            field_check(power <= 15, "RAR 5 dictionary is larger than 4 GiB")?;
            SMALLEST_DICTIONARY << power
        }
        1 => {
            field_check(
                information & OLD_ALGORITHM_IN_NEW_FORMAT != 0,
                "RAR 7 compression is not one Zipper reads",
            )?;
            field_check(power <= 15, "RAR 5 dictionary is larger than 4 GiB")?;
            let base = SMALLEST_DICTIONARY << power;
            base + base / 32 * fraction
        }
        _ => return Err(BadField("RAR 5 compression version is unknown")),
    };
    field_check(
        dictionary <= rar5_lz::LARGEST_DICTIONARY,
        "RAR 5 dictionary is larger than 4 GiB",
    )?;
    match kind {
        0 => Ok(Method::Stored),
        1..=5 => Ok(Method::Compressed { dictionary, solid }),
        _ => Err(BadField("RAR 5 compression method is unknown")),
    }
}

fn file_mode(header: &FileHeader) -> u32 {
    match header.host {
        HOST_UNIX => (header.attributes & 0o777) as u32,
        _ if header.directory => DIRECTORY_MODE,
        _ if header.attributes & READ_ONLY != 0 => READ_ONLY_MODE,
        _ => FILE_MODE,
    }
}

fn filetime(ticks: u64) -> Option<SystemTime> {
    let since_1601 = Duration::new(
        ticks / FILETIME_TICKS_PER_SECOND,
        (ticks % FILETIME_TICKS_PER_SECOND) as u32 * 100,
    );
    let offset = Duration::from_secs(FILETIME_TO_UNIX_SECONDS);
    match since_1601.checked_sub(offset) {
        Some(after) => std::time::UNIX_EPOCH.checked_add(after),
        None => std::time::UNIX_EPOCH.checked_sub(offset - since_1601),
    }
}

fn modified(header: &FileHeader) -> Option<SystemTime> {
    match header.extras.modified {
        Some(Time::Unix {
            seconds,
            nanoseconds,
        }) => std::time::UNIX_EPOCH.checked_add(Duration::new(
            u64::from(seconds),
            nanoseconds.min(999_999_999),
        )),
        Some(Time::Windows(ticks)) => filetime(ticks),
        None => header.mtime.and_then(|seconds| {
            std::time::UNIX_EPOCH.checked_add(Duration::from_secs(u64::from(seconds)))
        }),
    }
}

struct Block {
    base: Base,
    body: Vec<u8>,
    data: u64,
}

struct HeaderKey {
    cipher: aes::Aes256,
    password_checked: bool,
}

struct Blocks {
    reader: BufReader<ArchiveFile>,
    length: u64,
    next: u64,
    headers: Option<HeaderKey>,
    volume: u64,
    path: PathBuf,
    reopen: Option<Unchanged>,
    in_set: bool,
}

struct RawHeader {
    crc: [u8; 4],
    size: Vec<u8>,
    body: Vec<u8>,
}

fn header_size(bytes: &[u8]) -> Result<u64> {
    let size = Fields::new(bytes).vint()?;
    ensure!(
        (1..=LARGEST_HEADER).contains(&size),
        "RAR 5 header size {size} is outside 1 to 2 MiB"
    );
    Ok(size)
}

impl Blocks {
    fn open(
        file: ArchiveFile,
        path: PathBuf,
        reopen: Option<Unchanged>,
        volume: u64,
    ) -> Result<Self> {
        let length = file.len()?;
        let mut reader = BufReader::with_capacity(COPY_BUFFER, file);
        let mut signature = [0u8; 8];
        reader
            .read_exact(&mut signature)
            .context("truncated RAR signature")?;
        ensure!(signature == RAR5_SIGNATURE, "RAR 5 signature is missing");
        Ok(Blocks {
            reader,
            length,
            next: RAR5_SIGNATURE.len() as u64,
            headers: None,
            volume,
            path,
            reopen,
            in_set: volume > 0,
        })
    }

    fn label(&self) -> String {
        rar_volumes::file_name(&self.path)
    }

    fn seek_to(&mut self, offset: u64) -> Result<()> {
        let here = self.reader.stream_position()?;
        let step = i64::try_from(i128::from(offset) - i128::from(here))
            .context("RAR 5 block offset overflows")?;
        self.reader.seek_relative(step)?;
        Ok(())
    }

    // SPEC(RAR 5 technote, general archive block format): a CRC-32 of the
    // header, its size in a vint of at most 3 bytes, then the header.
    fn plain_header(&mut self) -> Result<RawHeader> {
        let mut crc = [0u8; 4];
        self.reader
            .read_exact(&mut crc)
            .context("truncated RAR 5 archive, which has no end of archive header")?;
        let mut size = Vec::with_capacity(LONGEST_SIZE_VINT);
        loop {
            let mut byte = [0u8; 1];
            self.reader
                .read_exact(&mut byte)
                .context("truncated RAR 5 header")?;
            size.push(byte[0]);
            if byte[0] & 0x80 == 0 {
                break;
            }
            ensure!(
                size.len() < LONGEST_SIZE_VINT,
                "RAR 5 header size is longer than 3 bytes"
            );
        }
        let mut body = vec![0u8; header_size(&size)? as usize];
        self.reader
            .read_exact(&mut body)
            .context("truncated RAR 5 header")?;
        Ok(RawHeader { crc, size, body })
    }

    // SPEC(RAR 5 technote, archive encryption header): each later header is a 16-byte IV, then
    // the encrypted header, padded to a whole number of 16-byte blocks.
    fn encrypted_header(&mut self) -> Result<RawHeader> {
        let cipher = &self.headers.as_ref().expect("headers are encrypted").cipher;
        let mut chain = [0u8; IV_BYTES];
        self.reader
            .read_exact(&mut chain)
            .context("truncated RAR 5 archive, which has no end of archive header")?;
        let mut whole = vec![0u8; BLOCK];
        self.reader
            .read_exact(&mut whole)
            .context("truncated RAR 5 header")?;
        rar5_crypto::decrypt_cbc(cipher, &mut chain, &mut whole);
        let size_end = (4..4 + LONGEST_SIZE_VINT)
            .find(|&at| whole[at] & 0x80 == 0)
            .context("RAR 5 header size is longer than 3 bytes")?
            + 1;
        let end = size_end + header_size(&whole[4..size_end])? as usize;
        whole.resize(end.next_multiple_of(BLOCK), 0);
        self.reader
            .read_exact(&mut whole[BLOCK..])
            .context("truncated RAR 5 header")?;
        rar5_crypto::decrypt_cbc(cipher, &mut chain, &mut whole[BLOCK..]);
        Ok(RawHeader {
            crc: whole[..4].try_into().unwrap(),
            size: whole[4..size_end].to_vec(),
            body: whole[size_end..end].to_vec(),
        })
    }

    fn next(&mut self) -> Result<Block> {
        let result = self.next_unless_the_password_is_wrong();
        let result = match &self.headers {
            Some(key) if !key.password_checked => {
                result.context("the password is wrong or the RAR 5 headers are damaged")
            }
            _ => result,
        };
        match self.in_set {
            true => result.with_context(|| format!("in RAR volume {}", self.label())),
            false => result,
        }
    }

    fn next_unless_the_password_is_wrong(&mut self) -> Result<Block> {
        self.seek_to(self.next)?;
        let RawHeader { crc, size, body } = match self.headers {
            None => self.plain_header()?,
            Some(_) => self.encrypted_header()?,
        };
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&size);
        hasher.update(&body);
        ensure!(
            hasher.finalize() == u32::from_le_bytes(crc),
            "RAR 5 header fails its CRC-32 check"
        );
        let base = base_header(&body)?;
        let data = self.reader.stream_position()?;
        self.next = data
            .checked_add(base.data_size)
            .filter(|&end| end <= self.length)
            .context("truncated RAR 5 data area")?;
        Ok(Block { base, body, data })
    }
}

enum Packed<'a> {
    Plain(Parts<'a>),
    Encrypted(Box<CbcReader<Parts<'a>>>),
}

impl Read for Packed<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Packed::Plain(area) => area.read(buffer),
            Packed::Encrypted(area) => area.read(buffer),
        }
    }
}

enum Data<'a> {
    Stored(io::Take<Packed<'a>>),
    Compressed(Box<rar5_lz::FileReader<'a, Packed<'a>>>),
}

struct Crypto {
    derived: Rc<Derived>,
    iv: [u8; IV_BYTES],
    tweaked: bool,
    password_checked: bool,
}

struct Checked<'a> {
    data: Data<'a>,
    crc: crc32fast::Hasher,
    blake: Option<Blake2sp>,
    read: u64,
    tweak: Option<Rc<Derived>>,
}

impl Read for Checked<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = match &mut self.data {
            Data::Stored(data) => data.read(buffer)?,
            Data::Compressed(data) => data.read(buffer)?,
        };
        self.crc.update(&buffer[..read]);
        if let Some(blake) = &mut self.blake {
            blake.update(&buffer[..read]);
        }
        self.read += read as u64;
        Ok(read)
    }
}

impl Checked<'_> {
    fn finish(self, name: &str, header: &FileHeader) -> Result<()> {
        if let Data::Compressed(data) = self.data {
            data.finish_at_declared_size()
                .with_context(|| format!("RAR 5 entry {name:?} is damaged"))?;
        }
        ensure!(
            self.read == header.unpacked,
            "truncated RAR 5 entry {name:?}"
        );
        if let Some(crc) = header.crc {
            let found = self.crc.finalize();
            let found = match &self.tweak {
                Some(derived) => derived.tweaked_crc(found),
                None => found,
            };
            ensure!(found == crc, "RAR 5 entry {name:?} fails its CRC-32 check");
        }
        if let (Some(blake), Some(hash)) = (self.blake, header.extras.hash) {
            let found = blake.finish();
            let found = match &self.tweak {
                Some(derived) => derived.tweaked_hash(&found),
                None => found,
            };
            ensure!(
                found == hash,
                "RAR 5 entry {name:?} fails its BLAKE2sp check"
            );
        }
        Ok(())
    }
}

fn entry_name(body: &[u8], header: &FileHeader) -> Result<String> {
    let name = std::str::from_utf8(&body[header.name.clone()])
        .map_err(|_| anyhow::anyhow!("RAR 5 name is not UTF-8"))?;
    Ok(match header.extras.version {
        Some(version) => format!("[VER]/{version}/{name}"),
        None => name.to_owned(),
    })
}

fn link_target(body: &[u8], range: &Range<usize>) -> Result<String> {
    String::from_utf8(body[range.clone()].to_vec())
        .map_err(|_| anyhow::anyhow!("RAR 5 link target is not UTF-8"))
}

fn derived(keys: &mut Keys, record: &Encryption, header: bool) -> Result<Rc<Derived>> {
    keys.derived(record.count, &record.salt)
        .map_err(|error| match error {
            KeyError::Needed => anyhow::Error::from(PasswordNeeded { header }),
            KeyError::NotUtf8 => anyhow::anyhow!("the password for a RAR archive must be UTF-8"),
            KeyError::OverBudget => anyhow::anyhow!(
                "RAR 5 archive asks for more key derivation than Zipper allows, which is two derivations at the KDF count limit of {MOST_KDF_COUNT} or 1,024 in all"
            ),
        })
}

fn password_checked(derived: &Derived, record: &Encryption, what: &str) -> Result<bool> {
    let Some(value) = &record.check else {
        return Ok(false);
    };
    match derived.check(value) {
        CheckValue::Matches => Ok(true),
        CheckValue::WrongPassword => bail!("the password is wrong for {what}"),
        CheckValue::Damaged => bail!("the password check value of {what} is damaged"),
    }
}

struct VolumeSet {
    names: Option<Names>,
    solid: bool,
    encrypted_headers: bool,
}

fn open_volume(
    file: ArchiveFile,
    path: PathBuf,
    reopen: Option<Unchanged>,
    volume: u64,
    keys: &mut Keys,
) -> Result<(Blocks, Main)> {
    let mut blocks = Blocks::open(file, path, reopen, volume)?;
    let mut first = blocks.next()?;
    if first.base.kind == ENCRYPTION_HEADER {
        let record = encryption(
            &mut Fields {
                bytes: &first.body[..first.base.fields.end],
                at: first.base.fields.start,
            },
            false,
        )?;
        let derived = derived(keys, &record, true)?;
        blocks.headers = Some(HeaderKey {
            cipher: derived.cipher(),
            password_checked: password_checked(&derived, &record, "the encrypted RAR 5 headers")?,
        });
        first = blocks.next()?;
    }
    ensure!(
        first.base.kind == MAIN_HEADER,
        "RAR 5 archive does not start with its main header"
    );
    let main = main_header(&first.body[first.base.fields.clone()])?;
    Ok((blocks, main))
}

fn next_volume(set: &VolumeSet, keys: &mut Keys, current: &Blocks) -> Result<Blocks> {
    let names = set.names.as_ref().with_context(|| {
        format!(
            "{} is a RAR 5 volume whose name does not end in .partN.rar, so Zipper cannot name the volume after it",
            current.label()
        )
    })?;
    let volume = current.volume + 1;
    let (path, file, unchanged) = rar_volumes::open(names, volume, &current.path)?;
    let label = rar_volumes::file_name(&path);
    let (blocks, main) = open_volume(file, path, Some(unchanged), volume, keys)?;
    ensure!(
        main.flags & VOLUME != 0,
        "RAR volume {label} does not mark itself as a volume, so it does not belong to the set"
    );
    ensure!(
        main.number == Some(volume),
        "RAR volume {label} is {} of its set, not volume {}, so the volumes are out of order or come from different sets",
        main.number
            .map_or("the first volume".to_owned(), |number| format!(
                "volume {}",
                number.saturating_add(1)
            )),
        volume + 1
    );
    ensure!(
        (main.flags & SOLID_ARCHIVE != 0) == set.solid,
        "RAR volume {label} and the first volume disagree on whether the archive is solid, so they come from different sets"
    );
    ensure!(
        blocks.headers.is_some() == set.encrypted_headers,
        "RAR volume {label} and the first volume disagree on whether the headers are encrypted, so they come from different sets"
    );
    Ok(blocks)
}

fn same_entry(first: (&[u8], &FileHeader), part: (&[u8], &FileHeader)) -> bool {
    let ((first_body, first), (part_body, part)) = (first, part);
    let encryption = |header: &FileHeader| {
        header
            .extras
            .encryption
            .map(|record| (record.count, record.salt, record.iv, record.check))
    };
    first_body[first.name.clone()] == part_body[part.name.clone()]
        && first.unpacked == part.unpacked
        && first.compression == part.compression
        && first.directory == part.directory
        && first.host == part.host
        && encryption(first) == encryption(part)
}

fn part_check(keys: &mut Keys, header: &FileHeader) -> Result<PartCheck> {
    let tweak = match &header.extras.encryption {
        Some(record) if record.tweaked => Some(derived(keys, record, false)?),
        _ => None,
    };
    Ok(PartCheck {
        crc: header.crc,
        blake: header.extras.hash,
        tweak,
    })
}

struct Reader {
    blocks: Blocks,
    stream: Stream,
    keys: Keys,
    set: Option<VolumeSet>,
}

impl Reader {
    fn open(path: &Path, password: Option<&[u8]>) -> Result<Self> {
        let mut keys = Keys::new(password);
        let file = crate::volumes::open(path)?;
        let (mut blocks, main) = open_volume(file, path.to_owned(), None, 0, &mut keys)?;
        let set = match (main.flags & VOLUME != 0, main.number) {
            (false, None) => None,
            (false, Some(_)) => {
                bail!("RAR 5 main header gives a volume number but does not mark a volume")
            }
            (true, Some(number)) => bail!(rar_volumes::later_volume(
                path,
                Names::new_style(path).as_ref(),
                Some(number)
            )),
            (true, None) => Some(VolumeSet {
                names: Names::new_style(path),
                solid: main.flags & SOLID_ARCHIVE != 0,
                encrypted_headers: blocks.headers.is_some(),
            }),
        };
        blocks.in_set = set.is_some();
        Ok(Reader {
            blocks,
            stream: Stream::default(),
            keys,
            set,
        })
    }

    fn crypto(&mut self, header: &FileHeader, name: &str) -> Result<Option<Crypto>> {
        let Some(record) = &header.extras.encryption else {
            return Ok(None);
        };
        let derived = derived(&mut self.keys, record, false)?;
        let password_checked =
            password_checked(&derived, record, &format!("encrypted RAR 5 entry {name:?}"))?;
        ensure!(
            password_checked || header.crc.is_some() || header.extras.hash.is_some(),
            "RAR 5 entry {name:?} is encrypted without a password check value or a checksum, so a wrong password could not be detected"
        );
        Ok(Some(Crypto {
            derived,
            iv: record.iv,
            tweaked: record.tweaked,
            password_checked,
        }))
    }

    // SPEC(RAR 5 technote, file header and hash record): a part that continues in the next
    // volume carries the CRC-32 or hash of its own packed data, the last part the whole file's.
    fn continuation(
        &mut self,
        block: &Block,
        first: FileHeader,
        name: &str,
    ) -> Result<(FileHeader, Span, Blocks)> {
        let set = self.set.as_ref().with_context(|| {
            format!("RAR 5 entry {name:?} continues into another volume, but the archive is not a volume")
        })?;
        let mut span = Span::starting(Part {
            path: Some(self.blocks.path.clone()),
            reopen: None,
            offset: block.data,
            size: block.base.data_size,
            check: Some(part_check(&mut self.keys, &first)?),
        });
        let mut later: Option<Blocks> = None;
        loop {
            let blocks = later.as_mut().unwrap_or(&mut self.blocks);
            loop {
                let after = blocks.next()?;
                match after.base.kind {
                    SERVICE_HEADER if after.base.flags & (SPLIT_BEFORE | SPLIT_AFTER) == 0 => {}
                    END_HEADER => {
                        let flags = Fields::new(&after.body[after.base.fields.clone()]).vint()?;
                        ensure!(
                            flags & NOT_LAST_VOLUME != 0,
                            "RAR 5 entry {name:?} continues into the next volume, but {} is the last volume",
                            blocks.label()
                        );
                        break;
                    }
                    _ => bail!(
                        "RAR 5 entry {name:?} continues into the next volume, but another header follows it in {}",
                        blocks.label()
                    ),
                }
            }
            let mut volume = next_volume(set, &mut self.keys, blocks)?;
            let part = volume.next()?;
            ensure!(
                part.base.kind == FILE_HEADER && part.base.flags & SPLIT_BEFORE != 0,
                "RAR volume {} does not start with the rest of RAR 5 entry {name:?}",
                volume.label()
            );
            let header = file_header(&part.body, &part.base)?;
            ensure!(
                same_entry((&block.body, &first), (&part.body, &header)),
                "RAR volume {} continues RAR 5 entry {name:?} with another name, size, method or encryption, so the volumes come from different sets",
                volume.label()
            );
            let last = part.base.flags & SPLIT_AFTER == 0;
            span.push(Part {
                path: Some(volume.path.clone()),
                reopen: volume.reopen,
                offset: part.data,
                size: part.base.data_size,
                check: match last {
                    true => None,
                    false => Some(part_check(&mut self.keys, &header)?),
                },
            })?;
            if last {
                let whole = FileHeader {
                    crc: header.crc,
                    extras: Extras {
                        hash: header.extras.hash,
                        encryption: header.extras.encryption,
                        ..first.extras
                    },
                    ..first
                };
                return Ok((whole, span, volume));
            }
            later = Some(volume);
        }
    }

    fn data<'a>(
        reader: &'a mut BufReader<ArchiveFile>,
        stream: &'a mut Stream,
        span: Span,
        header: &FileHeader,
        name: &str,
        crypto: Option<&Crypto>,
    ) -> Result<Checked<'a>> {
        let packed = span.packed;
        let area = Parts::new(reader, span, "RAR 5", name);
        let (source, padding) = match crypto {
            None => (Packed::Plain(area), 0),
            Some(crypto) => {
                ensure!(
                    packed.is_multiple_of(BLOCK as u64),
                    "RAR 5 entry {name:?} has encrypted data that is not whole AES blocks"
                );
                let cipher = crypto.derived.cipher();
                let decrypted = CbcReader::new(area, cipher, crypto.iv);
                (Packed::Encrypted(Box::new(decrypted)), BLOCK as u64 - 1)
            }
        };
        let data = match method(header.compression)? {
            Method::Stored => {
                let expected = match crypto {
                    None => Some(header.unpacked),
                    Some(_) => header.unpacked.checked_next_multiple_of(BLOCK as u64),
                };
                ensure!(
                    expected == Some(packed),
                    "RAR 5 stored entry {name:?} packs {packed} bytes for {}",
                    header.unpacked
                );
                Data::Stored(source.take(header.unpacked))
            }
            Method::Compressed { dictionary, solid } => {
                ensure!(
                    dictionary <= rar5_lz::DECODER_MEMORY,
                    "RAR 5 entry {name:?} needs a {dictionary}-byte window, more than the {}-byte decoder budget",
                    rar5_lz::DECODER_MEMORY
                );
                let started = if solid {
                    stream.continue_solid_file(dictionary, header.unpacked)
                } else {
                    stream.start_file(dictionary, header.unpacked)
                };
                started.map_err(BadField)?;
                Data::Compressed(Box::new(stream.reader(source, packed, padding)))
            }
        };
        Ok(Checked {
            data,
            crc: crc32fast::Hasher::new(),
            blake: header.extras.hash.map(|_| Blake2sp::new()),
            read: 0,
            tweak: crypto
                .filter(|crypto| crypto.tweaked)
                .map(|crypto| Rc::clone(&crypto.derived)),
        })
    }

    fn drain(
        &mut self,
        block: &Block,
        span: Span,
        header: &FileHeader,
        name: &str,
        crypto: Option<&Crypto>,
    ) -> Result<Vec<u8>> {
        ensure!(
            header.unpacked <= MAX_NAME,
            "RAR 5 link {name:?} carries more than {MAX_NAME} bytes"
        );
        self.blocks.seek_to(block.data)?;
        let mut checked = Self::data(
            &mut self.blocks.reader,
            &mut self.stream,
            span,
            header,
            name,
            crypto,
        )?;
        let mut bytes = Vec::new();
        (&mut checked)
            .take(header.unpacked)
            .read_to_end(&mut bytes)
            .with_context(|| format!("RAR 5 entry {name:?} is damaged"))?;
        checked.finish(name, header)?;
        Ok(bytes)
    }

    fn entry(&mut self, tree: &mut tree::Tree, block: &Block) -> Result<()> {
        let header = file_header(&block.body, &block.base)?;
        let name = entry_name(&block.body, &header)?;
        ensure!(
            block.base.flags & SPLIT_BEFORE == 0,
            "RAR 5 entry {name:?} continues from a volume before, but no entry there continues into it"
        );
        ensure!(
            matches!(header.host, HOST_WINDOWS | HOST_UNIX),
            "RAR 5 entry {name:?} comes from an unknown host"
        );
        let (header, span, after) = match block.base.flags & SPLIT_AFTER {
            0 => (header, Span::one(block.base.data_size), None),
            _ => {
                let (header, span, after) = self.continuation(block, header, &name)?;
                (header, span, Some(after))
            }
        };
        let crypto = self.crypto(&header, &name)?;
        let result = self.contents(tree, block, span, &header, &name, crypto.as_ref());
        if let Some(after) = after {
            self.blocks = after;
        }
        match crypto {
            Some(crypto) if !crypto.password_checked => result.with_context(|| {
                format!("the password is wrong or RAR 5 entry {name:?} is damaged")
            }),
            _ => result,
        }
    }

    fn contents(
        &mut self,
        tree: &mut tree::Tree,
        block: &Block,
        span: Span,
        header: &FileHeader,
        name: &str,
        crypto: Option<&Crypto>,
    ) -> Result<()> {
        let mode = file_mode(header);
        let time = modified(header);
        if header.directory {
            ensure!(
                span.packed == 0 && header.unpacked == 0,
                "RAR 5 folder entry {name:?} carries data"
            );
            return tree.directory(name, mode, time);
        }
        if let Some((redirect, target)) = header.extras.redirect.clone() {
            let target = link_target(&block.body, &target)?;
            if span.packed > 0 {
                self.drain(block, span, header, name, crypto)?;
            }
            return match redirect {
                Redirect::UnixSymlink => tree.symlink(name, target),
                Redirect::WindowsSymlink => tree.symlink(name, target.replace('\\', "/")),
                Redirect::HardLink => tree.hard_link(name, &target),
                other => bail!("RAR 5 entry {name:?} is a {other:?}, which Zipper does not create"),
            };
        }
        ensure!(
            block.base.flags & HAS_DATA != 0,
            "RAR 5 file entry {name:?} has no data area"
        );
        self.blocks.seek_to(block.data)?;
        let mut checked = Self::data(
            &mut self.blocks.reader,
            &mut self.stream,
            span,
            header,
            name,
            crypto,
        )?;
        tree.file(name, header.unpacked, mode, time, &mut checked)?;
        checked.finish(name, header)
    }

    fn parse(&mut self, tree: &mut tree::Tree) -> Result<()> {
        loop {
            tree.check()?;
            let block = self.blocks.next()?;
            match block.base.kind {
                FILE_HEADER => self.entry(tree, &block)?,
                SERVICE_HEADER => {}
                END_HEADER => {
                    let flags = Fields::new(&block.body[block.base.fields.clone()]).vint()?;
                    if flags & NOT_LAST_VOLUME == 0 {
                        return Ok(());
                    }
                    let set = self.set.as_ref().context(
                        "RAR 5 end of archive header says more volumes follow, but the main header does not mark a volume",
                    )?;
                    self.blocks = next_volume(set, &mut self.keys, &self.blocks)?;
                }
                MAIN_HEADER => bail!("RAR 5 archive has a second main header"),
                ENCRYPTION_HEADER => bail!("RAR 5 encryption header is not the first header"),
                other => bail!("RAR 5 header type {other} is unknown"),
            }
        }
    }
}

fn starts_with(path: &Path, signature: &[u8]) -> Result<bool> {
    let mut head = Vec::with_capacity(signature.len());
    crate::volumes::open(path)?
        .take(signature.len() as u64)
        .read_to_end(&mut head)?;
    Ok(head == signature)
}

/// Returns whether the file starts with the RAR 5 or RAR 4 signature.
pub fn sniff(path: &Path) -> Result<bool> {
    Ok(starts_with(path, &RAR5_SIGNATURE)? || starts_with(path, &crate::rar4::SIGNATURE)?)
}

/// Verify and optionally extract a RAR 5 archive, or a RAR 4 archive that
/// is not solid, into a new directory. It reads stored and compressed
/// entries, RAR 5 solid streams and filters, and RAR 4 PPMd blocks and
/// standard filters, and checks each header's checksum and each file's
/// CRC-32 or BLAKE2sp. It restores folders, modification times, Unix modes
/// and read-only attributes, symlinks and hard links. RAR 5 entries and
/// headers encrypted with AES-256 are decrypted with `options.password`.
/// Without one they fail with [`PasswordNeeded`], and a wrong one fails before
/// the entry it opens is written, or on the entry's checksum when the archive
/// has no password check value. A volume set is read when `path` names its
/// first volume, `name.part1.rar`, or for an old RAR 4 set `name.rar` before
/// `name.r00`. Each volume after it is found by name and must mark itself a
/// volume of the same set, with the next volume number where it records one.
/// A volume after the first named in `path` is refused. RAR 4 encryption, RAR
/// 4 solid archives, junctions and file copies are refused.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    if !starts_with(path, &RAR5_SIGNATURE)? {
        let mut reader = crate::rar4::Reader::open(path)?;
        return tree::extract("RAR", path, destination, options, |tree| reader.parse(tree));
    }
    let mut reader = Reader::open(path, options.password)?;
    tree::extract("RAR", path, destination, options, |tree| reader.parse(tree))
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn vint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    #[test]
    fn every_vint_reads_back_and_padded_ones_keep_their_value() {
        for value in [0u64, 1, 127, 128, 16_383, 16_384, u32::MAX as u64, u64::MAX] {
            let bytes = vint(value);
            let mut fields = Fields::new(&bytes);
            assert_eq!(fields.vint().unwrap(), value);
            assert_eq!(fields.at, bytes.len());
        }
        assert_eq!(Fields::new(&[0x85, 0x80, 0x00]).vint().unwrap(), 5);
        assert!(Fields::new(&[0x80; 10]).vint().is_err());
        assert!(
            Fields::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02])
                .vint()
                .is_err()
        );
        assert!(Fields::new(&[0x80]).vint().is_err());
    }

    #[test]
    fn base_headers_put_the_extra_area_at_the_end() {
        let body = [
            FILE_HEADER as u8,
            (HAS_EXTRA | HAS_DATA) as u8,
            2,
            9,
            0xAA,
            0xBB,
            0xCC,
        ];
        let base = base_header(&body).unwrap();
        assert_eq!(base.kind, FILE_HEADER);
        assert_eq!(base.data_size, 9);
        assert_eq!(base.fields, 4..5);
        assert_eq!(base.extra, 5..7);
        assert!(base_header(&[2, 1, 4, 0]).is_err());
    }

    #[test]
    fn methods_give_the_dictionary_the_technote_describes() {
        assert_eq!(method(0).unwrap(), Method::Stored);
        let normal = (3 << 7) | (5 << 10) | SOLID_FILE;
        assert_eq!(
            method(normal).unwrap(),
            Method::Compressed {
                dictionary: 4 << 20,
                solid: true
            }
        );
        assert!(method((3 << 7) | (16 << 10)).is_err());
        assert!(method(1 | (3 << 7)).is_err());
        assert_eq!(
            method(1 | (3 << 7) | (2 << 10) | (8 << 15) | OLD_ALGORITHM_IN_NEW_FORMAT).unwrap(),
            Method::Compressed {
                dictionary: (512 << 10) + (512 << 10) / 32 * 8,
                solid: false
            }
        );
        assert!(method(6 << 7).is_err());
        assert!(method(2).is_err());
    }

    #[test]
    fn extra_records_read_hashes_times_links_and_skip_unknown_types() {
        let mut area = Vec::new();
        let mut record = |kind: u64, data: &[u8]| {
            let mut body = vint(kind);
            body.extend_from_slice(data);
            area.extend(vint(body.len() as u64));
            area.extend(body);
        };
        let mut hash = vec![0u8];
        hash.extend([7u8; 32]);
        record(RECORD_HASH, &hash);
        let mut time = vec![(TIME_UNIX | TIME_MODIFIED | TIME_NANOSECONDS) as u8];
        time.extend(100u32.to_le_bytes());
        time.extend(5u32.to_le_bytes());
        record(RECORD_TIME, &time);
        record(RECORD_REDIRECT, &[1, 0, 3, b'a', b'/', b'b']);
        record(99, &[1, 2, 3]);
        let found = extras(&area, 0..area.len()).unwrap();
        assert_eq!(found.hash, Some([7; 32]));
        assert_eq!(
            found.modified,
            Some(Time::Unix {
                seconds: 100,
                nanoseconds: 5
            })
        );
        let (kind, target) = found.redirect.unwrap();
        assert_eq!(kind, Redirect::UnixSymlink);
        assert_eq!(&area[target], b"a/b");
        let cut = &area[..area.len() - 1];
        assert!(extras(cut, 0..cut.len()).is_err());
    }

    #[test]
    fn encryption_records_read_the_fields_the_technote_lists_and_refuse_cuts() {
        let mut record = vec![0, (PASSWORD_CHECK | TWEAKED_CHECKSUMS) as u8, 15];
        record.extend([1u8; SALT_BYTES]);
        record.extend([2u8; IV_BYTES]);
        record.extend([3u8; CHECK_BYTES]);
        let mut fields = Fields::new(&record);
        let found = encryption(&mut fields, true).unwrap();
        assert_eq!(
            found,
            Encryption {
                count: 15,
                salt: [1; SALT_BYTES],
                iv: [2; IV_BYTES],
                check: Some([3; CHECK_BYTES]),
                tweaked: true,
            }
        );
        assert_eq!(fields.at, record.len());
        for cut in 0..record.len() {
            assert!(encryption(&mut Fields::new(&record[..cut]), true).is_err());
        }
        let mut header = record.clone();
        header.drain(3 + SALT_BYTES..3 + SALT_BYTES + IV_BYTES);
        let found = encryption(&mut Fields::new(&header), false).unwrap();
        assert_eq!(
            (found.iv, found.check),
            ([0; IV_BYTES], Some([3; CHECK_BYTES]))
        );
        let mut bare = record[..3 + SALT_BYTES + IV_BYTES].to_vec();
        bare[1] = 0;
        let found = encryption(&mut Fields::new(&bare), true).unwrap();
        assert_eq!((found.check, found.tweaked), (None, false));
        let mut versioned = record.clone();
        versioned[0] = 1;
        assert!(encryption(&mut Fields::new(&versioned), true).is_err());
        let mut costly = record.clone();
        costly[2] = MOST_KDF_COUNT + 1;
        assert!(encryption(&mut Fields::new(&costly), true).is_err());
        costly[2] = MOST_KDF_COUNT;
        assert!(encryption(&mut Fields::new(&costly), true).is_ok());
    }

    #[test]
    fn names_have_the_tree_writers_limit() {
        assert_eq!(LONGEST_NAME, MAX_NAME);
    }

    #[test]
    fn modes_follow_the_host() {
        let mut header = FileHeader {
            directory: false,
            unpacked: 0,
            attributes: 0x20,
            mtime: None,
            crc: None,
            compression: 0,
            host: HOST_WINDOWS,
            name: 0..1,
            extras: Extras::default(),
        };
        assert_eq!(file_mode(&header), FILE_MODE);
        header.attributes = READ_ONLY;
        assert_eq!(file_mode(&header), READ_ONLY_MODE);
        header.directory = true;
        assert_eq!(file_mode(&header), DIRECTORY_MODE);
        header.host = HOST_UNIX;
        header.attributes = 0o104_755;
        assert_eq!(file_mode(&header), 0o755);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(12)]
    fn vints_never_panic_and_end_on_a_byte_without_the_high_bit() {
        let bytes: [u8; 11] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let mut fields = Fields::new(&bytes[..length]);
        if fields.vint().is_ok() {
            assert!(fields.at >= 1 && fields.at <= LONGEST_VINT && fields.at <= length);
            assert_eq!(bytes[fields.at - 1] & 0x80, 0);
        }
    }

    #[kani::proof]
    #[kani::unwind(12)]
    fn base_headers_keep_their_fields_and_extra_area_inside_the_header() {
        let bytes: [u8; 10] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(base) = base_header(&bytes[..length]) {
            assert!(base.fields.start <= base.fields.end);
            assert_eq!(base.fields.end, base.extra.start);
            assert_eq!(base.extra.end, length);
        }
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn extra_records_never_panic_and_keep_link_targets_inside_their_record() {
        let bytes: [u8; 8] = kani::any();
        let start: usize = kani::any();
        let end: usize = kani::any();
        kani::assume(start <= end && end <= bytes.len());
        if let Ok(found) = extras(&bytes, start..end) {
            if let Some((_, target)) = found.redirect {
                assert!(start < target.start && target.start < target.end && target.end <= end);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(12)]
    fn file_headers_never_panic_and_keep_the_name_inside_the_fields() {
        let bytes: [u8; 10] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let body = &bytes[..length];
        if let Ok(base) = base_header(body) {
            kani::assume(base.extra.is_empty());
            if let Ok(header) = file_header(body, &base) {
                assert!(base.fields.start < header.name.start);
                assert!(header.name.start < header.name.end && header.name.end <= base.fields.end);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn encryption_records_never_panic_and_read_only_their_fields() {
        let bytes: [u8; 48] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        let with_iv: bool = kani::any();
        let mut fields = Fields::new(&bytes[..length]);
        if let Ok(found) = encryption(&mut fields, with_iv) {
            assert!(found.count <= MOST_KDF_COUNT);
            let iv = if with_iv { IV_BYTES } else { 0 };
            let check = if found.check.is_some() {
                CHECK_BYTES
            } else {
                0
            };
            assert!(fields.at >= 3 + SALT_BYTES + iv + check && fields.at <= length);
            if !with_iv {
                assert_eq!(found.iv, [0; IV_BYTES]);
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(12)]
    fn main_headers_never_panic_and_give_a_volume_number_only_with_its_flag() {
        let bytes: [u8; 10] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(main) = main_header(&bytes[..length]) {
            assert_eq!(main.number.is_some(), main.flags & VOLUME_NUMBER != 0);
        }
    }

    #[kani::proof]
    fn methods_keep_every_dictionary_from_128_kib_to_4_gib() {
        let information: u64 = kani::any();
        if let Ok(Method::Compressed { dictionary, .. }) = method(information) {
            assert!((SMALLEST_DICTIONARY..=rar5_lz::LARGEST_DICTIONARY).contains(&dictionary));
        }
    }
}
