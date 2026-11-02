use std::{fs, io::Write, path::Path};
use zipper::native_tar::{ReadOptions, read_archive};

const BLOCK: usize = 512;

fn checksum(header: &mut [u8; BLOCK]) {
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

fn ustar(name: &[u8], kind: u8, size: u64, link: &[u8]) -> [u8; BLOCK] {
    let mut header = [0u8; BLOCK];
    header[..name.len()].copy_from_slice(name);
    header[100..108].copy_from_slice(b"0000644\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    header[136..148].copy_from_slice(b"13777777777\0");
    header[156] = kind;
    header[157..157 + link.len()].copy_from_slice(link);
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    checksum(&mut header);
    header
}

fn edited(mut header: [u8; BLOCK], edit: impl FnOnce(&mut [u8; BLOCK])) -> [u8; BLOCK] {
    edit(&mut header);
    checksum(&mut header);
    header
}

fn record(header: [u8; BLOCK], data: &[u8]) -> Vec<u8> {
    let mut bytes = header.to_vec();
    bytes.extend_from_slice(data);
    bytes.resize(bytes.len().div_ceil(BLOCK) * BLOCK, 0);
    bytes
}

fn file(name: &str, data: &[u8]) -> Vec<u8> {
    record(ustar(name.as_bytes(), b'0', data.len() as u64, b""), data)
}

fn directory(name: &str) -> Vec<u8> {
    record(ustar(name.as_bytes(), b'5', 0, b""), b"")
}

fn link(name: &str, kind: u8, target: &str) -> Vec<u8> {
    record(ustar(name.as_bytes(), kind, 0, target.as_bytes()), b"")
}

fn pax_data(records: &[(&str, &[u8])]) -> Vec<u8> {
    let mut data = Vec::new();
    for (key, value) in records {
        let body = 3 + key.len() + value.len();
        let mut length = body + body.to_string().len();
        if length.to_string().len() != body.to_string().len() {
            length += 1;
        }
        data.extend_from_slice(format!("{length} {key}=").as_bytes());
        data.extend_from_slice(value);
        data.push(b'\n');
    }
    data
}

fn pax(kind: u8, records: &[(&str, &[u8])]) -> Vec<u8> {
    let data = pax_data(records);
    record(ustar(b"PaxHeader", kind, data.len() as u64, b""), &data)
}

fn gnu_long(kind: u8, value: &[u8]) -> Vec<u8> {
    let mut data = value.to_vec();
    data.push(0);
    let header = edited(
        ustar(b"././@LongLink", kind, data.len() as u64, b""),
        |header| {
            header[257..265].copy_from_slice(b"ustar  \0");
        },
    );
    record(header, &data)
}

fn tar(members: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = members.concat();
    bytes.extend_from_slice(&[0; 2 * BLOCK]);
    bytes
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
        .filter(|path| path != "case.tar")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.tar");
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let result = read_archive(&archive, Some(&destination), ReadOptions::default());
    assert!(result.is_err(), "{case}: accepted a hostile archive");
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

fn extract(bytes: &[u8]) -> (tempfile::TempDir, anyhow::Result<Vec<zipper::Entry>>) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.tar");
    fs::write(&archive, bytes).unwrap();
    let result = read_archive(
        &archive,
        Some(&root.path().join("out")),
        ReadOptions::default(),
    );
    (root, result)
}

#[test]
fn unsafe_names_never_escape() {
    for (case, members) in [
        ("parent", vec![file("../evil", b"x")]),
        ("absolute", vec![file("/tmp/evil", b"x")]),
        ("nested parent", vec![file("a/../../evil", b"x")]),
        ("inner dot", vec![file("a/./b", b"x")]),
        ("backslash", vec![file("a\\..\\evil", b"x")]),
        ("empty name", vec![file("", b"x")]),
        (
            "pax path parent",
            vec![pax(b'x', &[("path", b"../evil")]), file("safe", b"x")],
        ),
        (
            "pax path NUL",
            vec![pax(b'x', &[("path", b"a\0b")]), file("safe", b"x")],
        ),
        (
            "GNU long name parent",
            vec![gnu_long(b'L', b"../evil"), file("safe", b"x")],
        ),
        (
            "ustar prefix parent",
            vec![record(
                edited(ustar(b"evil", b'0', 1, b""), |header| {
                    header[345..347].copy_from_slice(b"..")
                }),
                b"x",
            )],
        ),
        (
            "name not UTF-8",
            vec![record(ustar(b"bad\xff", b'0', 1, b""), b"x")],
        ),
    ] {
        assert_rejected_without_leftovers(case, &tar(&members));
    }
}

#[test]
fn links_stay_inside_and_never_carry_writes() {
    for (case, members) in [
        (
            "symlink climbs out",
            vec![link("a/l", b'2', "../../outside")],
        ),
        ("symlink absolute", vec![link("l", b'2', "/etc")]),
        (
            "symlink dot-dot after a name",
            vec![link("a/l", b'2', "b/../../x")],
        ),
        ("symlink empty", vec![link("l", b'2', "")]),
        (
            "symlink with data",
            vec![record(ustar(b"l", b'2', 1, b"x"), b"x")],
        ),
        (
            "file through symlink",
            vec![directory("d"), link("l", b'2', "d"), file("l/x", b"x")],
        ),
        ("hard link outside", vec![link("h", b'1', "../outside")]),
        ("hard link absolute", vec![link("h", b'1', "/etc/passwd")]),
        ("hard link to nothing", vec![link("h", b'1', "missing")]),
        (
            "hard link to a later file",
            vec![link("h", b'1', "f"), file("f", b"x")],
        ),
        (
            "hard link to a symlink",
            vec![file("f", b"x"), link("l", b'2', "f"), link("h", b'1', "l")],
        ),
        (
            "hard link to a directory",
            vec![directory("d"), link("h", b'1', "d")],
        ),
        (
            "hard link with data",
            vec![file("f", b"x"), record(ustar(b"h", b'1', 1, b"f"), b"y")],
        ),
        (
            "file through hard link",
            vec![file("f", b"x"), link("h", b'1', "f"), file("h/x", b"x")],
        ),
        (
            "pax linkpath climbs out",
            vec![
                pax(b'x', &[("linkpath", b"../../x")]),
                link("l", b'2', "ok"),
            ],
        ),
    ] {
        assert_rejected_without_leftovers(case, &tar(&members));
    }
}

#[test]
fn special_and_unsupported_types_are_rejected() {
    for (case, kind) in [
        ("character device", b'3'),
        ("block device", b'4'),
        ("FIFO", b'6'),
        ("GNU sparse", b'S'),
        ("GNU volume label", b'V'),
        ("GNU multivolume", b'M'),
        ("GNU dump directory", b'D'),
        ("vendor type", b'X'),
    ] {
        assert_rejected_without_leftovers(case, &tar(&[record(ustar(b"x", kind, 0, b""), b"")]));
    }
    assert_rejected_without_leftovers(
        "pax sparse",
        &tar(&[pax(b'x', &[("GNU.sparse.size", b"10")]), file("x", b"x")]),
    );
}

#[test]
fn ambiguous_entry_sets_are_rejected() {
    for (case, members) in [
        (
            "replacing a hard-linked file",
            vec![file("a", b"1"), link("h", b'1', "a"), file("a", b"2")],
        ),
        (
            "replacing a symlink",
            vec![link("l", b'2', "x"), link("l", b'2', "y")],
        ),
        ("file then directory", vec![file("a", b"1"), directory("a")]),
        ("directory then file", vec![directory("a"), file("a", b"1")]),
        (
            "file inside a file",
            vec![file("a", b"1"), file("a/b", b"2")],
        ),
        (
            "file after its child",
            vec![file("a/b", b"2"), file("a", b"1")],
        ),
        (
            "two pax headers",
            vec![
                pax(b'x', &[("path", b"a")]),
                pax(b'x', &[("path", b"b")]),
                file("c", b"x"),
            ],
        ),
        (
            "repeated pax key",
            vec![
                pax(b'x', &[("path", b"a"), ("path", b"b")]),
                file("c", b"x"),
            ],
        ),
        (
            "pax path and GNU long name",
            vec![
                pax(b'x', &[("path", b"a")]),
                gnu_long(b'L', b"b"),
                file("c", b"x"),
            ],
        ),
        (
            "two GNU long names",
            vec![gnu_long(b'L', b"a"), gnu_long(b'L', b"b"), file("c", b"x")],
        ),
        (
            "global path",
            vec![pax(b'g', &[("path", b"a")]), file("c", b"x")],
        ),
        (
            "global size",
            vec![pax(b'g', &[("size", b"0")]), file("c", b"x")],
        ),
        (
            "global after pax",
            vec![
                pax(b'x', &[("mtime", b"1")]),
                pax(b'g', &[("comment", b"x")]),
                file("c", b"x"),
            ],
        ),
        (
            "unknown pax key",
            vec![pax(b'x', &[("future", b"1")]), file("c", b"x")],
        ),
        (
            "binary pax names",
            vec![pax(b'x', &[("hdrcharset", b"BINARY")]), file("c", b"x")],
        ),
        (
            "malformed pax record",
            vec![
                record(ustar(b"PaxHeader", b'x', 9, b""), b"8 path=a\n"),
                file("c", b"x"),
            ],
        ),
        (
            "pax size not a number",
            vec![pax(b'x', &[("size", b"-1")]), file("c", b"x")],
        ),
    ] {
        assert_rejected_without_leftovers(case, &tar(&members));
    }
}

#[test]
fn truncation_and_trailing_data_are_rejected() {
    let whole = tar(&[file("a", &[7; 700])]);
    assert_rejected_without_leftovers("truncated header", &whole[..300]);
    assert_rejected_without_leftovers("truncated data", &whole[..BLOCK + 600]);
    assert_rejected_without_leftovers("no end marker", &whole[..3 * BLOCK]);
    assert_rejected_without_leftovers(
        "ends after a pax header",
        &tar(&[pax(b'x', &[("path", b"a")])]),
    );
    let mut lone_zero_then_header = file("a", b"x");
    lone_zero_then_header.extend_from_slice(&[0; BLOCK]);
    lone_zero_then_header.extend_from_slice(&tar(&[file("hidden", b"x")]));
    assert_rejected_without_leftovers("header after a zero block", &lone_zero_then_header);
    let mut trailing = whole.clone();
    trailing.extend_from_slice(b"hidden");
    assert_rejected_without_leftovers("data after the end marker", &trailing);
    let mut bad_sum = tar(&[file("a", b"x"), file("b", b"y")]);
    bad_sum[2 * BLOCK] = b'c';
    assert_rejected_without_leftovers("checksum mismatch", &bad_sum);
    for (case, edit) in [
        (
            "unknown magic",
            (|header: &mut [u8; BLOCK]| header[257..263].copy_from_slice(b"ustaX\0"))
                as fn(&mut [u8; BLOCK]),
        ),
        ("size not octal", |header| {
            header[124..136].copy_from_slice(b"0000000009\0\0")
        }),
        ("negative base-256 size", |header| {
            header[124..136].copy_from_slice(&[0xff; 12])
        }),
        ("bytes after a numeric terminator", |header| {
            header[124..136].copy_from_slice(b"0000000001 7")
        }),
    ] {
        let header = edited(ustar(b"a", b'0', 1, b""), edit);
        assert_rejected_without_leftovers(case, &tar(&[record(header, b"x")]));
    }
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn compressed_streams_must_end_cleanly() {
    let noise: Vec<u8> = (0..300_000u32)
        .map(|index| (index.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let plain = tar(&[file("a", &[7; 5000]), file("noise", &noise)]);
    let compressed = gzip(&plain);
    let mut trailing = compressed.clone();
    trailing.extend_from_slice(b"garbage");
    assert_rejected_without_leftovers("gzip trailing garbage", &trailing);
    let mut bad_crc = compressed.clone();
    let crc = bad_crc.len() - 8;
    bad_crc[crc] ^= 1;
    assert_rejected_without_leftovers("gzip CRC mismatch", &bad_crc);
    assert_rejected_without_leftovers("gzip truncated", &compressed[..compressed.len() - 4]);
    let xz = {
        let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
        encoder.write_all(&plain).unwrap();
        encoder.finish().unwrap()
    };
    assert_rejected_without_leftovers("xz truncated", &xz[..xz.len() - 4]);
    let zstd = zstd::encode_all(&plain[..], 3).unwrap();
    assert_rejected_without_leftovers("zstd truncated", &zstd[..zstd.len() - 4]);
    let hidden = gzip(&tar(&[file("a", b"x")]));
    let mut after_end = hidden.clone();
    after_end.extend_from_slice(&gzip(&tar(&[file("hidden", b"x")])));
    assert_rejected_without_leftovers("second tar in a second gzip member", &after_end);
    let mut pzstd = vec![0x50, 0x2a, 0x4d, 0x18, 4, 0, 0, 0, 0, 0, 0, 0];
    pzstd.extend_from_slice(&zstd);
    for (case, bytes) in [
        ("gzip", compressed),
        ("xz", xz),
        ("zstd", zstd),
        ("pzstd", pzstd),
    ] {
        let (root, result) = extract(&bytes);
        result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
        assert_eq!(
            fs::read(root.path().join("out/a")).unwrap(),
            [7; 5000],
            "{case}"
        );
    }
}

#[test]
fn output_limit_is_enforced_before_writing() {
    let bytes = tar(&[file("a", &[1; 3000]), file("b", &[2; 3000])]);
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.tar");
    fs::write(&archive, &bytes).unwrap();
    let limited = |limit| ReadOptions {
        max_output_bytes: Some(limit),
    };
    assert!(read_archive(&archive, Some(&root.path().join("small")), limited(5999)).is_err());
    assert!(!root.path().join("small").exists());
    read_archive(&archive, Some(&root.path().join("exact")), limited(6000)).unwrap();
}

#[test]
fn names_differing_only_in_case_never_lose_data() {
    let (root, result) = extract(&tar(&[
        file("Readme.txt", b"upper"),
        file("readme.txt", b"lower"),
    ]));
    let destination = root.path().join("out");
    match result {
        Ok(_) => {
            assert_eq!(fs::read(destination.join("Readme.txt")).unwrap(), b"upper");
            assert_eq!(fs::read(destination.join("readme.txt")).unwrap(), b"lower");
        }
        Err(_) => assert!(!destination.exists(), "failed extraction left output"),
    }
}

#[test]
fn real_writer_forms_extract() {
    let base256_size = edited(ustar(b"big-size", b'0', 0, b""), |header| {
        header[124..136].fill(0);
        header[124] = 0x80;
        header[135] = 3;
    });
    let v7 = edited(ustar(b"v7.txt", 0, 2, b""), |header| {
        header[257..265].fill(0)
    });
    let old_directory = ustar(b"old-dir/", b'0', 0, b"");
    let bytes = tar(&[
        pax(
            b'g',
            &[("comment", b"0123456789abcdef0123456789abcdef01234567")],
        ),
        directory("./"),
        file("./dotted.txt", b"dotted"),
        record(base256_size, b"abc"),
        record(v7, b"v7"),
        record(old_directory, b""),
        file("locked/child.txt", b"child"),
        record(
            edited(ustar(b"locked", b'5', 0, b""), |header| {
                header[100..108].copy_from_slice(b"0000555\0")
            }),
            b"",
        ),
        pax(
            b'x',
            &[
                ("mtime", b"-1.25"),
                ("LIBARCHIVE.xattr.user.test", b"dmFsdWU="),
            ],
        ),
        file("negative-time.txt", b"t"),
        pax(b'x', &[("mtime", b"1000000000000")]),
        file("far-future.txt", b"f"),
        gnu_long(b'L', "n".repeat(150).as_bytes()),
        file("short", b"long name"),
        file("chain/original", b"same"),
        link("chain/first", b'1', "chain/original"),
        link("chain/second", b'1', "./chain/first"),
    ]);
    let (root, result) = extract(&bytes);
    result.unwrap_or_else(|error| panic!("{error:#}"));
    let out = root.path().join("out");
    let read = |name: &str| fs::read(out.join(name)).unwrap();
    assert_eq!(read("dotted.txt"), b"dotted");
    assert_eq!(read("big-size"), b"abc");
    assert_eq!(read("v7.txt"), b"v7");
    assert!(out.join("old-dir").is_dir());
    assert_eq!(read("locked/child.txt"), b"child");
    assert_eq!(read(&"n".repeat(150)), b"long name");
    assert_eq!(read("chain/second"), b"same");
    let modified = fs::metadata(out.join("negative-time.txt"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        std::time::UNIX_EPOCH.duration_since(modified).unwrap(),
        std::time::Duration::from_millis(1250)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        assert_eq!(
            fs::metadata(out.join("locked"))
                .unwrap()
                .permissions()
                .mode()
                & 0o222,
            0
        );
        assert_eq!(
            fs::metadata(out.join("chain/second")).unwrap().ino(),
            fs::metadata(out.join("chain/original")).unwrap().ino()
        );
    }
    common::make_tree_deletable(root.path());
}

mod common;

#[test]
fn repeated_entries_follow_the_last_one_like_gnu_tar() {
    let (root, result) = extract(&tar(&[
        directory("d"),
        file("d/f", b"first"),
        record(
            edited(ustar(b"d", b'5', 0, b""), |header| {
                header[100..108].copy_from_slice(b"0000700\0")
            }),
            b"",
        ),
        file("d/f", b"second"),
    ]));
    result.unwrap_or_else(|error| panic!("{error:#}"));
    assert_eq!(fs::read(root.path().join("out/d/f")).unwrap(), b"second");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.path().join("out/d"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the last directory entry sets the mode");
    }
}

fn zstd_archive(path: &Path, members: impl Iterator<Item = Vec<u8>>) {
    let mut encoder = zstd::Encoder::new(fs::File::create(path).unwrap(), 1).unwrap();
    for member in members {
        encoder.write_all(&member).unwrap();
    }
    encoder.write_all(&[0; 2 * BLOCK]).unwrap();
    encoder.finish().unwrap();
}

#[test]
fn implied_directories_and_link_targets_count_toward_the_name_budget() {
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("deep.tar.zst");
    zstd_archive(
        &deep,
        (0..20).map(|index| {
            let name = format!("{index:04}/{}f", "a/".repeat(2040));
            [gnu_long(b'L', name.as_bytes()), file("ignored", b"x")].concat()
        }),
    );
    let target = "t".repeat(4000);
    let linked = root.path().join("linked.tar.zst");
    zstd_archive(
        &linked,
        std::iter::once([gnu_long(b'L', target.as_bytes()), file("ignored", b"x")].concat()).chain(
            (0..17_000).map(|index| {
                [
                    gnu_long(b'K', target.as_bytes()),
                    link(&format!("h{index}"), b'1', "ignored"),
                ]
                .concat()
            }),
        ),
    );
    for archive in [deep, linked] {
        let error = read_archive(&archive, None, ReadOptions::default()).unwrap_err();
        assert!(
            format!("{error:#}").contains("64 MiB"),
            "{}: {error:#}",
            archive.display()
        );
    }
}
