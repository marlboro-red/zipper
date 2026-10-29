use std::fs;

#[test]
fn roundtrip_multiple_batches_and_empty_entries() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let input = t.path().join("input");
    fs::create_dir_all(input.join("empty"))?;
    fs::write(input.join("zero"), [])?;
    let data: Vec<u8> = (0..10 * 1024 * 1024 + 17)
        .map(|i| (i % 251) as u8)
        .collect();
    fs::write(input.join("data"), &data)?;
    let archive = t.path().join("test.zipper");
    zipper::create(&archive, &[input], 1)?;
    let out = t.path().join("out");
    let entries = zipper::read_archive(&archive, Some(&out))?;
    assert_eq!(entries.len(), 4);
    assert_eq!(fs::read(out.join("input/data"))?, data);
    assert!(out.join("input/empty").is_dir());
    assert!(zipper::read_archive(&archive, Some(&out)).is_err());
    Ok(())
}

#[test]
fn reject_truncation_corruption_and_traversal() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let input = t.path().join("file");
    fs::write(&input, b"hello")?;
    let archive = t.path().join("test.zipper");
    zipper::create(&archive, &[input], 1)?;
    let original = fs::read(&archive)?;
    for len in 0..original.len() {
        fs::write(&archive, &original[..len])?;
        assert!(
            zipper::read_archive(&archive, None).is_err(),
            "accepted truncation {len}"
        );
    }
    let mut corrupt = original.clone();
    let index = corrupt.len() - 2;
    corrupt[index] ^= 0xff;
    fs::write(&archive, corrupt)?;
    let out = t.path().join("out");
    assert!(zipper::read_archive(&archive, Some(&out)).is_err());
    assert!(!out.exists());
    let mut traversal = original;
    traversal[13..17].copy_from_slice(b"../x");
    fs::write(&archive, traversal)?;
    assert!(zipper::read_archive(&archive, Some(&out)).is_err());
    Ok(())
}
