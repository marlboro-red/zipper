//! Encrypted ZIP archives from 7-Zip and Info-ZIP in `tests/data/encrypted-zip`,
//! extracted with the right password, a wrong one, none, and damage at every
//! byte. `trees.json` holds the tree `7zz x` gave when the fixtures were
//! written.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{Engine, Existing, ExtractOptions, native_zip::PasswordNeeded};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/encrypted-zip");
const PASSWORD: &[u8] = b"zipper";
const WRONG: &[u8] = b"Zipper-but-wrong";
const ENGINES: [Engine; 2] = [Engine::Native, Engine::NativeFast];
const AES_METHOD: u16 = 99;
const AES_FIELD: u16 = 0x9901;
const AES_CODE: usize = 10;

const SEVEN_ZIP_FIXTURES: &[(&str, &[&str])] = &[
    ("zipcrypto-deflate.zip", &["-mem=ZipCrypto", "-mm=Deflate"]),
    ("zipcrypto-stored.zip", &["-mem=ZipCrypto", "-mm=Copy"]),
    ("zipcrypto-bzip2.zip", &["-mem=ZipCrypto", "-mm=BZip2"]),
    ("zipcrypto-lzma.zip", &["-mem=ZipCrypto", "-mm=LZMA"]),
    ("zipcrypto-descriptor.zip", &["-mem=ZipCrypto", "-so"]),
    ("aes128-deflate.zip", &["-mem=AES128", "-mm=Deflate"]),
    ("aes192-lzma.zip", &["-mem=AES192", "-mm=LZMA"]),
    ("aes256-deflate.zip", &["-mem=AES256", "-mm=Deflate"]),
    ("aes256-stored.zip", &["-mem=AES256", "-mm=Copy"]),
    ("aes256-bzip2.zip", &["-mem=AES256", "-mm=BZip2"]),
    ("aes256-deflate64.zip", &["-mem=AES256", "-mm=Deflate64"]),
    ("aes256-ppmd.zip", &["-mem=AES256", "-mm=PPMd"]),
    ("aes256-xz.zip", &["-mem=AES256", "-mm=XZ"]),
    ("aes128-descriptor.zip", &["-mem=AES128", "-so"]),
];
const AE1_FIXTURES: &[(&str, &str)] = &[
    ("aes256-ae1.zip", "aes256-deflate.zip"),
    ("tiny-aes256-ae1.zip", "tiny-aes256.zip"),
];
const TINY_FIXTURES: &[(&str, &str, &str)] = &[
    ("tiny-zipcrypto.zip", "-mem=ZipCrypto", "-mm=Copy"),
    ("tiny-zipcrypto-descriptor.zip", INFO_ZIP, "-0"),
    ("tiny-aes128.zip", "-mem=AES128", "-mm=Deflate"),
    ("tiny-aes192.zip", "-mem=AES192", "-mm=Deflate"),
    ("tiny-aes256.zip", "-mem=AES256", "-mm=Deflate"),
];
const INFO_ZIP: &str = "Info-ZIP";
const TINY_NAME: &str = "secret.txt";
const TINY_TEXT: &[u8] = b"attack at dawn, attack at dawn, attack at dawn; bring the ladder\n";
const MANY_FILES: usize = 300;
const LARGE_FILE: usize = 17 << 20;

fn fixture(name: &str) -> PathBuf {
    Path::new(DATA).join(name)
}

fn every_fixture() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(DATA)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".zip"))
        .collect();
    names.sort();
    names
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
    serde_json::from_str(&fs::read_to_string(fixture("trees.json")).unwrap()).unwrap()
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

fn seven_zip_tree(binary: &Path, archive: &Path) -> BTreeMap<String, String> {
    let work = tempfile::tempdir().unwrap();
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(work.path().join("out"));
    let run = Command::new(binary)
        .args(["x", "-y", "-pzipper"])
        .arg(flag)
        .arg(archive)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "7-Zip failed on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&run.stdout)
    );
    tree(&work.path().join("out"))
}

fn extract(
    archive: &Path,
    destination: &Path,
    engine: Engine,
    password: Option<&[u8]>,
    existing: Existing,
    paths: &[String],
) -> anyhow::Result<()> {
    zipper::extract_archive(
        archive,
        destination,
        ExtractOptions {
            engine,
            password,
            existing,
            paths,
            ..Default::default()
        },
    )
}

#[derive(Clone, Debug)]
struct Record {
    central: usize,
    local: usize,
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: usize,
    central_extra: std::ops::Range<usize>,
    local_extra: std::ops::Range<usize>,
    data: usize,
}

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn records(zip: &[u8]) -> Vec<Record> {
    let end = (0..=zip.len() - 22)
        .rev()
        .find(|&at| zip[at..].starts_with(b"PK\x05\x06"))
        .unwrap();
    let count = le16(zip, end + 10) as usize;
    let mut at = le32(zip, end + 16) as usize;
    (0..count)
        .map(|_| {
            assert!(zip[at..].starts_with(b"PK\x01\x02"));
            let [name, extra, comment] = [28, 30, 32].map(|field| le16(zip, at + field) as usize);
            let local = le32(zip, at + 42) as usize;
            let local_name = le16(zip, local + 26) as usize;
            let local_extra = le16(zip, local + 28) as usize;
            let record = Record {
                central: at,
                local,
                name: String::from_utf8(zip[at + 46..at + 46 + name].to_vec()).unwrap(),
                flags: le16(zip, at + 8),
                method: le16(zip, at + 10),
                crc: le32(zip, at + 16),
                compressed: le32(zip, at + 20) as usize,
                central_extra: at + 46 + name..at + 46 + name + extra,
                local_extra: local + 30 + local_name..local + 30 + local_name + local_extra,
                data: local + 30 + local_name + local_extra,
            };
            at += 46 + name + extra + comment;
            record
        })
        .collect()
}

fn only_file(zip: &[u8]) -> Record {
    let mut files: Vec<Record> = records(zip)
        .into_iter()
        .filter(|record| !record.name.ends_with('/'))
        .collect();
    assert_eq!(files.len(), 1);
    files.remove(0)
}

fn aes_field_at(zip: &[u8], extra: std::ops::Range<usize>) -> Option<usize> {
    let mut at = extra.start;
    while at + 4 <= extra.end {
        let length = le16(zip, at + 2) as usize;
        if le16(zip, at) == AES_FIELD {
            return Some(at + 4);
        }
        at += 4 + length;
    }
    None
}

fn salt_len(zip: &[u8], record: &Record) -> usize {
    let field = aes_field_at(zip, record.central_extra.clone()).unwrap();
    4 + 4 * zip[field + 4] as usize
}

fn with_crc(zip: &[u8], record: &Record, crc: u32) -> Vec<u8> {
    let mut patched = zip.to_vec();
    patched[record.central + 16..record.central + 20].copy_from_slice(&crc.to_le_bytes());
    patched[record.local + 14..record.local + 18].copy_from_slice(&crc.to_le_bytes());
    patched
}

fn listing(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_fails_cleanly(zip: &[u8], password: Option<&[u8]>, case: &str) -> Vec<String> {
    let mut errors = Vec::new();
    for engine in ENGINES {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join("case.zip");
        fs::write(&archive, zip).unwrap();
        let kept = work.path().join("kept");
        fs::create_dir(&kept).unwrap();
        fs::write(kept.join(TINY_NAME), b"the file that was here").unwrap();
        let before = tree(&kept);
        for (destination, existing) in [
            (kept.clone(), Existing::Overwrite),
            (work.path().join("fresh"), Existing::Refuse),
        ] {
            let error = extract(&archive, &destination, engine, password, existing, &[])
                .expect_err(&format!("{case}: {engine:?} extracted into {existing:?}"));
            errors.push(format!("{error:?}"));
            assert_eq!(
                tree(&kept),
                before,
                "{case}: {engine:?} changed the destination"
            );
            assert_eq!(
                listing(work.path()),
                ["case.zip", "kept"],
                "{case}: {engine:?} left a staging folder or output"
            );
        }
    }
    errors
}

fn assert_says_wrong_password(errors: &[String], case: &str) {
    for error in errors {
        assert!(
            error.contains("password is wrong"),
            "{case}: the error does not say the password is wrong: {error}"
        );
    }
}

#[test]
fn every_fixture_extracts_to_the_tree_7zip_gave_on_both_engines() {
    let trees = recorded_trees();
    let live = seven_zip();
    assert!(
        live.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip was found"
    );
    let names = every_fixture();
    assert_eq!(
        names.len(),
        trees.len(),
        "trees.json and the fixtures differ"
    );
    for name in names {
        let archive = fixture(&name);
        let expected = &trees[&name];
        if let Some(binary) = &live {
            assert_eq!(&seven_zip_tree(binary, &archive), expected, "{name}: 7-Zip");
        }
        for engine in ENGINES {
            let work = tempfile::tempdir().unwrap();
            let output = work.path().join("out");
            extract(
                &archive,
                &output,
                engine,
                Some(PASSWORD),
                Existing::Refuse,
                &[],
            )
            .unwrap_or_else(|error| panic!("{name}: {engine:?}: {error:?}"));
            assert_eq!(&tree(&output), expected, "{name}: {engine:?}");
        }
    }
}

#[test]
fn selected_paths_extract_only_their_entries() {
    let trees = recorded_trees();
    for name in [
        "zipcrypto-deflate.zip",
        "aes256-ppmd.zip",
        "aes128-descriptor.zip",
    ] {
        for engine in ENGINES {
            let work = tempfile::tempdir().unwrap();
            let output = work.path().join("out");
            let paths = ["sub/text.txt".to_owned()];
            extract(
                &fixture(name),
                &output,
                engine,
                Some(PASSWORD),
                Existing::Refuse,
                &paths,
            )
            .unwrap();
            let expected: BTreeMap<String, String> = trees[name]
                .iter()
                .filter(|(path, _)| *path == "sub" || *path == "sub/text.txt")
                .map(|(path, kind)| (path.clone(), kind.clone()))
                .collect();
            assert_eq!(tree(&output), expected, "{name}: {engine:?}");
        }
    }
}

#[test]
fn integrity_checks_need_and_use_the_password() {
    for name in ["zipcrypto-stored.zip", "aes192-lzma.zip"] {
        let archive = fixture(name);
        let read = |password| {
            zipper::native_zip::read_archive_with_options(
                &archive,
                None,
                zipper::native_zip::ReadOptions {
                    password,
                    ..Default::default()
                },
            )
        };
        assert!(read(Some(PASSWORD)).is_ok(), "{name}");
        let error = read(Some(WRONG)).unwrap_err();
        assert!(format!("{error:?}").contains("password is wrong"), "{name}");
        assert!(read(None).unwrap_err().is::<PasswordNeeded>(), "{name}");
        let listed = zipper::native_zip::list(&archive).unwrap();
        assert!(listed.iter().any(|entry| entry.name == "sub/text.txt"));
    }
}

#[test]
fn a_wrong_password_changes_nothing_for_every_scheme() {
    for name in every_fixture() {
        let zip = fs::read(fixture(&name)).unwrap();
        let errors = assert_fails_cleanly(&zip, Some(WRONG), &name);
        assert_says_wrong_password(&errors, &name);
        for error in &errors {
            assert!(
                !error.contains(std::str::from_utf8(WRONG).unwrap()),
                "{name}: the error shows the password"
            );
        }
    }
}

#[test]
fn a_destination_that_cannot_be_used_fails_before_the_password_is_asked_for() {
    let work = tempfile::tempdir().unwrap();
    let taken = work.path().join("taken");
    fs::create_dir(&taken).unwrap();
    fs::write(taken.join("keep.txt"), b"kept").unwrap();
    for engine in ENGINES {
        let error = extract(
            &fixture("tiny-aes128.zip"),
            &taken,
            engine,
            None,
            Existing::Refuse,
            &[],
        )
        .unwrap_err();
        assert!(!error.is::<PasswordNeeded>(), "{engine:?}: {error:?}");
        assert_eq!(listing(&taken), ["keep.txt"]);
    }
    let run = zipper_command(&[
        "extract",
        fixture("tiny-aes128.zip").to_str().unwrap(),
        "-o",
        taken.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success());
    assert!(!stderr.contains("encrypted"), "{stderr}");
}

#[test]
fn a_password_for_an_archive_zipper_does_not_decrypt_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), b"a").unwrap();
    let tar = work.path().join("plain.tar");
    let made = Command::new("tar")
        .arg("-cf")
        .arg(&tar)
        .arg("-C")
        .arg(&source)
        .arg("a.txt")
        .status()
        .unwrap();
    assert!(made.success());
    let output = work.path().join("out");
    let error = zipper::extract_archive(
        &tar,
        &output,
        ExtractOptions {
            password: Some(PASSWORD),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error}").contains("--password"), "{error:?}");
    assert!(!output.exists());
    for command in [
        vec![
            "--password",
            "zipper",
            "extract",
            tar.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ],
        vec!["--password", "zipper", "test", tar.to_str().unwrap()],
    ] {
        let run = zipper_command(&command);
        assert!(!run.status.success(), "{command:?}");
        assert!(String::from_utf8_lossy(&run.stderr).contains("--password"));
    }
}

#[test]
fn no_password_fails_before_anything_is_written_and_asks_for_one() {
    for name in ["tiny-zipcrypto.zip", "tiny-aes256.zip"] {
        let zip = fs::read(fixture(name)).unwrap();
        let errors = assert_fails_cleanly(&zip, None, name);
        for error in errors {
            assert!(
                error.contains("is encrypted; give its password"),
                "{name}: {error}"
            );
        }
        let work = tempfile::tempdir().unwrap();
        let error = extract(
            &fixture(name),
            &work.path().join("out"),
            Engine::Auto,
            None,
            Existing::Refuse,
            &[],
        )
        .unwrap_err();
        assert!(error.is::<PasswordNeeded>(), "{name}: {error:?}");
    }
}

fn zip_crypto_reference_check(password: &[u8], header: &[u8]) -> u8 {
    let crc = |value: u32, byte: u8| {
        let mut crc = value ^ u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
        crc
    };
    let mut keys = [0x1234_5678u32, 0x2345_6789, 0x3456_7890];
    let update = |keys: &mut [u32; 3], byte: u8| {
        keys[0] = crc(keys[0], byte);
        keys[1] = (keys[1].wrapping_add(keys[0] & 0xff))
            .wrapping_mul(134_775_813)
            .wrapping_add(1);
        keys[2] = crc(keys[2], (keys[1] >> 24) as u8);
    };
    for &byte in password {
        update(&mut keys, byte);
    }
    let mut last = 0;
    for &byte in header {
        let temp = (keys[2] | 2) & 0xffff;
        last = byte ^ ((temp * (temp ^ 1)) >> 8) as u8;
        update(&mut keys, last);
    }
    last
}

#[test]
fn a_wrong_zip_crypto_password_that_passes_the_check_byte_is_stopped_by_the_crc() {
    for name in [
        "tiny-zipcrypto.zip",
        "tiny-zipcrypto-descriptor.zip",
        "zipcrypto-stored.zip",
    ] {
        let zip = fs::read(fixture(name)).unwrap();
        let file = records(&zip)
            .into_iter()
            .rfind(|record| record.compressed > 12)
            .unwrap();
        let header = &zip[file.data..file.data + 12];
        assert_eq!(zip_crypto_reference_check(PASSWORD, header), {
            if file.flags & 8 != 0 {
                zip[file.local + 11]
            } else {
                (file.crc >> 24) as u8
            }
        });
        let check = zip_crypto_reference_check(PASSWORD, header);
        let lucky = (0..100_000)
            .map(|number| format!("wrong-{number}").into_bytes())
            .find(|candidate| zip_crypto_reference_check(candidate, header) == check)
            .expect("one password in 256 passes the check byte");
        let only_that_entry = [file.name.clone()];
        for engine in ENGINES {
            let work = tempfile::tempdir().unwrap();
            let error = extract(
                &fixture(name),
                &work.path().join("out"),
                engine,
                Some(&lucky),
                Existing::Refuse,
                &only_that_entry,
            )
            .unwrap_err();
            let error = format!("{error:?}");
            assert!(error.contains("password is wrong"), "{name}: {error}");
            assert!(
                error.contains("CRC") || error.contains("invalid") || error.contains("size"),
                "{name}: the check byte passed, so the data check must refuse it: {error}"
            );
            assert_eq!(listing(work.path()), Vec::<String>::new(), "{name}");
        }
    }
}

#[test]
fn a_wrong_aes_password_that_passes_the_verifier_is_stopped_by_the_authentication_code() {
    for name in ["tiny-aes128.zip", "tiny-aes256.zip", "tiny-aes256-ae1.zip"] {
        let mut zip = fs::read(fixture(name)).unwrap();
        let file = only_file(&zip);
        let salt = salt_len(&zip, &file);
        let mut derived = vec![0; 2 * 2 * salt + 2];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(
            WRONG,
            &zip[file.data..file.data + salt],
            1000,
            &mut derived,
        );
        let verifier = file.data + salt;
        zip[verifier..verifier + 2].copy_from_slice(&derived[4 * salt..]);
        let errors = assert_fails_cleanly(&zip, Some(WRONG), name);
        assert_says_wrong_password(&errors, name);
        for error in &errors {
            assert!(
                error.contains("authentication code") && error.contains("is damaged"),
                "{name}: {error}"
            );
        }
    }
}

#[test]
fn damage_at_every_byte_of_an_encrypted_entry_changes_nothing() {
    let names = TINY_FIXTURES.iter().map(|tiny| tiny.0);
    for name in names.chain(AE1_FIXTURES.iter().skip(1).map(|ae1| ae1.0)) {
        let zip = fs::read(fixture(name)).unwrap();
        let file = only_file(&zip);
        for at in file.data..file.data + file.compressed {
            for flip in [0x01, 0x80] {
                let mut damaged = zip.clone();
                damaged[at] ^= flip;
                let case = format!(
                    "{name}: byte {} of {} xor {flip:#x}",
                    at - file.data,
                    file.compressed
                );
                let errors = assert_fails_cleanly(&damaged, Some(PASSWORD), &case);
                assert_says_wrong_password(&errors, &case);
            }
        }
        if file.method != AES_METHOD
            || zip[aes_field_at(&zip, file.central_extra.clone()).unwrap()] == 1
        {
            let damaged = with_crc(&zip, &file, file.crc ^ 1);
            assert_fails_cleanly(&damaged, Some(PASSWORD), &format!("{name}: CRC"));
        }
    }
}

#[test]
fn ae2_entries_ignore_the_crc_field_and_ae1_entries_check_it() {
    for (name, expect_ok) in [("tiny-aes256.zip", true), ("tiny-aes256-ae1.zip", false)] {
        let zip = fs::read(fixture(name)).unwrap();
        let file = only_file(&zip);
        let patched = with_crc(&zip, &file, file.crc ^ 0x0100);
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join("case.zip");
        fs::write(&archive, &patched).unwrap();
        for engine in ENGINES {
            let output = work.path().join(format!("{engine:?}"));
            let result = extract(
                &archive,
                &output,
                engine,
                Some(PASSWORD),
                Existing::Refuse,
                &[],
            );
            assert_eq!(result.is_ok(), expect_ok, "{name}: {engine:?}: {result:?}");
        }
    }
}

struct Built {
    name: &'static str,
    flags: u16,
    method: u16,
    crc: u32,
    size: u32,
    payload: Vec<u8>,
    local_extra: Vec<u8>,
    central_extra: Vec<u8>,
}

fn build(entries: &[Built]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for entry in entries {
        offsets.push(out.len() as u32);
        out.extend_from_slice(b"PK\x03\x04\x33\x00");
        out.extend_from_slice(&entry.flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&[0x0e, 0x13, 0x21, 0x5b]);
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&(entry.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entry.local_extra.len() as u16).to_le_bytes());
        out.extend_from_slice(entry.name.as_bytes());
        out.extend_from_slice(&entry.local_extra);
        out.extend_from_slice(&entry.payload);
    }
    let directory = out.len() as u32;
    for (entry, offset) in entries.iter().zip(offsets) {
        out.extend_from_slice(b"PK\x01\x02\x3f\x00\x33\x00");
        out.extend_from_slice(&entry.flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&[0x0e, 0x13, 0x21, 0x5b]);
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&(entry.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entry.central_extra.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0; 10]);
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(entry.name.as_bytes());
        out.extend_from_slice(&entry.central_extra);
    }
    let size = out.len() as u32 - directory;
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn aes_extra(body: &[u8]) -> Vec<u8> {
    let mut field = AES_FIELD.to_le_bytes().to_vec();
    field.extend_from_slice(&(body.len() as u16).to_le_bytes());
    field.extend_from_slice(body);
    field
}

fn tiny_entry(name: &str) -> Built {
    let zip = fs::read(fixture(name)).unwrap();
    let file = only_file(&zip);
    let field = aes_field_at(&zip, file.central_extra.clone())
        .map(|at| aes_extra(&zip[at..at + 7]))
        .unwrap_or_default();
    Built {
        name: TINY_NAME,
        flags: file.flags,
        method: file.method,
        crc: file.crc,
        size: TINY_TEXT.len() as u32,
        payload: zip[file.data..file.data + file.compressed].to_vec(),
        local_extra: field.clone(),
        central_extra: field,
    }
}

#[test]
fn the_entry_builder_reproduces_a_working_encrypted_entry() {
    for name in [
        "tiny-zipcrypto.zip",
        "tiny-aes128.zip",
        "tiny-aes256-ae1.zip",
    ] {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join("case.zip");
        fs::write(&archive, build(&[tiny_entry(name)])).unwrap();
        for engine in ENGINES {
            let output = work.path().join(format!("{engine:?}"));
            extract(
                &archive,
                &output,
                engine,
                Some(PASSWORD),
                Existing::Refuse,
                &[],
            )
            .unwrap();
            assert_eq!(
                fs::read(output.join(TINY_NAME)).unwrap(),
                TINY_TEXT,
                "{name}"
            );
        }
    }
}

#[test]
fn aes_fields_that_lie_are_refused() {
    let entry = || tiny_entry("tiny-aes256.zip");
    let honest = entry().central_extra[4..].to_vec();
    let body = |at: usize, value: u8| {
        let mut body = honest.clone();
        body[at] = value;
        aes_extra(&body)
    };
    let both = |extra: Vec<u8>| Built {
        local_extra: extra.clone(),
        central_extra: extra,
        ..entry()
    };
    let cases: Vec<(&str, Built)> = vec![
        ("vendor version 0", both(body(0, 0))),
        ("vendor version 3", both(body(0, 3))),
        ("another vendor", both(body(3, b'X'))),
        ("strength 0", both(body(4, 0))),
        ("strength 4", both(body(4, 4))),
        ("strength 128 that is really 256", both(body(4, 1))),
        ("strength 192 that is really 256", both(body(4, 2))),
        ("AES inside AES", both(body(5, AES_METHOD as u8))),
        ("an unknown real method", both(body(6, 0x42))),
        ("stored that is really Deflate", both(body(5, 0))),
        ("AE-1 stored that is really Deflate", {
            let ae1 = tiny_entry("tiny-aes256-ae1.zip");
            let mut lie = ae1.central_extra[4..].to_vec();
            lie[5] = 0;
            Built {
                local_extra: aes_extra(&lie),
                central_extra: aes_extra(&lie),
                ..ae1
            }
        }),
        ("a short field", both(aes_extra(&honest[..6]))),
        (
            "a long field",
            both(aes_extra(&[&honest[..], &[0]].concat())),
        ),
        ("an empty field", both(aes_extra(&[]))),
        (
            "two fields",
            both([aes_extra(&honest), aes_extra(&honest)].concat()),
        ),
        ("no field", both(Vec::new())),
        (
            "a local field that names another strength",
            Built {
                local_extra: body(4, 1),
                ..entry()
            },
        ),
        (
            "a local field that names another method",
            Built {
                local_extra: body(5, 0),
                ..entry()
            },
        ),
        (
            "no local field",
            Built {
                local_extra: Vec::new(),
                ..entry()
            },
        ),
        (
            "the AES method without the encryption flag",
            Built {
                flags: 0,
                ..entry()
            },
        ),
        (
            "PKWARE strong encryption",
            Built {
                flags: 0x41,
                ..entry()
            },
        ),
        (
            "a declared size that is too large",
            Built {
                size: TINY_TEXT.len() as u32 + 1,
                ..entry()
            },
        ),
        (
            "a declared size that is too small",
            Built {
                size: TINY_TEXT.len() as u32 - 1,
                ..entry()
            },
        ),
    ];
    let authentic_but_misread = [
        "stored that is really Deflate",
        "a declared size that is too large",
        "a declared size that is too small",
    ];
    for (case, built) in cases {
        let errors = assert_fails_cleanly(&build(&[built]), Some(PASSWORD), case);
        if authentic_but_misread.contains(&case) {
            for error in errors {
                assert!(
                    !error.contains("password is wrong"),
                    "{case}: the authentication code holds, so the password is right: {error}"
                );
            }
        }
    }
}

#[test]
fn entries_too_short_for_their_header_and_code_are_refused() {
    for name in ["tiny-zipcrypto.zip", "tiny-aes128.zip", "tiny-aes256.zip"] {
        let full = tiny_entry(name);
        let overhead = if full.method == AES_METHOD {
            let strength = full.central_extra[8] as usize;
            4 + 4 * strength + 2 + AES_CODE
        } else {
            12
        };
        for kept in [0, 1, overhead / 2, overhead - 1, overhead, overhead + 1] {
            let built = Built {
                payload: full.payload[..kept].to_vec(),
                ..tiny_entry(name)
            };
            assert_fails_cleanly(
                &build(&[built]),
                Some(PASSWORD),
                &format!("{name}: {kept} bytes"),
            );
        }
    }
}

#[test]
fn local_encryption_flags_must_agree_with_the_central_directory() {
    for name in ["tiny-zipcrypto.zip", "tiny-aes128.zip"] {
        let zip = fs::read(fixture(name)).unwrap();
        let file = only_file(&zip);
        let mut local_clear = zip.clone();
        local_clear[file.local + 6] &= !1;
        assert_fails_cleanly(
            &local_clear,
            Some(PASSWORD),
            &format!("{name}: local clear"),
        );
        let mut central_clear = zip.clone();
        central_clear[file.central + 8] &= !1;
        assert_fails_cleanly(
            &central_clear,
            Some(PASSWORD),
            &format!("{name}: central clear"),
        );
    }
}

#[test]
fn selections_that_leave_out_every_encrypted_entry_need_no_password() {
    let text = b"a plain entry, larger than the encrypted one\n".repeat(4);
    let plain = Built {
        name: "plain.txt",
        flags: 0,
        method: 0,
        crc: crc32fast::hash(&text),
        size: text.len() as u32,
        payload: text,
        local_extra: Vec::new(),
        central_extra: Vec::new(),
    };
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("mixed.zip");
    fs::write(&archive, build(&[plain, tiny_entry("tiny-aes192.zip")])).unwrap();
    for engine in ENGINES {
        let output = work.path().join(format!("{engine:?}"));
        extract(
            &archive,
            &output,
            engine,
            None,
            Existing::Refuse,
            &["plain.txt".to_owned()],
        )
        .unwrap();
        assert_eq!(listing(&output), ["plain.txt"]);
        let all = work.path().join(format!("{engine:?}-all"));
        let files_reported = std::sync::atomic::AtomicU64::new(0);
        let count = |progress: zipper::Progress| {
            files_reported.fetch_max(progress.files_done, std::sync::atomic::Ordering::Relaxed);
        };
        let error = zipper::extract_archive(
            &archive,
            &all,
            ExtractOptions {
                engine,
                threads: Some(1),
                control: zipper::Control {
                    progress: Some(&count),
                    cancel: None,
                },
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.is::<PasswordNeeded>());
        assert!(!all.exists());
        assert_eq!(
            files_reported.into_inner(),
            0,
            "{engine:?} wrote the plain entry before it asked for a password"
        );
    }
}

#[cfg(unix)]
const PTY_DRIVER: &str = r#"
import os, pty, select, sys, time
zipper, archive, output, typed = sys.argv[1], sys.argv[2], sys.argv[3], bytes.fromhex(sys.argv[4])
pid, terminal = pty.fork()
if pid == 0:
    os.execv("/bin/sh", ["sh", "-c",
        f"trap 'echo TRAPPED' INT; '{zipper}' extract '{archive}' -o '{output}'; echo RC=$?; stty -a"])
seen = b""
def read_until(token, seconds):
    global seen
    deadline = time.time() + seconds
    while token not in seen and time.time() < deadline:
        ready, _, _ = select.select([terminal], [], [], 0.2)
        if ready:
            try:
                seen += os.read(terminal, 4096)
            except OSError:
                break
read_until(b"Password: ", 20)
time.sleep(0.3)
os.write(terminal, typed)
read_until(b"speed", 20)
read_until(b"\x00never", 1)
sys.stdout.write(seen.decode(errors="replace"))
"#;

#[cfg(unix)]
fn at_the_prompt(typed: &[u8], output: &Path) -> String {
    let hex: String = typed.iter().map(|byte| format!("{byte:02x}")).collect();
    let run = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(PTY_DRIVER)
        .arg(env!("CARGO_BIN_EXE_zipper"))
        .arg(fixture("aes128-deflate.zip"))
        .arg(output)
        .arg(hex)
        .output()
        .expect("python3 drives the terminal");
    String::from_utf8_lossy(&run.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn the_password_prompt_restores_the_terminal_on_every_exit() {
    let cases: [(&str, &[u8], &str); 5] = [
        ("Ctrl-C", b"abc\x03", "RC=130"),
        ("Ctrl-D", b"\x04", "RC=1"),
        ("a wrong password", b"nope\r", "RC=1"),
        ("a corrected password", b"zipz\x7fper\n", "RC=0"),
        ("Ctrl-Z", b"zip\x1aper\r", "RC=0"),
    ];
    for (case, typed, exit) in cases {
        let work = tempfile::tempdir().unwrap();
        let output = work.path().join("out");
        let transcript = at_the_prompt(typed, &output);
        let words: Vec<&str> = transcript.split_whitespace().collect();
        assert!(words.contains(&exit), "{case}: {transcript}");
        for setting in ["echo", "icanon", "isig"] {
            assert!(
                words.contains(&setting) && !words.contains(&format!("-{setting}").as_str()),
                "{case}: the terminal kept {setting} off: {transcript}"
            );
        }
        assert!(!transcript.contains("TRAPPED"), "{case}: {transcript}");
        assert!(
            !transcript.contains("nope") && !transcript.contains("zipper\r"),
            "{case}: echoed"
        );
        assert_eq!(output.exists(), exit == "RC=0", "{case}");
    }
}

fn zipper_command(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_zipper"))
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn the_command_line_takes_a_password_and_refuses_to_ask_without_a_terminal() {
    let work = tempfile::tempdir().unwrap();
    let archive = fixture("aes256-deflate.zip");
    let archive = archive.to_str().unwrap();
    let out = work.path().join("out");
    let out = out.to_str().unwrap();
    let run = zipper_command(&["--password", "zipper", "extract", archive, "-o", out]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(tree(Path::new(out)), recorded_trees()["aes256-deflate.zip"]);
    let run = zipper_command(&["test", "--password", "zipper", archive]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let missing = work.path().join("missing");
    let missing = missing.to_str().unwrap();
    let run = zipper_command(&["extract", archive, "-o", missing]);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(!run.status.success());
    assert!(
        stderr.contains("is encrypted") && stderr.contains("not a terminal"),
        "{stderr}"
    );
    let secret = "Never-Show-This-Password";
    let hyphenated = "--Never-Show-This-Password";
    for command in [
        vec!["--password", secret, "extract", archive, "-o", missing],
        vec!["--password", secret, "test", archive],
        vec!["--password", hyphenated, "extract", archive, "-o", missing],
        vec!["extract", archive, "-o", missing, "--password", hyphenated],
        vec!["test", "--password", "-x", archive],
    ] {
        let run = zipper_command(&command);
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(!run.status.success());
        assert!(output.contains("password is wrong"), "{output}");
        assert!(
            !output.contains(secret),
            "the password appears in the output"
        );
    }
    assert!(!Path::new(missing).exists());
    let run = zipper_command(&[
        "--zip-engine",
        "backend",
        "--password",
        "zipper",
        "extract",
        archive,
        "-o",
        missing,
    ]);
    assert!(
        !run.status.success(),
        "the 7-Zip backend must refuse --password"
    );
}

fn write_source(root: &Path) {
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("a.txt"), b"hello world\n").unwrap();
    fs::write(root.join("empty"), b"").unwrap();
    let mut state = 0x2545_f491u32;
    let random: Vec<u8> = (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    fs::write(root.join("sub/random.bin"), random).unwrap();
    let text: String = (0..2000)
        .map(|line| format!("compressible line {line}\n"))
        .collect();
    fs::write(root.join("sub/text.txt"), text).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
}

fn seven_zip_write(binary: &Path, source: &Path, archive: &Path, switches: &[&str]) {
    let mut command = Command::new(binary);
    command
        .args(["a", "-tzip", "-pzipper", "-snl", "-bso0", "-bsp0"])
        .args(switches)
        .current_dir(source);
    if switches.contains(&"-so") {
        command.arg(".").stdout(fs::File::create(archive).unwrap());
    } else {
        command.arg(archive).arg(".");
    }
    assert!(command.status().unwrap().success(), "{}", archive.display());
}

fn info_zip_write(source: &Path, archive: &Path, switches: &[&str]) {
    let written = Command::new("zip")
        .args(["-q", "-r", "-y", "-P", "zipper"])
        .args(switches)
        .arg(archive)
        .arg(".")
        .current_dir(source)
        .status();
    assert!(
        written.is_ok_and(|status| status.success()),
        "Info-ZIP zip {}",
        archive.display()
    );
}

fn rewrite_as_ae1(zip: &[u8], source: &Path) -> Vec<u8> {
    let mut patched = zip.to_vec();
    for record in records(zip)
        .into_iter()
        .filter(|record| record.method == AES_METHOD)
    {
        assert_eq!(
            record.flags & 8,
            0,
            "rewrite only archives without data descriptors"
        );
        let path = source.join(&record.name);
        let content = match fs::read_link(&path) {
            Ok(target) => target.to_string_lossy().into_owned().into_bytes(),
            Err(_) => fs::read(&path).unwrap(),
        };
        patched = with_crc(&patched, &record, crc32fast::hash(&content));
        for extra in [record.central_extra.clone(), record.local_extra.clone()] {
            let field = aes_field_at(&patched, extra).unwrap();
            patched[field..field + 2].copy_from_slice(&1u16.to_le_bytes());
        }
    }
    patched
}

#[test]
#[ignore = "rewrites the fixtures with 7-Zip and Info-ZIP when ZIPPER_WRITE_ENCRYPTED_ZIPS=1"]
fn write_fixtures() {
    assert!(std::env::var_os("ZIPPER_WRITE_ENCRYPTED_ZIPS").is_some());
    let binary = seven_zip().expect("7-Zip writes the fixtures");
    let work = tempfile::tempdir().unwrap();
    let source = work.path().join("source");
    write_source(&source);
    let out = Path::new(DATA);
    fs::create_dir_all(out).unwrap();
    for name in every_fixture() {
        fs::remove_file(fixture(&name)).unwrap();
    }
    for (name, switches) in SEVEN_ZIP_FIXTURES {
        seven_zip_write(&binary, &source, &out.join(name), switches);
    }
    let tiny = work.path().join("tiny");
    fs::create_dir_all(&tiny).unwrap();
    fs::write(tiny.join(TINY_NAME), TINY_TEXT).unwrap();
    for (name, method, compression) in TINY_FIXTURES {
        if *method == INFO_ZIP {
            info_zip_write(&tiny, &out.join(name), &[compression]);
        } else {
            seven_zip_write(&binary, &tiny, &out.join(name), &[method, compression]);
        }
    }
    for (name, from) in AE1_FIXTURES {
        let from_source = if from.starts_with("tiny") {
            &tiny
        } else {
            &source
        };
        let zip = fs::read(out.join(from)).unwrap();
        fs::write(out.join(name), rewrite_as_ae1(&zip, from_source)).unwrap();
    }
    let many = work.path().join("many");
    fs::create_dir_all(&many).unwrap();
    for number in 0..MANY_FILES {
        fs::write(
            many.join(format!("file-{number:03}.txt")),
            format!("file {number}\n").repeat(number % 7),
        )
        .unwrap();
    }
    seven_zip_write(
        &binary,
        &many,
        &out.join("aes256-many.zip"),
        &["-mem=AES256"],
    );
    let large = work.path().join("large");
    fs::create_dir_all(&large).unwrap();
    let pattern = b"large file line\n".repeat(LARGE_FILE / 16);
    fs::write(large.join("large.bin"), pattern).unwrap();
    seven_zip_write(
        &binary,
        &large,
        &out.join("aes256-large.zip"),
        &["-mem=AES256", "-mx=9"],
    );
    seven_zip_write(
        &binary,
        &large,
        &out.join("zipcrypto-large.zip"),
        &["-mem=ZipCrypto", "-mx=9"],
    );
    info_zip_write(&source, &out.join("infozip-zipcrypto.zip"), &[]);
    let trees: BTreeMap<String, BTreeMap<String, String>> = every_fixture()
        .into_iter()
        .map(|name| {
            let tree = seven_zip_tree(&binary, &fixture(&name));
            (name, tree)
        })
        .collect();
    fs::write(
        out.join("trees.json"),
        serde_json::to_string_pretty(&trees).unwrap() + "\n",
    )
    .unwrap();
}
