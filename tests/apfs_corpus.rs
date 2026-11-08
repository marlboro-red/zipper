//! Every image in `tests/data/apfs-corpus` extracts to the tree recorded
//! when the corpus was written, and to the tree `7zz x` extracts now where
//! it is installed. The parity APFS image and DMGs, and generated containers
//! with many files, deep folders, hard links, links, sparse, fragmented,
//! cloned and compressed files, extended attributes, several volumes, plain
//! and hashed names and multi-level B-trees, raw, inside a DMG and inside a
//! GPT disk, extract exactly as `7zz x` extracts them, apart from the
//! differences `docs/reference/apfs-reader.md` lists. Generated containers
//! are also checked against the bytes they were written from. Without 7-Zip
//! 26 or later the differential checks are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/apfs_trees.rs"]
mod apfs_trees;
#[path = "common/apfs_writer.rs"]
mod apfs_writer;
#[path = "common/disk_writer.rs"]
mod disk_writer;
#[path = "common/dmg_writer.rs"]
mod dmg_writer;
#[path = "common/hfs_writer.rs"]
mod hfs_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/xattrs.rs"]
mod xattrs;

use apfs_trees::{Tree, as_zipper_writes, sevenzip_run, sha256, tree, without_modes};
use apfs_writer::{
    Container, Entry, Kind, ROOT, Volume, Xattr, compressed, decmpfs_header, file, folder,
    hard_link, sparse, symlink,
};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zipper::{Engine, ExtractOptions, Reader};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/apfs-corpus");
const PARITY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/parity/archives");
pub const APFS_PARTITION: [u8; 16] = [
    0xEF, 0x57, 0x34, 0x7C, 0x00, 0x00, 0xAA, 0x11, 0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC, 0xAC,
];

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn zipper_tree(image: &Path, into: &Path) -> anyhow::Result<Tree> {
    zipper::extract_archive(image, into, ExtractOptions::default())?;
    xattrs::assert_no_skipped_value(into);
    Ok(tree(into))
}

fn compare(binary: &Path, image: &Path, differs: &[&str], label: &str) -> Tree {
    let work = tempfile::tempdir().unwrap();
    let ours = zipper_tree(image, &work.path().join("zipper"))
        .unwrap_or_else(|error| panic!("{label}: {error:#}"));
    let (_, theirs, output) = sevenzip_run(binary, image, &work.path().join("7z"));
    assert_eq!(
        ours,
        as_zipper_writes(&theirs, &ours, differs, label),
        "{label} differs from 7-Zip:\n{output}"
    );
    ours
}

fn unpacked(stored: &Path, into: &Path) -> PathBuf {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(fs::File::open(stored).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    let name = stored
        .file_name()
        .unwrap()
        .to_string_lossy()
        .trim_end_matches(".xz")
        .trim_end_matches(".stored")
        .to_owned();
    let path = into.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

const SEVEN_ZIP_DIFFERS: [(&str, &str, &str); 3] = [
    (
        "special-udzo.dmg.xz",
        "clone.txt",
        "7-Zip fails on a clone whose data stream another inode owns",
    ),
    (
        "special-udzo.dmg.xz",
        "sparse.bin",
        "7-Zip fills holes with the container's first block",
    ),
    (
        "special-udzo.dmg.xz",
        "up/parent-link",
        "7-Zip writes a link with `..` as an empty file",
    ),
];

fn corpus_differences(image: &str) -> Vec<&'static str> {
    SEVEN_ZIP_DIFFERS
        .iter()
        .filter(|(name, _, _)| *name == image)
        .map(|(_, path, reason)| {
            eprintln!("{image}: {path} differs from 7-Zip, since {reason}");
            *path
        })
        .collect()
}

#[test]
fn every_corpus_image_extracts_the_recorded_tree_and_the_tree_7zip_extracts() {
    let recorded: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let binary = sevenzip_binary();
    let writing = std::env::var_os("ZIPPER_APFS_WRITE_TREES").is_some();
    let mut written: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let mut images: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "xz"))
        .collect();
    images.sort();
    assert!(images.len() >= 13, "{images:?}");
    for stored in images {
        let name = stored.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let image = unpacked(&stored, work.path());
        let ours = match &binary {
            Some(binary) => compare(binary, &image, &corpus_differences(&name), &name),
            None => zipper_tree(&image, &work.path().join("zipper"))
                .unwrap_or_else(|error| panic!("{name}: {error:#}")),
        };
        let ours = without_modes(&ours);
        let earlier = written
            .iter()
            .find(|(_, tree)| **tree == serde_json::to_value(&ours).unwrap())
            .map(|(other, _)| other.clone());
        if writing {
            let value = match earlier {
                Some(other) => serde_json::Value::String(other),
                None => serde_json::to_value(&ours).unwrap(),
            };
            written.insert(name, value);
            continue;
        }
        let value = match &recorded[&name] {
            serde_json::Value::String(other) => &recorded[other],
            value => value,
        };
        let expected: Tree = serde_json::from_value(value.clone()).unwrap();
        let expected: Tree = expected
            .into_iter()
            .filter(|(path, _)| xattrs::RESTORED_HERE || !path.contains(':'))
            .collect();
        assert_eq!(ours, expected, "{name} differs from the recorded tree");
        written.insert(name, serde_json::to_value(&ours).unwrap());
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
fn the_parity_apfs_image_and_dmgs_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    for name in [
        "apfs.img.stored.xz",
        "dmg-bzip2.dmg",
        "dmg-lzfse.dmg",
        "dmg-lzma.dmg",
        "dmg-zlib.dmg",
    ] {
        let work = tempfile::tempdir().unwrap();
        let source = Path::new(PARITY).join(name);
        let image = if name.ends_with(".xz") {
            unpacked(&source, work.path())
        } else {
            source
        };
        let expected_reader = if name.ends_with(".dmg") {
            Reader::Dmg
        } else {
            Reader::Apfs
        };
        assert_eq!(
            zipper::choose_reader(Engine::Auto, &image).unwrap(),
            expected_reader,
            "{name}"
        );
        let ours = compare(&binary, &image, &[], name);
        assert!(ours.contains_key("folder/data.txt"), "{name}: {ours:?}");
    }
}

fn seeded(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed;
    (0..length)
        .map(|index| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            if index % 3 == 0 {
                b"abcabc"[index % 6]
            } else {
                (state >> 56) as u8
            }
        })
        .collect()
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn chunks(data: &[u8], encode: impl Fn(&[u8]) -> Vec<u8>, stored: u8) -> Vec<Vec<u8>> {
    data.chunks(1 << 16)
        .map(|chunk| {
            let packed = encode(chunk);
            if packed.len() < chunk.len() {
                packed
            } else {
                [&[stored][..], chunk].concat()
            }
        })
        .collect()
}

type Method = (&'static str, Vec<u8>, Vec<u8>, Option<Vec<u8>>);

struct Generated {
    container: Container,
    expected: Tree,
}

fn expected_tree(volumes: &[Volume], several: bool) -> Tree {
    let mut expected = Tree::new();
    for (number, volume) in volumes.iter().enumerate() {
        let prefix = if several {
            let name = format!("Volume{}", number + 1);
            expected.insert(name.clone(), "dir".into());
            Some(name)
        } else {
            None
        };
        let mut paths: BTreeMap<u64, String> = BTreeMap::new();
        let mut data: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        let owners: BTreeMap<u64, &Entry> = volume
            .entries
            .iter()
            .map(|entry| (entry.id, entry))
            .collect();
        let mut pending: Vec<&Entry> = volume.entries.iter().collect();
        while !pending.is_empty() {
            let before = pending.len();
            pending.retain(|entry| {
                let parent = if entry.parent == ROOT {
                    prefix.clone()
                } else {
                    match paths.get(&entry.parent) {
                        Some(path) => Some(path.clone()),
                        None => return true,
                    }
                };
                let name = entry.name.replace('/', "_");
                let path = match parent {
                    Some(parent) => format!("{parent}/{name}"),
                    None => name,
                };
                let owner = match entry.kind {
                    Kind::HardLink(inode) => owners[&inode],
                    _ => entry,
                };
                for xattr in owner.xattrs.iter().filter(|xattr| restored(&xattr.name)) {
                    expected.insert(
                        format!("{path}:{}", xattrs::sevenzip_stream_name(&xattr.name)),
                        sha256(&xattr.value),
                    );
                }
                let content = match &entry.kind {
                    Kind::Folder => {
                        paths.insert(entry.id, path.clone());
                        expected.insert(path, "dir".into());
                        return false;
                    }
                    Kind::Symlink(target) => {
                        expected.insert(path, format!("link:{target}"));
                        return false;
                    }
                    Kind::File(bytes) => bytes.clone(),
                    Kind::Sparse { size, pieces } => {
                        let mut bytes = vec![0u8; *size as usize];
                        for (offset, piece) in pieces {
                            let end = (*offset as usize + piece.len()).min(bytes.len());
                            bytes[*offset as usize..end]
                                .copy_from_slice(&piece[..end - *offset as usize]);
                        }
                        bytes
                    }
                    Kind::Compressed { size, .. } => {
                        let original = volume
                            .entries
                            .iter()
                            .find(|other| other.name == format!("{}.original", entry.name))
                            .map(|other| match &other.kind {
                                Kind::File(bytes) => bytes.clone(),
                                _ => unreachable!(),
                            })
                            .unwrap();
                        assert_eq!(original.len() as u64, *size);
                        original
                    }
                    Kind::HardLink(inode) => match data.get(inode) {
                        Some(bytes) => bytes.clone(),
                        None => return true,
                    },
                    Kind::Clone { of, .. } => match data.get(of) {
                        Some(bytes) => bytes.clone(),
                        None => return true,
                    },
                    Kind::Fifo => Vec::new(),
                };
                data.insert(entry.id, content.clone());
                expected.insert(path, sha256(&content));
                false
            });
            assert!(pending.len() < before, "generated volume has an orphan");
        }
    }
    expected
}

fn restored(name: &str) -> bool {
    xattrs::RESTORED_HERE
        && !xattrs::never_restored(name)
        && !["com.apple.ResourceFork", "com.apple.decmpfs"].contains(&name)
}

fn xattr(name: &str, value: &[u8], stream: bool) -> Xattr {
    Xattr {
        name: name.into(),
        value: value.to_vec(),
        stream,
    }
}

fn skipped_names() -> Vec<Xattr> {
    [
        "com.apple.quarantine",
        "com.apple.provenance",
        "com.apple.macl",
        "com.apple.rootless",
        "com.apple.root.installed",
        "com.apple.system.Security",
    ]
    .into_iter()
    .map(|name| xattr(name, xattrs::SKIPPED_VALUE, false))
    .collect()
}

fn variety(seed: u64) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut id = 16u64;
    let mut next = || {
        id += 1;
        id
    };
    let docs = next();
    entries.push(folder(docs, ROOT, "docs"));
    let readme = next();
    entries.push(file(
        readme,
        docs,
        "readme.txt",
        b"hello from the APFS writer\n",
    ));
    entries.push(file(next(), docs, "empty", b""));
    let mut fragmented = file(next(), docs, "fragmented.bin", &seeded(seed, 50_000));
    fragmented.fragments = 4;
    entries.push(fragmented);
    let mut tagged = file(next(), ROOT, "tagged.txt", b"has attributes\n");
    tagged.xattrs = vec![
        Xattr {
            name: "user.small".into(),
            value: b"value".to_vec(),
            stream: false,
        },
        Xattr {
            name: "user.large".into(),
            value: seeded(seed + 1, 9000),
            stream: true,
        },
    ];
    tagged
        .xattrs
        .push(xattr(&"n".repeat(127), b"longest name", false));
    tagged.xattrs.push(xattr(
        "com.apple.metadata:kMDItemWhereFroms",
        b"colon",
        false,
    ));
    tagged.xattrs.push(xattr("user.empty", b"", false));
    tagged.xattrs.extend(skipped_names());
    entries.push(tagged);
    let mut large = file(next(), ROOT, "large-attribute.txt", b"one large value\n");
    large
        .xattrs
        .push(xattr("user.three-mib", &seeded(seed + 3, 3 << 20), true));
    large.mode = apfs_writer::REGULAR | 0o444;
    entries.push(large);
    for name in [
        "café ünïcödé.txt",
        "日本語.txt",
        "emoji 😀.txt",
        "slash/in name",
    ] {
        entries.push(file(next(), ROOT, name, name.as_bytes()));
    }
    let mut run = file(next(), ROOT, "run.sh", b"#!/bin/sh\n");
    run.mode = apfs_writer::REGULAR | 0o755;
    entries.push(run);
    let locked = next();
    let mut locked_folder = folder(locked, ROOT, "locked");
    locked_folder.mode = apfs_writer::DIRECTORY | 0o500;
    locked_folder.xattrs = vec![
        xattr("user.folder", b"on a locked folder", false),
        xattr("user.folder-stream", &seeded(seed + 4, 5000), true),
    ];
    locked_folder.xattrs.extend(skipped_names());
    entries.push(locked_folder);
    let mut private = file(next(), locked, "private.txt", b"secret\n");
    private.mode = apfs_writer::REGULAR | 0o600;
    entries.push(private);
    let shared = next();
    let mut shared_file = file(shared, ROOT, "shared.txt", b"one file, three names\n");
    shared_file
        .xattrs
        .push(xattr("user.shared", b"every name", false));
    entries.push(shared_file);
    entries.push(hard_link(next(), docs, "second-name.txt", shared));
    entries.push(hard_link(next(), ROOT, "third-name.txt", shared));
    let mut tagged_link = symlink(next(), ROOT, "link-to-readme", "docs/readme.txt");
    tagged_link.xattrs = vec![
        xattr("user.link", b"on the link itself", false),
        xattr("user.link-stream", &seeded(seed + 5, 4000), true),
    ];
    tagged_link.xattrs.extend(skipped_names());
    entries.push(tagged_link);
    entries.push(symlink(next(), docs, "link-to-sibling", "readme.txt"));
    let mut parent = ROOT;
    for level in 0..60 {
        let deep = next();
        entries.push(folder(deep, parent, &format!("d{level:02}")));
        parent = deep;
    }
    entries.push(file(next(), parent, "bottom.txt", b"sixty folders down\n"));
    let wide = next();
    entries.push(folder(wide, ROOT, "wide"));
    for number in 0..1500 {
        entries.push(file(
            next(),
            wide,
            &format!("file{number:04}.txt"),
            format!("file {number}\n").as_bytes(),
        ));
    }
    let packed = next();
    entries.push(folder(packed, ROOT, "packed"));
    let text: Vec<u8> = (0..200_000u32)
        .flat_map(|n| format!("line {n}\n").into_bytes())
        .take(150_000)
        .collect();
    let small = b"compressed inline text ".repeat(40);
    let noise = seeded(seed + 2, 70_000);
    let mut methods: Vec<Method> = vec![
        (
            "zlib-inline",
            small.clone(),
            decmpfs_header(3, small.len() as u64, &zlib(&small)),
            None,
        ),
        (
            "zlib-stored",
            small.clone(),
            decmpfs_header(3, small.len() as u64, &[&[0xFF][..], &small].concat()),
            None,
        ),
        (
            "lzvn-inline",
            small.clone(),
            decmpfs_header(7, small.len() as u64, &dmg_writer::lzvn(&small)),
            None,
        ),
        (
            "lzvn-stored",
            small.clone(),
            decmpfs_header(7, small.len() as u64, &[&[0x06][..], &small].concat()),
            None,
        ),
        (
            "copy-inline",
            small.clone(),
            decmpfs_header(9, small.len() as u64, &[&[0xCC][..], &small].concat()),
            None,
        ),
    ];
    for (name, data) in [("text", text.clone()), ("noise", noise.clone())] {
        methods.push((
            Box::leak(format!("zlib-fork-{name}").into_boxed_str()),
            data.clone(),
            decmpfs_header(4, data.len() as u64, &[]),
            Some(hfs_writer::zlib_fork(&chunks(&data, zlib, 0xFF))),
        ));
        methods.push((
            Box::leak(format!("lzvn-fork-{name}").into_boxed_str()),
            data.clone(),
            decmpfs_header(8, data.len() as u64, &[]),
            Some(hfs_writer::lzvn_fork(&chunks(
                &data,
                dmg_writer::lzvn,
                0x06,
            ))),
        ));
    }
    for (name, original, decmpfs, resource) in methods {
        entries.push(compressed(
            next(),
            packed,
            name,
            decmpfs,
            original.len() as u64,
            resource,
        ));
        entries.push(file(next(), packed, &format!("{name}.original"), &original));
    }
    entries
}

fn holes(seed: u64) -> Vec<Entry> {
    let original = seeded(seed, 30_000);
    let mut entries = vec![
        sparse(
            100,
            ROOT,
            "sparse.bin",
            3 << 20,
            vec![
                (1 << 20, seeded(seed + 1, 8192)),
                (2 << 20, b"tail".to_vec()),
            ],
        ),
        file(101, ROOT, "original.txt", &original),
    ];
    let mut clone = file(102, ROOT, "clone.txt", &[]);
    clone.kind = Kind::Clone {
        of: 101,
        size: original.len() as u64,
    };
    entries.push(clone);
    entries
}

fn wrapped(image: &[u8], form: &str, work: &Path, label: &str) -> PathBuf {
    let bytes = match form {
        "raw" => image.to_vec(),
        "dmg" => {
            dmg_writer::write(
                &[dmg_writer::Partition {
                    name: "disk image (Apple_APFS : 1)".into(),
                    data: image.to_vec(),
                }],
                &dmg_writer::Options::default(),
            )
            .0
        }
        "gpt" => disk_writer::gpt(&[(APFS_PARTITION, "disk image", 40, image)], 8),
        _ => unreachable!(),
    };
    let path = work.join(format!("{label}-{form}.img"));
    fs::write(&path, bytes).unwrap();
    path
}

fn check(generated: &Generated, label: &str, differs: &[&str]) {
    let binary = sevenzip_binary();
    let (image, _) = apfs_writer::build(&generated.container);
    let image = image.to_vec();
    for form in ["raw", "dmg", "gpt"] {
        let work = tempfile::tempdir().unwrap();
        let path = wrapped(&image, form, work.path(), label);
        let label = format!("{label} ({form})");
        let ours = match &binary {
            Some(binary) => compare(binary, &path, differs, &label),
            None => zipper_tree(&path, &work.path().join("zipper"))
                .unwrap_or_else(|error| panic!("{label}: {error:#}")),
        };
        assert_eq!(
            without_modes(&ours),
            generated.expected,
            "{label} differs from the bytes it was written from"
        );
    }
}

fn single(volume: Volume) -> Generated {
    let expected = expected_tree(std::slice::from_ref(&volume), false);
    Generated {
        container: Container::new(vec![volume]),
        expected,
    }
}

#[test]
fn generated_volumes_extract_byte_for_byte_as_7zip_extracts_them() {
    let hashed = single(Volume::new(variety(1)));
    check(&hashed, "hashed", &[]);
    let mut plain = Volume::new(variety(2));
    plain.normalization_insensitive = false;
    check(&single(plain), "plain names", &[]);
    let mut insensitive = Volume::new(variety(3));
    insensitive.case_insensitive = true;
    check(&single(insensitive), "case-insensitive", &[]);
    let mut deep_trees = single(Volume::new(variety(4)));
    deep_trees.container.records_per_leaf = 7;
    deep_trees.container.children_per_index = 3;
    deep_trees.container.omap_entries_per_leaf = 5;
    check(&deep_trees, "multi-level trees", &[]);
}

#[test]
fn several_volumes_extract_as_7zip_names_them() {
    let mut second = Volume::new(vec![
        file(16, ROOT, "Name.txt", b"upper\n"),
        folder(17, ROOT, "inside"),
        file(18, 17, "deeper.txt", b"second volume\n"),
    ]);
    second.normalization_insensitive = false;
    let volumes = vec![Volume::new(variety(5)), second, Volume::new(Vec::new())];
    let expected = expected_tree(&volumes, true);
    let mut container = Container::new(volumes);
    container.max_volumes = 5;
    check(
        &Generated {
            container,
            expected,
        },
        "three volumes",
        &[],
    );
}

#[test]
fn sparse_and_cloned_files_hold_the_bytes_they_were_written_from() {
    let generated = single(Volume::new(holes(6)));
    check(&generated, "holes and clones", &["sparse.bin", "clone.txt"]);
}

#[test]
fn a_container_past_4_gib_reads_its_far_blocks() {
    let binary = sevenzip_binary();
    let volume = Volume::new(vec![
        file(16, ROOT, "far.txt", &b"written past 4 GiB\n".repeat(400)),
        sparse(
            17,
            ROOT,
            "far-sparse.bin",
            1 << 20,
            vec![(1 << 16, b"far".to_vec())],
        ),
    ]);
    let expected = expected_tree(std::slice::from_ref(&volume), false);
    let mut container = Container::new(vec![volume]);
    container.data_at = Some((5u64 << 30) / 4096);
    let (image, layout) = apfs_writer::build(&container);
    assert!(layout.extents.iter().all(|extent| extent.1 << 12 > 4 << 30));
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("far.img");
    image.write_sparse(&path);
    let ours = match &binary {
        Some(binary) => compare(binary, &path, &["far-sparse.bin"], "far"),
        None => zipper_tree(&path, &work.path().join("zipper")).unwrap(),
    };
    assert_eq!(without_modes(&ours), expected);
}

#[test]
fn the_newest_checkpoint_wins_over_an_older_block_zero() {
    let volume = Volume::new(vec![file(
        16,
        ROOT,
        "newest.txt",
        b"in the newest checkpoint\n",
    )]);
    let mut container = Container::new(vec![volume]);
    container.stale_block_zero = true;
    let (image, _) = apfs_writer::build(&container);
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("stale.img");
    fs::write(&path, image.to_vec()).unwrap();
    let ours = zipper_tree(&path, &work.path().join("zipper")).unwrap();
    assert_eq!(
        without_modes(&ours),
        Tree::from([(
            "newest.txt".to_owned(),
            sha256(b"in the newest checkpoint\n")
        )])
    );
    if let Some(binary) = sevenzip_binary() {
        let (_, theirs, _) = sevenzip_run(&binary, &path, &work.path().join("7z"));
        assert!(
            theirs.is_empty(),
            "7-Zip read the newest checkpoint: {theirs:?}"
        );
    }
}

#[cfg(target_os = "macos")]
fn attribute_tree(root: &Path) -> BTreeMap<String, Vec<(String, Vec<u8>)>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let name = entry.path().strip_prefix(root).unwrap();
            (
                name.to_string_lossy().into_owned(),
                xattrs::attributes(entry.path()),
            )
        })
        .collect()
}

#[cfg(target_os = "macos")]
#[test]
fn attributes_follow_the_metadata_selection_strip_and_overwrite_options() {
    use zipper::native_apfs::{ReadOptions, read_archive};
    let tag = |name: &str, value: &[u8]| vec![xattr(name, value, false)];
    let mut docs = folder(16, ROOT, "docs");
    docs.xattrs = tag("user.folder", b"archive folder");
    let mut note = file(17, 16, "note.txt", b"archive note\n");
    note.xattrs = tag("user.note", b"archive note attribute");
    note.mode = apfs_writer::REGULAR | 0o400;
    let modified = note.modified;
    let mut link = symlink(18, 16, "link", "note.txt");
    link.xattrs = tag("user.link", b"archive link attribute");
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("options.img");
    let (bytes, _) = apfs_writer::build(&Container::new(vec![Volume::new(vec![docs, note, link])]));
    fs::write(&image, bytes.to_vec()).unwrap();
    let pair = |name: &str, value: &[u8]| vec![(name.to_owned(), value.to_vec())];

    let plain = work.path().join("plain");
    read_archive(
        &image,
        Some(&plain),
        ReadOptions {
            metadata: zipper::Metadata::ContentOnly,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        attribute_tree(&plain).values().all(Vec::is_empty),
        "{:?}",
        attribute_tree(&plain)
    );

    let stripped = work.path().join("stripped");
    let paths = vec!["docs".to_owned()];
    read_archive(
        &image,
        Some(&stripped),
        ReadOptions {
            paths: &paths,
            strip: zipper::Strip::Components(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        attribute_tree(&stripped),
        BTreeMap::from([
            (
                "link".to_owned(),
                pair("user.link", b"archive link attribute")
            ),
            (
                "note.txt".to_owned(),
                pair("user.note", b"archive note attribute")
            ),
        ])
    );
    let note_path = stripped.join("note.txt");
    let found = fs::metadata(&note_path).unwrap();
    assert_eq!(
        found.modified().unwrap(),
        std::time::UNIX_EPOCH + std::time::Duration::from_nanos(modified)
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(found.permissions().mode() & 0o777, 0o400);

    let existing = work.path().join("existing");
    fs::create_dir_all(existing.join("docs")).unwrap();
    fs::write(existing.join("docs/note.txt"), b"old note\n").unwrap();
    for path in [existing.join("docs"), existing.join("docs/note.txt")] {
        rustix::fs::setxattr(&path, "user.old", b"old", rustix::fs::XattrFlags::empty()).unwrap();
    }
    read_archive(
        &image,
        Some(&existing),
        ReadOptions {
            existing: zipper::Existing::Overwrite,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        attribute_tree(&existing),
        BTreeMap::from([
            ("docs".to_owned(), pair("user.old", b"old")),
            (
                "docs/link".to_owned(),
                pair("user.link", b"archive link attribute")
            ),
            (
                "docs/note.txt".to_owned(),
                pair("user.note", b"archive note attribute")
            ),
        ])
    );
    for root in [&plain, &stripped, &existing] {
        for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
            if !entry.file_type().is_symlink() {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700));
            }
        }
    }
}

#[test]
#[ignore = "writes a container of 999,000 inodes for peak-memory checks"]
fn write_a_container_of_many_inodes_to_zipper_apfs_many() {
    let Some(path) = std::env::var_os("ZIPPER_APFS_MANY") else {
        return;
    };
    let mut entries = Vec::new();
    let mut id = 16u64;
    for folder_number in 0..1000u64 {
        let parent = id;
        entries.push(folder(parent, ROOT, &format!("folder{folder_number:04}")));
        id += 1;
        for number in 0..998u64 {
            entries.push(file(id, parent, &format!("entry{number:04}.txt"), b""));
            id += 1;
        }
    }
    let (image, _) = apfs_writer::build(&Container::new(vec![Volume::new(entries)]));
    image.write_sparse(Path::new(&path));
}

#[cfg(target_os = "macos")]
fn clash_volume() -> Volume {
    let mut clash = file(16, ROOT, "clash", b"clash\n");
    clash.xattrs = vec![xattr("user.clash", b"clash attribute", false)];
    let double = file(17, ROOT, "._clash", b"archive data, not AppleDouble\n");
    let mut plain = Volume::new(vec![double, clash]);
    plain.normalization_insensitive = false;
    plain
}

#[cfg(target_os = "macos")]
#[test]
fn a_fat_destination_gets_no_attributes() {
    let work = tempfile::tempdir().unwrap();
    let disk = work.path().join("fat.dmg");
    let mount = work.path().join("mount");
    fs::create_dir(&mount).unwrap();
    let made = std::process::Command::new("hdiutil")
        .args([
            "create", "-quiet", "-size", "8m", "-fs", "MS-DOS", "-volname", "ZFAT",
        ])
        .arg(&disk)
        .status()
        .is_ok_and(|status| status.success());
    let attached = made
        && std::process::Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(&disk)
            .status()
            .is_ok_and(|status| status.success());
    if !attached {
        eprintln!("hdiutil cannot make a FAT volume here; skipped");
        return;
    }
    let image = work.path().join("clash.img");
    let (bytes, _) = apfs_writer::build(&Container::new(vec![clash_volume()]));
    fs::write(&image, bytes.to_vec()).unwrap();
    let out = mount.join("out");
    let result = zipper::native_apfs::read_archive(&image, Some(&out), Default::default());
    let attributes = xattrs::attributes(&out.join("clash"));
    let mut names: Vec<String> = fs::read_dir(&out)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let _ = std::process::Command::new("hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount)
        .status();
    result.unwrap();
    assert!(names.contains(&"clash".to_owned()), "{names:?}");
    assert!(
        !names.iter().any(|name| name.contains(".zipper-")),
        "{names:?}"
    );
    assert_eq!(attributes, Vec::<(String, Vec<u8>)>::new());
}

#[test]
#[ignore = "writes containers of many and of large attributes for peak-memory checks"]
fn write_containers_of_many_and_large_attributes_to_zipper_apfs_attributes() {
    let Some(folder) = std::env::var_os("ZIPPER_APFS_ATTRIBUTES") else {
        return;
    };
    let folder = Path::new(&folder);
    let mut many = Vec::new();
    for number in 0..7812u64 {
        let mut entry = file(16 + number, ROOT, &format!("entry{number:04}.txt"), b"");
        entry.xattrs = (0..128)
            .map(|index| xattr(&format!("user.{index:03}"), b"v", false))
            .collect();
        many.push(entry);
    }
    let (image, _) = apfs_writer::build(&Container::new(vec![Volume::new(many)]));
    image.write_sparse(&folder.join("many.img"));
    let mut large = Vec::new();
    let mut cap = file(16, ROOT, "cap.txt", b"one value at the cap\n");
    cap.xattrs = vec![xattr("user.cap", &seeded(1, 64 << 20), true)];
    large.push(cap);
    for number in 0..64u64 {
        let mut entry = file(17 + number, ROOT, &format!("large{number:02}.txt"), b"x\n");
        entry.xattrs = (0..16)
            .map(|index| {
                xattr(
                    &format!("user.{index:02}"),
                    &seeded(number * 16 + index, 256 << 10),
                    true,
                )
            })
            .collect();
        large.push(entry);
    }
    let (image, _) = apfs_writer::build(&Container::new(vec![Volume::new(large)]));
    image.write_sparse(&folder.join("large.img"));
}
