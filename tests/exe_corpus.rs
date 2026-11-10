//! Every executable in `tests/data/exe-corpus` extracts to the tree 7-Zip
//! extracted when the corpus was written, and to the tree, modes and times
//! 7-Zip extracts now where it is installed. PE images with resources and
//! sections, COFF objects, TE images, ELF files of both classes and byte
//! orders, Mach-O files and Mach-O universal files from the test writer must
//! do the same, and every section, segment and slice must hold the bytes the
//! writer put there. The default engine must send each corpus file here.
#[path = "common/mod.rs"]
mod common;
#[path = "common/exe_compare.rs"]
mod exe_compare;
#[path = "common/exe_writer.rs"]
mod exe_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use exe_compare::{Random, compare_with_7zip, sevenzip, sevenzip_tree, sha256, zipper_tree};
use exe_writer::{ElfSection, FixedInfo, Key, MachSection, PeOptions, PeSection, Resources};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/exe-corpus");

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            !name.ends_with(".py") && !name.ends_with(".txt") && !name.ends_with(".json")
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_corpus_executable_extracts_the_tree_7zip_extracts() {
    let recorded: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let files = corpus_files();
    assert_eq!(
        files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        recorded.keys().cloned().collect::<Vec<_>>(),
        "trees.json names exactly the corpus files"
    );
    let binary = sevenzip();
    for archive in &files {
        let name = archive.file_name().unwrap().to_string_lossy().into_owned();
        let root = tempfile::tempdir().unwrap();
        let ours = zipper_tree(archive, &root.path().join("plain"), false).unwrap();
        assert_eq!(
            ours, recorded[&name],
            "{name} differs from the recorded tree"
        );
        let listed = zipper::native_exe::read_archive(archive, None, Default::default()).unwrap();
        assert_eq!(
            listed.iter().filter(|entry| !entry.directory).count(),
            ours.values().filter(|kind| *kind != "dir").count(),
            "{name} lists every file it writes"
        );
        assert_eq!(
            zipper::choose_reader(zipper::Engine::Auto, archive).unwrap(),
            zipper::Reader::Exe,
            "{name} goes to the executable reader"
        );
        if let Some(binary) = &binary {
            let ours = zipper_tree(archive, &root.path().join("zipper"), true).unwrap();
            let theirs = sevenzip_tree(binary, archive, &root.path().join("7zip"), true)
                .unwrap_or_else(|| panic!("7-Zip refuses {name}"));
            assert_eq!(ours, theirs, "{name} differs from 7-Zip");
        }
        common::make_tree_deletable(root.path());
    }
}

fn pick<'a, T>(random: &mut Random, from: &'a [T]) -> &'a T {
    &from[random.below(from.len())]
}

fn text(random: &mut Random) -> String {
    pick(
        random,
        &[
            "v",
            "",
            "a \"q\"",
            "line\nbreak",
            "tab\there",
            "é中",
            "xxxxxxxxxxxxxxxxxxxxxxxxx",
            "\\",
        ],
    )
    .to_string()
}

fn version(random: &mut Random) -> Vec<u8> {
    let fixed = FixedInfo {
        file_version: [random.next() as u16, 2, 3, 4],
        product_version: [5, 6, 7, 8],
        flags_mask: *pick(random, &[0x3F, 0, 0xFFFF]),
        flags: *pick(random, &[0, 1, 0x21, 0x40, 0x3F, 0x41]),
        os: *pick(
            random,
            &[
                0, 1, 4, 5, 0x1_0001, 0x4_0004, 0x4_0000, 0x4_0005, 0x6_0004, 0x6_0000, 0x1_0003,
            ],
        ),
        kind: *pick(random, &[0, 1, 2, 3, 4, 5, 6, 7, 8]),
        subtype: *pick(random, &[0, 1, 3, 12, 13]),
    };
    let tables: Vec<(String, Vec<(String, String)>)> = (0..random.below(3))
        .map(|_| {
            let language = pick(random, &["040904b0", "000004e4", ""]).to_string();
            let pairs = (0..random.below(4))
                .map(|_| {
                    (
                        pick(random, &["CompanyName", "k", "", "abcdefghijklmnopqrst"]).to_string(),
                        text(random),
                    )
                })
                .collect();
            (language, pairs)
        })
        .collect();
    let borrowed: Vec<(&str, Vec<(&str, &str)>)> = tables
        .iter()
        .map(|(language, pairs)| {
            (
                language.as_str(),
                pairs
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect(),
            )
        })
        .collect();
    let translations: Vec<(u16, u16)> = (0..random.below(3))
        .map(|_| {
            (
                *pick(random, &[0x409, 0x407, 0, 0xFFFF]),
                *pick(random, &[1200, 1252, 0]),
            )
        })
        .collect();
    exe_writer::version_info(fixed, &borrowed, &translations)
}

fn image_resource(random: &mut Random) -> Vec<u8> {
    let bits: u16 = *pick(random, &[1, 4, 8, 16, 24, 32]);
    let width: i32 = *pick(random, &[1, 2, 16, 32, 256, 300]);
    let height = *pick(random, &[width, 2 * width, -2 * width, 2 * width + 1]);
    let mut bytes = Vec::new();
    for value in [40u32, width as u32, height as u32] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&bits.to_le_bytes());
    let compression: u32 = *pick(random, &[0, 0, 0, 1, 3]);
    bytes.extend_from_slice(&compression.to_le_bytes());
    bytes.extend_from_slice(&[0; 20]);
    let tail = random.below(600);
    bytes.extend(random.bytes(tail));
    bytes
}

fn resources(random: &mut Random) -> Resources {
    let mut resources = Resources::new();
    for _ in 0..1 + random.below(6) {
        let kind = match random.below(13) {
            0 => Key::Name("NAMED".into()),
            index => Key::Id([1, 2, 3, 4, 6, 6, 10, 14, 16, 16, 24, 300][index - 1]),
        };
        let name = match random.below(6) {
            0 => Key::Name("X".into()),
            1 => Key::Name("name".into()),
            index => Key::Id([1, 2, 7, 100][index - 2]),
        };
        let language = match random.below(5) {
            0 => Key::Name("L".into()),
            index => Key::Id([1033, 1033, 1031, 0][index - 1]),
        };
        let data = match kind {
            Key::Id(6) => {
                let strings: Vec<String> = (0..16)
                    .map(|_| pick(random, &["", "a", "tab\t", "nl\n", "q\"", "中"]).to_string())
                    .collect();
                let strings: [&str; 16] = std::array::from_fn(|index| strings[index].as_str());
                exe_writer::string_block(&strings)
            }
            Key::Id(16) => version(random),
            Key::Id(2 | 3) => image_resource(random),
            _ => {
                let length = random.below(40);
                random.bytes(length)
            }
        };
        resources
            .entry(kind)
            .or_default()
            .entry(name)
            .or_default()
            .insert(language, data);
    }
    resources
}

fn generated_pe(random: &mut Random) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let mut expected = Vec::new();
    let sections: Vec<PeSection> = (0..1 + random.below(4))
        .map(|index| {
            let name = format!(".s{index}");
            let length = random.below(300);
            let data = random.bytes(length);
            expected.push((name.clone(), data.clone()));
            PeSection {
                name: exe_writer::section_name(&name),
                data,
                virtual_size: None,
            }
        })
        .collect();
    let options = PeOptions {
        sections,
        resources: (random.below(10) < 7).then(|| resources(random)),
        wide: random.below(2) == 0,
        machine: *pick(random, &[0x8664, 0x14C, 0xAA64]),
        timestamp: *pick(random, &[0, 1_600_000_000]),
        certificate: (random.below(5) == 0).then(|| random.bytes(24)),
    };
    (exe_writer::pe(&options), expected)
}

fn elf_name(random: &mut Random) -> Vec<u8> {
    pick(
        random,
        &[
            &b".text"[..],
            b".data",
            b"",
            b".x\x01",
            b"\xc3\xa9t",
            b".bss",
        ],
    )
    .to_vec()
}

fn generated_elf(random: &mut Random) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let wide = random.below(2) == 0;
    let big = random.below(3) == 0;
    if random.below(4) == 0 {
        let segments: Vec<(u32, Vec<u8>)> = (0..1 + random.below(3))
            .map(|_| {
                let length = random.below(60);
                (*pick(random, &[1, 1, 2, 4, 6, 0, 7]), random.bytes(length))
            })
            .collect();
        let mut expected = Vec::new();
        for (kind, data) in &segments {
            if *kind != 6 {
                expected.push((expected.len().to_string(), data.clone()));
            }
        }
        return (exe_writer::elf(None, &segments, wide, big), expected);
    }
    let sections: Vec<ElfSection> = (0..random.below(6))
        .map(|index| {
            let length = random.below(60);
            ElfSection {
                name: if index % 2 == 0 {
                    format!(".s{index}").into_bytes()
                } else {
                    elf_name(random)
                },
                kind: *pick(random, &[1, 1, 2, 3, 8, 7]),
                data: random.bytes(length),
            }
        })
        .collect();
    let expected = sections
        .iter()
        .filter(|section| section.kind != 8 && section.name.starts_with(b".s"))
        .map(|section| {
            (
                String::from_utf8(section.name.clone()).unwrap(),
                section.data.clone(),
            )
        })
        .collect();
    (exe_writer::elf(Some(&sections), &[], wide, big), expected)
}

fn generated_macho(random: &mut Random) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let wide = random.below(4) != 0;
    let big = random.below(4) == 0;
    let mut expected = Vec::new();
    let segments: Vec<(&str, Vec<MachSection>)> = (0..1 + random.below(3))
        .map(|index| {
            let segment = ["__TEXT", "__DATA", "__LINKEDIT"][index];
            let sections: Vec<MachSection> = (0..random.below(4))
                .map(|number| {
                    let length = random.below(60);
                    let flags = *pick(random, &[0, 0, 1, 0x8000_0400]);
                    let section = MachSection {
                        name: format!("__s{number}"),
                        flags,
                        data: random.bytes(length),
                    };
                    expected.push((
                        format!("{segment}__s{number}"),
                        if flags == 1 {
                            Vec::new()
                        } else {
                            section.data.clone()
                        },
                    ));
                    section
                })
                .collect();
            (segment, sections)
        })
        .collect();
    let symbols = (random.below(3) == 0).then(|| (1 + random.below(3) as u32, random.bytes(9)));
    (exe_writer::macho(&segments, symbols, wide, big), expected)
}

fn generated_universal(random: &mut Random) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let mut expected = Vec::new();
    let mut used = Vec::new();
    let slices: Vec<(u32, u32, Vec<u8>)> = (0..1 + random.below(3))
        .map(|_| {
            let (cpu, subtype, name) = *pick(
                random,
                &[
                    (7, 3, "x86"),
                    (0x0100_0007, 3, "x64"),
                    (0x0100_000C, 0, "arm64"),
                    (0x0100_000C, 0x8000_0002, "arm64e"),
                    (12, 9, "armv7"),
                    (18, 0, "ppc"),
                    (99, 1, "cpu99_r1"),
                ],
            );
            let length = 1 + random.below(300);
            let data = random.bytes(length);
            if !used.contains(&name) {
                used.push(name);
            }
            expected.retain(|(earlier, _): &(String, Vec<u8>)| *earlier != format!("case.{name}"));
            expected.push((format!("case.{name}"), data.clone()));
            (cpu, subtype, data)
        })
        .collect();
    (exe_writer::universal(&slices), expected)
}

fn check_expected(label: &str, root: &Path, expected: &[(String, Vec<u8>)]) {
    for (name, data) in expected {
        let found = fs::read(root.join(name)).unwrap_or_else(|_| panic!("{label}: no {name}"));
        assert_eq!(
            sha256(&found),
            sha256(data),
            "{label}: {name} differs from its source"
        );
    }
}

#[test]
fn generated_executables_extract_byte_for_byte_and_exactly_as_7zip_extracts_them() {
    let binary = sevenzip();
    let mut random = Random(0xE8E0_0001);
    for case in 0..240 {
        let (bytes, expected, name) = match case % 6 {
            0 | 1 => {
                let (bytes, expected) = generated_pe(&mut random);
                (bytes, expected, "case.exe")
            }
            2 => {
                let (bytes, expected) = generated_elf(&mut random);
                (bytes, expected, "case.elf")
            }
            3 => {
                let (bytes, expected) = generated_macho(&mut random);
                (bytes, expected, "case.macho")
            }
            4 => {
                let (bytes, expected) = generated_universal(&mut random);
                (bytes, expected, "case.mub")
            }
            _ => {
                let sections: Vec<(String, Vec<u8>)> = (0..1 + random.below(3))
                    .map(|index| {
                        let length = random.below(90);
                        (format!(".t{index}"), random.bytes(length))
                    })
                    .collect();
                let borrowed: Vec<(&str, Vec<u8>)> = sections
                    .iter()
                    .map(|(name, data)| (name.as_str(), data.clone()))
                    .collect();
                let bytes = if random.below(2) == 0 {
                    exe_writer::te(&borrowed, *pick(&mut random, &[0x1B0, 0x200, 40]))
                } else {
                    exe_writer::coff(&borrowed, random.below(3) as u32, 1_600_000_000)
                };
                let name = if bytes.starts_with(b"VZ") {
                    "case.te"
                } else {
                    "case.obj"
                };
                (bytes, sections, name)
            }
        };
        let label = format!("case {case} ({name})");
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join(name);
        fs::write(&path, &bytes).unwrap();
        zipper_tree(&path, &work.path().join("plain"), false)
            .unwrap_or_else(|error| panic!("{label}: {error:#}"));
        check_expected(&label, &work.path().join("plain"), &expected);
        if let Some(binary) = &binary {
            compare_with_7zip(binary, &label, name, &bytes);
        }
        common::make_tree_deletable(work.path());
    }
}

#[test]
#[cfg_attr(
    not(all(feature = "zip", feature = "7z")),
    ignore = "routes to the ZIP and 7z readers"
)]
fn routing_leaves_self_extracting_archives_to_their_readers() {
    let work = tempfile::tempdir().unwrap();
    let stub = fs::read(Path::new(CORPUS).join("x64.exe")).unwrap();
    let zip = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/zip-corpus/infozip-default.zip"
    ))
    .unwrap();
    let path = work.path().join("sfx.exe");
    fs::write(&path, [&stub[..], &zip].concat()).unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &path).unwrap(),
        zipper::Reader::Zip
    );
    let seven = fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/parity/archives/7z.7z"
    ))
    .unwrap();
    fs::write(&path, [&stub[..], &seven].concat()).unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &path).unwrap(),
        zipper::Reader::SevenZip
    );
    fs::write(&path, [&stub[..], b"tail"].concat()).unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &path).unwrap(),
        zipper::Reader::Exe
    );
}

#[test]
fn stretches_no_item_covers_past_4_kib_extract_as_numbered_files() {
    let mut bytes = exe_writer::pe(&PeOptions {
        sections: vec![PeSection {
            name: exe_writer::section_name(".text"),
            data: vec![0x90; 4000],
            virtual_size: None,
        }],
        timestamp: 1_600_000_000,
        ..Default::default()
    });
    let gap_at = bytes.len();
    bytes.extend_from_slice(b"\x0e\0\0\0.eh_frame\0\0\0");
    let certificate_at = bytes.len() as u32;
    bytes.extend_from_slice(&[0xCE; 16]);
    let directory = 0x80 + 24 + 96 + 8 * 4;
    bytes[directory..directory + 4].copy_from_slice(&certificate_at.to_le_bytes());
    bytes[directory + 4..directory + 8].copy_from_slice(&16u32.to_le_bytes());
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("gap.exe");
    fs::write(&path, &bytes).unwrap();
    let ours = zipper_tree(&path, &work.path().join("plain"), false).unwrap();
    assert_eq!(ours["[0]"], sha256(&bytes[gap_at..gap_at + 16]));
    if let Some(binary) = sevenzip() {
        compare_with_7zip(&binary, "a gap before the certificate", "gap.exe", &bytes);
    }
    common::make_tree_deletable(work.path());
}
