//! Every compound file in `tests/data/msi-corpus` extracts to the tree 7-Zip
//! extracted when the corpus was written, and to the tree 7-Zip extracts now
//! where it is installed. Generated compound files of both versions, with
//! mini and regular streams, nested storages, scattered sectors, a DIFAT and
//! embedded CABs, extract exactly as `7zz x` extracts them. Without 7-Zip
//! 26 or later the differential tests are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set. `ZIPPER_MSI_SAMPLES` names a folder of
//! further samples to compare.
#[path = "common/cab_writer.rs"]
mod cab_writer;
#[path = "common/cfb_writer.rs"]
mod cfb_writer;
#[path = "common/lzx_encoder.rs"]
mod lzx_encoder;

use cfb_writer::{Compound, Node};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_msi::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/msi-corpus");

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

const FIRST_7ZIP_THESE_FILES_WERE_CHECKED_AGAINST: u32 = 26;

fn sevenzip() -> Option<PathBuf> {
    let found = ["7zz", "7z"]
        .into_iter()
        .find(|binary| {
            Command::new(binary)
                .arg("i")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .map(PathBuf::from)
        .filter(|binary| {
            major_version(binary)
                .is_some_and(|major| major >= FIRST_7ZIP_THESE_FILES_WERE_CHECKED_AGAINST)
        });
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn major_version(binary: &Path) -> Option<u32> {
    let banner = Command::new(binary).output().ok()?.stdout;
    let banner = String::from_utf8_lossy(&banner);
    let after = banner.split("7-Zip").nth(1)?;
    let number = after
        .split_whitespace()
        .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    number.split('.').next()?.parse().ok()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
            let kind = if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                sha256(&fs::read(entry.path()).unwrap())
            };
            (name, kind)
        })
        .collect()
}

fn zipper_tree(archive: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(archive, Some(into), ReadOptions::default())?;
    Ok(tree(into))
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| !matches!(extension.to_str(), Some("json" | "txt")))
        })
        .collect();
    files.sort();
    files
}

fn recorded_trees() -> BTreeMap<String, BTreeMap<String, String>> {
    serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
        .unwrap()
}

fn assert_same_as_7zip(binary: &Path, archive: &Path, scratch: &Path, label: &str) {
    fs::create_dir_all(scratch).unwrap();
    let ours = zipper_tree(archive, &scratch.join("zipper"))
        .unwrap_or_else(|error| panic!("{label}: zipper refused it: {error:#}"));
    let theirs = sevenzip_tree(binary, archive, &scratch.join("7zip"))
        .unwrap_or_else(|| panic!("{label}: 7-Zip refused it"));
    assert_eq!(ours, theirs, "{label} differs from 7-Zip");
}

#[test]
fn every_corpus_file_extracts_the_tree_7zip_extracts() {
    let recorded = recorded_trees();
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
        let ours = zipper_tree(archive, &root.path().join("zipper"))
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(
            ours, recorded[&name],
            "{name} differs from its recorded tree"
        );
        assert!(!ours.is_empty(), "{name} extracts nothing");
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, archive, &root.path().join("7zip"))
                .unwrap_or_else(|| panic!("7-Zip refused {name}"));
            assert_eq!(ours, theirs, "{name} differs from 7-Zip");
        }
    }
}

const NAME_CHARACTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789._-!#~ /";

fn random_name(random: &mut Random, taken: &mut Vec<String>) -> Vec<u16> {
    loop {
        let text: String = (0..1 + random.below(20))
            .map(|_| char::from(NAME_CHARACTERS[random.below(NAME_CHARACTERS.len())]))
            .collect();
        let (units, decoded): (Vec<u16>, String) = match random.below(6) {
            0 => (cfb_writer::msi_name(&text, false), text),
            1 => (cfb_writer::msi_name(&text, true), format!("!{text}")),
            2 => {
                let control = 1 + random.below(31) as u16;
                let units = [control].into_iter().chain(text.encode_utf16()).collect();
                (units, format!("[{control}]{text}"))
            }
            3 => {
                let text = format!("caf\u{e9} \u{2603}\u{1F600}{}", random.below(100));
                (text.encode_utf16().collect(), text)
            }
            _ => (text.encode_utf16().collect(), text),
        };
        let key = decoded.replace('/', "_").to_lowercase();
        if key != "." && key != ".." && !taken.contains(&key) {
            taken.push(key);
            return units;
        }
    }
}

fn random_data(random: &mut Random) -> Vec<u8> {
    const SIZES: [usize; 12] = [0, 1, 63, 64, 65, 511, 512, 4095, 4096, 4097, 9000, 70_000];
    let size = SIZES[random.below(SIZES.len())];
    (0..size).map(|_| random.next() as u8).collect()
}

fn random_children(random: &mut Random, depth: usize) -> Vec<Node> {
    let mut taken = Vec::new();
    let count = random.below(6);
    (0..count)
        .map(|_| {
            let name = random_name(random, &mut taken);
            if depth < 3 && random.below(4) == 0 {
                Node::Storage {
                    name,
                    children: random_children(random, depth + 1),
                    modified: 0x01D0_0000_0000_0000 + random.next() % (1 << 50),
                }
            } else {
                cfb_writer::raw_stream(name, &random_data(random))
            }
        })
        .collect()
}

fn small_cab(random: &mut Random) -> Vec<u8> {
    let text: Vec<u8> = (0..3000)
        .flat_map(|line| format!("installer line {line}\r\n").into_bytes())
        .collect();
    let noise: Vec<u8> = (0..5000).map(|_| random.next() as u8).collect();
    let files: [(&str, &[u8]); 3] = [
        ("readme.txt", b"hello from an embedded cab\r\n"),
        ("docs/manual.txt", &text),
        ("bin/tool.bin", &noise),
    ];
    let method = if random.below(2) == 0 {
        cab_writer::Method::Mszip
    } else {
        cab_writer::Method::Stored
    };
    cab_writer::one_folder_in_order(&files, &method).build()
}

fn installer(random: &mut Random, version: u16, cabs: usize) -> Compound {
    let mut children = vec![
        cfb_writer::raw_stream(cfb_writer::msi_name("_StringData", true), b"strings"),
        cfb_writer::raw_stream(cfb_writer::msi_name("_Tables", true), b"tables"),
        cfb_writer::raw_stream(
            [5u16]
                .into_iter()
                .chain("SummaryInformation".encode_utf16())
                .collect(),
            &[7; 300],
        ),
    ];
    for index in 0..cabs {
        children.push(cfb_writer::raw_stream(
            cfb_writer::msi_name(&format!("cab{index}.cab"), false),
            &small_cab(random),
        ));
    }
    Compound {
        version,
        children,
        root_modified: 0,
        scatter: (random.below(2) == 0).then(|| random.next()),
    }
}

#[test]
fn generated_compound_files_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = sevenzip() else {
        return;
    };
    let mut random = Random(0x5eed_c0de_0000_0016);
    let root = tempfile::tempdir().unwrap();
    for case in 0..160 {
        let compound = if case % 8 == 7 {
            installer(
                &mut random,
                if case % 16 == 7 { 3 } else { 4 },
                1 + case % 3,
            )
        } else {
            let children = random_children(&mut random, 0);
            Compound {
                version: if random.below(2) == 0 { 3 } else { 4 },
                children,
                root_modified: 0,
                scatter: (random.below(2) == 0).then(|| random.next()),
            }
        };
        let archive = root.path().join(format!("case{case}.msi"));
        fs::write(&archive, compound.build().bytes).unwrap();
        let scratch = root.path().join(format!("out{case}"));
        assert_same_as_7zip(
            &binary,
            &archive,
            &scratch,
            &format!("case {case}: {compound:?}"),
        );
    }
}

#[test]
fn a_compound_file_with_a_difat_extracts_as_7zip_extracts_it() {
    let Some(binary) = sevenzip() else {
        return;
    };
    let big: Vec<u8> = (0..7_400_000u32)
        .map(|index| (index * 7 + index / 513) as u8)
        .collect();
    let compound = Compound {
        version: 3,
        children: vec![
            cfb_writer::stream("big", &big),
            cfb_writer::stream("small", b"after the difat"),
        ],
        root_modified: 0,
        scatter: Some(99),
    };
    let built = compound.build();
    assert!(!built.difat_sectors.is_empty(), "the file needs a DIFAT");
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("difat.doc");
    fs::write(&archive, &built.bytes).unwrap();
    assert_same_as_7zip(&binary, &archive, root.path(), "difat");
}

#[test]
fn local_samples_extract_as_7zip_extracts_them() {
    let Some(folder) = std::env::var_os("ZIPPER_MSI_SAMPLES") else {
        return;
    };
    let binary = sevenzip().expect("ZIPPER_MSI_SAMPLES needs 7-Zip");
    let mut samples: Vec<PathBuf> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .collect();
    samples.sort();
    for sample in samples {
        let root = tempfile::tempdir().unwrap();
        let label = sample.display().to_string();
        let ours = zipper_tree(&sample, &root.path().join("zipper"));
        let theirs = sevenzip_tree(&binary, &sample, &root.path().join("7zip"));
        match (ours, theirs) {
            (Ok(ours), Some(theirs)) => assert_eq!(ours, theirs, "{label} differs from 7-Zip"),
            (Err(_), None) => {}
            (ours, theirs) => panic!(
                "{label}: zipper {:?}, 7-Zip {}",
                ours.map(|tree| tree.len()),
                if theirs.is_some() {
                    "extracted it"
                } else {
                    "failed"
                }
            ),
        }
    }
}

#[test]
#[ignore = "rewrites tests/data/msi-corpus; run with --ignored to regenerate"]
fn write_the_generated_corpus() {
    let binary = sevenzip().expect("7-Zip records the corpus trees");
    let mut random = Random(0x0c0f_fee0_0000_0016);
    let mut generated = vec![
        ("generated-v4-one-cab.msi", installer(&mut random, 4, 1)),
        ("generated-v3-two-cabs.msi", installer(&mut random, 3, 2)),
    ];
    let mut storages = Compound::new(
        4,
        vec![
            cfb_writer::stream("WordDocument", &[3; 5000]),
            cfb_writer::raw_stream(
                [1u16].into_iter().chain("CompObj".encode_utf16()).collect(),
                b"compobj",
            ),
            Node::Storage {
                name: cfb_writer::units("ObjectPool"),
                children: vec![Node::Storage {
                    name: cfb_writer::units("_1234567890"),
                    children: vec![cfb_writer::stream("Contents", &[9; 4500])],
                    modified: 0x01DA_0000_0000_0000,
                }],
                modified: 0x01D9_8000_0000_0000,
            },
        ],
    );
    storages.scatter = Some(7);
    generated.push(("generated-v4-storages.doc", storages));
    let mut recorded = recorded_trees();
    let root = tempfile::tempdir().unwrap();
    for (name, compound) in generated {
        fs::write(Path::new(CORPUS).join(name), compound.build().bytes).unwrap();
    }
    for archive in corpus_files() {
        let name = archive.file_name().unwrap().to_string_lossy().into_owned();
        let theirs = sevenzip_tree(&binary, &archive, &root.path().join(&name))
            .unwrap_or_else(|| panic!("7-Zip refused {name}"));
        recorded.insert(name, theirs);
    }
    fs::write(
        Path::new(CORPUS).join("trees.json"),
        serde_json::to_string_pretty(&recorded).unwrap() + "\n",
    )
    .unwrap();
}
