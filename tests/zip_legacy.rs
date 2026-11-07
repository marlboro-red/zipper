//! ZIP's legacy Shrink (method 1) and Implode (method 6) entries extract in
//! process. No current tool writes them, so test encoders in
//! `tests/common/legacy_encoder.rs` build the streams, and Info-ZIP `unzip`
//! or 7-Zip 26 or later, as each test names, checks each archive before
//! Zipper reads it.
#[path = "common/legacy_encoder.rs"]
mod legacy_encoder;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use legacy_encoder::{
    Bits, Clears, Code, EIGHT_K_WINDOW, Ending, Entry, IMPLODE, ImplodeTrees, LITERAL_TREE, Rng,
    SHRINK, Token, implode, implode_tokens, random_shrink, random_tokens, shrink, shrink_codes,
    shrink_trace, zip,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::native_zip::{read_archive, read_archive_fast};

type Reader = fn(&Path, Option<&Path>) -> anyhow::Result<Vec<zipper::Entry>>;

fn unzip() -> Option<PathBuf> {
    let help = Command::new("unzip").arg("-v").output().ok()?;
    String::from_utf8_lossy(&help.stdout)
        .contains("USE_UNSHRINK")
        .then(|| PathBuf::from("unzip"))
}

fn unzip_reads(archive: &Path, name: &str) -> Option<Result<Vec<u8>, String>> {
    let unzip = unzip()?;
    let output = Command::new(unzip)
        .arg("-p")
        .arg(archive)
        .arg(name)
        .output()
        .unwrap();
    Some(if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    })
}

fn seven_zip_extracts(archive: &Path) -> Option<Result<PathBuf, String>> {
    let seven_zip = sevenzip::sevenzip()?;
    let out = archive.with_extension("7zip");
    let output = Command::new(seven_zip)
        .args(["x", "-y", "-bd"])
        .arg(format!("-o{}", out.display()))
        .arg(archive)
        .stdout(Stdio::null())
        .output()
        .unwrap();
    Some(if output.status.success() {
        Ok(out)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    })
}

fn zipper_extracts(read: Reader, archive: &Path, label: &str) -> anyhow::Result<PathBuf> {
    let out = archive.with_extension(format!("zipper-{label}"));
    read(archive, Some(&out))?;
    Ok(out)
}

fn readers() -> [(&'static str, Reader); 2] {
    [
        ("native", read_archive as Reader),
        ("native-fast", read_archive_fast as Reader),
    ]
}

#[derive(Clone, Copy, PartialEq)]
enum Oracles {
    Both,
    SevenZipOnly,
}

fn assert_every_reader_extracts(archive: &Path, expected: &[(String, Vec<u8>)], oracles: Oracles) {
    if let Some(result) = seven_zip_extracts(archive) {
        let folder = result.unwrap_or_else(|error| panic!("7-Zip refused {archive:?}: {error}"));
        for (name, data) in expected {
            assert!(
                fs::read(folder.join(name)).unwrap() == *data,
                "7-Zip disagrees with the test encoder on {name}"
            );
        }
    }
    let unzip_disagrees: Vec<&String> = expected
        .iter()
        .filter(|_| oracles == Oracles::Both)
        .filter(|(name, data)| {
            unzip_reads(archive, name).is_some_and(|result| result.as_ref() != Ok(data))
        })
        .map(|(name, _)| name)
        .collect();
    assert!(
        unzip_disagrees.is_empty(),
        "unzip disagrees with the test encoder on {unzip_disagrees:?}"
    );
    for (label, read) in readers() {
        let folder = zipper_extracts(read, archive, label)
            .unwrap_or_else(|error| panic!("{label} refused {archive:?}: {error:#}"));
        for (name, data) in expected {
            assert!(
                fs::read(folder.join(name)).unwrap() == *data,
                "{label} extracted other bytes for {name}"
            );
        }
    }
}

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let mut rng = Rng::new(7);
    let mut noise = |count: usize, spread: u64| -> Vec<u8> {
        (0..count).map(|_| b'a' + rng.below(spread) as u8).collect()
    };
    let mut binary = Rng::new(11);
    vec![
        ("empty", Vec::new()),
        ("one-byte", b"x".to_vec()),
        ("repeats", b"a".repeat(5000)),
        ("zeros", vec![0; 20_000]),
        (
            "text",
            b"the quick brown fox jumps over the lazy dog ".repeat(300),
        ),
        ("few-letters", noise(60_000, 4)),
        ("many-letters", noise(150_000, 26)),
        ("binary", (0..40_000).map(|_| binary.next() as u8).collect()),
    ]
}

fn assert_shrink_samples_extract(schedules: &[Clears], oracles: Oracles) {
    let work = tempfile::tempdir().unwrap();
    let mut entries = Vec::new();
    let mut expected = Vec::new();
    for (label, data) in samples() {
        for &clears in schedules {
            let name = format!("{label}-{clears:?}.bin");
            entries.push(Entry::new(&name, SHRINK, 0, &data, shrink(&data, clears)));
            expected.push((name, data.clone()));
        }
    }
    let archive = work.path().join("shrink.zip");
    fs::write(&archive, zip(&entries)).unwrap();
    assert_every_reader_extracts(&archive, &expected, oracles);
}

#[test]
fn shrink_entries_that_clear_a_full_table_extract_as_unzip_and_7zip_read_them() {
    let clears_a_full_table = |data: &Vec<u8>| {
        shrink_trace(data, Clears::WhenFull)
            .windows(2)
            .any(|pair| pair[0].0 == 256 && pair[1].0 == 2)
    };
    assert!(samples().iter().any(|(_, data)| clears_a_full_table(data)));
    assert_shrink_samples_extract(&[Clears::WhenFull], Oracles::Both);
}

#[test]
fn shrink_entries_that_clear_at_any_time_extract_as_7zip_reads_them() {
    assert_shrink_samples_extract(
        &[1, 2, 37, 500].map(Clears::WhenFullAndEvery),
        Oracles::SevenZipOnly,
    );
}

#[test]
fn clears_of_a_full_table_can_still_split_unzip_from_7zip_after_a_reused_entry_survives() {
    let mut codes = vec![
        (97, 9),
        (256, 9),
        (1, 9),
        (256, 10),
        (1, 10),
        (256, 11),
        (1, 11),
    ];
    codes.extend([(256, 12), (1, 12)]);
    let run =
        |codes: &mut Vec<(u32, u32)>, count| codes.extend(std::iter::repeat_n((97, 13), count));
    let clear = |codes: &mut Vec<(u32, u32)>| codes.extend([(256, 13), (2, 13)]);
    run(&mut codes, 7934);
    codes.push((8191, 13));
    clear(&mut codes);
    codes.push((98, 13));
    run(&mut codes, 7933);
    codes.push((99, 13));
    clear(&mut codes);
    codes.push((100, 13));
    run(&mut codes, 7933);
    clear(&mut codes);
    codes.push((98, 13));
    run(&mut codes, 7934);
    let data = legacy_encoder::reference_unshrink(&codes).unwrap();
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("full-clears.zip");
    let name = "data.bin".to_owned();
    fs::write(
        &archive,
        zip(&[Entry::new(&name, SHRINK, 0, &data, shrink_codes(&codes))]),
    )
    .unwrap();
    assert_every_reader_extracts(
        &archive,
        &[(name.clone(), data.clone())],
        Oracles::SevenZipOnly,
    );
    if let Some(result) = unzip_reads(&archive, &name) {
        assert!(
            result.as_ref() != Ok(&data),
            "unzip now frees leaves past its free-code pointer; compare Shrink with it on every stream"
        );
    }
}

#[test]
fn a_partial_clear_frees_leaves_across_the_whole_table_as_7zip_does_where_unzip_scans_less() {
    let lit = |byte: u8| (u32::from(byte), 9);
    let (escape, clear) = ((256, 9), (2, 9));
    let codes = [
        lit(b'a'),
        lit(b'b'),
        lit(b'c'),
        lit(b'd'),
        (257, 9),
        lit(b'e'),
        (261, 9),
        lit(b'f'),
        escape,
        clear,
        lit(b'g'),
        escape,
        clear,
        (257, 9),
    ];
    let data = b"abcdabeabefgab";
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("two-clears.zip");
    let name = "data.bin".to_owned();
    fs::write(
        &archive,
        zip(&[Entry::new(&name, SHRINK, 0, data, shrink_codes(&codes))]),
    )
    .unwrap();
    assert_every_reader_extracts(
        &archive,
        &[(name.clone(), data.to_vec())],
        Oracles::SevenZipOnly,
    );
    if let Some(result) = unzip_reads(&archive, &name) {
        assert!(
            result.as_ref() != Ok(&data.to_vec()),
            "unzip now frees leaves past its free-code pointer; compare Shrink with it on every stream"
        );
    }
}

#[test]
fn the_longest_shrink_string_extracts_as_unzip_and_7zip_read_it() {
    let mut codes = vec![(u32::from(b'a'), 9)];
    let mut width = 9;
    let mut data = b"a".to_vec();
    for code in 257..8192u32 {
        while code >= 1 << width {
            codes.push((256, width));
            codes.push((1, width));
            width += 1;
        }
        codes.push((code, width));
        data.extend(std::iter::repeat_n(b'a', (code - 255) as usize));
    }
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("longest.zip");
    let name = "longest.bin".to_owned();
    fs::write(
        &archive,
        zip(&[Entry::new(&name, SHRINK, 0, &data, shrink_codes(&codes))]),
    )
    .unwrap();
    assert_every_reader_extracts(&archive, &[(name, data)], Oracles::Both);
}

#[test]
fn implode_entries_extract_as_unzip_and_7zip_read_them() {
    let work = tempfile::tempdir().unwrap();
    let mut entries = Vec::new();
    let mut expected = Vec::new();
    for (label, data) in samples() {
        for flags in [
            0,
            EIGHT_K_WINDOW,
            LITERAL_TREE,
            EIGHT_K_WINDOW | LITERAL_TREE,
        ] {
            for seed in 1..=3 {
                let name = format!("{label}-{flags}-{seed}.bin");
                entries.push(Entry::new(
                    &name,
                    IMPLODE,
                    flags,
                    &data,
                    implode(&data, flags, seed),
                ));
                expected.push((name, data.clone()));
            }
        }
    }
    let archive = work.path().join("implode.zip");
    fs::write(&archive, zip(&entries)).unwrap();
    assert_every_reader_extracts(&archive, &expected, Oracles::Both);
}

#[test]
fn implode_matches_before_the_start_read_zeros_as_unzip_and_7zip_do() {
    let work = tempfile::tempdir().unwrap();
    let mut entries = Vec::new();
    let mut expected = Vec::new();
    for flags in [
        0,
        EIGHT_K_WINDOW,
        LITERAL_TREE,
        EIGHT_K_WINDOW | LITERAL_TREE,
    ] {
        let window = if flags & EIGHT_K_WINDOW != 0 {
            8192
        } else {
            4096
        };
        let shortest = if flags & LITERAL_TREE != 0 { 3 } else { 2 };
        let tokens = [
            Token::Match {
                distance: window,
                length: shortest + 63 + 255,
            },
            Token::Literal(b'z'),
            Token::Match {
                distance: 5,
                length: shortest,
            },
        ];
        let mut data = vec![0; shortest + 63 + 255];
        data.push(b'z');
        let back = data.len() - 5;
        data.extend_from_within(back..back + shortest);
        let trees = ImplodeTrees {
            literals: (flags & LITERAL_TREE != 0).then(|| Code::flat(256)),
            lengths: Code::flat(64),
            distances: Code::flat(64),
        };
        let name = format!("before-start-{flags}.bin");
        entries.push(Entry::new(
            &name,
            IMPLODE,
            flags,
            &data,
            implode_tokens(&tokens, flags, &trees),
        ));
        expected.push((name, data));
    }
    let archive = work.path().join("before-start.zip");
    fs::write(&archive, zip(&entries)).unwrap();
    assert_every_reader_extracts(&archive, &expected, Oracles::Both);
}

fn assert_refused_by_unzip_and_zipper(case: &str, entry: Entry) {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("case.zip");
    fs::write(&archive, zip(&[entry])).unwrap();
    if let Some(result) = unzip_reads(&archive, "data.bin") {
        assert!(result.is_err(), "unzip accepted {case}");
    }
    for (label, read) in readers() {
        let out = work.path().join(label);
        assert!(
            read(&archive, Some(&out)).is_err(),
            "{label} accepted {case}"
        );
        assert!(!out.exists(), "{label} left output behind for {case}");
    }
}

fn assert_refused_by_7zip_and_zipper(case: &str, entry: Entry) {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("case.zip");
    fs::write(&archive, zip(&[entry])).unwrap();
    if let Some(result) = seven_zip_extracts(&archive) {
        assert!(result.is_err(), "7-Zip accepted {case}");
    }
    for (label, read) in readers() {
        let out = work.path().join(label);
        assert!(
            read(&archive, Some(&out)).is_err(),
            "{label} accepted {case}"
        );
        assert!(!out.exists(), "{label} left output behind for {case}");
    }
}

fn literal(byte: u8) -> (u32, u32) {
    (u32::from(byte), 9)
}

#[test]
fn hostile_shrink_streams_are_refused_as_7zip_refuses_them() {
    let shrink_case = |codes: &[(u32, u32)], data: &[u8]| {
        Entry::new("data.bin", SHRINK, 0, data, shrink_codes(codes))
    };
    let (a, b, c, d) = (literal(b'a'), literal(b'b'), literal(b'c'), literal(b'd'));
    let (escape, widen, clear) = ((256, 9), (1, 9), (2, 9));
    let cases: Vec<(&str, Entry)> = vec![
        (
            "an entry whose prefix is itself after a partial clear",
            shrink_case(&[a, b, (257, 9), escape, clear, c, (257, 9)], b"ababcc"),
        ),
        (
            "the same entry named right after the clear",
            shrink_case(&[a, b, (257, 9), escape, clear, (257, 9)], b"ababab"),
        ),
        (
            "an entry whose prefix was cleared and not reused",
            shrink_case(
                &[a, b, c, d, (259, 9), escape, clear, literal(b'e'), (257, 9)],
                b"abcdcdeee",
            ),
        ),
        ("a code not yet defined", shrink_case(&[a, (300, 9)], b"aa")),
        (
            "a control code first",
            shrink_case(&[escape, widen, a], b"a"),
        ),
        (
            "an unknown control code",
            shrink_case(&[a, escape, (3, 9), b], b"ab"),
        ),
        (
            "a widening past 13 bits",
            shrink_case(
                &[
                    a,
                    escape,
                    widen,
                    (256, 10),
                    (1, 10),
                    (256, 11),
                    (1, 11),
                    (256, 12),
                    (1, 12),
                    (256, 13),
                    (1, 13),
                    (98, 13),
                ],
                b"ab",
            ),
        ),
        (
            "a code after the table is full",
            Entry::new(
                "data.bin",
                SHRINK,
                0,
                &[b'a'; 7937],
                shrink_codes(&[a; 7937]),
            ),
        ),
        (
            "a control code cut off at the end",
            shrink_case(&[a, b, escape], b"ab"),
        ),
        (
            "padding bits that are not zero",
            Entry::new("data.bin", SHRINK, 0, b"ab", {
                let mut bits = Bits::default();
                bits.put(97, 9);
                bits.put(98, 9);
                bits.put(1 << 5, 6);
                bits.finish()
            }),
        ),
    ];
    for (case, entry) in cases {
        assert_refused_by_7zip_and_zipper(case, entry);
    }
}

#[test]
fn self_prefixed_shrink_entries_are_accepted_while_unused_as_unzip_and_7zip_accept_them() {
    let (a, b, c, d) = (literal(b'a'), literal(b'b'), literal(b'c'), literal(b'd'));
    type Case<'a> = (&'a [(u32, u32)], &'a [u8]);
    let cases: [Case; 3] = [
        (&[a, b, (257, 9), (256, 9), (2, 9), c], b"ababc"),
        (
            &[a, b, (257, 9), (256, 9), (2, 9), c, d, (258, 9)],
            b"ababcdcd",
        ),
        (
            &[
                a,
                b,
                c,
                (258, 9),
                (256, 9),
                (2, 9),
                d,
                literal(b'e'),
                (257, 9),
            ],
            b"abcbcdeded",
        ),
    ];
    let work = tempfile::tempdir().unwrap();
    for (index, (codes, data)) in cases.into_iter().enumerate() {
        let archive = work.path().join(format!("case{index}.zip"));
        let name = "data.bin".to_owned();
        fs::write(
            &archive,
            zip(&[Entry::new(&name, SHRINK, 0, data, shrink_codes(codes))]),
        )
        .unwrap();
        assert_every_reader_extracts(&archive, &[(name, data.to_vec())], Oracles::Both);
    }
}

#[test]
fn damaged_legacy_entries_are_refused() {
    let data = b"the quick brown fox jumps over the lazy dog ".repeat(40);
    for (method, flags, packed) in [
        (SHRINK, 0, shrink(&data, Clears::WhenFullAndEvery(50))),
        (IMPLODE, 0, implode(&data, 0, 1)),
        (
            IMPLODE,
            EIGHT_K_WINDOW | LITERAL_TREE,
            implode(&data, EIGHT_K_WINDOW | LITERAL_TREE, 2),
        ),
    ] {
        for cut in [0, 1, packed.len() / 2, packed.len() - 1] {
            assert_refused_by_7zip_and_zipper(
                &format!("method {method} cut to {cut} of {}", packed.len()),
                Entry::new("data.bin", method, flags, &data, packed[..cut].to_vec()),
            );
        }
        let mut longer = packed.clone();
        longer.push(0);
        let case = format!("method {method} with a byte after its end");
        let entry = Entry::new("data.bin", method, flags, &data, longer);
        if method == SHRINK {
            assert_refused_by_unzip_and_zipper(&case, entry);
        } else {
            assert_refused_by_7zip_and_zipper(&case, entry);
        }
        assert_refused_by_7zip_and_zipper(
            &format!("method {method} declared one byte short"),
            Entry::new(
                "data.bin",
                method,
                flags,
                &data[..data.len() - 1],
                packed.clone(),
            ),
        );
        let mut wrong_crc = Entry::new("data.bin", method, flags, &data, packed.clone());
        wrong_crc.crc ^= 1;
        assert_refused_by_7zip_and_zipper(&format!("method {method} with a wrong CRC"), wrong_crc);
    }
}

#[test]
fn hostile_implode_trees_are_refused_as_7zip_refuses_them() {
    let data = b"abcabcabc";
    let tokens = [
        Token::Literal(b'a'),
        Token::Literal(b'b'),
        Token::Literal(b'c'),
        Token::Match {
            distance: 3,
            length: 6,
        },
    ];
    let valid = |flags| {
        implode_tokens(
            &tokens,
            flags,
            &ImplodeTrees {
                literals: Some(Code::flat(256)),
                lengths: Code::flat(64),
                distances: Code::flat(64),
            },
        )
    };
    let with_lengths = |lengths: Vec<u8>| {
        let literals: Vec<Token> = data.iter().map(|&byte| Token::Literal(byte)).collect();
        let trees = ImplodeTrees {
            literals: None,
            lengths: Code { lengths },
            distances: Code::flat(64),
        };
        Entry::new(
            "data.bin",
            IMPLODE,
            0,
            data,
            implode_tokens(&literals, 0, &trees),
        )
    };
    let mut incomplete = vec![6u8; 64];
    incomplete[0] = 7;
    let mut oversubscribed = vec![6u8; 64];
    oversubscribed[0] = 5;
    let cases = vec![
        ("an incomplete length tree", with_lengths(incomplete)),
        (
            "an over-subscribed length tree",
            with_lengths(oversubscribed),
        ),
        ("a length tree of 63 symbols", with_lengths(vec![6; 63])),
        ("a length tree of 65 symbols", {
            let mut lengths = vec![6u8; 64];
            lengths[63] = 7;
            lengths.push(7);
            with_lengths(lengths)
        }),
        (
            "a stream without its literal tree",
            Entry::new("data.bin", IMPLODE, LITERAL_TREE, data, {
                let mut packed = valid(0);
                packed.truncate(packed.len() - 1);
                packed
            }),
        ),
        (
            "a match that runs past the declared size",
            Entry::new(
                "data.bin",
                IMPLODE,
                LITERAL_TREE,
                b"abcabc",
                valid(LITERAL_TREE),
            ),
        ),
    ];
    for (case, entry) in cases {
        assert_refused_by_7zip_and_zipper(case, entry);
    }
}

#[test]
fn implode_flags_must_agree_between_the_local_and_central_headers() {
    let data = b"the quick brown fox jumps over the lazy dog ".repeat(40);
    for (central, local) in [
        (EIGHT_K_WINDOW, 0),
        (0, EIGHT_K_WINDOW),
        (LITERAL_TREE, 0),
        (EIGHT_K_WINDOW | LITERAL_TREE, LITERAL_TREE),
    ] {
        let mut entry = Entry::new(
            "data.bin",
            IMPLODE,
            central,
            &data,
            implode(&data, central, 1),
        );
        entry.local_flags = local;
        assert_refused_by_7zip_and_zipper(
            &format!("central flags {central}, local {local}"),
            entry,
        );
    }
}

#[test]
fn third_party_legacy_archives_extract_as_unzip_and_7zip_read_them() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-legacy");
    let work = tempfile::tempdir().unwrap();
    for (archive, name, size) in [
        ("implode.zip", "first.txt", 1092),
        ("shrink.zip", "FIRST.TXT", 1092),
        ("shrink_full_table.zip", "x.txt", 26001),
    ] {
        let copy = work.path().join(archive);
        fs::copy(data.join(archive), &copy).unwrap();
        for (label, read) in readers() {
            let folder = zipper_extracts(read, &copy, label)
                .unwrap_or_else(|error| panic!("{label} refused {archive}: {error:#}"));
            let extracted = fs::read(folder.join(name)).unwrap();
            assert_eq!(extracted.len(), size, "{label} {archive}");
            if let Some(result) = unzip_reads(&copy, name) {
                assert!(result.unwrap() == extracted, "unzip disagrees on {archive}");
            }
            if let Some(result) = seven_zip_extracts(&copy) {
                let seven =
                    result.unwrap_or_else(|error| panic!("7-Zip refused {archive}: {error}"));
                assert!(
                    fs::read(seven.join(name)).unwrap() == extracted,
                    "7-Zip disagrees on {archive}"
                );
            }
        }
    }
}

#[test]
fn random_shrink_streams_decode_and_fail_as_7zip_does() {
    let mut rng = Rng::new(2026);
    let (mut valid, mut invalid) = (Vec::new(), Vec::new());
    for index in 0..600 {
        let steps = 1 + rng.below(if index % 10 == 0 { 3000 } else { 120 }) as usize;
        let stream = random_shrink(&mut rng, steps);
        let name = format!("stream{index}.bin");
        match stream.ending {
            Ending::Complete(data) => valid.push((name, stream.codes, data)),
            Ending::RefusedAtLastCode { bytes_before } => {
                invalid.push((name, stream.codes, bytes_before))
            }
        }
    }
    assert!(
        valid.len() > 100 && invalid.len() > 100,
        "{} valid and {} invalid streams",
        valid.len(),
        invalid.len()
    );
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("valid.zip");
    let entries: Vec<Entry> = valid
        .iter()
        .map(|(name, codes, data)| Entry::new(name, SHRINK, 0, data, shrink_codes(codes)))
        .collect();
    fs::write(&archive, zip(&entries)).unwrap();
    let expected: Vec<(String, Vec<u8>)> = valid
        .iter()
        .map(|(name, _, data)| (name.clone(), data.clone()))
        .collect();
    assert_every_reader_extracts(&archive, &expected, Oracles::SevenZipOnly);
    let declared = |bytes_before: &Vec<u8>| {
        let mut declared = bytes_before.clone();
        declared.push(0);
        declared
    };
    let archive = work.path().join("invalid.zip");
    let entries: Vec<Entry> = invalid
        .iter()
        .map(|(name, codes, bytes_before)| {
            Entry::new(
                name,
                SHRINK,
                0,
                &declared(bytes_before),
                shrink_codes(codes),
            )
        })
        .collect();
    fs::write(&archive, zip(&entries)).unwrap();
    match seven_zip_extracts(&archive) {
        Some(Err(report)) => {
            let folder = archive.with_extension("7zip");
            for (name, _, bytes_before) in &invalid {
                assert!(report.contains(name.as_str()), "7-Zip accepted {name}");
                let written = fs::read(folder.join(name)).unwrap_or_default();
                assert!(
                    written == *bytes_before,
                    "7-Zip stopped {name} after {} bytes, not {}",
                    written.len(),
                    bytes_before.len()
                );
            }
        }
        Some(Ok(_)) => panic!("7-Zip accepted every invalid stream"),
        None => {}
    }
    for (name, codes, bytes_before) in &invalid {
        let one = work.path().join(format!("{name}.zip"));
        fs::write(
            &one,
            zip(&[Entry::new(
                name,
                SHRINK,
                0,
                &declared(bytes_before),
                shrink_codes(codes),
            )]),
        )
        .unwrap();
        for (label, read) in readers() {
            let out = work.path().join(format!("{name}-{label}"));
            assert!(read(&one, Some(&out)).is_err(), "{label} accepted {name}");
        }
    }
}

#[test]
fn random_implode_tokens_and_trees_extract_as_unzip_and_7zip_read_them() {
    let mut rng = Rng::new(19);
    let work = tempfile::tempdir().unwrap();
    let mut entries = Vec::new();
    let mut expected = Vec::new();
    for index in 0..240 {
        let flags = [
            0,
            EIGHT_K_WINDOW,
            LITERAL_TREE,
            EIGHT_K_WINDOW | LITERAL_TREE,
        ][index % 4];
        let count = rng.below(if index % 8 == 0 { 4000 } else { 60 }) as usize;
        let (tokens, data) = random_tokens(&mut rng, flags, count);
        let trees = ImplodeTrees {
            literals: (flags & LITERAL_TREE != 0).then(|| Code::random(256, &mut rng)),
            lengths: Code::random(64, &mut rng),
            distances: Code::random(64, &mut rng),
        };
        let name = format!("tokens{index}.bin");
        entries.push(Entry::new(
            &name,
            IMPLODE,
            flags,
            &data,
            implode_tokens(&tokens, flags, &trees),
        ));
        expected.push((name, data));
    }
    let archive = work.path().join("random.zip");
    fs::write(&archive, zip(&entries)).unwrap();
    assert_every_reader_extracts(&archive, &expected, Oracles::Both);
}
