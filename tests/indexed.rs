use std::{fs, path::Path};

const BLOCK: usize = 1024 * 1024;
fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x12345678u32;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

#[test]
fn packed_files_cross_blocks_and_select_subtrees() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir_all(input.join("sub/empty"))?;
    let data = noise(BLOCK * 2 + 91);
    fs::write(input.join("first"), b"prefix")?;
    fs::write(input.join("sub/large"), &data)?;
    fs::write(input.join("sub/zero"), [])?;
    fs::write(input.join("tail"), b"suffix")?;
    let archive = temp.path().join("a.zipper");
    zipper::create(&archive, std::slice::from_ref(&input), 1)?;
    let entries = zipper::list_archive(&archive)?;
    assert_eq!(entries.len(), 7);
    let out = temp.path().join("all");
    zipper::read_archive(&archive, Some(&out))?;
    assert_eq!(fs::read(out.join("input/sub/large"))?, data);
    assert_eq!(fs::read(out.join("input/tail"))?, b"suffix");
    let out = temp.path().join("selected");
    let selected = zipper::extract_selected(&archive, &out, &["input/sub".into()])?;
    assert_eq!(selected.len(), 4);
    assert_eq!(fs::read(out.join("input/sub/large"))?, data);
    assert!(out.join("input/sub/empty").is_dir());
    assert!(!out.join("input/first").exists());
    assert!(!out.join("input/tail").exists());
    let missing = temp.path().join("missing");
    assert!(zipper::extract_selected(&archive, &missing, &["input/nope".into()]).is_err());
    assert!(!missing.exists());
    Ok(())
}

#[test]
fn index_listing_and_selection_skip_unrelated_corrupt_payload() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let input = t.path().join("input");
    fs::create_dir(&input)?;
    let data = noise(BLOCK);
    fs::write(input.join("a"), &data)?;
    fs::write(input.join("b"), &data)?;
    fs::write(input.join("c"), &data)?;
    let archive = t.path().join("a.zipper");
    zipper::create(&archive, &[input], 1)?;
    let mut bytes = fs::read(&archive)?;
    bytes[16] ^= 0x80;
    fs::write(&archive, bytes)?;
    assert_eq!(zipper::list_archive(&archive)?.len(), 4);
    assert!(zipper::read_archive(&archive, Some(&t.path().join("bad"))).is_err());
    assert!(!t.path().join("bad").exists());
    zipper::extract_selected(&archive, &t.path().join("good"), &["input/c".into()])?;
    assert_eq!(fs::read(t.path().join("good/input/c"))?, data);
    assert!(!t.path().join("good/input/a").exists());
    Ok(())
}

#[test]
fn small_file_packing_and_thread_count_determinism() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let input = t.path().join("input");
    fs::create_dir(&input)?;
    for n in 0..500 {
        fs::write(
            input.join(format!("file-{n:04}")),
            format!("id={n}\n{}", "shared configuration content\n".repeat(100)),
        )?;
    }
    let single = t.path().join("single.zipper");
    let parallel = t.path().join("parallel.zipper");
    for (threads, path) in [(1, &single), (4, &parallel)] {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()?
            .install(|| zipper::create(path, std::slice::from_ref(&input), 1))?;
    }
    assert_eq!(fs::read(&single)?, fs::read(&parallel)?);
    // 1.4 MiB of strongly related files should share history across boundaries.
    assert!(fs::metadata(&single)?.len() < 25_000);
    zipper::read_archive(&single, Some(&t.path().join("out")))?;
    for n in 0..500 {
        let name = format!("file-{n:04}");
        assert_eq!(
            fs::read(input.join(&name))?,
            fs::read(t.path().join("out/input").join(name))?
        );
    }
    Ok(())
}

fn write_v1(path: &Path) -> anyhow::Result<()> {
    let mut bytes = b"ZIPPER\0\x01".to_vec();
    bytes.push(2);
    bytes.extend(4u32.to_le_bytes());
    bytes.extend(b"file");
    bytes.extend(5u64.to_le_bytes());
    bytes.push(0);
    bytes.extend(5u32.to_le_bytes());
    bytes.extend(5u32.to_le_bytes());
    bytes.extend(crc32fast::hash(b"hello").to_le_bytes());
    bytes.extend(b"hello");
    bytes.push(0);
    fs::write(path, bytes)?;
    Ok(())
}

#[test]
fn legacy_v1_and_empty_v2_remain_readable() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let legacy = t.path().join("old.zipper");
    write_v1(&legacy)?;
    assert_eq!(zipper::list_archive(&legacy)?[0].name, "file");
    zipper::read_archive(&legacy, Some(&t.path().join("old")))?;
    assert_eq!(fs::read(t.path().join("old/file"))?, b"hello");
    assert!(
        zipper::extract_selected(&legacy, &t.path().join("selected"), &["file".into()]).is_err()
    );
    let input = t.path().join("empty");
    fs::create_dir(&input)?;
    let archive = t.path().join("empty.zipper");
    zipper::create(&archive, &[input], 1)?;
    zipper::read_archive(&archive, Some(&t.path().join("new")))?;
    assert!(t.path().join("new/empty").is_dir());
    Ok(())
}
