use crate::counters::{self, Counter};
use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Condvar, Mutex,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    time::SystemTime,
};

pub(crate) const CHUNK: usize = 1024 * 1024;
const QUEUED_FILES: usize = 16;
pub(crate) const QUEUED_CHUNKS_PER_FILE: usize = 4;
const BYTES_IN_FLIGHT: usize = 16 * 1024 * 1024;
// RECORD(benchmarks/cross-platform/tar-folder-lanes/README.md): one writer per folder is faster on Windows.
const ONE_WRITER_PER_FOLDER: bool = cfg!(windows);

#[derive(Default)]
pub(crate) struct InFlight {
    bytes: Mutex<usize>,
    freed: Condvar,
    spare: Mutex<Vec<Vec<u8>>>,
}

impl InFlight {
    pub(crate) fn take(&self, length: usize) -> Chunk<'_> {
        let mut bytes = self.bytes.lock().unwrap();
        while *bytes > 0 && *bytes + length > BYTES_IN_FLIGHT {
            bytes = self.freed.wait(bytes).unwrap();
        }
        *bytes += length;
        drop(bytes);
        let recycled = (length == CHUNK)
            .then(|| self.spare.lock().unwrap().pop())
            .flatten();
        Chunk {
            data: recycled.unwrap_or_else(|| vec![0; length]),
            flight: self,
        }
    }
}

pub(crate) struct Chunk<'f> {
    pub(crate) data: Vec<u8>,
    flight: &'f InFlight,
}

impl Drop for Chunk<'_> {
    fn drop(&mut self) {
        let data = std::mem::take(&mut self.data);
        let length = data.len();
        if length == CHUNK {
            self.flight.spare.lock().unwrap().push(data);
        }
        *self.flight.bytes.lock().unwrap() -= length;
        self.flight.freed.notify_all();
    }
}

pub(crate) struct Job<'f> {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) mode: u32,
    pub(crate) attributes: u32,
    pub(crate) modified: Option<SystemTime>,
    pub(crate) size: u64,
    pub(crate) first: Chunk<'f>,
    pub(crate) rest: Option<Receiver<Chunk<'f>>>,
}

pub(crate) type Queue<'f> = Mutex<Receiver<Job<'f>>>;

pub(crate) struct Writers<'f> {
    format: &'static str,
    lanes: Mutex<Vec<SyncSender<Job<'f>>>>,
    pub(crate) flight: &'f InFlight,
    pending: Mutex<usize>,
    idle: Condvar,
    failure: Mutex<Option<anyhow::Error>>,
    callback_panic: Mutex<Option<Box<dyn std::any::Any + Send>>>,
}

#[derive(Clone, Copy)]
pub(crate) enum Written {
    Bytes(u64),
    File,
}

impl<'f> Writers<'f> {
    pub(crate) fn new(
        format: &'static str,
        flight: &'f InFlight,
        writers: usize,
    ) -> (Self, Vec<Queue<'f>>) {
        let lane_count = if ONE_WRITER_PER_FOLDER { writers } else { 1 };
        let (lanes, queues): (Vec<_>, Vec<_>) = (0..lane_count)
            .map(|_| {
                let (lane, queue) = sync_channel(QUEUED_FILES);
                (lane, Mutex::new(queue))
            })
            .unzip();
        let pool = Writers {
            format,
            lanes: Mutex::new(lanes),
            flight,
            pending: Mutex::new(0),
            idle: Condvar::new(),
            failure: Mutex::new(None),
            callback_panic: Mutex::new(None),
        };
        (pool, queues)
    }

    pub(crate) fn send(&self, job: Job<'f>) -> Result<()> {
        *self.pending.lock().unwrap() += 1;
        let lanes = self.lanes.lock().unwrap();
        let lane = match lanes.len() {
            0 | 1 => 0,
            count => {
                let folder = job.name.rfind('/').map_or("", |slash| &job.name[..slash]);
                let mut hasher = std::hash::DefaultHasher::new();
                std::hash::Hash::hash(&folder.to_lowercase(), &mut hasher);
                std::hash::Hasher::finish(&hasher) as usize % count
            }
        };
        let sent = lanes.get(lane).is_some_and(|lane| lane.send(job).is_ok());
        ensure!(sent, "{} writers stopped", self.format);
        self.check()
    }

    pub(crate) fn check(&self) -> Result<()> {
        match self.failure.lock().unwrap().take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(crate) fn stopped(&self) -> anyhow::Error {
        self.check()
            .err()
            .unwrap_or_else(|| anyhow::anyhow!("{} writers stopped", self.format))
    }

    pub(crate) fn drain(&self) -> Result<()> {
        let mut pending = self.pending.lock().unwrap();
        while *pending > 0 {
            pending = self.idle.wait(pending).unwrap();
        }
        drop(pending);
        self.check()
    }

    fn close(&self) {
        self.lanes.lock().unwrap().clear();
    }

    pub(crate) fn resume_callback_panic(&self) {
        if let Some(panic) = self.callback_panic.lock().unwrap().take() {
            std::panic::resume_unwind(panic);
        }
    }

    fn report(&self, count: &dyn Fn(Written) -> Result<()>, written: Written) {
        let counted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = count(written);
        }));
        if let Err(panic) = counted {
            self.callback_panic.lock().unwrap().get_or_insert(panic);
            self.failure
                .lock()
                .unwrap()
                .get_or_insert_with(|| anyhow::anyhow!("the progress callback panicked"));
        }
    }

    pub(crate) fn work(&self, jobs: &Queue<'f>, count: &dyn Fn(Written) -> Result<()>) {
        loop {
            let next = jobs.lock().unwrap().recv();
            let Ok(mut job) = next else {
                return;
            };
            if self.failure.lock().unwrap().is_none() {
                let report = |bytes: u64| self.report(count, Written::Bytes(bytes));
                let written = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let (total, held) = write_job(self.format, &mut job, &report)?;
                    self.report(count, Written::Bytes(held));
                    if total == job.size {
                        self.report(count, Written::File);
                    }
                    Ok(())
                }))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "a {} writer panicked writing {:?}",
                        self.format,
                        job.name
                    ))
                });
                if let Err(error) = written {
                    self.failure.lock().unwrap().get_or_insert(error);
                }
            }
            drop(job);
            let mut pending = self.pending.lock().unwrap();
            *pending -= 1;
            if *pending == 0 {
                self.idle.notify_all();
            }
        }
    }
}

pub(crate) fn queue<'q, 'f>(queues: &'q [Queue<'f>], writer: usize) -> &'q Queue<'f> {
    &queues[writer % queues.len()]
}

fn write_job(format: &str, job: &mut Job, written: &dyn Fn(u64)) -> Result<(u64, u64)> {
    let name = &job.name;
    let mut file = crate::create_new_file(&job.path, job.mode, job.attributes).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!(
                "{format} entry {name:?} collides with another entry on this filesystem, which ignores case or Unicode normalization"
            )
        } else {
            anyhow::Error::from(error).context(format!("cannot create {format} entry {name:?}"))
        }
    })?;
    counters::add(Counter::FilesCreated, 1);
    let mut total = 0;
    let mut held = 0;
    let mut write = |bytes: &[u8]| -> Result<()> {
        total += bytes.len() as u64;
        if !bytes.is_empty() {
            file.write_all(bytes)
                .with_context(|| format!("cannot write {format} entry {name:?}"))?;
            written(std::mem::replace(&mut held, bytes.len() as u64));
            counters::add(Counter::OutputWrites, 1);
            counters::add(Counter::OutputBytesWritten, bytes.len() as u64);
        }
        Ok(())
    };
    write(&job.first.data)?;
    if let Some(rest) = &job.rest {
        for chunk in rest {
            write(&chunk.data)?;
        }
    }
    if let Some(time) = job.modified {
        crate::set_modified_best_effort(&file, time);
    }
    drop(file);
    Ok((total, held))
}

pub(crate) struct ClosesOnDrop<'w, 'f>(pub(crate) &'w Writers<'f>);

impl Drop for ClosesOnDrop<'_, '_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod in_flight_tests {
    use super::*;

    #[test]
    fn chunks_in_flight_never_exceed_the_budget_and_release_on_drop() {
        let flight = InFlight::default();
        let peak = Mutex::new(0usize);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    for round in 0..200 {
                        let held = flight.take(CHUNK + round % 2);
                        let now = *flight.bytes.lock().unwrap();
                        let mut peak = peak.lock().unwrap();
                        *peak = (*peak).max(now);
                        drop(peak);
                        std::thread::yield_now();
                        drop(held);
                    }
                });
            }
        });
        assert!(*peak.lock().unwrap() <= BYTES_IN_FLIGHT);
        assert_eq!(*flight.bytes.lock().unwrap(), 0);
        let spare = flight.spare.lock().unwrap();
        assert!(!spare.is_empty() && spare.len() <= BYTES_IN_FLIGHT / CHUNK);
    }
}
