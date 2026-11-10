//! Every ARJ in `tests/data/arj-corpus` extracts to the tree 7-Zip extracted
//! when the corpus was written, and to the tree 7-Zip extracts now where it
//! is installed. Generated ARJ of every method extract exactly as 7-Zip extracts them,
//! and a damaged one is refused exactly when 7-Zip refuses it. Without 7-Zip
//! 26.05 or later the differential tests are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/arj_writer.rs"]
mod arj_writer;
#[path = "common/lzh_encoder.rs"]
mod lzh_encoder;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use arj_writer::Entry;
use lzh_encoder::{Random, Shape, Token};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_arj::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/arj-corpus");

fn sevenzip() -> Option<PathBuf> {
    sevenzip::sevenzip()
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
    crate_readable(into);
    Ok(tree(into))
}

fn crate_readable(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        let mode = entry.metadata().unwrap().permissions().mode() | 0o600;
        let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = root;
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y", "-tarj"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

fn text(random: &mut Random, length: usize) -> Vec<u8> {
    let words = [
        "arj ",
        "method ",
        "four\r\n",
        "huffman ",
        "x",
        "  ",
        "0123456789",
    ];
    let mut out = Vec::with_capacity(length);
    while out.len() < length {
        out.extend_from_slice(words[random.below(words.len())].as_bytes());
    }
    out.truncate(length);
    out
}

fn sample(random: &mut Random, length: usize) -> Vec<u8> {
    match random.below(4) {
        0 => text(random, length),
        1 => (0..length).map(|_| random.next() as u8).collect(),
        2 => (0..length).map(|index| (index / 61) as u8).collect(),
        _ => {
            let mut out = text(random, length / 2);
            out.extend((0..length - out.len()).map(|_| random.next() as u8 & 0x07));
            out
        }
    }
}

fn generated(seed: u64) -> (Vec<u8>, BTreeMap<String, String>) {
    let mut random = Random(seed * 0x9E37_79B9 + 11);
    let mut entries = Vec::new();
    let mut expected = BTreeMap::new();
    for index in 0..1 + random.below(4) {
        let length = match random.below(4) {
            0 => random.below(4),
            1 => random.below(300),
            2 => random.below(20_000),
            _ => 60_000 + random.below(140_000),
        };
        let data = sample(&mut random, length);
        let method = random.below(5) as u8;
        let folder = random.below(2) == 1;
        let name = if folder {
            format!("folder{index}/file{index}.bin")
        } else {
            format!("file{index}.bin")
        };
        if folder {
            entries.push(Entry::directory(&format!("folder{index}")));
            expected.insert(format!("folder{index}"), "dir".to_owned());
        }
        let mut entry = if method == 0 {
            Entry::stored(&name, &data)
        } else {
            Entry::compressed(&name, &data, method, random.next())
        };
        if random.below(3) == 0 {
            entry.kind = arj_writer::TEXT;
        }
        if random.below(4) == 0 {
            entry.first_size = 34;
            entry.comment = b"a comment".to_vec();
            entry.extended = vec![b"extended data".to_vec()];
        }
        entries.push(entry);
        expected.insert(name, sha256(&data));
    }
    (arj_writer::archive(&entries), expected)
}

#[test]
fn generated_arj_extract_byte_for_byte_and_exactly_as_7zip_extracts_them() {
    let binary = sevenzip();
    for seed in 0..120 {
        let (archive, expected) = generated(seed);
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("case.arj");
        fs::write(&path, &archive).unwrap();
        let ours = zipper_tree(&path, &work.path().join("zipper"))
            .unwrap_or_else(|error| panic!("seed {seed}: {error:#}"));
        assert_eq!(ours, expected, "seed {seed}");
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, &path, &work.path().join("7zip"));
            assert_eq!(theirs.as_ref(), Some(&expected), "seed {seed}: 7-Zip");
        }
    }
}

fn compare_with_7zip(binary: &Path, label: &str, archive: &[u8]) -> bool {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("case.arj");
    fs::write(&path, archive).unwrap();
    let ours = zipper_tree(&path, &work.path().join("zipper")).ok();
    let theirs = sevenzip_tree(binary, &path, &work.path().join("7zip"));
    assert_eq!(ours, theirs, "{label}");
    ours.is_some()
}

fn token_streams(random: &mut Random, longest_reach: usize) -> Vec<Vec<Token>> {
    let mut streams = Vec::new();
    for round in 0..40 {
        let mut tokens = Vec::new();
        let mut decoded = 0usize;
        for _ in 0..1 + random.below(if round < 10 { 5 } else { 3000 }) {
            if decoded > 0 && random.below(3) != 0 {
                let longest = if round % 5 == 0 {
                    256
                } else {
                    3 + random.below(40)
                };
                let length = 3 + random.below(longest - 2);
                let reach = (decoded + 2).min(if round % 3 == 0 { 8 } else { longest_reach });
                let distance = match random.below(4) {
                    0 => 0,
                    1 => random.below(reach.min(4)),
                    _ => random.below(reach),
                };
                tokens.push(Token::Match { length, distance });
                decoded += length;
            } else {
                tokens.push(Token::Literal(random.next() as u8));
                decoded += 1;
            }
        }
        streams.push(tokens);
    }
    streams
}

fn replay_or_garbage(tokens: &[Token]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for token in tokens {
        match *token {
            Token::Literal(byte) => out.push(byte),
            Token::Match { length, distance } => {
                for _ in 0..length {
                    let byte = out.len().checked_sub(distance + 1).map_or(0, |at| out[at]);
                    out.push(byte);
                }
            }
        }
    }
    out
}

#[test]
fn token_streams_of_every_method_decode_as_7zip_decodes_them() {
    let Some(binary) = sevenzip() else { return };
    let mut random = Random(9);
    let mut accepted = [0; 2];
    for method in 1..=4u8 {
        let reach = if method == 4 { 15_872 } else { 30_000 };
        for (index, tokens) in token_streams(&mut random, reach).into_iter().enumerate() {
            let data = replay_or_garbage(&tokens);
            for single in [true, false] {
                if method == 4 && !single {
                    continue;
                }
                let packed = if method == 4 {
                    lzh_encoder::encode_fastest(&tokens)
                } else {
                    let blocks: Vec<usize> =
                        (0..8).map(|_| 1 + random.below(tokens.len())).collect();
                    lzh_encoder::encode(
                        &tokens,
                        5,
                        &blocks,
                        Shape {
                            single_when_possible: single,
                        },
                    )
                };
                let entry = Entry {
                    method,
                    packed,
                    size: data.len() as u32,
                    crc: crc32fast::hash(&data),
                    ..Entry::stored("f", b"")
                };
                let label = format!("method {method} stream {index} single {single}");
                accepted[usize::from(compare_with_7zip(
                    &binary,
                    &label,
                    &arj_writer::archive(&[entry]),
                ))] += 1;
            }
        }
    }
    assert!(
        accepted[0] > 50 && accepted[1] > 50,
        "refused and accepted: {accepted:?}"
    );
}

#[test]
fn damaged_arj_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = sevenzip() else { return };
    let mut random = Random(78);
    let mut accepted = [0; 2];
    for round in 0..300 {
        let length = 1 + random.below(30_000);
        let data = sample(&mut random, length);
        let method = 1 + (round % 4) as u8;
        let entry = Entry::compressed("f", &data, method, random.next());
        let header = arj_writer::main_header(0x10).len() + arj_writer::entry_header(&entry).len();
        let mut archive = arj_writer::archive(std::slice::from_ref(&entry));
        match round % 3 {
            0 => {
                let at = header + random.below(entry.packed.len());
                archive[at] ^= 1 << random.below(8);
            }
            1 => {
                let mut changed = entry.clone();
                changed.size = (entry.size as usize + random.below(5)).saturating_sub(2) as u32;
                changed.crc = crc32fast::hash(&data[..data.len().min(changed.size as usize)]);
                archive = arj_writer::archive(&[changed]);
            }
            _ => {
                let mut changed = entry.clone();
                let cut = random.below(4);
                changed
                    .packed
                    .truncate(changed.packed.len().saturating_sub(cut));
                changed
                    .packed
                    .extend(std::iter::repeat_n(0, random.below(3)));
                archive = arj_writer::archive(&[changed]);
            }
        }
        accepted[usize::from(compare_with_7zip(
            &binary,
            &format!("round {round}"),
            &archive,
        ))] += 1;
    }
    assert!(
        accepted[0] > 100 && accepted[1] > 10,
        "refused and accepted: {accepted:?}"
    );
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "arj"))
        .collect();
    files.sort();
    files
}

fn recorded_trees() -> BTreeMap<String, BTreeMap<String, String>> {
    let text = fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, tree)| {
            let tree = tree
                .as_object()
                .unwrap()
                .iter()
                .map(|(path, kind)| (path.clone(), kind.as_str().unwrap().to_owned()))
                .collect();
            (name.clone(), tree)
        })
        .collect()
}

#[test]
fn every_corpus_arj_extracts_the_tree_7zip_extracts() {
    let recorded = recorded_trees();
    let files = corpus_files();
    assert_eq!(files.len(), recorded.len(), "every corpus file has a tree");
    let binary = sevenzip();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let ours = zipper_tree(&file, &work.path().join("zipper"))
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(ours, recorded[&name], "{name}");
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, &file, &work.path().join("7zip"));
            assert_eq!(theirs.as_ref(), Some(&recorded[&name]), "{name}: 7-Zip");
        }
    }
}

#[test]
#[ignore = "writes the ARJ corpus with the test writer; needs 7-Zip"]
fn write_the_arj_corpus() {
    let binary = sevenzip().expect("7-Zip");
    let mut random = Random(2026);
    let hello = b"hello from the ARJ corpus\r\n".to_vec();
    let text_file = text(&mut random, 40_000);
    let noise: Vec<u8> = (0..6_000).map(|_| random.next() as u8).collect();
    let mut trees = serde_json::Map::new();
    for method in 0..=4u8 {
        let mut entries = vec![Entry::directory("folder"), Entry::directory("folder/sub")];
        let mut add = |name: &str, data: &[u8], seed: u64| {
            entries.push(if method == 0 {
                Entry::stored(name, data)
            } else {
                Entry::compressed(name, data, method, seed)
            })
        };
        add("hello.txt", &hello, 1);
        add("folder/text.txt", &text_file, 2);
        add("folder/sub/noise.bin", &noise, 3);
        add("folder/empty", b"", 4);
        let name = format!("method-{method}.arj");
        let path = Path::new(CORPUS).join(&name);
        fs::write(&path, arj_writer::archive(&entries)).unwrap();
        let work = tempfile::tempdir().unwrap();
        let tree = sevenzip_tree(&binary, &path, work.path()).expect("7-Zip reads the corpus");
        trees.insert(name, serde_json::json!(tree));
    }
    fs::write(
        Path::new(CORPUS).join("trees.json"),
        serde_json::to_string_pretty(&trees).unwrap() + "\n",
    )
    .unwrap();
}
