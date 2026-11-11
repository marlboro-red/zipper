//! The disk reader extracts the SquashFS volume of a lone partition and each
//! partition of an MBR or GPT disk byte for byte, in every container, rejects
//! each hostile layout with nothing left behind, refuses FAT, NTFS, HFS+ and
//! APFS volumes by name in every container, sends what it does not read to
//! 7-Zip by default, and honours the options the other readers take. FAT
//! volumes from the test writer serve only as partition bytes and refusals.
#[path = "common/disk_writer.rs"]
mod disk_writer;
#[path = "common/squashfs_writer.rs"]
mod squashfs_writer;

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

fn fat_volume(bits: u32) -> Built {
    let sectors = match bits {
        12 => 4000,
        16 => 40_000,
        _ => 140_000,
    };
    build(&Volume::new(bits, sectors), &sample())
}

fn payload_entries() -> Vec<squashfs_writer::Entry> {
    use squashfs_writer::{ROOT, file, folder};
    let mut read_only = file(ROOT, "READONLY.TXT", b"keep\n");
    read_only.mode = 0o444;
    let mut entries = vec![
        file(ROOT, "HELLO.TXT", b"hello\n"),
        file(ROOT, "A long file name with spaces.txt", &pattern(3000, 1)),
        file(ROOT, "empty", b""),
        read_only,
        folder(ROOT, "Folder"),
        file(4, "inner.bin", &pattern(70_000, 2)),
        file(4, "\u{dc}n\u{ef}code \u{2713}.txt", b"unicode\n"),
        folder(4, "EMPTYDIR"),
    ];
    let mut parent = ROOT;
    for level in 0..12 {
        entries.push(folder(parent, &format!("level {level}")));
        parent = entries.len() - 1;
    }
    entries.push(file(parent, "bottom.txt", b"at the bottom\n"));
    entries
}

fn squashfs_volume(entries: &[squashfs_writer::Entry]) -> Vec<u8> {
    let mut bytes =
        squashfs_writer::build(entries, squashfs_writer::Options { compress: true }).bytes;
    bytes.resize(bytes.len().next_multiple_of(4096), 0);
    bytes
}

fn payload() -> Vec<u8> {
    squashfs_volume(&payload_entries())
}

fn sample_tree() -> BTreeMap<String, Vec<u8>> {
    let entries = payload_entries();
    let path = |mut index: usize| {
        let mut parts = Vec::new();
        while index != squashfs_writer::ROOT {
            parts.push(String::from_utf8(entries[index].name.clone()).unwrap());
            index = entries[index].parent;
        }
        parts.reverse();
        parts.join("/")
    };
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let data = match &entry.kind {
                squashfs_writer::Kind::File(data) => data.clone(),
                _ => b"<dir>".to_vec(),
            };
            (path(index), data)
        })
        .collect()
}

#[test]
fn a_lone_squashfs_volume_extracts_byte_for_byte_in_every_container() {
    let wanted = sample_tree();
    let volume = payload();
    assert_eq!(extracted(&volume), wanted, "raw");
    let mbr = disk_writer::mbr(&[(0x83, 2048, &volume)], 64);
    assert_eq!(extracted(&mbr), wanted, "MBR");
    let gpt = disk_writer::gpt(&[(disk_writer::LINUX_DATA, "data", 2048, &volume)], 64);
    assert_eq!(extracted(&gpt), wanted, "GPT");
    assert_eq!(
        extracted_named(
            &disk_writer::vhd_fixed(&mbr),
            "case.vhd",
            ReadOptions::default()
        )
        .unwrap(),
        wanted,
        "fixed VHD"
    );
    assert_eq!(
        extracted(&disk_writer::vhd_dynamic(&mbr, 19)),
        wanted,
        "dynamic VHD"
    );
    for block_log in [20, 25] {
        assert_eq!(
            extracted(&disk_writer::vhdx(&gpt, block_log)),
            wanted,
            "VHDX {block_log}"
        );
    }
}

fn apfs_volume() -> Vec<u8> {
    let mut volume = vec![0u8; 64 * SECTOR];
    volume[24..28].copy_from_slice(&0x8000_0001u32.to_le_bytes());
    volume[32..36].copy_from_slice(b"NXSB");
    volume[36..40].copy_from_slice(&4096u32.to_le_bytes());
    volume
}

fn hfs_volume() -> Vec<u8> {
    let mut volume = vec![0u8; 64 * SECTOR];
    volume[1024..1028].copy_from_slice(b"H+\0\x04");
    volume
}

fn refused_volumes() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    vec![
        (
            "FAT12",
            fat_volume(12).bytes,
            "Zipper does not read FAT volumes",
        ),
        (
            "FAT32",
            fat_volume(32).bytes,
            "Zipper does not read FAT volumes",
        ),
        ("NTFS", ntfs_boot(), "Zipper does not read NTFS volumes"),
        (
            "HFS+",
            hfs_volume(),
            "Zipper does not read HFS+ and HFSX volumes",
        ),
        (
            "APFS",
            apfs_volume(),
            "Zipper does not read APFS containers",
        ),
    ]
}

#[test]
fn a_lone_fat_ntfs_hfs_or_apfs_volume_is_refused_by_name_in_every_container() {
    for (label, volume, words) in refused_volumes() {
        let mbr = disk_writer::mbr(&[(0x83, 64, &volume)], 0);
        let gpt = disk_writer::gpt(&[(disk_writer::LINUX_DATA, "data", 40, &volume)], 0);
        let qcow = disk_writer::qcow_sparse(
            qcow_options(3, false),
            mbr.len() as u64,
            &pieces(&mbr, 4096),
        );
        let images: Vec<(&str, Vec<u8>, &str)> = vec![
            ("MBR", mbr.clone(), "case.img"),
            ("GPT", gpt.clone(), "case.img"),
            ("fixed VHD", disk_writer::vhd_fixed(&mbr), "case.vhd"),
            (
                "dynamic VHD",
                disk_writer::vhd_dynamic(&gpt, 19),
                "case.vhd",
            ),
            ("VHDX", disk_writer::vhdx(&mbr, 20), "case.vhdx"),
            ("QCOW", qcow.bytes, "case.qcow2"),
        ];
        for (container, image, name) in images {
            let case = format!("{label} in {container}");
            refused_named(&case, &image, name, words);
            for engine in [Engine::Auto, Engine::Native] {
                assert_eq!(reader(&image, name, engine), Reader::Disk, "{case}");
            }
        }
        let raw = reader_error(&volume, "volume.img", Engine::Native);
        assert!(raw.contains(words), "raw {label}: {raw}");
    }
}

fn reader_error(bytes: &[u8], name: &str, engine: Engine) -> String {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join(name);
    fs::write(&archive, bytes).unwrap();
    let destination = root.path().join("out");
    let error = zipper::extract_archive(
        &archive,
        &destination,
        zipper::ExtractOptions {
            engine,
            ..zipper::ExtractOptions::default()
        },
    )
    .map(|_| panic!("{name} extracted"))
    .unwrap_err();
    assert!(!destination.exists(), "{name} left output behind");
    format!("{error:#}")
}

#[test]
fn several_partitions_extract_as_one_file_each_with_the_gaps_between() {
    let first = fat_volume(16);
    let second = fat_volume(12);
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
    let first = fat_volume(12);
    let logical = fat_volume(12);
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
fn file_systems_zipper_does_not_read_go_to_7zip_and_refused_ones_stay_in_process() {
    let ntfs = disk_writer::mbr(&[(0x07, 64, &ntfs_boot())], 0);
    refused(
        "lone NTFS partition",
        &ntfs,
        "Zipper does not read NTFS volumes",
    );
    assert_eq!(reader(&ntfs, "disk.img", Engine::Auto), Reader::Disk);
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
    refused(
        "lone NTFS GPT partition",
        &gpt,
        "Zipper does not read NTFS volumes",
    );
    assert_eq!(reader(&gpt, "disk.img", Engine::Auto), Reader::Disk);
    let vhdx = disk_writer::vhdx(&ntfs, 20);
    assert_eq!(reader(&vhdx, "disk.vhdx", Engine::Auto), Reader::Disk);
    let fat = fat_volume(12);
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
    let volume = payload();
    let mut table = disk_writer::gpt(&[(disk_writer::BASIC_DATA, "data", 40, &volume)], 0);
    assert_eq!(extracted(&table), sample_tree());
    table[2 * SECTOR + 60] ^= 1;
    refused(
        "GPT table that fails its checksum",
        &table,
        "no partition table or file system",
    );
    for header_size in [0u32, 4, 16, 19] {
        let mut tiny = disk_writer::gpt(&[(disk_writer::BASIC_DATA, "data", 40, &volume)], 0);
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
            sample_tree(),
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
    refused(
        "raw FAT volume",
        &fat.bytes,
        "Zipper does not read FAT volumes",
    );
}

#[test]
fn vhd_images_are_checked_before_their_disk_is_read() {
    let built = fat_volume(16);
    let disk = disk_writer::mbr(&[(0x83, 64, &payload())], 0);
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
    let disk = container_disk();
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
    disk_writer::vhdx_sparse(size, 20, &large_sparse_blocks(size), 0).bytes
}

fn large_sparse_blocks(size: u64) -> Vec<(u64, Vec<u8>)> {
    let lba: u64 = 2048;
    let sectors = ((size >> 9) - 4096) as u32;
    let volume = squashfs_volume(&[squashfs_writer::file(
        squashfs_writer::ROOT,
        "SMALL.TXT",
        b"tiny\n",
    )]);
    let block_log = 20;
    let mut blocks: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
    let mut put = |absolute: u64, bytes: &[u8]| {
        for (offset, piece) in bytes.chunks(1 << block_log).enumerate() {
            let at = absolute + ((offset as u64) << block_log);
            let block = blocks
                .entry(at >> block_log)
                .or_insert_with(|| vec![0u8; 1 << block_log]);
            let within = (at & ((1 << block_log) - 1)) as usize;
            block[within..within + piece.len()].copy_from_slice(piece);
        }
    };
    put(0, &disk_writer::mbr_sector(&[(0x83, lba as u32, sectors)]));
    put(lba << 9, &volume);
    blocks.into_iter().collect()
}

fn split_blocks(blocks: &[(u64, Vec<u8>)], piece_log: u32) -> Vec<(u64, Vec<u8>)> {
    let per_block = 1u64 << (20 - piece_log);
    blocks
        .iter()
        .flat_map(|(index, block)| {
            block
                .chunks(1 << piece_log)
                .enumerate()
                .filter(|(_, piece)| piece.iter().any(|byte| *byte != 0))
                .map(move |(place, piece)| (index * per_block + place as u64, piece.to_vec()))
        })
        .collect()
}

fn large_android_sparse(size: u64, blocks: &[(u64, Vec<u8>)], fills: u32) -> Vec<u8> {
    let block = 4096u32;
    let mut chunks: Vec<(u16, u32, Vec<u8>)> = Vec::new();
    let mut next = 0u64;
    let skip = |chunks: &mut Vec<(u16, u32, Vec<u8>)>, count: u64| {
        if count != 0 {
            chunks.push((disk_writer::SPARSE_SKIP, count as u32, Vec::new()));
        }
    };
    for (index, piece) in split_blocks(blocks, 12) {
        skip(&mut chunks, index - next);
        chunks.push((disk_writer::SPARSE_RAW, 1, piece));
        next = index + 1;
    }
    for _ in 0..fills {
        chunks.push((disk_writer::SPARSE_FILL, 1, vec![0; 4]));
        next += 1;
    }
    skip(&mut chunks, size / u64::from(block) - next);
    disk_writer::android_sparse_chunks(block, (size / u64::from(block)) as u32, &chunks).bytes
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
    let built = fat_volume(16);
    let disk = disk_writer::mbr(&[(0x83, 64, &payload())], 0);
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
    let volume = payload();
    let partition = 64 * SECTOR;
    let disk = disk_writer::mbr(&[(0x83, 64, &volume)], 0);
    let mut structures: Vec<usize> = (0..SECTOR).collect();
    structures.extend((partition..partition + volume.len()).step_by(7));
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
    let qcow_size = 1u64 << 37;
    let qcow = disk_writer::qcow_sparse(
        disk_writer::Qcow {
            version: 3,
            cluster_bits: 9,
            compressed: false,
            zero_flags: false,
        },
        qcow_size,
        &split_blocks(&large_sparse_blocks(qcow_size), 9),
    );
    assert_eq!(
        u32::from_be_bytes(qcow.bytes[36..40].try_into().unwrap()),
        1 << 22
    );
    fs::write(into.join("l1-limit-128gib.qcow2"), &qcow.bytes).unwrap();
    let size = 1u64 << 41;
    let blocks = large_sparse_blocks(size);
    fs::write(
        into.join("sparse-2tib.vdi"),
        disk_writer::vdi_sparse(
            disk_writer::Vdi {
                block_log: 20,
                fixed: false,
                zero_entries: false,
            },
            size,
            &blocks,
        )
        .bytes,
    )
    .unwrap();
    let vmdk_size = 1u64 << 39;
    fs::write(
        into.join("sparse-512gib.vmdk"),
        disk_writer::vmdk_sparse(
            disk_writer::Vmdk {
                grain_sectors: 128,
                table_entries: 512,
                stream: false,
            },
            vmdk_size,
            &split_blocks(&large_sparse_blocks(vmdk_size), 16),
            "sparse-512gib.vmdk",
        )
        .bytes,
    )
    .unwrap();
    fs::write(
        into.join("chunks-4m-2tib.simg"),
        large_android_sparse(size, &blocks, 1 << 22),
    )
    .unwrap();
}

fn refused_files(case: &str, files: &[(&str, &[u8])], words: &str) {
    let root = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        fs::write(root.path().join(name), bytes).unwrap();
    }
    let archive = root.path().join(files[0].0);
    let destination = root.path().join("out");
    let error = match read_archive(&archive, Some(&destination), ReadOptions::default()) {
        Ok(_) => panic!("{case}: accepted a hostile image"),
        Err(error) => format!("{error:#}"),
    };
    assert!(!destination.exists(), "{case}: left output behind");
    assert!(
        read_archive(&archive, None, ReadOptions::default()).is_err(),
        "{case}: verification accepted a hostile image"
    );
    assert!(
        error.contains(words),
        "{case}: {error:?} does not say {words:?}"
    );
}

fn extracted_files(files: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    let root = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        fs::write(root.path().join(name), bytes).unwrap();
    }
    let destination = root.path().join("out");
    read_archive(
        &root.path().join(files[0].0),
        Some(&destination),
        ReadOptions::default(),
    )
    .unwrap();
    let found = tree(&destination);
    common_deletable(root.path());
    found
}

fn sent_to_7zip(case: &str, bytes: &[u8], name: &str, words: &str) {
    refused_named(case, bytes, name, words);
    assert_eq!(
        reader(bytes, name, Engine::Auto),
        Reader::Backend { format: None },
        "{case}"
    );
    assert_eq!(reader(bytes, name, Engine::Native), Reader::Disk, "{case}");
}

fn put_be64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_be_bytes());
}

fn be64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn put_le32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn le32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn pieces(disk: &[u8], size: usize) -> Vec<(u64, Vec<u8>)> {
    disk.chunks(size)
        .enumerate()
        .filter(|(_, piece)| piece.iter().any(|byte| *byte != 0))
        .map(|(index, piece)| {
            let mut piece = piece.to_vec();
            piece.resize(size, 0);
            (index as u64, piece)
        })
        .collect()
}

fn container_disk() -> Vec<u8> {
    disk_writer::mbr(&[(0x83, 2048, &payload())], 0)
}

fn qcow_options(version: u32, compressed: bool) -> disk_writer::Qcow {
    disk_writer::Qcow {
        version,
        cluster_bits: 12,
        compressed,
        zero_flags: false,
    }
}

#[test]
fn writer_volumes_extract_byte_for_byte_in_the_newer_containers() {
    let wanted = sample_tree();
    let disk = container_disk();
    let gpt = disk_writer::gpt(&[(disk_writer::LINUX_DATA, "data", 2048, &payload())], 64);
    for (version, cluster_bits, compressed) in [
        (1, 9, false),
        (1, 16, false),
        (2, 9, true),
        (2, 16, false),
        (3, 12, true),
        (3, 21, false),
    ] {
        let options = disk_writer::Qcow {
            version,
            cluster_bits,
            compressed,
            zero_flags: version == 3,
        };
        for (label, source) in [("MBR", &disk), ("GPT", &gpt)] {
            let mut clusters = pieces(source, 1 << cluster_bits);
            if options.zero_flags {
                clusters.push((clusters.last().unwrap().0 + 1, vec![0; 1 << cluster_bits]));
            }
            let image = disk_writer::qcow_sparse(options, source.len() as u64, &clusters);
            assert_eq!(
                extracted(&image.bytes),
                wanted,
                "{label} QCOW {version} with {cluster_bits}-bit clusters"
            );
        }
    }
    for (fixed, zero_entries) in [(false, false), (false, true), (true, false)] {
        let options = disk_writer::Vdi {
            block_log: 20,
            fixed,
            zero_entries,
        };
        assert_eq!(
            extracted(&disk_writer::vdi(&gpt, options)),
            wanted,
            "VDI {options:?}"
        );
    }
    for (grain_sectors, stream) in [(1, false), (16, true), (128, false), (128, true)] {
        let options = disk_writer::Vmdk {
            grain_sectors,
            table_entries: 512,
            stream,
        };
        assert_eq!(
            extracted(&disk_writer::vmdk(&disk, options, "case.img")),
            wanted,
            "VMDK {options:?}"
        );
    }
    for block in [512, 4096, 65_536] {
        assert_eq!(
            extracted(&disk_writer::android_sparse(&gpt, block)),
            wanted,
            "Android sparse with {block}-byte blocks"
        );
    }
    let (descriptor, flat) = disk_writer::vmdk_flat(&disk, "case-flat.vmdk");
    assert_eq!(
        extracted_files(&[("case.vmdk", &descriptor), ("case-flat.vmdk", &flat)]),
        wanted,
        "flat VMDK"
    );
}

#[test]
fn qcow_images_are_checked_before_their_disk_is_read() {
    let disk = container_disk();
    let clusters = pieces(&disk, 4096);
    let image = disk_writer::qcow_sparse(qcow_options(3, false), disk.len() as u64, &clusters);
    assert_eq!(extracted(&image.bytes), sample_tree());
    let first_l2 = image.l2_at[0];
    let mut backing = image.bytes.clone();
    put_be64(&mut backing, 8, 4096);
    sent_to_7zip("backing file", &backing, "case.qcow2", "backing file");
    let mut encrypted = image.bytes.clone();
    encrypted[35] = 2;
    refused("encrypted", &encrypted, "encrypted");
    for (bit, words) in [
        (1u64 << 1, "marked corrupt"),
        (1 << 2, "external file"),
        (1 << 4, "extended second-level"),
        (1 << 9, "features Zipper does not know"),
    ] {
        let mut flagged = image.bytes.clone();
        put_be64(&mut flagged, 72, bit);
        refused(words, &flagged, words);
    }
    let mut past_end = image.bytes.clone();
    put_be64(
        &mut past_end,
        first_l2,
        (image.bytes.len() as u64) | 1 << 63,
    );
    refused(
        "cluster past the end",
        &past_end,
        "past the end of the image",
    );
    let mut shared = image.bytes.clone();
    let first_entry = be64_at(&shared, first_l2);
    put_be64(&mut shared, first_l2 + 8, first_entry);
    refused("two clusters at one place", &shared, "share a cluster");
    let mut table_as_data = image.bytes.clone();
    put_be64(&mut table_as_data, first_l2, first_l2 as u64 | 1 << 63);
    refused(
        "cluster on its own table",
        &table_as_data,
        "share a cluster",
    );
    let mut header_as_data = image.bytes.clone();
    put_be64(&mut header_as_data, first_l2, (1 << 63) | (4096 * 3));
    refused(
        "cluster on the first-level table",
        &header_as_data,
        "share a cluster",
    );
    let mut unaligned = image.bytes.clone();
    put_be64(
        &mut unaligned,
        first_l2,
        ((first_entry & !(1 << 63)) + 512) | (1 << 63),
    );
    refused(
        "cluster not aligned",
        &unaligned,
        "not aligned to a cluster",
    );
    let mut reserved = image.bytes.clone();
    put_be64(&mut reserved, first_l2, first_entry | 1 << 3);
    refused("second-level reserved bits", &reserved, "reserved bits");
    let mut l1_reserved = image.bytes.clone();
    let l1 = be64_at(&l1_reserved, image.l1_at);
    put_be64(&mut l1_reserved, image.l1_at, l1 | 1 << 60);
    refused("first-level reserved bits", &l1_reserved, "reserved bits");
    let mut short_l1 = image.bytes.clone();
    short_l1[36..40].copy_from_slice(&0u32.to_be_bytes());
    refused(
        "first-level table too short",
        &short_l1,
        "shorter than its disk needs",
    );
    let mut long_l1 = image.bytes.clone();
    long_l1[36..40].copy_from_slice(&(1u32 << 20).to_be_bytes());
    refused(
        "first-level table past the end",
        &long_l1,
        "past the end of the image",
    );
    let mut refcounts = image.bytes.clone();
    refcounts[56..60].copy_from_slice(&(1u32 << 20).to_be_bytes());
    refused(
        "reference counts past the end",
        &refcounts,
        "reference count table",
    );
    let mut cluster_size = image.bytes.clone();
    cluster_size[20..24].copy_from_slice(&22u32.to_be_bytes());
    refused("cluster of 4 MiB", &cluster_size, "cluster size");
    let truncated = &image.bytes[..image.bytes.len() - 4096];
    refused(
        "truncated in its last cluster",
        truncated,
        "past the end of the image",
    );

    let packed = disk_writer::qcow_sparse(qcow_options(3, true), disk.len() as u64, &clusters);
    assert_eq!(extracted(&packed.bytes), sample_tree());
    let mut zstd = packed.bytes.clone();
    put_be64(&mut zstd, 72, 1 << 3);
    zstd[104] = 1;
    refused("zstd clusters", &zstd, "zstd");
    let mut damaged = packed.bytes.clone();
    for at in &packed.data_at {
        damaged[*at] ^= 0xFF;
        damaged[*at + 1] ^= 0xFF;
    }
    refused(
        "damaged compressed clusters",
        &damaged,
        "compressed QCOW cluster",
    );
    let mut onto_table = packed.bytes.clone();
    let first_packed = be64_at(&onto_table, packed.l2_at[0]);
    let offset_bits = (1u64 << (62 - 4)) - 1;
    put_be64(
        &mut onto_table,
        packed.l2_at[0],
        (first_packed & !offset_bits) | packed.l2_at[0] as u64,
    );
    refused(
        "compressed cluster on a table",
        &onto_table,
        "share a cluster",
    );
    let mut too_long = packed.bytes.clone();
    let table = *packed.l2_at.last().unwrap();
    let last = (0..512)
        .map(|slot| table + 8 * slot)
        .max_by_key(|at| be64_at(&too_long, *at) & offset_bits)
        .unwrap();
    let value = be64_at(&too_long, last);
    put_be64(&mut too_long, last, value | 0xF << 58);
    refused(
        "compressed cluster past the end",
        &too_long,
        "past the end of the image",
    );
    let old = disk_writer::qcow_sparse(qcow_options(1, true), disk.len() as u64, &clusters);
    refused("version 1 compressed clusters", &old.bytes, "version 1");
}

#[test]
fn vdi_images_are_checked_before_their_disk_is_read() {
    let disk = container_disk();
    let options = disk_writer::Vdi {
        block_log: 20,
        fixed: false,
        zero_entries: false,
    };
    let image = disk_writer::vdi_sparse(options, disk.len() as u64, &pieces(&disk, 1 << 20));
    assert_eq!(extracted(&image.bytes), sample_tree());
    let mut differencing = image.bytes.clone();
    put_le32(&mut differencing, 0x4C, 4);
    sent_to_7zip(
        "differencing VDI",
        &differencing,
        "case.vdi",
        "differencing VDI",
    );
    let mut parent = image.bytes.clone();
    parent[0x1A8] = 1;
    sent_to_7zip("VDI with a parent", &parent, "case.vdi", "differencing VDI");
    let mut blocks = image.bytes.clone();
    put_le32(&mut blocks, 0x178, 1 << 16);
    refused("64 KiB blocks", &blocks, "not 1 MiB");
    let mut extra = image.bytes.clone();
    put_le32(&mut extra, 0x17C, 512);
    refused("block extra data", &extra, "extra data");
    let mut sectors = image.bytes.clone();
    put_le32(&mut sectors, 0x168, 4096);
    refused("4 KiB sectors", &sectors, "not 512 bytes");
    let mut shared = image.bytes.clone();
    let first = le32_at(&shared, image.map_at);
    put_le32(&mut shared, image.map_at + 4, first);
    refused("two blocks at one place", &shared, "share their bytes");
    let mut past = image.bytes.clone();
    put_le32(&mut past, image.map_at, 7);
    refused(
        "block past the allocated ones",
        &past,
        "past the allocated ones",
    );
    let truncated = &image.bytes[..image.bytes.len() - 4096];
    refused(
        "truncated in its last block",
        truncated,
        "allocated VDI blocks",
    );
    let mut map = image.bytes.clone();
    put_le32(&mut map, 0x158, 0x200);
    refused("map overlapping the data", &map, "overlaps");
    let mut header = image.bytes.clone();
    put_le32(&mut header, 0x48, 0x400);
    refused("header overlapping the map", &header, "overlaps");
}

#[test]
fn vmdk_images_are_checked_before_their_disk_is_read() {
    let disk = container_disk();
    let options = disk_writer::Vmdk {
        grain_sectors: 16,
        table_entries: 512,
        stream: false,
    };
    let image = disk_writer::vmdk_sparse(options, disk.len() as u64, &pieces(&disk, 8192), "a");
    assert_eq!(extracted(&image.bytes), sample_tree());
    let table = image.gt_at[0];
    let first = (0..512)
        .map(|slot| table + 4 * slot)
        .find(|at| le32_at(&image.bytes, *at) != 0)
        .unwrap();
    let mut past = image.bytes.clone();
    put_le32(&mut past, first, (image.bytes.len() / SECTOR) as u32);
    refused("grain past the end", &past, "past the end of the image");
    let mut shared = image.bytes.clone();
    let value = le32_at(&shared, first);
    put_le32(&mut shared, first + 4, value);
    refused("two grains at one place", &shared, "share their bytes");
    let mut metadata = image.bytes.clone();
    put_le32(&mut metadata, first, (image.gt_at[0] / SECTOR) as u32);
    refused("grain on its table", &metadata, "metadata");
    let mut tables = image.bytes.clone();
    put_le32(&mut tables, 0x2C, 256);
    refused("grain tables of 256 entries", &tables, "512 entries");
    let mut descriptor = image.bytes.clone();
    descriptor[SECTOR..SECTOR + 5].copy_from_slice(b"junk\n");
    refused(
        "descriptor junk",
        &descriptor,
        "not a comment, setting or extent",
    );
    let mut parent = image.bytes.clone();
    let text = String::from_utf8_lossy(&parent[SECTOR..2 * SECTOR]).into_owned();
    let at = SECTOR + text.find("parentCID=ffffffff").unwrap() + "parentCID=".len();
    parent[at..at + 8].copy_from_slice(b"12345678");
    sent_to_7zip(
        "VMDK with a parent",
        &parent,
        "case.vmdk",
        "differencing VMDK",
    );
    let mut newline = image.bytes.clone();
    newline[0x4A] = b'x';
    refused("newline test", &newline, "newline test");

    let stream = disk_writer::vmdk_sparse(
        disk_writer::Vmdk {
            stream: true,
            ..options
        },
        disk.len() as u64,
        &pieces(&disk, 8192),
        "a",
    );
    assert_eq!(extracted(&stream.bytes), sample_tree());
    let mut no_marker = stream.bytes.clone();
    no_marker[stream.gd_at - SECTOR + 12] = 9;
    refused(
        "grain directory without its marker",
        &no_marker,
        "lacks its marker",
    );
    let mut footer = stream.bytes.clone();
    let footer_at = footer.len() - 2 * SECTOR;
    footer[footer_at + 0x40] ^= 1;
    refused("footer that differs", &footer, "does not repeat");
    let mut place = stream.bytes.clone();
    place[stream.grain_at[1]] ^= 1;
    refused("grain naming another place", &place, "names another place");
    let mut size = stream.bytes.clone();
    let grain = stream.grain_at[0];
    let packed = le32_at(&size, grain + 8);
    put_le32(&mut size, grain + 8, packed - 1);
    refused(
        "grain size past its stream",
        &size,
        "does not end where its size says",
    );
    let mut eos = stream.bytes.clone();
    let last = eos.len() - SECTOR;
    eos[last] = 1;
    refused("stream without its end marker", &eos, "end marker");
}

#[test]
fn vmdk_descriptors_open_only_plain_extents_beside_them() {
    let disk = container_disk();
    let (descriptor, flat) = disk_writer::vmdk_flat(&disk, "case-flat.vmdk");
    let text = String::from_utf8(descriptor.clone()).unwrap();
    let sectors = flat.len() / SECTOR;
    for (case, name) in [
        ("subfolder", "sub/case-flat.vmdk"),
        ("parent folder", "../case-flat.vmdk"),
        ("absolute", "/etc/hosts"),
        ("backslash", "sub\\case-flat.vmdk"),
        ("dot dot", ".."),
    ] {
        let edited = text.replace("\"case-flat.vmdk\"", &format!("\"{name}\""));
        refused_files(
            case,
            &[("case.vmdk", edited.as_bytes()), ("case-flat.vmdk", &flat)],
            "plain file name",
        );
    }
    refused_files(
        "missing extent",
        &[("case.vmdk", &descriptor)],
        "cannot open the VMDK extent",
    );
    let long = text.replace(
        &format!("RW {sectors} FLAT"),
        &format!("RW {} FLAT", sectors + 8),
    );
    refused_files(
        "flat extent longer than its file",
        &[("case.vmdk", long.as_bytes()), ("case-flat.vmdk", &flat)],
        "shorter than the descriptor says",
    );
    let zero = text.replace("FLAT \"case-flat.vmdk\" 0", "ZERO \"case-flat.vmdk\"");
    refused_files(
        "zero extent",
        &[("case.vmdk", zero.as_bytes()), ("case-flat.vmdk", &flat)],
        "ZERO extent",
    );
    let junk = text.clone() + "junk\n";
    refused_files(
        "junk line",
        &[("case.vmdk", junk.as_bytes()), ("case-flat.vmdk", &flat)],
        "not a comment, setting or extent",
    );
    let unsigned = text.replacen("# Disk DescriptorFile", "# Disk Descriptor", 1);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("case.vmdk"), unsigned.as_bytes()).unwrap();
    assert!(!zipper::disk::sniff(&root.path().join("case.vmdk")).unwrap());
    let sparse_raw = text.replace("FLAT \"case-flat.vmdk\" 0", "SPARSE \"case-flat.vmdk\"");
    refused_files(
        "sparse extent that is raw",
        &[
            ("case.vmdk", sparse_raw.as_bytes()),
            ("case-flat.vmdk", &flat),
        ],
        "is not a VMDK sparse extent",
    );
    let parent = text.replace("parentCID=ffffffff", "parentCID=00000001");
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("case.vmdk"), parent.as_bytes()).unwrap();
    fs::write(root.path().join("case-flat.vmdk"), &flat).unwrap();
    assert_eq!(
        zipper::choose_reader(Engine::Auto, &root.path().join("case.vmdk")).unwrap(),
        Reader::Backend { format: None }
    );
    let error =
        read_archive(&root.path().join("case.vmdk"), None, ReadOptions::default()).unwrap_err();
    assert!(
        format!("{error:#}").contains("differencing VMDK"),
        "{error:#}"
    );
}

#[test]
fn android_sparse_images_are_checked_before_their_disk_is_read() {
    let disk = container_disk();
    let image = disk_writer::android_sparse(&disk, 4096);
    assert_eq!(extracted(&image), sample_tree());
    let second = 28 + 12 + 4096;
    for (case, at, value, words) in [
        ("unknown chunk type", second, 0xCAC5u32, "unknown type"),
        ("reserved field", second, 0x0001_CAC1, "reserved"),
        ("chunk size", second + 8, 99, "does not match its type"),
        ("block size", 12, 4097, "multiple of 4"),
        ("more blocks", 16, 1, "cover fewer blocks"),
    ] {
        let mut damaged = image.clone();
        let old = le32_at(&damaged, at);
        let new = if case == "more blocks" {
            old + value
        } else {
            value
        };
        if case == "reserved field" {
            damaged[at + 2] = 1;
        } else {
            put_le32(&mut damaged, at, new);
        }
        refused(case, &damaged, words);
    }
    let mut header = image.clone();
    header[8] = 32;
    refused("header size", &header, "header sizes");
    let mut fewer = image.clone();
    let blocks = le32_at(&fewer, 16);
    put_le32(&mut fewer, 16, blocks - 1);
    refused("fewer blocks", &fewer, "more blocks than the image holds");
    refused(
        "cut in a raw chunk",
        &image[..second - 100],
        "past the end of the image",
    );
    let chunks = disk_writer::android_sparse_chunks(
        4096,
        3,
        &[
            (
                disk_writer::SPARSE_FILL,
                1,
                0x5A5A_5A5Au32.to_le_bytes().to_vec(),
            ),
            (disk_writer::SPARSE_CRC, 1, vec![0; 4]),
            (disk_writer::SPARSE_SKIP, 2, Vec::new()),
        ],
    );
    refused(
        "checksum chunk covering blocks",
        &chunks.bytes,
        "checksum chunk covers blocks",
    );
}

#[test]
fn truncated_containers_are_refused_or_extract_the_whole_tree() {
    let disk = container_disk();
    let whole = sample_tree();
    let images: Vec<(&str, Vec<u8>)> = vec![
        ("qcow2", disk_writer::qcow(&disk, qcow_options(3, true))),
        ("qcow", disk_writer::qcow(&disk, qcow_options(1, false))),
        (
            "vdi",
            disk_writer::vdi(
                &disk,
                disk_writer::Vdi {
                    block_log: 20,
                    fixed: false,
                    zero_entries: false,
                },
            ),
        ),
        (
            "vmdk",
            disk_writer::vmdk(
                &disk,
                disk_writer::Vmdk {
                    grain_sectors: 16,
                    table_entries: 512,
                    stream: true,
                },
                "case.vmdk",
            ),
        ),
        ("simg", disk_writer::android_sparse(&disk, 4096)),
    ];
    for (extension, image) in images {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("case.{extension}"));
        let cuts: Vec<usize> = (0..image.len().min(1 << 16))
            .step_by(509)
            .chain((0..image.len()).step_by(image.len() / 61 + 1))
            .chain([image.len() - 1])
            .collect();
        for cut in cuts {
            fs::write(&archive, &image[..cut]).unwrap();
            let destination = root.path().join("out");
            match read_archive(&archive, Some(&destination), ReadOptions::default()) {
                Ok(_) => assert_eq!(tree(&destination), whole, "{extension} cut at {cut}"),
                Err(_) => assert!(
                    !destination.exists(),
                    "{extension} cut at {cut} left output"
                ),
            }
            common_deletable(root.path());
            let _ = fs::remove_dir_all(&destination);
        }
    }
}

#[test]
fn flipped_container_bytes_never_panic_or_leave_output() {
    let disk = disk_writer::mbr(&[(0x83, 64, &payload())], 0);
    let qcow = disk_writer::qcow_sparse(
        qcow_options(2, true),
        disk.len() as u64,
        &pieces(&disk, 4096),
    );
    let vdi = disk_writer::vdi_sparse(
        disk_writer::Vdi {
            block_log: 20,
            fixed: false,
            zero_entries: true,
        },
        disk.len() as u64,
        &pieces(&disk, 1 << 20),
    );
    let vmdk = disk_writer::vmdk_sparse(
        disk_writer::Vmdk {
            grain_sectors: 16,
            table_entries: 512,
            stream: true,
        },
        disk.len() as u64,
        &pieces(&disk, 8192),
        "case.vmdk",
    );
    let sparse = disk_writer::android_sparse(&disk, 4096);
    let mut qcow_places: Vec<usize> = (0..112).chain(qcow.l1_at..qcow.l1_at + 16).collect();
    for table in &qcow.l2_at {
        qcow_places.extend((*table..table + 4096).step_by(29));
    }
    let mut vmdk_places: Vec<usize> = (0..SECTOR * 2).step_by(3).collect();
    vmdk_places.extend((vmdk.gd_at - SECTOR..vmdk.gd_at + 8).step_by(3));
    for table in &vmdk.gt_at {
        vmdk_places.extend((table - SECTOR..table + 2048).step_by(13));
    }
    vmdk_places.extend(vmdk.grain_at.iter().flat_map(|at| *at..at + 12));
    let end = vmdk.bytes.len();
    vmdk_places.extend(end - 3 * SECTOR..end - SECTOR + 0x50);
    let cases: [(&str, &[u8], Vec<usize>); 4] = [
        ("qcow2", &qcow.bytes, qcow_places),
        (
            "vdi",
            &vdi.bytes,
            (0x40..0x1C8).chain(vdi.map_at..vdi.data_at).collect(),
        ),
        ("vmdk", &vmdk.bytes, vmdk_places),
        (
            "simg",
            &sparse,
            (0..200).chain((0..sparse.len()).step_by(4099)).collect(),
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

#[cfg(unix)]
#[expect(
    clippy::zombie_processes,
    reason = "wait4 reaps the child and reads its peak memory"
)]
fn peak_bytes_of_zipper_test(image: &Path) -> u64 {
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_zipper"))
        .arg("test")
        .arg(image)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut status = 0;
    // SAFETY: an all-zero `rusage` is a valid value of this plain C struct.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `child.id()` names a child of this process that nothing else waits for,
    // and both pointers name live locals of the types `wait4` writes.
    let waited = unsafe { libc::wait4(child.id() as libc::pid_t, &mut status, 0, &mut usage) };
    assert_eq!(waited, child.id() as libc::pid_t);
    let peak = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    }
}

#[cfg(unix)]
#[test]
fn extents_of_one_stream_file_hold_one_decoded_grain_at_a_time() {
    let base = disk_writer::mbr(&[(0x01, 64, &fat_volume(12).bytes)], 0);
    let copy_sectors = (base.len() / SECTOR) as u32;
    let copies = 8u32;
    let mut disk = base.clone();
    disk[..SECTOR].copy_from_slice(&disk_writer::mbr_sector(&[
        (0x83, 64, 4 * copy_sectors - 64),
        (0x83, 4 * copy_sectors, 4 * copy_sectors),
    ]));
    let stream = disk_writer::vmdk(
        &disk,
        disk_writer::Vmdk {
            grain_sectors: 65_536,
            table_entries: 512,
            stream: true,
        },
        "one.vmdk",
    );
    let extents: Vec<String> = (0..copies)
        .map(|_| format!("RW {copy_sectors} SPARSE \"one.vmdk\""))
        .collect();
    let descriptor = disk_writer::vmdk_descriptor("twoGbMaxExtentSparse", &extents, None);
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("one.vmdk"), &stream).unwrap();
    fs::write(root.path().join("many.vmdk"), &descriptor).unwrap();
    let peak = peak_bytes_of_zipper_test(&root.path().join("many.vmdk"));
    assert!(
        peak < 100 << 20,
        "peak {peak} bytes for {copies} extents of 32 MiB grains"
    );
}

#[cfg(unix)]
#[test]
fn a_vmdk_grain_directory_is_read_in_batches() {
    let options = disk_writer::Vmdk {
        grain_sectors: 1,
        table_entries: 512,
        stream: false,
    };
    let mut image = disk_writer::vmdk_sparse(options, 64 * SECTOR as u64, &[], "huge.vmdk");
    let capacity = 1u64 << 36;
    image.bytes[12..20].copy_from_slice(&capacity.to_le_bytes());
    let directory_end = (image.gd_at as u64) + 4 * (capacity >> 9);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("huge.vmdk");
    fs::write(&path, &image.bytes).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(directory_end + 4096)
        .unwrap();
    let peak = peak_bytes_of_zipper_test(&path);
    assert!(
        peak < 100 << 20,
        "peak {peak} bytes for a 512 MiB grain directory"
    );
}

#[test]
fn a_grain_directory_past_2_pow_55_sectors_is_refused_not_wrapped() {
    let disk = container_disk();
    let options = disk_writer::Vmdk {
        grain_sectors: 16,
        table_entries: 512,
        stream: false,
    };
    let image = disk_writer::vmdk_sparse(options, disk.len() as u64, &pieces(&disk, 8192), "a");
    let mut wrapped = image.bytes.clone();
    let gd = u64::from_le_bytes(wrapped[0x38..0x40].try_into().unwrap());
    wrapped[0x38..0x40].copy_from_slice(&(gd + (1 << 55)).to_le_bytes());
    refused(
        "grain directory past 2^55 sectors",
        &wrapped,
        "grain directory lies outside",
    );
}

#[test]
fn a_gzip_whose_name_holds_the_vdi_signature_stays_a_gzip() {
    use std::io::Write;
    let mut name = vec![b'n'; 54];
    name.extend_from_slice(&0xBEDA_107Fu32.to_le_bytes());
    name.extend_from_slice(b".txt");
    let mut encoder = flate2::GzBuilder::new()
        .filename(name)
        .write(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"a gzip, not a disk\n").unwrap();
    let gzip = encoder.finish().unwrap();
    assert_eq!(&gzip[0x40..0x44], &0xBEDA_107Fu32.to_le_bytes());
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("note.txt.gz");
    fs::write(&path, &gzip).unwrap();
    assert!(!zipper::disk::sniff(&path).unwrap());
    assert_ne!(
        zipper::choose_reader(Engine::Native, &path).unwrap(),
        Reader::Disk
    );
}

#[test]
fn container_disks_past_64_tib_and_first_level_tables_past_their_limit_are_refused() {
    let disk = container_disk();
    let clusters = pieces(&disk, 4096);
    let mut qcow =
        disk_writer::qcow_sparse(qcow_options(3, false), disk.len() as u64, &clusters).bytes;
    put_be64(&mut qcow, 24, 1 << 47);
    qcow[36..40].copy_from_slice(&(1u32 << 18).to_be_bytes());
    refused("QCOW of 128 TiB", &qcow, "larger than 64 TiB");
    let mut old = disk_writer::qcow_sparse(
        disk_writer::Qcow {
            version: 1,
            cluster_bits: 9,
            compressed: false,
            zero_flags: false,
        },
        disk.len() as u64,
        &pieces(&disk, 512),
    )
    .bytes;
    old[33] = 6;
    put_be64(&mut old, 24, 1 << 40);
    refused(
        "QCOW 1 with 33,554,432 first-level entries",
        &old,
        "4,194,304 entries",
    );
    let mut vdi = disk_writer::vdi(
        &disk,
        disk_writer::Vdi {
            block_log: 20,
            fixed: false,
            zero_entries: false,
        },
    );
    vdi[0x170..0x178].copy_from_slice(&(1u64 << 47).to_le_bytes());
    vdi[0x180..0x184].copy_from_slice(&(1u32 << 27).to_le_bytes());
    refused("VDI of 128 TiB", &vdi, "larger than 64 TiB");
    let mut vmdk = disk_writer::vmdk(
        &disk,
        disk_writer::Vmdk {
            grain_sectors: 16,
            table_entries: 512,
            stream: false,
        },
        "case.img",
    );
    vmdk[12..20].copy_from_slice(&(1u64 << 38).to_le_bytes());
    refused("VMDK of 128 TiB", &vmdk, "larger than 64 TiB");
    let mut sparse = disk_writer::android_sparse(&disk, 1 << 28);
    sparse[16..20].copy_from_slice(&(1u32 << 19).to_le_bytes());
    refused(
        "Android sparse image of 128 TiB",
        &sparse,
        "larger than 64 TiB",
    );
}

#[cfg(unix)]
#[test]
fn a_vmdk_extent_that_is_a_link_is_refused() {
    let disk = container_disk();
    let (descriptor, flat) = disk_writer::vmdk_flat(&disk, "case-flat.vmdk");
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("disk")).unwrap();
    fs::write(root.path().join("outside.raw"), &flat).unwrap();
    fs::write(root.path().join("disk/case.vmdk"), &descriptor).unwrap();
    std::os::unix::fs::symlink("../outside.raw", root.path().join("disk/case-flat.vmdk")).unwrap();
    let destination = root.path().join("out");
    let error = read_archive(
        &root.path().join("disk/case.vmdk"),
        Some(&destination),
        ReadOptions::default(),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("is a link"), "{error:#}");
    assert!(!destination.exists());
}
