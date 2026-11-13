//! Times ways of writing one in-memory tree to disk on macOS, with no archive
//! and no decoder. Build with `rustc -O write_probe.rs`.
//! Usage: write_probe SOURCE_DIR WORK_DIR ROUNDS [VARIANT...] > samples.json
//! Every variant writes the same folders and file bytes. A round runs the
//! variants in a shuffled order after one warmup round, and the tree is
//! removed outside the timed region.
use std::ffi::{CString, c_char, c_int, c_long, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Instant;

unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn openat(at: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn write(fd: c_int, data: *const c_void, size: usize) -> isize;
    fn futimens(fd: c_int, times: *const Timespec) -> c_int;
    fn utimensat(at: c_int, path: *const c_char, times: *const Timespec, flags: c_int) -> c_int;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    fn getrusage(who: c_int, usage: *mut [c_long; 18]) -> c_int;
}

#[repr(C)]
struct Timespec {
    seconds: i64,
    nanos: i64,
}
#[repr(C)]
struct Fstore {
    flags: u32,
    posmode: c_int,
    offset: i64,
    length: i64,
    allocated: i64,
}

const O_WRONLY: c_int = 0x1;
const O_CREAT: c_int = 0x200;
const O_EXCL: c_int = 0x800;
const O_DIRECTORY: c_int = 0x10_0000;
const O_CLOEXEC: c_int = 0x100_0000;
const CREATE: c_int = O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC;
const AT_FDCWD: c_int = -2;
const F_PREALLOCATE: c_int = 42;
const F_NOCACHE: c_int = 48;
const F_ALLOCATEALL: u32 = 4;
const F_PEOFPOSMODE: c_int = 3;
const TIME: [Timespec; 2] = [
    Timespec { seconds: 1_577_836_800, nanos: 0 },
    Timespec { seconds: 1_577_836_800, nanos: 0 },
];

#[derive(Clone, Copy, PartialEq)]
enum Open {
    FullPath,
    AtFolder,
}
#[derive(Clone, Copy, PartialEq)]
enum Times {
    OnTheOpenFile,
    None,
    ByPathAfterAllFiles,
}
#[derive(Clone, Copy, PartialEq)]
enum Order {
    Archive,
    LargestFirst,
}
#[derive(Clone, Copy, PartialEq)]
enum Close {
    Inline,
    OnAnotherThread,
}
#[derive(Clone, Copy)]
struct Variant {
    name: &'static str,
    threads: usize,
    open: Open,
    times: Times,
    order: Order,
    close: Close,
    preallocate: bool,
    no_cache: bool,
    write: bool,
    write_size: usize,
}

const BASE: Variant = Variant {
    name: "base",
    threads: 1,
    open: Open::FullPath,
    times: Times::OnTheOpenFile,
    order: Order::Archive,
    close: Close::Inline,
    preallocate: false,
    no_cache: false,
    write: true,
    write_size: usize::MAX,
};

fn variants() -> Vec<Variant> {
    let mut all = Vec::new();
    for threads in [1, 2, 3, 4, 6, 8] {
        all.push(Variant { name: "base", threads, ..BASE });
    }
    for threads in [1, 4] {
        all.push(Variant { name: "largest-first", threads, order: Order::LargestFirst, ..BASE });
        all.push(Variant { name: "openat", threads, open: Open::AtFolder, ..BASE });
        all.push(Variant { name: "no-times", threads, times: Times::None, ..BASE });
        all.push(Variant { name: "times-after", threads, times: Times::ByPathAfterAllFiles, ..BASE });
        all.push(Variant { name: "close-elsewhere", threads, close: Close::OnAnotherThread, ..BASE });
        all.push(Variant { name: "preallocate", threads, preallocate: true, ..BASE });
        all.push(Variant { name: "create-close-only", threads, write: false, times: Times::None, ..BASE });
        all.push(Variant { name: "write-64k", threads, write_size: 64 << 10, ..BASE });
        all.push(Variant { name: "write-1m", threads, write_size: 1 << 20, ..BASE });
        all.push(Variant { name: "write-1m-preallocate", threads, write_size: 1 << 20, preallocate: true, ..BASE });
        all.push(Variant { name: "write-1m-nocache", threads, write_size: 1 << 20, no_cache: true, ..BASE });
        all.push(Variant { name: "write-4m", threads, write_size: 4 << 20, ..BASE });
    }
    all
}

struct FileToWrite {
    full: CString,
    leaf: CString,
    folder: usize,
    bytes: Vec<u8>,
}

struct Tree {
    folders: Vec<PathBuf>,
    files: Vec<(PathBuf, usize, Vec<u8>)>,
}

fn load(source: &Path, relative: &Path, tree: &mut Tree) {
    let folder = tree.folders.len();
    tree.folders.push(relative.to_owned());
    let mut entries: Vec<_> = std::fs::read_dir(source.join(relative)).unwrap().map(|entry| entry.unwrap()).collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let below = relative.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            load(source, &below, tree);
        } else {
            tree.files.push((below.clone(), folder, std::fs::read(source.join(&below)).unwrap()));
        }
    }
}

fn c_path(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).unwrap()
}

fn cpu_micros() -> i64 {
    let mut usage = [0 as c_long; 18];
    unsafe { getrusage(0, &mut usage) };
    let part = |at: usize| usage[at] as i64 * 1_000_000 + (usage[at + 1] as i64 & 0xffff_ffff);
    part(0) + part(2)
}

fn write_one(variant: &Variant, file: &FileToWrite, folders: &[c_int], closes: Option<&std::sync::mpsc::Sender<c_int>>) {
    unsafe {
        let fd = match variant.open {
            Open::FullPath => open(file.full.as_ptr(), CREATE, 0o644 as c_int),
            Open::AtFolder => openat(folders[file.folder], file.leaf.as_ptr(), CREATE, 0o644 as c_int),
        };
        assert!(fd >= 0, "create failed: {:?}", std::io::Error::last_os_error());
        if variant.no_cache {
            fcntl(fd, F_NOCACHE, 1 as c_int);
        }
        if variant.preallocate && !file.bytes.is_empty() {
            let mut store = Fstore {
                flags: F_ALLOCATEALL,
                posmode: F_PEOFPOSMODE,
                offset: 0,
                length: file.bytes.len() as i64,
                allocated: 0,
            };
            fcntl(fd, F_PREALLOCATE, &mut store as *mut Fstore);
        }
        if variant.write {
            for piece in file.bytes.chunks(variant.write_size.min(file.bytes.len().max(1))) {
                let mut rest = piece;
                while !rest.is_empty() {
                    let wrote = write(fd, rest.as_ptr().cast(), rest.len());
                    assert!(wrote > 0, "write failed");
                    rest = &rest[wrote as usize..];
                }
            }
        }
        if variant.times == Times::OnTheOpenFile {
            futimens(fd, TIME.as_ptr());
        }
        match closes {
            Some(closes) => closes.send(fd).unwrap(),
            None => {
                close(fd);
            }
        }
    }
}

fn run(variant: &Variant, tree: &Tree, root: &Path) -> (f64, f64, f64) {
    let files: Vec<FileToWrite> = tree
        .files
        .iter()
        .map(|(relative, folder, bytes)| FileToWrite {
            full: c_path(&root.join(relative)),
            leaf: CString::new(relative.file_name().unwrap().as_bytes()).unwrap(),
            folder: *folder,
            bytes: bytes.clone(),
        })
        .collect();
    let mut order: Vec<usize> = (0..files.len()).collect();
    if variant.order == Order::LargestFirst {
        order.sort_by_key(|&index| std::cmp::Reverse(files[index].bytes.len()));
    }
    let cpu_before = cpu_micros();
    let started = Instant::now();
    let mut folders = Vec::new();
    for folder in &tree.folders {
        let path = root.join(folder);
        std::fs::create_dir(&path).unwrap();
        if variant.open == Open::AtFolder {
            let fd = unsafe { open(c_path(&path).as_ptr(), O_DIRECTORY | O_CLOEXEC) };
            assert!(fd >= 0);
            folders.push(fd);
        }
    }
    let folders_made = started.elapsed();
    let next = AtomicUsize::new(0);
    let (closes, to_close) = channel::<c_int>();
    std::thread::scope(|scope| {
        if variant.close == Close::OnAnotherThread {
            scope.spawn(move || {
                for fd in to_close {
                    unsafe { close(fd) };
                }
            });
        }
        let work = |closes: Option<std::sync::mpsc::Sender<c_int>>| {
            loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some(&index) = order.get(at) else { break };
                write_one(variant, &files[index], &folders, closes.as_ref());
            }
        };
        let sender = |on: bool| on.then(|| closes.clone());
        std::thread::scope(|workers| {
            for _ in 1..variant.threads {
                let closes = sender(variant.close == Close::OnAnotherThread);
                workers.spawn(move || work(closes));
            }
            work(sender(variant.close == Close::OnAnotherThread));
        });
        if variant.times == Times::ByPathAfterAllFiles {
            let next = AtomicUsize::new(0);
            std::thread::scope(|workers| {
                for _ in 0..variant.threads {
                    workers.spawn(|| {
                        loop {
                            let at = next.fetch_add(1, Ordering::Relaxed);
                            let Some(file) = files.get(at) else { break };
                            unsafe { utimensat(AT_FDCWD, file.full.as_ptr(), TIME.as_ptr(), 0) };
                        }
                    });
                }
            });
        }
        drop(closes);
    });
    for fd in folders {
        unsafe { close(fd) };
    }
    let wall = started.elapsed();
    let cpu = cpu_micros() - cpu_before;
    (wall.as_secs_f64() * 1e3, folders_made.as_secs_f64() * 1e3, cpu as f64 / 1e3)
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let source = PathBuf::from(&arguments[1]);
    let work = PathBuf::from(&arguments[2]);
    let rounds: usize = arguments[3].parse().unwrap();
    let wanted = &arguments[4..];
    let mut tree = Tree { folders: Vec::new(), files: Vec::new() };
    load(&source, Path::new(""), &mut tree);
    let all: Vec<Variant> = variants()
        .into_iter()
        .filter(|variant| wanted.is_empty() || wanted.iter().any(|name| name == variant.name))
        .collect();
    let mut samples: Vec<Vec<(f64, f64, f64)>> = vec![Vec::new(); all.len()];
    let mut seed = 0x2026_1113_u64;
    let mut orders = Vec::new();
    for round in 0..=rounds {
        let mut order: Vec<usize> = (0..all.len()).collect();
        for at in (1..order.len()).rev() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            order.swap(at, (seed >> 33) as usize % (at + 1));
        }
        for &index in &order {
            let root = work.join("probe-tree");
            let sample = run(&all[index], &tree, &root);
            let written: usize = walk_count(&root);
            assert_eq!(written, tree.files.len(), "{} wrote the wrong number of files", all[index].name);
            std::fs::remove_dir_all(&root).unwrap();
            if round > 0 {
                samples[index].push(sample);
            }
        }
        orders.push(order);
    }
    let total: usize = tree.files.iter().map(|file| file.2.len()).sum();
    println!("{{\"source\": {:?}, \"files\": {}, \"folders\": {}, \"bytes\": {}, \"rounds\": {}, \"warmup_rounds\": 1,", source.file_name().unwrap(), tree.files.len(), tree.folders.len(), total, rounds);
    println!("\"order_including_warmup\": {:?},", orders);
    println!("\"variants\": [");
    for (index, variant) in all.iter().enumerate() {
        let column = |pick: fn(&(f64, f64, f64)) -> f64| {
            samples[index].iter().map(|sample| format!("{:.3}", pick(sample))).collect::<Vec<_>>().join(", ")
        };
        println!(
            "{{\"name\": \"{}\", \"threads\": {}, \"wall_ms\": [{}], \"folders_ms\": [{}], \"cpu_ms\": [{}]}}{}",
            variant.name,
            variant.threads,
            column(|sample| sample.0),
            column(|sample| sample.1),
            column(|sample| sample.2),
            if index + 1 == all.len() { "" } else { "," }
        );
    }
    println!("]}}");
}

fn walk_count(root: &Path) -> usize {
    let mut count = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            count += walk_count(&entry.path());
        } else {
            count += 1;
        }
    }
    count
}
