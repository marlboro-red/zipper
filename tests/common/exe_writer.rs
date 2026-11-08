#![allow(dead_code)]

use std::collections::BTreeMap;

const FILE_ALIGNMENT: usize = 0x200;
const SECTION_ALIGNMENT: u32 = 0x1000;

fn align(value: usize, to: usize) -> usize {
    value.div_ceil(to) * to
}

fn put16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn set32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Name(String),
    Id(u32),
}

pub type Resources = BTreeMap<Key, BTreeMap<Key, BTreeMap<Key, Vec<u8>>>>;

pub fn resource_section(resources: &Resources, address: u32) -> Vec<u8> {
    // SPEC(PE Format, .rsrc Section): named entries come before ID entries.
    let mut names: Vec<String> = Vec::new();
    let mut collect = |key: &Key| {
        if let Key::Name(name) = key
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    };
    let mut directories = 16 + 8 * resources.len();
    let mut leaves = 0;
    for (kind, by_name) in resources {
        collect(kind);
        directories += 16 + 8 * by_name.len();
        for (name, by_language) in by_name {
            collect(name);
            directories += 16 + 8 * by_language.len();
            for language in by_language.keys() {
                collect(language);
                leaves += 1;
            }
        }
    }
    let mut strings = Vec::new();
    let mut name_at = BTreeMap::new();
    for name in &names {
        name_at.insert(name.clone(), directories + strings.len());
        let units: Vec<u16> = name.encode_utf16().collect();
        put16(&mut strings, units.len() as u16);
        for unit in units {
            put16(&mut strings, unit);
        }
    }
    let entries_at = align(directories + strings.len(), 4);
    let data_at = entries_at + 16 * leaves;
    let mut body = vec![0u8; data_at];
    body[directories..directories + strings.len()].copy_from_slice(&strings);
    let mut data = Vec::new();
    let mut next_directory = 0usize;
    let mut next_entry = entries_at;
    fn field(key: &Key, name_at: &BTreeMap<String, usize>) -> u32 {
        match key {
            Key::Id(id) => *id,
            Key::Name(name) => 0x8000_0000 | name_at[name] as u32,
        }
    }
    fn header(body: &mut [u8], at: usize, keys: &[&Key]) {
        let named = keys
            .iter()
            .filter(|key| matches!(key, Key::Name(_)))
            .count();
        body[at + 12..at + 14].copy_from_slice(&(named as u16).to_le_bytes());
        body[at + 14..at + 16].copy_from_slice(&((keys.len() - named) as u16).to_le_bytes());
    }
    let root = next_directory;
    next_directory += 16 + 8 * resources.len();
    header(&mut body, root, &resources.keys().collect::<Vec<_>>());
    for (index, (kind, by_name)) in resources.iter().enumerate() {
        let name_directory = next_directory;
        next_directory += 16 + 8 * by_name.len();
        set32(&mut body, root + 16 + 8 * index, field(kind, &name_at));
        set32(
            &mut body,
            root + 20 + 8 * index,
            0x8000_0000 | name_directory as u32,
        );
        header(
            &mut body,
            name_directory,
            &by_name.keys().collect::<Vec<_>>(),
        );
        for (name_index, (name, by_language)) in by_name.iter().enumerate() {
            let language_directory = next_directory;
            next_directory += 16 + 8 * by_language.len();
            let at = name_directory + 16 + 8 * name_index;
            set32(&mut body, at, field(name, &name_at));
            set32(&mut body, at + 4, 0x8000_0000 | language_directory as u32);
            header(
                &mut body,
                language_directory,
                &by_language.keys().collect::<Vec<_>>(),
            );
            for (language_index, (language, payload)) in by_language.iter().enumerate() {
                let at = language_directory + 16 + 8 * language_index;
                set32(&mut body, at, field(language, &name_at));
                set32(&mut body, at + 4, next_entry as u32);
                set32(
                    &mut body,
                    next_entry,
                    address + (data_at + data.len()) as u32,
                );
                set32(&mut body, next_entry + 4, payload.len() as u32);
                next_entry += 16;
                data.extend_from_slice(payload);
                data.resize(align(data.len(), 8), 0);
            }
        }
    }
    body.extend_from_slice(&data);
    body
}

pub fn string_block(strings: &[&str; 16]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for text in strings {
        let units: Vec<u16> = text.encode_utf16().collect();
        put16(&mut bytes, units.len() as u16);
        for unit in units {
            put16(&mut bytes, unit);
        }
    }
    bytes
}

fn version_node(
    key: &str,
    value: &[u8],
    value_length: u16,
    text: bool,
    children: &[Vec<u8>],
) -> Vec<u8> {
    let mut bytes = vec![0u8; 6];
    for unit in key.encode_utf16().chain([0]) {
        put16(&mut bytes, unit);
    }
    bytes.resize(align(bytes.len(), 4), 0);
    bytes.extend_from_slice(value);
    for child in children {
        bytes.resize(align(bytes.len(), 4), 0);
        bytes.extend_from_slice(child);
    }
    let length = bytes.len() as u16;
    bytes[0..2].copy_from_slice(&length.to_le_bytes());
    bytes[2..4].copy_from_slice(&value_length.to_le_bytes());
    bytes[4..6].copy_from_slice(&u16::from(text).to_le_bytes());
    bytes
}

#[derive(Clone, Copy, Debug)]
pub struct FixedInfo {
    pub file_version: [u16; 4],
    pub product_version: [u16; 4],
    pub flags_mask: u32,
    pub flags: u32,
    pub os: u32,
    pub kind: u32,
    pub subtype: u32,
}

pub fn version_info(
    fixed: FixedInfo,
    tables: &[(&str, Vec<(&str, &str)>)],
    translations: &[(u16, u16)],
) -> Vec<u8> {
    let mut value = Vec::new();
    let words = |version: [u16; 4]| {
        [
            u32::from(version[0]) << 16 | u32::from(version[1]),
            u32::from(version[2]) << 16 | u32::from(version[3]),
        ]
    };
    for word in [0xFEEF_04BD, 0x1_0000]
        .into_iter()
        .chain(words(fixed.file_version))
        .chain(words(fixed.product_version))
        .chain([
            fixed.flags_mask,
            fixed.flags,
            fixed.os,
            fixed.kind,
            fixed.subtype,
            0,
            0,
        ])
    {
        put32(&mut value, word);
    }
    let mut children = Vec::new();
    if !tables.is_empty() {
        let tables: Vec<Vec<u8>> = tables
            .iter()
            .map(|(language, pairs)| {
                let strings: Vec<Vec<u8>> = pairs
                    .iter()
                    .map(|(key, text)| {
                        let mut value = Vec::new();
                        for unit in text.encode_utf16().chain([0]) {
                            put16(&mut value, unit);
                        }
                        version_node(key, &value, (value.len() / 2) as u16, true, &[])
                    })
                    .collect();
                version_node(language, &[], 0, true, &strings)
            })
            .collect();
        children.push(version_node("StringFileInfo", &[], 0, true, &tables));
    }
    let mut pairs = Vec::new();
    for (language, page) in translations {
        put16(&mut pairs, *language);
        put16(&mut pairs, *page);
    }
    let translation = version_node("Translation", &pairs, pairs.len() as u16, false, &[]);
    children.push(version_node("VarFileInfo", &[], 0, true, &[translation]));
    version_node(
        "VS_VERSION_INFO",
        &value,
        value.len() as u16,
        false,
        &children,
    )
}

#[derive(Clone, Debug)]
pub struct PeSection {
    pub name: [u8; 8],
    pub data: Vec<u8>,
    pub virtual_size: Option<u32>,
}

pub fn section_name(name: &str) -> [u8; 8] {
    let mut raw = [0u8; 8];
    raw[..name.len()].copy_from_slice(name.as_bytes());
    raw
}

#[derive(Clone, Debug, Default)]
pub struct PeOptions {
    pub sections: Vec<PeSection>,
    pub resources: Option<Resources>,
    pub wide: bool,
    pub machine: u16,
    pub timestamp: u32,
    pub certificate: Option<Vec<u8>>,
}

pub fn pe(options: &PeOptions) -> Vec<u8> {
    let pointer = 0x80usize;
    let optional_size = if options.wide { 240 } else { 224 };
    let mut sections = options.sections.clone();
    let resource_address = SECTION_ALIGNMENT * (sections.len() as u32 + 1);
    if let Some(resources) = &options.resources {
        sections.push(PeSection {
            name: *b".rsrc\0\0\0",
            data: resource_section(resources, resource_address),
            virtual_size: None,
        });
    }
    let headers = align(
        pointer + 24 + optional_size + 40 * sections.len(),
        FILE_ALIGNMENT,
    );
    let mut table = Vec::new();
    let mut body = Vec::new();
    let mut address = SECTION_ALIGNMENT;
    let mut directories = [(0u32, 0u32); 16];
    for section in &sections {
        let raw = align(section.data.len(), FILE_ALIGNMENT);
        let virtual_size = section.virtual_size.unwrap_or(section.data.len() as u32);
        if &section.name == b".rsrc\0\0\0" {
            directories[2] = (address, section.data.len() as u32);
        }
        table.extend_from_slice(&section.name);
        for value in [
            virtual_size,
            address,
            raw as u32,
            if raw > 0 {
                (headers + body.len()) as u32
            } else {
                0
            },
            0,
            0,
        ] {
            put32(&mut table, value);
        }
        put32(&mut table, 0);
        put32(&mut table, 0x6000_0020);
        body.extend_from_slice(&section.data);
        body.resize(align(body.len(), FILE_ALIGNMENT), 0);
        address += align(virtual_size.max(1) as usize, SECTION_ALIGNMENT as usize) as u32;
    }
    let image_size = address;
    let certificate_at = headers + body.len();
    if let Some(certificate) = &options.certificate {
        directories[4] = (certificate_at as u32, certificate.len() as u32);
    }
    let mut bytes = vec![0u8; pointer];
    bytes[..2].copy_from_slice(b"MZ");
    set32(&mut bytes, 0x3C, pointer as u32);
    bytes.extend_from_slice(b"PE\0\0");
    put16(
        &mut bytes,
        if options.machine == 0 {
            0x8664
        } else {
            options.machine
        },
    );
    put16(&mut bytes, sections.len() as u16);
    put32(&mut bytes, options.timestamp);
    put32(&mut bytes, 0);
    put32(&mut bytes, 0);
    put16(&mut bytes, optional_size as u16);
    put16(&mut bytes, 0x22);
    put16(&mut bytes, if options.wide { 0x20B } else { 0x10B });
    bytes.extend_from_slice(&[14, 0]);
    for _ in 0..3 {
        put32(&mut bytes, 0);
    }
    put32(&mut bytes, SECTION_ALIGNMENT);
    put32(&mut bytes, SECTION_ALIGNMENT);
    if options.wide {
        put64(&mut bytes, 0x1_4000_0000);
    } else {
        put32(&mut bytes, 0);
        put32(&mut bytes, 0x40_0000);
    }
    put32(&mut bytes, SECTION_ALIGNMENT);
    put32(&mut bytes, FILE_ALIGNMENT as u32);
    for value in [6u16, 0, 0, 0, 6, 0] {
        put16(&mut bytes, value);
    }
    put32(&mut bytes, 0);
    put32(&mut bytes, image_size);
    put32(&mut bytes, headers as u32);
    put32(&mut bytes, 0);
    put16(&mut bytes, 3);
    put16(&mut bytes, 0x8160);
    for value in [1u64 << 20, 4096, 1 << 20, 4096] {
        if options.wide {
            put64(&mut bytes, value);
        } else {
            put32(&mut bytes, value as u32);
        }
    }
    put32(&mut bytes, 0);
    put32(&mut bytes, 16);
    for (address, size) in directories {
        put32(&mut bytes, address);
        put32(&mut bytes, size);
    }
    bytes.extend_from_slice(&table);
    bytes.resize(headers, 0);
    bytes.extend_from_slice(&body);
    if let Some(certificate) = &options.certificate {
        bytes.extend_from_slice(certificate);
    }
    bytes
}

pub fn coff(sections: &[(&str, Vec<u8>)], symbols: u32, timestamp: u32) -> Vec<u8> {
    let mut data_at = 20 + 40 * sections.len();
    let mut table = Vec::new();
    let mut body = Vec::new();
    for (name, data) in sections {
        table.extend_from_slice(&section_name(name));
        for value in [
            0,
            0,
            data.len() as u32,
            if data.is_empty() { 0 } else { data_at as u32 },
            0,
            0,
        ] {
            put32(&mut table, value);
        }
        put32(&mut table, 0);
        put32(&mut table, 0x6000_0020);
        body.extend_from_slice(data);
        data_at += data.len();
    }
    let mut bytes = Vec::new();
    put16(&mut bytes, 0x8664);
    put16(&mut bytes, sections.len() as u16);
    put32(&mut bytes, timestamp);
    put32(&mut bytes, data_at as u32);
    put32(&mut bytes, symbols);
    put16(&mut bytes, 0);
    put16(&mut bytes, 0);
    bytes.extend_from_slice(&table);
    bytes.extend_from_slice(&body);
    if symbols > 0 {
        bytes.extend((0..18 * symbols).map(|index| index as u8));
        put32(&mut bytes, 4);
    }
    bytes
}

pub fn te(sections: &[(&str, Vec<u8>)], stripped: u16) -> Vec<u8> {
    let table_end = 40 + 40 * sections.len();
    let mut bytes = b"VZ".to_vec();
    put16(&mut bytes, 0xAA64);
    bytes.push(sections.len() as u8);
    bytes.push(10);
    put16(&mut bytes, stripped);
    for value in [0x1000u32, 0x1000] {
        put32(&mut bytes, value);
    }
    put64(&mut bytes, 0);
    bytes.extend_from_slice(&[0; 16]);
    let mut at = table_end;
    let mut body = Vec::new();
    for (index, (name, data)) in sections.iter().enumerate() {
        bytes.extend_from_slice(&section_name(name));
        let raw_at = at + usize::from(stripped) - 40;
        for value in [
            data.len() as u32,
            0x1000 * (index as u32 + 1),
            data.len() as u32,
            raw_at as u32,
            0,
            0,
        ] {
            put32(&mut bytes, value);
        }
        put32(&mut bytes, 0);
        put32(&mut bytes, 0x6000_0020);
        body.extend_from_slice(data);
        at += data.len();
    }
    bytes.extend_from_slice(&body);
    bytes
}

#[derive(Clone, Debug)]
pub struct ElfSection {
    pub name: Vec<u8>,
    pub kind: u32,
    pub data: Vec<u8>,
}

pub fn elf(
    sections: Option<&[ElfSection]>,
    segments: &[(u32, Vec<u8>)],
    wide: bool,
    big: bool,
) -> Vec<u8> {
    let word = |bytes: &mut Vec<u8>, value: u64, size: usize| {
        let all = if big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        if big {
            bytes.extend_from_slice(&all[8 - size..]);
        } else {
            bytes.extend_from_slice(&all[..size]);
        }
    };
    let address = if wide { 8 } else { 4 };
    let header = if wide { 64 } else { 52 };
    let program = if wide { 56 } else { 32 };
    let section = if wide { 64 } else { 40 };
    let mut body = Vec::new();
    let data_start = header + program * segments.len();
    let mut segment_spans = Vec::new();
    for (_, data) in segments {
        segment_spans.push((data_start + body.len(), data.len()));
        body.extend_from_slice(data);
    }
    let mut table = Vec::new();
    let mut count = 0;
    let mut names_index = 0;
    if let Some(sections) = sections {
        let mut names = vec![0u8];
        let mut entries = vec![(0u32, 0u32, 0usize, 0usize)];
        for section in sections.iter().chain([&ElfSection {
            name: b".shstrtab".to_vec(),
            kind: 3,
            data: Vec::new(),
        }]) {
            let name_at = names.len() as u32;
            names.extend_from_slice(&section.name);
            names.push(0);
            entries.push((name_at, section.kind, 0, section.data.len()));
        }
        let last = entries.len() - 1;
        entries[last].3 = names.len();
        for (index, entry) in entries.iter_mut().enumerate().skip(1) {
            body.resize(align(body.len(), 8), 0);
            entry.2 = data_start + body.len();
            let data = if index == last {
                &names
            } else {
                &sections[index - 1].data
            };
            if entry.1 != 8 {
                body.extend_from_slice(data);
            }
        }
        body.resize(align(body.len(), 8), 0);
        for (name, kind, offset, size) in &entries {
            word(&mut table, u64::from(*name), 4);
            word(&mut table, u64::from(*kind), 4);
            word(&mut table, 0, address);
            word(&mut table, 0, address);
            word(&mut table, *offset as u64, address);
            word(&mut table, *size as u64, address);
            word(&mut table, 0, 4);
            word(&mut table, 0, 4);
            word(&mut table, 1, address);
            word(&mut table, 0, address);
        }
        count = entries.len();
        names_index = last;
    }
    let mut bytes = b"\x7FELF".to_vec();
    bytes.extend_from_slice(&[if wide { 2 } else { 1 }, if big { 2 } else { 1 }, 1, 0]);
    bytes.extend_from_slice(&[0; 8]);
    word(&mut bytes, 2, 2);
    word(&mut bytes, 0x3E, 2);
    word(&mut bytes, 1, 4);
    word(&mut bytes, 0x40_0000, address);
    word(
        &mut bytes,
        if segments.is_empty() {
            0
        } else {
            header as u64
        },
        address,
    );
    word(
        &mut bytes,
        if count > 0 {
            (data_start + body.len()) as u64
        } else {
            0
        },
        address,
    );
    word(&mut bytes, 0, 4);
    word(&mut bytes, header as u64, 2);
    word(&mut bytes, program as u64, 2);
    word(&mut bytes, segments.len() as u64, 2);
    word(&mut bytes, section as u64, 2);
    word(&mut bytes, count as u64, 2);
    word(&mut bytes, names_index as u64, 2);
    for ((kind, _), (offset, size)) in segments.iter().zip(&segment_spans) {
        word(&mut bytes, u64::from(*kind), 4);
        if wide {
            word(&mut bytes, 5, 4);
        }
        word(&mut bytes, *offset as u64, address);
        word(&mut bytes, 0x40_0000 + *offset as u64, address);
        word(&mut bytes, 0x40_0000 + *offset as u64, address);
        word(&mut bytes, *size as u64, address);
        word(&mut bytes, *size as u64, address);
        if !wide {
            word(&mut bytes, 5, 4);
        }
        word(&mut bytes, 0x1000, address);
    }
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&table);
    bytes
}

#[derive(Clone, Debug)]
pub struct MachSection {
    pub name: String,
    pub flags: u32,
    pub data: Vec<u8>,
}

fn name16(name: &str) -> [u8; 16] {
    let mut raw = [0u8; 16];
    raw[..name.len()].copy_from_slice(name.as_bytes());
    raw
}

pub fn macho(
    segments: &[(&str, Vec<MachSection>)],
    symbols: Option<(u32, Vec<u8>)>,
    wide: bool,
    big: bool,
) -> Vec<u8> {
    let word = |bytes: &mut Vec<u8>, value: u64, size: usize| {
        let all = if big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        if big {
            bytes.extend_from_slice(&all[8 - size..]);
        } else {
            bytes.extend_from_slice(&all[..size]);
        }
    };
    let address = if wide { 8 } else { 4 };
    let mut commands = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    let base = 4096usize;
    for (segment, sections) in segments {
        let start = base + data.len();
        let mut entries = Vec::new();
        for section in sections {
            let zero_fill = section.flags & 0xFF == 1;
            let offset = if zero_fill { 0 } else { base + data.len() };
            entries.extend_from_slice(&name16(&section.name));
            entries.extend_from_slice(&name16(segment));
            word(&mut entries, 0x1_0000_0000 + offset as u64, address);
            word(&mut entries, section.data.len() as u64, address);
            word(&mut entries, offset as u64, 4);
            for _ in 0..3 {
                word(&mut entries, 0, 4);
            }
            word(&mut entries, u64::from(section.flags), 4);
            for _ in 0..if wide { 3 } else { 2 } {
                word(&mut entries, 0, 4);
            }
            if !zero_fill {
                data.extend_from_slice(&section.data);
            }
        }
        let size = base + data.len() - start;
        let mut command = Vec::new();
        word(&mut command, if wide { 0x19 } else { 0x1 }, 4);
        word(
            &mut command,
            ((if wide { 72 } else { 56 }) + entries.len()) as u64,
            4,
        );
        command.extend_from_slice(&name16(segment));
        for value in [
            0x1_0000_0000 + start as u64,
            size as u64,
            start as u64,
            size as u64,
        ] {
            word(&mut command, value, address);
        }
        for value in [7u64, 5, sections.len() as u64, 0] {
            word(&mut command, value, 4);
        }
        command.extend_from_slice(&entries);
        commands.push(command);
    }
    if let Some((count, strings)) = &symbols {
        let symbols_at = base + data.len();
        data.extend((0..*count as usize * if wide { 16 } else { 12 }).map(|index| index as u8));
        let strings_at = base + data.len();
        data.extend_from_slice(strings);
        let mut command = Vec::new();
        for value in [
            2u64,
            24,
            symbols_at as u64,
            u64::from(*count),
            strings_at as u64,
            strings.len() as u64,
        ] {
            word(&mut command, value, 4);
        }
        commands.push(command);
    }
    let all: Vec<u8> = commands.concat();
    let mut bytes = Vec::new();
    let magic: u32 = if wide { 0xFEED_FACF } else { 0xFEED_FACE };
    word(&mut bytes, u64::from(magic), 4);
    word(&mut bytes, if wide { 0x0100_000C } else { 7 }, 4);
    word(&mut bytes, 0, 4);
    word(&mut bytes, 2, 4);
    word(&mut bytes, commands.len() as u64, 4);
    word(&mut bytes, all.len() as u64, 4);
    word(&mut bytes, 0, 4);
    if wide {
        word(&mut bytes, 0, 4);
    }
    bytes.extend_from_slice(&all);
    assert!(bytes.len() <= base, "load commands reach the data");
    bytes.resize(base, 0);
    bytes.extend_from_slice(&data);
    bytes
}

pub fn universal(slices: &[(u32, u32, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
    bytes.extend_from_slice(&(slices.len() as u32).to_be_bytes());
    let mut at = 4096;
    let mut placed = Vec::new();
    for (cpu, subtype, data) in slices {
        for value in [*cpu, *subtype, at as u32, data.len() as u32, 12] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        placed.push(at);
        at = align(at + data.len(), 4096);
    }
    for ((_, _, data), at) in slices.iter().zip(placed) {
        bytes.resize(at, 0);
        bytes.extend_from_slice(data);
    }
    bytes
}
