//! Seeded mutation testing of the native ZIP readers on the reference corpus.
//! Each case mutates one corpus archive and extracts it with both engines.
//! A case passes when a reader rejects it cleanly, leaving nothing behind, or
//! extracts it and verification agrees. Any panic fails.
//!
//! The gate runs a short pass. Set `ZIPPER_FUZZ_CASES` for a longer run and
//! `ZIPPER_FUZZ_SEED` to replay a reported failure. This is random mutation,
//! not coverage-guided fuzzing.
mod common;

use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::Path,
};
use zipper::{
    Entry,
    native_zip::{read_archive, read_archive_fast},
};

type Reader = fn(&Path, Option<&Path>) -> anyhow::Result<Vec<Entry>>;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

const SIGNATURES: [&[u8]; 4] = [b"PK\x01\x02", b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"];
const EDGE_VALUES: [u32; 6] = [0, 1, 0x7fff_ffff, 0xffff_fffe, 0xffff_ffff, 0x0001_0000];

fn overwrite_field_near_signature(bytes: &mut [u8], rng: &mut Rng) {
    let signature = SIGNATURES[rng.below(SIGNATURES.len())];
    let starts: Vec<usize> = bytes
        .windows(4)
        .enumerate()
        .filter(|(_, window)| window == &signature)
        .map(|(at, _)| at)
        .collect();
    if starts.is_empty() {
        return;
    }
    let at = starts[rng.below(starts.len())] + 4 + rng.below(42);
    let value = EDGE_VALUES[rng.below(EDGE_VALUES.len())].to_le_bytes();
    let width = [1, 2, 4][rng.below(3)];
    for (offset, byte) in value.iter().take(width).enumerate() {
        if let Some(slot) = bytes.get_mut(at + offset) {
            *slot = *byte;
        }
    }
}

fn mutate(archive: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut bytes = archive.to_vec();
    for _ in 0..1 + rng.below(3) {
        if bytes.is_empty() {
            break;
        }
        match rng.below(5) {
            0 => {
                let at = rng.below(bytes.len());
                bytes[at] ^= 1 << rng.below(8);
            }
            1 => {
                let len = bytes.len();
                bytes.truncate(rng.below(len));
            }
            2 => {
                let at = rng.below(bytes.len());
                let count = 1 + rng.below(16);
                let insert: Vec<u8> = (0..count).map(|_| rng.next() as u8).collect();
                bytes.splice(at..at, insert);
            }
            _ => overwrite_field_near_signature(&mut bytes, rng),
        }
    }
    bytes
}

fn reader_accepts_cleanly(bytes: &[u8], read: Reader) -> bool {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.zip");
    fs::write(&archive, bytes).unwrap();
    let parent = root.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let destination = parent.join("out");
    let extracted = read(&archive, Some(&destination));
    let verified = read(&archive, None);
    common::make_tree_deletable(root.path());
    let left: Vec<_> = fs::read_dir(&parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    match extracted {
        Err(_) => {
            assert!(left.is_empty(), "rejected archive left {left:?}");
            false
        }
        Ok(entries) => {
            assert_eq!(left, ["out"], "extraction left {left:?}");
            let verified = verified.expect("extraction succeeded but verification failed");
            assert_eq!(entries, verified, "extraction and verification disagree");
            let declared: u64 = entries.iter().map(|entry| entry.size).sum();
            let written: u64 = walkdir::WalkDir::new(&destination)
                .into_iter()
                .map(|entry| entry.unwrap().metadata().unwrap())
                // EXTERNAL(POSIX lstat): a link's size is its target's length.
                .filter(|metadata| metadata.is_file() || metadata.file_type().is_symlink())
                .map(|metadata| metadata.len())
                .sum();
            assert_eq!(
                written, declared,
                "written bytes differ from declared sizes"
            );
            true
        }
    }
}

#[test]
fn mutated_corpus_archives_never_panic_or_leak_output() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/zip-corpus");
    let mut archives: Vec<_> = fs::read_dir(&corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "zip"))
        .collect();
    archives.sort();
    let archives: Vec<Vec<u8>> = archives
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
    let cases: u64 =
        std::env::var("ZIPPER_FUZZ_CASES").map_or(1000, |value| value.parse().unwrap());
    let first: u64 = std::env::var("ZIPPER_FUZZ_SEED").map_or(1, |value| value.parse().unwrap());
    let mut accepted = 0u64;
    for seed in first..first + cases {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let source = &archives[rng.below(archives.len())];
        let mutant = mutate(source, &mut rng);
        for (engine, read) in [
            ("native", read_archive as Reader),
            ("native-fast", read_archive_fast as Reader),
        ] {
            match catch_unwind(AssertUnwindSafe(|| reader_accepts_cleanly(&mutant, read))) {
                Ok(true) => accepted += 1,
                Ok(false) => {}
                Err(panic) => {
                    eprintln!(
                        "failing case: ZIPPER_FUZZ_SEED={seed} ZIPPER_FUZZ_CASES=1, engine {engine}"
                    );
                    resume_unwind(panic);
                }
            }
        }
    }
    eprintln!("{accepted} of {} extractions accepted a mutant", 2 * cases);
}
