#![allow(dead_code)]
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub const SYNTHETIC: [&str; 2] = ["[SYS]", "[UNKNOWN]"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: String,
    pub mode: Option<u32>,
    pub mtime: Option<i128>,
    pub group: Option<String>,
}

pub type Tree = BTreeMap<String, Node>;

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn mtime_ns(metadata: &fs::Metadata) -> i128 {
    use std::os::unix::fs::MetadataExt;
    i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec())
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (metadata.nlink() > 1).then(|| (metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
pub fn tree(root: &Path) -> Tree {
    use std::os::unix::fs::PermissionsExt;
    let mut found = Tree::new();
    if !root.exists() {
        return found;
    }
    let mut metadata: Vec<(String, PathBuf, fs::Metadata)> = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(folder) = pending.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let info = fs::symlink_metadata(&path).unwrap();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if info.is_dir() {
                fs::set_permissions(&path, fs::Permissions::from_mode(mode(&info) | 0o700))
                    .unwrap();
                pending.push(path.clone());
            }
            metadata.push((name, path, info));
        }
    }
    let mut groups: HashMap<(u64, u64), String> = HashMap::new();
    for (name, _, info) in &metadata {
        if info.is_file()
            && let Some(key) = identity(info)
        {
            let group = groups.entry(key).or_insert_with(|| name.clone());
            if name < group {
                *group = name.clone();
            }
        }
    }
    for (name, path, info) in &metadata {
        let node = if info.file_type().is_symlink() {
            Node {
                kind: format!("link:{}", fs::read_link(path).unwrap().to_string_lossy()),
                mode: None,
                mtime: None,
                group: None,
            }
        } else if info.is_dir() {
            Node {
                kind: "dir".to_owned(),
                mode: Some(mode(info)),
                mtime: (!SYNTHETIC.contains(&name.as_str())).then(|| mtime_ns(info)),
                group: None,
            }
        } else if !info.is_file() {
            Node {
                kind: "special".to_owned(),
                mode: Some(mode(info)),
                mtime: Some(mtime_ns(info)),
                group: None,
            }
        } else {
            if mode(info) & 0o400 == 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(mode(info) | 0o400)).unwrap();
            }
            Node {
                kind: format!("file:{}", sha256(&fs::read(path).unwrap())),
                mode: Some(mode(info)),
                mtime: Some(mtime_ns(info)),
                group: identity(info).map(|key| groups[&key].clone()),
            }
        };
        found.insert(name.clone(), node);
    }
    found
}

pub fn from_json(value: &serde_json::Value) -> Tree {
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, node)| {
            (
                name.clone(),
                Node {
                    kind: node["kind"].as_str().unwrap().to_owned(),
                    mode: node.get("mode").map(|mode| mode.as_u64().unwrap() as u32),
                    mtime: node.get("mtime").map(|mtime| {
                        mtime
                            .as_i64()
                            .map(i128::from)
                            .unwrap_or_else(|| mtime.as_u64().unwrap().into())
                    }),
                    group: node
                        .get("group")
                        .map(|group| group.as_str().unwrap().to_owned()),
                },
            )
        })
        .collect()
}

#[cfg(unix)]
pub fn umask_ceiling(work: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let probe = work.join("umask-probe");
    fs::create_dir(&probe).unwrap();
    let mode = fs::metadata(&probe).unwrap().permissions().mode() & 0o777;
    fs::remove_dir(&probe).unwrap();
    mode
}

pub fn masked(tree: &Tree, ceiling: u32) -> Tree {
    tree.iter()
        .map(|(name, node)| {
            let mut node = node.clone();
            node.mode = node.mode.map(|mode| mode & (ceiling | !0o777));
            (name.clone(), node)
        })
        .collect()
}

pub fn sevenzip_extract(binary: &Path, image: &Path, into: &Path) -> (bool, Tree, String) {
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
    (run.status.success(), tree(into), output)
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

#[cfg(unix)]
pub fn tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(
            [
                "/opt/homebrew/opt/e2fsprogs/sbin",
                "/usr/local/opt/e2fsprogs/sbin",
                "/opt/homebrew/sbin",
                "/usr/local/sbin",
                "/usr/sbin",
                "/sbin",
            ]
            .map(PathBuf::from),
        )
        .map(|folder| folder.join(name))
        .find(|candidate| candidate.is_file())
}

fn whole_seconds(nanoseconds: i128) -> i128 {
    nanoseconds.div_euclid(1_000_000_000) * 1_000_000_000
}

// EXTERNAL(7-Zip 26.05 `7zz x` on ext): a file drops setuid, setgid and sticky, a folder
// adds its owner's rights, and a pipe, socket or device becomes a file of mode 0o666.
fn sevenzip_mode(kind: fs::FileType, mode: u32) -> u32 {
    if kind.is_dir() {
        (mode | 0o700) & 0o777
    } else if kind.is_file() {
        mode & 0o777
    } else {
        0o666
    }
}

#[cfg(unix)]
pub fn from_source(root: &Path) -> Tree {
    let mut found = tree(root);
    for (name, node) in &mut found {
        let info = fs::symlink_metadata(root.join(name)).unwrap();
        let kind = info.file_type();
        node.mtime = node
            .mtime
            .map(|mtime| mtime.div_euclid(1_000_000_000) * 1_000_000_000);
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            node.mode = node.mode.map(|mode| (mode | 0o700) & 0o777);
        } else if kind.is_file() {
            node.mode = node.mode.map(|mode| mode & 0o777);
        } else {
            node.kind = format!("file:{}", sha256(b""));
            node.mode = Some(0o666);
        }
    }
    found
}

#[cfg(unix)]
pub fn system_entry(name: &str) -> bool {
    name == "lost+found"
        || SYNTHETIC
            .iter()
            .any(|folder| name == *folder || name.starts_with(&format!("{folder}/")))
}

// EXTERNAL(7-Zip 26.05 `7zz x` on ext): a link held in inline data, and a block-mapped
// file whose pointer to an indirect block is zero before a later block, are refused as
// `Unsupported Method`.
pub fn sevenzip_unsupported_method_paths(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("ERROR: Unsupported Method : "))
        .map(str::to_owned)
        .collect()
}

pub fn assert_matches_sevenzip(ours: &Tree, theirs: &Tree, output: &str, label: &str) {
    let mut ours = ours.clone();
    let mut theirs = theirs.clone();
    for path in sevenzip_unsupported_method_paths(output) {
        let node = ours.remove(&path).unwrap_or_else(|| {
            panic!("{label}: 7-Zip could not read {path}, which Zipper did not write")
        });
        assert!(node.kind != "dir", "{label}: {path} is {node:?}");
        theirs.remove(&path);
        if let Some(group) = node.group {
            let siblings: Vec<String> = ours
                .iter()
                .filter(|(_, other)| other.group.as_ref() == Some(&group))
                .map(|(name, _)| name.clone())
                .collect();
            for sibling in siblings {
                ours.remove(&sibling);
                theirs.remove(&sibling);
            }
        }
    }
    assert_eq!(ours, theirs, "{label} differs from 7-Zip:\n{output}");
}
