//! Every NTFS volume in `tests/data/ntfs-corpus` extracts to the tree
//! recorded when the corpus was written, and to the tree 7-Zip extracts
//! where it is installed, apart from the differences the NTFS reader
//! reference lists. Volumes from the test writer cover every data form,
//! each byte for byte against its source and against 7-Zip.
#[path = "common/mod.rs"]
mod common;
#[path = "common/ntfs_writer.rs"]
mod ntfs_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use ntfs_writer::{
    Form, Geometry, Node, dir, file, file_as, hard_link_in_group, link, symlink_reparse,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_ntfs::{ReadOptions, read_archive, read_volume};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/ntfs-corpus");

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit.max(1)
    }

    fn bytes(&mut self, length: usize, text: bool) -> Vec<u8> {
        (0..length)
            .map(|index| {
                if text && index % 13 != 0 {
                    b"Zipper reads NTFS. "[index % 19]
                } else {
                    self.next() as u8
                }
            })
            .collect()
    }
}

fn sevenzip() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn file_kind(metadata: &fs::Metadata, bytes: &[u8]) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        format!(
            "{} {:o} {}",
            sha256(bytes),
            metadata.permissions().mode() & 0o777,
            metadata.mtime()
        )
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        sha256(bytes)
    }
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    if !root.exists() {
        return BTreeMap::new();
    }
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            let kind = if entry.file_type().is_symlink() {
                let target = fs::read_link(entry.path()).unwrap();
                let target = match target.strip_prefix(root) {
                    Ok(inside) => {
                        let up = "../".repeat(name.matches('/').count());
                        format!("{up}{}", inside.display())
                    }
                    Err(_) => target.display().to_string(),
                };
                format!("link:{target}")
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                file_kind(&metadata, &fs::read(entry.path()).unwrap())
            };
            (name, kind)
        })
        .collect()
}

// EXTERNAL(7-Zip NtfsHandler): 7-Zip writes each named stream of a system file as
// `[SYSTEM]/name:stream`, which Zipper leaves out.
fn without_system_streams(mut tree: BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.retain(|name, _| !(name.starts_with("[SYSTEM]/") && name.contains(':')));
    tree
}

fn sevenzip_tree(binary: &Path, image: &Path, into: &Path) -> (bool, BTreeMap<String, String>) {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(image)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (run.status.success(), without_system_streams(tree(into)))
}

fn without_modes_and_times(tree: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.iter()
        .map(|(name, kind)| {
            let kind = kind.split_once(' ').map_or(kind.as_str(), |(hash, _)| hash);
            (name.clone(), kind.to_owned())
        })
        .collect()
}

fn zipper_tree(image: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(image, Some(into), ReadOptions::default())?;
    Ok(tree(into))
}

fn unpacked(stored: &Path, into: &Path) -> PathBuf {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(fs::File::open(stored).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    let path = into.join(stored.file_stem().unwrap());
    fs::write(&path, bytes).unwrap();
    path
}

fn corpus_volumes() -> Vec<PathBuf> {
    let mut volumes: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "xz"))
        .collect();
    volumes.sort();
    volumes
}

fn documented_difference(
    name: &str,
    ours: &BTreeMap<String, String>,
    theirs: &BTreeMap<String, String>,
    sevenzip_succeeded: bool,
) -> bool {
    match name {
        "ntfs3g-4096-sectors.ntfs.xz" => !sevenzip_succeeded && theirs.is_empty(),
        "ntfs3g-sparse.ntfs.xz" => {
            ours.keys().eq(theirs.keys())
                && ours["holes.bin"] != theirs["holes.bin"]
                && ours["tail.bin"] != theirs["tail.bin"]
        }
        "ntfs3g-links.ntfs.xz" => {
            let mut expected = ours.clone();
            expected.insert(
                "folder/up.txt".to_owned(),
                theirs.get("folder/up.txt").cloned().unwrap_or_default(),
            );
            ours["folder/up.txt"] == "link:../hello.txt"
                && theirs["folder/up.txt"].starts_with(&sha256(b""))
                && &expected == theirs
        }
        _ => false,
    }
}

#[test]
fn every_corpus_volume_extracts_the_tree_7zip_extracts() {
    let recorded: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let binary = sevenzip();
    let mut written = BTreeMap::new();
    for stored in corpus_volumes() {
        let name = stored.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let image = unpacked(&stored, work.path());
        let ours = match zipper_tree(&image, &work.path().join("zipper")) {
            Ok(tree) => tree,
            Err(error) => BTreeMap::from([("refused".to_owned(), format!("{error:#}"))]),
        };
        if let Some(binary) = &binary
            && !ours.contains_key("refused")
        {
            let (succeeded, theirs) = sevenzip_tree(binary, &image, &work.path().join("7z"));
            assert!(
                (succeeded && ours == theirs)
                    || documented_difference(&name, &ours, &theirs, succeeded),
                "{name} differs from 7-Zip"
            );
        }
        if std::env::var_os("ZIPPER_NTFS_WRITE_TREES").is_some() {
            written.insert(name.clone(), without_modes_and_times(&ours));
        } else {
            assert_eq!(
                Some(&without_modes_and_times(&ours)),
                recorded.get(&name),
                "{name} differs from the recorded tree"
            );
        }
        common::make_tree_deletable(work.path());
    }
    if std::env::var_os("ZIPPER_NTFS_WRITE_TREES").is_some() {
        fs::write(
            Path::new(CORPUS).join("trees.json"),
            serde_json::to_string_pretty(&written).unwrap() + "\n",
        )
        .unwrap();
    }
}

#[test]
fn a_volume_with_a_named_stream_is_refused_where_7zip_writes_name_colon_stream() {
    let work = tempfile::tempdir().unwrap();
    let image = unpacked(
        &Path::new(CORPUS).join("ntfs3g-streams.ntfs.xz"),
        work.path(),
    );
    let error = read_archive(
        &image,
        Some(&work.path().join("out")),
        ReadOptions::default(),
    )
    .expect_err("a named stream outside [SYSTEM] was accepted");
    assert!(format!("{error:#}").contains("alternate data stream"));
    assert!(!work.path().join("out").exists());
    if let Some(binary) = sevenzip() {
        let (succeeded, theirs) = sevenzip_tree(&binary, &image, &work.path().join("7z"));
        assert!(succeeded);
        assert!(theirs.contains_key("hello.txt:Zone.Identifier"));
    }
}

#[cfg(unix)]
#[test]
fn hard_links_stay_linked_as_7zip_links_them() {
    use std::os::unix::fs::MetadataExt;
    let work = tempfile::tempdir().unwrap();
    let image = unpacked(&Path::new(CORPUS).join("ntfs3g-basic.ntfs.xz"), work.path());
    let into = work.path().join("out");
    read_archive(&image, Some(&into), ReadOptions::default()).unwrap();
    let first = fs::metadata(into.join("hello.txt")).unwrap();
    for other in ["folder/hard.txt", "folder/sub/second.txt"] {
        assert_eq!(fs::metadata(into.join(other)).unwrap().ino(), first.ino());
    }
    assert_eq!(first.nlink(), 3);
    if let Some(binary) = sevenzip() {
        let theirs = work.path().join("7z");
        sevenzip_tree(&binary, &image, &theirs);
        assert_eq!(fs::metadata(theirs.join("hello.txt")).unwrap().nlink(), 3);
    }
    common::make_tree_deletable(work.path());
}

#[test]
fn one_member_of_a_hard_link_extracts_alone_by_path_or_strip() {
    let work = tempfile::tempdir().unwrap();
    let image = unpacked(&Path::new(CORPUS).join("ntfs3g-basic.ntfs.xz"), work.path());
    let only = [String::from("folder/sub/second.txt")];
    let into = work.path().join("path");
    read_archive(
        &image,
        Some(&into),
        ReadOptions {
            paths: &only,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(into.join("folder/sub/second.txt")).unwrap().len(),
        13
    );
    assert!(!into.join("hello.txt").exists());
    let stripped = work.path().join("strip");
    read_archive(
        &image,
        Some(&stripped),
        ReadOptions {
            strip: zipper::Strip::Prefix("folder"),
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(stripped.join("hard.txt")).unwrap().len(),
        13,
        "the strip writes the link's data"
    );
    common::make_tree_deletable(work.path());
}

struct PartitionWindow {
    file: fs::File,
    start: u64,
    length: u64,
    position: u64,
}

impl Read for PartitionWindow {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = self.length.saturating_sub(self.position);
        let wanted = (buffer.len() as u64).min(left) as usize;
        self.file
            .seek(SeekFrom::Start(self.start + self.position))?;
        let count = self.file.read(&mut buffer[..wanted])?;
        self.position += count as u64;
        Ok(count)
    }
}

impl Seek for PartitionWindow {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.position = match to {
            SeekFrom::Start(at) => at,
            SeekFrom::End(back) => self.length.checked_add_signed(back).unwrap(),
            SeekFrom::Current(by) => self.position.checked_add_signed(by).unwrap(),
        };
        Ok(self.position)
    }
}

#[test]
fn a_volume_inside_a_larger_file_reads_through_a_bounded_view() {
    let work = tempfile::tempdir().unwrap();
    let image = unpacked(&Path::new(CORPUS).join("ntfs3g-basic.ntfs.xz"), work.path());
    let volume = fs::read(&image).unwrap();
    let disk = work.path().join("disk.img");
    let mut bytes = vec![0xA5u8; 1 << 20];
    bytes.extend_from_slice(&volume);
    bytes.extend(vec![0x5Au8; 4096]);
    fs::write(&disk, &bytes).unwrap();
    let view = PartitionWindow {
        file: fs::File::open(&disk).unwrap(),
        start: 1 << 20,
        length: volume.len() as u64,
        position: 0,
    };
    let into = work.path().join("view");
    read_volume(view, &disk, Some(&into), ReadOptions::default()).unwrap();
    let direct = work.path().join("direct");
    read_archive(&image, Some(&direct), ReadOptions::default()).unwrap();
    assert_eq!(tree(&into), tree(&direct));
    common::make_tree_deletable(work.path());
}

fn random_tree(random: &mut Random, depth: usize, group: &mut u32) -> Vec<Node> {
    let mut nodes = Vec::new();
    for index in 0..2 + random.below(5) {
        let name = format!("n{depth}-{index}");
        match random.below(10) {
            0 | 1 if depth < 3 => nodes.push(dir(&name, random_tree(random, depth + 1, group))),
            2 => {
                *group += 1;
                let length = random.below(9000) as usize;
                let data = random.bytes(length, true);
                nodes.push(hard_link_in_group(&format!("{name}.a"), &data, *group));
                nodes.push(hard_link_in_group(&format!("{name}.b"), &data, *group));
            }
            choice => {
                let text = random.below(2) == 0;
                let length = match random.below(4) {
                    0 => random.below(300),
                    1 => random.below(70_000),
                    2 => 65_536 * (1 + random.below(3)),
                    _ => random.below(200_000),
                } as usize;
                let mut data = random.bytes(length, text);
                if random.below(3) == 0 && length > 70_000 {
                    data[10_000..70_000].fill(0);
                }
                let form = match choice % 4 {
                    0 => Form::ResidentOrOneRun,
                    1 => Form::Fragments(1 + random.below(40) as usize),
                    2 => Form::Lznt1,
                    _ => Form::InExtensionRecord,
                };
                nodes.push(file_as(&format!("{name}.bin"), &data, form));
            }
        }
    }
    nodes
}

fn expected_tree(nodes: &[Node], prefix: &str, into: &mut BTreeMap<String, Vec<u8>>) {
    for node in nodes {
        match node {
            Node::Dir { name, children, .. } => {
                into.insert(format!("{prefix}{name}"), b"dir".to_vec());
                expected_tree(children, &format!("{prefix}{name}/"), into);
            }
            Node::File { name, data, .. } => {
                into.insert(format!("{prefix}{name}"), data.clone());
            }
            Node::Link { name, .. } => {
                into.insert(format!("{prefix}{name}"), b"link".to_vec());
            }
        }
    }
}

fn contents(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| !entry.path().starts_with(root.join("[SYSTEM]")))
        .map(|entry| {
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let kind = if entry.file_type().is_symlink() {
                b"link".to_vec()
            } else if entry.file_type().is_dir() {
                b"dir".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, kind)
        })
        .collect()
}

#[test]
fn generated_volumes_extract_byte_for_byte_as_7zip_extracts_them() {
    let binary = sevenzip();
    let mut random = Random(0x4E54_4653);
    for round in 0..12 {
        let mut group = 0;
        let mut nodes = random_tree(&mut random, 0, &mut group);
        nodes.push(link("relative.lnk", symlink_reparse("n0-0", true), false));
        let geometry = Geometry {
            cluster: [512, 1024, 4096][round % 3],
            records: 256,
            clusters: (24 << 20) / [512, 1024, 4096][round % 3] as u64,
            ..Geometry::default()
        };
        let volume = ntfs_writer::build(geometry, &nodes);
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("generated.ntfs");
        fs::write(&image, &volume).unwrap();
        let into = work.path().join("zipper");
        let ours =
            zipper_tree(&image, &into).unwrap_or_else(|error| panic!("round {round}: {error:#}"));
        let mut expected = BTreeMap::new();
        expected_tree(&nodes, "", &mut expected);
        assert_eq!(
            contents(&into),
            expected,
            "round {round} differs from its source"
        );
        if let Some(binary) = &binary {
            let (succeeded, theirs) = sevenzip_tree(binary, &image, &work.path().join("7z"));
            assert!(succeeded, "round {round}: 7-Zip failed");
            assert_eq!(ours, theirs, "round {round} differs from 7-Zip");
        }
        common::make_tree_deletable(work.path());
    }
}

#[test]
fn sparse_files_read_their_holes_as_zeros_where_7zip_misreads_them() {
    let binary = sevenzip();
    let mut data = vec![0u8; 300_000];
    data[..5000].fill(7);
    data[200_000..209_000].fill(9);
    for unit in [0u8, 4] {
        let nodes = [
            file_as("holes.bin", &data, Form::SparseWithUnitLog(unit)),
            file("plain.txt", b"plain\n"),
        ];
        let volume = ntfs_writer::build(Geometry::default(), &nodes);
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("sparse.ntfs");
        fs::write(&image, &volume).unwrap();
        let into = work.path().join("zipper");
        zipper_tree(&image, &into).unwrap();
        assert_eq!(
            fs::read(into.join("holes.bin")).unwrap(),
            data,
            "unit {unit}"
        );
        if let Some(binary) = &binary {
            let theirs = work.path().join("7z");
            let (succeeded, _) = sevenzip_tree(binary, &image, &theirs);
            assert!(
                !succeeded || fs::read(theirs.join("holes.bin")).unwrap() != data,
                "7-Zip now reads sparse files with unit {unit}; update the reference"
            );
        }
        common::make_tree_deletable(work.path());
    }
}

#[test]
fn listing_names_every_entry_it_writes() {
    let work = tempfile::tempdir().unwrap();
    let image = unpacked(&Path::new(CORPUS).join("ntfs3g-basic.ntfs.xz"), work.path());
    let listed = read_archive(&image, None, ReadOptions::default()).unwrap();
    let written = zipper_tree(&image, &work.path().join("out")).unwrap();
    assert_eq!(listed.len(), written.len());
    common::make_tree_deletable(work.path());
}
