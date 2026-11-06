//! In-process extraction of the files an RPM package installs, from the
//! newc cpio in its compressed payload. See `docs/reference/rpm-reader.md`
//! for what it accepts and restores.
pub use crate::native_cpio::ReadOptions;
use crate::native_cpio::tree::BACKEND_HINT;
use crate::native_tar::Compression;
use crate::{BadField, Entry, Field, field_check, volumes::ArchiveFile};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
};

const COPY_BUFFER: usize = 1024 * 1024;
const LEAD: usize = 96;
const LEAD_MAGIC: [u8; 4] = [0xED, 0xAB, 0xEE, 0xDB];
// SPEC(LSB 4.1 Package File Format, Lead): the signature type sits at bytes 78 and 79.
const SIGNATURE_TYPE_AT: usize = 78;
const SIGNATURE_IN_HEADER_FORM: u16 = 5;
const HEADER_MAGIC: [u8; 4] = [0x8E, 0xAD, 0xE8, 0x01];
const HEADER_INTRO: usize = 16;
const INDEX_ENTRY: usize = 16;
const SIGNATURE_ALIGNMENT: u64 = 8;
const MAX_INDEX_ENTRIES: u32 = 65_535;
const MAX_DATA: u32 = 256 * 1024 * 1024;
const PAYLOAD_FORMAT: u32 = 1124;
const PAYLOAD_COMPRESSOR: u32 = 1125;
const MAX_TAG_TEXT: u32 = 64;
const COMPRESSOR_HEAD: usize = 6;
const LZMA_PROPERTIES_LIMIT: u8 = 225;
const STRIPPED_CPIO: &[u8; 6] = b"07070X";
const DAMAGED: &str = "RPM payload is damaged";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Null,
    Char,
    Int8,
    Int16,
    Int32,
    Int64,
    String,
    Bin,
    StringArray,
    I18nString,
}

impl Kind {
    fn of(code: u32) -> Field<Kind> {
        Ok(match code {
            0 => Kind::Null,
            1 => Kind::Char,
            2 => Kind::Int8,
            3 => Kind::Int16,
            4 => Kind::Int32,
            5 => Kind::Int64,
            6 => Kind::String,
            7 => Kind::Bin,
            8 => Kind::StringArray,
            9 => Kind::I18nString,
            _ => return Err(BadField("RPM index entry has an unknown type")),
        })
    }

    fn least_bytes_per_value(self) -> u64 {
        match self {
            Kind::Null => 0,
            Kind::Int16 => 2,
            Kind::Int32 => 4,
            Kind::Int64 => 8,
            _ => 1,
        }
    }

    fn alignment(self) -> u32 {
        match self {
            Kind::Int16 => 2,
            Kind::Int32 => 4,
            Kind::Int64 => 8,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Extent {
    entries: u32,
    data: u32,
}

impl Extent {
    fn length(self) -> u64 {
        HEADER_INTRO as u64 + INDEX_ENTRY as u64 * u64::from(self.entries) + u64::from(self.data)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexEntry {
    tag: u32,
    kind: Kind,
    offset: u32,
    count: u32,
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn lead(bytes: &[u8; LEAD]) -> Field<()> {
    field_check(bytes[..4] == LEAD_MAGIC, "not an RPM package")?;
    field_check(matches!(bytes[4], 3 | 4), "RPM lead version is not 3 or 4")?;
    field_check(
        u16::from_be_bytes([bytes[SIGNATURE_TYPE_AT], bytes[SIGNATURE_TYPE_AT + 1]])
            == SIGNATURE_IN_HEADER_FORM,
        "RPM signature is not in header form",
    )
}

fn header_intro(bytes: &[u8; HEADER_INTRO]) -> Field<Extent> {
    field_check(bytes[..4] == HEADER_MAGIC, "RPM header has no header magic")?;
    let extent = Extent {
        entries: word(bytes, 8),
        data: word(bytes, 12),
    };
    field_check(
        extent.entries <= MAX_INDEX_ENTRIES,
        "RPM header has more than 65,535 index entries",
    )?;
    field_check(
        extent.data <= MAX_DATA,
        "RPM header data is larger than 256 MiB",
    )?;
    Ok(extent)
}

fn index_entry(bytes: &[u8; INDEX_ENTRY], data: u32) -> Field<IndexEntry> {
    let entry = IndexEntry {
        tag: word(bytes, 0),
        kind: Kind::of(word(bytes, 4))?,
        offset: word(bytes, 8),
        count: word(bytes, 12),
    };
    field_check(
        entry.kind != Kind::String || entry.count == 1,
        "RPM string entry has a count other than 1",
    )?;
    field_check(
        entry.offset.is_multiple_of(entry.kind.alignment()),
        "RPM index entry is misaligned",
    )?;
    let end = u64::from(entry.offset) + u64::from(entry.count) * entry.kind.least_bytes_per_value();
    field_check(
        end <= u64::from(data),
        "RPM index entry lies outside the header data",
    )?;
    Ok(entry)
}

fn text_window(entry: IndexEntry, data: u32) -> u32 {
    (data - entry.offset).min(MAX_TAG_TEXT + 1)
}

fn tag_text(window: &[u8]) -> Field<&[u8]> {
    let end = window.iter().position(|&byte| byte == 0).ok_or(
        if window.len() > MAX_TAG_TEXT as usize {
            BadField("RPM payload tag is longer than 64 bytes")
        } else {
            BadField("RPM payload tag has no NUL terminator")
        },
    )?;
    Ok(&window[..end])
}

fn signature_padding(data: u32) -> u64 {
    (SIGNATURE_ALIGNMENT - u64::from(data) % SIGNATURE_ALIGNMENT) % SIGNATURE_ALIGNMENT
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compressor {
    Gzip,
    Bzip2,
    Xz,
    Lzma,
    Zstd,
}

impl Compressor {
    fn named(name: &[u8]) -> Option<Compressor> {
        Some(match name {
            b"gzip" => Compressor::Gzip,
            b"bzip2" => Compressor::Bzip2,
            b"xz" => Compressor::Xz,
            b"lzma" => Compressor::Lzma,
            b"zstd" => Compressor::Zstd,
            _ => return None,
        })
    }

    fn starts(self, head: &[u8; COMPRESSOR_HEAD]) -> bool {
        match self {
            Compressor::Gzip => head[..2] == [0x1F, 0x8B],
            Compressor::Bzip2 => head[..3] == *b"BZh" && (b'1'..=b'9').contains(&head[3]),
            Compressor::Xz => head[..6] == [0xFD, b'7', b'z', b'X', b'Z', 0],
            Compressor::Lzma => head[0] < LZMA_PROPERTIES_LIMIT,
            Compressor::Zstd => head[..4] == [0x28, 0xB5, 0x2F, 0xFD],
        }
    }

    fn compression(self) -> Compression {
        match self {
            Compressor::Gzip => Compression::Gzip,
            Compressor::Bzip2 => Compression::Bzip2,
            Compressor::Xz => Compression::Xz,
            Compressor::Lzma => Compression::Lzma,
            Compressor::Zstd => Compression::Zstd,
        }
    }
}

/// Returns whether the file starts with the RPM lead signature.
pub fn sniff(path: &Path) -> Result<bool> {
    let mut magic = Vec::with_capacity(LEAD_MAGIC.len());
    crate::volumes::open(path)?
        .take(LEAD_MAGIC.len() as u64)
        .read_to_end(&mut magic)?;
    Ok(magic == LEAD_MAGIC)
}

struct Package {
    reader: BufReader<ArchiveFile>,
    length: u64,
    position: u64,
}

#[derive(Default)]
struct PayloadTags {
    format: Option<IndexEntry>,
    compressor: Option<IndexEntry>,
}

impl Package {
    fn exact(&mut self, buffer: &mut [u8], what: &str) -> Result<()> {
        self.reader
            .read_exact(buffer)
            .with_context(|| format!("truncated RPM {what}"))?;
        self.position += buffer.len() as u64;
        Ok(())
    }

    fn seek_to(&mut self, offset: u64) -> Result<()> {
        let step = i64::try_from(i128::from(offset) - i128::from(self.position))
            .context("RPM offset overflows")?;
        self.reader.seek_relative(step)?;
        self.position = offset;
        Ok(())
    }

    fn header(&mut self, mut keep: impl FnMut(IndexEntry) -> Result<()>) -> Result<(u64, u32)> {
        let mut intro = [0u8; HEADER_INTRO];
        self.exact(&mut intro, "header")?;
        let extent = header_intro(&intro)?;
        let end = self.position - HEADER_INTRO as u64 + extent.length();
        ensure!(
            end <= self.length,
            "RPM header runs past the end of the file"
        );
        for _ in 0..extent.entries {
            let mut bytes = [0u8; INDEX_ENTRY];
            self.exact(&mut bytes, "header")?;
            keep(index_entry(&bytes, extent.data)?)?;
        }
        Ok((self.position, extent.data))
    }

    fn text(
        &mut self,
        store: u64,
        data: u32,
        entry: Option<IndexEntry>,
    ) -> Result<Option<Vec<u8>>> {
        let Some(entry) = entry else {
            return Ok(None);
        };
        ensure!(
            entry.kind == Kind::String,
            "RPM payload tag is not a string"
        );
        self.seek_to(store + u64::from(entry.offset))?;
        let mut window = vec![0; text_window(entry, data) as usize];
        self.exact(&mut window, "header")?;
        Ok(Some(tag_text(&window)?.to_vec()))
    }

    fn open(path: &Path) -> Result<(Self, Compressor)> {
        let file = crate::volumes::open(path)?;
        let length = file.len()?;
        let mut package = Package {
            reader: BufReader::with_capacity(COPY_BUFFER, file),
            length,
            position: 0,
        };
        let mut bytes = [0u8; LEAD];
        package.exact(&mut bytes, "lead")?;
        lead(&bytes)?;
        let (store, data) = package.header(|_| Ok(()))?;
        let signature_end = store + u64::from(data);
        package.seek_to(signature_end + signature_padding(data))?;
        let mut tags = PayloadTags::default();
        let (store, data) = package.header(|entry| {
            let slot = match entry.tag {
                PAYLOAD_FORMAT => &mut tags.format,
                PAYLOAD_COMPRESSOR => &mut tags.compressor,
                _ => return Ok(()),
            };
            ensure!(slot.is_none(), "RPM header gives a payload tag twice");
            *slot = Some(entry);
            Ok(())
        })?;
        let format = package.text(store, data, tags.format)?;
        let compressor = package.text(store, data, tags.compressor)?;
        package.seek_to(store + u64::from(data))?;
        if let Some(format) = format {
            ensure!(
                format == b"cpio",
                "RPM payload format {:?} is not cpio; {BACKEND_HINT}",
                String::from_utf8_lossy(&format)
            );
        }
        let compressor = match compressor {
            None => Compressor::Gzip,
            Some(name) => Compressor::named(&name).with_context(|| {
                format!(
                    "RPM payload compressor {:?} is not one Zipper reads; {BACKEND_HINT}",
                    String::from_utf8_lossy(&name)
                )
            })?,
        };
        Ok((package, compressor))
    }
}

fn payload(path: &Path) -> Result<Box<dyn Read + Send>> {
    let (mut package, compressor) = Package::open(path)?;
    let mut head = [0u8; COMPRESSOR_HEAD];
    package.exact(&mut head, "payload")?;
    ensure!(
        compressor.starts(&head),
        "RPM payload does not start with the {compressor:?} signature its header names"
    );
    let input = std::io::Cursor::new(head).chain(package.reader);
    let decoder: Box<dyn Read + Send> = match compressor {
        Compressor::Lzma => Box::new(crate::native_tar::DecoderMemory(LzmaToTheEnd {
            decoder: crate::native_tar::lzma_decoder(input)?,
        })),
        _ => crate::native_tar::decode(input, compressor.compression())?,
    };
    Ok(Box::new(Damaged(decoder)))
}

struct LzmaToTheEnd<R: BufRead> {
    decoder: liblzma::bufread::XzDecoder<R>,
}

impl<R: BufRead> Read for LzmaToTheEnd<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.decoder.read(buffer)?;
        if read == 0 && !buffer.is_empty() && !self.decoder.get_mut().fill_buf()?.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "it has data after its LZMA stream",
            ));
        }
        Ok(read)
    }
}

struct Damaged<R>(R);

impl<R: Read> Read for Damaged<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer).map_err(|error| match error.kind() {
            std::io::ErrorKind::OutOfMemory => error,
            kind => std::io::Error::new(kind, format!("{DAMAGED}: {error}")),
        })
    }
}

/// Verify and optionally extract the files an RPM package installs. It reads
/// the lead and both headers, decompresses the payload with the compressor
/// the header names, and extracts its newc cpio as the cpio reader does,
/// with files, directories, hard links, Unix modes, modification times and
/// symlinks that stay inside the destination. Header digests and signatures
/// are not checked.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let mut payload = payload(path)?;
    let mut magic = [0u8; 6];
    let mut filled = 0;
    while filled < magic.len() {
        let read = payload.read(&mut magic[filled..])?;
        ensure!(read > 0, "{DAMAGED}: it ends before its first cpio header");
        filled += read;
    }
    if &magic == STRIPPED_CPIO {
        bail!(
            "RPM payload is the stripped cpio 07070X that rpm writes for files of 4 GiB or more and for version 6 packages, which Zipper and 7-Zip do not unpack; pass --zip-engine backend to write the payload's cpio"
        );
    }
    crate::native_cpio::read_newc_stream("rpm", path, magic, payload, destination, options)
}

#[cfg(test)]
mod field_tests {
    use super::*;

    fn entry_bytes(tag: u32, kind: u32, offset: u32, count: u32) -> [u8; INDEX_ENTRY] {
        let mut bytes = [0u8; INDEX_ENTRY];
        for (index, value) in [tag, kind, offset, count].into_iter().enumerate() {
            bytes[4 * index..4 * index + 4].copy_from_slice(&value.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn every_kind_fits_its_store_exactly_at_the_edge() {
        for (code, size, alignment) in [
            (0u32, 0u32, 1u32),
            (1, 1, 1),
            (2, 1, 1),
            (3, 2, 2),
            (4, 4, 4),
            (5, 8, 8),
            (6, 1, 1),
            (7, 1, 1),
            (8, 1, 1),
            (9, 1, 1),
        ] {
            for count in [1u32, 3] {
                if code == 6 && count != 1 {
                    assert!(index_entry(&entry_bytes(1, code, 0, count), 64).is_err());
                    continue;
                }
                let need = size * count;
                let offset = 16;
                let data = offset + need;
                assert!(
                    index_entry(&entry_bytes(1, code, offset, count), data).is_ok(),
                    "{code} {count}"
                );
                if need > 0 {
                    assert!(
                        index_entry(&entry_bytes(1, code, offset, count), data - 1).is_err(),
                        "{code} {count} one past"
                    );
                }
                if alignment > 1 {
                    assert!(
                        index_entry(&entry_bytes(1, code, offset + 1, count), data + 8).is_err(),
                        "{code} misaligned"
                    );
                }
            }
        }
        assert!(index_entry(&entry_bytes(1, 10, 0, 1), 64).is_err());
        assert!(index_entry(&entry_bytes(1, u32::MAX, 0, 1), 64).is_err());
        assert!(index_entry(&entry_bytes(1, 5, 8, u32::MAX), MAX_DATA).is_err());
    }

    #[test]
    fn intros_hold_the_limits_exactly() {
        let intro = |entries: u32, data: u32| {
            let mut bytes = [0u8; HEADER_INTRO];
            bytes[..4].copy_from_slice(&HEADER_MAGIC);
            bytes[8..12].copy_from_slice(&entries.to_be_bytes());
            bytes[12..].copy_from_slice(&data.to_be_bytes());
            bytes
        };
        assert!(header_intro(&intro(MAX_INDEX_ENTRIES, MAX_DATA)).is_ok());
        assert!(header_intro(&intro(MAX_INDEX_ENTRIES + 1, 0)).is_err());
        assert!(header_intro(&intro(0, MAX_DATA + 1)).is_err());
        let mut wrong = intro(0, 0);
        wrong[3] = 3;
        assert!(header_intro(&wrong).is_err());
    }

    #[test]
    fn tag_text_ends_at_the_first_nul_within_the_limit() {
        assert_eq!(tag_text(b"xz\0junk").unwrap(), b"xz");
        assert_eq!(tag_text(b"\0").unwrap(), b"");
        assert!(tag_text(b"xz").is_err());
        assert!(tag_text(&[b'a'; 65]).is_err());
        let mut longest = vec![b'a'; 64];
        longest.push(0);
        assert_eq!(tag_text(&longest).unwrap().len(), 64);
    }

    #[test]
    fn every_compressor_recognizes_its_own_head_and_no_other() {
        let heads: [(Compressor, [u8; COMPRESSOR_HEAD]); 4] = [
            (Compressor::Gzip, head(&[0x1F, 0x8B, 8])),
            (Compressor::Bzip2, head(b"BZh91A")),
            (Compressor::Xz, head(&[0xFD, b'7', b'z', b'X', b'Z', 0])),
            (Compressor::Zstd, head(&[0x28, 0xB5, 0x2F, 0xFD])),
        ];
        for (compressor, _) in heads {
            for (other, head) in heads {
                assert_eq!(compressor.starts(&head), compressor == other);
            }
        }
        assert!(Compressor::Lzma.starts(&head(&[0x5D, 0, 0, 0x80])));
        assert!(Compressor::Lzma.starts(&head(&[224])));
        assert!(!Compressor::Lzma.starts(&head(&[225])));
        assert!(!Compressor::Lzma.starts(&heads[2].1));
        assert!(!Compressor::Bzip2.starts(&head(b"BZh0AA")));
    }

    fn head(bytes: &[u8]) -> [u8; COMPRESSOR_HEAD] {
        let mut head = [0u8; COMPRESSOR_HEAD];
        head[..bytes.len()].copy_from_slice(bytes);
        head
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn leads_never_panic() {
        let bytes: [u8; LEAD] = kani::any();
        if lead(&bytes).is_ok() {
            assert!(bytes[..4] == LEAD_MAGIC);
        }
    }

    #[kani::proof]
    fn header_intros_stay_within_the_limits_and_their_length_fits() {
        let bytes: [u8; HEADER_INTRO] = kani::any();
        if let Ok(extent) = header_intro(&bytes) {
            assert!(extent.entries <= MAX_INDEX_ENTRIES);
            assert!(extent.data <= MAX_DATA);
            assert!(
                extent.length()
                    <= HEADER_INTRO as u64
                        + INDEX_ENTRY as u64 * u64::from(MAX_INDEX_ENTRIES)
                        + u64::from(MAX_DATA)
            );
        }
    }

    #[kani::proof]
    fn every_accepted_index_entry_lies_inside_the_data_store() {
        let bytes: [u8; INDEX_ENTRY] = kani::any();
        let data: u32 = kani::any();
        kani::assume(data <= MAX_DATA);
        if let Ok(entry) = index_entry(&bytes, data) {
            let least = u64::from(entry.count) * entry.kind.least_bytes_per_value();
            assert!(u64::from(entry.offset) + least <= u64::from(data));
            assert!(entry.offset.is_multiple_of(entry.kind.alignment()));
            assert!(entry.kind != Kind::String || entry.count == 1);
            assert!(word(&bytes, 4) <= 9);
            if entry.kind == Kind::String {
                let window = text_window(entry, data);
                assert!(window >= 1 && window <= MAX_TAG_TEXT + 1);
                assert!(u64::from(entry.offset) + u64::from(window) <= u64::from(data));
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(7)]
    fn tag_text_never_panics_and_stays_inside_its_window() {
        let bytes: [u8; 6] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(text) = tag_text(&bytes[..length]) {
            assert!(text.len() < length);
            assert!(bytes[text.len()] == 0);
        }
    }

    #[kani::proof]
    fn compressor_heads_never_panic() {
        let head: [u8; COMPRESSOR_HEAD] = kani::any();
        for compressor in [
            Compressor::Gzip,
            Compressor::Bzip2,
            Compressor::Xz,
            Compressor::Lzma,
            Compressor::Zstd,
        ] {
            let _ = compressor.starts(&head);
        }
    }

    #[kani::proof]
    fn signature_padding_aligns_every_store() {
        let data: u32 = kani::any();
        let padded = u64::from(data) + signature_padding(data);
        assert!(padded % SIGNATURE_ALIGNMENT == 0);
        assert!(padded - u64::from(data) < SIGNATURE_ALIGNMENT);
    }
}
