//! Damaged and hostile executables. Byte damage in corpus files and every cut
//! of a small one must be refused exactly when 7-Zip refuses it, or for a
//! reason `docs/reference/exe-reader.md` lists as stricter, and must
//! otherwise extract the tree 7-Zip extracts. Each refusal in the reader's
//! safety checks leaves nothing behind. Header bombs stay within their
//! limits and finish quickly.
#[path = "common/mod.rs"]
mod common;
#[path = "common/exe_compare.rs"]
mod exe_compare;
#[path = "common/exe_writer.rs"]
mod exe_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use exe_compare::{Random, compare_with_7zip, sevenzip};
use exe_writer::{ElfSection, Key, PeOptions, PeSection, Resources};
use std::{fs, path::Path};
use zipper::native_exe::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/exe-corpus");

fn corpus(name: &str) -> Vec<u8> {
    fs::read(Path::new(CORPUS).join(name)).unwrap()
}

const DAMAGED: &[&str] = &[
    "x64.exe",
    "x86.exe",
    "library.dll",
    "x64.obj",
    "x64.te",
    "x86_64.elf",
    "ppc-be.elf",
    "segments-only.elf",
    "x86_64.o",
    "arm64-object.macho",
    "x86_64.macho",
    "universal.mub",
];

fn damage(random: &mut Random, bytes: &mut Vec<u8>, round: usize) -> String {
    let skip = if bytes.starts_with(b"MZ") || bytes.starts_with(b"VZ") {
        2
    } else {
        4
    };
    let head = bytes.len().min(4096);
    match round % 5 {
        0 | 1 => {
            let at = skip + random.below(head - skip);
            let bit = random.below(8);
            bytes[at] ^= 1 << bit;
            format!("bit {bit} of byte {at}")
        }
        2 => {
            let at = skip + random.below(head - skip);
            bytes[at] = [0, 0xFF, 0x7F, 0x80, 1][random.below(5)];
            format!("byte {at} set to {:#x}", bytes[at])
        }
        3 => {
            let at = skip + random.below(head.saturating_sub(skip + 4).max(1));
            let value = [0, 1, u32::MAX, 0x8000_0000, bytes.len() as u32][random.below(5)];
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            format!("word at {at} set to {value:#x}")
        }
        _ => {
            let at = random.below(bytes.len());
            bytes.truncate(at);
            format!("cut at {at}")
        }
    }
}

fn sweep(rounds: usize, seed: u64) {
    let Some(binary) = sevenzip() else { return };
    let mut random = Random(seed);
    let mut outcomes = [0usize; 3];
    for round in 0..rounds {
        let name = DAMAGED[round % DAMAGED.len()];
        let mut bytes = corpus(name);
        let what = damage(&mut random, &mut bytes, round);
        let label = format!("round {round}: {name}, {what}");
        let (accepted, stricter) = compare_with_7zip(&binary, &label, name, &bytes);
        outcomes[if accepted {
            0
        } else if stricter {
            2
        } else {
            1
        }] += 1;
    }
    assert!(
        outcomes[0] > rounds / 10 && outcomes[1] > rounds / 10,
        "accepted, refused by both and refused only by Zipper: {outcomes:?}"
    );
}

#[test]
fn damaged_executables_are_refused_exactly_when_7zip_refuses_them() {
    sweep(480, 0xDA4A_E8E0);
}

#[test]
#[ignore = "the full sweep; cargo xtask verify-sync exe-fields runs it"]
fn many_damaged_executables_are_refused_exactly_when_7zip_refuses_them() {
    sweep(12_000, 0xDA4A_E8E1);
}

fn every_cut(name: &str) {
    let Some(binary) = sevenzip() else { return };
    let bytes = corpus(name);
    for cut in 0..bytes.len() {
        compare_with_7zip(
            &binary,
            &format!("{name} cut at {cut}"),
            name,
            &bytes[..cut],
        );
    }
}

#[test]
fn every_cut_of_a_small_elf_is_judged_as_7zip_judges_it() {
    every_cut("segments-only.elf");
}

#[test]
#[ignore = "every cut of four more files; cargo xtask verify-sync exe-fields runs it"]
fn every_cut_of_small_pe_coff_te_and_elf_files_is_judged_as_7zip_judges_it() {
    for name in ["x64.obj", "i386.elf", "x64.te", "x64.exe"] {
        every_cut(name);
    }
}

fn assert_refused(bytes: &[u8], name: &str, needle: &str) {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join(name);
    fs::write(&path, bytes).unwrap();
    let out = work.path().join("out");
    let error = read_archive(&path, Some(&out), ReadOptions::default())
        .err()
        .unwrap_or_else(|| panic!("{name}: extracted"));
    let message = format!("{error:#}");
    assert!(
        message.contains(needle),
        "{name}: {needle:?} not in {message}"
    );
    assert!(!out.exists(), "{name}: left output behind");
    common::make_tree_deletable(work.path());
}

fn plain_pe() -> Vec<u8> {
    exe_writer::pe(&PeOptions {
        sections: vec![PeSection {
            name: exe_writer::section_name(".text"),
            data: vec![0xC3; 16],
            virtual_size: None,
        }],
        wide: true,
        ..Default::default()
    })
}

#[test]
fn each_refusal_leaves_nothing_behind() {
    let pe = plain_pe();
    assert_refused(
        &[&pe[..], b"tail"].concat(),
        "tail.exe",
        "holds more data after it",
    );
    assert_refused(&pe[..pe.len() - 1], "cut.exe", "past the end of the");
    let mut pointer = pe.clone();
    pointer[0x3C..0x40].copy_from_slice(&0x20u32.to_le_bytes());
    assert_refused(&pointer, "pointer.exe", "inside the MZ header");
    let mut directories = pe.clone();
    directories[0x80 + 24 + 108..0x80 + 24 + 112].copy_from_slice(&17u32.to_le_bytes());
    assert_refused(
        &directories,
        "directories.exe",
        "more than 16 data directories",
    );
    let mut name = pe.clone();
    let table = 0x80 + 24 + 240;
    name[table..table + 8].copy_from_slice(b"..\0\0\0\0\0\0");
    assert_refused(&name, "dots.exe", "unsafe archive path");
    let elf = exe_writer::elf(
        Some(&[ElfSection {
            name: b"../x".to_vec(),
            kind: 1,
            data: b"x".to_vec(),
        }]),
        &[],
        true,
        false,
    );
    assert_refused(&elf, "escape.elf", "unsafe archive path");
    let mut header = exe_writer::elf(Some(&[]), &[], true, false);
    header[58] = 63;
    assert_refused(&header, "entry.elf", "section header size");
    let mut padding = exe_writer::elf(Some(&[]), &[], true, false);
    padding[12] = 1;
    assert_refused(&padding, "padding.elf", "padding is not zero");
    let fat = exe_writer::universal(&[(7, 3, vec![1; 10]), (12, 9, vec![2; 10])]);
    let mut overlap = fat.clone();
    overlap[8 + 20 + 8..8 + 20 + 12].copy_from_slice(&4096u32.to_be_bytes());
    assert_refused(&overlap, "overlap.mub", "share bytes");
    let mut many = fat.clone();
    many[4..8].copy_from_slice(&9u32.to_be_bytes());
    assert_refused(&many, "many.mub", "more than 8");
    let mut commands = exe_writer::macho(&[("__TEXT", Vec::new())], None, true, false);
    commands[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_refused(&commands, "commands.macho", "larger than 16 MiB");
    let te = exe_writer::te(&[(".text", vec![1; 16])], 0x200);
    let mut before = te.clone();
    before[40 + 20..40 + 24].copy_from_slice(&0x1F0u32.to_le_bytes());
    assert_refused(&before, "before.te", "before the end of its section table");
}

#[test]
fn a_resource_directory_that_loops_or_shares_tables_stays_one_raw_section() {
    let mut resources = Resources::new();
    resources
        .entry(Key::Id(10))
        .or_default()
        .entry(Key::Id(1))
        .or_default()
        .insert(Key::Id(1033), b"abc".to_vec());
    let mut bytes = exe_writer::pe(&PeOptions {
        sections: vec![PeSection {
            name: exe_writer::section_name(".text"),
            data: vec![0xC3; 16],
            virtual_size: None,
        }],
        resources: Some(resources),
        wide: true,
        ..Default::default()
    });
    let rsrc = 0x400;
    bytes[rsrc + 20..rsrc + 24].copy_from_slice(&0x8000_0000u32.to_le_bytes());
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("loop.exe");
    fs::write(&path, &bytes).unwrap();
    let entries = read_archive(&path, None, ReadOptions::default()).unwrap();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, [".text", ".rsrc"]);
    if let Some(binary) = sevenzip() {
        compare_with_7zip(&binary, "a looping resource directory", "loop.exe", &bytes);
    }
}

#[test]
fn header_bombs_are_bounded_and_quick() {
    let started = std::time::Instant::now();
    let sections: Vec<ElfSection> = (0..65_000)
        .map(|index| ElfSection {
            name: format!("s{index}").into_bytes(),
            kind: 8,
            data: Vec::new(),
        })
        .collect();
    let elf = exe_writer::elf(Some(&sections), &[], true, false);
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("many.elf");
    fs::write(&path, &elf).unwrap();
    let entries = read_archive(&path, None, ReadOptions::default()).unwrap();
    assert_eq!(entries.len(), 65_002);
    let mut resources = Resources::new();
    for kind in 0..64u32 {
        let names = resources.entry(Key::Id(100 + kind)).or_default();
        for name in 0..256u32 {
            names
                .entry(Key::Id(name))
                .or_default()
                .insert(Key::Id(1033), Vec::new());
        }
    }
    let pe = exe_writer::pe(&PeOptions {
        sections: Vec::new(),
        resources: Some(resources),
        wide: true,
        ..Default::default()
    });
    let path = work.path().join("resources.exe");
    fs::write(&path, &pe).unwrap();
    let entries = read_archive(&path, None, ReadOptions::default()).unwrap();
    assert_eq!(entries.len(), 64 * 256);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
}

#[test]
#[ignore = "writes files that fill the documented limits into ZIPPER_EXE_LIMITS for /usr/bin/time -l"]
fn write_executables_at_the_documented_limits() {
    let folder =
        std::path::PathBuf::from(std::env::var_os("ZIPPER_EXE_LIMITS").expect("ZIPPER_EXE_LIMITS"));
    let sections: Vec<ElfSection> = (0..65_533)
        .map(|index| ElfSection {
            name: format!("section-{index:05}-{}", "n".repeat(40)).into_bytes(),
            kind: 8,
            data: Vec::new(),
        })
        .collect();
    fs::write(
        folder.join("sections.elf"),
        exe_writer::elf(Some(&sections), &[], true, false),
    )
    .unwrap();
    let pe_sections: Vec<PeSection> = (0..65_000)
        .map(|index| PeSection {
            name: exe_writer::section_name(&format!("s{index:05}")),
            data: if index == 64_999 {
                vec![1; 16]
            } else {
                Vec::new()
            },
            virtual_size: Some(0),
        })
        .collect();
    fs::write(
        folder.join("sections.exe"),
        exe_writer::pe(&PeOptions {
            sections: pe_sections,
            wide: true,
            ..Default::default()
        }),
    )
    .unwrap();
    let mut resources = Resources::new();
    for kind in 0..999u32 {
        let names = resources.entry(Key::Id(100 + kind)).or_default();
        for name in 0..1000u32 {
            names
                .entry(Key::Name(format!("resource-name-{name:04}")))
                .or_default()
                .insert(Key::Id(1033), Vec::new());
        }
    }
    fs::write(
        folder.join("resources.exe"),
        exe_writer::pe(&PeOptions {
            resources: Some(resources),
            wide: true,
            ..Default::default()
        }),
    )
    .unwrap();
}

#[test]
fn two_types_that_share_one_name_directory_leave_the_section_raw() {
    let mut resources = Resources::new();
    for kind in [10, 11] {
        resources
            .entry(Key::Id(kind))
            .or_default()
            .entry(Key::Id(1))
            .or_default()
            .insert(Key::Id(1033), b"abc".to_vec());
    }
    let mut bytes = exe_writer::pe(&PeOptions {
        sections: vec![PeSection {
            name: exe_writer::section_name(".text"),
            data: vec![0xC3; 16],
            virtual_size: None,
        }],
        resources: Some(resources),
        wide: true,
        ..Default::default()
    });
    let rsrc = 0x400;
    let first: [u8; 4] = bytes[rsrc + 20..rsrc + 24].try_into().unwrap();
    bytes[rsrc + 28..rsrc + 32].copy_from_slice(&first);
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("shared.exe");
    fs::write(&path, &bytes).unwrap();
    let entries = read_archive(&path, None, ReadOptions::default()).unwrap();
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, [".text", ".rsrc"]);
    if let Some(binary) = sevenzip() {
        compare_with_7zip(&binary, "a shared name directory", "shared.exe", &bytes);
    }
}
