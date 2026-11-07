//! The single-stream formats 7-Zip reads that Zipper reads in process:
//! Microsoft `compress.exe` SZDD files, `lzma86` files, PPMd `.pmd` files and
//! Base64 `.b64` files.
//! Generated files of each extract to the tree `7zz x` extracts, through the
//! default engine, and damaged files are refused exactly when 7-Zip refuses
//! them, apart from the checks this reader adds. Every cut, except a Base64
//! cut after a whole quad, trailing data and an output cap are refused with
//! nothing published. Without 7-Zip
//! 26 or later the differential tests are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/legacy_stream_writers.rs"]
mod writers;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use writers::{Token, Variant};
use zipper::native_stream::{Format, ReadOptions, read_archive, sniff};

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

    fn bytes(&mut self, length: usize, alphabet: usize) -> Vec<u8> {
        (0..length).map(|_| self.below(alphabet) as u8).collect()
    }
}

fn binary() -> Option<PathBuf> {
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
                .into_owned();
            let kind = if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                sha256(&fs::read(entry.path()).unwrap())
            };
            (name, kind)
        })
        .collect()
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg("--")
        .arg(archive)
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

fn zipper_tree(archive: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    zipper::extract_archive(archive, into, zipper::ExtractOptions::default())?;
    Ok(tree(into))
}

struct Case {
    name: String,
    bytes: Vec<u8>,
    description: String,
}

/// Checks each case against 7-Zip. `allowed` names the refusals this reader
/// adds to 7-Zip's: a case 7-Zip extracts may be refused only with one of them.
fn assert_matches_7zip(binary: &Path, cases: &[Case], allowed: &[&str]) -> (usize, usize) {
    let root = tempfile::tempdir().unwrap();
    let (mut extracted, mut refused) = (0, 0);
    for (number, case) in cases.iter().enumerate() {
        let folder = root.path().join(format!("case{number}"));
        fs::create_dir(&folder).unwrap();
        let archive = folder.join(&case.name);
        fs::write(&archive, &case.bytes).unwrap();
        let theirs = sevenzip_tree(binary, &archive, &folder.join("7zip"));
        let ours = zipper_tree(&archive, &folder.join("zipper"));
        match (theirs, ours) {
            (Some(theirs), Ok(ours)) => {
                assert_eq!(ours, theirs, "{}: {}", case.name, case.description);
                extracted += 1;
            }
            (None, Err(_)) => {
                assert!(
                    !folder.join("zipper").exists(),
                    "{}: a refused file left output",
                    case.name
                );
                refused += 1;
            }
            (None, Ok(ours)) => panic!(
                "{}: 7-Zip refused it and Zipper extracted {ours:?}; {}",
                case.name, case.description
            ),
            (Some(_), Err(error)) => {
                let message = format!("{error:#}");
                assert!(
                    allowed.iter().any(|allowed| message.contains(allowed)),
                    "{}: 7-Zip extracted it and Zipper refused it: {message}; {}",
                    case.name,
                    case.description
                );
                assert!(!folder.join("zipper").exists());
                refused += 1;
            }
        }
        common_cleanup(&folder);
    }
    eprintln!("{extracted} extracted and {refused} refused as 7-Zip did");
    (extracted, refused)
}

fn common_cleanup(folder: &Path) {
    fs::remove_dir_all(folder).unwrap();
}

fn payload(random: &mut Random, case: usize) -> Vec<u8> {
    let length = match case % 5 {
        0 => random.below(40),
        1 => 4090 + random.below(20),
        2 => random.below(70_000),
        _ => random.below(5000),
    };
    match case % 4 {
        0 => random.bytes(length, 256),
        1 => random.bytes(length, 3),
        2 => b"the quick brown fox jumps over the lazy dog. "
            .iter()
            .cycle()
            .take(length)
            .copied()
            .collect(),
        _ => (0..length)
            .map(|at| match random.below(8) {
                0 => 0xe8,
                1 => 0xe9,
                2 => 0xff,
                3 => 0,
                _ => (at % 251) as u8,
            })
            .collect(),
    }
}

const SZDD_NAMES: [&str; 8] = [
    "SETUP.EX_",
    "driver.sy_",
    "lib.dl_",
    "Data.bi_",
    "plain.bin",
    "noext",
    "notes.tx_",
    "_",
];
const SZDD_CHARACTERS: [u8; 8] = [0, 0, b'E', b'.', b'Q', 0x7f, 0x80, b' '];

fn szdd_window_tokens(random: &mut Random) -> (Vec<Token>, u32) {
    let mut tokens = Vec::new();
    let mut size = 0u32;
    for _ in 0..random.below(40) {
        if random.below(3) == 0 {
            tokens.push(Token::Literal(random.below(256) as u8));
            size += 1;
        } else {
            let length = 3 + random.below(14) as u8;
            let slot = match random.below(3) {
                0 => 4080 + random.below(16) as u16,
                1 => random.below(32) as u16,
                _ => random.below(4096) as u16,
            };
            tokens.push(Token::Copy { slot, length });
            size += u32::from(length);
        }
    }
    (tokens, size)
}

fn szdd_cases(random: &mut Random) -> Vec<Case> {
    let mut cases = Vec::new();
    for case in 0..160 {
        let name = SZDD_NAMES[case % SZDD_NAMES.len()];
        let character = SZDD_CHARACTERS[random.below(SZDD_CHARACTERS.len())];
        let (bytes, description) = if case % 3 == 0 {
            let (tokens, size) = szdd_window_tokens(random);
            let mut bytes = writers::szdd_header(character, size);
            bytes.extend(writers::szdd_body(&tokens, 0));
            (bytes, format!("tokens {tokens:?}"))
        } else {
            let data = payload(random, case);
            (
                writers::szdd(&data, character),
                format!("{} bytes", data.len()),
            )
        };
        cases.push(Case {
            name: format!("c{case}{name}"),
            bytes,
            description: format!("{description}, character {character:#x}"),
        });
    }
    cases
}

#[test]
fn generated_szdd_files_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let cases = szdd_cases(&mut Random(0x5a5a_d0d0_0001));
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[]);
    assert_eq!((extracted, refused), (cases.len(), 0));
}

const SPARE_FLAGS: &str = "flag bits after its last token";

#[test]
fn damaged_szdd_files_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let mut random = Random(0x5a5a_d0d0_0002);
    let mut cases = Vec::new();
    for (number, case) in szdd_cases(&mut random).into_iter().enumerate().take(120) {
        let mut bytes = case.bytes;
        let at = 9 + random.below(bytes.len() - 9);
        let flip = 1 << random.below(8);
        bytes[at] ^= flip;
        if number % 4 == 0 {
            let extra = 1 + random.below(3);
            bytes.extend(random.bytes(extra, 256));
        }
        cases.push(Case {
            name: case.name,
            bytes,
            description: format!("byte {at} ^ {flip:#x}, {}", case.description),
        });
    }
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[SPARE_FLAGS]);
    assert!(
        extracted > 10 && refused > 10,
        "{extracted} extracted, {refused} refused"
    );
}

#[test]
fn szdd_names_follow_7zip_and_stay_safe() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let mut cases = Vec::new();
    for (name, character) in [
        ("SETUP.EX_", 0),
        ("setup.ex_", 0),
        ("setup.Ex_", 0),
        ("a.kmd_", 0),
        ("a.km_", 0),
        ("a.pd_", 0),
        ("a.zz_", 0),
        ("a.zz_", b'/'),
        ("a.zz_", b'*'),
        ("a.zz_", 0x1f),
        ("_", 0x1f),
        ("_", b'Q'),
        ("space _", 0),
        (".._", b'.'),
        ("x._", b'.'),
        ("archive.mslz", b'x'),
    ] {
        cases.push(Case {
            name: name.to_owned(),
            bytes: writers::szdd(b"abc", character),
            description: format!("character {character:#x}"),
        });
    }
    let (extracted, _) = assert_matches_7zip(&binary, &cases, &[]);
    assert_eq!(extracted, cases.len());
}

#[test]
fn szdd_names_turn_backslashes_and_colons_into_underscores() {
    for (name, character, expected) in [
        ("a\\b.ex_", 0u8, "a_b.exe"),
        ("a.zz_", b'\\', "a.zz_~"),
        ("a.zz_", b':', "a.zz_~"),
        ("a.zz_", b'_', "a.zz_~"),
        ("A.ZZ_", b'_', "A.ZZ_~"),
        ("a:b_", 0, "a_b"),
    ] {
        if cfg!(windows) && name.contains(['\\', ':']) {
            continue;
        }
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(name);
        fs::write(&archive, writers::szdd(b"abc", character)).unwrap();
        let entries = read_archive(
            &archive,
            Some(&root.path().join("out")),
            ReadOptions::default(),
        )
        .unwrap();
        assert_eq!(entries[0].name, expected, "{name} with {character:#x}");
        assert_eq!(
            fs::read(root.path().join("out").join(expected)).unwrap(),
            b"abc"
        );
    }
}

#[test]
fn a_decoded_file_never_replaces_its_own_archive_in_the_archives_folder() {
    let cases = [
        ("self.ex_", writers::szdd(b"decoded", b':')),
        ("self.ex_", writers::szdd(b"decoded", b'_')),
        (
            "self.pmd",
            writers::pmd(b"decoded", Variant::H, 4, 1, b"self.pmd"),
        ),
        (
            "self.pmd",
            writers::pmd(b"decoded", Variant::H, 4, 1, b"SELF.PMD"),
        ),
    ];
    for (name, bytes) in cases {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(name);
        fs::write(&archive, &bytes).unwrap();
        let result = zipper::extract_archive(
            &archive,
            root.path(),
            zipper::ExtractOptions {
                existing: zipper::Existing::Overwrite,
                ..Default::default()
            },
        );
        assert_eq!(fs::read(&archive).unwrap(), bytes, "{name}: {result:?}");
    }
}

fn lzma86_cases(binary: &Path, scratch: &Path, random: &mut Random) -> Vec<Case> {
    let mut cases = Vec::new();
    for case in 0..48 {
        let data = payload(random, case);
        let bcj = case % 2 == 0;
        let name = ["c{}.lzma86", "c{}.bin", "c{}"][case % 3].replace("{}", &case.to_string());
        let (bytes, description) = match case % 4 {
            0 | 1 => match writers::lzma86_from_7zip(binary, scratch, &data, bcj) {
                Some(bytes) => (bytes, "7-Zip's LZMA without an end marker"),
                None => (
                    writers::lzma86_with_end_marker(&data, bcj, true, 3, 0, 2),
                    "liblzma with an end marker",
                ),
            },
            2 => (
                writers::lzma86_with_end_marker(&data, bcj, true, (case % 5) as u32, 0, 2),
                "liblzma with an end marker and a size",
            ),
            _ => (
                writers::lzma86_with_end_marker(&data, bcj, false, 0, 2, 0),
                "liblzma with an end marker and no size",
            ),
        };
        cases.push(Case {
            name,
            bytes,
            description: format!("{description}, BCJ {bcj}, {} bytes", data.len()),
        });
    }
    let first = writers::lzma86_with_end_marker(&payload(random, 2), true, true, 3, 0, 2);
    let second = writers::lzma86_with_end_marker(&vec![7; 9000], false, false, 3, 0, 2);
    cases.push(Case {
        name: "two.lzma86".to_owned(),
        bytes: [first, second].concat(),
        description: "two streams".to_owned(),
    });
    cases
}

#[test]
fn generated_lzma86_files_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let cases = lzma86_cases(&binary, scratch.path(), &mut Random(0x1a86_0001));
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[]);
    assert_eq!((extracted, refused), (cases.len(), 0));
}

const LATER_STREAM: &str = "brings fewer than";

#[test]
fn damaged_lzma86_files_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let mut random = Random(0x1a86_0002);
    let mut cases = Vec::new();
    for case in lzma86_cases(&binary, scratch.path(), &mut random) {
        let mut bytes = case.bytes;
        if bytes.len() <= 16 {
            continue;
        }
        let at = 16 + random.below(bytes.len() - 16);
        bytes[at] ^= 1 << random.below(8);
        cases.push(Case {
            name: case.name.replace(".bin", ".lzma86"),
            bytes,
            description: format!("byte {at} flipped, {}", case.description),
        });
    }
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[LATER_STREAM]);
    assert!(refused > 10, "{extracted} extracted, {refused} refused");
}

fn pmd_cases(random: &mut Random) -> Vec<Case> {
    let mut cases = Vec::new();
    for case in 0..40 {
        let data = payload(random, case);
        let variant = match case % 3 {
            0 => Variant::H,
            1 => Variant::I { restore: 0 },
            _ => Variant::I { restore: 1 },
        };
        let order = 2 + random.below(15) as u16;
        let memory = 1 + random.below(8) as u16;
        let stored = [
            &b"inner.txt"[..],
            b"",
            b"folder/inner.bin",
            b"../up.txt",
            b"/root.txt",
            b"name\0ignored",
            b"..",
            b"a/./b/",
        ][case % 8];
        cases.push(Case {
            name: format!("c{case}.pmd"),
            bytes: writers::pmd(&data, variant, order, memory, stored),
            description: format!(
                "{variant:?}, order {order}, {memory} MiB, name {:?}, {} bytes",
                String::from_utf8_lossy(stored),
                data.len()
            ),
        });
    }
    cases
}

#[test]
fn generated_pmd_files_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let cases = pmd_cases(&mut Random(0x00dd_0001));
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[]);
    assert_eq!((extracted, refused), (cases.len(), 0));
}

const PMD_TRAILING: &str = "data after its end marker";

#[test]
fn damaged_pmd_files_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let mut random = Random(0x00dd_0002);
    let mut cases = Vec::new();
    for case in pmd_cases(&mut random) {
        let mut bytes = case.bytes;
        let at = 8 + random.below(bytes.len() - 8);
        bytes[at] ^= 1 << random.below(8);
        cases.push(Case {
            name: case.name,
            bytes,
            description: format!("byte {at} flipped, {}", case.description),
        });
    }
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[PMD_TRAILING]);
    assert!(refused > 5, "{extracted} extracted, {refused} refused");
}

const LINE_ENDS: [&[u8]; 6] = [b"\n", b"\r\n", b" \n", b"\t\n", b"\xa0\n", b""];

fn base64_cases(random: &mut Random) -> Vec<Case> {
    let mut cases = Vec::new();
    for case in 0..80 {
        let data = payload(random, case);
        let line = [0, 76, 64, 4, 1][case % 5];
        let end = LINE_ENDS[random.below(LINE_ENDS.len())];
        let padded = case % 7 != 0;
        let mut bytes = writers::base64(&data, line, end, padded);
        if case % 9 == 0 {
            bytes.extend_from_slice(b"\n\n  \t");
        }
        cases.push(Case {
            name: format!("c{case}{}", [".b64", ".B64"][case % 2]),
            bytes,
            description: format!(
                "{} bytes in lines of {line} ended by {end:?}, padded {padded}",
                data.len()
            ),
        });
    }
    cases
}

#[test]
fn generated_base64_files_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let cases = base64_cases(&mut Random(0xb64_0001));
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[]);
    assert!(
        extracted > 50 && refused > 3,
        "{extracted} extracted, {refused} refused"
    );
}

const BASE64_TRAILING: &str = "data after its end";

#[test]
fn damaged_base64_files_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = binary() else {
        eprintln!("skipped: no 7-Zip 26 or later");
        return;
    };
    let mut random = Random(0xb64_0002);
    let mut cases = Vec::new();
    for case in base64_cases(&mut random) {
        let mut bytes = case.bytes;
        if bytes.is_empty() {
            continue;
        }
        let at = random.below(bytes.len());
        let replacement = b"=A/+ \n\x00!\xa0"[random.below(9)];
        bytes[at] = replacement;
        if random.below(4) == 0 {
            bytes.extend_from_slice(b"=x");
        }
        cases.push(Case {
            name: case.name,
            bytes,
            description: format!("byte {at} set to {replacement:#x}, {}", case.description),
        });
    }
    let (extracted, refused) = assert_matches_7zip(&binary, &cases, &[BASE64_TRAILING]);
    assert!(
        extracted > 5 && refused > 10,
        "{extracted} extracted, {refused} refused"
    );
}

fn small_files() -> Vec<(&'static str, Vec<u8>)> {
    let text = b"hello hello hello, a small file for every cut\n".repeat(3);
    vec![
        ("small.ex_", writers::szdd(&text, 0)),
        ("empty.ex_", writers::szdd(b"", 0)),
        (
            "bcj.lzma86",
            writers::lzma86_with_end_marker(&text, true, true, 3, 0, 2),
        ),
        (
            "open.lzma86",
            writers::lzma86_with_end_marker(&text, false, false, 3, 0, 2),
        ),
        ("h.pmd", writers::pmd(&text, Variant::H, 6, 1, b"h.txt")),
        (
            "i.pmd",
            writers::pmd(&text, Variant::I { restore: 0 }, 6, 1, b"i.txt"),
        ),
    ]
}

#[test]
fn base64_cut_inside_a_quad_or_with_data_after_its_padding_is_refused() {
    let text = writers::base64(b"hello, a small file for every cut!", 8, b"\n", true);
    let ends_inside_a_quad = |length: usize| {
        let symbols = text[..length].iter().filter(|&&byte| byte != b'\n').count();
        symbols % 4 != 0 || length == 0
    };
    for length in (0..text.len()).filter(|&length| ends_inside_a_quad(length)) {
        assert_refused(
            &format!("Base64 cut to {length}"),
            "cut.b64",
            &text[..length],
            ReadOptions::default(),
        );
    }
    let mut trailing = text.clone();
    trailing.extend_from_slice(b"QUFB");
    assert_refused(
        "Base64 with symbols after its padding",
        "trailing.b64",
        &trailing,
        ReadOptions::default(),
    );
}

fn assert_refused(case: &str, name: &str, bytes: &[u8], options: ReadOptions) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join(name);
    fs::write(&archive, bytes).unwrap();
    let result = read_archive(&archive, Some(&root.path().join("out")), options);
    assert!(result.is_err(), "{case} was accepted");
    let left: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|left| left != name)
        .collect();
    assert!(left.is_empty(), "{case} left {left:?}");
}

#[test]
fn every_small_file_extracts_and_every_truncation_or_trailing_byte_is_refused() {
    for (name, bytes) in small_files() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(name);
        fs::write(&archive, &bytes).unwrap();
        read_archive(
            &archive,
            Some(&root.path().join("out")),
            ReadOptions::default(),
        )
        .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        for length in 0..bytes.len() {
            assert_refused(
                &format!("{name} cut to {length}"),
                name,
                &bytes[..length],
                ReadOptions::default(),
            );
        }
        for trailing in [&[0u8][..], b"junk", &[0; 20]] {
            assert_refused(
                &format!("{name} with {trailing:?} after it"),
                name,
                &[bytes.as_slice(), trailing].concat(),
                ReadOptions::default(),
            );
        }
    }
}

#[test]
fn the_output_cap_is_enforced_while_decoding() {
    for (name, bytes) in small_files() {
        if name.starts_with("empty") {
            continue;
        }
        assert_refused(
            &format!("{name} over its cap"),
            name,
            &bytes,
            ReadOptions {
                max_output_bytes: Some(10),
                ..Default::default()
            },
        );
    }
}

#[test]
fn szdd_copies_past_the_declared_size_or_longer_than_16_bytes_are_refused() {
    let mut past = writers::szdd_header(0, 4);
    past.extend(writers::szdd_body(
        &[Token::Literal(1), Token::Copy { slot: 0, length: 4 }],
        0,
    ));
    assert_refused(
        "a copy past the size",
        "past.ex_",
        &past,
        ReadOptions::default(),
    );
    let mut long = writers::szdd_header(0, 17);
    long.extend(writers::szdd_body(
        &[Token::Copy {
            slot: 0,
            length: 17,
        }],
        0,
    ));
    assert_refused("a 17-byte copy", "long.ex_", &long, ReadOptions::default());
    let mut spare = writers::szdd_header(0, 1);
    spare.extend(writers::szdd_body(&[Token::Literal(1)], 0x02));
    assert_refused(
        "a spare flag bit",
        "spare.ex_",
        &spare,
        ReadOptions::default(),
    );
    let mut huge = writers::szdd_header(0, (1 << 31) + 1);
    huge.extend(writers::szdd_body(&[Token::Literal(1)], 0));
    assert_refused(
        "a size past 2 GiB",
        "huge.ex_",
        &huge,
        ReadOptions::default(),
    );
}

#[test]
fn lzma86_windows_past_256_mib_and_unknown_pmd_models_are_refused() {
    let mut wide = writers::lzma86_header(0, 0x5d, 1 << 30, u64::MAX);
    wide.extend([0; 16]);
    assert_refused(
        "a 1 GiB window",
        "wide.lzma86",
        &wide,
        ReadOptions::default(),
    );
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("wide.lzma86");
    fs::write(&archive, &wide).unwrap();
    let error = read_archive(&archive, None, ReadOptions::default()).unwrap_err();
    assert!(
        format!("{error:#}").contains("more than the 256 MiB"),
        "{error:#}"
    );
    let first = writers::lzma86_with_end_marker(&[1; 5000], false, true, 3, 0, 2);
    let tiny = writers::lzma86_with_end_marker(&[2; 100], false, true, 3, 0, 2);
    assert_refused(
        "a later stream of 100 bytes",
        "tiny.lzma86",
        &[first.as_slice(), &tiny].concat(),
        ReadOptions::default(),
    );
    assert_refused(
        "a later header cut short",
        "cut.lzma86",
        &[first.as_slice(), &tiny[..9]].concat(),
        ReadOptions::default(),
    );
    for (version, order, flags, what) in [
        (6, 6, 0, "var.G"),
        (9, 6, 0, "version 9"),
        (7, 1, 0, "order 1"),
        (8, 6, 2, "restore 2"),
    ] {
        let mut file = writers::pmd_header(version, order, 1, flags, b"x");
        file.extend([0; 32]);
        assert_refused(what, "model.pmd", &file, ReadOptions::default());
    }
}

#[test]
fn a_pmd_folder_name_is_selected_and_stripped_by_its_full_path() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("nested.pmd");
    fs::write(
        &archive,
        writers::pmd(b"nested", Variant::H, 4, 1, b"folder/inner.txt"),
    )
    .unwrap();
    let selected = ["folder".to_owned()];
    read_archive(
        &archive,
        Some(&root.path().join("strip")),
        ReadOptions {
            paths: &selected,
            strip: zipper::Strip::Components(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        fs::read(root.path().join("strip/inner.txt")).unwrap(),
        b"nested"
    );
    let other = ["other".to_owned()];
    assert_refused(
        "a path that names nothing",
        "nested.pmd",
        &fs::read(&archive).unwrap(),
        ReadOptions {
            paths: &other,
            ..Default::default()
        },
    );
}

fn files_under(folder: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files_under(&path, found);
        } else {
            found.push(path);
        }
    }
}

#[test]
fn no_corpus_file_of_another_format_is_taken_for_one_of_these() {
    let mut files = Vec::new();
    files_under(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data"),
        &mut files,
    );
    assert!(files.len() > 100);
    for file in files {
        let found = sniff(&file).unwrap_or(None);
        assert!(
            !matches!(
                found,
                Some(Format::Szdd | Format::Lzma86 | Format::Ppmd | Format::Base64)
            ),
            "{} sniffs as {found:?}",
            file.display()
        );
    }
    let root = tempfile::tempdir().unwrap();
    for (name, bytes, expected) in [
        ("zeros.bin", vec![0; 4096], None),
        ("ones.bin", vec![1; 4096], None),
        ("text.txt", b"plain text\n".repeat(50), None),
        ("data.txt", b"aGVsbG8gd29ybGQ=\n".to_vec(), None),
        (
            "data.b64",
            b"aGVsbG8gd29ybGQ=\n".to_vec(),
            Some(Format::Base64),
        ),
        ("spaced.b64", b"aGVs bG8gd29ybGQ=\n".to_vec(), None),
        ("empty.b64", Vec::new(), None),
        (
            "data.hex",
            b":0300300002337A1E\n:00000001FF\n".to_vec(),
            None,
        ),
    ] {
        let path = root.path().join(name);
        fs::write(&path, bytes).unwrap();
        assert_eq!(sniff(&path).unwrap(), expected, "{name}");
    }
}
