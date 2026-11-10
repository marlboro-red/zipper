//! Every disk image in `tests/data/disk-corpus` extracts to the tree 7-Zip
//! extracted when the corpus was written, and to the tree 7-Zip extracts now
//! where it is installed. Where `mkfs.fat`, `mtools`, `sgdisk` and
//! `qemu-img` are installed, seeded FAT12, FAT16 and FAT32 volumes in raw,
//! MBR, GPT, VHD and VHDX images must extract exactly as `7zz x` extracts
//! them. Images from the test writer, whole and damaged, must too, or be
//! refused for a reason `docs/reference/disk-reader.md` documents. Without
//! 7-Zip 26.05 or later the differential tests are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set.
#[path = "common/mod.rs"]
mod common;
#[path = "common/disk_writer.rs"]
mod disk_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use disk_writer::{Item, SECTOR, Volume, build, file, folder};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::disk::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/disk-corpus");
const STRICTER: &[&str] = &[
    "share cluster",
    "two entries named",
    "not a safe file name",
    "outside ASCII",
    "not valid UTF-16",
    "does not repeat",
    "region tables differ",
    "never replayed",
    "share their bytes",
    "not one plain file name",
    "share a cluster",
    "not aligned to a cluster",
    "lies past the end of the image",
    "names another place in the disk",
    "shorter than the descriptor says",
    "carry extra data",
    "shorter than its disk needs",
    "larger than 64 TiB",
    "grain directory lies outside the image",
    "needs features Zipper does not know",
    "is not a VMDK sparse extent",
];

const SENT_TO_7ZIP: &str = "Zipper does not read it in process";

fn sevenzip_binary() -> Option<PathBuf> {
    sevenzip::sevenzip()
}

#[cfg(unix)]
fn tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(
            [
                "/opt/homebrew/sbin",
                "/usr/local/sbin",
                "/usr/sbin",
                "/sbin",
            ]
            .map(PathBuf::from),
        )
        .map(|folder| folder.join(name))
        .find(|candidate| candidate.is_file())
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tree(root: &Path) -> BTreeMap<String, String> {
    if !root.exists() {
        return BTreeMap::new();
    }
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(entry.path()).unwrap();
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                format!(" {:o}", metadata.permissions().mode() & 0o777)
            };
            #[cfg(not(unix))]
            let mode = String::new();
            // EXTERNAL(7-Zip): FatHandler's LocalFileTimeToFileTime applies today's UTC
            // offset, not the one on the file's date, so only the time within the hour is compared.
            let modified = metadata
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |time| time.as_secs() % 3600);
            let kind = if entry.file_type().is_dir() {
                format!("dir{mode}")
            } else {
                format!(
                    "{}{mode} {modified}",
                    sha256(&fs::read(entry.path()).unwrap())
                )
            };
            (name, kind)
        })
        // EXTERNAL(7-Zip): it writes HFS+ and APFS extended attributes and NTFS system file
        // streams as `file:name`, which those readers leave out, as their reference pages say.
        .filter(|(name, _)| {
            !(name.contains(":com.apple.") || name.starts_with("[SYSTEM]/") && name.contains(':'))
        })
        .collect()
}

fn without_metadata(tree: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.iter()
        .map(|(name, kind)| (name.clone(), kind.split(' ').next().unwrap().to_owned()))
        .collect()
}

fn without_partition_times(tree: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let partitions = !tree.is_empty()
        && tree.keys().all(|name| {
            !name.contains('/') && name.starts_with(|first: char| first.is_ascii_digit())
        });
    if !partitions {
        return tree;
    }
    tree.into_iter()
        .map(|(name, kind)| {
            (
                name,
                kind.rsplit_once(' ')
                    .map_or(kind.clone(), |(kept, _)| kept.to_owned()),
            )
        })
        .collect()
}

fn zipper_tree(archive: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(archive, Some(into), ReadOptions::default())?;
    let found = tree(into);
    common::make_tree_deletable(into);
    Ok(found)
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let found = run.status.success().then(|| tree(into));
    common::make_tree_deletable(into);
    found
}

fn compare(binary: &Path, label: &str, bytes: &[u8], name: &str) -> bool {
    compare_files(binary, label, &[(name, bytes)])
}

fn compare_files(binary: &Path, label: &str, files: &[(&str, &[u8])]) -> bool {
    let work = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        fs::write(work.path().join(name), bytes).unwrap();
    }
    let archive = work.path().join(files[0].0);
    let theirs =
        sevenzip_tree(binary, &archive, &work.path().join("7z")).map(without_partition_times);
    let ours = zipper_tree(&archive, &work.path().join("zipper")).map(without_partition_times);
    match (theirs, ours) {
        (Some(theirs), Ok(ours)) => assert_eq!(ours, theirs, "{label} differs from 7-Zip"),
        (None, Ok(_)) => panic!("{label}: 7-Zip refused and Zipper extracted"),
        (Some(_), Err(error)) => {
            let error = format!("{error:#}");
            assert!(
                STRICTER.iter().any(|reason| error.contains(reason)),
                "{label}: 7-Zip extracted and Zipper refused for an undocumented reason: {error}"
            );
            assert!(!work.path().join("zipper").exists(), "{label}: left output");
        }
        (None, Err(_)) => assert!(!work.path().join("zipper").exists(), "{label}: left output"),
    }
    fs::metadata(work.path().join("7z")).is_ok()
}

fn pattern(length: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9) | 1;
    let cycle: Vec<u8> = (0..257)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect();
    cycle.iter().copied().cycle().take(length).collect()
}

fn writer_items(seed: u32) -> Vec<Item> {
    let mut deep = file("deepest file.txt", &pattern(100, seed));
    for level in (0..9).rev() {
        deep = folder(&format!("Level{level}"), vec![deep]);
    }
    vec![
        file("README.TXT", b"read me\n"),
        file(
            "A rather long name, with spaces and a comma.dat",
            &pattern(5000, seed + 1),
        ),
        file("empty", b""),
        Item::File {
            name: "LOCKED.TXT".to_owned(),
            data: b"locked\n".to_vec(),
            read_only: true,
        },
        folder(
            "Data files",
            vec![
                file("large.bin", &pattern(150_000, seed + 2)),
                file("Ünïcödé name ✓.txt", b"unicode\n"),
                folder("EMPTY", vec![]),
            ],
        ),
        deep,
    ]
}

fn writer_images(seed: u32) -> Vec<(String, Vec<u8>, &'static str)> {
    let mut images = Vec::new();
    for (bits, sectors, cluster) in [(12, 8000, 4), (16, 40_000, 2), (32, 140_000, 1)] {
        let mut volume = Volume::new(bits, sectors);
        volume.cluster_sectors = cluster;
        volume.fragment = seed % 2 == 1;
        volume.deleted = seed.is_multiple_of(3);
        let built = build(&volume, &writer_items(seed));
        let mbr = disk_writer::mbr(&[(0x0C, 2048, &built.bytes)], 0);
        let gpt = disk_writer::gpt(
            &[(
                disk_writer::BASIC_DATA,
                "Basic data partition",
                2048,
                &built.bytes,
            )],
            0,
        );
        let two = disk_writer::mbr(
            &[(0x06, 63, &built.bytes), (0x0B, 63 + sectors, &built.bytes)],
            9,
        );
        images.push((format!("fat{bits}"), built.bytes.clone(), "img"));
        images.push((format!("mbr fat{bits}"), mbr.clone(), "img"));
        images.push((format!("gpt fat{bits}"), gpt.clone(), "img"));
        images.push((format!("two partitions fat{bits}"), two, "img"));
        images.push((
            format!("fixed vhd fat{bits}"),
            disk_writer::vhd_fixed(&mbr),
            "vhd",
        ));
        images.push((
            format!("dynamic vhd fat{bits}"),
            disk_writer::vhd_dynamic(&gpt, 21),
            "vhd",
        ));
        images.push((
            format!("vhdx 1m fat{bits}"),
            disk_writer::vhdx(&mbr, 20),
            "vhdx",
        ));
        images.push((
            format!("vhdx 32m fat{bits}"),
            disk_writer::vhdx(&gpt, 25),
            "vhdx",
        ));
        let fat_index = bits / 16;
        for (index, (label, image, extension)) in
            container_images(&mbr, &gpt, seed).into_iter().enumerate()
        {
            if (index as u32 + fat_index + seed).is_multiple_of(3) {
                images.push((format!("{label} fat{bits}"), image, extension));
            }
        }
    }
    images
}

fn container_images(mbr: &[u8], gpt: &[u8], seed: u32) -> Vec<(String, Vec<u8>, &'static str)> {
    let qcow = |version, cluster_bits, compressed| disk_writer::Qcow {
        version,
        cluster_bits,
        compressed,
        zero_flags: false,
    };
    let vmdk = |stream| disk_writer::Vmdk {
        grain_sectors: if seed.is_multiple_of(2) { 128 } else { 16 },
        table_entries: 512,
        stream,
    };
    let vdi = |fixed| disk_writer::Vdi {
        block_log: 20,
        fixed,
        zero_entries: seed % 2 == 1,
    };
    vec![
        (
            "qcow2 v3".to_owned(),
            disk_writer::qcow(mbr, qcow(3, 16, false)),
            "qcow2",
        ),
        (
            "qcow2 v2 zlib".to_owned(),
            disk_writer::qcow(gpt, qcow(2, 12 + seed % 3, true)),
            "qcow2",
        ),
        (
            "qcow v1".to_owned(),
            disk_writer::qcow(mbr, qcow(1, 12, false)),
            "qcow",
        ),
        (
            "vdi dynamic".to_owned(),
            disk_writer::vdi(gpt, vdi(false)),
            "vdi",
        ),
        (
            "vdi fixed".to_owned(),
            disk_writer::vdi(mbr, vdi(true)),
            "vdi",
        ),
        (
            "sparse vmdk".to_owned(),
            disk_writer::vmdk(mbr, vmdk(false), "disk.vmdk"),
            "vmdk",
        ),
        (
            "stream vmdk".to_owned(),
            disk_writer::vmdk(gpt, vmdk(true), "disk.vmdk"),
            "vmdk",
        ),
        (
            "android sparse".to_owned(),
            disk_writer::android_sparse(mbr, 4096 >> (seed % 3)),
            "simg",
        ),
        (
            "android sparse gpt".to_owned(),
            disk_writer::android_sparse(gpt, 4096),
            "img",
        ),
    ]
}

#[test]
fn writer_images_extract_as_7zip_extracts_them_in_every_container() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    for seed in [1, 2, 3] {
        for (label, bytes, extension) in writer_images(seed) {
            assert!(
                compare(
                    &binary,
                    &format!("seed {seed} {label}"),
                    &bytes,
                    &format!("disk.{extension}")
                ),
                "seed {seed} {label}: 7-Zip refused a whole image"
            );
        }
    }
}

#[test]
fn damaged_images_are_refused_exactly_when_7zip_refuses_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut refused = 0;
    let mut runs = 0;
    for (label, bytes, extension) in writer_images(4) {
        if !(label.starts_with("mbr")
            || label.starts_with("dynamic")
            || label.starts_with("vhdx 1m"))
        {
            continue;
        }
        let structure_end = bytes.len().min(4 << 20);
        let mut state = 0x1234_5678u32 ^ label.len() as u32;
        for _ in 0..90 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let at = match state % 3 {
                0 => (state as usize >> 4) % (4 * SECTOR),
                1 => (state as usize >> 4) % structure_end,
                _ => 2048 * SECTOR + (state as usize >> 4) % (200 * SECTOR),
            }
            .min(bytes.len() - 1);
            let mut damaged = bytes.clone();
            damaged[at] ^= 1 << ((state >> 2) % 8);
            if !compare(
                &binary,
                &format!("{label} flip at {at}"),
                &damaged,
                &format!("disk.{extension}"),
            ) {
                refused += 1;
            }
            runs += 1;
        }
    }
    assert!(
        refused > 0 && refused < runs,
        "7-Zip refused {refused} of {runs}"
    );
}

fn small_mbr(seed: u32) -> Vec<u8> {
    let mut volume = Volume::new(12, 8000);
    volume.cluster_sectors = 4;
    volume.fragment = true;
    let built = build(&volume, &writer_items(seed));
    disk_writer::mbr(&[(0x01, 64, &built.bytes)], 0)
}

fn spread(ranges: &[std::ops::Range<usize>], step: usize) -> Vec<usize> {
    ranges
        .iter()
        .flat_map(|range| range.clone().step_by(step.max(1)))
        .collect()
}

struct Damageable {
    label: &'static str,
    name: &'static str,
    bytes: Vec<u8>,
    places: Vec<usize>,
}

fn damageable_containers(seed: u32) -> Vec<Damageable> {
    let disk = small_mbr(seed);
    let pieces = |size: usize| -> Vec<(u64, Vec<u8>)> {
        disk.chunks(size)
            .enumerate()
            .filter(|(_, piece)| piece.iter().any(|byte| *byte != 0))
            .map(|(index, piece)| {
                let mut piece = piece.to_vec();
                piece.resize(size, 0);
                (index as u64, piece)
            })
            .collect()
    };
    let mut found = Vec::new();
    for (label, version, compressed) in [
        ("qcow2 v3", 3, false),
        ("qcow2 v2 zlib", 2, true),
        ("qcow v1", 1, false),
    ] {
        let options = disk_writer::Qcow {
            version,
            cluster_bits: 12,
            compressed,
            zero_flags: false,
        };
        let image = disk_writer::qcow_sparse(options, disk.len() as u64, &pieces(4096));
        let mut ranges = vec![0..112, image.l1_at..image.l1_at + 16];
        ranges.extend(image.l2_at.iter().map(|at| *at..at + 4096));
        ranges.extend(image.data_at.iter().take(8).map(|at| *at..at + 4));
        found.push(Damageable {
            label,
            name: if version == 1 {
                "disk.qcow"
            } else {
                "disk.qcow2"
            },
            places: spread(&ranges, 1),
            bytes: image.bytes,
        });
    }
    let options = disk_writer::Vdi {
        block_log: 20,
        fixed: false,
        zero_entries: true,
    };
    let image = disk_writer::vdi_sparse(options, disk.len() as u64, &pieces(1 << 20));
    found.push(Damageable {
        label: "vdi dynamic",
        name: "disk.vdi",
        places: spread(&[0x40..0x1C8, image.map_at..image.data_at], 1),
        bytes: image.bytes,
    });
    for (label, stream) in [("sparse vmdk", false), ("stream vmdk", true)] {
        let options = disk_writer::Vmdk {
            grain_sectors: 16,
            table_entries: 512,
            stream,
        };
        let image =
            disk_writer::vmdk_sparse(options, disk.len() as u64, &pieces(8192), "disk.vmdk");
        let mut ranges = vec![0..0x50, image.gd_at - SECTOR..image.gd_at + 8];
        ranges.extend(image.gt_at.iter().map(|at| at - SECTOR..at + 2048));
        ranges.extend(image.grain_at.iter().take(8).map(|at| *at..at + 14));
        if stream {
            let end = image.bytes.len();
            ranges.push(end - 3 * SECTOR..end - 2 * SECTOR + 0x50);
        }
        found.push(Damageable {
            label,
            name: "disk.vmdk",
            places: spread(&ranges, 1),
            bytes: image.bytes,
        });
    }
    let sparse = disk_writer::android_sparse(&disk, 4096);
    let mut chunks = Vec::new();
    chunks.push(0..28);
    let mut at = 28;
    while at + 12 <= sparse.len() {
        chunks.push(at..at + 12);
        at += u32::from_le_bytes(sparse[at + 8..at + 12].try_into().unwrap()) as usize;
    }
    found.push(Damageable {
        label: "android sparse",
        name: "disk.simg",
        places: spread(&chunks, 1),
        bytes: sparse,
    });
    found
}

fn outcome(binary: &Path, label: &str, files: &[(&str, &[u8])]) -> Result<bool, String> {
    std::panic::catch_unwind(|| compare_files(binary, label, files)).map_err(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| label.to_owned())
    })
}

fn sent_to_7zip_or_matching(binary: &Path, label: &str, files: &[(&str, &[u8])]) {
    if let Err(failure) = outcome(binary, label, files) {
        assert!(failure.contains(SENT_TO_7ZIP), "{failure}");
    }
}

fn damage_containers(flips_each: usize) {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    use rayon::prelude::*;
    let mut failures = Vec::new();
    for container in damageable_containers(8) {
        let mut state = 0x9E37_79B9u32 ^ container.label.len() as u32;
        let step = (container.places.len() / flips_each).max(1);
        let mut flips = Vec::new();
        for (index, at) in container.places.iter().enumerate() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            if flips_each >= container.places.len() || index % step == (state as usize) % step {
                flips.push((*at, 1u8 << (state % 8)));
            }
        }
        assert!(!flips.is_empty(), "{}: no flips ran", container.label);
        let outcomes: Vec<Result<bool, String>> = flips
            .par_iter()
            .map(|(at, bit)| {
                let mut damaged = container.bytes.clone();
                damaged[*at] ^= bit;
                let label = format!("{} flip of {bit:#04x} at {at}", container.label);
                outcome(&binary, &label, &[(container.name, &damaged)])
            })
            .collect();
        let refused = outcomes
            .iter()
            .filter(|outcome| outcome == &&Ok(false))
            .count();
        failures.extend(
            outcomes
                .into_iter()
                .filter_map(Result::err)
                .filter(|failure| !failure.contains(SENT_TO_7ZIP)),
        );
        eprintln!(
            "{}: 7-Zip refused {refused} of {}",
            container.label,
            flips.len()
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn damaged_containers_are_refused_exactly_when_7zip_refuses_them() {
    damage_containers(24);
}

#[test]
#[ignore = "flips every byte of every container structure; run with --ignored"]
fn every_flip_in_every_container_structure_matches_7zip() {
    damage_containers(usize::MAX);
}

#[test]
fn vmdk_descriptors_with_flat_and_sparse_extents_extract_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let disk = small_mbr(9);
    let (descriptor, flat) = disk_writer::vmdk_flat(&disk, "disk-flat.vmdk");
    assert!(compare_files(
        &binary,
        "flat vmdk",
        &[("disk.vmdk", &descriptor), ("disk-flat.vmdk", &flat)],
    ));
    let first = 1000 * SECTOR;
    let mut padded = vec![0x77u8; 3 * SECTOR];
    padded.extend_from_slice(&disk[..first]);
    let sparse = disk_writer::vmdk(
        &disk[first..],
        disk_writer::Vmdk {
            grain_sectors: 128,
            table_entries: 512,
            stream: false,
        },
        "part-2.vmdk",
    );
    let extents = [
        format!("RW {} FLAT \"part-1.bin\" 3", first / SECTOR),
        format!(
            "RW {} SPARSE \"part-2.vmdk\"",
            (disk.len() - first) / SECTOR
        ),
    ];
    let descriptor = disk_writer::vmdk_descriptor("twoGbMaxExtentSparse", &extents, None);
    assert!(compare_files(
        &binary,
        "two extents",
        &[
            ("disk.vmdk", descriptor.as_bytes()),
            ("part-1.bin", &padded),
            ("part-2.vmdk", &sparse),
        ],
    ));
    let extents = [
        format!("RW {} FLAT \"part-1.bin\" 3", first / SECTOR),
        format!("RW {} ZERO", (disk.len() - first) / SECTOR),
    ];
    let descriptor = disk_writer::vmdk_descriptor("monolithicFlat", &extents, None);
    sent_to_7zip_or_matching(
        &binary,
        "zero extent",
        &[
            ("disk.vmdk", descriptor.as_bytes()),
            ("part-1.bin", &padded),
        ],
    );
    let (descriptor, _) = disk_writer::vmdk_flat(&disk, "missing-flat.vmdk");
    sent_to_7zip_or_matching(&binary, "missing extent", &[("disk.vmdk", &descriptor)]);
    let (descriptor, flat) = disk_writer::vmdk_flat(&disk, "../disk-flat.vmdk");
    sent_to_7zip_or_matching(
        &binary,
        "extent outside the folder",
        &[("disk.vmdk", &descriptor), ("disk-flat.vmdk", &flat)],
    );
    let (descriptor, flat) = disk_writer::vmdk_flat(&disk, "disk-flat.vmdk");
    let text = String::from_utf8(descriptor).unwrap();
    for (label, edited) in [
        ("short extent", text.replace("RW 8064 FLAT", "RW 8000 FLAT")),
        ("long extent", text.replace("RW 8064 FLAT", "RW 9000 FLAT")),
        (
            "parent",
            text.replace("parentCID=ffffffff", "parentCID=12345678"),
        ),
        ("vmfs", text.replace(" FLAT ", " VMFS ")),
        ("line of spaces", text.clone() + "   \n"),
        ("indented comment", text.replace("# Extent", "  # Extent")),
        (
            "words after the offset",
            text.replace("\"disk-flat.vmdk\" 0", "\"disk-flat.vmdk\" 0 more words"),
        ),
        (
            "malformed offset",
            text.replace("\"disk-flat.vmdk\" 0", "\"disk-flat.vmdk\" zero"),
        ),
        (
            "quote before the equals sign",
            text.replace("version=1", "ver\"sion=1"),
        ),
        (
            "quote after the equals sign",
            text.replace("version=1", "version=\"1"),
        ),
        ("hexadecimal size", text.replace("RW 8064 ", "RW 0x1f80 ")),
        (
            "unquoted file",
            text.replace("\"disk-flat.vmdk\"", "disk-flat.vmdk"),
        ),
        (
            "no extent",
            text.replace("RW 8064 FLAT \"disk-flat.vmdk\" 0", ""),
        ),
    ] {
        sent_to_7zip_or_matching(
            &binary,
            label,
            &[("disk.vmdk", edited.as_bytes()), ("disk-flat.vmdk", &flat)],
        );
    }
}

fn unpacked(stored: &Path, into: &Path) -> PathBuf {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(fs::File::open(stored).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    let path = into.join(stored.file_stem().unwrap());
    fs::write(&path, bytes).unwrap();
    path
}

fn corpus_images() -> Vec<PathBuf> {
    let mut images: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "xz"))
        .collect();
    images.sort();
    images
}

#[test]
fn every_corpus_image_extracts_the_tree_7zip_extracts() {
    let recorded: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let binary = sevenzip_binary();
    let images = corpus_images();
    assert_eq!(
        images.len(),
        recorded.len(),
        "every corpus image has a recorded tree"
    );
    let work = tempfile::tempdir().unwrap();
    let unpacked: Vec<(String, PathBuf)> = images
        .iter()
        .map(|stored| {
            let name = stored.file_name().unwrap().to_string_lossy().into_owned();
            (name, unpacked(stored, work.path()))
        })
        .collect();
    for (index, (name, image)) in unpacked.into_iter().enumerate() {
        let ours = zipper_tree(&image, &work.path().join(format!("zipper {index}")))
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, &image, &work.path().join(format!("7z {index}")))
                .unwrap_or_else(|| panic!("{name}: 7-Zip failed"));
            assert_eq!(
                without_partition_times(ours.clone()),
                without_partition_times(theirs),
                "{name} differs from 7-Zip"
            );
        }
        assert_eq!(
            Some(&without_metadata(&ours)),
            recorded.get(&name),
            "{name} differs from the recorded tree"
        );
    }
}

#[cfg(unix)]
struct Tools {
    mkfs: PathBuf,
    mcopy: PathBuf,
    mdel: PathBuf,
    mattrib: PathBuf,
    mmd: PathBuf,
    sgdisk: PathBuf,
    qemu: PathBuf,
}

#[cfg(unix)]
fn tools() -> Option<Tools> {
    Some(Tools {
        mkfs: tool("mkfs.fat")?,
        mcopy: tool("mcopy")?,
        mdel: tool("mdel")?,
        mattrib: tool("mattrib")?,
        mmd: tool("mmd")?,
        sgdisk: tool("sgdisk")?,
        qemu: tool("qemu-img")?,
    })
}

#[cfg(unix)]
fn run(program: &Path, arguments: &[&std::ffi::OsStr]) {
    let output = Command::new(program)
        .args(arguments)
        .env("MTOOLS_SKIP_CHECK", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {arguments:?}: {}",
        program.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn os(text: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(text)
}

#[cfg(unix)]
fn source_tree(root: &Path, seed: u32) {
    let deep: PathBuf = (0..10).map(|level| format!("level {level}")).collect();
    fs::create_dir_all(root.join(&deep)).unwrap();
    fs::write(
        root.join(&deep).join("bottom of the tree.txt"),
        pattern(300, seed),
    )
    .unwrap();
    fs::write(root.join("hello.txt"), b"hello from mtools\n").unwrap();
    fs::write(
        root.join("A long file name with spaces.text"),
        pattern(4000, seed + 1),
    )
    .unwrap();
    fs::write(root.join("empty file"), b"").unwrap();
    fs::write(root.join("Ünïcode ✓.txt"), b"unicode\n").unwrap();
    fs::write(root.join("UPPER.TXT"), b"upper\n").unwrap();
    fs::write(root.join("lower.txt"), b"lower\n").unwrap();
    fs::write(root.join("MiXeD.Txt"), b"mixed\n").unwrap();
}

#[cfg(unix)]
fn tool_volume(tools: &Tools, work: &Path, bits: u32, seed: u32) -> Vec<u8> {
    let (kilobytes, cluster) = match bits {
        12 => ("4000", "4"),
        16 => ("20000", "2"),
        _ => ("40000", "1"),
    };
    let image = work.join(format!("fat{bits}-{seed}.img"));
    let _ = fs::remove_file(&image);
    run(
        &tools.mkfs,
        &[
            os("-C"),
            os("-F"),
            os(&bits.to_string()),
            os("-s"),
            os(cluster),
            os("-n"),
            os("ZIPPERCORP"),
            image.as_os_str(),
            os(kilobytes),
        ],
    );
    let source = work.join(format!("source-{bits}-{seed}"));
    source_tree(&source, seed);
    let target = image.display().to_string();
    let mut arguments: Vec<std::ffi::OsString> =
        vec!["-s".into(), "-i".into(), target.clone().into()];
    for entry in fs::read_dir(&source).unwrap() {
        arguments.push(entry.unwrap().path().into());
    }
    arguments.push("::/".into());
    let arguments: Vec<&std::ffi::OsStr> = arguments
        .iter()
        .map(|argument| argument.as_os_str())
        .collect();
    run(&tools.mcopy, &arguments);
    let image_text = image.display().to_string();
    for index in 0..6 {
        let piece = work.join(format!("piece{index}.bin"));
        fs::write(&piece, pattern(9000, seed * 10 + index)).unwrap();
        run(
            &tools.mcopy,
            &[
                os("-i"),
                os(&image_text),
                piece.as_os_str(),
                os(&format!("::/piece{index}.bin")),
            ],
        );
    }
    for index in [1, 3] {
        run(
            &tools.mdel,
            &[
                os("-i"),
                os(&image_text),
                os(&format!("::/piece{index}.bin")),
            ],
        );
    }
    let large = work.join("fragmented.bin");
    fs::write(&large, pattern(40_000, seed + 7)).unwrap();
    run(
        &tools.mmd,
        &[os("-i"), os(&image_text), os("::/later folder")],
    );
    run(
        &tools.mcopy,
        &[
            os("-i"),
            os(&image_text),
            large.as_os_str(),
            os("::/later folder/fragmented.bin"),
        ],
    );
    run(
        &tools.mattrib,
        &[os("-i"), os(&image_text), os("+r"), os("::/hello.txt")],
    );
    fs::read(&image).unwrap()
}

#[cfg(unix)]
fn sgdisk_image(
    tools: &Tools,
    work: &Path,
    name: &str,
    volumes: &[(&str, &str, &[u8])],
) -> Vec<u8> {
    let path = work.join(name);
    let total: usize = volumes
        .iter()
        .map(|(_, _, bytes)| bytes.len())
        .sum::<usize>()
        + (4 << 20);
    fs::write(&path, vec![0u8; total]).unwrap();
    let mut arguments: Vec<String> = Vec::new();
    for (index, (kind, label, bytes)) in volumes.iter().enumerate() {
        let number = index + 1;
        arguments.push(format!("--new={number}:0:+{}", bytes.len() / SECTOR));
        arguments.push(format!("--typecode={number}:{kind}"));
        arguments.push(format!("--change-name={number}:{label}"));
    }
    arguments.push(path.display().to_string());
    let arguments: Vec<&std::ffi::OsStr> = arguments.iter().map(|argument| os(argument)).collect();
    run(&tools.sgdisk, &arguments);
    let mut disk = fs::read(&path).unwrap();
    let mut lba = 2048;
    for (_, _, bytes) in volumes {
        disk[lba * SECTOR..lba * SECTOR + bytes.len()].copy_from_slice(bytes);
        lba += bytes.len() / SECTOR;
        lba = lba.next_multiple_of(2048);
    }
    disk
}

#[cfg(unix)]
fn qemu_convert(
    tools: &Tools,
    work: &Path,
    disk: &[u8],
    name: &str,
    format: &str,
    options: &str,
) -> Vec<u8> {
    let raw = work.join(format!("{name}.raw"));
    let out = work.join(name);
    fs::write(&raw, disk).unwrap();
    let _ = fs::remove_file(&out);
    run(
        &tools.qemu,
        &[
            os("convert"),
            os("-f"),
            os("raw"),
            os("-O"),
            os(format),
            os("-o"),
            os(options),
            raw.as_os_str(),
            out.as_os_str(),
        ],
    );
    fs::read(&out).unwrap()
}

#[cfg(unix)]
fn tool_images(tools: &Tools, work: &Path, seed: u32) -> Vec<(String, Vec<u8>)> {
    let fat12 = tool_volume(tools, work, 12, seed);
    let fat16 = tool_volume(tools, work, 16, seed);
    let fat32 = tool_volume(tools, work, 32, seed);
    let mbr_one = disk_writer::mbr(&[(0x06, 2048, &fat16)], 0);
    let mbr_two = disk_writer::mbr(
        &[
            (0x01, 2048, &fat12),
            (0x0C, 2048 + (fat12.len() / SECTOR) as u32, &fat32),
        ],
        0,
    );
    let gpt_one = sgdisk_image(
        tools,
        work,
        "gpt-one.img",
        &[("0700", "Basic data", &fat32)],
    );
    let gpt_two = sgdisk_image(
        tools,
        work,
        "gpt-two.img",
        &[("0700", "Main", &fat16), ("8300", "Linux", &fat12)],
    );
    vec![
        ("fat12.img".to_owned(), fat12.clone()),
        ("fat16.img".to_owned(), fat16.clone()),
        ("fat32.img".to_owned(), fat32.clone()),
        ("mbr-one.img".to_owned(), mbr_one.clone()),
        ("mbr-two.img".to_owned(), mbr_two.clone()),
        ("gpt-one.img".to_owned(), gpt_one.clone()),
        ("gpt-two.img".to_owned(), gpt_two.clone()),
        (
            "fixed.vhd".to_owned(),
            qemu_convert(tools, work, &mbr_one, "fixed.vhd", "vpc", "subformat=fixed"),
        ),
        (
            "dynamic.vhd".to_owned(),
            qemu_convert(
                tools,
                work,
                &gpt_one,
                "dynamic.vhd",
                "vpc",
                "subformat=dynamic",
            ),
        ),
        (
            "raw-fat.vhd".to_owned(),
            qemu_convert(
                tools,
                work,
                &fat12,
                "raw-fat.vhd",
                "vpc",
                "subformat=dynamic",
            ),
        ),
        (
            "blocks-1m.vhdx".to_owned(),
            qemu_convert(
                tools,
                work,
                &mbr_one,
                "blocks-1m.vhdx",
                "vhdx",
                "subformat=dynamic,block_size=1M",
            ),
        ),
        (
            "blocks-32m.vhdx".to_owned(),
            qemu_convert(
                tools,
                work,
                &gpt_two,
                "blocks-32m.vhdx",
                "vhdx",
                "subformat=dynamic,block_size=32M",
            ),
        ),
        (
            "fixed.vhdx".to_owned(),
            qemu_convert(
                tools,
                work,
                &mbr_two,
                "fixed.vhdx",
                "vhdx",
                "subformat=fixed",
            ),
        ),
    ]
}

#[cfg(unix)]
#[test]
fn tool_images_extract_as_7zip_extracts_them() {
    let (Some(binary), Some(tools)) = (sevenzip_binary(), tools()) else {
        return;
    };
    let work = tempfile::tempdir().unwrap();
    for seed in [5, 6] {
        for (name, bytes) in tool_images(&tools, work.path(), seed) {
            assert!(
                compare(&binary, &format!("seed {seed} {name}"), &bytes, &name),
                "seed {seed} {name}: 7-Zip refused a whole image"
            );
        }
    }
}

#[cfg(unix)]
fn qemu_files(
    qemu: &Path,
    work: &Path,
    disk: &[u8],
    name: &str,
    arguments: &[&str],
) -> Vec<(String, Vec<u8>)> {
    let folder = work.join(format!("{name}.d"));
    fs::create_dir_all(&folder).unwrap();
    let raw = work.join(format!("{name}.raw"));
    fs::write(&raw, disk).unwrap();
    let output = Command::new(qemu)
        .args(["convert", "-f", "raw", "-O"])
        .args(arguments)
        .arg(&raw)
        .arg(name)
        .current_dir(&folder)
        .output()
        .unwrap();
    assert!(
        folder.join(name).exists(),
        "qemu-img wrote no {name}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(&raw).unwrap();
    let mut files: Vec<(String, Vec<u8>)> = fs::read_dir(&folder)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_string_lossy().into_owned();
            (file, fs::read(&path).unwrap())
        })
        .collect();
    files.sort_by_key(|(file, _)| file != name);
    fs::remove_dir_all(&folder).unwrap();
    files
}

#[cfg(unix)]
fn corpus_raw(name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(
        fs::File::open(Path::new(CORPUS).join(format!("{name}.xz"))).unwrap(),
    )
    .read_to_end(&mut bytes)
    .unwrap();
    bytes
}

#[cfg(unix)]
#[test]
fn qemu_images_extract_as_7zip_extracts_them() {
    let (Some(binary), Some(qemu)) = (sevenzip_binary(), tool("qemu-img")) else {
        return;
    };
    let work = tempfile::tempdir().unwrap();
    let fat12 = corpus_raw("fat12.img");
    let mbr_one = corpus_raw("mbr-one.img");
    let gpt_two = corpus_raw("gpt-two.img");
    let readable: [(&[u8], &str, &[&str]); 16] = [
        (&mbr_one, "mbr-one.qcow2", &["qcow2"]),
        (&gpt_two, "zlib.qcow2", &["qcow2", "-c"]),
        (&gpt_two, "v2.qcow2", &["qcow2", "-o", "compat=0.10"]),
        (&fat12, "c512.qcow2", &["qcow2", "-o", "cluster_size=512"]),
        (&fat12, "c2m.qcow2", &["qcow2", "-o", "cluster_size=2M"]),
        (
            &fat12,
            "c4k-zlib.qcow2",
            &["qcow2", "-c", "-o", "cluster_size=4096"],
        ),
        (&fat12, "lazy.qcow2", &["qcow2", "-o", "lazy_refcounts=on"]),
        (
            &fat12,
            "refcount1.qcow2",
            &["qcow2", "-o", "refcount_bits=1"],
        ),
        (
            &fat12,
            "prealloc.qcow2",
            &["qcow2", "-o", "preallocation=metadata"],
        ),
        (&fat12, "fat12.qcow", &["qcow"]),
        (&gpt_two, "gpt-two.vdi", &["vdi"]),
        (&fat12, "static.vdi", &["vdi", "-o", "static=on"]),
        (&mbr_one, "mbr-one.vmdk", &["vmdk"]),
        (
            &gpt_two,
            "flat.vmdk",
            &["vmdk", "-o", "subformat=monolithicFlat"],
        ),
        (
            &fat12,
            "split.vmdk",
            &["vmdk", "-o", "subformat=twoGbMaxExtentSparse"],
        ),
        (
            &fat12,
            "split-flat.vmdk",
            &["vmdk", "-o", "subformat=twoGbMaxExtentFlat"],
        ),
    ];
    for (disk, name, arguments) in readable {
        let files = qemu_files(&qemu, work.path(), disk, name, arguments);
        let files: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(file, bytes)| (file.as_str(), &bytes[..]))
            .collect();
        assert!(
            compare_files(&binary, name, &files),
            "{name}: 7-Zip refused it"
        );
    }
    let refused: [(&str, &[&str]); 5] = [
        ("stream.vmdk", &["vmdk", "-o", "subformat=streamOptimized"]),
        ("extended.qcow2", &["qcow2", "-o", "extended_l2=on"]),
        (
            "zstd.qcow2",
            &["qcow2", "-c", "-o", "compression_type=zstd"],
        ),
        (
            "external.qcow2",
            &["qcow2", "-o", "data_file=external.raw,data_file_raw=on"],
        ),
        ("compressed.qcow", &["qcow", "-c"]),
    ];
    for (name, arguments) in refused {
        let files = qemu_files(&qemu, work.path(), &fat12, name, arguments);
        let files: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(file, bytes)| (file.as_str(), &bytes[..]))
            .collect();
        compare_files(&binary, name, &files);
    }
}

#[cfg(unix)]
fn container_corpus(
    qemu: &Path,
    work: &Path,
    raws: &BTreeMap<String, Vec<u8>>,
) -> Vec<(String, Vec<u8>)> {
    let raw = |name: &str| &raws[name][..];
    let mut files = Vec::new();
    for (disk, name, arguments) in [
        (raw("mbr-one.img"), "mbr-one.qcow2", &["qcow2"][..]),
        (raw("gpt-one.img"), "gpt-one-zlib.qcow2", &["qcow2", "-c"]),
        (
            raw("mbr-two.img"),
            "mbr-two-v2.qcow2",
            &["qcow2", "-o", "compat=0.10"],
        ),
        (raw("fat12.img"), "fat12.qcow", &["qcow"]),
        (raw("gpt-two.img"), "gpt-two.vdi", &["vdi"]),
        (
            raw("fat12.img"),
            "fat12-static.vdi",
            &["vdi", "-o", "static=on"],
        ),
        (raw("mbr-one.img"), "mbr-one.vmdk", &["vmdk"]),
        (
            raw("gpt-two.img"),
            "gpt-two-flat.vmdk",
            &["vmdk", "-o", "subformat=monolithicFlat"],
        ),
        (raw("apfs-gpt.img"), "apfs-gpt.vdi", &["vdi"]),
        (raw("hfs-gpt.img"), "hfs-gpt.qcow2", &["qcow2"]),
        (raw("ntfs-mbr.img"), "ntfs-mbr.vmdk", &["vmdk"]),
    ] {
        files.extend(qemu_files(qemu, work, disk, name, arguments));
    }
    files.push((
        "gpt-one-stream.vmdk".to_owned(),
        disk_writer::vmdk(
            raw("gpt-one.img"),
            disk_writer::Vmdk {
                grain_sectors: 128,
                table_entries: 512,
                stream: true,
            },
            "gpt-one-stream.vmdk",
        ),
    ));
    files.push((
        "mbr-two.simg".to_owned(),
        disk_writer::android_sparse(raw("mbr-two.img"), 4096),
    ));
    files
}

#[cfg(unix)]
fn hdiutil_disk(work: &Path, file_system: &str, name: &str) -> Vec<u8> {
    let source = work.join(format!("{name} source"));
    fs::create_dir_all(source.join("folder")).unwrap();
    fs::write(source.join("hello.txt"), b"hello from hdiutil\n").unwrap();
    fs::write(source.join("folder/pattern.bin"), pattern(20_000, 3)).unwrap();
    let image = work.join(name);
    run(
        &tool("hdiutil").expect("hdiutil writes the HFS+ and APFS corpus disks"),
        &[
            os("create"),
            os("-quiet"),
            os("-srcfolder"),
            source.as_os_str(),
            os("-fs"),
            os(file_system),
            os("-layout"),
            os("GPTSPUD"),
            os("-format"),
            os("UDTO"),
            os("-volname"),
            os("Zipper"),
            image.as_os_str(),
        ],
    );
    fs::read(work.join(format!("{name}.cdr"))).unwrap()
}

#[cfg(unix)]
fn container_sources(work: &Path, raws: &mut BTreeMap<String, Vec<u8>>) {
    raws.insert(
        "apfs-gpt.img".to_owned(),
        hdiutil_disk(work, "APFS", "apfs-gpt"),
    );
    raws.insert(
        "hfs-gpt.img".to_owned(),
        hdiutil_disk(work, "HFS+", "hfs-gpt"),
    );
    let mut ntfs = Vec::new();
    liblzma::read::XzDecoder::new(
        fs::File::open(Path::new(CORPUS).join("../ntfs-corpus/ntfs3g-basic.ntfs.xz")).unwrap(),
    )
    .read_to_end(&mut ntfs)
    .unwrap();
    raws.insert(
        "ntfs-mbr.img".to_owned(),
        disk_writer::mbr(&[(0x07, 2048, &ntfs)], 0),
    );
}

#[cfg(unix)]
fn record_corpus(
    binary: &Path,
    work: &Path,
    files: Vec<(String, Vec<u8>)>,
    trees: &mut BTreeMap<String, BTreeMap<String, String>>,
) {
    let images = work.join("images");
    fs::create_dir_all(&images).unwrap();
    for (name, bytes) in &files {
        let mut packed = Vec::new();
        liblzma::read::XzEncoder::new(&bytes[..], 9)
            .read_to_end(&mut packed)
            .unwrap();
        fs::write(Path::new(CORPUS).join(format!("{name}.xz")), packed).unwrap();
        fs::write(images.join(name), bytes).unwrap();
    }
    for (name, _) in &files {
        let theirs = sevenzip_tree(binary, &images.join(name), &work.join(format!("{name}.7z")))
            .unwrap_or_else(|| panic!("7-Zip failed on {name}"));
        trees.insert(format!("{name}.xz"), without_metadata(&theirs));
    }
}

#[cfg(unix)]
#[test]
#[ignore = "adds the QCOW, VDI, VMDK and Android sparse images to tests/data/disk-corpus from its raw disks with qemu-img and hdiutil; run with --ignored"]
fn write_the_container_corpus() {
    let qemu = tool("qemu-img").expect("qemu-img writes the container corpus");
    let binary = sevenzip_binary().expect("7-Zip records the corpus trees");
    let work = tempfile::tempdir().unwrap();
    let mut raws: BTreeMap<String, Vec<u8>> = [
        "fat12.img",
        "mbr-one.img",
        "mbr-two.img",
        "gpt-one.img",
        "gpt-two.img",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), corpus_raw(name)))
    .collect();
    container_sources(work.path(), &mut raws);
    let path = Path::new(CORPUS).join("trees.json");
    let mut trees: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let files = container_corpus(&qemu, work.path(), &raws);
    record_corpus(&binary, work.path(), files, &mut trees);
    fs::write(&path, serde_json::to_string_pretty(&trees).unwrap() + "\n").unwrap();
}

#[cfg(unix)]
#[test]
#[ignore = "rewrites tests/data/disk-corpus with mkfs.fat, mtools, sgdisk, hdiutil and qemu-img; run with --ignored to regenerate"]
fn write_the_corpus() {
    let tools = tools().expect("mkfs.fat, mtools, sgdisk and qemu-img write the corpus");
    let binary = sevenzip_binary().expect("7-Zip records the corpus trees");
    let work = tempfile::tempdir().unwrap();
    let mut trees = BTreeMap::new();
    let mut raws: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (name, bytes) in tool_images(&tools, work.path(), 1) {
        if name.ends_with(".img") {
            raws.insert(name.clone(), bytes.clone());
        }
        let mut packed = Vec::new();
        liblzma::read::XzEncoder::new(&bytes[..], 9)
            .read_to_end(&mut packed)
            .unwrap();
        fs::write(Path::new(CORPUS).join(format!("{name}.xz")), packed).unwrap();
        let image = work.path().join(&name);
        fs::write(&image, &bytes).unwrap();
        let theirs = sevenzip_tree(&binary, &image, &work.path().join(format!("{name}.7z")))
            .expect("7-Zip extracts the corpus");
        trees.insert(format!("{name}.xz"), without_metadata(&theirs));
    }
    container_sources(work.path(), &mut raws);
    let files = container_corpus(&tools.qemu, work.path(), &raws);
    record_corpus(&binary, work.path(), files, &mut trees);
    fs::write(
        Path::new(CORPUS).join("trees.json"),
        serde_json::to_string_pretty(&trees).unwrap() + "\n",
    )
    .unwrap();
}
