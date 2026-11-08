//! Damaged and hostile FLV and SWF files. A file cut at any length, or with
//! a damaged byte, is refused whenever 7-Zip refuses it, and when 7-Zip
//! extracts it, Zipper extracts the same tree or refuses for one of the
//! stricter checks `docs/reference/swf-reader.md` lists. A compressed SWF is
//! charged against the output cap before it is written, and a stream that
//! decodes past its declared length is refused at that length.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/swf_check.rs"]
mod swf_check;
#[path = "common/swf_writer.rs"]
mod swf_writer;

use std::{fs, path::Path};
use swf_check::{judge, report, sevenzip_binary};
use swf_writer::{
    SCRIPT, audio, flv, flv_tag, lzma_swf_from_7zip, lzma_swf_from_parts, lzma_swf_with_end_marker,
    movie, swf_tag, video, with_length, zlib_swf,
};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/swf-corpus");

const STRICTER: &[&str] = &[
    "has data after",
    "ends inside a tag header",
    "runs past the end of the file",
    "an FLV tag holds no data",
    "an FLV tag has a stream ID",
    "neither audio, video nor script data",
    "previous tag size does not match",
    "LZMA match runs past the declared size",
    "goes on past its declared size",
    "ends before its packed size",
];

fn small_files(scratch: &Path, binary: &Path) -> Vec<(&'static str, Vec<u8>)> {
    let swf = movie(
        10,
        5,
        &[
            swf_tag(9, b"\0\0\xff", false),
            swf_tag(87, b"binary data", true),
            swf_tag(1, b"", false),
        ],
    );
    vec![
        (
            "both.flv",
            flv(
                5,
                &[
                    audio(2, 15, b"abc", 0),
                    video(1, 2, b"defg", 33),
                    flv_tag(SCRIPT, b"meta", 0),
                    audio(2, 15, b"hi", 66),
                ],
            ),
        ),
        (
            "raw.flv",
            flv(4, &[audio(10, 1, b"aac0", 0), audio(10, 1, b"aac1", 23)]),
        ),
        ("plain.swf", swf.clone()),
        ("zlib.swf", zlib_swf(&swf, 9)),
        ("lzma.swf", lzma_swf_with_end_marker(&swf, 1 << 16, 3, 0, 2)),
        ("7zip-lzma.swf", lzma_swf_from_7zip(binary, scratch, &swf)),
    ]
}

fn judge_all(cases: Vec<(String, &str, Vec<u8>)>) -> Vec<String> {
    let Some(binary) = sevenzip_binary() else {
        return Vec::new();
    };
    std::thread::scope(|scope| {
        let workers: Vec<_> = cases
            .chunks(cases.len().div_ceil(8).max(1))
            .map(|chunk| {
                let binary = &binary;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|(label, name, bytes)| {
                            judge(binary, bytes, name, label, STRICTER)
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

fn alternate_flip(at: usize) -> &'static [u8] {
    match at % 2 {
        0 => &[0x01],
        _ => &[0x80],
    }
}

fn three_flips(_: usize) -> &'static [u8] {
    &[0x01, 0x80, 0xFF]
}

fn two_flips(_: usize) -> &'static [u8] {
    &[0x01, 0x80]
}

fn sweep(full: bool) {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let scratch = tempfile::tempdir().unwrap();
    let mut cases = Vec::new();
    let mut files: Vec<(String, &str, Vec<u8>)> = small_files(scratch.path(), &binary)
        .into_iter()
        .map(|(name, bytes)| (name.to_owned(), name, bytes))
        .collect();
    if full {
        for name in [
            "h264-aac.flv",
            "mp3.flv",
            "mjpeg.swf",
            "mjpeg-zlib.swf",
            "mjpeg-lzma.swf",
        ] {
            let bytes = fs::read(Path::new(CORPUS).join(name)).unwrap();
            files.push((
                name.to_owned(),
                if name.ends_with(".flv") {
                    "corpus.flv"
                } else {
                    "corpus.swf"
                },
                bytes,
            ));
        }
    }
    for (label, name, file) in &files {
        let small = file.len() < 512;
        let cut_step = if small { 1 } else { 7 };
        for cut in (0..file.len()).step_by(cut_step) {
            cases.push((format!("{label} cut at {cut}"), *name, file[..cut].to_vec()));
        }
        let (step, flips): (usize, fn(usize) -> &'static [u8]) = match (full, small) {
            (true, true) => (1, three_flips),
            (true, false) => (5, two_flips),
            (false, _) => (1, alternate_flip),
        };
        for at in (0..file.len()).step_by(step) {
            for &flip in flips(at) {
                let mut bytes = file.clone();
                bytes[at] ^= flip;
                cases.push((format!("{label} byte {at} ^ {flip:#x}"), *name, bytes));
            }
        }
    }
    let checked = cases.len();
    report(judge_all(cases), checked, if full { 5000 } else { 600 });
}

#[test]
fn every_cut_and_a_sampled_flip_of_each_small_file_is_refused_when_7zip_refuses_it() {
    sweep(false);
}

#[test]
#[ignore = "the full sweep runs in release-checks; the sampled one runs in the gate"]
fn every_cut_and_flip_of_small_and_corpus_files_is_refused_when_7zip_refuses_it() {
    sweep(true);
}

fn extract(bytes: &[u8], name: &str, cap: Option<u64>) -> (anyhow::Result<()>, tempfile::TempDir) {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join(name);
    fs::write(&archive, bytes).unwrap();
    let out = work.path().join("out");
    let result = zipper::extract_archive(
        &archive,
        &out,
        zipper::ExtractOptions {
            max_output_bytes: cap,
            ..Default::default()
        },
    );
    if result.is_err() {
        assert!(!out.exists(), "published after a failure");
    }
    (result, work)
}

#[test]
fn a_compressed_swf_is_charged_against_the_cap_before_it_is_decoded() {
    let declared = 64 * 1024 * 1024;
    let mut zlib = with_length(movie(10, 1, &[]), declared);
    zlib = zlib_swf(&zlib, 6);
    zlib.truncate(10);
    zlib.extend_from_slice(b"not deflate at all");
    let (result, _work) = extract(&zlib, "huge.swf", Some(1024 * 1024));
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("--max-extracted-bytes"), "{error}");
    let lzma = lzma_swf_from_parts(
        &with_length(movie(10, 1, &[]), declared),
        0x5D,
        1 << 16,
        b"\0garbage",
    );
    let (result, _work) = extract(&lzma, "huge.swf", Some(1024 * 1024));
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("--max-extracted-bytes"), "{error}");
    let (result, _work) = extract(&zlib, "huge.swf", Some(u64::from(declared)));
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("zlib stream is damaged"), "{error}");
    let tags = movie(
        10,
        1,
        &[swf_tag(1, b"", false), swf_tag(87, &[3; 5000], true)],
    );
    let (result, _work) = extract(&tags, "tags.swf", Some(4999));
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("--max-extracted-bytes 4999"), "{error}");
    let (result, _work) = extract(&tags, "tags.swf", Some(5000));
    result.unwrap();
}

#[test]
fn a_stream_that_decodes_past_its_declared_length_is_refused_at_that_length() {
    let mut big = b"FWS\x0a".to_vec();
    big.extend_from_slice(&0u32.to_le_bytes());
    big.resize(64 * 1024 * 1024, 0);
    for declared in [8, 9, 1000, 64 * 1024 * 1024 - 1] {
        let short = with_length(big.clone(), declared);
        let (result, _work) = extract(&zlib_swf(&short, 9), "bomb.swf", Some(u64::from(declared)));
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains("decodes past the length"),
            "{declared}: {error}"
        );
    }
    let swf = with_length(movie(10, 1, &[swf_tag(87, &[7; 5000], true)]), 1000);
    let (result, _work) = extract(
        &lzma_swf_with_end_marker(&swf, 1 << 16, 3, 0, 2),
        "bomb.swf",
        Some(1000),
    );
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("past the declared size"), "{error}");
    let whole = with_length(big.clone(), big.len() as u32);
    let (result, _work) = extract(&zlib_swf(&whole, 9), "whole.swf", None);
    result.unwrap();
}

#[test]
fn an_lzma_window_over_256_mib_is_refused_before_it_is_allocated() {
    let declared = 512 * 1024 * 1024;
    let swf = with_length(movie(10, 1, &[]), declared);
    let lzma = lzma_swf_from_parts(&swf, 0x5D, 1 << 30, b"\0\0\0\0\0");
    let (result, _work) = extract(&lzma, "window.swf", None);
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("window"), "{error}");
    let small =
        lzma_swf_with_end_marker(&movie(10, 1, &[swf_tag(1, b"", false)]), 1 << 16, 3, 0, 2);
    let mut huge_dictionary = small.clone();
    huge_dictionary[13..17].copy_from_slice(&u32::MAX.to_le_bytes());
    let (result, _work) = extract(&huge_dictionary, "small.swf", None);
    result.unwrap();
}

#[test]
fn an_flv_whose_lone_stream_changes_codec_is_refused_and_two_streams_may() {
    let lone = flv(4, &[audio(2, 0, b"a", 0), audio(3, 0, b"b", 1)]);
    let (result, _work) = extract(&lone, "lone.flv", None);
    assert!(format!("{:#}", result.unwrap_err()).contains("changes codec"));
    let two = flv(
        5,
        &[
            audio(2, 0, b"a", 0),
            audio(3, 0, b"b", 1),
            video(1, 2, b"v", 0),
        ],
    );
    let (result, _work) = extract(&two, "two.flv", None);
    result.unwrap();
}

#[test]
fn an_swf_of_more_than_a_million_tags_is_refused() {
    let work = tempfile::tempdir().unwrap();
    for (count, refused) in [(1_000_000, false), (1_000_001, true)] {
        let archive = work.path().join(format!("{count}.swf"));
        let tags = vec![swf_tag(1, b"", false); count];
        fs::write(&archive, movie(10, 0, &tags)).unwrap();
        let read = zipper::native_swf::read_archive(&archive, None, Default::default());
        assert_eq!(
            read.is_err(),
            refused,
            "{count}: {:?}",
            read.map(|entries| entries.len())
        );
    }
}
