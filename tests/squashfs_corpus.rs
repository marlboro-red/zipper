//! Every SquashFS and CramFS image in `tests/data/squashfs-corpus` extracts
//! to the tree recorded when the corpus was written, and to the tree
//! `7zz x` extracts where it is installed, with modes and, for SquashFS,
//! modification times. Images that `mksquashfs` and `mkfs.cramfs` write
//! here of every compressor at three block sizes and nine table and
//! compressor options, raw and in a lone GPT partition, extract as 7-Zip
//! extracts them and byte for byte as the files they were written from.
//! 7-Zip 26.05 does not open LZ4 images, so those are checked against
//! `unsquashfs` and the source bytes. Without 7-Zip 26 or later the
//! differential checks are skipped, unless `ZIPPER_PARITY_REQUIRE_7Z` is
//! set. Without `mksquashfs`, `mkfs.cramfs` or `unsquashfs`, the checks that
//! need them are skipped.
#[path = "common/disk_writer.rs"]
mod disk_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/squashfs_trees.rs"]
mod squashfs_trees;

use squashfs_trees::{Tree, contents, noise, sevenzip_run, tree, words};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::{Engine, ExtractOptions, Reader};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/squashfs-corpus");
const COMPRESSORS: [&str; 6] = ["gzip", "lzma", "lzo", "xz", "lz4", "zstd"];
const SEVEN_ZIP_REFUSES_LZ4: &str = "Not implemented";

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

#[derive(Clone, Copy)]
struct Shape {
    times: bool,
    lz4: bool,
}

fn shape(image: &Path) -> Shape {
    let bytes = fs::read(image).unwrap();
    let squashfs = bytes.starts_with(b"hsqs");
    Shape {
        times: squashfs,
        lz4: squashfs && bytes[20..22] == [5, 0],
    }
}

fn zipper_tree(image: &Path, into: &Path, shape: Shape) -> Tree {
    zipper::extract_archive(image, into, ExtractOptions::default())
        .unwrap_or_else(|error| panic!("{}: {error:#}", image.display()));
    tree(into, shape.times)
}

fn with_differences(theirs: &Tree, ours: &Tree, differs: &[&str], label: &str) -> Tree {
    let mut expected = theirs.clone();
    for path in differs {
        assert!(
            ours.contains_key(*path),
            "{label}: Zipper did not write {path}"
        );
        assert_ne!(
            theirs.get(*path),
            ours.get(*path),
            "{label}: 7-Zip now writes {path} as Zipper does; drop it from the differences"
        );
        expected.insert((*path).to_owned(), ours[*path].clone());
    }
    expected
}

fn unsquashfs_tree(image: &Path, into: &Path) -> Option<Tree> {
    let tool = squashfs_trees::unsquashfs()?;
    Command::new(tool)
        .args(["-quiet", "-no-progress", "-no-xattrs", "-d"])
        .arg(into)
        .arg(image)
        .output()
        .unwrap();
    Some(contents(&tree(into, false)))
}

fn check_lz4(image: &Path, ours: &Tree, work: &Path, label: &str) {
    if let Some(binary) = sevenzip_binary() {
        let (accepted, _, output) = sevenzip_run(&binary, image, &work.join("7z"), true);
        assert!(
            !accepted && output.contains(SEVEN_ZIP_REFUSES_LZ4),
            "{label}: 7-Zip now opens LZ4 images; compare with it\n{output}"
        );
    }
    let Some(theirs) = unsquashfs_tree(image, &work.join("unsquashfs")) else {
        return;
    };
    let ours = contents(ours);
    for (name, kind) in &theirs {
        if kind != "special" {
            assert_eq!(
                ours.get(name),
                Some(kind),
                "{label}: {name} differs from unsquashfs"
            );
        }
    }
    let empty = squashfs_trees::sha256(b"");
    for (name, kind) in &ours {
        assert!(
            theirs.contains_key(name) || *kind == empty,
            "{label}: unsquashfs did not write {name}"
        );
    }
}

fn check(image: &Path, shape: Shape, differs: &[&str], label: &str) -> Tree {
    let work = tempfile::tempdir().unwrap();
    let ours = zipper_tree(image, &work.path().join("zipper"), shape);
    if shape.lz4 {
        check_lz4(image, &ours, work.path(), label);
        return ours;
    }
    if let Some(binary) = sevenzip_binary() {
        let (_, theirs, output) =
            sevenzip_run(&binary, image, &work.path().join("7z"), shape.times);
        let theirs = with_differences(&theirs, &ours, differs, label);
        assert_eq!(ours, theirs, "{label} differs from 7-Zip:\n{output}");
    }
    ours
}

fn corpus_images() -> Vec<PathBuf> {
    let mut images: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sqfs" || extension == "cramfs")
        })
        .collect();
    images.sort();
    images
}

fn corpus_differences(name: &str) -> Vec<&'static str> {
    if name == "cramfs-holes.cramfs" {
        eprintln!("{name}: zeros.bin differs from 7-Zip, which fails on a CramFS hole");
        vec!["zeros.bin"]
    } else {
        Vec::new()
    }
}

#[test]
fn every_corpus_image_extracts_the_recorded_tree_and_the_tree_7zip_extracts() {
    let recorded: BTreeMap<String, Tree> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let writing = std::env::var_os("ZIPPER_SQUASHFS_WRITE_TREES").is_some();
    let mut written = BTreeMap::new();
    let images = corpus_images();
    assert!(images.len() >= 15, "{images:?}");
    for image in images {
        let name = image.file_name().unwrap().to_string_lossy().into_owned();
        let ours = contents(&check(
            &image,
            shape(&image),
            &corpus_differences(&name),
            &name,
        ));
        if !writing {
            assert_eq!(
                Some(&ours),
                recorded.get(&name),
                "{name} differs from the recorded tree"
            );
        }
        written.insert(name, ours);
    }
    if writing {
        fs::write(
            Path::new(CORPUS).join("trees.json"),
            serde_json::to_string_pretty(&written).unwrap() + "\n",
        )
        .unwrap();
    }
}

#[test]
fn the_default_engine_reads_squashfs_4_and_cramfs_in_process() {
    for image in corpus_images() {
        let expected = if shape(&image).times {
            Reader::Squashfs
        } else {
            Reader::Cramfs
        };
        assert_eq!(
            zipper::choose_reader(Engine::Auto, &image).unwrap(),
            expected,
            "{}",
            image.display()
        );
    }
}

fn write_source(root: &Path) {
    fs::create_dir_all(root.join("folder/deeper")).unwrap();
    fs::create_dir(root.join("empty")).unwrap();
    fs::write(root.join("one-block.bin"), noise(4096, 1)).unwrap();
    fs::write(root.join("words.txt"), words(300_000, 2)).unwrap();
    fs::write(root.join("noise.bin"), noise(150_000, 3)).unwrap();
    fs::write(root.join("zeros.bin"), vec![0u8; 200_000]).unwrap();
    fs::write(
        root.join("half-zeros.bin"),
        [vec![0u8; 70_000], words(70_000, 4)].concat(),
    )
    .unwrap();
    fs::write(root.join("empty.txt"), b"").unwrap();
    fs::write(root.join("folder/tail.txt"), words(1_234, 5)).unwrap();
    fs::write(root.join("folder/again.txt"), words(300_000, 2)).unwrap();
    for index in 0..30 {
        fs::write(
            root.join(format!("folder/deeper/small-{index:02}.txt")),
            words(index * 97 + 1, index as u32),
        )
        .unwrap();
    }
    fs::hard_link(root.join("folder/tail.txt"), root.join("hard.txt")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::os::unix::fs::symlink("folder/tail.txt", root.join("link")).unwrap();
        std::os::unix::fs::symlink("../words.txt", root.join("folder/up")).unwrap();
        fs::set_permissions(root.join("words.txt"), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(root.join("noise.bin"), fs::Permissions::from_mode(0o751)).unwrap();
        fs::set_permissions(
            root.join("folder/deeper"),
            fs::Permissions::from_mode(0o750),
        )
        .unwrap();
    }
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
    for entry in walkdir::WalkDir::new(root).contents_first(true) {
        let entry = entry.unwrap();
        if !entry.file_type().is_symlink() {
            let file = fs::File::open(entry.path()).unwrap();
            file.set_modified(time + std::time::Duration::from_secs(entry.depth() as u64 * 1000))
                .unwrap();
        }
    }
}

fn mksquashfs(tool: &Path, source: &Path, image: &Path, options: &[&str]) {
    let run = Command::new(tool)
        .arg(source)
        .arg(image)
        .args(["-noappend", "-no-xattrs", "-quiet", "-no-progress"])
        .args(options)
        .output()
        .unwrap();
    assert!(run.status.success(), "mksquashfs {options:?}: {run:?}");
}

fn source_contents(source: &Path) -> Tree {
    contents(&tree(source, false))
}

fn gpt(image: &Path, into: &Path) -> PathBuf {
    let mut bytes = fs::read(image).unwrap();
    bytes.resize(bytes.len().next_multiple_of(512), 0);
    let disk = disk_writer::gpt(&[(disk_writer::LINUX_DATA, "root", 40, &bytes)], 8);
    let path = into.join("disk.img");
    fs::write(&path, disk).unwrap();
    path
}

const UP_LINK: &str = "folder/up";

fn check_generated(image: &Path, source: &Tree, label: &str) {
    eprintln!(
        "{label}: {UP_LINK} differs from 7-Zip, which writes a link with `..` as an empty file"
    );
    let shape = shape(image);
    let ours = check(image, shape, &[UP_LINK], label);
    assert_eq!(&contents(&ours), source, "{label} differs from its source");
    let work = tempfile::tempdir().unwrap();
    let disk = gpt(image, work.path());
    assert_eq!(
        zipper::choose_reader(Engine::Auto, &disk).unwrap(),
        Reader::Disk,
        "{label}"
    );
    let label = format!("{label} in a GPT partition");
    let partitioned = if shape.lz4 {
        zipper_tree(&disk, &work.path().join("zipper"), shape)
    } else {
        check(&disk, shape, &[UP_LINK], &label)
    };
    assert_eq!(partitioned, ours, "{label}");
}

#[test]
fn generated_images_of_every_compressor_and_block_size_extract_as_7zip_extracts_them() {
    let Some(tool) = squashfs_trees::mksquashfs() else {
        eprintln!("mksquashfs is not installed; skipping");
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    write_source(&source);
    let expected = source_contents(&source);
    for compressor in COMPRESSORS {
        for block in ["4096", "65536", "1048576"] {
            let image = work.path().join(format!("{compressor}-{block}.sqfs"));
            mksquashfs(&tool, &source, &image, &["-comp", compressor, "-b", block]);
            check_generated(
                &image,
                &expected,
                &format!("{compressor} with {block}-byte blocks"),
            );
        }
    }
    let variants: [&[&str]; 9] = [
        &["-no-compression"],
        &["-noI", "-noD"],
        &["-noF", "-no-fragments"],
        &["-always-use-fragments", "-b", "8192"],
        &["-no-duplicates", "-no-exports", "-no-sparse"],
        &["-comp", "xz", "-Xbcj", "x86,arm", "-Xdict-size", "50%"],
        &[
            "-comp",
            "gzip",
            "-Xcompression-level",
            "1",
            "-Xwindow-size",
            "8",
        ],
        &["-comp", "lzo", "-Xalgorithm", "lzo1x_1"],
        &["-comp", "zstd", "-Xcompression-level", "22"],
    ];
    for (index, options) in variants.iter().enumerate() {
        let image = work.path().join(format!("variant-{index}.sqfs"));
        mksquashfs(&tool, &source, &image, options);
        check_generated(&image, &expected, &format!("mksquashfs {options:?}"));
    }
}

#[test]
fn generated_cramfs_images_in_both_byte_orders_extract_as_7zip_extracts_them() {
    let Some(tool) = squashfs_trees::mkcramfs() else {
        eprintln!("mkfs.cramfs is not installed; skipping");
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    write_source(&source);
    let expected = source_contents(&source);
    for endian in ["little", "big"] {
        let image = work.path().join(format!("{endian}.cramfs"));
        let run = Command::new(&tool)
            .args(["-b", "4096", "-N", endian])
            .arg(&source)
            .arg(&image)
            .output()
            .unwrap();
        assert!(run.status.success(), "{run:?}");
        check_generated(&image, &expected, &format!("CramFS {endian}-endian"));
    }
}

#[test]
fn selection_strip_and_hard_links_follow_the_paths_7zip_writes() {
    let image = Path::new(CORPUS).join("gzip.sqfs");
    let work = tempfile::tempdir().unwrap();
    let selected = vec!["folder/hard-link.txt".to_owned()];
    let into = work.path().join("selected");
    zipper::extract_archive(
        &image,
        &into,
        ExtractOptions {
            paths: &selected,
            ..ExtractOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(into.join("folder/hard-link.txt")).unwrap(),
        b"hello, squashfs\n"
    );
    assert!(!into.join("hello.txt").exists());
    let stripped = work.path().join("stripped");
    zipper::extract_archive(
        &image,
        &stripped,
        ExtractOptions {
            strip: zipper::Strip::Prefix("folder"),
            include: &["folder/**".to_owned()],
            ..ExtractOptions::default()
        },
    )
    .unwrap();
    assert!(stripped.join("deeper/deepest/leaf.txt").is_file());
    assert_eq!(
        fs::read(stripped.join("hard-link.txt")).unwrap(),
        b"hello, squashfs\n"
    );
    let entries = zipper::native_squashfs::read_archive(
        &image,
        None,
        zipper::native_squashfs::ReadOptions::default(),
    )
    .unwrap();
    assert_eq!(entries.len(), 65);
}
