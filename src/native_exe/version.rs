use super::u16_at;

const BLOCK_HEADER: usize = 6;
const FIXED_INFO: usize = 52;
const FIXED_SIGNATURE: u32 = 0xFEEF_04BD;
const KEY_COLUMN: usize = 20;
const FLAG_NAMES: [&str; 6] = [
    "VS_FF_DEBUG",
    "VS_FF_PRERELEASE",
    "VS_FF_PATCHED",
    "VS_FF_PRIVATEBUILD",
    "VS_FF_INFOINFERRED",
    "VS_FF_SPECIALBUILD",
];
const OS_HIGH: [&str; 6] = [
    "VOS_UNKNOWN",
    "VOS_DOS",
    "VOS_OS216",
    "VOS_OS232",
    "VOS_NT",
    "VOS_WINCE",
];
const OS_LOW: [&str; 5] = ["", "_WINDOWS16", "_PM16", "_PM32", "_WINDOWS32"];
const OS_COMBINED: [(u32, u32); 5] = [(1, 1), (1, 4), (2, 2), (3, 3), (4, 4)];
const DRIVERS: [&str; 12] = [
    "PRINTER",
    "KEYBOARD",
    "LANGUAGE",
    "DISPLAY",
    "MOUSE",
    "NETWORK",
    "SYSTEM",
    "INSTALLABLE",
    "SOUND",
    "COMM",
    "INPUTMETHOD",
    "VERSIONED_PRINTER",
];
const FONTS: [&str; 3] = ["RASTER", "VECTOR", "TRUETYPE"];

struct Block<'a> {
    key: Vec<u16>,
    value: &'a [u8],
    value_units: usize,
    text: bool,
    children: &'a [u8],
}

fn align(at: usize) -> usize {
    at.div_ceil(4) * 4
}

fn block(bytes: &[u8]) -> Option<(Block<'_>, usize)> {
    let length = usize::from(u16_at(bytes.get(..BLOCK_HEADER)?, 0, false));
    if length < BLOCK_HEADER || length > bytes.len() {
        return None;
    }
    let bytes = &bytes[..length];
    let value_units = usize::from(u16_at(bytes, 2, false));
    let text = match u16_at(bytes, 4, false) {
        0 => false,
        1 => true,
        _ => return None,
    };
    let mut key = Vec::new();
    let mut at = BLOCK_HEADER;
    loop {
        let unit = u16_at(bytes.get(at..at + 2)?, 0, false);
        at += 2;
        if unit == 0 {
            break;
        }
        key.push(unit);
    }
    let value_at = align(at).min(length);
    let value_bytes = if text { value_units * 2 } else { value_units };
    let value = bytes.get(value_at..value_at + value_bytes)?;
    let children = &bytes[align(value_at + value_bytes).min(length)..];
    Some((
        Block {
            key,
            value,
            value_units,
            text,
            children,
        },
        align(length),
    ))
}

fn blocks(mut bytes: &[u8]) -> Option<Vec<Block<'_>>> {
    let mut found = Vec::new();
    while !bytes.is_empty() {
        let (block, next) = block(bytes)?;
        found.push(block);
        bytes = bytes.get(next..).unwrap_or(&[]);
    }
    Some(found)
}

fn fixed_word(fixed: &[u8], index: usize) -> u32 {
    super::u32_at(fixed, 4 * index, false)
}

fn hex(value: u32) -> String {
    format!("0x{value:X}")
}

fn flags(value: u32) -> String {
    let mut parts: Vec<String> = FLAG_NAMES
        .iter()
        .enumerate()
        .filter(|(bit, _)| value & (1 << bit) != 0)
        .map(|(_, name)| (*name).to_owned())
        .collect();
    let rest = value & !0x3F;
    if rest != 0 || parts.is_empty() {
        parts.push(hex(rest));
    }
    parts.join(" | ")
}

fn os(value: u32) -> String {
    let (high, low) = (value >> 16, value & 0xFFFF);
    let high_name = OS_HIGH
        .get(high as usize)
        .map_or_else(|| hex(value & 0xFFFF_0000), |name| (*name).to_owned());
    if low == 0 {
        return high_name;
    }
    if OS_COMBINED.contains(&(high, low)) {
        return format!("{high_name}{}", OS_LOW[low as usize]);
    }
    let low_name = OS_LOW
        .get(low as usize)
        .map_or_else(|| hex(low), |name| format!("VOS_{name}"));
    format!("{high_name} | {low_name}")
}

fn file_type(value: u32) -> String {
    match value {
        0 => "VFT_UNKNOWN".to_owned(),
        1 => "VFT_APP".to_owned(),
        2 => "VFT_DLL".to_owned(),
        3 => "VFT_DRV".to_owned(),
        4 => "VFT_FONT".to_owned(),
        5 => "VFT_VXD".to_owned(),
        7 => "VFT_STATIC_LIB".to_owned(),
        other => hex(other),
    }
}

fn subtype(kind: u32, value: u32) -> String {
    let named = match kind {
        3 => DRIVERS
            .get(value.wrapping_sub(1) as usize)
            .map(|name| format!("VFT2_DRV_{name}")),
        4 => FONTS
            .get(value.wrapping_sub(1) as usize)
            .map(|name| format!("VFT2_FONT_{name}")),
        _ => None,
    };
    named.unwrap_or_else(|| hex(value))
}

struct Text(Vec<u16>);

impl Text {
    fn line(&mut self, depth: usize, text: &str) {
        self.0
            .extend(std::iter::repeat_n(u16::from(b' '), 2 * depth));
        self.0.extend(text.encode_utf16());
        self.0.extend("\r\n".encode_utf16());
    }

    fn quoted(&mut self, units: &[u16]) {
        self.0.push(u16::from(b'"'));
        self.0.extend_from_slice(units);
        self.0.push(u16::from(b'"'));
    }
}

fn text_value(block: &Block) -> Vec<u16> {
    let units: Vec<u16> = block
        .value
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let end = units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(units.len());
    units[..end].to_vec()
}

fn string_table(out: &mut Text, table: &Block) -> Option<()> {
    out.line(
        2,
        &format!("BLOCK \"{}\"", String::from_utf16(&table.key).ok()?),
    );
    out.line(2, "{");
    for string in blocks(table.children)? {
        // EXTERNAL(7-Zip PeHandler): a binary value in a string table becomes an empty line.
        if !string.text {
            out.0.extend("\r\n".encode_utf16());
            continue;
        }
        out.0.extend(std::iter::repeat_n(u16::from(b' '), 6));
        out.0.extend("VALUE ".encode_utf16());
        let start = out.0.len();
        out.quoted(&string.key);
        out.0.push(u16::from(b','));
        while out.0.len() - start < KEY_COLUMN {
            out.0.push(u16::from(b' '));
        }
        out.0.push(u16::from(b' '));
        out.quoted(&text_value(&string));
        out.0.extend("\r\n".encode_utf16());
    }
    out.line(2, "}");
    Some(())
}

pub(super) fn render(data: &[u8]) -> Option<Vec<u16>> {
    let (root, _) = block(data)?;
    if String::from_utf16(&root.key).ok()? != "VS_VERSION_INFO"
        || root.text
        || root.value_units != FIXED_INFO
        || fixed_word(root.value, 0) != FIXED_SIGNATURE
    {
        return None;
    }
    let fixed = root.value;
    let version = |high: u32, low: u32| {
        format!(
            "{},{},{},{}",
            high >> 16,
            high & 0xFFFF,
            low >> 16,
            low & 0xFFFF
        )
    };
    let mut out = Text(Vec::new());
    out.line(
        0,
        &format!(
            "FILEVERSION    {}",
            version(fixed_word(fixed, 2), fixed_word(fixed, 3))
        ),
    );
    out.line(
        0,
        &format!(
            "PRODUCTVERSION {}",
            version(fixed_word(fixed, 4), fixed_word(fixed, 5))
        ),
    );
    out.line(0, &format!("FILEFLAGSMASK  {}", hex(fixed_word(fixed, 6))));
    out.line(
        0,
        &format!("FILEFLAGS      {}", flags(fixed_word(fixed, 7))),
    );
    out.line(0, &format!("FILEOS         {}", os(fixed_word(fixed, 8))));
    out.line(
        0,
        &format!("FILETYPE       {}", file_type(fixed_word(fixed, 9))),
    );
    out.line(
        0,
        &format!(
            "FILESUBTYPE    {}",
            subtype(fixed_word(fixed, 9), fixed_word(fixed, 10))
        ),
    );
    out.line(0, "{");
    for child in blocks(root.children)? {
        match String::from_utf16(&child.key).ok()?.as_str() {
            "StringFileInfo" => {
                out.line(1, "BLOCK \"StringFileInfo\"");
                out.line(1, "{");
                for table in blocks(child.children)? {
                    string_table(&mut out, &table)?;
                }
                out.line(1, "}");
            }
            "VarFileInfo" => {
                out.line(1, "BLOCK \"VarFileInfo\"");
                out.line(1, "{");
                for var in blocks(child.children)? {
                    if String::from_utf16(&var.key).ok()? != "Translation"
                        || var.value.len() % 4 != 0
                    {
                        return None;
                    }
                    let mut line = "VALUE \"Translation\"".to_owned();
                    for pair in var.value.chunks_exact(4) {
                        let language = u16::from_le_bytes([pair[0], pair[1]]);
                        let page = u16::from_le_bytes([pair[2], pair[3]]);
                        line.push_str(&format!(", 0x{language:X}, {page}"));
                    }
                    out.line(2, &line);
                }
                out.line(1, "}");
            }
            _ => return None,
        }
    }
    out.line(0, "}");
    Some(out.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_fields_are_named_as_7zip_names_them() {
        for (value, name) in [
            (0, "VOS_UNKNOWN"),
            (1, "VOS_UNKNOWN | VOS__WINDOWS16"),
            (5, "VOS_UNKNOWN | 0x5"),
            (0x1_0000, "VOS_DOS"),
            (0x1_0001, "VOS_DOS_WINDOWS16"),
            (0x1_0003, "VOS_DOS | VOS__PM32"),
            (0x4_0004, "VOS_NT_WINDOWS32"),
            (0x5_0004, "VOS_WINCE | VOS__WINDOWS32"),
            (0x6_0000, "0x60000"),
            (0x1234_5678, "0x12340000 | 0x5678"),
        ] {
            assert_eq!(os(value), name, "{value:#x}");
        }
        assert_eq!(flags(0), "0x0");
        assert_eq!(flags(0x41), "VS_FF_DEBUG | 0x40");
        assert_eq!(flags(0xFFC0), "0xFFC0");
        assert_eq!(file_type(6), "0x6");
        assert_eq!(subtype(3, 12), "VFT2_DRV_VERSIONED_PRINTER");
        assert_eq!(subtype(3, 13), "0xD");
        assert_eq!(subtype(4, 3), "VFT2_FONT_TRUETYPE");
        assert_eq!(subtype(1, 5), "0x5");
    }
}
