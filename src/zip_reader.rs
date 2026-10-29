//! Cloneable buffered cursors over one file. Clones share the file and ZIP
//! metadata, but never a seek position. The lock covers only physical I/O.
use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct Cursor {
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

pub(crate) struct Reader(BufReader<Cursor>);
impl Reader {
    pub(crate) fn new(file: File) -> io::Result<Self> {
        let length = file.metadata()?.len();
        Ok(Self(BufReader::with_capacity(
            256 * 1024,
            Cursor {
                file: Arc::new(Mutex::new(file)),
                position: 0,
                length,
            },
        )))
    }
}
impl Clone for Reader {
    fn clone(&self) -> Self {
        let mut cursor = self.0.get_ref().clone();
        cursor.position -= self.0.buffer().len() as u64;
        Self(BufReader::with_capacity(256 * 1024, cursor))
    }
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Seek for Reader {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
