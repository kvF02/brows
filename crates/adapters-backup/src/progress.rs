use brows_application::BackupTaskControl;
use std::io::{self, Read, Write};

pub(super) struct ProgressReader<'a, R> {
    pub inner: R,
    pub control: &'a BackupTaskControl,
}
pub(super) struct ProgressWriter<'a, W> {
    pub inner: W,
    pub control: &'a BackupTaskControl,
}

fn check(control: &BackupTaskControl) -> io::Result<()> {
    control
        .checkpoint()
        .map_err(|_| io::Error::other("backup operation cancelled"))
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        check(self.control)?;
        let bytes = self.inner.read(buffer)?;
        self.control.processed(bytes);
        Ok(bytes)
    }
}

impl<W: Write> Write for ProgressWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        check(self.control)?;
        let written = self.inner.write(bytes)?;
        self.control.processed(written);
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        check(self.control)?;
        self.inner.flush()
    }
}
