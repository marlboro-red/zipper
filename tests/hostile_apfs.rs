//! Damaged and crafted APFS containers. A damaged container is refused
//! whenever 7-Zip refuses it, and otherwise extracts the tree 7-Zip
//! extracts or is refused for one of the stricter checks that
//! `docs/reference/apfs-reader.md` lists. Crafted containers cover B-tree
//! loops, nodes outside the container, bad checksums, extents past the end,
//! the features Zipper refuses by name, unsafe names and links, and folder
//! records that disagree with their inodes. Nothing is published on failure.
//!
//! The gate runs a sample of each sweep. The full sweeps are ignored tests
//! that `cargo xtask release-checks` runs under `[apfs-fields]`.
#[path = "common/apfs_trees.rs"]
mod apfs_trees;
#[path = "common/apfs_writer.rs"]
mod apfs_writer;
#[path = "common/dmg_writer.rs"]
mod dmg_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use apfs_trees::{Tree, sevenzip_run, tree};
use apfs_writer::{
    Container, Entry, Kind, Layout, ROOT, Volume, Xattr, compressed, decmpfs_header, file, folder,
    hard_link, seal, symlink,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use zipper::{Engine, Reader};

const BLOCK: usize = 4096;

// Checks Zipper makes that 7-Zip does not; a damaged container 7-Zip extracts may fail one.
const STRICTER: &[&str] = &[
    "checkpoint map",
    "newer than its checkpoint",
    "is not the node its parent names",
    "is not the object its",
    "lies past the end of the image",
    "holds more blocks than its image",
];

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn base_entries() -> Vec<Entry> {
    let mut entries = vec![
        folder(16, ROOT, "docs"),
        file(17, 16, "readme.txt", b"hello from a damaged container\n"),
        file(18, ROOT, "big.bin", &vec![0x5A; 3 * BLOCK + 100]),
        hard_link(19, ROOT, "big-again.bin", 18),
        symlink(20, 16, "link", "readme.txt"),
        folder(21, 16, "sub"),
    ];
    for number in 0..24u64 {
        entries.push(file(
            30 + number,
            21,
            &format!("f{number:02}.txt"),
            format!("small file {number}\n").as_bytes(),
        ));
    }
    let text = b"compressible ".repeat(300);
    let mut packed = compressed(
        60,
        ROOT,
        "packed.txt",
        decmpfs_header(7, text.len() as u64, &dmg_writer::lzvn(&text)),
        text.len() as u64,
        None,
    );
    packed.xattrs.push(Xattr {
        name: "user.note".into(),
        value: b"skipped".to_vec(),
        stream: false,
    });
    entries.push(packed);
    entries
}

fn base() -> (Vec<u8>, Layout) {
    let mut container = Container::new(vec![Volume::new(base_entries())]);
    container.records_per_leaf = 6;
    container.children_per_index = 3;
    container.omap_entries_per_leaf = 3;
    let (image, layout) = apfs_writer::build(&container);
    (image.to_vec(), layout)
}

enum Outcome {
    Extracted(Tree),
    Refused(String),
}

fn zipper_outcome(image: &Path, work: &Path) -> Outcome {
    let into = work.join("zipper");
    match zipper::native_apfs::read_archive(
        image,
        Some(&into),
        zipper::native_apfs::ReadOptions::default(),
    ) {
        Ok(_) => Outcome::Extracted(tree(&into)),
        Err(error) => {
            assert!(!into.exists(), "Zipper published after: {error:#}");
            Outcome::Refused(format!("{error:#}"))
        }
    }
}

fn sevenzip_accepts(binary: &Path, image: &Path) -> bool {
    Command::new(binary)
        .args(["t", "-y"])
        .arg(image)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn judge(binary: &Path, bytes: &[u8], label: &str) {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("damaged.img");
    fs::write(&image, bytes).unwrap();
    let ours = zipper_outcome(&image, work.path());
    let accepted = sevenzip_accepts(binary, &image);
    match (ours, accepted) {
        (Outcome::Refused(_), false) => {}
        (Outcome::Extracted(tree), false) => {
            panic!(
                "{label}: 7-Zip refuses it and Zipper extracted {} entries",
                tree.len()
            )
        }
        (Outcome::Refused(message), true) => assert!(
            STRICTER.iter().any(|check| message.contains(check)),
            "{label}: 7-Zip extracts it and Zipper refused: {message}"
        ),
        (Outcome::Extracted(ours), true) => {
            let (_, theirs, _) = sevenzip_run(binary, &image, &work.path().join("7z"));
            assert_eq!(
                ours, theirs,
                "{label}: Zipper and 7-Zip extract different trees"
            );
        }
    }
}

fn flip_sweep(bytes: &[u8], ranges: &[(usize, usize, usize)], label: &str) {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut damaged = bytes.to_vec();
    for &(start, end, step) in ranges {
        for at in (start..end.min(bytes.len())).step_by(step.max(1)) {
            damaged[at] ^= 0x10;
            judge(&binary, &damaged, &format!("{label}: byte {at} flipped"));
            damaged[at] = bytes[at];
        }
    }
}

fn block_ranges(
    blocks: &[u64],
    header_step: usize,
    rest_step: usize,
) -> Vec<(usize, usize, usize)> {
    blocks
        .iter()
        .flat_map(|&block| {
            let at = block as usize * BLOCK;
            [
                (at, at + 0x80, header_step),
                (at + 0x80, at + BLOCK, rest_step),
            ]
        })
        .collect()
}

fn superblock_ranges(
    layout: &Layout,
    header_step: usize,
    rest_step: usize,
) -> Vec<(usize, usize, usize)> {
    let mut ranges = vec![(0, 0x3D8, header_step), (0x3D8, BLOCK, rest_step)];
    let area: Vec<u64> = (layout.descriptor_base..layout.descriptor_base + 4)
        .chain([layout.data_base])
        .collect();
    ranges.extend(block_ranges(&area, header_step, rest_step));
    ranges
}

fn map_ranges(layout: &Layout, header_step: usize, rest_step: usize) -> Vec<(usize, usize, usize)> {
    let mut blocks = vec![layout.container_omap];
    blocks.extend(&layout.container_omap_nodes);
    blocks.extend(&layout.volume_omaps);
    blocks.extend(layout.volume_omap_nodes.iter().flatten());
    block_ranges(&blocks, header_step, rest_step)
}

fn tree_ranges(
    layout: &Layout,
    header_step: usize,
    rest_step: usize,
) -> Vec<(usize, usize, usize)> {
    let blocks: Vec<u64> = layout.file_tree_nodes.iter().flatten().copied().collect();
    block_ranges(&blocks, header_step, rest_step)
}

fn volume_ranges(
    layout: &Layout,
    header_step: usize,
    rest_step: usize,
) -> Vec<(usize, usize, usize)> {
    let mut ranges = block_ranges(&layout.volume_superblocks, header_step, rest_step);
    for (_, block, count) in &layout.extents {
        let at = *block as usize * BLOCK;
        ranges.push((at, at + *count as usize * BLOCK, rest_step));
    }
    ranges
}

#[test]
fn damaged_superblocks_and_checkpoints_are_refused_when_7zip_refuses_them() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &superblock_ranges(&layout, 3, 509), "superblock");
}

#[test]
#[ignore = "full sweep; cargo xtask release-checks runs it"]
fn every_superblock_and_checkpoint_byte_is_refused_when_7zip_refuses_it() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &superblock_ranges(&layout, 1, 7), "superblock");
}

#[test]
fn damaged_object_maps_are_refused_when_7zip_refuses_them() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &map_ranges(&layout, 5, 997), "object map");
}

#[test]
#[ignore = "full sweep; cargo xtask release-checks runs it"]
fn every_object_map_byte_is_refused_when_7zip_refuses_it() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &map_ranges(&layout, 1, 13), "object map");
}

#[test]
fn damaged_file_tree_nodes_are_refused_when_7zip_refuses_them() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &tree_ranges(&layout, 9, 1499), "file tree");
}

#[test]
#[ignore = "full sweep; cargo xtask release-checks runs it"]
fn every_file_tree_byte_is_refused_when_7zip_refuses_it() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &tree_ranges(&layout, 1, 11), "file tree");
}

#[test]
fn damaged_volume_superblocks_and_extents_are_refused_when_7zip_refuses_them() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &volume_ranges(&layout, 5, 1201), "volume");
}

#[test]
#[ignore = "full sweep; cargo xtask release-checks runs it"]
fn every_volume_superblock_and_extent_byte_is_refused_when_7zip_refuses_it() {
    let (bytes, layout) = base();
    flip_sweep(&bytes, &volume_ranges(&layout, 1, 17), "volume");
}

#[test]
fn cut_containers_are_refused_with_nothing_published() {
    let (bytes, _) = base();
    let binary = sevenzip_binary();
    for length in (0..bytes.len()).step_by(BLOCK / 2 + 3) {
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("cut.img");
        fs::write(&image, &bytes[..length]).unwrap();
        match zipper_outcome(&image, work.path()) {
            Outcome::Refused(_) => {}
            Outcome::Extracted(found) => panic!("cut at {length}: extracted {found:?}"),
        }
        if let Some(binary) = &binary {
            judge(binary, &bytes[..length], &format!("cut at {length}"));
        }
    }
}

struct Record {
    node: usize,
    key: std::ops::Range<usize>,
    value: std::ops::Range<usize>,
}

fn leaf_records(bytes: &[u8], layout: &Layout) -> Vec<Record> {
    let mut found = Vec::new();
    for &block in layout.file_tree_nodes.iter().flatten() {
        let node = block as usize * BLOCK;
        let at = |offset: usize| {
            u16::from_le_bytes([bytes[node + offset], bytes[node + offset + 1]]) as usize
        };
        let flags = at(0x20);
        if flags & 2 == 0 {
            continue;
        }
        let keys = u32::from_le_bytes(bytes[node + 0x24..node + 0x28].try_into().unwrap()) as usize;
        let table = at(0x2A);
        let value_end = node + if flags & 1 != 0 { BLOCK - 0x28 } else { BLOCK };
        for index in 0..keys {
            let toc = 0x38 + index * 8;
            let key = node + 0x38 + table + at(toc);
            let value = value_end - at(toc + 4);
            found.push(Record {
                node,
                key: key..key + at(toc + 2),
                value: value..value + at(toc + 6),
            });
        }
    }
    found
}

fn record_id(bytes: &[u8], record: &Record) -> (u64, u64) {
    let header = u64::from_le_bytes(
        bytes[record.key.start..record.key.start + 8]
            .try_into()
            .unwrap(),
    );
    (header & 0x0FFF_FFFF_FFFF_FFFF, header >> 60)
}

fn patch(
    bytes: &mut [u8],
    layout: &Layout,
    pick: impl Fn(&[u8], &Record) -> bool,
    edit: impl Fn(&mut [u8]),
) {
    let records = leaf_records(bytes, layout);
    let record = records
        .iter()
        .find(|record| pick(bytes, record))
        .expect("no record to patch");
    edit(&mut bytes[record.value.clone()]);
    let node = record.node;
    seal(&mut bytes[node..node + BLOCK]);
}

fn refused(bytes: &[u8], wanted: &str) {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("crafted.img");
    fs::write(&image, bytes).unwrap();
    match zipper_outcome(&image, work.path()) {
        Outcome::Refused(message) => assert!(
            message.contains(wanted),
            "refused for another reason: {message}; wanted {wanted:?}"
        ),
        Outcome::Extracted(found) => panic!("extracted {found:?}; wanted a refusal for {wanted:?}"),
    }
}

fn sevenzip_refuses(bytes: &[u8]) {
    if let Some(binary) = sevenzip_binary() {
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("crafted.img");
        fs::write(&image, bytes).unwrap();
        assert!(!sevenzip_accepts(&binary, &image), "7-Zip accepts it");
    }
}

fn put64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn b_tree_loops_and_nodes_outside_the_container_are_refused() {
    let (bytes, layout) = base();
    let root = layout.file_tree_roots[0] as usize * BLOCK;
    let root_oid = u64::from_le_bytes(bytes[root + 8..root + 16].try_into().unwrap());
    assert!(bytes[root + 0x22] > 0, "the file tree needs an index root");
    let mut looped = bytes.clone();
    let table = u16::from_le_bytes([looped[root + 0x2A], looped[root + 0x2B]]) as usize;
    let _ = table;
    let value = root + BLOCK
        - 0x28
        - u16::from_le_bytes([looped[root + 0x3C], looped[root + 0x3D]]) as usize;
    put64(&mut looped, value, root_oid);
    seal(&mut looped[root..root + BLOCK]);
    refused(&looped, "file-system tree");
    sevenzip_refuses(&looped);

    let omap_root = *layout.volume_omap_nodes[0].last().unwrap() as usize * BLOCK;
    assert!(
        looped[omap_root + 0x22] > 0,
        "the object map needs an index root"
    );
    let mut omap_loop = bytes.clone();
    let value = omap_root + BLOCK
        - 0x28
        - u16::from_le_bytes([omap_loop[omap_root + 0x3A], omap_loop[omap_root + 0x3B]]) as usize;
    put64(&mut omap_loop, value, omap_root as u64 / BLOCK as u64);
    seal(&mut omap_loop[omap_root..omap_root + BLOCK]);
    refused(&omap_loop, "volume object map");
    sevenzip_refuses(&omap_loop);

    let leaf = layout.volume_omap_nodes[0][0] as usize * BLOCK;
    let mut outside = bytes.clone();
    let value =
        leaf + BLOCK - u16::from_le_bytes([outside[leaf + 0x3A], outside[leaf + 0x3B]]) as usize;
    put64(&mut outside, value + 8, layout.block_count + 10);
    seal(&mut outside[leaf..leaf + BLOCK]);
    refused(&outside, "lies outside the container");
    sevenzip_refuses(&outside);
}

#[test]
fn bad_checksums_and_extents_past_the_end_are_refused() {
    let (bytes, layout) = base();
    let mut damaged = bytes.clone();
    let leaf = layout.file_tree_nodes[0][0] as usize * BLOCK;
    damaged[leaf + 200] ^= 1;
    refused(&damaged, "Fletcher-64");
    sevenzip_refuses(&damaged);

    let mut far = bytes.clone();
    let count = layout.block_count;
    patch(
        &mut far,
        &layout,
        |bytes, record| record_id(bytes, record) == (18, 8),
        |value| put64(value, 8, count - 1),
    );
    refused(&far, "past the end of the container");
    sevenzip_refuses(&far);
}

#[test]
fn records_that_disagree_with_their_inodes_are_refused() {
    let (bytes, layout) = base();
    let drec_of = |name: &'static str| {
        move |bytes: &[u8], record: &Record| {
            record_id(bytes, record).1 == 9
                && bytes[record.key.clone()]
                    .windows(name.len())
                    .any(|window| window == name.as_bytes())
        }
    };
    let mut missing = bytes.clone();
    patch(&mut missing, &layout, drec_of("readme.txt"), |value| {
        put64(value, 0, 999_999)
    });
    refused(&missing, "which the volume lacks");

    let mut unlinked = bytes.clone();
    patch(&mut unlinked, &layout, drec_of("readme.txt"), |value| {
        put64(value, 0, 30)
    });
    refused(&unlinked, "has no directory record");

    let mut kind = bytes.clone();
    patch(&mut kind, &layout, drec_of("readme.txt"), |value| {
        value[0x10] = 4
    });
    refused(&kind, "type that disagrees");

    let mut folder_twice = bytes.clone();
    patch(&mut folder_twice, &layout, drec_of("readme.txt"), |value| {
        put64(value, 0, 21);
        value[0x10] = 4;
    });
    refused(&folder_twice, "two names");
}

fn built(volume: Volume) -> Vec<u8> {
    apfs_writer::build(&Container::new(vec![volume])).0.to_vec()
}

#[test]
fn encrypted_snapshot_sealed_and_fusion_containers_are_refused_by_name_and_go_to_7zip() {
    let entries = vec![file(16, ROOT, "a.txt", b"a\n")];
    let mut encrypted = Volume::new(entries.clone());
    encrypted.encrypted = true;
    let mut snapshots = Volume::new(entries.clone());
    snapshots.snapshots = 2;
    let mut sealed = Volume::new(entries.clone());
    sealed.sealed = true;
    let mut fusion = Container::new(vec![Volume::new(entries.clone())]);
    fusion.fusion = true;
    let cases = [
        (built(encrypted), "encrypted"),
        (built(snapshots), "snapshots"),
        (built(sealed), "sealed volume"),
        (apfs_writer::build(&fusion).0.to_vec(), "Fusion container"),
    ];
    for (bytes, name) in cases {
        refused(&bytes, name);
        refused(&bytes, "--zip-engine backend");
        let work = tempfile::tempdir().unwrap();
        let image = work.path().join("refused.img");
        fs::write(&image, &bytes).unwrap();
        assert_eq!(
            zipper::choose_reader(Engine::Auto, &image).unwrap(),
            Reader::Backend { format: None },
            "{name}"
        );
        assert_eq!(
            zipper::choose_reader(Engine::Native, &image).unwrap(),
            Reader::Apfs,
            "{name}"
        );
    }
}

#[test]
fn methods_7zip_does_not_decode_and_resource_forks_are_refused_by_name() {
    let data = b"lzfse data".repeat(10);
    let lzfse = Volume::new(vec![compressed(
        16,
        ROOT,
        "lzfse.txt",
        decmpfs_header(12, data.len() as u64, &[]),
        data.len() as u64,
        Some(vec![0u8; 64]),
    )]);
    refused(&built(lzfse), "method 12, which 7-Zip does not decode");

    let mut forked = file(16, ROOT, "forked.txt", b"data fork\n");
    forked.xattrs.push(Xattr {
        name: "com.apple.ResourceFork".into(),
        value: b"resource".to_vec(),
        stream: true,
    });
    refused(&built(Volume::new(vec![forked])), "com.apple.ResourceFork");

    let mut empty = file(16, ROOT, "empty-fork.txt", b"data fork\n");
    empty.xattrs.push(Xattr {
        name: "com.apple.ResourceFork".into(),
        value: Vec::new(),
        stream: false,
    });
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("empty-fork.img");
    fs::write(&image, built(Volume::new(vec![empty]))).unwrap();
    match zipper_outcome(&image, work.path()) {
        Outcome::Extracted(found) => assert_eq!(found.len(), 1, "{found:?}"),
        Outcome::Refused(message) => panic!("an empty resource fork was refused: {message}"),
    }
}

#[test]
fn unsafe_names_links_and_folder_shapes_are_refused_with_nothing_published() {
    let cases: Vec<(Vec<Entry>, &str)> = vec![
        (vec![file(16, ROOT, "..", b"x")], "unsafe"),
        (vec![file(16, ROOT, "back\\slash", b"x")], "unsafe"),
        (vec![file(16, ROOT, "colon:name", b"x")], "unsafe"),
        (
            vec![symlink(16, ROOT, "escape", "../../outside")],
            "points outside the destination",
        ),
        (
            vec![file(16, ROOT, "twice", b"a"), file(17, ROOT, "twice", b"b")],
            "two entries named",
        ),
        (vec![folder(16, 17, "a"), folder(17, 16, "b")], "loop"),
    ];
    for (entries, wanted) in cases {
        refused(&built(Volume::new(entries)), wanted);
    }
    let mut deep = Vec::new();
    let mut parent = ROOT;
    for level in 0..1030u64 {
        deep.push(folder(16 + level, parent, "d"));
        parent = 16 + level;
    }
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("deep.img");
    fs::write(&image, built(Volume::new(deep))).unwrap();
    let error = zipper::native_apfs::read_archive(&image, None, Default::default()).unwrap_err();
    assert!(
        format!("{error:#}").contains("deeper than 1024"),
        "{error:#}"
    );
}

#[test]
fn the_private_folder_fifos_and_blank_names_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut volume = Volume::new(vec![
        file(16, ROOT, " ", b"a single space\n"),
        Entry {
            mode: 0o010_644,
            ..apfs_writer::file(17, ROOT, "pipe", b"")
        },
    ]);
    volume.entries[1].kind = Kind::Fifo;
    volume.private_entries = vec![file(
        18,
        apfs_writer::PRIVATE,
        "kept.txt",
        b"in private-dir\n",
    )];
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("shapes.img");
    fs::write(&image, built(volume)).unwrap();
    let Outcome::Extracted(ours) = zipper_outcome(&image, work.path()) else {
        panic!("refused");
    };
    let (_, theirs, output) = sevenzip_run(&binary, &image, &work.path().join("7z"));
    assert_eq!(ours, theirs, "{output}");
    assert!(ours.contains_key("private-dir/kept.txt"), "{ours:?}");
}

#[test]
fn selection_and_strip_pick_the_paths_7zip_writes() {
    let volumes = vec![
        Volume::new(vec![
            folder(16, ROOT, "docs"),
            file(17, 16, "a.txt", b"a\n"),
            file(18, ROOT, "b.txt", b"b\n"),
            hard_link(19, 16, "b-again.txt", 18),
        ]),
        Volume::new(vec![file(16, ROOT, "c.txt", b"c\n")]),
    ];
    let mut container = Container::new(volumes);
    container.max_volumes = 2;
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join("select.img");
    fs::write(&image, apfs_writer::build(&container).0.to_vec()).unwrap();
    let paths = vec!["Volume1/docs".to_owned()];
    let into = work.path().join("selected");
    zipper::native_apfs::read_archive(
        &image,
        Some(&into),
        zipper::native_apfs::ReadOptions {
            paths: &paths,
            strip: zipper::Strip::Components(1),
            ..Default::default()
        },
    )
    .unwrap();
    let found = tree(&into);
    assert_eq!(
        found.keys().cloned().collect::<Vec<_>>(),
        vec!["docs", "docs/a.txt", "docs/b-again.txt"],
        "{found:?}"
    );
    assert_eq!(fs::read(into.join("docs/b-again.txt")).unwrap(), b"b\n");
}
