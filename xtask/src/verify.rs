use anyhow::{Context, Result, bail, ensure};
use std::{fs, path::Path, process::Command};

pub(crate) const MANIFEST: &str = "verification/coverage.txt";
const UNCOVERED: &str = "verification/uncovered.txt";

pub(crate) struct Verification {
    pub(crate) name: String,
    skill: String,
    fingerprint: String,
    pub(crate) full_checks: Vec<String>,
    pub(crate) items: Vec<(String, String)>,
    pub(crate) item_end: ItemEnd,
    pub(crate) goto_fingerprints: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemEnd {
    FirstSemicolon,
    SemicolonOutsideBrackets,
}

pub(crate) fn manifest(root: &Path) -> Result<Vec<Verification>> {
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
                item_end: ItemEnd::FirstSemicolon,
                goto_fingerprints: Vec::new(),
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
            "goto-fingerprint" => {
                let (harness, hash) = value.split_once(' ').with_context(|| {
                    format!("{MANIFEST}:{number}: expected `goto-fingerprint = HARNESS HASH`")
                })?;
                entry
                    .goto_fingerprints
                    .push((harness.to_owned(), hash.to_owned()));
            }
            "item-end" if value == "semicolon outside brackets" => {
                entry.item_end = ItemEnd::SemicolonOutsideBrackets;
            }
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

fn item_spans_starting_with<'a>(source: &'a str, opening: &str, end: ItemEnd) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        if line.starts_with(opening) {
            found.push(
                &source[offset..offset + end_of_braced_body_or_semicolon(&source[offset..], end)],
            );
        }
        offset += line.len();
    }
    found
}

pub(crate) fn end_of_braced_body_or_semicolon(text: &str, end: ItemEnd) -> usize {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut brackets = 0usize;
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
            b'[' | b'(' if depth == 0 => brackets += 1,
            b']' | b')' if depth == 0 => brackets = brackets.saturating_sub(1),
            b';' if depth == 0 && (brackets == 0 || end == ItemEnd::FirstSemicolon) => {
                return index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    bytes.len()
}

// EXTERNAL(Git on Windows): checkouts can turn LF into CRLF.
pub(crate) fn fnv1a(mut hash: u64, bytes: impl Iterator<Item = u8>) -> u64 {
    for byte in bytes.filter(|&byte| byte != b'\r') {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn fingerprint(root: &Path, verification: &Verification) -> Result<String> {
    fingerprint_of(verification, |path| {
        fs::read_to_string(root.join(path)).with_context(|| format!("cannot read {path}"))
    })
}

fn fingerprint_of(
    verification: &Verification,
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<String> {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (path, opening) in &verification.items {
        let source = source_of(path)?;
        let texts = item_spans_starting_with(&source, opening, verification.item_end);
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
    crate::kani::fail_on_harnesses_not_run_as_listed(root)?;
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
    let goto = crate::kani::goto_fingerprints(root, verification)?;
    ensure!(
        fingerprint(root, verification)? == checked,
        "{name}: the covered code changed while the goto programs were fingerprinted; nothing was synced"
    );
    let text = fs::read_to_string(root.join(MANIFEST))?;
    let mut in_section = false;
    let mut updated = String::new();
    for line in text.lines() {
        if line.starts_with('[') {
            in_section = line == format!("[{name}]");
        }
        if in_section && line.starts_with("goto-fingerprint = ") {
            continue;
        }
        if in_section && line.starts_with("fingerprint = ") {
            updated.push_str(&format!("fingerprint = {current}\n"));
            for (harness, hash) in &goto {
                updated.push_str(&format!("goto-fingerprint = {harness} {hash}\n"));
            }
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

pub(crate) fn read_source(root: &Path, path: &str) -> Result<String> {
    fs::read_to_string(root.join(path)).with_context(|| format!("cannot read {path}"))
}

pub(crate) fn sources(root: &Path) -> Result<Vec<String>> {
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
    use std::{collections::BTreeSet, path::PathBuf};

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
            item_spans_starting_with(source, "const A", ItemEnd::FirstSemicolon),
            ["const A: u8 = 1;"]
        );
        assert_eq!(
            item_spans_starting_with(source, "fn f(", ItemEnd::FirstSemicolon),
            ["fn f() {\n    let s = \"}{\";\n    let c = '}';\n    // }\n    if x { y }\n}"]
        );
        assert!(item_spans_starting_with(source, "fn missing", ItemEnd::FirstSemicolon).is_empty());
        let bracketed =
            "fn t<const N: usize>(x: [u8; N]) -> u8 {\n    x[0]\n}\nconst M: [u8; 2] = [1; 2];\n";
        let outside = ItemEnd::SemicolonOutsideBrackets;
        assert_eq!(
            item_spans_starting_with(bracketed, "fn t<", outside),
            ["fn t<const N: usize>(x: [u8; N]) -> u8 {\n    x[0]\n}"]
        );
        assert_eq!(
            item_spans_starting_with(bracketed, "const M", outside),
            ["const M: [u8; 2] = [1; 2];"]
        );
        assert_eq!(
            item_spans_starting_with(bracketed, "fn t<", ItemEnd::FirstSemicolon),
            ["fn t<const N: usize>(x: [u8;"]
        );
    }

    #[test]
    #[ignore = "prints the items a first-semicolon section cuts short, to move it to the bracket-aware end"]
    fn print_items_cut_short_at_a_bracketed_semicolon() {
        let root = workspace();
        for verification in manifest(&root).unwrap() {
            if verification.item_end != ItemEnd::FirstSemicolon {
                continue;
            }
            for (path, opening) in &verification.items {
                let source = read_source(&root, path).unwrap();
                let cut = item_spans_starting_with(&source, opening, ItemEnd::FirstSemicolon);
                let whole =
                    item_spans_starting_with(&source, opening, ItemEnd::SemicolonOutsideBrackets);
                if cut != whole {
                    println!("{}: {path} | {opening}", verification.name);
                }
            }
        }
    }

    fn workspace() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives inside the workspace")
            .to_path_buf()
    }

    fn lzma_sections_made_stale_by(old: &str, new: &str) -> BTreeSet<String> {
        let root = workspace();
        let original = read_source(&root, "src/lzma.rs").unwrap();
        assert_eq!(original.matches(old).count(), 1, "{old:?} must occur once");
        let edited = original.replacen(old, new, 1);
        let with = |lzma: &str| {
            let lzma = lzma.to_owned();
            let root = root.clone();
            move |path: &str| {
                if path == "src/lzma.rs" {
                    Ok(lzma.clone())
                } else {
                    read_source(&root, path)
                }
            }
        };
        manifest(&root)
            .unwrap()
            .into_iter()
            .filter(|verification| {
                verification
                    .items
                    .iter()
                    .any(|(path, _)| path == "src/lzma.rs")
            })
            .filter(|verification| {
                fingerprint_of(verification, with(&original)).unwrap()
                    != fingerprint_of(verification, with(&edited)).unwrap()
            })
            .map(|verification| verification.name)
            .collect()
    }

    #[test]
    fn each_lzma_edit_makes_stale_only_the_groups_whose_proofs_reach_it() {
        let kani = [
            "lzma-bits",
            "lzma-trees",
            "lzma-lengths",
            "lzma-literal",
            "lzma-matched-literal",
            "lzma-distances",
        ];
        let cases: [(&str, &str, &[&str]); 14] = [
            (
                "None if range.complete() => return Err(data_error(\"truncated LZMA stream\")),",
                "None if range.complete() => return Err(data_error(\"cut LZMA stream\")),",
                &["lzma"],
            ),
            (
                "fn copy_in_chunks(&mut self, source: usize, run: usize) {",
                "fn copy_in_chunks(&mut self, source: usize, run: usize) {\n        let _ = run;",
                &["lzma"],
            ),
            (
                "let mut matched = usize::from(match_byte) << 1;",
                "let mut matched = usize::from(match_byte) << 2;",
                &["lzma", "lzma-matched-literal"],
            ),
            (
                "let zero = probabilities[symbol << 1];",
                "let zero = probabilities[symbol << 2];",
                &["lzma", "lzma-literal"],
            ),
            (
                "let byte = u32::from(byte);",
                "let byte = u32::from(byte) % 225;",
                &["lzma", "lzma-literal"],
            ),
            (
                "(index << 1 | bit as usize) as u32 - (1 << bits)",
                "(index << 1 | bit as usize) as u32 - (1 << (bits - 1))",
                &["lzma", "lzma-trees", "lzma-lengths"],
            ),
            (
                "16 + range.tree(&mut self.high) as usize",
                "17 + range.tree(&mut self.high) as usize",
                &["lzma", "lzma-lengths"],
            ),
            (
                "let base = distance - slot;",
                "let base = distance - slot + 1;",
                &["lzma", "lzma-distances"],
            ),
            (
                "self.range >>= 1;",
                "self.range >>= 2;",
                &["lzma", "lzma-bits", "lzma-distances"],
            ),
            (
                "self.taken - (self.end - self.start) as u64",
                "self.taken - (self.end - self.start) as u64 + 1",
                &["lzma", "lzma-input"],
            ),
            (
                "assert!(length < LENGTH_SYMBOLS);",
                "assert!(length <= LENGTH_SYMBOLS);",
                &["lzma-lengths"],
            ),
            (
                "let toward_zero = probability >> MOVE_BITS;",
                "let toward_zero = probability >> (MOVE_BITS + 1);",
                &["lzma", kani[0], kani[1], kani[2], kani[3], kani[4], kani[5]],
            ),
            (
                "const TOP: u32 = 1 << 24;",
                "const TOP: u32 = 1 << 23;",
                &["lzma", kani[0], kani[1], kani[2], kani[3], kani[4], kani[5]],
            ),
            (
                "const SYMBOL_INPUT_MAX: usize = 64;",
                "const SYMBOL_INPUT_MAX: usize = 65;",
                &["lzma", "lzma-input"],
            ),
        ];
        for (old, new, expected) in cases {
            let expected: BTreeSet<String> =
                expected.iter().map(|name| (*name).to_owned()).collect();
            assert_eq!(
                lzma_sections_made_stale_by(old, new),
                expected,
                "editing {old:?}"
            );
        }
    }
}
