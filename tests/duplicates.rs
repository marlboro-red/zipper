use anyhow::Result;
use std::{fs, process::Command};

#[test]
fn real_crc_collision_does_not_merge_different_contents() -> Result<()> {
    let root = tempfile::tempdir()?;
    let a = [0x41, 0x41, 0x41, 0x41, 0, 0, 0, 0];
    let b = [0x42, 0x42, 0x42, 0x42, 0xce, 0x13, 0xd7, 0xa2];
    assert_eq!(crc32fast::hash(&a), crc32fast::hash(&b));
    for (name, bytes) in [("a", a), ("a-copy", a), ("b", b), ("b-copy", b)] {
        fs::write(root.path().join(name), bytes)?;
    }
    let report = zipper::duplicates::find(&[root.path().to_owned()])?;
    assert_eq!(report.groups.len(), 2);
    assert_eq!(report.duplicate_bytes, 16);
    for group in report.groups {
        assert_eq!(group.paths.len(), 2);
        assert_eq!(fs::read(&group.paths[0])?, fs::read(&group.paths[1])?);
    }
    Ok(())
}

#[test]
fn duplicates_are_exact_deterministic_and_overlap_safe() -> Result<()> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    fs::create_dir_all(data.join("nested"))?;
    fs::write(data.join("a"), b"same bytes")?;
    fs::write(data.join("nested/b"), b"same bytes")?;
    fs::write(data.join("different"), b"other data")?;
    fs::write(data.join("empty1"), b"")?;
    fs::write(data.join("empty2"), b"")?;
    let mut large = vec![b'x'; 128 * 1024 + 1];
    fs::write(data.join("large1"), &large)?;
    fs::write(data.join("large2"), &large)?;
    large[128 * 1024] = b'y';
    fs::write(data.join("same-prefix-different-tail"), &large)?;
    let inputs = vec![data.clone(), data.join("a"), data.join("nested")];
    let report = zipper::duplicates::find(&inputs)?;
    assert_eq!(report.scanned_files, 8);
    assert_eq!(report.groups.len(), 3);
    assert_eq!(report.duplicate_bytes, 10 + 128 * 1024 + 1);
    for group in &report.groups {
        assert_eq!(group.paths.len(), 2);
        let original = fs::read(&group.paths[0])?;
        for path in &group.paths[1..] {
            assert_eq!(fs::read(path)?, original);
        }
    }
    for workers in [1, 4] {
        let result = Command::new(env!("CARGO_BIN_EXE_zipper"))
            .args(["--threads", &workers.to_string(), "duplicates", "--summary"])
            .arg(&data)
            .output()?;
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            String::from_utf8(result.stdout)?,
            "Scanned 8 files; 3 duplicate groups; 131083 duplicate payload bytes; 0 non-regular entries skipped\n"
        );
    }
    // The command never changes data, including nonmatching files.
    assert_eq!(fs::read(data.join("same-prefix-different-tail"))?, large);
    assert!(zipper::duplicates::find(&[data.join("missing")]).is_err());
    assert!(zipper::duplicates::find(&[]).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinks_are_skipped_and_hardlinks_are_logical_duplicates() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("a");
    fs::write(&source, b"payload")?;
    fs::hard_link(&source, root.path().join("hardlink"))?;
    std::os::unix::fs::symlink(&source, root.path().join("symlink"))?;
    std::os::unix::fs::symlink(root.path(), root.path().join("loop"))?;
    let result = zipper::duplicates::find(&[root.path().to_owned()])?;
    assert_eq!(result.scanned_files, 2);
    assert_eq!(result.skipped_entries, 2);
    assert_eq!(result.groups.len(), 1);
    assert_eq!(result.duplicate_bytes, 7); // Logical bytes, not disk savings.
    let result = zipper::duplicates::find(&[root.path().join("symlink")])?;
    assert_eq!(result.scanned_files, 0);
    assert_eq!(result.skipped_entries, 1);
    Ok(())
}
