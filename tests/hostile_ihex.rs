//! Damaged and hostile Intel HEX files. A file cut at every byte, or with
//! any byte changed, is refused whenever 7-Zip refuses it, and when 7-Zip
//! extracts it, Zipper extracts the same files or refuses for one of the
//! stricter checks `docs/reference/ihex-reader.md` lists. The run count and
//! the output size stay inside their limits, and nothing is published after
//! a refusal.
#[path = "common/ihex_writer.rs"]
mod ihex_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;
#[cfg(feature = "uefi")]
#[path = "common/uefi_writer.rs"]
mod uefi_writer;

use ihex_writer::*;
use std::path::Path;
use swf_check::{judge, report, sevenzip_binary};

const STRICTER: &[&str] = &["data after the Intel HEX end record"];
const MAX_RUNS: usize = 1_000_000;

fn judge_all(binary: &Path, cases: Vec<(String, Vec<u8>)>) -> Vec<String> {
    std::thread::scope(|scope| {
        let workers: Vec<_> = cases
            .chunks(cases.len().div_ceil(8).max(1))
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|(label, bytes)| {
                            judge(binary, bytes, "in.hex", label, STRICTER)
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

fn small_file() -> Vec<u8> {
    let mut records: Vec<String> = (0..3)
        .map(|index| record(DATA, index * 4, &noise(4, u32::from(index) + 1)))
        .collect();
    records.extend([
        segment(0x0100),
        record(DATA, 0, &noise(3, 7)),
        linear(0x0800),
        record(DATA, 0x10, &noise(2, 8)),
        record(DATA, 0x12, &noise(2, 9)),
        record(START_LINEAR, 0, &[8, 0, 0, 0x10]),
        record(START_SEGMENT, 0, &[0, 0, 0, 0]),
    ]);
    let mut all = records;
    all.push(END_RECORD.to_owned());
    lines(&all, "\r\n")
}

#[test]
fn every_cut_and_every_changed_byte_is_refused_when_7zip_refuses_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let bytes = small_file();
    let mut cases: Vec<(String, Vec<u8>)> = (0..=bytes.len())
        .map(|cut| (format!("cut at {cut}"), bytes[..cut].to_vec()))
        .collect();
    for at in 0..bytes.len() {
        for replacement in [
            b'0',
            b'1',
            b'F',
            b'f',
            b'G',
            b':',
            b'\n',
            b'\r',
            b'\0',
            bytes[at] ^ 0x20,
        ] {
            if replacement == bytes[at] {
                continue;
            }
            let mut changed = bytes.clone();
            changed[at] = replacement;
            cases.push((format!("byte {at} set to {replacement:#04x}"), changed));
        }
    }
    for at in 0..bytes.len() {
        let mut shorter = bytes.clone();
        shorter.remove(at);
        cases.push((format!("byte {at} removed"), shorter));
    }
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, 2000);
}

fn extract(
    bytes: &[u8],
    options: zipper::ExtractOptions,
) -> (tempfile::TempDir, anyhow::Result<()>) {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("in.hex");
    std::fs::write(&archive, bytes).unwrap();
    let result = zipper::extract_archive(&archive, &work.path().join("out"), options);
    (work, result)
}

fn native() -> zipper::ExtractOptions<'static> {
    zipper::ExtractOptions::default()
}

fn separate_runs(count: usize) -> Vec<u8> {
    let mut text = String::with_capacity(count * 13 + 32);
    for index in 0..count {
        let address = (index % 32768 * 2) as u16;
        if address == 0 && index > 0 {
            text.push_str(&linear((index / 32768) as u16));
            text.push('\n');
        }
        text.push_str(&record(DATA, address, &[index as u8]));
        text.push('\n');
    }
    text.push_str(END_RECORD);
    text.push('\n');
    text.into_bytes()
}

#[test]
fn more_runs_than_the_limit_are_refused_before_a_file_is_written() {
    let bytes = separate_runs(MAX_RUNS + 1);
    let listed = tempfile::tempdir().unwrap();
    let archive = listed.path().join("in.hex");
    std::fs::write(&archive, &bytes).unwrap();
    let error = zipper::native_ihex::read_archive(&archive, None, Default::default()).unwrap_err();
    assert!(
        format!("{error:#}").contains("more than 1000000 runs"),
        "{error:#}"
    );
    let (work, result) = extract(&bytes, native());
    assert!(result.is_err());
    assert!(!work.path().join("out").exists());
}

#[test]
fn as_many_runs_as_the_limit_extract() {
    let count = MAX_RUNS;
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("in.hex");
    std::fs::write(&archive, separate_runs(count)).unwrap();
    let entries = zipper::native_ihex::read_archive(&archive, None, Default::default()).unwrap();
    assert_eq!(entries.len(), count);
    assert_eq!(entries[count - 1].name, (count - 1).to_string());
    assert!(entries.iter().all(|entry| entry.size == 1));
}

#[test]
fn output_past_the_cap_is_refused_and_publishes_nothing() {
    let bytes = file(
        &(0..64)
            .map(|index| record(DATA, index * 255, &noise(255, u32::from(index) + 1)))
            .collect::<Vec<_>>(),
    );
    let (work, result) = extract(
        &bytes,
        zipper::ExtractOptions {
            max_output_bytes: Some(64 * 255 - 1),
            ..native()
        },
    );
    assert!(result.is_err());
    assert!(!work.path().join("out").exists());
    let (work, result) = extract(
        &bytes,
        zipper::ExtractOptions {
            max_output_bytes: Some(64 * 255),
            ..native()
        },
    );
    result.unwrap();
    assert_eq!(
        std::fs::read(work.path().join("out/in")).unwrap().len(),
        64 * 255
    );
}

#[test]
fn a_failure_after_the_first_run_publishes_nothing() {
    let mut records: Vec<String> = (0..4)
        .map(|index| record(DATA, index * 64, &noise(16, 3)))
        .collect();
    records.push(record(6, 0, &[]));
    let (work, result) = extract(&file(&records), native());
    let error = result.unwrap_err();
    assert!(format!("{error:#}").contains("Intel HEX"), "{error:#}");
    assert!(!work.path().join("out").exists());
}

#[test]
fn data_after_the_end_record_is_refused_and_publishes_nothing() {
    let mut bytes = file(&[record(DATA, 0, &noise(16, 3))]);
    bytes.extend(b"\n\x1Agarbage\n");
    let (work, result) = extract(&bytes, native());
    let error = result.unwrap_err();
    assert!(format!("{error:#}").contains(STRICTER[0]), "{error:#}");
    assert!(!work.path().join("out").exists());
}

#[test]
fn selected_runs_extract_alone() {
    let bytes = file(&[
        record(DATA, 0, b"first"),
        record(DATA, 0x100, b"second"),
        record(DATA, 0x200, b"third"),
    ]);
    let paths = ["1".to_owned()];
    let (work, result) = extract(
        &bytes,
        zipper::ExtractOptions {
            paths: &paths,
            ..native()
        },
    );
    result.unwrap();
    let out = work.path().join("out");
    assert_eq!(std::fs::read(out.join("1")).unwrap(), b"second");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 1);
}

#[cfg(feature = "uefi")]
#[test]
fn a_firmware_volume_after_a_broken_intel_hex_start_extracts_as_7zip_extracts_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let firmware = uefi_writer::volume(vec![uefi_writer::ffs(
        &uefi_writer::named(0x991, 1),
        1,
        b"one",
    )]);
    let mut problems = Vec::new();
    for broken in [
        record_with_count(DATA, 3, &[0x44], 1).replace("B8", "00"),
        record(6, 0, &[]),
        "garbage".to_owned(),
    ] {
        let records: Vec<String> = (0..3)
            .map(|index| record(DATA, index, &[index as u8]))
            .chain([broken.clone()])
            .collect();
        let mut bytes = lines(&records, "\n");
        bytes.resize(4096, 0xFF);
        bytes.extend_from_slice(&firmware);
        problems.extend(judge(&binary, &bytes, "firmware.fd", &broken, STRICTER));
    }
    report(problems, 3, 3);
}

#[test]
fn padding_past_the_scan_limit_is_refused_and_publishes_nothing() {
    let mut bytes = file(&[record(DATA, 0, &noise(16, 3))]);
    bytes.extend(vec![b' '; (64 << 10) + 1]);
    let (work, result) = extract(&bytes, native());
    let error = result.unwrap_err();
    assert!(format!("{error:#}").contains(STRICTER[0]), "{error:#}");
    assert!(!work.path().join("out").exists());
    let mut bytes = file(&[record(DATA, 0, &noise(16, 3))]);
    bytes.extend(vec![b' '; (64 << 10) - 1]);
    extract(&bytes, native()).1.unwrap();
}
