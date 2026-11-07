//! Encrypted 7z archives from 7-Zip in `tests/data/encrypted-7z`, and
//! archives built here with every AES property layout, extracted with the
//! right password, a wrong one, none, and damage at every byte. `trees.json`
//! holds the tree `7zz x` gave when the fixtures were written.
use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{
    Engine, Existing, ExtractOptions,
    native_7z::{PasswordNeeded, ReadOptions},
};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/encrypted-7z");
const PASSWORD: &str = "zipper";
const WRONG: &str = "Zipper-but-wrong";
const UNICODE_PASSWORD: &str = "пароль-日本-😀";
const AES: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];
const LARGE_FILE: usize = 200 * 1024;

struct Fixture {
    name: &'static str,
    switches: &'static [&'static str],
    password: &'static str,
    source: Source,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Tree,
    Large,
    Tiny,
}

const fn fixture(name: &'static str, switches: &'static [&'static str]) -> Fixture {
    Fixture {
        name,
        switches,
        password: PASSWORD,
        source: Source::Tree,
    }
}

const FIXTURES: &[Fixture] = &[
    fixture("lzma2-mx1-header.7z", &["-mx=1", "-mhe=on"]),
    fixture("lzma2-mx5.7z", &["-mx=5", "-mhe=off"]),
    fixture("lzma2-mx9-header.7z", &["-mx=9", "-mhe=on"]),
    fixture("lzma2-nonsolid-header.7z", &["-ms=off", "-mhe=on"]),
    fixture("lzma2-solid-per-2-files-header.7z", &["-ms=2f", "-mhe=on"]),
    fixture("copy-nonsolid.7z", &["-mx=0", "-ms=off", "-mhe=off"]),
    fixture(
        "copy-uncompressed-header.7z",
        &["-mx=0", "-mhc=off", "-mhe=on"],
    ),
    fixture("lzma-header.7z", &["-m0=LZMA", "-mhe=on"]),
    fixture("ppmd-header.7z", &["-m0=PPMd", "-mhe=on"]),
    fixture("ppmd.7z", &["-m0=PPMd", "-mhe=off"]),
    fixture("bzip2.7z", &["-m0=BZip2", "-mhe=off"]),
    fixture("deflate.7z", &["-m0=Deflate", "-mhe=off"]),
    fixture("deflate64-header.7z", &["-m0=Deflate64", "-mhe=on"]),
    fixture("bcj-header.7z", &["-mf=BCJ", "-mhe=on"]),
    fixture("bcj2-header.7z", &["-mf=BCJ2", "-mhe=on"]),
    fixture("bcj2-nonsolid.7z", &["-mf=BCJ2", "-ms=off", "-mhe=off"]),
    fixture("arm64.7z", &["-mf=ARM64", "-mhe=off"]),
    fixture("delta-header.7z", &["-mf=Delta:4", "-mhe=on"]),
    fixture("swap4.7z", &["-mf=Swap4", "-mhe=off"]),
    Fixture {
        name: "empty-password-header.7z",
        switches: &["-mhe=on"],
        password: "",
        source: Source::Tree,
    },
    Fixture {
        name: "unicode-password-header.7z",
        switches: &["-mhe=on"],
        password: UNICODE_PASSWORD,
        source: Source::Tree,
    },
    Fixture {
        name: "unicode-password.7z",
        switches: &["-mhe=off"],
        password: UNICODE_PASSWORD,
        source: Source::Tree,
    },
    Fixture {
        name: "large-copy-header.7z",
        switches: &["-mx=0", "-mhe=on"],
        password: PASSWORD,
        source: Source::Large,
    },
    Fixture {
        name: "large-lzma2.7z",
        switches: &["-mx=1", "-mhe=off"],
        password: PASSWORD,
        source: Source::Large,
    },
    Fixture {
        name: "tiny-header.7z",
        switches: &["-mhe=on"],
        password: PASSWORD,
        source: Source::Tiny,
    },
    Fixture {
        name: "tiny.7z",
        switches: &["-mhe=off"],
        password: PASSWORD,
        source: Source::Tiny,
    },
];

fn path(name: &str) -> PathBuf {
    Path::new(DATA).join(name)
}

fn header_encrypted(fixture: &Fixture) -> bool {
    fixture.switches.contains(&"-mhe=on")
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
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

fn recorded_trees() -> BTreeMap<String, BTreeMap<String, String>> {
    serde_json::from_str(&fs::read_to_string(path("trees.json")).unwrap()).unwrap()
}

fn seven_zip() -> Option<PathBuf> {
    if let Some(binary) = std::env::var_os("ZIPPER_7Z") {
        return Some(binary.into());
    }
    ["7zz", "7z"]
        .into_iter()
        .find(|binary| {
            Command::new(binary)
                .arg("i")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .map(PathBuf::from)
}

fn run_seven_zip(binary: &Path, arguments: &[&std::ffi::OsStr], password: &str) -> bool {
    run_seven_zip_in(binary, Path::new("."), arguments, password)
}

fn run_seven_zip_in(
    binary: &Path,
    folder: &Path,
    arguments: &[&std::ffi::OsStr],
    password: &str,
) -> bool {
    let mut child = Command::new(binary)
        .args(arguments)
        .current_dir(folder)
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{password}\n").as_bytes());
    child.wait().unwrap().success()
}

fn seven_zip_tree(binary: &Path, archive: &Path, password: &str) -> BTreeMap<String, String> {
    let work = tempfile::tempdir().unwrap();
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(work.path().join("out"));
    let extracted = run_seven_zip(
        binary,
        &["x".as_ref(), "-y".as_ref(), flag.as_ref(), archive.as_ref()],
        password,
    );
    assert!(extracted, "7-Zip failed on {}", archive.display());
    tree(&work.path().join("out"))
}

fn extract(
    archive: &Path,
    destination: &Path,
    password: Option<&str>,
    existing: Existing,
    paths: &[String],
    threads: Option<usize>,
) -> anyhow::Result<()> {
    zipper::extract_archive(
        archive,
        destination,
        ExtractOptions {
            engine: Engine::Auto,
            password: password.map(str::as_bytes),
            existing,
            paths,
            threads,
            ..Default::default()
        },
    )
}

fn listing(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_fails_cleanly(archive: &[u8], password: Option<&str>, case: &str) -> Vec<String> {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("case.7z");
    fs::write(&path, archive).unwrap();
    let kept = work.path().join("kept");
    fs::create_dir(&kept).unwrap();
    fs::write(kept.join("a.txt"), b"the file that was here").unwrap();
    let before = tree(&kept);
    let mut errors = Vec::new();
    for (destination, existing) in [
        (kept.clone(), Existing::Overwrite),
        (work.path().join("fresh"), Existing::Refuse),
    ] {
        let error = extract(&path, &destination, password, existing, &[], None)
            .expect_err(&format!("{case}: extracted into {existing:?}"));
        errors.push(format!("{error:#}"));
        assert_eq!(tree(&kept), before, "{case}: the destination changed");
        assert_eq!(
            listing(work.path()),
            ["case.7z", "kept"],
            "{case}: a staging folder or output was left"
        );
    }
    let verified = zipper::native_7z::read_archive(
        &path,
        None,
        ReadOptions {
            password: password.map(str::as_bytes),
            ..Default::default()
        },
    );
    errors.push(format!("{:#}", verified.expect_err(case)));
    errors
}

fn assert_all_say(errors: &[String], words: &str, case: &str) {
    for error in errors {
        assert!(error.contains(words), "{case}: {error}");
    }
}

#[test]
fn every_fixture_extracts_to_the_tree_7zip_gave_in_process() {
    let trees = recorded_trees();
    let live = seven_zip();
    assert!(
        live.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip was found"
    );
    assert_eq!(
        trees.len(),
        FIXTURES.len(),
        "trees.json and the fixtures differ"
    );
    for fixture in FIXTURES {
        let archive = path(fixture.name);
        let expected = &trees[fixture.name];
        if let Some(binary) = &live {
            assert_eq!(
                &seven_zip_tree(binary, &archive, fixture.password),
                expected,
                "{}: 7-Zip",
                fixture.name
            );
        }
        assert!(
            matches!(
                zipper::choose_reader(Engine::Auto, &archive).unwrap(),
                zipper::Reader::SevenZip
            ),
            "{} goes to 7-Zip",
            fixture.name
        );
        for threads in [Some(1), None] {
            let work = tempfile::tempdir().unwrap();
            let output = work.path().join("out");
            extract(
                &archive,
                &output,
                Some(fixture.password),
                Existing::Refuse,
                &[],
                threads,
            )
            .unwrap_or_else(|error| panic!("{}: {error:#}", fixture.name));
            assert_eq!(&tree(&output), expected, "{}", fixture.name);
        }
        zipper::native_7z::read_archive(
            &archive,
            None,
            ReadOptions {
                password: Some(fixture.password.as_bytes()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("{} verification: {error:#}", fixture.name));
        let listed =
            zipper::native_7z::list_with_password(&archive, Some(fixture.password.as_bytes()))
                .unwrap();
        let files: Vec<&String> = expected
            .iter()
            .filter(|(_, kind)| *kind != "dir")
            .map(|(name, _)| name)
            .collect();
        let mut listed_files: Vec<&String> = listed
            .iter()
            .filter(|entry| !entry.directory)
            .map(|entry| &entry.name)
            .collect();
        listed_files.sort();
        assert_eq!(listed_files, files, "{} listing", fixture.name);
    }
}

#[test]
fn selected_paths_extract_only_their_entries() {
    let trees = recorded_trees();
    for name in [
        "lzma2-mx9-header.7z",
        "bcj2-header.7z",
        "copy-nonsolid.7z",
        "lzma2-solid-per-2-files-header.7z",
    ] {
        let work = tempfile::tempdir().unwrap();
        let output = work.path().join("out");
        let wanted = ["sub/text.txt".to_owned()];
        extract(
            &path(name),
            &output,
            Some(PASSWORD),
            Existing::Refuse,
            &wanted,
            None,
        )
        .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        let expected: BTreeMap<String, String> = trees[name]
            .iter()
            .filter(|(entry, _)| *entry == "sub" || *entry == "sub/text.txt")
            .map(|(entry, kind)| (entry.clone(), kind.clone()))
            .collect();
        assert_eq!(tree(&output), expected, "{name}");
    }
}

#[test]
fn a_wrong_password_changes_nothing_in_the_destination() {
    for fixture in FIXTURES {
        let archive = fs::read(path(fixture.name)).unwrap();
        let wrong = if fixture.password.is_empty() {
            "x"
        } else {
            WRONG
        };
        let errors = assert_fails_cleanly(&archive, Some(wrong), fixture.name);
        assert_all_say(&errors, "the password is wrong", fixture.name);
        if !fixture.password.is_empty() {
            let errors = assert_fails_cleanly(&archive, Some(""), fixture.name);
            assert_all_say(&errors, "the password is wrong", fixture.name);
        }
    }
}

#[test]
fn no_password_fails_before_anything_is_written() {
    for fixture in FIXTURES {
        let archive = fs::read(path(fixture.name)).unwrap();
        let errors = assert_fails_cleanly(&archive, None, fixture.name);
        assert_all_say(&errors, "give its password with --password", fixture.name);
        let work = tempfile::tempdir().unwrap();
        let error = extract(
            &path(fixture.name),
            &work.path().join("out"),
            None,
            Existing::Refuse,
            &[],
            None,
        )
        .unwrap_err();
        let needed = error
            .downcast_ref::<PasswordNeeded>()
            .unwrap_or_else(|| panic!("{}: {error:#}", fixture.name));
        assert_eq!(needed.header, header_encrypted(fixture), "{}", fixture.name);
        assert_eq!(
            zipper::native_7z::list(&path(fixture.name)).is_ok(),
            !header_encrypted(fixture),
            "{}",
            fixture.name
        );
    }
}

#[test]
fn a_password_that_is_not_utf8_is_refused_for_an_encrypted_archive_only() {
    let errors = {
        let archive = fs::read(path("lzma2-mx5.7z")).unwrap();
        let work = tempfile::tempdir().unwrap();
        let file = work.path().join("a.7z");
        fs::write(&file, archive).unwrap();
        zipper::native_7z::read_archive(
            &file,
            None,
            ReadOptions {
                password: Some(b"\xFFzipper"),
                ..Default::default()
            },
        )
        .unwrap_err()
    };
    assert!(format!("{errors:#}").contains("UTF-8"), "{errors:#}");
    let plain = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/parity/archives/7z.7z");
    zipper::native_7z::read_archive(
        &plain,
        None,
        ReadOptions {
            password: Some(b"\xFF"),
            ..Default::default()
        },
    )
    .unwrap();
}

fn number(value: u64) -> Vec<u8> {
    let mut encoded = vec![0xFF];
    encoded.extend_from_slice(&value.to_le_bytes());
    encoded
}

fn reference_key(power: u8, salt: &[u8], password: &str) -> [u8; 32] {
    let password: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
    if power == 0x3F {
        let mut key = [0u8; 32];
        for (slot, byte) in key.iter_mut().zip(salt.iter().chain(&password)) {
            *slot = *byte;
        }
        return key;
    }
    let mut hasher = Sha256::new();
    for round in 0..1u64 << power {
        hasher.update(salt);
        hasher.update(&password);
        hasher.update(round.to_le_bytes());
    }
    hasher.finalize().into()
}

fn encrypt(key: &[u8; 32], iv: &[u8], plain: &[u8]) -> Vec<u8> {
    let cipher = aes::Aes256::new_from_slice(key).unwrap();
    let mut chain = [0u8; 16];
    chain[..iv.len()].copy_from_slice(iv);
    let mut encrypted = Vec::new();
    for block in plain.chunks(16) {
        let mut bytes = aes::Block::default();
        for (at, byte) in bytes.iter_mut().enumerate() {
            *byte = block.get(at).copied().unwrap_or(0) ^ chain[at];
        }
        cipher.encrypt_block(&mut bytes);
        chain.copy_from_slice(&bytes);
        encrypted.extend_from_slice(&bytes);
    }
    encrypted
}

#[derive(Clone)]
struct Built {
    properties: Vec<u8>,
    power: u8,
    salt: Vec<u8>,
    iv: Vec<u8>,
    password: &'static str,
    data: Vec<u8>,
    crc: bool,
    pack_size_delta: i64,
    decrypted_delta: i64,
    encrypted_header: bool,
    header_crc: bool,
    lzma2: Option<Vec<u8>>,
}

fn properties(power: u8, salt: &[u8], iv: &[u8]) -> Vec<u8> {
    if salt.is_empty() && iv.is_empty() {
        return vec![power];
    }
    let (salt_high, salt_low) = (salt.len().min(1), salt.len() - salt.len().min(1));
    let (iv_high, iv_low) = (iv.len().min(1), iv.len() - iv.len().min(1));
    let mut bytes = vec![
        power | (salt_high as u8) << 7 | (iv_high as u8) << 6,
        (salt_low as u8) << 4 | iv_low as u8,
    ];
    bytes.extend_from_slice(salt);
    bytes.extend_from_slice(iv);
    bytes
}

fn built(data: &[u8]) -> Built {
    let salt = b"pepper".to_vec();
    let iv = (1..=16).collect::<Vec<u8>>();
    Built {
        properties: properties(4, &salt, &iv),
        power: 4,
        salt,
        iv,
        password: PASSWORD,
        data: data.to_vec(),
        crc: true,
        pack_size_delta: 0,
        decrypted_delta: 0,
        encrypted_header: false,
        header_crc: true,
        lzma2: None,
    }
}

fn encrypted_folder(
    built: &Built,
    plain: &[u8],
    lzma2: Option<&[u8]>,
    pack_position: u64,
    crc: bool,
) -> (Vec<u8>, Vec<u8>) {
    let key = reference_key(built.power, &built.salt, built.password);
    let decrypted = lzma2.unwrap_or(plain);
    let packed = encrypt(&key, &built.iv, decrypted);
    let mut info = vec![0x06];
    info.extend(number(pack_position));
    info.extend(number(1));
    info.push(0x09);
    info.extend(number((packed.len() as i64 + built.pack_size_delta) as u64));
    info.push(0x00);
    info.extend([0x07, 0x0B]);
    info.extend(number(1));
    info.push(0x00);
    info.extend(number(2));
    if lzma2.is_some() {
        info.extend([0x21, 0x21, 0x01, 18]);
    } else {
        info.extend([0x01, 0x00]);
    }
    info.push(0x20 | AES.len() as u8);
    info.extend_from_slice(&AES);
    info.extend(number(built.properties.len() as u64));
    info.extend_from_slice(&built.properties);
    info.extend(number(0));
    info.extend(number(1));
    info.push(0x0C);
    info.extend(number(plain.len() as u64));
    info.extend(number(
        (decrypted.len() as i64 + built.decrypted_delta) as u64,
    ));
    if crc {
        info.extend([0x0A, 0x01]);
        info.extend_from_slice(&crc32fast::hash(plain).to_le_bytes());
    }
    info.push(0x00);
    info.push(0x00);
    (packed, info)
}

fn build(built: &Built) -> Vec<u8> {
    let (packed, streams) =
        encrypted_folder(built, &built.data, built.lzma2.as_deref(), 0, built.crc);
    let mut header = vec![0x01, 0x04];
    header.extend(streams);
    header.extend([0x05, 0x01, 0x11]);
    let mut names = vec![0u8];
    for unit in "a.txt".encode_utf16().chain([0]) {
        names.extend_from_slice(&unit.to_le_bytes());
    }
    header.extend(number(names.len() as u64));
    header.extend(names);
    header.extend([0x00, 0x00]);
    let mut body = packed;
    let mut next = header;
    if built.encrypted_header {
        let (header_packed, header_streams) =
            encrypted_folder(built, &next, None, body.len() as u64, built.header_crc);
        body.extend(header_packed);
        next = vec![0x17];
        next.extend(header_streams);
    }
    let mut start = Vec::new();
    start.extend_from_slice(&(body.len() as u64).to_le_bytes());
    start.extend_from_slice(&(next.len() as u64).to_le_bytes());
    start.extend_from_slice(&crc32fast::hash(&next).to_le_bytes());
    let mut archive = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
    archive.extend_from_slice(&crc32fast::hash(&start).to_le_bytes());
    archive.extend(start);
    archive.extend(body);
    archive.extend(next);
    archive
}

fn zipper_reads(archive: &[u8], password: Option<&str>) -> Result<Vec<u8>, String> {
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("a.7z");
    fs::write(&file, archive).unwrap();
    let output = work.path().join("out");
    extract(&file, &output, password, Existing::Refuse, &[], None)
        .map_err(|error| format!("{error:#}"))?;
    Ok(fs::read(output.join("a.txt")).unwrap())
}

fn seven_zip_reads(binary: &Path, archive: &[u8], password: &str) -> Option<Vec<u8>> {
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("a.7z");
    fs::write(&file, archive).unwrap();
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(work.path().join("out"));
    run_seven_zip(
        binary,
        &["x".as_ref(), "-y".as_ref(), flag.as_ref(), file.as_ref()],
        password,
    )
    .then(|| fs::read(work.path().join("out/a.txt")).unwrap())
}

#[test]
fn built_archives_with_every_salt_and_iv_size_extract_as_7zip_extracts_them() {
    let live = seven_zip();
    let data = b"attack at dawn, attack at dawn; bring the ladder\n".repeat(3);
    let salt: Vec<u8> = (100..116).collect();
    let iv: Vec<u8> = (200..216).collect();
    for power in [0, 1, 6, 0x3F] {
        for salt_len in [0, 1, 2, 15, 16] {
            for iv_len in [0, 1, 8, 15, 16] {
                for encrypted_header in [false, true] {
                    let mut case = built(&data);
                    case.power = power;
                    case.salt = salt[..salt_len].to_vec();
                    case.iv = iv[..iv_len].to_vec();
                    case.properties = properties(power, &case.salt, &case.iv);
                    case.encrypted_header = encrypted_header;
                    let archive = build(&case);
                    let label = format!("power {power}, salt {salt_len}, iv {iv_len}");
                    assert_eq!(
                        zipper_reads(&archive, Some(PASSWORD)).as_deref(),
                        Ok(&data[..]),
                        "{label}"
                    );
                    if let Some(binary) = &live {
                        assert_eq!(
                            seven_zip_reads(binary, &archive, PASSWORD).as_deref(),
                            Some(&data[..]),
                            "{label}: 7-Zip"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn malformed_aes_properties_are_refused_as_7zip_refuses_them() {
    let live = seven_zip();
    let data = b"some text".to_vec();
    let mut layouts: Vec<Vec<u8>> = vec![
        vec![0x04, 0x00],
        vec![0xC4],
        vec![0x84, 0x00],
        vec![0x84, 0x10],
        vec![0x44, 0x00, 1, 2],
        vec![0xC4, 0xFF],
    ];
    for (salt, iv) in [(3usize, 5usize), (0, 7), (16, 16), (1, 0)] {
        let good = properties(4, &vec![9; salt], &vec![8; iv]);
        layouts.push(good[..good.len() - 1].to_vec());
        layouts.push([&good[..], &[0]].concat());
    }
    for layout in layouts {
        let mut case = built(&data);
        case.properties = layout.clone();
        let archive = build(&case);
        let errors = assert_fails_cleanly(&archive, Some(PASSWORD), &format!("{layout:?}"));
        assert_all_say(
            &errors,
            "7z AES properties are malformed",
            &format!("{layout:?}"),
        );
        if let Some(binary) = &live {
            assert_eq!(
                seven_zip_reads(binary, &archive, PASSWORD),
                None,
                "{layout:?}: 7-Zip read it"
            );
        }
    }
}

#[test]
fn a_cycles_power_past_7zips_limit_is_refused_before_any_key_derivation() {
    for power in [25, 32, 40, 62] {
        for (salt, iv) in [(&b""[..], &b""[..]), (&b"salt"[..], &[7u8; 16][..])] {
            let mut case = built(b"text");
            case.properties = properties(power, salt, iv);
            let in_data = build(&case);
            let in_header = with_header_power(power, salt, iv);
            for (place, archive) in [("data", in_data), ("header", in_header)] {
                let label = format!("power {power} in the {place}, salt {}", salt.len());
                let started = std::time::Instant::now();
                let errors = assert_fails_cleanly(&archive, Some(PASSWORD), &label);
                assert!(
                    started.elapsed().as_secs() < 30,
                    "{label}: key derivation ran"
                );
                assert_all_say(&errors, &format!("2^{power} SHA-256 rounds"), &label);
            }
        }
    }
}

fn with_header_power(power: u8, salt: &[u8], iv: &[u8]) -> Vec<u8> {
    let mut case = built(b"text");
    case.encrypted_header = true;
    case.salt = salt.to_vec();
    case.iv = iv.to_vec();
    case.properties = properties(4, salt, iv);
    let honest = build(&case);
    let from = properties(4, salt, iv);
    let mut coder = AES.to_vec();
    coder.extend(number(from.len() as u64));
    coder.extend_from_slice(&from);
    let at = honest
        .windows(coder.len())
        .rposition(|window| window == coder)
        .unwrap();
    let mut patched = honest;
    patched[at + coder.len() - from.len()] = properties(power, salt, iv)[0];
    let header_start = 32 + u64::from_le_bytes(patched[12..20].try_into().unwrap()) as usize;
    let header_crc = crc32fast::hash(&patched[header_start..]);
    patched[28..32].copy_from_slice(&header_crc.to_le_bytes());
    let start_crc = crc32fast::hash(&patched[12..32]);
    patched[8..12].copy_from_slice(&start_crc.to_le_bytes());
    patched
}

#[test]
fn every_point_where_decryption_can_fail_leaves_the_destination_unchanged() {
    let data = b"attack at dawn, attack at dawn; bring the ladder\n".repeat(2);
    let mut cases: Vec<(String, Vec<u8>, Option<&str>, &str)> = Vec::new();
    for encrypted_header in [false, true] {
        let mut good = built(&data);
        good.encrypted_header = encrypted_header;
        let label = |what: &str| format!("{what}, encrypted header {encrypted_header}");
        let archive = build(&good);
        cases.push((
            label("no password"),
            archive.clone(),
            None,
            "give its password with --password",
        ));
        cases.push((
            label("a wrong password"),
            archive.clone(),
            Some(WRONG),
            "the password is wrong",
        ));
        let mut no_crc = good.clone();
        no_crc.crc = encrypted_header;
        no_crc.header_crc = !encrypted_header;
        cases.push((
            label("no CRC"),
            build(&no_crc),
            Some(PASSWORD),
            "without a CRC, so a wrong password could not be detected",
        ));
        for (what, pack, decrypted, words) in [
            (
                "a packed stream of part of a block",
                -1,
                0,
                "whole AES blocks",
            ),
            (
                "a packed stream past the header",
                16,
                0,
                "overlaps the header",
            ),
            (
                "more decrypted bytes than the blocks hold",
                0,
                15,
                "whole AES blocks",
            ),
            (
                "a decrypted size past the data",
                0,
                1,
                "the password is wrong",
            ),
            ("a whole block of padding", 0, -16, "whole AES blocks"),
        ] {
            let mut damaged = good.clone();
            damaged.pack_size_delta = pack;
            damaged.decrypted_delta = decrypted;
            cases.push((label(what), build(&damaged), Some(PASSWORD), words));
        }
        let packed_len = data.len().div_ceil(16) as i64 * 16;
        for (what, pack, decrypted) in [
            (
                "a decrypted size that rounds past 2^64",
                -packed_len,
                -(data.len() as i64) - 15,
            ),
            (
                "the largest decrypted size",
                -packed_len,
                -(data.len() as i64) - 1,
            ),
        ] {
            let mut damaged = good.clone();
            damaged.pack_size_delta = pack;
            damaged.decrypted_delta = decrypted;
            if !encrypted_header {
                let archive = build(&damaged);
                cases.push((label(what), archive, Some(PASSWORD), "whole AES blocks"));
            }
        }
        let data_end = 32 + data.len().div_ceil(16) * 16;
        let next_size = u64::from_le_bytes(archive[20..28].try_into().unwrap()) as usize;
        let header_end = archive.len() - next_size;
        for at in 32..header_end {
            let mut flipped = archive.clone();
            flipped[at] ^= 0x01;
            let place = if at < data_end { "data" } else { "header" };
            cases.push((
                label(&format!("encrypted {place} byte {at} flipped")),
                flipped,
                Some(PASSWORD),
                "the password is wrong",
            ));
        }
        for length in 0..archive.len() {
            cases.push((
                label(&format!("cut at {length}")),
                archive[..length].to_vec(),
                Some(PASSWORD),
                "",
            ));
        }
    }
    for (case, archive, password, words) in &cases {
        let errors = assert_fails_cleanly(archive, *password, case);
        assert_all_say(&errors, words, case);
    }
}

fn stored_lzma2_with_resets(data: &[u8]) -> Vec<u8> {
    let mut stream = Vec::new();
    for chunk in data.chunks(1 << 16) {
        stream.push(0x01);
        stream.extend_from_slice(&((chunk.len() - 1) as u16).to_be_bytes());
        stream.extend_from_slice(chunk);
    }
    stream.push(0x00);
    stream
}

#[test]
fn an_encrypted_solid_lzma2_folder_decodes_on_one_thread_to_its_bytes() {
    let data = noise(17 << 20, 99);
    let mut case = built(&data);
    case.lzma2 = Some(stored_lzma2_with_resets(&data));
    let archive = build(&case);
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("a.7z");
    fs::write(&file, &archive).unwrap();
    for threads in [Some(1), Some(8)] {
        let output = work.path().join(format!("out-{threads:?}"));
        extract(
            &file,
            &output,
            Some(PASSWORD),
            Existing::Refuse,
            &[],
            threads,
        )
        .unwrap_or_else(|error| panic!("{threads:?}: {error:#}"));
        assert!(
            fs::read(output.join("a.txt")).unwrap() == data,
            "{threads:?}"
        );
    }
}

fn decrypt(key: &[u8; 32], iv: &[u8], encrypted: &[u8]) -> Vec<u8> {
    let cipher = aes::Aes256::new_from_slice(key).unwrap();
    let mut chain = [0u8; 16];
    chain[..iv.len()].copy_from_slice(iv);
    let mut plain = Vec::new();
    for block in encrypted.chunks_exact(16) {
        let mut bytes = aes::Block::try_from(block).unwrap();
        cipher.decrypt_block(&mut bytes);
        plain.extend(
            bytes
                .iter()
                .zip(&chain)
                .map(|(byte, previous)| byte ^ previous),
        );
        chain.copy_from_slice(block);
    }
    plain
}

#[test]
fn an_encrypted_lzma2_folder_never_splits_its_packed_bytes_into_segments() {
    let data = noise(17 << 20, 5);
    let mut packed = stored_lzma2_with_resets(&data);
    packed.resize(packed.len().div_ceil(16) * 16, 0);
    let mut case = built(&data);
    let key = reference_key(case.power, &case.salt, case.password);
    case.lzma2 = Some(decrypt(&key, &case.iv, &packed));
    let archive = build(&case);
    assert!(
        archive[32..].starts_with(&packed),
        "the packed bytes are a valid LZMA2 stream"
    );
    let work = tempfile::tempdir().unwrap();
    let file = work.path().join("a.7z");
    fs::write(&file, &archive).unwrap();
    for threads in [Some(1), Some(8)] {
        let output = work.path().join(format!("out-{threads:?}"));
        let error = extract(
            &file,
            &output,
            Some(PASSWORD),
            Existing::Refuse,
            &[],
            threads,
        )
        .expect_err("the decrypted bytes are not LZMA2");
        assert!(
            format!("{error:#}").contains("the password is wrong"),
            "{error:#}"
        );
        assert!(!output.exists());
    }
}

#[test]
fn a_wrong_password_on_encrypted_data_is_caught_by_the_crc_alone() {
    let data: Vec<u8> = (0..1000u32).map(|at| (at * 7 % 256) as u8).collect();
    let mut case = built(&data);
    case.password = WRONG;
    let archive = build(&case);
    let errors = assert_fails_cleanly(&archive, Some(PASSWORD), "copy with another key");
    assert_all_say(
        &errors,
        "the password is wrong or the 7z data is damaged",
        "copy",
    );
    assert_all_say(&errors, "CRC mismatch", "copy");
}

#[test]
fn damage_at_every_byte_of_a_7zip_archive_fails_cleanly_or_changes_nothing() {
    let trees = recorded_trees();
    for name in ["tiny-header.7z", "tiny.7z"] {
        let archive = fs::read(path(name)).unwrap();
        let expected = &trees[name];
        for at in 0..archive.len() {
            let mut damaged = archive.clone();
            damaged[at] ^= 0x10;
            let work = tempfile::tempdir().unwrap();
            let file = work.path().join("case.7z");
            fs::write(&file, &damaged).unwrap();
            let output = work.path().join("out");
            match extract(&file, &output, Some(PASSWORD), Existing::Refuse, &[], None) {
                Ok(()) => assert_eq!(&tree(&output), expected, "{name} byte {at}"),
                Err(_) => assert_eq!(listing(work.path()), ["case.7z"], "{name} byte {at}"),
            }
        }
    }
}

#[test]
fn the_command_line_decrypts_lists_and_tests_with_a_password() {
    let archive = path("lzma2-mx1-header.7z");
    let zipper = |arguments: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let listed = zipper(&[
        "--password".as_ref(),
        PASSWORD.as_ref(),
        "list".as_ref(),
        archive.as_ref(),
    ]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(String::from_utf8_lossy(&listed.stdout).contains("sub/text.txt"));
    let unlisted = zipper(&["list".as_ref(), archive.as_ref()]);
    assert!(!unlisted.status.success());
    assert!(String::from_utf8_lossy(&unlisted.stderr).contains("--password"));
    let tested = zipper(&[
        "--password".as_ref(),
        PASSWORD.as_ref(),
        "test".as_ref(),
        archive.as_ref(),
    ]);
    assert!(
        tested.status.success(),
        "{}",
        String::from_utf8_lossy(&tested.stderr)
    );
    let wrong = zipper(&[
        "--password".as_ref(),
        WRONG.as_ref(),
        "test".as_ref(),
        archive.as_ref(),
    ]);
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("the password is wrong"));
    let work = tempfile::tempdir().unwrap();
    let output = work.path().join("out");
    let extracted = zipper(&[
        "--password".as_ref(),
        PASSWORD.as_ref(),
        "extract".as_ref(),
        archive.as_ref(),
        "-o".as_ref(),
        output.as_ref(),
    ]);
    assert!(
        extracted.status.success(),
        "{}",
        String::from_utf8_lossy(&extracted.stderr)
    );
    assert_eq!(tree(&output), recorded_trees()["lzma2-mx1-header.7z"]);
    let plain_listing = zipper(&["list".as_ref(), path("lzma2-mx5.7z").as_ref()]);
    assert!(plain_listing.status.success());
}

fn write_source(root: &Path) {
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir_all(root.join("ünï")).unwrap();
    fs::write(root.join("a.txt"), b"hello world\n").unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    fs::write(root.join("ünï/日本.txt"), "名前のあるファイル\n").unwrap();
    fs::write(root.join("sub/random.bin"), noise(8192, 0x2545_f491)).unwrap();
    let text: String = (0..2000)
        .map(|line| format!("compressible line {line}\n"))
        .collect();
    fs::write(root.join("sub/text.txt"), text).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
}

fn noise(length: usize, seed: u32) -> Vec<u8> {
    let mut state = seed;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

fn seven_zip_write(
    binary: &Path,
    source: &Path,
    archive: &Path,
    switches: &[&str],
    password: &str,
) {
    let mut arguments: Vec<&std::ffi::OsStr> = ["a", "-t7z", "-p", "-snl", "-bso0", "-bsp0"]
        .iter()
        .map(std::ffi::OsStr::new)
        .collect();
    arguments.extend(switches.iter().map(std::ffi::OsStr::new));
    arguments.push(archive.as_ref());
    let mut entries: Vec<std::ffi::OsString> = fs::read_dir(source)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    arguments.extend(entries.iter().map(|entry| entry.as_os_str()));
    assert!(
        run_seven_zip_in(
            binary,
            source,
            &arguments,
            &format!("{password}\n{password}")
        ),
        "{}",
        archive.display()
    );
}

#[test]
#[ignore = "rewrites the fixtures with 7-Zip when ZIPPER_WRITE_ENCRYPTED_7Z=1"]
fn write_fixtures() {
    assert!(std::env::var_os("ZIPPER_WRITE_ENCRYPTED_7Z").is_some());
    let binary = seven_zip().expect("7-Zip writes the fixtures");
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    write_source(&source);
    let large = work.path().join("large");
    fs::create_dir_all(&large).unwrap();
    let mut big = noise(LARGE_FILE / 2, 7);
    big.extend(b"a long run of text\n".repeat(LARGE_FILE / 2 / 19));
    fs::write(large.join("large.bin"), big).unwrap();
    let tiny = work.path().join("tiny");
    fs::create_dir_all(&tiny).unwrap();
    fs::write(tiny.join("secret.txt"), b"attack at dawn\n").unwrap();
    let out = Path::new(DATA);
    fs::create_dir_all(out).unwrap();
    for entry in fs::read_dir(out).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let mut trees = BTreeMap::new();
    for fixture in FIXTURES {
        let from = match fixture.source {
            Source::Tree => &source,
            Source::Large => &large,
            Source::Tiny => &tiny,
        };
        let archive = out.join(fixture.name);
        seven_zip_write(&binary, from, &archive, fixture.switches, fixture.password);
        trees.insert(
            fixture.name.to_owned(),
            seven_zip_tree(&binary, &archive, fixture.password),
        );
    }
    fs::write(
        out.join("trees.json"),
        serde_json::to_string_pretty(&trees).unwrap() + "\n",
    )
    .unwrap();
}
