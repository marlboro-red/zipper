//! Damaged and crafted ext volumes. A damaged volume is refused whenever
//! 7-Zip refuses it, and otherwise extracts the tree 7-Zip extracts or is
//! refused for one of the stricter checks that `docs/reference/ext-reader.md`
//! lists. On volumes with checksums, each flip is also tried with every
//! checksum recomputed, so the checks behind them are reached. Crafted
//! volumes cover shared and looping extents and block maps, folder loops,
//! unsafe names and links, link counts, the features Zipper refuses by name
//! and cut images. Nothing is published on failure.
#![cfg(unix)]
#[path = "common/ext_image.rs"]
mod ext_image;
#[path = "common/ext_trees.rs"]
mod ext_trees;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use ext_image::{Layout, Region, put16, put32};
use ext_trees::{
    Tree, assert_matches_sevenzip, sevenzip_extract, sevenzip_unsupported_method_paths, tree,
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use zipper::{Engine, Reader};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/ext-corpus");

const CHECKS_7ZIP_SKIPS: &[&str] = &[
    "checksum does not match",
    "has no checksum",
    "is used twice",
    "link count",
    "which Zipper does not read",
    "is not UTF-8",
    "unsafe archive path",
    "two entries named",
    "an ext folder record",
    "a slash or NUL",
    "holds more entries than fit",
    "extents out of order",
    "revision is not 0 or 1",
    "has a target with a NUL",
];

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn corpus(name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(
        fs::File::open(Path::new(CORPUS).join(format!("{name}.xz"))).unwrap(),
    )
    .read_to_end(&mut bytes)
    .unwrap();
    bytes
}

enum Outcome {
    Extracted(Tree),
    Refused(String),
}

const OUTPUT_LIMIT: u64 = 128 << 20;

fn capped() -> zipper::native_ext::ReadOptions<'static> {
    zipper::native_ext::ReadOptions {
        max_output_bytes: Some(OUTPUT_LIMIT),
        ..Default::default()
    }
}

fn zipper_outcome(image: &Path, work: &Path) -> Outcome {
    let into = work.join("zipper");
    let tested = zipper::native_ext::read_archive(image, None, capped());
    match zipper::native_ext::read_archive(image, Some(&into), capped()) {
        Ok(_) => {
            assert!(
                tested.is_ok(),
                "only testing refused: {:#}",
                tested.unwrap_err()
            );
            Outcome::Extracted(tree(&into))
        }
        Err(error) => {
            assert!(!into.exists(), "Zipper published after: {error:#}");
            assert!(tested.is_err(), "only extracting refused: {error:#}");
            Outcome::Refused(format!("{error:#}"))
        }
    }
}

fn listed_size(binary: &Path, image: &Path) -> u64 {
    let listing = std::process::Command::new(binary)
        .args(["l", "-slt"])
        .arg(image)
        .output()
        .unwrap();
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("Size = "))
        .filter_map(|size| size.parse::<u64>().ok())
        .fold(0u64, u64::saturating_add)
}

fn sevenzip_reads(accepted: bool, output: &str) -> bool {
    let errors = output
        .lines()
        .filter(|line| line.starts_with("ERROR:"))
        .count();
    accepted
        || errors > 0
            && errors == sevenzip_unsupported_method_paths(output).len()
            && ![
                "Headers Error",
                "Data Error",
                "Cannot open",
                "Unexpected end",
                "CRC Failed",
            ]
            .iter()
            .any(|problem| output.contains(problem))
}

fn judge(binary: &Path, bytes: &[u8], label: &str) {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("damaged.img");
    fs::write(&image, bytes).unwrap();
    let ours = zipper_outcome(&image, work.path());
    if listed_size(binary, &image) > OUTPUT_LIMIT {
        assert!(
            matches!(ours, Outcome::Refused(_)),
            "{label}: 7-Zip lists more than {OUTPUT_LIMIT} bytes and Zipper extracted them"
        );
        return;
    }
    let (accepted, theirs, output) = sevenzip_extract(binary, &image, &work.path().join("7z"));
    let reads = sevenzip_reads(accepted, &output);
    match (ours, reads) {
        (Outcome::Refused(_), _) if !accepted => {}
        (Outcome::Extracted(tree), false) => panic!(
            "{label}: 7-Zip refuses it and Zipper extracted {} entries:\n{output}",
            tree.len()
        ),
        (Outcome::Refused(message), _) => assert!(
            CHECKS_7ZIP_SKIPS
                .iter()
                .any(|check| message.contains(check)),
            "{label}: 7-Zip extracts it and Zipper refused: {message}"
        ),
        (Outcome::Extracted(ours), true) => assert_matches_sevenzip(&ours, &theirs, &output, label),
    }
    ext_trees::tree(&work.path().join("7z"));
}

fn mask(at: usize) -> u8 {
    [0x10, 0x01, 0x80, 0x04][at % 4]
}

fn region_name(region: Region) -> &'static str {
    match region {
        Region::Superblock => "superblock",
        Region::Descriptor(_) => "group descriptor",
        Region::Inode(_) => "inode",
        Region::ExtentBlock(_) => "extent block",
        Region::IndirectBlock => "indirect block",
        Region::FolderBlock(_) => "folder block",
    }
}

fn interesting(region: Region, offset: usize, length: usize) -> bool {
    match region {
        Region::Superblock => offset < 0x180 || (0x270..0x280).contains(&offset) || offset >= 0x3FC,
        Region::Inode(_) => offset < 0xA0,
        Region::ExtentBlock(_) | Region::IndirectBlock => offset < 64 || offset >= length - 8,
        Region::Descriptor(_) | Region::FolderBlock(_) => true,
    }
}

fn flips(bytes: &[u8], layout: &Layout, budget: Option<usize>) -> Vec<(usize, usize, Region)> {
    let all: Vec<(usize, usize, Region)> = layout
        .regions(bytes)
        .into_iter()
        .flat_map(|(start, end, region)| {
            (start..end)
                .filter(move |at| interesting(region, at - start, end - start))
                .map(move |at| (at, start, region))
        })
        .collect();
    match budget {
        None => all,
        Some(budget) => {
            let stride = (all.len() / budget).max(1);
            all.into_iter().skip(stride / 2).step_by(stride).collect()
        }
    }
}

fn sweep(name: &str, budget: Option<usize>, sealed: bool) {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let bytes = corpus(name);
    let layout = Layout::parse(&bytes);
    let mut damaged = bytes.clone();
    for (at, start, region) in flips(&bytes, &layout, budget) {
        damaged[at] ^= mask(at);
        if sealed {
            layout.seal(&mut damaged, region, start);
        }
        if damaged != bytes {
            let label = format!(
                "{name}{}: {} byte {} flipped at {at} with {:#x}",
                if sealed { " resealed" } else { "" },
                region_name(region),
                at - start,
                mask(at)
            );
            judge(&binary, &damaged, &label);
        }
        damaged.copy_from_slice(&bytes);
    }
}

#[test]
fn damaged_block_mapped_volumes_are_refused_when_7zip_refuses_them() {
    sweep("ext2-1k.img", Some(40), false);
    sweep("ext3-2k.img", Some(30), false);
}

#[test]
fn damaged_ext4_volumes_are_refused_when_7zip_refuses_them() {
    sweep("ext4.img", Some(20), false);
    sweep("ext4.img", Some(40), true);
    sweep("ext4-block-maps.img", Some(30), true);
    sweep("ext4-uninit-bg.img", Some(20), true);
}

#[test]
#[ignore = "flips 1,500 bytes spread over every structure of each image; cargo xtask verify-sync ext-fields runs it"]
fn bytes_spread_over_every_structure_are_refused_when_7zip_refuses_them() {
    for name in ["ext2-1k.img", "ext3-2k.img", "ext4-32bit.img"] {
        sweep(name, Some(1500), false);
    }
    for name in [
        "ext4.img",
        "ext4-block-maps.img",
        "ext4-uninit-bg.img",
        "ext4-4k.img",
        "ext4-quota.img",
    ] {
        sweep(name, Some(1500), true);
    }
}

fn inline_sweep(budget: Option<usize>) {
    let bytes = corpus("ext4-inline.img");
    let layout = Layout::parse(&bytes);
    let mut damaged = bytes.clone();
    for (at, start, region) in flips(&bytes, &layout, budget) {
        damaged[at] ^= mask(at);
        layout.seal(&mut damaged, region, start);
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("damaged.img");
        fs::write(&image, &damaged).unwrap();
        zipper_outcome(&image, work.path());
        damaged.copy_from_slice(&bytes);
    }
}

#[test]
fn damaged_inline_data_never_panics_or_publishes_on_failure() {
    inline_sweep(Some(100));
}

#[test]
#[ignore = "flips 3,000 bytes spread over the inline image; cargo xtask verify-sync ext-fields runs it"]
fn bytes_spread_over_inline_data_never_panic_or_publish_on_failure() {
    inline_sweep(Some(3000));
}

#[test]
fn cut_volumes_are_refused_with_nothing_published() {
    let bytes = corpus("ext4.img");
    for cut in (0..bytes.len())
        .step_by(bytes.len() / 97)
        .chain([1023, 2047, 2048])
    {
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("cut.img");
        fs::write(&image, &bytes[..cut]).unwrap();
        match zipper_outcome(&image, work.path()) {
            Outcome::Refused(_) => {}
            Outcome::Extracted(_) => panic!("a volume cut to {cut} bytes extracted"),
        }
    }
}

fn refused(bytes: &[u8], wanted: &str) {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("crafted.img");
    fs::write(&image, bytes).unwrap();
    match zipper_outcome(&image, work.path()) {
        Outcome::Refused(message) => {
            assert!(message.contains(wanted), "wanted {wanted:?}, got {message}")
        }
        Outcome::Extracted(_) => panic!("a crafted volume extracted; wanted {wanted:?}"),
    }
}

fn inode_field(bytes: &mut [u8], layout: &Layout, number: u32, at: usize, value: u32) {
    let start = layout.inode_at(bytes, number);
    put32(bytes, start + at, value);
    layout.seal_inode(bytes, number);
}

#[test]
fn blocks_two_files_share_are_refused() {
    let mut bytes = corpus("ext2-1k.img");
    let layout = Layout::parse(&bytes);
    let hello = layout.lookup(&bytes, "hello.txt");
    let exec = layout.lookup(&bytes, "exec.sh");
    let data = layout.blocks(&bytes, hello).data;
    inode_field(&mut bytes, &layout, exec, 0x28, data[0] as u32);
    refused(&bytes, "is used twice");

    let mut bytes = corpus("ext4.img");
    let layout = Layout::parse(&bytes);
    let hello = layout.lookup(&bytes, "hello.txt");
    let exec = layout.lookup(&bytes, "exec.sh");
    let data = layout.blocks(&bytes, hello).data;
    inode_field(&mut bytes, &layout, exec, 0x28 + 12 + 8, data[0] as u32);
    refused(&bytes, "is used twice");
}

#[test]
fn looping_block_maps_are_refused() {
    let bytes = corpus("ext2-1k.img");
    let layout = Layout::parse(&bytes);
    let medium = layout.lookup(&bytes, "medium.bin");
    let maps = layout.blocks(&bytes, medium).maps;
    let block_size = layout.block_size;
    for (index, (block, _)) in maps.iter().enumerate() {
        let mut looped = bytes.clone();
        put32(&mut looped, *block as usize * block_size, *block as u32);
        refused(&looped, "is used twice");
        if let Some((parent, _)) = index.checked_sub(1).map(|before| maps[before]) {
            let mut looped = bytes.clone();
            put32(&mut looped, *block as usize * block_size, parent as u32);
            refused(&looped, "is used twice");
        }
    }
}

#[test]
fn looping_and_shared_extent_trees_are_refused() {
    let bytes = corpus("ext4.img");
    let layout = Layout::parse(&bytes);
    let islands = layout.lookup(&bytes, "islands.bin");
    let maps = layout.blocks(&bytes, islands).maps;
    assert!(
        maps.len() >= 3,
        "islands.bin should need a two-level extent tree"
    );
    let block_size = layout.block_size;
    let (index_node, _) = maps[0];
    let mut looped = bytes.clone();
    let at = index_node as usize * block_size;
    put32(&mut looped, at + 12 + 4, index_node as u32);
    layout.seal_extent_block(&mut looped, islands, index_node);
    refused(&looped, "wrong depth");

    let mut shared = bytes.clone();
    put32(&mut shared, at + 12 + 12 + 4, maps[1].0 as u32);
    layout.seal_extent_block(&mut shared, islands, index_node);
    refused(&shared, "extents out of order");

    let mut root_loop = bytes.clone();
    let start = layout.inode_at(&bytes, islands);
    let root_depth = u16::from_le_bytes([bytes[start + 0x28 + 6], bytes[start + 0x28 + 7]]);
    put16(&mut root_loop, at + 6, root_depth);
    layout.seal_extent_block(&mut root_loop, islands, index_node);
    refused(&root_loop, "wrong depth");
}

#[test]
fn folder_loops_and_second_names_for_folders_are_refused() {
    let bytes = corpus("ext2-1k.img");
    let layout = Layout::parse(&bytes);
    let deep = layout.lookup(&bytes, "deep");
    let d1 = layout.lookup(&bytes, "deep/d0/d1");
    let at = layout.record(&bytes, d1, "d2").offset;
    let mut looped = bytes.clone();
    put32(&mut looped, at, deep);
    refused(&looped, "more than one name");

    let at = layout.record(&bytes, 2, "folder").offset;
    let mut root = bytes.clone();
    put32(&mut root, at, 2);
    refused(&root, "which is not in use");
}

fn rename(bytes: &mut [u8], layout: &Layout, folder: u32, name: &str, new: &[u8]) {
    let ext_image::RecordAt { offset: at, block } = layout.record(bytes, folder, name);
    assert_eq!(name.len(), new.len());
    bytes[at + 8..at + 8 + new.len()].copy_from_slice(new);
    layout.seal_folder_block(bytes, folder, block);
}

#[test]
fn unsafe_names_and_links_are_refused_with_nothing_published() {
    for (old, new, wanted) in [
        ("hello.txt", &b"hel\\o.txt"[..], "unsafe archive path"),
        ("hello.txt", b"hel:o.txt", "unsafe archive path"),
        ("hello.txt", b"hel/o.txt", "a slash or NUL"),
        ("hello.txt", b"hel\0o.txt", "a slash or NUL"),
        ("hello.txt", b"hel\xFFo.txt", "is not UTF-8"),
        ("hello.txt", b"exec.sh\0\0", "a slash or NUL"),
    ] {
        for image in ["ext2-1k.img", "ext4.img"] {
            let mut bytes = corpus(image);
            let layout = Layout::parse(&bytes);
            rename(&mut bytes, &layout, 2, old, new);
            refused(&bytes, wanted);
        }
    }
    let mut bytes = corpus("ext2-1k.img");
    let layout = Layout::parse(&bytes);
    let at = layout.record(&bytes, 2, "readonly.txt").offset;
    let mut dot = bytes.clone();
    dot[at + 6] = 2;
    dot[at + 8..at + 10].copy_from_slice(b"..");
    refused(&dot, "past its first records");
    let exec = layout.lookup(&bytes, "exec.sh");
    let at = layout.record(&bytes, 2, "exec.sh").offset;
    let mut twice = bytes.clone();
    let other = layout.record(&bytes, 2, "private.txt").offset;
    put32(&mut twice, other, exec);
    twice[other + 8..other + 15].copy_from_slice(b"exec.sh");
    twice[other + 6] = 7;
    let _ = at;
    refused(&twice, "two entries named");

    let link = layout.lookup(&bytes, "link");
    let start = layout.inode_at(&bytes, link);
    bytes[start + 0x28..start + 0x28 + 9].copy_from_slice(b"../../a.b");
    refused(&bytes, "points outside the destination");
}

#[test]
fn link_counts_and_unlinked_inodes_are_refused() {
    let bytes = corpus("ext4.img");
    let layout = Layout::parse(&bytes);
    let hello = layout.lookup(&bytes, "hello.txt");
    let folder = layout.lookup(&bytes, "folder");
    for (number, links, wanted) in [
        (hello, 3u16, "link count"),
        (hello, 1, "link count"),
        (folder, 5, "link count"),
        (hello, 0, "in use with no links"),
    ] {
        let mut damaged = bytes.clone();
        let start = layout.inode_at(&bytes, number);
        put16(&mut damaged, start + 0x1A, links);
        layout.seal_inode(&mut damaged, number);
        refused(&damaged, wanted);
    }
    let mut stray = bytes.clone();
    let ext_image::RecordAt { offset: at, block } = layout.record(&bytes, 2, "late.txt");
    put32(&mut stray, at, 0);
    layout.seal_folder_block(&mut stray, 2, block);
    let late = layout.lookup(&bytes, "late.txt");
    let start = layout.inode_at(&bytes, late);
    put16(&mut stray, start + 0x1A, 2);
    layout.seal_inode(&mut stray, late);
    refused(&stray, "in no folder");
}

#[test]
fn an_unreferenced_file_extracts_under_unknown_as_7zip_writes_it() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let bytes = corpus("ext2-1k.img");
    let layout = Layout::parse(&bytes);
    let mut stray = bytes.clone();
    let at = layout.record(&bytes, 2, "late.txt").offset;
    put32(&mut stray, at, 0);
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("stray.img");
    fs::write(&image, &stray).unwrap();
    let Outcome::Extracted(ours) = zipper_outcome(&image, work.path()) else {
        panic!("Zipper refused an unreferenced file");
    };
    let late = layout.lookup(&bytes, "late.txt");
    assert!(ours.contains_key(&format!("[UNKNOWN]/{late}")), "{ours:?}");
    let (_, theirs, output) = sevenzip_extract(&binary, &image, &work.path().join("7z"));
    assert_matches_sevenzip(&ours, &theirs, &output, "unreferenced late.txt");
}

#[test]
fn an_empty_record_whose_name_holds_a_nul_is_refused_as_7zip_refuses_it() {
    let bytes = corpus("ext2-1k.img");
    let layout = Layout::parse(&bytes);
    let lost = layout.lookup(&bytes, "lost+found");
    let data = layout.blocks(&bytes, lost).data;
    let at = data[1] as usize * layout.block_size;
    let mut named = bytes.clone();
    named[at + 6] = 1;
    named[at + 8] = b'A';
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("named.img");
    fs::write(&image, &named).unwrap();
    assert!(matches!(
        zipper_outcome(&image, work.path()),
        Outcome::Extracted(_)
    ));
    named[at + 8] = 0;
    refused(&named, "an empty record whose name has a NUL");
    if let Some(binary) = sevenzip_binary() {
        fs::write(&image, &named).unwrap();
        assert!(!ext_trees::sevenzip_accepts(&binary, &image));
    }
}

#[test]
fn record_types_follow_7zip_with_unknown_allowed() {
    let bytes = corpus("ext2-1k.img");
    let layout = Layout::parse(&bytes);
    let deep = layout.lookup(&bytes, "deep");
    let root_block = layout.blocks(&bytes, 2).data[0] as usize * layout.block_size;
    let deep_block = layout.blocks(&bytes, deep).data[0] as usize * layout.block_size;
    let deep_record_type = root_block + 51;
    let binary = sevenzip_binary();
    for (at, record) in [
        (root_block + 7, "root ."),
        (root_block + 19, "root .."),
        (deep_block + 7, "deep ."),
        (deep_block + 19, "deep .."),
        (deep_record_type, "deep"),
    ] {
        for kind in [0u8, 1, 6, 7] {
            let mut typed = bytes.clone();
            typed[at] = kind;
            let label = format!("{record} record of type {kind}");
            let work = tempfile::tempdir().unwrap();
            let image = work.path().join("typed.img");
            fs::write(&image, &typed).unwrap();
            let ours = zipper_outcome(&image, work.path());
            assert_eq!(
                matches!(ours, Outcome::Extracted(_)),
                kind == 0,
                "{label}: {}",
                match ours {
                    Outcome::Refused(message) => message,
                    Outcome::Extracted(_) => "extracted".into(),
                }
            );
            if let Some(binary) = &binary {
                judge(binary, &typed, &label);
                assert_eq!(
                    ext_trees::sevenzip_accepts(binary, &image),
                    kind == 0,
                    "{label}"
                );
            }
        }
    }
}

#[test]
fn an_unused_group_whose_inode_bitmap_marks_inodes_is_refused_as_7zip_refuses_it() {
    let bytes = corpus("ext4-block-maps.img");
    let layout = Layout::parse(&bytes);
    let descriptor = layout.descriptor_at(1);
    assert_ne!(
        u16::from_le_bytes([bytes[descriptor + 0x12], bytes[descriptor + 0x13]]) & 1,
        0
    );
    let bitmap = u32::from_le_bytes(bytes[descriptor + 4..descriptor + 8].try_into().unwrap())
        as usize
        * layout.block_size;
    let padding = bitmap + layout.inodes_per_group as usize / 8;
    let binary = sevenzip_binary();
    for (at, refused_too) in [(bitmap, true), (padding - 1, true), (padding, false)] {
        let mut marked = bytes.clone();
        marked[at] = 0x80;
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("marked.img");
        fs::write(&image, &marked).unwrap();
        let outcome = zipper_outcome(&image, work.path());
        if refused_too {
            refused(&marked, "marks inodes in use but its inodes unused");
        } else {
            assert!(matches!(outcome, Outcome::Extracted(_)));
        }
        if let Some(binary) = &binary {
            assert_eq!(ext_trees::sevenzip_accepts(binary, &image), !refused_too);
        }
    }
}

#[test]
fn verifying_files_of_huge_declared_holes_does_not_fill_them() {
    let mut bytes = corpus("ext4-4k.img");
    let layout = Layout::parse(&bytes);
    for name in ["exec.sh", "private.txt", "readonly.txt"] {
        let number = layout.lookup(&bytes, name);
        inode_field(&mut bytes, &layout, number, 0x04, u32::MAX);
        inode_field(&mut bytes, &layout, number, 0x6C, 0xFFF);
    }
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("holes.img");
    fs::write(&image, &bytes).unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    let path = image.clone();
    std::thread::spawn(move || {
        let _ = done.send(zipper::native_ext::read_archive(
            &path,
            None,
            Default::default(),
        ));
    });
    let verified = finished
        .recv_timeout(std::time::Duration::from_secs(120))
        .expect("verifying 48 TiB of holes did not finish");
    let entries = verified.unwrap();
    let exec = entries
        .iter()
        .find(|entry| entry.name == "exec.sh")
        .unwrap();
    assert_eq!(exec.size, (1 << 44) - 1);
    refused(&bytes, "more than --max-extracted-bytes");
}

#[test]
fn a_root_entry_named_like_the_system_folder_is_refused() {
    let mut bytes = corpus("ext3-2k.img");
    let layout = Layout::parse(&bytes);
    rename(&mut bytes, &layout, 2, "names", b"[SYS]");
    refused(&bytes, "the folder 7-Zip writes system files in");
}

fn with_feature(name: &str, at: usize, bit: u32) -> Vec<u8> {
    let mut bytes = corpus(name);
    let layout = Layout::parse(&bytes);
    let field = ext_image::SUPERBLOCK + at;
    let value = u32::from_le_bytes(bytes[field..field + 4].try_into().unwrap());
    put32(&mut bytes, field, value | bit);
    layout.seal_superblock(&mut bytes);
    bytes
}

#[test]
fn features_zipper_does_not_read_are_refused_by_name_and_go_to_7zip() {
    for (image, at, bit, wanted) in [
        ("ext4.img", 0x60, 0x4, "a journal that needs recovery"),
        ("ext3-2k.img", 0x60, 0x4, "a journal that needs recovery"),
        ("ext4.img", 0x60, 0x1_0000, "encryption"),
        ("ext2-1k.img", 0x60, 0x1, "compression"),
        ("ext4.img", 0x60, 0x400, "extended attributes in inodes"),
        (
            "ext4.img",
            0x60,
            0x8000_0000,
            "the incompatible feature 0x80000000",
        ),
        ("ext4.img", 0x64, 0x200, "clusters larger than a block"),
        ("ext4.img", 0x64, 0x4000, "shared blocks"),
        ("ext4.img", 0x64, 0x1_0000, "orphan inodes"),
        ("ext4.img", 0xE8, 13, "orphan inodes"),
        (
            "ext4.img",
            0x64,
            0x4000_0000,
            "the read-only feature 0x40000000",
        ),
    ] {
        let bytes = with_feature(image, at, bit);
        refused(&bytes, wanted);
        refused(&bytes, "--zip-engine backend");
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join("feature.img");
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            zipper::choose_reader(Engine::Auto, &path).unwrap(),
            Reader::Backend { format: None },
            "{wanted}"
        );
        assert_eq!(
            zipper::choose_reader(Engine::Native, &path).unwrap(),
            Reader::Ext,
            "{wanted}"
        );
    }
}

#[test]
fn checksums_guard_every_structure_that_has_one() {
    let bytes = corpus("ext4.img");
    let layout = Layout::parse(&bytes);
    let hello = layout.lookup(&bytes, "hello.txt");
    let islands = layout.lookup(&bytes, "islands.bin");
    let maps = layout.blocks(&bytes, islands).maps;
    let root_block = layout.record(&bytes, 2, "hello.txt").block;
    for (at, what) in [
        (ext_image::SUPERBLOCK + 0x78, "superblock checksum"),
        (layout.descriptor_at(0) + 0x10, "descriptor checksum"),
        (layout.inode_at(&bytes, hello) + 0x64, "inode"),
        (
            maps[0].0 as usize * layout.block_size + 12,
            "extent block checksum",
        ),
        (
            root_block as usize * layout.block_size + 20,
            "block checksum",
        ),
    ] {
        let mut damaged = bytes.clone();
        damaged[at] ^= 0x20;
        refused(&damaged, what);
    }
}

#[test]
fn selection_and_strip_pick_the_paths_7zip_writes() {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("ext4.img");
    fs::write(&image, corpus("ext4.img")).unwrap();
    let whole = work.path().join("whole");
    zipper::native_ext::read_archive(&image, Some(&whole), Default::default()).unwrap();
    let whole = tree(&whole);
    let paths = ["folder".to_owned(), "noise.bin".to_owned()];
    let picked = work.path().join("picked");
    zipper::native_ext::read_archive(
        &image,
        Some(&picked),
        zipper::native_ext::ReadOptions {
            paths: &paths,
            ..Default::default()
        },
    )
    .unwrap();
    let picked = tree(&picked);
    let names: Vec<&String> = picked.keys().collect();
    assert_eq!(
        names,
        [
            "folder",
            "folder/inner.txt",
            "folder/noise-link.bin",
            "noise.bin"
        ],
        "{picked:?}"
    );
    assert_eq!(
        picked["folder/inner.txt"].kind,
        whole["folder/inner.txt"].kind
    );
    assert_eq!(
        picked["noise.bin"].group.as_deref(),
        Some("folder/noise-link.bin")
    );
    let stripped = work.path().join("stripped");
    zipper::native_ext::read_archive(
        &image,
        Some(&stripped),
        zipper::native_ext::ReadOptions {
            strip: zipper::Strip::Prefix("deep/d0/d1"),
            ..Default::default()
        },
    )
    .unwrap();
    let stripped = tree(&stripped);
    assert!(stripped.contains_key("d2/d3"), "{stripped:?}");
    assert!(!stripped.contains_key("hello.txt"));
}
