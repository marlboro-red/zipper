//! Every FLV and SWF in `tests/data/swf-corpus` extracts to the tree 7-Zip
//! extracted when the corpus was written, and to the tree 7-Zip extracts now
//! where it is installed. Generated FLV of every audio format and video
//! codec, SWF of every tag form and SWF compressed with zlib and LZMA extract
//! byte for byte as `7zz x` extracts them. Without 7-Zip 26.05 or later the differential
//! tests are skipped, unless `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;
#[path = "common/swf_writer.rs"]
mod swf_writer;

use std::{fs, path::Path};
use swf_check::{Tree, judge, report, sevenzip_binary, sevenzip_tree, zipper_tree};
use swf_writer::{
    AUDIO, Random, SCRIPT, VIDEO, audio, flv, flv_tag, lzma_swf_from_7zip,
    lzma_swf_with_end_marker, movie, movie_with_tail, swf_tag, video, zlib_swf,
};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/swf-corpus");

#[test]
fn every_corpus_file_extracts_the_tree_7zip_extracts() {
    let recorded: std::collections::BTreeMap<String, Option<Tree>> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    assert!(recorded.len() >= 17);
    let binary = sevenzip_binary();
    for (name, expected) in &recorded {
        let archive = Path::new(CORPUS).join(name);
        let work = tempfile::tempdir().unwrap();
        let ours = zipper_tree(&archive, &work.path().join("zipper"));
        match expected {
            Some(expected) => assert_eq!(ours.as_ref().ok(), Some(expected), "{name}: {ours:?}"),
            None => assert!(
                ours.is_err(),
                "{name}: 7-Zip refused it and Zipper extracted it"
            ),
        }
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, &archive, &work.path().join("7z"));
            assert_eq!(&theirs, expected, "{name}: 7-Zip now extracts another tree");
        }
    }
}

fn check_all(cases: Vec<(String, Vec<u8>)>, least: usize) {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let checked = cases.len();
    let problems = std::thread::scope(|scope| {
        let workers: Vec<_> = cases
            .chunks(cases.len().div_ceil(8).max(1))
            .map(|chunk| {
                let binary = &binary;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter(|(name, _)| !sevenzip::windows_forbids(name))
                        .filter_map(|(name, bytes)| judge(binary, bytes, name, name, &[]))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    });
    report(problems, checked, least);
}

fn payload(random: &mut Random) -> Vec<u8> {
    let length = match random.below(4) {
        0 => random.below(3),
        1 => random.below(40),
        2 => random.below(400),
        _ => 2000 + random.below(30_000),
    };
    random.bytes(length)
}

fn one_stream(random: &mut Random, kind: u8, codec: u8, tags: usize) -> Vec<Vec<u8>> {
    (0..tags)
        .map(|index| {
            let data = payload(random);
            let timestamp = (index as u32 * 40) | (random.below(2) as u32) << 24;
            match kind {
                AUDIO => audio(codec, random.below(16) as u8, &data, timestamp),
                _ => video(1 + random.below(5) as u8, codec, &data, timestamp),
            }
        })
        .collect()
}

fn script(random: &mut Random) -> Vec<u8> {
    let data = payload(random);
    flv_tag(SCRIPT, &[&b"\x02\x00\x0aonMetaData"[..], &data].concat(), 0)
}

#[test]
fn generated_flv_of_every_codec_and_tag_kind_extract_as_7zip_extracts_them() {
    let mut random = Random(0x0F1E_2D3C_4B5A_6978);
    let mut cases = Vec::new();
    for (kind, flags) in [(AUDIO, 4), (VIDEO, 1)] {
        for codec in 0..16u8 {
            let count = 1 + random.below(5);
            let mut tags = one_stream(&mut random, kind, codec, count);
            if random.below(2) == 0 {
                tags.insert(random.below(tags.len() + 1), script(&mut random));
            }
            cases.push((format!("raw-{kind}-{codec}.flv"), flv(flags, &tags)));
        }
    }
    for round in 0..40u8 {
        let audio_codec = round % 16;
        let video_codec = ((usize::from(round) * 7 + 3) % 16) as u8;
        let (audio_count, video_count) = (1 + random.below(4), 1 + random.below(4));
        let mut audio_tags = one_stream(&mut random, AUDIO, audio_codec, audio_count);
        let video_tags = one_stream(&mut random, VIDEO, video_codec, video_count);
        if round % 5 == 0 {
            audio_tags.push(audio((audio_codec + 1) % 16, 0, b"other", 999));
        }
        let mut tags = Vec::new();
        let (mut a, mut v) = (audio_tags.into_iter(), video_tags.into_iter());
        loop {
            let pick = random.below(3);
            let next = match pick {
                0 => a.next().or_else(|| v.next()),
                1 => v.next().or_else(|| a.next()),
                _ => Some(script(&mut random)),
            };
            match next {
                Some(tag) => tags.push(tag),
                None => break,
            }
        }
        let flags = [0, 1, 4, 5][usize::from(round % 4)];
        cases.push((format!("mixed-{round}.flv"), flv(flags, &tags)));
    }
    let simple = flv(5, &[audio(2, 0, b"sound", 0), video(1, 2, b"frame", 0)]);
    for (at, value) in [(3, 2), (4, 2), (4, 8), (4, 0x80), (8, 10), (12, 1)] {
        let mut header = simple.clone();
        header[at] = value;
        cases.push((format!("header-{at}-{value}.flv"), header));
    }
    cases.push((
        "one-byte-tags.flv".into(),
        flv(5, &[audio(2, 0, b"", 0), audio(2, 0, b"", 1)]),
    ));
    cases.push((
        "changing-codec.flv".into(),
        flv(4, &[audio(2, 0, b"a", 0), audio(3, 0, b"b", 1)]),
    ));
    let both = flv(5, &[audio(2, 0, b"sound", 0), video(1, 2, b"frame", 0)]);
    let one = flv(4, &[audio(10, 1, b"sound", 0)]);
    for name in [
        "movie",
        "movie.FLV",
        ".flv",
        "a.b.flv",
        "clip.mp3",
        "tail .flv",
        " .flv",
        "a .b .flv",
        "tail.",
    ] {
        cases.push((name.to_owned(), both.clone()));
        cases.push((name.to_owned(), one.clone()));
    }
    check_all(cases, 96);
}

#[test]
fn generated_swf_of_every_tag_form_extract_as_7zip_extracts_them() {
    let mut random = Random(0x5EED_0F5A_1234_5678);
    let mut cases = Vec::new();
    for round in 0..48usize {
        let mut tags = Vec::new();
        for _ in 0..random.below(12) {
            let code = match random.below(3) {
                0 => 1 + random.below(100) as u16,
                1 => [1, 2, 6, 8, 9, 14, 18, 19, 21, 35, 45, 87, 90][random.below(13)],
                _ => 1 + random.below(1023) as u16,
            };
            let length = match random.below(5) {
                0 => 0,
                1 => 62 + random.below(3),
                _ => payload(&mut random).len(),
            };
            let body = random.bytes(length);
            tags.push(swf_tag(code, &body, random.below(4) == 0));
        }
        let version = [0, 6, 10, 13, 32, 63][round % 6];
        let width = (round * 5 % 32) as u8;
        cases.push((format!("movie-{round}.swf"), movie(version, width, &tags)));
    }
    let long_end = {
        let mut swf = movie(10, 15, &[swf_tag(9, b"\0\0\xff", false)]);
        swf.truncate(swf.len() - 2);
        swf.extend_from_slice(&swf_tag(0, b"", true));
        let length = swf.len() as u32;
        swf_writer::with_length(swf, length)
    };
    cases.push(("long-end.swf".into(), long_end));
    cases.push(("no-tags.swf".into(), movie(10, 0, &[])));
    let early_end = movie_with_tail(10, 1, &[], &[0, 0, 1, 0x40]);
    cases.push(("early-end.swf".into(), early_end));
    check_all(cases, 50);
}

#[test]
fn generated_compressed_swf_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let mut random = Random(0xC0FF_EE11_2233_4455);
    let mut cases = Vec::new();
    for round in 0..24usize {
        let tags: Vec<Vec<u8>> = (0..random.below(8))
            .map(|_| {
                let body = payload(&mut random);
                swf_tag(1 + random.below(90) as u16, &body, false)
            })
            .collect();
        let swf = movie([6, 10, 13, 40][round % 4], 15, &tags);
        let level = (round % 10) as u32;
        cases.push((format!("zlib-{round}.swf"), zlib_swf(&swf, level)));
        let (lc, lp, pb) = [(3, 0, 2), (0, 0, 0), (4, 0, 2), (1, 3, 4)][round % 4];
        cases.push((
            format!("lzma-{round}.swf"),
            lzma_swf_with_end_marker(&swf, 1 << (12 + round % 9), lc, lp, pb),
        ));
        if !swf[8..].is_empty() {
            cases.push((
                format!("7zip-lzma-{round}.swf"),
                lzma_swf_from_7zip(&binary, scratch.path(), &swf),
            ));
        }
    }
    let mut not_a_movie = b"FWS\x0a".to_vec();
    not_a_movie.extend_from_slice(&(8u32 + 300).to_le_bytes());
    not_a_movie.extend(random.bytes(300));
    cases.push(("inner.swf".into(), zlib_swf(&not_a_movie, 6)));
    cases.push((
        "inner-lzma.swf".into(),
        lzma_swf_with_end_marker(&not_a_movie, 1 << 16, 3, 0, 2),
    ));
    let empty = not_a_movie[..8].to_vec();
    let empty = swf_writer::with_length(empty, 8);
    cases.push(("empty.swf".into(), zlib_swf(&empty, 6)));
    cases.push((
        "empty-lzma.swf".into(),
        lzma_swf_with_end_marker(&empty, 1 << 16, 3, 0, 2),
    ));
    for length in 9..=10 {
        let mut short = not_a_movie[..8 + length].to_vec();
        short[4..8].copy_from_slice(&(8 + length as u32).to_le_bytes());
        cases.push((
            format!("short-lzma-{length}.swf"),
            lzma_swf_with_end_marker(&short, 1 << 16, 3, 0, 2),
        ));
    }
    let small = zlib_swf(&movie(10, 1, &[swf_tag(1, b"", false)]), 9);
    for name in [
        "movie",
        "movie.SWF",
        ".swf",
        "a.b.swf",
        "movie~.swf",
        "clip.bin",
        "tail .swf",
    ] {
        cases.push((name.to_owned(), small.clone()));
    }
    check_all(cases, 70);
}

#[test]
fn a_selected_tag_extracts_alone_and_a_missing_one_is_refused() {
    let swf = movie(
        10,
        15,
        &[swf_tag(9, b"\0\0\xff", false), swf_tag(87, b"binary", true)],
    );
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("movie.swf");
    fs::write(&archive, &swf).unwrap();
    let paths = ["1.87".to_owned()];
    let out = work.path().join("out");
    zipper::extract_archive(
        &archive,
        &out,
        zipper::ExtractOptions {
            paths: &paths,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(fs::read(out.join("1.87")).unwrap(), b"binary");
    assert_eq!(fs::read_dir(&out).unwrap().count(), 1);
    let missing = ["2.1".to_owned()];
    let error = zipper::extract_archive(
        &archive,
        &work.path().join("none"),
        zipper::ExtractOptions {
            paths: &missing,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("not found"), "{error:#}");
    let excluded = ["1.*".to_owned()];
    let out = work.path().join("excluded");
    zipper::extract_archive(
        &archive,
        &out,
        zipper::ExtractOptions {
            paths: &paths,
            exclude: &excluded,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    for (strip, said) in [
        (zipper::Strip::Components(1), "leaves nothing"),
        (zipper::Strip::Prefix("1.87"), "is not a folder"),
    ] {
        let error = zipper::extract_archive(
            &archive,
            &work.path().join("stripped"),
            zipper::ExtractOptions {
                strip,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains(said), "{error:#}");
    }
}

#[test]
fn a_name_7zip_would_give_the_archive_itself_gains_a_tilde_and_colons_become_underscores() {
    let work = tempfile::tempdir().unwrap();
    for (name, bytes, expected) in [
        (
            "x.aac.aac",
            flv(4, &[audio(10, 1, b"sound", 0)]),
            vec!["x.aac.aac~"],
        ),
        (
            "x:y.flv",
            flv(4, &[audio(2, 0, b"sound", 0)]),
            vec!["x_y.mp3"],
        ),
        ("a:b.swf", zlib_swf(&movie(10, 1, &[]), 6), vec!["a_b~.swf"]),
    ]
    .into_iter()
    .filter(|(name, _, _)| !sevenzip::windows_forbids(name))
    {
        let archive = work.path().join(name);
        fs::write(&archive, bytes).unwrap();
        let out = work.path().join(format!("out-{}", expected[0]));
        let tree = zipper_tree(&archive, &out).unwrap();
        assert_eq!(
            tree.keys().collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>()
        );
    }
}

#[test]
fn a_compressed_swf_in_numbered_volumes_is_named_from_its_first_volume() {
    let work = tempfile::tempdir().unwrap();
    let swf = movie(10, 15, &[swf_tag(87, &[5; 3000], true)]);
    let packed = zlib_swf(&swf, 0);
    let (first, second) = packed.split_at(packed.len() / 2);
    fs::write(work.path().join("movie.swf.001"), first).unwrap();
    fs::write(work.path().join("movie.swf.002"), second).unwrap();
    let archive = work.path().join("movie.swf.001");
    let tree = zipper_tree(&archive, &work.path().join("out")).unwrap();
    assert_eq!(tree.keys().collect::<Vec<_>>(), ["movie~.swf"]);
    assert_eq!(tree["movie~.swf"], swf_check::sha256(&swf));
}
