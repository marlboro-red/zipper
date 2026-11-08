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
fn mode(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!(" {:o}", metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn mode(_: &fs::Metadata) -> String {
    String::new()
}

fn open_up(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        if entry.file_type().is_dir() {
            let mode = entry.metadata().unwrap().permissions().mode();
            if mode & 0o700 != 0o700 {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode | 0o700));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

pub fn tree(root: &Path) -> Tree {
    let mut found = Tree::new();
    if !root.exists() {
        return found;
    }
    let modes: BTreeMap<PathBuf, fs::Metadata> = walkdir::WalkDir::new(root)
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
        let metadata = modes
            .get(entry.path())
            .cloned()
            .unwrap_or_else(|| fs::symlink_metadata(entry.path()).unwrap());
        let kind = if entry.file_type().is_symlink() {
            format!("link:{}", fs::read_link(entry.path()).unwrap().display())
        } else if entry.file_type().is_dir() {
            format!("dir{}", mode(&metadata))
        } else {
            format!(
                "{}{}",
                sha256(&fs::read(entry.path()).unwrap()),
                mode(&metadata)
            )
        };
        for (attribute, value) in crate::xattrs::attributes(entry.path()) {
            found.insert(
                format!("{name}:{}", crate::xattrs::sevenzip_stream_name(&attribute)),
                sha256(&value),
            );
        }
        found.insert(name, kind);
    }
    found
}

pub fn without_modes(tree: &Tree) -> Tree {
    tree.iter()
        .map(|(name, kind)| (name.clone(), kind.split(' ').next().unwrap().to_owned()))
        .collect()
}

pub fn sevenzip_run(binary: &Path, image: &Path, into: &Path) -> (bool, Tree, String) {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(image)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let as_attributes = tree(into)
        .into_iter()
        .filter(|(name, _)| crate::xattrs::sevenzip_stream_kept(name))
        .map(|(name, kind)| {
            let kind = match name.contains(':') {
                true => kind.split(' ').next().unwrap().to_owned(),
                false => kind,
            };
            (name, kind)
        })
        .collect();
    let output =
        String::from_utf8_lossy(&run.stdout).into_owned() + &String::from_utf8_lossy(&run.stderr);
    (run.status.success(), as_attributes, output)
}

pub fn as_zipper_writes(theirs: &Tree, ours: &Tree, differs: &[&str], label: &str) -> Tree {
    let mut expected = theirs.clone();
    for path in differs {
        assert!(
            ours.contains_key(*path),
            "{label}: Zipper did not write {path}"
        );
        assert_ne!(
            theirs.get(*path),
            ours.get(*path),
            "{label}: 7-Zip now writes {path} as Zipper does; drop it from the differences"
        );
        expected.insert((*path).to_owned(), ours[*path].clone());
    }
    for (path, kind) in ours {
        if crate::xattrs::sevenzip_writes_no_attribute_of(ours, path) {
            expected.insert(path.clone(), kind.clone());
        }
    }
    expected
}
