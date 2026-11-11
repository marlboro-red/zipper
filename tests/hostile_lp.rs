//! Damaged and hostile Android logical partition images. An image cut at
//! any boundary, or with any metadata byte changed and its checksums
//! repaired so the change reaches the field, is refused whenever 7-Zip
//! refuses it, and when 7-Zip extracts it, Zipper extracts the same tree or
//! refuses for one of the stricter checks `docs/reference/lp-reader.md`
//! lists. Nothing is published after a refusal.
#[path = "common/lp_writer.rs"]
mod lp_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;

use lp_writer::{
    GEOMETRY_AT, Layout, METADATA_AT, at, build, header_size, linear, partition, repair, zero,
};
use swf_check::{judge, report, sevenzip_binary};

const STRICTER: &[&str] = &[
    "unsafe archive path",
    "of different kinds",
    "is also a directory",
    "inside a file",
];
const MAX_SIZE: u32 = 4096;

fn small_image() -> (Layout, Vec<u8>) {
    let layout = Layout {
        partitions: vec![
            partition(
                "system_a",
                vec![linear(2, b"system"), zero(1), linear(1, b"tail")],
            ),
            partition("vendor_a", vec![linear(1, b"vendor"), at(1, 0)]),
            partition("empty", vec![]),
        ],
        slots: 1,
        max_size: MAX_SIZE,
        ..Layout::default()
    };
    let image = build(&layout);
    (layout, image)
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
                            judge(binary, bytes, "super.img", label, STRICTER)
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

const LARGEST_ZERO_OUTPUT: u64 = 64 << 20;

fn zero_output(image: &[u8]) -> u64 {
    let field = |at: usize| {
        image.get(at..at + 4).map_or(0, |bytes| {
            u32::from_le_bytes(bytes.try_into().unwrap()) as usize
        })
    };
    let header = field(METADATA_AT + 8);
    let (offset, count) = (field(METADATA_AT + 92), field(METADATA_AT + 96));
    (0..count.min(1 << 16))
        .filter_map(|index| {
            let at = METADATA_AT + header + offset + index * 24;
            let entry = image.get(at..at + 24)?;
            (entry[8..12] == [1, 0, 0, 0])
                .then(|| u64::from_le_bytes(entry[..8].try_into().unwrap()))
        })
        .fold(0u64, |sum, sectors| {
            sum.saturating_add(sectors.saturating_mul(512))
        })
}

fn tables_size(image: &[u8]) -> usize {
    u32::from_le_bytes(
        image[METADATA_AT + 44..METADATA_AT + 48]
            .try_into()
            .unwrap(),
    ) as usize
}

#[test]
fn every_cut_at_a_sector_or_boundary_is_refused_when_7zip_refuses_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let (_, image) = small_image();
    let header = header_size(0);
    let mut cuts: Vec<usize> = (0..image.len()).step_by(512).collect();
    for edge in [
        GEOMETRY_AT,
        GEOMETRY_AT + 52,
        METADATA_AT,
        METADATA_AT + header,
        METADATA_AT + header + tables_size(&image),
        image.len(),
    ] {
        cuts.extend(edge.saturating_sub(2)..(edge + 2).min(image.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let cases: Vec<_> = cuts
        .iter()
        .map(|&cut| (format!("cut at {cut}"), image[..cut].to_vec()))
        .collect();
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, 60);
}

#[test]
fn every_changed_metadata_byte_with_repaired_checksums_is_refused_when_7zip_refuses_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let (layout, image) = small_image();
    let header = header_size(0);
    let checksums = [
        GEOMETRY_AT + 8..GEOMETRY_AT + 40,
        METADATA_AT + 12..METADATA_AT + 44,
        METADATA_AT + 48..METADATA_AT + 80,
    ];
    let mut positions: Vec<usize> = (GEOMETRY_AT..GEOMETRY_AT + 52).collect();
    positions.extend(METADATA_AT..METADATA_AT + header + tables_size(&image));
    let mut cases = Vec::new();
    for at in positions {
        if checksums.iter().any(|field| field.contains(&at)) {
            continue;
        }
        for flip in [0x01u8, 0x80, 0xFF] {
            let mut bytes = image.clone();
            bytes[at] ^= flip;
            repair(&mut bytes, layout.slots, layout.max_size);
            if zero_output(&bytes) <= LARGEST_ZERO_OUTPUT {
                cases.push((format!("byte {at} ^ {flip:#x}, repaired"), bytes));
            }
        }
    }
    for at in (0..image.len()).step_by(97) {
        let mut bytes = image.clone();
        bytes[at] ^= 0x10;
        cases.push((format!("byte {at} ^ 0x10"), bytes));
    }
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, 1000);
}

#[test]
fn chosen_extents_sizes_and_slots_read_as_7zip_reads_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let shared = || {
        vec![
            partition("one", vec![linear(2, b"shared")]),
            partition("two", vec![at(2, 0), at(2, 0), at(1, 24)]),
            partition("metadata", vec![at(4, 8), at(8, 24)]),
        ]
    };
    let past = |sector| Layout {
        partitions: vec![partition("past", vec![at(1, sector)])],
        device_size: Some(4096),
        ..Layout::default()
    };
    let mut cases = vec![
        (
            "overlapping".to_owned(),
            build(&Layout {
                partitions: shared(),
                ..Layout::default()
            }),
        ),
        (
            "past the largest image".to_owned(),
            build(&past(u64::MAX / 512)),
        ),
        ("past the end of the file".to_owned(), build(&past(10_000))),
    ];
    let small = build(&Layout {
        partitions: shared(),
        device_size: Some(4096),
        ..Layout::default()
    });
    for (label, at, value) in [
        ("slots past the end of the file", GEOMETRY_AT + 44, 64u32),
        ("a header of 200 bytes", METADATA_AT + 8, 200),
        ("a header of 132 bytes", METADATA_AT + 8, 132),
        ("a header of 0 bytes", METADATA_AT + 8, 0),
    ] {
        let mut bytes = small.clone();
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        repair(&mut bytes, 1, 65536);
        cases.push((label.to_owned(), bytes));
    }
    let two = |second: (u64, u64)| Layout {
        partitions: shared(),
        device_size: Some(4096),
        more_devices: vec![second],
        ..Layout::default()
    };
    cases.push(("a second block device".to_owned(), build(&two((600, 4096)))));
    cases.push((
        "a second block device inside the metadata".to_owned(),
        build(&two((0, 4096))),
    ));
    cases.push((
        "a second block device past the end of the file".to_owned(),
        build(&two((600, 1 << 40))),
    ));
    let mut first_wraps = small.clone();
    let devices = METADATA_AT + header_size(0) + tables_size(&small) - 64;
    first_wraps[devices..devices + 8].copy_from_slice(&(1u64 << 55).to_le_bytes());
    repair(&mut first_wraps, 1, 65536);
    cases.push((
        "a first sector whose byte offset wraps to zero".to_owned(),
        first_wraps,
    ));
    let tables = METADATA_AT + header_size(0);
    for (label, kind, target, device) in [
        ("an unused extent of type 2", 2u8, 0u8, 0u8),
        ("an unused zero extent with a target", 1, 5, 0),
        (
            "an unused linear extent past the end of the file",
            0,
            0x40,
            0,
        ),
        (
            "an unused linear extent on a device the image lacks",
            0,
            1,
            1,
        ),
        ("an unused zero extent on a device the image lacks", 1, 0, 1),
    ] {
        let mut unused = small.clone();
        unused[tables + 2 * 52 + 44] = 1;
        let last = tables + 3 * 52 + 5 * 24;
        unused[last + 8] = kind;
        unused[last + 15] = target;
        unused[last + 20] = device;
        repair(&mut unused, 1, 65536);
        cases.push((label.to_owned(), unused));
    }
    let mut beyond = small.clone();
    beyond[GEOMETRY_AT + 44..GEOMETRY_AT + 48].copy_from_slice(&64u32.to_le_bytes());
    beyond[devices..devices + 8].copy_from_slice(&(1u64 << 40).to_le_bytes());
    repair(&mut beyond, 1, 65536);
    cases.push((
        "slots and the first sector past the end of the file".to_owned(),
        beyond,
    ));
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, 17);
}

#[test]
fn a_huge_zero_partition_lists_and_selects_without_reading_its_zeros() {
    let layout = Layout {
        partitions: vec![
            partition("big", vec![zero(1 << 40)]),
            partition("small", vec![linear(8, b"small")]),
        ],
        ..Layout::default()
    };
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("super.img");
    std::fs::write(&archive, build(&layout)).unwrap();
    let entries = zipper::native_lp::read_archive(&archive, None, Default::default()).unwrap();
    assert_eq!(entries[0].size, 1 << 49);
    let out = work.path().join("out");
    let paths = ["small.img".to_owned()];
    let options = zipper::ExtractOptions {
        paths: &paths,
        ..Default::default()
    };
    zipper::extract_archive(&archive, &out, options).unwrap();
    assert_eq!(std::fs::read(out.join("small.img")).unwrap().len(), 8 * 512);
}

#[test]
fn huge_declared_tables_and_slots_are_refused_before_they_are_read() {
    let (layout, image) = small_image();
    let work = tempfile::tempdir().unwrap();
    for (label, at, value) in [
        ("tables size", METADATA_AT + 44, u32::MAX),
        ("metadata size", GEOMETRY_AT + 40, 0xFFFF_FE00),
        ("slot count", GEOMETRY_AT + 44, u32::MAX),
        ("partition count", METADATA_AT + 84, u32::MAX),
    ] {
        let mut bytes = image.clone();
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        repair(&mut bytes, layout.slots, layout.max_size);
        let archive = work.path().join(format!("{label}.img"));
        std::fs::write(&archive, &bytes).unwrap();
        let out = work.path().join(label);
        let error =
            zipper::extract_archive(&archive, &out, zipper::ExtractOptions::default()).unwrap_err();
        assert!(format!("{error:#}").contains("LP"), "{label}: {error:#}");
        assert!(!out.exists(), "{label}");
    }
}
