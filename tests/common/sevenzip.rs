#![allow(dead_code)]
use std::path::PathBuf;

const THE_7ZIP_THE_TESTS_WERE_CHECKED_AGAINST: (u32, u32) = (26, 5);

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
            version(binary).is_some_and(|found| found >= THE_7ZIP_THE_TESTS_WERE_CHECKED_AGAINST)
        })
}

fn version(binary: &std::path::Path) -> Option<(u32, u32)> {
    let banner = std::process::Command::new(binary).output().ok()?.stdout;
    version_in_banner(&String::from_utf8_lossy(&banner))
}

fn version_in_banner(banner: &str) -> Option<(u32, u32)> {
    let after = banner.split("7-Zip").nth(1)?;
    let number = after
        .split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let (major, minor) = number.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

#[test]
fn only_the_7zip_the_tests_were_checked_against_or_a_newer_one_is_used() {
    let floor = THE_7ZIP_THE_TESTS_WERE_CHECKED_AGAINST;
    let at = |banner: &str| version_in_banner(banner).is_some_and(|found| found >= floor);
    assert!(at(
        "\n7-Zip (z) 26.05 (arm64) : Copyright (c) 1999-2026 Igor Pavlov : 2026-04-01\n"
    ));
    assert!(at("\n7-Zip (z) 27.00 (x64) : Copyright"));
    assert!(!at("\n7-Zip (z) 26.00 (x64) : Copyright"));
    assert!(!at(
        "\n7-Zip [64] 16.02 : Copyright (c) 1999-2016 Igor Pavlov"
    ));
    assert!(!at("no banner"));
}
