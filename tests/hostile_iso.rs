//! The disc image reader extracts ISO 9660, Joliet, Rock Ridge, El Torito
//! and UDF byte for byte, rejects each hostile layout with nothing left
//! behind, and honours the options the other readers take.
#[path = "common/iso_writer.rs"]
mod iso_writer;

use iso_writer::{
    Ads, Boot, Built, Disc, Item, SECTOR, Udf, dir, file, retag_with_stored_crc_length, symlink,
};
use std::{collections::BTreeMap, fs, path::Path};
type Damage = Box<dyn Fn(&mut Vec<u8>)>;

use zipper::{
    Existing, Strip,
    native_iso::{ReadOptions, read_archive},
};

fn leftovers(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .display()
                .to_string()
        })
        .filter(|path| path != "case.iso")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) -> String {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.iso");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let error = match read_archive(&archive, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: accepted a hostile image"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile image"
    );
    error
}

fn assert_rejected_with(case: &str, bytes: &[u8], words: &str) {
    let error = assert_rejected_without_leftovers(case, bytes);
    assert!(
        error.contains(words),
        "{case}: {error:?} does not say {words:?}"
    );
    assert!(
        !(error.contains("Rock Ridge") && error.contains("7-Zip")),
        "{case}: {error:?} names 7-Zip"
    );
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
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
                format!(
                    "-> {}",
                    fs::read_link(entry.path())
                        .unwrap()
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                )
                .into_bytes()
            } else if entry.file_type().is_dir() {
                b"/".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, kind)
        })
        .collect()
}

fn extract_with(bytes: &[u8], options: ReadOptions) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.iso");
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    read_archive(&archive, Some(&destination), options)?;
    Ok(tree(&destination))
}

fn extract(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    extract_with(bytes, ReadOptions::default()).unwrap()
}

fn expected(entries: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    entries
        .iter()
        .map(|(name, bytes)| (name.to_string(), bytes.to_vec()))
        .collect()
}

fn bytes_of(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u8).wrapping_mul(13).wrapping_add(seed))
        .collect()
}

fn items() -> Vec<Item> {
    vec![
        file("hello.txt", b"hello\n"),
        dir("folder"),
        file("folder/data.bin", &bytes_of(5000, 1)),
        file("folder/empty", b""),
        dir("folder/sub"),
        file("folder/sub/leaf", &bytes_of(2049, 2)),
    ]
}

fn iso(rock_ridge: bool) -> Disc {
    Disc {
        items: items(),
        iso: true,
        rock_ridge,
        ..Disc::default()
    }
}

fn udf(with_iso: bool) -> Disc {
    Disc {
        items: items(),
        iso: with_iso,
        udf: Some(Udf::default()),
        ..Disc::default()
    }
}

fn plain_tree(upper: bool) -> BTreeMap<String, Vec<u8>> {
    let name = |name: &str| {
        if upper {
            name.to_uppercase()
        } else {
            name.to_owned()
        }
    };
    let mut tree = BTreeMap::new();
    tree.insert(name("hello.txt"), b"hello\n".to_vec());
    tree.insert(name("folder"), b"/".to_vec());
    tree.insert(name("folder/data.bin"), bytes_of(5000, 1));
    tree.insert(name("folder/empty"), Vec::new());
    tree.insert(name("folder/sub"), b"/".to_vec());
    tree.insert(name("folder/sub/leaf"), bytes_of(2049, 2));
    tree
}

fn record(built: &Built, path: &str) -> usize {
    built.layout.iso_records[path]
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_both32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    bytes[at + 4..at + 8].copy_from_slice(&value.to_be_bytes());
}

#[test]
fn every_kind_of_disc_extracts_byte_for_byte_and_verifies_without_a_destination() {
    for (case, disc, upper) in [
        ("iso", iso(false), true),
        ("rock ridge", iso(true), false),
        (
            "joliet",
            Disc {
                joliet: true,
                ..iso(false)
            },
            false,
        ),
        ("udf", udf(false), false),
        ("udf and iso", udf(true), false),
        (
            "udf metadata",
            Disc {
                udf: Some(Udf {
                    revision: 0x0250,
                    extended: true,
                    metadata: true,
                    ads: Ads::Long,
                    ..Udf::default()
                }),
                ..udf(true)
            },
            false,
        ),
        (
            "multi-extent",
            Disc {
                extent_limit: Some(2048),
                ..iso(true)
            },
            false,
        ),
    ] {
        let built = disc.build();
        assert_eq!(extract(&built.bytes), plain_tree(upper), "{case}");
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.iso");
        fs::write(&archive, &built.bytes).unwrap();
        let listed = read_archive(&archive, None, ReadOptions::default()).unwrap();
        assert_eq!(listed.len(), plain_tree(upper).len(), "{case}");
    }
}

#[test]
fn rock_ridge_links_stay_inside_and_keep_their_targets() {
    let disc = Disc {
        items: vec![
            dir("a"),
            dir("a/b"),
            file("a/target", b"t"),
            symlink("a/b/up", "../target"),
            symlink("a/near", "target"),
        ],
        ..iso(true)
    };
    let tree = extract(&disc.build().bytes);
    assert_eq!(tree["a/b/up"], b"-> ../target");
    assert_eq!(tree["a/near"], b"-> target");
    for (case, target) in [
        ("escaping link", "../../../outside"),
        ("absolute link", "/etc/passwd"),
    ] {
        let disc = Disc {
            items: vec![dir("a"), symlink("a/link", target)],
            ..iso(true)
        };
        assert_rejected_with(case, &disc.build().bytes, "symlink");
    }
}

#[test]
fn iso_folders_that_loop_or_repeat_are_refused() {
    let built = iso(false).build();
    let root = built.layout.iso_dirs[""];
    let mut looping = built.bytes.clone();
    put_both32(&mut looping, record(&built, "folder/sub") + 2, root.0);
    assert_rejected_with("sub folder pointing at the root", &looping, "reached twice");

    let mut shared = built.bytes.clone();
    let folder = built.layout.iso_dirs["folder"];
    put_both32(&mut shared, record(&built, "folder/sub") + 2, folder.0);
    put_both32(&mut shared, record(&built, "folder/sub") + 10, folder.1);
    assert_rejected_with("folder pointing at itself", &shared, "reached twice");

    let disc = Disc {
        items: vec![dir("a"), dir("b"), file("a/x", b"1")],
        ..iso(false)
    };
    let built = disc.build();
    let mut twice = built.bytes.clone();
    let a = built.layout.iso_dirs["a"];
    put_both32(&mut twice, record(&built, "b") + 2, a.0);
    put_both32(&mut twice, record(&built, "b") + 10, a.1);
    assert_rejected_with("two folders sharing one extent", &twice, "reached twice");
}

#[test]
fn folders_nested_past_256_are_refused() {
    let mut items = Vec::new();
    let mut path = String::new();
    for _ in 0..257 {
        path = if path.is_empty() {
            "d".to_owned()
        } else {
            format!("{path}/d")
        };
        items.push(dir(&path));
    }
    let disc = Disc {
        items,
        iso: true,
        ..Disc::default()
    };
    assert_rejected_with("deep folders", &disc.build().bytes, "nest more than 256");
}

#[test]
fn iso_records_that_lie_are_refused() {
    let built = iso(false).build();
    let at = record(&built, "folder/data.bin");
    let image_sectors = (built.bytes.len() / SECTOR) as u32;
    let cases: Vec<(&str, Damage, &str)> = vec![
        (
            "extent past the end",
            Box::new(move |b| put_both32(b, at + 2, image_sectors)),
            "past the end",
        ),
        (
            "size past the end",
            Box::new(move |b| put_both32(b, at + 10, u32::MAX)),
            "past the end",
        ),
        (
            "two byte orders",
            Box::new(move |b| b[at + 9] ^= 1),
            "two byte orders",
        ),
        (
            "interleaved",
            Box::new(move |b| b[at + 26] = 1),
            "interleaved",
        ),
        (
            "extended attributes",
            Box::new(move |b| b[at + 1] = 1),
            "extended attribute",
        ),
        (
            "record longer than its folder",
            Box::new(move |b| {
                let folder = record(&iso(false).build(), "folder");
                put_both32(b, folder + 10, 40);
            }),
            "cut short",
        ),
        (
            "name past the record",
            Box::new(move |b| b[at + 32] = 200),
            "name runs past",
        ),
        (
            "empty name",
            Box::new(move |b| b[at + 32] = 0),
            "empty name",
        ),
        (
            "data after the records",
            Box::new(move |b| {
                let sector_end = (at / SECTOR + 1) * SECTOR;
                b[sector_end - 1] = 1;
            }),
            "after the end of its records",
        ),
        (
            "folder past the end",
            Box::new(move |b| {
                let folder = record(&iso(false).build(), "folder");
                put_both32(b, folder + 10, u32::MAX - 2047);
            }),
            "past the end",
        ),
        (
            "first extent of a file with no last one",
            Box::new(move |b| b[at + 25] |= 0x80),
            "without its last extent",
        ),
        (
            "folder with several extents",
            Box::new(move |b| {
                let folder = record(&iso(false).build(), "folder");
                b[folder + 25] |= 0x80;
            }),
            "several extents",
        ),
    ];
    for (case, change, words) in cases {
        let mut bytes = built.bytes.clone();
        change(&mut bytes);
        assert_rejected_with(case, &bytes, words);
    }
}

#[test]
fn a_multi_extent_file_whose_parts_disagree_is_refused() {
    let disc = Disc {
        items: vec![file("big", &bytes_of(6000, 3)), file("next", b"n")],
        extent_limit: Some(2048),
        ..iso(false)
    };
    let built = disc.build();
    let at = record(&built, "big");
    let length = built.bytes[at] as usize;
    let mut renamed = built.bytes.clone();
    renamed[at + length + 33] = b'Q';
    assert_rejected_with("second extent renamed", &renamed, "without its last extent");
    let mut flagged = built.bytes.clone();
    flagged[at + length + 25] |= 0x04;
    assert_rejected_with("second extent flagged", &flagged, "without its last extent");
    assert_eq!(
        extract(&built.bytes),
        expected(&[("BIG", &bytes_of(6000, 3)), ("NEXT", b"n")])
    );
}

#[test]
fn colliding_and_unsafe_names_are_refused() {
    let named = |items: Vec<Item>| Disc {
        items,
        iso: true,
        ..Disc::default()
    };
    let with_id = |path: &str, id: &[u8]| Item {
        iso_id: Some(id.to_vec()),
        ..file(path, b"x")
    };
    assert_rejected_with(
        "names equal once their version is dropped",
        &named(vec![with_id("a", b"A.;1"), with_id("b", b"A;1")])
            .build()
            .bytes,
        "two entries named",
    );
    assert_rejected_with(
        "a root folder named like the boot folder",
        &Disc {
            items: vec![Item {
                iso_id: Some(b"[BOOT]".to_vec()),
                ..dir("boot")
            }],
            boot: vec![Boot {
                bootable: true,
                media: 0,
                sectors: 4,
                data: vec![1; 2048],
                vendor: None,
            }],
            ..named(Vec::new())
        }
        .build()
        .bytes,
        "two entries named",
    );
    for (case, id) in [
        ("dot dot", &b".."[..]),
        ("backslash", b"A\\B;1"),
        ("colon", b"C:;1"),
        ("NUL", b"A\0B;1"),
        ("not UTF-8", b"\xff;1"),
    ] {
        assert_rejected_without_leftovers(case, &named(vec![with_id("a", id)]).build().bytes);
    }
    for (case, name) in [
        ("rock ridge slash", &b"a/b"[..]),
        ("rock ridge dot dot", b".."),
        ("rock ridge NUL", b"a\0"),
    ] {
        let disc = Disc {
            items: vec![Item {
                rock_ridge_name: Some(name.to_vec()),
                ..file("a", b"x")
            }],
            ..iso(true)
        };
        assert_rejected_without_leftovers(case, &disc.build().bytes);
    }
    for (case, name) in [
        ("joliet backslash", "a\\b"),
        ("joliet NUL", "a\0"),
        ("joliet dot dot", ".."),
    ] {
        let disc = Disc {
            items: vec![Item {
                joliet_id: Some(name.encode_utf16().collect()),
                ..file("a", b"x")
            }],
            joliet: true,
            ..iso(false)
        };
        assert_rejected_without_leftovers(case, &disc.build().bytes);
    }
    let unpaired = Disc {
        items: vec![Item {
            joliet_id: Some(vec![0xD800, b'a' as u16]),
            ..file("a", b"x")
        }],
        joliet: true,
        ..iso(false)
    };
    assert_rejected_with("unpaired surrogate", &unpaired.build().bytes, "surrogate");
}

fn rock_ridge_entry(built: &Built, path: &str, signature: &[u8; 2]) -> usize {
    let at = record(built, path);
    let length = built.bytes[at] as usize;
    let id_length = built.bytes[at + 32] as usize;
    let mut entry = at + 33 + id_length + usize::from(id_length.is_multiple_of(2));
    while entry < at + length {
        if &built.bytes[entry..entry + 2] == signature {
            return entry;
        }
        entry += built.bytes[entry + 2] as usize;
    }
    panic!("{path} has no {signature:?} entry")
}

#[test]
fn rock_ridge_fields_7zip_reads_differently_are_refused() {
    let built = Disc {
        items: vec![
            file("named", b"n"),
            dir("folder"),
            symlink("folder/link", "x/y"),
        ],
        ..iso(true)
    };
    let built = built.build();
    let nm = rock_ridge_entry(&built, "named", b"NM");
    let px = rock_ridge_entry(&built, "named", b"PX");
    let sl = rock_ridge_entry(&built, "folder/link", b"SL");
    let cases: Vec<(&str, Damage, &str)> = vec![
        (
            "name that continues",
            Box::new(move |b| b[nm + 4] = 1),
            "continues",
        ),
        (
            "name of the current folder",
            Box::new(move |b| b[nm + 4] = 2),
            "continues or names",
        ),
        (
            "name of another version",
            Box::new(move |b| b[nm + 3] = 2),
            "continues or names",
        ),
        (
            "relocated folder",
            Box::new(move |b| b[nm..nm + 2].copy_from_slice(b"RE")),
            "relocates",
        ),
        (
            "child link",
            Box::new(move |b| b[nm..nm + 2].copy_from_slice(b"CL")),
            "relocates",
        ),
        (
            "repeated mode",
            Box::new(move |b| b[nm..nm + 2].copy_from_slice(b"PX")),
            "",
        ),
        (
            "mode of a folder on a file",
            Box::new(move |b| put_both32(b, px + 4, 0o40755)),
            "another file type",
        ),
        (
            "mode of a device",
            Box::new(move |b| put_both32(b, px + 4, 0o60644)),
            "another file type",
        ),
        (
            "mode with two orders",
            Box::new(move |b| b[px + 4] ^= 1),
            "two byte orders",
        ),
        (
            "link that continues",
            Box::new(move |b| b[sl + 4] = 1),
            "continues",
        ),
        (
            "link component that continues",
            Box::new(move |b| b[sl + 5] = 1),
            "continues",
        ),
        (
            "link component of unknown kind",
            Box::new(move |b| b[sl + 5] = 0x40),
            "unknown flags",
        ),
        (
            "link component past its entry",
            Box::new(move |b| b[sl + 6] = 200),
            "runs past",
        ),
        (
            "entry longer than its area",
            Box::new(move |b| b[nm + 2] = 250),
            "does not fit",
        ),
    ];
    for (case, change, words) in cases {
        let mut bytes = built.bytes.clone();
        change(&mut bytes);
        assert_rejected_with(case, &bytes, words);
    }
}

#[test]
fn rock_ridge_names_in_continuation_areas_are_refused_and_their_loops_too() {
    let disc = Disc {
        items: vec![file("named", b"n")],
        names_in_continuation: true,
        ..iso(true)
    };
    let built = disc.build();
    assert_rejected_with(
        "name in a continuation area",
        &built.bytes,
        "continuation area",
    );
    let ce = rock_ridge_entry(&built, "named", b"CE");
    let area = built.layout.continuation["named"];
    let mut looping = built.bytes.clone();
    put_both32(&mut looping, ce + 20, 28);
    let entry = looping[ce..ce + 28].to_vec();
    looping[area..area + 28].copy_from_slice(&entry);
    assert_rejected_with("continuation area naming itself", &looping, "loop");
    let mut past = built.bytes.clone();
    put_both32(&mut past, ce + 4, (built.bytes.len() / SECTOR) as u32);
    assert_rejected_with("continuation area past the end", &past, "past the end");
    let mut crossing = built.bytes.clone();
    put_both32(&mut crossing, ce + 12, 2045);
    assert_rejected_with(
        "continuation area across a block",
        &crossing,
        "crosses a block",
    );
}

#[test]
fn link_targets_kept_as_data_follow_their_mode() {
    let built = Disc {
        items: vec![
            file("target", b"t"),
            Item {
                mode: 0o120777,
                ..file("link", b"target")
            },
        ],
        ..iso(true)
    }
    .build();
    assert_eq!(
        extract(&built.bytes),
        expected(&[("link", b"-> target"), ("target", b"t")])
    );
    let long = Disc {
        items: vec![Item {
            mode: 0o120777,
            ..file("link", &[b'a'; 4097])
        }],
        ..iso(true)
    };
    assert_rejected_with("long target", &long.build().bytes, "longer than 4096");
}

#[test]
fn boot_catalogs_that_lie_are_refused() {
    let disc = Disc {
        boot: vec![
            Boot {
                bootable: true,
                media: 0,
                sectors: 4,
                data: bytes_of(2048, 4),
                vendor: None,
            },
            Boot {
                bootable: false,
                media: 0,
                sectors: 4,
                data: bytes_of(2048, 5),
                vendor: Some("v".to_owned()),
            },
        ],
        ..iso(false)
    };
    let built = disc.build();
    let catalog = built.layout.catalog.unwrap();
    let mut tree = plain_tree(true);
    tree.insert("[BOOT]".into(), b"/".to_vec());
    tree.insert("[BOOT]/1-Boot-NoEmul.img".into(), bytes_of(2048, 4));
    tree.insert("[BOOT]/2-NotBoot-NoEmul-v.img".into(), bytes_of(2048, 5));
    assert_eq!(extract(&built.bytes), tree);
    let image_sectors = (built.bytes.len() / SECTOR) as u32;
    let boot_record = built.layout.boot_record.unwrap();
    let cases: Vec<(&str, Damage, &str)> = vec![
        (
            "validation checksum",
            Box::new(move |b| b[catalog + 28] ^= 1),
            "validation entry",
        ),
        (
            "initial entry",
            Box::new(move |b| b[catalog + 32] = 0x44),
            "initial entry",
        ),
        (
            "section entry",
            Box::new(move |b| b[catalog + 96] = 0x44),
            "section entry",
        ),
        (
            "image past the end",
            Box::new(move |b| put32(b, catalog + 40, image_sectors)),
            "past the end",
        ),
        (
            "catalog past the end",
            Box::new(move |b| put32(b, boot_record + 0x47, image_sectors)),
            "past the end",
        ),
        (
            "extension that never ends",
            Box::new(move |b| {
                b[catalog + 97] |= 0x20;
                for entry in 4..64 {
                    b[catalog + entry * 32] = 0x44;
                    b[catalog + entry * 32 + 1] = 0x20;
                }
            }),
            "extensions",
        ),
    ];
    for (case, change, words) in cases {
        let mut bytes = built.bytes.clone();
        change(&mut bytes);
        assert_rejected_with(case, &bytes, words);
    }
}

#[test]
fn volume_descriptors_that_lie_are_refused() {
    let built = Disc {
        joliet: true,
        ..iso(false)
    }
    .build();
    let primary = built.layout.primary.unwrap();
    let joliet = built.layout.joliet.unwrap();
    let cases: Vec<(&str, Damage, &str)> = vec![
        (
            "small blocks",
            Box::new(move |b| {
                b[joliet + 128..joliet + 132].copy_from_slice(&[0, 2, 2, 0]);
            }),
            "blocks of 512 bytes are not supported",
        ),
        (
            "primary not first",
            Box::new(move |b| b[primary] = 2),
            "not the primary",
        ),
        (
            "bad identifier",
            Box::new(move |b| b[joliet + 1] = b'X'),
            "CD001",
        ),
        (
            "unknown version",
            Box::new(move |b| b[joliet + 6] = 3),
            "version",
        ),
        (
            "unused field",
            Box::new(move |b| b[primary + 75] = 1),
            "unused field",
        ),
        (
            "date letters",
            Box::new(move |b| b[primary + 813] = b'x'),
            "date",
        ),
        (
            "root record",
            Box::new(move |b| b[primary + 156] = 40),
            "record",
        ),
        (
            "no terminator",
            Box::new(move |b| {
                for sector in 16..272 {
                    if sector * SECTOR + 7 > b.len() {
                        break;
                    }
                    b[sector * SECTOR..sector * SECTOR + 7].copy_from_slice(b"\x03CD001\x01");
                }
                b[primary] = 1;
            }),
            "volume descriptors",
        ),
    ];
    for (case, change, words) in cases {
        let mut bytes = built.bytes.clone();
        change(&mut bytes);
        assert_rejected_with(case, &bytes, words);
    }
}

fn fid(built: &Built, path: &str) -> usize {
    built.layout.udf_fids[path]
}

fn entry(built: &Built, path: &str) -> usize {
    built.layout.udf_entries[path]
}

fn point_fid_at(bytes: &mut [u8], at: usize, block_of: usize, built: &Built) {
    let block = ((block_of - built.layout.partition_start as usize * SECTOR) / SECTOR) as u32;
    put32(bytes, at + 24, block);
    retag_with_stored_crc_length(bytes, at);
}

#[test]
fn udf_folders_that_loop_fall_back_or_are_refused() {
    for with_iso in [false, true] {
        let built = udf(with_iso).build();
        let mut looping = built.bytes.clone();
        point_fid_at(
            &mut looping,
            fid(&built, "folder/sub"),
            entry(&built, "folder"),
            &built,
        );
        if with_iso {
            assert_eq!(
                extract(&looping),
                plain_tree(true),
                "a loop falls back to ISO 9660"
            );
        } else {
            assert_rejected_with("loop", &looping, "contains itself");
        }
        let mut shared = built.bytes.clone();
        point_fid_at(
            &mut shared,
            fid(&built, "hello.txt"),
            entry(&built, "folder/sub"),
            &built,
        );
        shared[fid(&built, "hello.txt") + 18] = 2;
        retag_with_stored_crc_length(&mut shared, fid(&built, "hello.txt"));
        assert_rejected_with("shared folder", &shared, "reached from two folders");
    }
}

#[test]
fn a_udf_file_named_twice_extracts_twice() {
    let built = udf(false).build();
    let mut linked = built.bytes.clone();
    point_fid_at(
        &mut linked,
        fid(&built, "folder/empty"),
        entry(&built, "hello.txt"),
        &built,
    );
    let mut tree = plain_tree(false);
    tree.insert("folder/empty".into(), b"hello\n".to_vec());
    assert_eq!(extract(&linked), tree);
}

#[test]
fn udf_structures_that_lie_are_refused() {
    let built = udf(false).build();
    let data_entry = entry(&built, "folder/data.bin");
    let ad = data_entry + 176;
    let partition = built.layout.partition_descriptor.unwrap();
    let cases: Vec<(&str, Damage, &str)> = vec![
        (
            "extent outside the partition",
            Box::new(move |b| {
                put32(b, ad + 4, 1 << 20);
                retag_with_stored_crc_length(b, data_entry);
            }),
            "outside its partition",
        ),
        (
            "unrecorded extent",
            Box::new(move |b| {
                b[ad + 3] |= 0x40;
                retag_with_stored_crc_length(b, data_entry);
            }),
            "do not hold its size",
        ),
        (
            "extents shorter than the file",
            Box::new(move |b| {
                put32(b, ad, 100);
                retag_with_stored_crc_length(b, data_entry);
            }),
            "do not hold its size",
        ),
        (
            "partition past the image",
            Box::new(move |b| {
                put32(b, partition + 192, u32::MAX / 4096);
                let past = (b.len() / SECTOR) as u32;
                put32(b, ad + 4, past);
                retag_with_stored_crc_length(b, partition);
                retag_with_stored_crc_length(b, data_entry);
            }),
            "past the end",
        ),
        (
            "reserved descriptor type",
            Box::new(move |b| {
                b[data_entry + 34] = 5;
                retag_with_stored_crc_length(b, data_entry);
            }),
            "reserved allocation",
        ),
    ];
    for (case, change, words) in cases {
        let mut bytes = built.bytes.clone();
        change(&mut bytes);
        assert_rejected_with(case, &bytes, words);
    }
}

#[test]
fn udf_names_that_are_unsafe_or_unclear_are_refused() {
    for (case, name) in [
        ("slash", &b"\x08a/b"[..]),
        ("dot dot", b"\x08.."),
        ("NUL", b"\x08a\0"),
        ("unknown compression", b"\x09abc"),
        ("odd UCS-2", b"\x10\0a\0"),
        ("UCS-2 NUL", b"\x10\0\0"),
        ("unpaired surrogate", b"\x10\xd8\x00"),
    ] {
        let disc = Disc {
            items: vec![Item {
                udf_name: Some(name.to_vec()),
                ..file("a", b"x")
            }],
            ..udf(false)
        };
        assert_rejected_without_leftovers(case, &disc.build().bytes);
    }
    let twice = Disc {
        items: vec![
            Item {
                udf_name: Some(b"\x08same".to_vec()),
                ..file("a", b"x")
            },
            Item {
                udf_name: Some(b"\x10\0s\0a\0m\0e".to_vec()),
                ..file("b", b"y")
            },
        ],
        ..udf(false)
    };
    assert_rejected_with(
        "one name in two spellings",
        &twice.build().bytes,
        "two entries named",
    );
}

#[test]
fn udf_the_reader_does_not_open_is_refused_by_name() {
    let built = udf(false).build();
    let lvd = built.layout.logical_volume.unwrap();
    let mut small = vec![0u8; 257 * 512 + 512];
    small[256 * 512..256 * 512 + 512]
        .copy_from_slice(&built.bytes[256 * SECTOR..256 * SECTOR + 512]);
    assert_rejected_with(
        "512-byte sectors",
        &small,
        "UDF with 512-byte sectors is not supported",
    );
    let mut two = built.bytes.clone();
    let terminator = lvd + SECTOR;
    two.copy_within(lvd..lvd + SECTOR, terminator);
    put32(&mut two, terminator + 12, (terminator / SECTOR) as u32);
    retag_with_stored_crc_length(&mut two, terminator);
    let anchor = built.layout.anchor.unwrap();
    put32(&mut two, anchor + 16, 5 * SECTOR as u32);
    retag_with_stored_crc_length(&mut two, anchor);
    let end = terminator + SECTOR;
    two[end..end + 16].copy_from_slice(&built.bytes[terminator..terminator + 16]);
    put32(&mut two, end + 12, (end / SECTOR) as u32);
    two[end + 10..end + 12].copy_from_slice(&496u16.to_le_bytes());
    retag_with_stored_crc_length(&mut two, end);
    assert_rejected_with("two logical volumes", &two, "several logical volumes");
}

fn every_cut_fails_cleanly_or_extracts_the_whole_tree(case: &str, built: &Built, step: usize) {
    let whole = extract(&built.bytes);
    for length in (0..built.bytes.len()).step_by(step) {
        let cut = &built.bytes[..length];
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.iso");
        fs::write(&archive, cut).unwrap();
        let destination = root.path().join("out");
        match read_archive(&archive, Some(&destination), ReadOptions::default()) {
            Ok(_) => assert_eq!(tree(&destination), whole, "{case} cut at {length}"),
            Err(_) => assert!(!destination.exists(), "{case} cut at {length} left output"),
        }
    }
}

#[test]
fn every_cut_is_refused_or_extracts_the_whole_tree() {
    every_cut_fails_cleanly_or_extracts_the_whole_tree("rock ridge", &iso(true).build(), 509);
    every_cut_fails_cleanly_or_extracts_the_whole_tree("udf", &udf(true).build(), 509);
}

#[test]
fn every_damaged_metadata_byte_is_refused_or_extracts_without_a_panic() {
    for (case, built) in [
        ("rock ridge", iso(true).build()),
        ("udf", udf(false).build()),
    ] {
        let mut regions: Vec<(usize, usize)> = Vec::new();
        for (sector, size) in built.layout.iso_dirs.values() {
            regions.push((*sector as usize * SECTOR, *size as usize));
        }
        for at in built.layout.udf_entries.values() {
            regions.push((*at, 256));
        }
        for (at, length) in built.layout.udf_dir_data.values() {
            regions.push((*at, *length));
        }
        for at in [
            built.layout.primary,
            built.layout.logical_volume,
            built.layout.file_set,
        ]
        .into_iter()
        .flatten()
        {
            regions.push((at, 512));
        }
        for (start, length) in regions {
            for at in start..start + length {
                for flip in [0x01u8, 0x80, 0xFF] {
                    let mut bytes = built.bytes.clone();
                    bytes[at] ^= flip;
                    let root = tempfile::tempdir().unwrap();
                    let archive = root.path().join("case.iso");
                    fs::write(&archive, &bytes).unwrap();
                    let destination = root.path().join("out");
                    if read_archive(&archive, Some(&destination), ReadOptions::default()).is_err() {
                        assert!(!destination.exists(), "{case} byte {at} left output");
                    }
                }
            }
        }
    }
}

#[test]
fn output_limit_and_options_apply() {
    let bytes = iso(true).build().bytes;
    let limited = extract_with(
        &bytes,
        ReadOptions {
            max_output_bytes: Some(100),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", limited.unwrap_err()).contains("max-extracted-bytes"));
    let selected = extract_with(
        &bytes,
        ReadOptions {
            paths: &["folder/sub".to_owned()],
            strip: Strip::Components(1),
            existing: Existing::Refuse,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        selected,
        expected(&[("sub", b"/"), ("sub/leaf", &bytes_of(2049, 2))])
    );
}

fn a_dot_folder_beside(name: impl Fn(Item) -> Item, disc: Disc) -> Vec<u8> {
    Disc {
        items: vec![file("x", b"outer"), name(dir("d")), file("d/x", b"inner")],
        ..disc
    }
    .build()
    .bytes
}

#[test]
fn a_root_folder_named_dot_or_dot_dot_is_refused_rather_than_merged() {
    let plain = Disc {
        iso: true,
        ..Disc::default()
    };
    for (case, bytes) in [
        (
            "ISO id ..",
            a_dot_folder_beside(
                |item| Item {
                    iso_id: Some(b"..".to_vec()),
                    ..item
                },
                plain.clone(),
            ),
        ),
        (
            "ISO id .;1",
            a_dot_folder_beside(
                |item| Item {
                    iso_id: Some(b".;1".to_vec()),
                    ..item
                },
                plain.clone(),
            ),
        ),
        (
            "Rock Ridge ..",
            a_dot_folder_beside(
                |item| Item {
                    rock_ridge_name: Some(b"..".to_vec()),
                    ..item
                },
                Disc {
                    rock_ridge: true,
                    ..plain.clone()
                },
            ),
        ),
        (
            "Joliet ..",
            a_dot_folder_beside(
                |item| Item {
                    joliet_id: Some("..".encode_utf16().collect()),
                    ..item
                },
                Disc {
                    joliet: true,
                    ..plain.clone()
                },
            ),
        ),
        (
            "UDF .",
            a_dot_folder_beside(
                |item| Item {
                    udf_name: Some(b"\x08.".to_vec()),
                    ..item
                },
                Disc {
                    udf: Some(Udf::default()),
                    ..Disc::default()
                },
            ),
        ),
    ] {
        assert_rejected_with(case, &bytes, "names no entry");
    }
}

#[test]
fn a_udf_folder_whose_extents_repeat_is_refused() {
    let built = udf(false).build();
    let folder = entry(&built, "folder");
    let mut repeated = built.bytes.clone();
    let ads = folder + 176;
    let length =
        u32::from_le_bytes(repeated[folder + 172..folder + 176].try_into().unwrap()) as usize;
    let first = repeated[ads..ads + length].to_vec();
    let size = u64::from_le_bytes(repeated[folder + 56..folder + 64].try_into().unwrap());
    for copy in 1..4 {
        repeated[ads + copy * length..ads + (copy + 1) * length].copy_from_slice(&first);
    }
    put32(&mut repeated, folder + 172, 4 * length as u32);
    repeated[folder + 56..folder + 64].copy_from_slice(&(4 * size).to_le_bytes());
    let crc_length = 176 + 4 * length - 16;
    repeated[folder + 10..folder + 12].copy_from_slice(&(crc_length as u16).to_le_bytes());
    retag_with_stored_crc_length(&mut repeated, folder);
    assert_rejected_with("one extent four times", &repeated, "overlap");
}

#[test]
fn a_folder_of_more_entries_than_the_limit_is_refused_while_it_is_read() {
    let mut items = vec![dir("crowd")];
    items.extend((0..1_000_001).map(|index| file(&format!("crowd/{index:07}"), b"")));
    let disc = Disc {
        items,
        iso: true,
        ..Disc::default()
    };
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.iso");
    fs::write(&archive, disc.build().bytes).unwrap();
    let error = read_archive(&archive, None, ReadOptions::default()).unwrap_err();
    assert!(
        format!("{error:#}").contains("folder holds more than 1000000 entries"),
        "{error:#}"
    );
}
