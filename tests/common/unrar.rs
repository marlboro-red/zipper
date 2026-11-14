#![allow(dead_code)]
use std::path::{Path, PathBuf};

const THE_UNRAR_THE_TESTS_WERE_CHECKED_AGAINST: (u32, u32) = (7, 23);

const REQUIRE_UNRAR: &str = "ZIPPER_PARITY_REQUIRE_UNRAR";

pub const NAME_ON_PATH: &str = "unrar";

pub fn unrar() -> Option<PathBuf> {
    found_or_skipped(installed(), std::env::var_os(REQUIRE_UNRAR).is_some())
}

fn found_or_skipped(found: Option<PathBuf>, required: bool) -> Option<PathBuf> {
    assert!(
        found.is_some() || !required,
        "{REQUIRE_UNRAR} is set and no unrar {}.{:02} or later is on PATH",
        THE_UNRAR_THE_TESTS_WERE_CHECKED_AGAINST.0,
        THE_UNRAR_THE_TESTS_WERE_CHECKED_AGAINST.1,
    );
    found
}

fn installed() -> Option<PathBuf> {
    let binary = PathBuf::from(NAME_ON_PATH);
    version(&binary)
        .is_some_and(|found| found >= THE_UNRAR_THE_TESTS_WERE_CHECKED_AGAINST)
        .then_some(binary)
}

fn version(binary: &Path) -> Option<(u32, u32)> {
    let banner = std::process::Command::new(binary).output().ok()?.stdout;
    version_in_banner(&String::from_utf8_lossy(&banner))
}

fn version_in_banner(banner: &str) -> Option<(u32, u32)> {
    let after = banner.split("UNRAR").nth(1)?;
    let number = after.split_whitespace().next()?;
    let (major, minor) = number.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

#[test]
fn only_the_unrar_the_tests_were_checked_against_or_a_newer_one_is_used() {
    let floor = THE_UNRAR_THE_TESTS_WERE_CHECKED_AGAINST;
    let at = |banner: &str| version_in_banner(banner).is_some_and(|found| found >= floor);
    assert!(at(
        "\nUNRAR 7.23 freeware      Copyright (c) 1993-2026 Alexander Roshal\n"
    ));
    assert!(at("\nUNRAR 8.00 freeware      Copyright"));
    assert!(!at("\nUNRAR 7.01 freeware      Copyright"));
    assert!(!at("\nUNRAR 6.24 freeware      Copyright"));
    assert!(!at("no banner"));
}

#[test]
#[should_panic(expected = "ZIPPER_PARITY_REQUIRE_UNRAR is set and no unrar 7.23 or later")]
fn a_missing_unrar_fails_the_comparisons_the_gate_requires() {
    found_or_skipped(None, true);
}

#[test]
fn a_missing_unrar_skips_the_comparisons_outside_the_gate() {
    assert_eq!(found_or_skipped(None, false), None);
    let found = Some(PathBuf::from(NAME_ON_PATH));
    assert_eq!(found_or_skipped(found.clone(), true), found);
}
