//! A file that holds one compressed stream and no tar, such as `notes.txt.gz`,
//! extracts to one file named after the archive without its compression
//! extension, as 7-Zip names it. Concatenated streams decode in order.
use crate::{Entry, Existing, native_tar::Compression, progress::Tracker, safe_path};
use anyhow::{Context, Result, ensure};
use std::{
    io::{BufWriter, Read, Write},
    path::Path,
};

const EXTENSIONS: [&str; 9] = [
    ".gz", ".gzip", ".bz2", ".bzip2", ".xz", ".zst", ".zstd", ".lzma", ".lz",
];
const TAR_EXTENSIONS: [&str; 7] = [".tgz", ".tbz", ".tbz2", ".txz", ".tzst", ".tpz", ".tlz"];
const LZMA_PROPERTIES_LIMIT: u8 = 9 * 5 * 5;
const COPY_BUFFER: usize = 1024 * 1024;

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

/// The compression of a file that holds one compressed stream and no tar,
/// or `None`. A file with no signature counts as LZMA only when it is named
/// `*.lzma`.
pub fn sniff(path: &Path) -> Result<Option<Compression>> {
    if crate::native_tar::sniff(path)?.is_some() {
        return Ok(None);
    }
    let compression = match crate::native_tar::compression_by_signature(path)? {
        Compression::None if has_extension(path, ".lzma") && has_lzma_properties(path)? => {
            Compression::Lzma
        }
        found => found,
    };
    Ok((compression != Compression::None).then_some(compression))
}

fn has_lzma_properties(path: &Path) -> Result<bool> {
    let mut first = [0u8; 1];
    let read = std::fs::File::open(path)?.read(&mut first)?;
    Ok(read == 1 && first[0] < LZMA_PROPERTIES_LIMIT)
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
    } else if let Some(dot) = name.rfind('.').filter(|&dot| dot > 0) {
        name[..dot].to_owned()
    } else {
        format!("{name}~")
    };
    let output = output.replace(['\\', ':'], "_");
    let trailing: &[char] = if cfg!(windows) { &[' ', '.'] } else { &[' '] };
    let output = match output.trim_end_matches(trailing) {
        "" | "." | ".." => "_".to_owned(),
        trimmed => trimmed.to_owned(),
    };
    Ok([output.clone(), format!("_{output}"), "_".to_owned()]
        .into_iter()
        .find(|candidate| safe_path(candidate).is_ok())
        .expect("`_` is a safe name"))
}

/// Verify a compressed file, and extract it into `destination` when given.
pub fn read_archive(
    path: &Path,
    destination: Option<&Path>,
    options: ReadOptions,
) -> Result<Vec<Entry>> {
    let compression = sniff(path)?.context("not a single compressed stream")?;
    let name = output_name(path)?;
    let selection = crate::Selection::new(options.paths, options.include, options.exclude)?;
    selection.ensure_each_path_matches(std::iter::once(name.as_str()))?;
    let placement = crate::placement::Placement::new(options.strip)?;
    placement.ensure_prefix_is_a_folder(std::iter::once((name.as_str(), false)))?;
    placement.ensure_something_placed(placement.place(&name).is_some())?;
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
    let mut output = match &staging {
        Some(staging) if selection.selects(&name) => Some(BufWriter::with_capacity(
            COPY_BUFFER,
            crate::create_new_file(&staging.path().join(&name), 0o666, 0)?,
        )),
        _ => None,
    };
    let mut lzma = None;
    let mut input: Box<dyn Read + Send> = match compression {
        Compression::Lzma => {
            let decoder = liblzma::bufread::XzDecoder::new_stream(
                std::io::BufReader::new(std::fs::File::open(path)?),
                liblzma::stream::Stream::new_lzma_decoder(u64::MAX)?,
            );
            Box::new(lzma.insert(decoder))
        }
        _ => crate::native_tar::decoder(path, compression)?,
    };
    let mut buffer = vec![0u8; COPY_BUFFER];
    let mut size = 0u64;
    loop {
        let read = input
            .read(&mut buffer)
            .with_context(|| format!("cannot decode {}", path.display()))?;
        if read == 0 {
            break;
        }
        size += read as u64;
        ensure!(
            size <= limit,
            "{name} decodes to more than {limit} bytes; pass --max-extracted-bytes to raise the cap"
        );
        tracker.add_bytes(read as u64)?;
        if let Some(output) = &mut output {
            output.write_all(&buffer[..read])?;
        }
    }
    drop(input);
    if let Some(decoder) = lzma {
        ensure!(
            std::io::BufRead::fill_buf(&mut decoder.into_inner())?.is_empty(),
            "{} has data after its LZMA stream",
            path.display()
        );
    }
    if let Some(output) = output {
        output.into_inner().map_err(|error| error.into_error())?;
    }
    tracker.file_done()?;
    tracker.finish();
    tracker.check()?;
    if let Some(staging) = staging {
        staging.publish(&[])?;
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
