use std::{collections::BTreeMap, fs, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expected {
    size_and_crc: Option<(u64, u32)>,
    pub link_target: Option<String>,
    mode: Option<u32>,
    modified: Option<i64>,
}

pub type Tree = BTreeMap<String, Expected>;

fn optional<T>(field: &str, parse: impl Fn(&str) -> T) -> Option<T> {
    (field != "-").then(|| parse(field))
}

pub fn expected(corpus: &Path) -> BTreeMap<String, Tree> {
    let manifest = fs::read_to_string(corpus.join("manifest.tsv")).unwrap();
    let mut archives: BTreeMap<String, Tree> = BTreeMap::new();
    for line in manifest.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [archive, path, size, crc, mode, modified] = fields[..] else {
            panic!("bad manifest line {line:?}");
        };
        let link_target = (size == "link").then(|| crc.to_string());
        let size_and_crc = (size != "dir" && size != "link")
            .then(|| (size.parse().unwrap(), u32::from_str_radix(crc, 16).unwrap()));
        let value = Expected {
            size_and_crc,
            link_target,
            mode: optional(mode, |mode| u32::from_str_radix(mode, 8).unwrap()),
            modified: optional(modified, |seconds| seconds.parse().unwrap()),
        };
        archives
            .entry(archive.to_string())
            .or_default()
            .insert(path.to_string(), value);
    }
    archives
}

fn tree(root: &Path) -> Tree {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.path().strip_prefix(root).unwrap();
            let name: Vec<_> = name.iter().map(|part| part.to_str().unwrap()).collect();
            let metadata = entry.metadata().unwrap();
            if entry.file_type().is_symlink() {
                let target = fs::read_link(entry.path()).unwrap();
                let target = target.to_str().unwrap();
                #[cfg(windows)]
                let target = target.replace('\\', "/");
                let value = Expected {
                    size_and_crc: None,
                    link_target: Some(target.to_string()),
                    mode: None,
                    modified: None,
                };
                return (name.join("/"), value);
            }
            let size_and_crc = entry.file_type().is_file().then(|| {
                let data = fs::read(entry.path()).unwrap();
                (data.len() as u64, crc32fast::hash(&data))
            });
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                Some(metadata.permissions().mode() & 0o777)
            };
            #[cfg(not(unix))]
            let mode = None;
            let modified = metadata
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs() as i64);
            let value = Expected {
                size_and_crc,
                link_target: None,
                mode,
                modified: Some(modified),
            };
            (name.join("/"), value)
        })
        .collect()
}

#[cfg(unix)]
fn umask_ceiling_mode(work: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let probe = work.join("probe");
    fs::create_dir(&probe).unwrap();
    fs::metadata(&probe).unwrap().permissions().mode() & 0o777
}

pub fn assert_tree_matches_manifest(work: &Path, wanted: &Tree, label: &str) {
    let destination = work.join("out");
    let mut actual = tree(&destination);
    #[cfg(unix)]
    let default = {
        let default = umask_ceiling_mode(work);
        let root = tree(work).remove("out").unwrap();
        assert_eq!(root.mode, Some(default), "{label} root");
        default
    };
    crate::common::make_tree_deletable(work);
    let mut expected = wanted.clone();
    for (path, value) in &mut expected {
        #[cfg(unix)]
        {
            value.mode = value.mode.map(|mode| mode & default);
        }
        #[cfg(not(unix))]
        {
            value.mode = None;
        }
        if let Some(found) = actual.get_mut(path) {
            if value.mode.is_none() {
                found.mode = None;
            }
            if value.modified.is_none() {
                found.modified = None;
            }
        }
    }
    for (path, found) in &mut actual {
        if !expected.contains_key(path) {
            found.mode = None;
            found.modified = None;
        }
    }
    assert_eq!(actual, expected, "{label}");
}
