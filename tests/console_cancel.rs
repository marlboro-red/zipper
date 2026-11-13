//! Ctrl-C and Ctrl-Break sent to the Windows command line cancel the
//! extraction as SIGINT does on Unix: the run ends as an interrupted console
//! program does, nothing is published and no staging folder is left. Ctrl-C
//! cannot reach a process group of its own, so its test runs a helper in a
//! console of its own that sends Ctrl-C to every process on it.
#![cfg(windows)]

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::STATUS_CONTROL_C_EXIT,
    System::Console::{
        AllocConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT, GenerateConsoleCtrlEvent,
        SetConsoleCtrlHandler,
    },
};

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const EVERY_PROCESS_ON_THIS_CONSOLE: u32 = 0;
const CTRL_C_HELPER: &str = "ctrl_c_from_a_console_of_its_own";
const IN_A_CONSOLE_OF_ITS_OWN: &str = "ZIPPER_CTRL_C_IN_A_CONSOLE_OF_ITS_OWN";
const FILES: usize = 20_000;
const LONGEST_WAIT: Duration = Duration::from_secs(600);
const POLL: Duration = Duration::from_millis(1);

fn archive_of_many_files(path: &Path) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for index in 0..FILES {
        writer
            .start_file(format!("f{index:05}.txt"), stored)
            .unwrap();
        writer.write_all(index.to_string().as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

fn tree(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    walkdir::WalkDir::new(root)
        .min_depth(1)
        .into_iter()
        .map(|entry| {
            let entry = entry.unwrap();
            let contents = entry
                .file_type()
                .is_file()
                .then(|| fs::read(entry.path()).unwrap());
            (
                entry.path().strip_prefix(root).unwrap().to_owned(),
                contents,
            )
        })
        .collect()
}

fn has_entry_starting_with(folder: &Path, prefix: &str) -> bool {
    fs::read_dir(folder).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
    })
}

fn start_zipper(archive: &Path, output: &Path, overwrite: bool, flags: u32) -> Child {
    // SAFETY: AllocConsole takes no arguments; it fails harmlessly when this process already
    // has a console, and otherwise gives this process and the child one to share.
    unsafe { AllocConsole() };
    let mut command = Command::new(env!("CARGO_BIN_EXE_zipper"));
    command.arg("extract").arg(archive).arg("-o").arg(output);
    if overwrite {
        command.arg("--overwrite");
    }
    command
        .creation_flags(flags)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn send_when(child: &mut Child, ready: impl Fn() -> bool, event: u32, group: u32) {
    let started = Instant::now();
    while !ready() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "zipper ended before the point to cancel at"
        );
        assert!(
            started.elapsed() < LONGEST_WAIT,
            "zipper never reached the point to cancel at"
        );
        std::thread::sleep(POLL);
    }
    // SAFETY: GenerateConsoleCtrlEvent takes plain integers.
    let sent = unsafe { GenerateConsoleCtrlEvent(event, group) };
    assert_ne!(
        sent,
        0,
        "cannot send console event {event}: {}",
        std::io::Error::last_os_error()
    );
}

fn assert_cancelled_and_unchanged(
    child: Child,
    parent: &Path,
    before: &BTreeMap<PathBuf, Option<Vec<u8>>>,
) {
    let ended = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&ended.stderr);
    assert_eq!(
        ended.status.code(),
        Some(STATUS_CONTROL_C_EXIT),
        "zipper did not end as a cancelled run: {stderr}"
    );
    assert_eq!(&tree(parent), before, "zipper changed the destination");
}

struct Merge {
    _work: tempfile::TempDir,
    archive: PathBuf,
    parent: PathBuf,
    output: PathBuf,
    before: BTreeMap<PathBuf, Option<Vec<u8>>>,
}

fn merge_into_a_folder_with_one_file() -> Merge {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("many.zip");
    archive_of_many_files(&archive);
    let parent = work.path().join("parent");
    let output = parent.join("out");
    fs::create_dir_all(&output).unwrap();
    fs::write(output.join("keep.txt"), b"kept\n").unwrap();
    let before = tree(&parent);
    Merge {
        _work: work,
        archive,
        parent,
        output,
        before,
    }
}

#[test]
fn ctrl_break_during_publication_into_a_folder_rolls_every_move_back() {
    let merge = merge_into_a_folder_with_one_file();
    let mut zipper = start_zipper(
        &merge.archive,
        &merge.output,
        true,
        CREATE_NEW_PROCESS_GROUP,
    );
    let ready = || has_entry_starting_with(&merge.output, "f");
    let group = zipper.id();
    send_when(&mut zipper, ready, CTRL_BREAK_EVENT, group);
    assert_cancelled_and_unchanged(zipper, &merge.parent, &merge.before);
}

#[test]
fn ctrl_break_during_extraction_into_a_new_folder_publishes_nothing() {
    let work = tempfile::tempdir().unwrap();
    let archive = work.path().join("many.zip");
    archive_of_many_files(&archive);
    let parent = work.path().join("parent");
    fs::create_dir(&parent).unwrap();
    let output = parent.join("out");
    let mut zipper = start_zipper(&archive, &output, false, CREATE_NEW_PROCESS_GROUP);
    let ready = || has_entry_starting_with(&parent, ".zipper-");
    let group = zipper.id();
    send_when(&mut zipper, ready, CTRL_BREAK_EVENT, group);
    assert_cancelled_and_unchanged(zipper, &parent, &BTreeMap::new());
}

#[test]
fn ctrl_c_during_publication_into_a_folder_rolls_every_move_back() {
    let helper = Command::new(std::env::current_exe().unwrap())
        .args([
            CTRL_C_HELPER,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(IN_A_CONSOLE_OF_ITS_OWN, "1")
        .creation_flags(CREATE_NEW_CONSOLE)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&helper.stdout),
        String::from_utf8_lossy(&helper.stderr)
    );
    assert!(helper.status.success(), "{said}");
    assert!(said.contains("1 passed"), "the helper did not run: {said}");
}

#[test]
#[ignore = "started by ctrl_c_during_publication_into_a_folder_rolls_every_move_back"]
fn ctrl_c_from_a_console_of_its_own() {
    if std::env::var_os(IN_A_CONSOLE_OF_ITS_OWN).is_none() {
        return;
    }
    let merge = merge_into_a_folder_with_one_file();
    // SAFETY: a null handler with FALSE only turns Ctrl-C on for this process and its children.
    unsafe { SetConsoleCtrlHandler(None, 0) };
    let mut zipper = start_zipper(&merge.archive, &merge.output, true, 0);
    // SAFETY: a null handler with TRUE only makes this process ignore the Ctrl-C it sends.
    unsafe { SetConsoleCtrlHandler(None, 1) };
    let ready = || has_entry_starting_with(&merge.output, "f");
    send_when(
        &mut zipper,
        ready,
        CTRL_C_EVENT,
        EVERY_PROCESS_ON_THIS_CONSOLE,
    );
    assert_cancelled_and_unchanged(zipper, &merge.parent, &merge.before);
}
