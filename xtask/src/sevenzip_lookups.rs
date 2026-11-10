use anyhow::{Result, ensure};
use std::{fs, path::Path};

const LOCATOR: &str = "tests/common/sevenzip.rs";
const DIRECT_LOOKUPS: &[&str] = &["\"7zz\"", "new(\"7z\")", "which(\"7z"];

pub fn lint(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    for entry in walkdir::WalkDir::new(root.join("tests")).sort_by_file_name() {
        let entry = entry?;
        let name = entry.path().strip_prefix(root)?.display().to_string();
        if !entry.file_type().is_file()
            || entry.path().extension().is_none_or(|e| e != "rs")
            || name == LOCATOR
        {
            continue;
        }
        for line in direct_lookups(&fs::read_to_string(entry.path())?) {
            problems.push(format!("{name}:{line}"));
        }
    }
    for problem in &problems {
        eprintln!("{problem}: looks up 7-Zip by name");
    }
    ensure!(
        problems.is_empty(),
        "{} direct 7-Zip lookup(s) in tests; call sevenzip::sevenzip() from {LOCATOR}, \
         which fails instead of skipping when ZIPPER_PARITY_REQUIRE_7Z is set",
        problems.len()
    );
    Ok(())
}

fn direct_lookups(source: &str) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| DIRECT_LOOKUPS.iter().any(|lookup| line.contains(lookup)))
        .map(|(index, _)| index + 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::direct_lookups;

    #[test]
    fn a_lookup_by_name_is_found_and_prose_or_paths_are_not() {
        let source = r#"//! `7zz x` extracts it.
let a = ["7zz", "7z"].into_iter();
let b = Command::new("7zz");
let c = which("7zz");
let d = Command::new("7z");
// EXTERNAL(7-Zip): "7zz" refuses this.
let e = work.path().join("7z");
let f = format!("7zz-{name}");
let g = sevenzip::sevenzip();
"#;
        assert_eq!(direct_lookups(source), [2, 3, 4, 5]);
    }
}
