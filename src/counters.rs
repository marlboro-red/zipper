//! Extraction counters for profiling outside timed runs. They count only when
//! the `counters` feature is enabled. Without it the counting calls compile to
//! nothing and [`take`] returns zeros.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Counter {
    /// Read calls on the archive file after the central directory check.
    ArchiveReads,
    /// Bytes those reads returned.
    ArchiveBytesRead,
    /// Regular files created in the staging directory.
    FilesCreated,
    /// Calls that create a directory and its missing parents.
    DirectoryCreateCalls,
    SymlinksCreated,
    /// Write calls on extracted files.
    OutputWrites,
    /// Bytes those writes accepted.
    OutputBytesWritten,
    /// Calls that set a mode or a modification time.
    MetadataCalls,
    /// Wall time inside Deflate decoder calls, summed across threads.
    DecodeNanos,
    /// SquashFS fragment blocks decoded or copied into the fragment cache.
    FragmentBlocksRead,
    /// Wall time of the ZIP reader's serial folder setup before its workers
    /// start.
    SetupNanos,
}

pub const ALL: [Counter; 11] = [
    Counter::ArchiveReads,
    Counter::ArchiveBytesRead,
    Counter::FilesCreated,
    Counter::DirectoryCreateCalls,
    Counter::SymlinksCreated,
    Counter::OutputWrites,
    Counter::OutputBytesWritten,
    Counter::MetadataCalls,
    Counter::DecodeNanos,
    Counter::FragmentBlocksRead,
    Counter::SetupNanos,
];

impl Counter {
    pub fn name(self) -> &'static str {
        match self {
            Self::ArchiveReads => "archive_reads",
            Self::ArchiveBytesRead => "archive_bytes_read",
            Self::FilesCreated => "files_created",
            Self::DirectoryCreateCalls => "directory_create_calls",
            Self::SymlinksCreated => "symlinks_created",
            Self::OutputWrites => "output_writes",
            Self::OutputBytesWritten => "output_bytes_written",
            Self::MetadataCalls => "metadata_calls",
            Self::DecodeNanos => "decode_nanos",
            Self::FragmentBlocksRead => "fragment_blocks_read",
            Self::SetupNanos => "setup_nanos",
        }
    }
}

#[cfg(feature = "counters")]
static VALUES: [std::sync::atomic::AtomicU64; ALL.len()] =
    [const { std::sync::atomic::AtomicU64::new(0) }; ALL.len()];

#[inline(always)]
pub(crate) fn add(counter: Counter, amount: u64) {
    #[cfg(feature = "counters")]
    VALUES[counter as usize].fetch_add(amount, std::sync::atomic::Ordering::Relaxed);
    #[cfg(not(feature = "counters"))]
    let _ = (counter, amount);
}

#[inline(always)]
pub(crate) fn time<T>(counter: Counter, work: impl FnOnce() -> T) -> T {
    #[cfg(feature = "counters")]
    {
        let start = std::time::Instant::now();
        let result = work();
        add(counter, start.elapsed().as_nanos() as u64);
        result
    }
    #[cfg(not(feature = "counters"))]
    {
        let _ = counter;
        work()
    }
}

/// Return every counter and reset it to zero. Without the `counters`
/// feature every value is zero.
pub fn take() -> Vec<(Counter, u64)> {
    ALL.iter()
        .map(|&counter| {
            #[cfg(feature = "counters")]
            let value = VALUES[counter as usize].swap(0, std::sync::atomic::Ordering::Relaxed);
            #[cfg(not(feature = "counters"))]
            let value = 0;
            (counter, value)
        })
        .collect()
}
