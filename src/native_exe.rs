//! In-process extraction of executables and object files: PE images and COFF
//! objects, UEFI TE images, ELF files, Mach-O files and Mach-O universal
//! files. See `docs/reference/exe-reader.md` for what it accepts and how its
//! tree matches `7zz x`.
pub use crate::tree::ReadOptions;
use crate::tree::{self, MAX_ENTRIES};
use crate::volumes::ArchiveFile;
use crate::{BadField, Entry, Field, field_check};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Read,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const HEAD: usize = 64;
const FILE_MODE: u32 = 0o644;
const MAX_NAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_BUILT_BYTES: usize = 64 * 1024 * 1024;
const ZIP_TAIL: u64 = 0xFFFF + 22;

const DOS_HEADER: usize = 0x40;
const PE_POINTER_AT: usize = 0x3C;
const PE_SIGNATURE: &[u8; 4] = b"PE\0\0";
const COFF_HEADER: usize = 20;
const SECTION_HEADER: usize = 40;
const PE32_MAGIC: u16 = 0x10B;
const PE32_PLUS_MAGIC: u16 = 0x20B;
const PE32_FIXED: usize = 96;
const PE32_PLUS_FIXED: usize = 112;
const DIRECTORIES: usize = 16;
const FILE_ALIGNMENT_AT: usize = 36;
const CERTIFICATES: usize = 4;
const RESOURCE_SECTION: &[u8; 8] = b".rsrc\0\0\0";
const RELOCATION_SECTION: &[u8; 8] = b".reloc\0\0";
const RELOCATIONS: usize = 5;
const DEBUG: usize = 6;
const DEBUG_ENTRY: u64 = 28;
const DEBUG_ENTRIES: u64 = 16;
const SYMBOL: u64 = 18;
const PE_UNLISTED_HEAD: u64 = 4096;
const TE_HEADER: usize = 40;
const TE_SIGNATURE: &[u8; 2] = b"VZ";
const TE_LAST_SUBSYSTEM: u8 = 17;
const TE_UNUSED_SUBSYSTEM: u8 = 15;
const TE_LARGEST_DIRECTORY: u32 = 0x0FFF_FFFF;

// EXTERNAL(7-Zip 26.05): the machines its COFF handler opens, found by trying every value.
const COFF_MACHINES: [u16; 38] = [
    0x0, 0x14C, 0x14D, 0x162, 0x166, 0x168, 0x169, 0x184, 0x1A2, 0x1A3, 0x1A4, 0x1A6, 0x1A8, 0x1C0,
    0x1C2, 0x1C4, 0x1D3, 0x1F0, 0x1F1, 0x1F2, 0x200, 0x266, 0x284, 0x366, 0x466, 0x520, 0xCEF,
    0xEBC, 0x5032, 0x5064, 0x6232, 0x6264, 0x8664, 0x9041, 0xA641, 0xA64E, 0xAA64, 0xC0EE,
];

const ELF_MAGIC: &[u8; 4] = b"\x7FELF";
const ELF32_HEADER: usize = 52;
const ELF64_HEADER: usize = 64;
const ELF_IDENT_PADDING: usize = 9;
const ELF_IDENT: usize = 16;
const ELF32_PROGRAM: usize = 32;
const ELF64_PROGRAM: usize = 56;
const ELF32_SECTION: usize = 40;
const ELF64_SECTION: usize = 64;
const ELF_LARGEST_ENTRY_SIZE: u64 = 1023;
const ELF_FIRST_LARGEST_ENTRY_SIZE: u64 = 0x7FFF_FFFF;
const LONGEST_NAME: usize = 4096;
const ELF_NOBITS: u32 = 8;
const ELF_PHDR: u32 = 6;

const MACHO_MAGICS: [u32; 4] = [0xFEED_FACE, 0xFEED_FACF, 0xCEFA_EDFE, 0xCFFA_EDFE];
const MACHO32_HEADER: usize = 28;
const MACHO64_HEADER: usize = 32;
const MACHO_LARGEST_COMMANDS: u32 = 1 << 24;
const MACHO_SEGMENT32: u32 = 0x1;
const MACHO_SEGMENT64: u32 = 0x19;
const MACHO_SYMTAB: u32 = 0x2;
const MACHO_SYMTAB_SIZE: usize = 24;
const MACHO_UUID: u32 = 0x1B;
const MACHO_UUID_SIZE: usize = 24;
const MACHO_VERSION_MIN_MACOS: u32 = 0x24;
const MACHO_VERSION_MIN_IOS: u32 = 0x25;
const MACHO_VERSION_MIN_TVOS: u32 = 0x2F;
const MACHO_VERSION_MIN_WATCHOS: u32 = 0x30;
const MACHO_SOURCE_VERSION: u32 = 0x2A;
const MACHO_VERSION_SIZE: usize = 16;
const MACHO_BUILD_VERSION: u32 = 0x32;
const MACHO_BUILD_VERSION_SIZE: usize = 24;
const MACHO_BUILD_TOOL: usize = 8;
const MACHO_ZEROFILL: u32 = 0x1;
const MACHO_NAME: usize = 16;

const FAT_MAGIC: u32 = 0xCAFE_BABE;
const MUB_OTHER_SIGNATURE: [u8; 4] = [0xB9, 0xFA, 0xF1, 0x0E];
const FAT_HEADER: usize = 8;
const FAT_ARCH: usize = 20;
const FAT_LARGEST_COUNT: u32 = 8;
const FAT_LARGEST_ALIGN: u32 = 31;
const FAT_SUBTYPE_UNUSED: u32 = 0x0000_FC00;
const CPU_ARCH_ABI64: u32 = 0x0100_0000;
const CPU_ARCH_ABI64_32: u32 = 0x0200_0000;
const CPU_SUBTYPE_LIB64: u32 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    Pe,
    Coff,
    Te,
    Elf,
    MachO,
    Mub,
}

impl Form {
    fn label(self) -> &'static str {
        match self {
            Form::Pe => "PE",
            Form::Coff => "COFF",
            Form::Te => "TE",
            Form::Elf => "ELF",
            Form::MachO => "Mach-O",
            Form::Mub => "Mach-O universal",
        }
    }
}

fn u16_at(bytes: &[u8], at: usize, big: bool) -> u16 {
    let pair = [bytes[at], bytes[at + 1]];
    if big {
        u16::from_be_bytes(pair)
    } else {
        u16::from_le_bytes(pair)
    }
}

fn u32_at(bytes: &[u8], at: usize, big: bool) -> u32 {
    let quad = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
    if big {
        u32::from_be_bytes(quad)
    } else {
        u32::from_le_bytes(quad)
    }
}

fn u64_at(bytes: &[u8], at: usize, big: bool) -> u64 {
    let (low, high) = (
        u64::from(u32_at(bytes, at, big)),
        u64::from(u32_at(bytes, at + 4, big)),
    );
    if big {
        low << 32 | high
    } else {
        high << 32 | low
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    offset: u64,
    size: u64,
}

impl Span {
    fn end(self) -> Field<u64> {
        self.offset
            .checked_add(self.size)
            .ok_or(BadField("executable range ends past 2^64 bytes"))
    }
}

enum Data {
    Range(Span),
    Built(Vec<u8>),
    Prefixed(Vec<u8>, Span),
}

impl Data {
    fn size(&self) -> u64 {
        match self {
            Data::Range(span) => span.size,
            Data::Built(bytes) => bytes.len() as u64,
            Data::Prefixed(prefix, span) => prefix.len() as u64 + span.size,
        }
    }

    fn span(&self) -> Option<Span> {
        match self {
            Data::Range(span) | Data::Prefixed(_, span) => Some(*span),
            Data::Built(_) => None,
        }
    }
}

struct Item {
    name: String,
    data: Data,
    modified: Option<SystemTime>,
}

struct Image {
    form: Form,
    items: Vec<Item>,
    end: u64,
}

struct Names<'a> {
    archive: &'a str,
    bytes: usize,
}

impl Names<'_> {
    fn charge(&mut self, name: &str) -> Result<()> {
        self.bytes += name.len();
        ensure!(
            self.bytes <= MAX_NAME_BYTES,
            "executable names exceed 64 MiB in total"
        );
        Ok(())
    }

    // EXTERNAL(7-Zip): an item with no name takes the archive's name without its extension,
    // or with `~` added when the name has none.
    fn or_default(&self, name: String) -> String {
        if !name.is_empty() {
            return name;
        }
        match self.archive.rfind('.') {
            Some(dot) => self.archive[..dot].to_owned(),
            None => format!("{}~", self.archive),
        }
    }
}

fn read_exact_at(file: &ArchiveFile, mut buffer: &mut [u8], mut offset: u64) -> Result<()> {
    while !buffer.is_empty() {
        let read = file.read_at(buffer, offset)?;
        ensure!(read > 0, "unexpected end of file");
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

fn read_up_to(file: &ArchiveFile, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    let mut done = 0;
    while done < buffer.len() {
        let read = file.read_at(&mut buffer[done..], offset + done as u64)?;
        if read == 0 {
            break;
        }
        done += read;
    }
    Ok(done)
}

fn read_span(file: &ArchiveFile, span: Span, length: u64, what: &str) -> Result<Vec<u8>> {
    ensure!(
        span.end()? <= length,
        "{what} at offset {} runs past the end of the file",
        span.offset
    );
    let mut bytes = vec![0u8; usize::try_from(span.size).context("range too large")?];
    read_exact_at(file, &mut bytes, span.offset)?;
    Ok(bytes)
}

fn time_from(seconds: u32) -> Option<SystemTime> {
    (seconds != 0).then(|| UNIX_EPOCH + Duration::from_secs(u64::from(seconds)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoffHeader {
    machine: u16,
    sections: u16,
    time: u32,
    symbols_at: u32,
    symbols: u32,
    optional_size: u16,
}

fn coff_header(bytes: &[u8; COFF_HEADER]) -> CoffHeader {
    CoffHeader {
        machine: u16_at(bytes, 0, false),
        sections: u16_at(bytes, 2, false),
        time: u32_at(bytes, 4, false),
        symbols_at: u32_at(bytes, 8, false),
        symbols: u32_at(bytes, 12, false),
        optional_size: u16_at(bytes, 16, false),
    }
}

fn pe_pointer(bytes: &[u8; DOS_HEADER]) -> Field<u32> {
    field_check(&bytes[..2] == b"MZ", "not a PE image: no MZ header")?;
    let pointer = u32_at(bytes, PE_POINTER_AT, false);
    field_check(
        pointer >= DOS_HEADER as u32,
        "PE header pointer lies inside the MZ header",
    )?;
    Ok(pointer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OptionalHeader {
    file_alignment: u32,
    directories: [(u32, u32); DIRECTORIES],
}

const NO_OPTIONAL_HEADER: OptionalHeader = OptionalHeader {
    file_alignment: 0,
    directories: [(0, 0); DIRECTORIES],
};

fn optional_header(bytes: &[u8]) -> Field<OptionalHeader> {
    field_check(bytes.len() >= 2, "PE optional header is too short")?;
    let fixed = match u16_at(bytes, 0, false) {
        PE32_MAGIC => PE32_FIXED,
        PE32_PLUS_MAGIC => PE32_PLUS_FIXED,
        _ => return Err(BadField("PE optional header is neither PE32 nor PE32+")),
    };
    field_check(bytes.len() >= fixed, "PE optional header is too short")?;
    let count = u32_at(bytes, fixed - 4, false) as usize;
    field_check(
        count <= DIRECTORIES,
        "PE optional header lists more than 16 data directories",
    )?;
    field_check(
        bytes.len() >= fixed + 8 * count,
        "PE data directories run past the optional header",
    )?;
    let mut directories = [(0, 0); DIRECTORIES];
    for (index, directory) in directories.iter_mut().enumerate().take(count) {
        let at = fixed + 8 * index;
        *directory = (u32_at(bytes, at, false), u32_at(bytes, at + 4, false));
    }
    Ok(OptionalHeader {
        file_alignment: u32_at(bytes, FILE_ALIGNMENT_AT, false),
        directories,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Section {
    name: [u8; 8],
    virtual_size: u32,
    address: u32,
    raw_size: u32,
    raw_at: u32,
}

impl Section {
    fn data_size(&self) -> u32 {
        // EXTERNAL(7-Zip PeHandler): a section's data is its raw size, cut to its virtual size.
        if self.virtual_size == 0 {
            self.raw_size
        } else {
            self.raw_size.min(self.virtual_size)
        }
    }

    fn holds_address(&self, address: u32) -> bool {
        address >= self.address
            && u64::from(address) < u64::from(self.address) + u64::from(self.raw_size)
    }
}

fn section_header(bytes: &[u8; SECTION_HEADER]) -> Section {
    Section {
        name: bytes[..8].try_into().unwrap(),
        virtual_size: u32_at(bytes, 8, false),
        address: u32_at(bytes, 12, false),
        raw_size: u32_at(bytes, 16, false),
        raw_at: u32_at(bytes, 20, false),
    }
}

// EXTERNAL(7-Zip on Unix): a byte that is not part of valid UTF-8 becomes U+EF00 plus the byte.
fn unix_name(bytes: &[u8]) -> String {
    let mut name = String::new();
    for chunk in bytes.utf8_chunks() {
        name.push_str(chunk.valid());
        name.extend(
            chunk
                .invalid()
                .iter()
                .map(|&byte| char::from_u32(0xEF00 + u32::from(byte)).unwrap()),
        );
    }
    name
}

fn section_name(raw: &[u8; 8]) -> String {
    let end = raw.iter().position(|&byte| byte == 0).unwrap_or(raw.len());
    unix_name(&raw[..end]).replace(['/', '\\'], "_")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TeHeader {
    sections: u8,
    stripped: u16,
}

fn te_header(bytes: &[u8; TE_HEADER], machines: &[u16]) -> Field<TeHeader> {
    field_check(&bytes[..2] == TE_SIGNATURE, "not a TE image: no VZ header")?;
    field_check(
        machines.contains(&u16_at(bytes, 2, false)),
        "TE image names a machine 7-Zip does not know",
    )?;
    let sections = bytes[4];
    field_check(sections > 0, "TE image has no sections")?;
    let subsystem = bytes[5];
    field_check(
        subsystem <= TE_LAST_SUBSYSTEM && subsystem != TE_UNUSED_SUBSYSTEM,
        "TE image names a subsystem 7-Zip does not know",
    )?;
    field_check(
        [28, 36]
            .iter()
            .all(|&at| u32_at(bytes, at, false) <= TE_LARGEST_DIRECTORY),
        "TE data directory is larger than 2^28 bytes",
    )?;
    Ok(TeHeader {
        sections,
        stripped: u16_at(bytes, 6, false),
    })
}

struct Reader<'a> {
    file: &'a ArchiveFile,
    length: u64,
    names: Names<'a>,
    archive_time: Option<SystemTime>,
}

impl Reader<'_> {
    fn read(&self, span: Span, what: &str) -> Result<Vec<u8>> {
        read_span(self.file, span, self.length, what)
    }

    fn sections(&self, at: u64, count: u64) -> Result<Vec<Section>> {
        let table = self.read(
            Span {
                offset: at,
                size: count * SECTION_HEADER as u64,
            },
            "section table",
        )?;
        Ok(table
            .chunks_exact(SECTION_HEADER)
            .map(|bytes| section_header(bytes.try_into().unwrap()))
            .collect())
    }

    fn item(&mut self, name: String, data: Data, modified: Option<SystemTime>) -> Result<Item> {
        let name = self.names.or_default(name);
        self.names.charge(&name)?;
        Ok(Item {
            name,
            data,
            modified: modified.or(self.archive_time),
        })
    }
}

fn pe_family(reader: &mut Reader, form: Form) -> Result<Image> {
    let label = form.label();
    let mut head = [0u8; DOS_HEADER];
    let (sections_at, header, directories) = match form {
        Form::Pe => {
            read_exact_at(reader.file, &mut head, 0).context("truncated MZ header")?;
            let pointer = u64::from(pe_pointer(&head)?);
            let mut bytes = [0u8; 4 + COFF_HEADER];
            read_exact_at(reader.file, &mut bytes, pointer).context("truncated PE header")?;
            ensure!(&bytes[..4] == PE_SIGNATURE, "PE header has no PE signature");
            let header = coff_header(bytes[4..].try_into().unwrap());
            let optional = reader.read(
                Span {
                    offset: pointer + 4 + COFF_HEADER as u64,
                    size: u64::from(header.optional_size),
                },
                "PE optional header",
            )?;
            let directories = optional_header(&optional)?;
            (
                pointer + 4 + COFF_HEADER as u64 + u64::from(header.optional_size),
                header,
                directories,
            )
        }
        _ => {
            let mut bytes = [0u8; COFF_HEADER];
            read_exact_at(reader.file, &mut bytes, 0).context("truncated COFF header")?;
            let header = coff_header(&bytes);
            ensure!(
                COFF_MACHINES.contains(&header.machine)
                    && header.optional_size == 0
                    && header.symbols_at != 0,
                "not a COFF object"
            );
            (COFF_HEADER as u64, header, NO_OPTIONAL_HEADER)
        }
    };
    ensure!(
        header.sections > 0 || form == Form::Pe,
        "{label} file has no sections"
    );
    let sections = reader.sections(sections_at, u64::from(header.sections))?;
    let modified = time_from(header.time);
    let sections_end = sections_at + u64::from(header.sections) * SECTION_HEADER as u64;
    let mut end = sections_end;
    for section in &sections {
        end = end.max(u64::from(section.raw_at) + u64::from(section.raw_size));
    }
    // EXTERNAL(7-Zip PeHandler): it reads resources from the section named `.rsrc`, from its
    // start, whatever the resource data directory says.
    let resource_section = sections
        .iter()
        .position(|section| section.name == *RESOURCE_SECTION);
    let resources = match resource_section {
        Some(index) if form == Form::Pe => {
            resources::read(reader, &sections[index], directories.file_alignment)?
                .map(|found| (index, found))
        }
        _ => None,
    };
    let mut items = Vec::with_capacity(sections.len());
    for (index, section) in sections.iter().enumerate() {
        if resources.as_ref().is_some_and(|(at, _)| *at == index) {
            continue;
        }
        let (relocations, relocation_size) = directories.directories[RELOCATIONS];
        // EXTERNAL(7-Zip PeHandler): the `.reloc` section that the relocation directory starts
        // is cut to the directory's size.
        let size = if section.name == *RELOCATION_SECTION && section.address == relocations {
            relocation_size
        } else {
            section.data_size()
        };
        let span = Span {
            offset: u64::from(section.raw_at),
            size: u64::from(size),
        };
        items.push(reader.item(section_name(&section.name), Data::Range(span), modified)?);
    }
    if let Some((_, found)) = resources {
        for resource in found {
            let time = if resource.timed { modified } else { None };
            items.push(reader.item(resource.name, resource.data, time)?);
        }
    }
    let (offset, size) = directories.directories[CERTIFICATES];
    if size > 0 {
        let span = Span {
            offset: u64::from(offset),
            size: u64::from(size),
        };
        end = end.max(span.end()?);
        items.push(reader.item("CERTIFICATE".to_owned(), Data::Range(span), modified)?);
    }
    let (address, size) = directories.directories[DEBUG];
    // EXTERNAL(7-Zip PeHandler): it refuses a debug directory of more than 16 entries, and one
    // inside a section that is no whole number of them.
    ensure!(
        u64::from(size) / DEBUG_ENTRY <= DEBUG_ENTRIES,
        "PE debug directory holds more than 16 entries"
    );
    let debug_section = sections
        .iter()
        .find(|section| section.holds_address(address));
    if let Some(section) = debug_section.filter(|_| size > 0 && address != 0) {
        ensure!(
            u64::from(size).is_multiple_of(DEBUG_ENTRY),
            "PE debug directory is not a whole number of 28-byte entries"
        );
        let at = u64::from(section.raw_at) + u64::from(address - section.address);
        let entries = reader.read(
            Span {
                offset: at,
                size: u64::from(size),
            },
            "PE debug directory",
        )?;
        for (index, entry) in entries.chunks_exact(DEBUG_ENTRY as usize).enumerate() {
            let span = Span {
                offset: u64::from(u32_at(entry, 24, false)),
                size: u64::from(u32_at(entry, 16, false)),
            };
            let inside = sections.iter().any(|section| {
                span.offset >= u64::from(section.raw_at)
                    && span.offset < u64::from(section.raw_at) + u64::from(section.raw_size)
            });
            if span.size == 0 || span.offset == 0 || inside {
                continue;
            }
            end = end.max(span.end()?);
            items.push(reader.item(format!(".debug{index}"), Data::Range(span), modified)?);
        }
    }
    if header.symbols > 0 && header.symbols_at != 0 {
        let table = Span {
            offset: u64::from(header.symbols_at),
            size: u64::from(header.symbols) * SYMBOL,
        };
        let mut length = [0u8; 4];
        let read = read_up_to(reader.file, &mut length, table.end()?)?;
        let strings = if read == 4 {
            u32::from_le_bytes(length)
        } else {
            0
        };
        let span = Span {
            offset: table.offset,
            size: table.size + u64::from(strings),
        };
        if form == Form::Coff {
            ensure!(
                read == 4 && span.end()? <= reader.length,
                "not a COFF object: its symbols run past the end of the file"
            );
        }
        end = end.max(span.end()?);
        items.push(reader.item("COFF_SYMBOLS".to_owned(), Data::Range(span), modified)?);
    }
    if form == Form::Pe {
        let mut covered: Vec<Span> = sections
            .iter()
            .map(|section| Span {
                offset: u64::from(section.raw_at),
                size: u64::from(section.raw_size),
            })
            .chain(items.iter().filter_map(|item| item.data.span()))
            .filter(|span| span.size > 0)
            .collect();
        covered.sort_unstable_by_key(|span| span.offset);
        // EXTERNAL(7-Zip PeHandler): each stretch past the first 4 KiB that no section or other
        // item covers becomes a file named `[0]`, `[1]` and on.
        let mut at = PE_UNLISTED_HEAD.max(sections_end);
        let mut gaps = Vec::new();
        for span in covered {
            if span.offset > at {
                gaps.push(Span {
                    offset: at,
                    size: span.offset - at,
                });
            }
            at = at.max(span.end()?);
        }
        for (index, gap) in gaps.into_iter().enumerate() {
            items.push(reader.item(format!("[{index}]"), Data::Range(gap), modified)?);
        }
    }
    Ok(Image { form, items, end })
}

fn te_image(reader: &mut Reader, machines: &[u16]) -> Result<Image> {
    let mut bytes = [0u8; TE_HEADER];
    read_exact_at(reader.file, &mut bytes, 0).context("truncated TE header")?;
    let header = te_header(&bytes, machines)?;
    let count = u64::from(header.sections);
    let table_end = TE_HEADER as u64 + count * SECTION_HEADER as u64;
    let sections = reader.sections(TE_HEADER as u64, count)?;
    let mut end = table_end;
    let mut items = Vec::with_capacity(sections.len());
    for section in &sections {
        let offset = (u64::from(section.raw_at) + TE_HEADER as u64)
            .checked_sub(u64::from(header.stripped))
            .filter(|&offset| offset >= table_end)
            .context("TE section data lies before the end of its section table")?;
        end = end.max(offset + u64::from(section.raw_size));
        let span = Span {
            offset,
            size: u64::from(section.data_size()),
        };
        let end = section.name.iter().position(|&byte| byte == 0).unwrap_or(8);
        let name = unix_name(&section.name[..end]).replace('\\', "_");
        items.push(reader.item(name, Data::Range(span), None)?);
    }
    Ok(Image {
        form: Form::Te,
        items,
        end,
    })
}

mod resources;

mod version;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ElfHeader {
    wide: bool,
    big: bool,
    program_at: u64,
    programs: u16,
    program_size: u16,
    sections_at: u64,
    sections: u16,
    section_size: u16,
    names: u16,
    header_size: u16,
}

fn elf_header(bytes: &[u8; ELF64_HEADER]) -> Field<ElfHeader> {
    field_check(&bytes[..4] == ELF_MAGIC, "not an ELF file")?;
    let wide = match bytes[4] {
        1 => false,
        2 => true,
        _ => return Err(BadField("ELF class is neither 32 nor 64 bits")),
    };
    let big = match bytes[5] {
        1 => false,
        2 => true,
        _ => {
            return Err(BadField(
                "ELF data encoding is neither little nor big endian",
            ));
        }
    };
    field_check(
        bytes[6] == 1 && u32_at(bytes, 20, big) == 1,
        "ELF version is not 1",
    )?;
    field_check(
        bytes[ELF_IDENT_PADDING..ELF_IDENT]
            .iter()
            .all(|&byte| byte == 0),
        "ELF identification padding is not zero",
    )?;
    let (program_at, sections_at, rest) = if wide {
        (u64_at(bytes, 32, big), u64_at(bytes, 40, big), 52)
    } else {
        (
            u64::from(u32_at(bytes, 28, big)),
            u64::from(u32_at(bytes, 32, big)),
            40,
        )
    };
    let header = ElfHeader {
        wide,
        big,
        program_at,
        sections_at,
        header_size: u16_at(bytes, rest, big),
        program_size: u16_at(bytes, rest + 2, big),
        programs: u16_at(bytes, rest + 4, big),
        section_size: u16_at(bytes, rest + 6, big),
        sections: u16_at(bytes, rest + 8, big),
        names: u16_at(bytes, rest + 10, big),
    };
    let (fixed, program, section) = if wide {
        (ELF64_HEADER, ELF64_PROGRAM, ELF64_SECTION)
    } else {
        (ELF32_HEADER, ELF32_PROGRAM, ELF32_SECTION)
    };
    field_check(
        usize::from(header.header_size) == fixed,
        "ELF header size does not match its class",
    )?;
    field_check(
        matches!(usize::from(header.program_size), 0) && header.programs == 0
            || usize::from(header.program_size) == program,
        "ELF program header size does not match its class",
    )?;
    field_check(
        matches!(usize::from(header.section_size), 0) && header.sections == 0
            || usize::from(header.section_size) == section,
        "ELF section header size does not match its class",
    )?;
    // EXTERNAL(7-Zip ElfHandler): a table offset is 0, or past the header and aligned to the
    // class's word.
    let word = if wide { 8 } else { 4 };
    field_check(
        [header.program_at, header.sections_at]
            .iter()
            .all(|&at| at == 0 || at >= fixed as u64 && at.is_multiple_of(word)),
        "ELF table offset lies inside the header or is not aligned",
    )?;
    Ok(header)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ElfSection {
    name: u32,
    kind: u32,
    span: Span,
    entry_size: u64,
}

fn elf_section(bytes: &[u8], header: &ElfHeader) -> ElfSection {
    let big = header.big;
    let (offset, size, entry_size) = if header.wide {
        (
            u64_at(bytes, 24, big),
            u64_at(bytes, 32, big),
            u64_at(bytes, 56, big),
        )
    } else {
        (
            u64::from(u32_at(bytes, 16, big)),
            u64::from(u32_at(bytes, 20, big)),
            u64::from(u32_at(bytes, 36, big)),
        )
    };
    ElfSection {
        name: u32_at(bytes, 0, big),
        kind: u32_at(bytes, 4, big),
        span: Span { offset, size },
        entry_size,
    }
}

fn elf_segment(bytes: &[u8], header: &ElfHeader) -> (u32, Span) {
    let big = header.big;
    let span = if header.wide {
        Span {
            offset: u64_at(bytes, 8, big),
            size: u64_at(bytes, 32, big),
        }
    } else {
        Span {
            offset: u64::from(u32_at(bytes, 4, big)),
            size: u64::from(u32_at(bytes, 16, big)),
        }
    };
    (u32_at(bytes, 0, big), span)
}

// EXTERNAL(7-Zip ElfHandler): section names are read one byte to one character.
fn elf_name(reader: &Reader, table: Option<Span>, at: u32) -> Result<String> {
    let Some(table) = table.filter(|table| u64::from(at) < table.size) else {
        return Ok("ERROR".to_owned());
    };
    let left = table.size - u64::from(at);
    let mut bytes = vec![0u8; left.min(LONGEST_NAME as u64 + 1) as usize];
    read_exact_at(reader.file, &mut bytes, table.offset + u64::from(at))?;
    let Some(end) = bytes.iter().position(|&byte| byte == 0) else {
        ensure!(
            left <= LONGEST_NAME as u64,
            "ELF section name is longer than 4096 bytes"
        );
        return Ok("ERROR".to_owned());
    };
    Ok(bytes[..end].iter().map(|&byte| char::from(byte)).collect())
}

fn elf_image(reader: &mut Reader) -> Result<Image> {
    let mut bytes = [0u8; ELF64_HEADER];
    let read = read_up_to(reader.file, &mut bytes, 0)?;
    let header = elf_header(&bytes)?;
    let fixed = if header.wide {
        ELF64_HEADER
    } else {
        ELF32_HEADER
    };
    ensure!(read >= fixed, "truncated ELF header");
    let mut end = fixed as u64;
    let mut items = Vec::new();
    let programs = Span {
        offset: header.program_at,
        size: u64::from(header.programs) * u64::from(header.program_size),
    };
    let mut segments = Vec::new();
    if header.programs > 0 {
        end = end.max(programs.end()?);
        let raw = reader.read(programs, "ELF program headers")?;
        for bytes in raw.chunks_exact(usize::from(header.program_size)) {
            let (kind, span) = elf_segment(bytes, &header);
            end = end.max(span.end()?);
            segments.push((kind, span));
        }
    }
    if header.sections > 0 {
        let table = Span {
            offset: header.sections_at,
            size: u64::from(header.sections) * u64::from(header.section_size),
        };
        end = end.max(table.end()?);
        let entry = usize::from(header.section_size);
        let raw = reader.read(table, "ELF section headers")?;
        let sections: Vec<ElfSection> = raw
            .chunks_exact(entry)
            .map(|bytes| elf_section(bytes, &header))
            .collect();
        let names = sections
            .get(usize::from(header.names))
            .filter(|_| header.names != 0)
            .map(|section| section.span);
        // EXTERNAL(7-Zip ElfHandler): it refuses an entry size past 1023 bytes, or past 2^31 - 1
        // in the first section.
        ensure!(
            sections.iter().enumerate().all(|(index, section)| {
                section.entry_size
                    <= if index == 0 {
                        ELF_FIRST_LARGEST_ENTRY_SIZE
                    } else {
                        ELF_LARGEST_ENTRY_SIZE
                    }
            }),
            "ELF section entry size is too large"
        );
        for (index, section) in sections.iter().enumerate() {
            let size = if section.kind == ELF_NOBITS {
                0
            } else {
                section.span.size
            };
            let span = Span {
                offset: section.span.offset,
                size,
            };
            end = end.max(span.end()?);
            let name = if index == 0 {
                "NULL".to_owned()
            } else {
                elf_name(reader, names, section.name)?
            };
            let span = Span {
                offset: section.span.offset,
                size,
            };
            items.push(reader.item(name, Data::Range(span), None)?);
        }
    } else {
        let mut number = 0;
        for (kind, span) in segments {
            if kind == ELF_PHDR {
                continue;
            }
            items.push(reader.item(number.to_string(), Data::Range(span), None)?);
            number += 1;
        }
    }
    Ok(Image {
        form: Form::Elf,
        items,
        end,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MachHeader {
    wide: bool,
    big: bool,
    commands: u32,
    commands_size: u32,
}

fn mach_header(bytes: &[u8; MACHO64_HEADER]) -> Field<MachHeader> {
    let magic = u32_at(bytes, 0, false);
    let index = MACHO_MAGICS
        .iter()
        .position(|&known| known == magic)
        .ok_or(BadField("not a Mach-O file"))?;
    let (wide, big) = (index % 2 == 1, index >= 2);
    let commands = u32_at(bytes, 16, big);
    let commands_size = u32_at(bytes, 20, big);
    field_check(commands > 0, "Mach-O file has no load commands")?;
    field_check(
        commands_size <= MACHO_LARGEST_COMMANDS
            && u64::from(commands) * 8 <= u64::from(commands_size),
        "Mach-O load commands are larger than 16 MiB or smaller than their count",
    )?;
    Ok(MachHeader {
        wide,
        big,
        commands,
        commands_size,
    })
}

// EXTERNAL(7-Zip MachoHandler): a name byte below 0x20 or past 0x7F, or `/`, becomes `_`.
fn mach_name(raw: &[u8]) -> String {
    raw.iter()
        .take(MACHO_NAME)
        .take_while(|&&byte| byte != 0)
        .map(|&byte| match byte {
            0x20..0x80 if byte != b'/' => char::from(byte),
            _ => '_',
        })
        .collect()
}

// EXTERNAL(7-Zip MachoHandler): a command's size is a multiple of 8, or of 4 in a 32-bit
// file, and the commands it reads have their exact sizes.
fn command_size_fits(kind: u32, command: &[u8], header: MachHeader) -> bool {
    let size = command.len();
    let exact = match kind {
        MACHO_SYMTAB => MACHO_SYMTAB_SIZE,
        MACHO_UUID => MACHO_UUID_SIZE,
        MACHO_VERSION_MIN_MACOS
        | MACHO_VERSION_MIN_IOS
        | MACHO_VERSION_MIN_TVOS
        | MACHO_VERSION_MIN_WATCHOS
        | MACHO_SOURCE_VERSION => MACHO_VERSION_SIZE,
        MACHO_BUILD_VERSION if size >= MACHO_BUILD_VERSION_SIZE => {
            MACHO_BUILD_VERSION_SIZE + MACHO_BUILD_TOOL * u32_at(command, 20, header.big) as usize
        }
        MACHO_BUILD_VERSION => MACHO_BUILD_VERSION_SIZE,
        _ => size,
    };
    size == exact && size.is_multiple_of(if header.wide { 8 } else { 4 })
}

fn macho_image(reader: &mut Reader) -> Result<Image> {
    let mut bytes = [0u8; MACHO64_HEADER];
    read_up_to(reader.file, &mut bytes, 0)?;
    let header = mach_header(&bytes)?;
    let big = header.big;
    let fixed = if header.wide {
        MACHO64_HEADER
    } else {
        MACHO32_HEADER
    };
    let commands = reader.read(
        Span {
            offset: fixed as u64,
            size: u64::from(header.commands_size),
        },
        "Mach-O load commands",
    )?;
    let mut end = (fixed + commands.len()) as u64;
    let mut segments: Vec<Span> = Vec::new();
    let mut items = Vec::new();
    let mut at = 0usize;
    for _ in 0..header.commands {
        ensure!(
            commands.len() - at >= 8,
            "Mach-O load commands run past their size"
        );
        let kind = u32_at(&commands, at, big);
        let size = u32_at(&commands, at + 4, big) as usize;
        ensure!(
            size >= 8 && size <= commands.len() - at,
            "Mach-O load command size is out of range"
        );
        let command = &commands[at..at + size];
        ensure!(
            command_size_fits(kind, command, header),
            "Mach-O load command {kind:#x} has a size 7-Zip does not accept"
        );
        match kind {
            MACHO_SEGMENT32 | MACHO_SEGMENT64 => {
                let wide = kind == MACHO_SEGMENT64;
                let (base, entry) = if wide { (72, 80) } else { (56, 68) };
                ensure!(size >= base, "Mach-O segment command is too short");
                let (file_span, count) = if wide {
                    (
                        Span {
                            offset: u64_at(command, 40, big),
                            size: u64_at(command, 48, big),
                        },
                        u32_at(command, 64, big),
                    )
                } else {
                    (
                        Span {
                            offset: u64::from(u32_at(command, 32, big)),
                            size: u64::from(u32_at(command, 36, big)),
                        },
                        u32_at(command, 48, big),
                    )
                };
                ensure!(
                    u64::from(count) * entry as u64 == (size - base) as u64,
                    "Mach-O segment command size does not match its sections"
                );
                end = end.max(file_span.end()?);
                let segment_name = mach_name(&command[8..24]);
                if count == 0 {
                    items.push(reader.item(segment_name, Data::Range(file_span), None)?);
                }
                for index in 0..count as usize {
                    let section = &command[base + index * entry..base + (index + 1) * entry];
                    let (length, offset, flags) = if wide {
                        (
                            u64_at(section, 40, big),
                            u64::from(u32_at(section, 48, big)),
                            u32_at(section, 64, big),
                        )
                    } else {
                        (
                            u64::from(u32_at(section, 36, big)),
                            u64::from(u32_at(section, 40, big)),
                            u32_at(section, 56, big),
                        )
                    };
                    let zero_fill = flags & 0xFF == MACHO_ZEROFILL;
                    let name = format!(
                        "{}{}",
                        mach_name(&section[16..32]),
                        mach_name(&section[..16])
                    );
                    let span = Span {
                        offset,
                        size: if zero_fill { 0 } else { length },
                    };
                    items.push(reader.item(name, Data::Range(span), None)?);
                }
                segments.push(file_span);
            }
            MACHO_SYMTAB => {
                let symbols = Span {
                    offset: u64::from(u32_at(command, 8, big)),
                    size: u64::from(u32_at(command, 12, big)) * if header.wide { 16 } else { 12 },
                };
                let strings = Span {
                    offset: u64::from(u32_at(command, 16, big)),
                    size: u64::from(u32_at(command, 20, big)),
                };
                let inside = |span: Span| {
                    segments.iter().any(|segment| {
                        span.offset >= segment.offset
                            && u128::from(span.offset) + u128::from(span.size)
                                <= u128::from(segment.offset) + u128::from(segment.size)
                    })
                };
                if !inside(symbols) || !inside(strings) {
                    end = end.max(symbols.end()?).max(strings.end()?);
                    items.push(reader.item(
                        "_SYMTAB_TABLE".to_owned(),
                        Data::Range(symbols),
                        None,
                    )?);
                    items.push(reader.item(
                        "_SYMTAB_STRINGS".to_owned(),
                        Data::Range(strings),
                        None,
                    )?);
                }
            }
            _ => {}
        }
        at += size;
    }
    Ok(Image {
        form: Form::MachO,
        items,
        end,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FatArch {
    cpu: u32,
    subtype: u32,
    span: Span,
    align: u32,
}

fn fat_arch(bytes: &[u8; FAT_ARCH]) -> Field<FatArch> {
    let cpu = u32_at(bytes, 0, true);
    field_check(
        cpu & 0x00FF_FF00 == 0,
        "Mach-O universal slice names an unknown CPU type",
    )?;
    let subtype = u32_at(bytes, 4, true);
    // EXTERNAL(7-Zip MubHandler): it refuses a slice whose subtype sets any of bits 10 to 15.
    field_check(
        subtype & FAT_SUBTYPE_UNUSED == 0,
        "Mach-O universal slice names an unknown CPU subtype",
    )?;
    let align = u32_at(bytes, 16, true);
    field_check(
        align <= FAT_LARGEST_ALIGN,
        "Mach-O universal slice alignment is past 2^31",
    )?;
    let span = Span {
        offset: u64::from(u32_at(bytes, 8, true)),
        size: u64::from(u32_at(bytes, 12, true)),
    };
    field_check(span.size > 0, "Mach-O universal slice is empty")?;
    Ok(FatArch {
        cpu,
        subtype,
        span,
        align,
    })
}

fn cpu_name(cpu: u32) -> Option<&'static str> {
    Some(match cpu {
        6 => "m68k",
        7 => "x86",
        11 => "hppa",
        12 => "arm",
        14 => "sparc",
        18 => "ppc",
        0x0100_0007 => "x64",
        0x0100_000C => "arm64",
        0x0100_0012 => "ppc64",
        0x0200_000C => "arm64_32",
        _ => return None,
    })
}

fn subtype_name(cpu: u32, subtype: u32) -> Option<&'static str> {
    Some(match (cpu, subtype) {
        (6, 1)
        | (7, 3)
        | (11 | 12 | 14 | 18 | 0x0100_000C | 0x0100_0012, 0)
        | (0x0100_0007, 3)
        | (0x0200_000C, 1) => "",
        (7, 4) => "_i486",
        (7, 0x84) => "_i486sx",
        (7, 5) => "_pentium",
        (7, 0x16) => "_pentpro",
        (7, 0x36) => "_pentIIm3",
        (7, 0x56) => "_pentIIm5",
        (7, 0xA) => "_pentium4",
        (0x0100_0007, 8) => "h",
        (12, 1) => "a500_arch",
        (12, 2) => "a500",
        (12, 3) => "a440",
        (12, 4) => "m4",
        (12, 5) => "v4t",
        (12, 6) => "v6",
        (12, 7) => "v5tej",
        (12, 8) => "xscale",
        (12, 9) => "v7",
        (12, 10) => "v7f",
        (12, 11) => "v7s",
        (12, 12) => "v7k",
        (12, 13) => "v8",
        (12, 14) => "v6m",
        (12, 15) => "v7m",
        (12, 16) => "v7em",
        (12, 17) => "v8m_main",
        (12, 18) => "v8m_base",
        (12, 19) => "v8_1m_main",
        (0x0100_000C, 1) => "v8",
        (0x0100_000C, 2) => "e",
        (0x0100_000C, 12) => "e_x1",
        (18, 1) => "601",
        (18, 2) => "602",
        (18, 3) => "603",
        (18, 4) => "603e",
        (18, 5) => "603ev",
        (18, 6) => "604",
        (18, 7) => "604e",
        (18, 8) => "620",
        (18, 9) => "750",
        (18, 10) => "7400",
        (18, 11) => "7450",
        (18, 100) => "970",
        (0x0100_0012, 100) => "-970",
        _ => return None,
    })
}

fn arch_name(cpu: u32, subtype: u32) -> String {
    let mut name = match cpu_name(cpu) {
        Some(known) => known.to_owned(),
        None => {
            let base = cpu & !(CPU_ARCH_ABI64 | CPU_ARCH_ABI64_32);
            let mut name = cpu_name(base).map_or_else(|| format!("cpu{base}"), str::to_owned);
            if cpu & CPU_ARCH_ABI64 != 0 {
                name.push_str("_64");
            }
            if cpu & CPU_ARCH_ABI64_32 != 0 {
                name.push_str("_64_32");
            }
            name
        }
    };
    let low = subtype & 0x00FF_FFFF;
    let caps = subtype >> 24;
    let named = cpu_name(cpu).and_then(|_| subtype_name(cpu, low));
    match named {
        Some(suffix) => name.push_str(suffix),
        None => name.push_str(&format!("_r{low}")),
    }
    match caps {
        0 => {}
        CPU_SUBTYPE_LIB64 if cpu & CPU_ARCH_ABI64 != 0 => {}
        CPU_SUBTYPE_LIB64 => name.push_str("_lib64"),
        other => name.push_str(&format!("_caps_{other:02x}")),
    }
    name
}

fn mub_image(reader: &mut Reader) -> Result<Image> {
    let mut head = [0u8; FAT_HEADER];
    read_exact_at(reader.file, &mut head, 0).context("truncated Mach-O universal header")?;
    let count = u32_at(&head, 4, true);
    ensure!(
        (1..=FAT_LARGEST_COUNT).contains(&count),
        "Mach-O universal file holds no slices or more than 8"
    );
    let table = reader.read(
        Span {
            offset: FAT_HEADER as u64,
            size: u64::from(count) * FAT_ARCH as u64,
        },
        "Mach-O universal header",
    )?;
    let header_end = (FAT_HEADER + table.len()) as u64;
    let archive = reader.names.archive;
    let stem = match archive.len().checked_sub(4) {
        Some(at) if archive.is_char_boundary(at) && archive[at..].eq_ignore_ascii_case(".mub") => {
            &archive[..at]
        }
        _ => archive,
    }
    .to_owned();
    let mut arches: Vec<FatArch> = Vec::new();
    for bytes in table.chunks_exact(FAT_ARCH) {
        let arch = fat_arch(bytes.try_into().unwrap())?;
        ensure!(
            arch.span.offset >= header_end,
            "Mach-O universal slice overlaps its header"
        );
        for earlier in &arches {
            ensure!(
                arch.span.offset >= earlier.span.end()?
                    || earlier.span.offset >= arch.span.end()?,
                "two Mach-O universal slices share bytes"
            );
        }
        arches.push(arch);
    }
    let mut end = header_end;
    let mut items = Vec::new();
    for arch in &arches {
        end = end.max(arch.span.end()?);
        let name = format!("{stem}.{}", arch_name(arch.cpu, arch.subtype));
        items.push(reader.item(name, Data::Range(arch.span), None)?);
    }
    Ok(Image {
        form: Form::Mub,
        items,
        end,
    })
}

fn magic_form(head: &[u8]) -> Option<Form> {
    if head.starts_with(b"MZ") {
        return Some(Form::Pe);
    }
    if head.starts_with(TE_SIGNATURE) {
        return Some(Form::Te);
    }
    if head.starts_with(ELF_MAGIC) {
        return Some(Form::Elf);
    }
    if head.len() >= 4 && MACHO_MAGICS.contains(&u32_at(head, 0, false)) {
        return Some(Form::MachO);
    }
    if head.len() >= 8 && u32_at(head, 0, true) == FAT_MAGIC && head[4..7] == [0, 0, 0] {
        return Some(Form::Mub);
    }
    None
}

fn archive_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parse(file: &ArchiveFile, path: &Path, form: Form) -> Result<Image> {
    let archive = archive_name(path);
    let mut reader = Reader {
        file,
        length: file.len()?,
        names: Names {
            archive: &archive,
            bytes: 0,
        },
        // EXTERNAL(7-Zip): an item that stores no time gets the archive file's modification time.
        archive_time: std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok(),
    };
    let image = match form {
        Form::Pe | Form::Coff => pe_family(&mut reader, form),
        Form::Te => te_image(&mut reader, &COFF_MACHINES[1..]),
        Form::Elf => elf_image(&mut reader),
        Form::MachO => macho_image(&mut reader),
        Form::Mub => mub_image(&mut reader),
    }?;
    let label = image.form.label();
    let length = reader.length;
    ensure!(
        image.end <= length,
        "{label} file places data up to byte {}, past the end of the {length}-byte file",
        image.end
    );
    ensure!(
        image.end == length || image.form == Form::Coff,
        "{label} file ends at byte {}, and the {length}-byte file holds more data after it, which 7-Zip opens only as an archive after an executable",
        image.end
    );
    for item in &image.items {
        if let Some(span) = item.data.span().filter(|span| span.size > 0) {
            ensure!(
                span.end()? <= length,
                "{label} item {:?} runs past the end of the file",
                item.name
            );
        }
    }
    Ok(image)
}

fn detect(file: &ArchiveFile) -> Result<Option<Form>> {
    let mut head = [0u8; HEAD];
    let read = read_up_to(file, &mut head, 0)?;
    let head = &head[..read];
    if let Some(form) = magic_form(head) {
        return Ok(Some(form));
    }
    if read >= COFF_HEADER {
        let header = coff_header(head[..COFF_HEADER].try_into().unwrap());
        if COFF_MACHINES.contains(&header.machine)
            && header.optional_size == 0
            && header.symbols_at != 0
            && header.sections > 0
            && u64::from(header.sections) * SECTION_HEADER as u64 + COFF_HEADER as u64
                <= file.len()?
        {
            return Ok(Some(Form::Coff));
        }
    }
    Ok(None)
}

fn zip_ends_the_file(file: &ArchiveFile) -> std::io::Result<bool> {
    let length = file.len()?;
    let from = length.saturating_sub(ZIP_TAIL);
    let mut tail = vec![0u8; (length - from) as usize];
    let read = read_up_to(file, &mut tail, from)?;
    Ok(tail[..read]
        .windows(4)
        .any(|window| window == b"PK\x05\x06"))
}

/// Returns whether the file is a PE, TE, ELF, Mach-O or Mach-O universal
/// file that this reader reads whole, with no data after it.
pub fn sniff(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    Ok(match detect(&file)? {
        None | Some(Form::Coff) => false,
        Some(form) => parse(&file, path, form).is_ok(),
    })
}

/// Returns whether the file starts as a PE, TE, ELF, Mach-O or Mach-O
/// universal file and does not end in a ZIP, as a self-extracting ZIP does.
pub fn sniff_damaged(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    Ok(match detect(&file)? {
        None | Some(Form::Coff) => false,
        Some(_) => !zip_ends_the_file(&file)?,
    })
}

/// Returns whether the file is a COFF object this reader takes.
pub fn sniff_object(path: &Path) -> Result<bool> {
    let file = crate::volumes::open(path)?;
    Ok(detect(&file)? == Some(Form::Coff) && parse(&file, path, Form::Coff).is_ok())
}

/// Returns whether the default engine sends this file to 7-Zip: a Mach-O
/// universal file with the `B9 FA F1 0E` signature, which Zipper does not
/// read.
pub fn needs_backend(path: &Path) -> Result<bool> {
    let mut head = [0u8; 4];
    let read = read_up_to(&crate::volumes::open(path)?, &mut head, 0)?;
    Ok(read == 4 && head == MUB_OTHER_SIGNATURE)
}

fn write_item(tree: &mut tree::Tree, file: &ArchiveFile, item: &Item) -> Result<()> {
    let size = item.data.size();
    match &item.data {
        Data::Range(span) => {
            let mut data = Stretch {
                file,
                at: span.offset,
            }
            .take(span.size);
            tree.file(&item.name, size, FILE_MODE, item.modified, &mut data)
        }
        Data::Built(bytes) => {
            tree.file(&item.name, size, FILE_MODE, item.modified, &mut &bytes[..])
        }
        Data::Prefixed(prefix, span) => {
            let mut data = (&prefix[..]).chain(
                Stretch {
                    file,
                    at: span.offset,
                }
                .take(span.size),
            );
            tree.file(&item.name, size, FILE_MODE, item.modified, &mut data)
        }
    }
}

struct Stretch<'a> {
    file: &'a ArchiveFile,
    at: u64,
}

impl Read for Stretch<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.file.read_at(buffer, self.at)?;
        self.at += read as u64;
        Ok(read)
    }
}

/// Verify and optionally extract a PE image, COFF object, TE image, ELF
/// file, Mach-O file or Mach-O universal file into a new directory. It
/// writes each section, segment or slice as a file named as `7zz x` names
/// it, and the resources of a PE image under `.rsrc`.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let file = crate::volumes::open(path)?;
    let Some(form) = detect(&file)? else {
        bail!("not a PE, COFF, TE, ELF, Mach-O or Mach-O universal file");
    };
    let image = parse(&file, path, form)?;
    tree::extract(image.form.label(), path, destination, options, |tree| {
        for item in &image.items {
            tree.check()?;
            write_item(tree, &file, item)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod field_tests {
    use super::*;

    #[test]
    fn slice_names_follow_what_7zip_writes_for_each_cpu_and_subtype() {
        for (cpu, subtype, name) in [
            (7, 3, "x86"),
            (7, 0, "x86_r0"),
            (7, 0x84, "x86_i486sx"),
            (7, 0x8000_0003, "x86_lib64"),
            (7, 0x8000_0002, "x86_r2_lib64"),
            (7, 0xFF00_0000, "x86_r0_caps_ff"),
            (0x0100_0007, 3, "x64"),
            (0x0100_0007, 8, "x64h"),
            (0x0100_0007, 0x8000_0003, "x64"),
            (0x0100_000C, 0x8000_0002, "arm64e"),
            (0x0100_000C, 12, "arm64e_x1"),
            (0x0100_000C, 1, "arm64v8"),
            (12, 9, "armv7"),
            (12, 19, "armv8_1m_main"),
            (12, 0x100, "arm_r256"),
            (18, 100, "ppc970"),
            (0x0100_0012, 100, "ppc64-970"),
            (0x0200_000C, 1, "arm64_32"),
            (0x0200_000C, 0, "arm64_32_r0"),
            (0x0200_0007, 3, "x86_64_32_r3"),
            (0x0300_0007, 3, "x86_64_64_32_r3"),
            (0x0100_000E, 0, "sparc_64_r0"),
            (0x8100_0007, 3, "cpu2147483655_64_r3"),
            (6, 1, "m68k"),
            (11, 0, "hppa"),
            (13, 0, "cpu13_r0"),
            (0x0100_0000, 0x0100_0003, "cpu0_64_r3_caps_01"),
        ] {
            assert_eq!(arch_name(cpu, subtype), name, "{cpu:#x} {subtype:#x}");
        }
    }

    #[test]
    fn names_take_7zips_unix_spelling_for_bytes_that_are_not_utf8() {
        assert_eq!(unix_name(b"\xc3\xa9t"), "\u{e9}t");
        assert_eq!(unix_name(b"\xe9t"), "\u{efe9}t");
        assert_eq!(section_name(b"a/b\\c\0\0\0"), "a_b_c");
        assert_eq!(
            mach_name(b"a/b\x01\x7f\xc3__________"),
            "a_b_\x7f___________"
        );
        let names = Names {
            archive: "tool.exe",
            bytes: 0,
        };
        assert_eq!(names.or_default(String::new()), "tool");
        let names = Names {
            archive: "tool",
            bytes: 0,
        };
        assert_eq!(names.or_default(String::new()), "tool~");
    }

    #[test]
    fn a_section_holds_its_raw_size_cut_to_its_virtual_size() {
        for (virtual_size, raw_size, expected) in
            [(0, 512, 512), (16, 512, 16), (2000, 512, 512), (8, 0, 0)]
        {
            let section = Section {
                name: [0; 8],
                virtual_size,
                address: 0,
                raw_size,
                raw_at: 0,
            };
            assert_eq!(section.data_size(), expected);
        }
    }

    #[test]
    fn elf_headers_need_7zips_sizes_version_and_padding() {
        let mut header = [0u8; ELF64_HEADER];
        header[..7].copy_from_slice(b"\x7FELF\x02\x01\x01");
        header[20] = 1;
        header[52] = 64;
        assert!(elf_header(&header).is_ok());
        let mut padding = header;
        padding[15] = 1;
        assert!(elf_header(&padding).is_err());
        let mut sections = header;
        sections[60] = 1;
        assert!(elf_header(&sections).is_err());
        sections[58] = 64;
        assert!(elf_header(&sections).is_ok());
        let mut version = header;
        version[20] = 2;
        assert!(elf_header(&version).is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn pe_headers_never_panic_and_keep_the_pointer_past_the_mz_header() {
        let dos: [u8; DOS_HEADER] = kani::any();
        if let Ok(pointer) = pe_pointer(&dos) {
            assert!(pointer >= DOS_HEADER as u32);
            assert!(&dos[..2] == b"MZ");
        }
        let coff: [u8; COFF_HEADER] = kani::any();
        let header = coff_header(&coff);
        assert!(header.machine == u16_at(&coff, 0, false));
        let raw: [u8; SECTION_HEADER] = kani::any();
        let section = section_header(&raw);
        assert!(section.data_size() <= section.raw_size);
    }

    #[kani::proof]
    #[kani::unwind(17)]
    fn optional_headers_keep_at_most_16_directories_inside_their_bytes() {
        let bytes: [u8; PE32_PLUS_FIXED + 8 * DIRECTORIES] = kani::any();
        let length: usize = kani::any();
        kani::assume(length <= bytes.len());
        if let Ok(header) = optional_header(&bytes[..length]) {
            let fixed = if u16_at(&bytes, 0, false) == PE32_MAGIC {
                PE32_FIXED
            } else {
                PE32_PLUS_FIXED
            };
            let count = u32_at(&bytes, fixed - 4, false) as usize;
            assert!(count <= DIRECTORIES && fixed + 8 * count <= length);
            for index in count..DIRECTORIES {
                assert!(header.directories[index] == (0, 0));
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(39)]
    fn te_headers_keep_a_known_machine_subsystem_and_directories() {
        let bytes: [u8; TE_HEADER] = kani::any();
        if let Ok(header) = te_header(&bytes, &COFF_MACHINES[1..]) {
            assert!(header.sections > 0);
            assert!(bytes[5] <= TE_LAST_SUBSYSTEM && bytes[5] != TE_UNUSED_SUBSYSTEM);
            assert!(u16_at(&bytes, 2, false) != 0);
            assert!(u32_at(&bytes, 28, false) <= TE_LARGEST_DIRECTORY);
        }
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn elf_headers_keep_the_entry_sizes_of_their_class() {
        let bytes: [u8; ELF64_HEADER] = kani::any();
        if let Ok(header) = elf_header(&bytes) {
            let (fixed, program, section) = if header.wide {
                (64, 56, 64)
            } else {
                (52, 32, 40)
            };
            assert!(usize::from(header.header_size) == fixed);
            assert!(header.programs == 0 || usize::from(header.program_size) == program);
            assert!(header.sections == 0 || usize::from(header.section_size) == section);
            assert!(bytes[6] == 1);
            assert!(
                bytes[ELF_IDENT_PADDING..ELF_IDENT]
                    .iter()
                    .all(|&byte| byte == 0)
            );
        }
    }

    #[kani::proof]
    #[kani::unwind(5)]
    fn mach_headers_keep_their_commands_inside_16_mib() {
        let bytes: [u8; MACHO64_HEADER] = kani::any();
        if let Ok(header) = mach_header(&bytes) {
            assert!(header.commands > 0);
            assert!(header.commands_size <= MACHO_LARGEST_COMMANDS);
            assert!(u64::from(header.commands) * 8 <= u64::from(header.commands_size));
        }
    }

    #[kani::proof]
    fn universal_slices_are_never_empty_and_name_a_small_cpu_type() {
        let bytes: [u8; FAT_ARCH] = kani::any();
        if let Ok(arch) = fat_arch(&bytes) {
            assert!(arch.span.size > 0 && arch.span.end().is_ok());
            assert!(arch.align <= FAT_LARGEST_ALIGN);
            assert!(arch.cpu & 0x00FF_FF00 == 0);
            assert!(arch.subtype & FAT_SUBTYPE_UNUSED == 0);
        }
    }
}
