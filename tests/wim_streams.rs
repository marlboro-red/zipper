//! NTFS alternate data streams in WIM images. Without `alternate_streams`
//! a named stream is refused. With it, stream names and data are checked on
//! every platform, written as NTFS streams on Windows and refused for
//! extraction elsewhere. 7-Zip, where installed outside Windows, checks that
//! the test writer stores streams as 7-Zip reads them.
#[path = "common/mod.rs"]
mod common;
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/wim_writer.rs"]
mod wim_writer;

use std::{fs, path::Path};
use wim_writer::{Node, Options, dir, file, utf16};
use zipper::native_wim::{ReadOptions, read_archive};

const WITH_STREAMS: ReadOptions = ReadOptions {
    max_output_bytes: None,
    existing: zipper::Existing::Refuse,
    paths: &[],
    include: &[],
    exclude: &[],
    strip: zipper::Strip::None,
    metadata: zipper::Metadata::Restore,
    control: zipper::Control {
        progress: None,
        cancel: None,
    },
    alternate_streams: true,
};

fn with_streams(name: &str, data: &[u8], attributes: u32, streams: &[(&str, &[u8])]) -> Node {
    Node::File {
        name: name.into(),
        data: data.to_vec(),
        attributes,
        link_group: 0,
        streams: streams
            .iter()
            .map(|(name, data)| ((*name).to_owned(), data.to_vec()))
            .collect(),
    }
}

fn sample() -> Vec<Vec<Node>> {
    vec![vec![
        with_streams(
            "base.txt",
            b"main data\n",
            0x80,
            &[
                ("Zone.Identifier", b"[ZoneTransfer]\r\nZoneId=3\r\n"),
                ("second", &b"0123456789".repeat(5000)),
                ("empty", b""),
            ],
        ),
        with_streams("locked.txt", b"read only\n", 0x01, &[("note", b"locked")]),
        with_streams("hollow", b"", 0x80, &[("only", b"stream of an empty file")]),
        dir("folder", vec![file("plain.txt", b"plain\n")]),
    ]]
}

fn write(images: &[Vec<Node>]) -> Vec<u8> {
    wim_writer::write(images, &Options::default())
}

fn with_metadata(images: &[Vec<Node>], hook: impl Fn(&mut Vec<u8>) + 'static) -> Vec<u8> {
    wim_writer::write(
        images,
        &Options {
            metadata_hook: Some(Box::new(move |_, meta| hook(meta))),
            ..Options::default()
        },
    )
}

fn find(meta: &[u8], needle: &[u8]) -> usize {
    meta.windows(needle.len())
        .position(|window| window == needle)
        .expect("the metadata holds the bytes")
}

fn dentry_of(meta: &[u8], name: &str) -> usize {
    let mut needle = (utf16(name).len() as u16).to_le_bytes().to_vec();
    needle.extend(utf16(name));
    find(meta, &needle) + 2 - wim_writer::DENTRY
}

fn archive_in(root: &Path, wim: &[u8]) -> std::path::PathBuf {
    let archive = root.join("streams.wim");
    fs::write(&archive, wim).unwrap();
    archive
}

#[track_caller]
fn refused(wim: &[u8], options: ReadOptions, destination: bool, because: &str) {
    let root = tempfile::tempdir().unwrap();
    let archive = archive_in(root.path(), wim);
    let into = root.path().join("out");
    let error = read_archive(&archive, destination.then_some(into.as_path()), options)
        .expect_err("the WIM was accepted");
    let error = format!("{error:#}");
    assert!(error.contains(because), "{because:?} not in {error:?}");
    assert!(!into.exists(), "a refused WIM published {}", into.display());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    common::make_tree_deletable(root.path());
}

#[test]
fn without_the_option_a_named_stream_is_refused_and_the_error_names_the_option() {
    for destination in [true, false] {
        refused(
            &write(&sample()),
            ReadOptions::default(),
            destination,
            "`zipper extract --alternate-streams` writes it",
        );
    }
}

#[test]
fn with_the_option_a_test_reads_and_checks_every_stream() {
    let root = tempfile::tempdir().unwrap();
    let archive = archive_in(root.path(), &write(&sample()));
    let mut listed: Vec<String> = read_archive(&archive, None, WITH_STREAMS)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    listed.sort();
    assert_eq!(
        listed,
        [
            "base.txt",
            "folder",
            "folder/plain.txt",
            "hollow",
            "locked.txt"
        ]
    );
}

#[test]
fn a_stream_whose_data_fails_its_sha1_is_refused() {
    let images = sample();
    let wim = wim_writer::write(
        &images,
        &Options {
            file_hook: Some(Box::new(|wim, _| {
                let at = find(wim, b"stream of an empty file");
                wim[at] ^= 1;
            })),
            ..Options::default()
        },
    );
    refused(&wim, WITH_STREAMS, false, "SHA-1");
}

#[test]
fn a_stream_whose_data_the_blob_table_lacks_is_refused() {
    let wim = with_metadata(&sample(), |meta| {
        let name = utf16("note");
        let mut needle = (name.len() as u16).to_le_bytes().to_vec();
        needle.extend(&name);
        let hash = find(meta, &needle) - 20;
        meta[hash] ^= 1;
    });
    refused(
        &wim,
        WITH_STREAMS,
        false,
        "whose data the blob table does not have",
    );
}

#[test]
fn stream_names_that_windows_would_change_or_refuse_are_refused() {
    for (name, because) in [
        ("a/b", "path separator"),
        ("a\\b", "path separator"),
        ("a:b", "colon"),
        ("a\u{1}b", "control character"),
        ("trailing.", "ends with a dot or a space"),
        ("CON", "reserved Windows device name"),
        ("lpt1.txt", "reserved Windows device name"),
        (&"x".repeat(256), "longer than the 255 UTF-16 units"),
    ] {
        let images = vec![vec![with_streams("base", b"data", 0x80, &[(name, b"x")])]];
        refused(&write(&images), WITH_STREAMS, false, because);
        refused(&write(&images), WITH_STREAMS, true, because);
    }
}

#[test]
fn two_streams_whose_names_ntfs_compares_equal_are_refused() {
    let images = vec![vec![with_streams(
        "base",
        b"data",
        0x80,
        &[("Second", b"1"), ("sECOND", b"2")],
    )]];
    refused(
        &write(&images),
        WITH_STREAMS,
        false,
        "two alternate data streams",
    );
}

#[test]
fn names_that_only_full_case_mapping_makes_equal_are_two_streams() {
    let images = vec![vec![with_streams(
        "base",
        b"data",
        0x80,
        &[
            ("stra\u{df}e", b"1"),
            ("STRASSE", b"2"),
            ("\u{fb00}", b"3"),
            ("FF", b"4"),
        ],
    )]];
    let root = tempfile::tempdir().unwrap();
    let archive = archive_in(root.path(), &write(&images));
    read_archive(&archive, None, WITH_STREAMS).unwrap();
}

fn linked(name: &str, streams: &[(&str, &[u8])]) -> Node {
    match with_streams(name, b"shared data", 0x80, streams) {
        Node::File {
            name,
            data,
            attributes,
            streams,
            ..
        } => Node::File {
            name,
            data,
            attributes,
            link_group: 9,
            streams,
        },
        _ => unreachable!(),
    }
}

#[test]
fn hard_linked_files_must_store_the_same_streams() {
    let same = vec![vec![
        linked("a", &[("s", b"linked stream")]),
        linked("b", &[("s", b"linked stream")]),
    ]];
    let root = tempfile::tempdir().unwrap();
    let archive = archive_in(root.path(), &write(&same));
    read_archive(&archive, None, WITH_STREAMS).unwrap();
    for (first, second) in [
        (vec![], vec![("t", &b"follower only"[..])]),
        (vec![("s", &b"one"[..])], vec![("s", &b"two"[..])]),
        (vec![("s", &b"one"[..])], vec![("t", &b"one"[..])]),
        (vec![("s", &b"one"[..])], vec![]),
    ] {
        let images = vec![vec![linked("a", &first), linked("b", &second)]];
        refused(
            &write(&images),
            WITH_STREAMS,
            false,
            "other alternate data streams",
        );
    }
}

#[test]
fn a_file_left_out_still_has_its_stream_data_checked() {
    let images = vec![vec![
        with_streams("kept", b"kept data", 0x80, &[]),
        dir(
            "out",
            vec![with_streams(
                "left",
                b"left data",
                0x80,
                &[("s", b"left stream")],
            )],
        ),
    ]];
    let wim = wim_writer::write(
        &images,
        &Options {
            file_hook: Some(Box::new(|wim, _| {
                let at = find(wim, b"left stream");
                wim[at] ^= 1;
            })),
            ..Options::default()
        },
    );
    let paths = ["kept".to_owned()];
    refused(
        &wim,
        ReadOptions {
            paths: &paths,
            ..WITH_STREAMS
        },
        false,
        "SHA-1",
    );
    refused(
        &wim,
        ReadOptions {
            strip: zipper::Strip::Prefix("out"),
            ..WITH_STREAMS
        },
        false,
        "SHA-1",
    );
}

#[test]
fn stream_data_counts_against_the_output_cap_without_a_destination() {
    let images = vec![vec![with_streams(
        "base",
        b"x",
        0x80,
        &[("big", &[7u8; 100_000])],
    )]];
    refused(
        &write(&images),
        ReadOptions {
            max_output_bytes: Some(1000),
            ..WITH_STREAMS
        },
        false,
        "--max-extracted-bytes",
    );
}

#[test]
fn a_stream_name_that_is_not_utf16_is_refused() {
    let images = vec![vec![with_streams("base", b"data", 0x80, &[("ab", b"x")])]];
    let wim = with_metadata(&images, |meta| {
        let at = find(meta, &utf16("ab"));
        meta[at..at + 2].copy_from_slice(&0xD800u16.to_le_bytes());
    });
    refused(&wim, WITH_STREAMS, false, "not valid UTF-16");
}

#[test]
fn streams_of_folders_and_links_are_refused() {
    let images = vec![vec![with_streams("holder", b"", 0x80, &[("s", b"x")])]];
    let folder = with_metadata(&images, |meta| {
        let at = dentry_of(meta, "holder") + 8;
        meta[at..at + 4].copy_from_slice(&0x10u32.to_le_bytes());
    });
    refused(&folder, WITH_STREAMS, false, "WIM folder \"holder\"");
    let body = wim_writer::symlink_reparse_body("target", true);
    let images = vec![vec![
        file("target", b"t"),
        with_streams("link", &body, 0x80, &[("s", b"x")]),
    ]];
    let link = with_metadata(&images, |meta| {
        let at = dentry_of(meta, "link");
        meta[at + 8..at + 12].copy_from_slice(&0x400u32.to_le_bytes());
        meta[at + 0x58..at + 0x60].copy_from_slice(&0xA000_000Cu64.to_le_bytes());
    });
    refused(&link, WITH_STREAMS, false, "WIM link \"link\"");
}

#[test]
fn the_option_is_refused_for_an_archive_that_is_not_a_wim() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("plain.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    zip.start_file("a.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    std::io::Write::write_all(&mut zip, b"a").unwrap();
    zip.finish().unwrap();
    let into = root.path().join("out");
    let error = zipper::extract_archive(
        &archive,
        &into,
        zipper::ExtractOptions {
            alternate_streams: true,
            ..Default::default()
        },
    )
    .expect_err("the option was accepted for a ZIP");
    assert!(
        format!("{error:#}").contains("applies only to WIM images"),
        "{error:#}"
    );
    assert!(!into.exists());
}

#[cfg(not(windows))]
#[test]
fn off_windows_extraction_with_the_option_is_refused_and_publishes_nothing() {
    refused(
        &write(&sample()),
        WITH_STREAMS,
        true,
        "writes NTFS streams only on Windows",
    );
}

#[cfg(not(windows))]
#[test]
fn off_windows_a_wim_without_streams_extracts_with_the_option() {
    let root = tempfile::tempdir().unwrap();
    let archive = archive_in(root.path(), &write(&[vec![file("a.txt", b"a\n")]]));
    let into = root.path().join("out");
    read_archive(&archive, Some(&into), WITH_STREAMS).unwrap();
    assert_eq!(fs::read(into.join("a.txt")).unwrap(), b"a\n");
}

#[test]
#[ignore = "writes a WIM of 640,000 files with a named stream each, for a peak memory measurement"]
fn write_a_wim_of_streams_near_the_limits() {
    let out = std::env::var_os("WIM_STREAMS_OUT").expect("set WIM_STREAMS_OUT to a file");
    let stream = "s".repeat(60);
    let root: Vec<Node> = (0..640)
        .map(|folder| {
            let children = (0..1000)
                .map(|index| {
                    let number = (folder * 1000 + index) as u64;
                    with_streams(
                        &format!("{number:07}"),
                        &number.to_le_bytes(),
                        0x80,
                        &[(&stream, b"")],
                    )
                })
                .collect();
            dir(&format!("d{folder:04}"), children)
        })
        .collect();
    fs::write(out, write(&[root])).unwrap();
}

#[test]
#[ignore = "tests the WIM named by WIM_STREAMS_IN with alternate streams, for a peak memory measurement"]
fn test_a_wim_with_streams() {
    let archive = std::env::var_os("WIM_STREAMS_IN").expect("set WIM_STREAMS_IN to a WIM");
    let entries = read_archive(Path::new(&archive), None, WITH_STREAMS).unwrap();
    println!("{} entries", entries.len());
}

#[cfg(not(windows))]
#[test]
fn the_test_writer_stores_streams_as_7zip_reads_them() {
    let Some(sevenzip) = sevenzip::sevenzip() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let archive = archive_in(root.path(), &write(&sample()));
    let into = root.path().join("7z");
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(&into);
    let run = std::process::Command::new(sevenzip)
        .args(["x", "-y"])
        .arg(flag)
        .arg(&archive)
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    for (file, stream, data) in expected_streams() {
        assert_eq!(
            fs::read(into.join(format!("{file}:{stream}"))).unwrap(),
            data,
            "{file}:{stream}"
        );
    }
    common::make_tree_deletable(root.path());
}

fn expected_streams() -> Vec<(String, String, Vec<u8>)> {
    let mut expected = Vec::new();
    for node in &sample()[0] {
        if let Node::File { name, streams, .. } = node {
            for (stream, data) in streams {
                expected.push((name.clone(), stream.clone(), data.clone()));
            }
        }
    }
    expected
}

#[cfg(windows)]
mod on_windows {
    use super::*;

    fn stream(file: &Path, name: &str) -> std::io::Result<Vec<u8>> {
        let mut path = file.as_os_str().to_owned();
        path.push(":");
        path.push(name);
        fs::read(path)
    }

    fn extract(wim: &[u8], options: ReadOptions) -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let archive = archive_in(root.path(), wim);
        let into = root.path().join("out");
        read_archive(&archive, Some(&into), options).unwrap();
        (root, into)
    }

    #[test]
    fn every_stream_is_written_after_its_file_and_reads_back_byte_for_byte() {
        let (root, into) = extract(&write(&sample()), WITH_STREAMS);
        for (file, name, data) in expected_streams() {
            assert_eq!(
                stream(&into.join(&file), &name).unwrap(),
                data,
                "{file}:{name}"
            );
        }
        assert_eq!(fs::read(into.join("base.txt")).unwrap(), b"main data\n");
        assert_eq!(fs::read(into.join("hollow")).unwrap(), b"");
        assert!(
            fs::metadata(into.join("locked.txt"))
                .unwrap()
                .permissions()
                .readonly()
        );
        let modified = |name: &str| fs::metadata(into.join(name)).unwrap().modified().unwrap();
        assert_eq!(modified("base.txt"), modified("folder/plain.txt"));
        common::make_tree_deletable(root.path());
    }

    #[test]
    fn an_unselected_file_gets_no_streams_and_a_selected_one_keeps_them() {
        let paths = ["base.txt".to_owned()];
        let (root, into) = extract(
            &write(&sample()),
            ReadOptions {
                paths: &paths,
                ..WITH_STREAMS
            },
        );
        assert_eq!(
            stream(&into.join("base.txt"), "second").unwrap().len(),
            50_000
        );
        assert!(!into.join("locked.txt").exists());
        common::make_tree_deletable(root.path());
    }

    #[test]
    fn content_only_writes_every_stream_but_the_mark_of_the_web() {
        let (root, into) = extract(
            &write(&sample()),
            ReadOptions {
                metadata: zipper::Metadata::ContentOnly,
                ..WITH_STREAMS
            },
        );
        let base = into.join("base.txt");
        assert!(stream(&base, "Zone.Identifier").is_err());
        assert_eq!(stream(&base, "second").unwrap().len(), 50_000);
        common::make_tree_deletable(root.path());
    }

    #[test]
    fn hard_linked_files_share_their_streams() {
        let linked = |name: &str| Node::File {
            name: name.into(),
            data: b"shared".to_vec(),
            attributes: 0x80,
            link_group: 7,
            streams: vec![("s".into(), b"linked stream".to_vec())],
        };
        let (root, into) = extract(&write(&[vec![linked("a"), linked("b")]]), WITH_STREAMS);
        for name in ["a", "b"] {
            assert_eq!(stream(&into.join(name), "s").unwrap(), b"linked stream");
        }
        common::make_tree_deletable(root.path());
    }

    #[test]
    fn a_failure_after_streams_were_written_publishes_nothing() {
        let images = vec![vec![
            with_streams("first", b"first data", 0x80, &[("s", b"first stream")]),
            file("second", b"second data"),
        ]];
        let wim = wim_writer::write(
            &images,
            &Options {
                file_hook: Some(Box::new(|wim, _| {
                    let at = find(wim, b"second data");
                    wim[at] ^= 1;
                })),
                ..Options::default()
            },
        );
        refused(&wim, WITH_STREAMS, true, "SHA-1");
    }
}
