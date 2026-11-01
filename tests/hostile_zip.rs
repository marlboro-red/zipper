use std::{fs, path::Path};
use zipper::{
    Entry,
    native_zip::{read_archive, read_archive_fast},
};

type Reader = fn(&Path, Option<&Path>) -> anyhow::Result<Vec<Entry>>;

const STORED: u16 = 0;
const DEFLATED: u16 = 8;

#[derive(Clone)]
struct Raw {
    name: Vec<u8>,
    local_name: Option<Vec<u8>>,
    method: u16,
    payload: Vec<u8>,
    crc: u32,
    size: u32,
    local_size: Option<u32>,
    version_made_by: u16,
    external: u32,
    local_offset: Option<u32>,
    descriptor: bool,
    local_compressed: Option<u32>,
    local_extra: Vec<u8>,
    /// Bytes written before the local header, outside any record.
    padding: Vec<u8>,
    /// Exact bytes written after the data when `descriptor` is set.
    descriptor_bytes: Option<Vec<u8>>,
    local_flags: u16,
    central_flags: u16,
    central_extra: Vec<u8>,
    /// DOS time (low 16 bits) and date (high 16 bits).
    dos: u32,
}

fn stored(name: &str, data: &[u8]) -> Raw {
    Raw {
        name: name.as_bytes().to_vec(),
        local_name: None,
        method: STORED,
        payload: data.to_vec(),
        crc: crc32fast::hash(data),
        size: data.len() as u32,
        local_size: None,
        version_made_by: 20,
        external: 0,
        local_offset: None,
        descriptor: false,
        local_compressed: None,
        local_extra: Vec::new(),
        padding: Vec::new(),
        descriptor_bytes: None,
        local_flags: 0,
        central_flags: 0,
        central_extra: Vec::new(),
        dos: 0,
    }
}

fn deflated(name: &str, data: &[u8]) -> Raw {
    let mut compressor = libdeflater::Compressor::new(libdeflater::CompressionLvl::default());
    let mut payload = vec![0; compressor.deflate_compress_bound(data.len())];
    let length = compressor.deflate_compress(data, &mut payload).unwrap();
    payload.truncate(length);
    Raw {
        method: DEFLATED,
        payload,
        ..stored(name, data)
    }
}

/// Writes a ZIP with every header field under the test's control.
fn write_zip(path: &Path, entries: &[Raw]) {
    write_zip_with_tail(path, entries, &[]);
}

/// A data descriptor with or without its signature, in 32-bit or ZIP64 form.
fn descriptor(entry: &Raw, signature: bool, wide: bool) -> Vec<u8> {
    let mut bytes = if signature {
        b"PK\x07\x08".to_vec()
    } else {
        Vec::new()
    };
    bytes.extend_from_slice(&entry.crc.to_le_bytes());
    for value in [entry.payload.len() as u64, entry.size as u64] {
        if wide {
            bytes.extend_from_slice(&value.to_le_bytes());
        } else {
            bytes.extend_from_slice(&(value as u32).to_le_bytes());
        }
    }
    bytes
}

/// Like `write_zip`, with `tail` written between the last record and the
/// central directory.
fn write_zip_with_tail(path: &Path, entries: &[Raw], tail: &[u8]) {
    fs::write(path, zip_bytes(entries, tail)).unwrap();
}

fn zip_bytes(entries: &[Raw], tail: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for entry in entries {
        out.extend_from_slice(&entry.padding);
        offsets.push(out.len() as u32);
        if entry.local_offset.is_some() {
            continue;
        }
        let name = entry.local_name.as_ref().unwrap_or(&entry.name);
        let flags = if entry.descriptor { 8u16 } else { 0 } | entry.local_flags;
        let deferred = |value: u32| if entry.descriptor { 0 } else { value };
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&entry.dos.to_le_bytes());
        out.extend_from_slice(&deferred(entry.crc).to_le_bytes());
        let local_compressed = entry
            .local_compressed
            .unwrap_or_else(|| deferred(entry.payload.len() as u32));
        out.extend_from_slice(&local_compressed.to_le_bytes());
        let local_size = match entry.local_size {
            Some(size) => size,
            None => deferred(entry.size),
        };
        out.extend_from_slice(&local_size.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entry.local_extra.len() as u16).to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(&entry.local_extra);
        out.extend_from_slice(&entry.payload);
        if entry.descriptor {
            let bytes = entry
                .descriptor_bytes
                .clone()
                .unwrap_or_else(|| descriptor(entry, true, false));
            out.extend_from_slice(&bytes);
        }
    }
    out.extend_from_slice(tail);
    let directory = out.len() as u32;
    for (entry, offset) in entries.iter().zip(&offsets) {
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&entry.version_made_by.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(
            &((if entry.descriptor { 8u16 } else { 0 }) | entry.central_flags).to_le_bytes(),
        );
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&entry.dos.to_le_bytes());
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&(entry.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entry.central_extra.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&entry.external.to_le_bytes());
        out.extend_from_slice(&entry.local_offset.unwrap_or(*offset).to_le_bytes());
        out.extend_from_slice(&entry.name);
        out.extend_from_slice(&entry.central_extra);
    }
    let size = out.len() as u32 - directory;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Lists every path under `root` except the archive itself.
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
        .filter(|path| path != "case.zip")
        .collect();
    found.sort();
    found
}

/// Extracts with both native readers and requires a rejection that publishes
/// nothing anywhere under the temporary root, including above the destination.
fn assert_rejected(case: &str, entries: &[Raw]) {
    for (reader, read) in [
        ("native", read_archive as Reader),
        ("native-fast", read_archive_fast as Reader),
    ] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, entries);
        fs::create_dir_all(root.path().join("outer/inner")).unwrap();
        let destination = root.path().join("outer/inner/out");
        let result = read(&archive, Some(&destination));
        assert!(
            result.is_err(),
            "{case}: {reader} accepted a hostile archive"
        );
        assert_eq!(
            leftovers(root.path()),
            ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
            "{case}: {reader} left output behind"
        );
        assert!(
            read(&archive, None).is_err(),
            "{case}: {reader} verification accepted a hostile archive"
        );
    }
}

/// A local ZIP64 extra field holding both sizes, original first.
fn zip64_extra(size: u64, compressed: u64) -> Vec<u8> {
    let mut field = vec![1, 0, 16, 0];
    field.extend_from_slice(&size.to_le_bytes());
    field.extend_from_slice(&compressed.to_le_bytes());
    field
}

fn megabyte_of_zeros() -> Vec<u8> {
    vec![0; 1 << 20]
}

#[test]
fn declared_sizes_bound_decoded_data() {
    let data = megabyte_of_zeros();
    let hidden = Raw {
        size: 0,
        crc: 0,
        ..deflated("bomb.bin", &data)
    };
    assert_rejected("zero declared size hides data", &[hidden]);
    assert_rejected(
        "declared size smaller than data",
        &[Raw {
            size: 10,
            ..deflated("short.bin", &data)
        }],
    );
    assert_rejected(
        "declared size larger than data",
        &[Raw {
            size: 2 << 20,
            ..deflated("long.bin", &data)
        }],
    );
    assert_rejected(
        "local and central sizes disagree",
        &[Raw {
            local_size: Some(10),
            ..deflated("split.bin", &data)
        }],
    );
}

#[test]
fn checksums_and_stored_lengths_are_enforced() {
    assert_rejected(
        "wrong CRC",
        &[Raw {
            crc: 1,
            ..deflated("crc.bin", b"payload")
        }],
    );
    assert_rejected(
        "stored entry shorter than declared",
        &[Raw {
            size: 100,
            ..stored("stored.bin", b"payload")
        }],
    );
}

#[test]
fn unsafe_names_never_escape() {
    for name in [
        "../escape.txt",
        "a/../../escape.txt",
        "/absolute.txt",
        "C:/drive.txt",
        "a\\..\\..\\escape.txt",
        "a/./b.txt",
        "a//b.txt",
        "",
        "nul\0byte.txt",
    ] {
        assert_rejected(name, &[stored(name, b"x")]);
    }
}

#[test]
fn ambiguous_entry_sets_are_rejected() {
    assert_rejected(
        "duplicate names",
        &[stored("a.txt", b"one"), stored("a.txt", b"two")],
    );
    assert_rejected(
        "file used as a directory",
        &[stored("a", b"file"), stored("a/b.txt", b"child")],
    );
}

#[test]
fn local_headers_must_match_the_central_directory() {
    assert_rejected(
        "local and central names disagree",
        &[Raw {
            local_name: Some(b"other.txt".to_vec()),
            ..stored("name.txt", b"x")
        }],
    );
    let data = megabyte_of_zeros();
    let entry = deflated("zip64.bin", &data);
    assert_rejected(
        "local ZIP64 compressed size disagrees",
        &[Raw {
            local_compressed: Some(u32::MAX),
            local_extra: zip64_extra(data.len() as u64, 7),
            ..entry
        }],
    );
    assert_rejected(
        "data descriptor with a wrong local size",
        &[Raw {
            descriptor: true,
            local_size: Some(10),
            ..deflated("deferred.bin", &megabyte_of_zeros())
        }],
    );
    assert_rejected(
        "two entries share one local record",
        &[
            stored("first.txt", b"shared"),
            Raw {
                local_offset: Some(0),
                ..stored("second.txt", b"shared")
            },
        ],
    );
}

#[test]
fn special_file_types_are_rejected() {
    assert_rejected(
        "device",
        &[Raw {
            version_made_by: 3 << 8 | 20,
            external: 0o020644 << 16,
            ..stored("device", b"")
        }],
    );
}

#[test]
fn names_differing_only_in_case_never_lose_data() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.zip");
    write_zip(
        &archive,
        &[
            stored("Readme.txt", b"upper"),
            stored("readme.txt", b"lower"),
        ],
    );
    let destination = root.path().join("out");
    match read_archive(&archive, Some(&destination)) {
        Ok(_) => {
            assert_eq!(fs::read(destination.join("Readme.txt")).unwrap(), b"upper");
            assert_eq!(fs::read(destination.join("readme.txt")).unwrap(), b"lower");
        }
        Err(_) => assert!(!destination.exists(), "failed extraction left output"),
    }
}

#[test]
fn data_descriptors_and_well_formed_records_still_extract() {
    let text = b"descriptor payload ".repeat(100);
    let plain = deflated("zip64-local.txt", &text);
    let entries = [
        Raw {
            descriptor: true,
            ..deflated("dir/deferred.txt", &text)
        },
        Raw {
            descriptor: true,
            ..stored("stored.txt", &text)
        },
        deflated("plain.txt", &text),
        stored("empty.txt", b""),
        Raw {
            descriptor: true,
            local_size: Some(text.len() as u32),
            ..deflated("infozip-pipe.txt", &text)
        },
        Raw {
            local_compressed: Some(u32::MAX),
            local_extra: zip64_extra(text.len() as u64, plain.payload.len() as u64),
            ..plain.clone()
        },
        Raw {
            descriptor: true,
            local_compressed: Some(u32::MAX),
            local_size: Some(u32::MAX),
            ..deflated("unknown-sizes.txt", &text)
        },
    ];
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, &entries);
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        for entry in &entries {
            let name = String::from_utf8(entry.name.clone()).unwrap();
            let expected: &[u8] = if name == "empty.txt" { b"" } else { &text };
            assert_eq!(
                fs::read(destination.join(&name)).unwrap(),
                expected,
                "{name}"
            );
        }
    }
}

#[test]
fn cp437_names_are_matched_by_their_stored_bytes() {
    let cp437 = Raw {
        name: b"\x81ber.txt".to_vec(),
        ..stored("", b"cp437")
    };
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.zip");
    write_zip(&archive, std::slice::from_ref(&cp437));
    let destination = root.path().join("out");
    read_archive(&archive, Some(&destination)).unwrap();
    assert_eq!(
        fs::read(destination.join("\u{fc}ber.txt")).unwrap(),
        b"cp437"
    );
    assert_rejected(
        "CP437 local name differs",
        &[Raw {
            local_name: Some(b"\x82ber.txt".to_vec()),
            ..cp437
        }],
    );
}

#[test]
fn records_must_tile_the_archive_without_gaps() {
    // A complete local record that no central entry points to.
    let hidden_record = zip_bytes(&[stored("evil.txt", b"hidden")], &[])[..30 + 8 + 6].to_vec();
    assert_rejected(
        "record hidden before the first entry",
        &[Raw {
            padding: hidden_record.clone(),
            ..stored("a.txt", b"a")
        }],
    );
    assert_rejected(
        "record hidden between entries",
        &[
            stored("a.txt", b"a"),
            Raw {
                padding: hidden_record.clone(),
                ..stored("b.txt", b"b")
            },
        ],
    );
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip_with_tail(&archive, &[stored("a.txt", b"a")], &hidden_record);
        assert!(
            read(&archive, Some(&root.path().join("out"))).is_err(),
            "record hidden before the central directory was accepted"
        );
        assert!(!root.path().join("out").exists());
    }
}

#[test]
fn data_descriptors_must_match_the_central_directory() {
    let entry = Raw {
        descriptor: true,
        ..deflated("deferred.bin", &megabyte_of_zeros())
    };
    let mut wrong_crc = entry.clone();
    wrong_crc.crc ^= 1;
    assert_rejected(
        "descriptor CRC disagrees",
        &[Raw {
            descriptor_bytes: Some(descriptor(&wrong_crc, true, false)),
            ..entry.clone()
        }],
    );
    let mut wrong_size = entry.clone();
    wrong_size.size += 1;
    assert_rejected(
        "descriptor size disagrees",
        &[Raw {
            descriptor_bytes: Some(descriptor(&wrong_size, true, false)),
            ..entry.clone()
        }],
    );
    assert_rejected(
        "descriptor with trailing bytes",
        &[Raw {
            descriptor_bytes: Some([descriptor(&entry, true, false), vec![0; 4]].concat()),
            ..entry.clone()
        }],
    );
    assert_rejected(
        "flagged entry without a descriptor",
        &[Raw {
            descriptor_bytes: Some(Vec::new()),
            ..entry
        }],
    );
}

#[test]
fn local_fields_that_other_readers_trust_must_agree() {
    assert_rejected(
        "local encryption flag",
        &[Raw {
            local_flags: 1,
            ..stored("a.txt", b"a")
        }],
    );
    let data = megabyte_of_zeros();
    let entry = deflated("zip64.bin", &data);
    let packed = entry.payload.len() as u64;
    let size = entry.size as u64;
    assert_rejected(
        "two local ZIP64 fields",
        &[Raw {
            local_size: Some(u32::MAX),
            local_compressed: Some(u32::MAX),
            local_extra: [zip64_extra(size, packed), zip64_extra(7, 7)].concat(),
            ..entry.clone()
        }],
    );
    assert_rejected(
        "two central ZIP64 fields",
        &[Raw {
            central_extra: [zip64_extra(size, packed), zip64_extra(7, 7)].concat(),
            ..entry.clone()
        }],
    );
    assert_rejected(
        "local ZIP64 field lies while 32-bit sizes are real",
        &[Raw {
            local_extra: zip64_extra(7, packed),
            ..entry
        }],
    );
}

#[test]
fn every_descriptor_form_extracts() {
    let text = b"descriptor forms ".repeat(64);
    let base = deflated("x", &text);
    let entries: Vec<Raw> = [(true, false), (false, false), (true, true), (false, true)]
        .into_iter()
        .enumerate()
        .map(|(index, (signature, wide))| Raw {
            name: format!("form-{index}.txt").into_bytes(),
            descriptor: true,
            descriptor_bytes: Some(descriptor(&base, signature, wide)),
            ..base.clone()
        })
        .collect();
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, &entries);
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        for index in 0..4 {
            assert_eq!(
                fs::read(destination.join(format!("form-{index}.txt"))).unwrap(),
                text
            );
        }
    }
}

#[test]
fn entries_hold_nothing_beyond_their_own_data() {
    // A complete local record that no central entry points to.
    let hidden = zip_bytes(&[stored("evil.txt", b"hidden")], &[])[..30 + 8 + 6].to_vec();
    let text = b"visible ".repeat(64);
    let mut trailing = deflated("a.txt", &text);
    trailing.payload.extend_from_slice(&hidden);
    assert_rejected(
        "record after the Deflate stream ends",
        std::slice::from_ref(&trailing),
    );
    assert_rejected(
        "record after the Deflate stream ends, with a descriptor",
        &[Raw {
            descriptor: true,
            ..trailing
        }],
    );
    let prefix = b"innocent\n";
    let mut smuggled = prefix.to_vec();
    smuggled.extend_from_slice(b"PK\x07\x08");
    smuggled.extend_from_slice(&crc32fast::hash(prefix).to_le_bytes());
    smuggled.extend_from_slice(&(prefix.len() as u32).to_le_bytes());
    smuggled.extend_from_slice(&(prefix.len() as u32).to_le_bytes());
    smuggled.extend_from_slice(&hidden);
    assert_rejected(
        "fake descriptor inside stored data",
        &[Raw {
            descriptor: true,
            ..stored("a.txt", &smuggled)
        }],
    );
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        let honest = Raw {
            descriptor: true,
            ..stored("stored.bin", &text)
        };
        write_zip(
            &archive,
            &[honest, stored("signature.bin", b"PK\x07\x08 is fine here")],
        );
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        assert_eq!(fs::read(destination.join("stored.bin")).unwrap(), text);
    }
}

/// Data that a streaming reader would end at a fake descriptor: `prefix`,
/// then a descriptor that matches it, then `rest`.
fn fake_descriptor(prefix: &[u8], wide: bool, rest: &[u8]) -> Vec<u8> {
    let mut data = prefix.to_vec();
    data.extend_from_slice(b"PK\x07\x08");
    data.extend_from_slice(&crc32fast::hash(prefix).to_le_bytes());
    for _ in 0..2 {
        if wide {
            data.extend_from_slice(&(prefix.len() as u64).to_le_bytes());
        } else {
            data.extend_from_slice(&(prefix.len() as u32).to_le_bytes());
        }
    }
    data.extend_from_slice(rest);
    data
}

#[test]
fn stored_descriptor_signatures_are_judged_by_their_fields() {
    let hidden = zip_bytes(&[stored("evil.txt", b"hidden")], &[])[..30 + 8 + 6].to_vec();
    let boundary = vec![b'x'; 256 * 1024 - 2];
    for (case, data) in [
        (
            "ZIP64 fake descriptor",
            fake_descriptor(b"innocent\n", true, &hidden),
        ),
        (
            "fake descriptor split across reads",
            fake_descriptor(&boundary, false, &hidden),
        ),
    ] {
        assert_rejected(
            case,
            &[Raw {
                descriptor: true,
                ..stored("a.txt", &data)
            }],
        );
    }
    // A nested archive written by a streaming tool contains descriptors that
    // describe its own entries, not the outer prefix.
    let mut nested = b"PK\x03\x04 inner header ".to_vec();
    nested.extend_from_slice(&fake_descriptor(b"inner data", false, b" more")[..]);
    nested.splice(0..0, b"outer prefix ".iter().copied());
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        let entry = Raw {
            descriptor: true,
            ..stored("nested.zip", &nested)
        };
        write_zip(&archive, &[entry]);
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        assert_eq!(fs::read(destination.join("nested.zip")).unwrap(), nested);
    }
}

/// An Info-ZIP Unicode path field (0x7075) naming `name` for stored name `stored`.
fn unicode_path(stored: &[u8], name: &str) -> Vec<u8> {
    let mut field = vec![0x75, 0x70];
    field.extend_from_slice(&((1 + 4 + name.len()) as u16).to_le_bytes());
    field.push(1);
    field.extend_from_slice(&crc32fast::hash(stored).to_le_bytes());
    field.extend_from_slice(name.as_bytes());
    field
}

#[test]
fn unicode_path_fields_cannot_rename_entries() {
    let renaming = unicode_path(b"a.txt", "renamed.txt");
    assert_rejected(
        "central Unicode path renames the entry",
        &[Raw {
            central_extra: renaming.clone(),
            ..stored("a.txt", b"x")
        }],
    );
    assert_rejected(
        "local Unicode path renames the entry",
        &[Raw {
            local_extra: renaming,
            ..stored("a.txt", b"x")
        }],
    );
    assert_rejected(
        "directory renamed by a Unicode path",
        &[Raw {
            central_extra: unicode_path(b"d/", "evil/"),
            ..stored("d/", b"")
        }],
    );
    let same_field = unicode_path(b"a.txt", "a.txt");
    assert_rejected(
        "two Unicode path fields",
        &[Raw {
            central_extra: [same_field.clone(), unicode_path(b"a.txt", "evil.txt")].concat(),
            ..stored("a.txt", b"x")
        }],
    );
    assert_rejected(
        "short Unicode path field",
        &[Raw {
            central_extra: vec![0x75, 0x70, 4, 0, 1, 0, 0, 0],
            ..stored("a.txt", b"x")
        }],
    );
    let same = same_field;
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        let entry = Raw {
            central_extra: same.clone(),
            local_extra: same.clone(),
            ..stored("a.txt", b"x")
        };
        write_zip(&archive, &[entry]);
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"x");
    }
}

/// APFS ignores case and Unicode normalization when it compares names.
#[cfg(target_os = "macos")]
#[test]
fn names_that_collide_on_the_filesystem_fail_or_merge_in_archive_order() {
    for (case, entries) in [
        (
            "files that differ by case",
            [stored("A.txt", b"1"), stored("a.txt", b"2")],
        ),
        (
            "files that differ by normalization",
            [stored("\u{e9}.txt", b"1"), stored("e\u{301}.txt", b"2")],
        ),
    ] {
        for read in [read_archive as Reader, read_archive_fast as Reader] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("case.zip");
            write_zip(&archive, &entries);
            let error = read(&archive, Some(&root.path().join("out")))
                .unwrap_err()
                .to_string();
            assert!(error.contains("collides"), "{case}: {error}");
            assert_eq!(leftovers(root.path()), Vec::<String>::new());
        }
    }
    // A merged directory takes the first entry's mode and time, as in unzip.
    let directory = |name: &str, mode: u32, time: u32| Raw {
        version_made_by: 3 << 8 | 20,
        external: mode << 16,
        central_extra: extended_time(time),
        ..stored(name, b"")
    };
    let entries = [
        directory("d/", 0o40700, 631_152_000),
        directory("D/", 0o40750, 1_591_401_600),
        stored("d/x", b"1"),
    ];
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        for _ in 0..8 {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("case.zip");
            write_zip(&archive, &entries);
            let destination = root.path().join("out");
            read(&archive, Some(&destination)).unwrap();
            let metadata = fs::metadata(destination.join("d")).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
            assert_eq!(metadata.mtime(), 631_152_000);
        }
    }
    // Directories merge, as in unzip and Python, under the first spelling.
    for (first, second) in [("D", "d"), ("d", "D")] {
        for read in [read_archive as Reader, read_archive_fast as Reader] {
            for _ in 0..8 {
                let root = tempfile::tempdir().unwrap();
                let archive = root.path().join("case.zip");
                write_zip(
                    &archive,
                    &[
                        stored(&format!("{first}/x"), b"1"),
                        stored(&format!("{second}/y"), b"2"),
                    ],
                );
                let destination = root.path().join("out");
                read(&archive, Some(&destination)).unwrap();
                let names: Vec<_> = fs::read_dir(&destination)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect();
                assert_eq!(names, [first]);
            }
        }
    }
}

/// `café.txt` with `é` stored as CP437 byte 0x82, without the UTF-8 flag.
const CP437_NAME: &[u8] = b"caf\x82.txt";

#[test]
fn cp437_names_decode_and_may_carry_a_matching_unicode_path() {
    let field = unicode_path(CP437_NAME, "caf\u{e9}.txt");
    for entry in [
        Raw {
            name: CP437_NAME.to_vec(),
            ..stored("x", b"x")
        },
        Raw {
            name: CP437_NAME.to_vec(),
            central_extra: field.clone(),
            local_extra: field.clone(),
            ..stored("x", b"x")
        },
    ] {
        for read in [read_archive as Reader, read_archive_fast as Reader] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("case.zip");
            write_zip(&archive, std::slice::from_ref(&entry));
            let destination = root.path().join("out");
            read(&archive, Some(&destination)).unwrap();
            assert_eq!(fs::read(destination.join("caf\u{e9}.txt")).unwrap(), b"x");
        }
    }
    // A UTF-8 stored name decodes as UTF-8, so its CP437 reading is a rename.
    let utf8 = "caf\u{e9}.txt".as_bytes();
    assert_rejected(
        "Unicode path equal to the CP437 reading of a UTF-8 name",
        &[Raw {
            central_extra: unicode_path(utf8, "caf\u{251c}\u{ae}.txt"),
            ..stored("caf\u{e9}.txt", b"x")
        }],
    );
    // Python rejects a Unicode path field whose content is not UTF-8.
    assert_rejected(
        "Unicode path field that is not UTF-8",
        &[Raw {
            name: CP437_NAME.to_vec(),
            central_extra: {
                let mut field = unicode_path(CP437_NAME, "");
                field[2] += CP437_NAME.len() as u8;
                field.extend_from_slice(CP437_NAME);
                field
            },
            ..stored("x", b"x")
        }],
    );
    // Python and 7-Zip do not fall back to CP437 for a name flagged as UTF-8.
    assert_rejected(
        "UTF-8 flag on a name that is not UTF-8",
        &[Raw {
            name: CP437_NAME.to_vec(),
            local_flags: 0x800,
            central_flags: 0x800,
            ..stored("x", b"x")
        }],
    );
    // A reader that checks the CRC ignores this field and keeps the stored name.
    let mut stale = field;
    stale[5] ^= 1;
    assert_rejected(
        "local Unicode path with a stale CRC",
        &[Raw {
            name: CP437_NAME.to_vec(),
            local_extra: stale,
            ..stored("x", b"x")
        }],
    );
}

#[test]
fn output_that_cannot_fit_is_refused_before_writing() {
    // About 16 TiB declared by 4,096 one-byte entries: more than any test
    // machine has free, and refused before a byte is decoded.
    let entries: Vec<Raw> = (0..4096)
        .map(|index| Raw {
            size: u32::MAX - 1,
            ..stored(&format!("f{index}"), b"x")
        })
        .collect();
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, &entries);
        let destination = root.path().join("out");
        let error = read(&archive, Some(&destination)).unwrap_err().to_string();
        assert!(error.contains("free"), "{error}");
        assert_eq!(leftovers(root.path()), Vec::<String>::new());
    }
    // A ZIP64 size of 2^64 - 1 must not wrap the rounded estimate to zero.
    let mut endless = deflated("endless.bin", &megabyte_of_zeros());
    endless.size = u32::MAX;
    endless.central_extra = [vec![1, 0, 8, 0], u64::MAX.to_le_bytes().to_vec()].concat();
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, std::slice::from_ref(&endless));
        let error = read(&archive, Some(&root.path().join("out")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("free"), "{error}");
    }
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.zip");
    write_zip(&archive, &[deflated("big.bin", &megabyte_of_zeros())]);
    let destination = root.path().join("out");
    let options = zipper::native_zip::ReadOptions {
        max_output_bytes: Some(1 << 19),
        ..Default::default()
    };
    let error =
        zipper::native_zip::read_archive_with_options(&archive, Some(&destination), options)
            .unwrap_err()
            .to_string();
    assert!(error.contains("--max-extracted-bytes"), "{error}");
    assert!(!destination.exists());
    let options = zipper::native_zip::ReadOptions {
        max_output_bytes: Some(1 << 20),
        ..Default::default()
    };
    zipper::native_zip::read_archive_with_options(&archive, Some(&destination), options).unwrap();
    assert_eq!(
        fs::read(destination.join("big.bin")).unwrap(),
        megabyte_of_zeros()
    );
}

#[cfg(unix)]
#[test]
fn restored_modes_never_include_special_bits() {
    use std::os::unix::fs::PermissionsExt;
    let unix = |name: &str, mode: u32, data: &[u8]| Raw {
        version_made_by: 3 << 8 | 20,
        external: mode << 16,
        ..stored(name, data)
    };
    let entries = [
        unix("setuid", 0o104755, b"x"),
        unix("sticky/", 0o041777, b""),
        unix("sticky/inside", 0o100644, b"y"),
    ];
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, &entries);
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        let mode = |path: &str| {
            fs::metadata(destination.join(path))
                .unwrap()
                .permissions()
                .mode()
        };
        assert_eq!(mode("setuid") & 0o7000, 0, "setuid bit restored");
        assert_eq!(mode("sticky") & 0o7000, 0, "sticky bit restored");
        assert_eq!(
            mode("setuid") & 0o111,
            0o111 & mode("sticky"),
            "execute bits lost"
        );
    }
}

#[test]
fn an_entry_may_use_any_safe_root_name() {
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, &[stored(".zipper-mode-probe", b"mine")]);
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        assert_eq!(
            fs::read(destination.join(".zipper-mode-probe")).unwrap(),
            b"mine"
        );
    }
}

/// A DOS date and time packed as the ZIP header stores them.
fn dos(year: u32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> u32 {
    ((year - 1980) << 25 | month << 21 | day << 16) | (hour << 11 | minute << 5 | (second / 2))
}

fn extended_time(seconds: u32) -> Vec<u8> {
    [vec![0x55, 0x54, 5, 0, 1], seconds.to_le_bytes().to_vec()].concat()
}

fn unix_time(seconds: u32) -> Vec<u8> {
    [
        vec![0x55, 0x58, 8, 0],
        seconds.to_le_bytes().to_vec(),
        seconds.to_le_bytes().to_vec(),
    ]
    .concat()
}

fn extracted_time(entry: Raw) -> i64 {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.zip");
    write_zip(&archive, &[entry]);
    let destination = root.path().join("out");
    read_archive(&archive, Some(&destination)).unwrap();
    let modified = fs::metadata(destination.join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(after) => after.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    }
}

#[test]
fn exact_times_follow_the_dos_era_and_field_priority() {
    let at = |central_extra: Vec<u8>, dos: u32| Raw {
        central_extra,
        dos,
        ..stored("a.txt", b"x")
    };
    // A 1969 time stored as 32 bits reads as 2106 when taken as unsigned.
    let before_1970 = -54_000_i32 as u32;
    assert_eq!(
        extracted_time(at(extended_time(before_1970), dos(1980, 1, 1, 0, 0, 0))),
        -54_000
    );
    // A real 2040 time has its top bit set too.
    assert_eq!(
        extracted_time(at(extended_time(2_208_988_800), dos(2040, 1, 1, 0, 0, 0))),
        2_208_988_800
    );
    let both = [extended_time(1_000_000_000), unix_time(1_100_000_000)];
    let dos_2001 = dos(2001, 9, 9, 0, 0, 0);
    assert_eq!(extracted_time(at(both.concat(), dos_2001)), 1_000_000_000);
    let reversed = [both[1].clone(), both[0].clone()].concat();
    assert_eq!(extracted_time(at(reversed, dos_2001)), 1_000_000_000);
    assert_eq!(
        extracted_time(at(unix_time(1_100_000_000), dos_2001)),
        1_100_000_000
    );
}

/// A Unix symlink entry whose data is `target`.
fn symlink(name: &str, target: &str) -> Raw {
    Raw {
        version_made_by: 3 << 8 | 20,
        external: 0o120777 << 16,
        ..stored(name, target.as_bytes())
    }
}

#[test]
fn symlinks_must_stay_inside_the_destination() {
    for (case, entries) in [
        ("escaping link", vec![symlink("a/link", "../../outside")]),
        ("absolute link", vec![symlink("link", "/etc/passwd")]),
        ("empty target", vec![symlink("link", "")]),
        (
            "link through another link",
            vec![symlink("x/up", ".."), symlink("y", "x/up/../outside")],
        ),
        // APFS matches `l` to `L` and NFD to NFC, so a name check alone misses these.
        (
            "link through a link spelled in another case",
            vec![
                stored("a/b/", b""),
                symlink("a/b/L", "../.."),
                symlink("m", "a/b/l/../outside"),
            ],
        ),
        (
            "link through a link in another normalization",
            vec![
                stored("a/b/", b""),
                symlink("a/b/\u{e9}", "../.."),
                symlink("m", "a/b/e\u{301}/../outside"),
            ],
        ),
        (
            "dot-dot after a name",
            vec![symlink("m", "a/../../outside")],
        ),
        (
            "file written through a link",
            vec![
                symlink("dir", "real"),
                stored("real/", b""),
                stored("dir/f", b"x"),
            ],
        ),
    ] {
        assert_rejected(case, &entries);
    }
}

#[cfg(unix)]
#[test]
fn symlinks_inside_the_destination_are_created_after_files() {
    let entries = [
        stored("text/readme.txt", b"hello"),
        symlink("links/file", "../text/readme.txt"),
        symlink("links/dir", "../text"),
        symlink("links/self", "."),
        symlink("links/chain", "file"),
        symlink("links/through", "dir/readme.txt"),
    ];
    for read in [read_archive as Reader, read_archive_fast as Reader] {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.zip");
        write_zip(&archive, &entries);
        let destination = root.path().join("out");
        read(&archive, Some(&destination)).unwrap();
        let link = |name: &str| fs::read_link(destination.join(name)).unwrap();
        assert_eq!(link("links/file"), Path::new("../text/readme.txt"));
        assert_eq!(link("links/dir"), Path::new("../text"));
        assert_eq!(fs::read(destination.join("links/chain")).unwrap(), b"hello");
        assert_eq!(
            fs::read(destination.join("links/through")).unwrap(),
            b"hello"
        );
        assert_eq!(
            fs::read(destination.join("links/dir/readme.txt")).unwrap(),
            b"hello"
        );
    }
}

#[cfg(not(unix))]
#[test]
fn symlinks_are_left_to_the_backend_on_this_platform() {
    assert_rejected("any symlink", &[symlink("link", "target")]);
}
