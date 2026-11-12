#![allow(dead_code)]
use std::path::PathBuf;

const THE_7ZIP_THE_TESTS_WERE_CHECKED_AGAINST: (u32, u32) = (26, 5);

const REQUIRE_7Z: &str = "ZIPPER_PARITY_REQUIRE_7Z";

pub const NAMES_ON_PATH: [&str; 2] = ["7zz", "7z"];

pub fn sevenzip() -> Option<PathBuf> {
    found_or_skipped(installed(), std::env::var_os(REQUIRE_7Z).is_some())
}

const CHARACTERS_WINDOWS_FORBIDS: &[char] = &['<', '>', ':', '"', '|', '?', '*'];

pub fn windows_forbids(name: &str) -> bool {
    cfg!(windows)
        && name.split('/').any(|part| {
            part.chars()
                .any(|c| c.is_control() || CHARACTERS_WINDOWS_FORBIDS.contains(&c))
                || part.ends_with(['.', ' '])
        })
}

pub fn zipper_refused_a_name_windows_forbids(error: &str) -> bool {
    const REFUSALS: [&str; 3] = [
        "(os error 123)",
        "ambiguous Windows path",
        "Windows device path",
    ];
    cfg!(windows) && REFUSALS.iter().any(|refusal| error.contains(refusal))
}

pub fn as_7zip_names_it_on_this_system(name: &str) -> String {
    if !cfg!(windows) {
        return name.to_owned();
    }
    name.chars()
        .map(
            |c| match c.is_control() || CHARACTERS_WINDOWS_FORBIDS.contains(&c) {
                true => '_',
                false => c,
            },
        )
        .collect()
}

pub fn spelled_through_the_windows_code_page<'a>(
    names: impl IntoIterator<Item = &'a String>,
) -> bool {
    cfg!(windows) && names.into_iter().any(|name| !name.is_ascii())
}

fn found_or_skipped(found: Option<PathBuf>, required: bool) -> Option<PathBuf> {
    assert!(
        found.is_some() || !required,
        "{REQUIRE_7Z} is set and no 7-Zip {}.{:02} or later is on PATH as 7zz or 7z",
        THE_7ZIP_THE_TESTS_WERE_CHECKED_AGAINST.0,
        THE_7ZIP_THE_TESTS_WERE_CHECKED_AGAINST.1,
    );
    found
}

fn installed() -> Option<PathBuf> {
    NAMES_ON_PATH
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
fn names_windows_forbids_are_judged_only_on_windows() {
    let windows = cfg!(windows);
    assert_eq!(windows_forbids("folder/a|b"), windows);
    assert_eq!(windows_forbids("folder/tail."), windows);
    assert_eq!(windows_forbids("e\u{1}"), windows);
    assert!(!windows_forbids("folder/plain name.txt"));
    let renamed = as_7zip_names_it_on_this_system("a|b\u{1}/c");
    assert_eq!(renamed, if windows { "a_b_/c" } else { "a|b\u{1}/c" });
    assert!(!zipper_refused_a_name_windows_forbids(
        "cannot create entry"
    ));
    assert_eq!(
        spelled_through_the_windows_code_page(&["na\u{e9}me".to_owned()]),
        windows
    );
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

#[test]
#[should_panic(expected = "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26.05 or later")]
fn a_missing_7zip_fails_the_comparisons_the_gate_requires() {
    found_or_skipped(None, true);
}

#[test]
fn a_missing_7zip_skips_the_comparisons_outside_the_gate() {
    assert_eq!(found_or_skipped(None, false), None);
    let found = Some(PathBuf::from("7zz"));
    assert_eq!(found_or_skipped(found.clone(), true), found);
}
