use crate::verify::{
    ItemEnd, MANIFEST, end_of_braced_body_or_semicolon, manifest, read_source, sources,
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

pub(crate) struct KaniCheck<'a> {
    base: Vec<&'a str>,
    filters: Vec<&'a str>,
    exact: bool,
}

pub(crate) fn kani_check(check: &str) -> Option<KaniCheck<'_>> {
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
            other => match other.strip_prefix("--harness=") {
                Some(filter) => parsed.filters.push(filter),
                None => parsed.base.push(other),
            },
        }
    }
    Some(parsed)
}

impl KaniCheck<'_> {
    fn runs(&self, harness: &Harness) -> bool {
        if harness.needs_tests && !self.base.contains(&"--tests") {
            return false;
        }
        self.filters.is_empty()
            || self.filters.iter().any(|filter| {
                if self.exact {
                    harness.path == *filter
                } else {
                    harness.path.contains(filter)
                }
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Harness {
    path: String,
    file: String,
    line: usize,
    needs_tests: bool,
}

fn file_module(path: &str) -> Option<String> {
    let inner = path.strip_prefix("src/")?.strip_suffix(".rs")?;
    if inner == "lib" {
        return Some(String::new());
    }
    if inner == "main" || inner.starts_with("bin/") {
        return None;
    }
    let inner = inner.strip_suffix("/mod").unwrap_or(inner);
    Some(inner.replace('/', "::"))
}

fn module_files(module: &str) -> Vec<String> {
    if module.is_empty() {
        return vec!["src/lib.rs".to_owned()];
    }
    let folder = module.replace("::", "/");
    vec![format!("src/{folder}.rs"), format!("src/{folder}/mod.rs")]
}

fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("::")
}

fn unspaced(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

fn needs_tests_or_unsupported(attributes: &[String]) -> std::result::Result<bool, String> {
    let mut needs_tests = false;
    for attribute in attributes {
        let inner = attribute
            .strip_prefix("#![")
            .or_else(|| attribute.strip_prefix("#["))
            .and_then(|rest| rest.strip_suffix(']'))
            .unwrap_or(attribute);
        if inner.starts_with("cfg(") {
            match inner {
                "cfg(kani)" => {}
                "cfg(test)" => needs_tests = true,
                _ => return Err(attribute.clone()),
            }
        } else if inner.starts_with("cfg_attr(") && inner.contains("kani::proof") {
            return Err(attribute.clone());
        }
    }
    Ok(needs_tests)
}

fn bracket_balance(text: &str) -> isize {
    text.matches('[').count() as isize - text.matches(']').count() as isize
}

struct Scanned {
    harnesses: Vec<(String, usize, Vec<String>)>,
    declarations: BTreeMap<String, Vec<String>>,
    problems: Vec<String>,
}

fn scan_items(path: &str, module: &str, code: &str) -> Scanned {
    let mut scanned = Scanned {
        harnesses: Vec::new(),
        declarations: BTreeMap::new(),
        problems: Vec::new(),
    };
    let mut depth = 0usize;
    let mut frames: Vec<(String, usize, Vec<String>)> = Vec::new();
    let mut file_attributes: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut open_attribute: Option<String> = None;
    for (index, line) in code.lines().enumerate() {
        let number = index + 1;
        let trimmed = line.trim();
        let attribute_text = match open_attribute.take() {
            Some(mut text) => {
                text.push_str(trimmed);
                Some(text)
            }
            None if trimmed.starts_with("#[") || trimmed.starts_with("#![") => {
                Some(trimmed.to_owned())
            }
            None => None,
        };
        if let Some(text) = attribute_text {
            if bracket_balance(&text) > 0 {
                open_attribute = Some(text);
            } else if text.starts_with("#![") {
                match frames.last_mut() {
                    Some(frame) => frame.2.push(unspaced(&text)),
                    None => file_attributes.push(unspaced(&text)),
                }
            } else {
                pending.push(unspaced(&text));
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let words: Vec<&str> = trimmed
            .split_whitespace()
            .skip_while(|word| word.starts_with("pub"))
            .collect();
        let proof = pending
            .iter()
            .any(|attribute| attribute == "#[kani::proof]");
        for attribute in &pending {
            if attribute.contains("kani::proof_for_contract")
                || (attribute.starts_with("#[cfg_attr(") && attribute.contains("kani::proof"))
            {
                scanned.problems.push(format!(
                    "{path}:{number}: {attribute} is not supported; use #[kani::proof]"
                ));
            }
        }
        let function = words
            .iter()
            .skip_while(|word| matches!(**word, "const" | "unsafe" | "async" | "extern"))
            .collect::<Vec<_>>();
        if words.first() == Some(&"mod") {
            let name = words
                .get(1)
                .map(|word| identifiers(word).next().unwrap_or_default());
            let name = name.unwrap_or_default().to_owned();
            if trimmed.ends_with(';') {
                scanned.declarations.insert(name, pending.clone());
            } else if trimmed.contains('{') {
                frames.push((name, depth, pending.clone()));
            }
        }
        let item_depth = frames.last().map_or(0, |frame| frame.1 + 1);
        if function.first() == Some(&&"fn") && proof && depth != item_depth {
            scanned.problems.push(format!(
                "{path}:{number}: a #[kani::proof] inside another item is not supported, because the gate cannot name it"
            ));
        } else if function.first() == Some(&&"fn") && proof {
            let name = function
                .get(1)
                .and_then(|word| identifiers(word).next())
                .unwrap_or_default();
            let mut attributes = pending.clone();
            for frame in frames.iter().rev() {
                attributes.extend(frame.2.iter().cloned());
            }
            attributes.extend(file_attributes.iter().cloned());
            let names: Vec<&str> = std::iter::once(module)
                .chain(frames.iter().map(|frame| frame.0.as_str()))
                .chain(std::iter::once(name))
                .collect();
            scanned.harnesses.push((joined(&names), number, attributes));
        } else if proof {
            scanned.problems.push(format!(
                "{path}:{number}: #[kani::proof] is not on a function"
            ));
        }
        pending.clear();
        for c in line.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        while frames.last().is_some_and(|frame| frame.1 >= depth) {
            frames.pop();
        }
    }
    scanned
}

pub(crate) fn discover_harnesses(
    files: &[String],
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<(Vec<Harness>, Vec<String>)> {
    let mut harnesses = Vec::new();
    let mut problems = Vec::new();
    let mut declared: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    let mut declarations_in = |file: &str| -> Result<BTreeMap<String, Vec<String>>> {
        if let Some(found) = declared.get(file) {
            return Ok(found.clone());
        }
        let found = match source_of(file) {
            Ok(source) => {
                let module = file_module(file).unwrap_or_default();
                scan_items(file, &module, &blank_strings_and_comments(&source)).declarations
            }
            Err(_) => BTreeMap::new(),
        };
        declared.insert(file.to_owned(), found.clone());
        Ok(found)
    };
    for path in files {
        let source = source_of(path)?;
        if !source.contains("kani::proof") {
            continue;
        }
        let code = blank_strings_and_comments(&source);
        if !code.contains("kani::proof") {
            continue;
        }
        let Some(module) = file_module(path) else {
            problems.push(format!(
                "{path} holds a Kani proof outside the library crate, which no check runs"
            ));
            continue;
        };
        let mut declaration = Vec::new();
        let mut parts: Vec<&str> = module.split("::").filter(|part| !part.is_empty()).collect();
        while let Some(last) = parts.pop() {
            let parent = parts.join("::");
            let mut found = None;
            for file in module_files(&parent) {
                if let Some(attributes) = declarations_in(&file)?.get(last) {
                    found = Some(attributes.clone());
                }
            }
            match found {
                Some(attributes) => declaration.extend(attributes),
                None => {
                    problems.push(format!(
                        "{path}: no `mod {last};` declares it in {}",
                        module_files(&parent).join(" or ")
                    ));
                    break;
                }
            }
        }
        let scanned = scan_items(path, &module, &code);
        problems.extend(scanned.problems);
        for (harness, line, mut attributes) in scanned.harnesses {
            attributes.extend(declaration.iter().cloned());
            match needs_tests_or_unsupported(&attributes) {
                Ok(needs_tests) => harnesses.push(Harness {
                    path: harness,
                    file: path.clone(),
                    line,
                    needs_tests,
                }),
                Err(attribute) => problems.push(format!(
                    "{harness} ({path}:{line}) sits under {attribute}; only #[cfg(kani)] and #[cfg(test)] are understood, so the gate cannot tell which check runs it"
                )),
            }
        }
    }
    Ok((harnesses, problems))
}

fn parent(path: &str) -> &str {
    path.rsplit_once("::").map_or("", |(parent, _)| parent)
}

fn harnesses_not_run_as_listed(checks: &[KaniCheck], harnesses: &[Harness]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut exact_modules = BTreeSet::new();
    for check in checks.iter().filter(|check| check.exact) {
        for name in &check.filters {
            if harnesses.iter().any(|harness| harness.path == *name) {
                exact_modules.insert(parent(name));
            } else {
                problems.push(format!(
                    "{MANIFEST} runs {name}, which is no #[kani::proof] in src/"
                ));
            }
        }
    }
    for harness in harnesses {
        let name = &harness.path;
        let runs = checks.iter().filter(|check| check.runs(harness)).count();
        if exact_modules.contains(parent(name)) && runs != 1 {
            problems.push(format!(
                "{name} is run by {runs} full-checks in {MANIFEST}; give it exactly one `cargo kani ... --harness {name} --exact`"
            ));
        } else if runs == 0 {
            problems.push(format!(
                "{name} ({}:{}) is run by no full-check in {MANIFEST}; add a `--harness` filter that matches it{}",
                harness.file,
                harness.line,
                if harness.needs_tests { " with --tests" } else { "" }
            ));
        }
    }
    problems
}

fn all_kani_checks(all: &[crate::verify::Verification]) -> Vec<KaniCheck<'_>> {
    all.iter()
        .flat_map(|verification| &verification.full_checks)
        .filter_map(|check| kani_check(check))
        .collect()
}

fn workspace_harnesses(root: &Path) -> Result<(Vec<Harness>, Vec<String>)> {
    discover_harnesses(&sources(root)?, |path| read_source(root, path))
}

pub(crate) fn fail_on_harnesses_not_run_as_listed(root: &Path) -> Result<()> {
    let all = manifest(root)?;
    let (harnesses, mut problems) = workspace_harnesses(root)?;
    problems.extend(harnesses_not_run_as_listed(
        &all_kani_checks(&all),
        &harnesses,
    ));
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    Ok(())
}

fn listed_names(json: &str, key: &str) -> Option<BTreeSet<String>> {
    let start = json.find(&format!("\"{key}\""))?;
    let rest = &json[start..];
    let open = rest.find('{')?;
    let mut depth = 0;
    let mut end = open;
    for (index, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = open + index;
                    break;
                }
            }
            _ => {}
        }
    }
    let object = &rest[open..=end];
    let mut names = BTreeSet::new();
    for array in object.split('[').skip(1) {
        let inside = array.split(']').next().unwrap_or_default();
        names.extend(inside.split('"').skip(1).step_by(2).map(str::to_owned));
    }
    Some(names)
}

pub(crate) fn fail_on_kani_list_mismatch(root: &Path) -> Result<()> {
    let (harnesses, mut problems) = workspace_harnesses(root)?;
    for tests in [false, true] {
        let folder = tempfile::tempdir()?;
        let mut list = Command::new("cargo");
        list.args(["kani", "--manifest-path"])
            .arg(root.join("Cargo.toml"));
        if tests {
            list.arg("--tests");
        }
        let output = list
            .args(["list", "--format", "json"])
            .current_dir(folder.path())
            .output()?;
        ensure!(
            output.status.success(),
            "`cargo kani list` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json = fs::read_to_string(folder.path().join("kani-list.json"))
            .context("`cargo kani list` wrote no kani-list.json")?;
        let listed = listed_names(&json, "standard-harnesses")
            .context("kani-list.json has no standard-harnesses")?;
        let contracts = listed_names(&json, "contract-harnesses").unwrap_or_default();
        let parsed: BTreeSet<String> = harnesses
            .iter()
            .filter(|harness| tests || !harness.needs_tests)
            .map(|harness| harness.path.clone())
            .collect();
        let flag = if tests { " --tests" } else { "" };
        for name in listed.difference(&parsed) {
            problems.push(format!(
                "`cargo kani{flag} list` names {name}, which the gate's harness parser misses"
            ));
        }
        for name in parsed.difference(&listed) {
            problems.push(format!(
                "the gate's harness parser finds {name}, which `cargo kani{flag} list` does not"
            ));
        }
        for name in contracts {
            problems.push(format!(
                "{name} is a contract harness, which the gate does not check"
            ));
        }
    }
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    println!(
        "kani list: Kani and the gate's parser agree on {} harnesses",
        harnesses.len()
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Lines {
    first: usize,
    last: usize,
}

impl Lines {
    fn contains(self, line: usize) -> bool {
        (self.first..=self.last).contains(&line)
    }
}

fn blank_strings_and_comments(source: &str) -> String {
    let original = source.as_bytes();
    let mut bytes = original.to_vec();
    let mut index = 0;
    while index < original.len() {
        let start = index;
        let at = |offset: usize| original.get(index + offset).copied();
        match original[index] {
            b'/' if at(1) == Some(b'/') => {
                while index < original.len() && original[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if at(1) == Some(b'*') => {
                index += 2;
                while index + 1 < original.len() && &original[index..index + 2] != b"*/" {
                    index += 1;
                }
                index += 2;
            }
            b'r' if (at(1) == Some(b'"') || at(1) == Some(b'#'))
                && (index == 0 || !original[index - 1].is_ascii_alphanumeric()) =>
            {
                let hashes = original[index + 1..]
                    .iter()
                    .take_while(|byte| **byte == b'#')
                    .count();
                if original.get(index + 1 + hashes) != Some(&b'"') {
                    index += 1;
                    continue;
                }
                let close: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                index += 2 + hashes;
                while index < original.len() && !original[index..].starts_with(&close) {
                    index += 1;
                }
                index += close.len();
            }
            b'"' => {
                index += 1;
                while index < original.len() && original[index] != b'"' {
                    index += if original[index] == b'\\' { 2 } else { 1 };
                }
                index += 1;
            }
            b'\'' if at(2) == Some(b'\'') => index += 3,
            b'\'' if at(1) == Some(b'\\') => {
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
        for byte in &mut bytes[start..index.min(original.len())] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| source.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Value,
    Type,
    Macro,
    ConstFunction,
    Function,
}

struct FileModel {
    source: String,
    code: String,
    offsets: Vec<usize>,
    listed: Vec<Lines>,
    functions: Vec<Lines>,
    impls: Vec<Lines>,
    definitions: BTreeMap<String, Vec<(Lines, Kind)>>,
    imports: BTreeMap<String, String>,
}

impl FileModel {
    fn new(source: String, openings: &[&str]) -> Self {
        let code = blank_strings_and_comments(&source);
        let mut offsets = vec![0];
        for line in source.split_inclusive('\n') {
            offsets.push(offsets.last().copied().unwrap_or(0) + line.len());
        }
        let mut model = FileModel {
            source,
            code,
            offsets,
            listed: Vec::new(),
            functions: Vec::new(),
            impls: Vec::new(),
            definitions: BTreeMap::new(),
            imports: BTreeMap::new(),
        };
        for number in 1..model.offsets.len() {
            let line = model.line(number);
            if openings.iter().any(|opening| line.starts_with(opening)) {
                model.listed.push(model.item_at(number));
            }
            let code_line = model.code_line(number).to_owned();
            if let Some((name, kind)) = definition(&code_line) {
                let span = model.item_at(number);
                if matches!(kind, Kind::Function | Kind::ConstFunction) {
                    model.functions.push(span);
                }
                model
                    .definitions
                    .entry(name.to_owned())
                    .or_default()
                    .push((span, kind));
            }
            if code_line.trim_start().starts_with("impl") {
                model.impls.push(model.item_at(number));
            }
        }
        model.imports = imports(&model.code);
        model
    }

    fn line_count(&self) -> usize {
        self.offsets.len() - 1
    }

    fn line(&self, number: usize) -> &str {
        if number == 0 || number > self.line_count() {
            return "";
        }
        self.source[self.offsets[number - 1]..self.offsets[number]].trim_end_matches(['\n', '\r'])
    }

    fn code_line(&self, number: usize) -> &str {
        if number == 0 || number > self.line_count() {
            return "";
        }
        self.code[self.offsets[number - 1]..self.offsets[number]].trim_end_matches(['\n', '\r'])
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

    fn code_of(&self, span: Lines) -> &str {
        &self.code[self.offsets[span.first - 1]..self.offsets[span.last.min(self.line_count())]]
    }

    fn covered(&self, line: usize) -> bool {
        self.listed.iter().any(|span| span.contains(line))
    }

    fn opening_for(&self, first: usize, name: &str) -> String {
        let line = self.line(first).trim_end();
        let unique = |opening: &str| {
            (1..=self.line_count())
                .filter(|&number| self.line(number).starts_with(opening))
                .count()
                == 1
        };
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

fn definition(line: &str) -> Option<(&str, Kind)> {
    let words: Vec<&str> = line
        .split_whitespace()
        .skip_while(|word| word.starts_with("pub"))
        .collect();
    let name_after = |at: usize| words.get(at).and_then(|word| identifiers(word).next());
    match *words.first()? {
        "macro_rules!" => Some((name_after(1)?, Kind::Macro)),
        "const" | "unsafe" | "async" | "extern" => {
            let rest: Vec<&&str> = words
                .iter()
                .skip_while(|word| matches!(**word, "const" | "unsafe" | "async" | "extern"))
                .collect();
            if rest.first() == Some(&&"fn") {
                let name = identifiers(rest.get(1)?).next()?;
                let kind = if words.contains(&"const") {
                    Kind::ConstFunction
                } else {
                    Kind::Function
                };
                Some((name, kind))
            } else if words[0] == "const" {
                Some((name_after(1)?, Kind::Value))
            } else {
                None
            }
        }
        "fn" => Some((name_after(1)?, Kind::Function)),
        "static" => {
            let at = if words.get(1) == Some(&"mut") { 2 } else { 1 };
            Some((name_after(at)?, Kind::Value))
        }
        "struct" | "enum" | "type" | "union" | "trait" => Some((name_after(1)?, Kind::Type)),
        _ => None,
    }
}

fn imports(code: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for statement in code.split(';') {
        let statement = unspaced(statement);
        let Some(at) = statement.find("usecrate::") else {
            continue;
        };
        let path = &statement[at + "usecrate::".len()..];
        let (prefix, entries): (&str, Vec<&str>) = match path.split_once('{') {
            Some((prefix, rest)) => (
                prefix.trim_end_matches("::"),
                rest.trim_end_matches('}').split(',').collect(),
            ),
            None => ("", vec![path]),
        };
        for entry in entries {
            let (target, alias) = match entry.split_once("as") {
                Some((target, alias)) if !target.is_empty() && !alias.contains(':') => {
                    (target, Some(alias))
                }
                _ => (entry, None),
            };
            let full = joined(&[prefix, target]);
            let Some((module, name)) = full.rsplit_once("::") else {
                continue;
            };
            found.insert(alias.unwrap_or(name).to_owned(), module.to_owned());
        }
    }
    found
}

struct Reference {
    module: Option<String>,
    name: String,
    called: bool,
}

fn references(code: &str) -> Vec<Reference> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    let ident = |at: usize| -> usize {
        let mut end = at;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            end += 1;
        }
        end
    };
    while index < bytes.len() {
        let c = bytes[index];
        let starts = (c.is_ascii_alphabetic() || c == b'_')
            && (index == 0
                || !(bytes[index - 1].is_ascii_alphanumeric() || bytes[index - 1] == b'_'));
        if !starts {
            index += 1;
            continue;
        }
        let mut segments = Vec::new();
        let mut at = index;
        loop {
            let end = ident(at);
            segments.push(&code[at..end]);
            at = end;
            if code[at..].starts_with("::") && ident(at + 2) > at + 2 {
                at += 2;
            } else {
                break;
            }
        }
        let mut after = at;
        while after < bytes.len() && bytes[after] == b' ' {
            after += 1;
        }
        let called = bytes.get(after) == Some(&b'(') || bytes.get(after) == Some(&b'!');
        for (position, segment) in segments.iter().enumerate() {
            let last = position + 1 == segments.len();
            found.push(Reference {
                module: None,
                name: (*segment).to_owned(),
                called: called && last,
            });
        }
        if segments.first() == Some(&"crate") {
            for split in 2..=segments.len() {
                found.push(Reference {
                    module: Some(segments[1..split - 1].join("::")),
                    name: segments[split - 1].to_owned(),
                    called: called && split == segments.len(),
                });
            }
        }
        index = at;
    }
    found
}

fn reach_not_listed(
    items: &[(String, String)],
    reached: &BTreeSet<(String, usize)>,
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<BTreeSet<String>> {
    let mut problems = BTreeSet::new();
    let mut models: BTreeMap<String, Option<FileModel>> = BTreeMap::new();
    let load = |path: &str, models: &mut BTreeMap<String, Option<FileModel>>| {
        models.entry(path.to_owned()).or_insert_with(|| {
            let openings: Vec<&str> = items
                .iter()
                .filter(|(item_path, _)| item_path == path)
                .map(|(_, opening)| opening.as_str())
                .collect();
            source_of(path)
                .ok()
                .map(|source| FileModel::new(source, &openings))
        });
    };
    let mut work: Vec<(String, Lines, bool)> = Vec::new();
    for (path, line) in reached {
        load(path, &mut models);
        let Some(model) = models[path].as_ref() else {
            problems.insert(format!("{path}: reached by a proof but cannot be read"));
            continue;
        };
        let function = innermost(&model.functions, *line);
        if !model.covered(*line) {
            match function {
                Some(span) => {
                    let name = definition(model.code_line(span.first))
                        .map(|(name, _)| name)
                        .unwrap_or_default();
                    problems.insert(format!(
                        "item = {path} | {}",
                        model.opening_for(span.first, &format!("fn {name}"))
                    ));
                }
                None => {
                    let above = model
                        .functions
                        .iter()
                        .filter(|span| span.first < *line)
                        .max_by_key(|span| span.first);
                    problems.insert(match above {
                        Some(span) => format!(
                            "{path}:{line} is reached but lies in no function's span; the function at {path}:{} ends at line {}, so its span is cut short",
                            span.first, span.last
                        ),
                        None => format!("{path}:{line} is reached but lies in no function"),
                    });
                }
            }
        }
        if let Some(span) = function {
            let compile_time = definition(model.code_line(span.first))
                .is_some_and(|(_, kind)| kind == Kind::ConstFunction);
            work.push((path.clone(), span, compile_time));
        }
        if let Some(span) = innermost(&model.impls, *line) {
            work.push((
                path.clone(),
                Lines {
                    first: span.first,
                    last: span.first,
                },
                false,
            ));
        }
    }
    let mut seen = BTreeSet::new();
    while let Some((path, span, compile_time)) = work.pop() {
        if !seen.insert((path.clone(), span, compile_time)) {
            continue;
        }
        let found = match models.get(&path).and_then(Option::as_ref) {
            Some(model) => references(model.code_of(span)),
            None => continue,
        };
        for reference in found {
            let mut targets: Vec<(String, String)> = Vec::new();
            match &reference.module {
                Some(module) => {
                    for file in module_files(module) {
                        targets.push((file, reference.name.clone()));
                    }
                }
                None => {
                    targets.push((path.clone(), reference.name.clone()));
                    let imported = models[&path]
                        .as_ref()
                        .and_then(|model| model.imports.get(&reference.name).cloned());
                    if let Some(module) = imported {
                        for file in module_files(&module) {
                            targets.push((file, reference.name.clone()));
                        }
                    }
                }
            }
            for (file, name) in targets {
                load(&file, &mut models);
                let Some(model) = models[&file].as_ref() else {
                    continue;
                };
                for &(definition_span, kind) in model.definitions.get(&name).into_iter().flatten() {
                    let needed = match kind {
                        Kind::Value | Kind::Type | Kind::Macro | Kind::ConstFunction => true,
                        Kind::Function => compile_time && reference.called,
                    };
                    if !needed {
                        continue;
                    }
                    if !model.covered(definition_span.first) {
                        problems.insert(format!(
                            "item = {file} | {}",
                            model.opening_for(definition_span.first, &name)
                        ));
                    }
                    work.push((file.clone(), definition_span, kind != Kind::Function));
                }
            }
        }
    }
    Ok(problems)
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

fn target_folder(root: &Path) -> PathBuf {
    match env::var_os("CARGO_TARGET_DIR") {
        Some(folder) => root.join(folder),
        None => root.join("target"),
    }
}

fn newest_goto_program(folder: &Path, harness: &str, since: SystemTime) -> Result<PathBuf> {
    let suffix = format!("{}.out", mangled_path(harness));
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    let mut folders = vec![folder.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in
            fs::read_dir(&folder).with_context(|| format!("cannot read {}", folder.display()))?
        {
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

pub(crate) fn fail_on_unlisted_kani_reach(root: &Path, name: &str) -> Result<()> {
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
            let program = newest_goto_program(&target_folder(root).join("kani"), harness, started)?;
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

    fn sources<'a>(files: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Result<String> + 'a {
        |path| {
            files
                .iter()
                .find(|(name, _)| *name == path)
                .map(|(_, text)| (*text).to_owned())
                .with_context(|| format!("no {path}"))
        }
    }

    const LIB: &str = "mod a;\n#[cfg(test)]\nmod t;\n#[cfg(unix)]\nmod u;\nmod n;\n";

    const A: &str = "#[cfg(kani)]
mod proofs {
    #[kani::proof]
    pub fn public() {}

    #[kani::proof]
    /// documented
    // SAFETY: none
    #[kani::unwind(3)]
    pub(crate) fn documented() {
        let _ = \"#[kani::proof] fn in_a_string() {}\";
    }

    fn helper() {}
}

#[cfg(kani)]
mod extra_proofs {
    #[kani::proof]
    fn extra() {}
}

#[cfg(all(kani, test))]
mod gated {
    #[kani::proof]
    fn gated_one() {}
}

#[cfg(kani)]
mod off {
    #[kani::proof]
    #[cfg(any())]
    fn never() {}

    #[kani::proof_for_contract(f)]
    fn contract() {}

    #[cfg_attr(kani, kani::proof)]
    fn attribute() {}

    fn outer() {
        #[kani::proof]
        fn hidden() {}
    }
}
";

    const T: &str = "#[cfg(kani)]\nmod proofs {\n    #[kani::proof]\n    fn in_tests() {}\n}\n";
    const U: &str = "#[cfg(kani)]\nmod proofs {\n    #[kani::proof]\n    fn on_unix() {}\n}\n";
    const N: &str = "mod inner;\n";
    const INNER: &str = "#[cfg(kani)]\nmod proofs {\n    /* #[kani::proof] fn commented() {} */\n    #[kani::proof]\n    fn nested() {}\n}\n";

    fn fixture() -> (Vec<Harness>, Vec<String>) {
        let files = [
            ("src/lib.rs", LIB),
            ("src/a.rs", A),
            ("src/t.rs", T),
            ("src/u.rs", U),
            ("src/n.rs", N),
            ("src/n/inner.rs", INNER),
        ];
        let paths: Vec<String> = files.iter().map(|(path, _)| (*path).to_owned()).collect();
        discover_harnesses(&paths, sources(&files)).unwrap()
    }

    #[test]
    fn harnesses_are_found_under_any_visibility_comment_attribute_and_module_name() {
        let (harnesses, _) = fixture();
        let found: Vec<(&str, bool)> = harnesses
            .iter()
            .map(|harness| (harness.path.as_str(), harness.needs_tests))
            .collect();
        assert_eq!(
            found,
            [
                ("a::proofs::public", false),
                ("a::proofs::documented", false),
                ("a::extra_proofs::extra", false),
                ("t::proofs::in_tests", true),
                ("n::inner::proofs::nested", false),
            ]
        );
    }

    #[test]
    fn an_unknown_cfg_or_an_unsupported_proof_attribute_fails_by_name() {
        let (_, problems) = fixture();
        let named = |text: &str| problems.iter().filter(|p| p.contains(text)).count();
        assert_eq!(named("a::gated::gated_one"), 1, "{problems:#?}");
        assert_eq!(named("a::off::never"), 1, "{problems:#?}");
        assert_eq!(named("kani::proof_for_contract"), 1, "{problems:#?}");
        assert_eq!(named("#[cfg_attr(kani,kani::proof)]"), 1, "{problems:#?}");
        assert_eq!(named("u::proofs::on_unix"), 1, "{problems:#?}");
        assert_eq!(named("inside another item"), 1, "{problems:#?}");
        assert_eq!(problems.len(), 6, "{problems:#?}");
    }

    fn harness(path: &str, needs_tests: bool) -> Harness {
        Harness {
            path: path.to_owned(),
            file: "src/m.rs".to_owned(),
            line: 1,
            needs_tests,
        }
    }

    #[test]
    fn every_harness_runs_somewhere_and_exactly_named_ones_run_once() {
        let harnesses = [
            harness("m::proofs::first", true),
            harness("m::proofs::second", true),
            harness("other::proofs::first", false),
            harness("other::proofs::second", false),
        ];
        let problems = |checks: &[&str]| {
            let parsed: Vec<KaniCheck> = checks.iter().filter_map(|c| kani_check(c)).collect();
            harnesses_not_run_as_listed(&parsed, &harnesses)
        };
        let first = "cargo kani --tests --harness m::proofs::first --exact";
        let second = "cargo kani --tests --exact --harness=m::proofs::second";
        let other = "cargo kani --harness other::proofs::";
        assert!(problems(&[first, second, other, "cargo test --lib m"]).is_empty());
        assert!(problems(&[first, second, "cargo kani"]).is_empty());
        assert!(problems(&[first, second, other, other]).is_empty());
        let unrun = problems(&[first, second, "cargo kani --harness=elsewhere::proofs::"]);
        assert_eq!(unrun.len(), 2);
        assert!(unrun[0].starts_with("other::proofs::first (src/m.rs:1) is run by no full-check"));
        let missing = problems(&[first, other]);
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("m::proofs::second is run by 0"));
        let without_tests = problems(&[
            first,
            "cargo kani --harness m::proofs::second --exact",
            other,
        ]);
        assert_eq!(without_tests.len(), 1);
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

    #[test]
    fn kani_list_names_come_from_their_own_object() {
        let json = r#"{"kani-version": "0.68.0", "standard-harnesses": {"src/a.rs": ["a::proofs::x", "a::proofs::y"], "src/b.rs": ["b::proofs::z"]}, "contract-harnesses": {}, "contracts": [], "totals": {}}"#;
        let names = listed_names(json, "standard-harnesses").unwrap();
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["a::proofs::x", "a::proofs::y", "b::proofs::z"]
        );
        assert!(listed_names(json, "contract-harnesses").unwrap().is_empty());
    }

    const REACHED: &str = "const LIMIT: u8 = BASE + 1;\nconst BASE: u8 = 1;\nconst UNUSED: u8 = 2;\nstruct Coder {\n    table: [u8; WIDTH],\n}\nconst WIDTH: usize = 4;\nimpl Coder {\n    fn step(&mut self) -> u8 {\n        LIMIT // \"UNUSED\"\n    }\n\n    fn other(&self) {}\n}\nfn free() {\n    let _ = || 1;\n}\n";

    fn unlisted_in(files: &[(&str, &str)], items: &[(&str, &str)], lines: &[usize]) -> Vec<String> {
        let items: Vec<(String, String)> = items
            .iter()
            .map(|(path, opening)| ((*path).to_owned(), (*opening).to_owned()))
            .collect();
        let reached = lines
            .iter()
            .map(|line| ("src/m.rs".to_owned(), *line))
            .collect();
        reach_not_listed(&items, &reached, sources(files))
            .unwrap()
            .into_iter()
            .collect()
    }

    fn unlisted(openings: &[&str], lines: &[usize]) -> Vec<String> {
        let items: Vec<(&str, &str)> = openings
            .iter()
            .map(|opening| ("src/m.rs", *opening))
            .collect();
        unlisted_in(&[("src/m.rs", REACHED)], &items, lines)
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

    const STEP: &str = "fn step() -> u32 {\n    TOP\n}\n";
    const STEP_ITEMS: [(&str, &str); 2] = [("src/m.rs", "fn step("), ("src/m.rs", "const TOP:")];

    #[test]
    fn a_const_needs_every_function_it_calls() {
        let constant = format!(
            "{STEP}const TOP: u32 = top_from(24);\nconst fn top_from(bits: u32) -> u32 {{\n    1 << bits\n}}\n"
        );
        let files = [("src/m.rs", constant.as_str())];
        assert_eq!(
            unlisted_in(&files, &STEP_ITEMS, &[2]),
            ["item = src/m.rs | const fn top_from("]
        );
        let listed = [
            STEP_ITEMS[0],
            STEP_ITEMS[1],
            ("src/m.rs", "const fn top_from("),
        ];
        assert!(unlisted_in(&files, &listed, &[2]).is_empty());
    }

    #[test]
    fn a_const_needs_what_it_names_in_another_file_by_path_or_import() {
        let memory = "pub(crate) const TOP_BITS: u32 = 24;\npub(crate) const OTHER: u32 = 1;\n";
        for (m, line) in [
            (
                format!("{STEP}const TOP: u32 = 1 << crate::memory::TOP_BITS;\n"),
                2,
            ),
            (
                format!(
                    "use crate::memory::{{OTHER, TOP_BITS}};\n{STEP}const TOP: u32 = 1 << TOP_BITS;\n"
                ),
                3,
            ),
        ] {
            let files = [("src/m.rs", m.as_str()), ("src/memory.rs", memory)];
            assert_eq!(
                unlisted_in(&files, &STEP_ITEMS, &[line]),
                ["item = src/memory.rs | pub(crate) const TOP_BITS:"],
                "{m}"
            );
            let listed = [
                STEP_ITEMS[0],
                STEP_ITEMS[1],
                ("src/memory.rs", "pub(crate) const TOP_BITS:"),
            ];
            assert!(unlisted_in(&files, &listed, &[line]).is_empty(), "{m}");
        }
    }

    #[test]
    fn a_const_needs_the_macro_it_invokes() {
        let source = format!(
            "{STEP}const TOP: u32 = shift!(24);\nmacro_rules! shift {{\n    ($x:expr) => {{\n        1 << $x\n    }};\n}}\n"
        );
        let files = [("src/m.rs", source.as_str())];
        assert_eq!(
            unlisted_in(&files, &STEP_ITEMS, &[2]),
            ["item = src/m.rs | macro_rules! shift {"]
        );
    }

    #[test]
    fn a_struct_field_named_like_a_function_needs_no_function() {
        let source = "fn step(table: Table) -> u16 {\n    table.literal\n}\nstruct Table {\n    literal: u16,\n}\nfn literal() {}\n";
        let files = [("src/m.rs", source)];
        let items = [("src/m.rs", "fn step("), ("src/m.rs", "struct Table {")];
        assert!(unlisted_in(&files, &items, &[2]).is_empty());
    }

    #[test]
    fn a_reached_line_past_a_cut_span_is_reported_as_cut() {
        let source = "fn f() {\n}\n    stray();\n";
        let files = [("src/m.rs", source)];
        assert_eq!(
            unlisted_in(&files, &[("src/m.rs", "fn f(")], &[3]),
            [
                "src/m.rs:3 is reached but lies in no function's span; the function at src/m.rs:1 ends at line 2, so its span is cut short"
            ]
        );
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
}
