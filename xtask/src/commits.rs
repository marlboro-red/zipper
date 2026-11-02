use anyhow::{Result, ensure};
use std::{fs, path::Path, process::Command};

const SHORTEST_HASH: usize = 7;
const LONGEST_HASH: usize = 40;

fn looks_like_hash(word: &str) -> bool {
    (SHORTEST_HASH..=LONGEST_HASH).contains(&word.len())
        && word
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && word.bytes().any(|byte| byte.is_ascii_digit())
        && word.bytes().any(|byte| byte.is_ascii_alphabetic())
}

fn names_commit(root: &Path, word: &str) -> bool {
    Command::new("git")
        .current_dir(root)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{word}^{{commit}}"),
        ])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn unknown_hashes(message: &str, root: &Path) -> Vec<String> {
    let mut unknown: Vec<String> = message
        .lines()
        .filter(|line| !line.starts_with('#'))
        .flat_map(|line| line.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|word| looks_like_hash(word) && !names_commit(root, word))
        .map(str::to_owned)
        .collect();
    unknown.dedup();
    unknown
}

fn report(unknown: &[String], subject: &str) -> Result<()> {
    ensure!(
        unknown.is_empty(),
        "{subject} cites {} in no commit of this repository: {}. \
         Copy hashes from `git log --oneline`; never type them from memory",
        if unknown.len() == 1 {
            "a hash found"
        } else {
            "hashes found"
        },
        unknown.join(", ")
    );
    Ok(())
}

pub fn lint_message_file(root: &Path, path: &Path) -> Result<()> {
    report(
        &unknown_hashes(&fs::read_to_string(path)?, root),
        "the commit message",
    )
}

pub fn lint_unpushed(root: &Path) -> Result<()> {
    let range = Command::new("git")
        .current_dir(root)
        .args(["rev-list", "@{upstream}..HEAD"])
        .output()?;
    ensure!(
        range.status.success(),
        "HEAD has no upstream to compare with"
    );
    for commit in String::from_utf8(range.stdout)?.lines() {
        let message = Command::new("git")
            .current_dir(root)
            .args(["log", "-1", "--format=%B", commit])
            .output()?;
        report(
            &unknown_hashes(&String::from_utf8(message.stdout)?, root),
            &format!("commit {}", &commit[..SHORTEST_HASH]),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hashes_that_name_no_commit_are_reported() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let real = String::from_utf8(
            Command::new("git")
                .current_dir(root)
                .args(["rev-parse", "--short=7", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let message = format!(
            "fix: cite {} and 0000abc\n\nRun 41373591477, word facade, Abc1234.\n# 1111bbb is a comment\n",
            real.trim()
        );
        assert_eq!(unknown_hashes(&message, root), ["0000abc"]);
    }
}
