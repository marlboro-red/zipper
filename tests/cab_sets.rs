//! A CAB set, whose folders continue from one cabinet into the next, extracts
//! in process byte for byte and exactly as `7zz x -tcab` extracts it from its
//! first cabinet. A set that is missing, cut short, misnumbered, looped or
//! inconsistent is refused with nothing left behind, and the error names the
//! cabinet. Without 7-Zip 26.05 or later the differential test is skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/cab_writer.rs"]
mod cab_writer;
#[path = "common/quantum_encoder.rs"]
mod quantum_encoder;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use cab_writer::{Cabinet, Cut, File, Folder, Method};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use zipper::native_cab::{ReadOptions, read_archive};

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }
}

fn methods() -> Vec<(&'static str, Method)> {
    let mut quantum = quantum_encoder::Options::plain(16);
    quantum.literal_odds = 5;
    quantum.shorten_odds = 20;
    vec![
        ("stored", Method::Stored),
        ("mszip", Method::Mszip),
        ("quantum", Method::Quantum(quantum)),
    ]
}

fn contents(random: &mut Random, length: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        if random.below(3) == 0 {
            bytes.push(random.next() as u8);
        } else {
            let line = format!("line {} of {}\r\n", random.below(500), random.below(9));
            bytes.extend_from_slice(line.as_bytes());
        }
    }
    bytes.truncate(length);
    bytes
}

struct Source {
    folders: Vec<Folder>,
    files: Vec<(usize, File)>,
    tree: BTreeMap<String, Vec<u8>>,
}

fn source(method: &Method, layout: &[&[usize]]) -> Source {
    let mut random =
        Random(0x5eed ^ layout.iter().flat_map(|sizes| sizes.iter()).sum::<usize>() as u64);
    let mut source = Source {
        folders: Vec::new(),
        files: Vec::new(),
        tree: BTreeMap::new(),
    };
    for (index, sizes) in layout.iter().enumerate() {
        let mut data = Vec::new();
        for (number, &size) in sizes.iter().enumerate() {
            let name = format!("folder{index}/file{number}.bin");
            let bytes = contents(&mut random, size);
            source.files.push((
                index,
                cab_writer::file(&name, 0, data.len() as u32, size as u32),
            ));
            data.extend_from_slice(&bytes);
            source
                .tree
                .insert(format!("folder{index}"), b"<dir>".to_vec());
            source.tree.insert(name, bytes);
        }
        source.folders.push(cab_writer::folder(method, &data));
    }
    source
}

fn names(count: usize) -> Vec<String> {
    (1..=count)
        .map(|number| format!("disk{number}.cab"))
        .collect()
}

fn write_set(root: &Path, cabinets: &[Cabinet], names: &[String]) -> PathBuf {
    for (cabinet, name) in cabinets.iter().zip(names) {
        fs::write(root.join(name), cabinet.build()).unwrap();
    }
    root.join(&names[0])
}

fn build_set(source: &Source, cuts: &[Cut]) -> (Vec<Cabinet>, Vec<String>) {
    let names = names(cuts.len() + 1);
    let borrowed: Vec<&str> = names.iter().map(String::as_str).collect();
    (
        cab_writer::cabinet_set(&source.folders, &source.files, cuts, &borrowed),
        names,
    )
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
                .replace('\\', "/");
            let contents = if entry.file_type().is_dir() {
                b"<dir>".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, contents)
        })
        .collect()
}

fn sevenzip_tree(first: &Path) -> Option<Option<BTreeMap<String, Vec<u8>>>> {
    let binary = sevenzip::sevenzip();
    let into = tempfile::tempdir().unwrap();
    let out = into.path().join("out");
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(&out);
    let run = std::process::Command::new(binary?)
        .args(["x", "-y", "-tcab"])
        .arg(flag)
        .arg(first)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    Some(run.status.success().then(|| tree(&out)))
}

fn zipper_tree(first: &Path) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let into = tempfile::tempdir().unwrap();
    let out = into.path().join("out");
    read_archive(first, Some(&out), ReadOptions::default())?;
    Ok(tree(&out))
}

fn cut_at(source: &Source, mut offset: usize) -> Cut {
    for (folder, contents) in source.folders.iter().enumerate() {
        for (block, packed) in contents.blocks.iter().enumerate() {
            if offset < packed.packed.len() {
                return Cut {
                    folder,
                    block,
                    at: offset,
                };
            }
            offset -= packed.packed.len();
        }
    }
    panic!("cut past the set");
}

fn packed_total(source: &Source) -> usize {
    source
        .folders
        .iter()
        .flat_map(|folder| &folder.blocks)
        .map(|block| block.packed.len())
        .sum()
}

fn leftovers(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".cab") && name != "outside")
        .collect();
    found.sort();
    found
}

fn assert_refused(case: &str, root: &Path, first: &Path, words: &[&str]) -> String {
    let destination = root.join("out");
    let error = match read_archive(first, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: extracted a hostile set"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(leftovers(root), Vec::<String>::new(), "{case}: left output");
    assert!(
        read_archive(first, None, ReadOptions::default()).is_err(),
        "{case}: verified a hostile set"
    );
    for word in words {
        assert!(
            error.contains(word),
            "{case}: {error:?} does not say {word:?}"
        );
    }
    error
}

fn two_cabinet_stored_set() -> (Source, Vec<Cabinet>, Vec<String>) {
    let source = source(&Method::Stored, &[&[40_000, 30_000]]);
    let (cabinets, names) = build_set(
        &source,
        &[Cut {
            folder: 0,
            block: 1,
            at: 100,
        }],
    );
    (source, cabinets, names)
}

#[test]
fn generated_sets_extract_byte_for_byte_and_exactly_as_7zip_extracts_them() {
    let layouts: [&[&[usize]]; 5] = [
        &[&[100_000]],
        &[&[5_000, 70_000, 3]],
        &[&[20_000], &[90_000, 10], &[0, 7_000]],
        &[&[1, 2, 3], &[150_000]],
        &[&[33_000, 33_000, 33_000]],
    ];
    let mut compared = 0;
    let mut extracted = 0;
    let mut random = Random(0x00CA_B5E7);
    for (label, method) in methods() {
        for layout in layouts {
            let source = source(&method, layout);
            let total = packed_total(&source);
            let mut plans: Vec<Vec<usize>> = vec![
                vec![total / 2],
                vec![1, total - 1],
                vec![total / 3, total / 3 + 1, 2 * total / 3],
                vec![total - 30, total - 20, total - 10],
            ];
            for _ in 0..4 {
                let mut cuts: Vec<usize> = (0..1 + random.below(4))
                    .map(|_| 1 + random.below(total - 1))
                    .collect();
                cuts.sort();
                cuts.dedup();
                plans.push(cuts);
            }
            let mut boundary = 0;
            for block in &source.folders[0].blocks[..source.folders[0].blocks.len() - 1] {
                boundary += block.packed.len();
            }
            if boundary > 0 {
                plans.push(vec![boundary]);
            }
            for plan in plans {
                let cuts: Vec<Cut> = plan.iter().map(|&offset| cut_at(&source, offset)).collect();
                let (cabinets, names) = build_set(&source, &cuts);
                let root = tempfile::tempdir().unwrap();
                let first = write_set(root.path(), &cabinets, &names);
                let case = format!("{label} {layout:?} cut at {plan:?}");
                let ours = zipper_tree(&first);
                if let Some(theirs) = sevenzip_tree(&first) {
                    assert_eq!(
                        ours.as_ref().ok(),
                        theirs.as_ref(),
                        "{case}: Zipper ({:?}) and 7-Zip disagree",
                        ours.as_ref().err()
                    );
                    compared += 1;
                    extracted += usize::from(theirs.is_some());
                }
                if let Ok(ours) = ours {
                    assert_eq!(ours, source.tree, "{case}");
                }
            }
        }
    }
    eprintln!("compared {compared} sets with 7-Zip, of which it extracted {extracted}");
}

#[test]
fn a_set_spanning_every_method_extracts_and_a_selected_spanning_file_extracts_alone() {
    for (label, method) in methods() {
        let source = source(&method, &[&[10_000], &[60_000, 50_000], &[500]]);
        let total = packed_total(&source);
        let cuts = [cut_at(&source, total / 4), cut_at(&source, total / 2 + 3)];
        let (cabinets, names) = build_set(&source, &cuts);
        assert!(
            cabinets
                .iter()
                .flat_map(|cabinet| &cabinet.files)
                .any(|file| file.folder >= cab_writer::FROM_PREVIOUS),
            "{label}: no file continues"
        );
        let root = tempfile::tempdir().unwrap();
        let first = write_set(root.path(), &cabinets, &names);
        assert_eq!(
            zipper_tree(&first).unwrap_or_else(|error| panic!("{label}: {error:#}")),
            source.tree,
            "{label}"
        );
        assert_eq!(
            zipper::choose_reader(&first).unwrap(),
            zipper::Reader::Cab,
            "{label}"
        );
        let selected = ["folder1/file1.bin".to_owned()];
        let out = root.path().join("selected");
        read_archive(
            &first,
            Some(&out),
            ReadOptions {
                paths: &selected,
                ..ReadOptions::default()
            },
        )
        .unwrap();
        let mut wanted = BTreeMap::new();
        wanted.insert("folder1".to_owned(), b"<dir>".to_vec());
        wanted.insert(selected[0].clone(), source.tree[&selected[0]].clone());
        assert_eq!(tree(&out), wanted, "{label}");
    }
}

#[test]
fn a_missing_or_cut_cabinet_fails_before_publishing_and_is_named() {
    let source = source(&Method::Mszip, &[&[50_000, 50_000, 50_000]]);
    let total = packed_total(&source);
    let cuts = [cut_at(&source, total / 3), cut_at(&source, 2 * total / 3)];
    let (cabinets, names) = build_set(&source, &cuts);
    let root = tempfile::tempdir().unwrap();
    let first = write_set(root.path(), &cabinets, &names);
    fs::remove_file(root.path().join(&names[1])).unwrap();
    assert_refused("missing middle", root.path(), &first, &["disk2.cab"]);
    let middle = cabinets[1].build();
    let last = cabinets[2].build();
    for (name, bytes) in [(&names[1], &middle), (&names[2], &last)] {
        write_set(root.path(), &cabinets, &names);
        for length in (0..bytes.len()).step_by(997).chain([bytes.len() - 1]) {
            fs::write(root.path().join(name), &bytes[..length]).unwrap();
            assert_refused(
                &format!("{name} cut at {length}"),
                root.path(),
                &first,
                &[name],
            );
        }
    }
    write_set(root.path(), &cabinets, &names);
    fs::remove_file(root.path().join(&names[2])).unwrap();
    assert_refused("missing last", root.path(), &first, &["disk3.cab"]);
    write_set(root.path(), &cabinets, &names);
    let mut damaged = middle.clone();
    let last_byte = damaged.len() - 1;
    damaged[last_byte] ^= 0x40;
    fs::write(root.path().join(&names[1]), &damaged).unwrap();
    assert_refused(
        "damaged middle data",
        root.path(),
        &first,
        &["in a folder of cabinet disk2.cab", "checksum"],
    );
}

#[test]
fn sets_that_are_misnumbered_looped_or_from_another_set_are_refused() {
    let (_, cabinets, names) = two_cabinet_stored_set();
    let cases: Vec<(&str, Vec<Cabinet>, Vec<&str>)> = vec![
        (
            "another set ID",
            {
                let mut set = cabinets.clone();
                set[1].set = Some((cab_writer::SET_ID + 1, 1));
                set
            },
            vec!["disk2.cab", "set"],
        ),
        (
            "skipped number",
            {
                let mut set = cabinets.clone();
                set[1].set = Some((cab_writer::SET_ID, 2));
                set
            },
            vec!["disk2.cab", "numbered"],
        ),
        (
            "names itself",
            {
                let mut set = cabinets.clone();
                set[0].next = Some(("disk1.cab".into(), "disk 1".into()));
                set
            },
            vec!["disk1.cab"],
        ),
        (
            "loop back to the first",
            {
                let mut set = cabinets.clone();
                set[1].next = Some(("disk1.cab".into(), "disk 1".into()));
                set
            },
            vec!["disk1.cab"],
        ),
        (
            "next cabinet without a previous name",
            {
                let mut set = cabinets.clone();
                set[1].previous = None;
                set
            },
            vec!["disk2.cab"],
        ),
    ];
    for (case, set, words) in cases {
        let root = tempfile::tempdir().unwrap();
        let first = write_set(root.path(), &set, &names);
        assert_refused(case, root.path(), &first, &words);
        if matches!(case, "another set ID" | "skipped number") {
            assert!(
                !matches!(sevenzip_tree(&first), Some(Some(_))),
                "{case}: 7-Zip extracted it"
            );
        }
    }
}

#[test]
fn a_set_opened_at_a_later_cabinet_names_its_previous_cabinet() {
    let (_, cabinets, names) = two_cabinet_stored_set();
    let root = tempfile::tempdir().unwrap();
    write_set(root.path(), &cabinets, &names);
    assert_refused(
        "second cabinet",
        root.path(),
        &root.path().join(&names[1]),
        &["disk1.cab", "first cabinet"],
    );
}

#[test]
fn continued_folders_whose_halves_disagree_are_refused() {
    let (_, cabinets, names) = two_cabinet_stored_set();
    let continued = |cabinet: &Cabinet, marker: u16| {
        cabinet
            .files
            .iter()
            .position(|file| file.folder == marker)
            .unwrap()
    };
    let mszip = cab_writer::folder(&Method::Mszip, b"other half").type_compress;
    let cases: Vec<(&str, Vec<Cabinet>, &str)> = vec![
        (
            "other method",
            {
                let mut set = cabinets.clone();
                set[1].folders[0].type_compress = mszip;
                set
            },
            "method",
        ),
        (
            "only the first half continues",
            {
                let mut set = cabinets.clone();
                for file in &mut set[1].files {
                    file.folder = 0;
                }
                set
            },
            "continue",
        ),
        (
            "only the second half continues",
            {
                let mut set = cabinets.clone();
                for file in &mut set[0].files {
                    file.folder = 0;
                }
                set
            },
            "continue",
        ),
        (
            "halves give the file other sizes",
            {
                let mut set = cabinets.clone();
                let at = continued(&set[1], cab_writer::FROM_PREVIOUS);
                set[1].files[at].size -= 1;
                set
            },
            "share bytes",
        ),
        (
            "a split block ends a folder that does not continue",
            {
                let mut set = cabinets.clone();
                for file in &mut set[0].files {
                    file.folder = 0;
                }
                set[0].files.retain(|file| file.offset == 0);
                set[0].next = None;
                set
            },
            "continues",
        ),
        (
            "both markers in a cabinet of two folders",
            {
                let mut set = cabinets.clone();
                let at = continued(&set[1], cab_writer::FROM_PREVIOUS);
                set[1].files[at].folder = cab_writer::PREVIOUS_AND_NEXT;
                set[1]
                    .folders
                    .push(cab_writer::folder(&Method::Stored, b"x"));
                set
            },
            "folder",
        ),
        (
            "continued from before the first cabinet",
            {
                let mut set = cabinets.clone();
                set[0].files[0].folder = cab_writer::FROM_PREVIOUS;
                set
            },
            "previous",
        ),
        (
            "continued past the last cabinet",
            {
                let mut set = cabinets.clone();
                let at = continued(&set[1], cab_writer::FROM_PREVIOUS);
                set[1].files[at].folder = cab_writer::PREVIOUS_AND_NEXT;
                set
            },
            "next",
        ),
    ];
    for (case, set, words) in cases {
        let root = tempfile::tempdir().unwrap();
        let first = write_set(root.path(), &set, &names);
        assert_refused(case, root.path(), &first, &[words]);
    }
}

#[test]
fn windows_device_names_are_refused_as_cabinet_names_on_every_system() {
    let (source, cabinets, _) = two_cabinet_stored_set();
    let devices = [
        "CON",
        "con",
        "Prn",
        "aux",
        "NUL",
        "nul.cab",
        "con.cab",
        "COM1.txt",
        "com0",
        "COM9",
        "lpt0.cab",
        "LPT9",
        "COM¹",
        "com².cab",
        "LPT³",
        "CONIN$",
        "conout$.cab",
        "con.",
        "CON ",
        "nul . ",
        "aux .cab",
        "COM1.cab.",
        "lpt1...",
    ];
    for name in devices {
        for previous in [false, true] {
            let mut cabinet = cabinets[usize::from(previous)].clone();
            let names = Some((name.to_owned(), "disk".to_owned()));
            if previous {
                cabinet.previous = names;
            } else {
                cabinet.next = names;
            }
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("case.cab");
            fs::write(&path, cabinet.build()).unwrap();
            assert_refused(
                &format!("{name:?} as previous {previous}"),
                root.path(),
                &path,
                &["not a plain file name"],
            );
        }
    }
    for name in [
        "CONSOLE.cab",
        "COM10.cab",
        "com.cab",
        "LPT.cab",
        "aux1.cab",
        "nul-1.cab",
        "xcon.cab",
        "COM1x.cab",
        "lpt¹¹.cab",
    ] {
        let mut set = cabinets.clone();
        set[0].next = Some((name.to_owned(), "disk 2".into()));
        let root = tempfile::tempdir().unwrap();
        let first = write_set(
            root.path(),
            &set,
            &["disk1.cab".to_owned(), name.to_owned()],
        );
        assert_eq!(
            zipper_tree(&first).unwrap_or_else(|error| panic!("{name:?}: {error:#}")),
            source.tree,
            "{name:?}"
        );
    }
}

#[test]
fn cabinet_names_that_leave_the_folder_are_refused_before_anything_beside_is_opened() {
    let (_, cabinets, _) = two_cabinet_stored_set();
    for name in [
        "../outside.cab",
        "..\\outside.cab",
        "sub/outside.cab",
        "C:outside.cab",
        "/tmp/outside.cab",
        "..",
        ".",
    ] {
        let root = tempfile::tempdir().unwrap();
        let inner = root.path().join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(root.path().join("outside.cab"), cabinets[1].build()).unwrap();
        let mut first = cabinets[0].clone();
        first.next = Some((name.to_owned(), "disk 2".to_owned()));
        let path = inner.join("disk1.cab");
        fs::write(&path, first.build()).unwrap();
        assert_refused(name, &inner, &path, &["not a plain file name"]);
    }
}

#[test]
fn the_cabinet_names_of_a_set_count_toward_the_64_mib_of_names() {
    let count = 64 * 1024 * 1024 / 8000 + 2;
    let names: Vec<String> = (0..count).map(|index| format!("c{index}.cab")).collect();
    let root = tempfile::tempdir().unwrap();
    for index in 0..count {
        let mut cabinet = Cabinet {
            checksums: true,
            set: Some((cab_writer::SET_ID, index as u16)),
            ..Cabinet::default()
        };
        if index > 0 {
            cabinet.previous = Some(("p".repeat(8000), "d".into()));
        }
        if index + 1 < count {
            cabinet.next = Some((names[index + 1].clone(), "d".into()));
        } else {
            cabinet
                .folders
                .push(cab_writer::folder(&Method::Stored, b"last"));
            cabinet.files.push(cab_writer::file("last.txt", 0, 0, 4));
        }
        fs::write(root.path().join(&names[index]), cabinet.build()).unwrap();
    }
    assert_refused(
        "long previous names",
        root.path(),
        &root.path().join(&names[0]),
        &["64 MiB"],
    );
}

#[test]
fn a_set_holds_at_most_65535_file_records_in_all() {
    let empty = |count: u32, number: u16| {
        let mut cabinet = Cabinet {
            checksums: true,
            set: Some((cab_writer::SET_ID, number)),
            ..Cabinet::default()
        };
        cabinet
            .folders
            .push(cab_writer::folder(&Method::Stored, b""));
        for index in 0..count {
            cabinet
                .files
                .push(cab_writer::file(&format!("c{number}f{index}"), 0, 0, 0));
        }
        cabinet
    };
    let names = names(2);
    let mut set = vec![empty(40_000, 0), empty(25_535, 1)];
    set[0].next = Some((names[1].clone(), "disk 2".into()));
    set[1].previous = Some((names[0].clone(), "disk 1".into()));
    let root = tempfile::tempdir().unwrap();
    let first = write_set(root.path(), &set, &names);
    assert_eq!(
        read_archive(&first, None, ReadOptions::default())
            .unwrap()
            .len(),
        65_535
    );
    set[1].files.push(cab_writer::file("one more", 0, 0, 0));
    write_set(root.path(), &set, &names);
    assert_refused("65,536 records", root.path(), &first, &["65535"]);
}

#[test]
fn a_later_folder_that_reuses_the_blocks_of_a_continued_folder_is_refused() {
    let (_, mut cabinets, names) = two_cabinet_stored_set();
    cabinets[1]
        .folders
        .push(cab_writer::folder(&Method::Stored, &[7; 100]));
    cabinets[1]
        .files
        .push(cab_writer::file("reused.bin", 1, 0, 100));
    let mut second = cabinets[1].build();
    let files_at = u32::from_le_bytes(second[16..20].try_into().unwrap()) as usize;
    let folders_at = files_at - 2 * 8;
    let continued_offset = second[folders_at..folders_at + 4].to_vec();
    second[folders_at + 8..folders_at + 12].copy_from_slice(&continued_offset);
    let root = tempfile::tempdir().unwrap();
    let first = write_set(root.path(), &cabinets, &names);
    fs::write(root.path().join(&names[1]), &second).unwrap();
    assert_refused("reused blocks", root.path(), &first, &["share data blocks"]);
}

#[test]
fn a_block_split_inside_one_cabinet_is_refused() {
    let data = contents(&mut Random(7), 50_000);
    let mut cabinet = cab_writer::one_folder_in_order(&[("one.bin", &data)], &Method::Stored);
    let whole = cabinet.folders[0].blocks.remove(0);
    cabinet.folders[0].blocks.splice(
        0..0,
        [
            cab_writer::Block {
                packed: whole.packed[..100].to_vec(),
                size: 0,
            },
            cab_writer::Block {
                packed: whole.packed[100..].to_vec(),
                size: whole.size,
            },
        ],
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("one.cab");
    fs::write(&path, cabinet.build()).unwrap();
    assert_refused(
        "split inside",
        root.path(),
        &path,
        &["does not continue there"],
    );
    if let Some(theirs) = sevenzip_tree(&path) {
        eprintln!(
            "7-Zip extracts a block split inside one cabinet: {}",
            theirs.is_some()
        );
    }
}

fn source_from(folder: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = walkdir::WalkDir::new(folder)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            let name = entry.path().strip_prefix(folder).unwrap();
            (
                name.to_string_lossy().replace('\\', "/"),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

#[test]
#[ignore = "writes the parity corpus cabinet set; scripts/parity.py corpus runs it"]
fn write_the_parity_set() {
    let source = PathBuf::from(std::env::var_os("ZIPPER_CAB_SET_SOURCE").expect("source folder"));
    let out = PathBuf::from(std::env::var_os("ZIPPER_CAB_SET_OUT").expect("output folder"));
    let files = source_from(&source);
    let borrowed: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let cabinet = cab_writer::one_folder_in_order(&borrowed, &Method::Mszip);
    let records: Vec<(usize, File)> = cabinet.files.iter().map(|file| (0, file.clone())).collect();
    let packed = cabinet.folders[0].blocks[0].packed.len();
    let cuts = [packed / 3, 2 * packed / 3].map(|at| Cut {
        folder: 0,
        block: 0,
        at,
    });
    let names = ["cab-set-1.cab", "cab-set-2.cab", "cab-set-3.cab"];
    for (cabinet, name) in cab_writer::cabinet_set(&cabinet.folders, &records, &cuts, &names)
        .iter()
        .zip(names)
    {
        fs::write(out.join(name), cabinet.build()).unwrap();
    }
}

#[test]
#[ignore = "writes cabinets whose names fill the limits, for a peak memory check"]
fn write_the_limit_cabinets() {
    let out = PathBuf::from(std::env::var_os("ZIPPER_CAB_LIMIT_OUT").expect("output folder"));
    let record = |cabinet: &mut Cabinet, index: u32| {
        let name = format!("{index:05}{}", "x".repeat(995));
        cabinet.files.push(cab_writer::file(&name, 0, 0, 0));
    };
    let empty_folder = || cab_writer::folder(&Method::Stored, b"");
    let mut alone = Cabinet {
        checksums: true,
        folders: vec![empty_folder()],
        ..Cabinet::default()
    };
    for index in 0..u32::from(u16::MAX) {
        record(&mut alone, index);
    }
    fs::write(out.join("limit-alone.cab"), alone.build()).unwrap();
    let names = ["limit-set-1.cab", "limit-set-2.cab"];
    let mut set: Vec<Cabinet> = (0..2u16)
        .map(|number| Cabinet {
            checksums: true,
            folders: vec![empty_folder()],
            set: Some((cab_writer::SET_ID, number)),
            ..Cabinet::default()
        })
        .collect();
    for index in 0..u32::from(u16::MAX) {
        record(&mut set[(index % 2) as usize], index);
    }
    set[0].next = Some((names[1].to_owned(), "d".repeat(8000)));
    set[1].previous = Some((names[0].to_owned(), "d".repeat(8000)));
    for (cabinet, name) in set.iter().zip(names) {
        fs::write(out.join(name), cabinet.build()).unwrap();
    }
}
