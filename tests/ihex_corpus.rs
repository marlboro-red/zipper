//! Intel HEX files extract as `7zz x` extracts them: one file for each run
//! of data records whose addresses follow on, named after the archive when
//! there is one run and `0`, `1` and on when there are more. Files come
//! from `objcopy -O ihex` and from a test writer that covers every record
//! type, and 7-Zip checks each one. `docs/reference/ihex-reader.md` gives
//! the rules.
#[path = "common/ihex_writer.rs"]
mod ihex_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;

use ihex_writer::*;
use std::path::{Path, PathBuf};
use swf_check::{judge, report, sevenzip_binary};

const STRICTER: &[&str] = &["data after its end-of-file record"];

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

fn sixteen(at: u16) -> String {
    record(DATA, at, &noise(16, u32::from(at) + 1))
}

fn three_data_records() -> Vec<String> {
    (0..3).map(|index| sixteen(index * 16)).collect()
}

fn with_three_first(rest: &[String]) -> Vec<String> {
    let mut records = three_data_records();
    records.extend_from_slice(rest);
    records
}

fn named_cases() -> Vec<(String, Vec<u8>)> {
    let long = noise(255, 9);
    let mut cases: Vec<(&str, Vec<u8>)> = vec![
        ("one run", file(&[sixteen(0), sixteen(16)])),
        ("one run, CRLF", {
            let mut all = vec![sixteen(0), sixteen(16)];
            all.push(END_RECORD.to_owned());
            lines(&all, "\r\n")
        }),
        ("one run, CR", {
            let mut all = vec![sixteen(0), sixteen(16)];
            all.push(END_RECORD.to_owned());
            lines(&all, "\r")
        }),
        ("no line breaks", {
            let mut all = vec![sixteen(0), sixteen(16)];
            all.push(END_RECORD.to_owned());
            lines(&all, "")
        }),
        ("no break after the end record", {
            let mut bytes = file(&[sixteen(0)]);
            bytes.pop();
            bytes
        }),
        ("lowercase digits", {
            String::from_utf8(file(&[sixteen(0), sixteen(16)]))
                .unwrap()
                .to_ascii_lowercase()
                .into_bytes()
        }),
        ("a gap", file(&[sixteen(0), sixteen(32)])),
        (
            "twelve runs",
            file(&(0..12).map(|index| sixteen(index * 32)).collect::<Vec<_>>()),
        ),
        (
            "out of order",
            file(&[sixteen(32), sixteen(0), sixteen(16)]),
        ),
        ("overlap", file(&[sixteen(0), sixteen(8)])),
        (
            "back to the end of an earlier run",
            file(&[sixteen(0), sixteen(32), sixteen(16)]),
        ),
        (
            "a 255-byte record",
            file(&[record(DATA, 0, &long), record(DATA, 255, &long)]),
        ),
        (
            "past 64 KiB without an address record",
            file(&[sixteen(0xFFF8), sixteen(8)]),
        ),
        ("only the end record", file(&[])),
        (
            "segment after three",
            file(&with_three_first(&[segment(0x1000), sixteen(0)])),
        ),
        (
            "segment that follows on",
            file(&with_three_first(&[segment(3), sixteen(0)])),
        ),
        (
            "segment FFFF",
            file(&with_three_first(&[segment(0xFFFF), sixteen(0xFFF8)])),
        ),
        (
            "linear after three",
            file(&with_three_first(&[linear(1), sixteen(0)])),
        ),
        (
            "linear at the top of 32 bits",
            file(&with_three_first(&[
                linear(0xFFFF),
                sixteen(0xFFF8),
                linear(0),
                sixteen(8),
            ])),
        ),
        (
            "a run that ends at 4 GiB does not wrap to 0",
            file(&with_three_first(&[
                linear(0xFFFF),
                sixteen(0xFFF0),
                linear(0),
                sixteen(0),
            ])),
        ),
        (
            "a segment run past 1 MiB",
            file(&with_three_first(&[
                segment(0xFFFF),
                sixteen(0xFFFF),
                sixteen(0x000F),
            ])),
        ),
        (
            "segment then linear reset",
            file(&with_three_first(&[segment(3), linear(0), sixteen(0x30)])),
        ),
        (
            "linear across 64 KiB",
            file(&with_three_first(&[
                sixteen(0xFFF0),
                linear(1),
                sixteen(0),
                sixteen(16),
            ])),
        ),
        (
            "start segment after three",
            file(&with_three_first(&[record(
                START_SEGMENT,
                0,
                &[0, 1, 2, 3],
            )])),
        ),
        (
            "start linear after three",
            file(&with_three_first(&[record(
                START_LINEAR,
                0,
                &[8, 0, 1, 0x23],
            )])),
        ),
        ("sixteen breaks", {
            let mut bytes = lines(&three_data_records(), "\n");
            bytes.extend(b"\n".repeat(15));
            bytes.extend(file(&[sixteen(0x30)]));
            bytes
        }),
        ("seventeen breaks", {
            let mut bytes = lines(&three_data_records(), "\n");
            bytes.extend(b"\n".repeat(16));
            bytes.extend(file(&[sixteen(0x30)]));
            bytes
        }),
        ("eight CRLF", {
            let mut bytes = lines(&three_data_records(), "\r\n");
            bytes.extend(b"\r\n".repeat(7));
            bytes.extend(file(&[sixteen(0x30)]));
            bytes
        }),
        ("an empty file", Vec::new()),
        (
            "a line break first",
            [b"\n".as_slice(), &file(&[sixteen(0)])].concat(),
        ),
        ("a space before a record", {
            let mut bytes = file(&[sixteen(0)]);
            bytes.insert(bytes.len() - END_RECORD.len() - 1, b' ');
            bytes
        }),
        (
            "a NUL between records",
            lines(&[sixteen(0), END_RECORD.to_owned()], "\0"),
        ),
        ("no end record", lines(&three_data_records(), "\n")),
        (
            "CRLF after the end record",
            lines(&[sixteen(0), END_RECORD.to_owned()], "\r\n"),
        ),
    ];
    for (kind, data) in [
        (SEGMENT, vec![0, 0]),
        (LINEAR, vec![0, 0]),
        (START_SEGMENT, vec![0; 4]),
        (START_LINEAR, vec![0; 4]),
    ] {
        for at in 0..4 {
            let mut records = three_data_records();
            records.insert(at, record(kind, 0, &data));
            cases.push((
                "an address or start record among the first four",
                file(&records),
            ));
        }
    }
    let bad: Vec<(&str, String)> = vec![
        ("a bad checksum", {
            let mut text = sixteen(0x30);
            let last = text.pop().unwrap();
            text.push(if last == '0' { '1' } else { '0' });
            text
        }),
        ("an empty data record", record(DATA, 0x30, &[])),
        ("an end record with data", record(END, 0, &[1, 2])),
        ("an end record with an address", record(END, 0x1234, &[])),
        (
            "a segment record with an address",
            record(SEGMENT, 1, &[0, 0]),
        ),
        ("a linear record of 4 bytes", record(LINEAR, 0, &[0; 4])),
        (
            "a start record of 2 bytes",
            record(START_LINEAR, 0, &[0; 2]),
        ),
        (
            "a start record with an address",
            record(START_SEGMENT, 1, &[0; 4]),
        ),
        ("type 6", record(6, 0, &[])),
        ("type FF", record(0xFF, 0, &[])),
        (
            "a count past the record",
            record_with_count(DATA, 0x30, &[1, 2], 3),
        ),
        ("a G", sixteen(0x30).replacen('0', "G", 1)),
    ];
    for (label, text) in bad {
        cases.push((label, file(&with_three_first(std::slice::from_ref(&text)))));
        cases.push((label, file(&[sixteen(0), text])));
    }
    for tail in ["\n\n", "\r\r", "\n\r", "\0", "garbage\n"] {
        let mut bytes = file(&[sixteen(0)]);
        bytes.extend(tail.as_bytes());
        cases.push(("data after the end record", bytes));
    }
    cases
        .into_iter()
        .enumerate()
        .map(|(index, (label, bytes))| (format!("{index} {label}"), bytes))
        .collect()
}

#[test]
fn generated_files_of_every_record_type_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let cases = named_cases();
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, 75);
}

#[test]
fn random_files_extract_or_are_refused_as_7zip_extracts_or_refuses_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut random = Random::new(19);
    let cases: Vec<_> = (0..3000)
        .map(|index| (format!("random file {index}"), random_file(&mut random)))
        .collect();
    let checked = cases.len();
    report(judge_all(&binary, cases), checked, 3000);
}

fn objcopy() -> Option<PathBuf> {
    let candidates = [
        "objcopy",
        "gobjcopy",
        "llvm-objcopy",
        "/opt/homebrew/opt/binutils/bin/objcopy",
        "/opt/homebrew/opt/llvm/bin/llvm-objcopy",
        "/opt/homebrew/opt/llvm@21/bin/llvm-objcopy",
        "/usr/local/opt/binutils/bin/objcopy",
    ];
    candidates.into_iter().map(PathBuf::from).find(|binary| {
        std::process::Command::new(binary)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

fn objcopy_files(binary: &Path) -> Vec<(String, Vec<u8>)> {
    let work = tempfile::tempdir().unwrap();
    let mut cases = Vec::new();
    for (size, seed) in [(1, 1), (700, 2), (65536, 3), (70000, 4), (300_000, 5)] {
        let raw = work.path().join(format!("data-{size}.bin"));
        std::fs::write(&raw, noise(size, seed)).unwrap();
        let section = "--change-section-address";
        let options: [&[&str]; 6] = [
            &[],
            &["--set-start", "0x1234"],
            &[section, ".data+0x12340"],
            &[section, ".data+0x08000000"],
            &["--set-start", "0x08000123", section, ".data+0x08000000"],
            &[section, ".data+0xFFFFF000"],
        ];
        for option in options {
            let output = work.path().join("out.hex");
            let run = std::process::Command::new(binary)
                .args(["-I", "binary", "-O", "ihex"])
                .args(option)
                .arg(&raw)
                .arg(&output)
                .output()
                .unwrap();
            if !run.status.success() {
                continue;
            }
            cases.push((
                format!("objcopy {size} bytes {}", option.join(" ")),
                std::fs::read(&output).unwrap(),
            ));
        }
    }
    cases
}

#[test]
fn objcopy_files_extract_as_7zip_extracts_them() {
    let Some(sevenzip) = sevenzip_binary() else {
        return;
    };
    let Some(objcopy) = objcopy() else {
        eprintln!("no objcopy or llvm-objcopy on PATH; the objcopy files are not checked");
        return;
    };
    let cases = objcopy_files(&objcopy);
    let checked = cases.len();
    let starting_with_data = cases
        .iter()
        .filter(|(_, bytes)| bytes.starts_with(b":10"))
        .count();
    assert!(starting_with_data >= 5, "{starting_with_data}");
    report(judge_all(&sevenzip, cases), checked, 20);
}

#[test]
fn one_run_takes_the_archive_name_and_several_take_their_numbers() {
    let work = tempfile::tempdir().unwrap();
    let numbers = |count: u16| (0..count).map(|index| index.to_string()).collect();
    let named = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
    for (archive, bytes, names) in [
        (
            "firmware.hex",
            file(&[sixteen(0), sixteen(16)]),
            named(&["firmware"]),
        ),
        ("a.b.hex", file(&[sixteen(0)]), named(&["a.b"])),
        ("noextension", file(&[sixteen(0)]), named(&["noextension~"])),
        ("1", file(&[sixteen(0), sixteen(64)]), numbers(2)),
        (
            "many.ihex",
            file(&(0..11).map(|index| sixteen(index * 32)).collect::<Vec<_>>()),
            numbers(11),
        ),
    ] {
        let mut names: Vec<String> = names;
        let path = work.path().join(archive);
        std::fs::write(&path, &bytes).unwrap();
        let out = work.path().join(format!("{archive}.out"));
        let options = zipper::ExtractOptions {
            engine: zipper::Engine::Native,
            ..Default::default()
        };
        zipper::extract_archive(&path, &out, options).unwrap();
        let mut found: Vec<String> = std::fs::read_dir(&out)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        found.sort();
        names.sort();
        assert_eq!(found, names, "{archive}");
    }
}

#[test]
fn one_line_break_of_any_kind_after_the_end_record_belongs_to_it() {
    let work = tempfile::tempdir().unwrap();
    for (label, newline) in [("none", ""), ("LF", "\n"), ("CR", "\r"), ("CRLF", "\r\n")] {
        let mut bytes = lines(&[sixteen(0)], newline);
        bytes.extend(END_RECORD.as_bytes());
        bytes.extend(newline.as_bytes());
        let archive = work.path().join(format!("{label}.hex"));
        std::fs::write(&archive, &bytes).unwrap();
        let out = work.path().join(format!("{label}.out"));
        let options = zipper::ExtractOptions {
            engine: zipper::Engine::Native,
            ..Default::default()
        };
        zipper::extract_archive(&archive, &out, options)
            .unwrap_or_else(|error| panic!("{label}: {error:#}"));
        assert!(out.join(label).is_file(), "{label}");
    }
}

#[test]
fn each_file_takes_the_archive_files_modified_time_as_7zip_gives_it() {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("firmware.hex");
    std::fs::write(&archive, file(&[sixteen(0), sixteen(64)])).unwrap();
    let time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
    std::fs::File::options()
        .write(true)
        .open(&archive)
        .unwrap()
        .set_modified(time)
        .unwrap();
    let out = work.path().join("out");
    let options = zipper::ExtractOptions {
        engine: zipper::Engine::Native,
        ..Default::default()
    };
    zipper::extract_archive(&archive, &out, options).unwrap();
    for name in ["0", "1"] {
        let modified = std::fs::metadata(out.join(name))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(modified, time, "{name}");
    }
}
