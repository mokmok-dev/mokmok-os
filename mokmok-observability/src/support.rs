//! Fixtures the tests share: a directory of their own, a fixed principal, and a byte sink.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mokmok_protocol::PrincipalID;
use tracing_subscriber::fmt::MakeWriter;

/// A directory unique to one test.
///
/// The process id separates concurrent runs of the suite and `name` separates the tests within a
/// run. Removing it in `Drop` rather than at the end of each test body keeps a failing assertion
/// from leaving the directory behind.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates an empty directory for the test named `name`, clearing one left by an earlier run.
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "mokmok-observability-{pid}-{name}",
            pid = std::process::id()
        ));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).expect("the test directory can be created");
        Self { path }
    }

    /// The directory, which exists until the returned guard is dropped.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

/// A principal a test can put in an entry.
pub fn actor() -> PrincipalID {
    PrincipalID::from_uuid(uuid::Uuid::from_u128(1))
}

/// A [`MakeWriter`] that keeps everything written to it.
#[derive(Clone, Default)]
pub struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Capture {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything written so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .expect("the capture is not poisoned")
            .clone()
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter(Arc::clone(&self.bytes))
    }
}

/// The writer a [`Capture`] hands out.
pub struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(
        &mut self,
        buf: &[u8],
    ) -> io::Result<usize> {
        self.0
            .lock()
            .expect("the capture is not poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
