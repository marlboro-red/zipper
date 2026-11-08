use anyhow::{Context, Result, ensure};
use std::{collections::HashSet, fs::File, io::Read, path::Path};

pub(crate) const WRITES: bool = cfg!(target_os = "macos");
// EXTERNAL(macOS sys/xattr.h): XATTR_MAXNAMELEN is 127 bytes.
pub(crate) const MAX_NAME: usize = 127;
pub(crate) const MAX_PER_ENTRY: usize = 128;
pub(crate) const MAX_VALUE: u64 = 64 << 20;
const READ_PIECE: usize = 1 << 20;
const NEVER_RESTORED: [&str; 5] = [
    "com.apple.quarantine",
    "com.apple.provenance",
    "com.apple.macl",
    "com.apple.rootless",
    "com.apple.root.installed",
];
const SYSTEM_PREFIX: &str = "com.apple.system.";

pub(crate) fn is_restored(name: &str) -> bool {
    !NEVER_RESTORED.contains(&name) && !name.starts_with(SYSTEM_PREFIX)
}

pub(crate) fn checked_name(name: &[u8]) -> Result<&str> {
    ensure!(!name.is_empty(), "an extended attribute has an empty name");
    ensure!(
        !name.contains(&0),
        "an extended attribute's name holds a NUL"
    );
    let name = std::str::from_utf8(name).context("an extended attribute's name is not UTF-8")?;
    ensure!(
        name.len() <= MAX_NAME,
        "extended attribute {name:?} has a name longer than {MAX_NAME} bytes"
    );
    Ok(name)
}

pub(crate) fn check_size(name: &str, size: u64) -> Result<()> {
    ensure!(
        size <= MAX_VALUE,
        "extended attribute {name:?} holds {size} bytes, more than {MAX_VALUE}"
    );
    Ok(())
}

#[derive(Default)]
pub(crate) struct EntryNames {
    names: HashSet<Box<[u8]>>,
}

impl EntryNames {
    pub(crate) fn clear(&mut self) {
        self.names.clear();
    }

    pub(crate) fn add<'n>(&mut self, name: &'n [u8]) -> Result<&'n str> {
        let checked = checked_name(name)?;
        ensure!(
            self.names.len() < MAX_PER_ENTRY,
            "an entry has more than {MAX_PER_ENTRY} extended attributes"
        );
        ensure!(
            self.names.insert(name.into()),
            "an entry has two extended attributes named {checked:?}"
        );
        Ok(checked)
    }
}

pub(crate) fn read_value(mut data: impl Read, size: u64, name: &str) -> Result<Vec<u8>> {
    check_size(name, size)?;
    let mut value = Vec::new();
    let mut left = size;
    while left > 0 {
        let piece = left.min(READ_PIECE as u64) as usize;
        let start = value.len();
        value.resize(start + piece, 0);
        data.read_exact(&mut value[start..])
            .with_context(|| format!("cannot read extended attribute {name:?}"))?;
        left -= piece as u64;
    }
    Ok(value)
}

pub(crate) fn set_on_file(file: &File, name: &str, value: &[u8], what: &str) -> Result<()> {
    written(platform::on_file(file, name, value), name, what)
}

pub(crate) fn set_on_path(path: &Path, name: &str, value: &[u8], what: &str) -> Result<()> {
    written(platform::on_path(path, name, value), name, what)
}

pub(crate) fn make_writable(file: &File) -> Result<Option<std::fs::Permissions>> {
    platform::make_writable(file)
}

pub(crate) fn kept_natively_in(folder: &Path) -> Result<bool> {
    platform::kept_natively_in(folder)
        .with_context(|| format!("cannot probe extended attributes in {}", folder.display()))
}

fn written(result: std::io::Result<()>, name: &str, what: &str) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if platform::unsupported(&error) => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("cannot set extended attribute {name:?} on {what}"))
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use rustix::fs::XattrFlags;
    use rustix::io::Errno;
    use std::{fs::File, os::unix::fs::PermissionsExt, path::Path};

    const OWNER_WRITE: u32 = 0o200;

    pub(super) fn on_file(file: &File, name: &str, value: &[u8]) -> std::io::Result<()> {
        Ok(rustix::fs::fsetxattr(
            file,
            name,
            value,
            XattrFlags::empty(),
        )?)
    }

    pub(super) fn on_path(path: &Path, name: &str, value: &[u8]) -> std::io::Result<()> {
        Ok(rustix::fs::lsetxattr(
            path,
            name,
            value,
            XattrFlags::empty(),
        )?)
    }

    pub(super) fn unsupported(error: &std::io::Error) -> bool {
        let code = Errno::from_io_error(error);
        code == Some(Errno::NOTSUP) || code == Some(Errno::OPNOTSUPP)
    }

    pub(super) fn make_writable(file: &File) -> anyhow::Result<Option<std::fs::Permissions>> {
        let permissions = file.metadata()?.permissions();
        if permissions.mode() & OWNER_WRITE != 0 {
            return Ok(None);
        }
        file.set_permissions(std::fs::Permissions::from_mode(
            permissions.mode() | OWNER_WRITE,
        ))?;
        Ok(Some(permissions))
    }

    // EXTERNAL(macOS vfs_xattr.c): a file system without native attributes keeps them in an
    // AppleDouble file `._name` beside the file, which could replace an archive entry.
    pub(super) fn kept_natively_in(folder: &Path) -> std::io::Result<bool> {
        let probe = tempfile::Builder::new()
            .prefix(".zipper-xattr-")
            .tempfile_in(folder)?;
        let name = probe.path().file_name().unwrap().to_string_lossy();
        let double = folder.join(format!("._{name}"));
        if std::fs::symlink_metadata(&double).is_ok() {
            return Ok(false);
        }
        match on_file(probe.as_file(), PROBE, b"") {
            Err(error) if unsupported(&error) => return Ok(false),
            result => result?,
        }
        let emulated = std::fs::symlink_metadata(&double).is_ok();
        drop(probe);
        if emulated {
            let _ = std::fs::remove_file(&double);
        }
        Ok(!emulated)
    }

    const PROBE: &str = "org.zipper.probe";
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::{fs::File, path::Path};

    pub(super) fn on_file(_: &File, _: &str, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    pub(super) fn on_path(_: &Path, _: &str, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    pub(super) fn unsupported(_: &std::io::Error) -> bool {
        false
    }

    pub(super) fn make_writable(_: &File) -> anyhow::Result<Option<std::fs::Permissions>> {
        Ok(None)
    }

    pub(super) fn kept_natively_in(_: &Path) -> std::io::Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_the_listed_names_and_the_system_namespace_are_never_restored() {
        for name in NEVER_RESTORED {
            assert!(!is_restored(name), "{name}");
        }
        for name in ["com.apple.system.Security", "com.apple.system.x"] {
            assert!(!is_restored(name), "{name}");
        }
        for name in [
            "user.note",
            "com.apple.FinderInfo",
            "com.apple.metadata:kMDItemWhereFroms",
            "com.apple.quarantine2",
            "com.apple.systemx",
            "com.apple.provenance.x",
            "com.apple.root.installedx",
        ] {
            assert!(is_restored(name), "{name}");
        }
    }

    #[test]
    fn names_are_refused_when_empty_with_a_nul_not_utf8_or_past_127_bytes() {
        assert!(checked_name(b"").is_err());
        assert!(checked_name(b"a\0b").is_err());
        assert!(checked_name(b"\xFF").is_err());
        assert!(checked_name(&[b'a'; 128]).is_err());
        assert_eq!(checked_name(&[b'a'; 127]).unwrap().len(), 127);
        let accent = "é".repeat(63);
        assert_eq!(checked_name(accent.as_bytes()).unwrap(), accent);
        assert!(checked_name("é".repeat(64).as_bytes()).is_err());
    }

    #[test]
    fn an_entry_takes_at_most_its_count_and_no_name_twice() {
        let mut names = EntryNames::default();
        for number in 0..MAX_PER_ENTRY {
            names.add(format!("user.{number}").as_bytes()).unwrap();
        }
        assert!(names.add(b"user.one-more").is_err());
        names.clear();
        names.add(b"user.a").unwrap();
        assert!(names.add(b"user.a").is_err());
    }

    #[test]
    fn values_read_in_pieces_and_stop_at_their_cap() {
        let source: Vec<u8> = (0..3 * READ_PIECE + 7).map(|n| n as u8).collect();
        assert_eq!(
            read_value(&source[..], source.len() as u64, "user.a").unwrap(),
            source
        );
        assert!(read_value(&source[..10], 11, "user.a").is_err());
        assert!(read_value(std::io::repeat(0), MAX_VALUE + 1, "user.a").is_err());
    }
}
