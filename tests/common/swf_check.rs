#![allow(dead_code)]
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub type Tree = BTreeMap<String, String>;

pub fn sevenzip_binary() -> Option<PathBuf> {
    super::sevenzip::sevenzip()
}

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn tree(root: &Path) -> Tree {
    if !root.exists() {
        return Tree::new();
    }
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let kind = match entry.file_type().is_dir() {
                true => "dir".to_owned(),
                false => sha256(&fs::read(entry.path()).unwrap()),
            };
            (name, kind)
        })
        .collect()
}

pub fn zipper_tree(archive: &Path, into: &Path) -> Result<Tree, String> {
    let options = zipper::ExtractOptions::default();
    match zipper::extract_archive(archive, into, options) {
        Ok(()) => Ok(tree(into)),
        Err(error) => {
            assert!(
                !into.exists(),
                "{}: published after a failure",
                archive.display()
            );
            Err(format!("{error:#}"))
        }
    }
}

pub fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<Tree> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

pub struct Outcome {
    pub zipper: Result<Tree, String>,
    pub sevenzip: Option<Tree>,
}

pub fn run_both(binary: &Path, bytes: &[u8], name: &str) -> Outcome {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join(name);
    fs::write(&archive, bytes).unwrap();
    Outcome {
        zipper: zipper_tree(&archive, &work.path().join("zipper")),
        sevenzip: sevenzip_tree(binary, &archive, &work.path().join("7z")),
    }
}

pub fn judge(
    binary: &Path,
    bytes: &[u8],
    name: &str,
    label: &str,
    stricter: &[&str],
) -> Option<String> {
    let outcome = run_both(binary, bytes, name);
    match (&outcome.zipper, &outcome.sevenzip) {
        (Err(_), None) => None,
        (Ok(ours), None) => Some(format!(
            "{label}: 7-Zip refused and Zipper extracted {ours:?}"
        )),
        (Err(error), Some(_)) if super::sevenzip::zipper_refused_a_name_windows_forbids(error) => {
            None
        }
        (Err(error), Some(theirs)) => match stricter.iter().find(|known| error.contains(*known)) {
            Some(_) => None,
            None => Some(format!(
                "{label}: Zipper refused with {error:?} and 7-Zip extracted {theirs:?}"
            )),
        },
        (Ok(ours), Some(theirs))
            if super::sevenzip::spelled_through_the_windows_code_page(
                ours.keys().chain(theirs.keys()),
            ) =>
        {
            None
        }
        (Ok(ours), Some(theirs)) => (ours != theirs)
            .then(|| format!("{label}: trees differ\n  zipper {ours:?}\n  7-Zip  {theirs:?}")),
    }
}

pub fn report(problems: Vec<String>, checked: usize, least: usize) {
    assert!(checked >= least, "{checked}");
    assert!(
        problems.is_empty(),
        "{} of {checked} disagree:\n{}",
        problems.len(),
        problems.join("\n")
    );
}
