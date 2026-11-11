//! The legacy, Deflate64, BZip2, LZMA and XZ, and PPMd methods and ZIP
//! encryption each have a Cargo feature on top of `zip-deflate`. A build
//! reads an entry whose method and encryption it has, and refuses one it
//! leaves out with `NotBuilt` naming the feature to enable, writing nothing.
//! 7-Zip reads each fixture in `tests/data/zip-methods` back to the text the
//! reader must give.
#![cfg(feature = "zip-deflate")]

#[path = "common/sevenzip.rs"]
mod sevenzip;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use zipper::{Engine, ExtractOptions, NotBuilt, extract_archive};

const TEXT_LINE: &[u8] = b"hello from every ZIP method\n";
const TEXT_LINES: usize = 20;
const TINY_TEXT: &[u8] = b"attack at dawn, attack at dawn, attack at dawn; bring the ladder\n";
const PASSWORD: &[u8] = b"zipper";
const ENCRYPTION: &str = "zip-encryption";

#[derive(Clone, Copy)]
struct Method {
    feature: &'static str,
    built: bool,
}

const BASE: Method = Method {
    feature: "zip-deflate",
    built: true,
};
const BZIP2: Method = Method {
    feature: "zip-bzip2",
    built: cfg!(feature = "zip-bzip2"),
};
const DEFLATE64: Method = Method {
    feature: "zip-deflate64",
    built: cfg!(feature = "zip-deflate64"),
};
const LZMA: Method = Method {
    feature: "zip-lzma",
    built: cfg!(feature = "zip-lzma"),
};
const PPMD: Method = Method {
    feature: "zip-ppmd",
    built: cfg!(feature = "zip-ppmd"),
};
const LEGACY: Method = Method {
    feature: "zip-legacy",
    built: cfg!(feature = "zip-legacy"),
};

struct Case {
    archive: &'static str,
    entries: &'static [(Method, Encrypted)],
    file: &'static str,
    content: Content,
}

#[derive(Clone, Copy, PartialEq)]
enum Encrypted {
    No,
    Yes,
}

enum Content {
    Text,
    Tiny,
    Length(usize),
    Sha256(&'static str),
}

fn text() -> Vec<u8> {
    TEXT_LINE.repeat(TEXT_LINES)
}

const CASES: &[Case] = &[
    Case {
        archive: "zip-methods/bzip2.zip",
        entries: &[(BZIP2, Encrypted::No)],
        file: "hello.txt",
        content: Content::Text,
    },
    Case {
        archive: "zip-methods/deflate64.zip",
        entries: &[(DEFLATE64, Encrypted::No)],
        file: "hello.txt",
        content: Content::Text,
    },
    Case {
        archive: "zip-methods/lzma.zip",
        entries: &[(LZMA, Encrypted::No)],
        file: "hello.txt",
        content: Content::Text,
    },
    Case {
        archive: "zip-methods/xz.zip",
        entries: &[(LZMA, Encrypted::No)],
        file: "hello.txt",
        content: Content::Text,
    },
    Case {
        archive: "zip-methods/ppmd.zip",
        entries: &[(PPMD, Encrypted::No)],
        file: "hello.txt",
        content: Content::Text,
    },
    Case {
        archive: "zip-legacy/shrink.zip",
        entries: &[(LEGACY, Encrypted::No)],
        file: "FIRST.TXT",
        content: Content::Length(1092),
    },
    Case {
        archive: "zip-legacy/implode.zip",
        entries: &[(LEGACY, Encrypted::No)],
        file: "first.txt",
        content: Content::Length(1092),
    },
    Case {
        archive: "encrypted-zip/tiny-zipcrypto.zip",
        entries: &[(BASE, Encrypted::Yes)],
        file: "secret.txt",
        content: Content::Tiny,
    },
    Case {
        archive: "encrypted-zip/tiny-aes256.zip",
        entries: &[(BASE, Encrypted::Yes)],
        file: "secret.txt",
        content: Content::Tiny,
    },
    Case {
        archive: "encrypted-zip/aes256-bzip2.zip",
        entries: &[
            (BASE, Encrypted::Yes),
            (BASE, Encrypted::Yes),
            (BASE, Encrypted::Yes),
            (BASE, Encrypted::No),
            (BASE, Encrypted::Yes),
            (BZIP2, Encrypted::Yes),
        ],
        file: "sub/text.txt",
        content: Content::Sha256(
            "69d3d96cd89274ffdb556769c9e765b68dba1147a03522601695ab671fa60864",
        ),
    },
];

impl Case {
    fn missing_feature(&self) -> Option<&'static str> {
        self.entries.iter().find_map(|&(method, encrypted)| {
            if !method.built {
                Some(method.feature)
            } else if encrypted == Encrypted::Yes && cfg!(not(feature = "zip-encryption")) {
                Some(ENCRYPTION)
            } else {
                None
            }
        })
    }

    fn encrypted(&self) -> bool {
        self.entries
            .iter()
            .any(|&(_, encrypted)| encrypted == Encrypted::Yes)
    }

    fn path(&self) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(self.archive)
    }

    fn extract(&self, engine: Engine, destination: &Path) -> anyhow::Result<()> {
        extract_archive(
            &self.path(),
            destination,
            ExtractOptions {
                engine,
                password: self.encrypted().then_some(PASSWORD),
                ..ExtractOptions::default()
            },
        )
    }

    fn assert_extracted(&self, bytes: &[u8]) {
        match self.content {
            Content::Text => assert_eq!(bytes, text(), "{}", self.archive),
            Content::Tiny => assert_eq!(bytes, TINY_TEXT, "{}", self.archive),
            Content::Length(length) => assert_eq!(bytes.len(), length, "{}", self.archive),
            Content::Sha256(digest) => {
                let found: String = Sha256::digest(bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                assert_eq!(found, digest, "{}", self.archive);
            }
        }
    }
}

#[test]
fn an_entry_reads_when_its_method_and_encryption_are_built_and_is_refused_otherwise() {
    for case in CASES {
        for engine in [Engine::Native, Engine::Auto] {
            let work = tempfile::tempdir().unwrap();
            let destination = work.path().join("out");
            let extracted = case.extract(engine, &destination);
            match case.missing_feature() {
                Some(feature) => {
                    let error = extracted.expect_err(case.archive);
                    let not_built = error
                        .downcast_ref::<NotBuilt>()
                        .unwrap_or_else(|| panic!("{}: {error:#}", case.archive));
                    assert_eq!(not_built.0, feature, "{}", case.archive);
                    assert_eq!(
                        std::fs::read_dir(work.path()).unwrap().count(),
                        0,
                        "{} wrote output",
                        case.archive
                    );
                }
                None => {
                    extracted.unwrap_or_else(|error| panic!("{}: {error:#}", case.archive));
                    case.assert_extracted(&std::fs::read(destination.join(case.file)).unwrap());
                }
            }
        }
    }
}

#[test]
fn stored_and_deflate_entries_read_in_every_zip_build() {
    for archive in [
        "zip-corpus/python-stored.zip",
        "zip-corpus/7zip-deflate.zip",
    ] {
        let work = tempfile::tempdir().unwrap();
        let destination = work.path().join("out");
        extract_archive(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/data")
                .join(archive),
            &destination,
            ExtractOptions {
                engine: Engine::Native,
                ..ExtractOptions::default()
            },
        )
        .unwrap_or_else(|error| panic!("{archive}: {error:#}"));
        assert!(std::fs::read_dir(&destination).unwrap().count() > 0);
    }
}

#[test]
fn seven_zip_reads_each_method_fixture_back_to_its_text() {
    let Some(seven_zip) = sevenzip::sevenzip() else {
        return;
    };
    for case in CASES
        .iter()
        .filter(|case| case.archive.starts_with("zip-methods/"))
    {
        let work = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(&seven_zip)
            .arg("x")
            .arg(case.path())
            .arg(format!("-o{}", work.path().display()))
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "{}", case.archive);
        assert_eq!(
            std::fs::read(work.path().join(case.file)).unwrap(),
            text(),
            "{}",
            case.archive
        );
    }
}
