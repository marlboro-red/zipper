use anyhow::{Result, ensure};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

const DEFERRAL: &[&str] = &[
    "todo",
    "tbd",
    "fixme",
    "later",
    "eventually",
    "someday",
    "placeholder",
    "followup",
    "followups",
];

const SLOP: &[&str] = &[
    "additionally",
    "crucial",
    "delve",
    "enhance",
    "enhances",
    "fostering",
    "garner",
    "harness",
    "interplay",
    "intricate",
    "landscape",
    "leverage",
    "leverages",
    "facilitate",
    "facilitates",
    "numerous",
    "paradigm",
    "pivotal",
    "seamless",
    "seamlessly",
    "showcase",
    "simply",
    "substrate",
    "tapestry",
    "testament",
    "underscore",
    "underscores",
    "utilize",
    "utilizes",
    "vibrant",
];

const SLOP_PHRASES: &[&str] = &[
    "for now",
    "follow up",
    "in order to",
    "it is important to note",
    "north star",
    "due to the fact",
];

const MAX_SENTENCE_WORDS: usize = 35;

const RECORD_HEADINGS: &[&str] = &[
    "## Question",
    "## Setup",
    "## Results",
    "## Decision",
    "## Limits",
    "## Reproduce",
];

pub fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = vec![root.join("README.md"), root.join("AGENTS.md")];
    for dir in ["docs", "benchmarks", ".claude/skills"] {
        for entry in walkdir::WalkDir::new(root.join(dir)).sort_by_file_name() {
            let entry = entry?;
            if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "md") {
                files.push(entry.into_path());
            }
        }
    }
    Ok(files)
}

pub fn lint(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    let files = files(root)?;
    let log = root.join("benchmarks/README.md");
    for file in &files {
        let text = fs::read_to_string(file)?;
        let name = file.strip_prefix(root)?.display().to_string();
        for (line, message) in lint_text(&text, file.parent().unwrap_or(root)) {
            problems.push(format!("{name}:{line}: {message}"));
        }
        for (line, message) in named_paths(&text, root) {
            problems.push(format!("{name}:{line}: {message}"));
        }
        if file.starts_with(root.join("benchmarks")) && *file != log {
            let headings = record_headings(&text);
            if headings != RECORD_HEADINGS {
                problems.push(format!(
                    "{name}: experiment record headings are {headings:?}, expected {RECORD_HEADINGS:?}"
                ));
            }
        }
    }
    let reached = reachable(&root.join("README.md"))?;
    for file in &files {
        if !reached.contains(&fs::canonicalize(file)?) {
            let name = file.strip_prefix(root)?.display();
            problems.push(format!(
                "{name}: no link path from README.md reaches this page"
            ));
        }
    }
    for problem in &problems {
        eprintln!("{problem}");
    }
    ensure!(
        problems.is_empty(),
        "{} documentation problem(s)",
        problems.len()
    );
    Ok(())
}

const XTASK_COMMANDS: &[&str] = &[
    "gate",
    "lint-docs",
    "lint-comments",
    "install-hooks",
    "ci-wait",
    "ship",
    "lint-commit-msg",
    "lint-commits",
    "verify-sync",
    "bench",
];

const REPO_DIRS: &[&str] = &[
    "scripts/",
    "src/",
    "docs/",
    "benchmarks/",
    "xtask/",
    "tests/",
    ".claude/",
    ".githooks/",
];

fn named_paths(text: &str, root: &Path) -> Vec<(usize, String)> {
    let mut problems = Vec::new();
    let mut fenced = false;
    for (index, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if fenced {
            continue;
        }
        for span in line.split('`').skip(1).step_by(2) {
            if let Some(rest) = span.strip_prefix("cargo xtask ") {
                let command = rest.split_whitespace().next().unwrap_or_default();
                if !XTASK_COMMANDS.contains(&command) {
                    problems.push((
                        index + 1,
                        format!("unknown command `cargo xtask {command}`"),
                    ));
                }
            }
            for word in span.split_whitespace() {
                let path = word.trim_end_matches([',', '.', ':', ')']);
                let checkable = REPO_DIRS.iter().any(|dir| path.starts_with(dir))
                    && !path.contains(['<', '*', '{', '$']);
                if checkable && !root.join(path).exists() {
                    problems.push((index + 1, format!("names missing path `{path}`")));
                }
            }
        }
    }
    problems
}

fn record_headings(text: &str) -> Vec<&str> {
    let mut fenced = false;
    text.lines()
        .filter(|line| {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
            }
            !fenced && line.starts_with("## ")
        })
        .map(str::trim_end)
        .collect()
}

fn reachable(start: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![fs::canonicalize(start)?];
    while let Some(page) = pending.pop() {
        if !seen.insert(page.clone()) {
            continue;
        }
        let dir = page.parent().unwrap_or(&page).to_path_buf();
        let mut fenced = false;
        for line in fs::read_to_string(&page)?.lines() {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
            }
            if fenced {
                continue;
            }
            for target in link_targets(line) {
                let path = target.split('#').next().unwrap_or_default();
                if path.ends_with(".md")
                    && !target.contains("://")
                    && let Ok(found) = fs::canonicalize(dir.join(path))
                {
                    pending.push(found);
                }
            }
        }
    }
    Ok(seen)
}

fn lint_text(text: &str, dir: &Path) -> Vec<(usize, String)> {
    let mut problems = Vec::new();
    let mut fenced = false;
    let mut paragraph: Vec<(usize, String)> = Vec::new();
    let frontmatter = if text.lines().next() == Some("---") {
        text.lines()
            .skip(1)
            .position(|l| l == "---")
            .map_or(0, |end| end + 2)
    } else {
        0
    };
    for (index, raw) in text.lines().enumerate().skip(frontmatter) {
        let line = index + 1;
        if raw.trim_start().starts_with("```") {
            fenced = !fenced;
            flush(&mut paragraph, &mut problems);
            continue;
        }
        if fenced {
            continue;
        }
        for target in link_targets(raw) {
            if !target_exists(dir, &target) {
                problems.push((line, format!("broken link `{target}`")));
            }
        }
        let prose = prose(raw);
        if prose_dash(&prose) {
            problems.push((line, "long dash; use a period, a comma, or \"to\"".into()));
        }
        if prose.contains(['\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}']) {
            problems.push((line, "curly quote".into()));
        }
        let words = words(&prose);
        for word in &words {
            if DEFERRAL.contains(&word.as_str()) {
                problems.push((line, format!("deferral word \"{word}\"")));
            } else if SLOP.contains(&word.as_str()) {
                problems.push((line, format!("plain-language word list rejects \"{word}\"")));
            }
        }
        let joined = format!(" {} ", words.join(" "));
        for phrase in SLOP_PHRASES {
            if joined.contains(&format!(" {phrase} ")) {
                problems.push((line, format!("phrase \"{phrase}\"")));
            }
        }
        let trimmed = raw.trim();
        let table = trimmed.starts_with('|');
        if !table && mid_sentence_colon(&prose) {
            problems.push((line, "mid-sentence colon; split the sentence".into()));
        }
        let block_start = trimmed.is_empty()
            || table
            || trimmed.starts_with('#')
            || trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed
                .split_once(". ")
                .is_some_and(|(n, _)| n.parse::<u32>().is_ok());
        if block_start {
            flush(&mut paragraph, &mut problems);
        }
        if !trimmed.is_empty() && !table && !trimmed.starts_with('#') {
            paragraph.push((line, prose));
        }
    }
    flush(&mut paragraph, &mut problems);
    problems.sort();
    problems
}

fn flush(paragraph: &mut Vec<(usize, String)>, problems: &mut Vec<(usize, String)>) {
    let Some(&(start, _)) = paragraph.first() else {
        return;
    };
    let text: Vec<&str> = paragraph.iter().map(|(_, prose)| prose.as_str()).collect();
    let text = text.join(" ");
    let mut count = 0;
    for word in text.split_whitespace() {
        count += 1;
        if word
            .trim_end_matches(['*', '_', ')', '"'])
            .ends_with(['.', '?', '!'])
        {
            if count > MAX_SENTENCE_WORDS {
                problems.push((start, format!("sentence of {count} words; split it")));
            }
            count = 0;
        }
    }
    if count > MAX_SENTENCE_WORDS {
        problems.push((start, format!("sentence of {count} words; split it")));
    }
    paragraph.clear();
}

fn prose(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            in_code = !in_code;
            out.push_str(if in_code { "CODE" } else { "" });
            continue;
        }
        if in_code {
            continue;
        }
        if c == ']' && chars.peek() == Some(&'(') {
            out.push(']');
            for skipped in chars.by_ref() {
                if skipped == ')' {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out.split_whitespace()
        .filter(|word| !word.contains("://"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn prose_dash(prose: &str) -> bool {
    let chars: Vec<char> = prose.chars().collect();
    chars.iter().enumerate().any(|(at, &c)| {
        let around = |offset: isize| {
            at.checked_add_signed(offset)
                .and_then(|i| chars.get(i))
                .copied()
                .unwrap_or(' ')
        };
        let range = around(-1).is_ascii_digit() && around(1).is_ascii_digit();
        let empty_cell = around(-1) == ' ' && around(1) == ' ' && {
            let rest: String = chars[at + 1..].iter().collect();
            rest.trim_start().starts_with('|')
        };
        matches!(c, '\u{2014}' | '\u{2013}') && !range && !empty_cell
    })
}

fn words(prose: &str) -> Vec<String> {
    prose
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .map(|word| word.replace('-', "").to_lowercase())
        .filter(|word| !word.is_empty())
        .collect()
}

fn mid_sentence_colon(prose: &str) -> bool {
    let bytes = prose.as_bytes();
    prose.match_indices(": ").any(|(at, _)| {
        let before = prose[..at].trim_start_matches(['-', '*', ' ']);
        let label = before.trim_matches('*');
        let restates_label = !label.is_empty() && !label.contains(' ') && before.starts_with("**");
        !restates_label && bytes.get(at + 2).is_some_and(|b| !b.is_ascii_whitespace())
    })
}

fn link_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut code = false;
    let mut rest = line;
    while let Some(at) = rest.find(['`', ']']) {
        let (marker, after) = (rest.as_bytes()[at], &rest[at + 1..]);
        if marker == b'`' {
            code = !code;
        } else if !code
            && after.starts_with('(')
            && let Some(end) = after.find(')')
        {
            targets.push(after[1..end].trim().to_string());
        }
        rest = after;
    }
    targets
}

fn target_exists(dir: &Path, target: &str) -> bool {
    if target.contains("://") || target.starts_with("mailto:") || target.starts_with('#') {
        return true;
    }
    let path = target.split('#').next().unwrap_or(target);
    dir.join(path).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<String> {
        lint_text(text, Path::new("."))
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    #[test]
    fn flags_each_rule_once() {
        assert_eq!(messages("A fast path \u{2014} sometimes.").len(), 1);
        assert_eq!(messages("Says 1 \u{2013} 2.").len(), 1);
        assert_eq!(messages("Say \u{201c}hi\u{201d}.").len(), 1);
        assert_eq!(messages("We fix this later.").len(), 1);
        assert_eq!(messages("We utilize buffers.").len(), 1);
        assert_eq!(messages("Keep it for now.").len(), 1);
        assert_eq!(messages("Limitations: only bytes.").len(), 1);
        assert_eq!(messages(&"word ".repeat(36)).len(), 1);
        assert_eq!(messages("See [x](missing.md).").len(), 1);
    }

    #[test]
    fn ignores_code_links_tables_and_list_introductions() {
        let clean = "Run `a: b \u{2014} later`.\n\n```sh\nlater: utilize\n```\n\
            | a: b | 1\u{2013}2 | \u{2014} |\n\nThe gate runs:\n\n- **Size.** It shrinks.\n\
            See [the format](https://example.com/a:b).\n";
        assert_eq!(messages(clean), Vec::<String>::new());
    }

    #[test]
    fn named_paths_and_commands_must_exist() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir(dir.path().join("scripts"))?;
        fs::write(dir.path().join("scripts/real.py"), "")?;
        let text = "Run `python3 scripts/real.py`, `scripts/gone.py`, `cargo xtask gate fast`, \
            `cargo xtask fly` and `benchmarks/<area>/README.md`.\n```sh\nscripts/fenced.py\n```\n";
        let found: Vec<String> = named_paths(text, dir.path())
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        assert_eq!(
            found,
            [
                "names missing path `scripts/gone.py`",
                "unknown command `cargo xtask fly`"
            ]
        );
        Ok(())
    }

    #[test]
    fn record_headings_ignore_code_blocks() {
        let text = "# T\n\n## Question\n```md\n## Not a heading\n```\n## Setup  \n";
        assert_eq!(record_headings(text), ["## Question", "## Setup"]);
    }

    #[test]
    fn reachability_follows_relative_markdown_links() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir(dir.path().join("docs"))?;
        fs::write(
            dir.path().join("README.md"),
            "[a](docs/a.md) [web](https://x.dev/b.md)",
        )?;
        fs::write(dir.path().join("docs/a.md"), "[back](../README.md#top)")?;
        fs::write(dir.path().join("docs/orphan.md"), "")?;
        let reached = reachable(&dir.path().join("README.md"))?;
        assert!(reached.contains(&fs::canonicalize(dir.path().join("docs/a.md"))?));
        assert!(!reached.contains(&fs::canonicalize(dir.path().join("docs/orphan.md"))?));
        Ok(())
    }

    #[test]
    fn skips_yaml_frontmatter() {
        assert_eq!(
            messages("---\nname: x\ndescription: a: b\n---\n# T\n"),
            Vec::<String>::new()
        );
        assert_eq!(
            messages("---\r\nname: x\r\n---\r\n# T\r\n"),
            Vec::<String>::new()
        );
        assert_eq!(messages("# T\n\nLabel: text here.\n").len(), 1);
    }

    #[test]
    fn sentences_end_inside_emphasis_and_brackets() {
        let words = "word ".repeat(20);
        for close in ["**", "*", "_", ")", "\""] {
            let text = format!("**{words}end.{close} {words}end.\n");
            assert_eq!(messages(&text), Vec::<String>::new(), "closing {close}");
        }
    }

    #[test]
    fn splits_paragraphs_at_list_items() {
        let text = format!("{}\n- {}\n", "word ".repeat(30), "word ".repeat(30));
        assert_eq!(messages(&text), Vec::<String>::new());
    }
}
