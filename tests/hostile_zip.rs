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
    let mut out = Vec::new();
    let mut offsets = Vec::new();
    for entry in entries {
        offsets.push(out.len() as u32);
        if entry.local_offset.is_some() {
            continue;
        }
        let name = entry.local_name.as_ref().unwrap_or(&entry.name);
        let flags = if entry.descriptor { 8u16 } else { 0 };
        let deferred = |value: u32| if entry.descriptor { 0 } else { value };
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
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
            out.extend_from_slice(b"PK\x07\x08");
            out.extend_from_slice(&entry.crc.to_le_bytes());
            out.extend_from_slice(&(entry.payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&entry.size.to_le_bytes());
        }
    }
    let directory = out.len() as u32;
    for (entry, offset) in entries.iter().zip(&offsets) {
        out.extend_from_slice(b"PK\x01\x02");
        out.extend_from_slice(&entry.version_made_by.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&(if entry.descriptor { 8u16 } else { 0 }).to_le_bytes());
        out.extend_from_slice(&entry.method.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&entry.crc.to_le_bytes());
        out.extend_from_slice(&(entry.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&entry.size.to_le_bytes());
        out.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&entry.external.to_le_bytes());
        out.extend_from_slice(&entry.local_offset.unwrap_or(*offset).to_le_bytes());
        out.extend_from_slice(&entry.name);
    }
    let size = out.len() as u32 - directory;
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    fs::write(path, out).unwrap();
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
        "symlink",
        &[Raw {
            version_made_by: 3 << 8 | 20,
            external: 0o120777 << 16,
            ..stored("link", b"../../outside")
        }],
    );
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
