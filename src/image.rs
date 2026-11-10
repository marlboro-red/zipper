use anyhow::Result;

pub(crate) trait Volume {
    fn size(&self) -> u64;
    fn read_exact_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<()>;
}
