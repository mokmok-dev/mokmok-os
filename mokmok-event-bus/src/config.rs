use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub path: PathBuf,
    pub limits: Limits,
    pub retention: Retention,
    pub timeouts: Timeouts,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub connections: NonZeroUsize,
    pub topics: NonZeroUsize,
    pub batch: NonZeroUsize,
    pub frame_bytes: NonZeroUsize,
}

#[derive(Clone, Copy, Debug)]
pub struct Retention {
    pub events: NonZeroUsize,
    pub bytes: NonZeroUsize,
    pub idle: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// Milliseconds between liveness pings.
    pub heartbeat: NonZeroU32,
    /// Milliseconds a peer has to answer a ping, and every write has to drain.
    pub deadline: NonZeroU32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: default_socket_path(),
            limits: Limits::default(),
            retention: Retention::default(),
            timeouts: Timeouts::default(),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: nonzero(512),
            topics: nonzero(1024),
            batch: nonzero(128),
            frame_bytes: nonzero(1024 * 1024),
        }
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            events: nonzero(4096),
            bytes: nonzero(16 * 1024 * 1024),
            idle: Duration::from_mins(10),
        }
    }
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            heartbeat: millis(15 * 1000),
            deadline: millis(30 * 1000),
        }
    }
}

const fn nonzero(value: usize) -> NonZeroUsize {
    assert!(value > 0, "a limit is nonzero");

    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => NonZeroUsize::MIN,
    }
}

const fn millis(value: u32) -> NonZeroU32 {
    assert!(value > 0, "a duration is nonzero");

    match NonZeroU32::new(value) {
        Some(value) => value,
        None => NonZeroU32::MIN,
    }
}

fn default_socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map_or_else(
            || PathBuf::from("/tmp/mokmok/bus.sock"),
            |dir| PathBuf::from(dir).join("mokmok/bus.sock"),
        )
}
