//! Crafted and damaged WIMs, each refused with nothing published, or read
//! to exactly the bytes the archive stores.
#[path = "common/mod.rs"]
mod common;
#[path = "common/wim_writer.rs"]
mod wim_writer;

use std::{collections::BTreeMap, fs, path::Path};
use wim_writer::{self as wim, Compression, Layout, Node, Options, dir, file, symlink, utf16};
use zipper::native_wim::{ReadOptions, read_archive};

fn sample_images() -> Vec<Vec<Node>> {
    vec![vec![
        file("hello.txt", b"hello\n"),
        dir(
            "folder",
            vec![
                file("data.bin", &b"0123456789".repeat(2000)),
                dir("inner", vec![file("deep.txt", b"deep\n")]),
            ],
        ),
        file("empty", b""),
    ]]
}

fn options() -> Options {
    Options::default()
}

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
                .replace(std::path::MAIN_SEPARATOR, "/");
            let kind = if entry.file_type().is_symlink() {
                let target = fs::read_link(entry.path()).unwrap();
                let target = target
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/");
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

fn extract(wim: &[u8]) -> (tempfile::TempDir, anyhow::Result<BTreeMap<String, Vec<u8>>>) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("hostile.wim");
    fs::write(&archive, wim).unwrap();
    let into = root.path().join("out");
    let result = read_archive(&archive, Some(&into), ReadOptions::default()).map(|_| tree(&into));
    if result.is_err() {
        assert!(!into.exists(), "a refused WIM published {}", into.display());
        let left: Vec<_> = fs::read_dir(root.path()).unwrap().collect();
        assert_eq!(
            left.len(),
            1,
            "a refused WIM left files beside the destination"
        );
        assert!(read_archive(&archive, None, ReadOptions::default()).is_err());
    }
    common::make_tree_deletable(root.path());
    (root, result)
}

#[track_caller]
fn refused(wim: &[u8], because: &str) {
    let (_root, result) = extract(wim);
    let error = format!("{:#}", result.expect_err("the WIM was accepted"));
    assert!(
        error.contains(because),
        "{error:?} does not say {because:?}"
    );
    assert!(!error.contains("to 7-Zip"), "{error:?} names 7-Zip");
}

fn with_metadata(images: &[Vec<Node>], hook: impl Fn(&mut Vec<u8>) + 'static) -> Vec<u8> {
    wim_writer::write(
        images,
        &Options {
            metadata_hook: Some(Box::new(move |_, meta| hook(meta))),
            ..options()
        },
    )
}

fn with_file(
    images: &[Vec<Node>],
    options: Options,
    hook: impl Fn(&mut Vec<u8>, &Layout) + 'static,
) -> Vec<u8> {
    wim_writer::write(
        images,
        &Options {
            file_hook: Some(Box::new(hook)),
            ..options
        },
    )
}

fn dentry_of(meta: &[u8], name: &str) -> usize {
    let name_bytes = utf16(name);
    let mut needle = (name_bytes.len() as u16).to_le_bytes().to_vec();
    needle.extend(&name_bytes);
    needle.extend([0, 0]);
    let at = meta
        .windows(needle.len())
        .enumerate()
        .position(|(at, window)| window == needle && at + 2 >= wim_writer::DENTRY)
        .unwrap_or_else(|| panic!("no dentry named {name}"));
    at + 2 - wim_writer::DENTRY
}

fn put_u64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

#[test]
fn every_cut_is_refused_and_publishes_nothing() {
    for compression in [Compression::Stored, Compression::Xpress] {
        let wim = wim_writer::write(
            &sample_images(),
            &Options {
                compression,
                chunk_size: 4096,
                ..options()
            },
        );
        let (_root, whole) = extract(&wim);
        whole.unwrap();
        for length in 0..wim.len() {
            let (_root, result) = extract(&wim[..length]);
            assert!(
                result.is_err(),
                "{compression:?} cut to {length} bytes was accepted"
            );
        }
    }
}

#[test]
fn every_flipped_byte_is_refused_or_reads_the_stored_files() {
    for compression in [Compression::Stored, Compression::Xpress] {
        let wim = wim_writer::write(
            &sample_images(),
            &Options {
                compression,
                chunk_size: 4096,
                ..options()
            },
        );
        let (_root, whole) = extract(&wim);
        let whole = whole.unwrap();
        let table = u64::from_le_bytes(wim[56..64].try_into().unwrap()) as usize;
        let table_bytes = u64::from_le_bytes(wim[64..72].try_into().unwrap()) as usize;
        let last = table + table_bytes - 50;
        let data_end = u64::from_le_bytes(wim[last + 8..last + 16].try_into().unwrap()) as usize;
        let positions = (0..wim::HEADER)
            .chain((wim::HEADER..data_end).step_by(37))
            .chain(data_end..wim.len());
        for at in positions {
            let mut damaged = wim.clone();
            damaged[at] ^= if at % 2 == 0 { 0x01 } else { 0x80 };
            if let (_root, Ok(found)) = extract(&damaged) {
                assert_eq!(found, whole, "{compression:?} byte {at} flipped");
            }
        }
    }
}

#[test]
fn a_folder_list_that_loops_back_is_refused() {
    let images = sample_images();
    refused(
        &with_metadata(&images, |meta| {
            let inner = dentry_of(meta, "inner");
            let folder = dentry_of(meta, "folder");
            let list = get_u64(meta, folder + 16);
            put_u64(meta, inner + 16, list);
        }),
        "overlap or form a loop",
    );
    refused(
        &with_metadata(&images, |meta| {
            let inner = dentry_of(meta, "inner");
            put_u64(meta, inner + 16, 8);
        }),
        "overlap or form a loop",
    );
}

#[test]
fn two_folders_sharing_one_list_are_refused() {
    let images = vec![vec![
        dir("a", vec![file("x", b"x")]),
        dir("b", vec![file("y", b"y")]),
    ]];
    refused(
        &with_metadata(&images, |meta| {
            let a = dentry_of(meta, "a");
            let b = dentry_of(meta, "b");
            let list = get_u64(meta, a + 16);
            put_u64(meta, b + 16, list);
        }),
        "overlap or form a loop",
    );
}

#[test]
fn folder_lists_outside_the_metadata_or_inside_the_security_data_are_refused() {
    let images = sample_images();
    refused(
        &with_metadata(&images, |meta| {
            let folder = dentry_of(meta, "folder");
            let beyond = meta.len() as u64 + 8;
            put_u64(meta, folder + 16, beyond);
        }),
        "outside its metadata",
    );
    refused(
        &with_metadata(&images, |meta| {
            meta.splice(0..8, [16, 0, 0, 0, 0, 0, 0, 0]);
            meta.splice(8..8, [0u8; 8]);
            let folder = dentry_of(meta, "folder");
            put_u64(meta, folder + 16, 8);
        }),
        "security data",
    );
}

#[test]
fn hostile_names_are_refused() {
    for name in ["..", ".", "a\\b", "c:d", "nul\0inside"] {
        refused(
            &wim_writer::write(&[vec![file(name, b"x")]], &options()),
            if name.contains('\0') {
                "holds a NUL"
            } else {
                "unsafe archive path"
            },
        );
    }
    refused(
        &with_metadata(&[vec![file("x", b"x")]], |meta| {
            let at = dentry_of(meta, "x");
            meta[at + 100] = 0;
            meta[at + 98] = 2;
        }),
        "short name and no name",
    );
    refused(
        &with_metadata(&[vec![file("xy", b"x")]], |meta| {
            let at = dentry_of(meta, "xy");
            meta[at + 100] = 3;
        }),
        "odd number of bytes",
    );
    refused(
        &with_metadata(&[vec![file("xy", b"x")]], |meta| {
            let at = dentry_of(meta, "xy");
            meta[at + 100] = 2;
        }),
        "does not end with one",
    );
}

#[test]
fn a_name_with_a_slash_becomes_one_name_with_an_underscore() {
    let (_root, found) = extract(&wim_writer::write(&[vec![file("a/b", b"x")]], &options()));
    assert_eq!(found.unwrap().keys().collect::<Vec<_>>(), ["a_b"]);
}

#[test]
fn folders_named_dot_and_names_repeated_in_one_folder_are_refused() {
    for name in [".", ".."] {
        let images = vec![vec![dir(name, vec![file("x", b"x")])]];
        refused(
            &wim_writer::write(&images, &options()),
            "unsafe archive path",
        );
    }
    let images = vec![vec![
        file("x", b"outer"),
        dir(".", vec![file("x", b"inner")]),
    ]];
    refused(
        &wim_writer::write(&images, &options()),
        "unsafe archive path",
    );
    for images in [
        vec![vec![file("same", b"one"), file("same", b"two")]],
        vec![vec![file("a/b", b"one"), file("a_b", b"two")]],
        vec![vec![dir("d", Vec::new()), dir("d", vec![file("x", b"x")])]],
    ] {
        refused(&wim_writer::write(&images, &options()), "two entries named");
    }
}

fn linked(name: &str, data: &[u8], group: u64) -> Node {
    Node::File {
        name: name.to_owned(),
        data: data.to_vec(),
        attributes: 0x80,
        link_group: group,
        streams: Vec::new(),
    }
}

#[test]
fn a_hard_link_never_names_a_file_that_a_later_entry_replaced() {
    let whole = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd";
    let images = vec![vec![
        linked("a", whole, 7),
        file("a", &whole[..10]),
        linked("b", whole, 7),
    ]];
    let solid = Options {
        solid: true,
        ..options()
    };
    let wim = with_file(&images, solid, |wim, layout| {
        let shorter = blob_entry(layout, 2);
        put_u64(wim, shorter + 8, 0);
    });
    refused(&wim, "two entries named \"a\"");
}

#[cfg(unix)]
#[test]
fn a_chunk_that_packs_more_than_its_chunk_size_is_refused_before_it_is_read() {
    let images = vec![vec![file("z", &[b'z'; 3000])]];
    let xpress = Options {
        compression: Compression::Xpress,
        chunk_size: 4096,
        ..options()
    };
    let huge = 1u64 << 40;
    let wim = with_file(&images, xpress, move |wim, layout| {
        let entry = blob_entry(layout, 0);
        let end = wim.len() as u64;
        put_u64(wim, entry + 8, end);
        wim[entry..entry + 7].copy_from_slice(&huge.to_le_bytes()[..7]);
    });
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("sparse.wim");
    fs::write(&archive, &wim).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&archive)
        .unwrap()
        .set_len(wim.len() as u64 + huge)
        .unwrap();
    let into = root.path().join("out");
    for destination in [Some(into.as_path()), None] {
        let error = read_archive(&archive, destination, ReadOptions::default()).unwrap_err();
        assert!(
            format!("{error:#}").contains("no less than its chunk size"),
            "{error:#}"
        );
    }
    assert!(!into.exists());
}

#[test]
fn links_that_leave_the_image_are_refused_and_links_inside_are_created() {
    for (target, relative) in [
        ("..\\..\\outside", true),
        ("\\..\\outside", false),
        ("\\??\\UNC\\server\\share", false),
        ("relative\\without\\flag", false),
    ] {
        let images = vec![vec![dir("folder", vec![symlink("link", target, relative)])]];
        let (_root, result) = extract(&wim_writer::write(&images, &options()));
        assert!(result.is_err(), "{target:?} was accepted");
    }
    let images = vec![vec![
        file("target", b"t"),
        dir(
            "folder",
            vec![
                symlink("up", "..\\target", true),
                symlink("rooted", "\\??\\C:\\target", false),
                symlink("root", "\\", false),
            ],
        ),
    ]];
    let (_root, found) = extract(&wim_writer::write(&images, &options()));
    let found = found.unwrap();
    assert_eq!(found["folder/up"], b"link:../target");
    assert_eq!(found["folder/rooted"], b"link:../target");
    assert_eq!(found["folder/root"], b"link:..");
}

#[test]
fn junctions_and_wsl_links_inside_the_image_are_created_and_other_reparse_points_are_refused() {
    let mut junction = wim_writer::symlink_reparse_body("\\??\\C:\\folder", false);
    junction.drain(8..12);
    let mut wsl = 2u32.to_le_bytes().to_vec();
    wsl.extend(b"folder/x");
    let images = vec![vec![
        dir("folder", vec![file("x", b"x")]),
        Node::Reparse {
            name: "junction".into(),
            tag: 0xA000_0003,
            data: junction,
            directory: true,
        },
        Node::Reparse {
            name: "wsl".into(),
            tag: 0xA000_001D,
            data: wsl,
            directory: false,
        },
    ]];
    let (_root, found) = extract(&wim_writer::write(&images, &options()));
    let found = found.unwrap();
    assert_eq!(found["junction"], b"link:folder");
    assert_eq!(found["wsl"], b"link:folder/x");
    let other = vec![vec![Node::Reparse {
        name: "dedup".into(),
        tag: 0x8000_0013,
        data: vec![1, 2, 3, 4],
        directory: false,
    }]];
    refused(
        &wim_writer::write(&other, &options()),
        "Zipper does not read it",
    );
    let damaged = vec![vec![Node::Reparse {
        name: "short".into(),
        tag: 0xA000_000C,
        data: vec![0, 0, 200, 0, 0, 0, 0, 0, 1, 0, 0, 0],
        directory: false,
    }]];
    refused(
        &wim_writer::write(&damaged, &options()),
        "damaged reparse data",
    );
}

#[test]
fn a_link_folder_with_entries_under_it_is_refused() {
    let images = vec![vec![
        dir("real", vec![file("x", b"x")]),
        Node::Reparse {
            name: "linked".into(),
            tag: 0xA000_000C,
            data: wim_writer::symlink_reparse_body("real", true),
            directory: true,
        },
    ]];
    refused(
        &with_metadata(&images, |meta| {
            let real = dentry_of(meta, "real");
            let linked = dentry_of(meta, "linked");
            let list = get_u64(meta, real + 16);
            put_u64(meta, real + 16, 0);
            put_u64(meta, linked + 16, list);
        }),
        "has entries under it",
    );
}

#[test]
fn alternate_data_streams_are_refused() {
    let named = vec![vec![Node::File {
        name: "base".into(),
        data: b"main".to_vec(),
        attributes: 0x80,
        link_group: 0,
        streams: vec![("Zone.Identifier".into(), b"[ZoneTransfer]".to_vec())],
    }]];
    refused(
        &wim_writer::write(&named, &options()),
        "alternate data stream",
    );
    let unnamed = vec![vec![Node::File {
        name: "base".into(),
        data: b"main".to_vec(),
        attributes: 0x80,
        link_group: 0,
        streams: vec![(String::new(), b"second".to_vec())],
    }]];
    refused(
        &wim_writer::write(&unnamed, &options()),
        "second unnamed stream",
    );
}

#[test]
fn data_or_metadata_that_fails_its_sha1_is_refused() {
    let images = sample_images();
    refused(
        &with_file(&images, options(), |wim, layout| {
            let (at, _) = layout.data[1];
            wim[at] ^= 1;
        }),
        "SHA-1",
    );
    refused(
        &with_file(&images, options(), |wim, layout| {
            let (at, length) = layout.metadata[0];
            wim[at + length - 1] ^= 1;
        }),
        "SHA-1",
    );
}

fn blob_entry(layout: &Layout, index: usize) -> usize {
    layout.blob_table + index * 50
}

#[test]
fn blob_tables_that_lose_repeat_or_overlap_data_are_refused() {
    let images = sample_images();
    refused(
        &with_file(&images, options(), |wim, layout| {
            let at = blob_entry(layout, 0);
            wim[at + 30] ^= 1;
        }),
        "the blob table does not have",
    );
    refused(
        &with_file(&images, options(), |wim, layout| {
            let first = blob_entry(layout, 0);
            let second = blob_entry(layout, 1);
            let hash = wim[first + 30..first + 50].to_vec();
            wim[second + 30..second + 50].copy_from_slice(&hash);
        }),
        "one hash twice",
    );
    refused(
        &with_file(&images, options(), |wim, layout| {
            let first = blob_entry(layout, 0);
            let second = blob_entry(layout, 1);
            let offset = wim[first + 8..first + 16].to_vec();
            wim[second + 8..second + 16].copy_from_slice(&offset);
        }),
        "share bytes of the file",
    );
    refused(
        &with_file(&images, options(), |wim, layout| {
            let at = blob_entry(layout, 0);
            put_u64(wim, at + 8, 1 << 40);
        }),
        "past the end of the file",
    );
    refused(
        &with_file(&images, options(), |wim, layout| {
            let at = blob_entry(layout, 0);
            put_u64(wim, at + 8, 16);
        }),
        "overlaps the header",
    );
    refused(
        &with_file(&images, options(), |wim, layout| {
            let at = blob_entry(layout, 0);
            wim[at] -= 1;
        }),
        "packed size unlike its size",
    );
}

#[test]
fn chunk_tables_that_go_backward_or_past_their_resource_are_refused() {
    let images = vec![vec![file("big", &[b'z'; 20_000])]];
    let xpress = || Options {
        compression: Compression::Xpress,
        chunk_size: 4096,
        ..options()
    };
    let (_root, plain) = extract(&wim_writer::write(&images, &xpress()));
    plain.unwrap();
    let entry = |wim: &[u8], at: usize, index: usize| {
        u32::from_le_bytes(wim[at + 4 * index..at + 4 * index + 4].try_into().unwrap())
    };
    refused(
        &with_file(&images, xpress(), move |wim, layout| {
            let (at, _) = layout.data[0];
            let first = entry(wim, at, 0);
            wim[at + 4..at + 8].copy_from_slice(&(first - 1).to_le_bytes());
        }),
        "backward or past the resource",
    );
    refused(
        &with_file(&images, xpress(), move |wim, layout| {
            let (at, packed) = layout.data[0];
            let beyond = (packed - 16 + 1) as u32;
            assert!(beyond > entry(wim, at, 2));
            wim[at + 12..at + 16].copy_from_slice(&beyond.to_le_bytes());
        }),
        "backward or past the resource",
    );
    let incompressible: Vec<u8> = (0..20_000u32)
        .map(|value| ((value * 7919) >> 3) as u8)
        .collect();
    let images = vec![vec![file("noise", &incompressible)]];
    refused(
        &with_file(&images, xpress(), |wim, layout| {
            let (at, _) = layout.data[0];
            let size = |index: usize| {
                u32::from_le_bytes(wim[at + 4 * index..at + 4 * index + 4].try_into().unwrap())
            };
            let [first, second] = [size(0), size(1)];
            assert_eq!([first, second], [4096, 8192]);
            wim[at..at + 4].copy_from_slice(&(first + 1).to_le_bytes());
        }),
        "no less than its chunk size",
    );
}

#[test]
fn split_wims_are_refused_by_this_reader() {
    let mut wim = wim_writer::write(&sample_images(), &options());
    wim[42] = 2;
    refused(&wim, "split WIM");
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("part.swm");
    fs::write(&archive, &wim).unwrap();
    assert_eq!(
        zipper::choose_reader(&archive).unwrap(),
        zipper::Reader::Wim
    );
    wim[42] = 1;
    fs::write(&archive, &wim).unwrap();
    assert_eq!(
        zipper::choose_reader(&archive).unwrap(),
        zipper::Reader::Wim
    );
    wim[40] = 2;
    refused(&wim, "part number outside its part count");
    let mut esd = wim_writer::write(
        &sample_images(),
        &Options {
            solid: true,
            ..options()
        },
    );
    esd[16..20].copy_from_slice(&(0x2u32 | 1 << 20).to_le_bytes());
    fs::write(&archive, &esd).unwrap();
    assert_eq!(
        zipper::choose_reader(&archive).unwrap(),
        zipper::Reader::Wim,
        "a damaged ESD header stays with the WIM reader"
    );
}

#[test]
fn headers_of_other_versions_sizes_and_methods_are_refused() {
    let wim = wim_writer::write(&sample_images(), &options());
    let mut old = wim.clone();
    old[12..16].copy_from_slice(&0x10C00u32.to_le_bytes());
    refused(&old, "Zipper does not read it");
    let mut short = wim.clone();
    short[8] = 0x74;
    refused(&short, "Zipper does not read it");
    let mut method = wim.clone();
    method[16..20].copy_from_slice(&(0x2u32 | 1 << 20).to_le_bytes());
    refused(&method, "unknown compression method");
    let mut chunk = wim.clone();
    chunk[20..24].copy_from_slice(&3000u32.to_le_bytes());
    refused(&chunk, "power of two");
    let mut uncompressed = wim_writer::write(
        &sample_images(),
        &Options {
            compression: Compression::Xpress,
            ..options()
        },
    );
    uncompressed[16] = 0;
    refused(&uncompressed, "names no compression method");
}

#[test]
fn image_counts_references_and_boot_metadata_must_agree() {
    let images = sample_images();
    let mut count = wim_writer::write(&images, &options());
    count[44] = 2;
    refused(&count, "counts 2 images");
    refused(
        &with_file(&images, options(), |wim, layout| {
            let last = blob_entry(layout, layout.blob_table_entries - 1);
            wim[last + 26] = 2;
        }),
        "referenced 2 times",
    );
    let boot = |index: u32| {
        with_file(&images, options(), move |wim, layout| {
            let last = blob_entry(layout, layout.blob_table_entries - 1);
            let entry = wim[last..last + 24].to_vec();
            wim[96..120].copy_from_slice(&entry);
            wim[120..124].copy_from_slice(&index.to_le_bytes());
        })
    };
    let (_root, accepted) = extract(&boot(1));
    accepted.unwrap();
    refused(&boot(2), "boot metadata disagrees");
    refused(
        &with_file(&images, options(), |wim, _| {
            wim[96 + 16] = 9;
            wim[96 + 8] = 1;
        }),
        "boot metadata is not the metadata of any image",
    );
}

#[test]
fn an_encrypted_esd_is_refused() {
    let mut wim = wim_writer::write(&sample_images(), &options());
    let xml_at = u64::from_le_bytes(wim[80..88].try_into().unwrap()) as usize;
    wim.truncate(xml_at);
    let mut xml = vec![0xFF, 0xFE];
    xml.extend(utf16(
        "<WIM><ESD><ENCRYPTED>1</ENCRYPTED></ESD><IMAGE INDEX=\"1\"></IMAGE></WIM>",
    ));
    wim.extend(&xml);
    let packed = xml.len() as u64;
    wim[72..79].copy_from_slice(&packed.to_le_bytes()[..7]);
    wim[88..96].copy_from_slice(&packed.to_le_bytes());
    refused(&wim, "encrypted ESD");
}

#[test]
fn solid_blobs_outside_their_resource_and_short_solid_tables_are_refused() {
    let images = sample_images();
    let solid = || Options {
        solid: true,
        ..options()
    };
    let (_root, plain) = extract(&wim_writer::write(&images, &solid()));
    plain.unwrap();
    refused(
        &with_file(&images, solid(), |wim, layout| {
            let at = blob_entry(layout, 1);
            put_u64(wim, at + 8, 1 << 30);
        }),
        "outside every solid resource",
    );
    refused(
        &with_file(&images, solid(), |wim, layout| {
            let at = blob_entry(layout, 1);
            wim[at] = 0xFF;
            wim[at + 1] = 0xFF;
        }),
        "past the end of its solid resource",
    );
    refused(
        &with_file(&images, solid(), |wim, layout| {
            let (at, _) = layout.data[0];
            wim[at + 16] ^= 1;
        }),
        "do not fill its packed size",
    );
    refused(
        &with_file(&images, solid(), |wim, layout| {
            let (at, _) = layout.data[0];
            wim[at + 12] = 9;
        }),
        "unknown compression method",
    );
    refused(
        &with_file(&images, solid(), |wim, layout| {
            let at = blob_entry(layout, 0);
            wim[at + 7] = 0x14;
        }),
        "flags other than the solid flag",
    );
}

#[test]
fn metadata_beyond_its_limit_is_refused_before_it_is_read() {
    let images = sample_images();
    let xpress = Options {
        compression: Compression::Xpress,
        ..options()
    };
    refused(
        &with_file(&images, xpress, |wim, layout| {
            let last = blob_entry(layout, layout.blob_table_entries - 1);
            put_u64(wim, last + 16, 1 << 29);
        }),
        "metadata is larger than",
    );
}

#[test]
fn a_root_other_than_one_unnamed_folder_is_refused() {
    let images = sample_images();
    refused(
        &with_metadata(&images, |meta| {
            meta[8 + 8] = 0x80;
        }),
        "root is not one folder",
    );
    refused(
        &with_metadata(&images, |meta| {
            let root_end = 8 + get_u64(meta, 8) as usize;
            let hello = dentry_of(meta, "hello.txt");
            let length = get_u64(meta, hello) as usize;
            let copy = meta[hello..hello + length].to_vec();
            meta.splice(root_end..root_end, copy);
            let shift = length as u64;
            let folder = dentry_of(meta, "folder");
            let inner = dentry_of(meta, "inner");
            for at in [8, folder, inner] {
                let list = get_u64(meta, at + 16);
                put_u64(meta, at + 16, list + shift);
            }
        }),
        "root list holds more than the root folder",
    );
}

fn write_limit_image(
    out: &Path,
    folders: usize,
    files: usize,
    name: impl Fn(usize) -> String,
    entry: impl Fn(&str, usize) -> Node,
) {
    let root: Vec<Node> = (0..folders)
        .map(|folder| {
            let children = (0..files)
                .map(|index| {
                    let number = folder * files + index;
                    entry(&name(number), number)
                })
                .collect();
            dir(&format!("d{folder:04}"), children)
        })
        .collect();
    fs::write(out, wim_writer::write(&[root], &options())).unwrap();
}

#[test]
#[ignore = "writes WIMs that fill the name and blob limits, for a peak memory measurement"]
fn write_wims_at_the_documented_limits() {
    let out = std::env::var_os("WIM_LIMITS_OUT").expect("set WIM_LIMITS_OUT to a folder");
    let out = Path::new(&out);
    write_limit_image(
        &out.join("names.wim"),
        1_000,
        998,
        |number| format!("{number:08}-{}", "n".repeat(48)),
        |name, _| file(name, b""),
    );
    write_limit_image(
        &out.join("folders.wim"),
        1_000,
        999,
        |number| format!("{number:08}-{}", "n".repeat(48)),
        |name, _| dir(name, Vec::new()),
    );
    write_limit_image(
        &out.join("groups.wim"),
        1_000,
        670,
        |number| format!("{number:07}"),
        |name, number| linked(name, &(number as u64).to_le_bytes(), number as u64 + 1),
    );
    write_limit_image(
        &out.join("blobs.wim"),
        1_000,
        670,
        |number| format!("{number:07}"),
        |name, number| file(name, &(number as u64).to_le_bytes()),
    );
}
