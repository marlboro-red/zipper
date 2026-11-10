use std::io::{BufRead, Read};

pub(crate) const OUTPUT_PER_EXTRA_STREAM: u64 = 4096;
const BACKEND_HINT: &str = "pass --zip-engine backend to read it with 7-Zip";

pub(crate) struct Bzip2Streams<R: BufRead> {
    decoder: Option<bzip2::bufread::BzDecoder<R>>,
    streams: u64,
    decoded: u64,
}

impl<R: BufRead> Read for Bzip2Streams<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        while let Some(decoder) = &mut self.decoder {
            let read = decoder.read(buffer)?;
            if read > 0 || buffer.is_empty() {
                self.decoded += read as u64;
                return Ok(read);
            }
            let mut input = self.decoder.take().expect("a decoder").into_inner();
            if input.fill_buf()?.is_empty() {
                return Ok(0);
            }
            if self.streams > self.decoded / OUTPUT_PER_EXTRA_STREAM {
                return Err(std::io::Error::other(format!(
                    "bzip2 has more streams than {OUTPUT_PER_EXTRA_STREAM} decoded bytes each; {BACKEND_HINT}"
                )));
            }
            self.streams += 1;
            self.decoder = Some(bzip2::bufread::BzDecoder::new(input));
        }
        Ok(0)
    }
}

impl<R: BufRead> Bzip2Streams<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            decoder: Some(bzip2::bufread::BzDecoder::new(input)),
            streams: 1,
            decoded: 0,
        }
    }
}
