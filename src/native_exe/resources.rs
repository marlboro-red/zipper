use super::{
    BadField, Data, Field, MAX_BUILT_BYTES, MAX_ENTRIES, Reader, Section, Span, field_check,
    u16_at, u32_at,
};
use std::collections::BTreeMap;

const DIRECTORY: usize = 16;
const ENTRY: usize = 8;
const DATA_ENTRY: usize = 16;
const SUBDIRECTORY: u32 = 0x8000_0000;
const MAX_VISITED: usize = 1 << 22;
const MAX_CONVERTED: u32 = 1 << 24;
const STRING_TYPE: u32 = 6;
const VERSION_TYPE: u32 = 16;
const ICON_TYPE: u32 = 3;
const BITMAP_TYPE: u32 = 2;
const BITMAP_INFO: u32 = 40;
const ICON_HEADER: usize = 22;
const BITMAP_FILE_HEADER: usize = 14;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Key {
    Name(u32, Vec<u16>),
    Id(u32),
}

struct Leaf {
    kind: Key,
    name: Key,
    language: Key,
    address: u32,
    size: u32,
}

pub(super) fn type_name(id: u32) -> Option<&'static str> {
    Some(match id {
        1 => "CURSOR",
        2 => "BITMAP",
        3 => "ICON",
        4 => "MENU",
        5 => "DIALOG",
        6 => "STRING",
        7 => "FONTDIR",
        8 => "FONT",
        9 => "ACCELERATOR",
        10 => "RCDATA",
        11 => "MESSAGETABLE",
        12 => "GROUP_CURSOR",
        14 => "GROUP_ICON",
        16 => "VERSION",
        17 => "DLGINCLUDE",
        19 => "PLUGPLAY",
        20 => "VXD",
        21 => "ANICURSOR",
        22 => "ANIICON",
        23 => "HTML",
        24 => "MANIFEST",
        _ => return None,
    })
}

pub(super) fn key_text(key: &Key) -> Field<String> {
    match key {
        Key::Id(id) => Ok(id.to_string()),
        Key::Name(_, units) if units.is_empty() => Ok("[]".to_owned()),
        Key::Name(_, units) => {
            String::from_utf16(units).map_err(|_| BadField("PE resource name is not valid UTF-16"))
        }
    }
}

struct Tree<'a, 'b> {
    reader: &'a Reader<'b>,
    base: u64,
    size: u64,
    visited: usize,
    leaves: Vec<Leaf>,
    tables: BTreeMap<u64, u64>,
}

impl Tree<'_, '_> {
    // EXTERNAL(7-Zip PeHandler): directory tables that share a byte leave
    // the section raw.
    fn claim(&mut self, start: u64, end: u64) -> anyhow::Result<()> {
        let before = self.tables.range(..=start).next_back();
        let after = self.tables.range(start..).next();
        anyhow::ensure!(
            before.is_none_or(|(_, &before_end)| before_end <= start)
                && after.is_none_or(|(&after_start, _)| after_start >= end || start == end),
            "PE resource directory tables overlap"
        );
        self.tables.insert(start, end);
        Ok(())
    }

    fn bytes(&self, offset: u64, length: usize) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(
            offset + length as u64 <= self.size,
            "PE resource structure runs past its section"
        );
        self.reader.read(
            Span {
                offset: self.base + offset,
                size: length as u64,
            },
            "PE resource structure",
        )
    }

    // EXTERNAL(7-Zip PeHandler): a name it cannot read stands as the number in its field.
    fn name(&mut self, field: u32) -> Key {
        if field & SUBDIRECTORY == 0 {
            return Key::Id(field);
        }
        let offset = u64::from(field & !SUBDIRECTORY);
        let Ok(length) = self.bytes(offset, 2) else {
            return Key::Id(field);
        };
        let count = usize::from(u16_at(&length, 0, false));
        let Ok(units) = self.bytes(offset + 2, 2 * count) else {
            return Key::Id(field);
        };
        Key::Name(
            field & !SUBDIRECTORY,
            units
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
        )
    }

    fn directory(&mut self, offset: u64, depth: usize, path: &mut Vec<Key>) -> anyhow::Result<()> {
        anyhow::ensure!(
            offset.is_multiple_of(4),
            "PE resource directory is not 4-byte aligned"
        );
        let header = self.bytes(offset, DIRECTORY)?;
        let named = usize::from(u16_at(&header, 12, false));
        let count = named + usize::from(u16_at(&header, 14, false));
        let entries = self.bytes(offset + DIRECTORY as u64, count * ENTRY)?;
        self.claim(offset, offset + (DIRECTORY + count * ENTRY) as u64)?;
        for (index, entry) in entries.chunks_exact(ENTRY).enumerate() {
            self.visited += 1;
            anyhow::ensure!(
                self.visited <= MAX_VISITED,
                "PE resource tree has more than 2^22 entries"
            );
            let field = u32_at(entry, 0, false);
            anyhow::ensure!(
                (field & SUBDIRECTORY != 0) == (index < named),
                "PE resource entry is named where its directory counts a number, or the reverse"
            );
            let key = self.name(field);
            let target = u32_at(entry, 4, false);
            let child = u64::from(target & !SUBDIRECTORY);
            path.push(key);
            if depth < 2 {
                anyhow::ensure!(
                    target & SUBDIRECTORY != 0,
                    "PE resource directory entry is not a directory"
                );
                self.directory(child, depth + 1, path)?;
            } else {
                anyhow::ensure!(
                    target & SUBDIRECTORY == 0,
                    "PE resource leaf is a directory"
                );
                let data = self.bytes(child, DATA_ENTRY)?;
                anyhow::ensure!(
                    u32_at(&data, 12, false) == 0,
                    "PE resource data entry has a reserved field that is not 0"
                );
                anyhow::ensure!(
                    self.leaves.len() < MAX_ENTRIES,
                    "PE has more than {MAX_ENTRIES} resources"
                );
                self.leaves.push(Leaf {
                    kind: path[0].clone(),
                    name: path[1].clone(),
                    language: path[2].clone(),
                    address: u32_at(&data, 0, false),
                    size: u32_at(&data, 4, false),
                });
            }
            path.pop();
        }
        Ok(())
    }
}

pub(super) fn string_block(id: u32, data: &[u8], out: &mut Vec<u16>) -> Field<()> {
    field_check(
        data.len().is_multiple_of(2),
        "PE string table has an odd size",
    )?;
    let units: Vec<u16> = data
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let mut at = 0;
    let mut lines = Vec::new();
    for index in 0..16u32 {
        let length = usize::from(
            *units
                .get(at)
                .ok_or(BadField("PE string table is shorter than 16 strings"))?,
        );
        at += 1;
        let text = units
            .get(at..at + length)
            .ok_or(BadField("PE string runs past its table"))?;
        at += length;
        if length == 0 {
            continue;
        }
        let number = id.wrapping_sub(1).wrapping_mul(16).wrapping_add(index);
        lines.extend(number.to_string().encode_utf16());
        lines.push(u16::from(b'\t'));
        for &unit in text {
            if unit == u16::from(b'\n') {
                lines.extend("\\n".encode_utf16());
            } else {
                lines.push(unit);
            }
        }
        lines.extend("\r\n".encode_utf16());
    }
    // EXTERNAL(7-Zip PeHandler): a table may end with one zero unit after its 16 strings.
    field_check(
        matches!(units[at..], [] | [0]),
        "PE string table holds more after its 16 strings",
    )?;
    out.extend(lines);
    Ok(())
}

pub(super) fn icon_header(head: &[u8]) -> Option<Vec<u8>> {
    if head.len() < BITMAP_INFO as usize || u32_at(head, 0, false) != BITMAP_INFO {
        return None;
    }
    let width = u32_at(head, 4, false) as i32;
    let height = (u32_at(head, 8, false) as i32).unsigned_abs();
    let bits = u16_at(head, 14, false);
    if u16_at(head, 12, false) != 1
        || !matches!(bits, 1 | 4 | 8 | 24 | 32)
        || u32_at(head, 16, false) != 0
    {
        return None;
    }
    if !(1..=256).contains(&width)
        || !height.is_multiple_of(2)
        || !(1..=256).contains(&(height / 2))
    {
        return None;
    }
    let colors: u8 = match bits {
        1 => 2,
        4 => 16,
        _ => 0,
    };
    let rows = height / 2;
    let palette = if bits <= 8 { 4 << bits } else { 0 };
    let color_rows = (width as u32 * u32::from(bits)).div_ceil(32) * 4 * rows;
    let mask_rows = (width as u32).div_ceil(32) * 4 * rows;
    // EXTERNAL(7-Zip PeHandler): the icon directory gives the size the header implies, not
    // the resource's size.
    let implied = BITMAP_INFO + palette + color_rows + mask_rows;
    let mut header = vec![0, 0, 1, 0, 1, 0, width as u8, rows as u8, colors];
    header.extend_from_slice(&[0; 5]);
    header.extend_from_slice(&implied.to_le_bytes());
    header.extend_from_slice(&(ICON_HEADER as u32).to_le_bytes());
    Some(header)
}

pub(super) fn bitmap_header(head: &[u8], size: u32) -> Option<Vec<u8>> {
    if head.len() < BITMAP_INFO as usize || u32_at(head, 0, false) != BITMAP_INFO {
        return None;
    }
    if u16_at(head, 12, false) != 1 {
        return None;
    }
    let width = (u32_at(head, 4, false) as i32).unsigned_abs();
    let height = (u32_at(head, 8, false) as i32).unsigned_abs();
    let bits = u32::from(u16_at(head, 14, false));
    // EXTERNAL(7-Zip PeHandler): a compressed bitmap gives its pixels' size in its header, and
    // one that gives 0 stays raw.
    let image = match (u32_at(head, 16, false), u32_at(head, 20, false)) {
        (0, _) => width
            .wrapping_mul(bits)
            .wrapping_add(31)
            .wrapping_div(32)
            .wrapping_mul(4)
            .wrapping_mul(height),
        (_, 0) => return None,
        (_, stored) => stored,
    };
    let file_size = size.wrapping_add(BITMAP_FILE_HEADER as u32);
    let mut header = b"BM".to_vec();
    header.extend_from_slice(&file_size.to_le_bytes());
    header.extend_from_slice(&[0; 4]);
    header.extend_from_slice(&file_size.wrapping_sub(image).to_le_bytes());
    Some(header)
}

enum Plan {
    Raw,
    Dropped,
    Version(Vec<u16>),
    Strings(Vec<u16>),
    BadStrings,
    Image(Vec<u8>, &'static str),
}

pub(super) struct Resource {
    pub(super) name: String,
    pub(super) data: Data,
    pub(super) timed: bool,
}

fn nonzero_after(reader: &Reader, span: Span) -> anyhow::Result<bool> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut at = span.offset;
    let end = span.offset + span.size;
    while at < end {
        let want = (end - at).min(buffer.len() as u64) as usize;
        super::read_exact_at(reader.file, &mut buffer[..want], at)?;
        if buffer[..want].iter().any(|&byte| byte != 0) {
            return Ok(true);
        }
        at += want as u64;
    }
    Ok(false)
}

fn round_up_to_power_of_two(value: u64, alignment: u32) -> u64 {
    if alignment.is_power_of_two() {
        value
            .div_ceil(u64::from(alignment))
            .saturating_mul(u64::from(alignment))
    } else {
        value
    }
}

pub(super) fn read(
    reader: &Reader,
    section: &Section,
    file_alignment: u32,
) -> anyhow::Result<Option<Vec<Resource>>> {
    let size =
        u64::from(section.raw_size).min(reader.length.saturating_sub(u64::from(section.raw_at)));
    // EXTERNAL(7-Zip PeHandler): the directory must lie inside the virtual size rounded up to
    // a file alignment that is a power of two, so a virtual size of 0 leaves the section raw.
    let mut tree = Tree {
        reader,
        base: u64::from(section.raw_at),
        size: size.min(round_up_to_power_of_two(
            u64::from(section.virtual_size),
            file_alignment,
        )),
        visited: 0,
        leaves: Vec::new(),
        tables: BTreeMap::new(),
    };
    // EXTERNAL(7-Zip PeHandler): a section whose resource directory does not parse stays one
    // raw file.
    if tree.directory(0, 0, &mut Vec::new()).is_err() {
        anyhow::ensure!(
            tree.visited <= MAX_VISITED && tree.leaves.len() < MAX_ENTRIES,
            "PE resource directory has more than 2^22 entries or {MAX_ENTRIES} resources"
        );
        return Ok(None);
    }
    let mut plans = Vec::with_capacity(tree.leaves.len());
    let mut built = 0usize;
    for leaf in &tree.leaves {
        let offset = u64::from(leaf.address)
            .checked_sub(u64::from(section.address))
            .filter(|&offset| offset + u64::from(leaf.size) <= size);
        // EXTERNAL(7-Zip PeHandler): a version or string resource outside its section is left
        // out, and any other resource there is listed and fails to extract.
        let offset = match (offset, &leaf.kind) {
            (Some(offset), _) => offset,
            (None, Key::Id(VERSION_TYPE | STRING_TYPE)) => {
                plans.push((Span { offset: 0, size: 0 }, Plan::Dropped));
                continue;
            }
            (None, _) => anyhow::bail!("PE resource data lies outside the .rsrc section"),
        };
        let span = Span {
            offset: u64::from(section.raw_at) + offset,
            size: u64::from(leaf.size),
        };
        let converted = leaf.size <= MAX_CONVERTED;
        let plan = match (&leaf.kind, &leaf.name) {
            (Key::Id(STRING_TYPE), Key::Id(id) | Key::Name(id, _)) if converted => {
                let data = reader.read(span, "PE string table")?;
                let mut lines = Vec::new();
                match string_block(*id, &data, &mut lines) {
                    Ok(()) => Plan::Strings(lines),
                    Err(_) => Plan::BadStrings,
                }
            }
            (Key::Id(STRING_TYPE), _) => Plan::BadStrings,
            (Key::Id(VERSION_TYPE), _) if converted => {
                let data = reader.read(span, "PE version resource")?;
                super::version::render(&data).map_or(Plan::Raw, Plan::Version)
            }
            (Key::Id(kind @ (ICON_TYPE | BITMAP_TYPE)), _) if converted => {
                let head = reader.read(
                    Span {
                        offset: span.offset,
                        size: span.size.min(u64::from(BITMAP_INFO)),
                    },
                    "PE image resource",
                )?;
                let header = if *kind == ICON_TYPE {
                    icon_header(&head).map(|header| (header, "ico"))
                } else {
                    bitmap_header(&head, leaf.size).map(|header| (header, "bmp"))
                };
                header.map_or(Plan::Raw, |(header, extension)| {
                    Plan::Image(header, extension)
                })
            }
            _ => Plan::Raw,
        };
        if let Plan::Strings(text) | Plan::Version(text) = &plan {
            built += 2 * text.len();
            anyhow::ensure!(built <= MAX_BUILT_BYTES, "PE text resources exceed 64 MiB");
        }
        plans.push((span, plan));
    }
    // EXTERNAL(7-Zip PeHandler): one string table that does not parse leaves every string
    // table raw.
    if plans
        .iter()
        .any(|(_, plan)| matches!(plan, Plan::BadStrings))
    {
        for (_, plan) in &mut plans {
            if matches!(plan, Plan::Strings(_) | Plan::BadStrings) {
                *plan = Plan::Raw;
            }
        }
    }
    // EXTERNAL(7-Zip PeHandler): resources go in folders named by language when a resource
    // after the first one that is not a converted version names another language.
    let several = plans
        .iter()
        .position(|(_, plan)| !matches!(plan, Plan::Version(_) | Plan::Dropped))
        .is_some_and(|first| {
            let language = &tree.leaves[first].language;
            tree.leaves[first + 1..]
                .iter()
                .any(|leaf| leaf.language != *language)
        });
    let mut versions = Vec::new();
    let mut strings: BTreeMap<String, Vec<u16>> = BTreeMap::new();
    let mut string_order = Vec::new();
    let mut others = Vec::new();
    for (leaf, (span, plan)) in tree.leaves.iter().zip(plans) {
        let prefix = if several {
            format!("{}/", key_text(&leaf.language)?)
        } else {
            String::new()
        };
        let kind_name = match &leaf.kind {
            Key::Id(id) => type_name(*id).map_or_else(|| id.to_string(), str::to_owned),
            name => key_text(name)?,
        };
        let name = key_text(&leaf.name)?;
        match plan {
            Plan::Dropped => {}
            Plan::Version(text) => versions.push((format!("{prefix}version.txt"), text)),
            Plan::Strings(lines) => {
                let file = format!("{prefix}string.txt");
                if !strings.contains_key(&file) {
                    string_order.push(file.clone());
                }
                strings.entry(file).or_default().extend(lines);
            }
            Plan::Image(header, extension) => others.push((
                format!("{prefix}{kind_name}/{name}.{extension}"),
                Data::Prefixed(header, span),
            )),
            Plan::Raw | Plan::BadStrings => {
                others.push((format!("{prefix}{kind_name}/{name}"), Data::Range(span)));
            }
        }
    }
    let mut items = Vec::new();
    let mut add =
        |name: String, data: Data, timed: bool| items.push(Resource { name, data, timed });
    for (name, text) in versions {
        add(
            format!(".rsrc/{name}"),
            Data::Built(utf16_bytes(&text)),
            false,
        );
    }
    for name in string_order {
        let text = strings.remove(&name).unwrap_or_default();
        add(
            format!(".rsrc/{name}"),
            Data::Built(utf16_bytes(&text)),
            false,
        );
    }
    for (name, data) in others {
        add(format!(".rsrc/{name}"), data, false);
    }
    // EXTERNAL(7-Zip PeHandler): the bytes after the last resource or the virtual size, rounded
    // up to the file alignment, become `.rsrc_1`, from the last resource when one is not zero.
    let used = tree
        .leaves
        .iter()
        .map(|leaf| u64::from(leaf.address.wrapping_sub(section.address)) + u64::from(leaf.size))
        .max()
        .unwrap_or(0);
    let rounded =
        round_up_to_power_of_two(used.max(u64::from(section.virtual_size)), file_alignment);
    let start = if used < size
        && nonzero_after(
            reader,
            Span {
                offset: u64::from(section.raw_at) + used,
                size: rounded.min(size) - used,
            },
        )? {
        used
    } else {
        rounded
    };
    if start < size {
        let tail = Span {
            offset: u64::from(section.raw_at) + start,
            size: size - start,
        };
        add(".rsrc_1".to_owned(), Data::Range(tail), true);
    }
    Ok(Some(items))
}

fn utf16_bytes(units: &[u16]) -> Vec<u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(width: i32, height: i32, bits: u16, compression: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in [40, width as u32, height as u32] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        bytes.extend_from_slice(&compression.to_le_bytes());
        bytes.resize(40, 0);
        bytes
    }

    #[test]
    fn icon_directories_give_the_size_their_header_implies_as_7zip_writes_it() {
        for (width, height, bits, colors, size) in [
            (16, 32, 1, 2, 0xB0),
            (16, 32, 4, 16, 0x128),
            (16, 32, 8, 0, 0x568),
            (16, 32, 24, 0, 0x368),
            (16, 32, 32, 0, 0x468),
            (16, -32, 8, 0, 0x568),
        ] {
            let header = icon_header(&info(width, height, bits, 0)).unwrap();
            assert_eq!(header.len(), ICON_HEADER);
            assert_eq!(header[6], width as u8);
            assert_eq!(header[8], colors);
            assert_eq!(u32_at(&header, 14, false), size, "{width}x{height}x{bits}");
        }
        for (width, height, bits, compression) in [
            (16, 32, 16, 0),
            (300, 600, 32, 0),
            (16, 33, 8, 0),
            (16, 32, 32, 1),
        ] {
            assert!(icon_header(&info(width, height, bits, compression)).is_none());
        }
    }

    #[test]
    fn bitmap_files_point_at_the_pixels_their_header_implies() {
        let header = bitmap_header(&info(2, 2, 24, 0), 104).unwrap();
        assert_eq!(&header[..2], b"BM");
        assert_eq!(u32_at(&header, 2, false), 118);
        assert_eq!(u32_at(&header, 10, false), 0x66);
        let header = bitmap_header(&info(16, -32, 16, 0), 395).unwrap();
        assert_eq!(u32_at(&header, 10, false), 0xFFFF_FD99);
        assert!(bitmap_header(&info(2, 2, 16, 3), 104).is_none());
        let mut stored = info(31, 16, 4, 2);
        stored[20..24].copy_from_slice(&0x130u32.to_le_bytes());
        assert_eq!(u32_at(&bitmap_header(&stored, 384).unwrap(), 10, false), 94);
    }

    #[test]
    fn string_tables_number_and_escape_their_strings_as_7zip_writes_them() {
        let mut data = Vec::new();
        for text in ["tab\there", "nl\nhere", "", "x"]
            .iter()
            .chain([""; 12].iter())
        {
            data.extend_from_slice(&(text.len() as u16).to_le_bytes());
            for unit in text.encode_utf16() {
                data.extend_from_slice(&unit.to_le_bytes());
            }
        }
        let mut lines = Vec::new();
        string_block(1, &data, &mut lines).unwrap();
        assert_eq!(
            String::from_utf16(&lines).unwrap(),
            "0\ttab\there\r\n1\tnl\\nhere\r\n3\tx\r\n"
        );
        let mut lines = Vec::new();
        string_block(0, &data, &mut lines).unwrap();
        assert!(
            String::from_utf16(&lines)
                .unwrap()
                .starts_with("4294967280\t")
        );
        assert!(string_block(1, &data[..data.len() - 2], &mut Vec::new()).is_err());
        assert!(string_block(1, &[data.as_slice(), &[0]].concat(), &mut Vec::new()).is_err());
        assert!(string_block(1, &[data.as_slice(), &[0, 0]].concat(), &mut Vec::new()).is_ok());
        assert!(string_block(1, &[data.as_slice(), &[1, 0]].concat(), &mut Vec::new()).is_err());
        assert!(string_block(1, &[data.as_slice(), &[0; 4]].concat(), &mut Vec::new()).is_err());
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn image_headers_never_panic_and_icons_stay_within_256_pixels() {
        let head: [u8; BITMAP_INFO as usize] = kani::any();
        if let Some(header) = icon_header(&head) {
            assert!(header.len() == ICON_HEADER);
            let width = u32_at(&head, 4, false) as i32;
            assert!((1..=256).contains(&width));
            assert!(u32_at(&head, 16, false) == 0);
        }
        let size: u32 = kani::any();
        if let Some(header) = bitmap_header(&head, size) {
            assert!(header.len() == BITMAP_FILE_HEADER);
            assert!(u32_at(&header, 2, false) == size.wrapping_add(BITMAP_FILE_HEADER as u32));
        }
    }
}
