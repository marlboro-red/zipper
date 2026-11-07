use crate::verify::{
    MANIFEST, Verification, end_of_braced_body_or_semicolon, manifest, read_source, sources,
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

fn names_a_proof(text: &str) -> bool {
    identifiers(text).any(|name| name == "proof" || name.starts_with("proof_"))
}

fn proof_form_problems(path: &str, source: &str) -> Vec<String> {
    let model = FileModel::new(path, source.to_owned(), &[]);
    let code = model.code.as_str();
    let bytes = code.as_bytes();
    let macros: Vec<Lines> = model
        .definitions
        .values()
        .flatten()
        .filter(|(_, kind)| *kind == Kind::Macro)
        .map(|(span, _)| *span)
        .collect();
    let mut problems = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'#' {
            index += 1;
            continue;
        }
        let mut open = index + 1;
        while open < bytes.len() && (bytes[open] == b'!' || bytes[open].is_ascii_whitespace()) {
            open += 1;
        }
        if bytes.get(open) != Some(&b'[') {
            index += 1;
            continue;
        }
        let mut depth = 0usize;
        let mut close = open;
        while close < bytes.len() {
            match bytes[close] {
                b'[' => depth += 1,
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            close += 1;
        }
        let text = &code[index..(close + 1).min(code.len())];
        let line = line_of(&model.offsets, index);
        if names_a_proof(text) {
            let alone = text == "#[kani::proof]" && model.code_line(line).trim() == text;
            let in_macro = macros.iter().any(|span| span.contains(line));
            if !alone || in_macro {
                problems.push(format!(
                    "{path}:{line}: `{text}` marks a Kani proof in a form the gate does not read; write `#[kani::proof]` alone on its own line, outside any macro"
                ));
            }
        }
        index = close + 1;
    }
    for (line, tree) in kani_imports(code, &model.offsets) {
        problems.push(format!(
            "{path}:{line}: `use {tree}` imports Kani's proof attribute under another name, which the gate does not read"
        ));
    }
    problems.sort_by_key(|problem| {
        problem
            .split(':')
            .nth(1)
            .and_then(|line| line.parse::<usize>().ok())
            .unwrap_or(0)
    });
    problems
}

fn kani_imports(code: &str, offsets: &[usize]) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (start, end) in use_statements(code) {
        let text = &code[start + 3..end];
        let Some(tokens) = use_tokens(text) else {
            continue;
        };
        let mut at = usize::from(tokens.first() == Some(&UseToken::Separator));
        let mut trees = Vec::new();
        if parse_use_tree(&tokens, &mut at, &[], &mut trees).is_none() {
            continue;
        }
        let imports_proof = trees.iter().any(|tree| match tree {
            UseTree::Glob(prefix) => prefix.first().map(String::as_str) == Some("kani"),
            UseTree::Leaf { path, .. } => {
                path.first().map(String::as_str) == Some("kani")
                    && (path.len() == 1
                        || path.iter().skip(1).any(|segment| names_a_proof(segment)))
            }
        });
        if imports_proof {
            found.push((line_of(offsets, start), text.trim().to_owned()));
        }
    }
    found
}

fn use_statements(code: &str) -> Vec<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(position) = code[index..].find("use") {
        let start = index + position;
        index = start + 3;
        let before_ok =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let after_ok = bytes
            .get(start + 3)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b':');
        if !before_ok || !after_ok {
            continue;
        }
        let line_start = code[..start].rfind('\n').map_or(0, |at| at + 1);
        let lead = code[line_start..start].trim();
        if !(lead.is_empty() || lead.starts_with("pub")) {
            continue;
        }
        if let Some(end) = code[start..].find(';') {
            found.push((start, start + end));
        }
    }
    found
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
        if !source.contains("proof") {
            continue;
        }
        let code = blank_strings_and_comments(&source);
        if !names_a_proof(&code) {
            continue;
        }
        problems.extend(proof_form_problems(path, &source));
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

fn section_kani_checks(verification: &Verification) -> Vec<KaniCheck<'_>> {
    verification
        .full_checks
        .iter()
        .filter_map(|check| kani_check(check))
        .collect()
}

fn harness_names(check: &KaniCheck, harnesses: &[Harness]) -> Vec<String> {
    if check.exact {
        return check
            .filters
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
    }
    harnesses
        .iter()
        .filter(|harness| check.runs(harness))
        .map(|harness| harness.path.clone())
        .collect()
}

fn names_a_kani_cfg(attribute: &str) -> bool {
    attribute.starts_with("#[cfg(")
        && !attribute.contains("not(")
        && identifiers(attribute).any(|name| name == "kani")
}

fn opens_impl(code_line: &str) -> bool {
    let line = without_visibility(code_line.trim_start());
    line.starts_with("impl ") || line.starts_with("impl<")
}

struct KaniCode {
    first: usize,
    opening: String,
    scope: Lines,
    module: Option<String>,
}

fn kani_code(path: &str, source: &str) -> Vec<KaniCode> {
    let model = FileModel::new(path, source.to_owned(), &[]);
    let whole_file = Lines {
        first: 1,
        last: model.line_count(),
    };
    let member = |first: usize| -> Option<(KaniCode, Lines)> {
        let code_line = model.code_line(first);
        let opening = match definition(code_line) {
            Some((name, Kind::Function | Kind::ConstFunction)) => {
                model.opening_for(first, &format!("fn {name}"))
            }
            Some((name, _)) => model.opening_for(first, name),
            None if opens_impl(code_line) => model.line(first).trim_end().to_owned(),
            None => return None,
        };
        let span = model.item_at(first);
        Some((
            KaniCode {
                first,
                opening,
                scope: whole_file,
                module: None,
            },
            span,
        ))
    };
    let mut found = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut open_attribute: Option<String> = None;
    let mut number = 1;
    while number <= model.line_count() {
        let trimmed = model.code_line(number).trim();
        let attribute_text = match open_attribute.take() {
            Some(mut text) => {
                text.push_str(trimmed);
                Some(text)
            }
            None if trimmed.starts_with("#[") => Some(trimmed.to_owned()),
            None => None,
        };
        if let Some(text) = attribute_text {
            if bracket_balance(&text) > 0 {
                open_attribute = Some(text);
            } else {
                pending.push(unspaced(&text));
            }
            number += 1;
            continue;
        }
        if trimmed.is_empty() {
            number += 1;
            continue;
        }
        let under_kani = pending.iter().any(|attribute| names_a_kani_cfg(attribute));
        pending.clear();
        let words: Vec<&str> = without_visibility(trimmed).split_whitespace().collect();
        if under_kani && words.first() == Some(&"mod") && trimmed.ends_with('{') {
            let module = model.item_at(number);
            let module_opening = model.line(number).trim_end().to_owned();
            let mut inner = number + 1;
            while inner < module.last {
                match member(inner) {
                    Some((mut code, span)) => {
                        code.scope = module;
                        code.module = Some(module_opening.clone());
                        found.push(code);
                        inner = span.last + 1;
                    }
                    None => inner += 1,
                }
            }
            number = module.last + 1;
            continue;
        }
        if under_kani && let Some((code, span)) = member(number) {
            found.push(code);
            number = span.last + 1;
            continue;
        }
        number += 1;
    }
    found
}

struct Section<'a> {
    name: &'a str,
    checks: Vec<KaniCheck<'a>>,
    items: &'a [(String, String)],
}

impl Section<'_> {
    fn runs(&self, harness: &Harness) -> bool {
        self.checks.iter().any(|check| check.runs(harness))
    }
}

fn kani_code_not_listed(
    sections: &[Section],
    harnesses: &[Harness],
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<Vec<String>> {
    let files: BTreeSet<&str> = harnesses
        .iter()
        .map(|harness| harness.file.as_str())
        .collect();
    let mut problems = Vec::new();
    for file in files {
        let source = source_of(file)?;
        let listed_by: Vec<FileModel> = sections
            .iter()
            .map(|section| {
                let openings: Vec<&str> = section
                    .items
                    .iter()
                    .filter(|(path, _)| path == file)
                    .map(|(_, opening)| opening.as_str())
                    .collect();
                FileModel::new(file, source.clone(), &openings)
            })
            .collect();
        let lists = |section: usize, line: usize| listed_by[section].covered(line);
        for code in kani_code(file, &source) {
            let in_scope: Vec<&Harness> = harnesses
                .iter()
                .filter(|harness| harness.file == file && code.scope.contains(harness.line))
                .collect();
            let add = |index: usize| {
                let item = format!("`item = {file} | {}`", code.opening);
                match &code.module {
                    Some(module)
                        if in_scope.iter().all(|harness| sections[index].runs(harness)) =>
                    {
                        format!("{item}, or `item = {file} | {module}` for the whole module")
                    }
                    _ => item,
                }
            };
            if let Some(harness) = in_scope.iter().find(|harness| harness.line == code.first) {
                for (index, section) in sections.iter().enumerate() {
                    if section.runs(harness) && !lists(index, code.first) {
                        problems.push(format!(
                            "{}: it runs {}, which no item of the section lists; add {}",
                            section.name,
                            harness.path,
                            add(index)
                        ));
                    }
                }
                continue;
            }
            let running: Vec<usize> = (0..sections.len())
                .filter(|&index| in_scope.iter().any(|harness| sections[index].runs(harness)))
                .collect();
            let running_all: Vec<usize> = running
                .iter()
                .copied()
                .filter(|&index| in_scope.iter().all(|harness| sections[index].runs(harness)))
                .collect();
            for &index in &running_all {
                if !lists(index, code.first) {
                    problems.push(format!(
                        "{}: it runs every proof that may use the Kani-only code at {file}:{}, which no item of the section lists; add {}",
                        sections[index].name,
                        code.first,
                        add(index)
                    ));
                }
            }
            if running_all.is_empty()
                && let Some(&first) = running.first()
                && !running.iter().any(|&index| lists(index, code.first))
            {
                let names: Vec<&str> = running.iter().map(|&index| sections[index].name).collect();
                problems.push(format!(
                    "{file}:{} is Kani-only code that no section running its proofs lists; add {} to whichever of {} has proofs that use it",
                    code.first,
                    add(first),
                    names.join(", ")
                ));
            }
        }
    }
    Ok(problems)
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
    let sections: Vec<Section> = all
        .iter()
        .map(|verification| Section {
            name: &verification.name,
            checks: section_kani_checks(verification),
            items: &verification.items,
        })
        .collect();
    problems.extend(kani_code_not_listed(&sections, &harnesses, |path| {
        read_source(root, path)
    })?);
    for verification in &all {
        problems.extend(goto_fingerprints_not_listed(verification, &harnesses));
        problems.extend(reach_check_missing(
            &verification.name,
            &verification.full_checks,
        ));
    }
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

pub(crate) fn blank_strings_and_comments(source: &str) -> String {
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

struct Binding {
    name: String,
    path: Vec<String>,
    line: usize,
    exported: bool,
}

struct FileModel {
    source: String,
    code: String,
    offsets: Vec<usize>,
    listed: Vec<Lines>,
    functions: Vec<Lines>,
    impls: Vec<Lines>,
    definitions: BTreeMap<String, Vec<(Lines, Kind)>>,
    bindings: Vec<Binding>,
    refusals: Vec<String>,
}

impl FileModel {
    fn new(path: &str, source: String, openings: &[&str]) -> Self {
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
            bindings: Vec::new(),
            refusals: Vec::new(),
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
            if without_visibility(code_line.trim_start()).starts_with("impl") {
                model.impls.push(model.item_at(number));
            }
            if code_line.contains("include!") {
                model.refusals.push(format!(
                    "{path}:{number}: `include!` brings in code the reach check cannot see"
                ));
            }
        }
        let (bindings, refusals) = use_bindings(path, &model.code, &model.offsets);
        model.bindings = bindings;
        model.refusals.extend(refusals);
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
        let body = &rest[..end_of_braced_body_or_semicolon(rest)];
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

    fn binding(&self, name: &str) -> Option<&Binding> {
        self.bindings.iter().find(|binding| binding.name == name)
    }

    fn defines(&self, name: &str) -> bool {
        self.definitions.contains_key(name)
    }

    fn item_owning(&self, line: usize) -> Option<(usize, &str, Kind)> {
        if !self.code_line(line).trim_start().starts_with("#[") {
            return self
                .definitions
                .iter()
                .flat_map(|(name, spans)| spans.iter().map(move |(span, kind)| (name, span, kind)))
                .filter(|(_, span, _)| span.contains(line))
                .max_by_key(|(_, span, _)| span.first)
                .map(|(name, span, kind)| (span.first, name.as_str(), *kind));
        }
        let mut below = line + 1;
        while below <= self.line_count() {
            let code = self.code_line(below).trim();
            if !(code.is_empty() || code.starts_with("#[")) {
                return definition(self.code_line(below)).map(|(name, kind)| (below, name, kind));
            }
            below += 1;
        }
        None
    }

    fn item_opening(&self, first: usize, name: &str, kind: Kind) -> String {
        match kind {
            Kind::Function | Kind::ConstFunction => self.opening_for(first, &format!("fn {name}")),
            _ => self.opening_for(first, name),
        }
    }

    fn has_variant(&self, name: &str, variant: &str) -> bool {
        self.definitions
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(span, kind)| {
                *kind == Kind::Type
                    && without_visibility(self.code_line(span.first).trim_start())
                        .starts_with("enum ")
            })
            .any(|(span, _)| {
                (span.first + 1..=span.last).any(|line| {
                    self.code_line(line)
                        .trim_start()
                        .strip_prefix(variant)
                        .is_some_and(|rest| {
                            !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                        })
                })
            })
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

fn line_of(offsets: &[usize], offset: usize) -> usize {
    offsets.partition_point(|&start| start <= offset).max(1)
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

fn without_visibility(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("pub") else {
        return line;
    };
    let trimmed = rest.trim_start();
    if let Some(inside) = trimmed.strip_prefix('(') {
        return inside
            .find(')')
            .map_or(line, |close| inside[close + 1..].trim_start());
    }
    if rest.starts_with(char::is_whitespace) {
        return trimmed;
    }
    line
}

fn definition(line: &str) -> Option<(&str, Kind)> {
    let words: Vec<&str> = without_visibility(line.trim_start())
        .split_whitespace()
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum UseToken {
    Name(String),
    Separator,
    Open,
    Close,
    Comma,
    Glob,
}

fn use_tokens(text: &str) -> Option<Vec<UseToken>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            ':' if chars.peek().map(|(_, next)| *next) == Some(':') => {
                chars.next();
                tokens.push(UseToken::Separator);
            }
            '{' => tokens.push(UseToken::Open),
            '}' => tokens.push(UseToken::Close),
            ',' => tokens.push(UseToken::Comma),
            '*' => tokens.push(UseToken::Glob),
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let mut end = at + c.len_utf8();
                while let Some((next_at, next)) = chars.peek().copied() {
                    if next.is_ascii_alphanumeric() || next == '_' {
                        end = next_at + next.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(UseToken::Name(text[at..end].to_owned()));
            }
            _ => return None,
        }
    }
    Some(tokens)
}

enum UseTree {
    Leaf {
        path: Vec<String>,
        alias: Option<String>,
    },
    Glob(Vec<String>),
}

fn parse_use_tree(
    tokens: &[UseToken],
    at: &mut usize,
    prefix: &[String],
    out: &mut Vec<UseTree>,
) -> Option<()> {
    let mut path = prefix.to_vec();
    loop {
        match tokens.get(*at)? {
            UseToken::Open => {
                *at += 1;
                loop {
                    if tokens.get(*at) == Some(&UseToken::Close) {
                        *at += 1;
                        return Some(());
                    }
                    parse_use_tree(tokens, at, &path, out)?;
                    match tokens.get(*at)? {
                        UseToken::Comma => *at += 1,
                        UseToken::Close => {
                            *at += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            UseToken::Glob => {
                *at += 1;
                out.push(UseTree::Glob(path));
                return Some(());
            }
            UseToken::Name(name) => {
                *at += 1;
                path.push(name.clone());
                if tokens.get(*at) == Some(&UseToken::Separator) {
                    *at += 1;
                    continue;
                }
                let mut alias = None;
                if tokens.get(*at) == Some(&UseToken::Name("as".to_owned())) {
                    match tokens.get(*at + 1)? {
                        UseToken::Name(name) => alias = Some(name.clone()),
                        _ => return None,
                    }
                    *at += 2;
                }
                if path.last().map(String::as_str) == Some("self") {
                    path.pop();
                }
                out.push(UseTree::Leaf { path, alias });
                return Some(());
            }
            _ => return None,
        }
    }
}

fn use_bindings(path: &str, code: &str, offsets: &[usize]) -> (Vec<Binding>, Vec<String>) {
    let mut bindings = Vec::new();
    let mut refusals = Vec::new();
    for (start, end) in use_statements(code) {
        let line = line_of(offsets, start);
        let line_start = code[..start].rfind('\n').map_or(0, |at| at + 1);
        let exported = !code[line_start..start].trim().is_empty();
        let Some(tokens) = use_tokens(&code[start + 3..end]) else {
            refusals.push(format!(
                "{path}:{line}: a `use` the reach check cannot parse"
            ));
            continue;
        };
        let mut trees = Vec::new();
        let mut at = usize::from(tokens.first() == Some(&UseToken::Separator));
        if parse_use_tree(&tokens, &mut at, &[], &mut trees).is_none() || at != tokens.len() {
            refusals.push(format!(
                "{path}:{line}: a `use` the reach check cannot parse"
            ));
            continue;
        }
        for tree in trees {
            match tree {
                UseTree::Glob(prefix) => {
                    if prefix.first().map(String::as_str) == Some("crate") {
                        refusals.push(format!(
                            "{path}:{line}: `use {}::*` imports names the reach check cannot list",
                            prefix.join("::")
                        ));
                    }
                }
                UseTree::Leaf {
                    path: target,
                    alias,
                } => {
                    if target.first().map(String::as_str) != Some("crate") || target.len() < 2 {
                        continue;
                    }
                    let name = alias.unwrap_or_else(|| target.last().cloned().unwrap_or_default());
                    bindings.push(Binding {
                        name,
                        path: target[1..].to_vec(),
                        line,
                        exported,
                    });
                }
            }
        }
    }
    (bindings, refusals)
}

struct Reference {
    segments: Vec<String>,
    called: bool,
    offset: usize,
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
                || !(bytes[index - 1].is_ascii_alphanumeric()
                    || bytes[index - 1] == b'_'
                    || bytes[index - 1] == b'.'));
        if !starts {
            index += 1;
            continue;
        }
        if &code[index..ident(index)] == "pub" {
            let mut next = ident(index);
            while next < bytes.len() && bytes[next] == b' ' {
                next += 1;
            }
            if bytes.get(next) == Some(&b'(') {
                index = code[next..]
                    .find(')')
                    .map_or(bytes.len(), |close| next + close + 1);
                continue;
            }
        }
        let mut segments = Vec::new();
        let mut at = index;
        loop {
            let end = ident(at);
            segments.push(code[at..end].to_owned());
            at = end;
            let mut next = at;
            while next < bytes.len() && bytes[next] == b' ' {
                next += 1;
            }
            if code[next..].starts_with("::") {
                let mut name = next + 2;
                while name < bytes.len() && bytes[name] == b' ' {
                    name += 1;
                }
                if ident(name) > name {
                    at = name;
                    continue;
                }
            }
            break;
        }
        let mut after = at;
        while after < bytes.len() && bytes[after] == b' ' {
            after += 1;
        }
        let called = bytes.get(after) == Some(&b'(') || bytes.get(after) == Some(&b'!');
        found.push(Reference {
            segments,
            called,
            offset: index,
        });
        index = at;
    }
    found
}

const EXTERNAL_ROOTS: &[&str] = &["std", "core", "alloc", "kani"];

enum Target {
    Here(String, String),
    Nothing,
}

fn resolve_crate_path(
    segments: &[String],
    load: &mut dyn FnMut(&str) -> bool,
    defines: &dyn Fn(&str, &str) -> Option<bool>,
) -> std::result::Result<Target, String> {
    let (name, module) = match segments.split_last() {
        Some((name, module)) => (name, module.join("::")),
        None => return Ok(Target::Nothing),
    };
    for file in module_files(&module) {
        if load(&file) {
            match defines(&file, name) {
                Some(true) => return Ok(Target::Here(file, name.clone())),
                Some(false) => {}
                None => {
                    return Err(format!(
                        "crate::{} is re-exported from {file}, which the reach check does not follow",
                        segments.join("::")
                    ));
                }
            }
        }
    }
    for file in module_files(&segments.join("::")) {
        if load(&file) {
            return Err(format!(
                "crate::{} names a module, which the reach check does not follow",
                segments.join("::")
            ));
        }
    }
    if segments.len() >= 2 {
        let type_name = &segments[segments.len() - 2];
        let parent = segments[..segments.len() - 2].join("::");
        for file in module_files(&parent) {
            if load(&file) && defines(&file, type_name) == Some(true) {
                return Err(format!(
                    "crate::{} is an associated item of a type in {file}, which the reach check does not follow",
                    segments.join("::")
                ));
            }
        }
    }
    Err(format!(
        "crate::{} resolves to no definition",
        segments.join("::")
    ))
}

fn reach_not_listed(
    items: &[(String, String)],
    reached: &BTreeSet<(String, usize)>,
    source_of: impl Fn(&str) -> Result<String>,
) -> Result<BTreeSet<String>> {
    let mut problems = BTreeSet::new();
    let models: std::cell::RefCell<BTreeMap<String, Option<FileModel>>> =
        std::cell::RefCell::new(BTreeMap::new());
    let load = |path: &str| -> bool {
        let mut models = models.borrow_mut();
        models
            .entry(path.to_owned())
            .or_insert_with(|| {
                let openings: Vec<&str> = items
                    .iter()
                    .filter(|(item_path, _)| item_path == path)
                    .map(|(_, opening)| opening.as_str())
                    .collect();
                source_of(path)
                    .ok()
                    .map(|source| FileModel::new(path, source, &openings))
            })
            .is_some()
    };
    let defines = |file: &str, name: &str| -> Option<bool> {
        let models = models.borrow();
        let model = models.get(file)?.as_ref()?;
        if model.defines(name) {
            Some(true)
        } else if model.binding(name).is_some_and(|binding| binding.exported) {
            None
        } else {
            Some(false)
        }
    };
    let mut work: Vec<(String, Lines, bool)> = Vec::new();
    for (path, line) in reached {
        if !load(path) {
            problems.insert(format!("{path}: reached by a proof but cannot be read"));
            continue;
        }
        let models_ref = models.borrow();
        let model = models_ref[path].as_ref().expect("loaded");
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
                None if model.item_owning(*line).is_some() => {
                    let (first, name, kind) = model.item_owning(*line).expect("checked");
                    if !model.covered(first) {
                        problems.insert(format!(
                            "item = {path} | {}",
                            model.item_opening(first, name, kind)
                        ));
                    }
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
        if function.is_none()
            && let Some((first, _, kind)) = model.item_owning(*line)
        {
            work.push((path.clone(), model.item_at(first), kind != Kind::Function));
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
        let (found, span_offset, offsets) = {
            let models_ref = models.borrow();
            let Some(model) = models_ref.get(&path).and_then(Option::as_ref) else {
                continue;
            };
            (
                references(model.code_of(span)),
                model.offsets[span.first - 1],
                model.offsets.clone(),
            )
        };
        for reference in found {
            let line = line_of(&offsets, span_offset + reference.offset);
            let segments = &reference.segments;
            let first = segments[0].as_str();
            let mut targets: Vec<(String, String)> = Vec::new();
            let refuse = |problems: &mut BTreeSet<String>, why: String| {
                problems.insert(format!("{path}:{line}: {why}"));
            };
            let binding = {
                let models_ref = models.borrow();
                models_ref[&path]
                    .as_ref()
                    .and_then(|model| model.binding(first))
                    .map(|binding| (binding.path.clone(), binding.line))
            };
            let local = defines(&path, first) == Some(true);
            if first == "crate" {
                match resolve_crate_path(&segments[1..], &mut |file| load(file), &defines) {
                    Ok(Target::Here(file, name)) => targets.push((file, name)),
                    Ok(Target::Nothing) => {}
                    Err(why) => refuse(&mut problems, why),
                }
            } else if matches!(first, "self" | "super" | "Self") {
                let name = segments.last().cloned().unwrap_or_default();
                if segments.len() > 1 && defines(&path, &name) != Some(true) && first != "Self" {
                    refuse(
                        &mut problems,
                        format!(
                            "`{}` leaves this file, which the reach check does not follow",
                            segments.join("::")
                        ),
                    );
                } else {
                    targets.push((path.clone(), name));
                }
            } else if !local && let Some((target, binding_line)) = binding {
                let mut full = target.clone();
                full.extend(segments[1..].iter().cloned());
                if segments.len() == 1 {
                    match resolve_crate_path(&full, &mut |file| load(file), &defines) {
                        Ok(Target::Here(file, name)) => targets.push((file, name)),
                        Ok(Target::Nothing) => {}
                        Err(why) => refuse(
                            &mut problems,
                            format!("{why} (imported at line {binding_line})"),
                        ),
                    }
                } else if let (2, Ok(Target::Here(file, name))) = (
                    segments.len(),
                    resolve_crate_path(&target, &mut |file| load(file), &defines),
                ) && models.borrow()[&file]
                    .as_ref()
                    .is_some_and(|model| model.has_variant(&name, &segments[1]))
                {
                    targets.push((file, name));
                } else {
                    let module_exists = module_files(&target.join("::"))
                        .iter()
                        .any(|file| load(file));
                    let why = if module_exists {
                        format!(
                            "`{}` goes through the module imported at line {binding_line}, which the reach check does not follow",
                            segments.join("::")
                        )
                    } else {
                        format!(
                            "`{}` names an item of `{first}`, imported at line {binding_line} from another file, which the reach check does not follow",
                            segments.join("::")
                        )
                    };
                    refuse(&mut problems, why);
                }
            } else {
                if segments.len() > 1 && !local && !EXTERNAL_ROOTS.contains(&first) {
                    let crate_module = module_files(first).iter().any(|file| load(file));
                    if crate_module {
                        refuse(
                            &mut problems,
                            format!(
                                "`{}` goes through a module path the reach check does not follow",
                                segments.join("::")
                            ),
                        );
                    }
                }
                for segment in segments {
                    targets.push((path.clone(), segment.clone()));
                }
            }
            let last = segments.last().cloned().unwrap_or_default();
            for (file, name) in targets {
                if !load(&file) {
                    continue;
                }
                let found: Vec<(Lines, Kind, bool, String)> = {
                    let models_ref = models.borrow();
                    let model = models_ref[&file].as_ref().expect("loaded");
                    model
                        .definitions
                        .get(&name)
                        .into_iter()
                        .flatten()
                        .map(|&(span, kind)| {
                            (
                                span,
                                kind,
                                model.covered(span.first),
                                model.opening_for(span.first, &name),
                            )
                        })
                        .collect()
                };
                for (definition_span, kind, covered, opening) in found {
                    let called = reference.called && name == last;
                    let needed = match kind {
                        Kind::Value | Kind::Type | Kind::Macro | Kind::ConstFunction => true,
                        Kind::Function => compile_time && called,
                    };
                    if !needed {
                        continue;
                    }
                    if !covered {
                        problems.insert(format!("item = {file} | {opening}"));
                    }
                    work.push((file.clone(), definition_span, kind != Kind::Function));
                }
            }
        }
    }
    for model in models.borrow().values().flatten() {
        problems.extend(model.refusals.iter().cloned());
    }
    Ok(problems)
}

fn reach_check_missing(name: &str, full_checks: &[String]) -> Option<String> {
    let reach = format!("cargo xtask kani-reach {name}");
    let runs_kani = full_checks.iter().any(|check| kani_check(check).is_some());
    (runs_kani && !full_checks.contains(&reach)).then(|| {
        format!("{name}: it runs Kani, so it needs `full-check = {reach}` to keep its items honest")
    })
}

fn goto_fingerprints_not_listed(verification: &Verification, harnesses: &[Harness]) -> Vec<String> {
    let expected: BTreeSet<String> = section_kani_checks(verification)
        .iter()
        .flat_map(|check| harness_names(check, harnesses))
        .collect();
    let recorded: Vec<&String> = verification
        .goto_fingerprints
        .iter()
        .map(|(harness, _)| harness)
        .collect();
    let recorded_set: BTreeSet<String> = recorded.iter().map(|name| (*name).clone()).collect();
    let mut problems = Vec::new();
    if recorded.len() != recorded_set.len() || recorded_set != expected {
        problems.push(format!(
            "{}: its goto-fingerprint lines must name exactly the harnesses its Kani checks run, once each; run `cargo xtask verify-sync {}`",
            verification.name, verification.name
        ));
    }
    problems
}

fn normalized_goto_fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut allocations: BTreeMap<String, usize> = BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || line.starts_with("Reading GOTO program") {
            continue;
        }
        let line = with_allocations_in_order(&without_crate_disambiguators(line), &mut allocations);
        hash = crate::verify::fnv1a(hash, line.bytes().chain([b'\n']));
    }
    format!("{hash:016x}")
}

fn with_allocations_in_order(line: &str, allocations: &mut BTreeMap<String, usize>) -> String {
    const MARK: &str = "AllocId(";
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(MARK) {
        out.push_str(&rest[..at + MARK.len()]);
        rest = &rest[at + MARK.len()..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let next = allocations.len();
        let order = *allocations.entry(rest[..digits].to_owned()).or_insert(next);
        out.push_str(&order.to_string());
        rest = &rest[digits..];
    }
    out.push_str(rest);
    out
}

fn without_crate_disambiguators(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut index = 0;
    while index < bytes.len() {
        if line[index..].starts_with("Cs") {
            let mut end = index + 2;
            while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
                end += 1;
            }
            if end > index + 2
                && bytes.get(end) == Some(&b'_')
                && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
            {
                out.push_str("Cs_");
                index = end + 1;
                continue;
            }
        }
        if bytes[index] == b'.' && index > 0 && bytes[index - 1].is_ascii_alphanumeric() {
            let hex = line[index + 1..]
                .bytes()
                .take_while(u8::is_ascii_hexdigit)
                .count();
            if hex == 16 && line[index + 17..].starts_with("::") {
                out.push('.');
                index += 17;
                continue;
            }
        }
        let c = line[index..].chars().next().unwrap_or(' ');
        out.push(c);
        index += c.len_utf8();
    }
    out
}

fn codegen(root: &Path, name: &str, check: &KaniCheck, harnesses: &[String]) -> Result<SystemTime> {
    let started = SystemTime::now();
    let mut codegen = Command::new(check.base[0]);
    codegen.args(&check.base[1..]).arg("--only-codegen");
    for harness in harnesses {
        codegen.args(["--harness", harness, "--exact"]);
    }
    let codegen = codegen.current_dir(root).output()?;
    ensure!(
        codegen.status.success(),
        "{name}: Kani codegen failed:\n{}{}",
        String::from_utf8_lossy(&codegen.stdout),
        String::from_utf8_lossy(&codegen.stderr)
    );
    Ok(started)
}

pub(crate) fn goto_fingerprints(
    root: &Path,
    verification: &Verification,
) -> Result<Vec<(String, String)>> {
    let checks = section_kani_checks(verification);
    if checks.is_empty() {
        return Ok(Vec::new());
    }
    let (harnesses, _) = workspace_harnesses(root)?;
    let goto_instrument = kani_tool("goto-instrument")?;
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for check in &checks {
        let names: Vec<String> = harness_names(check, &harnesses)
            .into_iter()
            .filter(|name| seen.insert(name.clone()))
            .collect();
        if names.is_empty() {
            continue;
        }
        let started = codegen(root, &verification.name, check, &names)?;
        for harness in &names {
            let program = newest_goto_program(&target_folder(root).join("kani"), harness, started)?;
            let shown = Command::new(&goto_instrument)
                .arg("--show-goto-functions")
                .arg(&program)
                .output()?;
            ensure!(
                shown.status.success(),
                "goto-instrument failed on {}",
                program.display()
            );
            let settings = format!(
                "{}\n{}\n",
                check.base.join(" "),
                harness_attributes(&program, harness)?
            );
            found.push((
                harness.clone(),
                normalized_goto_fingerprint(&format!(
                    "{settings}{}",
                    String::from_utf8_lossy(&shown.stdout)
                )),
            ));
        }
    }
    Ok(found)
}

fn harness_attributes(program: &Path, harness: &str) -> Result<String> {
    let folder = program.parent().context("a goto program has no folder")?;
    let pretty = format!("\"pretty_name\":\"{harness}\"");
    for entry in fs::read_dir(folder)? {
        let path = entry?.path();
        if !path.to_string_lossy().ends_with(".kani-metadata.json") {
            continue;
        }
        let text = unspaced(&fs::read_to_string(&path)?);
        let Some(at) = text.find(&pretty) else {
            continue;
        };
        let rest = &text[at..];
        let start = rest
            .find("\"attributes\"")
            .with_context(|| format!("{} gives {harness} no attributes", path.display()))?;
        let open = start
            + rest[start..]
                .find('{')
                .context("attributes is not an object")?;
        let mut depth = 0;
        for (index, c) in rest[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(rest[open..=open + index].to_owned());
                    }
                }
                _ => {}
            }
        }
    }
    anyhow::bail!(
        "no Kani metadata beside {} describes {harness}",
        program.display()
    )
}

pub(crate) fn print_goto_fingerprints(root: &Path, name: &str) -> Result<()> {
    let all = manifest(root)?;
    let verification = all
        .iter()
        .find(|verification| verification.name == name)
        .with_context(|| format!("no verification named {name:?} in {MANIFEST}"))?;
    for (harness, hash) in goto_fingerprints(root, verification)? {
        println!("goto-fingerprint = {harness} {hash}");
    }
    Ok(())
}

pub(crate) fn fail_on_goto_fingerprint_drift(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let (harnesses, _) = workspace_harnesses(root)?;
    for verification in manifest(root)? {
        problems.extend(goto_fingerprints_not_listed(&verification, &harnesses));
        let recorded: BTreeMap<&String, &String> = verification
            .goto_fingerprints
            .iter()
            .map(|(harness, hash)| (harness, hash))
            .collect();
        for (harness, hash) in goto_fingerprints(root, &verification)? {
            match recorded.get(&harness) {
                Some(&recorded) if *recorded == hash => {}
                Some(recorded) => problems.push(format!(
                    "{}: {harness} compiles to a different goto program ({hash}, recorded {recorded}); what it proves changed, so run `cargo xtask verify-sync {}`",
                    verification.name, verification.name
                )),
                None => problems.push(format!(
                    "{}: {harness} has no goto-fingerprint",
                    verification.name
                )),
            }
        }
    }
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    println!(
        "goto fingerprints: every recorded harness compiles to the goto program its proof last checked"
    );
    Ok(())
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
    let checks = section_kani_checks(verification);
    ensure!(!checks.is_empty(), "{name}: it has no Kani full-check");
    let (harnesses, _) = workspace_harnesses(root)?;
    let goto_instrument = kani_tool("goto-instrument")?;
    let mut problems = BTreeSet::new();
    for check in &checks {
        let names = harness_names(check, &harnesses);
        ensure!(
            !names.is_empty(),
            "{name}: a Kani full-check runs no harness"
        );
        let started = codegen(root, name, check, &names)?;
        for harness in &names {
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
        assert_eq!(named("#[kani::proof_for_contract(f)]"), 1, "{problems:#?}");
        assert_eq!(named("#[cfg_attr(kani, kani::proof)]"), 1, "{problems:#?}");
        assert_eq!(named("u::proofs::on_unix"), 1, "{problems:#?}");
        assert_eq!(named("inside another item"), 1, "{problems:#?}");
        assert_eq!(problems.len(), 6, "{problems:#?}");
    }

    #[test]
    fn a_proof_attribute_in_any_other_form_fails_by_line() {
        let cases = [
            ("use kani::proof;\n\n#[proof]\nfn a() {}\n", &[1, 3][..]),
            ("use kani::*;\n#[proof]\nfn a() {}\n", &[1, 2]),
            ("use ::kani::{proof as p};\n", &[1]),
            ("#[::kani::proof]\nfn a() {}\n", &[1]),
            ("# [kani :: proof]\nfn a() {}\n", &[1]),
            ("#[kani::proof] #[kani::unwind(9)] fn a() {}\n", &[1]),
            (
                "macro_rules! make {\n    ($name:ident) => {\n        #[kani::proof]\n        fn $name() {}\n    };\n}\n",
                &[3],
            ),
            ("#[kani::proof_for_contract(f)]\nfn a() {}\n", &[1]),
            ("#[cfg_attr(kani, kani::proof)]\nfn a() {}\n", &[1]),
            (
                "#[kani::proof]\n#[kani::unwind(3)]\nfn fine() {}\n#[doc = \"#[proof]\"]\nfn b() {}\n",
                &[],
            ),
        ];
        for (source, lines) in cases {
            let problems = proof_form_problems("src/m.rs", source);
            let found: Vec<usize> = problems
                .iter()
                .map(|problem| problem.split(':').nth(1).unwrap().parse().unwrap())
                .collect();
            assert_eq!(found, lines, "{source}: {problems:#?}");
        }
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

    const KANI_ONLY: &str = "fn parse() {}
#[cfg(kani)]
fn stand_in() {}
#[cfg(kani)]
mod proofs {
    const BOUND: usize = 2;
    fn helper() {}
    #[kani::proof]
    fn first() {
        helper();
    }
    #[kani::proof]
    #[kani::unwind(3)]
    fn second() {}
}
";

    fn kani_code_problems(sections: &[(&str, &str, &[&str])]) -> Vec<String> {
        let files = [("src/lib.rs", "mod m;\n"), ("src/m.rs", KANI_ONLY)];
        let paths: Vec<String> = files.iter().map(|(path, _)| (*path).to_owned()).collect();
        let (harnesses, problems) = discover_harnesses(&paths, sources(&files)).unwrap();
        assert!(problems.is_empty(), "{problems:#?}");
        let items: Vec<Vec<(String, String)>> = sections
            .iter()
            .map(|(_, _, openings)| {
                openings
                    .iter()
                    .map(|opening| ("src/m.rs".to_owned(), (*opening).to_owned()))
                    .collect()
            })
            .collect();
        let sections: Vec<Section> = sections
            .iter()
            .zip(&items)
            .map(|((name, check, _), items)| Section {
                name,
                checks: kani_check(check).into_iter().collect(),
                items,
            })
            .collect();
        kani_code_not_listed(&sections, &harnesses, sources(&files)).unwrap()
    }

    #[test]
    fn a_section_that_runs_kani_needs_its_reach_check() {
        let checks = |lines: &[&str]| {
            lines
                .iter()
                .map(|line| (*line).to_owned())
                .collect::<Vec<_>>()
        };
        let kani = "cargo kani --harness m::proofs::";
        assert!(reach_check_missing("m", &checks(&["cargo xtask kani-reach m", kani])).is_none());
        assert!(reach_check_missing("m", &checks(&["cargo test --lib m"])).is_none());
        let missing = reach_check_missing("m", &checks(&[kani, "cargo xtask kani-reach other"]));
        assert!(
            missing
                .is_some_and(|problem| problem.contains("`full-check = cargo xtask kani-reach m`"))
        );
    }

    #[test]
    fn every_proof_and_kani_only_item_is_listed_by_a_section_that_runs_it() {
        let whole = "cargo kani --harness m::proofs::";
        assert!(
            kani_code_problems(&[("all", whole, &["mod proofs {", "fn stand_in("])]).is_empty()
        );
        let problems = kani_code_problems(&[("all", whole, &["    fn first("])]);
        let named = |text: &str| problems.iter().filter(|p| p.contains(text)).count();
        assert_eq!(named("all: it runs m::proofs::second"), 1, "{problems:#?}");
        assert_eq!(
            named("`item = src/m.rs |     const BOUND:`"),
            1,
            "{problems:#?}"
        );
        assert_eq!(
            named("`item = src/m.rs |     fn helper(`"),
            1,
            "{problems:#?}"
        );
        assert_eq!(
            named("`item = src/m.rs | fn stand_in(`"),
            1,
            "{problems:#?}"
        );
        assert_eq!(
            named("or `item = src/m.rs | mod proofs {`"),
            3,
            "{problems:#?}"
        );
        assert_eq!(problems.len(), 4, "{problems:#?}");

        let first = "cargo kani --exact --harness m::proofs::first";
        let second = "cargo kani --exact --harness m::proofs::second";
        let split = |a: &'static [&'static str], b: &'static [&'static str]| {
            kani_code_problems(&[("a", first, a), ("b", second, b)])
        };
        assert!(
            split(
                &["    fn first(", "    fn helper(", "    const BOUND:"],
                &["    fn second(", "fn stand_in("]
            )
            .is_empty()
        );
        let unlisted = split(
            &["    fn first(", "    const BOUND:"],
            &["    fn second(", "fn stand_in("],
        );
        assert_eq!(unlisted.len(), 1, "{unlisted:#?}");
        assert!(
            unlisted[0].contains("add `item = src/m.rs |     fn helper(` to whichever of a, b"),
            "{unlisted:#?}"
        );
        let unproved = split(
            &["    fn helper(", "    const BOUND:"],
            &["    fn second(", "fn stand_in("],
        );
        assert_eq!(unproved.len(), 1, "{unproved:#?}");
        assert!(
            unproved[0].starts_with("a: it runs m::proofs::first"),
            "{unproved:#?}"
        );
        assert!(kani_code_problems(&[("tests", "cargo test --lib m", &[])]).is_empty());
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

    const MEMORY: &str = "pub(crate) const READ_CHUNK: usize = 4096;\npub(crate) const READ_SLACK: usize = 64;\npub(crate) const fn fast_read_chunk() -> usize {\n    4096\n}\npub(crate) struct ReadChunk;\n\nimpl ReadChunk {\n    pub(crate) const BYTES: usize = 4096;\n}\npub(crate) use crate::progress::SHARED;\npub(in crate::m) const NARROW: usize = 1;\n";
    const PROGRESS: &str =
        "pub(crate) const SHARED: usize = 1;\npub(crate) const OTHER: usize = 2;\n";

    fn reach_through(imports: &str, value: &str) -> Vec<String> {
        let m = format!("{imports}\n{STEP}const TOP: u32 = {value} as u32;\n");
        let step_line = imports.lines().count() + 3;
        let files = [
            ("src/m.rs", m.as_str()),
            ("src/memory.rs", MEMORY),
            ("src/progress.rs", PROGRESS),
        ];
        unlisted_in(&files, &STEP_ITEMS, &[step_line])
    }

    #[test]
    fn imports_resolve_through_aliases_and_nested_groups() {
        assert_eq!(
            reach_through("use crate::memory::fast_read_chunk;", "fast_read_chunk()"),
            ["item = src/memory.rs | pub(crate) const fn fast_read_chunk("]
        );
        assert_eq!(
            reach_through("use crate::memory::READ_CHUNK as CHUNK;", "CHUNK"),
            ["item = src/memory.rs | pub(crate) const READ_CHUNK:"]
        );
        assert_eq!(
            reach_through(
                "use crate::{memory::{READ_CHUNK, READ_SLACK}, progress::OTHER};",
                "READ_CHUNK + READ_SLACK + OTHER"
            ),
            [
                "item = src/memory.rs | pub(crate) const READ_CHUNK:",
                "item = src/memory.rs | pub(crate) const READ_SLACK:",
                "item = src/progress.rs | pub(crate) const OTHER:",
            ]
        );
        assert_eq!(
            reach_through("", "crate::memory::NARROW"),
            ["item = src/memory.rs | pub(in crate::m) const NARROW:"]
        );
    }

    fn refused(problems: &[String], text: &str) -> bool {
        problems.len() == 1 && problems[0].contains(text)
    }

    #[test]
    fn a_file_that_includes_code_is_refused_when_a_name_is_looked_up_in_it() {
        let m = format!("{STEP}const TOP: u32 = crate::other::INCLUDED;\n");
        let other = "pub(crate) const SEEN: u32 = 1;\ninclude!(\"/elsewhere/included.rs\");\n";
        let files = [("src/m.rs", m.as_str()), ("src/other.rs", other)];
        let problems = unlisted_in(&files, &STEP_ITEMS, &[2]);
        assert!(
            problems
                .iter()
                .any(|problem| problem.starts_with("src/other.rs:2: `include!` brings in code")),
            "{problems:#?}"
        );
    }

    #[test]
    fn forms_the_reach_check_cannot_follow_are_refused_by_line() {
        let cases = [
            (
                "use crate::memory;",
                "memory::READ_CHUNK",
                "src/m.rs:5: `memory::READ_CHUNK` goes through the module imported at line 1",
            ),
            (
                "use crate::memory::ReadChunk;",
                "ReadChunk::BYTES",
                "src/m.rs:5: `ReadChunk::BYTES` names an item of `ReadChunk`, imported at line 1 from another file",
            ),
            (
                "",
                "crate::memory::ReadChunk::BYTES",
                "is an associated item of a type in src/memory.rs",
            ),
            (
                "use crate::memory::SHARED;",
                "SHARED",
                "crate::memory::SHARED is re-exported from src/memory.rs",
            ),
            (
                "use crate::memory::MISSING;",
                "MISSING",
                "crate::memory::MISSING resolves to no definition (imported at line 1)",
            ),
            (
                "",
                "crate::memory::MISSING",
                "crate::memory::MISSING resolves to no definition",
            ),
            (
                "use crate::memory::*;",
                "READ_CHUNK",
                "src/m.rs:1: `use crate::memory::*` imports names",
            ),
            (
                "include!(\"other.rs\");",
                "1",
                "src/m.rs:1: `include!` brings in code",
            ),
        ];
        for (imports, value, expected) in cases {
            let problems = reach_through(imports, value);
            assert!(
                refused(&problems, expected),
                "{imports} / {value}: {problems:#?}"
            );
        }
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
    fn derived_code_needs_the_item_it_is_derived_for() {
        let source = "#[derive(Clone, Copy, PartialEq)]\n#[repr(u8)]\nenum Kind {\n    One(Width),\n}\nstruct Width(u8);\n";
        let files = [("src/m.rs", source)];
        for line in [1, 4] {
            assert_eq!(
                unlisted_in(&files, &[], &[line]),
                [
                    "item = src/m.rs | enum Kind {",
                    "item = src/m.rs | struct Width("
                ],
                "line {line}"
            );
            let listed = [("src/m.rs", "enum Kind {"), ("src/m.rs", "struct Width(")];
            assert!(unlisted_in(&files, &listed, &[line]).is_empty());
        }
    }

    #[test]
    fn a_variant_of_an_enum_imported_from_another_file_needs_the_enum() {
        let other = "pub(crate) enum Extra {\n    Absent,\n    Present(u8),\n}\nimpl Extra {\n    pub(crate) const NONE: u8 = 0;\n}\n";
        let m = format!(
            "use crate::other::Extra;\n{STEP}const TOP: u32 = match Extra::Present(1) {{\n    Extra::Absent => 0,\n    _ => 1,\n}};\n"
        );
        let files = [("src/m.rs", m.as_str()), ("src/other.rs", other)];
        assert_eq!(
            unlisted_in(&files, &STEP_ITEMS, &[3]),
            ["item = src/other.rs | pub(crate) enum Extra {"]
        );
        let listed = [
            STEP_ITEMS[0],
            STEP_ITEMS[1],
            ("src/other.rs", "pub(crate) enum Extra {"),
        ];
        assert!(unlisted_in(&files, &listed, &[3]).is_empty());
        let associated =
            format!("use crate::other::Extra;\n{STEP}const TOP: u32 = Extra::NONE as u32;\n");
        let files = [("src/m.rs", associated.as_str()), ("src/other.rs", other)];
        assert!(refused(
            &unlisted_in(&files, &listed, &[3]),
            "`Extra::NONE` names an item of `Extra`, imported at line 1 from another file"
        ));
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
    fn goto_fingerprints_ignore_locations_and_crate_hashes_but_not_code() {
        let program = "Reading GOTO program from 'a/b.out'\nf /* _RNvCs25vTPsMbSID_6zipper1f */\n        // 3 file src/m.rs line 10 column 2 function f\n        ASSIGN x := 1\n";
        let moved = "Reading GOTO program from 'c/d.out'\nf /* _RNvCsZZZ9_6zipper1f */\n        // 3 file src/m.rs line 99 column 7 function f\n        ASSIGN x := 1\n";
        let changed = program.replace("x := 1", "x := 2");
        assert_eq!(
            normalized_goto_fingerprint(program),
            normalized_goto_fingerprint(moved)
        );
        assert_ne!(
            normalized_goto_fingerprint(program),
            normalized_goto_fingerprint(&changed)
        );
        assert_eq!(
            without_crate_disambiguators("Cs25vTPsMbSID_6zipper Csx Cs_1"),
            "Cs_6zipper Csx Cs_1"
        );
        assert_eq!(
            without_crate_disambiguators("address_of(zipper.1854caf7989fb411::zipper::x)"),
            "address_of(zipper.::zipper::x)"
        );
        let first = "x := f(AllocId(198, T))\ny := f(AllocId(7, T))\nz := AllocId(198, T)\n";
        let renumbered = "x := f(AllocId(193, T))\ny := f(AllocId(2, T))\nz := AllocId(193, T)\n";
        let swapped = "x := f(AllocId(193, T))\ny := f(AllocId(2, T))\nz := AllocId(2, T)\n";
        assert_eq!(
            normalized_goto_fingerprint(first),
            normalized_goto_fingerprint(renumbered)
        );
        assert_ne!(
            normalized_goto_fingerprint(first),
            normalized_goto_fingerprint(swapped)
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
