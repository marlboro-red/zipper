//! The `ar` reader extracts GNU, BSD and System V archives byte for byte,
//! extracts the tar a Debian package holds, rejects each hostile layout with
//! nothing left behind, and honours the options the tar reader takes. Where
//! 7-Zip is installed, it is the oracle for the valid forms.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};
use zipper::{
    Control, Existing, Metadata, Strip,
    native_ar::{ReadOptions, read_archive},
};

const TIME: u64 = 1_700_000_000;

fn header(name: &[u8], size: usize, mode: u32) -> Vec<u8> {
    let mut bytes = name.to_vec();
    bytes.resize(16, b' ');
    bytes
        .extend_from_slice(format!("{TIME:<12}{:<6}{:<6}{mode:<8o}{size:<10}`\n", 0, 0).as_bytes());
    assert_eq!(bytes.len(), 60);
    bytes
}

fn raw(name: &[u8], data: &[u8]) -> Vec<u8> {
    let mut bytes = header(name, data.len(), 0o100644);
    bytes.extend_from_slice(data);
    if data.len() % 2 == 1 {
        bytes.push(b'\n');
    }
    bytes
}

fn gnu(name: &str, data: &[u8]) -> Vec<u8> {
    raw(format!("{name}/").as_bytes(), data)
}

fn bsd(name: &str, data: &[u8]) -> Vec<u8> {
    let mut padded = name.as_bytes().to_vec();
    padded.resize(padded.len().next_multiple_of(4), 0);
    let mut body = padded.clone();
    body.extend_from_slice(data);
    let mut bytes = header(
        format!("#1/{}", padded.len()).as_bytes(),
        body.len(),
        0o100755,
    );
    bytes.extend_from_slice(&body);
    if body.len() % 2 == 1 {
        bytes.push(b'\n');
    }
    bytes
}

fn ar(members: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = b"!<arch>\n".to_vec();
    for member in members {
        bytes.extend_from_slice(member);
    }
    bytes
}

fn long_names(names: &[&str]) -> (Vec<u8>, Vec<usize>) {
    let mut table = Vec::new();
    let mut offsets = Vec::new();
    for name in names {
        offsets.push(table.len());
        table.extend_from_slice(name.as_bytes());
        table.extend_from_slice(b"/\n");
    }
    (raw(b"//", &table), offsets)
}

fn ustar(name: &str, kind: u8, data: &[u8], link: &str) -> Vec<u8> {
    let mut header = vec![0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000755\0");
    header[108..116].copy_from_slice(b"0000000\0");
    header[116..124].copy_from_slice(b"0000000\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    header[136..148].copy_from_slice(format!("{TIME:011o}\0").as_bytes());
    header[156] = kind;
    header[157..157 + link.len()].copy_from_slice(link.as_bytes());
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    header.extend_from_slice(data);
    header.resize(header.len().next_multiple_of(512), 0);
    header
}

fn tar(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = entries.concat();
    bytes.extend_from_slice(&[0; 1024]);
    bytes
}

fn package_tar() -> Vec<u8> {
    tar(&[
        ustar("./", b'5', b"", ""),
        ustar("./usr/", b'5', b"", ""),
        ustar("./usr/bin/", b'5', b"", ""),
        ustar("./usr/bin/hello", b'0', b"#!/bin/sh\necho hi\n", ""),
        ustar("./usr/share/doc/hello/copyright", b'0', b"free\n", ""),
    ])
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn xz(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 1);
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn zstd(bytes: &[u8]) -> Vec<u8> {
    zstd::encode_all(bytes, 1).unwrap()
}

fn deb(data_name: &str, data: &[u8]) -> Vec<u8> {
    ar(&[
        raw(b"debian-binary", b"2.0\n"),
        raw(
            b"control.tar.gz",
            &gzip(&tar(&[ustar("./control", b'0', b"Package: hello\n", "")])),
        ),
        raw(data_name.as_bytes(), data),
    ])
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
        .filter(|path| path != "case.a")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.a");
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

fn extract_with(
    bytes: &[u8],
    options: ReadOptions,
) -> (
    tempfile::TempDir,
    PathBuf,
    anyhow::Result<Vec<zipper::Entry>>,
) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.a");
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

fn expected(files: &[(&str, &[u8])]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(name, data)| ((*name).to_owned(), format!("{data:?}")))
        .collect()
}

fn gnu_library() -> Vec<u8> {
    let long = "a_member_name_longer_than_sixteen_bytes.o";
    let other = "another_rather_long_member_name.txt";
    let (table, offsets) = long_names(&[long, other]);
    ar(&[
        raw(b"/", &[0, 0, 0, 1, 0, 0, 0, 0x60, b'f', 0]),
        table,
        gnu("short.o", b"odd"),
        raw(format!("/{}", offsets[0]).as_bytes(), b"long data"),
        raw(format!("/{}", offsets[1]).as_bytes(), b"even"),
        gnu("empty", b""),
    ])
}

fn gnu_tree() -> BTreeMap<String, String> {
    expected(&[
        ("short.o", b"odd"),
        ("a_member_name_longer_than_sixteen_bytes.o", b"long data"),
        ("another_rather_long_member_name.txt", b"even"),
        ("empty", b""),
    ])
}

fn bsd_library() -> Vec<u8> {
    ar(&[
        bsd("__.SYMDEF SORTED", &[0; 8]),
        bsd("short.o", b"bsd"),
        bsd("a_member_name_longer_than_sixteen_bytes.o", b"long bsd"),
        raw(b"plain.txt", b"plain"),
    ])
}

fn bsd_tree() -> BTreeMap<String, String> {
    expected(&[
        ("short.o", b"bsd"),
        ("a_member_name_longer_than_sixteen_bytes.o", b"long bsd"),
        ("plain.txt", b"plain"),
    ])
}

#[test]
fn every_writer_form_extracts_byte_for_byte() {
    for (case, bytes, tree_expected) in [
        ("gnu", gnu_library(), gnu_tree()),
        ("bsd", bsd_library(), bsd_tree()),
    ] {
        let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
        assert_eq!(tree(&destination), tree_expected, "{case}");
        let modified = fs::metadata(destination.join("short.o"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            modified,
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(TIME),
            "{case}"
        );
        let listed =
            read_archive(&root.path().join("case.a"), None, ReadOptions::default()).unwrap();
        assert_eq!(listed.len(), tree_expected.len(), "{case}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let (_root, destination, result) = extract_with(&bsd_library(), ReadOptions::default());
        result.unwrap();
        let mode = |name: &str| {
            fs::metadata(destination.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o111
        };
        assert_ne!(mode("short.o"), 0);
        assert_eq!(mode("plain.txt"), 0);
    }
    let mut unpadded = ar(&[gnu("a", b"x"), gnu("b", b"odd")]);
    assert_eq!(unpadded.pop(), Some(b'\n'));
    let (_root, destination, result) = extract_with(&unpadded, ReadOptions::default());
    result.unwrap();
    assert_eq!(tree(&destination), expected(&[("a", b"x"), ("b", b"odd")]));
    let (_root, destination, result) = extract_with(b"!<arch>\n", ReadOptions::default());
    assert!(result.unwrap().is_empty());
    assert!(destination.is_dir());
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

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> BTreeMap<String, String> {
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
        "7-Zip failed on {}",
        archive.display()
    );
    let inner: Vec<_> = fs::read_dir(into)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    match &inner[..] {
        [only] if only.extension().is_some_and(|extension| extension == "tar") => {
            sevenzip_tree(binary, only, &into.with_extension("inner"))
        }
        _ => tree(into),
    }
}

#[test]
fn archives_and_packages_extract_the_tree_7zip_extracts() {
    let Some(binary) = sevenzip() else {
        eprintln!("7-Zip is not installed; skipped");
        return;
    };
    let data = package_tar();
    let (table, offsets) = long_names(&["a_member_name_longer_than_sixteen_bytes.o"]);
    for (case, bytes) in [
        (
            "gnu long names",
            ar(&[
                table,
                gnu("x.o", b"x"),
                raw(format!("/{}", offsets[0]).as_bytes(), b"long"),
            ]),
        ),
        (
            "bsd long names",
            ar(&[
                bsd("short.o", b"bsd"),
                bsd("a_member_name_longer_than_sixteen_bytes.o", b"y"),
            ]),
        ),
        (
            "sysv short names",
            ar(&[raw(b"a.txt", b"a"), raw(b"b c", b"bc")]),
        ),
        ("deb with xz data", deb("data.tar.xz", &xz(&data))),
        ("deb with gzip data", deb("data.tar.gz", &gzip(&data))),
        ("deb with zstd data", deb("data.tar.zst", &zstd(&data))),
        ("deb with plain data.tar", deb("data.tar", &data)),
        ("deb with odd data name", deb("data.tar.foo", &data)),
        (
            "deb with two data members",
            ar(&[
                raw(b"debian-binary", b"2.0\n"),
                raw(b"data.tar.gz", &gzip(&data)),
                raw(b"data.tar.xz", &xz(&data)),
            ]),
        ),
        (
            "debian-binary not first",
            ar(&[
                raw(b"control.tar.gz", b"c"),
                raw(b"debian-binary", b"2.0\n"),
                raw(b"data.tar.xz", &xz(&data)),
            ]),
        ),
        (
            "debian-binary with GNU's slash",
            ar(&[
                gnu("debian-binary", b"2.0\n"),
                gnu("data.tar.xz", &xz(&data)),
            ]),
        ),
        (
            "data member with GNU's slash",
            ar(&[
                raw(b"debian-binary", b"2.0\n"),
                gnu("data.tar.xz", &xz(&data)),
            ]),
        ),
    ] {
        let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
        let reference = sevenzip_tree(
            &binary,
            &root.path().join("case.a"),
            &root.path().join("direct"),
        );
        assert_eq!(tree(&destination), reference, "{case}");
    }
}

#[test]
fn a_debian_package_extracts_its_data_tar_with_every_option() {
    let data = package_tar();
    for (name, bytes) in [
        ("data.tar.xz", xz(&data)),
        ("data.tar.gz", gzip(&data)),
        ("data.tar.zst", zstd(&data)),
    ] {
        let (_root, destination, result) = extract_with(&deb(name, &bytes), ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{name}: {error:#}"));
        assert_eq!(
            fs::read(destination.join("usr/bin/hello")).unwrap(),
            b"#!/bin/sh\necho hi\n",
            "{name}"
        );
        assert!(!destination.join("debian-binary").exists());
    }
    let package = deb("data.tar.xz", &xz(&data));
    let paths = ["usr/bin".to_owned()];
    let (_root, destination, result) = extract_with(
        &package,
        ReadOptions {
            paths: &paths,
            strip: Strip::Components(3),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(
        tree(&destination).into_keys().collect::<Vec<_>>(),
        ["hello"]
    );
    let cancel = AtomicBool::new(true);
    let (_root, destination, result) = extract_with(
        &package,
        ReadOptions {
            control: Control {
                progress: None,
                cancel: Some(&cancel),
            },
            ..ReadOptions::default()
        },
    );
    assert!(result.unwrap_err().is::<zipper::Cancelled>());
    assert!(!destination.exists());
    let escaping = tar(&[ustar("./usr/link", b'2', b"", "../../../etc/passwd")]);
    assert_rejected_without_leftovers(
        "deb data with an escaping link",
        &deb("data.tar.gz", &gzip(&escaping)),
    );
    let traversal = tar(&[ustar("../../escape", b'0', b"x", "")]);
    assert_rejected_without_leftovers(
        "deb data with traversal",
        &deb("data.tar.xz", &xz(&traversal)),
    );
    assert_rejected_without_leftovers(
        "deb data that is not a tar",
        &deb("data.tar.xz", &xz(b"text\n")),
    );
    let lzma = {
        let options = liblzma::stream::LzmaOptions::new_preset(1).unwrap();
        let stream = liblzma::stream::Stream::new_lzma_encoder(&options).unwrap();
        let mut encoder = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
        encoder.write_all(&data).unwrap();
        encoder.finish().unwrap()
    };
    let (_root, destination, result) =
        extract_with(&deb("data.tar.lzma", &lzma), ReadOptions::default());
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("--zip-engine backend"), "{error}");
    assert!(!destination.exists());
}

#[test]
fn unsafe_names_never_escape() {
    for name in [
        "..",
        "../escape",
        "a/../../escape",
        "/etc/passwd",
        "C:escape",
        "a\\..\\escape",
        "a//b",
        "a\0b",
    ] {
        assert_rejected_without_leftovers(&format!("bsd {name:?}"), &ar(&[bsd(name, b"x")]));
        if !name.contains('\0') {
            let (table, _) = long_names(&[name]);
            assert_rejected_without_leftovers(
                &format!("gnu long {name:?}"),
                &ar(&[table, raw(b"/0", b"x")]),
            );
        }
    }
    for name in [&b".."[..], b"C:x", b"a\\b"] {
        assert_rejected_without_leftovers(&format!("short {name:?}"), &ar(&[raw(name, b"x")]));
    }
    assert_rejected_without_leftovers("empty bsd name", &ar(&[bsd("", b"x")]));
    assert_rejected_without_leftovers("name of NUL bytes", &ar(&[bsd("\0\0\0\0", b"x")]));
    assert_rejected_without_leftovers("name that is not UTF-8", &ar(&[raw(b"bad\xff", b"x")]));
    assert_rejected_without_leftovers("unknown special member", &ar(&[raw(b"/LINKER/", b"x")]));
    assert_rejected_without_leftovers("thin archive", b"!<thin>\n");
    let error = format!(
        "{:#}",
        extract_with(b"!<thin>\n", ReadOptions::default())
            .2
            .unwrap_err()
    );
    assert!(error.contains("thin"), "{error}");
}

#[test]
fn ambiguous_entry_sets_are_rejected_and_repeats_follow_the_last() {
    assert_rejected_without_leftovers(
        "member inside a member",
        &ar(&[raw(b"a", b"x"), bsd("a/b", b"y")]),
    );
    assert_rejected_without_leftovers(
        "member where an implied folder is",
        &ar(&[bsd("a/b", b"y"), raw(b"a", b"x")]),
    );
    let (_root, destination, result) = extract_with(
        &ar(&[gnu("x.o", b"old"), gnu("x.o", b"new")]),
        ReadOptions::default(),
    );
    result.unwrap();
    assert_eq!(fs::read(destination.join("x.o")).unwrap(), b"new");
    let (_root, destination, result) = extract_with(
        &ar(&[raw(b"Readme.txt", b"upper"), raw(b"readme.txt", b"lower")]),
        ReadOptions::default(),
    );
    match result {
        Ok(_) => {
            assert_eq!(fs::read(destination.join("Readme.txt")).unwrap(), b"upper");
            assert_eq!(fs::read(destination.join("readme.txt")).unwrap(), b"lower");
        }
        Err(_) => assert!(!destination.exists(), "failed extraction left output"),
    }
}

fn members_of(case: &str) -> Vec<Vec<u8>> {
    match case {
        "gnu" => {
            let (table, offsets) = long_names(&["a_member_name_longer_than_sixteen_bytes.o"]);
            vec![
                raw(b"/", &[0, 0, 0, 1, 0, 0, 0, 0x60, b'f', 0]),
                table,
                gnu("short.o", b"odd"),
                raw(format!("/{}", offsets[0]).as_bytes(), b"long data"),
                gnu("tail", b"abc"),
            ]
        }
        "bsd" => vec![
            bsd("__.SYMDEF SORTED", &[0; 8]),
            bsd("short.o", b"bsd"),
            bsd("a_member_name_longer_than_sixteen_bytes.o", b"long bsd"),
        ],
        _ => vec![
            raw(b"debian-binary", b"2.0\n"),
            raw(b"data.tar.gz", &gzip(&package_tar())),
        ],
    }
}

#[test]
fn truncation_inside_any_member_is_rejected_and_a_cut_between_members_keeps_the_ones_before() {
    for case in ["gnu", "bsd", "deb"] {
        let members = members_of(case);
        let bytes = ar(&members);
        let mut clean_cuts = vec![8];
        let mut end = 8;
        for member in &members {
            end += member.len();
            let size: usize = String::from_utf8_lossy(&member[48..58])
                .trim()
                .parse()
                .unwrap();
            if size % 2 == 1 {
                clean_cuts.push(end - 1);
            }
            clean_cuts.push(end);
        }
        for cut in 8..bytes.len() {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("case.a");
            fs::write(&archive, &bytes[..cut]).unwrap();
            let destination = root.path().join("out");
            let result = read_archive(&archive, Some(&destination), ReadOptions::default());
            if clean_cuts.contains(&cut) {
                result.unwrap_or_else(|error| {
                    panic!("{case} cut between members at {cut}: {error:#}")
                });
                continue;
            }
            assert!(
                result.is_err(),
                "{case} cut at {cut} of {} was accepted",
                bytes.len()
            );
            assert!(!destination.exists(), "{case} cut at {cut} left output");
        }
    }
    for cut in 0..8 {
        assert_rejected_without_leftovers(&format!("magic cut at {cut}"), &b"!<arch>\n"[..cut]);
    }
}

#[test]
fn sizes_that_lie_are_rejected() {
    let mut longer = ar(&[raw(b"a", b"data")]);
    longer[8 + 48..8 + 58].copy_from_slice(b"100       ");
    assert_rejected_without_leftovers("size past the end", &longer);
    let mut shorter = ar(&[raw(b"a", b"datadata"), raw(b"b", b"more")]);
    shorter[8 + 48..8 + 58].copy_from_slice(b"2         ");
    assert_rejected_without_leftovers("size short of the next header", &shorter);
    let (table, _) = long_names(&["long_name_number_one"]);
    assert_rejected_without_leftovers(
        "offset outside the table",
        &ar(&[table.clone(), raw(b"/99", b"x")]),
    );
    assert_rejected_without_leftovers(
        "long name before its table",
        &ar(&[raw(b"/0", b"x"), table.clone()]),
    );
    assert_rejected_without_leftovers(
        "two long name tables",
        &ar(&[table.clone(), table, raw(b"/0", b"x")]),
    );
    assert_rejected_without_leftovers(
        "unterminated long name",
        &ar(&[raw(b"//", b"abc"), raw(b"/0", b"x")]),
    );
    let mut bsd_name = bsd("name", b"x");
    bsd_name[..16].copy_from_slice(b"#1/9            ");
    let bsd_name = ar(&[bsd_name]);
    assert_rejected_without_leftovers("bsd name longer than its member", &bsd_name);
    let error = format!(
        "{:#}",
        extract_with(&bsd_name, ReadOptions::default())
            .2
            .unwrap_err()
    );
    assert!(error.contains("does not fit its member"), "{error}");
}

#[test]
fn numeric_fields_that_overflow_or_are_not_numbers_are_rejected() {
    for (case, range, value) in [
        ("size not decimal", 48..58, &b"abc       "[..]),
        ("size blank", 48..58, b"          "),
        ("size with a leading space", 48..58, b" 4        "),
        ("size with a sign", 48..58, b"+4        "),
        ("mode not octal", 40..48, b"100648  "),
        ("time not decimal", 16..28, b"12x         "),
        ("no end marker", 58..60, b"``"),
    ] {
        let mut bytes = ar(&[raw(b"a", b"data")]);
        bytes[8 + range.start..8 + range.end].copy_from_slice(value);
        assert_rejected_without_leftovers(case, &bytes);
    }
}

#[test]
fn huge_declared_sizes_are_refused_before_allocation() {
    for (case, name) in [("member", &b"a"[..]), ("long name table", b"//")] {
        let mut bytes = ar(&[raw(name, b"data")]);
        bytes[8 + 48..8 + 58].copy_from_slice(b"9999999999");
        let (_root, _destination, result) = extract_with(&bytes, ReadOptions::default());
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("truncated"), "{case}: {error}");
    }
    let bytes = ar(&[raw(b"#1/99999999999", b"data")]);
    let error = format!(
        "{:#}",
        extract_with(&bytes, ReadOptions::default()).2.unwrap_err()
    );
    assert!(error.contains("does not fit its member"), "{error}");
    let mut table = vec![b'n'; 16 * 1024 * 1024];
    table.extend_from_slice(b"/\n");
    let bytes = ar(&[raw(b"//", &table), raw(b"/0", b"x")]);
    let error = format!(
        "{:#}",
        extract_with(&bytes, ReadOptions::default()).2.unwrap_err()
    );
    assert!(error.contains("16 MiB"), "{error}");
}

#[test]
fn output_limit_is_enforced_before_writing() {
    let bytes = ar(&[raw(b"a", &[1; 3000]), raw(b"b", &[2; 3000])]);
    let limited = |limit| ReadOptions {
        max_output_bytes: Some(limit),
        ..ReadOptions::default()
    };
    let (_root, destination, result) = extract_with(&bytes, limited(5999));
    assert!(result.is_err());
    assert!(!destination.exists());
    extract_with(&bytes, limited(6000)).2.unwrap();
}

#[test]
fn selection_strip_metadata_existing_and_progress_options_apply() {
    let bytes = ar(&[
        bsd("lib/a.o", b"a"),
        bsd("lib/b.txt", b"bb"),
        raw(b"top", b"t"),
    ]);
    let include = ["*.o".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            include: &include,
            strip: Strip::Prefix("lib"),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(tree(&destination), expected(&[("a.o", b"a")]));
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            metadata: Metadata::ContentOnly,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_ne!(
        fs::metadata(destination.join("top"))
            .unwrap()
            .modified()
            .unwrap(),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(TIME)
    );
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.a");
    fs::write(&archive, &bytes).unwrap();
    let destination = root.path().join("out");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("top"), b"old").unwrap();
    assert!(read_archive(&archive, Some(&destination), ReadOptions::default()).is_err());
    read_archive(
        &archive,
        Some(&destination),
        ReadOptions {
            existing: Existing::Skip,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(fs::read(destination.join("top")).unwrap(), b"old");
    assert_eq!(fs::read(destination.join("lib/a.o")).unwrap(), b"a");
    let seen = std::sync::Mutex::new(Vec::new());
    let report = |progress: zipper::Progress| seen.lock().unwrap().push(progress);
    let (_root, _destination, result) = extract_with(
        &bytes,
        ReadOptions {
            control: Control {
                progress: Some(&report),
                cancel: None,
            },
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    let last = *seen.lock().unwrap().last().unwrap();
    assert_eq!((last.files_done, last.bytes_done), (3, 4));
    let cancel = AtomicBool::new(false);
    let cancel_on_first = |_: zipper::Progress| cancel.store(true, Ordering::Relaxed);
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            control: Control {
                progress: Some(&cancel_on_first),
                cancel: Some(&cancel),
            },
            ..ReadOptions::default()
        },
    );
    assert!(result.unwrap_err().is::<zipper::Cancelled>());
    assert!(!destination.exists());
}

#[test]
fn the_default_engine_routes_ar_and_packages_to_this_reader() {
    let root = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("lib.a", gnu_library()),
        ("hello.deb", deb("data.tar.xz", &xz(&package_tar()))),
        ("thin.a", b"!<thin>\n".to_vec()),
    ] {
        let archive = root.path().join(name);
        fs::write(&archive, bytes).unwrap();
        assert_eq!(
            zipper::choose_reader(zipper::Engine::Auto, &archive).unwrap(),
            zipper::Reader::Ar,
            "{name}"
        );
    }
}
