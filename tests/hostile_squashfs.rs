//! Damaged and crafted SquashFS and CramFS images. A damaged image is
//! refused whenever 7-Zip refuses it, and otherwise extracts the tree 7-Zip
//! extracts or is refused for one of the stricter checks that
//! `docs/reference/squashfs-reader.md` lists. Crafted images cover folder
//! loops, references into the middle of an inode, blocks that decode past
//! their size, unsafe names and links, holes, and the versions and layouts
//! Zipper refuses by name. Nothing is published on failure.
#[path = "common/sevenzip.rs"]
mod sevenzip;
#[path = "common/squashfs_trees.rs"]
mod squashfs_trees;
#[path = "common/squashfs_writer.rs"]
mod squashfs_writer;

use squashfs_trees::{
    Tree, sevenzip_accepts, sevenzip_run, tree, unsquashfs_difference, unsquashfs_tree,
};
use squashfs_writer::{Entry, Kind, Options, ROOT, again, build, build_cramfs, file, folder, link};
use std::{
    fs,
    path::{Path, PathBuf},
};
use zipper::{Engine, Reader};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/squashfs-corpus");

const ZSTD: u16 = 6;

const CHECKS_7ZIP_SKIPS: &[&str] = &[
    "data block is larger than the block size",
    "has a stored block of the wrong size",
    "has a block outside the data area",
    "lies outside the data area",
    "is not UTF-8",
    "CramFS root inode is not a folder",
    "another block layout",
    "decodes past its",
    "decodes to",
    "holds two entries named",
    "unsafe archive path",
];

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

enum Outcome {
    Extracted(Tree),
    Refused(String),
}

fn is_cramfs(bytes: &[u8]) -> bool {
    bytes.get(16..32) == Some(b"Compressed ROMFS")
}

fn is_zstd(bytes: &[u8]) -> bool {
    !is_cramfs(bytes) && bytes.get(20..22) == Some(&ZSTD.to_le_bytes())
}

fn zipper_outcome(image: &Path, work: &Path, cramfs: bool) -> Outcome {
    let into = work.join("zipper");
    let result = if cramfs {
        zipper::native_cramfs::read_archive(
            image,
            Some(&into),
            zipper::native_cramfs::ReadOptions::default(),
        )
    } else {
        zipper::native_squashfs::read_archive(
            image,
            Some(&into),
            zipper::native_squashfs::ReadOptions::default(),
        )
    };
    match result {
        Ok(_) => Outcome::Extracted(tree(&into, !cramfs)),
        Err(error) => {
            assert!(!into.exists(), "Zipper published after: {error:#}");
            Outcome::Refused(format!("{error:#}"))
        }
    }
}

fn judge(binary: &Path, bytes: &[u8], label: &str) {
    let work = tempfile::tempdir().unwrap();
    let cramfs = is_cramfs(bytes);
    let image = work.path().join(if cramfs {
        "damaged.cramfs"
    } else {
        "damaged.sqfs"
    });
    fs::write(&image, bytes).unwrap();
    let ours = zipper_outcome(&image, work.path(), cramfs);
    let accepted = sevenzip_accepts(binary, &image);
    match (ours, accepted) {
        (Outcome::Refused(_), false) => {}
        (Outcome::Extracted(tree), false) => {
            let libzstd_decodes_it = is_zstd(bytes)
                && unsquashfs_tree(&image, &work.path().join("unsquashfs"))
                    .is_some_and(|theirs| unsquashfs_difference(&tree, &theirs).is_none());
            assert!(
                libzstd_decodes_it,
                "{label}: 7-Zip refuses it and Zipper extracted {} entries",
                tree.len()
            );
        }
        (Outcome::Refused(message), true) => assert!(
            CHECKS_7ZIP_SKIPS
                .iter()
                .any(|check| message.contains(check)),
            "{label}: 7-Zip extracts it and Zipper refused: {message}"
        ),
        (Outcome::Extracted(ours), true) => {
            let (_, theirs, _) = sevenzip_run(binary, &image, &work.path().join("7z"), !cramfs);
            assert_eq!(
                ours, theirs,
                "{label}: Zipper and 7-Zip extract different trees"
            );
        }
    }
}

fn reseal(bytes: &mut [u8], at: usize) {
    if is_cramfs(bytes) && !(32..36).contains(&at) && bytes[8] & 1 != 0 {
        let big = bytes[0] == 0x28;
        let size = if big {
            u32::from_be_bytes(bytes[4..8].try_into().unwrap())
        } else {
            u32::from_le_bytes(bytes[4..8].try_into().unwrap())
        } as usize;
        if size <= bytes.len() {
            squashfs_writer::seal_cramfs(&mut bytes[..size], big);
        }
    }
}

fn flip_sweep(bytes: &[u8], ranges: &[(usize, usize, usize)], label: &str) {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    for &(start, end, step) in ranges {
        for at in (start..end.min(bytes.len())).step_by(step.max(1)) {
            let mut damaged = bytes.to_vec();
            damaged[at] ^= 0x10;
            reseal(&mut damaged, at);
            judge(&binary, &damaged, &format!("{label}: byte {at} flipped"));
        }
    }
}

fn corpus(name: &str) -> Vec<u8> {
    fs::read(Path::new(CORPUS).join(name)).unwrap()
}

fn le64(bytes: &[u8], at: usize) -> usize {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize
}

struct Areas {
    superblock: (usize, usize),
    data: (usize, usize),
    tables: (usize, usize),
}

fn areas(bytes: &[u8]) -> Areas {
    let inode_table = le64(bytes, 64);
    Areas {
        superblock: (0, 96),
        data: (96, inode_table),
        tables: (inode_table, le64(bytes, 40)),
    }
}

fn squashfs_ranges(
    name: &str,
    table_step: usize,
    data_step: usize,
    superblock_step: usize,
) -> (Vec<u8>, Vec<(usize, usize, usize)>) {
    let bytes = corpus(name);
    let areas = areas(&bytes);
    let ranges = vec![
        (areas.superblock.0, areas.superblock.1, superblock_step),
        (areas.data.0, areas.data.1, data_step),
        (areas.tables.0, areas.tables.1, table_step),
    ];
    (bytes, ranges)
}

#[test]
fn damaged_squashfs_metadata_is_refused_when_7zip_refuses_it() {
    let (bytes, ranges) = squashfs_ranges("uncompressed.sqfs", 41, 4099, 3);
    flip_sweep(&bytes, &ranges, "uncompressed");
    let (bytes, ranges) = squashfs_ranges("gzip.sqfs", 61, 997, 5);
    flip_sweep(&bytes, &ranges, "gzip");
}

#[test]
fn damaged_squashfs_blocks_of_every_compressor_are_refused_when_7zip_refuses_them() {
    for name in [
        "lzma.sqfs",
        "lzo.sqfs",
        "xz.sqfs",
        "zstd.sqfs",
        "xz-x86-1m.sqfs",
    ] {
        let (bytes, ranges) = squashfs_ranges(name, 251, 1499, 31);
        flip_sweep(&bytes, &ranges, name);
    }
}

#[test]
#[ignore = "full sweep; cargo xtask release-checks runs it"]
fn every_squashfs_metadata_byte_is_refused_when_7zip_refuses_it() {
    for name in [
        "uncompressed.sqfs",
        "gzip.sqfs",
        "no-fragments.sqfs",
        "always-fragments.sqfs",
    ] {
        let (bytes, ranges) = squashfs_ranges(name, 1, 7, 1);
        flip_sweep(&bytes, &ranges, name);
    }
}

#[test]
#[ignore = "full sweep; cargo xtask release-checks runs it"]
fn every_squashfs_block_of_every_compressor_is_refused_when_7zip_refuses_it() {
    for name in [
        "lzma.sqfs",
        "lzo.sqfs",
        "xz.sqfs",
        "zstd.sqfs",
        "xz-x86-1m.sqfs",
    ] {
        let (bytes, ranges) = squashfs_ranges(name, 3, 11, 1);
        flip_sweep(&bytes, &ranges, name);
    }
}

fn cramfs_ranges(bytes: &[u8], header_step: usize, data_step: usize) -> Vec<(usize, usize, usize)> {
    let listing_end = 3000.min(bytes.len());
    vec![
        (0, listing_end, header_step),
        (listing_end, bytes.len(), data_step),
    ]
}

#[test]
fn damaged_cramfs_images_are_refused_when_7zip_refuses_them() {
    let bytes = corpus("cramfs-little.cramfs");
    flip_sweep(
        &bytes,
        &cramfs_ranges(&bytes, 23, 509),
        "CramFS little-endian",
    );
    let bytes = corpus("cramfs-big.cramfs");
    flip_sweep(
        &bytes,
        &cramfs_ranges(&bytes, 61, 1021),
        "CramFS big-endian",
    );
}

#[test]
#[ignore = "full sweep; cargo xtask release-checks runs it"]
fn every_cramfs_byte_is_refused_when_7zip_refuses_it() {
    for name in ["cramfs-little.cramfs", "cramfs-big.cramfs"] {
        let bytes = corpus(name);
        flip_sweep(&bytes, &cramfs_ranges(&bytes, 1, 7), name);
    }
}

#[test]
fn cut_images_are_refused_with_nothing_published() {
    let binary = sevenzip_binary();
    for (name, step) in [
        ("uncompressed.sqfs", 4093),
        ("gzip.sqfs", 997),
        ("cramfs-little.cramfs", 1009),
    ] {
        let bytes = corpus(name);
        let cramfs = is_cramfs(&bytes);
        let used = if cramfs {
            bytes.len()
        } else {
            le64(&bytes, 40)
        };
        for length in (0..used).step_by(step).chain([used - 1]) {
            let work = tempfile::tempdir().unwrap();
            let image = work.path().join("cut.img");
            fs::write(&image, &bytes[..length]).unwrap();
            if let Outcome::Extracted(found) = zipper_outcome(&image, work.path(), cramfs) {
                panic!("{name} cut at {length}: extracted {} entries", found.len());
            }
            if let Some(binary) = &binary {
                judge(binary, &bytes[..length], &format!("{name} cut at {length}"));
            }
        }
    }
}

fn write(bytes: &[u8], cramfs: bool) -> (tempfile::TempDir, PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join(if cramfs {
        "crafted.cramfs"
    } else {
        "crafted.sqfs"
    });
    fs::write(&image, bytes).unwrap();
    (work, image)
}

fn refused(bytes: &[u8], wanted: &str) {
    let cramfs = is_cramfs(bytes);
    let (work, image) = write(bytes, cramfs);
    match zipper_outcome(&image, work.path(), cramfs) {
        Outcome::Refused(message) => assert!(
            message.contains(wanted),
            "refused for another reason: {message}; wanted {wanted:?}"
        ),
        Outcome::Extracted(found) => panic!("extracted {found:?}; wanted a refusal for {wanted:?}"),
    }
}

fn extracted(bytes: &[u8]) -> Tree {
    let cramfs = is_cramfs(bytes);
    let (work, image) = write(bytes, cramfs);
    match zipper_outcome(&image, work.path(), cramfs) {
        Outcome::Extracted(found) => found,
        Outcome::Refused(message) => panic!("refused: {message}"),
    }
}

fn sevenzip_verdict(bytes: &[u8]) -> Option<bool> {
    let binary = sevenzip_binary()?;
    let (_work, image) = write(bytes, is_cramfs(bytes));
    Some(sevenzip_accepts(&binary, &image))
}

fn base() -> Vec<Entry> {
    vec![
        folder(ROOT, "docs"),
        file(0, "readme.txt", b"hello from a crafted image\n"),
        file(ROOT, "big.bin", &[0x5A; 3 * squashfs_writer::BLOCK + 100]),
        link(0, "link", "readme.txt"),
        folder(0, "sub"),
        file(4, "deep.txt", b"deep\n"),
    ]
}

#[test]
fn the_test_writer_builds_images_7zip_extracts_as_zipper_does() {
    let mut entries = base();
    entries.push(again(ROOT, "big-again.bin", 2));
    for (index, options) in [Options::default(), Options { compress: true }]
        .into_iter()
        .enumerate()
    {
        let built = build(&entries, options);
        let ours = extracted(&built.bytes);
        assert_eq!(ours.len(), 7, "{index}");
        assert_eq!(ours["big-again.bin"], ours["big.bin"]);
        if let Some(binary) = sevenzip_binary() {
            let (work, image) = write(&built.bytes, false);
            let (accepted, theirs, output) =
                sevenzip_run(&binary, &image, &work.path().join("7z"), true);
            assert!(accepted, "{output}");
            assert_eq!(ours, theirs);
        }
        let cram = build_cramfs(&entries, index == 1);
        let ours = extracted(&cram.bytes);
        assert_eq!(ours.len(), 7);
        if let Some(binary) = sevenzip_binary() {
            let (work, image) = write(&cram.bytes, true);
            let (accepted, theirs, output) =
                sevenzip_run(&binary, &image, &work.path().join("7z"), false);
            assert!(accepted, "{output}");
            assert_eq!(ours, theirs);
        }
    }
}

#[test]
fn folder_loops_and_shared_folders_are_refused() {
    let mut entries = base();
    entries.push(again(4, "back-to-root", ROOT));
    refused(
        &build(&entries, Options::default()).bytes,
        "appears twice or forms a loop",
    );
    let mut entries = base();
    entries.push(again(ROOT, "docs-again", 0));
    refused(
        &build(&entries, Options::default()).bytes,
        "appears twice or forms a loop",
    );
    let mut entries = base();
    entries.push(again(4, "back-to-root", ROOT));
    refused(&build_cramfs(&entries, false).bytes, "forms a loop");
}

#[test]
fn unsafe_and_repeated_names_are_refused_with_nothing_published() {
    for (name, wanted) in [
        (&b".."[..], "unsafe name"),
        (b".", "unsafe name"),
        (b"a/b", "unsafe name"),
        (b"bad\xFFname", "not UTF-8"),
        (b"back\\slash", "unsafe archive path"),
        (b"readme.txt", "two entries named"),
    ] {
        let mut entries = base();
        let mut extra = file(0, "x", b"x");
        extra.name = name.to_vec();
        entries.push(extra);
        refused(&build(&entries, Options::default()).bytes, wanted);
        refused(&build_cramfs(&entries, false).bytes, wanted);
    }
}

#[test]
fn links_that_leave_the_destination_are_refused() {
    for target in ["../../outside", "/etc/passwd", "sub/../../../x"] {
        let mut entries = base();
        entries.push(link(0, "escape", target));
        refused(&build(&entries, Options::default()).bytes, "symlink");
        refused(&build_cramfs(&entries, true).bytes, "symlink");
    }
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn a_block_that_decodes_past_its_size_is_refused_while_it_decodes() {
    let entries = vec![file(ROOT, "text.txt", &b"abcdefgh".repeat(1000))];
    let built = build(&entries, Options { compress: true });
    let mut bytes = built.bytes.clone();
    let size_at = built.inode_at[0] + 16 + 12;
    put32(&mut bytes, size_at, 7000);
    refused(&bytes, "decodes past its size");
    if let Some(accepted) = sevenzip_verdict(&bytes) {
        assert!(
            accepted,
            "7-Zip now refuses a block longer than its file; drop it from the differences"
        );
    }
}

#[test]
fn references_into_the_middle_of_an_inode_are_refused() {
    let built = build(&base(), Options::default());
    let mut bytes = built.bytes.clone();
    let listing = built.listing_at[0].unwrap();
    let first_entry = listing + 12;
    let offset = u16::from_le_bytes([bytes[first_entry], bytes[first_entry + 1]]);
    bytes[first_entry..first_entry + 2].copy_from_slice(&(offset + 4).to_le_bytes());
    refused(&bytes, "does not name the start of an inode");
    if let Some(accepted) = sevenzip_verdict(&bytes) {
        assert!(!accepted, "7-Zip accepts a reference into an inode");
    }
}

#[test]
fn sparse_blocks_and_cramfs_holes_extract_as_zeros() {
    let entries = vec![file(ROOT, "sparse.bin", &[7u8; 3 * squashfs_writer::BLOCK])];
    let built = build(&entries, Options::default());
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.inode_at[0] + 32 + 4, 0);
    let ours = extracted(&bytes);
    let mut expected = vec![7u8; 3 * squashfs_writer::BLOCK];
    expected[squashfs_writer::BLOCK..2 * squashfs_writer::BLOCK].fill(0);
    assert!(ours["sparse.bin"].starts_with(&squashfs_trees::sha256(&expected)));
    if let Some(binary) = sevenzip_binary() {
        let (work, image) = write(&bytes, false);
        let (_, theirs, _) = sevenzip_run(&binary, &image, &work.path().join("7z"), true);
        assert_eq!(ours, theirs);
    }
    let cram = build_cramfs(&entries, false);
    let mut bytes = cram.bytes.clone();
    let table = cram.data_at[0].unwrap();
    let end = |bytes: &[u8], block: usize| {
        u32::from_le_bytes(
            bytes[table + 4 * block..table + 4 * block + 4]
                .try_into()
                .unwrap(),
        )
    };
    let (first, second, third) = (end(&bytes, 0), end(&bytes, 1), end(&bytes, 2));
    let hole = second - first;
    put32(&mut bytes, table + 4, first);
    put32(&mut bytes, table + 8, third - hole);
    let moved = bytes[second as usize..].to_vec();
    bytes[first as usize..first as usize + moved.len()].copy_from_slice(&moved);
    squashfs_writer::seal_cramfs(&mut bytes, false);
    let found = extracted(&bytes);
    assert!(found["sparse.bin"].starts_with(&squashfs_trees::sha256(&expected)));
    if let Some(accepted) = sevenzip_verdict(&bytes) {
        assert!(!accepted, "7-Zip now reads CramFS holes; compare its tree");
    }
}

fn reader_for(bytes: &[u8]) -> Reader {
    let (_work, image) = write(bytes, false);
    zipper::choose_reader(Engine::Auto, &image).unwrap()
}

#[test]
fn other_squashfs_versions_and_byte_orders_go_to_7zip_by_name() {
    let mut version_3 = corpus("gzip.sqfs");
    version_3[28] = 3;
    let mut big_endian = corpus("gzip.sqfs");
    big_endian[..4].copy_from_slice(b"sqsh");
    for bytes in [&version_3, &big_endian] {
        assert_eq!(reader_for(bytes), Reader::Backend { format: None });
        refused(bytes, "--zip-engine backend");
    }
    assert_eq!(reader_for(&corpus("gzip.sqfs")), Reader::Squashfs);
}

#[test]
fn bad_metadata_blocks_and_cramfs_fields_are_refused() {
    let built = build(&base(), Options::default());
    for header in [0x8000u16, 0x8000 | 8193, 0x7FFF] {
        let mut bytes = built.bytes.clone();
        bytes[built.inode_table..built.inode_table + 2].copy_from_slice(&header.to_le_bytes());
        refused(&bytes, "SquashFS inode table");
    }
    let cram = build_cramfs(&base(), false);
    let mut bytes = cram.bytes.clone();
    bytes[40] ^= 1;
    refused(&bytes, "CRC-32");
    for (field, value, wanted) in [
        (44, (1u32 << 19) + 1, "more files than 7-Zip opens"),
        (8, 0x803, "another block layout"),
    ] {
        let mut bytes = cram.bytes.clone();
        put32(&mut bytes, field, value);
        squashfs_writer::seal_cramfs(&mut bytes, false);
        refused(&bytes, wanted);
        if let Some(accepted) = sevenzip_verdict(&bytes) {
            assert!(
                !accepted || wanted.contains("layout"),
                "7-Zip accepts {wanted}"
            );
        }
    }
}

#[test]
fn every_entry_kind_extracts_as_7zip_extracts_it() {
    let mut entries = base();
    let mut fifo = file(ROOT, "fifo", b"");
    fifo.kind = Kind::Fifo;
    fifo.mode = 0o600;
    entries.push(fifo);
    let mut locked = folder(ROOT, "locked");
    locked.mode = 0o500;
    entries.push(locked);
    let mut setuid = file(ROOT, "setuid", b"run\n");
    setuid.mode = 0o4755;
    entries.push(setuid);
    let mut sticky = folder(ROOT, "sticky");
    sticky.mode = 0o1777;
    entries.push(sticky);
    let mut epoch = file(ROOT, "epoch.txt", b"1970\n");
    epoch.modified = 0;
    entries.push(epoch);
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    for cramfs in [false, true] {
        let bytes = if cramfs {
            build_cramfs(&entries, false).bytes
        } else {
            build(&entries, Options::default()).bytes
        };
        let ours = extracted(&bytes);
        let (work, image) = write(&bytes, cramfs);
        let (accepted, theirs, output) =
            sevenzip_run(&binary, &image, &work.path().join("7z"), !cramfs);
        assert!(accepted, "{output}");
        assert_eq!(ours, theirs, "CramFS: {cramfs}");
    }
}

#[test]
fn inodes_and_listings_that_start_a_metadata_block_are_read() {
    let entries: Vec<Entry> = (0..600)
        .map(|index| file(ROOT, &format!("empty-{index:03}"), b""))
        .collect();
    let built = build(&entries, Options::default());
    assert!(
        built
            .inode_at
            .iter()
            .any(|at| (at - built.inode_table) % 8194 == 2),
        "no inode starts a metadata block"
    );
    let ours = extracted(&built.bytes);
    assert_eq!(ours.len(), 600);
    if let Some(binary) = sevenzip_binary() {
        let (work, image) = write(&built.bytes, false);
        let (accepted, theirs, output) =
            sevenzip_run(&binary, &image, &work.path().join("7z"), true);
        assert!(accepted, "{output}");
        assert_eq!(ours, theirs);
    }
}

#[test]
fn a_file_that_starts_near_the_end_of_the_address_space_is_refused() {
    let entries = vec![
        file(ROOT, "data.bin", &[1u8; 100]),
        again(ROOT, "again.bin", 0),
    ];
    let built = build(&entries, Options::default());
    let mut bytes = built.bytes.clone();
    let start = built.inode_at[0] + 16;
    bytes[start..start + 8].copy_from_slice(&(u64::MAX - 10).to_le_bytes());
    refused(&bytes, "outside the data area");
}

#[test]
fn a_folder_table_of_too_many_blocks_is_refused() {
    let built = build(&base(), Options::default());
    let mut bytes = built.bytes[..built.directory_table].to_vec();
    let listings = built.bytes[built.directory_table..].to_vec();
    let blocks = (1 << 20) + 1;
    for _ in 0..blocks {
        bytes.extend_from_slice(&[0x01, 0x80, 0]);
    }
    let shift = (bytes.len() - built.directory_table) as u64;
    bytes.extend_from_slice(&listings);
    for field in [40, 48, 80] {
        let value = u64::from_le_bytes(bytes[field..field + 8].try_into().unwrap()) + shift;
        bytes[field..field + 8].copy_from_slice(&value.to_le_bytes());
    }
    let id_table = u64::from_le_bytes(bytes[48..56].try_into().unwrap()) as usize;
    let pointer = u64::from_le_bytes(bytes[id_table..id_table + 8].try_into().unwrap()) + shift;
    bytes[id_table..id_table + 8].copy_from_slice(&pointer.to_le_bytes());
    refused(&bytes, "folder table has more than");
}

#[test]
fn listing_starts_block_words_and_owner_tables_are_checked() {
    let mut entries = base();
    entries.push(folder(ROOT, "empty"));
    let built = build(&entries, Options::default());
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.inode_at[6] + 16, 5);
    refused(&bytes, "starts outside the folder table");
    if let Some(accepted) = sevenzip_verdict(&bytes) {
        assert!(
            !accepted,
            "7-Zip accepts a listing outside the folder table"
        );
    }
    let mut bytes = built.bytes.clone();
    let word_at = built.inode_at[2] + 32;
    let word = u32::from_le_bytes(bytes[word_at..word_at + 4].try_into().unwrap());
    put32(&mut bytes, word_at, word | 1 << 25);
    refused(&bytes, "larger than the block size");
    let mut bytes = built.bytes.clone();
    bytes[26..28].copy_from_slice(&2u16.to_le_bytes());
    refused(&bytes, "owner table block 0 has the wrong length");
    if let Some(accepted) = sevenzip_verdict(&bytes) {
        assert!(
            !accepted,
            "7-Zip accepts an owner table shorter than its count"
        );
    }
}

#[test]
fn cramfs_offsets_past_the_image_are_refused_and_offset_zero_is_empty() {
    let mut entries = base();
    entries.push(folder(ROOT, "empty"));
    entries.push(file(ROOT, "nothing.txt", b""));
    let cram = build_cramfs(&entries, false);
    let mut bytes = cram.bytes.clone();
    let word_at = cram.inode_at[6] + 8;
    let name_words = u32::from_le_bytes(bytes[word_at..word_at + 4].try_into().unwrap()) & 0x3F;
    let past_the_end = bytes.len() as u32 / 4 + 1;
    put32(&mut bytes, word_at, name_words | (past_the_end << 6));
    squashfs_writer::seal_cramfs(&mut bytes, false);
    refused(&bytes, "points past the end of the image");
    if let Some(accepted) = sevenzip_verdict(&bytes) {
        assert!(!accepted, "7-Zip accepts an offset past the image");
    }
    let mut bytes = cram.bytes.clone();
    put32(&mut bytes, cram.inode_at[7] + 4, 16);
    squashfs_writer::seal_cramfs(&mut bytes, false);
    let ours = extracted(&bytes);
    assert!(ours["nothing.txt"].starts_with(&squashfs_trees::sha256(b"")));
    if let Some(binary) = sevenzip_binary() {
        let (work, image) = write(&bytes, true);
        let (_, theirs, _) = sevenzip_run(&binary, &image, &work.path().join("7z"), false);
        assert_eq!(ours, theirs);
    }
}

#[test]
fn a_file_block_inside_the_superblock_is_refused() {
    let entries = vec![file(ROOT, "data.bin", &[1u8; 100])];
    let built = build(&entries, Options::default());
    let mut bytes = built.bytes.clone();
    put32(&mut bytes, built.inode_at[0] + 16, 0);
    refused(&bytes, "outside the data area");
}

#[test]
fn a_cramfs_link_longer_than_every_reader_allows_is_refused() {
    let target = "a/".repeat(2500);
    let entries = vec![file(ROOT, "a", b"x"), link(ROOT, "long", &target)];
    let cram = build_cramfs(&entries, false);
    refused(&cram.bytes, "longer than 4096 bytes");
}

#[test]
fn a_zstd_block_7zip_refuses_and_libzstd_decodes_extracts_as_unsquashfs_writes_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut bytes = corpus("zstd.sqfs");
    bytes[151] ^= 0x10;
    let (work, image) = write(&bytes, false);
    assert!(!sevenzip_accepts(&binary, &image), "7-Zip now accepts it");
    judge(&binary, &bytes, "zstd.sqfs: byte 151 flipped");
    drop(work);
}
