//! Progress counts only grow and reach the totals, ZIP knows its totals and
//! tar does not, and cancelling at any point returns `Cancelled` and leaves
//! the destination unchanged.
use std::{
    fs,
    io::Write,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use zipper::{Cancelled, Control, Existing, Progress, native_tar, native_zip};

const FILES: usize = 40;

fn data(index: usize) -> Vec<u8> {
    (0..index * 9_000 + 1)
        .map(|byte| (byte * 31 + index) as u8)
        .collect()
}

fn write_zip(path: &Path) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    writer
        .add_directory("dir/", zip::write::SimpleFileOptions::default())
        .unwrap();
    for index in 0..FILES {
        writer
            .start_file(
                format!("dir/{index}"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(&data(index)).unwrap();
    }
    writer.finish().unwrap();
}

fn ustar(name: &str, kind: u8, size: usize) -> Vec<u8> {
    let mut header = vec![0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    header[100..108].copy_from_slice(b"0000755\0");
    header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    header[136..148].copy_from_slice(b"00000000000\0");
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[148..156].fill(b' ');
    let sum: u32 = header.iter().map(|&byte| u32::from(byte)).sum();
    header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    header
}

fn write_tar(path: &Path) {
    let mut bytes = ustar("dir/", b'5', 0);
    for index in 0..FILES {
        let data = data(index);
        bytes.extend(ustar(&format!("dir/{index}"), b'0', data.len()));
        bytes.extend_from_slice(&data);
        bytes.resize(bytes.len().div_ceil(512) * 512, 0);
    }
    bytes.extend_from_slice(&[0; 1024]);
    fs::write(path, bytes).unwrap();
}

fn total_bytes() -> u64 {
    (0..FILES).map(|index| data(index).len() as u64).sum()
}

type Extract = fn(&Path, Option<&Path>, Existing, Control) -> anyhow::Result<Vec<zipper::Entry>>;

fn zip_reader(
    archive: &Path,
    destination: Option<&Path>,
    existing: Existing,
    control: Control,
) -> anyhow::Result<Vec<zipper::Entry>> {
    native_zip::read_archive_with_options(
        archive,
        destination,
        native_zip::ReadOptions {
            existing,
            control,
            ..Default::default()
        },
    )
}

fn tar_reader(
    archive: &Path,
    destination: Option<&Path>,
    existing: Existing,
    control: Control,
) -> anyhow::Result<Vec<zipper::Entry>> {
    native_tar::read_archive(
        archive,
        destination,
        native_tar::ReadOptions {
            existing,
            control,
            ..Default::default()
        },
    )
}

type Format = (&'static str, fn(&Path), Extract);

const FORMATS: [Format; 2] = [
    ("zip", write_zip, zip_reader),
    ("tar", write_tar, tar_reader),
];

#[test]
fn progress_grows_to_the_totals() {
    for (format, write, extract) in FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{format}"));
        write(&archive);
        for destination in [None, Some(root.path().join("out"))] {
            let reports = Mutex::new(Vec::<Progress>::new());
            let record = |progress: Progress| reports.lock().unwrap().push(progress);
            let control = Control {
                progress: Some(&record),
                cancel: None,
            };
            extract(&archive, destination.as_deref(), Existing::Refuse, control).unwrap();
            let reports = reports.into_inner().unwrap();
            assert!(
                reports
                    .windows(2)
                    .all(|pair| pair[0].files_done <= pair[1].files_done
                        && pair[0].bytes_done <= pair[1].bytes_done),
                "{format}: counts went down"
            );
            let last = *reports.last().unwrap();
            assert_eq!(last.files_done, FILES as u64, "{format}");
            assert_eq!(last.bytes_done, total_bytes(), "{format}");
            let totals = (format == "zip").then_some((FILES as u64, total_bytes()));
            assert_eq!(last.files_total.zip(last.bytes_total), totals, "{format}");
        }
    }
}

fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .map(|entry| entry.unwrap())
        .map(|entry| {
            let content = if entry.file_type().is_file() {
                fs::read(entry.path()).unwrap()
            } else {
                Vec::new()
            };
            (entry.path().display().to_string(), content)
        })
        .collect();
    files.sort();
    files
}

#[test]
fn cancelling_before_or_during_extraction_changes_nothing() {
    for (format, write, extract) in FORMATS {
        for cancel_after_reports in [0, 1, 25] {
            let root = tempfile::tempdir().unwrap();
            let archive = root.path().join(format!("a.{format}"));
            write(&archive);
            let destination = root.path().join("out");
            fs::create_dir_all(destination.join("dir")).unwrap();
            fs::write(destination.join("dir/0"), b"old").unwrap();
            let before = snapshot(&destination);
            let cancel = AtomicBool::new(cancel_after_reports == 0);
            let reports = Mutex::new(0);
            let count = |_: Progress| {
                let mut reports = reports.lock().unwrap();
                *reports += 1;
                if *reports >= cancel_after_reports {
                    cancel.store(true, Ordering::Relaxed);
                }
            };
            let control = Control {
                progress: Some(&count),
                cancel: Some(&cancel),
            };
            let error = extract(&archive, Some(&destination), Existing::Overwrite, control)
                .expect_err("cancelled extraction succeeded");
            assert!(
                error.is::<Cancelled>(),
                "{format} after {cancel_after_reports}: {error:#}"
            );
            assert_eq!(
                snapshot(&destination),
                before,
                "{format} after {cancel_after_reports}"
            );
        }
    }
}

#[test]
fn cancelling_a_new_destination_leaves_nothing() {
    for (format, write, extract) in FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{format}"));
        write(&archive);
        let cancel = AtomicBool::new(true);
        let control = Control {
            progress: None,
            cancel: Some(&cancel),
        };
        let destination = root.path().join("out");
        let error = extract(&archive, Some(&destination), Existing::Refuse, control).unwrap_err();
        assert!(error.is::<Cancelled>(), "{format}: {error:#}");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1, "{format}");
        assert!(
            extract(&archive, None, Existing::Refuse, control)
                .unwrap_err()
                .is::<Cancelled>()
        );
    }
}

#[test]
fn a_panicking_callback_reaches_the_caller_without_hanging() {
    for (format, write, extract) in FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{format}"));
        write(&archive);
        let destination = root.path().join("out");
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let explode = |_: Progress| panic!("callback failed");
            let control = Control {
                progress: Some(&explode),
                cancel: None,
            };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                extract(&archive, Some(&destination), Existing::Refuse, control)
            }));
            let _ = done.send((outcome.is_err(), destination.exists()));
        });
        let (panicked, published) = finished
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{format}: extraction hung after the callback panicked"));
        assert!(panicked, "{format}");
        assert!(!published, "{format}");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1, "{format}");
    }
}

#[cfg(unix)]
#[test]
fn zip_symlinks_count_toward_the_byte_total() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
    writer
        .start_file("target", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"twelve bytes").unwrap();
    writer
        .add_symlink("link", "target", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.finish().unwrap();
    let last = Mutex::new(Progress::default());
    let record = |progress: Progress| *last.lock().unwrap() = progress;
    let control = Control {
        progress: Some(&record),
        cancel: None,
    };
    zip_reader(
        &archive,
        Some(&root.path().join("out")),
        Existing::Refuse,
        control,
    )
    .unwrap();
    let last = last.into_inner().unwrap();
    assert_eq!(last.files_done, 2);
    assert_eq!(Some(last.bytes_done), last.bytes_total);
}
