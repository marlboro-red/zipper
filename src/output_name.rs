use crate::safe_path;
use anyhow::{Context, Result};
use std::path::Path;

pub(crate) fn named(path: &Path) -> std::path::PathBuf {
    crate::volumes::joined_name(path).unwrap_or_else(|| path.to_owned())
}

pub(crate) fn archive_file_name(archive: &Path) -> Result<String> {
    Ok(archive
        .file_name()
        .context("the archive path has no file name")?
        .to_string_lossy()
        .into_owned())
}

// EXTERNAL(7-Zip 26.05): an item with no name of its own takes 7-Zip's default name.
pub(crate) fn default_name(archive_name: &str, added: &str) -> String {
    match archive_name.rfind('.').filter(|&dot| dot > 0) {
        Some(dot) => format!("{}{added}", &archive_name[..dot]),
        None if added.is_empty() => format!("{archive_name}~"),
        None => format!("{archive_name}{added}"),
    }
}

pub(crate) fn without_trailing_spaces(name: &str) -> &str {
    let trailing: &[char] = if cfg!(windows) { &[' ', '.'] } else { &[' '] };
    name.trim_end_matches(trailing)
}

pub(crate) fn safe_output_name(output: &str, archive_name: &str) -> String {
    let output = output.replace(['\\', ':'], "_");
    let output = match without_trailing_spaces(&output) {
        "" | "." | ".." => "_".to_owned(),
        trimmed => trimmed.to_owned(),
    };
    let output = match output.eq_ignore_ascii_case(archive_name) {
        true => format!("{output}~"),
        false => output,
    };
    [output.clone(), format!("_{output}"), "_".to_owned()]
        .into_iter()
        .find(|candidate| safe_path(candidate).is_ok())
        .expect("`_` is a safe name")
}
