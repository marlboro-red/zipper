//! Every DMG in `tests/data/dmg-corpus` extracts to the tree 7-Zip
//! extracted when the corpus was written, and to the tree `7zz x` extracts
//! now where it is installed. Generated DMGs of every chunk method, both
//! partition list forms, Apple, GPT and MBR partition maps inside a DMG, and
//! generated HFS+ and HFSX volumes with compressed files, hard links,
//! symlinks, deep and wide folders and Unicode names extract exactly as
//! `7zz x` extracts them, apart from the differences
//! `docs/reference/dmg-reader.md` lists. Without 7-Zip 26.05 or later the
//! differential tests are skipped, unless `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/dmg_writer.rs"]
mod dmg_writer;
#[path = "common/hfs_writer.rs"]
mod hfs_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/xattrs.rs"]
mod xattrs;

use dmg_writer::{Chunk, Form, Options, Partition};
use hfs_writer::{Attribute, AttributeValue, Entry, Kind, ROOT, ROOT_PARENT, Volume};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_dmg::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/dmg-corpus");
const PARITY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/parity/archives");
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const PRIVATE: &str = "[HFS+ Private Data]";

type Tree = BTreeMap<String, String>;

fn sevenzip_binary() -> Option<PathBuf> {
    sevenzip::sevenzip()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    format!(" {:o}", metadata.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn mode(_: &fs::Metadata) -> String {
    String::new()
}

#[cfg(unix)]
fn links(metadata: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(metadata)
}

#[cfg(not(unix))]
fn links(_: &fs::Metadata) -> u64 {
    1
}

fn open_up(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        if entry.file_type().is_dir() {
            let mode = entry.metadata().unwrap().permissions().mode();
            if mode & 0o500 != 0o500 {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode | 0o700));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

fn tree(root: &Path) -> (Tree, Vec<String>) {
    let mut found = Tree::new();
    let mut hard = Vec::new();
    if !root.exists() {
        return (found, hard);
    }
    let modes: BTreeMap<PathBuf, fs::Metadata> = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            (
                entry.path().to_owned(),
                fs::symlink_metadata(entry.path()).unwrap(),
            )
        })
        .collect();
    open_up(root);
    for entry in walkdir::WalkDir::new(root).min_depth(1).sort_by_file_name() {
        let entry = entry.unwrap();
        let name = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = modes
            .get(entry.path())
            .cloned()
            .unwrap_or_else(|| fs::symlink_metadata(entry.path()).unwrap());
        let kind = if entry.file_type().is_symlink() {
            format!("link:{}", fs::read_link(entry.path()).unwrap().display())
        } else if entry.file_type().is_dir() {
            format!("dir{}", mode(&metadata))
        } else {
            if links(&metadata) > 1 && !name.contains(PRIVATE) {
                hard.push(name.clone());
            }
            format!(
                "{}{}",
                sha256(&fs::read(entry.path()).unwrap()),
                mode(&metadata)
            )
        };
        for (attribute, value) in xattrs::attributes(entry.path()) {
            found.insert(
                format!("{name}:{}", xattrs::sevenzip_stream_name(&attribute)),
                sha256(&value),
            );
        }
        found.insert(name, kind);
    }
    (found, hard)
}

fn without_modes(tree: &Tree) -> Tree {
    tree.iter()
        .map(|(name, kind)| (name.clone(), kind.split(' ').next().unwrap().to_owned()))
        .collect()
}

fn zipper_tree(image: &Path, into: &Path) -> anyhow::Result<(Tree, Vec<String>)> {
    read_archive(image, Some(into), ReadOptions::default())?;
    xattrs::assert_no_skipped_value(into);
    Ok(tree(into))
}

fn sevenzip_run(binary: &Path, image: &Path, into: &Path) -> (bool, Tree) {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(image)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (run.status.success(), tree(into).0)
}

fn as_zipper_writes(theirs: &Tree, ours: &Tree, hard: &[String]) -> Tree {
    let mut expected: Tree = theirs
        .iter()
        .filter(|(name, _)| xattrs::sevenzip_stream_kept(name))
        .map(|(name, kind)| match name.contains(':') {
            true => (name.clone(), kind.split(' ').next().unwrap().to_owned()),
            false => (name.clone(), kind.clone()),
        })
        .collect();
    for (name, kind) in ours {
        if xattrs::sevenzip_writes_no_attribute_of(ours, name) {
            expected.insert(name.clone(), kind.clone());
        }
    }
    for path in hard {
        let theirs = expected
            .get(path)
            .map(|kind| kind.split(' ').next().unwrap());
        assert_eq!(
            theirs,
            Some(EMPTY),
            "7-Zip wrote hard link {path} with data"
        );
        expected.insert(path.clone(), ours[path].clone());
        let attributes = format!("{path}:");
        for (name, kind) in ours.range(attributes.clone()..) {
            if !name.starts_with(&attributes) {
                break;
            }
            expected.insert(name.clone(), kind.clone());
        }
    }
    expected
}

fn compare(binary: &Path, image: &Path, label: &str) -> Tree {
    let work = tempfile::tempdir().unwrap();
    let (ours, hard) = zipper_tree(image, &work.path().join("zipper"))
        .unwrap_or_else(|error| panic!("{label}: {error:#}"));
    let (succeeded, theirs) = sevenzip_run(binary, image, &work.path().join("7z"));
    assert!(succeeded, "{label}: 7-Zip failed");
    assert_eq!(
        ours,
        as_zipper_writes(&theirs, &ours, &hard),
        "{label} differs from 7-Zip"
    );
    ours
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

fn corpus_images() -> Vec<PathBuf> {
    let mut images: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "xz"))
        .collect();
    images.sort();
    images
}

fn recorded_trees() -> BTreeMap<String, Tree> {
    let stored: BTreeMap<String, serde_json::Value> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let mut trees = BTreeMap::new();
    for (image, value) in &stored {
        let value = match value.as_str() {
            Some(same) => &stored[same],
            None => value,
        };
        trees.insert(
            image.clone(),
            serde_json::from_value(value.clone()).unwrap(),
        );
    }
    trees
}

fn shared_trees(trees: &BTreeMap<String, Tree>) -> BTreeMap<String, serde_json::Value> {
    let mut stored: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (image, tree) in trees {
        let earlier = trees
            .iter()
            .take_while(|(other, _)| *other != image)
            .find(|(_, other)| *other == tree)
            .map(|(other, _)| other.clone());
        let value = match earlier {
            Some(other) => serde_json::Value::String(other),
            None => serde_json::to_value(tree).unwrap(),
        };
        stored.insert(image.clone(), value);
    }
    stored
}

#[test]
fn every_corpus_dmg_extracts_the_tree_7zip_extracts() {
    let recorded = recorded_trees();
    let binary = sevenzip_binary();
    let writing = std::env::var_os("ZIPPER_DMG_WRITE_TREES").is_some();
    let mut written = BTreeMap::new();
    let images = corpus_images();
    assert!(images.len() >= 9, "{images:?}");
    for stored in images {
        let name = stored.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let image = unpacked(&stored, work.path());
        let ours = match &binary {
            Some(binary) => compare(binary, &image, &name),
            None => {
                zipper_tree(&image, &work.path().join("zipper"))
                    .unwrap_or_else(|error| panic!("{name}: {error:#}"))
                    .0
            }
        };
        if writing {
            written.insert(name, without_modes(&ours));
        } else {
            assert_eq!(
                Some(&without_modes(&ours)),
                recorded.get(&name),
                "{name} differs from the recorded tree"
            );
        }
    }
    if writing {
        fs::write(
            Path::new(CORPUS).join("trees.json"),
            serde_json::to_string_pretty(&shared_trees(&written)).unwrap() + "\n",
        )
        .unwrap();
    }
}

#[test]
fn the_parity_hfs_and_apm_images_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    for name in ["hfs.img.stored.xz", "apm.img.stored.xz"] {
        let work = tempfile::tempdir().unwrap();
        let image = unpacked(&Path::new(PARITY).join(name), work.path());
        let image = {
            let renamed = work.path().join(name.trim_end_matches(".stored.xz"));
            fs::rename(&image, &renamed).unwrap();
            renamed
        };
        let ours = compare(&binary, &image, name);
        assert!(
            ours.contains_key("parity/folder/data.txt"),
            "{name}: {ours:?}"
        );
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

fn compressed(
    id: u32,
    name: &str,
    method: u32,
    data: &[u8],
    payload: Vec<u8>,
    resource: Vec<u8>,
) -> (Entry, Attribute) {
    let mut entry = hfs_writer::file(id, ROOT, name, &[]);
    entry.kind = Kind::File {
        data: Vec::new(),
        resource,
    };
    let attribute = Attribute {
        id,
        name: "com.apple.decmpfs".into(),
        value: AttributeValue::Inline(hfs_writer::decmpfs_header(
            method,
            data.len() as u64,
            &payload,
        )),
    };
    (entry, attribute)
}

fn feature_volume(hfsx: bool, fragment: bool, per_leaf: usize) -> Volume {
    let mut volume = Volume::new("Generated");
    volume.hfsx = hfsx;
    volume.fragment = fragment;
    volume.records_per_leaf = per_leaf;
    let mut entries = vec![
        hfs_writer::folder(ROOT, ROOT_PARENT, "Generated"),
        hfs_writer::folder(16, ROOT, "folder"),
        hfs_writer::file(17, ROOT, "hello.txt", b"hello, world\n"),
        hfs_writer::file(18, 16, "data.bin", &seeded(1, 100_000)),
        hfs_writer::file(19, 16, "empty", b""),
        hfs_writer::symlink(20, 16, "link", "data.bin"),
        hfs_writer::file(
            21,
            ROOT,
            "caf\u{e9} \u{fc}n\u{ef}c\u{f6}d\u{e9}.txt",
            b"unicode\n",
        ),
        hfs_writer::file(22, ROOT, "cafe\u{301} decomposed.txt", b"nfd\n"),
        hfs_writer::file(23, ROOT, "slash/in name", b"slash\n"),
        hfs_writer::file(24, ROOT, " ", b"blank\n"),
        hfs_writer::file(25, ROOT, "\u{1F600} smile", b"emoji\n"),
        hfs_writer::private_folder(30),
        hfs_writer::file(31, 30, "iNode100", b"linked data\n"),
        hfs_writer::hard_link(32, ROOT, "linked one", 100),
        hfs_writer::hard_link(33, 16, "linked two", 100),
        hfs_writer::folder(50, ROOT, "modes"),
    ];
    let mut modes = vec![
        hfs_writer::file(51, 50, "exec", b"x\n"),
        hfs_writer::file(52, 50, "readonly", b"r\n"),
        hfs_writer::file(53, 50, "typeless", b"t\n"),
        hfs_writer::file(54, 50, "fifo", b""),
        hfs_writer::folder(55, 50, "locked"),
        hfs_writer::folder(56, 50, "typeless folder"),
    ];
    modes[0].mode = 0o100_755;
    modes[1].mode = 0o100_444;
    modes[2].mode = 0o644;
    modes[3].mode = 0o010_644;
    modes[4].mode = 0o040_555;
    modes[5].mode = 0o555;
    entries.extend(modes);
    let mut parent = 16;
    for depth in 0..60 {
        let id = 100 + depth;
        entries.push(hfs_writer::folder(id, parent, &format!("d{depth:02}")));
        parent = id;
    }
    entries.push(hfs_writer::file(200, parent, "leaf.txt", b"deep\n"));
    entries.push(hfs_writer::folder(201, ROOT, "wide"));
    for index in 0..1500 {
        entries.push(hfs_writer::file(
            1000 + index,
            201,
            &format!("f{index:04}"),
            format!("{index}\n").as_bytes(),
        ));
    }
    let text: Vec<u8> = b"compressible text line\n".repeat(7000);
    let mut attributes = Vec::new();
    let mut add = |pair: (Entry, Attribute)| {
        entries.push(pair.0);
        attributes.push(pair.1);
    };
    add(compressed(
        40,
        "zlib inline",
        3,
        &text[..3000],
        zlib(&text[..3000]),
        Vec::new(),
    ));
    let mut stored = vec![0xFF];
    stored.extend(b"stored inline");
    add(compressed(
        41,
        "zlib stored",
        3,
        b"stored inline",
        stored,
        Vec::new(),
    ));
    add(compressed(
        42,
        "lzvn inline",
        7,
        &text[..3000],
        dmg_writer::lzvn(&text[..3000]),
        Vec::new(),
    ));
    let mut stored = vec![0x06];
    stored.extend(b"lzvn stored");
    add(compressed(
        43,
        "lzvn stored",
        7,
        b"lzvn stored",
        stored,
        Vec::new(),
    ));
    let mut stored = vec![0xCC];
    stored.extend(b"copied");
    add(compressed(
        44,
        "copy inline",
        9,
        b"copied",
        stored,
        Vec::new(),
    ));
    let noise = seeded(9, 70_000);
    let chunks: Vec<&[u8]> = text.chunks(1 << 16).collect();
    let mut zlib_chunks: Vec<Vec<u8>> = chunks.iter().map(|chunk| zlib(chunk)).collect();
    let mut raw = vec![0xFF];
    raw.extend(chunks[1]);
    zlib_chunks[1] = raw;
    add(compressed(
        45,
        "zlib fork",
        4,
        &text,
        Vec::new(),
        hfs_writer::zlib_fork(&zlib_chunks),
    ));
    let mut lzvn_chunks: Vec<Vec<u8>> = noise.chunks(1 << 16).map(dmg_writer::lzvn).collect();
    let mut raw = vec![0x06];
    raw.extend(noise.chunks(1 << 16).next().unwrap());
    lzvn_chunks[0] = raw;
    add(compressed(
        46,
        "lzvn fork",
        8,
        &noise,
        Vec::new(),
        hfs_writer::lzvn_fork(&lzvn_chunks),
    ));
    add(compressed(
        47,
        "empty fork",
        8,
        b"",
        Vec::new(),
        hfs_writer::lzvn_fork(&[]),
    ));
    attributes.extend(tagged_attributes());
    volume.entries = entries;
    volume.attributes = attributes;
    volume
}

fn inline(id: u32, name: &str, value: &[u8]) -> Attribute {
    Attribute {
        id,
        name: name.into(),
        value: AttributeValue::Inline(value.to_vec()),
    }
}

fn never_restored(id: u32) -> Vec<Attribute> {
    [
        "com.apple.macl",
        "com.apple.provenance",
        "com.apple.quarantine",
        "com.apple.rootless",
        "com.apple.root.installed",
        "com.apple.system.Security",
    ]
    .into_iter()
    .map(|name| inline(id, name, xattrs::SKIPPED_VALUE))
    .collect()
}

const FOLDER_VALUE: &[u8] = b"on a folder";
const LARGE_ATTRIBUTE: usize = 200_000;

fn tagged_attributes() -> Vec<Attribute> {
    let mut attributes = vec![inline(16, "user.folder", FOLDER_VALUE)];
    attributes.extend(never_restored(16));
    attributes.push(inline(17, &"n".repeat(127), b"longest name"));
    attributes.push(inline(17, "com.apple.metadata:kMDItemWhereFroms", b"colon"));
    attributes.extend(never_restored(17));
    attributes.push(inline(17, "user.empty", b""));
    attributes.push(Attribute {
        id: 17,
        name: "user.large".into(),
        value: AttributeValue::Fork(seeded(5, LARGE_ATTRIBUTE)),
    });
    attributes.push(inline(17, "user.note", b"restored"));
    attributes.push(inline(20, "user.link", b"on the link itself"));
    attributes.extend(never_restored(20));
    attributes.push(inline(31, "user.linked", b"every name"));
    attributes.push(inline(52, "user.read-only", b"on a read-only file"));
    attributes
}

fn wrap(volume: &[u8], methods: &[Chunk], form: Form) -> Vec<u8> {
    let partitions = [
        Partition {
            name: "Protective Master Boot Record (MBR : 0)".into(),
            data: vec![0; 512],
        },
        Partition {
            name: "disk image (Apple_HFS : 1)".into(),
            data: volume.to_vec(),
        },
        Partition {
            name: " (Apple_Free : 2)".into(),
            data: vec![0; 4096],
        },
    ];
    let options = Options {
        chunk_sectors: 24,
        methods: methods.to_vec(),
        form,
        data_checksum: true,
        ..Options::default()
    };
    dmg_writer::write(&partitions, &options).0
}

#[test]
fn generated_volumes_extract_byte_for_byte_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    for (hfsx, fragment, per_leaf) in [(false, false, 1000), (true, false, 7), (false, true, 30)] {
        let (bytes, _) = hfs_writer::build(&feature_volume(hfsx, fragment, per_leaf));
        let work = tempfile::tempdir().unwrap();
        let label = format!("hfsx {hfsx} fragment {fragment} leaves of {per_leaf}");
        let image = work.path().join("volume.dmg");
        fs::write(
            &image,
            wrap(&bytes, &[Chunk::Zlib, Chunk::Raw], Form::Plist),
        )
        .unwrap();
        let ours = compare(&binary, &image, &label);
        assert_eq!(
            ours["Generated/modes/typeless"].split(' ').nth(1),
            Some("644")
        );
        if xattrs::RESTORED_HERE {
            assert_eq!(ours["Generated/folder:user.folder"], sha256(FOLDER_VALUE));
            assert_eq!(
                ours["Generated/hello.txt:user.large"],
                sha256(&seeded(5, LARGE_ATTRIBUTE))
            );
            assert_eq!(
                ours["Generated/modes/readonly"].split(' ').nth(1),
                Some("444")
            );
        }
        assert_eq!(
            ours["Generated/zlib fork"].split(' ').next().unwrap(),
            sha256(&b"compressible text line\n".repeat(7000))
        );
        assert!(ours.keys().any(|key| key.ends_with("d59/leaf.txt")));
        let raw = work.path().join("volume.hfs");
        fs::write(&raw, &bytes).unwrap();
        compare(&binary, &raw, &format!("raw {label}"));
    }
}

#[test]
fn generated_dmgs_of_every_chunk_method_and_form_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let (bytes, _) = hfs_writer::build(&feature_volume(false, false, 50));
    let forms = [Form::Plist, Form::ResourceFork];
    let data_methods: Vec<Chunk> = Chunk::ALL
        .into_iter()
        .filter(|method| !matches!(method, Chunk::Zero | Chunk::Ignore))
        .collect();
    let mut lists: Vec<Vec<Chunk>> = data_methods.iter().map(|&method| vec![method]).collect();
    lists.push(data_methods);
    for form in forms {
        for methods in &lists {
            let work = tempfile::tempdir().unwrap();
            let image = work.path().join("chunks.dmg");
            fs::write(&image, wrap(&bytes, methods, form)).unwrap();
            compare(&binary, &image, &format!("{form:?} {methods:?}"));
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn dmgs_with_apple_lzfse_chunks_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("apple_compression.c");
    fs::write(&source, include_str!("common/apple_compression.c")).unwrap();
    let helper = work.path().join("apple_compression");
    let built = Command::new("cc")
        .args(["-O2", "-o"])
        .arg(&helper)
        .arg(&source)
        .arg("-lcompression")
        .status()
        .is_ok_and(|status| status.success());
    if !built {
        return;
    }
    let folder = work.path().to_owned();
    let encode = move |data: &[u8]| {
        let input = folder.join("chunk.in");
        let output = folder.join("chunk.out");
        fs::write(&input, data).unwrap();
        assert!(
            Command::new(&helper)
                .args(["enc", "lzfse"])
                .arg(&input)
                .arg(&output)
                .status()
                .unwrap()
                .success()
        );
        fs::read(&output).unwrap()
    };
    let (bytes, _) = hfs_writer::build(&feature_volume(false, false, 50));
    let partitions = [Partition {
        name: "disk image (Apple_HFS : 0)".into(),
        data: bytes,
    }];
    let options = Options {
        chunk_sectors: 2048,
        methods: vec![Chunk::Lzfse],
        lzfse: Some(Box::new(encode)),
        ..Options::default()
    };
    let (image, layout) = dmg_writer::write(&partitions, &options);
    assert!(
        image.windows(4).any(|magic| magic == b"bvx2"),
        "{:?}",
        layout.chunks.len()
    );
    let path = work.path().join("lzfse.dmg");
    fs::write(&path, image).unwrap();
    compare(&binary, &path, "Apple LZFSE chunks");
}

fn apm_disk(volume: &[u8]) -> Vec<u8> {
    let block = 512usize;
    let start = 64u32;
    let mut disk = vec![0u8; start as usize * block];
    disk[..2].copy_from_slice(b"ER");
    disk[2..4].copy_from_slice(&512u16.to_be_bytes());
    let entries = [
        ("Apple", "Apple_partition_map", 1u32, 63u32),
        (
            "disk image",
            "Apple_HFS",
            start,
            (volume.len() / block) as u32,
        ),
    ];
    for (index, (name, kind, first, count)) in entries.iter().enumerate() {
        let sector = &mut disk[(index + 1) * block..(index + 2) * block];
        sector[..2].copy_from_slice(b"PM");
        sector[4..8].copy_from_slice(&(entries.len() as u32).to_be_bytes());
        sector[8..12].copy_from_slice(&first.to_be_bytes());
        sector[12..16].copy_from_slice(&count.to_be_bytes());
        sector[16..16 + name.len()].copy_from_slice(name.as_bytes());
        sector[48..48 + kind.len()].copy_from_slice(kind.as_bytes());
        sector[0x58..0x5C].copy_from_slice(&0x33u32.to_be_bytes());
    }
    disk.extend(volume);
    disk
}

fn gpt_disk(volume: &[u8]) -> Vec<u8> {
    let first = 40u64;
    let last = first + (volume.len() / 512) as u64 - 1;
    let mut disk = vec![0u8; first as usize * 512];
    disk[0x1BE + 4] = 0xEE;
    disk[0x1BE + 8..0x1BE + 12].copy_from_slice(&1u32.to_le_bytes());
    disk[0x1BE + 12..0x1BE + 16].copy_from_slice(&(last as u32).to_le_bytes());
    disk[0x1FE] = 0x55;
    disk[0x1FF] = 0xAA;
    let mut entry = vec![0u8; 128];
    entry[..16].copy_from_slice(&[
        0x00, 0x53, 0x46, 0x48, 0x00, 0x00, 0xAA, 0x11, 0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC,
        0xAC,
    ]);
    entry[16] = 1;
    entry[32..40].copy_from_slice(&first.to_le_bytes());
    entry[40..48].copy_from_slice(&last.to_le_bytes());
    let mut table = entry.clone();
    table.resize(128 * 4, 0);
    disk[1024..1024 + table.len()].copy_from_slice(&table);
    let header = &mut disk[512..1024];
    header[..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
    header[12..16].copy_from_slice(&92u32.to_le_bytes());
    header[0x18..0x20].copy_from_slice(&1u64.to_le_bytes());
    header[0x20..0x28].copy_from_slice(&(last + 33).to_le_bytes());
    header[0x28..0x30].copy_from_slice(&first.to_le_bytes());
    header[0x30..0x38].copy_from_slice(&last.to_le_bytes());
    header[0x48..0x50].copy_from_slice(&2u64.to_le_bytes());
    header[0x50..0x54].copy_from_slice(&4u32.to_le_bytes());
    header[0x54..0x58].copy_from_slice(&128u32.to_le_bytes());
    header[0x58..0x5C].copy_from_slice(&crc32fast::hash(&table).to_le_bytes());
    let crc = crc32fast::hash(&header[..92]);
    header[0x10..0x14].copy_from_slice(&crc.to_le_bytes());
    disk.extend(volume);
    disk.extend(vec![0u8; 33 * 512]);
    disk
}

fn mbr_disk(volume: &[u8]) -> Vec<u8> {
    let first = 63u32;
    let mut disk = vec![0u8; first as usize * 512];
    disk[0x1BE + 2] = 1;
    disk[0x1BE + 4] = 0xAF;
    disk[0x1BE + 6] = 1;
    disk[0x1BE + 8..0x1BE + 12].copy_from_slice(&first.to_le_bytes());
    disk[0x1BE + 12..0x1BE + 16].copy_from_slice(&((volume.len() / 512) as u32).to_le_bytes());
    disk[0x1FE] = 0x55;
    disk[0x1FF] = 0xAA;
    disk.extend(volume);
    disk
}

#[test]
fn partition_maps_inside_a_dmg_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let (bytes, _) = hfs_writer::build(&feature_volume(false, false, 100));
    for (name, disk) in [
        ("apm", apm_disk(&bytes)),
        ("gpt", gpt_disk(&bytes)),
        ("mbr", mbr_disk(&bytes)),
    ] {
        let work = tempfile::tempdir().unwrap();
        let partitions = [Partition {
            name: "whole disk (Apple_partition_scheme : 0)".into(),
            data: disk.clone(),
        }];
        let image = work.path().join(format!("{name}.dmg"));
        fs::write(
            &image,
            dmg_writer::write(&partitions, &Options::default()).0,
        )
        .unwrap();
        compare(&binary, &image, name);
        if name == "apm" {
            let raw = work.path().join("disk.apm");
            fs::write(&raw, &disk).unwrap();
            compare(&binary, &raw, "raw apm");
        }
    }
}

#[test]
fn an_hfs_wrapper_extracts_as_7zip_extracts_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let (volume, _) = hfs_writer::build(&feature_volume(false, false, 100));
    let first_block_sector = 4u16;
    let mut image = vec![0u8; usize::from(first_block_sector) * 512];
    image[..2].copy_from_slice(b"LK");
    let mdb = &mut image[1024..1536];
    mdb[..2].copy_from_slice(b"BD");
    let blocks = (volume.len() / 512) as u16;
    mdb[0x12..0x14].copy_from_slice(&blocks.to_be_bytes());
    mdb[0x14..0x18].copy_from_slice(&512u32.to_be_bytes());
    mdb[0x1C..0x1E].copy_from_slice(&first_block_sector.to_be_bytes());
    mdb[0x7C..0x7E].copy_from_slice(b"H+");
    mdb[0x80..0x82].copy_from_slice(&blocks.to_be_bytes());
    image.extend(&volume);
    image.extend(vec![0u8; 1024]);
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("wrapped.hfs");
    fs::write(&path, &image).unwrap();
    let ours = compare(&binary, &path, "HFS wrapper");
    assert!(ours.contains_key("Generated/hello.txt"));
}

#[test]
fn overflow_extents_that_continue_into_the_next_node_extract_where_7zip_refuses_them() {
    let mut volume = feature_volume(false, true, 2);
    volume
        .entries
        .retain(|entry| entry.parent != 201 && entry.id != 201);
    let (bytes, layout) = hfs_writer::build(&volume);
    assert!(
        layout.extents_leaves.len() >= 2,
        "{:?}",
        layout.extents_leaves
    );
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("fragmented.hfs");
    fs::write(&path, &bytes).unwrap();
    let (ours, _) = zipper_tree(&path, &work.path().join("zipper")).unwrap();
    assert_eq!(
        ours["Generated/folder/data.bin"].split(' ').next().unwrap(),
        sha256(&seeded(1, 100_000))
    );
    if let Some(binary) = sevenzip_binary() {
        let (succeeded, _) = sevenzip_run(&binary, &path, &work.path().join("7z"));
        assert!(!succeeded, "7-Zip now reads overflow records across nodes");
    }
}
