#![allow(dead_code)]
use std::path::Path;

pub const RESTORED_HERE: bool = cfg!(target_os = "macos");

const NEVER_RESTORED: [&str; 4] = [
    "com.apple.quarantine",
    "com.apple.provenance",
    "com.apple.macl",
    "com.apple.rootless",
];
const STREAMS_ZIPPER_DOES_NOT_WRITE: [&str; 3] =
    ["com.apple.ResourceFork", "com.apple.decmpfs", "rsrc"];

pub fn sevenzip_stream_name(name: &str) -> String {
    name.replace([':', '/'], "_")
}

pub fn sevenzip_writes_no_attribute_of(
    tree: &std::collections::BTreeMap<String, String>,
    path: &str,
) -> bool {
    path.split_once(':')
        .and_then(|(owner, _)| tree.get(owner))
        .is_some_and(|kind| kind.starts_with("dir"))
}

pub fn never_restored(name: &str) -> bool {
    NEVER_RESTORED.contains(&name) || name.starts_with("com.apple.system.")
}

pub fn sevenzip_stream_kept(path: &str) -> bool {
    let Some((_, name)) = path.split_once(':') else {
        return true;
    };
    RESTORED_HERE && !never_restored(name) && !STREAMS_ZIPPER_DOES_NOT_WRITE.contains(&name)
}

#[cfg(target_os = "macos")]
pub fn attributes(path: &Path) -> Vec<(String, Vec<u8>)> {
    let size = rustix::fs::llistxattr_query(path).unwrap();
    let mut list = vec![0u8; size];
    let length = rustix::fs::llistxattr(path, &mut list[..]).unwrap();
    list.truncate(length);
    let mut found = Vec::new();
    for name in list
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).unwrap().to_owned();
        if never_restored(&name) {
            continue;
        }
        let size = rustix::fs::lgetxattr_query(path, name.as_str()).unwrap();
        let mut value = vec![0u8; size];
        let length = rustix::fs::lgetxattr(path, name.as_str(), &mut value[..]).unwrap();
        value.truncate(length);
        found.push((name, value));
    }
    found.sort();
    found
}

#[cfg(not(target_os = "macos"))]
pub fn attributes(_: &Path) -> Vec<(String, Vec<u8>)> {
    Vec::new()
}

pub const SKIPPED_VALUE: &[u8] = b"never restored";

#[cfg(target_os = "macos")]
pub fn assert_no_skipped_value(root: &Path) {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        let path = entry.path();
        let size = rustix::fs::llistxattr_query(path).unwrap();
        let mut list = vec![0u8; size];
        let length = rustix::fs::llistxattr(path, &mut list[..]).unwrap();
        for name in list[..length]
            .split(|&byte| byte == 0)
            .filter(|name| !name.is_empty())
        {
            let name = std::str::from_utf8(name).unwrap();
            let mut value = vec![0u8; SKIPPED_VALUE.len() + 1];
            let length = rustix::fs::lgetxattr(path, name, &mut value[..]).unwrap_or(0);
            assert_ne!(
                &value[..length],
                SKIPPED_VALUE,
                "{} kept {name}, which is never restored",
                path.display()
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn assert_no_skipped_value(_: &Path) {}
