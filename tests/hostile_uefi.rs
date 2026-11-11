//! Damaged and hostile UEFI firmware volumes and capsules. Random volumes
//! of files, sections, nested volumes and compressed sections, cut at
//! every boundary and with a bit flipped in their bodies, are refused
//! whenever 7-Zip refuses them, and when 7-Zip extracts them, Zipper
//! extracts the same tree or refuses for one of the stricter checks
//! `docs/reference/uefi-reader.md` lists. Nothing is published after a
//! refusal, decoded sections stay within their budget, and nesting is
//! bounded.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;
#[path = "common/uefi_writer.rs"]
mod uefi_writer;

use std::fs;
use swf_check::{judge, report, sevenzip_binary};
use uefi_writer::*;

const STRICTER: &[&str] = &[
    "unsafe archive path",
    "invalid path length",
    "of different kinds",
    "is also a directory",
    "inside a file",
    "nest too deeply",
    "need more than 256 MiB",
];

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn bytes(&mut self, most: usize) -> Vec<u8> {
        let length = self.below(most + 1);
        (0..length).map(|_| self.next() as u8).collect()
    }

    fn name(&mut self) -> String {
        let first = [1, 2, 3, 0x777, self.next() as u32][self.below(5)];
        named(first, self.next())
    }
}

fn leaf(random: &mut Random) -> Vec<u8> {
    match [
        0x10u8, 0x10, 0x19, 0x19, 0x11, 0x12, 0x16, 0x13, 0x1B, 0x1C, 0x14, 0x15, 0x18, 0x03, 0x20,
    ][random.below(15)]
    {
        0x15 => ui(["A", "Drv", "", "xxxxx", "na\u{e9}me"][random.below(5)]),
        0x14 => version(["1.0", ""][random.below(2)]),
        kind @ (0x13 | 0x1B | 0x1C) => {
            let bodies: [&[u8]; 5] = [b"\x06\x08", &[2; 18], b"\x08", b"\x06\x08\x00", b"\x0b"];
            section(kind, bodies[random.below(5)])
        }
        0x18 => section(
            0x18,
            &[&[5u8; 16][..], &b"sub".repeat(random.below(3))].concat(),
        ),
        kind => section(kind, &random.bytes(12)),
    }
}

fn section_list(random: &mut Random, depth: usize) -> Vec<u8> {
    let items: Vec<Vec<u8>> = (0..random.below(4))
        .map(|_| {
            let roll = random.below(100);
            match roll {
                _ if depth >= 3 => leaf(random),
                0..12 => lzma_section(&section_list(random, depth + 1)),
                12..20 => {
                    let name = random.name();
                    guided(
                        &name,
                        &section_list(random, depth + 1),
                        random.below(2) as u16,
                        &[],
                    )
                }
                20..26 => compression(&section_list(random, depth + 1), 0, None),
                26..30 => crc_section(&section_list(random, depth + 1)),
                30..37 => tiano_section(&section_list(random, depth + 1)),
                37..46 => fv_section(&make_volume(random, depth + 1)),
                _ => leaf(random),
            }
        })
        .collect();
    sections(&items)
}

fn make_file(random: &mut Random, depth: usize) -> Vec<u8> {
    let roll = random.below(100);
    match roll {
        0..10 => {
            let name = random.name();
            ffs(&name, 1, &random.bytes(8))
        }
        10..15 => ffs(
            PAD,
            0xF0,
            if random.chance(50) {
                &[0xFF; 8]
            } else {
                b"pad!"
            },
        ),
        _ => {
            let name = random.name();
            let kind = [2u8, 3, 4, 5, 6, 7, 7, 7, 9, 0x0B][random.below(10)];
            let body = section_list(random, depth);
            ffs(&name, kind, &body)
        }
    }
}

fn make_volume(random: &mut Random, depth: usize) -> Vec<u8> {
    let count = random.below(4) + usize::from(depth == 0);
    let files = (0..count).map(|_| make_file(random, depth)).collect();
    let extended = random.chance(20).then(|| random.name());
    build(&Volume {
        files,
        extended_name: extended.as_deref(),
        ..Volume::default()
    })
}

fn judge_all(binary: &std::path::Path, cases: Vec<(String, Vec<u8>)>) -> Vec<String> {
    std::thread::scope(|scope| {
        let workers: Vec<_> = cases
            .chunks(cases.len().div_ceil(8).max(1))
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|(label, bytes)| {
                            judge(binary, bytes, "firmware.fd", label, STRICTER)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    })
}

fn sweep(seed: u64, volumes: usize, flips: usize) {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut random = Random(seed);
    let mut cases = Vec::new();
    for index in 0..volumes {
        let image = make_volume(&mut random, 0);
        cases.push((format!("volume {index}"), image.clone()));
        let used = image.iter().rposition(|&byte| byte != 0xFF).unwrap_or(0) + 1;
        for _ in 0..flips {
            let at = 72 + random.below(used.saturating_sub(72).max(1));
            let mut bytes = image.clone();
            let bit = 1u8 << random.below(8);
            bytes[at] ^= bit;
            cases.push((format!("volume {index} byte {at} ^ {bit:#x}"), bytes));
        }
        for cut in [used / 2, used - 1, 100, 72 + 24] {
            cases.push((
                format!("volume {index} cut at {cut}"),
                image[..cut.min(image.len())].to_vec(),
            ));
        }
    }
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, volumes);
}

#[test]
fn random_volumes_cut_and_flipped_are_refused_when_7zip_refuses_them() {
    sweep(0x5EED_1234, 60, 6);
}

#[test]
#[ignore = "the long sweep runs in release-checks; the short one runs in the gate"]
fn many_random_volumes_cut_and_flipped_are_refused_when_7zip_refuses_them() {
    sweep(0x0BAD_CAFE, 600, 12);
}

#[test]
fn every_cut_of_a_small_volume_and_capsule_is_refused_when_7zip_refuses_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let image = volume(vec![
        ffs(
            &named(1, 5),
            7,
            &sections(&[pe(b"MZ"), raw(b"r"), ui("Small")]),
        ),
        ffs(
            &named(2, 5),
            7,
            &sections(&[lzma_section(&sections(&[pe(b"MZ inner")]))]),
        ),
    ]);
    let used = image.iter().rposition(|&byte| byte != 0xFF).unwrap() + 1;
    let capsule = capsule(CAPSULE, &image[..], 28);
    let mut cases: Vec<(String, Vec<u8>)> = (0..used + 8)
        .map(|cut| (format!("volume cut at {cut}"), image[..cut].to_vec()))
        .collect();
    cases.extend(
        (0..used + 36).map(|cut| (format!("capsule cut at {cut}"), capsule[..cut].to_vec())),
    );
    for at in 0..72 {
        let mut bytes = image.clone();
        bytes[at] ^= 0x10;
        fix_volume_checksum(&mut bytes);
        cases.push((
            format!("volume header byte {at} ^ 0x10, checksum repaired"),
            bytes,
        ));
    }
    for (label, map) in [
        (
            "a block map with an entry after its terminator",
            [[1u8, 0, 0, 0, 0, 16, 0, 0], [0; 8], [0; 8]],
        ),
        (
            "a block map in two runs",
            [[1, 0, 0, 0, 0, 8, 0, 0], [1, 0, 0, 0, 0, 8, 0, 0], [0; 8]],
        ),
        (
            "a block map without a terminator",
            [[1, 0, 0, 0, 0, 16, 0, 0], [0, 0, 0, 0, 5, 0, 0, 0], [3; 8]],
        ),
    ] {
        let mut bytes = image[..56].to_vec();
        bytes.extend(map.concat());
        bytes.extend_from_slice(&image[72..image.len() - 8]);
        bytes[48..50].copy_from_slice(&80u16.to_le_bytes());
        fix_volume_checksum(&mut bytes);
        cases.push((label.to_owned(), bytes));
    }
    let mut short_lzma = lzma_section(&sections(&[pe(b"MZ inner")]));
    let size_at = 4 + 20 + 5;
    short_lzma[size_at..size_at + 8].copy_from_slice(&3u64.to_le_bytes());
    let mut empty_lzma = lzma_section(&sections(&[pe(b"MZ")]));
    empty_lzma[size_at..size_at + 8].copy_from_slice(&0u64.to_le_bytes());
    let mut bad_sum = ffs_with(&named(7, 5), 7, &sections(&[pe(b"MZ")]), 0x40, 0x07);
    bad_sum[17] ^= 1;
    for (label, file) in [
        (
            "an LZMA section that goes on past its size",
            ffs(&named(3, 5), 7, &sections(&[short_lzma])),
        ),
        (
            "an LZMA section of size 0 with data",
            ffs(&named(4, 5), 7, &sections(&[empty_lzma])),
        ),
        (
            "a section whose padding runs past its file",
            ffs(&named(5, 5), 7, &[&pe(b"MZ")[..], &[0]].concat()),
        ),
        (
            "a section list that ends with 1 byte",
            ffs(&named(6, 5), 7, &[&pe(b"MZ")[..], &[0, 0, 0]].concat()),
        ),
        ("a file whose data checksum does not match", bad_sum),
        (
            "two files of one name",
            ffs(&named(1, 5), 7, &sections(&[pe(b"MZ second")])),
        ),
    ] {
        cases.push((
            label.to_owned(),
            volume(vec![ffs(&named(1, 5), 7, &sections(&[pe(b"MZ")])), file]),
        ));
    }
    let past = [0x40u8, 0, 0, 0x19, b'x', b'y'].to_vec();
    let extended = [0xFFu8, 0xFF, 0xFF, 0x19, 12, 0, 0, 0, 1, 2, 3, 4].to_vec();
    let small = [2u8, 0, 0, 0x19].to_vec();
    let after = volume(vec![ffs(&named(9, 5), 1, b"after")]);
    for (label, damaged) in [
        ("past its list", &past),
        ("of the extended form", &extended),
        ("shorter than its header", &small),
    ] {
        for (holder, section) in [
            ("a compression section", compression(damaged, 0, None)),
            (
                "a CRC32 section",
                guided(CRC32, damaged, 2, &crc32fast::hash(damaged).to_le_bytes()),
            ),
            (
                "a compression section in a GUID section",
                guided(&named(0x777, 5), &compression(damaged, 0, None), 0, &[]),
            ),
        ] {
            let image = volume(vec![ffs(&named(2, 5), 7, &sections(&[section]))]);
            cases.push((
                format!("a section {label} inside {holder}, then a volume"),
                [image, after.clone()].concat(),
            ));
        }
    }
    let mut bad_header = ffs(&named(3, 5), 1, b"bad");
    bad_header[16] ^= 1;
    let then_bad = volume(vec![
        ffs(&named(2, 5), 7, &sections(&[compression(&past, 0, None)])),
        bad_header,
    ]);
    cases.push((
        "a damaged compression section, then a bad file header, then a volume".to_owned(),
        [then_bad, after.clone()].concat(),
    ));
    for at in 0..28 {
        let mut bytes = capsule.clone();
        bytes[at] ^= 0x04;
        cases.push((format!("capsule header byte {at} ^ 0x04"), bytes));
    }
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, 200);
}

fn extract(bytes: &[u8]) -> (anyhow::Result<()>, tempfile::TempDir) {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("firmware.fd");
    fs::write(&archive, bytes).unwrap();
    let out = work.path().join("out");
    let result = zipper::extract_archive(&archive, &out, zipper::ExtractOptions::default());
    if result.is_err() {
        assert!(!out.exists(), "published after a failure");
    }
    (result, work)
}

#[test]
fn compressed_sections_that_declare_more_than_the_budget_are_refused_before_they_are_decoded() {
    let mut lzma = lzma_alone(b"tiny");
    lzma[5..13].copy_from_slice(&(1u64 << 40).to_le_bytes());
    let image = volume(vec![ffs(
        &named(1, 1),
        7,
        &sections(&[guided(LZMA, &lzma, 1, &[])]),
    )]);
    let (result, _work) = extract(&image);
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("need more than 256 MiB"), "{error}");
    let mut windowed = lzma_alone(b"tiny");
    windowed[1..5].copy_from_slice(&(256u32 << 20).to_le_bytes());
    windowed[5..13].copy_from_slice(&(200u64 << 20).to_le_bytes());
    let image = volume(vec![ffs(
        &named(1, 1),
        7,
        &sections(&[guided(LZMA, &windowed, 1, &[])]),
    )]);
    let (result, _work) = extract(&image);
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("need more than 256 MiB"), "{error}");
}

#[test]
fn the_literal_tables_count_toward_the_budget_and_passing_it_stops_the_search() {
    let mut wide = lzma_alone(b"tiny");
    wide[0] = 224;
    wide[1..5].copy_from_slice(&(200u32 << 20).to_le_bytes());
    wide[5..13].copy_from_slice(&(126u64 << 20).to_le_bytes());
    let budgeted = volume(vec![ffs(
        &named(1, 1),
        7,
        &sections(&[guided(LZMA, &wide, 1, &[])]),
    )]);
    let after = volume(vec![ffs(&named(2, 1), 1, b"after")]);
    let (result, _work) = extract(&[budgeted, after].concat());
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("need more than 256 MiB"), "{error}");
}

#[test]
fn more_files_and_sections_than_the_entry_limit_are_refused_before_they_are_kept() {
    let empty = section(0x19, b"");
    let body = empty.repeat(1_100_000);
    let image = volume(vec![ffs(&named(1, 1), 7, &body)]);
    let (result, _work) = extract(&image);
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("more than 1000000"), "{error}");
}

#[test]
fn volumes_nested_past_the_limit_are_refused() {
    let mut inner = sections(&[pe(b"MZ")]);
    for _ in 0..70 {
        inner = sections(&[compression(&inner, 0, None)]);
    }
    let image = volume(vec![ffs(&named(1, 1), 7, &inner)]);
    let (result, _work) = extract(&image);
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("nest too deeply"), "{error}");
}
