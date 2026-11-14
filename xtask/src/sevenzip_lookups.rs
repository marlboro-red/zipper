use anyhow::{Result, ensure};
use std::{fs, path::Path};

struct Tool {
    name: &'static str,
    locator: &'static str,
    call: &'static str,
    flag: &'static str,
    lookups: &'static [&'static str],
}

const TOOLS: [Tool; 2] = [
    Tool {
        name: "7-Zip",
        locator: "tests/common/sevenzip.rs",
        call: "sevenzip::sevenzip()",
        flag: "ZIPPER_PARITY_REQUIRE_7Z",
        lookups: &["\"7zz\"", "new(\"7z\")", "which(\"7z"],
    },
    Tool {
        name: "unrar",
        locator: "tests/common/unrar.rs",
        call: "unrar::unrar()",
        flag: "ZIPPER_PARITY_REQUIRE_UNRAR",
        lookups: &["\"unrar\"", "which(\"unrar"],
    },
];

pub fn lint(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    for tool in &TOOLS {
        let found = lookups_outside_locator(root, tool)?;
        for problem in &found {
            eprintln!("{problem}: looks up {} by name", tool.name);
        }
        if !found.is_empty() {
            problems.push(format!(
                "{} direct {} lookup(s) in tests; call {} from {}, \
                 which fails instead of skipping when {} is set",
                found.len(),
                tool.name,
                tool.call,
                tool.locator,
                tool.flag
            ));
        }
    }
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    Ok(())
}

fn lookups_outside_locator(root: &Path, tool: &Tool) -> Result<Vec<String>> {
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(root.join("tests")).sort_by_file_name() {
        let entry = entry?;
        let name = entry.path().strip_prefix(root)?.display().to_string();
        if !entry.file_type().is_file()
            || entry.path().extension().is_none_or(|e| e != "rs")
            || name == tool.locator
        {
            continue;
        }
        for line in direct_lookups(&fs::read_to_string(entry.path())?, tool.lookups) {
            found.push(format!("{name}:{line}"));
        }
    }
    Ok(found)
}

fn direct_lookups(source: &str, lookups: &[&str]) -> Vec<usize> {
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| lookups.iter().any(|lookup| line.contains(lookup)))
        .map(|(index, _)| index + 1)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{TOOLS, direct_lookups};

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
        assert_eq!(direct_lookups(source, TOOLS[0].lookups), [2, 3, 4, 5]);
        assert!(direct_lookups(source, TOOLS[1].lookups).is_empty());
    }

    #[test]
    fn an_unrar_lookup_by_name_is_found_and_prose_or_paths_are_not() {
        let source = r#"//! `unrar x` extracts it.
let a = Command::new("unrar");
let b = which("unrar");
let c = work.path().join("theirs-unrar");
let d = unrar::unrar();
"#;
        assert_eq!(direct_lookups(source, TOOLS[1].lookups), [2, 3]);
        assert!(direct_lookups(source, TOOLS[0].lookups).is_empty());
    }
}
