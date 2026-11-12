//! Every image in `tests/data/ext-corpus` extracts to the tree recorded when
//! the corpus was written, which is the tree `7zz x` extracts, and to the
//! tree 7-Zip extracts now where it is installed. 7-Zip opens no image with
//! an inline folder and refuses a block-mapped file whose pointer to an
//! indirect block is zero before a later block, so those images are checked
//! against the source tree `mke2fs -d` copied. Where
//! `mke2fs` is installed, seeded source trees are copied into ext2, ext3 and
//! ext4 volumes of every layout Zipper reads, and each must extract byte for
//! byte with its modes and times as the source and 7-Zip give them. Without
//! 7-Zip 26.05 or later the differential checks are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set.
#![cfg(unix)]
#[path = "common/ext_trees.rs"]
mod ext_trees;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use ext_trees::{Tree, from_json, from_source, masked, system_entry, tool, tree, umask_ceiling};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::{ExtractOptions, Reader};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/ext-corpus");

fn sevenzip_binary() -> Option<PathBuf> {
    sevenzip::sevenzip()
}

fn unpacked(name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(fs::File::open(Path::new(CORPUS).join(name)).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn zipper_tree(image: &Path, into: &Path) -> Tree {
    zipper::extract_archive(image, into, ExtractOptions::default())
        .unwrap_or_else(|error| panic!("{}: {error:#}", image.display()));
    tree(into)
}

struct Recorded {
    from_7zip: bool,
    tree: Tree,
}

fn recorded() -> Vec<(String, Recorded)> {
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let common = from_json(&record["common"]);
    record["images"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, image)| {
            let mut tree = common.clone();
            for missing in image["missing"].as_array().unwrap() {
                tree.remove(missing.as_str().unwrap());
            }
            tree.extend(from_json(&image["tree"]));
            let from_7zip = image["source"] == "7zz x";
            (name.clone(), Recorded { from_7zip, tree })
        })
        .collect()
}

fn assert_source_paths_match(ours: &Tree, expected: &Tree, label: &str) {
    for (name, node) in expected {
        assert_eq!(ours.get(name), Some(node), "{label}: {name}");
    }
    for name in ours.keys() {
        assert!(
            expected.contains_key(name) || system_entry(name),
            "{label}: Zipper wrote {name}, which the source lacks"
        );
    }
}

#[test]
fn every_corpus_image_extracts_the_recorded_tree_and_the_tree_7zip_extracts() {
    let binary = sevenzip_binary();
    let images = recorded();
    assert!(images.len() >= 12, "{}", images.len());
    for (name, recorded) in images {
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join(&name);
        fs::write(&image, unpacked(&format!("{name}.xz"))).unwrap();
        assert_eq!(
            zipper::choose_reader(&image).unwrap(),
            Reader::Ext,
            "{name}"
        );
        let ours = zipper_tree(&image, &work.path().join("zipper"));
        let expected = masked(&recorded.tree, umask_ceiling(work.path()));
        if recorded.from_7zip {
            assert_eq!(ours, expected, "{name} differs from its recorded tree");
        } else {
            assert_source_paths_match(&ours, &expected, &name);
        }
        let Some(binary) = &binary else {
            continue;
        };
        let (accepted, theirs, output) =
            ext_trees::sevenzip_extract(binary, &image, &work.path().join("7z"));
        if recorded.from_7zip {
            assert!(accepted, "{name}: 7-Zip failed:\n{output}");
            assert_eq!(ours, theirs, "{name} differs from 7-Zip:\n{output}");
        } else {
            assert!(
                !accepted,
                "{name}: 7-Zip now reads it; record its tree from 7-Zip"
            );
        }
    }
}

#[test]
#[cfg_attr(not(feature = "cli"), ignore = "runs the zipper binary")]
fn list_and_test_read_the_volume_in_process() {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("ext4.img");
    fs::write(&image, unpacked("ext4.img.xz")).unwrap();
    for (command, wanted) in [("list", "hello.txt"), ("test", "")] {
        let run = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .arg(command)
            .arg(&image)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{command}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(String::from_utf8_lossy(&run.stdout).contains(wanted));
    }
    let entries = zipper::native_ext::read_archive(&image, None, Default::default()).unwrap();
    assert!(entries.iter().any(|entry| entry.name == "[SYS]/Journal"));
    assert!(entries.iter().any(|entry| entry.name == "deep/d0/d1"));
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }

    fn bytes(&mut self, length: usize) -> Vec<u8> {
        (0..length).map(|_| self.next() as u8).collect()
    }
}

fn set_time(path: &Path, seconds: i64) {
    let time = if seconds >= 0 {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64)
    } else {
        std::time::UNIX_EPOCH - std::time::Duration::from_secs(seconds.unsigned_abs())
    };
    let file = fs::File::options()
        .write(!path.is_dir())
        .read(path.is_dir())
        .open(path)
        .unwrap();
    file.set_modified(time).unwrap();
}

fn seeded_source(root: &Path, seed: u64) {
    let mut random = Random(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut folders = vec![root.to_owned()];
    for index in 0..12 {
        let parent = folders[random.below(folders.len() as u64) as usize].clone();
        let folder = parent.join(format!("folder-{index}-{}", random.below(1000)));
        fs::create_dir(&folder).unwrap();
        folders.push(folder);
    }
    let mut files = Vec::new();
    for index in 0..60 {
        let folder = &folders[random.below(folders.len() as u64) as usize];
        let path = folder.join(format!("file-{index}-{}.bin", random.below(1 << 20)));
        let size = match random.below(6) {
            0 => random.below(70) as usize,
            1 => 59 + random.below(4) as usize,
            2 => random.below(5000) as usize,
            3 => random.below(70_000) as usize,
            _ => random.below(200) as usize,
        };
        let mut data = random.bytes(size);
        if random.below(4) == 0 && size > 9000 {
            let hole = random.below((size - 8192) as u64) as usize;
            data[hole..hole + 8192].fill(0);
        }
        fs::write(&path, &data).unwrap();
        files.push(path);
    }
    let sparse = root.join("sparse.bin");
    let mut file = fs::File::create(&sparse).unwrap();
    std::io::Seek::seek(
        &mut file,
        std::io::SeekFrom::Start(random.below(3 << 20) + (1 << 20)),
    )
    .unwrap();
    std::io::Write::write_all(&mut file, &random.bytes(3000)).unwrap();
    drop(file);
    files.push(sparse);
    for index in 0..6 {
        let target = files[random.below(files.len() as u64) as usize].clone();
        let folder = &folders[random.below(folders.len() as u64) as usize];
        fs::hard_link(&target, folder.join(format!("hard-{index}"))).unwrap();
    }
    for index in 0..6 {
        let folder = &folders[random.below(folders.len() as u64) as usize];
        let target = if index % 2 == 0 {
            "x".repeat(random.below(120) as usize + 1)
        } else {
            format!("t{index}")
        };
        std::os::unix::fs::symlink(target, folder.join(format!("link-{index}"))).unwrap();
    }
    let made = Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(root.join("pipe"))
        .status()
        .unwrap();
    assert!(made.success());
    for path in &files {
        set_time(path, random.below(2_100_000_000) as i64 - 50_000_000);
        let mode = [0o644, 0o755, 0o600, 0o444, 0o640, 0o4711][random.below(6) as usize];
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.components().count()));
    for folder in &folders[..folders.len() - 1] {
        set_time(folder, random.below(2_000_000_000) as i64);
        if random.below(4) == 0 {
            fs::set_permissions(folder, fs::Permissions::from_mode(0o750)).unwrap();
        }
    }
}

const LAYOUTS: [(&str, &[&str]); 11] = [
    ("ext2", &["-t", "ext2", "-b", "1024", "-I", "128"]),
    ("ext2-4k", &["-t", "ext2", "-b", "4096"]),
    ("ext3", &["-t", "ext3", "-b", "1024"]),
    ("ext4", &["-t", "ext4", "-b", "1024"]),
    ("ext4-4k", &["-t", "ext4", "-b", "4096"]),
    ("ext4-block-maps", &["-t", "ext4", "-O", "^extent,^64bit"]),
    ("ext4-32bit", &["-t", "ext4", "-O", "^64bit"]),
    ("ext4-inline", &["-t", "ext4", "-O", "inline_data"]),
    (
        "ext4-uninit-bg",
        &["-t", "ext4", "-O", "^metadata_csum,uninit_bg"],
    ),
    (
        "ext4-meta-bg",
        &["-t", "ext4", "-O", "meta_bg,^resize_inode"],
    ),
    (
        "ext4-128",
        &["-t", "ext4", "-I", "128", "-O", "^orphan_file"],
    ),
];

fn mke2fs(mke2fs: &Path, options: &[&str], source: &Path, image: &Path, size: &str) {
    let run = Command::new(mke2fs)
        .args(["-q", "-F"])
        .args(options)
        .arg("-d")
        .arg(source)
        .arg(image)
        .arg(size)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "mke2fs {options:?}: {}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn generated_volumes_extract_byte_for_byte_as_7zip_extracts_them() {
    let Some(mke2fs_binary) = tool("mke2fs") else {
        return;
    };
    let binary = sevenzip_binary();
    for seed in [1, 2] {
        let work = tempfile::tempdir().unwrap();
        let source = work.path().join("source");
        fs::create_dir(&source).unwrap();
        seeded_source(&source, seed);
        let expected = masked(&from_source(&source), umask_ceiling(work.path()));
        for (layout, options) in LAYOUTS {
            let label = format!("seed {seed} {layout}");
            let image = work.path().join(format!("{layout}.img"));
            mke2fs(&mke2fs_binary, options, &source, &image, "16M");
            let into = work.path().join(format!("{layout}.zipper"));
            let ours = zipper_tree(&image, &into);
            assert_source_paths_match(&ours, &expected, &label);
            if let Some(binary) = &binary {
                let (accepted, theirs, output) = ext_trees::sevenzip_extract(
                    binary,
                    &image,
                    &work.path().join(format!("{layout}.7z")),
                );
                if layout.contains("inline") && output.contains("Cannot open the file as archive") {
                    continue;
                }
                assert!(
                    accepted || !ext_trees::sevenzip_unsupported_method_paths(&output).is_empty(),
                    "{label}: 7-Zip failed:\n{output}"
                );
                ext_trees::assert_matches_sevenzip(&ours, &theirs, &output, &label);
            }
        }
    }
}
