//! The cpio reader extracts every form byte for byte, rejects each hostile
//! layout with nothing left behind, and honours the options the tar reader
//! takes. Where 7-Zip is installed, it is the oracle for the valid forms.
#[path = "common/sevenzip.rs"]
mod sevenzip;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};
use zipper::{
    Control, Existing, Metadata, Strip,
    native_cpio::{ReadOptions, read_archive},
};

const FILE: u32 = 0o100644;
const EXECUTABLE: u32 = 0o100755;
const DIRECTORY: u32 = 0o040755;
const SYMLINK: u32 = 0o120777;
const TIME: u64 = 1_700_000_000;
const NEWC_NAME: usize = 110;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Form {
    Newc,
    Crc,
    Odc,
    Binary,
    BigBinary,
}

const FORMS: [Form; 5] = [
    Form::Newc,
    Form::Crc,
    Form::Odc,
    Form::Binary,
    Form::BigBinary,
];

#[derive(Clone)]
struct Member {
    name: Vec<u8>,
    mode: u32,
    data: Vec<u8>,
    inode: u64,
    links: u64,
}

fn member(name: &str, mode: u32, data: &[u8]) -> Member {
    Member {
        name: name.as_bytes().to_vec(),
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

fn header(form: Form, member: &Member, index: u64) -> Vec<u8> {
    let inode = if member.inode == 0 {
        index + 1
    } else {
        member.inode
    };
    let name_size = member.name.len() as u64 + 1;
    let size = member.data.len() as u64;
    let mut bytes = match form {
        Form::Newc | Form::Crc => {
            let check = if form == Form::Crc {
                member
                    .data
                    .iter()
                    .fold(0u32, |sum, &byte| sum.wrapping_add(u32::from(byte)))
            } else {
                0
            };
            let fields = [
                inode,
                u64::from(member.mode),
                0,
                0,
                member.links,
                TIME,
                size,
                0,
                0,
                0,
                0,
                name_size,
                u64::from(check),
            ];
            let magic = if form == Form::Crc {
                "070702"
            } else {
                "070701"
            };
            let mut text = magic.to_owned();
            for field in fields {
                text.push_str(&format!("{field:08X}"));
            }
            text.into_bytes()
        }
        Form::Odc => format!(
            "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}",
            0, inode, member.mode, 0, 0, member.links, 0, TIME, name_size, size
        )
        .into_bytes(),
        Form::Binary | Form::BigBinary => {
            let words = [
                0o070707,
                0,
                inode as u16,
                member.mode as u16,
                0,
                0,
                member.links as u16,
                0,
                (TIME >> 16) as u16,
                TIME as u16,
                name_size as u16,
                (size >> 16) as u16,
                size as u16,
            ];
            words
                .iter()
                .flat_map(|word| {
                    if form == Form::BigBinary {
                        word.to_be_bytes()
                    } else {
                        word.to_le_bytes()
                    }
                })
                .collect()
        }
    };
    bytes.extend_from_slice(&member.name);
    bytes.push(0);
    pad(&mut bytes, alignment(form));
    bytes
}

fn alignment(form: Form) -> usize {
    match form {
        Form::Newc | Form::Crc => 4,
        Form::Odc => 1,
        Form::Binary | Form::BigBinary => 2,
    }
}

fn record(form: Form, member: &Member, index: u64) -> Vec<u8> {
    let mut bytes = header(form, member, index);
    bytes.extend_from_slice(&member.data);
    pad(&mut bytes, alignment(form));
    bytes
}

fn trailer(form: Form) -> Vec<u8> {
    record(form, &member("TRAILER!!!", 0, b""), 0)
}

fn cpio(form: Form, members: &[Member]) -> Vec<u8> {
    let mut bytes: Vec<u8> = members
        .iter()
        .enumerate()
        .flat_map(|(index, member)| record(form, member, index as u64))
        .collect();
    bytes.extend(trailer(form));
    pad(&mut bytes, 512);
    bytes
}

fn newc(members: &[Member]) -> Vec<u8> {
    cpio(Form::Newc, members)
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
        .filter(|path| path != "case.cpio")
        .collect();
    found.sort();
    found
}

fn assert_rejected_without_leftovers(case: &str, bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.cpio");
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
    let archive = root.path().join("case.cpio");
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
                format!(
                    "link:{}",
                    fs::read_link(entry.path())
                        .unwrap()
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                )
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
        directory("."),
        directory("./folder"),
        file("./folder/a.txt", b"alpha\n"),
        member("odd", EXECUTABLE, b"x"),
        file("folder/three", b"abc"),
        file("empty", b""),
        file(
            "big",
            &(0..70_000u32).map(|n| (n * 7) as u8).collect::<Vec<_>>(),
        ),
    ];
    if cfg!(unix) {
        members.push(symlink("folder/link", "a.txt"));
    }
    members
}

fn sample_tree() -> BTreeMap<String, String> {
    let mut expected = BTreeMap::new();
    for member in sample() {
        let name = String::from_utf8(member.name).unwrap();
        let name = name.trim_start_matches("./").to_owned();
        if name == "." {
            continue;
        }
        let kind = match member.mode & 0o170000 {
            0o040000 => "dir".to_owned(),
            0o120000 => format!("link:{}", String::from_utf8(member.data).unwrap()),
            _ => format!("{:?}", member.data),
        };
        expected.insert(name, kind);
    }
    expected
}

#[test]
fn every_form_extracts_byte_for_byte() {
    for form in FORMS {
        let bytes = cpio(form, &sample());
        let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{form:?}: {error:#}"));
        assert_eq!(tree(&destination), sample_tree(), "{form:?}");
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
            assert_ne!(mode("odd"), 0, "{form:?}: lost the execute bit");
            assert_eq!(mode("empty"), 0, "{form:?}");
            let modified = fs::metadata(destination.join("big"))
                .unwrap()
                .modified()
                .unwrap();
            assert_eq!(
                modified,
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(TIME),
                "{form:?}"
            );
        }
        let listed =
            read_archive(&root.path().join("case.cpio"), None, ReadOptions::default()).unwrap();
        assert_eq!(
            listed.iter().filter(|entry| !entry.directory).count(),
            sample_tree().values().filter(|kind| *kind != "dir").count()
        );
    }
}

#[test]
fn every_form_extracts_the_tree_7zip_extracts() {
    let Some(binary) = sevenzip::sevenzip() else {
        eprintln!("7-Zip is not installed; skipped");
        return;
    };
    let shared = [
        linked("first", b"", 40, 3),
        linked("second", b"", 40, 3),
        linked("third", b"shared\n", 40, 3),
        linked("empty-1", b"", 41, 2),
        linked("empty-2", b"", 41, 2),
    ];
    for (case, bytes) in FORMS
        .map(|form| (format!("{form:?}"), cpio(form, &sample())))
        .into_iter()
        .chain([("newc hard links".to_owned(), newc(&shared))])
    {
        let (root, destination, result) = extract_with(&bytes, ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
        let direct = root.path().join("direct");
        let mut flag = std::ffi::OsString::from("-o");
        flag.push(&direct);
        let run = Command::new(&binary)
            .args(["x", "-y"])
            .arg(flag)
            .arg(root.path().join("case.cpio"))
            .output()
            .unwrap();
        assert!(run.status.success(), "{case}: 7-Zip failed");
        assert_eq!(tree(&destination), tree(&direct), "{case}");
    }
}

#[test]
fn newc_hard_links_share_the_data_of_their_last_entry() {
    let bytes = newc(&[
        linked("a", b"", 7, 3),
        linked("b", b"", 7, 3),
        linked("c", b"shared\n", 7, 3),
        linked("e1", b"", 8, 2),
        linked("e2", b"", 8, 2),
        linked("f1", b"first\n", 9, 2),
        linked("f2", b"", 9, 2),
    ]);
    let (_root, destination, result) = extract_with(&bytes, ReadOptions::default());
    result.unwrap();
    for name in ["a", "b", "c"] {
        assert_eq!(fs::read(destination.join(name)).unwrap(), b"shared\n");
    }
    for name in ["e1", "e2"] {
        assert_eq!(fs::read(destination.join(name)).unwrap(), b"");
    }
    assert_eq!(fs::read(destination.join("f2")).unwrap(), b"first\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let inode = |name: &str| fs::metadata(destination.join(name)).unwrap().ino();
        assert_eq!(inode("a"), inode("c"));
        assert_eq!(inode("b"), inode("c"));
        assert_eq!(inode("e1"), inode("e2"));
        assert_eq!(inode("f1"), inode("f2"));
    }
}

#[test]
fn unsafe_names_never_escape() {
    for name in [
        "/etc/passwd",
        "../escape",
        "a/../../escape",
        "a/./b",
        "a//b",
        "C:/escape",
        "C:escape",
        "a\\..\\escape",
        "",
        "..",
        "./../escape",
        "a/\0b",
    ] {
        for form in [Form::Newc, Form::Odc, Form::Binary] {
            assert_rejected_without_leftovers(
                &format!("{form:?} {name:?}"),
                &cpio(form, &[file("ok", b"x"), file(name, b"data")]),
            );
        }
    }
    assert_rejected_without_leftovers(
        "deferred hard link with an unsafe name",
        &newc(&[linked("../escape", b"", 5, 2), linked("ok", b"data", 5, 2)]),
    );
    assert_rejected_without_leftovers(
        "name that is not UTF-8",
        &newc(&[Member {
            name: b"bad\xff".to_vec(),
            ..file("", b"x")
        }]),
    );
}

#[test]
fn links_stay_inside_and_never_carry_writes() {
    for (case, members) in [
        ("parent target", vec![symlink("link", "../outside")]),
        (
            "deep parent target",
            vec![symlink("a/link", "../../outside")],
        ),
        ("absolute target", vec![symlink("link", "/etc/passwd")]),
        ("empty target", vec![symlink("link", "")]),
        ("dot dot after a name", vec![symlink("link", "a/../../x")]),
        (
            "file through a symlink",
            vec![directory("d"), symlink("l", "d"), file("l/x", b"through")],
        ),
        (
            "file through a hard link",
            vec![
                linked("t", b"x", 3, 2),
                linked("h", b"", 3, 2),
                file("h/x", b"through"),
            ],
        ),
        (
            "symlink then file of the same name",
            vec![symlink("l", "t"), file("l", b"overwrite")],
        ),
        ("long target", vec![symlink("link", &"a/".repeat(2049))]),
    ] {
        assert_rejected_without_leftovers(case, &newc(&members));
    }
}

#[test]
fn devices_fifos_sockets_and_unknown_types_are_rejected() {
    for (case, mode) in [
        ("fifo", 0o010644),
        ("character device", 0o020644),
        ("block device", 0o060644),
        ("socket", 0o140644),
        ("no type", 0o000644),
        ("unknown type", 0o170644),
    ] {
        for form in [Form::Newc, Form::Odc, Form::Binary] {
            assert_rejected_without_leftovers(
                &format!("{form:?} {case}"),
                &cpio(form, &[member("node", mode, b"")]),
            );
        }
    }
}

#[test]
fn ambiguous_entry_sets_are_rejected() {
    for (case, members) in [
        ("file then directory", vec![file("a", b"x"), directory("a")]),
        ("directory then file", vec![directory("a"), file("a", b"x")]),
        (
            "file inside a file",
            vec![file("a", b"x"), file("a/b", b"y")],
        ),
        (
            "implied directory then file",
            vec![file("a/b", b"x"), file("a", b"y")],
        ),
        (
            "file replaced after a hard link names it",
            vec![
                linked("t", b"x", 3, 2),
                linked("h", b"", 3, 2),
                file("t", b"new"),
            ],
        ),
    ] {
        assert_rejected_without_leftovers(case, &newc(&members));
    }
    assert_rejected_saying(
        "directory with data",
        &newc(&[member("d", DIRECTORY, b"x")]),
        "has data",
    );
}

#[test]
fn repeated_files_follow_the_last_entry_and_case_twins_never_lose_data() {
    let (_root, destination, result) = extract_with(
        &newc(&[file("a", b"old"), file("a", b"new")]),
        ReadOptions::default(),
    );
    result.unwrap();
    assert_eq!(fs::read(destination.join("a")).unwrap(), b"new");
    let (_root, destination, result) = extract_with(
        &newc(&[file("Readme.txt", b"upper"), file("readme.txt", b"lower")]),
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

#[test]
fn truncation_at_every_byte_is_rejected() {
    for form in FORMS {
        let members = [
            directory("d"),
            file("d/a", b"hello"),
            symlink("l", "d/a"),
            file("odd", b"xyz"),
        ];
        let bytes = cpio(form, &members);
        let end: usize = members
            .iter()
            .enumerate()
            .map(|(index, member)| record(form, member, index as u64).len())
            .sum::<usize>()
            + trailer(form).len();
        for cut in 0..end {
            let shorter = &bytes[..cut];
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join("case.cpio");
            fs::write(&archive, shorter).unwrap();
            let destination = root.path().join("out");
            let result = read_archive(&archive, Some(&destination), ReadOptions::default());
            assert!(
                result.is_err(),
                "{form:?} cut at {cut} of {end} was accepted"
            );
            assert!(!destination.exists(), "{form:?} cut at {cut} left output");
        }
    }
}

#[test]
fn sizes_that_lie_are_rejected() {
    let mut longer = newc(&[file("a", b"data")]);
    longer[54..62].copy_from_slice(b"00010000");
    assert_rejected_without_leftovers("size past the end", &longer);
    let mut shorter = newc(&[file("a", b"datadata"), file("b", b"more")]);
    shorter[54..62].copy_from_slice(b"00000002");
    assert_rejected_without_leftovers("size short of the next header", &shorter);
    let mut name = newc(&[file("abc", b"data")]);
    name[94..102].copy_from_slice(b"00000002");
    assert_rejected_without_leftovers("name size short of its terminator", &name);
    let mut unterminated = newc(&[file("abc", b"data")]);
    unterminated[NEWC_NAME + 3] = b'd';
    assert_rejected_saying("name without its terminator", &unterminated, "NUL");
    let mut crc = cpio(Form::Crc, &[file("a", b"data")]);
    crc[102..110].copy_from_slice(b"00000001");
    assert_rejected_without_leftovers("checksum mismatch", &crc);
    let mut odc = cpio(Form::Odc, &[file("a", b"data")]);
    odc[65..76].copy_from_slice(b"00000000100");
    assert_rejected_without_leftovers("odc size past its data", &odc);
}

#[test]
fn numeric_fields_that_overflow_or_are_not_numbers_are_rejected() {
    for (case, range, value) in [
        ("hex letter out of range", 54..62, &b"0000000G"[..]),
        ("space in hex", 54..62, b" 0000004"),
        ("sign in hex", 54..62, b"+0000004"),
        ("hex prefix", 14..22, b"0x81A400"),
    ] {
        let mut bytes = newc(&[file("a", b"data")]);
        bytes[range].copy_from_slice(value);
        assert_rejected_without_leftovers(case, &bytes);
    }
    for (case, range, value) in [
        ("odc digit eight", 65..76, &b"00000000008"[..]),
        ("odc space", 59..65, b" 00002"),
    ] {
        let mut bytes = cpio(Form::Odc, &[file("a", b"data")]);
        bytes[range].copy_from_slice(value);
        assert_rejected_without_leftovers(case, &bytes);
    }
}

#[test]
fn huge_declared_sizes_are_refused_before_allocation() {
    let mut name = newc(&[file("a", b"data")]);
    name[94..102].copy_from_slice(b"FFFFFFFF");
    let (_root, _destination, result) = extract_with(&name, ReadOptions::default());
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("name size"), "{error}");
    let mut size = newc(&[file("a", b"data")]);
    size[54..62].copy_from_slice(b"FFFFFFFF");
    let (_root, _destination, result) = extract_with(&size, ReadOptions::default());
    let error = format!("{:#}", result.unwrap_err());
    assert!(
        error.contains("free") || error.contains("truncated"),
        "{error}"
    );
    let mut target = newc(&[symlink("l", "t")]);
    target[54..62].copy_from_slice(b"FFFFFFFF");
    let (_root, _destination, result) = extract_with(&target, ReadOptions::default());
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("longer than"), "{error}");
}

#[test]
fn implied_directories_and_deferred_links_count_toward_the_name_budget() {
    let deep: Vec<Member> = (0..20)
        .map(|index| file(&format!("{index:04}/{}f", "a/".repeat(2040)), b"x"))
        .collect();
    let deferred: Vec<Member> = (0..17_000)
        .map(|index| linked(&format!("{index:05}{}", "n".repeat(4000)), b"", 9, 2))
        .collect();
    let root = tempfile::tempdir().unwrap();
    let without_trailer: Vec<u8> = deferred
        .iter()
        .enumerate()
        .flat_map(|(index, member)| record(Form::Newc, member, index as u64))
        .collect();
    for (case, bytes) in [
        ("deep", newc(&deep)),
        ("deferred", newc(&deferred)),
        ("deferred without a trailer", without_trailer),
    ] {
        let archive = root.path().join("budget.cpio");
        fs::write(&archive, bytes).unwrap();
        let result = read_archive(&archive, None, ReadOptions::default());
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("64 MiB"), "{case}: {error}");
    }
}

#[test]
fn the_trailer_ends_the_archive_and_forms_never_mix() {
    let mut bytes = newc(&[file("a", b"x")]);
    bytes.extend_from_slice(b"junk");
    assert_rejected_without_leftovers("data after the trailer", &bytes);
    let mut missing = record(Form::Newc, &file("a", b"x"), 0);
    assert_rejected_without_leftovers("no trailer", &missing);
    missing.extend(record(Form::Odc, &file("b", b"y"), 1));
    missing.extend(trailer(Form::Odc));
    assert_rejected_saying("odc after newc", &missing, "changes form");
    let mut with_data = record(Form::Newc, &file("a", b"x"), 0);
    with_data.extend(record(Form::Newc, &file("TRAILER!!!", b"\0\0\0\0"), 1));
    assert_rejected_without_leftovers("trailer with zero data", &with_data);
    let mut concatenated = newc(&[file("a", b"x")]);
    concatenated.extend(newc(&[file("b", b"y")]));
    assert_rejected_without_leftovers("a second archive after the first", &concatenated);
}

#[test]
fn output_limit_is_enforced_before_writing() {
    let bytes = newc(&[file("a", &[1; 3000]), file("b", &[2; 3000])]);
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
fn selection_strip_and_metadata_options_apply() {
    let bytes = newc(&sample());
    let paths = ["folder".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            paths: &paths,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    let expected: Vec<String> = sample_tree()
        .into_keys()
        .filter(|name| name.starts_with("folder"))
        .collect();
    assert_eq!(tree(&destination).into_keys().collect::<Vec<_>>(), expected);
    let exclude = ["*.txt".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            exclude: &exclude,
            strip: Strip::Prefix("folder"),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    let expected: Vec<String> = sample_tree()
        .into_keys()
        .filter_map(|name| name.strip_prefix("folder/").map(str::to_owned))
        .filter(|name| !name.ends_with(".txt"))
        .collect();
    assert_eq!(tree(&destination).into_keys().collect::<Vec<_>>(), expected);
    let missing = ["nothing".to_owned()];
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            paths: &missing,
            ..ReadOptions::default()
        },
    );
    assert!(result.is_err());
    assert!(!destination.exists());
    let hard = newc(&[linked("t", b"", 3, 2), linked("h", b"x", 3, 2)]);
    let only = ["t".to_owned()];
    let (_root, destination, result) = extract_with(
        &hard,
        ReadOptions {
            paths: &only,
            ..ReadOptions::default()
        },
    );
    assert!(result.is_err(), "a hard link whose target is left out");
    assert!(!destination.exists());
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            metadata: Metadata::ContentOnly,
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    let modified = fs::metadata(destination.join("big"))
        .unwrap()
        .modified()
        .unwrap();
    assert_ne!(
        modified,
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(TIME)
    );
}

#[test]
fn existing_destinations_follow_their_mode() {
    let bytes = newc(&[file("a", b"new"), file("b", b"added")]);
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.cpio");
    fs::write(&archive, &bytes).unwrap();
    let destination = root.path().join("out");
    fs::create_dir(&destination).unwrap();
    fs::write(destination.join("a"), b"old").unwrap();
    fs::write(destination.join("keep"), b"kept").unwrap();
    assert!(read_archive(&archive, Some(&destination), ReadOptions::default()).is_err());
    read_archive(
        &archive,
        Some(&destination),
        ReadOptions {
            existing: Existing::Overwrite,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(fs::read(destination.join("a")).unwrap(), b"new");
    assert_eq!(fs::read(destination.join("b")).unwrap(), b"added");
    assert_eq!(fs::read(destination.join("keep")).unwrap(), b"kept");
}

#[test]
fn progress_counts_every_file_and_cancel_publishes_nothing() {
    let bytes = newc(&sample());
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
    let files = sample_tree().values().filter(|kind| *kind != "dir").count();
    assert_eq!(last.files_done, files as u64);
    assert_eq!(last.bytes_done, 6 + 1 + 3 + 70_000);
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
fn cpio_goes_to_this_reader() {
    let root = tempfile::tempdir().unwrap();
    for form in FORMS {
        let archive = root.path().join(format!("{form:?}.cpio"));
        fs::write(&archive, cpio(form, &sample())).unwrap();
        assert_eq!(
            zipper::choose_reader(&archive).unwrap(),
            zipper::Reader::Cpio,
            "{form:?}"
        );
    }
}

#[test]
fn hard_links_waiting_for_their_data_are_charged_to_a_memory_budget() {
    let waiting: Vec<Member> = (0..600_000u64)
        .map(|index| linked(&format!("{index:06}"), b"", index + 10, 2))
        .collect();
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("waiting.cpio");
    fs::write(&archive, newc(&waiting)).unwrap();
    let error = format!(
        "{:#}",
        read_archive(&archive, None, ReadOptions::default()).unwrap_err()
    );
    assert!(error.contains("hard links"), "{error}");
}

#[test]
fn files_whose_other_links_are_outside_the_archive_extract() {
    let snapshot: Vec<Member> = (0..100_001u64)
        .map(|index| linked(&format!("{index:06}"), b"content", index + 10, 2))
        .collect();
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("snapshot.cpio");
    fs::write(&archive, newc(&snapshot)).unwrap();
    let entries = read_archive(&archive, None, ReadOptions::default()).unwrap();
    assert_eq!(entries.len(), 100_001);
}

#[test]
fn a_name_after_its_group_is_complete_waits_for_new_data_or_stays_empty() {
    for (case, members, linked_names, empty_names) in [
        (
            "low",
            vec![
                linked("a", b"", 5, 2),
                linked("b", b"BBBB", 5, 2),
                linked("c", b"", 5, 2),
            ],
            &["a", "b"][..],
            &["c"][..],
        ),
        (
            "two after the data",
            vec![
                linked("a", b"AAAA", 5, 2),
                linked("b", b"", 5, 2),
                linked("c", b"", 5, 2),
            ],
            &["a", "b"],
            &["c"],
        ),
        (
            "second data completes the group",
            vec![
                linked("a", b"AAAA", 5, 2),
                linked("b", b"BBBB", 5, 2),
                linked("c", b"", 5, 2),
            ],
            &["a"],
            &["c"],
        ),
        (
            "new data after the group",
            vec![
                linked("a", b"AAAA", 5, 2),
                linked("b", b"", 5, 2),
                linked("c", b"", 5, 2),
                linked("d", b"DDDD", 5, 2),
            ],
            &["a", "b"],
            &[],
        ),
    ] {
        let (_root, destination, result) = extract_with(&newc(&members), ReadOptions::default());
        result.unwrap_or_else(|error| panic!("{case}: {error:#}"));
        let data = fs::read(destination.join(linked_names[0])).unwrap();
        for name in linked_names {
            assert_eq!(
                fs::read(destination.join(name)).unwrap(),
                data,
                "{case} {name}"
            );
        }
        for name in empty_names {
            assert_eq!(
                fs::read(destination.join(name)).unwrap(),
                b"",
                "{case} {name}"
            );
        }
    }
}

#[test]
fn a_data_name_that_links_may_still_wait_on_is_never_replaced() {
    for (case, members) in [
        (
            "replaced after a wait",
            vec![
                linked("x", b"GOOD", 5, 2),
                linked("y", b"GOOD", 5, 2),
                linked("c", b"", 5, 2),
                linked("x", b"EVIL", 6, 1),
            ],
        ),
        (
            "replaced before its link",
            vec![
                linked("a", b"AA", 5, 2),
                linked("a", b"ZZ", 7, 1),
                linked("b", b"", 5, 2),
            ],
        ),
    ] {
        assert_rejected_saying(case, &newc(&members), "an earlier hard link names");
    }
}

#[test]
fn a_waiting_name_is_checked_when_it_arrives() {
    let bytes = record(Form::Newc, &linked("../escape", b"", 5, 2), 0);
    let error = format!(
        "{:#}",
        extract_with(&bytes, ReadOptions::default()).2.unwrap_err()
    );
    assert!(error.contains("unsafe archive path"), "{error}");
}

#[test]
fn cancel_while_links_are_created_publishes_nothing() {
    let mut members = vec![linked("data", b"shared", 9, 3001)];
    members.extend((0..3000).map(|index| linked(&format!("link{index:04}"), b"", 9, 3001)));
    let finishing = std::sync::atomic::AtomicUsize::new(0);
    let cancel = AtomicBool::new(false);
    let cancel_on_second_finishing_report = |progress: zipper::Progress| {
        if progress.phase == zipper::Phase::Finishing
            && finishing.fetch_add(1, Ordering::Relaxed) == 1
        {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let (root, destination, result) = extract_with(
        &newc(&members),
        ReadOptions {
            control: Control {
                progress: Some(&cancel_on_second_finishing_report),
                cancel: Some(&cancel),
            },
            ..ReadOptions::default()
        },
    );
    assert!(result.unwrap_err().is::<zipper::Cancelled>());
    assert!(!destination.exists());
    let left: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name != "case.cpio")
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_name_that_joins_a_group_with_data_counts_once_toward_the_name_limit() {
    let mut members = vec![linked("data", b"x", 9, 9_001)];
    members.extend(
        (0..9_000).map(|index| linked(&format!("{index:05}{}", "n".repeat(4000)), b"", 9, 9_001)),
    );
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("first.cpio");
    fs::write(&archive, newc(&members)).unwrap();
    read_archive(&archive, None, ReadOptions::default()).unwrap();
}

#[test]
fn a_data_name_that_lands_elsewhere_under_a_strip_may_be_reused_by_another_entry() {
    let bytes = newc(&[
        linked("./t/a", b"AA", 5, 2),
        linked("t/a", b"ZZ", 6, 1),
        linked("t/b", b"", 5, 2),
    ]);
    let (_root, destination, result) = extract_with(
        &bytes,
        ReadOptions {
            strip: Strip::Components(1),
            ..ReadOptions::default()
        },
    );
    result.unwrap();
    assert_eq!(fs::read(destination.join("a")).unwrap(), b"ZZ");
    assert_eq!(fs::read(destination.join("t/a")).unwrap(), b"AA");
    assert_eq!(fs::read(destination.join("b")).unwrap(), b"AA");
    let same_landing = newc(&[
        linked("./t/a", b"AA", 5, 2),
        linked("./t/a", b"ZZ", 6, 1),
        linked("t/b", b"", 5, 2),
    ]);
    let error = format!(
        "{:#}",
        extract_with(
            &same_landing,
            ReadOptions {
                strip: Strip::Components(1),
                ..ReadOptions::default()
            },
        )
        .2
        .unwrap_err()
    );
    assert!(error.contains("an earlier hard link names"), "{error}");
}
