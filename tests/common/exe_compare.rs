#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use zipper::native_exe::{ReadOptions, read_archive};

pub const REFUSALS_STRICTER_THAN_7ZIP: &[&str] = &[
    "unsafe archive path",
    "invalid path length",
    "collides with another entry",
    "two entries of different kinds",
    "is inside a file or link",
    "is also a directory",
    "ELF section name is longer than 4096 bytes",
    "PE resource name is not valid UTF-16",
    "more than 2^22 entries",
    "PE text resources exceed 64 MiB",
    "names exceed 64 MiB",
];

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// EXTERNAL(7-Zip on Linux): a stored U+EF00 plus byte name character is written back as the raw byte.
fn name_as_7zip_stores_it(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut name = String::new();
        for chunk in path.as_os_str().as_bytes().utf8_chunks() {
            name.push_str(chunk.valid());
            name.extend(
                chunk
                    .invalid()
                    .iter()
                    .map(|&byte| char::from_u32(0xEF00 + u32::from(byte)).unwrap()),
            );
        }
        name
    }
    #[cfg(not(unix))]
    path.to_string_lossy().into_owned()
}

#[test]
fn a_raw_byte_in_a_7zip_name_reads_as_the_character_7zip_stores() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"\xffbss");
        assert_eq!(name_as_7zip_stores_it(Path::new(raw)), "\u{efff}bss");
    }
    assert_eq!(name_as_7zip_stores_it(Path::new("caf\u{e9}")), "caf\u{e9}");
}

pub fn tree(root: &Path, metadata: bool) -> BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let name =
                name_as_7zip_stores_it(entry.path().strip_prefix(root).unwrap()).replace('\\', "/");
            let found = fs::symlink_metadata(entry.path()).unwrap();
            let mut kind = if found.is_dir() {
                "dir".to_owned()
            } else {
                sha256(&fs::read(entry.path()).unwrap())
            };
            if metadata {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    kind.push_str(&format!(" {:o}", found.permissions().mode() & 0o777));
                }
                if !found.is_dir() {
                    let seconds = found
                        .modified()
                        .unwrap()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |time| time.as_secs());
                    kind.push_str(&format!(" {seconds}"));
                }
            }
            (name, kind)
        })
        .collect()
}

pub fn sevenzip() -> Option<PathBuf> {
    crate::sevenzip::sevenzip()
}

pub fn sevenzip_tree(
    binary: &Path,
    archive: &Path,
    into: &Path,
    metadata: bool,
) -> Option<BTreeMap<String, String>> {
    let mut flag = std::ffi::OsString::from("-o");
    flag.push(into);
    let run = Command::new(binary)
        .args(["x", "-y"])
        .arg(flag)
        .arg(archive)
        .output()
        .unwrap();
    run.status.success().then(|| tree(into, metadata))
}

pub fn zipper_tree(
    archive: &Path,
    into: &Path,
    metadata: bool,
) -> anyhow::Result<BTreeMap<String, String>> {
    read_archive(archive, Some(into), ReadOptions::default())?;
    Ok(tree(into, metadata))
}

pub fn compare_with_7zip(binary: &Path, label: &str, name: &str, bytes: &[u8]) -> (bool, bool) {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join(name);
    fs::write(&path, bytes).unwrap();
    if let Some(keep) = std::env::var_os("ZIPPER_EXE_KEEP") {
        fs::write(Path::new(&keep).join(name), bytes).unwrap();
    }
    let ours = zipper_tree(&path, &work.path().join("zipper"), true);
    let theirs = sevenzip_tree(binary, &path, &work.path().join("7zip"), true);
    let outcome = match (&ours, &theirs) {
        (Ok(ours), Some(theirs))
            if crate::sevenzip::spelled_through_the_windows_code_page(
                ours.keys().chain(theirs.keys()),
            ) =>
        {
            (true, false)
        }
        (Ok(ours), Some(theirs)) => {
            assert_eq!(ours, theirs, "{label}");
            (true, false)
        }
        (Ok(_), None) => panic!("{label}: Zipper extracts what 7-Zip refuses"),
        (Err(_), None) => {
            assert!(
                !work.path().join("zipper").exists(),
                "{label}: Zipper left output behind"
            );
            (false, false)
        }
        (Err(error), Some(_)) => {
            let message = format!("{error:#}");
            assert!(
                REFUSALS_STRICTER_THAN_7ZIP
                    .iter()
                    .any(|reason| message.contains(reason))
                    || crate::sevenzip::zipper_refused_a_name_windows_forbids(&message),
                "{label}: Zipper refuses what 7-Zip extracts: {message}"
            );
            (false, true)
        }
    };
    crate::common::make_tree_deletable(work.path());
    outcome
}

pub struct Random(pub u64);

impl Random {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit.max(1) as u64) as usize
    }

    pub fn bytes(&mut self, length: usize) -> Vec<u8> {
        (0..length).map(|_| self.next() as u8).collect()
    }
}
