//! Every CAB in `tests/data/cab-corpus` extracts to the tree 7-Zip extracted
//! when the corpus was written, and to the tree 7-Zip extracts now where it
//! is installed. Generated stored, MSZIP and Quantum CABs of every Quantum
//! window extract exactly as 7-Zip extracts them, and a
//! damaged Quantum CAB is refused exactly when 7-Zip refuses it. Without 7-Zip 26.05 or later the
//! differential test is skipped, unless `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/cab_writer.rs"]
mod cab_writer;
#[path = "common/quantum_encoder.rs"]
mod quantum_encoder;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use cab_writer::{Cabinet, Method};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_cab::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/cab-corpus");

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
        .args(["x", "-y", "-tcab"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "cab"))
        .collect();
    files.sort();
    files
}

fn recorded_trees() -> BTreeMap<String, BTreeMap<String, String>> {
    serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
        .unwrap()
}

#[test]
fn every_corpus_cab_extracts_the_tree_7zip_extracts() {
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
    let binary = sevenzip::sevenzip();
    for archive in &files {
        let name = archive.file_name().unwrap().to_string_lossy().into_owned();
        let root = tempfile::tempdir().unwrap();
        let ours = zipper_tree(archive, &root.path().join("zipper")).unwrap();
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

fn sample(random: &mut Random, length: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(length);
    while data.len() < length {
        match random.below(6) {
            0 => data.extend((0..random.below(64)).map(|_| random.next() as u8)),
            1 if !data.is_empty() => {
                let back = 1 + random.below(data.len().min(3_000_000));
                for _ in 0..2 + random.below(400) {
                    data.push(data[data.len() - back]);
                }
            }
            2 => data.extend([0xE8, random.next() as u8, random.next() as u8, 0, 0]),
            3 => data.extend([0xE8, 0xFF, 0xFF, 0xFF, 0xFF, 0xE8]),
            4 => data.extend(std::iter::repeat_n(
                random.next() as u8 % 3,
                random.below(90),
            )),
            _ => data.extend_from_slice(b"CAB folders hold the files of a cabinet. "),
        }
    }
    data.truncate(length);
    data
}

fn generated(random: &mut Random, case: usize) -> (Cabinet, String) {
    let window_bits = 15 + (case % 7) as u8;
    let method = match case % 2 {
        0 => Method::Stored,
        _ => Method::Mszip,
    };
    let length = match random.below(4) {
        0 => random.below(100),
        1 => random.below(cab_writer::BLOCK * 3),
        2 => cab_writer::BLOCK * (1 + random.below(4)),
        _ => random.below(1 << (window_bits + 1)).min(3 << 20),
    };
    let mut data = sample(random, length.max(1));
    let mut files = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while start < data.len() {
        let size = (1 + random.below(data.len() / 3 + 1)).min(data.len() - start);
        files.push((format!("dir{}/file{index}.bin", index % 3), start, size));
        start += size;
        index += 1;
    }
    if random.below(3) == 0 {
        files.push(("empty.txt".to_owned(), data.len(), 0));
    }
    generated_with(method, &mut data, files)
}

fn generated_with(
    method: Method,
    data: &mut [u8],
    files: Vec<(String, usize, usize)>,
) -> (Cabinet, String) {
    let description = format!("{method:?}").chars().take(300).collect();
    let mut cabinet = Cabinet {
        folders: vec![cab_writer::folder(&method, data)],
        checksums: true,
        ..Cabinet::default()
    };
    for (name, start, size) in files {
        cabinet
            .files
            .push(cab_writer::file(&name, 0, start as u32, size as u32));
    }
    (cabinet, description)
}

#[test]
fn generated_cabs_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = sevenzip::sevenzip() else {
        eprintln!("skipped: no 7-Zip");
        return;
    };
    let mut random = Random(0x00c0_ffee_cab0_0001);
    let root = tempfile::tempdir().unwrap();
    for case in 0..400 {
        let (cabinet, description) = generated(&mut random, case);
        let archive = root.path().join(format!("case{case}.cab"));
        fs::write(&archive, cabinet.build()).unwrap();
        let theirs = sevenzip_tree(&binary, &archive, &root.path().join(format!("7zip{case}")))
            .unwrap_or_else(|| panic!("7-Zip refused case {case}: {description}"));
        let ours = zipper_tree(&archive, &root.path().join(format!("zipper{case}")))
            .unwrap_or_else(|error| panic!("Zipper refused case {case}: {error:#}; {description}"));
        assert_eq!(ours, theirs, "case {case}: {description}");
        fs::remove_file(&archive).unwrap();
        for side in ["7zip", "zipper"] {
            fs::remove_dir_all(root.path().join(format!("{side}{case}"))).unwrap();
        }
    }
}

fn files_over(random: &mut Random, length: usize) -> Vec<(String, usize, usize)> {
    let mut files = Vec::new();
    let mut start = 0;
    while start < length {
        let size = (1 + random.below(length / 3 + 1)).min(length - start);
        files.push((
            format!("dir{}/file{}.bin", files.len() % 3, files.len()),
            start,
            size,
        ));
        start += size;
    }
    files
}

#[test]
fn generated_quantum_cabs_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = sevenzip::sevenzip() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let mut random = Random(0x00c0_ffee_0a17_0001);
    let root = tempfile::tempdir().unwrap();
    for case in 0..220 {
        let window_bits = (case % 22) as u8;
        let options = quantum_encoder::Options::random(&mut || random.next(), window_bits);
        let window = 1usize << window_bits.max(15);
        let length = match random.below(4) {
            0 => 1 + random.below(100),
            1 => 1 + random.below(cab_writer::BLOCK * 3),
            2 => cab_writer::BLOCK * (1 + random.below(4)),
            _ => 1 + random.below(2 * window + cab_writer::BLOCK).min(3 << 20),
        };
        let mut data = sample(&mut random, length);
        let files = files_over(&mut random, data.len());
        let (cabinet, description) = generated_with(Method::Quantum(options), &mut data, files);
        let archive = root.path().join(format!("quantum{case}.cab"));
        fs::write(&archive, cabinet.build()).unwrap();
        let theirs = sevenzip_tree(&binary, &archive, &root.path().join(format!("7zip{case}")))
            .unwrap_or_else(|| panic!("7-Zip refused case {case}: {description}"));
        let ours = zipper_tree(&archive, &root.path().join(format!("zipper{case}")))
            .unwrap_or_else(|error| panic!("Zipper refused case {case}: {error:#}; {description}"));
        assert_eq!(ours, theirs, "case {case}: {description}");
        fs::remove_file(&archive).unwrap();
        for side in ["7zip", "zipper"] {
            fs::remove_dir_all(root.path().join(format!("{side}{case}"))).unwrap();
        }
    }
}

#[test]
fn damaged_quantum_cabs_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = sevenzip::sevenzip() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let mut random = Random(0x0bad_0a17_0000_0003);
    let root = tempfile::tempdir().unwrap();
    let (mut both_read, mut both_refused) = (0, 0);
    for case in 0..660 {
        let window_bits = (case % 22) as u8;
        let options = quantum_encoder::Options::random(&mut || random.next(), window_bits);
        let length = 1 + random.below(3 * cab_writer::BLOCK);
        let data = sample(&mut random, length);
        let mut cabinet =
            cab_writer::one_folder_in_order(&[("a.bin", &data)], &Method::Quantum(options));
        cabinet.checksums = false;
        let blocks = &mut cabinet.folders[0].blocks;
        let block = random.below(blocks.len());
        let packed = &mut blocks[block].packed;
        let damage = match random.below(5) {
            0 | 1 => {
                let at = random.below(packed.len());
                let bit = random.below(8);
                packed[at] ^= 1 << bit;
                format!("flip bit {bit} of byte {at}")
            }
            2 => {
                let at = random.below(packed.len());
                packed.truncate(at.max(1));
                format!("cut at {}", at.max(1))
            }
            3 => {
                let byte = [0, 0, 0xFF][random.below(3)];
                packed.push(byte);
                format!("append {byte:#x}")
            }
            _ => {
                let size = &mut blocks[block].size;
                let old = *size;
                *size = if old > 1 && random.below(2) == 0 {
                    old - 1
                } else {
                    (old + 1).min(cab_writer::BLOCK)
                };
                format!("size {old} to {size}")
            }
        };
        let archive = root.path().join(format!("damaged{case}.cab"));
        fs::write(&archive, cabinet.build()).unwrap();
        let theirs = sevenzip_tree(&binary, &archive, &root.path().join(format!("7zip{case}")));
        let ours = zipper_tree(&archive, &root.path().join(format!("zipper{case}")));
        match (&ours, &theirs) {
            (Ok(ours), Some(theirs)) => {
                assert_eq!(ours, theirs, "case {case}: block {block}, {damage}");
                both_read += 1;
            }
            (Err(_), None) => both_refused += 1,
            _ => panic!(
                "case {case}: block {block}, {damage}: Zipper {:?}, 7-Zip read it: {}",
                ours.as_ref()
                    .map(|_| ())
                    .map_err(|error| format!("{error:#}")),
                theirs.is_some()
            ),
        }
        fs::remove_file(&archive).unwrap();
        for side in ["7zip", "zipper"] {
            let _ = fs::remove_dir_all(root.path().join(format!("{side}{case}")));
        }
    }
    assert!(
        both_read > 0 && both_refused > 0,
        "{both_read} read, {both_refused} refused"
    );
}

#[test]
#[ignore = "rewrites tests/data/cab-corpus; run with --ignored to regenerate"]
fn write_the_generated_corpus() {
    let binary = sevenzip::sevenzip().expect("7-Zip records the corpus trees");
    let mut random = Random(0x0c0f_fee0_0000_0042);
    let text: Vec<u8> = (0..2500)
        .flat_map(|line| format!("line {line} of a cabinet corpus file\r\n").into_bytes())
        .collect();
    let mut binary_data = sample(&mut random, 40_000);
    binary_data.extend_from_slice(&text);
    let files: [(&str, &[u8]); 4] = [
        ("readme.txt", b"hello from the CAB corpus\r\n"),
        ("docs/manual.txt", &text),
        ("bin/tool.exe", &binary_data),
        ("docs/empty.txt", b""),
    ];
    let mut cabinets: Vec<(String, Cabinet)> = vec![
        (
            "stored.cab".into(),
            cab_writer::one_folder_in_order(&files, &Method::Stored),
        ),
        (
            "mszip.cab".into(),
            cab_writer::one_folder_in_order(&files, &Method::Mszip),
        ),
    ];
    let mut folders = cab_writer::one_folder_in_order(
        &[
            files[0],
            files[1],
            ("docs/caf\u{e9} \u{2603}.txt", b"snow\r\n"),
        ],
        &Method::Mszip,
    );
    folders.files[2].attributes = cab_writer::NAME_IS_UTF8;
    let second = cab_writer::one_folder_in_order(
        &files[2..],
        &Method::Quantum(quantum_encoder::Options::plain(16)),
    );
    folders.folders.extend(second.folders);
    folders
        .files
        .extend(second.files.into_iter().map(|mut file| {
            file.folder = 1;
            file
        }));
    folders.files[0].attributes |= cab_writer::READ_ONLY;
    folders.reserve = Some((vec![7; 20], vec![1, 2, 3], vec![9; 5]));
    cabinets.push(("folders-and-reserves.cab".into(), folders));
    let mut quantum_random = Random(0x0c0f_fee0_0a17_0042);
    for window_bits in 10..=21u8 {
        let mut options =
            quantum_encoder::Options::random(&mut || quantum_random.next(), window_bits);
        options.literal_odds = options.literal_odds.min(5);
        cabinets.push((
            format!("quantum-{window_bits}.cab"),
            cab_writer::one_folder_in_order(&files, &Method::Quantum(options)),
        ));
    }
    let mut recorded = recorded_trees();
    let root = tempfile::tempdir().unwrap();
    for (name, cabinet) in cabinets {
        let archive = Path::new(CORPUS).join(&name);
        fs::write(&archive, cabinet.build()).unwrap();
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
