//! Every archive RARLAB's `rar` wrote into `tests/data/rar-written` extracts
//! to the tree `scripts/make_rar_corpus.py` recorded from its source, with
//! the same bytes, links, folders, modes and RAR 5 times. Every archive that
//! uses a feature Zipper refuses is refused with the reason the manifest
//! names, and leaves nothing behind. Where `unrar` is installed, each archive
//! Zipper reads extracts to the tree `unrar` extracts, and `unrar` extracts
//! each refused one. Where 7-Zip is installed, each archive extracts to the
//! tree 7-Zip extracts wherever it decodes the archive, which must include
//! the stored ones. Without them those comparisons are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_UNRAR` or `ZIPPER_PARITY_REQUIRE_7Z` is set. Every
//! encrypted archive Zipper reads extracts with the password the manifest
//! records, and a wrong or missing password is refused and leaves nothing.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/unrar.rs"]
mod unrar;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_rar::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/rar-written");
const PASSWORD: &str = "zipper";
const STORED: [&str; 2] = ["rar5-m0.rar", "rar4-m0.rar"];

#[derive(Debug, PartialEq, Eq, Clone)]
enum Node {
    Folder,
    File {
        sha256: String,
        mode: Option<u32>,
        mtime_ns: Option<i128>,
    },
    Link(String),
}

type Tree = BTreeMap<String, Node>;

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn mode_of(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn mode_of(_: &fs::Metadata) -> Option<u32> {
    None
}

fn mtime_ns(metadata: &fs::Metadata) -> i128 {
    let since = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i128;
    if cfg!(windows) {
        since - since % 100
    } else {
        since
    }
}

fn read_unlocked(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).unwrap().permissions().mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o600)).unwrap();
        let bytes = fs::read(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        bytes
    }
    #[cfg(not(unix))]
    fs::read(path).unwrap()
}

fn tree(root: &Path) -> Tree {
    let mut found = Tree::new();
    for entry in walkdir::WalkDir::new(root).min_depth(1) {
        let entry = entry.unwrap();
        let name = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::symlink_metadata(entry.path()).unwrap();
        let node = if metadata.file_type().is_symlink() {
            let target = fs::read_link(entry.path()).unwrap();
            Node::Link(
                target
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            )
        } else if metadata.is_dir() {
            Node::Folder
        } else {
            Node::File {
                sha256: sha256(&read_unlocked(entry.path())),
                mode: mode_of(&metadata),
                mtime_ns: Some(mtime_ns(&metadata)),
            }
        };
        found.insert(name, node);
    }
    found
}

fn make_deletable(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        if !entry.path_is_symlink() {
            let mode = entry.metadata().unwrap().permissions().mode() | 0o700;
            let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode));
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

#[cfg(unix)]
fn umask_ceiling(work: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    let probe = work.join("probe");
    fs::create_dir(&probe).unwrap();
    let mode = fs::metadata(&probe).unwrap().permissions().mode() & 0o777;
    fs::remove_dir(&probe).unwrap();
    Some(mode)
}

#[cfg(not(unix))]
fn umask_ceiling(_: &Path) -> Option<u32> {
    None
}

struct Archive {
    name: String,
    tree: Option<Tree>,
    refused: Option<String>,
    files: Vec<String>,
    password: Option<String>,
}

impl Archive {
    fn options(&self) -> ReadOptions<'_> {
        ReadOptions {
            password: self.password.as_deref().map(str::as_bytes),
            ..ReadOptions::default()
        }
    }
}

fn manifest(ceiling: Option<u32>) -> Vec<Archive> {
    let text = fs::read_to_string(Path::new(CORPUS).join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    manifest["archives"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, entry)| Archive {
            name: name.clone(),
            tree: entry.get("tree").map(|tree| recorded(tree, ceiling)),
            refused: entry
                .get("refused")
                .map(|reason| reason.as_str().unwrap().to_owned()),
            files: entry["files"]
                .as_array()
                .unwrap()
                .iter()
                .map(|file| file.as_str().unwrap().to_owned())
                .collect(),
            password: entry
                .get("password")
                .map(|password| password.as_str().unwrap().to_owned()),
        })
        .collect()
}

fn recorded(entries: &serde_json::Value, ceiling: Option<u32>) -> Tree {
    entries
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, node)| {
            let value = if node.get("dir").is_some() {
                Node::Folder
            } else if let Some(target) = node.get("link") {
                Node::Link(target.as_str().unwrap().to_owned())
            } else {
                Node::File {
                    sha256: node["sha256"].as_str().unwrap().to_owned(),
                    mode: node.get("mode").and_then(|mode| {
                        let mode =
                            u32::from_str_radix(mode.as_str()?.trim_start_matches("0o"), 8).ok()?;
                        Some(mode & ceiling?)
                    }),
                    mtime_ns: node
                        .get("mtime_ns")
                        .and_then(serde_json::Value::as_i64)
                        .map(|ns| {
                            let ns = i128::from(ns);
                            if cfg!(windows) { ns - ns % 100 } else { ns }
                        }),
                }
            };
            (name.clone(), value)
        })
        .collect()
}

fn without_unrecorded_fields(mut actual: Tree, wanted: &Tree) -> Tree {
    for (name, node) in &mut actual {
        if let (
            Node::File { mode, mtime_ns, .. },
            Some(Node::File {
                mode: wanted_mode,
                mtime_ns: wanted_mtime,
                ..
            }),
        ) = (node, wanted.get(name))
        {
            if wanted_mode.is_none() {
                *mode = None;
            }
            if wanted_mtime.is_none() {
                *mtime_ns = None;
            }
        }
    }
    actual
}

fn without_times(mut found: Tree) -> Tree {
    for node in found.values_mut() {
        if let Node::File { mtime_ns, .. } = node {
            *mtime_ns = None;
        }
    }
    found
}

fn corpus(name: &str) -> PathBuf {
    Path::new(CORPUS).join(name)
}

fn zipper_tree(archive: &Archive, work: &Path) -> Tree {
    let out = work.join("zipper");
    read_archive(&corpus(&archive.name), Some(&out), archive.options())
        .unwrap_or_else(|error| panic!("{}: {error:#}", archive.name));
    tree(&out)
}

fn unrar_tree(binary: &Path, archive: &Path, password: Option<&str>, into: &Path) -> Tree {
    fs::create_dir(into).unwrap();
    // EXTERNAL(unrar 7.23): skips relative links as unsafe when the destination path runs
    // through a symlink, such as macOS's /var or /tmp, so it gets the resolved path.
    let resolved = fs::canonicalize(into).unwrap();
    let run = Command::new(binary)
        .args(["x", "-idq", "-y", "-ver"])
        .arg(format!("-p{}", password.unwrap_or(PASSWORD)))
        .arg(archive)
        .arg(&resolved)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "unrar failed on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&run.stderr)
    );
    with_versions_named_as_7zip_names_them(tree(into))
}

fn with_versions_named_as_7zip_names_them(found: Tree) -> Tree {
    let mut renamed = Tree::new();
    for (name, node) in found {
        let name = match name.rsplit_once(';') {
            Some((base, number)) if number.bytes().all(|b| b.is_ascii_digit()) => {
                format!("[VER]/{number}/{base}")
            }
            _ => name,
        };
        let parts: Vec<&str> = name.split('/').collect();
        if parts[0] == "[VER]" {
            for depth in 1..parts.len() {
                renamed.insert(parts[..depth].join("/"), Node::Folder);
            }
        }
        renamed.insert(name, node);
    }
    renamed
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Result<Tree, String> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y", "-snld"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    if run.status.success() {
        Ok(tree(into))
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        ))
    }
}

fn is_rar4(name: &str) -> bool {
    name.starts_with("rar4-")
}

#[test]
fn every_archive_rar_wrote_extracts_its_source_tree() {
    let work = tempfile::tempdir().unwrap();
    let archives = manifest(umask_ceiling(work.path()));
    let mut extracted = 0;
    for archive in &archives {
        let Some(wanted) = &archive.tree else {
            continue;
        };
        let work = tempfile::tempdir().unwrap();
        let actual = without_unrecorded_fields(zipper_tree(archive, work.path()), wanted);
        assert_eq!(&actual, wanted, "{}", archive.name);
        read_archive(&corpus(&archive.name), None, archive.options())
            .unwrap_or_else(|error| panic!("{} fails its test: {error:#}", archive.name));
        make_deletable(work.path());
        extracted += 1;
    }
    assert!(extracted >= 40, "only {extracted} archives extracted");
}

#[test]
fn every_archive_with_a_feature_zipper_refuses_is_refused_by_name_and_leaves_nothing() {
    let mut refused = 0;
    for archive in manifest(None) {
        let Some(reason) = &archive.refused else {
            continue;
        };
        let work = tempfile::tempdir().unwrap();
        let out = work.path().join("out");
        for into in [Some(out.as_path()), None] {
            let error = read_archive(&corpus(&archive.name), into, ReadOptions::default())
                .err()
                .unwrap_or_else(|| panic!("{} was read", archive.name));
            assert!(
                format!("{error:#}").contains(reason.as_str()),
                "{}: {error:#} does not say {reason:?}",
                archive.name
            );
        }
        assert_eq!(
            fs::read_dir(work.path()).unwrap().count(),
            0,
            "{}",
            archive.name
        );
        refused += 1;
    }
    assert!(refused >= 5, "only {refused} archives refused");
}

#[test]
fn every_archive_extracts_the_tree_unrar_and_7zip_extract() {
    let unrar = unrar::unrar();
    let sevenzip = sevenzip::sevenzip();
    let mut against_7zip = Vec::new();
    for archive in manifest(None) {
        let work = tempfile::tempdir().unwrap();
        let ours = archive
            .tree
            .as_ref()
            .map(|_| zipper_tree(&archive, work.path()));
        if let Some(binary) = &unrar {
            let theirs = unrar_tree(
                binary,
                &corpus(&archive.name),
                archive.password.as_deref(),
                &work.path().join("theirs"),
            );
            assert!(
                !theirs.is_empty(),
                "unrar extracted nothing from {}",
                archive.name
            );
            if let Some(ours) = &ours {
                assert_eq!(ours, &theirs, "{} against unrar", archive.name);
            }
        }
        if let (Some(binary), Some(ours)) = (&sevenzip, ours) {
            let into = work.path().join("7zip");
            match sevenzip_tree(binary, &corpus(&archive.name), &into) {
                Ok(theirs) => {
                    let (ours, theirs) = if is_rar4(&archive.name) {
                        (without_times(ours), without_times(theirs))
                    } else {
                        (ours, theirs)
                    };
                    assert_eq!(ours, theirs, "{} against 7-Zip", archive.name);
                    against_7zip.push(archive.name.clone());
                }
                Err(output) => assert!(
                    !STORED.contains(&archive.name.as_str()),
                    "7-Zip did not extract {}: {output}",
                    archive.name
                ),
            }
        }
        make_deletable(work.path());
    }
    if sevenzip.is_some() {
        for stored in STORED {
            assert!(
                against_7zip.iter().any(|name| name == stored),
                "7-Zip was not compared on {stored}"
            );
        }
    }
}

#[test]
fn every_volume_the_manifest_names_is_in_the_corpus() {
    for archive in manifest(None) {
        for file in &archive.files {
            assert!(corpus(file).is_file(), "{} names {file}", archive.name);
        }
    }
}

fn copied_set(archive: &Archive, work: &Path) -> Vec<PathBuf> {
    archive
        .files
        .iter()
        .map(|file| {
            let copy = work.join(file);
            fs::copy(corpus(file), &copy).unwrap();
            copy
        })
        .collect()
}

fn refusal_of_copied(archive: &Archive, work: &Path, first: &Path) -> String {
    let out = work.join("out");
    let error = read_archive(first, Some(&out), archive.options())
        .err()
        .unwrap_or_else(|| panic!("{} was read", archive.name));
    assert!(!out.exists(), "{} left output", archive.name);
    format!("{error:#}")
}

fn name_of(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn every_volume_set_refuses_a_missing_cut_or_misplaced_volume_by_name() {
    let mut sets = 0;
    for archive in manifest(None) {
        if archive.files.len() < 2 {
            continue;
        }
        sets += 1;
        let work = tempfile::tempdir().unwrap();
        let volumes = copied_set(&archive, work.path());
        let last = volumes.last().unwrap();
        let bytes = fs::read(last).unwrap();

        fs::remove_file(last).unwrap();
        let error = refusal_of_copied(&archive, work.path(), &volumes[0]);
        let wanted = format!("RAR volume {} is missing", name_of(last));
        assert!(error.contains(&wanted), "{}: {error}", archive.name);

        fs::write(last, &bytes[..bytes.len() - 1]).unwrap();
        let error = refusal_of_copied(&archive, work.path(), &volumes[0]);
        let wanted = format!("in RAR volume {}", name_of(last));
        assert!(error.contains(&wanted), "{}: {error}", archive.name);
        fs::write(last, &bytes).unwrap();

        let error = refusal_of_copied(&archive, work.path(), &volumes[1]);
        let wanted = format!("open its first volume, {}", name_of(&volumes[0]));
        assert!(error.contains(&wanted), "{}: {error}", archive.name);

        if volumes.len() > 2 {
            let (second, third) = (&volumes[1], &volumes[2]);
            let swap = work.path().join("swap");
            fs::rename(second, &swap).unwrap();
            fs::rename(third, second).unwrap();
            fs::rename(&swap, third).unwrap();
            let error = refusal_of_copied(&archive, work.path(), &volumes[0]);
            assert!(
                error.contains(&format!("RAR volume {}", name_of(second))),
                "{}: {error}",
                archive.name
            );
        }
        make_deletable(work.path());
    }
    assert!(sets >= 9, "only {sets} volume sets");
}

#[test]
fn a_volume_from_another_set_is_refused() {
    let archives = manifest(None);
    let find = |name: &str| {
        archives
            .iter()
            .find(|archive| archive.name == name)
            .unwrap()
    };
    for (name, stranger, reason) in [
        (
            "rar5-volumes-m3.part01.rar",
            "rar5-volumes.part1.rar",
            "continues RAR 5 entry",
        ),
        (
            "rar5-volumes-solid.part01.rar",
            "rar5-volumes-m3.part01.rar",
            "disagree on whether the archive is solid",
        ),
        (
            "rar5-volumes-encrypted-headers.part01.rar",
            "rar5-volumes-solid.part01.rar",
            "disagree on whether the headers are encrypted",
        ),
        (
            "rar4-volumes-m3.part01.rar",
            "rar4-volumes.part1.rar",
            "continues RAR 4 entry",
        ),
    ] {
        let (archive, stranger) = (find(name), find(stranger));
        let work = tempfile::tempdir().unwrap();
        let volumes = copied_set(archive, work.path());
        fs::copy(corpus(&stranger.files[1]), &volumes[1]).unwrap();
        let error = refusal_of_copied(archive, work.path(), &volumes[0]);
        let place = format!("RAR volume {}", name_of(&volumes[1]));
        assert!(
            error.contains(&place) && error.contains(reason),
            "{name}: {error}"
        );
        make_deletable(work.path());
    }
}

#[test]
fn a_selected_path_late_in_a_solid_stream_extracts_only_its_entries() {
    let work = tempfile::tempdir().unwrap();
    let ceiling = umask_ceiling(work.path());
    for (name, path) in [
        ("rar5-solid-m5.rar", "src/text"),
        ("rar5-solid-small-texts.rar", "src/notes/note-119.txt"),
        ("rar5-encrypted-headers-solid.rar", "src/notes/note-119.txt"),
        ("rar5-encrypted-solid.rar", "src/text"),
        ("rar5-volumes-solid.part01.rar", "src/text"),
        ("rar5-volumes-encrypted-headers.part01.rar", "src/text"),
    ] {
        let archive = manifest(ceiling)
            .into_iter()
            .find(|archive| archive.name == name)
            .unwrap();
        let wanted: Tree = archive
            .tree
            .clone()
            .unwrap()
            .into_iter()
            .filter(|(entry, _)| {
                entry == path
                    || entry.starts_with(&format!("{path}/"))
                    || path.starts_with(&format!("{entry}/"))
            })
            .collect();
        assert!(wanted.len() >= 2, "{name} has no {path}");
        let work = tempfile::tempdir().unwrap();
        let out = work.path().join("out");
        let paths = [path.to_owned()];
        read_archive(
            &corpus(name),
            Some(&out),
            ReadOptions {
                paths: &paths,
                ..archive.options()
            },
        )
        .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(
            without_unrecorded_fields(tree(&out), &wanted),
            wanted,
            "{name}"
        );
        make_deletable(work.path());
    }
}

#[test]
fn every_encrypted_archive_refuses_a_wrong_or_missing_password_and_leaves_nothing() {
    let mut encrypted = 0;
    for archive in manifest(None) {
        let (Some(password), Some(_)) = (&archive.password, &archive.tree) else {
            continue;
        };
        let wrong = format!("!{password}");
        for given in [Some(wrong.as_bytes()), Some(&b""[..]), None] {
            let work = tempfile::tempdir().unwrap();
            let out = work.path().join("out");
            for into in [Some(out.as_path()), None] {
                let options = ReadOptions {
                    password: given,
                    ..ReadOptions::default()
                };
                let error = read_archive(&corpus(&archive.name), into, options)
                    .err()
                    .unwrap_or_else(|| panic!("{} was read with {given:?}", archive.name));
                if given.is_none() {
                    assert!(
                        error.is::<zipper::native_rar::PasswordNeeded>(),
                        "{}: {error:#}",
                        archive.name
                    );
                } else {
                    assert!(
                        format!("{error:#}").contains("the password is wrong"),
                        "{}: {error:#}",
                        archive.name
                    );
                }
            }
            assert_eq!(
                fs::read_dir(work.path()).unwrap().count(),
                0,
                "{} with {given:?}",
                archive.name
            );
        }
        encrypted += 1;
    }
    assert!(encrypted >= 10, "only {encrypted} encrypted archives");
}
