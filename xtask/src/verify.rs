use anyhow::{Context, Result, bail, ensure};
use std::{fs, path::Path, process::Command};

const MANIFEST: &str = "verification/coverage.txt";
const UNCOVERED: &str = "verification/uncovered.txt";

struct Verification {
    name: String,
    skill: String,
    fingerprint: String,
    full_checks: Vec<String>,
    items: Vec<(String, String)>,
}

fn manifest(root: &Path) -> Result<Vec<Verification>> {
    let text = fs::read_to_string(root.join(MANIFEST))
        .with_context(|| format!("cannot read {MANIFEST}"))?;
    let mut all: Vec<Verification> = Vec::new();
    for (number, line) in text
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
    {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            all.push(Verification {
                name: name.to_owned(),
                skill: String::new(),
                fingerprint: String::new(),
                full_checks: Vec::new(),
                items: Vec::new(),
            });
            continue;
        }
        let (key, value) = line
            .split_once(" = ")
            .with_context(|| format!("{MANIFEST}:{number}: expected `key = value`"))?;
        let entry = all
            .last_mut()
            .with_context(|| format!("{MANIFEST}:{number}: a key before any [name]"))?;
        match key {
            "skill" => entry.skill = value.to_owned(),
            "fingerprint" => entry.fingerprint = value.to_owned(),
            "full-check" => entry.full_checks.push(value.to_owned()),
            "item" => {
                let (path, opening) = value.split_once(" | ").with_context(|| {
                    format!("{MANIFEST}:{number}: expected `path | opening line`")
                })?;
                entry.items.push((path.to_owned(), opening.to_owned()));
            }
            other => bail!("{MANIFEST}:{number}: unknown key {other:?}"),
        }
    }
    Ok(all)
}

fn item_spans_starting_with<'a>(source: &'a str, opening: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.starts_with(opening) {
            found
                .push(&source[offset..offset + end_of_braced_body_or_semicolon(&source[offset..])]);
        }
        offset += line.len();
    }
    found
}

fn end_of_braced_body_or_semicolon(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'"' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    index += if bytes[index] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if bytes.get(index + 2) == Some(&b'\'') => index += 2,
            b'\'' if bytes.get(index + 1) == Some(&b'\\') => {
                while index + 1 < bytes.len() && bytes[index + 1] != b'\'' {
                    index += 1;
                }
                index += 1;
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + 1;
                }
            }
            b';' if depth == 0 => return index + 1,
            _ => {}
        }
        index += 1;
    }
    bytes.len()
}

// EXTERNAL(Git on Windows): checkouts can turn LF into CRLF.
fn fnv1a(mut hash: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    for byte in bytes.filter(|&byte| byte != b'\r') {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn fingerprint(root: &Path, verification: &Verification) -> Result<String> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (path, opening) in &verification.items {
        let source =
            fs::read_to_string(root.join(path)).with_context(|| format!("cannot read {path}"))?;
        let texts = item_spans_starting_with(&source, opening);
        ensure!(
            !texts.is_empty(),
            "{}: no line in {path} starts with {opening:?}; update {MANIFEST} and the model",
            verification.name
        );
        for text in texts {
            hash = fnv1a(hash, path.bytes().chain(text.bytes()));
        }
    }
    Ok(format!("{hash:016x}"))
}

fn fail_on_interrupted_plants(root: &Path) -> Result<()> {
    let journal = root.join("target/plant-journal");
    let mut left = Vec::new();
    if journal.is_dir() {
        for entry in fs::read_dir(&journal)? {
            left.push(entry?.path().display().to_string());
        }
    }
    ensure!(
        left.is_empty(),
        "an interrupted scripts/plant.py may have left a break in the code ({}); run `python3 scripts/plant.py --restore`",
        left.join(", ")
    );
    Ok(())
}

pub fn fail_on_stale_fingerprints(root: &Path) -> Result<()> {
    fail_on_interrupted_plants(root)?;
    fail_on_unclassified_sources(root)?;
    let mut stale = Vec::new();
    for verification in manifest(root)? {
        let current = fingerprint(root, &verification)?;
        if current != verification.fingerprint {
            stale.push(format!(
                "{}: follow {}, then run `cargo xtask verify-sync {}`",
                verification.name, verification.skill, verification.name
            ));
        }
    }
    ensure!(
        stale.is_empty(),
        "code covered by a formal check changed since the check last passed:\n  {}",
        stale.join("\n  ")
    );
    Ok(())
}

fn run_full_check(root: &Path, check: &str) -> Result<bool> {
    let mut words = check.split_whitespace();
    let mut command = Command::new(words.next().context("empty full-check")?);
    for word in words {
        match word.split_once('=') {
            Some((key, value)) if key.chars().all(|c| c.is_ascii_uppercase() || c == '_') => {
                command.env(key, value);
            }
            _ => {
                command.arg(word);
            }
        }
    }
    Ok(command.current_dir(root).status()?.success())
}

pub fn run_every_full_check(root: &Path) -> Result<()> {
    let mut failed = Vec::new();
    for verification in manifest(root)? {
        if verification.full_checks.is_empty() {
            println!(
                "xtask: release check {}: no full check; the gate's verify step keeps it in step with {}",
                verification.name, verification.skill
            );
        }
        for check in &verification.full_checks {
            println!("xtask: release check {}: {check}", verification.name);
            if !run_full_check(root, check)? {
                failed.push(verification.name.clone());
            }
        }
    }
    ensure!(
        failed.is_empty(),
        "release checks failed: {}",
        failed.join(", ")
    );
    Ok(())
}

pub fn record_checked_fingerprint(root: &Path, name: &str) -> Result<()> {
    let all = manifest(root)?;
    let verification = all
        .iter()
        .find(|verification| verification.name == name)
        .with_context(|| format!("no verification named {name:?} in {MANIFEST}"))?;
    let checked = fingerprint(root, verification)?;
    for check in &verification.full_checks {
        ensure!(
            run_full_check(root, check)?,
            "{name}: the full check `{check}` failed; nothing was synced"
        );
    }
    if verification.full_checks.is_empty() {
        println!(
            "{name}: recording the current code as checked; {} must have passed on it",
            verification.skill
        );
    }
    let current = fingerprint(root, verification)?;
    ensure!(
        current == checked,
        "{name}: the covered code changed while the check ran; nothing was synced"
    );
    let text = fs::read_to_string(root.join(MANIFEST))?;
    let mut in_section = false;
    let mut updated = String::new();
    for line in text.lines() {
        if line.starts_with('[') {
            in_section = line == format!("[{name}]");
        }
        if in_section && line.starts_with("fingerprint = ") {
            updated.push_str(&format!("fingerprint = {current}\n"));
        } else {
            updated.push_str(line);
            updated.push('\n');
        }
    }
    fs::write(root.join(MANIFEST), updated)?;
    println!("{name}: synced at fingerprint {current}");
    Ok(())
}

fn uncovered(root: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(root.join(UNCOVERED))
        .with_context(|| format!("cannot read {UNCOVERED}"))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|(index, line)| {
            let (path, reason) = line
                .split_once(" | ")
                .with_context(|| format!("{UNCOVERED}:{}: expected `path | reason`", index + 1))?;
            ensure!(
                !reason.trim().is_empty(),
                "{UNCOVERED}:{}: {path} needs a reason",
                index + 1
            );
            Ok(path.to_owned())
        })
        .collect()
}

fn sources(root: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut folders = vec![root.join("src")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let relative = path
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push(relative);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn unclassified(sources: &[String], covered: &[String], uncovered: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    for source in sources {
        match (covered.contains(source), uncovered.contains(source)) {
            (false, false) => problems.push(format!(
                "{source} has no formal check in {MANIFEST} and no reason in {UNCOVERED}"
            )),
            (true, true) => problems.push(format!(
                "{source} is covered in {MANIFEST}, so remove its line from {UNCOVERED}"
            )),
            _ => {}
        }
    }
    for path in uncovered {
        if !sources.contains(path) {
            problems.push(format!("{UNCOVERED} lists {path}, which does not exist"));
        }
    }
    problems
}

fn fail_on_unclassified_sources(root: &Path) -> Result<()> {
    let covered: Vec<String> = manifest(root)?
        .into_iter()
        .flat_map(|verification| verification.items.into_iter().map(|(path, _)| path))
        .collect();
    let problems = unclassified(&sources(root)?, &covered, &uncovered(root)?);
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_needs_a_formal_check_or_a_reason_and_never_both() {
        let owned = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        let sources = owned(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        assert!(
            unclassified(
                &sources,
                &owned(&["src/a.rs"]),
                &owned(&["src/b.rs", "src/c.rs"])
            )
            .is_empty()
        );
        let missing = unclassified(&sources, &owned(&["src/a.rs"]), &owned(&["src/b.rs"]));
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("src/c.rs has no formal check"));
        let both = unclassified(
            &sources,
            &owned(&["src/a.rs", "src/b.rs"]),
            &owned(&["src/b.rs", "src/c.rs"]),
        );
        assert_eq!(both.len(), 1);
        assert!(both[0].starts_with("src/b.rs is covered"));
        let gone = unclassified(
            &sources,
            &owned(&["src/a.rs"]),
            &owned(&["src/b.rs", "src/c.rs", "src/d.rs"]),
        );
        assert_eq!(
            gone,
            ["verification/uncovered.txt lists src/d.rs, which does not exist"]
        );
    }

    #[test]
    fn line_endings_do_not_change_a_fingerprint() {
        assert_eq!(fnv1a(1, "a\r\nb\r\n".bytes()), fnv1a(1, "a\nb\n".bytes()));
    }

    #[test]
    fn items_end_at_their_closing_brace_or_semicolon() {
        let source = "const A: u8 = 1;\nfn f() {\n    let s = \"}{\";\n    let c = '}';\n    // }\n    if x { y }\n}\nfn g() {}\n";
        assert_eq!(
            item_spans_starting_with(source, "const A"),
            ["const A: u8 = 1;"]
        );
        assert_eq!(
            item_spans_starting_with(source, "fn f("),
            ["fn f() {\n    let s = \"}{\";\n    let c = '}';\n    // }\n    if x { y }\n}"]
        );
        assert!(item_spans_starting_with(source, "fn missing").is_empty());
    }
}
