use anyhow::{Context, Result, bail, ensure};
use std::{fs, path::Path, process::Command};

const MANIFEST: &str = "verification/coverage.txt";

struct Verification {
    name: String,
    skill: String,
    fingerprint: String,
    full_check: Option<String>,
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
                full_check: None,
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
            "full-check" => entry.full_check = Some(value.to_owned()),
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
            for byte in path.bytes().chain(text.bytes()) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    Ok(format!("{hash:016x}"))
}

pub fn fail_on_stale_fingerprints(root: &Path) -> Result<()> {
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

pub fn record_checked_fingerprint(root: &Path, name: &str) -> Result<()> {
    let all = manifest(root)?;
    let verification = all
        .iter()
        .find(|verification| verification.name == name)
        .with_context(|| format!("no verification named {name:?} in {MANIFEST}"))?;
    if let Some(check) = &verification.full_check {
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
        let status = command.current_dir(root).status()?;
        ensure!(
            status.success(),
            "{name}: the full check failed; nothing was synced"
        );
    }
    if verification.full_check.is_none() {
        println!(
            "{name}: recording the current code as checked; {} must have passed on it",
            verification.skill
        );
    }
    let current = fingerprint(root, verification)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
