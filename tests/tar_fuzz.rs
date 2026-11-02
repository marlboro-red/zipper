//! Seeded mutation testing of the native tar reader on the reference corpus.
//! Each case mutates one plain corpus tar, sometimes repairing the header
//! checksum so the parser goes past it, and sometimes gzips the result. A
//! case passes when the reader rejects it cleanly, leaving nothing behind, or
//! extracts it, verification agrees, and every file has its declared size.
//! Any panic fails.
//!
//! The gate runs a short pass. Set `ZIPPER_FUZZ_CASES` for a longer run and
//! `ZIPPER_FUZZ_SEED` to replay a reported failure. This is random mutation,
//! not coverage-guided fuzzing.
mod common;

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
};
use zipper::native_tar::{ReadOptions, read_archive};

const BLOCK: usize = 512;

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

const FIELDS: [(usize, usize); 9] = [
    (0, 100),
    (100, 8),
    (124, 12),
    (136, 12),
    (156, 1),
    (157, 100),
    (257, 8),
    (345, 155),
    (482, 30),
];

const VALUES: [&[u8]; 16] = [
    b"../escape",
    b"/absolute",
    b"a/../../b",
    b"0000000\0",
    b"77777777777\0",
    b"\x80\0\0\0\0\0\0\0\0\0\0\x10",
    b"\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff\xff",
    b"1",
    b"2",
    b"5",
    b"L",
    b"x",
    b"g",
    b"S",
    b"ustar\0",
    b"ustar  \0",
];

const PAX_RECORDS: [&[u8]; 6] = [
    b"path=../escape",
    b"linkpath=/etc",
    b"size=999999999",
    b"mtime=-99999999999.5",
    b"GNU.sparse.size=1",
    b"path=",
];

fn header_starts(bytes: &[u8]) -> Vec<usize> {
    (0..bytes.len() / BLOCK)
        .map(|block| block * BLOCK)
        .filter(|&start| bytes[start + 257..start + 262] == *b"ustar" || bytes[start] != 0)
        .collect()
}

fn repair_checksum(header: &mut [u8]) {
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
}

fn overwrite_header_field(bytes: &mut [u8], rng: &mut Rng) {
    let starts = header_starts(bytes);
    if starts.is_empty() {
        return;
    }
    let start = starts[rng.below(starts.len())];
    let (offset, width) = FIELDS[rng.below(FIELDS.len())];
    let value = VALUES[rng.below(VALUES.len())];
    let field = &mut bytes[start + offset..start + offset + width];
    field.fill(0);
    let length = value.len().min(width);
    field[..length].copy_from_slice(&value[..length]);
    repair_checksum(&mut bytes[start..start + BLOCK]);
}

fn insert_pax_header(bytes: &mut Vec<u8>, rng: &mut Rng) {
    let starts = header_starts(bytes);
    if starts.is_empty() {
        return;
    }
    let at = starts[rng.below(starts.len())];
    let record = PAX_RECORDS[rng.below(PAX_RECORDS.len())];
    let body = record.len() + 2;
    let mut length = body + body.to_string().len();
    if length.to_string().len() != body.to_string().len() {
        length += 1;
    }
    let mut data = format!("{length} ").into_bytes();
    data.extend_from_slice(record);
    data.push(b'\n');
    let mut header = [0u8; BLOCK];
    header[..9].copy_from_slice(b"PaxHeader");
    header[100..108].copy_from_slice(b"0000644\0");
    header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[156] = b'x';
    header[257..265].copy_from_slice(b"ustar\x0000");
    repair_checksum(&mut header);
    data.resize(data.len().div_ceil(BLOCK) * BLOCK, 0);
    let mut inserted = header.to_vec();
    inserted.extend_from_slice(&data);
    bytes.splice(at..at, inserted);
}

fn mutate(archive: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut bytes = archive.to_vec();
    for _ in 0..1 + rng.below(3) {
        if bytes.is_empty() {
            break;
        }
        match rng.below(6) {
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
            3 => insert_pax_header(&mut bytes, rng),
            _ => overwrite_header_field(&mut bytes, rng),
        }
    }
    if rng.below(4) == 0 {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&bytes).unwrap();
        bytes = encoder.finish().unwrap();
    }
    bytes
}

fn reader_accepts_cleanly(bytes: &[u8], writers: usize) -> bool {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.tar");
    fs::write(&archive, bytes).unwrap();
    let parent = root.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let destination = parent.join("out");
    let options = ReadOptions {
        writers: Some(writers),
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
            let last_sizes: BTreeMap<_, _> = entries
                .iter()
                .filter(|entry| !entry.directory)
                .map(|entry| (entry.name.as_str(), entry.size))
                .collect();
            for (name, size) in last_sizes.into_iter().filter(|&(_, size)| size > 0) {
                let metadata = fs::symlink_metadata(destination.join(name))
                    .unwrap_or_else(|error| panic!("{name:?} was not written: {error}"));
                assert!(metadata.is_file(), "{name:?} is not a file");
                assert_eq!(
                    metadata.len(),
                    size,
                    "{name:?} differs from its declared size"
                );
            }
            true
        }
    }
}

#[test]
fn mutated_corpus_tars_never_panic_or_leak_output() {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tar-corpus");
    let mut archives: Vec<_> = fs::read_dir(&corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "tar"))
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
        let writers = [1, 4][rng.below(2)];
        match catch_unwind(AssertUnwindSafe(|| {
            reader_accepts_cleanly(&mutant, writers)
        })) {
            Ok(true) => accepted += 1,
            Ok(false) => {}
            Err(panic) => {
                eprintln!(
                    "failing case: ZIPPER_FUZZ_SEED={seed} ZIPPER_FUZZ_CASES=1, {writers} writers"
                );
                resume_unwind(panic);
            }
        }
    }
    eprintln!("{accepted} of {cases} extractions accepted a mutant");
}
