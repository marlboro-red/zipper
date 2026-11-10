//! Crafted and damaged NTFS volumes, each refused with nothing published,
//! or read to exactly the bytes the volume stores. Where 7-Zip is
//! installed, every damaged volume is also handed to it: Zipper refuses
//! what 7-Zip refuses, apart from the reads the reference lists as more
//! lenient, and refuses more only with a check the reference lists.
#[path = "common/mod.rs"]
mod common;
#[path = "common/ntfs_writer.rs"]
mod ntfs_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use ntfs_writer::{
    DATA, DIRECTORY, DOS_NAME, FILE_NAME, Form, Geometry, IN_USE, Image, Node, POSIX_NAME,
    REPARSE_POINT, ROOT, Record, STANDARD_INFORMATION, Sizes, TICKS_2020, WIN32_AND_DOS_NAME,
    WIN32_NAME, dir, file, file_as, file_name, hard_link_in_group, junction_reparse, link,
    non_resident, reference, reparse, resident, runlist, standard_information_all_times,
    symlink_reparse,
};
use std::{collections::BTreeMap, fs, path::Path, process::Command};
use zipper::native_ntfs::{ReadOptions, read_archive};

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let kind = if entry.file_type().is_symlink() {
                let target = fs::read_link(entry.path()).unwrap();
                let target = match target.strip_prefix(root) {
                    Ok(inside) => format!(
                        "{}{}",
                        "../".repeat(name.matches('/').count()),
                        inside.display()
                    ),
                    Err(_) => target.display().to_string(),
                };
                format!("link:{target}").into_bytes()
            } else if entry.file_type().is_dir() {
                b"dir".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, kind)
        })
        .collect()
}

fn extract(volume: &[u8]) -> (tempfile::TempDir, anyhow::Result<BTreeMap<String, Vec<u8>>>) {
    let root = tempfile::tempdir().unwrap();
    let image = root.path().join("hostile.ntfs");
    fs::write(&image, volume).unwrap();
    let into = root.path().join("out");
    let result = read_archive(&image, Some(&into), ReadOptions::default()).map(|_| tree(&into));
    if result.is_err() {
        assert!(
            !into.exists(),
            "a refused volume published {}",
            into.display()
        );
        let left: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
        assert_eq!(
            left.len(),
            1,
            "a refused volume left files beside the destination"
        );
        assert!(read_archive(&image, None, ReadOptions::default()).is_err());
    }
    common::make_tree_deletable(root.path());
    (root, result)
}

#[track_caller]
fn refused(volume: &[u8], because: &str) {
    let (_root, result) = extract(volume);
    let error = format!("{:#}", result.expect_err("the volume was accepted"));
    assert!(
        error.contains(because),
        "refused for another reason: {error}"
    );
}

#[track_caller]
fn accepted(volume: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let (_root, result) = extract(volume);
    let mut tree = result.unwrap_or_else(|error| panic!("refused: {error:#}"));
    tree.retain(|name, _| !name.starts_with("[SYSTEM]"));
    tree
}

fn sample() -> Vec<Node> {
    vec![
        file("hello.txt", b"hello\n"),
        dir(
            "folder",
            vec![
                file("data.bin", &b"0123456789".repeat(2000)),
                dir("inner", vec![file("deep.txt", b"deep\n")]),
            ],
        ),
        file("empty", b""),
    ]
}

fn geometry() -> Geometry {
    Geometry {
        clusters: 256,
        records: 40,
        ..Geometry::default()
    }
}

fn image(nodes: &[Node]) -> Image {
    ntfs_writer::build_image(geometry(), nodes)
}

fn kind_of(attribute: &[u8]) -> u32 {
    u32::from_le_bytes(attribute[0..4].try_into().unwrap())
}

fn attribute_mut(record: &mut Record, kind: u32) -> &mut Vec<u8> {
    record
        .attributes
        .iter_mut()
        .find(|attribute| kind_of(attribute) == kind)
        .expect("no such attribute")
}

fn put_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn the_sample_volume_reads_back_exactly() {
    let tree = accepted(&image(&sample()).finish());
    assert_eq!(tree["hello.txt"], b"hello\n");
    assert_eq!(tree["folder/data.bin"], b"0123456789".repeat(2000));
    assert_eq!(tree["folder/inner/deep.txt"], b"deep\n");
    assert_eq!(tree["empty"], b"");
}

#[test]
fn damaged_boot_sectors_are_refused() {
    let good = image(&sample()).finish();
    let cases: [(usize, u8, &str); 9] = [
        (510, 0, "55 AA"),
        (0, 0x90, "jump"),
        (3, b'X', "not an NTFS boot sector"),
        (12, 0x01, "sector size"),
        (13, 3, "sectors per cluster"),
        (14, 1, "reserved fields"),
        (21, 0xF0, "reserved fields"),
        (0x41, 1, "record or index block size"),
        (0x2F, 0x40, "sector count is too large"),
    ];
    for (at, value, because) in cases {
        let mut volume = good.clone();
        volume[at] = value;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("bad.ntfs");
        fs::write(&path, &volume).unwrap();
        let error = read_archive(&path, None, ReadOptions::default()).unwrap_err();
        assert!(
            format!("{error:#}").contains(because),
            "byte {at}: {error:#}"
        );
        assert!(!zipper::native_ntfs::sniff(&path).unwrap());
    }
}

#[test]
fn a_volume_shorter_than_its_boot_sector_declares_is_refused() {
    let good = image(&sample()).finish();
    refused(&good[..good.len() - 4097], "short of");
    for cut in [0, 100, 511, 512, 4096 * 4 + 100] {
        let (_root, result) = extract(&good[..cut]);
        assert!(result.is_err(), "cut at {cut}");
    }
}

#[test]
fn an_mft_outside_the_volume_is_refused() {
    let mut volume = image(&sample()).finish();
    put_u64(&mut volume, 0x30, 1 << 40);
    refused(&volume, "MFT starts outside the volume");
}

#[test]
fn records_of_the_wrong_size_or_signature_are_refused() {
    let built = image(&sample());
    let at = built.record_offset(0);
    let good = built.finish();
    let mut volume = good.clone();
    volume[at..at + 4].copy_from_slice(b"FILX");
    refused(&volume, "unknown signature");
    for size in [1000u32, 256, 8192] {
        let mut volume = good.clone();
        put_u32(&mut volume, at + 0x1C, size);
        refused(&volume, "MFT record size");
    }
}

#[test]
fn update_sequence_damage_is_refused() {
    let built = image(&sample());
    let at = built.record_offset(16);
    let good = built.finish();
    let mut volume = good.clone();
    volume[at + 510] ^= 1;
    refused(&volume, "fails its update sequence check");
    let mut volume = good.clone();
    volume[at + 4] = 0x31;
    refused(&volume, "damaged update sequence array");
    let mut volume = good.clone();
    volume[at + 6] = 4;
    refused(&volume, "damaged update sequence array");
    let mut volume = good.clone();
    put_u32(&mut volume, at + 0x2C, 17);
    refused(&volume, "number of another record");
    let mut volume = good.clone();
    put_u32(&mut volume, at + 0x2C, 0);
    accepted(&volume);
}

#[test]
fn damage_in_records_not_in_use_is_still_refused_as_7zip_refuses_it() {
    let built = image(&sample());
    let at = built.record_offset(30);
    let mut volume = built.finish();
    volume[at + 1022] ^= 0x10;
    refused(&volume, "update sequence check");
}

#[test]
fn record_headers_outside_their_record_are_refused() {
    let built = image(&sample());
    let at = built.record_offset(16);
    let good = built.finish();
    for (offset, value) in [
        (0x14usize, 0x3Cu32),
        (0x18, 2048),
        (0x18, 0x3C),
        (0x1C, 2048),
    ] {
        let mut volume = good.clone();
        if offset == 0x14 {
            put_u16(&mut volume, at + offset, value as u16);
        } else {
            put_u32(&mut volume, at + offset, value);
        }
        refused(&volume, "NTFS record");
    }
}

#[test]
fn attributes_that_leave_their_record_are_refused() {
    type Edit = Box<dyn Fn(&mut Vec<u8>)>;
    let edits: Vec<(&str, Edit)> = vec![
        ("length outside", Box::new(|a| put_u32(a, 4, 0x1000))),
        ("length outside", Box::new(|a| put_u32(a, 4, 0x13))),
        ("name lies outside", Box::new(|a| a[9] = 200)),
        ("value lies outside", Box::new(|a| put_u32(a, 0x10, 0x400))),
    ];
    for (because, edit) in edits {
        let mut built = image(&sample());
        let index = built.find_by_first_name("hello.txt");
        edit(attribute_mut(built.record_mut(index), DATA));
        refused(&built.finish(), because);
    }
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    let name = attribute_mut(built.record_mut(index), FILE_NAME);
    let length = name.len();
    name[0x18 + 0x40] = 100;
    let _ = length;
    refused(&built.finish(), "runs past its attribute");
}

#[test]
fn non_resident_damage_is_refused() {
    let cases: [(&str, usize, u64); 6] = [
        ("does not fill", 0x18, 5),
        ("sizes that do not match", 0x28, 4096),
        ("sizes that do not match", 0x28, 4096 * 5 + 1),
        ("sizes that do not match", 0x30, 4096 * 6),
        ("does not continue", 0x10, 3),
        ("compression unit", 0x22, 2),
    ];
    for (because, at, value) in cases {
        let mut built = image(&sample());
        let index = built.find_by_first_name("data.bin");
        let attribute = attribute_mut(built.record_mut(index), DATA);
        if at == 0x22 {
            let mut sizes = sizes(5);
            sizes.unit = value as u8;
            sizes.flags = 1;
            sizes.packed = Some(5 * 4096);
            *attribute = non_resident(DATA, "", sizes, &runlist(&[(5, Some(100))]));
            refused(&built.finish(), because);
            continue;
        }
        put_u64(attribute, at, value);
        if because == "does not fill" {
            put_u64(attribute, 0x28, 6 * 4096);
        }
        refused(&built.finish(), because);
    }
}

fn data_file_with_runs(runs: &[u8], sizes: Sizes) -> Vec<u8> {
    let mut built = image(&sample());
    let index = built.find_by_first_name("data.bin");
    *attribute_mut(built.record_mut(index), DATA) = non_resident(DATA, "", sizes, runs);
    built.finish()
}

fn sizes(clusters: u64) -> Sizes {
    Sizes {
        high_vcn: clusters - 1,
        allocated: clusters * 4096,
        size: clusters * 4096,
        initialized: clusters * 4096,
        ..Sizes::default()
    }
}

#[test]
fn runs_outside_the_volume_or_damaged_are_refused() {
    refused(
        &data_file_with_runs(&runlist(&[(5, Some(254))]), sizes(5)),
        "outside the volume",
    );
    refused(
        &data_file_with_runs(&runlist(&[(5, Some(1 << 40))]), sizes(5)),
        "outside the volume",
    );
    refused(
        &data_file_with_runs(&[0x21, 5, 0x10, 0xF0, 0x11, 1, 0xF0], sizes(6)),
        "outside the volume",
    );
    refused(
        &data_file_with_runs(&[0x09, 1, 0, 0, 0, 0, 0, 0, 0, 0], sizes(1)),
        "damaged length",
    );
    refused(&data_file_with_runs(&[0x90, 1], sizes(1)), "damaged length");
    refused(
        &data_file_with_runs(&runlist(&[(5, Some(100)), (2, Some(120))]), sizes(5)),
        "passes the end",
    );
    refused(
        &data_file_with_runs(&runlist(&[(3, Some(100)), (2, None)]), sizes(5)),
        "not sparse",
    );
}

#[test]
fn overlapping_runs_are_refused() {
    refused(
        &data_file_with_runs(&runlist(&[(3, Some(100)), (3, Some(101))]), sizes(6)),
        "share cluster",
    );
    let mut built = image(&sample());
    let hello = built.find_by_first_name("hello.txt");
    let data = built.find_by_first_name("data.bin");
    let shared = attribute_mut(built.record_mut(data), DATA).clone();
    *attribute_mut(built.record_mut(hello), DATA) = shared;
    refused(&built.finish(), "share cluster");
}

#[test]
fn mft_loops_are_refused() {
    let mut built = image(&sample());
    let folder = built.find_by_first_name("folder");
    let inner = built.find_by_first_name("inner");
    *attribute_mut(built.record_mut(folder), FILE_NAME) =
        file_name(reference(inner, 1), WIN32_AND_DOS_NAME, "folder");
    refused(&built.finish(), "inside itself");
    let mut built = image(&sample());
    let folder = built.find_by_first_name("folder");
    *attribute_mut(built.record_mut(folder), FILE_NAME) =
        file_name(reference(folder, 1), WIN32_AND_DOS_NAME, "folder");
    refused(&built.finish(), "inside itself");
}

#[test]
fn folders_deeper_than_256_are_refused_and_256_are_read() {
    for (depth, works) in [(256usize, true), (257, false)] {
        let mut node = file("bottom.txt", b"x");
        for level in (0..depth).rev() {
            node = dir(&format!("{}", level % 10), vec![node]);
        }
        let volume = ntfs_writer::build(
            Geometry {
                records: depth + 24,
                clusters: 1024,
                ..Geometry::default()
            },
            &[node],
        );
        let (_root, result) = extract(&volume);
        match works {
            true => assert!(result.is_ok(), "{depth}: {:#}", result.unwrap_err()),
            false => assert!(format!("{:#}", result.unwrap_err()).contains("folders deep")),
        }
    }
}

#[test]
fn files_whose_folder_is_missing_go_to_lost_and_records_without_names_are_named() {
    let mut built = image(&sample());
    let deep = built.find_by_first_name("deep.txt");
    *attribute_mut(built.record_mut(deep), FILE_NAME) =
        file_name(reference(35, 1), WIN32_AND_DOS_NAME, "deep.txt");
    let data = built.find_by_first_name("data.bin");
    let wrong_sequence = attribute_mut(built.record_mut(data), FILE_NAME);
    let parent = u64::from_le_bytes(wrong_sequence[0x18..0x20].try_into().unwrap());
    put_u64(wrong_sequence, 0x18, parent + (1 << 48));
    built.set(
        33,
        Record::new(
            IN_USE,
            vec![
                standard_information_all_times(TICKS_2020, 0x20),
                resident(DATA, "", b"anon"),
            ],
        ),
    );
    let tree = accepted(&built.finish());
    assert_eq!(tree["[LOST]/deep.txt"], b"deep\n");
    assert_eq!(tree["[LOST]/data.bin"], b"0123456789".repeat(2000));
    assert_eq!(tree["[LOST]/[NONAME]-33"], b"anon");
}

#[test]
fn bad_extension_records_are_refused_and_good_ones_join_their_base() {
    let nodes = [file_as(
        "far.bin",
        &b"far away ".repeat(900),
        Form::InExtensionRecord,
    )];
    let tree = accepted(&ntfs_writer::build(geometry(), &nodes));
    assert_eq!(tree["far.bin"], b"far away ".repeat(900));
    let mut built = image(&nodes);
    let base = built.find_by_first_name("far.bin");
    built.record_mut(base + 1).base = reference(base, 2);
    refused(&built.finish(), "which does not take it");
    let mut built = image(&nodes);
    let base = built.find_by_first_name("far.bin");
    built.record_mut(base + 1).base = reference(1000, 1);
    refused(&built.finish(), "which the MFT does not hold");
    let mut built = image(&nodes);
    let base = built.find_by_first_name("far.bin");
    built.record_mut(base).flags = 0;
    let tree = accepted(&built.finish());
    assert!(tree.is_empty(), "{:?}", tree.keys());
}

#[test]
fn unsafe_and_duplicate_names_are_refused() {
    for (name, because) in [
        ("..", "unsafe archive path"),
        (".", "unsafe archive path"),
        ("a:b", "unsafe archive path"),
        ("nul\0in", "NUL"),
        ("", "empty name"),
    ] {
        let mut built = image(&sample());
        let index = built.find_by_first_name("hello.txt");
        *attribute_mut(built.record_mut(index), FILE_NAME) =
            file_name(reference(ROOT, 1), WIN32_AND_DOS_NAME, name);
        refused(&built.finish(), because);
    }
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    let name = attribute_mut(built.record_mut(index), FILE_NAME);
    let end = name.len();
    let units = 0x18 + 0x42;
    name[units..units + 2].copy_from_slice(&0xD800u16.to_le_bytes());
    let _ = end;
    refused(&built.finish(), "not valid UTF-16");
    let nodes = [file("same", b"1"), file("same", b"2")];
    refused(&ntfs_writer::build(geometry(), &nodes), "two entries named");
}

#[test]
fn slashes_in_names_become_underscores_as_7zip_writes_them() {
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    *attribute_mut(built.record_mut(index), FILE_NAME) =
        file_name(reference(ROOT, 1), WIN32_AND_DOS_NAME, "a/b\\c");
    let tree = accepted(&built.finish());
    assert_eq!(tree["a_b_c"], b"hello\n");
}

#[test]
fn dos_names_are_left_out_only_beside_a_win32_name_of_the_same_folder() {
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    let record = built.record_mut(index);
    *attribute_mut(record, FILE_NAME) =
        file_name(reference(ROOT, 1), WIN32_NAME, "Hello World.txt");
    record
        .attributes
        .insert(2, file_name(reference(ROOT, 1), DOS_NAME, "HELLOW~1.TXT"));
    let tree = accepted(&built.finish());
    assert!(tree.contains_key("Hello World.txt"));
    assert!(!tree.contains_key("HELLOW~1.TXT"));
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    *attribute_mut(built.record_mut(index), FILE_NAME) =
        file_name(reference(ROOT, 1), DOS_NAME, "ONLYDOS.TXT");
    let tree = accepted(&built.finish());
    assert_eq!(tree["ONLYDOS.TXT"], b"hello\n");
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    let record = built.record_mut(index);
    record
        .attributes
        .insert(2, file_name(reference(ROOT, 1), POSIX_NAME, "posix.txt"));
    let tree = accepted(&built.finish());
    assert_eq!(tree["posix.txt"], b"hello\n");
    assert_eq!(tree["hello.txt"], b"hello\n");
}

#[test]
fn a_folder_with_two_names_is_refused() {
    let mut built = image(&sample());
    let folder = built.find_by_first_name("folder");
    built
        .record_mut(folder)
        .attributes
        .insert(2, file_name(reference(ROOT, 1), POSIX_NAME, "again"));
    refused(&built.finish(), "more than one name");
}

#[test]
fn alternate_streams_are_refused_outside_system_and_skipped_inside() {
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    built
        .record_mut(index)
        .attributes
        .push(resident(DATA, "Zone.Identifier", b"[ZoneTransfer]"));
    refused(&built.finish(), "alternate data stream");
    let mut built = image(&sample());
    let folder = built.find_by_first_name("folder");
    built
        .record_mut(folder)
        .attributes
        .push(resident(DATA, "", b"folder data"));
    refused(&built.finish(), "alternate data stream");
    let mut built = image(&sample());
    let mft = built.mft_record();
    built.set(0, mft);
    built
        .record_mut(0)
        .attributes
        .push(resident(DATA, "$Bitmap", b"\xFF"));
    let mut root = built.record_mut(ROOT).clone();
    root.attributes.push(resident(DATA, "rootstream", b"x"));
    built.set(ROOT, root);
    let tree = accepted(&built.finish());
    assert_eq!(tree["hello.txt"], b"hello\n");
}

#[test]
fn encrypted_files_are_refused() {
    let mut built = image(&sample());
    let index = built.find_by_first_name("hello.txt");
    *attribute_mut(built.record_mut(index), STANDARD_INFORMATION) =
        standard_information_all_times(TICKS_2020, 0x4000);
    refused(&built.finish(), "encrypted");
}

#[test]
fn links_are_created_inside_and_refused_outside() {
    let nodes = [
        file("hello.txt", b"hello\n"),
        dir(
            "folder",
            vec![link(
                "up.lnk",
                symlink_reparse("..\\hello.txt", true),
                false,
            )],
        ),
        link("abs.lnk", symlink_reparse("\\??\\C:\\folder", false), false),
        link("junction", junction_reparse("\\??\\D:\\folder"), true),
    ];
    let tree = accepted(&ntfs_writer::build(geometry(), &nodes));
    assert_eq!(tree["folder/up.lnk"], b"link:../hello.txt");
    assert_eq!(tree["abs.lnk"], b"link:folder");
    assert_eq!(tree["junction"], b"link:folder");
    for (target, relative) in [("..\\..\\outside", true), ("\\??\\C:\\..\\x", false)] {
        let nodes = [link("bad.lnk", symlink_reparse(target, relative), false)];
        refused(&ntfs_writer::build(geometry(), &nodes), "outside");
    }
    let nodes = [link("odd", reparse(0x8000_0017, b"wof"), false)];
    refused(
        &ntfs_writer::build(geometry(), &nodes),
        "reparse point of tag 0x80000017",
    );
    let mut built = image(&sample());
    let folder = built.find_by_first_name("folder");
    built.record_mut(folder).attributes.push(resident(
        REPARSE_POINT,
        "",
        &junction_reparse("\\??\\C:\\x"),
    ));
    refused(&built.finish(), "has entries under it");
}

#[test]
fn compressed_units_that_decode_short_are_refused() {
    let data: Vec<u8> = b"compress me please ".repeat(5000);
    let nodes = [file_as("packed.txt", &data, Form::Lznt1)];
    let good = ntfs_writer::build(geometry(), &nodes);
    assert_eq!(accepted(&good)["packed.txt"], data);
    let packed = ntfs_writer::lznt1_padded_to_chunks(&data[..16 * 4096]);
    let mut volume = good.clone();
    let at = volume
        .windows(32)
        .position(|window| window == &packed[..32])
        .expect("the packed unit is on the volume");
    volume[at] = 0x05;
    volume[at + 1] = 0xB0;
    refused(&volume, "decodes to");
}

#[test]
fn every_short_record_layout_is_refused_or_read_without_a_panic() {
    let built = image(&sample());
    let at = built.record_offset(16);
    let good = built.finish();
    for offset in 0x30..0x60 {
        for value in [0u8, 1, 0x7F, 0x80, 0xFF] {
            let mut volume = good.clone();
            volume[at + offset] = value;
            let _ = extract(&volume);
        }
    }
}

const STRICTER: &[&str] = &[
    "outside the volume",
    "share cluster",
    "inside itself",
    "folders deep",
    "two entries named",
    "has entries under it",
    "runs past its attribute",
    "outside the record",
    "with a NUL",
    "not valid UTF-16",
    "empty name",
    "unsafe archive path",
    "more than one name",
    "reparse point of tag",
    "reparse data",
    "points outside",
    "alternate data stream",
    "encrypted",
    "system record",
    "does not continue its attribute",
    "passes the end of the volume",
    "MFT record size",
    "collides with another entry",
    "inside a file or link",
    "different kinds",
    "also a directory",
    "folder Zipper makes",
    "clusters over 4 KiB",
];

fn sweep_volume() -> (Vec<u8>, Vec<(usize, usize)>, Vec<usize>) {
    let nodes = [
        file("hello.txt", b"hello\n"),
        dir(
            "folder",
            vec![
                file_as("frag.bin", &b"fragmented ".repeat(3000), Form::Fragments(4)),
                hard_link_in_group("one.txt", b"linked\n", 1),
                hard_link_in_group("two.txt", b"linked\n", 1),
            ],
        ),
        file_as(
            "packed.txt",
            &b"compressible text ".repeat(4000),
            Form::Lznt1,
        ),
        file_as(
            "far.bin",
            &b"far away ".repeat(700),
            Form::InExtensionRecord,
        ),
    ];
    let mut built = image(&nodes);
    let named = built.find_by_first_name("hello.txt");
    let record = built.record_mut(named);
    *attribute_mut(record, FILE_NAME) =
        file_name(reference(ROOT, 1), WIN32_NAME, "Hello World.txt");
    record
        .attributes
        .insert(2, file_name(reference(ROOT, 1), DOS_NAME, "HELLOW~1.TXT"));
    let used: Vec<u64> = (0..built.records.len() as u64)
        .filter(|&index| built.records[index as usize].is_some() || index == 0)
        .collect();
    let spans = used
        .iter()
        .map(|&index| (built.record_offset(index), built.geometry.record))
        .collect();
    let packed_first = built.next_cluster;
    let volume = built.finish();
    let packed: Vec<usize> = (0..volume.len())
        .filter(|&at| at as u64 / 4096 >= packed_first.saturating_sub(4) && at % 4096 < 64)
        .take(128)
        .collect();
    (volume, spans, packed)
}

fn sevenzip_accepts(binary: &Path, image: &Path, into: &Path) -> Option<BTreeMap<String, Vec<u8>>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(image)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    run.status.success().then(|| {
        let mut tree = tree(into);
        tree.retain(|name, _| !(name.starts_with("[SYSTEM]/") && name.contains(':')));
        tree
    })
}

fn is_data_attribute_flags_byte(clean: &[u8], spans: &[(usize, usize)], at: usize) -> bool {
    spans.iter().any(|&(start, length)| {
        if at < start || at >= start + length {
            return false;
        }
        let record = &clean[start..start + length];
        let mut offset = usize::from(u16::from_le_bytes([record[0x14], record[0x15]]));
        while offset + 8 <= length {
            let kind = u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap());
            if kind == u32::MAX {
                return false;
            }
            let size =
                u32::from_le_bytes(record[offset + 4..offset + 8].try_into().unwrap()) as usize;
            if size == 0 {
                return false;
            }
            if kind == DATA && (start + offset + 0x0C..start + offset + 0x0E).contains(&at) {
                return true;
            }
            offset += size;
        }
        false
    })
}

#[test]
fn damage_sweeps_are_refused_exactly_when_7zip_refuses_them_or_by_a_listed_stricter_check() {
    let Some(binary) = sevenzip::sevenzip() else {
        return;
    };
    let (clean, spans, packed) = sweep_volume();
    let mut offsets: Vec<usize> = (0..512).step_by(3).collect();
    for &(start, length) in &spans {
        let used =
            u32::from_le_bytes(clean[start + 0x18..start + 0x1C].try_into().unwrap()) as usize;
        offsets.extend((start..start + used.min(length)).step_by(7));
    }
    offsets.extend(packed);
    let mut stricter = 0;
    for (round, &at) in offsets.iter().enumerate() {
        let mut volume = clean.clone();
        volume[at] ^= 1 << (round % 8);
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("damaged.ntfs");
        fs::write(&image, &volume).unwrap();
        let ours = read_archive(
            &image,
            Some(&work.path().join("zipper")),
            ReadOptions::default(),
        )
        .map(|_| tree(&work.path().join("zipper")));
        let theirs = sevenzip_accepts(&binary, &image, &work.path().join("7z"));
        match (&ours, &theirs) {
            (Ok(ours), Some(theirs)) => assert!(
                ours == theirs || is_data_attribute_flags_byte(&clean, &spans, at),
                "byte {at}: both read the damaged volume, to different trees: {:?}",
                ours.keys()
                    .chain(theirs.keys())
                    .filter(|name| ours.get(*name) != theirs.get(*name))
                    .map(|name| (
                        name,
                        ours.get(name)
                            .map(|v| String::from_utf8_lossy(&v[..v.len().min(40)]).into_owned()),
                        theirs
                            .get(name)
                            .map(|v| String::from_utf8_lossy(&v[..v.len().min(40)]).into_owned())
                    ))
                    .collect::<Vec<_>>()
            ),
            (Err(_), None) => {}
            (Ok(_), None) => panic!("byte {at}: Zipper read a volume 7-Zip refuses"),
            (Err(error), Some(_)) => {
                let error = format!("{error:#}");
                assert!(
                    STRICTER.iter().any(|check| error.contains(check)),
                    "byte {at}: Zipper refused a volume 7-Zip reads: {error}"
                );
                stricter += 1;
            }
        }
        common::make_tree_deletable(work.path());
    }
    eprintln!(
        "{} damaged volumes, {stricter} refused by a stricter check",
        offsets.len()
    );
}

#[test]
fn every_kind_of_record_is_read_without_the_index_or_bitmap() {
    let mut built = image(&sample());
    let folder = built.find_by_first_name("folder");
    built.record_mut(folder).attributes.push(resident(
        ntfs_writer::INDEX_ROOT,
        "$I30",
        &[0xEE; 64],
    ));
    let tree = accepted(&built.finish());
    assert_eq!(tree["folder/inner/deep.txt"], b"deep\n");
    let _ = DIRECTORY;
}

const LIMIT_VOLUMES: &str = "ZIPPER_NTFS_LIMIT_VOLUMES";

#[test]
fn write_volumes_at_the_documented_limits() {
    let Some(folder) = std::env::var_os(LIMIT_VOLUMES) else {
        return;
    };
    let folder = Path::new(&folder);
    let folders = 1000u64;
    let per_folder = 998u64;
    let geometry = Geometry {
        records: (16 + folders * (per_folder + 1)) as usize + 8,
        clusters: (16 + folders * (per_folder + 1) + 64) / 4 + 64,
        ..Geometry::default()
    };
    let mut built = Image::new(geometry);
    built.set(
        ROOT,
        Record::new(
            IN_USE | DIRECTORY,
            vec![
                standard_information_all_times(TICKS_2020, 0x16),
                file_name(reference(ROOT, 1), WIN32_AND_DOS_NAME, "."),
            ],
        ),
    );
    let mut index = 16u64;
    for folder_number in 0..folders {
        let folder_record = index;
        index += 1;
        built.write_record_now(
            folder_record,
            &Record::new(
                IN_USE | DIRECTORY,
                vec![
                    standard_information_all_times(TICKS_2020, 0x10),
                    file_name(
                        reference(ROOT, 1),
                        WIN32_AND_DOS_NAME,
                        &format!("folder-{folder_number:04}-with-a-longer-name"),
                    ),
                ],
            ),
        );
        for file_number in 0..per_folder {
            built.write_record_now(
                index,
                &Record::new(
                    IN_USE,
                    vec![
                        standard_information_all_times(TICKS_2020, 0x20),
                        file_name(
                            reference(folder_record, 1),
                            WIN32_AND_DOS_NAME,
                            &format!("empty-file-{file_number:04}-name.txt"),
                        ),
                        resident(DATA, "", b""),
                    ],
                ),
            );
            index += 1;
        }
    }
    fs::write(folder.join("many-files.ntfs"), built.finish()).unwrap();
    let data: Vec<u8> = (0..512u32 << 20).map(|at| (at % 251) as u8).collect();
    let volume = ntfs_writer::build(
        Geometry {
            clusters: (520 << 20) / 4096,
            ..Geometry::default()
        },
        &[file_as("large.bin", &data, Form::Fragments(64))],
    );
    fs::write(folder.join("large-file.ntfs"), volume).unwrap();
}

#[test]
fn a_real_folder_named_like_a_folder_7zip_makes_is_refused() {
    let nodes = [
        dir("[SYSTEM]", vec![file("$MFT", b"not the MFT")]),
        file("hello.txt", b"hello\n"),
    ];
    refused(
        &ntfs_writer::build(geometry(), &nodes),
        "folder Zipper makes",
    );
    let nodes = [
        dir("[LOST]", vec![file("x", b"real")]),
        dir("gone", vec![file("x", b"lost")]),
    ];
    let mut built = image(&nodes);
    let gone = built.find_by_first_name("gone");
    built.record_mut(gone).flags = 0;
    refused(&built.finish(), "folder Zipper makes");
    let nodes = [dir("[LOST]", vec![file("x", b"real")])];
    assert_eq!(
        accepted(&ntfs_writer::build(geometry(), &nodes))["[LOST]/x"],
        b"real"
    );
}

#[test]
fn compression_on_clusters_over_4_kib_is_refused() {
    let data = b"compress me please ".repeat(5000);
    let volume = ntfs_writer::build(
        Geometry {
            cluster: 8192,
            clusters: 512,
            ..Geometry::default()
        },
        &[file_as("packed.txt", &data, Form::Lznt1)],
    );
    refused(&volume, "clusters over 4 KiB");
}
