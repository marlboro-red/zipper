//! Single compressed files read by `native_stream`: every truncation,
//! trailing data, a damaged checksum and an output cap are refused with
//! nothing published.
use std::{fs, path::Path};
use zipper::native_stream::{ReadOptions, output_name, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/parity/archives");
const STREAMS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/stream");
const SINGLE_FILES: [&str; 6] = [
    "hello.txt.gz",
    "hello.txt.bz2",
    "hello.txt.xz",
    "hello.txt.zst",
    "hello.txt.lzma",
    "hello.txt.lz",
];

fn single_file(name: &str) -> Vec<u8> {
    [CORPUS, STREAMS]
        .iter()
        .find_map(|folder| fs::read(Path::new(folder).join(name)).ok())
        .unwrap_or_else(|| panic!("{name} is in neither corpus"))
}

fn extract(
    name: &str,
    bytes: &[u8],
    options: ReadOptions,
) -> (tempfile::TempDir, anyhow::Result<()>) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join(name);
    fs::write(&archive, bytes).unwrap();
    let result = read_archive(&archive, Some(&root.path().join("out")), options).map(drop);
    (root, result)
}

fn assert_refused(case: &str, name: &str, bytes: &[u8], options: ReadOptions) {
    let (root, result) = extract(name, bytes, options);
    assert!(result.is_err(), "{case} was accepted");
    let left: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|left| left != name)
        .collect();
    assert!(left.is_empty(), "{case} left {left:?}");
}

#[test]
fn every_single_file_extracts_to_its_bytes() {
    for name in SINGLE_FILES {
        let (root, result) = extract(name, &single_file(name), ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(
            fs::read(root.path().join("out/hello.txt")).unwrap(),
            b"hello from the parity corpus\n",
            "{name}"
        );
    }
}

#[test]
fn every_truncation_is_refused() {
    for name in SINGLE_FILES {
        let bytes = single_file(name);
        for length in 0..bytes.len() {
            assert_refused(
                &format!("{name} cut to {length}"),
                name,
                &bytes[..length],
                ReadOptions::default(),
            );
        }
    }
}

#[test]
fn trailing_data_and_damaged_checksums_are_refused() {
    for name in SINGLE_FILES {
        let mut bytes = single_file(name);
        let mut trailing = bytes.clone();
        trailing.extend_from_slice(b"trailing junk");
        assert_refused(
            &format!("{name} with trailing data"),
            name,
            &trailing,
            ReadOptions::default(),
        );
        if name.ends_with(".lz") {
            let mut damaged = bytes.clone();
            let crc = damaged.len() - 20;
            damaged[crc] ^= 0xff;
            assert_refused(
                "lzip with a damaged CRC",
                name,
                &damaged,
                ReadOptions::default(),
            );
        }
        if name.ends_with(".gz") {
            let crc = bytes.len() - 8;
            bytes[crc] ^= 0xff;
            assert_refused(
                "gzip with a damaged CRC",
                name,
                &bytes,
                ReadOptions::default(),
            );
        }
    }
}

#[test]
fn the_output_cap_is_enforced_while_decoding() {
    for name in SINGLE_FILES {
        let bytes = single_file(name);
        assert_refused(
            &format!("{name} over its cap"),
            name,
            &bytes,
            ReadOptions {
                max_output_bytes: Some(10),
                ..Default::default()
            },
        );
    }
}

#[test]
fn the_output_name_drops_only_a_compression_extension() {
    for (archive, name) in [
        ("notes.txt.gz", "notes.txt"),
        ("NOTES.TXT.GZ", "NOTES.TXT"),
        ("a.bz2", "a"),
        ("x.tgz", "x.tar"),
        ("y.TBZ2", "y.tar"),
        ("notes.txt.lz", "notes.txt"),
        ("z.tlz", "z.tar"),
        ("data.bin", "data"),
        ("noext", "noext~"),
        (".gz", ".gz~"),
        ("..gz", "_"),
        ("x.gz:y", "x"),
        ("sp ace .gz", "sp ace"),
    ] {
        assert_eq!(output_name(Path::new(archive)).unwrap(), name, "{archive}");
    }
}

#[test]
fn bzip2_streams_must_each_bring_output() {
    let empty = {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
        std::io::Write::write_all(&mut encoder, b"").unwrap();
        encoder.finish().unwrap()
    };
    let (_root, single) = extract("empty.bz2", &empty, ReadOptions::default());
    single.unwrap();
    let many: Vec<u8> = empty
        .iter()
        .copied()
        .cycle()
        .take(empty.len() * 1000)
        .collect();
    let started = std::time::Instant::now();
    assert_refused(
        "a thousand empty bzip2 streams",
        "many.bz2",
        &many,
        ReadOptions::default(),
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let large = |byte: u8| {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &[byte; 10_000]).unwrap();
        encoder.finish().unwrap()
    };
    let mut two = large(b'a');
    two.extend(large(b'b'));
    let (root, joined) = extract("two.bz2", &two, ReadOptions::default());
    joined.unwrap();
    assert_eq!(fs::read(root.path().join("out/two")).unwrap().len(), 20_000);
}

#[test]
fn a_7z_named_lzma_still_reads_as_7z() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("seven.lzma");
    fs::copy(Path::new(CORPUS).join("7z.7z"), &archive).unwrap();
    zipper::extract_archive(&archive, &root.path().join("out"), Default::default()).unwrap();
    assert!(root.path().join("out/hello.txt").exists());
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_unix_file_name_becomes_an_underscore() {
    assert_eq!(output_name(Path::new("a\\b.gz")).unwrap(), "a_b");
}

#[cfg(windows)]
#[test]
fn windows_output_names_avoid_devices_and_trailing_dots() {
    for (archive, name) in [
        ("CON.gz", "_CON"),
        ("a..gz", "a"),
        ("nul.txt.gz", "_nul.txt"),
    ] {
        assert_eq!(output_name(Path::new(archive)).unwrap(), name, "{archive}");
    }
}

#[test]
fn a_second_lzip_member_cut_at_any_length_is_refused() {
    let member = single_file("hello.txt.lz");
    let two = [member.clone(), member.clone()].concat();
    let (root, result) = extract("hello.txt.lz", &two, ReadOptions::default());
    result.unwrap();
    assert_eq!(
        fs::read(root.path().join("out/hello.txt")).unwrap(),
        b"hello from the parity corpus\nhello from the parity corpus\n"
    );
    for length in member.len() + 1..two.len() {
        assert_refused(
            &format!("two lzip members cut to {length}"),
            "hello.txt.lz",
            &two[..length],
            ReadOptions::default(),
        );
    }
}

#[test]
fn stripping_a_single_file_leaves_nothing_and_is_refused() {
    for strip in [
        zipper::Strip::Components(1),
        zipper::Strip::Prefix("hello.txt"),
    ] {
        assert_refused(
            &format!("{strip:?}"),
            "hello.txt.gz",
            &single_file("hello.txt.gz"),
            ReadOptions {
                strip,
                ..ReadOptions::default()
            },
        );
    }
    let (root, result) = extract(
        "hello.txt.gz",
        &single_file("hello.txt.gz"),
        ReadOptions {
            strip: zipper::Strip::Components(0),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert!(root.path().join("out/hello.txt").exists());
}
