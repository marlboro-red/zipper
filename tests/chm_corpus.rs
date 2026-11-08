//! Every CHM in `tests/data/chm-corpus` extracts to the tree 7-Zip extracted
//! when the corpus was written, and to the tree 7-Zip extracts now where it
//! is installed. CHM and Help 2 files from the test writer cover every LZX
//! window, reset intervals from one block to many, empty files, many small
//! files, deep paths and files that cross reset boundaries. Each must
//! extract byte for byte to its source and to the tree 7-Zip extracts, and
//! damaged copies must be refused exactly when 7-Zip refuses them, or for a
//! reason `docs/reference/chm-reader.md` lists as stricter.
#[path = "common/chm_writer.rs"]
mod chm_writer;
#[path = "common/mod.rs"]
mod common;
#[path = "common/lzx_encoder.rs"]
mod lzx_encoder;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use chm_writer::{File, Form, Options, dir, file};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_chm::{ReadOptions, read_archive};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/chm-corpus");

const REFUSALS_STRICTER_THAN_7ZIP: &[&str] = &[
    "neither a listing nor an index",
    "holds a NUL byte",
    "is not UTF-8",
    "unsafe archive path",
    "collides with another entry",
    "two entries of different kinds",
    "packs more than 1 MiB",
    "shares bytes with the entry before it",
    "CHM section name is not UTF-16",
    "chunks are not 32 bytes to 1 MiB",
    "chunks are not 64 bytes to 1 MiB",
    "uncompressed size overflows",
    "entry offset overflows",
    "ends past 2^64 bytes",
];

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
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tree(root: &Path) -> BTreeMap<String, String> {
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
            let kind = if entry.file_type().is_dir() {
                "dir".to_owned()
            } else {
                sha256(&fs::read(entry.path()).unwrap())
            };
            (name, kind)
        })
        .collect()
}

fn sevenzip_tree(binary: &Path, archive: &Path, into: &Path) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    run.status.success().then(|| tree(into))
}

fn sevenzip() -> Option<PathBuf> {
    let found = sevenzip::sevenzip();
    assert!(
        found.is_some() || std::env::var_os("ZIPPER_PARITY_REQUIRE_7Z").is_none(),
        "ZIPPER_PARITY_REQUIRE_7Z is set and no 7-Zip 26 or later is installed"
    );
    found
}

fn zipper_tree(archive: &Path, into: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(archive, Some(into), ReadOptions::default())?;
    Ok(tree(into))
}

fn sample(random: &mut Random, length: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(length);
    while data.len() < length {
        match random.below(5) {
            0 => data.extend((0..random.below(40)).map(|_| random.next() as u8)),
            1 if !data.is_empty() => {
                let back = 1 + random.below(data.len().min(70_000));
                for _ in 0..2 + random.below(300) {
                    data.push(data[data.len() - back]);
                }
            }
            2 => data.extend([0xE8, random.next() as u8, 0, 0, 0]),
            3 => data.extend(std::iter::repeat_n(
                random.next() as u8 % 4,
                random.below(60),
            )),
            _ => data.extend_from_slice(b"<p>the quick brown fox</p> "),
        }
    }
    data.truncate(length);
    data
}

fn expected_tree(files: &[File]) -> BTreeMap<String, String> {
    let mut expected = BTreeMap::new();
    for file in files {
        let name = String::from_utf8(file.name[1..].to_vec()).unwrap();
        let name = name.trim_end_matches('/');
        let mut parent = name;
        while let Some(slash) = parent.rfind('/') {
            parent = &parent[..slash];
            expected.insert(parent.to_owned(), "dir".to_owned());
        }
        if file.name.ends_with(b"/") {
            expected.insert(name.to_owned(), "dir".to_owned());
        } else {
            expected.insert(name.to_owned(), sha256(&file.data));
        }
    }
    expected
}

fn random_files(random: &mut Random, count: usize, largest: usize) -> Vec<File> {
    let mut files = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    if random.below(2) == 0 {
        files.push(file("/#SYSTEM", &sample(random, 300), false));
        files.push(file("/$OBJINST", &sample(random, 2000), true));
        names.extend(["/#SYSTEM".to_owned(), "/$OBJINST".to_owned()]);
    }
    for index in 0..count {
        let depth = random.below(6);
        let parts: Vec<String> = (0..depth)
            .map(|level| format!("level{level}-{}", random.below(3)))
            .collect();
        let mut name = String::from("/");
        for part in &parts {
            name.push_str(part);
            name.push('/');
            if random.below(4) == 0 && names.insert(name.clone()) {
                files.push(dir(&name));
            }
        }
        name.push_str(&format!("page{index}.html"));
        let size = match random.below(6) {
            0 => 0,
            1 => 1 + random.below(40),
            2 => 1 + random.below(3000),
            _ => 1 + random.below(largest),
        };
        names.insert(name.clone());
        files.push(file(&name, &sample(random, size), random.below(5) != 0));
    }
    files
}

fn random_options(random: &mut Random, form: Form, case: usize) -> Options {
    let window_bits = 15 + (case % 7) as u8;
    let mut options = Options::plain(form);
    options.window_bits = window_bits;
    options.interval_bits = [0, 0, 1, 1, 2, 3, 16][random.below(7)] as u32;
    options.lzx = lzx_encoder::Options::random(&mut || random.next(), window_bits);
    options.lzx.longest_block = options.lzx.longest_block.max(2000);
    options.chunk_size = match form {
        Form::Chm => [512, 1024, 4096][random.below(3)],
        Form::Help2 => [512, 2048, 8192][random.below(3)],
    };
    options.index_chunk = random.below(3) == 0;
    options.extra_reset_entry = random.below(2) == 0;
    options.zero_count = form == Form::Chm && random.below(4) == 0;
    options
}

fn check_generated(form: Form, cases: usize, seed: u64) {
    let binary = sevenzip();
    let mut random = Random(seed);
    for case in 0..cases {
        let count = [1, 3, 20, 120][case % 4];
        let largest = [70_000, 200_000, 5_000][random.below(3)];
        let files = random_files(&mut random, count, largest);
        let options = random_options(&mut random, form, case);
        let (bytes, _) = chm_writer::write(&files, &options);
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join(if form == Form::Chm {
            "case.chm"
        } else {
            "case.hxs"
        });
        fs::write(&path, &bytes).unwrap();
        let label = format!(
            "case {case}: {} files, window 2^{}, interval 2^{} blocks",
            files.len(),
            options.window_bits,
            options.interval_bits
        );
        let ours = zipper_tree(&path, &work.path().join("zipper"))
            .unwrap_or_else(|error| panic!("{label}: {error:#}"));
        assert_eq!(
            ours,
            expected_tree(&files),
            "{label} differs from its source"
        );
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, &path, &work.path().join("7zip"))
                .unwrap_or_else(|| panic!("{label}: 7-Zip refuses it"));
            assert_eq!(ours, theirs, "{label} differs from 7-Zip");
        }
        common::make_tree_deletable(work.path());
    }
}

#[test]
fn generated_chm_extract_byte_for_byte_and_exactly_as_7zip_extracts_them() {
    check_generated(Form::Chm, 56, 0xC0FF_EE01);
}

#[test]
fn generated_help2_files_extract_byte_for_byte_and_exactly_as_7zip_extracts_them() {
    check_generated(Form::Help2, 28, 0xBEEF_0002);
}

fn corpus_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(CORPUS)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "chm"))
        .collect();
    files.sort();
    files
}

#[test]
fn every_corpus_chm_extracts_the_tree_7zip_extracts() {
    let recorded: BTreeMap<String, BTreeMap<String, String>> =
        serde_json::from_str(&fs::read_to_string(Path::new(CORPUS).join("trees.json")).unwrap())
            .unwrap();
    let files = corpus_files();
    assert_eq!(
        files
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        recorded.keys().cloned().collect::<Vec<_>>(),
        "trees.json names exactly the corpus files"
    );
    let binary = sevenzip();
    for archive in &files {
        let name = archive.file_name().unwrap().to_string_lossy().into_owned();
        let root = tempfile::tempdir().unwrap();
        let ours = zipper_tree(archive, &root.path().join("zipper")).unwrap();
        assert_eq!(
            ours, recorded[&name],
            "{name} differs from the recorded tree"
        );
        let listed = read_archive(archive, None, ReadOptions::default()).unwrap();
        assert_eq!(
            listed.iter().filter(|entry| !entry.directory).count(),
            ours.values().filter(|kind| *kind != "dir").count(),
            "{name} lists every file it writes"
        );
        if let Some(binary) = &binary {
            let theirs = sevenzip_tree(binary, archive, &root.path().join("7zip"))
                .unwrap_or_else(|| panic!("7-Zip refuses {name}"));
            assert_eq!(ours, theirs, "{name} differs from 7-Zip");
        }
        common::make_tree_deletable(root.path());
    }
}

fn compare_with_7zip(binary: &Path, label: &str, bytes: &[u8], form: Form) -> (bool, bool) {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join(if form == Form::Chm {
        "case.chm"
    } else {
        "case.hxs"
    });
    fs::write(&path, bytes).unwrap();
    let ours = zipper_tree(&path, &work.path().join("zipper"));
    let theirs = sevenzip_tree(binary, &path, &work.path().join("7zip"));
    let stricter = match (&ours, &theirs) {
        (Ok(ours), Some(theirs)) => {
            assert_eq!(ours, theirs, "{label}");
            false
        }
        (Ok(_), None) => panic!("{label}: Zipper extracts what 7-Zip refuses"),
        (Err(_), None) => false,
        (Err(error), Some(_)) => {
            let message = format!("{error:#}");
            assert!(
                REFUSALS_STRICTER_THAN_7ZIP
                    .iter()
                    .any(|reason| message.contains(reason)),
                "{label}: Zipper refuses what 7-Zip extracts: {message}"
            );
            true
        }
    };
    common::make_tree_deletable(work.path());
    (ours.is_ok(), stricter)
}

fn damage_case(random: &mut Random, form: Form) -> (Vec<u8>, chm_writer::Layout) {
    let count = 1 + random.below(8);
    let files = random_files(random, count, 80_000);
    let case = random.below(7);
    let mut options = random_options(random, form, case);
    options.chunk_size = if form == Form::Chm { 512 } else { 1024 };
    options.index_chunk = true;
    chm_writer::write(&files, &options)
}

fn sweep(form: Form, rounds: usize, seed: u64) {
    let Some(binary) = sevenzip() else { return };
    let mut random = Random(seed);
    let mut outcomes = [0usize; 3];
    for round in 0..rounds {
        let (mut bytes, layout) = damage_case(&mut random, form);
        let mut regions: Vec<(&str, std::ops::Range<usize>)> = vec![
            ("header", layout.header.clone()),
            ("section 0", layout.section_zero.clone()),
            ("directory header", layout.directory_header.clone()),
        ];
        regions.extend(layout.chunks.iter().map(|chunk| ("chunk", chunk.clone())));
        regions.extend(
            layout
                .system
                .iter()
                .filter(|(name, _)| !name.ends_with("Content"))
                .map(|(_, range)| ("system file", range.clone())),
        );
        regions.extend(layout.blocks.iter().map(|block| ("block", block.clone())));
        regions.retain(|(_, range)| !range.is_empty());
        let (what, range) = regions[random.below(regions.len())].clone();
        let label = match round % 4 {
            0 | 1 => {
                let at = range.start + random.below(range.len());
                let bit = random.below(8);
                bytes[at] ^= 1 << bit;
                format!("round {round}: bit {bit} of byte {at} in the {what}")
            }
            2 => {
                let at = range.start + random.below(range.len());
                bytes[at] = [0, 0xFF, 0x7F, 0x80][random.below(4)];
                format!("round {round}: byte {at} in the {what} set")
            }
            _ => {
                let at = range.start + random.below(range.len() + 1);
                bytes.truncate(at);
                format!("round {round}: cut at {at} in the {what}")
            }
        };
        let (accepted, stricter) = compare_with_7zip(&binary, &label, &bytes, form);
        outcomes[if accepted {
            0
        } else if stricter {
            2
        } else {
            1
        }] += 1;
    }
    assert!(
        outcomes[0] > rounds / 20 && outcomes[1] > rounds / 5,
        "accepted, refused by both and refused only by Zipper: {outcomes:?}"
    );
}

#[test]
fn damaged_chm_are_refused_exactly_when_7zip_refuses_them() {
    sweep(Form::Chm, 1500, 0xDA4A_6E01);
}

#[test]
fn damaged_help2_files_are_refused_exactly_when_7zip_refuses_them() {
    sweep(Form::Help2, 800, 0xDA4A_6E02);
}

#[test]
fn every_header_and_directory_byte_and_every_cut_of_a_small_file_is_judged_as_7zip_judges_it() {
    let Some(binary) = sevenzip() else { return };
    let mut random = Random(0x5EED_C4A1);
    for form in [Form::Chm, Form::Help2] {
        let files = vec![
            dir("/folder/"),
            file("/folder/page.html", &sample(&mut random, 40_000), true),
            file("/#SYSTEM", &sample(&mut random, 90), false),
            file("/empty.html", b"", true),
        ];
        let mut options = Options::plain(form);
        options.chunk_size = if form == Form::Chm { 256 } else { 512 };
        options.index_chunk = true;
        let (bytes, layout) = chm_writer::write(&files, &options);
        let structural = layout.header.start..layout.chunks.last().unwrap().end;
        for at in structural {
            for value in [bytes[at] ^ 0x01, bytes[at] ^ 0x80, 0xFF] {
                let mut damaged = bytes.clone();
                damaged[at] = value;
                compare_with_7zip(
                    &binary,
                    &format!("{form:?}: byte {at} set to {value:#x}"),
                    &damaged,
                    form,
                );
            }
        }
        for cut in (0..bytes.len()).step_by(37) {
            compare_with_7zip(
                &binary,
                &format!("{form:?}: cut at {cut}"),
                &bytes[..cut],
                form,
            );
        }
    }
}

const CHMCMD_CASES: [(&str, usize, usize); 3] = [
    ("small", 12, 30_000),
    ("many", 400, 600),
    ("large", 6, 400_000),
];

#[test]
#[ignore = "writes the chmcmd corpus; needs chmcmd from Free Pascal in ZIPPER_CHMCMD and 7-Zip"]
fn write_the_chmcmd_corpus() {
    let chmcmd = PathBuf::from(std::env::var_os("ZIPPER_CHMCMD").expect("ZIPPER_CHMCMD"));
    let binary = sevenzip().expect("7-Zip");
    let work = tempfile::tempdir().unwrap();
    let mut records = BTreeMap::new();
    for (seed, (name, count, largest)) in CHMCMD_CASES.into_iter().enumerate() {
        let mut random = Random(0xC4A1_0000 + seed as u64);
        let project = work.path().join(name);
        let mut listed = Vec::new();
        for file in random_files(&mut random, count, largest) {
            let path = String::from_utf8(file.name[1..].to_vec()).unwrap();
            if path.ends_with('/') || path.starts_with(['#', '$']) {
                continue;
            }
            let target = project.join(&path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, &file.data).unwrap();
            listed.push(path);
        }
        let mut hhp = format!(
            "[OPTIONS]\nCompiled file={name}.chm\nDefault topic={}\nTitle={name}\n\n[FILES]\n",
            listed[0]
        );
        for path in &listed {
            hhp.push_str(path);
            hhp.push('\n');
        }
        fs::write(project.join("project.hhp"), hhp).unwrap();
        let status = Command::new(&chmcmd)
            .args(["--no-html-scan", "project.hhp"])
            .current_dir(&project)
            .status()
            .unwrap();
        assert!(status.success(), "{name}");
        let out = Path::new(CORPUS).join(format!("{name}.chm"));
        fs::copy(project.join(format!("{name}.chm")), &out).unwrap();
        let tree = sevenzip_tree(&binary, &out, &work.path().join(format!("{name}-7zip")))
            .unwrap_or_else(|| panic!("7-Zip refuses {name}"));
        records.insert(format!("{name}.chm"), tree);
    }
    fs::write(
        Path::new(CORPUS).join("trees.json"),
        serde_json::to_string_pretty(&records).unwrap() + "\n",
    )
    .unwrap();
}
