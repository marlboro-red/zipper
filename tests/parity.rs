//! Every archive in `tests/data/parity`, extracted the way its row in
//! `status.json` says Zipper handles it. A native archive must give the tree
//! 7-Zip gave when the corpus was built. A backend archive must give the same
//! tree through `--zip-engine backend` as a direct 7-Zip extraction on this
//! machine, or fail with nothing published where that 7-Zip fails. A missing archive must be refused with nothing published. An
//! archive on a dropped row is skipped. An archive on a refused row, or named
//! in a row's `refused archives`, must be refused by every engine with an
//! error saying Zipper does not read it, and nothing published.
//!
//! Without 7-Zip 26.05 or later the backend archives are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set, as `cargo xtask gate` sets it outside CI.
#[path = "common/sevenzip.rs"]
mod sevenzip;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[path = "common/xattrs.rs"]
mod xattrs;

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/parity");
const STORED_SUFFIX: &str = ".stored.xz";

#[derive(Debug, PartialEq)]
enum Expect {
    Native,
    Backend,
    Refused,
    Licence,
    Skipped,
}

fn json(name: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(Path::new(DATA).join(name)).unwrap()).unwrap()
}

fn expectation(status: &Value, archive: &str, row: &[Value]) -> Expect {
    let entry = &status[row[0].as_str().unwrap()][row[1].as_str().unwrap()];
    let named = |key: &str| {
        entry[key]
            .as_array()
            .is_some_and(|names| names.iter().any(|name| name == archive))
    };
    match entry["status"].as_str().unwrap() {
        _ if named("refused archives") => Expect::Licence,
        _ if named("native archives") => Expect::Native,
        "refused" => Expect::Licence,
        "native" => Expect::Native,
        "partial" | "backend" => Expect::Backend,
        "missing" => Expect::Refused,
        _ => Expect::Skipped,
    }
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .flat_map(|entry| {
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let kind = if entry.file_type().is_symlink() {
                format!(
                    "link:{}",
                    fs::read_link(entry.path())
                        .unwrap()
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                )
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                hex(&Sha256::digest(fs::read(entry.path()).unwrap()))
            };
            let attributes = xattrs::attributes(entry.path())
                .into_iter()
                .map(|(attribute, value)| {
                    (
                        format!("{name}:{}", xattrs::sevenzip_stream_name(&attribute)),
                        hex(&Sha256::digest(value)),
                    )
                })
                .collect::<Vec<_>>();
            std::iter::once((name, kind)).chain(attributes)
        })
        .collect()
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn reference_tree(record: &Value) -> BTreeMap<String, String> {
    record["tree"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, kind)| (name.clone(), kind.as_str().unwrap().to_owned()))
        .collect()
}

fn unpack(record: &Value, into: &Path) -> PathBuf {
    let files: Vec<&str> = record["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file.as_str().unwrap())
        .collect();
    for file in &files {
        let stored = fs::read(Path::new(DATA).join("archives").join(file)).unwrap();
        let digest: String = Sha256::digest(&stored)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            record["sha256"][*file], digest,
            "{file} changed on checkout; tests/data/parity/archives must stay binary"
        );
        let (name, bytes) = match file.strip_suffix(STORED_SUFFIX) {
            Some(name) => {
                let mut bytes = Vec::new();
                liblzma::read::XzDecoder::new(&stored[..])
                    .read_to_end(&mut bytes)
                    .unwrap();
                (name, bytes)
            }
            None => (*file, stored),
        };
        fs::write(into.join(name), bytes).unwrap();
    }
    into.join(files[0].strip_suffix(STORED_SUFFIX).unwrap_or(files[0]))
}

fn sevenzip() -> Option<PathBuf> {
    std::env::var_os("ZIPPER_7Z")
        .map(PathBuf::from)
        .or_else(sevenzip::sevenzip)
}

fn zipper(
    engine: Option<&str>,
    password: Option<&str>,
    archive: &Path,
    output: &Path,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zipper"));
    if let Some(engine) = engine {
        command.args(["--zip-engine", engine]);
    }
    if let Some(password) = password {
        command.args(["--password", password]);
    }
    command
        .arg("extract")
        .arg(archive)
        .arg("-o")
        .arg(output)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn leftovers(folder: &Path) -> Vec<String> {
    fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".zipper-") || name == "out")
        .collect()
}

#[test]
fn every_parity_archive_extracts_as_its_row_says() {
    let status = json("status.json");
    let corpus = json("archives.json");
    let backend = sevenzip();
    let mut counts = BTreeMap::new();
    for (name, record) in corpus["archives"].as_object().unwrap() {
        let expect = expectation(&status, name, record["row"].as_array().unwrap());
        let work = tempfile::tempdir().unwrap();
        let inputs = work.path().join("in");
        fs::create_dir(&inputs).unwrap();
        let archive = unpack(record, &inputs);
        let output = work.path().join("out");
        match (&expect, &backend) {
            (Expect::Native, _) => {
                let run = zipper(
                    Some("native"),
                    corpus["password"]
                        .as_str()
                        .filter(|_| name.ends_with(".zip") || name.ends_with(".7z")),
                    &archive,
                    &output,
                );
                assert!(
                    run.status.success(),
                    "{name}: {}",
                    String::from_utf8_lossy(&run.stderr)
                );
                let reference: BTreeMap<String, String> = reference_tree(record)
                    .into_iter()
                    .filter(|(path, _)| xattrs::sevenzip_stream_kept(path))
                    .collect();
                assert_eq!(tree(&output), reference, "{name}");
            }
            (Expect::Backend, Some(binary)) => {
                let run = zipper(Some("backend"), None, &archive, &output);
                let direct = work.path().join("direct");
                let mut flag = std::ffi::OsString::from("-o");
                flag.push(&direct);
                let direct_run = Command::new(binary)
                    .args(["x", "-y"])
                    .arg(flag)
                    .arg(&archive)
                    .stdin(Stdio::null())
                    .output()
                    .unwrap();
                if direct_run.status.success() {
                    assert!(
                        run.status.success(),
                        "{name}: {}",
                        String::from_utf8_lossy(&run.stderr)
                    );
                    assert_eq!(tree(&output), tree(&direct), "{name}");
                } else {
                    assert!(
                        !run.status.success(),
                        "{name}: 7-Zip failed and Zipper did not"
                    );
                    assert!(!output.exists(), "{name}: published after 7-Zip failed");
                }
            }
            (Expect::Backend, None) | (Expect::Skipped, _) => {
                *counts.entry("skipped").or_insert(0) += 1;
                continue;
            }
            (Expect::Refused, _) => {
                let run = zipper(None, None, &archive, &output);
                assert!(!run.status.success(), "{name} was extracted");
                assert_eq!(leftovers(work.path()), Vec::<String>::new(), "{name}");
            }
            (Expect::Licence, _) => {
                for engine in [None, Some("native"), Some("backend")] {
                    let run = zipper(engine, None, &archive, &output);
                    let stderr = String::from_utf8_lossy(&run.stderr);
                    assert!(!run.status.success(), "{name} was extracted by {engine:?}");
                    assert!(
                        stderr.contains("Zipper does not read"),
                        "{name} with {engine:?}: {stderr}"
                    );
                    assert_eq!(leftovers(work.path()), Vec::<String>::new(), "{name}");
                }
            }
        }
        *counts
            .entry(format!("{expect:?}").leak() as &str)
            .or_insert(0) += 1;
    }
    eprintln!("parity archives: {counts:?}");
}
