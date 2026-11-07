#![allow(dead_code)]
use std::path::PathBuf;

const FIRST_7ZIP_THESE_CABS_WERE_CHECKED_AGAINST: u32 = 26;

pub fn sevenzip() -> Option<PathBuf> {
    ["7zz", "7z"]
        .into_iter()
        .find(|binary| {
            std::process::Command::new(binary)
                .arg("i")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .map(PathBuf::from)
        .filter(|binary| {
            major_version(binary)
                .is_some_and(|major| major >= FIRST_7ZIP_THESE_CABS_WERE_CHECKED_AGAINST)
        })
}

fn major_version(binary: &std::path::Path) -> Option<u32> {
    let banner = std::process::Command::new(binary).output().ok()?.stdout;
    let banner = String::from_utf8_lossy(&banner);
    let after = banner.split("7-Zip").nth(1)?;
    let number = after
        .split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    number.split('.').next()?.parse().ok()
}
