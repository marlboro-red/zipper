//! Seeded mutation testing of the native 7z reader on the reference corpus.
//! Each case mutates one corpus archive and usually repairs both header CRCs,
//! so the damage reaches the parser and the decoders. A case passes when the
//! reader rejects it, leaving nothing behind, or extracts it, verification
//! agrees, and every file has its declared size. Any panic fails.
mod common;

use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
};
use zipper::native_7z::{ReadOptions, read_archive};

const CASES: &str = "ZIPPER_FUZZ_CASES";
const FIRST_SEED: &str = "ZIPPER_FUZZ_SEED";

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

fn header_range(bytes: &[u8]) -> Option<std::ops::Range<usize>> {
    let field = |at: usize| u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?).into();
    let offset: Option<u64> = field(12);
    let size: Option<u64> = field(20);
    let start = 32usize.checked_add(usize::try_from(offset?).ok()?)?;
    let end = start.checked_add(usize::try_from(size?).ok()?)?;
    (end <= bytes.len()).then_some(start..end)
}

fn repair(bytes: &mut [u8]) {
    if let Some(header) = header_range(bytes) {
        let crc = crc32fast::hash(&bytes[header]);
        bytes[28..32].copy_from_slice(&crc.to_le_bytes());
    }
    if bytes.len() >= 32 {
        let crc = crc32fast::hash(&bytes[12..32]);
        bytes[8..12].copy_from_slice(&crc.to_le_bytes());
    }
}

fn mutate(archive: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut bytes = archive.to_vec();
    for _ in 0..1 + rng.below(3) {
        let target = match header_range(&bytes) {
            Some(header) if rng.below(3) > 0 => header,
            _ => 0..bytes.len(),
        };
        if target.is_empty() {
            break;
        }
        let at = target.start + rng.below(target.len());
        match rng.below(5) {
            0 | 1 => bytes[at] ^= 1 << rng.below(8),
            2 => bytes[at] = [0, 1, 0x7F, 0x80, 0xFF][rng.below(5)],
            3 => {
                let length = bytes.len();
                bytes.truncate(rng.below(length));
            }
            _ => {
                let count = 1 + rng.below(8);
                let insert: Vec<u8> = (0..count).map(|_| rng.next() as u8).collect();
                bytes.splice(at..at, insert);
            }
        }
    }
    if rng.below(8) > 0 {
        repair(&mut bytes);
    }
    bytes
}

fn reader_accepts_cleanly(bytes: &[u8], threads: usize) -> bool {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.7z");
    fs::write(&archive, bytes).unwrap();
    let parent = root.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let destination = parent.join("out");
    let options = ReadOptions {
        threads: Some(threads),
        ..ReadOptions::default()
    };
    let extracted = read_archive(&archive, Some(&destination), options);
    let verified = read_archive(&archive, None, options);
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
            for entry in entries.iter().filter(|entry| !entry.directory) {
                let metadata = fs::symlink_metadata(destination.join(&entry.name))
                    .unwrap_or_else(|error| panic!("{:?} was not written: {error}", entry.name));
                if metadata.is_file() {
                    assert_eq!(metadata.len(), entry.size, "{:?}", entry.name);
                }
            }
            true
        }
    }
}

#[test]
fn mutated_corpus_archives_never_panic_or_leak_output() {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/7z-corpus");
    let mut archives: Vec<_> = fs::read_dir(&corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "7z"))
        .collect();
    archives.sort();
    let archives: Vec<Vec<u8>> = archives
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
    let cases: u64 = std::env::var(CASES).map_or(500, |value| value.parse().unwrap());
    let first: u64 = std::env::var(FIRST_SEED).map_or(1, |value| value.parse().unwrap());
    let mut accepted = 0u64;
    for seed in first..first + cases {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let source = &archives[rng.below(archives.len())];
        let mutant = mutate(source, &mut rng);
        let threads = [1, 4][rng.below(2)];
        match catch_unwind(AssertUnwindSafe(|| {
            reader_accepts_cleanly(&mutant, threads)
        })) {
            Ok(true) => accepted += 1,
            Ok(false) => {}
            Err(panic) => {
                eprintln!(
                    "failing case: ZIPPER_FUZZ_SEED={seed} ZIPPER_FUZZ_CASES=1, {threads} threads"
                );
                resume_unwind(panic);
            }
        }
    }
    eprintln!("{accepted} of {cases} extractions accepted a mutant");
}
