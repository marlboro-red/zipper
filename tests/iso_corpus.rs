//! Every disc image in `tests/data/iso-corpus` extracts to the tree 7-Zip
//! extracted when the corpus was written, and to the tree 7-Zip extracts now
//! where it is installed. Generated discs of ISO 9660, Joliet, Rock Ridge,
//! El Torito and UDF 1.02 to 2.60, alone and together, extract exactly as
//! `7zz x` extracts them, including which tree a hybrid disc gives. Without
//! 7-Zip 26 or later the differential tests are skipped, unless
//! `ZIPPER_PARITY_REQUIRE_7Z` is set. `ZIPPER_ISO_SAMPLES` names a folder of
//! further images to compare.
#[path = "common/iso_writer.rs"]
mod iso_writer;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use iso_writer::{Ads, Boot, Disc, Item, Kind, Udf, dir, file, symlink};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_iso::{ReadOptions, read_archive};

type Damage = Box<dyn Fn(&mut Vec<u8>)>;

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/iso-corpus");

fn sevenzip_binary() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn mode(metadata: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        format!(" {:o}", metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        String::new()
    }
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
            let kind = if entry.file_type().is_symlink() {
                format!("link:{}", fs::read_link(entry.path()).unwrap().display())
            } else if entry.file_type().is_dir() {
                format!("dir{}", mode(&metadata))
            } else {
                format!(
                    "{}{}",
                    sha256(&fs::read(entry.path()).unwrap()),
                    mode(&metadata)
                )
            };
            (name, kind)
        })
        .collect()
}

fn without_modes(tree: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    tree.iter()
        .map(|(name, kind)| {
            let kind = match kind.split_once(' ') {
                Some((kind, _)) => kind,
                None => kind,
            };
            (name.clone(), kind.to_owned())
        })
        .collect()
}

fn zipper_tree(archive: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(archive, Some(into), ReadOptions::default())?;
    Ok(tree(into))
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
    run.status.success().then(|| tree(into))
}

fn unpacked(stored: &Path, into: &Path) -> PathBuf {
    let mut bytes = Vec::new();
    liblzma::read::XzDecoder::new(fs::File::open(stored).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    let name = stored.file_stem().unwrap();
    let path = into.join(name);
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
    let mut written = BTreeMap::new();
    for stored in corpus_images() {
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
        if std::env::var_os("ZIPPER_ISO_WRITE_TREES").is_some() {
            written.insert(name.clone(), without_modes(&ours));
        } else {
            assert_eq!(
                Some(&without_modes(&ours)),
                recorded.get(&name),
                "{name} differs from the recorded tree"
            );
        }
    }
    if std::env::var_os("ZIPPER_ISO_WRITE_TREES").is_some() {
        fs::write(
            Path::new(CORPUS).join("trees.json"),
            serde_json::to_string_pretty(&written).unwrap() + "\n",
        )
        .unwrap();
    }
}

#[test]
fn sample_images_extract_as_7zip_extracts_them() {
    let (Some(folder), Some(binary)) = (std::env::var_os("ZIPPER_ISO_SAMPLES"), sevenzip_binary())
    else {
        return;
    };
    for entry in fs::read_dir(folder).unwrap() {
        let image = entry.unwrap().path();
        let work = tempfile::tempdir().unwrap();
        let theirs = sevenzip_tree(&binary, &image, &work.path().join("7z"));
        let ours = zipper_tree(&image, &work.path().join("zipper"));
        match (theirs, ours) {
            (Some(theirs), Ok(ours)) => assert_eq!(ours, theirs, "{}", image.display()),
            (None, Ok(_)) => panic!("{}: 7-Zip failed and Zipper did not", image.display()),
            (_, Err(error)) => eprintln!("{}: refused: {error:#}", image.display()),
        }
    }
}

fn compare(binary: &Path, name: &str, disc: &Disc) {
    let built = disc.build();
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join(format!("{name}.iso"));
    fs::write(&image, &built.bytes).unwrap();
    if let Some(keep) = std::env::var_os("ZIPPER_ISO_KEEP") {
        fs::write(Path::new(&keep).join(format!("{name}.iso")), &built.bytes).unwrap();
    }
    let theirs = sevenzip_tree(binary, &image, &work.path().join("7z"))
        .unwrap_or_else(|| panic!("{name}: 7-Zip failed"));
    let ours = zipper_tree(&image, &work.path().join("zipper"))
        .unwrap_or_else(|error| panic!("{name}: {error:#}"));
    assert_eq!(ours, theirs, "{name} differs from 7-Zip");
}

fn bytes_of(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn sample_items() -> Vec<Item> {
    vec![
        Item {
            mode: 0o100755,
            ..file("hello.txt", b"hello\n")
        },
        Item {
            mode: 0o40700,
            ..dir("folder")
        },
        Item {
            mode: 0o100600,
            ..file("folder/data.bin", &bytes_of(5000, 1))
        },
        file("folder/empty", b""),
        file("folder/exact", &bytes_of(2048, 2)),
        file("folder/one more", &bytes_of(2049, 3)),
        symlink("folder/near", "data.bin"),
        dir("folder/sub"),
        dir("folder/sub/deeper"),
        file("folder/sub/deeper/leaf.txt", &bytes_of(70_000, 4)),
        file("Mixed Case name.text", b"x"),
        file("café ünïcödé.txt", b"unicode"),
        file("日本語のファイル.txt", b"cjk"),
        dir("empty folder"),
    ]
}

fn named(item: Item, iso: &[u8], rock_ridge: &[u8], joliet: &str, udf: &[u8]) -> Item {
    Item {
        iso_id: Some(iso.to_vec()),
        rock_ridge_name: Some(rock_ridge.to_vec()),
        joliet_id: Some(joliet.encode_utf16().collect()),
        udf_name: Some(udf.to_vec()),
        ..item
    }
}

fn odd_names() -> Vec<Item> {
    vec![
        named(file("a", b"1"), b"NAME.;1", b"rr;1", "jo;1", b"\x08u;1"),
        named(file("b", b"2"), b"VER;2", b"trail.", "jot.", b"\x08ut."),
        named(
            file("c", b"3"),
            b"DOT..;1",
            b"two..",
            "two..;1",
            b"\x08two..",
        ),
        named(file("d", b"4"), b"X.Y;1", b"x;2", "x;2", b"\x08x;2"),
        named(file("e", b"5"), b"E.;1", b" lead", " lead", b"\x08 lead"),
        named(
            file("f", b"6"),
            b"F.;1",
            "🎵 note".as_bytes(),
            "🎵 note",
            &[16, 0xD8, 0x3C, 0xDF, 0x75, 0, b' ', 0, b'n'],
        ),
        dir("g"),
        named(file("g/h", b"7"), b"H.;1", b"h", "h", &[]),
        dir("i"),
        named(file("i/j", b"8"), b"J.;1", b"j", "j", b"\x08   "),
    ]
}

fn without_links(items: &[Item]) -> Vec<Item> {
    items
        .iter()
        .filter(|item| !matches!(item.kind, Kind::Symlink(_)))
        .cloned()
        .collect()
}

fn named_discs() -> Vec<(&'static str, Disc)> {
    let items = sample_items();
    let plain = without_links(&items);
    let iso = Disc {
        items: items.clone(),
        iso: true,
        ..Disc::default()
    };
    let udf = |udf: Udf| Disc {
        items: plain.clone(),
        udf: Some(udf),
        ..Disc::default()
    };
    let boot = |entries: Vec<Boot>| Disc {
        boot: entries,
        ..iso.clone()
    };
    vec![
        ("iso", iso.clone()),
        (
            "odd names",
            Disc {
                items: odd_names(),
                iso: true,
                ..Disc::default()
            },
        ),
        (
            "odd rock ridge names",
            Disc {
                items: odd_names(),
                iso: true,
                rock_ridge: true,
                ..Disc::default()
            },
        ),
        (
            "odd joliet names",
            Disc {
                items: odd_names(),
                iso: true,
                joliet: true,
                ..Disc::default()
            },
        ),
        (
            "odd udf names",
            Disc {
                items: odd_names(),
                udf: Some(Udf::default()),
                ..Disc::default()
            },
        ),
        (
            "joliet",
            Disc {
                joliet: true,
                ..iso.clone()
            },
        ),
        (
            "joliet with versions",
            Disc {
                joliet: true,
                joliet_versions: true,
                ..iso.clone()
            },
        ),
        (
            "rock ridge",
            Disc {
                rock_ridge: true,
                ..iso.clone()
            },
        ),
        (
            "rock ridge and joliet",
            Disc {
                rock_ridge: true,
                joliet: true,
                ..iso.clone()
            },
        ),
        (
            "multi-extent",
            Disc {
                extent_limit: Some(4096),
                rock_ridge: true,
                ..iso.clone()
            },
        ),
        (
            "multi-extent joliet",
            Disc {
                extent_limit: Some(2048),
                joliet: true,
                ..iso.clone()
            },
        ),
        ("udf 1.02 short", udf(Udf::default())),
        (
            "udf 1.50 long",
            udf(Udf {
                revision: 0x0150,
                ads: Ads::Long,
                ..Udf::default()
            }),
        ),
        (
            "udf 2.01 extended",
            udf(Udf {
                revision: 0x0201,
                extended: true,
                ..Udf::default()
            }),
        ),
        (
            "udf 2.50 metadata",
            udf(Udf {
                revision: 0x0250,
                extended: true,
                metadata: true,
                ..Udf::default()
            }),
        ),
        (
            "udf 2.60 metadata inline",
            udf(Udf {
                revision: 0x0260,
                extended: true,
                metadata: true,
                ads: Ads::Inline,
                inline_dirs: true,
                ..Udf::default()
            }),
        ),
        (
            "udf inline",
            udf(Udf {
                ads: Ads::Inline,
                inline_dirs: true,
                ..Udf::default()
            }),
        ),
        (
            "udf at partition start",
            udf(Udf {
                partition_gap: 0,
                ..Udf::default()
            }),
        ),
        (
            "iso and udf",
            Disc {
                items: plain.clone(),
                iso: true,
                udf: Some(Udf::default()),
                ..Disc::default()
            },
        ),
        (
            "iso joliet rock ridge and udf",
            Disc {
                items: plain.clone(),
                iso: true,
                joliet: true,
                rock_ridge: true,
                udf: Some(Udf::default()),
                ..Disc::default()
            },
        ),
        (
            "udf with a link falls back to rock ridge",
            Disc {
                items: items.clone(),
                iso: true,
                rock_ridge: true,
                udf: Some(Udf::default()),
                ..Disc::default()
            },
        ),
        (
            "udf with a link falls back to joliet",
            Disc {
                items: items.clone(),
                iso: true,
                joliet: true,
                udf: Some(Udf::default()),
                ..Disc::default()
            },
        ),
        (
            "udf hides el torito",
            Disc {
                items: plain.clone(),
                iso: true,
                udf: Some(Udf::default()),
                boot: vec![Boot {
                    bootable: true,
                    media: 0,
                    sectors: 4,
                    data: bytes_of(2048, 5),
                    vendor: None,
                }],
                ..Disc::default()
            },
        ),
        (
            "one boot image",
            boot(vec![Boot {
                bootable: true,
                media: 0,
                sectors: 4,
                data: bytes_of(2048, 6),
                vendor: None,
            }]),
        ),
        (
            "boot images with vendors and floppies",
            boot(vec![
                Boot {
                    bootable: true,
                    media: 0,
                    sectors: 4,
                    data: bytes_of(2048, 7),
                    vendor: None,
                },
                Boot {
                    bootable: false,
                    media: 2,
                    sectors: 1,
                    data: bytes_of(4096, 8),
                    vendor: Some("EFI/boot\\x".to_owned()),
                },
                Boot {
                    bootable: true,
                    media: 4,
                    sectors: 3,
                    data: bytes_of(1536, 9),
                    vendor: Some("disk".to_owned()),
                },
                Boot {
                    bootable: true,
                    media: 0,
                    sectors: 1,
                    data: bytes_of(6000, 10),
                    vendor: None,
                },
            ]),
        ),
        (
            "joliet with boot",
            Disc {
                joliet: true,
                ..boot(vec![Boot {
                    bootable: false,
                    media: 1,
                    sectors: 0,
                    data: bytes_of(512, 11),
                    vendor: None,
                }])
            },
        ),
    ]
}

#[test]
fn generated_discs_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    for (name, disc) in named_discs() {
        compare(&binary, name, &disc);
    }
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

fn random_disc(random: &mut Random) -> Disc {
    let mut items = Vec::new();
    let mut folders = vec![String::new()];
    for index in 0..random.below(40) + 1 {
        let parent = folders[random.below(folders.len())].clone();
        let name = format!(
            "{}n{index}{}",
            ["", "Up", "sp ace "][random.below(3)],
            [".txt", "", ".tar.gz"][random.below(3)]
        );
        let path = if parent.is_empty() {
            name
        } else {
            format!("{parent}/{name}")
        };
        if random.chance(25) && path.matches('/').count() < 6 {
            folders.push(path.clone());
            items.push(dir(&path));
        } else {
            let length = [0, 1, 2047, 2048, 2049, 10_000][random.below(6)];
            items.push(Item {
                mode: [0o100644, 0o100600, 0o100755][random.below(3)],
                ..file(&path, &bytes_of(length, index as u8))
            });
        }
    }
    let udf = random.chance(50).then(|| Udf {
        revision: [0x0102, 0x0150, 0x0200, 0x0201, 0x0250, 0x0260][random.below(6)],
        extended: random.chance(50),
        ads: [Ads::Short, Ads::Long, Ads::Inline][random.below(3)],
        metadata: random.chance(30),
        inline_dirs: random.chance(30),
        partition_gap: random.below(4) as u32,
    });
    let udf = udf.map(|udf| Udf {
        extended: udf.extended || udf.metadata,
        revision: if udf.metadata {
            udf.revision.max(0x0250)
        } else {
            udf.revision
        },
        ..udf
    });
    Disc {
        items,
        iso: udf.is_none()
            || udf.as_ref().is_some_and(|udf| udf.ads != Ads::Inline) && random.chance(70),
        joliet: random.chance(50),
        rock_ridge: random.chance(50),
        joliet_versions: random.chance(50),
        extent_limit: random
            .chance(30)
            .then_some(2048 * (1 + random.below(3) as u32)),
        udf,
        ..Disc::default()
    }
}

#[test]
fn random_discs_extract_exactly_as_7zip_extracts_them() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let mut random = Random(0x1505_2026);
    for case in 0..100 {
        let disc = random_disc(&mut random);
        compare(&binary, &format!("random disc {case}"), &disc);
    }
}

fn compare_bytes(binary: &Path, name: &str, bytes: &[u8]) {
    let work = tempfile::tempdir().unwrap();
    let image = work.path().join(format!("{name}.iso"));
    fs::write(&image, bytes).unwrap();
    let theirs = sevenzip_tree(binary, &image, &work.path().join("7z"));
    let ours = zipper_tree(&image, &work.path().join("zipper"));
    match (theirs, ours) {
        (Some(theirs), Ok(ours)) => assert_eq!(ours, theirs, "{name} differs from 7-Zip"),
        (Some(_), Err(error)) => {
            panic!("{name}: 7-Zip extracted it and Zipper refused: {error:#}")
        }
        (None, Ok(_)) => panic!("{name}: 7-Zip failed and Zipper did not"),
        (None, Err(_)) => {}
    }
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn a_udf_tree_7zip_cannot_open_falls_back_to_iso_9660_as_7zip_does() {
    let Some(binary) = sevenzip_binary() else {
        return;
    };
    let disc = Disc {
        items: without_links(&sample_items()),
        iso: true,
        joliet: true,
        udf: Some(Udf::default()),
        ..Disc::default()
    };
    let built = disc.build();
    let layout = &built.layout;
    let entry = layout.udf_entries["folder/data.bin"];
    let folder = layout.udf_entries["folder"];
    let folder_block = ((folder - layout.partition_start as usize * iso_writer::SECTOR)
        / iso_writer::SECTOR) as u32;
    let lvd = layout.logical_volume.unwrap();
    let fid = layout.udf_fids["folder/sub"];
    let cases: Vec<(&str, Damage)> = vec![
        ("file entry CRC", Box::new(move |b| b[entry + 200] ^= 1)),
        (
            "extended allocation descriptors",
            Box::new(move |b| {
                b[entry + 34] = 2;
                iso_writer::retag_in_place(b, entry);
            }),
        ),
        (
            "a link entry",
            Box::new(move |b| {
                b[entry + 27] = 12;
                iso_writer::retag_in_place(b, entry);
            }),
        ),
        (
            "a folder entry named as a file",
            Box::new(move |b| {
                b[folder + 27] = 5;
                iso_writer::retag_in_place(b, folder);
            }),
        ),
        (
            "a loop",
            Box::new(move |b| {
                put32(b, fid + 24, folder_block);
                iso_writer::retag_in_place(b, fid);
            }),
        ),
        (
            "block size",
            Box::new(move |b| {
                put32(b, lvd + 212, 512);
                iso_writer::retag_in_place(b, lvd);
            }),
        ),
        (
            "unknown partition map",
            Box::new(move |b| {
                b[lvd + 440] = 3;
                iso_writer::retag_in_place(b, lvd);
            }),
        ),
        (
            "file identifier tag",
            Box::new(move |b| {
                b[fid] = 9;
                iso_writer::retag_in_place(b, fid);
            }),
        ),
        (
            "volume sequence without a terminator",
            Box::new(move |b| {
                let terminator = lvd + iso_writer::SECTOR;
                b[terminator] = 7;
                iso_writer::retag_in_place(b, terminator);
            }),
        ),
        (
            "damaged anchor",
            Box::new(move |b| b[256 * iso_writer::SECTOR + 20] ^= 1),
        ),
    ];
    for (name, change) in cases {
        let mut bytes = built.bytes.clone();
        change(&mut bytes);
        compare_bytes(&binary, name, &bytes);
    }
}
