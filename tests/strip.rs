//! Stripping leading folders through the public API, on every archive of the
//! ZIP, tar and 7z corpora. Each strip, by folder or by count, must give the
//! manifest's tree with the same folders dropped, computed here from path
//! segments alone. Where the stripped tree would hold two entries at one path,
//! or a link that leaves the destination, the extraction must fail and
//! publish nothing.
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
                    let expected = stripped(&tree, |_| Some(count));
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
