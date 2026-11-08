//! Every disk image in `tests/data/disk-corpus` extracts to the tree 7-Zip
//! extracted when the corpus was written, and to the tree 7-Zip extracts now
//! where it is installed. Where `mkfs.fat`, `mtools`, `sgdisk` and
//! `qemu-img` are installed, seeded FAT12, FAT16 and FAT32 volumes in raw,
//! MBR, GPT, VHD and VHDX images must extract exactly as `7zz x` extracts
//! them. Images from the test writer, whole and damaged, must too, or be
//! refused for a reason `docs/reference/disk-reader.md` documents. Without
//! 7-Zip 26 or later the differential tests are skipped, unless
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
];

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

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
            // EXTERNAL(7-Zip): it turns a FAT local time into UTC with the offset in force
            // now, and Zipper with the one in force on that date, so only the time within
            // the hour is compared.
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

/// Compare Zipper with 7-Zip on one image. Returns whether 7-Zip extracted it.
fn compare(binary: &Path, label: &str, bytes: &[u8], name: &str) -> bool {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join(name);
    fs::write(&archive, bytes).unwrap();
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
    }
    images
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
    for stored in images {
        let name = stored.file_name().unwrap().to_string_lossy().into_owned();
        let work = tempfile::tempdir().unwrap();
        let image = unpacked(&stored, work.path());
        let ours = zipper_tree(&image, &work.path().join("zipper"))
            .unwrap_or_else(|error| panic!("{name}: {error:#}"));
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, &image, &work.path().join("7z"))
                .unwrap_or_else(|| panic!("{name}: 7-Zip failed"));
            assert_eq!(ours, theirs, "{name} differs from 7-Zip");
        }
        assert_eq!(
            Some(&without_metadata(&ours)),
            recorded.get(&name),
            "{name} differs from the recorded tree"
        );
    }
}

struct Tools {
    mkfs: PathBuf,
    mcopy: PathBuf,
    mdel: PathBuf,
    mattrib: PathBuf,
    mmd: PathBuf,
    sgdisk: PathBuf,
    qemu: PathBuf,
}

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

fn os(text: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(text)
}

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

/// Format a FAT volume with `mkfs.fat`, fill it with `mcopy`, mark a file
/// read-only, delete files and write a large one across the holes they
/// leave, so its clusters are fragmented.
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
#[test]
#[ignore = "rewrites tests/data/disk-corpus with mkfs.fat, mtools, sgdisk and qemu-img; run with --ignored to regenerate"]
fn write_the_corpus() {
    let tools = tools().expect("mkfs.fat, mtools, sgdisk and qemu-img write the corpus");
    let binary = sevenzip_binary().expect("7-Zip records the corpus trees");
    let work = tempfile::tempdir().unwrap();
    let mut trees = BTreeMap::new();
    for (name, bytes) in tool_images(&tools, work.path(), 1) {
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
    fs::write(
        Path::new(CORPUS).join("trees.json"),
        serde_json::to_string_pretty(&trees).unwrap() + "\n",
    )
    .unwrap();
}
