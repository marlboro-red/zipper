use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

const MANIFEST: &str = "verification/coverage.txt";
const UNCOVERED: &str = "verification/uncovered.txt";

struct Verification {
    name: String,
    skill: String,
    fingerprint: String,
    full_checks: Vec<String>,
    items: Vec<(String, String)>,
    item_end: ItemEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemEnd {
    FirstSemicolon,
    SemicolonOutsideBrackets,
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
                item_end: ItemEnd::FirstSemicolon,
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

fn end_of_braced_body_or_semicolon(text: &str, end: ItemEnd) -> usize {
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
fn fnv1a(mut hash: u64, bytes: impl Iterator<Item = u8>) -> u64 {
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
    fail_on_harnesses_not_run_exactly_once(root)?;
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

struct KaniCheck<'a> {
    base: Vec<&'a str>,
    filters: Vec<&'a str>,
    exact: bool,
}

fn kani_check(check: &str) -> Option<KaniCheck<'_>> {
    let mut words = check.split_whitespace();
    if (words.next(), words.next()) != (Some("cargo"), Some("kani")) {
        return None;
    }
    let mut parsed = KaniCheck {
        base: vec!["cargo", "kani"],
        filters: Vec::new(),
        exact: false,
    };
    while let Some(word) = words.next() {
        match word {
            "--harness" => parsed.filters.extend(words.next()),
            "--exact" => parsed.exact = true,
            other => parsed.base.push(other),
        }
    }
    Some(parsed)
}

impl KaniCheck<'_> {
    fn runs(&self, harness: &str, module_is_test_only: bool) -> bool {
        if module_is_test_only && !self.base.contains(&"--tests") {
            return false;
        }
        self.filters.is_empty()
            || self.filters.iter().any(|filter| {
                if self.exact {
                    harness == *filter
                } else {
                    harness.contains(filter)
                }
            })
    }
}

fn proof_harnesses(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut after_proof_attribute = false;
    for line in source.lines().map(str::trim) {
        if line == "#[kani::proof]" {
            after_proof_attribute = true;
        } else if after_proof_attribute && !line.starts_with("#[") {
            if let Some(rest) = line.strip_prefix("fn ") {
                let end = rest.find(['(', '<']).unwrap_or(rest.len());
                found.push(rest[..end].to_owned());
            }
            after_proof_attribute = false;
        }
    }
    found
}

fn test_only_modules(lib: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = lib.lines().map(str::trim).collect();
    lines
        .windows(2)
        .filter(|pair| pair[0] == "#[cfg(test)]")
        .filter_map(|pair| {
            let declaration = pair[1].strip_prefix("pub ").unwrap_or(pair[1]);
            let name = declaration.strip_prefix("mod ")?.strip_suffix(';')?;
            Some(name.to_owned())
        })
        .collect()
}

fn harness_module(harness: &str) -> Option<&str> {
    harness.split_once("::proofs::").map(|(module, _)| module)
}

fn module_source(module: &str) -> String {
    format!("src/{}.rs", module.replace("::", "/"))
}

fn harnesses_not_run_exactly_once(
    checks: &[KaniCheck],
    proof_files: &[String],
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<Vec<String>> {
    let test_only = test_only_modules(&source_of("src/lib.rs")?);
    let mut problems = Vec::new();
    let mut modules = BTreeSet::new();
    for check in checks.iter().filter(|check| check.exact) {
        for name in &check.filters {
            let known = match (harness_module(name), name.rsplit_once("::")) {
                (Some(module), Some((_, bare))) => {
                    modules.insert(module);
                    proof_harnesses(&source_of(&module_source(module))?).contains(&bare.to_owned())
                }
                _ => false,
            };
            if !known {
                problems.push(format!(
                    "{MANIFEST} runs {name}, which is no #[kani::proof] in a `proofs` module"
                ));
            }
        }
    }
    for path in proof_files {
        let module = path
            .trim_start_matches("src/")
            .trim_end_matches(".rs")
            .replace('/', "::");
        let top = module.split("::").next().unwrap_or(&module);
        let named_exactly = modules.contains(module.as_str());
        for bare in proof_harnesses(&source_of(path)?) {
            let harness = format!("{module}::proofs::{bare}");
            let runs = checks
                .iter()
                .filter(|check| check.runs(&harness, test_only.contains(top)))
                .count();
            if named_exactly && runs != 1 {
                problems.push(format!(
                    "{harness} is run by {runs} full-checks in {MANIFEST}; give it exactly one `cargo kani ... --harness {harness} --exact`"
                ));
            } else if runs == 0 {
                problems.push(format!(
                    "{harness} is run by no full-check in {MANIFEST}; add a `--harness` filter that matches it"
                ));
            }
        }
    }
    Ok(problems)
}

fn fail_on_harnesses_not_run_exactly_once(root: &Path) -> Result<()> {
    let all = manifest(root)?;
    let checks: Vec<KaniCheck> = all
        .iter()
        .flat_map(|verification| &verification.full_checks)
        .filter_map(|check| kani_check(check))
        .collect();
    let mut proof_files = Vec::new();
    for path in sources(root)? {
        if !proof_harnesses(&read_source(root, &path)?).is_empty() {
            proof_files.push(path);
        }
    }
    let problems =
        harnesses_not_run_exactly_once(&checks, &proof_files, |path| read_source(root, path))?;
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    Ok(())
}

fn read_source(root: &Path, path: &str) -> Result<String> {
    fs::read_to_string(root.join(path)).with_context(|| format!("cannot read {path}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lines {
    first: usize,
    last: usize,
}

impl Lines {
    fn contains(self, line: usize) -> bool {
        (self.first..=self.last).contains(&line)
    }
}

struct SourceLines<'a> {
    source: &'a str,
    code: String,
    lines: Vec<&'a str>,
    offsets: Vec<usize>,
}

fn blank_strings_and_comments(source: &str) -> String {
    let mut bytes = source.as_bytes().to_vec();
    let mut index = 0;
    let blank_until = |bytes: &mut [u8], from: usize, to: usize| {
        for byte in &mut bytes[from..to.min(source.len())] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    let original = source.as_bytes();
    while index < original.len() {
        let start = index;
        match original[index] {
            b'/' if original.get(index + 1) == Some(&b'/') => {
                while index < original.len() && original[index] != b'\n' {
                    index += 1;
                }
            }
            b'"' => {
                index += 1;
                while index < original.len() && original[index] != b'"' {
                    index += if original[index] == b'\\' { 2 } else { 1 };
                }
                index += 1;
            }
            b'\'' if original.get(index + 2) == Some(&b'\'') => index += 3,
            b'\'' if original.get(index + 1) == Some(&b'\\') => {
                index += 2;
                while index < original.len() && original[index] != b'\'' {
                    index += 1;
                }
                index += 1;
            }
            _ => {
                index += 1;
                continue;
            }
        }
        blank_until(&mut bytes, start, index);
    }
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_owned())
}

impl<'a> SourceLines<'a> {
    fn new(source: &'a str) -> Self {
        let mut offsets = vec![0];
        let mut lines = Vec::new();
        for line in source.split_inclusive('\n') {
            lines.push(line.trim_end_matches(['\n', '\r']));
            offsets.push(offsets.last().copied().unwrap_or(0) + line.len());
        }
        SourceLines {
            source,
            code: blank_strings_and_comments(source),
            lines,
            offsets,
        }
    }

    fn line(&self, number: usize) -> &'a str {
        self.lines.get(number - 1).copied().unwrap_or_default()
    }

    fn item_at(&self, first: usize) -> Lines {
        let rest = &self.source[self.offsets[first - 1]..];
        let body =
            &rest[..end_of_braced_body_or_semicolon(rest, ItemEnd::SemicolonOutsideBrackets)];
        Lines {
            first,
            last: first + body.trim_end_matches('\n').matches('\n').count(),
        }
    }

    fn items_where(&self, starts: impl Fn(&str) -> bool) -> Vec<Lines> {
        (1..=self.lines.len())
            .filter(|&number| starts(self.line(number)))
            .map(|number| self.item_at(number))
            .collect()
    }

    fn code(&self, span: Lines) -> &str {
        &self.code[self.offsets[span.first - 1]..self.offsets[span.last]]
    }

    fn opening_for(&self, first: usize, name: &str) -> String {
        let line = self.line(first).trim_end();
        let unique =
            |opening: &str| self.lines.iter().filter(|l| l.starts_with(opening)).count() == 1;
        line.find(name)
            .and_then(|at| line.get(..at + name.len() + 1))
            .filter(|opening| !opening.ends_with(' ') && unique(opening))
            .unwrap_or(line)
            .to_owned()
    }
}

fn innermost(spans: &[Lines], line: usize) -> Option<Lines> {
    spans
        .iter()
        .filter(|span| span.contains(line))
        .max_by_key(|span| span.first)
        .copied()
}

fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
}

fn without_visibility(line: &str) -> impl Iterator<Item = &str> {
    line.split_whitespace()
        .skip_while(|word| word.starts_with("pub"))
}

fn definition_name(line: &str) -> Option<&str> {
    let mut words = without_visibility(line);
    if !matches!(
        words.next()?,
        "const" | "static" | "struct" | "enum" | "type" | "union" | "trait"
    ) {
        return None;
    }
    identifiers(words.next()?)
        .next()
        .filter(|name| *name != "fn")
}

fn function_name(line: &str) -> Option<&str> {
    let mut words = without_visibility(line).skip_while(|word| matches!(*word, "const" | "unsafe"));
    if words.next()? != "fn" {
        return None;
    }
    identifiers(words.next()?).next()
}

fn reach_not_listed(
    items: &[(String, String)],
    reached: &BTreeSet<(String, usize)>,
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<BTreeSet<String>> {
    let mut missing = BTreeSet::new();
    let paths: BTreeSet<&String> = reached.iter().map(|(path, _)| path).collect();
    for path in paths {
        let source = source_of(path)?;
        let file = SourceLines::new(&source);
        let listed: Vec<Lines> = items
            .iter()
            .filter(|(item_path, _)| item_path == path)
            .flat_map(|(_, opening)| file.items_where(|line| line.starts_with(opening.as_str())))
            .collect();
        let covered = |line: usize| listed.iter().any(|span| span.contains(line));
        let functions = file.items_where(|line| function_name(line).is_some());
        let impls = file.items_where(|line| line.trim_start().starts_with("impl"));
        let mut definitions: BTreeMap<&str, Vec<Lines>> = BTreeMap::new();
        for number in 1..=file.lines.len() {
            if let Some(name) = definition_name(file.line(number)) {
                definitions
                    .entry(name)
                    .or_default()
                    .push(file.item_at(number));
            }
        }
        let mut names: Vec<&str> = Vec::new();
        for &(_, line) in reached
            .iter()
            .filter(|(reached_path, _)| reached_path == path)
        {
            let function = innermost(&functions, line);
            if !covered(line) {
                let opening = function
                    .and_then(|span| {
                        let name = function_name(file.line(span.first))?;
                        Some(file.opening_for(span.first, &format!("fn {name}")))
                    })
                    .unwrap_or_else(|| file.line(line).trim_end().to_owned());
                missing.insert(format!("item = {path} | {opening}"));
            }
            if let Some(span) = function {
                names.extend(identifiers(file.code(span)));
            }
            if let Some(span) = innermost(&impls, line) {
                names.extend(identifiers(file.code(Lines {
                    first: span.first,
                    last: span.first,
                })));
            }
        }
        let mut seen = BTreeSet::new();
        while let Some(name) = names.pop() {
            if !seen.insert(name) {
                continue;
            }
            for &span in definitions.get(name).into_iter().flatten() {
                if !covered(span.first) {
                    missing.insert(format!(
                        "item = {path} | {}",
                        file.opening_for(span.first, name)
                    ));
                }
                names.extend(identifiers(file.code(span)));
            }
        }
    }
    Ok(missing)
}

fn kani_tool(program: &str) -> Result<PathBuf> {
    let version = Command::new("cargo").args(["kani", "--version"]).output()?;
    let version = String::from_utf8_lossy(&version.stdout);
    let number = version
        .split_whitespace()
        .find(|word| word.starts_with(|c: char| c.is_ascii_digit()))
        .context("`cargo kani --version` printed no version")?;
    let home = env::var_os("HOME").context("HOME is not set")?;
    let tool = Path::new(&home).join(format!(".kani/kani-{number}/bin/{program}"));
    ensure!(tool.is_file(), "no {program} at {}", tool.display());
    Ok(tool)
}

fn mangled_path(harness: &str) -> String {
    harness
        .split("::")
        .map(|segment| format!("{}{segment}", segment.len()))
        .collect()
}

fn newest_goto_program(folder: &Path, harness: &str, since: SystemTime) -> Result<PathBuf> {
    let suffix = format!("{}.out", mangled_path(harness));
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let mut folders = vec![folder.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder)? {
            let path = entry?.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.to_string_lossy().ends_with(&suffix) {
                let modified = path.metadata()?.modified()?;
                if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
                    newest = Some((modified, path));
                }
            }
        }
    }
    let (modified, path) =
        newest.with_context(|| format!("Kani wrote no goto program for {harness}"))?;
    ensure!(
        modified >= since,
        "the goto program for {harness} predates this codegen run: {}",
        path.display()
    );
    Ok(path)
}

fn reached_lines(root: &Path, eloc: &str) -> BTreeSet<(String, usize)> {
    let prefix = format!("{}/", root.display());
    eloc.lines()
        .filter_map(|line| {
            let (file, number) = line.trim().rsplit_once(':')?;
            let relative = file.strip_prefix(&prefix).unwrap_or(file);
            Some((relative.to_owned(), number.parse().ok()?))
        })
        .filter(|(path, number)| path.starts_with("src/") && *number > 0)
        .collect()
}

pub fn fail_on_unlisted_kani_reach(root: &Path, name: &str) -> Result<()> {
    let all = manifest(root)?;
    let verification = all
        .iter()
        .find(|verification| verification.name == name)
        .with_context(|| format!("no verification named {name:?} in {MANIFEST}"))?;
    let checks: Vec<KaniCheck> = verification
        .full_checks
        .iter()
        .filter_map(|check| kani_check(check))
        .collect();
    ensure!(
        !checks.is_empty() && checks.iter().all(|check| check.exact),
        "{name}: every Kani full-check must name its harnesses with --exact"
    );
    ensure!(
        verification.item_end == ItemEnd::SemicolonOutsideBrackets,
        "{name}: set `item-end = semicolon outside brackets`, so each item it checks is fingerprinted whole"
    );
    let goto_instrument = kani_tool("goto-instrument")?;
    let mut problems = BTreeSet::new();
    for check in &checks {
        let started = SystemTime::now();
        let mut codegen = Command::new(check.base[0]);
        codegen.args(&check.base[1..]).arg("--only-codegen");
        for harness in &check.filters {
            codegen.args(["--harness", harness, "--exact"]);
        }
        let codegen = codegen.current_dir(root).output()?;
        ensure!(
            codegen.status.success(),
            "{name}: Kani codegen failed:\n{}{}",
            String::from_utf8_lossy(&codegen.stdout),
            String::from_utf8_lossy(&codegen.stderr)
        );
        for harness in &check.filters {
            let program = newest_goto_program(&root.join("target/kani"), harness, started)?;
            let eloc = Command::new(&goto_instrument)
                .arg("--list-eloc")
                .arg(&program)
                .output()?;
            ensure!(
                eloc.status.success(),
                "goto-instrument failed on {}",
                program.display()
            );
            let reached = reached_lines(root, &String::from_utf8_lossy(&eloc.stdout));
            ensure!(
                !reached.is_empty(),
                "{harness} reaches no line under src/, so its goto program was not read"
            );
            println!(
                "{name}: {harness} reaches {} lines under src/",
                reached.len()
            );
            problems.extend(reach_not_listed(&verification.items, &reached, |path| {
                read_source(root, path)
            })?);
        }
    }
    ensure!(
        problems.is_empty(),
        "{name}: its proofs reach code that no item of the section lists; add\n  {}",
        problems.into_iter().collect::<Vec<_>>().join("\n  ")
    );
    println!("{name}: every line its proofs reach lies inside its items");
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

    const PROOFS: &str = "mod proofs {\n    #[kani::proof]\n    #[kani::unwind(3)]\n    fn first() {}\n\n    #[kani::proof]\n    fn second<const N: usize>() {}\n\n    fn helper() {}\n}\n";

    fn sources<'a>(files: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Result<String> + 'a {
        |path| {
            files
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, text)| (*text).to_owned())
                .with_context(|| format!("no {path}"))
        }
    }

    #[test]
    fn every_harness_of_a_module_named_exactly_runs_in_exactly_one_check() {
        assert_eq!(proof_harnesses(PROOFS), ["first", "second"]);
        let files = [
            ("src/lib.rs", "#[cfg(test)]\nmod m;\nmod other;\n"),
            ("src/m.rs", PROOFS),
            ("src/other.rs", PROOFS),
        ];
        let proof_files = ["src/m.rs".to_owned(), "src/other.rs".to_owned()];
        let problems = |checks: &[&str]| {
            let parsed: Vec<KaniCheck> = checks.iter().filter_map(|c| kani_check(c)).collect();
            harnesses_not_run_exactly_once(&parsed, &proof_files, sources(&files)).unwrap()
        };
        let first = "cargo kani --tests --harness m::proofs::first --exact";
        let second = "cargo kani --tests --harness m::proofs::second --exact";
        let other = "cargo kani --harness other::proofs::";
        assert!(problems(&[first, second, other, "cargo test --lib m"]).is_empty());
        assert!(problems(&[first, second, "cargo kani"]).is_empty());
        assert!(problems(&[first, second, other, other]).is_empty());
        let unrun = problems(&[first, second, "cargo kani --harness elsewhere::proofs::"]);
        assert_eq!(unrun.len(), 2);
        assert!(unrun[0].starts_with("other::proofs::first is run by no full-check"));
        let missing = problems(&[first, other]);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("m::proofs::second is run by 0"));
        let twice = problems(&[
            first,
            second,
            other,
            "cargo kani --tests --harness m::proofs::",
        ]);
        assert_eq!(twice.len(), 2);
        let unknown = problems(&[
            first,
            second,
            other,
            "cargo kani --tests --harness m::proofs::helper --exact",
        ]);
        assert_eq!(unknown.len(), 1);
        assert!(unknown[0].contains("m::proofs::helper, which is no #[kani::proof]"));
    }

    const REACHED: &str = "const LIMIT: u8 = BASE + 1;\nconst BASE: u8 = 1;\nconst UNUSED: u8 = 2;\nstruct Coder {\n    table: [u8; WIDTH],\n}\nconst WIDTH: usize = 4;\nimpl Coder {\n    fn step(&mut self) -> u8 {\n        LIMIT // \"UNUSED\"\n    }\n\n    fn other(&self) {}\n}\nfn free() {\n    let _ = || 1;\n}\n";

    fn unlisted(openings: &[&str], lines: &[usize]) -> Vec<String> {
        let items: Vec<(String, String)> = openings
            .iter()
            .map(|opening| ("src/m.rs".to_owned(), (*opening).to_owned()))
            .collect();
        let reached = lines
            .iter()
            .map(|line| ("src/m.rs".to_owned(), *line))
            .collect();
        reach_not_listed(&items, &reached, sources(&[("src/m.rs", REACHED)]))
            .unwrap()
            .into_iter()
            .collect()
    }

    #[test]
    fn a_reached_line_needs_its_function_and_every_definition_it_names() {
        let all = ["const LIMIT", "const BASE", "struct Coder", "const WIDTH"];
        assert!(unlisted(&[&all[..], &["    fn step("]].concat(), &[10]).is_empty());
        assert!(unlisted(&[&all[..], &["impl Coder {"]].concat(), &[10]).is_empty());
        assert_eq!(unlisted(&all, &[10]), ["item = src/m.rs |     fn step("]);
        assert_eq!(
            unlisted(&["    fn step(", "struct Coder", "const WIDTH"], &[10]),
            [
                "item = src/m.rs | const BASE:",
                "item = src/m.rs | const LIMIT:"
            ]
        );
        assert_eq!(
            unlisted(&["    fn step(", "const LIMIT", "const BASE"], &[10]),
            [
                "item = src/m.rs | const WIDTH:",
                "item = src/m.rs | struct Coder {"
            ]
        );
        assert_eq!(unlisted(&["fn free("], &[16]), Vec::<String>::new());
        assert_eq!(unlisted(&[], &[16]), ["item = src/m.rs | fn free("]);
    }

    #[test]
    fn eloc_lines_keep_only_sources_inside_the_workspace() {
        let root = Path::new("/w");
        let eloc = "Reading GOTO program from 'x.out'\nsrc/m.rs:3\n/w/src/m.rs:4\nsrc/m.rs:10\nlibrary/core/src/num.rs:1\n/rust/src/lib.rs:2\n/w/tests/t.rs:5\n";
        let reached = reached_lines(root, eloc);
        let expected: BTreeSet<(String, usize)> = [3, 4, 10]
            .into_iter()
            .map(|line| ("src/m.rs".to_owned(), line))
            .collect();
        assert_eq!(reached, expected);
        assert_eq!(mangled_path("lzma::proofs::tree"), "4lzma6proofs4tree");
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
