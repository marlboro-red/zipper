//! Every WIM in `tests/data/wim-corpus` extracts to the tree 7-Zip extracted
//! when the corpus was written, and to the tree 7-Zip extracts now where it
//! is installed. Where `wimlib-imagex` is installed too, seeded trees are
//! captured with XPRESS, LZX and LZMS at many chunk sizes, solid and not,
//! and each image must extract byte for byte to its source tree and to the
//! tree 7-Zip extracts. WIMs from the test writer cover what wimlib cannot
//! write on macOS and Linux.
#[path = "common/mod.rs"]
mod common;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/wim_writer.rs"]
mod wim_writer;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use wim_writer::{Compression, Node, Options, dir, file, symlink};
use zipper::native_wim::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/wim-corpus");

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tree(root: &Path) -> BTreeMap<String, String> {
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
            let kind = if entry.file_type().is_symlink() {
                format!("link:{}", fs::read_link(entry.path()).unwrap().display())
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                sha256(&fs::read(entry.path()).unwrap())
            };
            (name, kind)
        })
        .collect()
}

// EXTERNAL(7-Zip WimIn): beside each link 7-Zip writes an empty `name:_` file for the
// second unnamed stream that wimlib stores, which Zipper does not write.
fn without_link_streams(mut tree: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let links: Vec<String> = tree
        .iter()
        .filter(|(_, kind)| kind.starts_with("link:"))
        .map(|(name, _)| format!("{name}:_"))
        .collect();
    for name in links {
        tree.remove(&name);
    }
    tree
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    run.status
        .success()
        .then(|| without_link_streams(tree(into)))
}

fn sevenzip() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn zipper_tree(archive: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(archive, Some(into), ReadOptions::default())?;
    Ok(tree(into))
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "wim" || extension == "esd")
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_corpus_wim_extracts_the_tree_7zip_extracts() {
    let recorded: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let files = corpus_files();
    assert_eq!(
        files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        recorded.keys().cloned().collect::<Vec<_>>(),
        "trees.json names exactly the corpus files"
    );
    let binary = sevenzip();
    for archive in &files {
        let name = archive.file_name().unwrap().to_string_lossy().into_owned();
        let root = tempfile::tempdir().unwrap();
        let ours = zipper_tree(archive, &root.path().join("zipper")).unwrap();
        assert_eq!(
            ours, recorded[&name],
            "{name} differs from the recorded tree"
        );
        let listed = read_archive(archive, None, ReadOptions::default()).unwrap();
        assert_eq!(
            listed.len(),
            ours.len(),
            "{name} lists as many entries as it writes"
        );
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, archive, &root.path().join("7zip"))
                .unwrap_or_else(|| panic!("7-Zip refuses {name}"));
            assert_eq!(ours, theirs, "{name} differs from 7-Zip");
        }
        common::make_tree_deletable(root.path());
    }
}

#[cfg(unix)]
#[test]
fn hard_links_stay_linked_as_7zip_links_them() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let into = root.path().join("out");
    read_archive(
        &Path::new(CORPUS).join("links.wim"),
        Some(&into),
        ReadOptions::default(),
    )
    .unwrap();
    let first = fs::metadata(into.join("hello.txt")).unwrap();
    let second = fs::metadata(into.join("folder/hard.txt")).unwrap();
    assert_eq!(first.ino(), second.ino());
    assert_eq!(first.nlink(), 2);
    let copy = fs::metadata(into.join("folder/deeper/copy.bin")).unwrap();
    let data = fs::metadata(into.join("folder/data.bin")).unwrap();
    assert_ne!(
        copy.ino(),
        data.ino(),
        "files that share data are not links"
    );
}

#[test]
fn one_member_of_a_hard_link_extracts_alone_by_path_or_strip() {
    let archive = Path::new(CORPUS).join("links.wim");
    let root = tempfile::tempdir().unwrap();
    let only = [String::from("folder/hard.txt")];
    let into = root.path().join("path");
    read_archive(
        &archive,
        Some(&into),
        ReadOptions {
            paths: &only,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(into.join("folder/hard.txt")).unwrap(),
        b"hello from a WIM\n"
    );
    let into = root.path().join("strip");
    read_archive(
        &archive,
        Some(&into),
        ReadOptions {
            strip: zipper::Strip::Prefix("folder"),
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(into.join("hard.txt")).unwrap(),
        b"hello from a WIM\n"
    );
    assert!(!into.join("hello.txt").exists());
}

fn wimlib() -> Option<PathBuf> {
    Command::new("wimlib-imagex")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
        .then(|| PathBuf::from("wimlib-imagex"))
}

fn sample(random: &mut Random, length: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(length);
    while data.len() < length {
        match random.below(6) {
            0 => data.extend((0..random.below(64)).map(|_| random.next() as u8)),
            1 if !data.is_empty() => {
                let back = 1 + random.below(data.len().min(1 << 21));
                for _ in 0..3 + random.below(400) {
                    data.push(data[data.len() - back]);
                }
            }
            2 => data.extend([0xE8, random.next() as u8, random.below(3) as u8, 0, 0]),
            3 => data.extend([0x48, 0x8D, 0x05, random.next() as u8, 0, 0, 0]),
            4 => data.extend(std::iter::repeat_n(random.below(3) as u8, random.below(90))),
            _ => data.extend_from_slice(b"Windows Imaging Format "),
        }
    }
    data.truncate(length);
    data
}

fn write_source(random: &mut Random, root: &Path) {
    fs::create_dir_all(root.join("a/b")).unwrap();
    for index in 0..4 {
        let length = match index {
            0 => random.below(70_000),
            1 => 1 + random.below(300),
            _ => random.below(1_500_000),
        };
        let folder = ["", "a", "a/b", "a"][index];
        fs::write(
            root.join(folder).join(format!("f{index}")),
            sample(random, length),
        )
        .unwrap();
    }
    let repeated = fs::read(root.join("f0")).unwrap();
    fs::write(root.join("a/b/same"), repeated).unwrap();
}

#[test]
fn generated_wims_of_every_method_and_chunk_size_extract_byte_for_byte_as_7zip_extracts_them() {
    let (Some(wimlib), Some(binary)) = (wimlib(), sevenzip()) else {
        eprintln!("wimlib-imagex or 7-Zip 26 is not installed; skipping the generated WIMs");
        return;
    };
    let shapes: &[&[&str]] = &[
        &["--compress=none"],
        &["--compress=xpress", "--chunk-size=4096"],
        &["--compress=xpress", "--chunk-size=65536"],
        &["--compress=lzx", "--chunk-size=32768"],
        &["--compress=lzx", "--chunk-size=65536"],
        &["--compress=lzx", "--chunk-size=2097152"],
        &["--compress=lzms", "--chunk-size=32768"],
        &["--compress=lzms", "--chunk-size=1048576"],
        &["--solid"],
        &["--solid", "--solid-chunk-size=32768"],
        &["--solid", "--solid-compress=xpress"],
        &["--solid", "--solid-compress=lzx"],
    ];
    let mut random = Random(0x5eed_0f00_0000_0001);
    for round in 0..3 {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        write_source(&mut random, &source);
        for (index, flags) in shapes.iter().enumerate() {
            let archive = root.path().join(format!("{round}-{index}.wim"));
            let run = Command::new(&wimlib)
                .arg("capture")
                .arg(&source)
                .arg(&archive)
                .arg("image")
                .args(*flags)
                .output()
                .unwrap();
            assert!(run.status.success(), "{flags:?}: {run:?}");
            let ours = zipper_tree(&archive, &root.path().join(format!("z{index}"))).unwrap();
            assert_eq!(
                ours,
                tree(&source),
                "round {round} {flags:?} differs from its source"
            );
            let theirs = sevenzip_tree(&binary, &archive, &root.path().join(format!("s{index}")))
                .unwrap_or_else(|| panic!("7-Zip refuses round {round} {flags:?}"));
            assert_eq!(ours, theirs, "round {round} {flags:?} differs from 7-Zip");
        }
    }
}

fn writer_images() -> Vec<Vec<Node>> {
    let mut read_only = file("read only.txt", b"cannot write me\n");
    if let Node::File { attributes, .. } = &mut read_only {
        *attributes = 0x81;
    }
    let mut linked = file("linked", b"one inode\n");
    if let Node::File { link_group, .. } = &mut linked {
        *link_group = 7;
    }
    let mut also = file("also linked", b"one inode\n");
    if let Node::File { link_group, .. } = &mut also {
        *link_group = 7;
    }
    vec![
        vec![
            dir(
                "docs",
                vec![
                    file("readme", &b"repeat ".repeat(9000)),
                    read_only,
                    dir("empty", Vec::new()),
                ],
            ),
            file("a/b", b"a name with a slash\n"),
            linked,
            dir("sub", vec![also]),
            symlink("into", "docs\\readme", true),
            symlink("rooted", "\\docs\\readme", false),
            file("zero", b""),
        ],
        vec![file("second image", &[7u8; 70_000])],
    ]
}

#[test]
fn writer_wims_extract_as_7zip_extracts_them_in_every_layout() {
    let binary = sevenzip();
    for (compression, solid) in [
        (Compression::Stored, false),
        (Compression::Xpress, false),
        (Compression::Stored, true),
    ] {
        for images in [writer_images()[..1].to_vec(), writer_images()] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("made.wim");
            let options = Options {
                compression,
                solid,
                chunk_size: 4096,
                ..Options::default()
            };
            fs::write(&archive, wim_writer::write(&images, &options)).unwrap();
            let ours = zipper_tree(&archive, &root.path().join("zipper"))
                .unwrap_or_else(|error| panic!("{compression:?} solid {solid}: {error:#}"));
            let prefix = if images.len() > 1 { "1/" } else { "" };
            assert_eq!(
                ours[&format!("{prefix}a_b")],
                sha256(b"a name with a slash\n")
            );
            assert_eq!(ours[&format!("{prefix}into")], "link:docs/readme");
            assert_eq!(ours[&format!("{prefix}rooted")], "link:docs/readme");
            assert_eq!(images.len() > 1, ours.contains_key("[1].xml"));
            let Some(binary) = &binary else { continue };
            let mut theirs = sevenzip_tree(binary, &archive, &root.path().join("7zip"))
                .unwrap_or_else(|| panic!("7-Zip refuses the {compression:?} WIM"));
            let rooted = format!("{prefix}rooted");
            let resolved = theirs[&rooted].clone();
            assert!(
                resolved.ends_with("7zip/docs/readme"),
                "7-Zip points the rooted link at {resolved}, from the destination and not the image"
            );
            theirs.insert(rooted, "link:docs/readme".to_owned());
            assert_eq!(
                ours, theirs,
                "{compression:?} solid {solid} differs from 7-Zip"
            );
            common::make_tree_deletable(root.path());
        }
    }
}

#[cfg(unix)]
#[test]
fn the_read_only_attribute_becomes_mode_0o444() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("made.wim");
    fs::write(
        &archive,
        wim_writer::write(&writer_images()[..1], &Options::default()),
    )
    .unwrap();
    let into = root.path().join("out");
    read_archive(&archive, Some(&into), ReadOptions::default()).unwrap();
    let mode = |name: &str| fs::metadata(into.join(name)).unwrap().permissions().mode() & 0o222;
    assert_eq!(mode("docs/read only.txt"), 0);
    assert_ne!(mode("docs/readme"), 0);
    common::make_tree_deletable(root.path());
}

#[cfg(unix)]
fn write_corpus_tree(root: &Path, seed: u64, links: bool) {
    let mut random = Random(seed);
    fs::create_dir_all(root.join("folder/deeper")).unwrap();
    fs::create_dir_all(root.join("empty folder")).unwrap();
    fs::write(root.join("hello.txt"), b"hello from a WIM\n").unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    let data = sample(&mut random, 90_000);
    fs::write(root.join("folder/data.bin"), &data).unwrap();
    fs::write(root.join("folder/deeper/copy.bin"), &data).unwrap();
    fs::write(
        root.join("folder/deeper/small.bin"),
        sample(&mut random, 3000),
    )
    .unwrap();
    fs::write(root.join("n\u{e4}me \u{fc}nicode.txt"), b"unicode name\n").unwrap();
    if links {
        fs::hard_link(root.join("hello.txt"), root.join("folder/hard.txt")).unwrap();
        std::os::unix::fs::symlink("hello.txt", root.join("near")).unwrap();
        std::os::unix::fs::symlink("folder/deeper/small.bin", root.join("down")).unwrap();
        std::os::unix::fs::symlink("folder", root.join("folder link")).unwrap();
    }
}

#[cfg(unix)]
#[test]
#[ignore = "rewrites tests/data/wim-corpus with wimlib-imagex; run with --ignored to regenerate"]
fn write_the_corpus() {
    let wimlib = wimlib().expect("wimlib-imagex writes the corpus");
    let binary = sevenzip().expect("7-Zip records the corpus trees");
    let root = tempfile::tempdir().unwrap();
    let [plain, linked, second] = ["plain", "linked", "second"].map(|name| root.path().join(name));
    write_corpus_tree(&plain, 1, false);
    write_corpus_tree(&linked, 2, true);
    write_corpus_tree(&second, 3, false);
    let shapes: &[(&str, &Path, &[&str])] = &[
        ("stored.wim", &plain, &["--compress=none"]),
        ("xpress.wim", &plain, &["--compress=xpress"]),
        (
            "xpress-4k.wim",
            &plain,
            &["--compress=xpress", "--chunk-size=4096"],
        ),
        ("lzx.wim", &plain, &["--compress=lzx"]),
        (
            "lzx-128k.wim",
            &plain,
            &["--compress=lzx", "--chunk-size=131072"],
        ),
        ("lzms.wim", &plain, &["--compress=lzms"]),
        ("solid.esd", &plain, &["--solid"]),
        (
            "solid-xpress.wim",
            &plain,
            &["--solid", "--solid-compress=xpress"],
        ),
        (
            "solid-lzx.wim",
            &plain,
            &["--solid", "--solid-compress=lzx"],
        ),
        ("links.wim", &linked, &["--compress=lzx"]),
        ("two-images.wim", &plain, &["--compress=xpress"]),
        ("checked.wim", &plain, &["--compress=lzx", "--check"]),
    ];
    let run = |arguments: &[&std::ffi::OsStr]| {
        let output = Command::new(&wimlib).args(arguments).output().unwrap();
        assert!(output.status.success(), "{arguments:?}: {output:?}");
    };
    let mut recorded = BTreeMap::new();
    for (name, source, flags) in shapes {
        let archive = Path::new(CORPUS).join(name);
        let _ = fs::remove_file(&archive);
        let mut arguments = vec!["capture".as_ref(), source.as_os_str(), archive.as_os_str()];
        arguments.push("image".as_ref());
        arguments.extend(flags.iter().map(std::ffi::OsStr::new));
        run(&arguments);
        if *name == "two-images.wim" {
            run(&[
                "append".as_ref(),
                second.as_os_str(),
                archive.as_os_str(),
                "second".as_ref(),
            ]);
        }
        let theirs = sevenzip_tree(&binary, &archive, &root.path().join(format!("x-{name}")))
            .unwrap_or_else(|| panic!("7-Zip refused {name}"));
        recorded.insert(name.to_string(), theirs);
    }
    fs::write(
        Path::new(CORPUS).join("trees.json"),
        serde_json::to_string_pretty(&recorded).unwrap() + "\n",
    )
    .unwrap();
}
