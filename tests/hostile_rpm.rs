//! The RPM reader extracts the files a package installs byte for byte,
//! rejects each hostile lead, header and payload with nothing left behind,
//! and honours the options the cpio reader takes. Where 7-Zip is installed,
//! it is the oracle: 7-Zip writes the payload's cpio, and the cpio it wrote
//! extracts to the tree Zipper writes.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_rpm::{ReadOptions, read_archive};

const FILE: u32 = 0o100644;
const EXECUTABLE: u32 = 0o100755;
const DIRECTORY: u32 = 0o040755;
const SYMLINK: u32 = 0o120777;
const TIME: u64 = 1_700_000_000;
const STRING: u32 = 6;
const INT32: u32 = 4;
const BIN: u32 = 7;
const NAME: u32 = 1000;
const VERSION: u32 = 1001;
const RELEASE: u32 = 1002;
const SIZE: u32 = 1009;
const PAYLOAD_FORMAT: u32 = 1124;
const PAYLOAD_COMPRESSOR: u32 = 1125;
const MAX_INDEX_ENTRIES: u32 = 65_535;
const MAX_DATA: u32 = 256 * 1024 * 1024;
const COMPRESSORS: [&str; 5] = ["gzip", "bzip2", "xz", "lzma", "zstd"];

#[derive(Clone)]
struct Member {
    name: String,
    mode: u32,
    data: Vec<u8>,
    inode: u64,
    links: u64,
}

fn member(name: &str, mode: u32, data: &[u8]) -> Member {
    Member {
        name: name.to_owned(),
        mode,
        data: data.to_vec(),
        inode: 0,
        links: 1,
    }
}

fn file(name: &str, data: &[u8]) -> Member {
    member(name, FILE, data)
}

fn directory(name: &str) -> Member {
    member(name, DIRECTORY, b"")
}

fn symlink(name: &str, target: &str) -> Member {
    member(name, SYMLINK, target.as_bytes())
}

fn linked(name: &str, data: &[u8], inode: u64, links: u64) -> Member {
    Member {
        inode,
        links,
        ..file(name, data)
    }
}

fn pad(bytes: &mut Vec<u8>, alignment: usize) {
    bytes.resize(bytes.len().next_multiple_of(alignment), 0);
}

fn newc_record(magic: &str, member: &Member, index: u64) -> Vec<u8> {
    let inode = if member.inode == 0 {
        index + 1
    } else {
        member.inode
    };
    let fields = [
        inode,
        u64::from(member.mode),
        0,
        0,
        member.links,
        TIME,
        member.data.len() as u64,
        0,
        0,
        0,
        0,
        member.name.len() as u64 + 1,
        0,
    ];
    let mut bytes = magic.as_bytes().to_vec();
    for field in fields {
        bytes.extend(format!("{field:08X}").bytes());
    }
    bytes.extend(member.name.bytes());
    bytes.push(0);
    pad(&mut bytes, 4);
    bytes.extend(&member.data);
    pad(&mut bytes, 4);
    bytes
}

fn cpio_with_magic(magic: &str, members: &[Member]) -> Vec<u8> {
    let mut bytes: Vec<u8> = members
        .iter()
        .enumerate()
        .flat_map(|(index, member)| newc_record(magic, member, index as u64))
        .collect();
    bytes.extend(newc_record(magic, &member("TRAILER!!!", 0, b""), 0));
    pad(&mut bytes, 512);
    bytes
}

fn cpio(members: &[Member]) -> Vec<u8> {
    cpio_with_magic("070701", members)
}

fn compress(compressor: &str, bytes: &[u8]) -> Vec<u8> {
    match compressor {
        "gzip" => {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        "bzip2" => {
            let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        "xz" => {
            let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        "lzma" => {
            let options = liblzma::stream::LzmaOptions::new_preset(6).unwrap();
            let stream = liblzma::stream::Stream::new_lzma_encoder(&options).unwrap();
            let mut encoder = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        }
        "zstd" => zstd::encode_all(bytes, 3).unwrap(),
        other => panic!("no compressor {other}"),
    }
}

#[derive(Clone)]
struct Tag {
    tag: u32,
    kind: u32,
    count: u32,
    data: Vec<u8>,
}

fn text(tag: u32, value: &str) -> Tag {
    Tag {
        tag,
        kind: STRING,
        count: 1,
        data: [value.as_bytes(), b"\0"].concat(),
    }
}

fn int32(tag: u32, value: u32) -> Tag {
    Tag {
        tag,
        kind: INT32,
        count: 1,
        data: value.to_be_bytes().to_vec(),
    }
}

fn alignment(kind: u32) -> usize {
    match kind {
        3 => 2,
        4 => 4,
        5 => 8,
        _ => 1,
    }
}

fn header(tags: &[Tag]) -> Vec<u8> {
    let mut store = Vec::new();
    let mut index = Vec::new();
    for tag in tags {
        pad(&mut store, alignment(tag.kind));
        for word in [tag.tag, tag.kind, store.len() as u32, tag.count] {
            index.extend(word.to_be_bytes());
        }
        store.extend(&tag.data);
    }
    let mut bytes = vec![0x8E, 0xAD, 0xE8, 0x01, 0, 0, 0, 0];
    bytes.extend((tags.len() as u32).to_be_bytes());
    bytes.extend((store.len() as u32).to_be_bytes());
    bytes.extend(index);
    bytes.extend(store);
    bytes
}

fn lead() -> Vec<u8> {
    let mut bytes = vec![0xED, 0xAB, 0xEE, 0xDB, 3, 0, 0, 0, 0, 1];
    let mut name = b"case-1.0-1".to_vec();
    name.resize(66, 0);
    bytes.extend(name);
    bytes.extend([0, 1, 0, 5]);
    bytes.resize(96, 0);
    bytes
}

fn signature() -> Vec<Tag> {
    vec![
        int32(SIZE, 1234),
        Tag {
            tag: 1004,
            kind: BIN,
            count: 16,
            data: vec![7; 16],
        },
    ]
}

fn main_tags(compressor: Option<&str>) -> Vec<Tag> {
    let mut tags = vec![
        text(NAME, "case"),
        text(VERSION, "1.0"),
        text(RELEASE, "1"),
        text(PAYLOAD_FORMAT, "cpio"),
    ];
    if let Some(compressor) = compressor {
        tags.push(text(PAYLOAD_COMPRESSOR, compressor));
    }
    tags
}

fn package_from(signature: &[u8], main: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = lead();
    bytes.extend(signature);
    pad(&mut bytes, 8);
    bytes.extend(main);
    bytes.extend(payload);
    bytes
}

fn package_with(tags: &[Tag], payload: &[u8]) -> Vec<u8> {
    package_from(&header(&signature()), &header(tags), payload)
}

fn rpm(compressor: &str, members: &[Member]) -> Vec<u8> {
    package_with(
        &main_tags(Some(compressor)),
        &compress(compressor, &cpio(members)),
    )
}

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
        .filter(|path| path != "case.rpm")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.rpm");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let result = read_archive(&archive, Some(&destination), ReadOptions::default());
    assert!(result.is_err(), "{case}: accepted a hostile package");
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile package"
    );
}

fn assert_rejected_saying(case: &str, bytes: &[u8], message: &str) {
    assert_rejected_without_leftovers(case, bytes);
    let error = format!(
        "{:#}",
        extract_with(bytes, ReadOptions::default()).2.unwrap_err()
    );
    assert!(error.contains(message), "{case}: {error}");
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
    let archive = root.path().join("case.rpm");
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    let result = read_archive(&archive, Some(&destination), options);
    (root, destination, result)
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

fn sample() -> Vec<Member> {
    let mut members = vec![
        directory("./usr"),
        directory("./usr/share"),
        file("./usr/share/a.txt", b"alpha\n"),
        member("./usr/bin-tool", EXECUTABLE, b"#!/bin/sh\n"),
        file("./usr/empty", b""),
        file(
            "./usr/share/big",
            &(0..70_000u32).map(|n| (n * 7) as u8).collect::<Vec<_>>(),
        ),
        linked("./usr/share/first", b"", 40, 2),
        linked("./usr/share/second", b"shared\n", 40, 2),
    ];
    if cfg!(unix) {
        members.push(symlink("./usr/share/link", "a.txt"));
    }
    members
}

fn sample_tree() -> BTreeMap<String, String> {
    let mut expected = BTreeMap::new();
    for member in sample() {
        let name = member.name.trim_start_matches("./").to_owned();
        let kind = match member.mode & 0o170000 {
            0o040000 => "dir".to_owned(),
            0o120000 => format!("link:{}", String::from_utf8(member.data).unwrap()),
            _ if member.name.ends_with("first") => format!("{:?}", b"shared\n"),
            _ => format!("{:?}", member.data),
        };
        expected.insert(name, kind);
    }
    expected
}

#[test]
fn every_compressor_extracts_byte_for_byte() {
    for compressor in COMPRESSORS {
        let bytes = rpm(compressor, &sample());
        let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{compressor}: {error:#}"));
        assert_eq!(tree(&destination), sample_tree(), "{compressor}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata = |name: &str| fs::metadata(destination.join(name)).unwrap();
            assert_ne!(metadata("usr/bin-tool").permissions().mode() & 0o111, 0);
            assert_eq!(metadata("usr/empty").permissions().mode() & 0o111, 0);
            assert_eq!(
                metadata("usr/share/big").modified().unwrap(),
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(TIME),
            );
            assert_eq!(
                metadata("usr/share/first").ino(),
                metadata("usr/share/second").ino(),
                "{compressor}: the hard link is a copy"
            );
        }
        let listed =
            read_archive(&root.path().join("case.rpm"), None, ReadOptions::default()).unwrap();
        assert_eq!(
            listed.iter().filter(|entry| !entry.directory).count(),
            sample_tree().values().filter(|kind| *kind != "dir").count()
        );
    }
}

#[test]
fn a_package_without_a_compressor_tag_holds_gzip_as_rpm_assumes() {
    let bytes = package_with(&main_tags(None), &compress("gzip", &cpio(&sample())));
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    assert_eq!(tree(&destination), sample_tree());
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

fn sevenzip_extract(binary: &Path, archive: &Path, into: &Path) {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "7-Zip failed on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn every_compressor_extracts_the_tree_7zip_extracts_from_its_cpio() {
    let Some(binary) = sevenzip() else {
        eprintln!("7-Zip is not installed; skipped");
        return;
    };
    for compressor in COMPRESSORS {
        let (root, destination, result) =
            extract_with(&rpm(compressor, &sample()), ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{compressor}: {error:#}"));
        let payload = root.path().join("payload");
        sevenzip_extract(&binary, &root.path().join("case.rpm"), &payload);
        let written: Vec<PathBuf> = fs::read_dir(&payload)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let cpio = match &written[..] {
            [cpio]
                if cpio
                    .extension()
                    .is_some_and(|extension| extension == "cpio") =>
            {
                cpio
            }
            _ if compressor == "zstd" => {
                eprintln!("this 7-Zip did not decode the zstd payload; it wrote {written:?}");
                continue;
            }
            _ => panic!("{compressor}: 7-Zip wrote {written:?}, not one cpio"),
        };
        let direct = root.path().join("direct");
        sevenzip_extract(&binary, cpio, &direct);
        assert_eq!(tree(&destination), tree(&direct), "{compressor}");
    }
}

#[test]
fn the_default_engine_routes_packages_to_this_reader() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("package.rpm");
    fs::write(&archive, rpm("xz", &sample())).unwrap();
    for engine in [zipper::Engine::Auto, zipper::Engine::Native] {
        assert_eq!(
            zipper::choose_reader(engine, &archive).unwrap(),
            zipper::Reader::Rpm
        );
    }
    let destination = root.path().join("out");
    zipper::extract_archive(&archive, &destination, zipper::ExtractOptions::default()).unwrap();
    assert_eq!(tree(&destination), sample_tree());
}

#[test]
fn a_cut_anywhere_in_the_lead_headers_or_payload_is_rejected() {
    for compressor in ["gzip", "zstd"] {
        let bytes = rpm(compressor, &sample()[..3]);
        for length in 0..bytes.len() {
            assert_rejected_without_leftovers(
                &format!("{compressor} cut at {length}"),
                &bytes[..length],
            );
        }
    }
}

fn with_word(mut bytes: Vec<u8>, at: usize, word: u32) -> Vec<u8> {
    bytes[at..at + 4].copy_from_slice(&word.to_be_bytes());
    bytes
}

const SIGNATURE_AT: usize = 96;

fn main_at() -> usize {
    (SIGNATURE_AT + header(&signature()).len()).next_multiple_of(8)
}

#[test]
fn leads_that_are_not_version_3_or_4_with_a_header_signature_are_rejected() {
    let good = rpm("gzip", &sample());
    let mut magic = good.clone();
    magic[3] = 0;
    assert_rejected_saying("lead magic", &magic, "not an RPM package");
    let mut version = good.clone();
    version[4] = 2;
    assert_rejected_saying("lead version", &version, "version");
    let mut signature = good.clone();
    signature[79] = 1;
    assert_rejected_saying("signature type", &signature, "signature");
}

#[test]
fn headers_without_their_magic_are_rejected() {
    let good = rpm("gzip", &sample());
    for (case, at) in [("signature", SIGNATURE_AT), ("main", main_at())] {
        let mut bytes = good.clone();
        bytes[at + 1] ^= 0xFF;
        assert_rejected_saying(case, &bytes, "header magic");
    }
}

#[test]
fn index_counts_and_data_sizes_over_the_limits_are_refused_before_reading() {
    let good = rpm("gzip", &sample());
    for (case, at) in [("signature", SIGNATURE_AT), ("main", main_at())] {
        assert_rejected_saying(
            &format!("{case} entries"),
            &with_word(good.clone(), at + 8, MAX_INDEX_ENTRIES + 1),
            "65,535 index entries",
        );
        assert_rejected_saying(
            &format!("{case} data"),
            &with_word(good.clone(), at + 12, MAX_DATA + 1),
            "256 MiB",
        );
        assert_rejected_saying(
            &format!("{case} entries past the file"),
            &with_word(good.clone(), at + 8, MAX_INDEX_ENTRIES),
            "past the end of the file",
        );
    }
}

#[test]
fn index_entries_outside_the_data_store_or_of_unknown_type_are_rejected() {
    let base = main_tags(Some("gzip"));
    let payload = compress("gzip", &cpio(&sample()));
    let entry_at = |index: usize| main_at() + 16 + 16 * index;
    let good = package_with(&base, &payload);
    let store = u32::from_be_bytes(good[main_at() + 12..main_at() + 16].try_into().unwrap());
    let mut cases = vec![
        (
            "string offset at the end",
            with_word(good.clone(), entry_at(0) + 8, store),
            "outside the header data",
        ),
        (
            "string offset past the end",
            with_word(good.clone(), entry_at(0) + 8, u32::MAX),
            "outside the header data",
        ),
        (
            "unknown type",
            with_word(good.clone(), entry_at(1) + 4, 10),
            "unknown type",
        ),
        (
            "string count",
            with_word(good.clone(), entry_at(2) + 12, 2),
            "count",
        ),
    ];
    let mut tags = base.clone();
    tags.push(int32(1006, 1));
    let with_number = package_with(&tags, &payload);
    let number = entry_at(tags.len() - 1);
    let number_offset =
        u32::from_be_bytes(with_number[number + 8..number + 12].try_into().unwrap());
    cases.push((
        "int32 count past the end",
        with_word(with_number.clone(), number + 12, 2),
        "outside the header data",
    ));
    cases.push((
        "int32 count overflows",
        with_word(with_number.clone(), number + 12, u32::MAX),
        "outside the header data",
    ));
    cases.push((
        "misaligned int32",
        with_word(with_number, number + 8, number_offset - 1),
        "misaligned",
    ));
    let mut signature_entry = package_with(&base, &payload);
    let at = SIGNATURE_AT + 16 + 16 + 12;
    signature_entry[at..at + 4].copy_from_slice(&17u32.to_be_bytes());
    cases.push((
        "signature binary past the end",
        signature_entry,
        "outside the header data",
    ));
    for (case, bytes, message) in cases {
        assert_rejected_saying(case, &bytes, message);
    }
}

#[test]
fn payload_tags_that_are_unreadable_twice_given_or_not_text_are_rejected() {
    let payload = compress("gzip", &cpio(&sample()));
    let mut twice = main_tags(Some("gzip"));
    twice.push(text(PAYLOAD_COMPRESSOR, "gzip"));
    assert_rejected_saying("two compressors", &package_with(&twice, &payload), "twice");
    let mut number = main_tags(None);
    number.push(int32(PAYLOAD_COMPRESSOR, 1));
    assert_rejected_saying(
        "numeric compressor",
        &package_with(&number, &payload),
        "not a string",
    );
    let mut long = main_tags(None);
    long.push(text(PAYLOAD_COMPRESSOR, &"z".repeat(100)));
    assert_rejected_saying(
        "long compressor",
        &package_with(&long, &payload),
        "64 bytes",
    );
    let mut unterminated = main_tags(None);
    unterminated.push(Tag {
        tag: PAYLOAD_COMPRESSOR,
        kind: STRING,
        count: 1,
        data: b"gzip".to_vec(),
    });
    assert_rejected_saying(
        "unterminated compressor",
        &package_with(&unterminated, &payload),
        "NUL",
    );
}

#[test]
fn an_unknown_compressor_is_refused_with_its_name() {
    for name in ["lzip", "brotli", "none"] {
        let bytes = package_with(&main_tags(Some(name)), &compress("gzip", &cpio(&sample())));
        assert_rejected_saying(name, &bytes, &format!("{name:?}"));
    }
}

#[test]
fn a_payload_that_its_compressor_tag_does_not_describe_is_rejected() {
    for (named, actual) in [
        ("xz", "gzip"),
        ("gzip", "zstd"),
        ("zstd", "bzip2"),
        ("bzip2", "xz"),
    ] {
        let bytes = package_with(&main_tags(Some(named)), &compress(actual, &cpio(&sample())));
        assert_rejected_saying(&format!("{named} holding {actual}"), &bytes, "signature");
    }
    let plain = package_with(&main_tags(Some("lzma")), &cpio(&sample()));
    assert_rejected_without_leftovers("lzma holding plain cpio", &plain);
    let mut lzma = compress("lzma", &cpio(&sample()));
    lzma[0] = 225;
    assert_rejected_saying(
        "lzma properties past the limit",
        &package_with(&main_tags(Some("lzma")), &lzma),
        "signature",
    );
}

#[test]
fn a_payload_that_is_not_newc_cpio_is_rejected() {
    let mut tags = main_tags(Some("gzip"));
    tags[3] = text(PAYLOAD_FORMAT, "drpm");
    let delta = package_with(&tags, &compress("gzip", &cpio(&sample())));
    assert_rejected_saying("delta rpm", &delta, "\"drpm\"");
    let mut tar = vec![0u8; 1024];
    tar[..5].copy_from_slice(b"hello");
    tar[257..262].copy_from_slice(b"ustar");
    assert_rejected_saying(
        "tar payload",
        &package_with(&main_tags(Some("gzip")), &compress("gzip", &tar)),
        "newc",
    );
    let odc = b"070707000000000000000000000000000000000000000000000000000000000000000000000000";
    assert_rejected_saying(
        "odc payload",
        &package_with(&main_tags(Some("gzip")), &compress("gzip", odc)),
        "newc",
    );
    assert_rejected_saying(
        "empty payload",
        &package_with(&main_tags(Some("gzip")), &compress("gzip", b"")),
        "cpio",
    );
}

#[test]
fn the_stripped_cpio_of_large_file_and_version_6_packages_is_refused_clearly() {
    let stripped = cpio_with_magic("07070X", &sample());
    let bytes = package_with(&main_tags(Some("zstd")), &compress("zstd", &stripped));
    assert_rejected_saying("synthetic", &bytes, "07070X");
    let real = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/rpm/rpm6-format6.rpm"
    ))
    .unwrap();
    assert_rejected_saying("rpm 6.1.0 default format", &real, "07070X");
}

fn zstd_with_checksum(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    encoder.include_checksum(true).unwrap();
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn a_damaged_payload_fails_its_compressors_check() {
    let mut payloads: Vec<(&str, &str, Vec<u8>)> = ["gzip", "bzip2", "xz", "lzma"]
        .into_iter()
        .map(|compressor| {
            (
                compressor,
                compressor,
                compress(compressor, &cpio(&sample())),
            )
        })
        .collect();
    payloads.push((
        "zstd",
        "zstd with a checksum",
        zstd_with_checksum(&cpio(&sample())),
    ));
    for (compressor, case, payload) in payloads {
        let good = package_with(&main_tags(Some(compressor)), &payload);
        let payload_start = good.len() - payload.len();
        let mut damaged = good.clone();
        damaged[payload_start + payload.len() / 2] ^= 0x55;
        assert_rejected_without_leftovers(&format!("{case} damaged"), &damaged);
        let mut tail = good.clone();
        let last = tail.len() - 1;
        tail[last] ^= 0xff;
        assert_rejected_without_leftovers(&format!("{case} trailer"), &tail);
    }
}

#[test]
fn unsafe_names_never_escape() {
    let mut cases = vec![
        ("traversal", vec![file("./../evil", b"x")]),
        ("nested traversal", vec![file("./usr/../../evil", b"x")]),
        ("absolute", vec![file("/etc/evil", b"x")]),
        ("backslash", vec![file("./a\\..\\evil", b"x")]),
    ];
    if cfg!(unix) {
        cases.push((
            "escaping symlink",
            vec![directory("./usr"), symlink("./usr/link", "../../outside")],
        ));
        cases.push(("absolute symlink", vec![symlink("./link", "/etc/passwd")]));
        cases.push((
            "write through a symlink",
            vec![symlink("./link", "."), file("./link/evil", b"x")],
        ));
    }
    for (case, members) in cases {
        assert_rejected_without_leftovers(case, &rpm("gzip", &members));
    }
}

#[test]
fn duplicate_names_follow_the_cpio_rules() {
    let repeated = rpm("gzip", &[file("./a", b"first\n"), file("./a", b"second\n")]);
    let (_root, destination, result) = extract_with(&repeated, ReadOptions::default());
    result.unwrap();
    assert_eq!(fs::read(destination.join("a")).unwrap(), b"second\n");
    assert_rejected_saying(
        "file then folder",
        &rpm("gzip", &[file("./a", b"x"), directory("./a")]),
        "different kinds",
    );
    assert_rejected_saying(
        "linked file replaced",
        &rpm(
            "gzip",
            &[
                linked("./a", b"", 9, 2),
                linked("./b", b"data\n", 9, 2),
                file("./b", b"other\n"),
            ],
        ),
        "hard link",
    );
}

#[test]
fn selection_strip_and_limits_apply_to_the_payload_paths() {
    let bytes = rpm("xz", &sample());
    let paths = ["usr/share/a.txt".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            paths: &paths,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        fs::read(destination.join("usr/share/a.txt")).unwrap(),
        b"alpha\n"
    );
    assert!(!destination.join("usr/share/big").exists());
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            strip: zipper::Strip::Components(3),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"alpha\n");
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            strip: zipper::Strip::Prefix("usr/share"),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"alpha\n");
    let (root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            max_output_bytes: Some(1000),
            ..ReadOptions::default()
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("--max-extracted-bytes"));
    assert!(!destination.exists());
    drop(root);
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/rpm")
            .join(name),
    )
    .unwrap()
}

#[test]
fn data_after_an_lzma_payload_is_rejected() {
    for name in ["after-lzma-garbage.rpm", "after-lzma-hidden-cpio.rpm"] {
        assert_rejected_saying(name, &fixture(name), "after its LZMA stream");
    }
    let payload = compress("lzma", &cpio(&sample()));
    let hidden = compress("lzma", &cpio(&[file("./hidden", b"x")]));
    for (case, tail) in [("garbage", b"junk".to_vec()), ("second stream", hidden)] {
        let bytes = package_with(&main_tags(Some("lzma")), &[payload.clone(), tail].concat());
        assert_rejected_saying(case, &bytes, "after its LZMA stream");
    }
}

#[test]
fn a_cut_or_trailing_garbage_in_the_payload_says_the_payload_is_damaged() {
    for compressor in COMPRESSORS {
        let payload = compress(compressor, &cpio(&sample()));
        let good = package_with(&main_tags(Some(compressor)), &payload);
        let start = good.len() - payload.len();
        for cut in [start + 8, start + payload.len() / 2, good.len() - 1] {
            let error = format!(
                "{:#}",
                extract_with(&good[..cut], ReadOptions::default())
                    .2
                    .unwrap_err()
            );
            assert!(
                error.contains("RPM payload is damaged"),
                "{compressor} cut at {cut}: {error}"
            );
        }
        let short = package_with(&main_tags(Some(compressor)), &compress(compressor, b"0707"));
        assert_rejected_saying(
            &format!("{compressor} short"),
            &short,
            "RPM payload is damaged",
        );
        if compressor != "lzma" {
            let garbage = [good, b"garbage!".to_vec()].concat();
            assert_rejected_saying(
                &format!("{compressor} garbage"),
                &garbage,
                "RPM payload is damaged",
            );
        }
    }
}

#[test]
fn a_selected_path_with_the_leading_dot_slash_says_how_to_select_it() {
    let paths = ["./usr/share/a.txt".to_owned()];
    let (_root, _destination, result) = extract_with(
        &rpm("gzip", &sample()),
        ReadOptions {
            paths: &paths,
            ..ReadOptions::default()
        },
    );
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("\"usr/share/a.txt\""), "{error}");
}

#[test]
fn xz_and_lzma_payloads_past_the_decoder_memory_limit_are_refused() {
    let mut lzma = compress("lzma", &cpio(&sample()));
    lzma[1..5].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_rejected_saying(
        "lzma with a 4 GiB dictionary",
        &package_with(&main_tags(Some("lzma")), &lzma),
        "4294967295-byte dictionary",
    );
    let mut xz = compress("xz", &cpio(&sample()));
    let block = 12;
    let size = (usize::from(xz[block]) + 1) * 4;
    let filter = (block..block + size)
        .find(|&at| xz[at..at + 2] == [0x21, 0x01])
        .unwrap();
    xz[filter + 2] = 40;
    let check = crc32fast::hash(&xz[block..block + size - 4]);
    xz[block + size - 4..block + size].copy_from_slice(&check.to_le_bytes());
    let bytes = package_with(&main_tags(Some("xz")), &xz);
    assert_rejected_saying(
        "xz with a 4 GiB dictionary",
        &bytes,
        "the largest dictionary Zipper accepts, 256 MiB",
    );
    let error = format!(
        "{:#}",
        extract_with(&bytes, ReadOptions::default()).2.unwrap_err()
    );
    assert!(!error.contains("damaged"), "{error}");
}

#[test]
fn the_dot_slash_hint_never_suggests_a_path_that_would_also_fail() {
    for (path, unwanted) in [
        ("./../etc", "\"../etc\""),
        ("././usr/share/a.txt", "\"./usr/share/a.txt\""),
        ("./", "\"\""),
        ("./usr//a", "\"usr//a\""),
    ] {
        let paths = [path.to_owned()];
        let (_root, _destination, result) = extract_with(
            &rpm("gzip", &sample()),
            ReadOptions {
                paths: &paths,
                ..ReadOptions::default()
            },
        );
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            !error.contains("leading ./") && !error.contains(unwanted),
            "{path}: {error}"
        );
    }
}
