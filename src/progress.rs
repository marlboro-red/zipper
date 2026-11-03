use anyhow::Result;
use std::sync::{
    Mutex, TryLockError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Extraction progress. Byte counts are decoded file bytes. ZIP knows both
/// totals before it starts. A tar stream reports `None` for them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub files_done: u64,
    pub files_total: Option<u64>,
    pub bytes_done: u64,
    pub bytes_total: Option<u64>,
}

/// The error an extraction returns after its cancel flag is set. Test for it
/// with `error.is::<zipper::Cancelled>()`. A cancelled extraction changes
/// nothing in the destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("extraction cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Hooks an application passes to follow or stop an extraction.
#[derive(Clone, Copy, Default)]
pub struct Control<'a> {
    /// Called one call at a time, after files and decoded chunks, so every
    /// count only grows. A report that would wait for a running call is
    /// skipped, and the last call, from the calling thread, has the final
    /// counts. Keep it fast. If it panics, the panic reaches the caller and
    /// nothing is published.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
    /// Set it from any thread to stop the extraction. Once publishing into the
    /// destination has begun, the flag is ignored.
    pub cancel: Option<&'a AtomicBool>,
}

pub(crate) struct Tracker<'a> {
    control: Control<'a>,
    files_total: Option<u64>,
    bytes_total: Option<u64>,
    files_done: AtomicU64,
    bytes_done: AtomicU64,
    reporting: Mutex<()>,
}

impl<'a> Tracker<'a> {
    pub(crate) fn new(
        control: Control<'a>,
        files_total: Option<u64>,
        bytes_total: Option<u64>,
    ) -> Self {
        Self {
            control,
            files_total,
            bytes_total,
            files_done: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            reporting: Mutex::new(()),
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        match self.control.cancel {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(Cancelled.into()),
            _ => Ok(()),
        }
    }

    pub(crate) fn add_bytes(&self, bytes: u64) -> Result<()> {
        self.bytes_done.fetch_add(bytes, Ordering::Relaxed);
        self.report_unless_busy();
        self.check()
    }

    pub(crate) fn file_done(&self) -> Result<()> {
        self.files_done.fetch_add(1, Ordering::Relaxed);
        self.report_unless_busy();
        self.check()
    }

    /// Report the final counts. Call it from the calling thread once workers
    /// have stopped, because it waits for any report still running.
    pub(crate) fn finish(&self) {
        if let Some(report) = self.control.progress {
            let _turn = self
                .reporting
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            report(self.snapshot());
        }
    }

    fn report_unless_busy(&self) {
        let Some(report) = self.control.progress else {
            return;
        };
        let _turn = match self.reporting.try_lock() {
            Ok(turn) => turn,
            Err(TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(TryLockError::WouldBlock) => return,
        };
        report(self.snapshot());
    }

    fn snapshot(&self) -> Progress {
        Progress {
            files_done: self.files_done.load(Ordering::Relaxed),
            files_total: self.files_total,
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            bytes_total: self.bytes_total,
        }
    }
}
