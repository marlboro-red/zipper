//! Every refused format and method is refused, through the CLI and the
//! library, with an error that names the format or method and nothing
//! written. A fake 7-Zip, named by `ZIPPER_7Z` or found on `PATH`, is never
//! started for any of them.
#![cfg(unix)]
#[path = "common/cab_writer.rs"]
mod cab_writer;
#[path = "common/quantum_encoder.rs"]
mod quantum_encoder;
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

struct Case {
    name: &'static str,
    bytes: Vec<u8>,
    words: &'static str,
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

// SPEC(Microsoft Virtual Hard Disk Image Format Specification, Hard Disk Footer Format): the
// checksum is the one's complement of the sum of the footer's bytes without it.
fn fixed_vhd(disk: &[u8]) -> Vec<u8> {
    let mut footer = [0u8; SECTOR];
    footer[..8].copy_from_slice(b"conectix");
    footer[8..12].copy_from_slice(&2u32.to_be_bytes());
    footer[12..16].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    footer[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
    footer[40..48].copy_from_slice(&(disk.len() as u64).to_be_bytes());
    footer[48..56].copy_from_slice(&(disk.len() as u64).to_be_bytes());
    footer[60..64].copy_from_slice(&2u32.to_be_bytes());
    let sum = footer.iter().fold(0u32, |sum, byte| sum + u32::from(*byte));
    footer[64..68].copy_from_slice(&(!sum).to_be_bytes());
    [disk, &footer].concat()
}

fn case(name: &'static str, bytes: Vec<u8>, words: &'static str) -> Case {
    Case { name, bytes, words }
}

fn cases() -> Vec<Case> {
    let chm = "Zipper does not read CHM and Help 2 files";
    let compound = "Zipper does not read compound files such as MSI, DOC and XLS";
    let fat = "Zipper does not read FAT volumes";
    let ntfs = "Zipper does not read NTFS volumes";
    let hfs = "Zipper does not read HFS+ and HFSX volumes";
    let apfs = "Zipper does not read APFS containers";
    let vhd = "Zipper does not read VHD images";
    let vhdx = "Zipper does not read VHDX images";
    let qcow = "Zipper does not read QCOW images";
    let vdi = "Zipper does not read VDI images";
    let vmdk = "Zipper does not read VMDK images";
    let sparse = "Zipper does not read Android sparse images";
    let mbr = "Zipper does not read MBR disk images";
    let gpt = "Zipper does not read GPT disk images";
    let compound_magic = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";
    vec![
        case("help.chm", stored("refused/chm.chm.xz"), chm),
        case("help.hxs", padded(b"ITOLITLS\x01\0\0\0", 0, 4096), chm),
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
            gpt,
        ),
        case(
            "mbr-fat.img",
            stored("parity/archives/mbr.img.stored.xz"),
            mbr,
        ),
        case("ntfs.img", ntfs_volume(), ntfs),
        case("ntfs-mbr.img", lone_partition(0x07, &ntfs_volume()), mbr),
        case(
            "linux-mbr.img",
            lone_partition(0x83, &[0; 64 * SECTOR]),
            mbr,
        ),
        case(
            "ext-magic-mbr.img",
            with_ext_magic(lone_partition(0x83, &[0; 64 * SECTOR])),
            mbr,
        ),
        case("mbr-two.img", stored("refused/mbr-two.img.xz"), mbr),
        case("gpt-two.img", stored("refused/gpt-two.img.xz"), gpt),
        case(
            "gpt-4k.img",
            padded(b"EFI PART\0\0\x01\0", 4096, 16384),
            gpt,
        ),
        case("dynamic.vhd", stored("refused/dynamic.vhd.xz"), vhd),
        case("raw-fat.vhd", stored("refused/raw-fat.vhd.xz"), vhd),
        case("fixed.vhd", fixed_vhd(&[0x5A; 8 * SECTOR]), vhd),
        case("fixed.bin", fixed_vhd(&[0x5A; 8 * SECTOR]), vhd),
        case(
            "fixed-mbr.vhd",
            fixed_vhd(&lone_partition(0x83, &[0; 8 * SECTOR])),
            vhd,
        ),
        case("blocks-1m.vhdx", stored("refused/blocks-1m.vhdx.xz"), vhdx),
        case("fat12.qcow", stored("refused/fat12.qcow.xz"), qcow),
        case("mbr-one.qcow2", stored("refused/mbr-one.qcow2.xz"), qcow),
        case("hfs-gpt.qcow2", stored("refused/hfs-gpt.qcow2.xz"), qcow),
        case(
            "fat12-static.vdi",
            stored("refused/fat12-static.vdi.xz"),
            vdi,
        ),
        case("apfs-gpt.vdi", stored("refused/apfs-gpt.vdi.xz"), vdi),
        case("mbr-one.vmdk", stored("refused/mbr-one.vmdk.xz"), vmdk),
        case(
            "gpt-two-flat.vmdk",
            stored("refused/gpt-two-flat.vmdk.xz"),
            vmdk,
        ),
        case(
            "descriptor.txt",
            stored("refused/gpt-two-flat.vmdk.xz"),
            vmdk,
        ),
        case("mbr-two.simg", stored("refused/mbr-two.simg.xz"), sparse),
        case(
            "hello.b64",
            b"aGVsbG8sIGEgc21hbGwgZmlsZQo=\n".to_vec(),
            "Zipper does not read Base64 files",
        ),
        case(
            "lzx.cab",
            stored("refused/lzx.cab"),
            "CAB folder is compressed with LZX, which Zipper does not decode",
        ),
        case(
            "lzx.wim",
            stored("refused/lzx.wim.xz"),
            "WIM image is compressed with LZX, which Zipper does not decode",
        ),
        case(
            "lzms.wim",
            stored("refused/lzms.wim.xz"),
            "WIM image is compressed with LZMS, which Zipper does not decode",
        ),
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

fn assert_refused_by_the_cli(fake: &FakeSevenZip, by_variable: bool) {
    for case in cases() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(case.name);
        fs::write(&archive, &case.bytes).unwrap();
        let output = work.path().join("out");
        for action in ["extract", "list", "test"] {
            let mut args: Vec<&std::ffi::OsStr> = vec![action.as_ref(), archive.as_os_str()];
            if action == "extract" {
                args.extend(["-o".as_ref(), output.as_os_str()]);
            }
            let (success, stderr) = run_cli(fake, by_variable, &args);
            let label = format!("{} {action}", case.name);
            assert!(!success, "{label} succeeded");
            assert!(stderr.contains(case.words), "{label}: {stderr}");
            assert_eq!(
                leftovers(work.path(), case.name),
                Vec::<String>::new(),
                "{label} wrote output"
            );
        }
        assert_eq!(fake.started(), None, "{} reached 7-Zip", case.name);
    }
}

#[test]
fn every_refused_format_is_refused_with_nothing_written() {
    let fake = FakeSevenZip::new();
    assert_refused_by_the_cli(&fake, true);
    for case in cases() {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(case.name);
        fs::write(&archive, &case.bytes).unwrap();
        let error = zipper::extract_archive(
            &archive,
            &work.path().join("out"),
            zipper::ExtractOptions::default(),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains(case.words),
            "{}: {error:#}",
            case.name
        );
        assert_eq!(leftovers(work.path(), case.name), Vec::<String>::new());
        if case.words.starts_with("Zipper does not read") {
            let error = zipper::choose_reader(&archive).unwrap_err();
            assert!(
                error.downcast_ref::<zipper::Refused>().is_some(),
                "{}: {error:#}",
                case.name
            );
        }
    }
    assert_eq!(fake.started(), None);
}

#[test]
fn no_refused_format_reaches_a_fake_7zip_on_path() {
    let fake = FakeSevenZip::new();
    assert_refused_by_the_cli(&fake, false);
}

fn prefixed(prefix: &[u8], sample: &[u8]) -> Vec<u8> {
    [prefix, sample].concat()
}

fn with_byte(mut bytes: Vec<u8>, at: usize, value: u8) -> Vec<u8> {
    bytes[at] = value;
    bytes
}

fn with_disc_descriptor(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes[0x8000..0x8007].copy_from_slice(b"\x01CD001\x01");
    bytes
}

fn crafted() -> Vec<(&'static str, Vec<u8>)> {
    let installer = stored("refused/installer.msi.xz");
    let mut mub_prefix = vec![0xB9, 0xFA, 0xF1, 0x0E];
    mub_prefix.resize(16, 0);
    let fat = stored("parity/archives/fat.img.stored.xz");
    let vhd = fixed_vhd(&lone_partition(0x83, &[0; 8 * SECTOR]));
    let qcow = stored("refused/mbr-one.qcow2.xz");
    let vhdx = stored("refused/blocks-1m.vhdx.xz");
    let gpt = stored("refused/gpt-two.img.xz");
    vec![
        ("one-byte-prefix.bin", prefixed(&[0], &installer)),
        (
            "far.bin",
            prefixed(&vec![0; 70_000], &stored("refused/lzx.cab")),
        ),
        (
            "mub-chm.bin",
            prefixed(&mub_prefix, &stored("refused/chm.chm.xz")),
        ),
        (
            "mub-lzx.bin",
            prefixed(&mub_prefix, &stored("refused/lzx.wim.xz")),
        ),
        ("media0.img", with_byte(fat.clone(), 21, 0)),
        (
            "mbr-media0.img",
            with_byte(stored("parity/archives/mbr.img.stored.xz"), 533, 0),
        ),
        ("cd001.img", with_disc_descriptor(fat)),
        ("x.b64.001", b"aGVsbG8sIGEgc21h".to_vec()),
        ("vhd-prefix.bin", prefixed(&[0], &vhd)),
        ("far-qcow.bin", prefixed(&vec![0; 70_000], &qcow)),
        ("mub-vhdx.bin", prefixed(&mub_prefix, &vhdx)),
        ("macho-qcow.bin", prefixed(b"\xCF\xFA\xED\xFE", &qcow)),
        ("cd001-gpt.img", with_disc_descriptor(gpt.clone())),
        ("mub-gpt.bin", prefixed(&mub_prefix, &gpt)),
    ]
}

#[test]
fn crafted_refused_files_never_open_in_process_or_start_7zip() {
    for (name, bytes) in crafted() {
        let fake = FakeSevenZip::new();
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(name);
        fs::write(&archive, &bytes).unwrap();
        if name.ends_with(".001") {
            fs::write(work.path().join("x.b64.002"), b"bGwgZmlsZQo=\n").unwrap();
        }
        let output = work.path().join("out");
        for action in ["extract", "list", "test"] {
            let mut args: Vec<&std::ffi::OsStr> = vec![action.as_ref(), archive.as_os_str()];
            if action == "extract" {
                args.extend(["-o".as_ref(), output.as_os_str()]);
            }
            let (success, stderr) = run_cli(&fake, true, &args);
            let label = format!("{name} {action}");
            assert_eq!(fake.started(), None, "{label} started 7-Zip");
            assert!(!success, "{label} was read in process: {stderr}");
            assert!(!output.exists(), "{label} wrote output");
        }
    }
}

fn tar_with(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name);
    header[100..107].copy_from_slice(b"0000644");
    header[108..115].copy_from_slice(b"0000000");
    header[116..123].copy_from_slice(b"0000000");
    header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
    header[136..147].copy_from_slice(b"00000000000");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
    header[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    let mut tar = header.to_vec();
    tar.extend(data);
    tar.resize(tar.len().next_multiple_of(512) + 1024, 0);
    tar
}

#[test]
fn archives_that_hold_a_refused_signature_still_extract() {
    let dmg = stored("parity/archives/dmg-zlib.dmg");
    let cab = cab_writer::one_folder_in_order(
        &[("a.txt", b"hello\n"), ("disk.dmg", &dmg)],
        &cab_writer::Method::Stored,
    )
    .build();
    let vhd = fixed_vhd(&lone_partition(0x83, &[0; 8 * SECTOR]));
    let cases: [(&str, Vec<u8>, &str); 6] = [
        ("holds-a-dmg.cab", cab, "disk.dmg"),
        ("holds-a-vhd.tar", tar_with(b"disk.vhd", &vhd), "disk.vhd"),
        ("kdmv.tar", tar_with(b"KDMV-notes.txt", b"notes\n"), "KDMV"),
        (
            "holds-a-qcow.tar",
            tar_with(b"disk.qcow2", &stored("refused/mbr-one.qcow2.xz")),
            "disk.qcow2",
        ),
        (
            "koly.tar",
            tar_with(b"koly\0", b"koly\0\0\0\x04\0\0\x02\0"),
            "koly",
        ),
        ("itsf.tar", tar_with(b"ITSF-notes.txt", b"notes\n"), "ITSF"),
    ];
    for (name, bytes, wanted) in cases {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(name);
        fs::write(&archive, &bytes).unwrap();
        let output = work.path().join("out");
        zipper::extract_archive(&archive, &output, zipper::ExtractOptions::default())
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        let found: Vec<String> = fs::read_dir(&output)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            found.iter().any(|file| file.starts_with(wanted)),
            "{name}: {found:?}"
        );
    }
}

#[test]
fn a_disk_image_split_into_numbered_volumes_is_refused() {
    let fake = FakeSevenZip::new();
    let image = fixed_vhd(&[0x5A; 8 * SECTOR]);
    let work = tempfile::tempdir().unwrap();
    let first = work.path().join("disk.vhd.001");
    let (front, back) = image.split_at(6 * SECTOR);
    fs::write(&first, front).unwrap();
    fs::write(work.path().join("disk.vhd.002"), back).unwrap();
    let output = work.path().join("out");
    for action in ["extract", "list", "test"] {
        let mut args: Vec<&std::ffi::OsStr> = vec![action.as_ref(), first.as_os_str()];
        if action == "extract" {
            args.extend(["-o".as_ref(), output.as_os_str()]);
        }
        let (success, stderr) = run_cli(&fake, true, &args);
        assert!(!success, "{action} succeeded");
        assert!(
            stderr.contains("Zipper does not read VHD images"),
            "{action}: {stderr}"
        );
        assert!(!output.exists(), "{action} wrote output");
    }
    assert_eq!(fake.started(), None);
}

fn with_ext_magic(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes[1080..1082].copy_from_slice(&[0x53, 0xEF]);
    bytes
}

#[test]
fn disks_with_a_planted_disc_descriptor_are_refused() {
    let cases = [
        ("cd001-gpt.img", stored("refused/gpt-two.img.xz")),
        ("cd001-mbr.img", stored("refused/mbr-two.img.xz")),
        ("cd001-fixed.vhd", fixed_vhd(&vec![0; 0x9000])),
    ];
    for (name, bytes) in cases {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(name);
        fs::write(&archive, with_disc_descriptor(bytes)).unwrap();
        let error = zipper::extract_archive(
            &archive,
            &work.path().join("out"),
            zipper::ExtractOptions::default(),
        )
        .unwrap_err();
        assert!(!work.path().join("out").exists(), "{name}");
        assert!(
            format!("{error:#}").contains("Zipper does not read")
                || format!("{error:#}").contains("ISO"),
            "{name}: {error:#}"
        );
    }
}

#[test]
fn a_first_volume_read_alone_never_reaches_7zip() {
    let fake = FakeSevenZip::new();
    let image = fixed_vhd(&stored("ext-corpus/ext4.img.xz"));
    let work = tempfile::tempdir().unwrap();
    let first = work.path().join("disk.img.001");
    let (front, back) = image.split_at(4096);
    fs::write(&first, front).unwrap();
    fs::write(work.path().join("disk.img.002"), back).unwrap();
    let output = work.path().join("out");
    for action in ["extract", "list", "test"] {
        let mut args: Vec<&std::ffi::OsStr> = vec![action.as_ref(), first.as_os_str()];
        if action == "extract" {
            args.extend(["-o".as_ref(), output.as_os_str()]);
        }
        let _ = run_cli(&fake, true, &args);
    }
    assert_eq!(fake.started(), None);
}

fn with_partition_table(mut bytes: Vec<u8>) -> Vec<u8> {
    let table = lone_partition(0x83, &[0; SECTOR]);
    bytes[0x1BE..SECTOR].copy_from_slice(&table[0x1BE..SECTOR]);
    bytes
}

#[test]
fn discs_and_ext_volumes_that_start_with_a_partition_table_still_read() {
    let cases = [
        (
            "hybrid.iso",
            with_partition_table(stored("parity/archives/iso.iso.stored.xz")),
        ),
        (
            "booted.ext4",
            with_partition_table(stored("ext-corpus/ext4.img.xz")),
        ),
    ];
    for (name, bytes) in cases {
        let work = tempfile::tempdir().unwrap();
        let archive = work.path().join(name);
        fs::write(&archive, &bytes).unwrap();
        let output = work.path().join("out");
        zipper::extract_archive(&archive, &output, zipper::ExtractOptions::default())
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert!(fs::read_dir(&output).unwrap().next().is_some(), "{name}");
    }
}

#[test]
fn intel_hex_is_read_in_process() {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("firmware.hex");
    fs::write(
        &archive,
        ":10000000000102030405060708090A0B0C0D0E0F78\n:00000001FF\n",
    )
    .unwrap();
    let output = work.path().join("out");
    zipper::extract_archive(&archive, &output, zipper::ExtractOptions::default())
        .unwrap_or_else(|error| panic!("{error:#}"));
    let bytes: Vec<Vec<u8>> = fs::read_dir(&output)
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect();
    assert_eq!(bytes, [(0..16).collect::<Vec<u8>>()]);
}
