//! A file that holds one compressed stream and no tar, such as `notes.txt.gz`,
//! extracts to one file named after the archive without its compression
//! extension, as 7-Zip names it. Concatenated streams decode in order.
use crate::{Entry, Existing, native_tar::Compression, progress::Tracker, safe_path};
use anyhow::{Context, Result, ensure};
use std::{
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::Path,
};

const EXTENSIONS: [&str; 11] = [
    ".gz", ".gzip", ".bz2", ".bzip2", ".xz", ".zst", ".zstd", ".lzma", ".lz", ".lz4", ".z",
];
const TAR_EXTENSIONS: [&str; 10] = [
    ".tgz", ".tbz", ".tbz2", ".txz", ".tzst", ".tpz", ".tlz", ".tlz4", ".taz", ".tz",
];
const LZMA_PROPERTIES_LIMIT: u8 = 9 * 5 * 5;
const COPY_BUFFER: usize = 1024 * 1024;
const GZIP_WINDOW: usize = 512 * 1024 + crate::inflate::HISTORY;

enum Decoded<'a> {
    Copied {
        input: Box<dyn Read + Send + 'a>,
        buffer: Vec<u8>,
    },
    Held(Box<dyn BufRead + Send + 'a>),
}

impl<'a> Decoded<'a> {
    fn copied(input: Box<dyn Read + Send + 'a>) -> Self {
        Self::Copied {
            input,
            buffer: vec![0; COPY_BUFFER],
        }
    }

    fn next(&mut self) -> std::io::Result<&[u8]> {
        match self {
            Self::Copied { input, buffer } => {
                let read = input.read(buffer)?;
                Ok(&buffer[..read])
            }
            Self::Held(input) => input.fill_buf(),
        }
    }

    fn written(&mut self, count: usize) {
        if let Self::Held(input) = self {
            input.consume(count);
        }
    }

    fn hands_out_large_pieces(&self) -> bool {
        matches!(self, Self::Held(_))
    }
}

/// Options for [`read_archive`].
#[derive(Clone, Copy, Default)]
pub struct ReadOptions<'a> {
    /// Cap on decoded bytes. Without it, the output must fit in free space.
    pub max_output_bytes: Option<u64>,
    /// What to do when the destination exists.
    pub existing: Existing,
    /// Exact entry paths to read, written as for `ExtractOptions::paths`.
    pub paths: &'a [String],
    /// Wildcard patterns to read, written as for `ExtractOptions::include`.
    pub include: &'a [String],
    /// Wildcard patterns to leave out, written as for `ExtractOptions::include`.
    pub exclude: &'a [String],
    /// Leading path segments to drop before writing, as [`crate::Strip`]
    /// describes. Selection still matches the full archive paths.
    pub strip: crate::Strip<'a>,
    /// Which metadata to restore beside contents, as [`crate::Metadata`]
    /// describes.
    pub metadata: crate::Metadata,
    /// Progress reports and cancellation.
    pub control: crate::Control<'a>,
}

/// What a file that holds one stream and no tar holds, as [`sniff`] finds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Format {
    /// gzip, bzip2, xz, zstd, lzip, LZ4, `compress` or LZMA.
    Compressed(Compression),
    /// A Microsoft `compress.exe` SZDD file, such as `SETUP.EX_`.
    Szdd,
    /// The x86 BCJ and LZMA format of 7-Zip's `lzma` tool, `.lzma86`.
    Lzma86,
    /// A PPMd var.H or var.I file, `.pmd`.
    Ppmd,
    /// A Base64 text file named `*.b64`.
    Base64,
}

/// The format of a file that holds one compressed stream and no tar, or
/// `None`. A file with no signature counts as LZMA only when it is named
/// `*.lzma`, as Base64 only when it is named `*.b64` and its first 4 KiB
/// pass 7-Zip's check, and as lzma86 only when it starts with a header
/// 7-Zip accepts.
pub fn sniff(path: &Path) -> Result<Option<Format>> {
    if crate::native_tar::sniff(path)?.is_some() {
        return Ok(None);
    }
    let compression = match crate::native_tar::compression_by_signature(path)? {
        Compression::None if has_extension(&named(path), ".lzma") && has_lzma_properties(path)? => {
            Compression::Lzma
        }
        found => found,
    };
    if compression != Compression::None {
        return Ok(Some(Format::Compressed(compression)));
    }
    let mut start = Vec::with_capacity(crate::base64::SNIFF_BYTES);
    crate::volumes::open(path)?
        .take(crate::base64::SNIFF_BYTES as u64)
        .read_to_end(&mut start)?;
    let lzma86_start = &start[..start.len().min(crate::lzma86::SNIFF_BYTES)];
    Ok(if start.starts_with(&crate::mslz::SIGNATURE) {
        Some(Format::Szdd)
    } else if start.starts_with(&crate::pmd::SIGNATURE) {
        Some(Format::Ppmd)
    } else if has_extension(&named(path), crate::base64::EXTENSION)
        && !start.is_empty()
        && crate::base64::looks_like_base64(&start)
    {
        Some(Format::Base64)
    } else if crate::lzma86::looks_like_lzma86(lzma86_start) {
        Some(Format::Lzma86)
    } else {
        None
    })
}

const LEGACY_LZ4_MAGIC: [u8; 4] = [0x02, 0x21, 0x4c, 0x18];

fn starts_with_legacy_lz4(path: &Path) -> Result<bool> {
    let mut magic = [0u8; 4];
    let read = crate::volumes::open(path)?.read(&mut magic)?;
    Ok(read == magic.len() && magic == LEGACY_LZ4_MAGIC)
}

fn has_lzma_properties(path: &Path) -> Result<bool> {
    let mut first = [0u8; 1];
    let read = crate::volumes::open(path)?.read(&mut first)?;
    Ok(read == 1 && first[0] < LZMA_PROPERTIES_LIMIT)
}

fn named(path: &Path) -> std::path::PathBuf {
    crate::volumes::joined_name(path).unwrap_or_else(|| path.to_owned())
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(extension))
}

/// The name the decoded file gets, as 7-Zip names it. A compression
/// extension is dropped, and `.tgz`, `.tbz`, `.tbz2`, `.txz`, `.tzst` and
/// `.tpz` become `.tar`. Any other extension is dropped, and a name without
/// one gains `~`, so the decoded file never takes the archive's own name.
/// `\` and `:` become `_`, trailing spaces go, and on Windows trailing dots
/// too. A name left as `.` or `..` becomes `_`, and a name that is still not
/// safe, such as `CON` on Windows, gains a leading `_`.
pub fn output_name(archive: &Path) -> Result<String> {
    let name = archive
        .file_name()
        .context("the archive path has no file name")?
        .to_string_lossy();
    let lower = name.to_ascii_lowercase();
    let stem = |extensions: &[&str]| {
        extensions.iter().find_map(|extension| {
            lower
                .strip_suffix(extension)
                .filter(|stem| !stem.is_empty())
                .map(|stem| name[..stem.len()].to_owned())
        })
    };
    let output = if let Some(stem) = stem(&TAR_EXTENSIONS) {
        format!("{stem}.tar")
    } else if let Some(stem) = stem(&EXTENSIONS) {
        stem
    } else {
        return default_name(archive);
    };
    Ok(safe_name(&output, &[' ']))
}

fn is_the_archive_name(name: &str, archive_name: &str) -> bool {
    name.eq_ignore_ascii_case(archive_name)
}

fn archive_file_name(archive: &Path) -> Result<String> {
    Ok(archive
        .file_name()
        .context("the archive path has no file name")?
        .to_string_lossy()
        .into_owned())
}

// EXTERNAL(7-Zip DefaultName.cpp, GetDefaultName2): an item with no name of its own takes the
// archive's name without its last extension, or with `~` added when it has none.
fn default_name(archive: &Path) -> Result<String> {
    let name = archive_file_name(archive)?;
    let output = match name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => name[..dot].to_owned(),
        None => format!("{name}~"),
    };
    Ok(safe_name(&output, &[' ']))
}

fn safe_name(output: &str, trailing: &[char]) -> String {
    let output = output.replace(['\\', ':'], "_");
    let trailing: Vec<char> = trailing
        .iter()
        .copied()
        .chain(cfg!(windows).then_some([' ', '.']).into_iter().flatten())
        .collect();
    let output = match output.trim_end_matches(trailing.as_slice()) {
        "" | "." | ".." => "_".to_owned(),
        trimmed => trimmed.to_owned(),
    };
    [output.clone(), format!("_{output}"), "_".to_owned()]
        .into_iter()
        .find(|candidate| safe_path(candidate).is_ok())
        .expect("`_` is a safe name")
}

// EXTERNAL(7-Zip 26.05 ExtractingFilePath.cpp, Correct_FsPath): a stored name loses its empty, `.` and `..` folders, and a
// last part that is empty, `.` or `..` becomes `_`.
fn stored_path(stored: &str) -> String {
    let mut parts: Vec<&str> = stored.split('/').collect();
    let last = parts.pop().unwrap_or_default();
    parts
        .into_iter()
        .filter(|part| !matches!(*part, "" | "." | ".."))
        .chain(std::iter::once(last))
        .map(|part| safe_name(part, &[]))
        .collect::<Vec<_>>()
        .join("/")
}

const SZDD_EXTENSIONS: [&[u8; 3]; 6] = [b"bin", b"dll", b"exe", b"kmd", b"pdf", b"sys"];
const PRINTABLE_ASCII: std::ops::Range<u8> = 0x20..0x80;

// EXTERNAL(7-Zip 26.05 MslzHandler.cpp, ParseName): a trailing `_` gives way to a printable
// header byte, or for a zero byte to the last letter of an extension the table names.
fn szdd_name(archive: &Path, last_character: u8) -> Result<String> {
    let name = archive_file_name(archive)?;
    let Some(stem) = name.strip_suffix('_') else {
        return default_name(archive);
    };
    let replacement = match (last_character, stem.as_bytes()) {
        (0, [.., b'.', first, second]) => SZDD_EXTENSIONS
            .iter()
            .find(|extension| extension[..2] == [*first, *second])
            .map_or(0, |extension| extension[2]),
        (byte, _) => byte,
    };
    let mut output = stem.to_owned();
    if PRINTABLE_ASCII.contains(&replacement) {
        output.push(char::from(replacement));
    }
    if output.is_empty() {
        return default_name(archive);
    }
    Ok(stored_path(&output))
}

type Opened = (String, Option<Box<dyn Read + Send>>);

fn read_exactly(input: &mut impl Read, bytes: &mut [u8], format: &str) -> Result<()> {
    input.read_exact(bytes).map_err(|error| match error.kind() {
        std::io::ErrorKind::UnexpectedEof => anyhow::anyhow!("the {format} header is truncated"),
        _ => error.into(),
    })
}

fn opened_szdd(path: &Path) -> Result<Opened> {
    let mut input = BufReader::with_capacity(COPY_BUFFER, crate::volumes::open(path)?);
    let mut bytes = [0; crate::mslz::HEADER];
    read_exactly(&mut input, &mut bytes, "SZDD")?;
    let header = crate::mslz::header(bytes)?;
    Ok((
        szdd_name(&named(path), header.last_character)?,
        Some(Box::new(crate::mslz::SzddReader::new(input, header.size))),
    ))
}

fn opened_pmd(path: &Path) -> Result<Opened> {
    let mut input = BufReader::with_capacity(COPY_BUFFER, crate::volumes::open(path)?);
    let mut bytes = [0; crate::pmd::HEADER];
    read_exactly(&mut input, &mut bytes, "PPMd")?;
    let header = crate::pmd::header(bytes)?;
    let mut stored = vec![0; usize::from(header.name_length)];
    read_exactly(&mut input, &mut stored, "PPMd")?;
    let stored = String::from_utf8_lossy(crate::pmd::stored_name(&stored)).into_owned();
    let name = match stored.is_empty() {
        true => default_name(&named(path))?,
        false => stored_path(&stored),
    };
    Ok((
        name,
        Some(Box::new(crate::pmd::PmdReader::new(input, &header)?)),
    ))
}

/// Verify a compressed file, and extract it into `destination` when given.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let format = sniff(path)?.context("not a single compressed stream")?;
    crate::volumes::ensure_stream_set_ends_short(path)?;
    match format {
        Format::Compressed(Compression::Compress) => {
            crate::volumes::ensure_not_split(path, "compress")?
        }
        Format::Compressed(Compression::Lz4) if starts_with_legacy_lz4(path)? => {
            crate::volumes::ensure_not_split(path, "legacy LZ4")?
        }
        Format::Base64 => crate::volumes::ensure_not_split(path, "Base64")?,
        _ => {}
    }
    let (name, opened) = match format {
        Format::Compressed(_) => (output_name(&named(path))?, None),
        Format::Szdd => opened_szdd(path)?,
        Format::Lzma86 => (
            default_name(&named(path))?,
            Some(
                Box::new(crate::lzma86::Lzma86Streams::new(BufReader::with_capacity(
                    COPY_BUFFER,
                    crate::volumes::open(path)?,
                ))) as Box<dyn Read + Send>,
            ),
        ),
        Format::Ppmd => opened_pmd(path)?,
        Format::Base64 => (
            default_name(&named(path))?,
            Some(
                Box::new(crate::base64::Base64Reader::new(BufReader::with_capacity(
                    COPY_BUFFER,
                    crate::volumes::open(path)?,
                ))) as Box<dyn Read + Send>,
            ),
        ),
    };
    let archive_name = archive_file_name(path)?;
    let name = match is_the_archive_name(&name, &archive_name) {
        true => format!("{name}~"),
        false => name,
    };
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    selection.ensure_each_path_matches(std::iter::once(name.as_str()))?;
    let placement = crate::placement::Placement::new(options.strip)?;
    placement.ensure_prefix_is_a_folder(std::iter::once((name.as_str(), false)))?;
    let placed = placement.place(&name);
    placement.ensure_something_placed(placed.is_some())?;
    ensure!(
        !placed.is_some_and(|placed| is_the_archive_name(placed, &archive_name)),
        "the strip would name the decoded file {archive_name}, the archive's own name"
    );
    let tracker = Tracker::new(options.control, Some(1), None);
    tracker.check()?;
    let staging = destination
        .map(|destination| {
            crate::staging::stage(path, destination, options.existing, options.metadata)
        })
        .transpose()?;
    let limit = match (&staging, options.max_output_bytes) {
        (_, Some(limit)) => limit,
        (Some(staging), None) => crate::available_bytes(staging.path()).with_context(|| {
            format!("cannot measure free space at {}", staging.path().display())
        })?,
        (None, None) => u64::MAX,
    };
    let mut lzma = None;
    let mut input = match (format, opened) {
        (_, Some(opened)) => Decoded::copied(opened),
        (Format::Compressed(Compression::Lzma), None) => {
            let decoder =
                crate::native_tar::lzma_decoder(BufReader::new(crate::volumes::open(path)?))?;
            Decoded::copied(Box::new(crate::dictionary::DecoderMemory(
                lzma.insert(decoder),
            )))
        }
        (Format::Compressed(Compression::Gzip), None) => {
            Decoded::Held(Box::new(crate::framing::Reader::gzip_with_window(
                BufReader::with_capacity(COPY_BUFFER, crate::volumes::open(path)?),
                GZIP_WINDOW,
            )))
        }
        (Format::Compressed(compression), None) => {
            Decoded::copied(crate::native_tar::decoder(path, compression)?)
        }
        (_, None) => anyhow::bail!("{} has no decoder", path.display()),
    };
    let mut output: Option<Box<dyn Write>> = match (&staging, placed) {
        (Some(staging), Some(placed)) if selection.selects(&name) => {
            let target = staging.path().join(placed);
            if let Some(folder) = target.parent() {
                std::fs::create_dir_all(folder)?;
            }
            let file = crate::create_new_file(&target, 0o666, 0)?;
            Some(match input.hands_out_large_pieces() {
                true => Box::new(file),
                false => Box::new(BufWriter::with_capacity(COPY_BUFFER, file)),
            })
        }
        _ => None,
    };
    let mut size = 0u64;
    let mut held = 0u64;
    loop {
        let piece = input
            .next()
            .with_context(|| format!("cannot decode {}", path.display()))?;
        let read = piece.len();
        if read == 0 {
            break;
        }
        size += read as u64;
        ensure!(
            size <= limit,
            crate::OverLimit(format!(
                "{name} decodes to more than {limit} bytes; pass --max-extracted-bytes to raise the cap"
            ))
        );
        if let Some(output) = &mut output {
            output.write_all(piece)?;
        }
        input.written(read);
        tracker.add_bytes(std::mem::replace(&mut held, read as u64))?;
    }
    drop(input);
    if let Some(decoder) = lzma {
        ensure!(
            std::io::BufRead::fill_buf(&mut decoder.into_inner())?.is_empty(),
            "{} has data after its LZMA stream",
            path.display()
        );
    }
    if let Some(mut output) = output {
        output.flush()?;
    }
    tracker.add_bytes(held)?;
    tracker.file_done()?;
    tracker.finish();
    tracker.check()?;
    if let Some(staging) = staging {
        staging.publish(&[], options.control.cancel)?;
    }
    Ok(vec![Entry {
        name,
        size,
        directory: false,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names_are_safe_and_never_the_archive_name_on_every_short_name() {
        let mut names = vec![String::new()];
        let mut checked = 0;
        for _ in 0..6 {
            names = names
                .iter()
                .flat_map(|name| {
                    ['a', '.', 'g', 'z', 'T', ' ', '\\', ':']
                        .map(|character| format!("{name}{character}"))
                })
                .collect();
            for name in names.iter().filter(|name| {
                Path::new(name.as_str()).file_name() == Some(std::ffi::OsStr::new(name.as_str()))
            }) {
                let output = output_name(Path::new(name))
                    .unwrap_or_else(|error| panic!("{name:?}: {error:#}"));
                assert!(
                    safe_path(&output).is_ok() && output != *name,
                    "{name:?} gave {output:?}"
                );
                checked += 1;
            }
        }
        assert!(checked > 100_000);
    }
}
