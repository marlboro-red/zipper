//! Cloneable file or immutable-memory cursors. File cursors share a handle
//! with a lock covering physical I/O; memory cursors share bytes. Positions
//! are private to each clone.
use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub(crate) struct Cursor {
    file: Arc<Mutex<File>>,
    position: u64,
    length: u64,
}
impl Read for Cursor {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("ZIP reader lock poisoned"))?;
        file.seek(SeekFrom::Start(self.position))?;
        let read = file.read(bytes)?;
        self.position += read as u64;
        Ok(read)
    }
}
impl Seek for Cursor {
    fn seek(&mut self, seek: SeekFrom) -> io::Result<u64> {
        let position = match seek {
            SeekFrom::Start(position) => position as i128,
            SeekFrom::Current(delta) => self.position as i128 + delta as i128,
            SeekFrom::End(delta) => self.length as i128 + delta as i128,
        };
        self.position = u64::try_from(position)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid ZIP seek"))?;
        Ok(self.position)
    }
}

#[cfg(windows)]
const SMALL_BUFFER: usize = 8 * 1024;
#[cfg(windows)]
const LARGE_BUFFER: usize = 256 * 1024;
#[cfg(windows)]
const SEQUENTIAL_THRESHOLD: usize = 64 * 1024;

#[cfg(windows)]
pub(crate) struct FileReader {
    reader: BufReader<Cursor>,
    sequential_bytes: usize,
}
#[cfg(windows)]
impl FileReader {
    fn new(cursor: Cursor) -> Self {
        Self {
            reader: BufReader::with_capacity(SMALL_BUFFER, cursor),
            sequential_bytes: 0,
        }
    }
    fn position(&self) -> u64 {
        self.reader.get_ref().position - self.reader.buffer().len() as u64
    }
    fn resize(&mut self, capacity: usize) {
        let mut cursor = self.reader.get_ref().clone();
        cursor.position = self.position();
        self.reader = BufReader::with_capacity(capacity, cursor);
    }
}

pub(crate) enum Reader {
    #[cfg(not(windows))]
    File(BufReader<Cursor>),
    #[cfg(windows)]
    AdaptiveFile(FileReader),
    Memory(io::Cursor<Arc<[u8]>>),
}
impl Reader {
    pub(crate) fn new(file: File) -> io::Result<Self> {
        let length = file.metadata()?.len();
        let cursor = Cursor {
            file: Arc::new(Mutex::new(file)),
            position: 0,
            length,
        };
        #[cfg(windows)]
        {
            Ok(Self::AdaptiveFile(FileReader::new(cursor)))
        }
        #[cfg(not(windows))]
        {
            Ok(Self::File(BufReader::with_capacity(8 * 1024, cursor)))
        }
    }

    pub(crate) fn cached(mut file: File, limit: u64) -> io::Result<Self> {
        let read_limit = limit.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid archive cache limit")
        })?;
        if file.metadata()?.len() > limit {
            return Self::new(file);
        }
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.take(read_limit).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::other("ZIP grew beyond archive cache limit"));
        }
        Ok(Self::Memory(io::Cursor::new(bytes.into())))
    }
}
impl Clone for Reader {
    fn clone(&self) -> Self {
        match self {
            #[cfg(windows)]
            Self::AdaptiveFile(reader) => {
                let mut cursor = reader.reader.get_ref().clone();
                cursor.position = reader.position();
                Self::AdaptiveFile(FileReader::new(cursor))
            }
            Self::Memory(cursor) => Self::Memory(cursor.clone()),
            #[cfg(not(windows))]
            Self::File(reader) => {
                let mut cursor = reader.get_ref().clone();
                cursor.position -= reader.buffer().len() as u64;
                Self::File(BufReader::with_capacity(8 * 1024, cursor))
            }
        }
    }
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(not(windows))]
            Self::File(reader) => reader.read(bytes),
            #[cfg(windows)]
            Self::AdaptiveFile(file) => {
                // Tiny entries keep small read-ahead. Sustained decoder reads
                // amortize physical I/O through a larger window.
                if file.sequential_bytes >= SEQUENTIAL_THRESHOLD
                    && bytes.len() >= SMALL_BUFFER
                    && file.reader.capacity() == SMALL_BUFFER
                {
                    file.resize(LARGE_BUFFER);
                }
                let read = file.reader.read(bytes)?;
                file.sequential_bytes = file.sequential_bytes.saturating_add(read);
                Ok(read)
            }

            Self::Memory(reader) => reader.read(bytes),
        }
    }
}
impl Seek for Reader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            #[cfg(not(windows))]
            Self::File(reader) => {
                // ZIP header/data seeks often stay inside the current read-ahead
                // window. Preserve those bytes instead of refilling the window.
                let current = reader.get_ref().position - reader.buffer().len() as u64;
                let target = match position {
                    SeekFrom::Start(target) => i128::from(target),
                    SeekFrom::Current(delta) => i128::from(current) + i128::from(delta),
                    SeekFrom::End(delta) => i128::from(reader.get_ref().length) + i128::from(delta),
                };
                let target = u64::try_from(target)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid ZIP seek"))?;
                if let Ok(delta) = i64::try_from(i128::from(target) - i128::from(current)) {
                    reader.seek_relative(delta)?;
                    Ok(target)
                } else {
                    reader.seek(SeekFrom::Start(target))
                }
            }
            #[cfg(windows)]
            Self::AdaptiveFile(file) => {
                let current = file.position();
                let reader = &mut file.reader;
                // ZIP header/data seeks often stay inside the current read-ahead
                // window. Preserve those bytes instead of refilling the window.
                let target = match position {
                    SeekFrom::Start(target) => i128::from(target),
                    SeekFrom::Current(delta) => i128::from(current) + i128::from(delta),
                    SeekFrom::End(delta) => i128::from(reader.get_ref().length) + i128::from(delta),
                };
                let target = u64::try_from(target)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid ZIP seek"))?;
                if target != current {
                    file.sequential_bytes = 0;
                    if file.reader.capacity() != SMALL_BUFFER {
                        file.resize(SMALL_BUFFER);
                    }
                }
                let reader = &mut file.reader;
                if let Ok(delta) = i64::try_from(i128::from(target) - i128::from(current)) {
                    reader.seek_relative(delta)?;
                    Ok(target)
                } else {
                    reader.seek(SeekFrom::Start(target))
                }
            }
            Self::Memory(reader) => reader.seek(position),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequential_reads_then_seek_and_clone_preserve_every_byte() -> io::Result<()> {
        let mut file = tempfile::tempfile()?;
        let bytes: Vec<_> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::io::Write::write_all(&mut file, &bytes)?;
        let mut reader = Reader::new(file)?;
        let mut position = 0;
        for _ in 0..10 {
            let mut actual = [0; 8192];
            reader.read_exact(&mut actual)?;
            assert_eq!(actual, bytes[position..position + actual.len()]);
            position += actual.len();
        }
        assert_eq!(reader.stream_position()?, position as u64);
        let mut clone = reader.clone();
        let mut actual = vec![0; 70_001];
        clone.read_exact(&mut actual)?;
        assert_eq!(actual, bytes[position..position + actual.len()]);
        assert_eq!(reader.stream_position()?, position as u64);
        reader.seek(SeekFrom::Current(-17))?;
        reader.read_exact(&mut actual)?;
        assert_eq!(actual, bytes[position - 17..position - 17 + actual.len()]);
        reader.seek(SeekFrom::Start(19))?;
        let mut small = [0; 61];
        reader.read_exact(&mut small)?;
        assert_eq!(small, bytes[19..80]);
        assert!(reader.seek(SeekFrom::Current(i64::MIN)).is_err());
        assert_eq!(reader.stream_position()?, 80);
        reader.seek(SeekFrom::End(-100))?;
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail)?;
        assert_eq!(tail, bytes[bytes.len() - 100..]);
        Ok(())
    }

    #[test]
    fn cached_boundaries_and_concurrent_clones() -> io::Result<()> {
        for length in [0, 127, 128, 129] {
            let mut file = tempfile::tempfile()?;
            let bytes: Vec<_> = (0..length).map(|i| (i % 251) as u8).collect();
            std::io::Write::write_all(&mut file, &bytes)?;
            let reader = Reader::cached(file, 128)?;
            assert_eq!(matches!(&reader, Reader::Memory(_)), length <= 128);
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                for worker in 0..8 {
                    let mut clone = reader.clone();
                    let bytes = &bytes;
                    handles.push(scope.spawn(move || -> io::Result<()> {
                        for step in 0..50 {
                            let position = (worker * 17 + step * 7) % (bytes.len() + 1);
                            clone.seek(SeekFrom::Start(position as u64))?;
                            let mut actual = Vec::new();
                            clone.by_ref().take(13).read_to_end(&mut actual)?;
                            assert_eq!(actual, bytes[position..(position + 13).min(bytes.len())]);
                        }
                        Ok(())
                    }));
                }
                for handle in handles {
                    handle.join().unwrap()?;
                }
                Ok::<_, io::Error>(())
            })?;
        }
        Ok(())
    }
    #[test]
    fn seeks_match_unbuffered_cursor_at_boundaries() -> io::Result<()> {
        let mut file = tempfile::tempfile()?;
        let bytes: Vec<_> = (0..700_000).map(|i| (i % 251) as u8).collect();
        std::io::Write::write_all(&mut file, &bytes)?;
        let mut reader = Reader::new(file)?;
        let mut expected = io::Cursor::new(&bytes);
        for seek in [
            SeekFrom::Start(13),
            SeekFrom::Current(-100),
            SeekFrom::Start(262_140),
            SeekFrom::Current(-257),
            SeekFrom::End(-13),
            SeekFrom::End(10),
            SeekFrom::Current(-1000),
            SeekFrom::Start(0),
        ] {
            assert_eq!(reader.seek(seek)?, expected.seek(seek)?);
            let mut actual_bytes = Vec::new();
            let mut expected_bytes = Vec::new();
            reader.by_ref().take(257).read_to_end(&mut actual_bytes)?;
            expected
                .by_ref()
                .take(257)
                .read_to_end(&mut expected_bytes)?;
            assert_eq!(actual_bytes, expected_bytes);
        }
        assert!(reader.seek(SeekFrom::Current(i64::MIN)).is_err());
        assert_eq!(reader.seek(SeekFrom::Start(u64::MAX))?, u64::MAX);
        assert_eq!(reader.seek(SeekFrom::Start(257))?, 257);
        let mut clone = reader.clone();
        let mut actual = [0; 31];
        clone.read_exact(&mut actual)?;
        assert_eq!(actual, bytes[257..288]);
        Ok(())
    }
    #[test]
    fn buffered_clones_have_independent_concurrent_positions() -> io::Result<()> {
        let mut file = tempfile::tempfile()?;
        let bytes: Vec<_> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        std::io::Write::write_all(&mut file, &bytes)?;
        let mut reader = Reader::new(file)?;
        let mut prefix = [0; 13];
        reader.read_exact(&mut prefix)?;
        let mut clone = reader.clone();
        reader.seek(SeekFrom::End(-20))?;
        clone.read_exact(&mut prefix)?;
        assert_eq!(prefix, bytes[13..26]);
        std::thread::scope(|scope| {
            let mut tasks = Vec::new();
            for worker in 0..8 {
                let mut reader = reader.clone();
                let bytes = &bytes;
                tasks.push(scope.spawn(move || -> io::Result<()> {
                    for step in 0..100 {
                        let position = (worker * 104729 + step * 7919) % (bytes.len() - 257);
                        reader.seek(SeekFrom::Start(position as u64))?;
                        let mut recovered = [0; 257];
                        reader.read_exact(&mut recovered)?;
                        assert_eq!(recovered, bytes[position..position + 257]);
                    }
                    Ok(())
                }));
            }
            for task in tasks {
                task.join().unwrap()?;
            }
            Ok::<_, io::Error>(())
        })?;
        assert!(reader.seek(SeekFrom::End(-2 * 1024 * 1024)).is_err());
        Ok(())
    }
}
