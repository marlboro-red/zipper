//! No run of Zipper starts 7-Zip. Every file in `tests/data`, and every
//! case that once went to 7-Zip, goes to Zipper's own reader or fails with
//! an error from it or from `zipper::Refused`, while a fake 7-Zip sits first
//! on `PATH` and in `ZIPPER_7Z` and records any start.
//!
//! The fixture sweep checks every 13th file by default, so it fits every
//! push. It then skips an `.xz` file that unpacks to more than 1 MiB, whose
//! decoding is the cost, and checks only what it unpacks to.
//! `ZIPPER_NO_7ZIP_STRIDE=1` checks every file and every `.xz` file, as the
//! `[refused-signatures]` full check in `verification/coverage.txt` asks.
#![cfg(unix)]
mod common;
#[path = "common/ext_image.rs"]
mod ext_image;
#[path = "common/fake_7zip.rs"]
mod fake_7zip;
#[path = "common/lp_writer.rs"]
mod lp_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/uefi_writer.rs"]
mod uefi_writer;

use fake_7zip::Fake7Zip;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");

fn xz(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(fs::File::open(path).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn written(work: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = work.join(name);
    fs::write(&path, bytes).unwrap();
    path
}

fn fixtures() -> Vec<PathBuf> {
    walkdir::WalkDir::new(DATA)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .collect()
}

const SWEEP_THREADS: usize = 8;
const STRIDE: &str = "ZIPPER_NO_7ZIP_STRIDE";
const DEFAULT_STRIDE: usize = 13;
const SAMPLED_LARGEST_STREAM: usize = 1 << 20;

fn stride() -> usize {
    std::env::var(STRIDE).map_or(DEFAULT_STRIDE, |value| {
        value
            .parse()
            .ok()
            .filter(|stride| *stride > 0)
            .unwrap_or_else(|| panic!("{STRIDE} must be a whole number above 0, not {value:?}"))
    })
}

fn sweep_inputs(work: &Path, stride: usize) -> Vec<PathBuf> {
    let mut inputs = Vec::new();
    for (index, fixture) in fixtures().into_iter().enumerate().step_by(stride) {
        if fixture
            .extension()
            .is_none_or(|extension| extension != "xz")
        {
            inputs.push(fixture);
            continue;
        }
        let mut bytes = Vec::new();
        let unpacked = liblzma::read::XzDecoder::new(fs::File::open(&fixture).unwrap())
            .read_to_end(&mut bytes)
            .is_ok();
        if !unpacked || stride == 1 || bytes.len() <= SAMPLED_LARGEST_STREAM {
            inputs.push(fixture.clone());
        }
        if unpacked {
            let stem = fixture.file_stem().unwrap().to_string_lossy();
            inputs.push(written(work, &format!("{index}-{stem}"), &bytes));
        }
    }
    inputs
}

fn assert_no_action_starts_7zip(fake: &Fake7Zip, input: &Path) {
    let output = tempfile::tempdir().unwrap();
    let destination = output.path().join("out");
    for args in [
        vec!["test".as_ref(), input.as_os_str()],
        vec!["list".as_ref(), input.as_os_str()],
        vec![
            "extract".as_ref(),
            input.as_os_str(),
            "-o".as_ref(),
            destination.as_os_str(),
        ],
    ] {
        let run = fake.zipper(&args);
        if let Some(log) = fake.started() {
            panic!("{args:?} started 7-Zip: {log}");
        }
        assert!(
            !run.stderr.contains("7-Zip backend") && !run.stderr.contains("zip-engine"),
            "{args:?}: {}",
            run.stderr
        );
    }
    common::make_tree_deletable(output.path());
}

#[test]
fn no_fixture_and_no_unpacked_fixture_starts_7zip() {
    let fake = Fake7Zip::new(&sevenzip::NAMES_ON_PATH);
    let work = tempfile::tempdir().unwrap();
    let stride = stride();
    let inputs = sweep_inputs(work.path(), stride);
    assert!(
        inputs.len() * stride > 600,
        "found only {} files at stride {stride}",
        inputs.len()
    );
    let share = inputs.len().div_ceil(SWEEP_THREADS);
    std::thread::scope(|scope| {
        for chunk in inputs.chunks(share) {
            let fake = &fake;
            scope.spawn(move || {
                for input in chunk {
                    assert_no_action_starts_7zip(fake, input);
                }
            });
        }
    });
}

#[test]
fn a_mach_o_universal_file_with_the_other_signature_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let mut bytes = vec![0xB9, 0xFA, 0xF1, 0x0E];
    bytes.extend([0; 60]);
    let archive = written(work.path(), "other.mub", &bytes);
    Fake7Zip::new(&sevenzip::NAMES_ON_PATH).assert_refused(
        &archive,
        "Zipper does not read Mach-O universal files with the B9 FA F1 0E signature",
    );
}

#[test]
fn squashfs_images_of_other_versions_and_byte_orders_are_refused() {
    let image = fs::read(Path::new(DATA).join("squashfs-corpus/gzip.sqfs")).unwrap();
    let mut version_3 = image.clone();
    version_3[28] = 3;
    let mut big_endian = image;
    big_endian[..4].copy_from_slice(b"sqsh");
    let work = tempfile::tempdir().unwrap();
    for (name, bytes) in [("v3.sqfs", version_3), ("big.sqfs", big_endian)] {
        let archive = written(work.path(), name, &bytes);
        Fake7Zip::new(&sevenzip::NAMES_ON_PATH).assert_refused(
            &archive,
            "Zipper does not read SquashFS images other than version 4.0 in little-endian order",
        );
    }
}

#[test]
fn an_ext_volume_that_needs_journal_recovery_goes_to_the_ext_reader() {
    let mut bytes = xz(&Path::new(DATA).join("ext-corpus/ext3-2k.img.xz"));
    let layout = ext_image::Layout::parse(&bytes);
    let field = ext_image::SUPERBLOCK + 0x60;
    let value = u32::from_le_bytes(bytes[field..field + 4].try_into().unwrap());
    ext_image::put32(&mut bytes, field, value | 0x4);
    layout.seal_superblock(&mut bytes);
    let work = tempfile::tempdir().unwrap();
    let archive = written(work.path(), "recover.img", &bytes);
    Fake7Zip::new(&sevenzip::NAMES_ON_PATH).assert_refused(
        &archive,
        "the ext volume uses a journal that needs recovery, which Zipper does not read",
    );
}

#[test]
fn rar_volumes_and_encrypted_headers_go_to_the_rar_reader() {
    let work = tempfile::tempdir().unwrap();
    for (fixture, words) in [
        (
            "test_read_format_rar_encryption_header.rar",
            "RAR 4 archive has encrypted headers, which Zipper does not decrypt",
        ),
        (
            "test_read_format_rar5_encrypted_filenames.rar",
            "RAR 5 archive has encrypted headers, which Zipper does not decrypt",
        ),
        (
            "test_read_format_rar_multivolume.part0001.rar",
            "Zipper does not read RAR 4 volumes",
        ),
        (
            "test_read_format_rar5_multiarchive.part01.rar",
            "Zipper does not read RAR 5 volumes",
        ),
    ] {
        let bytes = fs::read(Path::new(DATA).join("rar-corpus").join(fixture)).unwrap();
        let archive = written(work.path(), fixture, &bytes);
        Fake7Zip::new(&sevenzip::NAMES_ON_PATH).assert_refused(&archive, words);
    }
}

#[test]
fn logical_partition_names_that_leave_the_folder_go_to_the_lp_reader() {
    use lp_writer::{Layout, build, linear, partition};
    let layout = Layout {
        partitions: vec![
            partition("/abs", vec![linear(1, b"x")]),
            partition("a/../../x", vec![linear(1, b"y")]),
        ],
        ..Layout::default()
    };
    let work = tempfile::tempdir().unwrap();
    let archive = written(work.path(), "super.img", &build(&layout));
    Fake7Zip::new(&sevenzip::NAMES_ON_PATH).assert_refused(&archive, "unsafe archive path");
}

#[test]
fn uefi_names_that_leave_the_folder_go_to_the_uefi_reader() {
    use uefi_writer::{ffs, named, pe, sections, ui, volume};
    let name = |first| named(first, 0x2222_3333_4444_5555);
    let image = volume(vec![
        ffs(&name(1), 7, &sections(&[pe(b"MZ"), ui("a/../../x")])),
        ffs(&name(2), 7, &sections(&[pe(b"MZ"), ui("..")])),
    ]);
    let work = tempfile::tempdir().unwrap();
    let archive = written(work.path(), "firmware.fd", &image);
    Fake7Zip::new(&sevenzip::NAMES_ON_PATH).assert_refused(&archive, "unsafe archive path");
    let capsule = uefi_writer::capsule(uefi_writer::CAPSULE, &image, 28);
    let archive = written(work.path(), "firmware.cap", &capsule);
    Fake7Zip::new(&sevenzip::NAMES_ON_PATH).assert_refused(&archive, "unsafe archive path");
}

#[test]
fn a_damaged_file_with_a_known_signature_goes_to_its_own_reader() {
    let work = tempfile::tempdir().unwrap();
    for (name, signature, words) in [
        ("damaged.7z", &b"7z\xBC\xAF\x27\x1C"[..], "7z"),
        ("damaged.rar", b"Rar!\x1A\x07\x01\x00", "RAR"),
        ("damaged.rar4", b"Rar!\x1A\x07\x00", "RAR"),
        ("damaged.gz", b"\x1F\x8B", "gzip"),
        ("damaged.bz2", b"BZh", "single compressed stream"),
        ("damaged.xz", b"\xFD7zXZ\x00", "xz"),
        ("damaged.cab", b"MSCF", "CAB"),
    ] {
        let mut bytes = signature.to_vec();
        bytes.extend([0xA5; 64]);
        let archive = written(work.path(), name, &bytes);
        let fake = Fake7Zip::new(&sevenzip::NAMES_ON_PATH);
        for action in ["list", "test"] {
            let run = fake.zipper(&[action.as_ref(), archive.as_os_str()]);
            assert_eq!(fake.started(), None, "{action} {name}: {}", run.stderr);
            assert!(!run.success, "{action} {name} succeeded");
            assert!(
                run.stderr.contains(words) && !run.stderr.contains("ZIP end record"),
                "{action} {name}: wanted {words:?}, got {}",
                run.stderr
            );
        }
    }
}

#[test]
fn a_numbered_first_volume_reads_in_process_as_one_archive() {
    let archive = fs::read(Path::new(DATA).join("7z-corpus/7zip-lzma2-long.7z")).unwrap();
    let work = tempfile::tempdir().unwrap();
    let (front, back) = archive.split_at(archive.len() / 2);
    let first = written(work.path(), "set.7z.001", front);
    written(work.path(), "set.7z.002", back);
    let output = work.path().join("out");
    let fake = Fake7Zip::new(&sevenzip::NAMES_ON_PATH);
    let run = fake.zipper(&[
        "extract".as_ref(),
        first.as_os_str(),
        "-o".as_ref(),
        output.as_os_str(),
    ]);
    assert_eq!(fake.started(), None);
    assert!(run.success, "{}", run.stderr);
    assert!(output.join("src").is_dir());
}

#[test]
fn the_engine_flag_is_gone() {
    let fake = Fake7Zip::new(&sevenzip::NAMES_ON_PATH);
    for engine in ["auto", "native", "backend"] {
        let run = fake.zipper(&["--zip-engine", engine, "list", "a.zip"]);
        assert!(!run.success, "{engine}");
        assert!(
            run.stderr.contains("unexpected argument '--zip-engine'"),
            "{engine}: {}",
            run.stderr
        );
    }
    assert_eq!(fake.started(), None);
}
