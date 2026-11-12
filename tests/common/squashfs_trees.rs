#![allow(dead_code)]
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub type Tree = BTreeMap<String, String>;

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn mode_and_time(metadata: &fs::Metadata, times: bool) -> String {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mode = metadata.permissions().mode() & 0o7777;
    if times {
        format!(" {mode:o} {}", metadata.mtime())
    } else {
        format!(" {mode:o}")
    }
}

#[cfg(not(unix))]
fn mode_and_time(_: &fs::Metadata, _: bool) -> String {
    String::new()
}

pub fn open_up(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        let wanted = if entry.file_type().is_dir() {
            0o700
        } else if entry.file_type().is_file() {
            0o400
        } else {
            continue;
        };
        let mode = entry.metadata().unwrap().permissions().mode();
        if mode & wanted != wanted {
            let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode | wanted));
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

pub fn tree(root: &Path, times: bool) -> Tree {
    let mut found = Tree::new();
    if !root.exists() {
        return found;
    }
    let metadata: BTreeMap<PathBuf, fs::Metadata> = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            (
                entry.path().to_owned(),
                fs::symlink_metadata(entry.path()).unwrap(),
            )
        })
        .collect();
    open_up(root);
    for entry in walkdir::WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let entry = entry.unwrap();
        let name = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let kept = &metadata[entry.path()];
        let kind = if entry.file_type().is_symlink() {
            format!(
                "link:{}",
                fs::read_link(entry.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/")
            )
        } else if entry.file_type().is_dir() {
            format!("dir{}", mode_and_time(kept, times))
        } else if !entry.file_type().is_file() {
            "special".to_owned()
        } else {
            format!(
                "{}{}",
                sha256(&fs::read(entry.path()).unwrap()),
                mode_and_time(kept, times)
            )
        };
        found.insert(name, kind);
    }
    found
}

pub fn contents(tree: &Tree) -> Tree {
    tree.iter()
        .map(|(name, kind)| (name.clone(), kind.split(' ').next().unwrap().to_owned()))
        .collect()
}

pub fn sevenzip_run(binary: &Path, image: &Path, into: &Path, times: bool) -> (bool, Tree, String) {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(image)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let output =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    (run.status.success(), tree(into, times), output)
}

pub fn sevenzip_accepts(binary: &Path, image: &Path) -> bool {
    Command::new(binary)
        .args(["t", "-y"])
        .arg(image)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn runs(tool: &Path) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

pub fn mksquashfs() -> Option<PathBuf> {
    Some(PathBuf::from("mksquashfs")).filter(|tool| runs(tool))
}

pub fn unsquashfs() -> Option<PathBuf> {
    Some(PathBuf::from("unsquashfs")).filter(|tool| runs(tool))
}

pub fn unsquashfs_tree(image: &Path, into: &Path) -> Option<Tree> {
    let tool = unsquashfs()?;
    Command::new(tool)
        .args(["-quiet", "-no-progress", "-no-xattrs", "-d"])
        .arg(into)
        .arg(image)
        .output()
        .unwrap();
    Some(contents(&tree(into, false)))
}

pub fn unsquashfs_difference(ours: &Tree, theirs: &Tree) -> Option<String> {
    let ours = contents(ours);
    for (name, kind) in theirs {
        if kind != "special" && ours.get(name) != Some(kind) {
            return Some(format!("{name} differs from unsquashfs"));
        }
    }
    let empty = sha256(b"");
    ours.iter()
        .find(|(name, kind)| !theirs.contains_key(*name) && **kind != empty)
        .map(|(name, _)| format!("unsquashfs did not write {name}"))
}

pub fn mkcramfs() -> Option<PathBuf> {
    [
        PathBuf::from("mkfs.cramfs"),
        PathBuf::from("/opt/homebrew/opt/util-linux/sbin/mkfs.cramfs"),
        PathBuf::from("/sbin/mkfs.cramfs"),
        PathBuf::from("/usr/sbin/mkfs.cramfs"),
    ]
    .into_iter()
    .find(|tool| {
        Command::new(tool)
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    })
}

pub fn words(length: usize, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    let mut out = Vec::with_capacity(length + 16);
    while out.len() < length {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        if state.is_multiple_of(11) {
            out.push(b'\n');
        }
        let index = (state >> 8) % 64;
        out.extend_from_slice(format!("word{index}{} ", "x".repeat(index as usize % 7)).as_bytes());
    }
    out.truncate(length);
    out
}

pub fn noise(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 56) as u8
        })
        .collect()
}
