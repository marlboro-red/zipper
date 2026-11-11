//! UEFI firmware volumes and capsules extract as `7zz x` extracts them. The
//! QEMU firmware images that Homebrew's QEMU installs are checked where they
//! are present, and the test writer builds volumes from the UEFI PI format
//! that cover every file, section, compression and naming rule 7-Zip shows.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;
#[path = "common/uefi_writer.rs"]
mod uefi_writer;

use std::{fs, path::Path};
use swf_check::{judge, report, sevenzip_binary, sevenzip_tree, tree};
use uefi_writer::*;

const QEMU_FIRMWARE: &str = "/opt/homebrew/share/qemu";

fn name(first: u32) -> String {
    named(first, 0x2222_3333_4444_5555)
}

fn other(first: u32, rest: u64) -> String {
    named(first, rest)
}

fn noise(length: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

pub fn generated() -> Vec<(String, Vec<u8>)> {
    let driver = |first: u32, items: &[Vec<u8>]| ffs(&name(first), 7, &sections(items));
    let two = volume(vec![
        ffs(&name(0x991), 1, b"one"),
        ffs(&name(0x992), 1, b"two"),
    ]);
    let one = volume(vec![ffs(&name(0x991), 1, b"one")]);
    let inner_file = volume(vec![driver(0x993, &[pe(b"MZ"), raw(b"z")])]);
    let unknown = name(0x777);
    let big = noise(300_000, 9);
    let mut cases = vec![
        (
            "every leaf section".to_owned(),
            volume(
                [
                    0x10u8, 0x11, 0x12, 0x16, 0x19, 0x1A, 0x1C, 0x1D, 0x03, 0x20, 0xF0, 0x00,
                ]
                .iter()
                .enumerate()
                .flat_map(|(index, &kind)| {
                    let body = format!("data{kind:02x}").into_bytes();
                    [
                        driver(0x100 + index as u32, &[section(kind, &body)]),
                        driver(0x200 + index as u32, &[section(kind, &body), raw(b"r")]),
                    ]
                })
                .collect(),
            ),
        ),
        (
            "names, versions and dependencies".to_owned(),
            volume(vec![
                driver(1, &[pe(b"MZ\x90\0"), ui("Driver"), version("1.0")]),
                driver(2, &[pe(b"MZ"), raw(b"rawdata"), ui("Two")]),
                driver(3, &[ui("A"), ui("B"), pe(b"MZ")]),
                driver(4, &[ui("OnlyName")]),
                driver(5, &[]),
                driver(6, &[pe(b"MZ"), ui("")]),
                driver(7, &[pe(b"MZ"), ui("caf\u{e9} \u{4e2d}\u{1F600}")]),
                driver(8, &[pe(b"MZ"), section(0x15, b"a\0b")]),
                driver(9, &[pe(b"MZ"), section(0x15, &[b'a', 0, b'b', 0])]),
                driver(10, &[pe(b"MZ"), section(0x14, b"\x01")]),
                driver(11, &[pe(b"MZ"), section(0x14, b"")]),
                driver(12, &[pe(b"MZ"), section(0x14, b"\x01\0")]),
                driver(13, &[section(0x13, b"\x06\x08"), pe(b"MZ")]),
                driver(14, &[section(0x13, b"\x06\x08\x00"), pe(b"MZ")]),
                driver(15, &[section(0x13, b"\x06"), pe(b"MZ")]),
                driver(16, &[section(0x13, b"\x02abc"), pe(b"MZ")]),
                driver(17, &[section(0x13, b"\x0a\x08"), pe(b"MZ")]),
                driver(18, &[section(0x1B, b"\x09\x08"), pe(b"MZ")]),
                driver(19, &[section(0x13, b""), pe(b"MZ")]),
                driver(
                    20,
                    &[section(0x18, &[&[7u8; 16][..], b"sub"].concat()), pe(b"MZ")],
                ),
                driver(21, &[section(0x18, b"short"), pe(b"MZ")]),
                driver(22, &[section(0x1C, b"\x06\x08"), pe(b"MZ")]),
            ]),
        ),
        (
            "file kinds, pads and repeated names".to_owned(),
            volume(vec![
                ffs(&name(1), 1, b"raw file body"),
                ffs(PAD, 0xF0, &[0xFF; 40]),
                ffs(PAD, 0xF0, b"pad data"),
                ffs(PAD, 0xF0, b"more pad data"),
                ffs(&other(1, 2), 7, &sections(&[pe(b"MZ")])),
                ffs(&other(1, 2), 7, &sections(&[pe(b"MZ"), raw(b"x")])),
                ffs(&name(2), 2, &sections(&[raw(b"free")])),
                ffs(&name(3), 0xC0, &sections(&[pe(b"MZ"), raw(b"x")])),
                ffs(&name(4), 9, &sections(&[pe(b"MZ"), ui("App")])),
                ffs_with(&name(5), 7, &sections(&[pe(b"MZ")]), 0x40, 0x07),
            ]),
        ),
        (
            "volumes inside files".to_owned(),
            volume(vec![
                driver(1, &[fv_section(&two)]),
                driver(2, &[fv_section(&one)]),
                driver(3, &[fv_section(&two), raw(b"r")]),
                driver(
                    4,
                    &[raw(b"r"), fv_section(&one), raw(b"s"), fv_section(&one)],
                ),
                ffs(&name(5), 0x0B, &sections(&[fv_section(&two)])),
                driver(6, &[fv_section(&two), ui("Named")]),
                driver(7, &[fv_section(&inner_file)]),
                driver(8, &[fv_section(&volume(vec![]))]),
                driver(9, &[fv_section(&volume(vec![])), raw(b"r")]),
                driver(10, &[fv_section(&two), fv_section(&two)]),
                driver(
                    11,
                    &[fv_section(&build(&Volume {
                        files: vec![ffs(&name(0x995), 1, b"r")],
                        extended_name: Some(&name(0xEEE)),
                        ..Volume::default()
                    }))],
                ),
                driver(
                    12,
                    &[
                        fv_section(&build(&Volume {
                            files: vec![ffs(&name(0x996), 1, b"r")],
                            file_system: FFS3,
                            ..Volume::default()
                        })),
                        raw(b"q"),
                    ],
                ),
                driver(
                    13,
                    &[
                        fv_section(&[b"junk".repeat(14), vec![0; 0]].concat()),
                        raw(b"q"),
                    ],
                ),
                driver(
                    14,
                    &[fv_section(&[two.clone(), b"extra".to_vec()].concat())],
                ),
            ]),
        ),
        (
            "GUID, CRC and compression sections".to_owned(),
            volume(vec![
                driver(1, &[lzma_section(&sections(&[pe(b"MZinner")]))]),
                driver(2, &[lzma_section(&sections(&[pe(b"MZinner"), raw(b"rr")]))]),
                driver(3, &[lzma_section(&sections(&[pe(b"MZinner")])), ui("Lz")]),
                driver(
                    4,
                    &[lzma_section(&sections(&[pe(b"MZ"), raw(b"rr")])), raw(b"q")],
                ),
                driver(5, &[crc_section(&sections(&[pe(b"MZinner")]))]),
                driver(6, &[guided(&unknown, &sections(&[pe(b"MZ")]), 0, &[])]),
                driver(7, &[compression(&sections(&[pe(b"MZ")]), 0, None)]),
                driver(
                    8,
                    &[
                        compression(&sections(&[pe(b"MZ"), raw(b"c")]), 0, None),
                        raw(b"d"),
                    ],
                ),
                driver(9, &[lzma_section(&sections(&[fv_section(&two)]))]),
                ffs(
                    &name(10),
                    0x0B,
                    &sections(&[lzma_section(&sections(&[fv_section(&two)]))]),
                ),
                driver(
                    11,
                    &[lzma_section(&sections(&[raw(b"a"), fv_section(&two)]))],
                ),
                driver(
                    12,
                    &[guided(&unknown, &sections(&[pe(b"MZ"), ui("In")]), 0, &[])],
                ),
                driver(
                    13,
                    &[guided(
                        &unknown,
                        &sections(&[guided(&name(0x888), &sections(&[pe(b"MZ")]), 0, &[])]),
                        0,
                        &[],
                    )],
                ),
                driver(14, &[guided(&unknown, b"", 0, &[])]),
                driver(
                    15,
                    &[
                        guided(&unknown, &sections(&[raw(b"a")]), 0, &[]),
                        guided(&unknown, &sections(&[raw(b"b"), raw(b"c")]), 0, &[]),
                    ],
                ),
                driver(
                    16,
                    &[
                        raw(b"a"),
                        guided(&unknown, &sections(&[pe(b"MZ"), raw(b"r")]), 0, &[]),
                        raw(b"b"),
                    ],
                ),
                driver(
                    17,
                    &[guided(&unknown, &sections(&[fv_section(&one)]), 0, &[])],
                ),
                driver(18, &[compression(&sections(&[fv_section(&two)]), 0, None)]),
                driver(
                    19,
                    &[crc_section(&sections(&[raw(b"g"), pe(b"MZ")])), raw(b"h")],
                ),
                driver(20, &[lzma_section(&sections(&[pe(&big)]))]),
            ]),
        ),
        (
            "Tiano and compressed EFI sections".to_owned(),
            volume(vec![
                driver(1, &[tiano_section(&sections(&[pe(b"MZ"), raw(b"r")]))]),
                driver(2, &[tiano_section(&sections(&[pe(b"MZ")]))]),
            ]),
        ),
        (
            "an EFI compressed section 7-Zip does not decode".to_owned(),
            volume(vec![driver(
                1,
                &[compression(
                    &sections(&[pe(b"MZ")]),
                    1,
                    Some(b"\x05\0\0\0\x08\0\0\0abcde"),
                )],
            )]),
        ),
    ];
    let nested_files = volume(vec![driver(1, &[pe(b"MZ")]), ffs(&name(2), 1, b"r")]);
    cases.push(("an extended header and free space junk".to_owned(), {
        let mut built = build(&Volume {
            files: vec![driver(1, &[pe(b"MZ"), raw(b"x")]), ffs(&name(2), 1, b"r")],
            extended_name: Some(&name(0xABC)),
            ..Volume::default()
        });
        let at = built.len() - 100;
        built[at] = 0;
        built
    }));
    let crc_inner = sections(&[pe(b"MZ"), raw(b"r")]);
    let crc = crc32fast::hash(&crc_inner).to_le_bytes();
    cases.push((
        "CRC32 sections with other data offsets".to_owned(),
        volume(vec![
            driver(1, &[guided(CRC32, &crc_inner, 2, &[])]),
            driver(
                2,
                &[guided(CRC32, &crc_inner, 2, &[&crc[..], b"XXXX"].concat())],
            ),
            driver(3, &[guided(CRC32, &sections(&[pe(b"MZ")]), 2, &[])]),
        ]),
    ));
    let mut damaged_lzma = lzma_section(&sections(&[pe(b"MZ")]));
    let last = damaged_lzma.len() - 3;
    damaged_lzma[last] ^= 0x55;
    cases.push((
        "a header error, then a damaged LZMA section, then a volume".to_owned(),
        [
            volume(vec![
                ffs(
                    &name(1),
                    7,
                    &[&section(0x10, b"MZ")[..], &[0x40, 0, 0, 0x10]].concat(),
                ),
                driver(2, &[damaged_lzma]),
            ]),
            one.clone(),
        ]
        .concat(),
    ));
    let leaves = |count: usize| -> Vec<Vec<u8>> {
        (0..count)
            .map(|index| raw(format!("leaf {index}").as_bytes()))
            .collect()
    };
    let folder = || guided(&unknown, &sections(&[pe(b"MZ"), raw(b"r")]), 0, &[]);
    cases.push((
        "folders of more than 10 leaves".to_owned(),
        volume(vec![
            driver(1, &leaves(11)),
            driver(2, &leaves(101)),
            driver(3, &[leaves(9), vec![folder(), folder()]].concat()),
            driver(4, &[vec![folder()], leaves(10)].concat()),
            driver(
                5,
                &[leaves(10), vec![ui("Ten"), section(0x13, b"\x06")]].concat(),
            ),
        ]),
    ));
    cases.push(("a capsule".to_owned(), capsule(CAPSULE, &nested_files, 28)));
    cases.push((
        "a capsule with a long header".to_owned(),
        capsule(CAPSULE, &nested_files, 80),
    ));
    cases.push((
        "a capsule of two volumes".to_owned(),
        capsule(CAPSULE, &[nested_files.clone(), one.clone()].concat(), 28),
    ));
    cases.push((
        "a capsule of no volume".to_owned(),
        capsule(CAPSULE, &b"junk".repeat(30), 28),
    ));
    cases.push((
        "a capsule 7-Zip opens as a volume".to_owned(),
        capsule(EFI_CAPSULE, &nested_files, 28),
    ));
    cases.push((
        "code before the volume".to_owned(),
        [vec![0x14; 4096], nested_files.clone()].concat(),
    ));
    cases.push((
        "a volume after the end of another".to_owned(),
        [nested_files.clone(), one.clone()].concat(),
    ));
    cases
}

#[test]
fn generated_volumes_and_capsules_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let cases = generated();
    let problems: Vec<String> = cases
        .iter()
        .filter_map(|(label, bytes)| judge(&binary, bytes, "firmware.fd", label, &[]))
        .collect();
    report(problems, cases.len(), 14);
}

#[test]
fn qemu_firmware_images_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let Ok(folder) = fs::read_dir(QEMU_FIRMWARE) else {
        eprintln!("no QEMU firmware in {QEMU_FIRMWARE}; the generated volumes still run");
        return;
    };
    let mut images: Vec<_> = folder
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "fd"))
        .filter(|path| !path.to_string_lossy().contains("riscv"))
        .collect();
    images.sort();
    let problems: Vec<String> = images
        .iter()
        .filter_map(|path| {
            let bytes = fs::read(path).unwrap();
            judge(
                &binary,
                &bytes,
                "firmware.fd",
                &path.display().to_string(),
                &[],
            )
        })
        .collect();
    report(problems, images.len(), 0);
}

#[test]
fn names_that_leave_the_folder_go_to_7zip_and_the_native_engine_refuses_them() {
    let image = volume(vec![
        ffs(&name(1), 7, &sections(&[pe(b"MZ"), ui("a/../../x")])),
        ffs(&name(2), 7, &sections(&[pe(b"MZ"), raw(b"r"), ui("..")])),
        ffs(&name(3), 7, &sections(&[pe(b"MZ"), ui("c:d\\e")])),
    ]);
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("firmware.fd");
    fs::write(&archive, &image).unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &archive).unwrap(),
        zipper::Reader::Backend {
            format: Some("-tuefif".to_owned())
        }
    );
    let native = work.path().join("native");
    let options = zipper::ExtractOptions {
        engine: zipper::Engine::Native,
        ..Default::default()
    };
    let error = zipper::extract_archive(&archive, &native, options).unwrap_err();
    assert!(
        format!("{error:#}").contains("unsafe archive path"),
        "{error:#}"
    );
    assert!(!native.exists());
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let auto = work.path().join("auto");
    zipper::extract_archive(&archive, &auto, zipper::ExtractOptions::default()).unwrap();
    let theirs = sevenzip_tree(&binary, &archive, &work.path().join("7z")).unwrap();
    assert_eq!(tree(&auto), theirs);
}

#[test]
fn a_selected_file_extracts_alone_with_its_bytes() {
    let image = volume(vec![
        ffs(&name(1), 7, &sections(&[pe(b"MZ first"), ui("First")])),
        ffs(
            &name(2),
            7,
            &sections(&[lzma_section(&sections(&[pe(b"MZ second"), raw(b"r")]))]),
        ),
    ]);
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("firmware.fd");
    fs::write(&archive, &image).unwrap();
    let out = work.path().join("out");
    let paths = ["00000002/EE4E5898/0.efi".to_owned()];
    let options = zipper::ExtractOptions {
        paths: &paths,
        ..Default::default()
    };
    zipper::extract_archive(&archive, &out, options).unwrap();
    assert_eq!(
        fs::read(out.join("00000002/EE4E5898/0.efi")).unwrap(),
        b"MZ second"
    );
    assert!(!Path::new(&out.join("First.efi")).exists());
}
