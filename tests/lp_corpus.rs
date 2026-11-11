//! Android logical partition images written from the liblp metadata format
//! extract as `7zz x` extracts them: one file for each partition, named
//! after it, with its linear extents read from the image and its zero
//! extents as zeros. No AOSP `lpmake` runs here, so the test writer builds
//! every image, and 7-Zip checks each one.
#[path = "common/lp_writer.rs"]
mod lp_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;

use lp_writer::{Data, Extent, Layout, Partition, at, build, linear, partition, zero};
use std::fs;
use swf_check::{judge, report, sevenzip_binary, sevenzip_tree, sha256, tree};

fn noise(length: usize, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

fn layouts() -> Vec<(&'static str, Layout)> {
    let big = noise(3 << 20, 7);
    vec![
        (
            "a-b slots",
            Layout {
                partitions: vec![
                    Partition {
                        attributes: 1,
                        group: 1,
                        ..partition(
                            "system_a",
                            vec![linear(8, b"system"), zero(4), linear(1, b"tail")],
                        )
                    },
                    Partition {
                        group: 2,
                        ..partition("system_b", vec![])
                    },
                    Partition {
                        group: 1,
                        ..partition("vendor_a", vec![linear(3, &noise(1500, 3))])
                    },
                    Partition {
                        group: 2,
                        ..partition("vendor_b", vec![])
                    },
                ],
                groups: vec![("default", 0, 0), ("main_a", 0, 8192), ("main_b", 0, 0)],
                ..Layout::default()
            },
        ),
        (
            "version 10.2 with three slots",
            Layout {
                partitions: vec![partition(
                    "product",
                    vec![linear(2, b"product"), linear(2, b"more")],
                )],
                minor: 2,
                slots: 3,
                max_size: 8192,
                ..Layout::default()
            },
        ),
        (
            "version 10.1 with one slot",
            Layout {
                partitions: vec![partition("odm", vec![zero(9)])],
                minor: 1,
                slots: 1,
                ..Layout::default()
            },
        ),
        (
            "a large partition in many extents",
            Layout {
                partitions: vec![partition(
                    "system",
                    big.chunks(1 << 18)
                        .map(|chunk| linear(chunk.len() as u64 / 512, chunk))
                        .collect(),
                )],
                ..Layout::default()
            },
        ),
        (
            "shared and backward extents",
            Layout {
                partitions: vec![
                    partition("first", vec![linear(2, b"first data")]),
                    partition("again", vec![at(1, 7), at(2, 5), at(1, 0)]),
                    partition("geometry", vec![at(1, 8)]),
                ],
                ..Layout::default()
            },
        ),
        (
            "names 7-Zip writes as they are",
            Layout {
                partitions: vec![
                    partition("", vec![linear(1, b"no name")]),
                    partition(&"n".repeat(36), vec![linear(1, b"long")]),
                    Partition {
                        name: b"caf\xe9 \xff".to_vec(),
                        ..partition("", vec![linear(1, b"latin")])
                    },
                    partition("folder/inner", vec![linear(1, b"inner")]),
                    partition("empty", vec![]),
                    partition(
                        "zero length",
                        vec![Extent {
                            sectors: 0,
                            data: Data::Zero,
                        }],
                    ),
                    partition("twice", vec![linear(1, b"first")]),
                    partition("twice", vec![linear(1, b"second")]),
                ],
                ..Layout::default()
            },
        ),
    ]
}

#[test]
fn generated_images_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let layouts = layouts();
    let problems: Vec<String> = layouts
        .iter()
        .filter_map(|(label, layout)| judge(&binary, &build(layout), "super.img", label, &[]))
        .collect();
    report(problems, layouts.len(), 6);
}

#[test]
fn each_partition_holds_its_extents_in_order_with_zeros_for_zero_extents() {
    let layout = Layout {
        partitions: vec![partition(
            "system_a",
            vec![linear(2, b"head"), zero(1), linear(1, b"end")],
        )],
        ..Layout::default()
    };
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("super.img");
    fs::write(&archive, build(&layout)).unwrap();
    let out = work.path().join("out");
    zipper::extract_archive(&archive, &out, zipper::ExtractOptions::default()).unwrap();
    let mut expected = b"head".to_vec();
    expected.resize(3 * 512, 0);
    expected.extend_from_slice(b"end");
    expected.resize(4 * 512, 0);
    assert_eq!(fs::read(out.join("system_a.img")).unwrap(), expected);
    assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
}

#[test]
fn names_that_leave_the_folder_go_to_7zip_and_the_native_engine_refuses_them() {
    let layout = Layout {
        partitions: vec![
            partition("/abs", vec![linear(1, b"x")]),
            partition("a/../../x", vec![linear(1, b"y")]),
            partition("c:d", vec![linear(1, b"z")]),
        ],
        ..Layout::default()
    };
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("super.img");
    fs::write(&archive, build(&layout)).unwrap();
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &archive).unwrap(),
        zipper::Reader::Backend {
            format: Some("-tlp".to_owned())
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
fn a_huge_zero_partition_is_charged_against_the_cap_before_it_is_written() {
    let layout = Layout {
        partitions: vec![partition("huge", vec![zero(1 << 40)])],
        ..Layout::default()
    };
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("super.img");
    fs::write(&archive, build(&layout)).unwrap();
    let out = work.path().join("out");
    let options = zipper::ExtractOptions {
        max_output_bytes: Some(1 << 20),
        ..Default::default()
    };
    let error = zipper::extract_archive(&archive, &out, options).unwrap_err();
    assert!(
        format!("{error:#}").contains("--max-extracted-bytes"),
        "{error:#}"
    );
    assert!(!out.exists());
}

#[test]
fn a_selected_partition_extracts_alone() {
    let layout = &layouts()[0].1;
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("super.img");
    fs::write(&archive, build(layout)).unwrap();
    let out = work.path().join("out");
    let paths = ["vendor_a.img".to_owned()];
    let options = zipper::ExtractOptions {
        paths: &paths,
        ..Default::default()
    };
    zipper::extract_archive(&archive, &out, options).unwrap();
    let found = tree(&out);
    assert_eq!(found.keys().collect::<Vec<_>>(), ["vendor_a.img"]);
    let mut expected = noise(1500, 3);
    expected.resize(1536, 0);
    assert_eq!(found["vendor_a.img"], sha256(&expected));
}
