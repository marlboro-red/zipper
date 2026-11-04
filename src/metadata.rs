/// Which metadata an extraction restores beside file contents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Metadata {
    /// Restore what the archive carries: Unix modes, Windows attributes,
    /// modification times, and on Windows the archive's Mark of the Web.
    #[default]
    Restore,
    /// Write contents only, for files that a build or runtime reads. Files
    /// get mode 0o666, or 0o777 when the archive marks them executable, and
    /// folders the default mode, all less the umask. No Windows attribute,
    /// such as read-only or hidden, no modification time and no
    /// `Zone.Identifier` stream is written. Links are still created.
    ContentOnly,
}

const ANY_EXECUTE: u32 = 0o111;

impl Metadata {
    pub(crate) fn restores(self) -> bool {
        self == Metadata::Restore
    }

    pub(crate) fn file_mode(self, mode: Option<u32>) -> Option<u32> {
        match self {
            Metadata::Restore => mode,
            Metadata::ContentOnly => mode.map(|mode| {
                if mode & ANY_EXECUTE != 0 {
                    0o777
                } else {
                    0o666
                }
            }),
        }
    }

    pub(crate) fn directory_mode(self, mode: Option<u32>) -> Option<u32> {
        mode.filter(|_| self.restores())
    }

    pub(crate) fn windows_attributes(self, attributes: u32) -> u32 {
        if self.restores() { attributes } else { 0 }
    }

    pub(crate) fn time<T>(self, time: Option<T>) -> Option<T> {
        time.filter(|_| self.restores())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_only_keeps_exactly_the_execute_bit_of_every_mode() {
        for mode in 0..=0o7777u32 {
            assert_eq!(Metadata::Restore.file_mode(Some(mode)), Some(mode));
            let plain = Metadata::ContentOnly.file_mode(Some(mode)).unwrap();
            assert_eq!(
                plain,
                if mode & 0o111 == 0 { 0o666 } else { 0o777 },
                "{mode:o}"
            );
            assert_eq!(Metadata::ContentOnly.directory_mode(Some(mode)), None);
            assert_eq!(Metadata::Restore.directory_mode(Some(mode)), Some(mode));
        }
        assert_eq!(Metadata::ContentOnly.file_mode(None), None);
        assert_eq!(Metadata::ContentOnly.windows_attributes(0x27), 0);
        assert_eq!(Metadata::ContentOnly.time(Some(5)), None);
        assert_eq!(Metadata::Restore.time(Some(5)), Some(5));
    }
}
