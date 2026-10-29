use std::fs;
use zipper::{CreateOptions, create_with_options, extract_selected, read_archive};

#[test]
fn compressed_blocks_at_size_extremes_roundtrip() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("data");
    for chunk in [64 * 1024, 16 * 1024 * 1024] {
        let bytes: Vec<_> = (0..chunk + 97).map(|i| (i % 251) as u8).collect();
        fs::write(&input, &bytes)?;
        let archive = temp.path().join(format!("{chunk}.zipper"));
        create_with_options(
            &archive,
            std::slice::from_ref(&input),
            CreateOptions {
                level: 1,
                block_size: chunk,
            },
        )?;
        assert!(fs::metadata(&archive)?.len() < (bytes.len() / 10) as u64);
        let out = temp.path().join(format!("full-{chunk}"));
        read_archive(&archive, Some(&out))?;
        assert_eq!(fs::read(out.join("data"))?, bytes);
        let selected = temp.path().join(format!("selected-{chunk}"));
        extract_selected(&archive, &selected, &["data".into()])?;
        assert_eq!(fs::read(selected.join("data"))?, bytes);
    }
    Ok(())
}

#[test]
fn every_block_size_roundtrips_and_selects_across_boundaries() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::create_dir(&input)?;
    for exponent in 16..=24 {
        let chunk = 1usize << exponent;
        let mut state = 0x12345678u32;
        let bytes: Vec<u8> = (0..chunk + 97)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        fs::write(input.join("first"), b"prefix")?;
        fs::write(input.join("large"), &bytes)?;
        fs::write(input.join("tail"), b"suffix")?;
        fs::write(input.join("zero"), [])?;
        let archive = temp.path().join(format!("{exponent}.zipper"));
        create_with_options(
            &archive,
            std::slice::from_ref(&input),
            CreateOptions {
                level: 1,
                block_size: chunk,
            },
        )?;
        let header = fs::read(&archive)?;
        assert_eq!(u32::from_le_bytes(header[8..12].try_into()?), chunk as u32);
        let out = temp.path().join(format!("out-{exponent}"));
        read_archive(&archive, Some(&out))?;
        assert_eq!(fs::read(out.join("input/large"))?, bytes);
        assert_eq!(fs::read(out.join("input/first"))?, b"prefix");
        assert_eq!(fs::read(out.join("input/tail"))?, b"suffix");
        assert_eq!(fs::read(out.join("input/zero"))?, b"");
        let selected = temp.path().join(format!("selected-{exponent}"));
        extract_selected(&archive, &selected, &["input/large".into()])?;
        assert_eq!(fs::read(selected.join("input/large"))?, bytes);
        assert!(!selected.join("input/first").exists());
        assert!(!selected.join("input/tail").exists());
    }
    Ok(())
}

#[test]
fn invalid_options_do_not_publish_and_defaults_preserve_output() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input");
    fs::write(&input, b"same payload")?;
    let archive = temp.path().join("a.zipper");
    for block_size in [0, 65535, 98304, 32 * 1024 * 1024, usize::MAX] {
        assert!(
            create_with_options(
                &archive,
                std::slice::from_ref(&input),
                CreateOptions {
                    block_size,
                    ..CreateOptions::default()
                }
            )
            .is_err()
        );
        assert!(!archive.exists());
    }
    for level in [-8, 23] {
        assert!(
            create_with_options(
                &archive,
                std::slice::from_ref(&input),
                CreateOptions {
                    level,
                    ..CreateOptions::default()
                }
            )
            .is_err()
        );
        assert!(!archive.exists());
    }
    zipper::create(&archive, std::slice::from_ref(&input), 1)?;
    let other = temp.path().join("b.zipper");
    create_with_options(&other, &[input], CreateOptions::default())?;
    assert_eq!(fs::read(archive)?, fs::read(other)?);
    Ok(())
}
