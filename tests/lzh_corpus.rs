//! Every LZH in `tests/data/lzh-corpus` extracts to the tree 7-Zip extracted
//! when the corpus was written, or is refused where the record says so, and
//! to the tree 7-Zip extracts now where it is installed. Generated LZH of
//! every header level and method extract exactly as 7-Zip extracts them,
//! and a damaged one is refused exactly when 7-Zip refuses it. Without 7-Zip
//! the differential tests are skipped, unless `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/lzh_encoder.rs"]
mod lzh_encoder;
#[path = "common/lzh_writer.rs"]
mod lzh_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use lzh_encoder::{Random, Shape, Token};
use lzh_writer::Entry;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_lzh::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/lzh-corpus");

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

fn make_readable(root: &Path) {
    #[cfg(unix)]
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        use std::os::unix::fs::PermissionsExt;
        if entry.file_type().is_dir() || entry.file_type().is_file() {
            let mode = entry.metadata().unwrap().permissions().mode() | 0o700;
            let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode));
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    make_readable(root);
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

fn zipper_tree(archive: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(archive, Some(into), ReadOptions::default())?;
    Ok(tree(into))
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y", "-tlzh"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

#[derive(Debug, PartialEq)]
struct Record {
    tree: Option<BTreeMap<String, String>>,
    zipper_tree: Option<BTreeMap<String, String>>,
    zipper: Option<String>,
}

fn tree_value(value: &serde_json::Value) -> Option<BTreeMap<String, String>> {
    value.as_object().map(|tree| {
        tree.iter()
            .map(|(path, kind)| (path.clone(), kind.as_str().unwrap().to_owned()))
            .collect()
    })
}

fn records() -> BTreeMap<String, Record> {
    let text = fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, record)| {
            let tree = tree_value(&record["tree"]);
            let zipper_tree = record.get("zipper_tree").and_then(tree_value);
            let zipper = record
                .get("zipper")
                .and_then(|reason| reason.as_str())
                .map(str::to_owned);
            (
                name.clone(),
                Record {
                    tree,
                    zipper_tree,
                    zipper,
                },
            )
        })
        .collect()
}

fn write_records(records: &BTreeMap<String, Record>) {
    let value: serde_json::Map<String, serde_json::Value> = records
        .iter()
        .map(|(name, record)| {
            let mut entry = serde_json::Map::new();
            entry.insert("tree".to_owned(), serde_json::json!(record.tree));
            if let Some(tree) = &record.zipper_tree {
                entry.insert("zipper_tree".to_owned(), serde_json::json!(tree));
            }
            if let Some(reason) = &record.zipper {
                entry.insert("zipper".to_owned(), serde_json::json!(reason));
            }
            (name.clone(), serde_json::Value::Object(entry))
        })
        .collect();
    fs::write(
        Path::new(CORPUS).join("trees.json"),
        serde_json::to_string_pretty(&value).unwrap() + "\n",
    )
    .unwrap();
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "lzh"))
        .collect();
    files.sort();
    files
}

#[test]
fn every_corpus_lzh_extracts_the_tree_7zip_extracts_or_the_refusal_its_record_gives() {
    let records = records();
    let files = corpus_files();
    assert_eq!(files.len(), records.len(), "every corpus file has a record");
    let binary = sevenzip();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let record = &records[&name];
        let work = tempfile::tempdir().unwrap();
        let ours = zipper_tree(&file, &work.path().join("zipper"));
        match &record.zipper {
            Some(reason) => {
                let error = format!("{:#}", ours.expect_err(&name));
                assert!(error.contains(reason.as_str()), "{name}: {error}");
                assert!(!work.path().join("zipper").exists(), "{name}");
            }
            None => assert_eq!(
                Some(ours.unwrap_or_else(|error| panic!("{name}: {error:#}"))),
                record.zipper_tree.clone().or(record.tree.clone()),
                "{name}"
            ),
        }
        if let Some(binary) = &binary {
            let now = sevenzip_tree(binary, &file, &work.path().join("7zip"));
            assert_eq!(now, record.tree, "{name}: 7-Zip now extracts another tree");
        }
    }
}

fn text(random: &mut Random, length: usize) -> Vec<u8> {
    let words = [
        "alpha ",
        "beta ",
        "gamma\n",
        "delta ",
        "lzh ",
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

pub fn sample(random: &mut Random, length: usize) -> Vec<u8> {
    match random.below(4) {
        0 => text(random, length),
        1 => (0..length).map(|_| random.next() as u8).collect(),
        2 => (0..length).map(|index| (index / 97) as u8).collect(),
        _ => {
            let mut out = text(random, length / 2);
            out.extend((0..length - out.len()).map(|_| random.next() as u8 & 0x0F));
            out
        }
    }
}

const METHODS: [u8; 5] = [b'0', b'4', b'5', b'6', b'7'];

fn generated(seed: u64) -> (Vec<u8>, BTreeMap<String, String>) {
    let mut random = Random(seed * 0x9E37_79B9 + 7);
    let level = random.below(3) as u8;
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
        let digit = METHODS[random.below(METHODS.len())];
        let folder = random.below(2) == 1;
        let name = if folder {
            format!("folder{index}/file{index}.bin")
        } else {
            format!("file{index}.bin")
        };
        if folder {
            entries.push(Entry::directory(level, &format!("folder{index}")));
            expected.insert(format!("folder{index}"), "dir".to_owned());
        }
        let entry = if digit == b'0' {
            Entry::stored(level, &name, &data)
        } else {
            Entry::compressed(
                level,
                &name,
                &data,
                lzh_writer::method(digit),
                random.next(),
            )
        };
        entries.push(entry);
        expected.insert(name, sha256(&data));
    }
    (lzh_writer::archive(&entries), expected)
}

#[test]
fn generated_lzh_extract_byte_for_byte_and_exactly_as_7zip_extracts_them() {
    let binary = sevenzip();
    for seed in 0..120 {
        let (archive, expected) = generated(seed);
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("case.lzh");
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

#[test]
fn level_3_headers_extract_byte_for_byte() {
    let mut random = Random(33);
    for digit in METHODS {
        let length = 5_000 + random.below(50_000);
        let data = sample(&mut random, length);
        let entry = if digit == b'0' {
            Entry::stored(3, "deep/folder/file.bin", &data)
        } else {
            Entry::compressed(
                3,
                "deep/folder/file.bin",
                &data,
                lzh_writer::method(digit),
                9,
            )
        };
        let archive = lzh_writer::archive(&[Entry::directory(3, "deep"), entry]);
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("case.lzh");
        fs::write(&path, &archive).unwrap();
        let ours = zipper_tree(&path, &work.path().join("out")).unwrap();
        assert_eq!(
            ours["deep/folder/file.bin"],
            sha256(&data),
            "-lh{}-",
            digit as char
        );
    }
}

fn token_streams(random: &mut Random) -> Vec<Vec<Token>> {
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
                let reach = decoded.min(if round % 3 == 0 { 8 } else { 1 << 16 });
                let distance = match random.below(4) {
                    0 => 0,
                    1 => random.below(reach.min(4)),
                    _ => random.below(reach),
                };
                tokens.push(Token::Match { length, distance });
                decoded += length;
            } else {
                tokens.push(Token::Literal(if round % 4 == 0 {
                    b'a'
                } else {
                    random.next() as u8
                }));
                decoded += 1;
            }
        }
        streams.push(tokens);
    }
    streams
}

fn compare_with_7zip(binary: &Path, label: &str, archive: &[u8]) -> bool {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("case.lzh");
    fs::write(&path, archive).unwrap();
    let ours = zipper_tree(&path, &work.path().join("zipper")).ok();
    let theirs = sevenzip_tree(binary, &path, &work.path().join("7zip"));
    assert_eq!(ours, theirs, "{label}");
    ours.is_some()
}

#[test]
fn token_streams_and_every_table_form_decode_as_7zip_decodes_them() {
    let Some(binary) = sevenzip() else { return };
    let mut random = Random(5);
    let mut accepted = [0; 2];
    for (index, tokens) in token_streams(&mut random).into_iter().enumerate() {
        for digit in [b'4', b'5', b'6', b'7'] {
            let method = lzh_writer::method(digit);
            let dictionary = lzh_writer::dictionary(method);
            let fits = tokens.iter().all(|token| match token {
                Token::Match { distance, .. } => *distance < dictionary,
                Token::Literal(_) => true,
            });
            let data = lzh_encoder::replay(&tokens);
            for single in [true, false] {
                let blocks: Vec<usize> = (0..8)
                    .map(|_| 1 + random.below(tokens.len().max(1)))
                    .collect();
                let packed = lzh_encoder::encode(
                    &tokens,
                    lzh_writer::position_count_bits(method),
                    &blocks,
                    Shape {
                        single_when_possible: single,
                    },
                );
                let entry = Entry {
                    method,
                    packed,
                    size: data.len() as u32,
                    crc: lzh_encoder::crc16(&data),
                    ..Entry::stored(1, "f", b"")
                };
                let label = format!(
                    "stream {index} -lh{}- single {single} fits {fits}",
                    digit as char
                );
                accepted[usize::from(compare_with_7zip(
                    &binary,
                    &label,
                    &lzh_writer::archive(&[entry]),
                ))] += 1;
            }
        }
    }
    assert!(
        accepted[0] > 20 && accepted[1] > 100,
        "refused and accepted: {accepted:?}"
    );
}

#[test]
fn damaged_lzh_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = sevenzip() else { return };
    let mut random = Random(77);
    let mut accepted = [0; 2];
    for round in 0..300 {
        let length = 1 + random.below(30_000);
        let data = sample(&mut random, length);
        let digit = [b'4', b'5', b'6', b'7'][round % 4];
        let entry = Entry::compressed(1, "f", &data, lzh_writer::method(digit), random.next());
        let header = lzh_writer::entry_bytes(&Entry {
            packed: Vec::new(),
            ..entry.clone()
        })
        .len();
        let mut archive = lzh_writer::archive(std::slice::from_ref(&entry));
        let packed = entry.packed.len();
        match round % 3 {
            0 => {
                let at = header + random.below(packed);
                archive[at] ^= 1 << random.below(8);
            }
            1 => {
                let mut changed = entry.clone();
                changed.size = (entry.size as usize + random.below(5)).saturating_sub(2) as u32;
                archive = lzh_writer::archive(&[changed]);
            }
            _ => {
                let mut changed = entry.clone();
                let cut = random.below(4);
                changed.packed.truncate(packed.saturating_sub(cut));
                changed
                    .packed
                    .extend(std::iter::repeat_n(0, random.below(3)));
                archive = lzh_writer::archive(&[changed]);
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

#[test]
#[ignore = "writes the lha corpus; needs LHa for UNIX in ZIPPER_LHA and 7-Zip"]
fn write_the_lha_corpus() {
    let lha = PathBuf::from(std::env::var_os("ZIPPER_LHA").expect("ZIPPER_LHA"));
    let binary = sevenzip().expect("7-Zip");
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir_all(source.join("dir/sub")).unwrap();
    let mut random = Random(2026);
    fs::write(source.join("hello.txt"), b"hello from the LZH corpus\n").unwrap();
    fs::write(source.join("dir/empty"), b"").unwrap();
    fs::write(source.join("dir/text.txt"), text(&mut random, 60_000)).unwrap();
    fs::write(
        source.join("dir/sub/noise.bin"),
        sample(&mut random, 12_000),
    )
    .unwrap();
    fs::write(source.join("dir/sub/runs.bin"), vec![7u8; 70_000]).unwrap();
    let mut records = if Path::new(CORPUS).join("trees.json").exists() {
        records()
    } else {
        BTreeMap::new()
    };
    for level in ["0", "1", "2"] {
        for method in ["z", "o5", "o6", "o7"] {
            if method == "z" && level != "1" {
                continue;
            }
            let name = format!("lha-h{level}-{}.lzh", method.trim_start_matches('o'));
            let out = Path::new(CORPUS).join(&name);
            let _ = fs::remove_file(&out);
            let status = Command::new(&lha)
                .arg(format!("a{level}{method}q"))
                .arg(&out)
                .args(["hello.txt", "dir"])
                .current_dir(&source)
                .status()
                .unwrap();
            assert!(status.success(), "{name}");
            let tree = sevenzip_tree(&binary, &out, &work.path().join(&name));
            assert!(tree.is_some(), "7-Zip refuses {name}");
            records.insert(
                name,
                Record {
                    tree,
                    zipper_tree: None,
                    zipper: None,
                },
            );
        }
    }
    write_records(&records);
}

const LHASA_SAMPLES: [&str; 30] = [
    "explzh_723/h0_lh5.lzh",
    "explzh_723/h1_lh6.lzh",
    "explzh_723/h2_lh7.lzh",
    "explzh_723/h2_subdir.lzh",
    "lha213/lh5.lzh",
    "lha_amiga_122/lh4.lzh",
    "lha_amiga_122/lh0_dirs_bug.lzh",
    "lha_amiga_212/lh6.lzh",
    "lha_os2_208/h3_lh5.lzh",
    "lha_os2_208/h3_subdir.lzh",
    "lha_os2_208/lfn.lzh",
    "lha_os9_211c/h2_lh0.lzh",
    "lha_osk_201/h2_lh5.lzh",
    "lha_osk_201/h0_subdir.lzh",
    "lha_unix114i/h1_lh7.lzh",
    "lha_unix114i/h2_symlink2.lzh",
    "lha_unix114i/h1_symlink3.lzh",
    "lha_x68k_213/h1_lh5.lzh",
    "lharc_atari_313a/h1_lz5.lzh",
    "lhark04d/lh7.lzh",
    "lhmelt_16536/h2_subdir.lzh",
    "maclha_224/l2_lh5.lzh",
    "morphos_lha_2717/h2_huge.lzh",
    "regression/dotdot.lzh",
    "regression/symlink2.lzh",
    "regression/symlink3.lzh",
    "regression/truncated.lzh",
    "tascal_lha_051h/lh5.lzh",
    "unlha32/lh2.lzh",
    "lengths/lh1-1.lzh",
];

#[test]
#[ignore = "copies Lhasa's test archives and records both trees; needs ZIPPER_LHASA_ARCHIVES and 7-Zip"]
fn record_the_lhasa_samples() {
    let archives =
        PathBuf::from(std::env::var_os("ZIPPER_LHASA_ARCHIVES").expect("ZIPPER_LHASA_ARCHIVES"));
    let binary = sevenzip().expect("7-Zip");
    let mut records = records();
    for sample in LHASA_SAMPLES {
        let name = format!("lhasa-{}", sample.replace('/', "-"));
        let out = Path::new(CORPUS).join(&name);
        fs::copy(archives.join(sample), &out).unwrap();
        let work = tempfile::tempdir().unwrap();
        let tree = sevenzip_tree(&binary, &out, &work.path().join("7zip"));
        let (zipper_tree, zipper) = match zipper_tree(&out, &work.path().join("zipper")) {
            Ok(ours) if Some(&ours) == tree.as_ref() => (None, None),
            Ok(ours) => (Some(ours), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        println!(
            "{name}: 7-Zip {:?}, Zipper {:?} {:?}",
            tree.is_some(),
            zipper_tree,
            zipper
        );
        records.insert(
            name,
            Record {
                tree,
                zipper_tree,
                zipper,
            },
        );
    }
    write_records(&records);
}
