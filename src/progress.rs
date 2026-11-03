use anyhow::Result;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
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
    /// Called one call at a time, from the calling thread or a ZIP worker,
    /// after each file and each decoded chunk, so every count only grows.
    /// Keep it fast, because decoding waits while it runs. If it panics, the
    /// panic reaches the caller and nothing is published.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
    /// Set it from any thread to stop the extraction. Once publishing into the
    /// destination has begun, the flag is ignored.
    pub cancel: Option<&'a AtomicBool>,
}

pub(crate) struct Tracker<'a> {
    control: Control<'a>,
    state: Mutex<Progress>,
}

impl<'a> Tracker<'a> {
    pub(crate) fn new(
        control: Control<'a>,
        files_total: Option<u64>,
        bytes_total: Option<u64>,
    ) -> Self {
        Self {
            control,
            state: Mutex::new(Progress {
                files_total,
                bytes_total,
                ..Progress::default()
            }),
        }
    }

    pub(crate) fn check(&self) -> Result<()> {
        match self.control.cancel {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(Cancelled.into()),
            _ => Ok(()),
        }
    }

    pub(crate) fn add_bytes(&self, bytes: u64) -> Result<()> {
        self.update(|progress| progress.bytes_done += bytes)
    }

    pub(crate) fn file_done(&self) -> Result<()> {
        self.update(|progress| progress.files_done += 1)
    }

    fn update(&self, change: impl FnOnce(&mut Progress)) -> Result<()> {
        if let Some(report) = self.control.progress {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            change(&mut state);
            report(*state);
        }
        self.check()
    }
}
