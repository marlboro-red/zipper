//! Single compressed files read by `native_stream`: every truncation,
//! trailing data, a damaged checksum and an output cap are refused with
//! nothing published.
use std::{fs, path::Path};
use zipper::native_stream::{ReadOptions, output_name, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/parity/archives");
const STREAMS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/stream");
const SINGLE_FILES: [&str; 7] = [
    "hello.txt.gz",
    "hello.txt.bz2",
    "hello.txt.xz",
    "hello.txt.zst",
    "hello.txt.lzma",
    "hello.txt.lz",
    "hello.txt.lz4",
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
        if name.ends_with(".lz4") {
            let mut damaged = bytes.clone();
            let checksum = damaged.len() - 1;
            damaged[checksum] ^= 0xff;
            assert_refused(
                "LZ4 with a damaged content checksum",
                name,
                &damaged,
                ReadOptions::default(),
            );
        }
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
        ("notes.txt.lz4", "notes.txt"),
        ("w.tlz4", "w.tar"),
        ("notes.txt.Z", "notes.txt"),
        ("v.taz", "v.tar"),
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

#[test]
fn a_compress_file_extracts_and_a_code_it_never_defined_is_refused() {
    let name = "hello.txt.Z";
    let bytes = single_file(name);
    let (root, result) = extract(name, &bytes, ReadOptions::default());
    result.unwrap_or_else(|error| panic!("{name}: {error:#}"));
    assert_eq!(
        fs::read(root.path().join("out/hello.txt")).unwrap(),
        b"hello from the parity corpus\n"
    );
    let mut undefined = bytes[..3].to_vec();
    undefined.extend_from_slice(&[0x41, 0xFE, 0x03]);
    assert_refused(
        "a code past the next free entry",
        name,
        &undefined,
        ReadOptions::default(),
    );
    assert_refused(
        "a header with reserved bits",
        name,
        &[0x1F, 0x9D, 0xF0],
        ReadOptions::default(),
    );
    assert_refused(
        "a header and nothing else",
        name,
        &[0x1F, 0x9D],
        ReadOptions::default(),
    );
}

#[test]
fn a_nine_bit_compress_file_widens_to_ten_bit_codes_as_7zip_reads_it() {
    let (root, result) = extract(
        "nine-bits.bin.Z",
        &fs::read(Path::new(STREAMS).join("nine-bits.bin.Z")).unwrap(),
        ReadOptions::default(),
    );
    result.unwrap();
    assert!(
        fs::read(root.path().join("out/nine-bits.bin")).unwrap()
            == fs::read(Path::new(STREAMS).join("nine-bits.bin")).unwrap(),
        "the 9-bit file decoded to other bytes"
    );
}

#[test]
fn an_lzma_dictionary_past_the_decoder_memory_limit_is_refused_by_its_size() {
    let encode = |dictionary: u32| {
        let mut options = liblzma::stream::LzmaOptions::new_preset(0).unwrap();
        options.dict_size(1 << 20);
        let stream = liblzma::stream::Stream::new_lzma_encoder(&options).unwrap();
        let mut encoder = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
        std::io::Write::write_all(&mut encoder, b"hello\n").unwrap();
        let mut encoded = encoder.finish().unwrap();
        encoded[1..5].copy_from_slice(&dictionary.to_le_bytes());
        encoded
    };
    let (_root, result) = extract("ok.tar.lzma", &encode(64 << 20), ReadOptions::default());
    result.unwrap_or_else(|error| panic!("a 64 MiB dictionary: {error:#}"));
    let bomb = encode(u32::MAX);
    assert_refused(
        "4 GiB dictionary",
        "bomb.tar.lzma",
        &bomb,
        ReadOptions::default(),
    );
    let error = format!(
        "{:#}",
        extract("bomb.tar.lzma", &bomb, ReadOptions::default())
            .1
            .unwrap_err()
    );
    assert!(
        error.contains("4294967295-byte dictionary")
            && error.contains("the largest dictionary Zipper accepts, 256 MiB")
            && error.contains("--zip-engine backend"),
        "{error}"
    );
    let (_root, result) = extract(
        "largest.tar.lzma",
        &encode(256 << 20),
        ReadOptions::default(),
    );
    result.unwrap_or_else(|error| panic!("a 256 MiB dictionary, the largest: {error:#}"));
    let error = format!(
        "{:#}",
        extract(
            "past.tar.lzma",
            &encode((256 << 20) + 1),
            ReadOptions::default()
        )
        .1
        .unwrap_err()
    );
    assert!(
        error.contains(
            "268435457-byte dictionary, more than the largest dictionary Zipper accepts, 256 MiB"
        ),
        "{error}"
    );
}

fn gzip_member(data: &[u8], level: u32) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn several_decode_windows_of_text() -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut data = Vec::with_capacity(3_000_000);
    while data.len() < 3_000_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let word = (state % 4096) as usize;
        data.extend_from_slice(format!("entry {word} of {} ", state % 7).as_bytes());
        if word < 40 {
            data.extend((0..word * 50).map(|index| (state >> (index % 57)) as u8));
        }
    }
    data
}

#[test]
fn a_gzip_file_of_several_decode_windows_extracts_whole_or_leaves_nothing() {
    let data = several_decode_windows_of_text();
    let (first, second) = data.split_at(2_200_000);
    for level in [0, 1, 6] {
        let members = [gzip_member(first, level), gzip_member(second, level)].concat();
        let (root, result) = extract("big.bin.gz", &members, ReadOptions::default());
        result.unwrap_or_else(|error| panic!("level {level}: {error:#}"));
        assert!(
            fs::read(root.path().join("out/big.bin")).unwrap() == data,
            "level {level} extracted other bytes"
        );
        let exact_cap = ReadOptions {
            max_output_bytes: Some(data.len() as u64),
            ..Default::default()
        };
        extract("big.bin.gz", &members, exact_cap).1.unwrap();
        let one_short = ReadOptions {
            max_output_bytes: Some(data.len() as u64 - 1),
            ..Default::default()
        };
        assert_refused("a cap one byte short", "big.bin.gz", &members, one_short);
        for cut in [members.len() / 3, members.len() - 9, members.len() - 1] {
            assert_refused(
                &format!("level {level} cut to {cut}"),
                "big.bin.gz",
                &members[..cut],
                ReadOptions::default(),
            );
        }
        for flipped in [members.len() / 2, members.len() - 8, members.len() - 1] {
            let mut damaged = members.clone();
            damaged[flipped] ^= 0x10;
            assert_refused(
                &format!("level {level} with byte {flipped} flipped"),
                "big.bin.gz",
                &damaged,
                ReadOptions::default(),
            );
        }
    }
}

#[test]
fn a_gzip_file_cancelled_between_decode_windows_leaves_nothing_and_reports_growing_counts() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let data = several_decode_windows_of_text();
    let members = gzip_member(&data, 6);
    let largest = AtomicU64::new(0);
    let reports = AtomicU64::new(0);
    let watch = |progress: zipper::Progress| {
        let before = largest.swap(progress.bytes_done, Ordering::SeqCst);
        assert!(before <= progress.bytes_done, "the byte count shrank");
        reports.fetch_add(1, Ordering::SeqCst);
    };
    let followed = ReadOptions {
        control: zipper::Control {
            progress: Some(&watch),
            cancel: None,
        },
        ..Default::default()
    };
    extract("big.bin.gz", &members, followed).1.unwrap();
    assert_eq!(largest.load(Ordering::SeqCst), data.len() as u64);
    assert!(reports.load(Ordering::SeqCst) >= 4, "one report a window");

    let cancel = AtomicBool::new(false);
    let stop_after_a_window = |progress: zipper::Progress| {
        if progress.bytes_done > 0 {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let stopped = ReadOptions {
        control: zipper::Control {
            progress: Some(&stop_after_a_window),
            cancel: Some(&cancel),
        },
        ..Default::default()
    };
    assert_refused("a cancelled extraction", "big.bin.gz", &members, stopped);
}
