//! Archives split into raw numbered volumes, `name.001`, `name.002` and on,
//! read in process as one stream. Each split must give the tree its whole
//! archive gives, and 7-Zip's own volumes the tree 7-Zip extracts. A missing,
//! truncated, repeated, swapped or extra volume must fail before anything is
//! published, with an error that names a volume. A plain tar's own errors have
//! no type that marks them as about the archive's bytes, so they name none.
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
    let plain_tar_errors_carry_no_volume_names = label.starts_with("tar.tar:");
    for name in names {
        assert!(
            plain_tar_errors_carry_no_volume_names || error.contains(name),
            "{label}: {name} not in: {error}"
        );
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
    assert!(!top.starts_with("7z.7z.001 is the first"), "{top}");
    assert!(top.contains(&set.name(set.count)), "{top}");

    let error = refused_with(&set.path(1), true);
    assert!(!format!("{error:#}").contains("split volumes"), "{error:#}");

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
    assert!(!format!("{error:#}").contains("split volumes"), "{error:#}");

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
fn neighbours_that_are_whole_archives_of_their_own_extract_as_main_does() {
    let folder = tempfile::tempdir().unwrap();
    let big = zip_of(&[("a.txt", &[b'a'; 400])]);
    let small = zip_of(&[("b.txt", b"b")]);
    assert!(small.len() < big.len());

    fs::write(folder.path().join("db.001"), &big).unwrap();
    fs::write(folder.path().join("db.002"), &small).unwrap();
    assert_eq!(names_in(&folder.path().join("db.001")), ["a.txt"]);
    assert_eq!(names_in(&folder.path().join("db.002")), ["b.txt"]);

    let zero = tempfile::tempdir().unwrap();
    fs::write(zero.path().join("x.000"), &small).unwrap();
    fs::write(zero.path().join("x.001"), &big).unwrap();
    assert_eq!(names_in(&zero.path().join("x.000")), ["b.txt"]);

    let tar = tempfile::tempdir().unwrap();
    let whole_tar = fs::read(parity("tar.tar")).unwrap();
    fs::write(tar.path().join("x.tar.001"), &whole_tar).unwrap();
    fs::write(tar.path().join("x.tar.002"), &whole_tar[..2048]).unwrap();
    assert_eq!(
        whole_tree(&tar.path().join("x.tar.001")),
        whole_tree(&parity("tar.tar"))
    );

    let gzip = tempfile::tempdir().unwrap();
    let hello = fs::read(parity("hello.txt.gz")).unwrap();
    fs::write(gzip.path().join("log.gz.001"), &hello).unwrap();
    fs::write(gzip.path().join("log.gz.002"), &hello[..hello.len() - 1]).unwrap();
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
fn errors_that_are_not_about_the_archive_bytes_name_no_volumes() {
    let folder = tempfile::tempdir().unwrap();
    let escaping = zip_of(&[("../escape.txt", b"x")]);
    let first = split(&escaping, folder.path(), "e.zip", 40);
    let error = format!("{:#}", refused_with(&first, false));
    assert!(!error.contains("split volumes"), "unsafe path: {error}");

    let set = Set::new("zip.zip", 300);
    for (case, options) in [
        (
            "selected path not found",
            ExtractOptions {
                paths: &["no such entry".to_owned()],
                ..Default::default()
            },
        ),
        (
            "max extracted bytes",
            ExtractOptions {
                max_output_bytes: Some(1),
                ..Default::default()
            },
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let error = extract_archive(&set.path(1), &root.path().join("out"), options).unwrap_err();
        let error = format!("{error:#}");
        assert!(!error.contains("split volumes"), "{case}: {error}");
    }

    let missing = Set::new("zip.zip", 300);
    fs::remove_file(missing.path(missing.count)).unwrap();
    let error = format!("{:#}", refused_with(&missing.path(1), false));
    assert!(error.contains("split volumes"), "{error}");
    assert!(!error.contains("--zip-engine backend"), "{error}");
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
