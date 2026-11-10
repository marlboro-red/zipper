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

fn git_in(root: &Path) -> Command {
    let mut git = Command::new("git");
    git.current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    git
}

fn names_commit(root: &Path, word: &str) -> bool {
    git_in(root)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{word}^{{commit}}"),
        ])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn is_ancestor(root: &Path, word: &str, tip: &str) -> bool {
    git_in(root)
        .args(["merge-base", "--is-ancestor", word, tip])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn unknown_hashes(message: &str, root: &Path, tip: &str) -> Vec<String> {
    let mut unknown: Vec<String> = message
        .lines()
        .filter(|line| !line.starts_with('#'))
        .flat_map(|line| line.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|word| {
            looks_like_hash(word) && !(names_commit(root, word) && is_ancestor(root, word, tip))
        })
        .map(str::to_owned)
        .collect();
    unknown.dedup();
    unknown
}

fn report(unknown: &[String], subject: &str) -> Result<()> {
    ensure!(
        unknown.is_empty(),
        "{subject} cites {} in no commit it descends from: {}. \
         Copy hashes from `git log --oneline`, never type them from memory, \
         and copy them again after a rebase rewrites them",
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
        &unknown_hashes(&fs::read_to_string(path)?, root, "HEAD"),
        "the commit message",
    )
}

pub fn lint_unpushed(root: &Path) -> Result<()> {
    let range = git_in(root)
        .args(["rev-list", "HEAD", "--not", "--remotes"])
        .output()?;
    ensure!(
        range.status.success(),
        "cannot list commits missing from every remote"
    );
    for commit in String::from_utf8(range.stdout)?.lines() {
        let message = git_in(root)
            .args(["log", "-1", "--format=%B", commit])
            .output()?;
        report(
            &unknown_hashes(&String::from_utf8(message.stdout)?, root, commit),
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
        assert_eq!(unknown_hashes(&message, root, "HEAD"), ["0000abc"]);
    }

    #[test]
    fn a_hash_off_the_citing_commits_history_is_reported() {
        let work = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = git_in(work.path())
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = git(&["rev-parse", "--short=7", "HEAD"]);
        git(&["commit", "-q", "--allow-empty", "-m", "side"]);
        let side = git(&["rev-parse", "--short=7", "HEAD"]);
        git(&["reset", "-q", "--hard", &base]);
        git(&["commit", "-q", "--allow-empty", "-m", "main"]);
        let message = format!("cite {base} and {side}\n");
        assert_eq!(unknown_hashes(&message, work.path(), "HEAD"), [side]);
    }
}
