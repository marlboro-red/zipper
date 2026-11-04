use anyhow::Context;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::ops::{Deref, DerefMut};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::ptr::NonNull;
use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_NO_BUFFERING, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, ReOpenFile,
};

pub(crate) const MIN_SIZE: u64 = 1024 * 1024;
// EXTERNAL(Windows): unbuffered writes need sector-aligned addresses, offsets and lengths.
// 4 KiB covers 512-byte and 4 KiB sectors. A larger sector fails and falls back.
const ALIGNMENT: usize = 4096;
const CHUNK: usize = 1024 * 1024;

pub(crate) fn padding(length: u64) -> Option<u64> {
    if length < MIN_SIZE {
        return Some(0);
    }
    Some(length.checked_next_multiple_of(ALIGNMENT as u64)? - length)
}

pub(crate) struct AlignedBuffer {
    start: NonNull<u8>,
    length: usize,
    layout: Layout,
}

// SAFETY: the buffer owns its allocation, and only `&mut self` hands out a mutable view.
unsafe impl Send for AlignedBuffer {}

impl AlignedBuffer {
    pub(crate) fn zeroed(length: usize) -> anyhow::Result<Self> {
        let layout = length
            .max(1)
            .checked_next_multiple_of(ALIGNMENT)
            .and_then(|size| Layout::from_size_align(size, ALIGNMENT).ok())
            .context("ZIP entry too large for an aligned buffer")?;
        // SAFETY: `layout` has a nonzero size.
        let start = NonNull::new(unsafe { alloc_zeroed(layout) })
            .context("cannot allocate ZIP entry buffer")?;
        Ok(Self {
            start,
            length,
            layout,
        })
    }

    fn padded(&self) -> &[u8] {
        // SAFETY: `alloc_zeroed` initialized all `layout.size()` bytes, which live until drop.
        unsafe { std::slice::from_raw_parts(self.start.as_ptr(), self.layout.size()) }
    }
}

impl Deref for AlignedBuffer {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.padded()[..self.length]
    }
}

impl DerefMut for AlignedBuffer {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as in `padded`, and `&mut self` makes this the only view.
        unsafe { std::slice::from_raw_parts_mut(self.start.as_ptr(), self.length) }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        // SAFETY: `zeroed` allocated `start` with this layout.
        unsafe { dealloc(self.start.as_ptr(), self.layout) }
    }
}

pub(crate) fn write(file: &mut File, buffer: &AlignedBuffer) -> std::io::Result<()> {
    if write_unbuffered(file, buffer).is_err() {
        file.seek(SeekFrom::Start(0))?;
        // RECORD(benchmarks/native-zip/windows-write-size/README.md): 256 KiB slices.
        for slice in buffer.chunks(256 * 1024) {
            file.write_all(slice)?;
        }
    }
    file.set_len(buffer.len() as u64)
}

fn write_unbuffered(file: &File, buffer: &AlignedBuffer) -> std::io::Result<()> {
    // SAFETY: `file` keeps its handle open for the whole call.
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_NO_BUFFERING,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: ReOpenFile returned a new handle that nothing else owns.
    let mut unbuffered = unsafe { File::from_raw_handle(handle) };
    for chunk in buffer.padded().chunks(CHUNK) {
        unbuffered.write_all(chunk)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_exact_bytes_for_aligned_and_unaligned_lengths() {
        let folder = tempfile::tempdir().unwrap();
        for length in [0, 1, 4095, 4096, 4097, MIN_SIZE as usize + 12_345] {
            let mut buffer = AlignedBuffer::zeroed(length).unwrap();
            for (index, byte) in buffer.iter_mut().enumerate() {
                *byte = (index * 7 % 251) as u8;
            }
            let path = folder.path().join(length.to_string());
            let mut file = File::create_new(&path).unwrap();
            write(&mut file, &buffer).unwrap();
            drop(file);
            assert_eq!(std::fs::read(&path).unwrap(), &buffer[..]);
        }
    }

    #[test]
    fn falls_back_when_the_file_cannot_be_reopened_for_writing() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("read-only");
        let mut file = crate::create_new_file(&path, 0o444, 0x01).unwrap();
        let mut buffer = AlignedBuffer::zeroed(MIN_SIZE as usize + 1).unwrap();
        buffer.fill(0xA5);
        write(&mut file, &buffer).unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), &buffer[..]);
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
    }
}
