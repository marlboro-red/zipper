//! Archives split into raw numbered volumes, `name.001`, `name.002` and on,
//! read in process as one stream. Each split must give the tree its whole
//! archive gives, and 7-Zip's own volumes the tree 7-Zip extracts. A missing,
//! truncated, repeated, swapped or extra volume must fail before anything is
//! published, with an error that names a volume.
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
            let stream = name.starts_with("hello.txt.");
            if stream && (volume >= length || length.is_multiple_of(volume)) {
                assert_refused(&first, &[&volume_name(name, 1)], &label);
                continue;
            }
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

const CHECKED: &[&str] = &[
    "7z.7z",
    "zip.zip",
    "zip-store.zip",
    "tar.tar.gz",
    "tar.tar.xz",
    "tar.tar",
];

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
            &[&missing_middle.name(2), &missing_middle.name(3)],
            &label("missing middle"),
        );

        let gap = set();
        fs::remove_file(gap.path(3)).unwrap();
        fs::remove_file(gap.path(4)).unwrap();
        assert_refused(&gap.path(1), &[&gap.name(3), &gap.name(5)], &label("gap"));

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

fn whole_tree(archive: &Path) -> BTreeMap<String, String> {
    extracted_tree(archive, 4, &archive.display().to_string())
}

#[test]
fn a_first_volume_whose_neighbours_do_not_form_a_set_is_read_alone() {
    let folder = tempfile::tempdir().unwrap();
    let zip = fs::read(parity("zip.zip")).unwrap();
    let expected = whole_tree(&parity("zip.zip"));
    fs::write(folder.path().join("backup.001"), &zip).unwrap();
    fs::write(folder.path().join("backup.002"), vec![7u8; zip.len() * 2]).unwrap();
    assert_eq!(
        whole_tree(&folder.path().join("backup.001")),
        expected,
        "a larger unrelated .002"
    );

    let alone = tempfile::tempdir().unwrap();
    fs::write(alone.path().join("backup.002"), &zip).unwrap();
    assert_eq!(
        whole_tree(&alone.path().join("backup.002")),
        expected,
        "a .002 with no .001"
    );

    let trailing = tempfile::tempdir().unwrap();
    let mut sevenz = fs::read(parity("7z.7z")).unwrap();
    sevenz.extend_from_slice(b"trailing bytes");
    fs::write(trailing.path().join("plain.7z"), &sevenz).unwrap();
    fs::write(trailing.path().join("a.7z.001"), &sevenz).unwrap();
    assert_eq!(
        whole_tree(&trailing.path().join("a.7z.001")),
        whole_tree(&trailing.path().join("plain.7z")),
        "a single 7z named .001 with trailing bytes"
    );

    let dots = tempfile::tempdir().unwrap();
    let gzip = fs::read(parity("hello.txt.gz")).unwrap();
    for name in ["..001", "...001"] {
        fs::write(dots.path().join(name), &gzip).unwrap();
        let names: Vec<_> = whole_tree(&dots.path().join(name)).into_keys().collect();
        assert_eq!(names, ["_"], "{name}");
    }
}

#[cfg(unix)]
#[test]
fn a_set_in_a_folder_that_cannot_be_listed_still_extracts() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("locked");
    fs::create_dir(&folder).unwrap();
    let bytes = fs::read(parity("zip.zip")).unwrap();
    let first = split(&bytes, &folder, "a.zip", 300);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o311)).unwrap();
    let listable = fs::read_dir(&folder).is_ok();
    let tree = extract(&first, 4);
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    if listable {
        return;
    }
    let (out, result) = tree;
    result.unwrap();
    assert_eq!(tree_of(out.path()), whole_tree(&parity("zip.zip")));
}

#[cfg(unix)]
fn tree_of(root: &Path) -> BTreeMap<String, String> {
    tree(&root.join("out"))
}

fn refused_with(archive: &Path, destination_exists: bool) -> anyhow::Error {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("out");
    if destination_exists {
        fs::create_dir(&destination).unwrap();
    }
    extract_archive(archive, &destination, ExtractOptions::default()).unwrap_err()
}

#[test]
fn the_volume_hint_follows_the_real_error_and_only_for_archive_failures() {
    let set = Set::new("7z.7z", 100);
    fs::remove_file(set.path(set.count)).unwrap();
    let error = refused_with(&set.path(1), false);
    let top = error.to_string();
    assert!(!top.starts_with("read as split volumes"), "{top}");
    assert!(top.contains(&set.name(set.count)), "{top}");

    let intact = Set::new("7z.7z", 100);
    let error = refused_with(&intact.path(1), true);
    assert!(
        !format!("{error:#}").contains("read as split volumes"),
        "{error:#}"
    );

    let whole = Set::new("tar.tar", 1000);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let cancel_on_report = |_: zipper::Progress| {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    };
    let root = tempfile::tempdir().unwrap();
    let error = extract_archive(
        &whole.path(1),
        &root.path().join("out"),
        ExtractOptions {
            threads: Some(1),
            control: zipper::Control {
                progress: Some(&cancel_on_report),
                cancel: Some(&cancel),
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.is::<zipper::Cancelled>(), "{error:#}");
    assert!(
        !format!("{error:#}").contains("read as split volumes"),
        "{error:#}"
    );

    for command in ["test", "list"] {
        let output = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .arg(command)
            .arg(set.path(1))
            .output()
            .unwrap();
        assert!(!output.status.success(), "{command}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&set.name(set.count)), "{command}: {stderr}");
    }
}

#[test]
fn numbered_names_map_to_their_first_volume_whatever_their_width() {
    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("a.bin.001"), b"first").unwrap();
    fs::write(folder.path().join("a.bin.1000"), b"later").unwrap();
    let error = format!(
        "{:#}",
        refused_with(&folder.path().join("a.bin.1000"), false)
    );
    assert!(error.contains("a.bin.001"), "{error}");
}

#[test]
fn a_gap_is_found_through_names_the_filesystem_treats_as_equal() {
    let probe = tempfile::tempdir().unwrap();
    fs::write(probe.path().join("A"), b"").unwrap();
    let folds_case = probe.path().join("a").exists();
    let nfc = "caf\u{e9}";
    let nfd = "cafe\u{301}";
    fs::write(probe.path().join(nfd), b"").unwrap();
    let folds_unicode = probe.path().join(nfc).exists();
    for (folds, stem, other_spelling) in [
        (folds_case, "a.zip", "A.ZIP"),
        (folds_unicode, "caf\u{e9}.zip", "cafe\u{301}.zip"),
    ] {
        if !folds {
            continue;
        }
        let set = Set::new("zip.zip", 291);
        assert_eq!(set.count, 4);
        let folder = set._folder.path();
        for number in 1..=set.count {
            fs::rename(set.path(number), folder.join(volume_name(stem, number))).unwrap();
        }
        fs::remove_file(folder.join(volume_name(stem, 3))).unwrap();
        fs::rename(
            folder.join(volume_name(stem, 4)),
            folder.join(volume_name(other_spelling, 4)),
        )
        .unwrap();
        let error = format!(
            "{:#}",
            refused_with(&folder.join(volume_name(stem, 1)), false)
        );
        assert!(error.contains(&volume_name(stem, 3)), "{stem}: {error}");
        assert!(error.contains("is present"), "{stem}: {error}");
    }
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn names_in(archive: &Path) -> Vec<String> {
    whole_tree(archive).into_keys().collect()
}

#[test]
fn neighbours_that_are_whole_archives_of_their_own_are_read_as_7zip_reads_them() {
    let folder = tempfile::tempdir().unwrap();
    let big = zip_of(&[("a.txt", &[b'a'; 400])]);
    let small = zip_of(&[("b.txt", b"b")]);
    assert!(small.len() < big.len());

    fs::write(folder.path().join("db.001"), &big).unwrap();
    fs::write(folder.path().join("db.002"), &small).unwrap();
    assert_refused(
        &folder.path().join("db.001"),
        &["db.001", "db.002", "rename"],
        "a whole ZIP beside a smaller ZIP",
    );
    assert_eq!(names_in(&folder.path().join("db.002")), ["b.txt"]);

    let zero = tempfile::tempdir().unwrap();
    fs::write(zero.path().join("x.000"), &small).unwrap();
    fs::write(zero.path().join("x.001"), &big).unwrap();
    assert_eq!(names_in(&zero.path().join("x.000")), ["b.txt"]);

    let tar = tempfile::tempdir().unwrap();
    let whole_tar = fs::read(parity("tar.tar")).unwrap();
    fs::write(tar.path().join("x.tar.001"), &whole_tar).unwrap();
    fs::write(tar.path().join("x.tar.002"), &whole_tar[..2048]).unwrap();
    assert_refused(
        &tar.path().join("x.tar.001"),
        &["x.tar.001", "x.tar.002", "rename"],
        "a whole tar beside part of another",
    );

    let gzip = tempfile::tempdir().unwrap();
    let hello = fs::read(parity("hello.txt.gz")).unwrap();
    fs::write(gzip.path().join("log.gz.001"), &hello).unwrap();
    fs::write(gzip.path().join("log.gz.002"), gzip_member(b"hi")).unwrap();
    let joined = whole_tree(&gzip.path().join("log.gz.001"));
    assert_eq!(joined.keys().collect::<Vec<_>>(), ["log"]);
}

#[test]
fn a_set_replaced_while_another_call_holds_it_extracts_as_the_new_set() {
    let folder = tempfile::tempdir().unwrap();
    let bytes = fs::read(parity("7z.7z")).unwrap();
    let first = split(&bytes, folder.path(), "s.7z", 30);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (holding, started) = std::sync::mpsc::channel::<()>();
    let holder = {
        let first = first.clone();
        std::thread::spawn(move || {
            zipper::read_volumes(&first, || {
                zipper::choose_reader(Engine::Auto, &first)?;
                holding.send(()).unwrap();
                held.recv().unwrap();
                Ok(())
            })
        })
    };
    started.recv().unwrap();
    for number in 1..=bytes.len().div_ceil(30) {
        fs::remove_file(folder.path().join(volume_name("s.7z", number))).unwrap();
    }
    split(&bytes, folder.path(), "s.7z", 50);
    assert_eq!(
        extracted_tree(&first, 4, "replaced set"),
        whole_tree(&parity("7z.7z"))
    );
    release.send(()).unwrap();
    holder.join().unwrap().unwrap();
}

#[test]
fn every_failure_but_cancel_destination_and_size_notes_the_volumes() {
    const NOTE: &str = "read as split volumes";
    let folder = tempfile::tempdir().unwrap();
    let escaping = zip_of(&[("../escape.txt", b"x")]);
    let first = split(&escaping, folder.path(), "e.zip", 40);
    let error = format!("{:#}", refused_with(&first, false));
    assert!(error.contains(NOTE), "unsafe path: {error}");

    let set = Set::new("zip.zip", 300);
    for (case, noted, options) in [
        (
            "selected path not found",
            true,
            ExtractOptions {
                paths: &["no such entry".to_owned()],
                ..Default::default()
            },
        ),
        (
            "max extracted bytes",
            false,
            ExtractOptions {
                max_output_bytes: Some(1),
                ..Default::default()
            },
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let error = extract_archive(&set.path(1), &root.path().join("out"), options).unwrap_err();
        let error = format!("{error:#}");
        assert_eq!(error.contains(NOTE), noted, "{case}: {error}");
    }

    let root = tempfile::tempdir().unwrap();
    let error = extract_archive(
        &set.path(1),
        &root.path().join("no such folder").join("out"),
        ExtractOptions::default(),
    )
    .unwrap_err();
    let error = format!("{error:#}");
    assert!(!error.contains(NOTE), "staging cannot be created: {error}");

    let missing = Set::new("zip.zip", 300);
    fs::remove_file(missing.path(missing.count)).unwrap();
    let error = format!("{:#}", refused_with(&missing.path(1), false));
    assert!(error.contains(NOTE), "{error}");
    assert!(!error.contains("--zip-engine backend"), "{error}");

    let tiny = tempfile::tempdir().unwrap();
    let first = split(b"PK\x05\x06tiny", tiny.path(), "t.zip", 4);
    let error = format!("{:#}", refused_with(&first, false));
    assert!(error.contains(NOTE), "{error}");
    assert!(!error.contains("--zip-engine backend"), "{error}");
}

#[cfg(unix)]
#[test]
fn a_volume_that_cannot_be_opened_is_named_by_its_path() {
    use std::os::unix::fs::PermissionsExt;
    let set = Set::new("zip.zip", 300);
    let locked = set.path(2);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&locked).is_ok() {
        return;
    }
    let error = format!("{:#}", refused_with(&set.path(1), false));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(error.contains(&locked.display().to_string()), "{error}");
}

#[test]
fn a_damaged_set_is_never_read_as_its_first_volume_alone() {
    for damage in ["missing middle", "short middle"] {
        let folder = tempfile::tempdir().unwrap();
        let stem = "log.bin.gz";
        let count = member_boundary_set(folder.path(), stem, gzip_member);
        assert!(count >= 4);
        let second = folder.path().join(volume_name(stem, 2));
        match damage {
            "missing middle" => fs::remove_file(&second).unwrap(),
            _ => {
                let bytes = fs::read(&second).unwrap();
                fs::write(&second, &bytes[..bytes.len() - 1]).unwrap();
            }
        }
        let first = folder.path().join(volume_name(stem, 1));
        assert_refused(&first, &[&volume_name(stem, 2)], damage);
        let tested = zipper_cli(&["test".as_ref(), first.as_os_str()]);
        assert!(!tested.status.success(), "{damage}: zipper test passed");
    }
}

fn stored_gzip_member(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
    std::io::Write::write_all(&mut encoder, bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn a_stream_set_whose_last_volume_is_full_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let members: Vec<Vec<u8>> = (0..3)
        .map(|seed| stored_gzip_member(&noise(5000, seed + 10)))
        .collect();
    assert!(
        members
            .iter()
            .all(|member| member.len() == members[0].len())
    );
    for (index, member) in members.iter().enumerate() {
        fs::write(
            folder.path().join(volume_name("m.bin.gz", index + 1)),
            member,
        )
        .unwrap();
    }
    fs::remove_file(folder.path().join(volume_name("m.bin.gz", 3))).unwrap();
    let first = folder.path().join(volume_name("m.bin.gz", 1));
    assert_refused(&first, &["m.bin.gz.003"], "trailing volume dropped");
    let tested = zipper_cli(&["test".as_ref(), first.as_os_str()]);
    assert!(!tested.status.success(), "zipper test passed");
}

#[test]
fn a_retry_alone_never_makes_progress_counts_shrink() {
    let folder = tempfile::tempdir().unwrap();
    let whole_tar = fs::read(parity("tar.tar")).unwrap();
    fs::write(folder.path().join("x.tar.001"), &whole_tar).unwrap();
    fs::write(folder.path().join("x.tar.002"), &whole_tar[..2048]).unwrap();
    let reports = std::sync::Mutex::new(Vec::new());
    let record = |progress: zipper::Progress| {
        reports
            .lock()
            .unwrap()
            .push((progress.files_done, progress.bytes_done));
    };
    let root = tempfile::tempdir().unwrap();
    let _ = extract_archive(
        &folder.path().join("x.tar.001"),
        &root.path().join("out"),
        ExtractOptions {
            control: zipper::Control {
                progress: Some(&record),
                cancel: None,
            },
            ..Default::default()
        },
    );
    let reports = reports.into_inner().unwrap();
    assert!(!reports.is_empty());
    for pair in reports.windows(2) {
        assert!(
            pair[1].0 >= pair[0].0 && pair[1].1 >= pair[0].1,
            "{reports:?}"
        );
    }
}

#[test]
fn a_cancelled_read_runs_once_and_a_zero_numbered_file_is_no_volume() {
    let set = Set::new("zip.zip", 300);
    let calls = std::cell::Cell::new(0);
    let error = zipper::read_volumes(&set.path(1), || -> anyhow::Result<()> {
        calls.set(calls.get() + 1);
        Err(zipper::Cancelled.into())
    })
    .unwrap_err();
    assert!(error.is::<zipper::Cancelled>());
    assert_eq!(calls.get(), 1);

    let folder = tempfile::tempdir().unwrap();
    fs::write(folder.path().join("x.000"), b"not an archive").unwrap();
    fs::write(
        folder.path().join("x.001"),
        fs::read(parity("zip.zip")).unwrap(),
    )
    .unwrap();
    let error = format!("{:#}", refused_with(&folder.path().join("x.000"), false));
    assert!(!error.contains("x.001"), "{error}");
}

fn gzip_member(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut encoder, bytes).unwrap();
    encoder.finish().unwrap()
}

fn zstd_frame(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1).unwrap();
    encoder.include_checksum(true).unwrap();
    std::io::Write::write_all(&mut encoder, bytes).unwrap();
    encoder.finish().unwrap()
}

fn xz_stream(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 1);
    std::io::Write::write_all(&mut encoder, bytes).unwrap();
    encoder.finish().unwrap()
}

fn noise(length: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..length)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 24) as u8
        })
        .collect()
}

fn member_boundary_set(folder: &Path, stem: &str, member: fn(&[u8]) -> Vec<u8>) -> usize {
    let first = member(&noise(4000, 1));
    let mut rest = member(&noise(9000, 2));
    rest.extend(member(&noise(9000, 3)));
    fs::write(folder.join(volume_name(stem, 1)), &first).unwrap();
    for (index, chunk) in rest.chunks(first.len()).enumerate() {
        fs::write(folder.join(volume_name(stem, index + 2)), chunk).unwrap();
    }
    1 + rest.len().div_ceil(first.len())
}

fn zipper_cli(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_zipper"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_damaged_set_whose_first_volume_ends_on_a_member_boundary_fails() {
    for (stem, member) in [
        ("log.bin.gz", gzip_member as fn(&[u8]) -> Vec<u8>),
        ("log.bin.zst", zstd_frame),
        ("log.bin.xz", xz_stream),
    ] {
        for damage in ["flip", "drop last"] {
            let folder = tempfile::tempdir().unwrap();
            let count = member_boundary_set(folder.path(), stem, member);
            let first = folder.path().join(volume_name(stem, 1));
            match damage {
                "flip" => {
                    let second = folder.path().join(volume_name(stem, 2));
                    let mut bytes = fs::read(&second).unwrap();
                    let middle = bytes.len() / 2;
                    bytes[middle] ^= 0x40;
                    fs::write(&second, bytes).unwrap();
                }
                _ => fs::remove_file(folder.path().join(volume_name(stem, count))).unwrap(),
            }
            let label = format!("{stem}: {damage}");
            assert_refused(&first, &[&volume_name(stem, 1)], &label);
            let tested = zipper_cli(&["test".as_ref(), first.as_os_str()]);
            assert!(!tested.status.success(), "{label}: zipper test passed");
            let root = tempfile::tempdir().unwrap();
            let out = root.path().join("out");
            let extracted = zipper_cli(&[
                "extract".as_ref(),
                first.as_os_str(),
                "-o".as_ref(),
                out.as_os_str(),
            ]);
            assert!(
                !extracted.status.success(),
                "{label}: zipper extract passed"
            );
        }
    }
}

#[test]
fn a_size_cap_on_an_intact_set_fails_instead_of_reading_the_first_volume_alone() {
    let folder = tempfile::tempdir().unwrap();
    member_boundary_set(folder.path(), "log.bin.gz", gzip_member);
    let first = folder.path().join("log.bin.gz.001");
    let root = tempfile::tempdir().unwrap();
    let capped = extract_archive(
        &first,
        &root.path().join("out"),
        ExtractOptions {
            max_output_bytes: Some(10_000),
            ..Default::default()
        },
    );
    assert!(capped.is_err(), "the cap was not applied to the whole set");
    let out = root.path().join("cli");
    let cli = zipper_cli(&[
        "extract".as_ref(),
        "--max-extracted-bytes".as_ref(),
        "10000".as_ref(),
        first.as_os_str(),
        "-o".as_ref(),
        out.as_os_str(),
    ]);
    assert!(!cli.status.success(), "the CLI read the first volume alone");
    assert!(!out.exists());
}

#[test]
fn every_decoder_error_on_a_damaged_set_names_its_volumes() {
    for (stem, member) in [
        ("blob.bin.zst", zstd_frame as fn(&[u8]) -> Vec<u8>),
        ("blob.bin.gz", gzip_member),
    ] {
        let folder = tempfile::tempdir().unwrap();
        let mut whole = member(&noise(30000, 4));
        whole.extend(member(&noise(30000, 5)));
        let first = split(&whole, folder.path(), stem, 4000);
        let third = folder.path().join(volume_name(stem, 3));
        fs::copy(folder.path().join(volume_name(stem, 2)), &third).unwrap();
        assert_refused(
            &first,
            &[
                &volume_name(stem, 1),
                &volume_name(stem, 3),
                &volume_name(stem, 2),
            ],
            &format!("{stem}: repeated"),
        );
    }
}

#[test]
fn a_volume_opened_by_name_gets_no_backend_advice() {
    for archive in ["7z.7z", "tar.tar.gz", "tar.tar"] {
        let set = Set::new(archive, 300);
        let error = format!("{:#}", refused_with(&set.path(2), false));
        assert!(error.contains(&set.name(1)), "{archive}: {error}");
        assert!(
            !error.contains("--zip-engine backend"),
            "{archive}: {error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_metadata_change_that_leaves_the_bytes_alone_does_not_fail_a_set() {
    use std::os::unix::fs::PermissionsExt;
    let set = Set::new("7z.7z", 100);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (opened, started) = std::sync::mpsc::channel::<()>();
    let first = set.path(1);
    let reader = std::thread::spawn(move || {
        let held = std::cell::RefCell::new(Some(held));
        zipper::read_volumes(&first, || {
            if let Some(held) = held.borrow_mut().take() {
                opened.send(()).ok();
                held.recv().ok();
            }
            zipper::native_7z::list(&first)
        })
    });
    started.recv().unwrap();
    for number in 1..=set.count {
        fs::set_permissions(set.path(number), fs::Permissions::from_mode(0o600)).unwrap();
    }
    release.send(()).unwrap();
    let entries = reader.join().unwrap().unwrap();
    assert!(!entries.is_empty());
}

#[test]
fn a_missing_or_dangling_first_volume_is_a_clean_error_naming_its_path() {
    let folder = tempfile::tempdir().unwrap();
    let missing = folder.path().join("nothere.zip.001");
    let dangling = folder.path().join("dangling.zip.001");
    #[cfg(unix)]
    std::os::unix::fs::symlink(folder.path().join("gone"), &dangling).unwrap();
    for first in [&missing, &dangling] {
        if !cfg!(unix) && first == &dangling {
            continue;
        }
        let error = format!("{:#}", refused_with(first, false));
        assert!(error.contains(&first.display().to_string()), "{error}");
        for command in ["extract", "list", "test"] {
            let root = tempfile::tempdir().unwrap();
            let out = root.path().join("out");
            let mut args: Vec<&std::ffi::OsStr> = vec![command.as_ref(), first.as_os_str()];
            if command == "extract" {
                args.extend(["-o".as_ref(), out.as_os_str()]);
            }
            let run = zipper_cli(&args);
            assert_eq!(run.status.code(), Some(1), "{command} {}", first.display());
            let stderr = String::from_utf8_lossy(&run.stderr);
            assert!(
                stderr.contains(&first.display().to_string()),
                "{command}: {stderr}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_first_volume_that_is_not_a_regular_file_is_named_by_its_full_path() {
    let folder = tempfile::tempdir().unwrap();
    let fifo = folder.path().join("pipe.zip.001");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let error = format!("{:#}", refused_with(&fifo, false));
    assert!(
        error.contains(&format!("{} is not a regular file", fifo.display())),
        "{error}"
    );
    assert!(!error.contains("later volumes"), "{error}");
}

#[test]
fn a_stream_first_volume_is_never_read_alone() {
    let folder = tempfile::tempdir().unwrap();
    let stem = "log.bin.gz";
    member_boundary_set(folder.path(), stem, gzip_member);
    let mut index = 2;
    while folder.path().join(volume_name(stem, index)).exists() {
        fs::remove_file(folder.path().join(volume_name(stem, index))).unwrap();
        index += 1;
    }
    let first = folder.path().join(volume_name(stem, 1));
    assert_refused(&first, &[&volume_name(stem, 1)], "lone stream .001");

    let larger = folder.path().join(volume_name(stem, 2));
    fs::write(
        &larger,
        vec![9u8; fs::metadata(&first).unwrap().len() as usize * 2],
    )
    .unwrap();
    assert_refused(
        &first,
        &[&volume_name(stem, 1)],
        "stream .001 beside a larger .002",
    );
}

fn split_into(source: &Path, chunks: usize, folder: &Path, stem: &str) -> Option<PathBuf> {
    let prefix = folder.join(format!("{stem}.part."));
    let status = Command::new("split")
        .arg("-n")
        .arg(chunks.to_string())
        .arg(source)
        .arg(&prefix)
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let mut parts: Vec<PathBuf> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().contains(".part."))
        .collect();
    parts.sort();
    assert_eq!(parts.len(), chunks);
    for (index, part) in parts.iter().enumerate() {
        fs::rename(part, folder.join(volume_name(stem, index + 1))).unwrap();
    }
    Some(folder.join(volume_name(stem, 1)))
}

#[test]
fn sets_from_split_by_chunk_count_join_as_7zip_joins_them() {
    let work = tempfile::tempdir().unwrap();
    let odd_zip = work.path().join("odd.zip");
    let mut zip = fs::read(parity("zip.zip")).unwrap();
    if zip.len().is_multiple_of(2) {
        zip = zip_of(&[("a.txt", &[b'a'; 401]), ("b.txt", b"odd")]);
    }
    assert_eq!(zip.len() % 2, 1);
    fs::write(&odd_zip, &zip).unwrap();
    for (source, stem) in [
        (odd_zip.clone(), "o.zip"),
        (parity("zip.zip"), "z.zip"),
        (parity("tar.tar.gz"), "t.tar.gz"),
        (parity("hello.txt.gz"), "hello.txt.gz"),
    ] {
        for chunks in [2, 3] {
            let folder = tempfile::tempdir().unwrap();
            let Some(first) = split_into(&source, chunks, folder.path(), stem) else {
                return;
            };
            let label = format!("{stem} split into {chunks}");
            assert_eq!(
                extracted_tree(&first, 4, &label),
                whole_tree(&source),
                "{label}"
            );
        }
    }
}
