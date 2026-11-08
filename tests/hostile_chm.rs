//! Hostile CHM and Help 2 files: entries outside their section, overlapping
//! entries, looping and oversized directory chunks, encoded integers that
//! overflow, reset tables that lie, unsafe names and unsupported transforms.
//! Each is refused with nothing published, or extracts as `7zz x` does.
#[path = "common/chm_writer.rs"]
mod chm_writer;
#[path = "common/mod.rs"]
mod common;
#[path = "common/lzx_encoder.rs"]
mod lzx_encoder;
#[path = "common/sevenzip.rs"]
mod sevenzip;

use chm_writer::{File, Form, Options, RawEntry, dir, file};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_chm::{ReadOptions, read_archive};

const PMGL_HEADER: usize = 20;

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut random = Random(seed | 1);
    (0..length)
        .map(|index| {
            if index % 7 < 3 {
                b'a' + (index % 5) as u8
            } else {
                random.next() as u8
            }
        })
        .collect()
}

fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
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
            let data = if entry.file_type().is_dir() {
                b"<dir>".to_vec()
            } else {
                fs::read(entry.path()).unwrap()
            };
            (name, data)
        })
        .collect()
}

struct Case {
    _work: tempfile::TempDir,
    path: PathBuf,
}

impl Case {
    fn new(bytes: &[u8], form: Form) -> Self {
        let work = tempfile::tempdir().unwrap();
        let path = work.path().join(if form == Form::Chm {
            "case.chm"
        } else {
            "case.hxs"
        });
        fs::write(&path, bytes).unwrap();
        Case { _work: work, path }
    }

    fn out(&self, name: &str) -> PathBuf {
        self.path.parent().unwrap().join(name)
    }

    fn zipper(&self) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
        let into = self.out("zipper");
        let result = read_archive(&self.path, Some(&into), ReadOptions::default());
        match result {
            Ok(_) => Ok(tree(&into)),
            Err(error) => {
                assert!(
                    !into.exists(),
                    "a refused file published {}",
                    into.display()
                );
                Err(error)
            }
        }
    }

    fn refused(&self, reason: &str) {
        let error = self.zipper().expect_err("Zipper extracts it");
        let message = format!("{error:#}");
        assert!(
            message.contains(reason),
            "refused for another reason: {message}"
        );
    }

    fn sevenzip(&self) -> Option<Option<BTreeMap<String, Vec<u8>>>> {
        let binary = sevenzip::sevenzip()?;
        let into = self.out("7zip");
        let mut flag = std::ffi::OsString::from("-o");
        flag.push(&into);
        let run = Command::new(binary)
            .args(["x", "-y"])
            .arg(flag)
            .arg(&self.path)
            .output()
            .unwrap();
        Some(run.status.success().then(|| tree(&into)))
    }

    fn sevenzip_refuses_too(&self) {
        if let Some(theirs) = self.sevenzip() {
            assert!(theirs.is_none(), "7-Zip extracts it");
        }
    }

    fn same_as_sevenzip(&self) -> BTreeMap<String, Vec<u8>> {
        let ours = self.zipper().unwrap();
        if let Some(theirs) = self.sevenzip() {
            assert_eq!(Some(&ours), theirs.as_ref(), "the trees differ");
        }
        ours
    }
}

fn base_files() -> Vec<File> {
    vec![
        dir("/folder/"),
        file("/folder/first.html", &noise(1, 50_000), true),
        file("/folder/second.html", &noise(2, 30_000), true),
        file("/#SYSTEM", &noise(3, 100), false),
        file("/empty.html", b"", true),
    ]
}

fn write_with(form: Form, files: &[File], change: impl FnOnce(&mut Options)) -> Vec<u8> {
    let mut options = Options::plain(form);
    change(&mut options);
    chm_writer::write(files, &options).0
}

fn raw(name: &str, section: u64, offset: u64, size: u64) -> RawEntry {
    RawEntry {
        name: name.as_bytes().to_vec(),
        section,
        offset,
        size,
    }
}

fn with_extra(form: Form, extra: Vec<RawEntry>) -> Case {
    Case::new(
        &write_with(form, &base_files(), |options| options.extra = extra),
        form,
    )
}

#[test]
fn plain_files_of_both_forms_extract_as_7zip_extracts_them() {
    for form in [Form::Chm, Form::Help2] {
        let case = Case::new(&write_with(form, &base_files(), |_| {}), form);
        let ours = case.same_as_sevenzip();
        assert_eq!(ours["folder/first.html"], noise(1, 50_000));
        assert_eq!(ours["#SYSTEM"], noise(3, 100));
        assert!(!ours.keys().any(|name| name.starts_with("::")));
    }
}

#[test]
fn an_entry_past_the_blocks_of_its_section_is_refused() {
    for form in [Form::Chm, Form::Help2] {
        let case = with_extra(form, vec![raw("/far.html", 1, 10 * 32_768, 100)]);
        case.refused("past the blocks its reset table lists");
        case.sevenzip_refuses_too();
    }
}

#[test]
fn an_entry_past_the_end_of_the_file_in_section_0_is_refused() {
    let case = with_extra(Form::Chm, vec![raw("/far.html", 0, 1 << 40, 10)]);
    case.refused("runs past the end of the file");
    case.sevenzip_refuses_too();
}

#[test]
fn overlapping_entries_in_an_lzx_section_are_refused() {
    let inside = with_extra(
        Form::Chm,
        vec![raw("/folder/overlap.html", 1, 49_000, 2_000)],
    );
    inside.refused("shares bytes with the entry before it");
    inside.sevenzip_refuses_too();
    let alone = Case::new(
        &write_with(
            Form::Chm,
            &[file("/a.html", &noise(4, 4000), true)],
            |options| options.extra = vec![raw("/b.html", 1, 1000, 1000)],
        ),
        Form::Chm,
    );
    alone.refused("shares bytes with the entry before it");
    alone.sevenzip_refuses_too();
}

#[test]
fn overlapping_entries_in_section_0_extract_as_7zip_extracts_them() {
    let case = with_extra(Form::Chm, vec![raw("/again.bin", 0, 0, 40)]);
    let ours = case.same_as_sevenzip();
    assert_eq!(ours["again.bin"].len(), 40);
}

#[test]
fn an_entry_in_a_section_the_name_list_does_not_name_is_refused() {
    for size in [0, 10] {
        let case = with_extra(Form::Chm, vec![raw("/lost.html", 2, 0, size)]);
        case.refused("names section 2");
        case.sevenzip_refuses_too();
    }
}

#[test]
fn unsafe_names_are_refused_with_nothing_published() {
    for name in [
        "/../escape.html",
        "//absolute.html",
        "/a/../../b.html",
        "/c:d.html",
    ] {
        let case = with_extra(Form::Chm, vec![raw(name, 0, 0, 10)]);
        case.refused("unsafe archive path");
    }
}

#[test]
fn a_name_with_a_nul_byte_or_invalid_utf8_is_refused() {
    let nul = with_extra(Form::Chm, vec![raw("/a\0b.html", 0, 0, 10)]);
    nul.refused("holds a NUL byte");
    let mut bad = raw("/x.html", 0, 0, 10);
    bad.name = b"/\xFF\xFE.html".to_vec();
    with_extra(Form::Chm, vec![bad]).refused("is not UTF-8");
}

#[test]
fn a_later_entry_of_the_same_name_replaces_the_earlier_as_7zip_writes_it() {
    let case = with_extra(Form::Chm, vec![raw("/#SYSTEM", 0, 3, 20)]);
    let ours = case.same_as_sevenzip();
    assert_eq!(ours["#SYSTEM"], noise(3, 100)[3..23]);
}

fn patch_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn chunked(form: Form) -> (Vec<u8>, chm_writer::Layout) {
    let mut files = base_files();
    for index in 0..60 {
        files.push(file(
            &format!("/many/page{index:03}.html"),
            &noise(index, 50),
            index % 2 == 0,
        ));
    }
    let mut options = Options::plain(form);
    options.chunk_size = if form == Form::Chm { 512 } else { 1024 };
    options.index_chunk = true;
    chm_writer::write(&files, &options)
}

#[test]
fn looping_chunk_links_change_nothing_because_listings_are_read_in_order() {
    let (mut bytes, layout) = chunked(Form::Chm);
    assert!(layout.chunks.len() > 3);
    for chunk in &layout.chunks[..layout.chunks.len() - 1] {
        patch_u32(&mut bytes, chunk.start + 12, 0);
        patch_u32(&mut bytes, chunk.start + 16, 0);
    }
    let (plain, _) = chunked(Form::Chm);
    let looped = Case::new(&bytes, Form::Chm).same_as_sevenzip();
    assert_eq!(looped, Case::new(&plain, Form::Chm).same_as_sevenzip());
}

#[test]
fn a_directory_that_counts_more_chunks_than_it_holds_is_refused() {
    for form in [Form::Chm, Form::Help2] {
        let (mut bytes, layout) = chunked(form);
        let count_at = layout.directory_header.start + if form == Form::Chm { 0x2C } else { 24 };
        patch_u32(&mut bytes, count_at, layout.chunks.len() as u32 + 1);
        let case = Case::new(&bytes, form);
        case.refused("chunks of");
        case.sevenzip_refuses_too();
        patch_u32(&mut bytes, count_at, u32::MAX);
        Case::new(&bytes, form).refused("chunks of");
    }
}

#[test]
fn a_chunk_size_past_1_mib_is_refused_before_it_is_allocated() {
    let (mut bytes, layout) = chunked(Form::Chm);
    patch_u32(&mut bytes, layout.directory_header.start + 16, u32::MAX);
    Case::new(&bytes, Form::Chm).refused("chunks are not 32 bytes to 1 MiB");
}

#[test]
fn an_unknown_chunk_is_refused_where_7zip_skips_it() {
    let (mut bytes, layout) = chunked(Form::Chm);
    bytes[layout.chunks[1].start] = b'X';
    let case = Case::new(&bytes, Form::Chm);
    case.refused("neither a listing nor an index");
    assert!(
        case.sevenzip().is_none_or(|theirs| theirs.is_some()),
        "7-Zip skips an unknown chunk"
    );
}

#[test]
fn a_listing_whose_entries_cross_its_quickref_area_is_refused() {
    for form in [Form::Chm, Form::Help2] {
        let (mut bytes, layout) = chunked(form);
        let chunk = layout.chunks[0].clone();
        let quickref =
            u32::from_le_bytes(bytes[chunk.start + 4..chunk.start + 8].try_into().unwrap());
        patch_u32(&mut bytes, chunk.start + 4, quickref + 3);
        let case = Case::new(&bytes, form);
        case.refused("runs past its listing");
        case.sevenzip_refuses_too();
        patch_u32(&mut bytes, chunk.start + 4, 1);
        Case::new(&bytes, form).refused("quickref area is under 2 bytes");
        let empty = chunk.end - chunk.start - if form == Form::Chm { 19 } else { 47 };
        patch_u32(&mut bytes, chunk.start + 4, empty as u32);
        bytes[chunk.end - 2..chunk.end].fill(0);
        let case = Case::new(&bytes, form);
        case.refused("overlaps its chunk header");
        case.sevenzip_refuses_too();
    }
}

#[test]
fn an_encoded_integer_of_ten_bytes_is_refused() {
    let (mut bytes, layout) = chunked(Form::Chm);
    let chunk = layout.chunks[0].clone();
    let at = chunk.start + PMGL_HEADER;
    assert_eq!(
        &bytes[at..at + 2],
        b"\x01/",
        "the first entry is the root folder"
    );
    bytes[at + 2..at + 12].fill(0x80);
    let case = Case::new(&bytes, Form::Chm);
    case.refused("longer than nine bytes");
    case.sevenzip_refuses_too();
}

#[test]
fn reset_tables_that_lie_are_refused() {
    let (bytes, layout) = chm_writer::write(&base_files(), &Options::plain(Form::Chm));
    let table = layout
        .system
        .iter()
        .find(|(name, _)| name.ends_with("ResetTable"))
        .unwrap()
        .1
        .clone();
    let cases: [(usize, u64, &str); 5] = [
        (4, u64::from(u32::MAX), "does not match its entry count"),
        (16, 1 << 40, "does not match its uncompressed size"),
        (16, u64::MAX - 10, "uncompressed size overflows"),
        (40, 5, "does not start at offset 0"),
        (48, 1 << 40, "offsets go backwards"),
    ];
    for (offset, value, reason) in cases {
        let mut damaged = bytes.clone();
        let at = table.start + offset;
        let width = if offset == 4 { 4 } else { 8 };
        damaged[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
        let case = Case::new(&damaged, Form::Chm);
        case.refused(reason);
        if !reason.contains("overflows") {
            case.sevenzip_refuses_too();
        }
    }
}

#[test]
fn a_window_past_2_mib_is_refused_when_its_section_holds_data() {
    let files = [file("/a.html", &noise(5, 100), true)];
    let mut bytes = write_with(Form::Chm, &files, |_| {});
    let (_, layout) = chm_writer::write(&files, &Options::plain(Form::Chm));
    let control = layout
        .system
        .iter()
        .find(|(name, _)| name.ends_with("ControlData"))
        .unwrap()
        .1
        .clone();
    patch_u32(&mut bytes, control.start + 16, 128);
    let case = Case::new(&bytes, Form::Chm);
    case.refused("window is not 2^15 to 2^21 bytes");
    case.sevenzip_refuses_too();
}

#[test]
fn a_block_that_packs_more_than_1_mib_is_refused_where_7zip_reads_it() {
    let data = noise(6, 32_768);
    let bytes = write_with(Form::Chm, &[file("/big.html", &data, true)], |options| {
        options.lzx.longest_block = 1;
        options.lzx.kinds = vec![lzx_encoder::Kind::Verbatim];
    });
    assert!(
        bytes.len() > 1 << 20,
        "the block packs {} bytes",
        bytes.len()
    );
    let case = Case::new(&bytes, Form::Chm);
    case.refused("packs more than 1 MiB");
    if let Some(theirs) = case.sevenzip() {
        assert_eq!(theirs.unwrap()["big.html"], data, "7-Zip reads the block");
    }
}

#[test]
fn a_help2_section_with_another_transform_is_refused_as_7zip_refuses_it() {
    for transforms in [
        vec![chm_writer::DES_GUID],
        vec![chm_writer::DES_GUID, chm_writer::HELP2_LZX_GUID],
    ] {
        let bytes = write_with(Form::Help2, &base_files(), |options| {
            options.transforms = Some(transforms.clone())
        });
        let case = Case::new(&bytes, Form::Help2);
        case.refused("which Zipper and 7-Zip do not decode");
        case.sevenzip_refuses_too();
    }
}

#[test]
fn a_help2_directory_that_lists_another_count_than_its_header_is_refused() {
    let bytes = write_with(Form::Help2, &base_files(), |_| {});
    let counted = u64::from_le_bytes(bytes[0xB8..0xC0].try_into().unwrap());
    for (count, reason) in [
        (counted + 1, "fewer entries"),
        (counted - 1, "more entries"),
    ] {
        let mut damaged = bytes.clone();
        damaged[0xB8..0xC0].copy_from_slice(&count.to_le_bytes());
        let case = Case::new(&damaged, Form::Help2);
        case.refused(reason);
        case.sevenzip_refuses_too();
    }
}

#[test]
fn a_help2_file_without_an_itsf_record_goes_to_7zip() {
    let mut bytes = write_with(Form::Help2, &base_files(), |_| {});
    patch_u32(&mut bytes, 0x118, 0x2C);
    let case = Case::new(&bytes, Form::Help2);
    case.refused("no ITSF record");
    assert!(zipper::native_chm::needs_backend(&case.path).unwrap());
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Auto, &case.path).unwrap(),
        zipper::Reader::Backend { format: None }
    );
    assert_eq!(
        zipper::choose_reader(zipper::Engine::Native, &case.path).unwrap(),
        zipper::Reader::Chm
    );
}

#[test]
fn a_missing_name_list_is_refused_as_7zip_refuses_its_raw_entries() {
    let mut bytes = write_with(Form::Chm, &base_files(), |_| {});
    let at = bytes
        .windows(20)
        .position(|window| window == b"::DataSpace/NameList")
        .unwrap();
    bytes[at + 19] = b'x';
    let case = Case::new(&bytes, Form::Chm);
    case.refused("lists only as raw entries");
    case.sevenzip_refuses_too();
}

#[test]
fn an_empty_section_with_an_empty_reset_table_extracts() {
    let files = [
        file("/plain.txt", b"stored", false),
        file("/none.html", b"", true),
    ];
    let (mut bytes, layout) = chm_writer::write(&files, &Options::plain(Form::Chm));
    let table = layout
        .system
        .iter()
        .find(|(name, _)| name.ends_with("ResetTable"))
        .unwrap()
        .1
        .clone();
    let at = bytes
        .windows(10)
        .position(|window| window == b"ResetTable")
        .unwrap();
    let mut size_at = at + "ResetTable".len() + 1;
    while bytes[size_at] & 0x80 != 0 {
        size_at += 1;
    }
    size_at += 1;
    assert_eq!(usize::from(bytes[size_at]), table.len());
    bytes[size_at] = 0;
    let case = Case::new(&bytes, Form::Chm);
    let ours = case.same_as_sevenzip();
    assert_eq!(ours["plain.txt"], b"stored");
}

#[test]
fn a_selected_file_past_a_reset_extracts_alone_byte_for_byte() {
    let files = [
        file("/a.html", &noise(7, 100_000), true),
        file("/sub/b.html", &noise(8, 90_000), true),
        file("/sub/c.html", &noise(9, 10), true),
    ];
    let case = Case::new(
        &write_with(Form::Chm, &files, |options| options.interval_bits = 0),
        Form::Chm,
    );
    let into = case.out("selected");
    let paths = [String::from("sub/b.html")];
    read_archive(
        &case.path,
        Some(&into),
        ReadOptions {
            paths: &paths,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    let ours = tree(&into);
    assert_eq!(ours.len(), 2);
    assert_eq!(ours["sub/b.html"], noise(8, 90_000));
    let stripped = case.out("stripped");
    read_archive(
        &case.path,
        Some(&stripped),
        ReadOptions {
            strip: zipper::Strip::Prefix("sub"),
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(tree(&stripped)["c.html"], noise(9, 10));
    common::make_tree_deletable(case.path.parent().unwrap());
}

#[test]
#[ignore = "writes CHM files that fill the entry and name limits, for a peak memory measurement"]
fn write_chm_at_the_documented_limits() {
    let out = std::env::var_os("CHM_LIMITS_OUT").expect("set CHM_LIMITS_OUT to a folder");
    let out = Path::new(&out);
    for (name, size) in [("names.chm", 0usize), ("data.chm", 8)] {
        let mut files = Vec::with_capacity(998_000);
        for folder in 0..1_000 {
            for number in 0..998 {
                let data = (folder * 998 + number) as u64;
                files.push(file(
                    &format!("/{folder:03}/{number:08}-{}.html", "n".repeat(48)),
                    &data.to_le_bytes()[..size],
                    true,
                ));
            }
        }
        let (bytes, _) = chm_writer::write(&files, &Options::plain(Form::Chm));
        fs::write(out.join(name), bytes).unwrap();
    }
}
