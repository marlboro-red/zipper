//! Archives split into raw numbered volumes, `name.001`, `name.002` and on,
//! read in process as one stream. Each split must give the tree its whole
//! archive gives, and 7-Zip's own volumes the tree 7-Zip extracts. A missing,
//! truncated, repeated, swapped or extra volume must fail with an error that
//! names a volume, before anything is published.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{Engine, ExtractOptions, Reader, choose_reader, extract_archive};

fn parity(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/parity/archives")
        .join(name)
}

fn volume_name(stem: &str, number: usize) -> String {
    format!("{stem}.{number:03}")
}

fn split(bytes: &[u8], folder: &Path, stem: &str, volume: usize) -> PathBuf {
    for (index, chunk) in bytes.chunks(volume).enumerate() {
        fs::write(folder.join(volume_name(stem, index + 1)), chunk).unwrap();
    }
    folder.join(volume_name(stem, 1))
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let kind = if entry.file_type().is_symlink() {
                format!("link:{}", fs::read_link(entry.path()).unwrap().display())
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                Sha256::digest(fs::read(entry.path()).unwrap())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            };
            (name, kind)
        })
        .collect()
}

fn extract(archive: &Path, threads: usize) -> (tempfile::TempDir, anyhow::Result<()>) {
    let root = tempfile::tempdir().unwrap();
    let result = extract_archive(
        archive,
        &root.path().join("out"),
        ExtractOptions {
            threads: Some(threads),
            ..Default::default()
        },
    );
    (root, result)
}

fn extracted_tree(archive: &Path, threads: usize, label: &str) -> BTreeMap<String, String> {
    let (root, result) = extract(archive, threads);
    if let Err(error) = result {
        panic!("{label}: {error:#}");
    }
    tree(&root.path().join("out"))
}

fn assert_refused(archive: &Path, names: &[&str], label: &str) {
    let (root, result) = extract(archive, 4);
    let error = match result {
        Ok(()) => panic!("{label}: extracted"),
        Err(error) => format!("{error:#}"),
    };
    for name in names {
        assert!(error.contains(name), "{label}: {name} not in: {error}");
    }
    let left: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
    assert!(left.is_empty(), "{label}: left {left:?}");
}

const NATIVE: &[&str] = &[
    "7z.7z",
    "7z-bcj2.7z",
    "7z-ppmd.7z",
    "zip.zip",
    "zip-lzma.zip",
    "tar.tar",
    "tar.tar.gz",
    "tar.tar.xz",
    "tar.tar.zst",
    "tar.tar.bz2",
    "hello.txt.gz",
    "hello.txt.xz",
    "hello.txt.lzma",
];

#[test]
fn every_native_format_reads_through_volumes_of_every_awkward_size() {
    for name in NATIVE {
        let bytes = fs::read(parity(name)).unwrap();
        let whole_reader = choose_reader(Engine::Auto, &parity(name)).unwrap();
        let whole = extracted_tree(&parity(name), 4, name);
        let length = bytes.len();
        for volume in [
            1,
            2,
            3,
            7,
            31,
            511,
            512,
            513,
            length - 1,
            length,
            length + 1,
        ] {
            let folder = tempfile::tempdir().unwrap();
            let first = split(&bytes, folder.path(), name, volume);
            let label = format!("{name} in {volume}-byte volumes");
            assert_eq!(
                choose_reader(Engine::Auto, &first).unwrap(),
                whole_reader,
                "{label}"
            );
            for threads in [1, 4] {
                assert_eq!(extracted_tree(&first, threads, &label), whole, "{label}");
            }
        }
    }
}

fn sevenzip() -> Option<PathBuf> {
    let found = ["7zz", "7z"]
        .into_iter()
        .find(|binary| {
            Command::new(binary)
                .arg("i")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .map(PathBuf::from);
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip was found"
    );
    found
}

fn run_7zip(binary: &Path, folder: &Path, args: &[&str]) {
    let output = Command::new(binary)
        .current_dir(folder)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "7-Zip {args:?}: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn source_tree(folder: &Path) {
    let source = folder.join("source");
    fs::create_dir_all(source.join("nested/deeper")).unwrap();
    fs::create_dir_all(source.join("empty folder")).unwrap();
    fs::write(source.join("empty.txt"), b"").unwrap();
    fs::write(source.join("one.txt"), b"1").unwrap();
    let text: Vec<u8> = (0..3000u32)
        .flat_map(|i| format!("line {i}\n").into_bytes())
        .collect();
    fs::write(source.join("nested/text.txt"), &text).unwrap();
    let mut state = 7u32;
    let noise: Vec<u8> = (0..1500)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 24) as u8
        })
        .collect();
    fs::write(source.join("nested/deeper/noise.bin"), noise).unwrap();
}

#[test]
fn volumes_7zip_writes_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip() else {
        return;
    };
    let work = tempfile::tempdir().unwrap();
    source_tree(work.path());
    let source = work.path().join("source");
    let tar = work.path().join("whole.tar");
    run_7zip(
        &binary,
        &source,
        &["a", "-ttar", tar.to_str().unwrap(), "."],
    );
    for volume in ["1b", "13b", "100b", "511b", "1k", "64k"] {
        let folder = work.path().join(format!("v{volume}"));
        fs::create_dir(&folder).unwrap();
        let mut sets = Vec::new();
        for (name, kind) in [("a.7z", "-t7z"), ("a.zip", "-tzip"), ("a.tar", "-ttar")] {
            let archive = folder.join(name);
            run_7zip(
                &binary,
                &source,
                &[
                    "a",
                    kind,
                    &format!("-v{volume}"),
                    archive.to_str().unwrap(),
                    ".",
                ],
            );
            sets.push((name, false));
        }
        for (name, kind) in [("a.tar.gz", "-tgzip"), ("a.tar.xz", "-txz")] {
            let archive = folder.join(name);
            run_7zip(
                &binary,
                &folder,
                &[
                    "a",
                    kind,
                    &format!("-v{volume}"),
                    archive.to_str().unwrap(),
                    tar.to_str().unwrap(),
                ],
            );
            sets.push((name, true));
        }
        for (name, compressed_tar) in sets {
            let first = folder.join(volume_name(name, 1));
            let label = format!("{name} in {volume} volumes");
            assert!(
                !matches!(
                    choose_reader(Engine::Auto, &first).unwrap(),
                    Reader::Backend { .. }
                ),
                "{label}: sent to 7-Zip"
            );
            let direct = folder.join(format!("{name}-7zip"));
            run_7zip(
                &binary,
                &folder,
                &[
                    "x",
                    "-y",
                    &format!("-o{}", direct.display()),
                    first.to_str().unwrap(),
                ],
            );
            let expected = if compressed_tar {
                let inner = fs::read_dir(&direct)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .next()
                    .unwrap();
                let unpacked = folder.join(format!("{name}-7zip-tar"));
                run_7zip(
                    &binary,
                    &folder,
                    &[
                        "x",
                        "-y",
                        &format!("-o{}", unpacked.display()),
                        inner.to_str().unwrap(),
                    ],
                );
                tree(&unpacked)
            } else {
                tree(&direct)
            };
            assert_eq!(expected, tree(&source), "{label}: 7-Zip");
            for threads in [1, 4] {
                assert_eq!(extracted_tree(&first, threads, &label), expected, "{label}");
            }
        }
    }
}

struct Set {
    _folder: tempfile::TempDir,
    stem: String,
    count: usize,
}

impl Set {
    fn new(name: &str, volume: usize) -> Self {
        let folder = tempfile::tempdir().unwrap();
        let bytes = fs::read(parity(name)).unwrap();
        split(&bytes, folder.path(), name, volume);
        Set {
            stem: name.to_owned(),
            count: bytes.len().div_ceil(volume),
            _folder: folder,
        }
    }

    fn path(&self, number: usize) -> PathBuf {
        self._folder.path().join(self.name(number))
    }

    fn name(&self, number: usize) -> String {
        volume_name(&self.stem, number)
    }
}

const CHECKED: &[&str] = &["7z.7z", "zip.zip", "tar.tar.gz", "tar.tar.xz", "tar.tar"];

#[test]
fn a_broken_volume_set_is_refused_and_named_before_publish() {
    for archive in CHECKED {
        let volume = fs::metadata(parity(archive)).unwrap().len() as usize / 4 - 3;
        let set = || Set::new(archive, volume);
        let label = |case: &str| format!("{archive}: {case}");

        let missing_middle = set();
        fs::remove_file(missing_middle.path(2)).unwrap();
        assert_refused(
            &missing_middle.path(1),
            &[
                &missing_middle.name(2),
                &missing_middle.name(missing_middle.count),
            ],
            &label("missing middle"),
        );

        let gap = set();
        fs::remove_file(gap.path(3)).unwrap();
        fs::remove_file(gap.path(4)).unwrap();
        assert_refused(
            &gap.path(1),
            &[&gap.name(3), &gap.name(gap.count)],
            &label("gap"),
        );

        let missing_last = set();
        fs::remove_file(missing_last.path(missing_last.count)).unwrap();
        assert_refused(
            &missing_last.path(1),
            &[&missing_last.name(missing_last.count)],
            &label("missing last"),
        );

        let truncated_middle = set();
        let bytes = fs::read(truncated_middle.path(2)).unwrap();
        fs::write(truncated_middle.path(2), &bytes[..bytes.len() - 1]).unwrap();
        assert_refused(
            &truncated_middle.path(1),
            &[&truncated_middle.name(2), &truncated_middle.name(3)],
            &label("truncated middle"),
        );

        let truncated_last = set();
        let last = truncated_last.path(truncated_last.count);
        let bytes = fs::read(&last).unwrap();
        fs::write(&last, &bytes[..bytes.len() - 1]).unwrap();
        assert_refused(
            &truncated_last.path(1),
            &[&truncated_last.name(truncated_last.count)],
            &label("truncated last"),
        );

        let extra = set();
        fs::write(extra.path(extra.count + 1), b"unrelated").unwrap();
        assert_refused(
            &extra.path(1),
            &[&extra.name(extra.count + 1)],
            &label("extra volume"),
        );

        let later = set();
        assert_refused(
            &later.path(2),
            &[&later.name(2), &later.name(1)],
            &label("later volume opened"),
        );

        if *archive == "tar.tar" {
            continue;
        }
        let repeated = set();
        fs::copy(repeated.path(2), repeated.path(3)).unwrap();
        assert_refused(
            &repeated.path(1),
            &[&repeated.name(3), &repeated.name(2)],
            &label("repeated"),
        );

        let swapped = set();
        fs::rename(swapped.path(2), swapped.path(99)).unwrap();
        fs::rename(swapped.path(3), swapped.path(2)).unwrap();
        fs::rename(swapped.path(99), swapped.path(3)).unwrap();
        assert_refused(
            &swapped.path(1),
            &[&swapped.name(1), "out of order"],
            &label("swapped"),
        );
    }
}

#[test]
fn an_extra_volume_after_a_full_last_volume_is_refused() {
    for archive in CHECKED {
        let length = fs::metadata(parity(archive)).unwrap().len() as usize;
        let volume = (2..length)
            .rev()
            .find(|size| length.is_multiple_of(*size))
            .unwrap();
        for junk in [&b"u"[..], b"unrelated"] {
            let set = Set::new(archive, volume);
            fs::write(set.path(set.count + 1), &junk[..junk.len().min(volume)]).unwrap();
            assert_refused(
                &set.path(1),
                &[&set.name(set.count + 1)],
                &format!(
                    "{archive}: {}-byte extra after {volume}-byte volumes",
                    junk.len()
                ),
            );
        }
    }
}
