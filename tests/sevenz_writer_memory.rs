//! Heap memory while a solid 7z folder hands its small files to the writer
//! threads. Every file takes a whole reused buffer, so the buffers never
//! outnumber the pool's 16 MiB budget, whatever the file count, and the peak
//! stays within it.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};
use zipper::native_7z::{ReadOptions, read_archive};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static BUFFER_SIZED: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to `System` with the caller's arguments and only adds atomic counting.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            grow(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            grow(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` came from this allocator with `layout`, so from `System`.
        unsafe { System.dealloc(pointer, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: as for `dealloc`, and `size` meets `realloc`'s contract.
        let moved = unsafe { System.realloc(pointer, layout, size) };
        if !moved.is_null() {
            grow(size);
            CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        moved
    }
}

fn grow(size: usize) {
    if (MIB - 1..=MIB).contains(&size) {
        BUFFER_SIZED.fetch_add(1, Ordering::Relaxed);
    }
    let now = CURRENT.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const MIB: usize = 1 << 20;
const POOL_BUDGET: usize = 16 * MIB;
const EXTRACTION_BUFFERS: usize = 4 * MIB;
const EXACT_FILES: usize = 32;
const NEAR_FILES: usize = 64;
const POOL_BUFFERS: usize = 16;
const WORKER_BUFFERS: usize = 4;

fn number(value: u64) -> Vec<u8> {
    let mut bytes = vec![0xFF];
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes
}

fn solid_copy_archive(sizes: &[usize]) -> Vec<u8> {
    let packed: Vec<u8> = sizes
        .iter()
        .enumerate()
        .flat_map(|(index, &size)| (0..size).map(move |byte| (byte ^ index) as u8))
        .collect();
    let mut header = vec![1u8, 4, 6];
    header.extend(number(0));
    header.extend(number(1));
    header.push(9);
    header.extend(number(packed.len() as u64));
    header.extend([0, 7, 11]);
    header.extend(number(1));
    header.extend([0, 1, 1, 0, 12]);
    header.extend(number(packed.len() as u64));
    header.extend([10, 1]);
    header.extend_from_slice(&crc32fast::hash(&packed).to_le_bytes());
    header.extend([0, 8, 13]);
    header.extend(number(sizes.len() as u64));
    header.push(9);
    for &size in &sizes[..sizes.len() - 1] {
        header.extend(number(size as u64));
    }
    header.extend([0, 0, 5]);
    header.extend(number(sizes.len() as u64));
    let mut names = vec![0u8];
    for index in 0..sizes.len() {
        for unit in format!("f{index}").encode_utf16().chain([0]) {
            names.extend_from_slice(&unit.to_le_bytes());
        }
    }
    header.push(17);
    header.extend(number(names.len() as u64));
    header.extend(names);
    header.extend([0, 0]);
    let mut start = Vec::new();
    start.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    start.extend_from_slice(&(header.len() as u64).to_le_bytes());
    start.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
    let mut archive = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
    archive.extend_from_slice(&crc32fast::hash(&start).to_le_bytes());
    archive.extend(start);
    archive.extend(packed);
    archive.extend(header);
    archive
}

#[test]
fn files_waiting_for_the_writers_stay_within_the_pool_budget() {
    let sizes: Vec<usize> = std::iter::repeat_n(MIB, EXACT_FILES)
        .chain(std::iter::repeat_n(MIB - 1, NEAR_FILES))
        .collect();
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("solid.7z");
    fs::write(&archive, solid_copy_archive(&sizes)).unwrap();
    for threads in [Some(4), Some(64), None] {
        let destination = root.path().join(format!("out-{threads:?}"));
        let before = CURRENT.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        BUFFER_SIZED.store(0, Ordering::Relaxed);
        read_archive(
            &archive,
            Some(&destination),
            ReadOptions {
                threads,
                ..ReadOptions::default()
            },
        )
        .unwrap();
        let peak = PEAK.load(Ordering::Relaxed) - before;
        let buffers = BUFFER_SIZED.load(Ordering::Relaxed);
        assert!(
            buffers <= POOL_BUFFERS + WORKER_BUFFERS,
            "{threads:?} threads allocated {buffers} buffers of about 1 MiB for {} files",
            sizes.len()
        );
        assert!(
            peak <= POOL_BUDGET + EXTRACTION_BUFFERS,
            "{threads:?} threads peaked at {:.1} MiB",
            peak as f64 / MIB as f64
        );
        for (index, &size) in sizes.iter().enumerate() {
            let data = fs::read(destination.join(format!("f{index}"))).unwrap();
            assert!(
                data.len() == size
                    && data
                        .iter()
                        .enumerate()
                        .all(|(byte, &value)| value == (byte ^ index) as u8),
                "{threads:?} threads: f{index}"
            );
        }
    }
}
