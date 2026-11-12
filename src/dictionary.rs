use std::io::Read;

pub(crate) const LARGEST_DICTIONARY: u64 = 256 * 1024 * 1024;

pub(crate) fn largest_dictionary() -> String {
    format!(
        "the largest dictionary Zipper accepts, {} MiB",
        LARGEST_DICTIONARY >> 20
    )
}

pub(crate) fn ensure_dictionary_fits(format: &str, dictionary: u64) -> std::io::Result<()> {
    if dictionary <= LARGEST_DICTIONARY {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::OutOfMemory,
        format!(
            "{format} stream declares a {dictionary}-byte dictionary, more than {}",
            largest_dictionary()
        ),
    ))
}

pub(crate) struct DecoderMemory<R>(pub(crate) R);

impl<R: Read> Read for DecoderMemory<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer).map_err(|error| {
            let over = error
                .get_ref()
                .is_some_and(|inner| inner.is::<crate::lzma::DictionaryOverLimit>());
            if over {
                std::io::Error::new(
                    std::io::ErrorKind::OutOfMemory,
                    format!(
                        "the stream needs a dictionary larger than {}",
                        largest_dictionary()
                    ),
                )
            } else {
                error
            }
        })
    }
}
