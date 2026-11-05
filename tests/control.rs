//! Progress counts only grow and reach the totals, ZIP knows its totals and
//! tar does not, and cancelling at any point returns `Cancelled` and leaves
//! the destination unchanged.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod common;

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

#[test]
fn a_callback_that_runs_rayon_work_does_not_deadlock() {
    use rayon::prelude::*;
    for (format, write, extract) in FORMATS {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join(format!("a.{format}"));
        if format == "zip" {
            let mut writer = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            for index in 0..400 {
                writer
                    .start_file(
                        format!("f{index}"),
                        zip::write::SimpleFileOptions::default(),
                    )
                    .unwrap();
                writer.write_all(&data(index % 7)).unwrap();
            }
            writer.finish().unwrap();
        } else {
            write(&archive);
        }
        let destination = root.path().join("out");
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let busy = |_: Progress| {
                let _: u64 = (0..200_000u32)
                    .into_par_iter()
                    .with_max_len(1)
                    .map(|value| u64::from(value).wrapping_mul(3))
                    .sum();
            };
            let control = Control {
                progress: Some(&busy),
                cancel: None,
            };
            let _ =
                done.send(extract(&archive, Some(&destination), Existing::Refuse, control).is_ok());
        });
        let succeeded = finished
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("{format}: extraction deadlocked in the callback"));
        assert!(succeeded, "{format}");
    }
}

fn staged_bytes(parent: &Path) -> u64 {
    fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".zipper-"))
        .flat_map(|staging| walkdir::WalkDir::new(staging.path()).into_iter().flatten())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| fs::metadata(entry.path()).map_or(0, |metadata| metadata.len()))
        .sum()
}

#[test]
fn reported_bytes_never_run_ahead_of_bytes_on_disk() {
    const SIZE: usize = 48 << 20;
    const THREADS: usize = 4;
    const SLACK: u64 = (1 << 20) + (64 << 10);
    let big: Vec<u8> = (0..SIZE).map(|index| (index / 4096) as u8).collect();
    let root = tempfile::tempdir().unwrap();
    let zip = root.path().join("big.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&zip).unwrap());
    writer
        .start_file("big", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(&big).unwrap();
    writer.finish().unwrap();
    let tar = root.path().join("big.tar");
    let mut bytes = ustar("big", b'0', SIZE);
    bytes.extend_from_slice(&big);
    bytes.resize(bytes.len().next_multiple_of(512) + 1024, 0);
    fs::write(&tar, bytes).unwrap();
    let stored = root.path().join("stored.zip");
    let mut writer = zip::ZipWriter::new(fs::File::create(&stored).unwrap());
    for index in 0..16 {
        writer
            .start_file(
                format!("{index}"),
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(&big[..15 << 20]).unwrap();
    }
    writer.finish().unwrap();
    for (archive, total) in [(zip, SIZE), (tar, SIZE), (stored, 16 * (15 << 20))] {
        let work = tempfile::tempdir().unwrap();
        let ahead = std::sync::Mutex::new(0u64);
        let last = std::sync::Mutex::new(0u64);
        let watch = |progress: Progress| {
            let lead = progress
                .bytes_done
                .saturating_sub(staged_bytes(work.path()));
            let mut ahead = ahead.lock().unwrap();
            *ahead = (*ahead).max(lead);
            *last.lock().unwrap() = progress.bytes_done;
        };
        zipper::extract_archive(
            &archive,
            &work.path().join("out"),
            zipper::ExtractOptions {
                control: Control {
                    progress: Some(&watch),
                    cancel: None,
                },
                threads: Some(THREADS),
                ..Default::default()
            },
        )
        .unwrap();
        let ahead = *ahead.lock().unwrap();
        assert!(
            ahead <= SLACK,
            "{archive:?} reported {ahead} bytes before they were written"
        );
        assert_eq!(*last.lock().unwrap(), total as u64, "{archive:?}");
    }
}

#[test]
fn a_tar_file_cut_short_is_never_counted_as_done() {
    let root = tempfile::tempdir().unwrap();
    let tar = root.path().join("cut.tar");
    let mut bytes = ustar("whole", b'0', 512);
    bytes.extend_from_slice(&[7; 512]);
    bytes.extend(ustar("cut", b'0', 8 << 20));
    bytes.extend(std::iter::repeat_n(9u8, 3 << 20));
    fs::write(&tar, bytes).unwrap();
    let files = std::sync::Mutex::new(0u64);
    let watch = |progress: Progress| {
        let mut files = files.lock().unwrap();
        *files = (*files).max(progress.files_done);
    };
    let result = zipper::extract_archive(
        &tar,
        &root.path().join("out"),
        zipper::ExtractOptions {
            control: Control {
                progress: Some(&watch),
                cancel: None,
            },
            ..Default::default()
        },
    );
    assert!(result.is_err());
    assert!(*files.lock().unwrap() <= 1, "the cut file was counted");
}

fn every_reader(root: &Path) -> Vec<std::path::PathBuf> {
    let zip = root.join("a.zip");
    write_zip(&zip);
    let tar = root.join("a.tar");
    write_tar(&tar);
    let parity = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/parity/archives");
    let deflate64 = root.join("deflate64-small.zip");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/progress/deflate64-small.zip"),
        &deflate64,
    )
    .unwrap();
    let mut archives = vec![zip, tar, deflate64];
    for name in ["7z.7z", "hello.txt.gz"] {
        let copy = root.join(name);
        fs::copy(parity.join(name), &copy).unwrap();
        archives.push(copy);
    }
    archives
}

fn reports(
    archive: &Path,
    destination: &Path,
    during: &(dyn Fn(Progress) + Sync),
) -> Vec<Progress> {
    let seen = Mutex::new(Vec::new());
    let watch = |progress: Progress| {
        during(progress);
        seen.lock().unwrap().push(progress);
    };
    zipper::extract_archive(
        archive,
        destination,
        zipper::ExtractOptions {
            control: Control {
                progress: Some(&watch),
                cancel: None,
            },
            ..Default::default()
        },
    )
    .unwrap_or_else(|error| panic!("{archive:?}: {error:#}"));
    seen.into_inner().unwrap()
}

#[test]
fn the_last_report_says_finishing_with_the_final_counts() {
    let root = tempfile::tempdir().unwrap();
    for archive in every_reader(root.path()) {
        let seen = reports(
            &archive,
            &root.path().join(format!(
                "{}.out",
                archive.file_name().unwrap().to_str().unwrap()
            )),
            &|_| {},
        );
        let first = seen
            .iter()
            .position(|progress| progress.phase == zipper::Phase::Finishing)
            .unwrap_or_else(|| panic!("{archive:?} never reported finishing"));
        assert!(
            seen[..first]
                .iter()
                .all(|progress| progress.phase == zipper::Phase::Extracting)
        );
        assert!(
            seen[first..]
                .iter()
                .all(|progress| *progress == seen[first]),
            "{archive:?}"
        );
        let last = seen[first];
        assert!(
            last.files_total
                .is_none_or(|total| last.files_done == total),
            "{archive:?}"
        );
        assert!(
            last.bytes_total
                .is_none_or(|total| last.bytes_done == total),
            "{archive:?}"
        );
    }
}

#[cfg(target_os = "linux")]
fn open_paths() -> Vec<std::path::PathBuf> {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|fd| fs::read_link(fd.ok()?.path()).ok())
        .collect()
}

#[cfg(target_os = "macos")]
fn open_paths() -> Vec<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let fds: Vec<i32> = fs::read_dir("/dev/fd")
        .unwrap()
        .filter_map(|fd| fd.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    fds.into_iter()
        .filter_map(|fd| {
            let mut path = [0u8; libc::PATH_MAX as usize];
            // SAFETY: F_GETPATH writes at most PATH_MAX bytes, a NUL-terminated path, into `path`.
            let found = unsafe { libc::fcntl(fd, libc::F_GETPATH, path.as_mut_ptr()) };
            let end = path.iter().position(|&byte| byte == 0)?;
            (found == 0).then(|| std::ffi::OsStr::from_bytes(&path[..end]).into())
        })
        .collect()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn counts_reach_their_totals_only_after_every_file_is_closed() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().canonicalize().unwrap();
    for archive in every_reader(&work) {
        let still_open = Mutex::new(Vec::new());
        let entries = zipper::extract_archive(
            &archive,
            &work.join("listed"),
            zipper::ExtractOptions::default(),
        );
        entries.unwrap();
        let listed = walkdir::WalkDir::new(work.join("listed"))
            .into_iter()
            .flatten()
            .filter(|entry| entry.file_type().is_file())
            .count() as u64;
        let total: u64 = walkdir::WalkDir::new(work.join("listed"))
            .into_iter()
            .flatten()
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        common::make_tree_deletable(&work.join("listed"));
        fs::remove_dir_all(work.join("listed")).unwrap();
        let check = |progress: Progress| {
            if progress.files_done == listed || progress.bytes_done == total {
                let staged: Vec<_> = open_paths()
                    .into_iter()
                    .filter(|path| {
                        path.strip_prefix(&work)
                            .is_ok_and(|rest| rest.to_string_lossy().starts_with(".zipper-"))
                    })
                    .collect();
                still_open.lock().unwrap().extend(staged);
            }
        };
        reports(&archive, &work.join("out"), &check);
        common::make_tree_deletable(&work.join("out"));
        fs::remove_dir_all(work.join("out")).unwrap();
        let still_open = still_open.into_inner().unwrap();
        assert!(
            still_open.is_empty(),
            "{archive:?} reported its totals with {still_open:?} open"
        );
    }
}

fn xz_entries_that_each_reserve_most_of_the_budget(path: &Path) {
    const XZ: u16 = 95;
    const DECLARED: u32 = 1_200 << 20;
    let mut payload = Vec::new();
    let mut encoder = liblzma::write::XzEncoder::new(&mut payload, 0);
    encoder.write_all(&vec![0u8; 4 << 20]).unwrap();
    encoder.finish().unwrap();
    let mut out = Vec::new();
    let mut central = Vec::new();
    for name in ["a", "b"] {
        let offset = out.len() as u32;
        let fields = |record: &mut Vec<u8>| {
            record.extend_from_slice(&0u16.to_le_bytes());
            record.extend_from_slice(&XZ.to_le_bytes());
            record.extend_from_slice(&0u32.to_le_bytes());
            record.extend_from_slice(&0u32.to_le_bytes());
            record.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            record.extend_from_slice(&DECLARED.to_le_bytes());
            record.extend_from_slice(&(name.len() as u16).to_le_bytes());
            record.extend_from_slice(&0u16.to_le_bytes());
        };
        out.extend_from_slice(b"PK\x03\x04\x3f\x00");
        fields(&mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&payload);
        central.extend_from_slice(b"PK\x01\x02\x3f\x00\x3f\x00");
        fields(&mut central);
        central.extend_from_slice(&[0; 6]);
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06\x00\x00\x00\x00\x02\x00\x02\x00");
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    fs::write(path, out).unwrap();
}

#[test]
fn a_callback_that_runs_rayon_work_never_waits_on_memory_its_own_thread_holds() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    xz_entries_that_each_reserve_most_of_the_budget(&archive);
    let destination = root.path().join("out");
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let yields = |_: Progress| {
            rayon::yield_local();
        };
        let result = native_zip::read_archive_with_options(
            &archive,
            Some(&destination),
            native_zip::ReadOptions {
                threads: Some(1),
                control: Control {
                    progress: Some(&yields),
                    cancel: None,
                },
                ..Default::default()
            },
        );
        let _ = done.send(result.map(|_| ()).map_err(|error| format!("{error:#}")));
    });
    let result = finished
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the callback ran the second entry, which waits for memory the first holds");
    assert!(
        result.is_err(),
        "the declared sizes are false, so the read must fail"
    );
}

#[test]
fn an_extraction_started_inside_a_callback_on_the_same_thread_finishes() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("a.zip");
    write_zip(&archive);
    let (done, finished) = std::sync::mpsc::channel();
    let paths = (
        archive.clone(),
        root.path().join("outer"),
        root.path().join("inner"),
    );
    std::thread::spawn(move || {
        let (archive, outer, inner) = paths;
        let started = AtomicBool::new(false);
        let inner_result = Mutex::new(None);
        let nested = |_: Progress| {
            if !started.swap(true, Ordering::Relaxed) {
                let read = native_zip::read_archive_with_options(
                    &archive,
                    Some(&inner),
                    Default::default(),
                );
                *inner_result.lock().unwrap() = Some(
                    read.map(|entries| entries.len())
                        .map_err(|error| error.to_string()),
                );
            }
        };
        let read = native_zip::read_archive_with_options(
            &archive,
            Some(&outer),
            native_zip::ReadOptions {
                threads: Some(1),
                control: Control {
                    progress: Some(&nested),
                    cancel: None,
                },
                ..Default::default()
            },
        );
        let inner_read = inner_result.into_inner().unwrap();
        let _ = done.send((
            read.map(|entries| entries.len())
                .map_err(|error| error.to_string()),
            inner_read,
        ));
    });
    let (outer, inner) = finished
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("the inner extraction kept putting off its own entries");
    let entries = outer.unwrap();
    assert_eq!(inner, Some(Ok(entries)));
}
