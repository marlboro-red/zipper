//! The xar reader extracts every encoding and checksum byte for byte,
//! rejects each hostile table of contents, heap and path with nothing left
//! behind, and honours the options the cpio reader takes. Where 7-Zip is
//! installed, it is the oracle for the valid archives.
#[path = "common/xar_writer.rs"]
mod xar_writer;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use xar_writer::*;
use zipper::{
    Strip,
    native_xar::{ReadOptions, read_archive},
};

const TOC_FILE: &str = "[TOC].xml";

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
        .filter(|path| path != "case.xar")
        .collect();
    found.sort();
    found
}

fn extract_with(
    bytes: &[u8],
    options: ReadOptions,
) -> (
    tempfile::TempDir,
    PathBuf,
    anyhow::Result<Vec<zipper::Entry>>,
) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.xar");
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    let result = read_archive(&archive, Some(&destination), options);
    (root, destination, result)
}

fn assert_rejected_saying(case: &str, bytes: &[u8], message: &str) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.xar");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let result = read_archive(&archive, Some(&destination), ReadOptions::default());
    let error = format!(
        "{:#}",
        result
            .err()
            .unwrap_or_else(|| panic!("{case}: accepted a hostile archive"))
    );
    assert!(error.contains(message), "{case}: {error}");
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile archive"
    );
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
                .replace('\\', "/");
            let kind = if entry.path_is_symlink() {
                format!("link:{}", fs::read_link(entry.path()).unwrap().display())
            } else if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                format!("{:?}", fs::read(entry.path()).unwrap())
            };
            (name, kind)
        })
        .collect()
}

fn big() -> Vec<u8> {
    (0..300_000u32).map(|n| (n * 7 % 251) as u8).collect()
}

fn sample(encoding: Encoding, toc_sum: Sum, file_sum: Sum) -> Vec<u8> {
    xar(toc_sum, file_sum, |heap| {
        let alpha = heap.store_data_element(encoding, b"alpha\n");
        let large = heap.store_data_element(encoding, &big());
        let odd = heap.store_data_element(encoding, b"x");
        let mut files = directory(
            1,
            "folder",
            &(file(2, "a.txt", "0644", &alpha) + &file(3, "big", "0644", &large)),
        );
        files += &file(4, "odd & <strange>", "0755", &odd);
        files += &file(5, "empty", "0644", "");
        files += &directory(6, "empty folder", "");
        if cfg!(unix) {
            files += &symlink(7, "link", "folder/a.txt");
        }
        files
    })
}

fn sample_tree(with_toc: bool) -> BTreeMap<String, String> {
    let mut expected: BTreeMap<String, String> = [
        ("folder", "dir".to_owned()),
        ("folder/a.txt", format!("{:?}", b"alpha\n".to_vec())),
        ("folder/big", format!("{:?}", big())),
        ("odd & <strange>", format!("{:?}", b"x".to_vec())),
        ("empty", "[]".to_owned()),
        ("empty folder", "dir".to_owned()),
    ]
    .into_iter()
    .map(|(name, kind)| (name.to_owned(), kind))
    .collect();
    if cfg!(unix) {
        expected.insert("link".to_owned(), "link:folder/a.txt".to_owned());
    }
    if !with_toc {
        expected.remove(TOC_FILE);
    }
    expected
}

fn without_toc(mut tree: BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.remove(TOC_FILE);
    tree
}

#[test]
fn every_encoding_and_checksum_extracts_byte_for_byte() {
    for encoding in Encoding::ALL {
        for (toc_sum, file_sum) in [
            (Sum::Sha1, Sum::Sha1),
            (Sum::Sha256, Sum::Sha512),
            (Sum::Sha512, Sum::Md5),
            (Sum::Md5, Sum::Sha256),
            (Sum::None, Sum::None),
        ] {
            let case = format!("{encoding:?} {toc_sum:?} {file_sum:?}");
            let bytes = sample(encoding, toc_sum, file_sum);
            let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
            result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
            let got = tree(&destination);
            assert!(got.contains_key(TOC_FILE), "{case}: no {TOC_FILE}");
            assert_eq!(without_toc(got), sample_tree(false), "{case}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = |name: &str| {
                    fs::metadata(destination.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o111
                };
                assert_ne!(mode("odd & <strange>"), 0, "{case}: lost the execute bit");
                assert_eq!(mode("empty"), 0, "{case}");
                let modified = fs::metadata(destination.join("folder/big"))
                    .unwrap()
                    .modified()
                    .unwrap();
                assert_eq!(
                    modified,
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(TIME_SECONDS),
                    "{case}"
                );
            }
            let listed =
                read_archive(&root.path().join("case.xar"), None, ReadOptions::default()).unwrap();
            assert_eq!(
                listed.iter().filter(|entry| !entry.directory).count(),
                sample_tree(false)
                    .values()
                    .filter(|kind| *kind != "dir")
                    .count()
                    + 1,
                "{case}"
            );
        }
    }
}

#[test]
fn the_table_of_contents_is_written_as_7zip_writes_it() {
    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let data = heap.store_data_element(Encoding::Zlib, b"hi");
    let text = toc_with_checksum_element(&heap, &file(1, "a", "0644", &data));
    let bytes = assemble(&text, heap);
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(
        fs::read_to_string(destination.join(TOC_FILE)).unwrap(),
        text
    );
}

#[test]
fn an_entry_named_like_the_table_of_contents_wins() {
    let bytes = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"mine");
        file(1, TOC_FILE, "0644", &data)
    });
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(fs::read(destination.join(TOC_FILE)).unwrap(), b"mine");
    let other_case = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"mine");
        file(1, "[toc].XML", "0644", &data)
    });
    let (_root, destination, result) = extract_with(&other_case, ReadOptions::default());
    result.unwrap();
    assert_eq!(fs::read(destination.join("[toc].XML")).unwrap(), b"mine");
}

#[test]
fn repeated_attributes_are_refused_as_xar_refuses_them() {
    let link = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let first = heap.store_data_element(Encoding::Stored, b"AAAA");
        let second = heap.store_data_element(Encoding::Stored, b"CCCC");
        original(1, "a", &first)
            + &original(2, "c", &second)
            + "<file id=\"3\"><type link=\"1\" link=\"2\">hardlink</type><name>b</name></file>"
    });
    assert_rejected_saying("two link attributes", &link, "attribute");
    let id = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        file(1, "a", "0644", &data).replace("<file id=\"1\">", "<file id=\"1\" id=\"2\">")
    });
    assert_rejected_saying("two ids", &id, "attribute");
    let ignored = xar(Sum::Sha1, Sum::Sha1, |_| {
        "<other a=\"1\" a=\"2\"/>".to_owned()
    });
    assert_rejected_saying(
        "two attributes on an ignored element",
        &ignored,
        "attribute",
    );
}

#[test]
fn hard_links_share_the_original_data() {
    let bytes = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Zlib, b"shared\n");
        hard_link(1, "first", 2)
            + &original(2, "second", &data)
            + &directory(3, "d", &hard_link(4, "third", 2))
    });
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    for name in ["first", "second", "d/third"] {
        assert_eq!(
            fs::read(destination.join(name)).unwrap(),
            b"shared\n",
            "{name}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let inode = |name: &str| fs::metadata(destination.join(name)).unwrap().ino();
        assert_eq!(inode("first"), inode("second"));
        assert_eq!(inode("d/third"), inode("second"));
    }
}

#[test]
fn identical_data_ranges_may_be_shared() {
    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let data = heap.store_data_element(Encoding::Zlib, b"same bytes");
    let text = toc_with_checksum_element(
        &heap,
        &(file(1, "a", "0644", &data) + &file(2, "b", "0644", &data)),
    );
    let (_root, destination, result) = extract_with(&assemble(&text, heap), ReadOptions::default());
    result.unwrap();
    assert_eq!(fs::read(destination.join("b")).unwrap(), b"same bytes");
}

#[test]
fn repeated_fields_that_agree_are_read_as_productbuild_writes_them() {
    let bytes = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Zlib, b"bom");
        file(1, "Bom", "0644", &data)
            .replace("<name>Bom</name>", "<name>Bom</name><name>Bom</name>")
    });
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(fs::read(destination.join("Bom")).unwrap(), b"bom");
}

#[test]
fn files_that_share_an_inode_keep_their_own_data_as_apple_signed_packages_need() {
    let bytes = xar(Sum::Sha1, Sum::Sha1, |heap| {
        ["Payload", "Bom", "PackageInfo"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let data = heap.store_data_element(Encoding::Stored, name.as_bytes());
                format!(
                    "<file id=\"{}\">{data}<inode>0</inode><type>file</type><name>{name}</name></file>",
                    index + 1
                )
            })
            .collect()
    });
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    for name in ["Payload", "Bom", "PackageInfo"] {
        assert_eq!(fs::read(destination.join(name)).unwrap(), name.as_bytes());
    }
}

#[test]
fn selection_and_strip_apply_to_xar_paths() {
    let bytes = sample(Encoding::Zlib, Sum::Sha1, Sum::Sha1);
    let paths = ["folder".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            paths: &paths,
            strip: Strip::Components(1),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination),
        BTreeMap::from([
            ("a.txt".to_owned(), format!("{:?}", b"alpha\n".to_vec())),
            ("big".to_owned(), format!("{:?}", big())),
        ])
    );
}

fn simple(name_xml: &str) -> Vec<u8> {
    xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        format!("<file id=\"1\">{name_xml}<type>file</type>{data}</file>")
    })
}

#[test]
fn unsafe_names_are_refused() {
    for (case, name, message) in [
        ("dot dot", "<name>..</name>", "not a plain name"),
        ("dot", "<name>.</name>", "not a plain name"),
        ("slash", "<name>a/b</name>", "not a plain name"),
        ("absolute", "<name>/etc</name>", "not a plain name"),
        ("empty", "<name></name>", "not a plain name"),
        ("backslash", "<name>..\\x</name>", "unsafe archive path"),
        ("colon", "<name>c:x</name>", "unsafe archive path"),
        ("NUL", "<name>a&#0;b</name>", "character reference"),
        ("missing", "", "has no name"),
        (
            "base64",
            "<name enctype=\"base64\">YQ==</name>",
            "name is encoded",
        ),
        (
            "two names",
            "<name>a</name><name>b</name>",
            "two different values",
        ),
    ] {
        assert_rejected_saying(case, &simple(name), message);
    }
    let long = "a".repeat(5000);
    let nested = xar(Sum::Sha1, Sum::Sha1, |_| {
        directory(1, &long, &directory(2, &long, ""))
    });
    assert_rejected_saying("long path", &nested, "path length");
}

#[test]
fn links_that_leave_the_destination_are_refused() {
    for (case, target) in [
        ("parent", "../outside"),
        ("absolute", "/etc/passwd"),
        ("deep", "a/../../outside"),
        ("empty", ""),
    ] {
        let bytes = xar(Sum::Sha1, Sum::Sha1, |_| symlink(1, "link", target));
        assert_rejected_saying(case, &bytes, "symlink \"link\"");
    }
    let no_target = xar(Sum::Sha1, Sum::Sha1, |_| {
        "<file id=\"1\"><name>link</name><type>symlink</type></file>".to_owned()
    });
    assert_rejected_saying("no target", &no_target, "has no target");
    let inside_link = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        format!(
            "<file id=\"1\"><name>d</name><type>symlink</type><link>.</link><file id=\"2\"><name>x</name><type>file</type>{data}</file></file>"
        )
    });
    assert_rejected_saying(
        "file under a symlink",
        &inside_link,
        "inside a file or link",
    );
    let inside_file = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        format!(
            "<file id=\"1\"><name>f</name><type>file</type><file id=\"2\"><name>x</name><type>file</type>{data}</file></file>"
        )
    });
    assert_rejected_saying("file under a file", &inside_file, "inside a file or link");
}

#[test]
fn hard_links_that_name_no_file_are_refused() {
    let to = |target: u32| {
        xar(Sum::Sha1, Sum::Sha1, |heap| {
            let data = heap.store_data_element(Encoding::Stored, b"x");
            original(1, "file", &data) + &directory(2, "folder", "") + &hard_link(3, "link", target)
        })
    };
    assert_rejected_saying("missing id", &to(9), "which no file has");
    assert_rejected_saying("folder", &to(2), "which is not a file");
    assert_rejected_saying("itself", &to(3), "which is not a file");
    let no_attribute = xar(Sum::Sha1, Sum::Sha1, |_| {
        "<file id=\"1\"><name>l</name><type>hardlink</type></file>".to_owned()
    });
    assert_rejected_saying("no link attribute", &no_attribute, "no link attribute");
    let same_id = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        file(1, "a", "0644", &data) + &file(1, "b", "0644", &data)
    });
    assert_rejected_saying("same id", &same_id, "same id");
    let replaced = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let first = heap.store_data_element(Encoding::Stored, b"linked");
        let second = heap.store_data_element(Encoding::Stored, b"other");
        original(1, "file", &first) + &file(2, "file", "0644", &second) + &hard_link(3, "link", 1)
    });
    assert_rejected_saying("linked file replaced", &replaced, "hard link names");
}

#[test]
fn bad_checksums_are_refused() {
    let mut bytes = sample(Encoding::Zlib, Sum::Sha1, Sum::Sha1);
    let toc_length = u64::from_be_bytes(bytes[8..16].try_into().unwrap()) as usize;
    bytes[28 + toc_length] ^= 1;
    assert_rejected_saying("table of contents checksum", &bytes, "fails its checksum");

    let flip_extracted = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Zlib, b"alpha");
        let good = hex(&Sum::Sha1.digest(b"alpha"));
        file(
            1,
            "a",
            "0644",
            &data.replace(&good, &hex(&Sum::Sha1.digest(b"beta"))),
        )
    });
    assert_rejected_saying(
        "extracted checksum",
        &flip_extracted,
        "fails its extracted checksum",
    );

    let flip_archived = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let archived = Encoding::Zlib.encode(b"alpha");
        let data = heap.store_data_element(Encoding::Zlib, b"alpha");
        let good = hex(&Sum::Sha1.digest(&archived));
        file(
            1,
            "a",
            "0644",
            &data.replace(&good, &hex(&Sum::Sha1.digest(b"beta"))),
        )
    });
    assert_rejected_saying(
        "archived checksum",
        &flip_archived,
        "fails its archived checksum",
    );

    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let mut archived = Encoding::Zlib.encode(b"alpha");
    archived.extend_from_slice(b"trailing");
    let offset = heap.bytes.len();
    heap.bytes.extend_from_slice(&archived);
    let mut data = heap.data_element_at(Encoding::Zlib, offset, &archived, b"alpha");
    let trailing_sum = hex(&Sum::Sha1.digest(&archived));
    data = data.replace(
        &trailing_sum,
        &hex(&Sum::Sha1.digest(&archived[..archived.len() - 8])),
    );
    let text = toc_with_checksum_element(&heap, &file(1, "a", "0644", &data));
    assert_rejected_saying(
        "trailing bytes outside the checksum",
        &assemble(&text, heap),
        "fails its archived checksum",
    );

    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let mut archived = Encoding::Zlib.encode(b"alpha");
    archived.resize(3 * 1024 * 1024, 0x5A);
    let offset = heap.bytes.len();
    heap.bytes.extend_from_slice(&archived);
    let data = heap.data_element_at(Encoding::Zlib, offset, &archived, b"alpha");
    for prefix in [1024 * 1024, 2 * 1024 * 1024] {
        let sum = hex(&Sum::Sha1.digest(&archived[..prefix]));
        let fooled = data.replace(&hex(&Sum::Sha1.digest(&archived)), &sum);
        let text = toc_with_checksum_element(&heap, &file(1, "a", "0644", &fooled));
        let mut copy = Heap::new(Sum::Sha1, Sum::Sha1);
        copy.bytes = heap.bytes.clone();
        assert_rejected_saying(
            "a checksum of the range read before the stream ended",
            &assemble(&text, copy),
            "fails its archived checksum",
        );
    }
}

#[test]
fn bad_sizes_are_refused() {
    let larger = xar(Sum::Sha1, Sum::None, |heap| {
        let data = heap.store_data_element(Encoding::Zlib, b"alpha");
        file(
            1,
            "a",
            "0644",
            &data.replace("<size>5</size>", "<size>6</size>"),
        )
    });
    assert_rejected_saying("size past the data", &larger, "truncated");
    let smaller = xar(Sum::Sha1, Sum::None, |heap| {
        let data = heap.store_data_element(Encoding::Zlib, b"alpha");
        file(
            1,
            "a",
            "0644",
            &data.replace("<size>5</size>", "<size>4</size>"),
        )
    });
    assert_rejected_saying("size short of the data", &smaller, "decodes past its size");
    let stored = xar(Sum::Sha1, Sum::None, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"alpha");
        file(
            1,
            "a",
            "0644",
            &data.replace("<size>5</size>", "<size>4</size>"),
        )
    });
    assert_rejected_saying("stored length", &stored, "length other than its size");
    let huge = xar(Sum::Sha1, Sum::None, |heap| {
        let data = heap.store_data_element(Encoding::Zlib, b"alpha");
        file(
            1,
            "a",
            "0644",
            &data.replace("<size>5</size>", &format!("<size>{}</size>", u64::MAX)),
        )
    });
    let (_root, _destination, result) = extract_with(
        &huge,
        ReadOptions {
            max_output_bytes: Some(1 << 30),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("max-extracted-bytes"));
}

#[test]
fn data_outside_the_heap_or_overlapping_is_refused() {
    let at = |offset: &str, length: &str| {
        xar(Sum::Sha1, Sum::None, |heap| {
            let data = heap.store_data_element(Encoding::Stored, b"alpha");
            let data = data
                .replace("<offset>20</offset>", &format!("<offset>{offset}</offset>"))
                .replace("<length>5</length>", &format!("<length>{length}</length>"));
            file(1, "a", "0644", &data)
        })
    };
    assert_rejected_saying("past the end", &at("21", "5"), "outside the heap");
    assert_rejected_saying(
        "overflow",
        &at(&u64::MAX.to_string(), "5"),
        "outside the heap",
    );
    assert_rejected_saying(
        "length overflow",
        &at("20", &u64::MAX.to_string()),
        "outside the heap",
    );
    assert_rejected_saying("over the checksum", &at("19", "5"), "overlap");
    let checksum_as_data = xar(Sum::Sha1, Sum::None, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"alpha");
        let data = data
            .replace("<offset>20</offset>", "<offset>0</offset>")
            .replace("<size>5</size>", "<size>20</size>")
            .replace("<length>5</length>", "<length>20</length>");
        file(1, "sum", "0644", &data)
    });
    assert_rejected_saying("the checksum itself", &checksum_as_data, "checksum");
    let overlapping = xar(Sum::Sha1, Sum::None, |heap| {
        let first = heap.store_data_element(Encoding::Stored, b"abcdef");
        let second = first
            .replace("<offset>20</offset>", "<offset>22</offset>")
            .replace("<size>6</size>", "<size>4</size>")
            .replace("<length>6</length>", "<length>4</length>");
        file(1, "a", "0644", &first) + &file(2, "b", "0644", &second)
    });
    assert_rejected_saying("partial overlap", &overlapping, "overlap");
}

#[test]
fn hostile_tables_of_contents_are_refused() {
    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let text = toc_with_checksum_element(&heap, "");
    let compressed = zlib(text.as_bytes());
    let huge = assemble_raw(&compressed, 256 * 1024 * 1024 + 1, &mut heap, 1);
    assert_rejected_saying("huge declared size", &huge, "larger than 256 MiB");
    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let longer = assemble_raw(&compressed, text.len() as u64 + 1, &mut heap, 1);
    assert_rejected_saying("declared longer", &longer, "short of the length");
    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let shorter = assemble_raw(&compressed, text.len() as u64 - 1, &mut heap, 1);
    assert_rejected_saying("declared shorter", &shorter, "past the length");

    let mut past_end = sample(Encoding::Zlib, Sum::Sha1, Sum::Sha1);
    let length = past_end.len() as u64;
    past_end[8..16].copy_from_slice(&length.to_be_bytes());
    assert_rejected_saying(
        "compressed past the end",
        &past_end,
        "past the end of the file",
    );

    let deep = xar(Sum::Sha1, Sum::Sha1, |_| {
        "<x>".repeat(300) + &"</x>".repeat(300)
    });
    assert_rejected_saying("deep XML", &deep, "nests deeper");
    let deep_files = xar(Sum::Sha1, Sum::Sha1, |_| {
        "<file><name>d</name><type>directory</type>".repeat(300) + &"</file>".repeat(300)
    });
    assert_rejected_saying("deep folders", &deep_files, "nests deeper");

    let long_token = xar(Sum::Sha1, Sum::Sha1, |_| {
        format!("<ignored value=\"{}\"/>", "a".repeat(2 * 1024 * 1024))
    });
    assert_rejected_saying("long token", &long_token, "longer than 1 MiB");
    let long_field = simple(&format!("<name>{}</name>", "a&amp;".repeat(5000)));
    assert_rejected_saying("long field", &long_field, "field longer than");

    let laughs = "<?xml version=\"1.0\"?><!DOCTYPE xar [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;\">]><xar><toc><file><name>&b;</name></file></toc></xar>";
    let heap = Heap::new(Sum::None, Sum::None);
    assert_rejected_saying("document type", &assemble(laughs, heap), "document type");
    let undefined = simple("<name>&b;</name>");
    assert_rejected_saying("undefined entity", &undefined, "entity XML does not define");
    let mut heap = Heap::new(Sum::None, Sum::None);
    let broken = "<xar><toc><file><name>a</name></toc></xar>";
    assert_rejected_saying(
        "mismatched end",
        &assemble_raw(&zlib(broken.as_bytes()), broken.len() as u64, &mut heap, 0),
        "not well-formed",
    );
    let heap = Heap::new(Sum::None, Sum::None);
    assert_rejected_saying("unclosed", &assemble("<xar><toc>", heap), "ends before");
    let heap = Heap::new(Sum::None, Sum::None);
    assert_rejected_saying("no toc", &assemble("<xar></xar>", heap), "no <toc>");
    let heap = Heap::new(Sum::None, Sum::None);
    assert_rejected_saying(
        "two tocs",
        &assemble("<xar><toc></toc><toc></toc></xar>", heap),
        "two tables",
    );
    let heap = Heap::new(Sum::None, Sum::None);
    assert_rejected_saying(
        "other root",
        &assemble("<zip><toc></toc></zip>", heap),
        "root",
    );

    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let text = toc_with_checksum_element(&heap, "");
    let mismatch = assemble_raw(&zlib(text.as_bytes()), text.len() as u64, &mut heap, 4);
    assert_rejected_saying(
        "header names another checksum",
        &mismatch,
        "not the one its header names",
    );
    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let text = toc_with_checksum_element(&heap, "").replace(
        "<checksum style=\"sha1\"><size>20</size><offset>0</offset></checksum>",
        "",
    );
    let missing = assemble_raw(&zlib(text.as_bytes()), text.len() as u64, &mut heap, 1);
    assert_rejected_saying("checksum missing", &missing, "has no checksum");
    let mut heap = Heap::new(Sum::Sha1, Sum::Sha1);
    let text = toc_with_checksum_element(&heap, "").replace("<size>20</size>", "<size>19</size>");
    let wrong_size = assemble_raw(&zlib(text.as_bytes()), text.len() as u64, &mut heap, 1);
    assert_rejected_saying("checksum size", &wrong_size, "wrong size");
}

#[test]
fn unsupported_entries_and_encodings_are_refused() {
    for kind in ["fifo", "character special", "block special", "socket"] {
        let bytes = xar(Sum::Sha1, Sum::Sha1, |_| {
            format!("<file id=\"1\"><name>d</name><type>{kind}</type></file>")
        });
        assert_rejected_saying(kind, &bytes, "device, FIFO or socket");
    }
    let unknown = xar(Sum::Sha1, Sum::Sha1, |_| {
        "<file id=\"1\"><name>a</name><type>door</type></file>".to_owned()
    });
    assert_rejected_saying("unknown type", &unknown, "unknown type");
    let directory_data = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        format!("<file id=\"1\"><name>d</name><type>directory</type>{data}</file>")
    });
    assert_rejected_saying(
        "folder with data",
        &directory_data,
        "carries data but is not a file",
    );
    let encoding = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        file(
            1,
            "a",
            "0644",
            &data.replace("application/octet-stream", "application/zstd"),
        )
    });
    assert_rejected_saying("unknown encoding", &encoding, "not one Zipper reads");
    let style = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        file(
            1,
            "a",
            "0644",
            &data.replace("style=\"sha1\"", "style=\"crc32\""),
        )
    });
    assert_rejected_saying("unknown checksum", &style, "not one Zipper knows");
    let two_data = xar(Sum::Sha1, Sum::Sha1, |heap| {
        let data = heap.store_data_element(Encoding::Stored, b"x");
        file(1, "a", "0644", &(data.clone() + &data))
    });
    assert_rejected_saying("two data", &two_data, "twice");
}

#[test]
fn every_truncation_is_refused() {
    let bytes = sample(Encoding::Zlib, Sum::Sha1, Sum::Sha1);
    let step = (bytes.len() / 300).max(1);
    for length in (0..bytes.len())
        .step_by(step)
        .chain(bytes.len() - 64..bytes.len())
    {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.xar");
        fs::write(&archive, &bytes[..length]).unwrap();
        let destination = root.path().join("out");
        assert!(
            read_archive(&archive, Some(&destination), ReadOptions::default()).is_err(),
            "accepted a cut at {length}"
        );
        assert_eq!(
            leftovers(root.path()),
            Vec::<String>::new(),
            "cut at {length}"
        );
    }
}

fn sevenzip() -> Option<PathBuf> {
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

#[test]
fn every_encoding_7zip_reads_extracts_the_tree_7zip_extracts() {
    let Some(binary) = sevenzip() else {
        eprintln!("7-Zip is not installed; skipped");
        return;
    };
    for encoding in [Encoding::Stored, Encoding::Zlib, Encoding::Bzip2] {
        for (toc_sum, file_sum) in [
            (Sum::Sha1, Sum::Sha1),
            (Sum::Sha256, Sum::Sha512),
            (Sum::Sha1, Sum::Md5),
        ] {
            let case = format!("{encoding:?} {toc_sum:?} {file_sum:?}");
            let bytes = sample(encoding, toc_sum, file_sum);
            let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
            result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
            let direct = root.path().join("direct");
            let mut flag = std::ffi::OsString::from("-o");
            flag.push(&direct);
            let run = Command::new(&binary)
                .args(["x", "-y", "-txar"])
                .arg(flag)
                .arg(root.path().join("case.xar"))
                .output()
                .unwrap();
            assert!(
                run.status.success(),
                "{case}: 7-Zip failed: {}",
                String::from_utf8_lossy(&run.stdout)
            );
            assert_eq!(tree(&destination), tree(&direct), "{case}");
        }
    }
    for encoding in [Encoding::Xz, Encoding::LzmaAlone, Encoding::LzmaAsXz] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.xar");
        fs::write(&archive, sample(encoding, Sum::Sha1, Sum::Sha1)).unwrap();
        let run = Command::new(&binary)
            .args(["t", "-txar"])
            .arg(&archive)
            .output()
            .unwrap();
        assert!(
            !run.status.success()
                && String::from_utf8_lossy(&[run.stdout, run.stderr].concat())
                    .contains("Unsupported Method"),
            "{encoding:?}: 7-Zip now reads it, so compare its tree"
        );
    }
}
