//! Peak heap memory while a solid LZMA2 7z folder decodes in segments. The
//! folder's reader decodes the front segment straight into the extraction's
//! buffer, so only the segments behind it hold their output in memory.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};
use zipper::native_7z::{ReadOptions, read_archive};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

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
    let now = CURRENT.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const MIB: usize = 1 << 20;
const FRONT_OUTPUT: usize = 16 * MIB;
const BEHIND_OUTPUT: usize = 9 * MIB;
const DICTIONARY: usize = 2 * MIB;
const EXTRACTION_BUFFERS: usize = 4 * MIB;
const LZMA2_PROPERTY_2_MIB: u8 = 18;

fn number(value: u64) -> Vec<u8> {
    let mut bytes = vec![0xFF];
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes
}

fn solid_lzma2_archive(packed: &[u8], data: &[u8]) -> Vec<u8> {
    let mut header = vec![1u8, 4, 6];
    header.extend(number(0));
    header.extend(number(1));
    header.push(9);
    header.extend(number(packed.len() as u64));
    header.extend([0, 7, 11]);
    header.extend(number(1));
    header.push(0);
    header.extend(number(1));
    header.extend([0x21, 0x21]);
    header.extend(number(1));
    header.push(LZMA2_PROPERTY_2_MIB);
    header.push(12);
    header.extend(number(data.len() as u64));
    header.extend([10, 1]);
    header.extend_from_slice(&crc32fast::hash(data).to_le_bytes());
    header.extend([0, 0, 5]);
    header.extend(number(1));
    let mut names = vec![17u8];
    let name: Vec<u8> = "big\0"
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    names.extend(number(name.len() as u64 + 1));
    names.push(0);
    names.extend(name);
    header.extend(names);
    header.extend([0, 0]);
    let mut start = Vec::new();
    start.extend_from_slice(&(packed.len() as u64).to_le_bytes());
    start.extend_from_slice(&(header.len() as u64).to_le_bytes());
    start.extend_from_slice(&crc32fast::hash(&header).to_le_bytes());
    let mut archive = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
    archive.extend_from_slice(&crc32fast::hash(&start).to_le_bytes());
    archive.extend(start);
    archive.extend_from_slice(packed);
    archive.extend(header);
    archive
}

fn segmented() -> (Vec<u8>, Vec<u8>) {
    let mut packed = Vec::new();
    let mut data = Vec::new();
    for (part, output) in [FRONT_OUTPUT, BEHIND_OUTPUT].into_iter().enumerate() {
        let plain: Vec<u8> = (0..output as u32)
            .map(|index| (index.wrapping_mul(2_654_435_761) >> 24) as u8 ^ part as u8)
            .collect();
        let mut filters = liblzma::stream::Filters::new();
        filters.lzma2(&liblzma::stream::LzmaOptions::new_preset(1).unwrap());
        let mut encoder = liblzma::stream::Stream::new_raw_encoder(&filters).unwrap();
        let mut encoded = Vec::with_capacity(plain.len() + MIB);
        encoder
            .process_vec(&plain, &mut encoded, liblzma::stream::Action::Finish)
            .unwrap();
        assert_eq!(encoded.pop(), Some(0), "a raw LZMA2 stream ends with 0x00");
        packed.extend_from_slice(&encoded);
        data.extend_from_slice(&plain);
    }
    packed.push(0);
    (solid_lzma2_archive(&packed, &data), data)
}

#[test]
fn only_the_segment_behind_the_front_holds_its_output_in_memory() {
    let (bytes, data) = segmented();
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("solid.7z");
    fs::write(&archive, &bytes).unwrap();
    drop(bytes);
    for threads in [2, 4] {
        let destination = root.path().join(format!("out-{threads}"));
        let before = CURRENT.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        read_archive(
            &archive,
            Some(&destination),
            ReadOptions {
                threads: Some(threads),
                ..ReadOptions::default()
            },
        )
        .unwrap();
        let peak = PEAK.load(Ordering::Relaxed) - before;
        assert!(
            peak <= BEHIND_OUTPUT + 2 * DICTIONARY + EXTRACTION_BUFFERS,
            "{threads} threads peaked at {:.1} MiB",
            peak as f64 / MIB as f64
        );
        assert!(
            fs::read(destination.join("big")).unwrap() == data,
            "{threads} threads"
        );
    }
}
