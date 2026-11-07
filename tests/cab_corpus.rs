//! Every CAB in `tests/data/cab-corpus` extracts to the tree 7-Zip extracted
//! when the corpus was written, and to the tree 7-Zip extracts now where it
//! is installed. Generated CABs of every method, LZX window, block mix and
//! E8 setting extract exactly as 7-Zip extracts them. Without 7-Zip the
//! differential test is skipped, unless `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/cab_writer.rs"]
mod cab_writer;
#[path = "common/lzx_encoder.rs"]
mod lzx_encoder;

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
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip is installed"
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
    let binary = sevenzip();
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
    let method = match case % 4 {
        0 => Method::Stored,
        1 => Method::Mszip,
        _ => Method::Lzx(lzx_encoder::Options::random(
            &mut || random.next(),
            window_bits,
        )),
    };
    let length = match random.below(4) {
        0 => random.below(100),
        1 => random.below(cab_writer::BLOCK * 3),
        2 => cab_writer::BLOCK * (1 + random.below(4)),
        _ => random.below(1 << (window_bits + 1)).min(3 << 20),
    };
    let tiny_blocks = matches!(&method, Method::Lzx(options) if options.longest_block < 2000);
    let length = if tiny_blocks { length.min(120) } else { length };
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
    if let Method::Lzx(options) = &method
        && options.reset_interval.is_some()
    {
        let mut options = options.clone();
        options.reset_interval = None;
        return generated_with(Method::Lzx(options), &mut data, files);
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
    let Some(binary) = sevenzip() else {
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

#[test]
fn lzx_block_edges_at_chunk_ends_extract_as_7zip_extracts_them() {
    use lzx_encoder::Kind::{Aligned, Uncompressed, Verbatim};
    let Some(binary) = sevenzip() else {
        eprintln!("skipped: no 7-Zip");
        return;
    };
    let mut random = Random(0x0123_4567_89ab_cdef);
    let data = sample(&mut random, 3 * cab_writer::BLOCK + 777);
    let chunk = cab_writer::BLOCK;
    let plans: Vec<Vec<(lzx_encoder::Kind, usize)>> = vec![
        vec![
            (Uncompressed, chunk - 1),
            (Uncompressed, 1),
            (Verbatim, usize::MAX),
        ],
        vec![
            (Verbatim, chunk - 1),
            (Uncompressed, 1),
            (Aligned, usize::MAX),
        ],
        vec![
            (Uncompressed, chunk + 1),
            (Verbatim, chunk - 1),
            (Uncompressed, usize::MAX),
        ],
        vec![
            (Uncompressed, 2 * chunk + 3),
            (Uncompressed, chunk - 3),
            (Uncompressed, usize::MAX),
        ],
        vec![
            (Aligned, chunk),
            (Uncompressed, chunk),
            (Verbatim, 0),
            (Verbatim, usize::MAX),
        ],
        vec![
            (Uncompressed, 0),
            (Uncompressed, 5),
            (Aligned, 0),
            (Verbatim, usize::MAX),
        ],
    ];
    let root = tempfile::tempdir().unwrap();
    let mut case = 0;
    for plan in plans {
        for padding_in_next_chunk in [false, true] {
            for translation in [None, Some(12_000_000)] {
                let mut options = lzx_encoder::Options::plain(15 + (case % 7) as u8);
                options.plan = plan.clone();
                options.padding_in_next_chunk = padding_in_next_chunk;
                options.translation = translation;
                let cabinet = cab_writer::simple(&[("a.bin", &data)], &Method::Lzx(options));
                let archive = root.path().join(format!("edge{case}.cab"));
                fs::write(&archive, cabinet.build()).unwrap();
                let theirs =
                    sevenzip_tree(&binary, &archive, &root.path().join(format!("7zip{case}")))
                        .unwrap_or_else(|| {
                            panic!(
                                "7-Zip refused plan {plan:?}, padding next {padding_in_next_chunk}"
                            )
                        });
                let ours =
                    zipper_tree(&archive, &root.path().join(format!("zipper{case}"))).unwrap();
                assert_eq!(
                    ours, theirs,
                    "plan {plan:?}, padding next {padding_in_next_chunk}"
                );
                case += 1;
            }
        }
    }
}

#[test]
#[ignore = "rewrites tests/data/cab-corpus; run with --ignored to regenerate"]
fn write_the_generated_corpus() {
    let binary = sevenzip().expect("7-Zip records the corpus trees");
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
            cab_writer::simple(&files, &Method::Stored),
        ),
        (
            "mszip.cab".into(),
            cab_writer::simple(&files, &Method::Mszip),
        ),
    ];
    for window_bits in 15..=21u8 {
        let mut options = lzx_encoder::Options::random(&mut || random.next(), window_bits);
        options.reset_interval = None;
        options.longest_block = 20_000;
        options.kinds = vec![
            lzx_encoder::Kind::Verbatim,
            lzx_encoder::Kind::Aligned,
            lzx_encoder::Kind::Uncompressed,
        ];
        options.translation = Some(12_000_000);
        cabinets.push((
            format!("lzx-{window_bits}.cab"),
            cab_writer::simple(&files, &Method::Lzx(options)),
        ));
    }
    let mut folders = cab_writer::simple(
        &[
            files[0],
            files[1],
            ("docs/caf\u{e9} \u{2603}.txt", b"snow\r\n"),
        ],
        &Method::Mszip,
    );
    folders.files[2].attributes = cab_writer::NAME_IS_UTF8;
    let second = cab_writer::simple(&files[2..], &Method::Lzx(lzx_encoder::Options::plain(16)));
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
