//! `zipper::extract_archive` sends ZIP, tar and `.zipper` to their readers
//! whatever the file is named, and passes `existing`, `paths` and `control`.
use std::{fs, io::Write, path::Path, sync::atomic::AtomicBool};
use zipper::{Cancelled, Control, Existing, ExtractOptions, Reader, extract_archive};

fn zip(path: &Path) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for name in ["keep/a", "skip/b"] {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(name.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

fn tar(path: &Path) {
    let mut header = vec![0u8; 512];
    header[..6].copy_from_slice(b"keep/a");
    header[100..108].copy_from_slice(b"0000644\0");
    header[124..136].copy_from_slice(b"00000000006\0");
    header[136..148].copy_from_slice(b"00000000000\0");
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    header.extend_from_slice(b"keep/a");
    header.resize(1024 + 1024, 0);
    fs::write(path, header).unwrap();
}

fn native(path: &Path) {
    let input = path.with_extension("input");
    fs::create_dir_all(input.join("keep")).unwrap();
    fs::write(input.join("keep/a"), "keep/a").unwrap();
    zipper::create(path, &[input.join("keep")], 1).unwrap();
}

#[test]
fn each_format_goes_to_its_reader_and_merges() {
    let root = tempfile::tempdir().unwrap();
    for (name, write, reader) in [
        ("a.zip", zip as fn(&Path), Reader::Zip { fast: false }),
        ("a.tar", tar, Reader::Tar),
        ("a.zipper", native, Reader::Zipper),
    ] {
        let archive = root.path().join(name);
        write(&archive);
        assert_eq!(
            zipper::choose_reader(Default::default(), &archive).unwrap(),
            reader
        );
        let destination = root.path().join(format!("{name}.out"));
        fs::create_dir_all(destination.join("keep")).unwrap();
        fs::write(destination.join("keep/a"), "old").unwrap();
        fs::write(destination.join("other"), "other").unwrap();
        extract_archive(
            &archive,
            &destination,
            ExtractOptions {
                existing: Existing::Overwrite,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("keep/a")).unwrap(),
            "keep/a",
            "{name}"
        );
        assert_eq!(
            fs::read_to_string(destination.join("other")).unwrap(),
            "other"
        );
    }
}

#[test]
fn paths_reach_the_zip_reader_and_tar_refuses_them() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    zip(&archive);
    let paths = ["keep".to_string()];
    let options = ExtractOptions {
        paths: &paths,
        ..Default::default()
    };
    let destination = root.path().join("out");
    extract_archive(&archive, &destination, options).unwrap();
    assert!(destination.join("keep/a").exists());
    assert!(!destination.join("skip").exists());
    let tar_archive = root.path().join("a.tar");
    tar(&tar_archive);
    assert!(extract_archive(&tar_archive, &root.path().join("tar"), options).is_err());
}

#[test]
fn a_set_cancel_flag_stops_every_reader() {
    let root = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(true);
    for (name, write) in [
        ("a.zip", zip as fn(&Path)),
        ("a.tar", tar),
        ("a.zipper", native),
    ] {
        let archive = root.path().join(name);
        write(&archive);
        let destination = root.path().join(format!("{name}.out"));
        let error = extract_archive(
            &archive,
            &destination,
            ExtractOptions {
                control: Control {
                    progress: None,
                    cancel: Some(&cancel),
                },
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(error.is::<Cancelled>(), "{name}: {error:#}");
        assert!(!destination.exists(), "{name}");
    }
}

#[test]
fn the_name_does_not_pick_the_reader() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("really-a-zip.tar.gz");
    zip(&archive);
    assert_eq!(
        zipper::choose_reader(Default::default(), &archive).unwrap(),
        Reader::Zip { fast: false }
    );
    let destination = root.path().join("out");
    extract_archive(&archive, &destination, ExtractOptions::default()).unwrap();
    assert!(destination.join("keep/a").exists());
}
