use anyhow::{Result, ensure};
use std::{fs, path::Path};

const TAGS: &[&str] = &["SAFETY:", "SPEC(", "EXTERNAL(", "RECORD(", "LINK:"];
const MAX_LINES: usize = 3;

pub fn lint(root: &Path) -> Result<()> {
    let mut problems = Vec::new();
    for dir in ["src", "tests", "xtask/src"] {
        for entry in walkdir::WalkDir::new(root.join(dir)).sort_by_file_name() {
            let entry = entry?;
            if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "rs") {
                let name = entry.path().strip_prefix(root)?.display().to_string();
                for (line, message) in lint_source(&fs::read_to_string(entry.path())?, root) {
                    problems.push(format!("{name}:{line}: {message}"));
                }
            }
        }
    }
    for problem in &problems {
        eprintln!("{problem}");
    }
    ensure!(
        problems.is_empty(),
        "{} comment problem(s); see the zipper-comments agent for the keep-list",
        problems.len()
    );
    Ok(())
}

fn line_comment(line: &str) -> Option<&str> {
    let text = line.trim_start().strip_prefix("//")?;
    (!text.starts_with('/') && !text.starts_with('!')).then(|| text.trim())
}

fn trailing_comment(line: &str) -> bool {
    line.match_indices(" // ").any(|(at, _)| {
        let before = &line[..at];
        let quotes = before.matches('"').count() - before.matches("\\\"").count();
        !before.trim().is_empty() && quotes.is_multiple_of(2)
    })
}

fn lint_source(source: &str, root: &Path) -> Vec<(usize, String)> {
    let mut problems = Vec::new();
    let lines: Vec<&str> = source.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        if trailing_comment(lines[index]) {
            problems.push((index + 1, "comment after code".to_owned()));
        }
        let Some(first) = line_comment(lines[index]) else {
            index += 1;
            continue;
        };
        let start = index;
        while index < lines.len() && line_comment(lines[index]).is_some() {
            index += 1;
        }
        match TAGS.iter().find(|tag| first.starts_with(**tag)) {
            None => problems.push((
                start + 1,
                format!("untagged comment; start it with one of {TAGS:?} or delete it"),
            )),
            Some(&"RECORD(") => {
                let path = first["RECORD(".len()..].split(')').next().unwrap_or("");
                if !path.starts_with("benchmarks/") || !root.join(path).exists() {
                    problems.push((start + 1, format!("RECORD names no record: {path}")));
                }
            }
            Some(_) => {}
        }
        if index - start > MAX_LINES {
            problems.push((
                start + 1,
                format!(
                    "comment of {} lines; keep it to {MAX_LINES} and move the rest to the commit or record",
                    index - start
                ),
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problems(source: &str) -> usize {
        lint_source(
            source,
            Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap(),
        )
        .len()
    }

    #[test]
    fn only_tagged_short_comments_pass() {
        assert_eq!(problems("// SAFETY: the pointer is live.\nunsafe {}"), 0);
        assert_eq!(problems("// SPEC(APPNOTE 4.5.3): both sizes.\n"), 0);
        assert_eq!(
            problems("/// Doc comments are judged by review.\n//! So are these.\n"),
            0
        );
        assert_eq!(problems("// Narration.\nlet x = 1;"), 1);
        assert_eq!(problems("let x = 1; // narration"), 1);
        assert_eq!(problems("let url = \"http://a // b\";"), 0);
        assert_eq!(problems("// EXTERNAL(unzip): a\n// b\n// c\n// d\n"), 1);
        assert_eq!(problems("// RECORD(benchmarks/missing): measured.\n"), 1);
        assert_eq!(problems("// RECORD(benchmarks/README.md): measured.\n"), 0);
    }
}
