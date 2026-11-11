//! Every refused format and method is refused by every engine, through the
//! CLI and the library, with an error that names the format or method and
//! nothing written. A fake 7-Zip, named by `ZIPPER_7Z` or found on `PATH`,
//! is never started for any of them.
#![cfg(unix)]
#[path = "common/sevenzip.rs"]
mod sevenzip;

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
const SECTOR: usize = 512;
const CLI_ENGINES: [Option<&str>; 3] = [None, Some("native"), Some("backend")];

struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    words: &'static str,
    backend_words: &'static str,
}

fn stored(path: &str) -> Vec<u8> {
    let bytes = fs::read(Path::new(DATA).join(path)).unwrap();
    if !path.ends_with(".xz") {
        return bytes;
    }
    let mut plain = Vec::new();
    liblzma::read::XzDecoder::new(&bytes[..])
        .read_to_end(&mut plain)
        .unwrap();
    plain
}

fn padded(start: &[u8], at: usize, length: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; length];
    bytes[at..at + start.len()].copy_from_slice(start);
    bytes
}

fn ntfs_volume() -> Vec<u8> {
    let mut volume = vec![0u8; 64 * SECTOR];
    volume[..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    volume[3..11].copy_from_slice(b"NTFS    ");
    volume[11..13].copy_from_slice(&512u16.to_le_bytes());
    volume[13] = 8;
    volume[21] = 0xF8;
    volume[SECTOR - 2..SECTOR].copy_from_slice(&[0x55, 0xAA]);
    volume
}

fn lone_partition(kind: u8, volume: &[u8]) -> Vec<u8> {
    let start = 64usize;
    let sectors = volume.len() / SECTOR;
    let mut disk = vec![0u8; (start + sectors) * SECTOR];
    let entry = &mut disk[0x1BE..0x1BE + 16];
    entry[1..4].copy_from_slice(&[0, 2, 0]);
    entry[4] = kind;
    entry[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    entry[8..12].copy_from_slice(&(start as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
    disk[SECTOR - 2..SECTOR].copy_from_slice(&[0x55, 0xAA]);
    disk[start * SECTOR..].copy_from_slice(volume);
    disk
}

fn case(name: &'static str, bytes: Vec<u8>, words: &'static str) -> Case {
    Case {
        name,
        bytes,
        words,
        backend_words: words,
    }
}

fn cases() -> Vec<Case> {
    let chm = "Zipper does not read CHM and Help 2 files";
    let compound = "Zipper does not read compound files such as MSI, DOC and XLS";
    let fat = "Zipper does not read FAT volumes";
    let ntfs = "Zipper does not read NTFS volumes";
    let hfs = "Zipper does not read HFS+ and HFSX volumes";
    let apfs = "Zipper does not read APFS containers";
    let compound_magic = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
    vec![
        case("help.chm", stored("refused/chm.chm.xz"), chm),
        case("help.hxs", padded(b"ITOLITLS", 0, 4096), chm),
        case("storages.doc", stored("refused/compound.doc.xz"), compound),
        case(
            "installer.msi",
            stored("refused/installer.msi.xz"),
            compound,
        ),
        case("patch.msp", padded(compound_magic, 0, 4096), compound),
        case(
            "dmg-zlib.dmg",
            stored("parity/archives/dmg-zlib.dmg"),
            "Zipper does not read DMG images",
        ),
        case("hfs.img", stored("parity/archives/hfs.img.stored.xz"), hfs),
        case("hfsx.img", padded(b"HX\0\x05", 1024, 8192), hfs),
        case(
            "apm.img",
            stored("parity/archives/apm.img.stored.xz"),
            "Zipper does not read Apple partition maps",
        ),
        case(
            "apfs.img",
            stored("parity/archives/apfs.img.stored.xz"),
            apfs,
        ),
        case("fat.img", stored("parity/archives/fat.img.stored.xz"), fat),
        case(
            "gpt-fat.img",
            stored("parity/archives/gpt.img.stored.xz"),
            fat,
        ),
        case(
            "mbr-fat.img",
            stored("parity/archives/mbr.img.stored.xz"),
            fat,
        ),
        case("ntfs.img", ntfs_volume(), ntfs),
        case("ntfs-mbr.img", lone_partition(0x07, &ntfs_volume()), ntfs),
        case("raw-fat.vhd", stored("disk-corpus/raw-fat.vhd.xz"), fat),
        case("fat12.qcow", stored("disk-corpus/fat12.qcow.xz"), fat),
        case("hfs-gpt.qcow2", stored("disk-corpus/hfs-gpt.qcow2.xz"), hfs),
        case("apfs-gpt.vdi", stored("disk-corpus/apfs-gpt.vdi.xz"), apfs),
        case(
            "ntfs-mbr.vmdk",
            stored("disk-corpus/ntfs-mbr.vmdk.xz"),
            ntfs,
        ),
        case(
            "hello.b64",
            b"aGVsbG8sIGEgc21hbGwgZmlsZQo=\n".to_vec(),
            "Zipper does not read Base64 files",
        ),
        Case {
            name: "lzx.cab",
            bytes: stored("refused/lzx.cab"),
            words: "CAB folder is compressed with LZX, which Zipper does not decode",
            backend_words: "Zipper does not read CAB folders compressed with LZX",
        },
        Case {
            name: "lzx.wim",
            bytes: stored("refused/lzx.wim.xz"),
            words: "WIM image is compressed with LZX, which Zipper does not decode",
            backend_words: "Zipper does not read WIM images compressed with LZX",
        },
        Case {
            name: "lzms.wim",
            bytes: stored("refused/lzms.wim.xz"),
            words: "WIM image is compressed with LZMS, which Zipper does not decode",
            backend_words: "Zipper does not read WIM and ESD images compressed with LZMS",
        },
    ]
}

struct FakeSevenZip {
    folder: tempfile::TempDir,
}

impl FakeSevenZip {
    fn new() -> FakeSevenZip {
        use std::os::unix::fs::PermissionsExt;
        let folder = tempfile::tempdir().unwrap();
        let marker = folder.path().join("started");
        for name in sevenzip::NAMES_ON_PATH {
            let script = folder.path().join(name);
            fs::write(
                &script,
                format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n", marker.display()),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        FakeSevenZip { folder }
    }

    fn binary(&self) -> PathBuf {
        self.folder.path().join(sevenzip::NAMES_ON_PATH[0])
    }

    fn path(&self) -> std::ffi::OsString {
        let mut path = self.folder.path().as_os_str().to_owned();
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(":");
            path.push(rest);
        }
        path
    }

    fn started(&self) -> Option<String> {
        fs::read_to_string(self.folder.path().join("started")).ok()
    }
}

fn leftovers(folder: &Path, archive: &str) -> Vec<String> {
    fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != archive)
        .collect()
}

fn run_cli(fake: &FakeSevenZip, by_variable: bool, args: &[&std::ffi::OsStr]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zipper"));
    command
        .args(args)
        .env("PATH", fake.path())
        .stdin(Stdio::null());
    if by_variable {
        command.env("ZIPPER_7Z", fake.binary());
    } else {
        command.env_remove("ZIPPER_7Z");
    }
    let output = command.output().unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn assert_refused_by_every_cli_engine(fake: &FakeSevenZip, by_variable: bool) {
    for case in cases() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(case.name);
        fs::write(&archive, &case.bytes).unwrap();
        let output = work.path().join("out");
        for engine in CLI_ENGINES {
            let words = if engine == Some("backend") {
                case.backend_words
            } else {
                case.words
            };
            for action in ["extract", "list", "test"] {
                let mut args: Vec<&std::ffi::OsStr> = Vec::new();
                if let Some(engine) = engine {
                    args.extend([std::ffi::OsStr::new("--zip-engine"), engine.as_ref()]);
                }
                args.extend([action.as_ref(), archive.as_os_str()]);
                if action == "extract" {
                    args.extend(["-o".as_ref(), output.as_os_str()]);
                }
                let (success, stderr) = run_cli(fake, by_variable, &args);
                let label = format!("{} {action} with {engine:?}", case.name);
                assert!(!success, "{label} succeeded");
                assert!(stderr.contains(words), "{label}: {stderr}");
                assert_eq!(
                    leftovers(work.path(), case.name),
                    Vec::<String>::new(),
                    "{label} wrote output"
                );
            }
        }
        assert_eq!(fake.started(), None, "{} reached 7-Zip", case.name);
    }
}

#[test]
fn every_refused_format_is_refused_in_every_engine_with_nothing_written() {
    let fake = FakeSevenZip::new();
    assert_refused_by_every_cli_engine(&fake, true);
    for case in cases() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(case.name);
        fs::write(&archive, &case.bytes).unwrap();
        for engine in [zipper::Engine::Native, zipper::Engine::Auto] {
            let error = zipper::extract_archive(
                &archive,
                &work.path().join("out"),
                zipper::ExtractOptions {
                    engine,
                    ..zipper::ExtractOptions::default()
                },
            )
            .unwrap_err();
            assert!(
                format!("{error:#}").contains(case.words),
                "{} with {engine:?}: {error:#}",
                case.name
            );
            assert_eq!(leftovers(work.path(), case.name), Vec::<String>::new());
        }
        let error = zipper::choose_reader(zipper::Engine::Backend, &archive).unwrap_err();
        assert!(
            error.downcast_ref::<zipper::Refused>().is_some(),
            "{}: {error:#}",
            case.name
        );
        let error = zipper::compat::run(
            "x",
            &archive,
            Some(&work.path().join("out")),
            None,
            zipper::Existing::Refuse,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains(case.backend_words),
            "{} through compat::run: {error:#}",
            case.name
        );
        assert_eq!(leftovers(work.path(), case.name), Vec::<String>::new());
    }
    assert_eq!(fake.started(), None);
}

#[test]
fn no_refused_format_reaches_a_fake_7zip_on_path() {
    let fake = FakeSevenZip::new();
    assert_refused_by_every_cli_engine(&fake, false);
    let work = tempfile::tempdir().unwrap();
    let rar = work.path().join("probe.rar");
    fs::write(&rar, b"Rar!\x1A\x07\x01\x00 not a real archive").unwrap();
    let (_, stderr) = run_cli(
        &fake,
        false,
        &[
            "extract".as_ref(),
            rar.as_os_str(),
            "-o".as_ref(),
            work.path().join("out").as_os_str(),
        ],
    );
    assert!(
        fake.started().is_some(),
        "the fake 7-Zip on PATH is the one Zipper starts for other formats: {stderr}"
    );
}
