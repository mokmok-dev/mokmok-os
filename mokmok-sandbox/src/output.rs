use std::borrow::Cow;
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinHandle;

/// Bytes one stream produced, up to the capture cap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Captured {
    #[must_use]
    pub const fn new(
        bytes: Vec<u8>,
        truncated: bool,
    ) -> Self {
        Self { bytes, truncated }
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The captured bytes as text, with any byte that is not UTF-8 replaced.
    #[must_use]
    pub fn text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.bytes)
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

pub struct Pump(JoinHandle<io::Result<Captured>>);

impl Pump {
    pub fn spawn<R>(
        reader: R,
        cap: usize,
    ) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        Self(tokio::spawn(drain(reader, cap)))
    }

    /// Awaited through a mutable borrow so the owning `Run` can keep its `Drop`.
    pub async fn join(&mut self) -> io::Result<Captured> {
        match (&mut self.0).await {
            Ok(captured) => captured,
            Err(error) => Err(io::Error::other(error)),
        }
    }
}

const CHUNK: usize = 16 * 1024;

async fn drain<R>(
    mut reader: R,
    cap: usize,
) -> io::Result<Captured>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = [0_u8; CHUNK];

    loop {
        let read = reader.read(&mut chunk).await?;

        if read == 0 {
            break;
        }

        // Reading past the cap rather than stopping at it: a pump that stopped would leave the
        // pipe full and deadlock a workload that writes more than the cap.
        let kept = read.min(cap.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..kept]);
        truncated |= kept < read;
    }

    Ok(Captured::new(bytes, truncated))
}
