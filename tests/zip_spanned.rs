//! ZIP split sets, `name.z01`, `name.z02` and on with `name.zip` last, read
//! in process. Offsets in a set count from the start of their own volume, so
//! every disk number and offset must land inside the volume it names. A set
//! must extract the tree 7-Zip extracts, and a gap, a repeat, a swap, an
//! overlap, a missing or extra volume, or a disk number outside the set must
//! fail before anything is published, with an error that names a volume.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{Engine, ExtractOptions, Reader, choose_reader, extract_archive};

const SET: &str = "infozip-split";
const ONE_PIECE: &str = "infozip-split-one-piece";

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-corpus")
}

fn volume_paths(folder: &Path, stem: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = (1..)
        .map(|number| folder.join(format!("{stem}.z{number:02}")))
        .take_while(|path| path.exists())
        .collect();
    paths.push(folder.join(format!("{stem}.zip")));
    paths
}

fn copy_set(stem: &str, into: &Path) -> Vec<PathBuf> {
    volume_paths(&corpus(), stem)
        .iter()
        .map(|path| {
            let copy = into.join(path.file_name().unwrap());
            fs::copy(path, &copy).unwrap();
            copy
        })
        .collect()
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

fn extracted_tree(archive: &Path, threads: usize) -> BTreeMap<String, String> {
    let (root, result) = extract(archive, threads);
    if let Err(error) = result {
        panic!("{}: {error:#}", archive.display());
    }
    tree(&root.path().join("out"))
}

fn assert_refused(archive: &Path, names: &[&str], label: &str) -> String {
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
    error
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

fn tree_by_7zip(binary: &Path, archive: &Path) -> BTreeMap<String, String> {
    let out = tempfile::tempdir().unwrap();
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(out.path());
    let output = Command::new(binary)
        .args(["x", "-y", "-bd"])
        .arg(flag)
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "7-Zip on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&output.stdout)
    );
    tree(out.path())
}

fn info_zip() -> bool {
    Command::new("zip")
        .arg("-v")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn many_files_tree(folder: &Path) -> PathBuf {
    let source = folder.join("source");
    for index in 0..1500u32 {
        let path = source.join(format!(
            "folder-{:02}/a-name-long-enough-to-grow-the-directory-{index:05}.txt",
            index % 17
        ));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("file {index}\n").repeat(index as usize % 40)).unwrap();
    }
    let mut state = 11u32;
    let noise: Vec<u8> = (0..200_000)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 24) as u8
        })
        .collect();
    fs::write(source.join("noise.bin"), noise).unwrap();
    source
}

fn info_zip_split(folder: &Path, stem: &str, flags: &[&str]) -> PathBuf {
    let status = Command::new("zip")
        .current_dir(folder)
        .args(["-q", "-r"])
        .args(flags)
        .arg(format!("{stem}.zip"))
        .arg("source")
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "zip {flags:?}");
    folder.join(format!("{stem}.zip"))
}

#[test]
fn info_zip_sets_extract_as_7zip_extracts_them() {
    let seven = sevenzip();
    for stem in [SET, ONE_PIECE] {
        let folder = tempfile::tempdir().unwrap();
        let volumes = copy_set(stem, folder.path());
        let last = volumes.last().unwrap();
        assert_eq!(
            choose_reader(Engine::Auto, last).unwrap(),
            Reader::Zip { fast: false },
            "{stem}"
        );
        let ours = extracted_tree(last, 1);
        assert_eq!(extracted_tree(last, 4), ours, "{stem}");
        if let Some(binary) = &seven {
            assert_eq!(tree_by_7zip(binary, last), ours, "{stem}");
        }
    }
    if !info_zip() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let source = many_files_tree(folder.path());
    let unsplit = info_zip_split(folder.path(), "whole", &[]);
    let whole = extracted_tree(&unsplit, 4);
    assert!(whole.len() > 1500);
    for (stem, flags) in [
        ("stored-64k", &["-0", "-s", "64k"][..]),
        ("deflated-64k", &["-s", "64k"][..]),
        ("deflated-65k", &["-s", "65k"][..]),
        ("deflated-100k", &["-s", "100k"][..]),
        ("zip64-64k", &["-fz", "-s", "64k"][..]),
    ] {
        let last = info_zip_split(folder.path(), stem, flags);
        assert!(
            folder.path().join(format!("{stem}.z02")).exists(),
            "{stem} is not split"
        );
        for threads in [1, 4] {
            assert_eq!(extracted_tree(&last, threads), whole, "{stem}");
        }
        if let Some(binary) = &seven {
            assert_eq!(tree_by_7zip(binary, &last), whole, "{stem}");
        }
    }
    drop(source);
}

fn joined(volumes: &[PathBuf]) -> (Vec<u8>, Vec<usize>) {
    let mut bytes = Vec::new();
    let mut sizes = Vec::new();
    for volume in volumes {
        let data = fs::read(volume).unwrap();
        sizes.push(data.len());
        bytes.extend_from_slice(&data);
    }
    (bytes, sizes)
}

fn rewrite(volumes: &[PathBuf], bytes: &[u8], sizes: &[usize]) {
    let mut at = 0;
    for (volume, size) in volumes.iter().zip(sizes) {
        fs::write(volume, &bytes[at..at + size]).unwrap();
        at += size;
    }
    assert_eq!(at, bytes.len());
}

fn le16(bytes: &[u8], at: usize) -> usize {
    u16::from_le_bytes([bytes[at], bytes[at + 1]]).into()
}

fn le32(bytes: &[u8], at: usize) -> usize {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
}

struct Layout {
    end: usize,
    central: Vec<usize>,
}

fn layout(bytes: &[u8], sizes: &[usize]) -> Layout {
    let end = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .unwrap();
    let starts: Vec<usize> = sizes
        .iter()
        .scan(0, |at, size| {
            let start = *at;
            *at += size;
            Some(start)
        })
        .collect();
    let mut position = starts[le16(bytes, end + 6)] + le32(bytes, end + 16);
    let mut central = Vec::new();
    for _ in 0..le16(bytes, end + 10) {
        assert_eq!(&bytes[position..position + 4], b"PK\x01\x02");
        central.push(position);
        position += 46
            + le16(bytes, position + 28)
            + le16(bytes, position + 30)
            + le16(bytes, position + 32);
    }
    Layout { end, central }
}

fn patched_set(
    change: impl FnOnce(&mut Vec<u8>, &Layout, &[usize]),
) -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let volumes = copy_set(SET, folder.path());
    let (mut bytes, sizes) = joined(&volumes);
    let found = layout(&bytes, &sizes);
    change(&mut bytes, &found, &sizes);
    rewrite(&volumes, &bytes, &sizes);
    let last = volumes.last().unwrap().clone();
    (folder, last)
}

#[test]
fn disk_numbers_and_offsets_outside_their_volume_are_refused() {
    let (_folder, last) = patched_set(|bytes, found, _| {
        let entry = found.central[3];
        bytes[entry + 34..entry + 36].copy_from_slice(&9u16.to_le_bytes());
    });
    assert_refused(
        &last,
        &["disk 10", "infozip-split.z01 to infozip-split.zip"],
        "entry disk",
    );
    let (_folder, last) = patched_set(|bytes, found, sizes| {
        let entry = *found.central.last().unwrap();
        let disk = le16(bytes, entry + 34);
        let past_its_volume = (sizes[disk] + 4) as u32;
        bytes[entry + 42..entry + 46].copy_from_slice(&past_its_volume.to_le_bytes());
    });
    assert_refused(&last, &["infozip-split.z0"], "entry offset past its volume");
    let (_folder, last) = patched_set(|bytes, found, _| {
        bytes[found.end + 6..found.end + 8].copy_from_slice(&9u16.to_le_bytes());
    });
    assert_refused(&last, &["disk 10"], "directory disk");
    let (_folder, last) = patched_set(|bytes, found, sizes| {
        let disk = le16(bytes, found.end + 6);
        let past = (sizes[disk] + 1) as u32;
        bytes[found.end + 16..found.end + 20].copy_from_slice(&past.to_le_bytes());
    });
    assert_refused(
        &last,
        &["infozip-split.z"],
        "directory offset past its volume",
    );
    let (_folder, last) = patched_set(|bytes, found, _| {
        bytes[found.end + 4..found.end + 6].copy_from_slice(&3u16.to_le_bytes());
    });
    assert_refused(
        &last,
        &["4 volumes", "6 were found"],
        "end record disk count",
    );
}

#[test]
fn a_gap_repeat_swap_overlap_or_wrong_count_is_refused_by_name() {
    let refused = |change: &dyn Fn(&[PathBuf]), names: &[&str], label: &str| {
        let folder = tempfile::tempdir().unwrap();
        let volumes = copy_set(SET, folder.path());
        change(&volumes);
        assert_refused(volumes.last().unwrap(), names, label);
    };
    refused(
        &|volumes| fs::remove_file(&volumes[2]).unwrap(),
        &["infozip-split.z03 is missing", "infozip-split.z04"],
        "gap",
    );
    refused(
        &|volumes| fs::copy(&volumes[1], &volumes[2]).map(drop).unwrap(),
        &["infozip-split.z03 holds the same bytes as infozip-split.z02"],
        "repeat",
    );
    refused(
        &|volumes| {
            let (first, second) = (
                fs::read(&volumes[0]).unwrap(),
                fs::read(&volumes[1]).unwrap(),
            );
            fs::write(&volumes[0], second).unwrap();
            fs::write(&volumes[1], first).unwrap();
        },
        &["infozip-split.z01 to infozip-split.zip"],
        "swap",
    );
    refused(
        &|volumes| {
            let (bytes, sizes) = joined(volumes);
            let mut overlapped = Vec::new();
            let mut at = 0;
            for (index, size) in sizes.iter().enumerate() {
                let start = if index == 2 { at - 100 } else { at };
                overlapped.push(bytes[start..at + size].to_vec());
                at += size;
            }
            for (volume, data) in volumes.iter().zip(overlapped) {
                fs::write(volume, data).unwrap();
            }
        },
        &["infozip-split.z01 to infozip-split.zip"],
        "overlap",
    );
    refused(
        &|volumes| {
            let last_volume = &volumes[volumes.len() - 2];
            let extra = last_volume.with_extension("z06");
            fs::copy(last_volume, extra).unwrap();
        },
        &["has 6 volumes", "7 were found"],
        "extra volume",
    );
    refused(
        &|volumes| fs::remove_file(&volumes[volumes.len() - 2]).unwrap(),
        &["has 6 volumes", "5 were found"],
        "missing last volume",
    );
    refused(
        &|volumes| fs::write(&volumes[1], b"").unwrap(),
        &["infozip-split.z02 is empty"],
        "empty volume",
    );
    refused(
        &|volumes| {
            fs::remove_file(&volumes[1]).unwrap();
            fs::create_dir(&volumes[1]).unwrap();
        },
        &["infozip-split.z02 is not a file"],
        "folder volume",
    );
    #[cfg(unix)]
    refused(
        &|volumes| {
            fs::remove_file(&volumes[1]).unwrap();
            let made = Command::new("mkfifo").arg(&volumes[1]).status().unwrap();
            assert!(made.success());
        },
        &["infozip-split.z02 is not a file"],
        "FIFO volume",
    );
}

#[test]
fn a_volume_opened_by_itself_points_at_the_last_volume() {
    let folder = tempfile::tempdir().unwrap();
    let volumes = copy_set(SET, folder.path());
    assert_refused(
        &volumes[0],
        &["volume 1 of a split ZIP", "infozip-split.zip"],
        "first volume",
    );
    assert_refused(&volumes[2], &["volume 3 of a split ZIP"], "middle volume");
    fs::remove_file(volumes.last().unwrap()).unwrap();
    assert_refused(
        &volumes[0],
        &["infozip-split.zip", "is missing"],
        "missing last volume",
    );
}

fn folder_with_a_whole_7z_named_zip() -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/parity/archives/7z.7z"),
        folder.path().join("x.zip"),
    )
    .unwrap();
    fs::write(folder.path().join("x.z01"), b"junk").unwrap();
    folder
}

#[test]
fn a_whole_zip_beside_a_stray_volume_or_a_lone_last_volume_is_refused() {
    let folder = tempfile::tempdir().unwrap();
    let whole = folder.path().join("whole.zip");
    fs::copy(corpus().join("infozip-default.zip"), &whole).unwrap();
    fs::write(folder.path().join("whole.z01"), b"stray").unwrap();
    assert_refused(
        &whole,
        &["whole.zip looks whole", "whole.z01", "move them aside"],
        "stray volume",
    );
    let stray_7z = folder_with_a_whole_7z_named_zip();
    assert_refused(
        &stray_7z.path().join("x.zip"),
        &["by itself is a 7z archive", "move them aside"],
        "a whole 7z named x.zip",
    );
    let folder = tempfile::tempdir().unwrap();
    let lone = folder.path().join("lone.zip");
    fs::copy(corpus().join(format!("{SET}.zip")), &lone).unwrap();
    assert_refused(&lone, &["split archive", ".z01"], "lone last volume");
}

#[test]
fn an_encrypted_set_stays_refused() {
    if !info_zip() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    many_files_tree(folder.path());
    let last = info_zip_split(
        folder.path(),
        "locked",
        &["-0", "-P", "secret", "-s", "64k"],
    );
    assert!(folder.path().join("locked.z02").exists());
    assert_refused(&last, &["encrypted"], "encrypted set");
}

#[test]
fn a_zip64_locator_outside_the_set_is_refused_by_name() {
    if !info_zip() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    many_files_tree(folder.path());
    let last = info_zip_split(folder.path(), "wide", &["-fz", "-s", "64k"]);
    let volumes = volume_paths(folder.path(), "wide");
    assert!(volumes.len() > 2);
    extracted_tree(&last, 4);
    let original = fs::read(&last).unwrap();
    let locator = original
        .windows(4)
        .rposition(|window| window == b"PK\x06\x07")
        .unwrap();
    for (field, value, names) in [
        (
            16,
            volumes.len() as u32 + 1,
            &["ZIP64 locator", "wide.zip"][..],
        ),
        (4, 40, &["ZIP64 locator", "disk 41"][..]),
    ] {
        let mut patched = original.clone();
        patched[locator + field..locator + field + 4].copy_from_slice(&value.to_le_bytes());
        fs::write(&last, &patched).unwrap();
        assert_refused(&last, names, "ZIP64 locator");
    }
}

fn zipper_cli(args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_zipper"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_volume_beside_a_whole_zip_is_never_joined_for_another_reader() {
    for (stem, other) in [("poly-7z", "from-7z.txt"), ("poly-tar", "from-tar.txt")] {
        let folder = tempfile::tempdir().unwrap();
        for extension in ["z01", "zip"] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("tests/data/zip-hostile/{stem}.{extension}")),
                folder.path().join(format!("x.{extension}")),
            )
            .unwrap();
        }
        let last = folder.path().join("x.zip");
        assert_eq!(
            choose_reader(Engine::Auto, &last).unwrap(),
            Reader::Zip { fast: false },
            "{stem}"
        );
        let error = assert_refused(&last, &["x.z01"], stem);
        assert!(!error.contains(other), "{stem}: {error}");
        for action in ["list", "test"] {
            let output = zipper_cli(&[action.as_ref(), last.as_os_str()]);
            let printed = String::from_utf8_lossy(&output.stdout);
            assert!(!output.status.success(), "{stem} {action}: {printed}");
            assert!(!printed.contains(other), "{stem} {action}: {printed}");
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(error.contains("x.z01"), "{stem} {action}: {error}");
        }
    }
}

#[test]
fn other_readers_open_a_zip_with_volumes_alone_and_the_set_goes_to_the_zip_reader() {
    let folder = tempfile::tempdir().unwrap();
    for extension in ["z01", "zip"] {
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/data/zip-hostile/poly-7z.{extension}")),
            folder.path().join(format!("x.{extension}")),
        )
        .unwrap();
    }
    let last = folder.path().join("x.zip");
    let out = folder.path().join("out");
    let error = zipper::native_7z::read_archive(&last, Some(&out), Default::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a 7z archive"), "{error}");
    assert!(!out.exists());
    let folder = tempfile::tempdir().unwrap();
    let volumes = copy_set(SET, folder.path());
    let last = volumes.last().unwrap();
    let mut bytes = fs::read(last).unwrap();
    bytes[..6].copy_from_slice(b"7z\xBC\xAF\x27\x1C");
    fs::write(last, bytes).unwrap();
    assert_eq!(
        choose_reader(Engine::Auto, last).unwrap(),
        Reader::Zip { fast: false }
    );
}

#[test]
fn a_disk_number_that_defers_to_zip64_is_named_as_such() {
    let (_folder, last) = patched_set(|bytes, found, _| {
        let entry = found.central[3];
        bytes[entry + 34..entry + 36].copy_from_slice(&u16::MAX.to_le_bytes());
    });
    assert_refused(&last, &["0xFFFF"], "entry disk 0xFFFF");
}

#[test]
fn zip64_records_without_a_sentinel_are_judged_by_the_stored_directory_offset() {
    if !info_zip() {
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    many_files_tree(folder.path());
    let last = info_zip_split(folder.path(), "wide", &["-fz", "-s", "64k"]);
    let mut bytes = fs::read(&last).unwrap();
    let end = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x05\x06")
        .unwrap();
    let record = bytes
        .windows(4)
        .rposition(|window| window == b"PK\x06\x06")
        .unwrap();
    assert_eq!(le32(&bytes, end + 16), u32::MAX as usize);
    let stored = u64::from_le_bytes(bytes[record + 48..record + 56].try_into().unwrap());
    bytes[end + 16..end + 20].copy_from_slice(&(stored as u32).to_le_bytes());
    fs::write(&last, &bytes).unwrap();
    assert_refused(
        &last,
        &["ZIP64 locator its end record does not use"],
        "no sentinel",
    );
}
