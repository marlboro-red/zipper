//! The disk reader extracts FAT volumes, MBR and GPT disks and VHD and VHDX
//! images byte for byte, rejects each hostile layout with nothing left
//! behind, sends what it does not read to 7-Zip by default, and honours the
//! options the other readers take.
#[path = "common/disk_writer.rs"]
mod disk_writer;

use disk_writer::{Built, Item, SECTOR, Volume, build, file, folder};
use std::{collections::BTreeMap, fs, path::Path};
use zipper::{
    Engine, Reader, Strip,
    disk::{ReadOptions, read_archive},
};

fn leftovers(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .display()
                .to_string()
        })
        .filter(|path| !path.starts_with("case."))
        .collect();
    found.sort();
    found
}

fn refused_named(case: &str, bytes: &[u8], name: &str, words: &str) {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join(name);
    fs::write(&archive, bytes).unwrap();
    fs::create_dir_all(root.path().join("outer/inner")).unwrap();
    let destination = root.path().join("outer/inner/out");
    let error = match read_archive(&archive, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: accepted a hostile image"),
        Err(error) => format!("{error:#}"),
    };
    assert_eq!(
        leftovers(root.path()),
        ["outer", "outer/inner"].map(|path| path.replace('/', std::path::MAIN_SEPARATOR_STR)),
        "{case}: left output behind"
    );
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile image"
    );
    assert!(
        error.contains(words),
        "{case}: {error:?} does not say {words:?}"
    );
}

fn refused(case: &str, bytes: &[u8], words: &str) {
    refused_named(case, bytes, "case.img", words);
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let data = if entry.file_type().is_dir() {
                b"<dir>".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, data)
        })
        .collect()
}

fn extracted_named(
    bytes: &[u8],
    name: &str,
    options: ReadOptions,
) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join(name);
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    read_archive(&archive, Some(&destination), options)?;
    let found = tree(&destination);
    common_deletable(root.path());
    Ok(found)
}

fn common_deletable(root: &Path) {
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_file() {
            let mut permissions = fs::metadata(entry.path()).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(entry.path(), permissions).unwrap();
        }
    }
}

fn extracted(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    extracted_named(bytes, "case.img", ReadOptions::default()).unwrap()
}

fn expected(items: &[Item], prefix: &str, into: &mut BTreeMap<String, Vec<u8>>) {
    for item in items {
        match item {
            Item::File { name, data, .. } => {
                into.insert(format!("{prefix}{}", name.replace('/', "_")), data.clone());
            }
            Item::Folder { name, items } => {
                into.insert(format!("{prefix}{name}"), b"<dir>".to_vec());
                expected(items, &format!("{prefix}{name}/"), into);
            }
        }
    }
}

fn pattern(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3] ^ seed)
        .collect()
}

fn sample() -> Vec<Item> {
    let mut deep = file("bottom.txt", b"at the bottom\n");
    for level in (0..12).rev() {
        deep = folder(&format!("level {level}"), vec![deep]);
    }
    vec![
        file("HELLO.TXT", b"hello\n"),
        file("A long file name with spaces.txt", &pattern(3000, 1)),
        file("empty", b""),
        Item::File {
            name: "READONLY.TXT".to_owned(),
            data: b"keep\n".to_vec(),
            read_only: true,
        },
        folder(
            "Folder",
            vec![
                file("inner.bin", &pattern(70_000, 2)),
                file("Ünïcode ✓.txt", b"unicode\n"),
                folder("EMPTYDIR", vec![]),
            ],
        ),
        deep,
    ]
}

fn sample_volume(bits: u32) -> Built {
    let sectors = match bits {
        12 => 4000,
        16 => 40_000,
        _ => 140_000,
    };
    build(&Volume::new(bits, sectors), &sample())
}

fn sample_tree() -> BTreeMap<String, Vec<u8>> {
    let mut wanted = BTreeMap::new();
    expected(&sample(), "", &mut wanted);
    wanted
}

#[test]
fn writer_volumes_extract_byte_for_byte_in_every_container() {
    let wanted = sample_tree();
    for bits in [12, 16, 32] {
        for (fragment, deleted) in [(false, false), (true, true)] {
            let mut volume = Volume::new(
                bits,
                if bits == 32 {
                    140_000
                } else if bits == 16 {
                    40_000
                } else {
                    4000
                },
            );
            volume.fragment = fragment;
            volume.deleted = deleted;
            let built = build(&volume, &sample());
            if fragment {
                let chain = &built.chains["Folder/inner.bin"];
                assert!(
                    chain.windows(2).any(|pair| pair[1] != pair[0] + 1),
                    "not fragmented"
                );
            }
            let label = format!("FAT{bits} fragment {fragment}");
            assert_eq!(extracted(&built.bytes), wanted, "{label} raw");
            let mbr = disk_writer::mbr(&[(0x0C, 2048, &built.bytes)], 64);
            assert_eq!(extracted(&mbr), wanted, "{label} MBR");
            let gpt =
                disk_writer::gpt(&[(disk_writer::BASIC_DATA, "data", 2048, &built.bytes)], 64);
            assert_eq!(extracted(&gpt), wanted, "{label} GPT");
            assert_eq!(
                extracted_named(
                    &disk_writer::vhd_fixed(&mbr),
                    "case.vhd",
                    ReadOptions::default()
                )
                .unwrap(),
                wanted,
                "{label} fixed VHD"
            );
            assert_eq!(
                extracted(&disk_writer::vhd_dynamic(&mbr, 19)),
                wanted,
                "{label} dynamic VHD"
            );
            for block_log in [20, 25] {
                assert_eq!(
                    extracted(&disk_writer::vhdx(&gpt, block_log)),
                    wanted,
                    "{label} VHDX {block_log}"
                );
            }
        }
    }
}

#[test]
fn several_partitions_extract_as_one_file_each_with_the_gaps_between() {
    let first = sample_volume(16);
    let second = sample_volume(12);
    let disk = disk_writer::mbr(
        &[
            (0x06, 64, &first.bytes),
            (0x83, 64 + 40_000 + 10, &second.bytes),
        ],
        7,
    );
    let found = extracted(&disk);
    let mut names: Vec<&str> = found.keys().map(String::as_str).collect();
    names.sort();
    assert_eq!(names, ["0.fat", "1.img", "2"]);
    assert_eq!(found["0.fat"], first.bytes);
    assert_eq!(found["1.img"], second.bytes);
    assert_eq!(found["2"], vec![0; 7 * SECTOR]);
    let gpt = disk_writer::gpt(
        &[
            (disk_writer::BASIC_DATA, "Basic data", 40, &first.bytes),
            (disk_writer::LINUX_DATA, "", 40 + 40_000, &second.bytes),
        ],
        0,
    );
    let found = extracted(&gpt);
    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        ["0.Basic data.fat", "1.img"]
    );
    assert_eq!(found["1.img"], second.bytes);
}

#[test]
fn extended_partitions_list_their_logical_partitions_as_7zip_does() {
    let first = sample_volume(12);
    let logical = sample_volume(12);
    let sectors = (first.bytes.len() / SECTOR) as u32;
    let extended = 64 + sectors;
    let ebr2 = extended + 63 + sectors;
    let end = ebr2 + 63 + sectors;
    let mut disk = vec![0u8; (end as usize + 2) * SECTOR];
    disk[..SECTOR].copy_from_slice(&disk_writer::mbr_sector(&[
        (0x01, 64, sectors),
        (0x05, extended, end - extended),
    ]));
    disk[extended as usize * SECTOR..][..SECTOR].copy_from_slice(&disk_writer::mbr_sector(&[
        (0x01, 63, sectors),
        (0x05, ebr2 - extended, end - ebr2),
    ]));
    disk[ebr2 as usize * SECTOR..][..SECTOR]
        .copy_from_slice(&disk_writer::mbr_sector(&[(0x01, 63, sectors)]));
    for at in [64, extended + 63, ebr2 + 63] {
        disk[at as usize * SECTOR..][..first.bytes.len()].copy_from_slice(&logical.bytes);
    }
    let found = extracted(&disk);
    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        ["0.fat", "1.fat", "2.fat", "3"]
    );
    assert!(found.values().take(3).all(|bytes| *bytes == logical.bytes));
    let mut looping = disk.clone();
    looping[extended as usize * SECTOR..][..SECTOR].copy_from_slice(&disk_writer::mbr_sector(&[
        (0x01, 63, sectors),
        (0x05, 0, end - extended),
    ]));
    let found = extracted(&looping);
    assert!(
        found.len() <= 130,
        "an extended partition that names itself loops"
    );
}

fn first_cluster(built: &Built, path: &str) -> u32 {
    built.chains[path][0]
}

fn set_record_cluster(built: &mut Built, path: &str, cluster: u32) {
    let at = built.record(path);
    built.bytes[at + 26..at + 28].copy_from_slice(&(cluster as u16).to_le_bytes());
    built.bytes[at + 20..at + 22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
}

#[test]
fn cluster_chains_that_loop_cross_or_end_early_are_refused() {
    for bits in [12, 16, 32] {
        let label = |case: &str| format!("FAT{bits} {case}");
        let mut looping = sample_volume(bits);
        let chain = looping.chains["Folder/inner.bin"].clone();
        looping.set_entry(chain[3], chain[1]);
        refused(&label("file chain loop"), &looping.bytes, "share cluster");
        let mut crossed = sample_volume(bits);
        let target = *crossed.chains["Folder/inner.bin"].last().unwrap();
        set_record_cluster(&mut crossed, "HELLO.TXT", target);
        refused(
            &label("cross-linked files"),
            &crossed.bytes,
            "share cluster",
        );
        let mut into_folder = sample_volume(bits);
        let folder_cluster = first_cluster(&into_folder, "Folder");
        set_record_cluster(&mut into_folder, "HELLO.TXT", folder_cluster);
        refused(
            &label("file in a folder's cluster"),
            &into_folder.bytes,
            "which a folder holds",
        );
        let many: Vec<Item> = (0..100)
            .map(|index| file(&format!("F{index:03}"), b""))
            .collect();
        let sectors = match bits {
            12 => 8000,
            16 => 40_000,
            _ => 140_000,
        };
        let mut volume = Volume::new(bits, sectors);
        volume.cluster_sectors = 2;
        let mut folder_loop = build(&volume, &[folder("MANY", many)]);
        let chain = folder_loop.chains["MANY"].clone();
        assert!(chain.len() >= 3, "{chain:?}");
        folder_loop.set_entry(chain[1], chain[0]);
        refused(
            &label("folder chain loop"),
            &folder_loop.bytes,
            "claim cluster",
        );
        let mut cycle = sample_volume(bits);
        let parent = first_cluster(&cycle, "level 0");
        set_record_cluster(&mut cycle, "level 0/level 1/level 2", parent);
        refused(&label("folder cycle"), &cycle.bytes, "claim cluster");
        let mut shared = sample_volume(bits);
        let folder_cluster = first_cluster(&shared, "Folder");
        let at = shared.record("Folder/EMPTYDIR");
        shared.bytes[at + 26..at + 28].copy_from_slice(&(folder_cluster as u16).to_le_bytes());
        refused(
            &label("folder inside itself"),
            &shared.bytes,
            "claim cluster",
        );
        let mut short = sample_volume(bits);
        let chain = short.chains["Folder/inner.bin"].clone();
        let end = short.end_of_chain();
        short.set_entry(chain[2], end);
        refused(
            &label("chain shorter than its size"),
            &short.bytes,
            "ends before its size",
        );
        let mut long = sample_volume(bits);
        let at = long.record("Folder/inner.bin");
        long.bytes[at + 28..at + 32].copy_from_slice(&1000u32.to_le_bytes());
        refused(
            &label("chain longer than its size"),
            &long.bytes,
            "does not end where its size does",
        );
        let mut empty = sample_volume(bits);
        set_record_cluster(&mut empty, "empty", 5);
        refused(
            &label("empty file with a cluster"),
            &empty.bytes,
            "names a cluster",
        );
        let mut outside = sample_volume(bits);
        let clusters = outside.clusters;
        set_record_cluster(&mut outside, "HELLO.TXT", clusters + 2);
        refused(
            &label("cluster past the volume"),
            &outside.bytes,
            "outside the volume",
        );
        let mut free = sample_volume(bits);
        let chain = free.chains["Folder/inner.bin"].clone();
        free.set_entry(chain[1], 0);
        refused(
            &label("chain into a free cluster"),
            &free.bytes,
            "outside the volume",
        );
    }
}

#[test]
fn damaged_boot_sectors_and_tables_are_refused() {
    let built = sample_volume(16);
    let mut media = built.bytes.clone();
    media[built.fat_at] = 0x12;
    refused("media byte", &media, "media");
    let cut = built.fat_at + built.fat_bytes / 2;
    refused("image ends inside its FAT", &built.bytes[..cut], "FAT");
    let mut both = built.bytes.clone();
    let at = built.record("HELLO.TXT");
    both[at + 11] = 0x18;
    refused(
        "record both folder and label",
        &both,
        "both a folder and a volume label",
    );
    let mut long = built.bytes.clone();
    let at = built.record("A long file name with spaces.txt") - 32;
    long[at + 26] = 1;
    refused("long name record with a cluster", &long, "long name record");
    let mut order = built.bytes.clone();
    let at = built.record("A long file name with spaces.txt") - 32;
    order[at] = 0x05;
    refused("long name records out of order", &order, "out of order");
    let mut volume = Volume::new(16, 40_000);
    volume.label = None;
    let plain = build(&volume, &sample());
    assert_eq!(extracted(&plain.bytes), sample_tree());
}

#[test]
fn truncated_images_are_refused_or_extract_the_whole_tree() {
    let built = sample_volume(12);
    let whole = extracted(&built.bytes);
    for cut in (0..built.bytes.len()).step_by(SECTOR).chain([1, 511, 513]) {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("case.img");
        fs::write(&archive, &built.bytes[..cut]).unwrap();
        let destination = root.path().join("out");
        match read_archive(&archive, Some(&destination), ReadOptions::default()) {
            Ok(_) => assert_eq!(tree(&destination), whole, "cut at {cut}"),
            Err(_) => assert!(!destination.exists(), "cut at {cut} left output"),
        }
        common_deletable(root.path());
    }
}

fn long_name_record(built: &mut Built, path: &str) -> usize {
    built.record(path) - 32
}

#[test]
fn names_follow_7zip_and_unsafe_ones_are_refused() {
    let unsafe_names = [
        ("back\\slash.txt", "not a safe"),
        ("colon:name.txt", "not a safe"),
        ("..", "not a safe"),
    ];
    for (name, words) in unsafe_names {
        let built = build(&Volume::new(16, 40_000), &[file(name, b"x")]);
        refused(name, &built.bytes, words);
    }
    let slash = build(&Volume::new(16, 40_000), &[file("a/b.txt", b"slash")]);
    assert_eq!(
        extracted(&slash.bytes).keys().collect::<Vec<_>>(),
        ["a_b.txt"]
    );
    let twice = build(
        &Volume::new(16, 40_000),
        &[file("SAME.TXT", b"1"), file("same.txt", b"2")],
    );
    let mut twice = twice;
    let at = twice.record("same.txt");
    let short = twice.bytes[twice.record("SAME.TXT")..][..11].to_vec();
    twice.bytes[at..at + 11].copy_from_slice(&short);
    twice.bytes[at - 32] = 0xE5;
    refused("two entries of one name", &twice.bytes, "two entries named");
    let mut checksum = sample_volume(16);
    let at = long_name_record(&mut checksum, "A long file name with spaces.txt");
    checksum.bytes[at + 13] ^= 0xFF;
    let found = extracted(&checksum.bytes);
    assert!(found.contains_key("ALONGF~1.TXT"), "{:?}", found.keys());
    let mut blank = sample_volume(16);
    let at = blank.record("HELLO.TXT");
    blank.bytes[at..at + 11].copy_from_slice(b"           ");
    assert!(extracted(&blank.bytes).contains_key("[NONAME]-0"));
    let mut high = sample_volume(16);
    let at = high.record("HELLO.TXT");
    high.bytes[at] = 0x05;
    refused("short name outside ASCII", &high.bytes, "outside ASCII");
}

#[test]
fn folders_nested_too_deep_or_too_long_are_refused() {
    let mut deep = file("leaf", b"x");
    for level in 0..258 {
        deep = folder(&format!("D{level}"), vec![deep]);
    }
    let built = build(&Volume::new(16, 40_000), &[deep]);
    refused("folders 258 deep", &built.bytes, "deep");
    let mut volume = Volume::new(32, 140_000);
    volume.cluster_sectors = 8;
    let many: Vec<Item> = (0..65_600)
        .map(|index| file(&format!("F{index:07}"), b""))
        .collect();
    let built = build(&volume, &[folder("BIG", many)]);
    refused("a folder over 2 MiB", &built.bytes, "longer than 2 MiB");
}

fn ntfs_boot() -> Vec<u8> {
    let mut volume = vec![0u8; 64 * SECTOR];
    volume[..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
    volume[3..12].copy_from_slice(b"NTFS    \0");
    volume[510] = 0x55;
    volume[511] = 0xAA;
    volume
}

fn reader(bytes: &[u8], name: &str, engine: Engine) -> Reader {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join(name);
    fs::write(&archive, bytes).unwrap();
    zipper::choose_reader(engine, &archive).unwrap()
}

#[test]
fn file_systems_zipper_does_not_read_go_to_7zip_and_are_refused_by_name_in_process() {
    let ntfs = disk_writer::mbr(&[(0x07, 64, &ntfs_boot())], 0);
    refused("lone NTFS partition", &ntfs, "NTFS");
    assert_eq!(
        reader(&ntfs, "disk.img", Engine::Auto),
        Reader::Backend { format: None }
    );
    assert_eq!(reader(&ntfs, "disk.img", Engine::Native), Reader::Disk);
    let gpt = disk_writer::gpt(
        &[(
            disk_writer::BASIC_DATA,
            "Basic data partition",
            40,
            &ntfs_boot(),
        )],
        0,
    );
    refused("lone NTFS GPT partition", &gpt, "NTFS");
    assert_eq!(
        reader(&gpt, "disk.img", Engine::Auto),
        Reader::Backend { format: None }
    );
    let vhdx = disk_writer::vhdx(&ntfs, 20);
    assert_eq!(
        reader(&vhdx, "disk.vhdx", Engine::Auto),
        Reader::Backend { format: None }
    );
    let fat = sample_volume(12);
    let both = disk_writer::mbr(&[(0x07, 64, &ntfs_boot()), (0x01, 128, &fat.bytes)], 0);
    let found = extracted(&both);
    assert_eq!(found.keys().collect::<Vec<_>>(), ["0.ntfs", "1.fat"]);
    assert_eq!(reader(&both, "disk.img", Engine::Auto), Reader::Disk);
    let named = disk_writer::gpt(
        &[
            (disk_writer::LINUX_DATA, "linux/part", 40, &fat.bytes),
            (disk_writer::LINUX_DATA, "second", 40 + 4000, &fat.bytes),
        ],
        0,
    );
    refused("partition name with a slash", &named, "plain file name");
    assert_eq!(
        reader(&named, "disk.img", Engine::Auto),
        Reader::Backend { format: None }
    );
    let mut table = disk_writer::gpt(&[(disk_writer::BASIC_DATA, "data", 40, &fat.bytes)], 0);
    assert_eq!(extracted(&table), extracted(&fat.bytes));
    table[2 * SECTOR + 60] ^= 1;
    refused(
        "GPT table that fails its checksum",
        &table,
        "no partition table or file system",
    );
    for header_size in [0u32, 4, 16, 19] {
        let mut tiny = disk_writer::gpt(&[(disk_writer::BASIC_DATA, "data", 40, &fat.bytes)], 0);
        tiny[SECTOR + 12..SECTOR + 16].copy_from_slice(&header_size.to_le_bytes());
        refused(
            "GPT header shorter than its checksum",
            &tiny,
            "no partition table",
        );
        let mut sealed = tiny.clone();
        let mut header = sealed[SECTOR..2 * SECTOR].to_vec();
        header[16..20].fill(0);
        let crc = crc32fast::hash(&header[..header_size as usize]);
        sealed[SECTOR + 16..SECTOR + 20].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            extracted(&sealed),
            extracted(&fat.bytes),
            "a {header_size}-byte GPT header whose checksum covers it"
        );
    }
    let zeros = disk_writer::mbr(&[(0x06, 64, &vec![0u8; 64 * SECTOR])], 0);
    refused(
        "partition of zeros",
        &zeros,
        "no partition table or file system",
    );
    assert_eq!(
        reader(&zeros, "disk.img", Engine::Auto),
        Reader::Backend { format: None }
    );
    assert_eq!(reader(&fat.bytes, "disk.img", Engine::Auto), Reader::Disk);
}

#[test]
fn vhd_images_are_checked_before_their_disk_is_read() {
    let built = sample_volume(16);
    let disk = disk_writer::mbr(&[(0x06, 64, &built.bytes)], 0);
    let dynamic = disk_writer::vhd_dynamic(&disk, 19);
    let mut differencing = dynamic.clone();
    let mut footer = disk_writer::vhd_footer(disk.len() as u64, 4, 512);
    differencing[..SECTOR].copy_from_slice(&footer);
    let end = differencing.len() - SECTOR;
    differencing[end..].copy_from_slice(&footer);
    refused("differencing VHD", &differencing, "differencing VHD");
    assert_eq!(
        reader(&differencing, "disk.vhd", Engine::Auto),
        Reader::Backend { format: None }
    );
    assert_eq!(
        reader(&differencing, "disk.vhd", Engine::Native),
        Reader::Disk
    );
    let mut checksum = dynamic.clone();
    checksum[SECTOR + 40] ^= 1;
    refused("dynamic header checksum", &checksum, "checksum");
    let mut mismatch = dynamic.clone();
    let last = mismatch.len() - 1;
    mismatch[last - 100] ^= 1;
    refused("footer copy differs", &mismatch, "does not repeat");
    let mut bat_past_end = dynamic.clone();
    bat_past_end[SECTOR + 16..SECTOR + 24].copy_from_slice(&(dynamic.len() as u64).to_be_bytes());
    let sum = !bat_past_end[SECTOR..SECTOR + 1024]
        .iter()
        .enumerate()
        .filter(|(index, _)| !(36..40).contains(index))
        .fold(0u32, |sum, (_, byte)| sum.wrapping_add(u32::from(*byte)));
    bat_past_end[SECTOR + 36..SECTOR + 40].copy_from_slice(&sum.to_be_bytes());
    refused(
        "block table past the end",
        &bat_past_end,
        "block table lies past",
    );
    let mut absent = dynamic.clone();
    let table = 1536;
    let first_block = (0..disk.len().div_ceil(1 << 19))
        .map(|index| {
            u32::from_be_bytes(
                absent[table + 4 * index..table + 4 * index + 4]
                    .try_into()
                    .unwrap(),
            )
        })
        .find(|entry| *entry != u32::MAX)
        .unwrap() as usize
        * SECTOR;
    absent[first_block] = 0x7F;
    refused("absent sector holding data", &absent, "bitmap marks absent");
    footer = disk_writer::vhd_footer(disk.len() as u64, 2, u64::MAX);
    let mut fixed = disk.clone();
    fixed.extend(footer);
    assert_eq!(
        extracted_named(&fixed, "disk.vhd", ReadOptions::default()).unwrap(),
        sample_tree()
    );
    let two = disk_writer::mbr(
        &[(0x06, 64, &built.bytes), (0x06, 64 + 40_000, &built.bytes)],
        0,
    );
    let mut fixed_two = two.clone();
    fixed_two.extend(disk_writer::vhd_footer(two.len() as u64, 2, u64::MAX));
    let as_vhd = extracted_named(&fixed_two, "disk.vhd", ReadOptions::default()).unwrap();
    assert_eq!(as_vhd.keys().collect::<Vec<_>>(), ["0.fat", "1.fat"]);
    let as_img = extracted_named(&fixed_two, "disk.img", ReadOptions::default()).unwrap();
    assert_eq!(
        as_img["2"].len(),
        SECTOR,
        "outside a .vhd name the footer is a trailing gap"
    );
}

#[test]
fn vhdx_images_are_checked_before_their_disk_is_read() {
    let built = sample_volume(16);
    let disk = disk_writer::mbr(&[(0x06, 64, &built.bytes)], 0);
    let image = disk_writer::vhdx(&disk, 20);
    let mut header = image.clone();
    header[(1 << 16) + 100] ^= 1;
    refused("header checksum", &header, "header checksum");
    let mut regions = image.clone();
    regions[(4 << 16) + 100] ^= 1;
    refused("region tables differ", &regions, "region tables differ");
    let mut log = image.clone();
    for at in [1usize << 16, 2 << 16] {
        log[at + 0x30..at + 0x40].fill(0x33);
        disk_writer::seal_crc32c(&mut log[at..at + 4096], 4);
    }
    refused("log never replayed", &log, "never replayed");
    let blocks: Vec<(u64, Vec<u8>)> = disk
        .chunks(1 << 20)
        .enumerate()
        .map(|(index, piece)| (index as u64, piece.to_vec()))
        .collect();
    let sparse = disk_writer::vhdx_sparse(disk.len() as u64, 20, &blocks, 0);
    assert_eq!(
        extracted_named(&sparse.bytes, "disk.vhdx", ReadOptions::default()).unwrap(),
        sample_tree()
    );
    let mut overlap = sparse.bytes.clone();
    let first = u64::from_le_bytes(
        overlap[sparse.bat_at..sparse.bat_at + 8]
            .try_into()
            .unwrap(),
    );
    overlap[sparse.bat_at + 8..sparse.bat_at + 16].copy_from_slice(&first.to_le_bytes());
    refused("blocks that overlap", &overlap, "share their bytes");
    let mut partial = sparse.bytes.clone();
    partial[sparse.bat_at] = (partial[sparse.bat_at] & !7) | 7;
    refused("partly present block", &partial, "partly present");
    let mut reserved = sparse.bytes.clone();
    reserved[sparse.bat_at + 1] |= 0x10;
    refused("reserved bits", &reserved, "reserved bits");
    let differencing = disk_writer::vhdx_sparse(disk.len() as u64, 20, &blocks, 2);
    refused(
        "differencing VHDX",
        &differencing.bytes,
        "differencing VHDX",
    );
    assert_eq!(
        reader(&differencing.bytes, "disk.vhdx", Engine::Auto),
        Reader::Backend { format: None }
    );
    let mut unknown = sparse.bytes.clone();
    let entry = sparse.metadata_at + 32 * 4;
    unknown[entry..entry + 16].copy_from_slice(&[0x99; 16]);
    unknown[entry + 24..entry + 28].copy_from_slice(&4u32.to_le_bytes());
    unknown[sparse.metadata_at + 10] = 5;
    refused("required unknown metadata", &unknown, "requires metadata");
    let mut short_table = sparse.bytes.clone();
    let region = (3 << 16) + 16 + 24;
    short_table[region..region + 4].copy_from_slice(&0u32.to_le_bytes());
    let table = short_table[3 << 16..4 << 16].to_vec();
    let mut table = table;
    disk_writer::seal_crc32c(&mut table, 4);
    for at in [3usize << 16, 4 << 16] {
        short_table[at..at + (1 << 16)].copy_from_slice(&table);
    }
    refused("empty block table", &short_table, "block table is empty");
}

pub fn large_sparse_disk() -> Vec<u8> {
    let size: u64 = 1 << 41;
    let lba: u64 = 2048;
    let sectors = ((size >> 9) - 4096) as u32;
    let clusters = (sectors - 32) / 64;
    let fat_sectors = (clusters + 2).div_ceil(128);
    let mut boot = build(&Volume::new(32, 140_000), &[]).bytes[..SECTOR].to_vec();
    boot[13] = 64;
    boot[32..36].copy_from_slice(&sectors.to_le_bytes());
    boot[36..40].copy_from_slice(&fat_sectors.to_le_bytes());
    let fat_at = 32 * SECTOR as u64;
    let data_at = fat_at + 2 * u64::from(fat_sectors) * SECTOR as u64;
    let mut fat = Vec::new();
    for value in [0x0FFF_FFF8u32, 0x0FFF_FFFF, 0x0FFF_FFFF, 0x0FFF_FFFF] {
        fat.extend(value.to_le_bytes());
    }
    let mut data = b"tiny\n".to_vec();
    data.resize(64 * SECTOR, 0);
    let block_log = 20;
    let mut blocks: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut put = |absolute: u64, bytes: &[u8]| {
        let block = blocks
            .entry(absolute >> block_log)
            .or_insert_with(|| vec![0u8; 1 << block_log]);
        let within = (absolute & ((1 << block_log) - 1)) as usize;
        block[within..within + bytes.len()].copy_from_slice(bytes);
    };
    put(0, &disk_writer::mbr_sector(&[(0x0C, lba as u32, sectors)]));
    let partition = lba << 9;
    put(partition, &boot);
    put(partition + fat_at, &fat);
    put(
        partition + data_at,
        &disk_writer::short_record(b"SMALL   TXT", 0x20, 3, 5),
    );
    put(partition + data_at + 64 * SECTOR as u64, &data);
    let blocks: Vec<(u64, Vec<u8>)> = blocks.into_iter().collect();
    disk_writer::vhdx_sparse(size, block_log, &blocks, 0).bytes
}

#[test]
fn a_large_sparse_disk_reads_only_its_folders() {
    let image = large_sparse_disk();
    assert!(image.len() < 64 << 20, "{}", image.len());
    let found = extracted_named(&image, "large.vhdx", ReadOptions::default()).unwrap();
    assert_eq!(
        found,
        BTreeMap::from([("SMALL.TXT".to_owned(), b"tiny\n".to_vec())])
    );
    if let Some(folder) = std::env::var_os("ZIPPER_WRITE_LARGE_DISK") {
        fs::write(Path::new(&folder).join("large.vhdx"), &image).unwrap();
    }
}

#[test]
fn selection_strip_and_listing_work_on_disks() {
    let built = sample_volume(16);
    let disk = disk_writer::mbr(&[(0x06, 64, &built.bytes)], 0);
    let paths = ["Folder".to_owned()];
    let found = extracted_named(
        &disk,
        "case.img",
        ReadOptions {
            paths: &paths,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        [
            "Folder",
            "Folder/EMPTYDIR",
            "Folder/inner.bin",
            "Folder/Ünïcode ✓.txt"
        ]
    );
    let found = extracted_named(
        &disk,
        "case.img",
        ReadOptions {
            strip: Strip::Prefix("Folder"),
            paths: &paths,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert!(found.contains_key("inner.bin"), "{:?}", found.keys());
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("case.img");
    fs::write(&archive, &disk).unwrap();
    let listed = read_archive(&archive, None, ReadOptions::default()).unwrap();
    assert_eq!(listed.iter().filter(|entry| !entry.directory).count(), 7);
    let two = disk_writer::mbr(
        &[(0x06, 64, &built.bytes), (0x06, 64 + 40_000, &built.bytes)],
        0,
    );
    let include = ["*.fat".to_owned()];
    let found = extracted_named(
        &two,
        "case.img",
        ReadOptions {
            include: &include,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(found.keys().collect::<Vec<_>>(), ["0.fat", "1.fat"]);
}

#[test]
fn flipped_bytes_in_every_structure_never_panic_or_leave_output() {
    let built = sample_volume(12);
    let partition = 64 * SECTOR;
    let disk = disk_writer::mbr(&[(0x01, 64, &built.bytes)], 0);
    let mut structures: Vec<usize> = (0..SECTOR).collect();
    structures.extend((partition..partition + built.data_at).step_by(3));
    structures.extend(
        built
            .records
            .values()
            .flat_map(|at| (partition + at - 64..partition + at + 32).step_by(5)),
    );
    let vhd = disk_writer::vhd_dynamic(&disk, 19);
    let vhdx = disk_writer::vhdx(&disk, 20);
    let cases: [(&str, &[u8], Vec<usize>); 3] = [
        ("img", &disk, structures),
        (
            "vhd",
            &vhd,
            (0..3 * SECTOR + 64)
                .chain((3 * SECTOR..vhd.len()).step_by(4099))
                .collect(),
        ),
        (
            "vhdx",
            &vhdx,
            (0..512)
                .chain((1 << 16..5 << 16).step_by(61))
                .chain(((2 << 20)..(4 << 20)).step_by(4093))
                .collect(),
        ),
    ];
    let root = tempfile::tempdir().unwrap();
    for (extension, image, places) in cases {
        let archive = root.path().join(format!("case.{extension}"));
        for (count, at) in places
            .into_iter()
            .filter(|at| *at < image.len())
            .enumerate()
        {
            let mut damaged = image.to_vec();
            damaged[at] ^= 0x41;
            fs::write(&archive, &damaged).unwrap();
            let listed = read_archive(&archive, None, ReadOptions::default());
            if count % 16 == 0 {
                let destination = root.path().join("out");
                let written = read_archive(&archive, Some(&destination), ReadOptions::default());
                assert_eq!(written.is_ok(), listed.is_ok(), "{extension} flip at {at}");
                if written.is_err() {
                    assert!(
                        !destination.exists(),
                        "{extension} flip at {at} left output"
                    );
                } else {
                    common_deletable(&destination);
                    fs::remove_dir_all(&destination).unwrap();
                }
            }
        }
    }
}

#[test]
#[ignore = "writes images at the documented limits into ZIPPER_DISK_LIMITS for a peak memory measurement"]
fn write_disks_at_the_documented_limits() {
    let into = std::env::var_os("ZIPPER_DISK_LIMITS").expect("ZIPPER_DISK_LIMITS names a folder");
    let into = Path::new(&into);
    fs::write(into.join("sparse-2tib.vhdx"), large_sparse_disk()).unwrap();
    let per_folder = 10_800;
    let folders: Vec<Item> = (0..92)
        .map(|index| {
            let files = (0..per_folder)
                .map(|entry| {
                    file(
                        &format!(
                            "{index:02}{entry:05} a file name that fills about sixty bytes.txt"
                        ),
                        b"",
                    )
                })
                .collect();
            folder(&format!("folder {index:02}"), files)
        })
        .collect();
    let mut volume = Volume::new(32, 600_000);
    volume.cluster_sectors = 8;
    volume.label = None;
    let built = build(&volume, &folders);
    fs::write(into.join("names.img"), &built.bytes).unwrap();
}
