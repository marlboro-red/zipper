//! Stripping leading folders through the public API, on every archive of the
//! ZIP, tar and 7z corpora. Each strip, by folder or by count, must give the
//! manifest's tree with the same folders dropped, computed here from path
//! segments alone. Where the stripped tree would hold two entries at one path,
//! or a link that leaves the destination, or when a count strips every
//! entry away, the extraction must fail and publish nothing.
mod common;
#[path = "common/corpus.rs"]
mod corpus;

use std::path::{Path, PathBuf};
use zipper::{Engine, ExtractOptions, Strip, extract_archive};

fn data(folder: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(folder)
}

fn archive_file(corpus: &Path, name: &str) -> PathBuf {
    std::fs::read_dir(corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let file = path.file_name().unwrap().to_str().unwrap();
            file.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('.') && rest != ".tsv" && rest != ".json")
        })
        .unwrap_or_else(|| panic!("{name} has no archive file"))
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').collect()
}

fn stripped(tree: &corpus::Tree, keep: impl Fn(&[&str]) -> Option<usize>) -> Option<corpus::Tree> {
    let mut placed = corpus::Tree::new();
    for (path, value) in tree {
        let parts = segments(path);
        let Some(drop) = keep(&parts) else { continue };
        if drop >= parts.len() {
            continue;
        }
        let rest = parts[drop..].join("/");
        if let Some(target) = &value.link_target {
            let mut depth = rest.matches('/').count() as i64;
            for part in target.split('/') {
                match part {
                    "" | "." => {}
                    ".." => depth -= 1,
                    _ => depth += 1,
                }
                if depth < 0 {
                    return None;
                }
            }
        }
        if placed.insert(rest, value.clone()).is_some() {
            return None;
        }
    }
    Some(placed)
}

fn check(
    archive: &Path,
    engine: Engine,
    strip: Strip,
    expected: Option<corpus::Tree>,
    label: &str,
) {
    let work = tempfile::tempdir().unwrap();
    let destination = work.path().join("out");
    let result = extract_archive(
        archive,
        &destination,
        ExtractOptions {
            engine,
            strip,
            ..Default::default()
        },
    );
    match expected {
        Some(tree) => {
            result.unwrap_or_else(|error| panic!("{label}: {error:#}"));
            corpus::assert_tree_matches_manifest(work.path(), &tree, label);
        }
        None => {
            assert!(result.is_err(), "{label} was accepted");
            assert!(!destination.exists(), "{label} published after a failure");
            common::make_tree_deletable(work.path());
        }
    }
}

#[test]
fn every_corpus_archive_strips_to_its_manifest_without_the_leading_folders() {
    let mut checked = 0;
    for (folder, engines) in [
        ("zip-corpus", &[Engine::Native, Engine::NativeFast][..]),
        ("tar-corpus", &[Engine::Native]),
        ("7z-corpus", &[Engine::Native]),
    ] {
        let corpus = data(folder);
        for (name, tree) in corpus::expected(&corpus) {
            let archive = archive_file(&corpus, &name);
            let folders: Vec<&String> = tree
                .iter()
                .filter(|(_, value)| value.is_directory())
                .map(|(path, _)| path)
                .collect();
            for &engine in engines {
                for count in 1..=3 {
                    let expected = stripped(&tree, |_| Some(count)).filter(|tree| !tree.is_empty());
                    let label = format!("{name} {engine:?} less {count}");
                    check(&archive, engine, Strip::Components(count), expected, &label);
                    checked += 1;
                }
                for prefix in &folders {
                    let prefix_parts = segments(prefix);
                    let expected = stripped(&tree, |parts| {
                        parts
                            .starts_with(&prefix_parts)
                            .then_some(prefix_parts.len())
                    });
                    let label = format!("{name} {engine:?} under {prefix}");
                    check(&archive, engine, Strip::Prefix(prefix), expected, &label);
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 500, "{checked}");
}

fn tar_of(path: &Path, names: &[&str]) {
    let mut builder = Vec::new();
    for name in names {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let directory = name.ends_with('/');
        header[100..108].copy_from_slice(if directory {
            b"0000755\0"
        } else {
            b"0000644\0"
        });
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size = if directory { 0 } else { name.len() };
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[156] = if directory { b'5' } else { b'0' };
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        builder.extend_from_slice(&header);
        if !directory {
            let mut data = name.as_bytes().to_vec();
            data.resize(512, 0);
            builder.extend_from_slice(&data);
        }
    }
    builder.resize(builder.len() + 1024, 0);
    std::fs::write(path, builder).unwrap();
}

fn files(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .replace('\\', "/")
        })
        .collect();
    found.sort();
    found
}

#[test]
fn a_dot_segment_counts_as_tar_counts_it() {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("dot.tar");
    tar_of(
        &archive,
        &["./", "./pkg/", "./pkg/a.txt", "./pkg/sub/b.txt"],
    );
    for (count, expected) in [
        (1, &["pkg/a.txt", "pkg/sub/b.txt"][..]),
        (2, &["a.txt", "sub/b.txt"]),
        (3, &["b.txt"]),
    ] {
        let destination = work.path().join(format!("out{count}"));
        extract_archive(
            &archive,
            &destination,
            ExtractOptions {
                strip: Strip::Components(count),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(files(&destination), expected, "less {count}");
    }
    let destination = work.path().join("prefix");
    extract_archive(
        &archive,
        &destination,
        ExtractOptions {
            strip: Strip::Prefix("./pkg"),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(files(&destination), ["a.txt", "sub/b.txt"]);
}

#[test]
fn a_strip_that_leaves_nothing_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("dot.tar");
    tar_of(&archive, &["pkg/", "pkg/a.txt", "top.txt"]);
    let corpus = data("zip-corpus");
    let zip = archive_file(&corpus, "7zip-deflate");
    let only_top = ["top.txt".to_string()];
    for (archive, strip, paths) in [
        (&archive, Strip::Components(usize::MAX), &[][..]),
        (&archive, Strip::Components(9), &[][..]),
        (&archive, Strip::Prefix("pkg"), &only_top[..]),
        (&zip, Strip::Components(usize::MAX), &[][..]),
    ] {
        let destination = work.path().join("refused");
        let result = extract_archive(
            archive,
            &destination,
            ExtractOptions {
                strip,
                paths,
                ..Default::default()
            },
        );
        let error = format!("{:#}", result.expect_err(&format!("{strip:?}")));
        assert!(
            error.contains("leaves nothing to extract"),
            "{strip:?}: {error}"
        );
        assert!(!destination.exists());
    }
}
