use std::ffi::OsStr;
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
    socket_path_for(std::env::var_os("XDG_RUNTIME_DIR").as_deref())
}

/// Decides the socket location from a runtime directory alone, so the fallback rules are
/// reachable from a test even though `set_var` is unsafe and denied here.
fn socket_path_for(xdg: Option<&OsStr>) -> PathBuf {
    xdg.filter(|dir| !dir.is_empty()).map_or_else(
        || PathBuf::from("/tmp/mokmok/bus.sock"),
        |dir| PathBuf::from(dir).join("mokmok/bus.sock"),
    )
}

#[cfg(test)]
mod tests {
    use super::{default_socket_path, socket_path_for};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    #[test]
    fn the_runtime_directory_scopes_the_socket() {
        assert_eq!(
            socket_path_for(Some(OsStr::new("/run/user/1000"))),
            PathBuf::from("/run/user/1000/mokmok/bus.sock"),
        );
    }

    #[test]
    fn an_empty_runtime_directory_falls_back_to_tmp() {
        assert_eq!(
            socket_path_for(Some(OsStr::new(""))),
            PathBuf::from("/tmp/mokmok/bus.sock"),
        );
    }

    #[test]
    fn a_missing_runtime_directory_falls_back_to_tmp() {
        assert_eq!(socket_path_for(None), PathBuf::from("/tmp/mokmok/bus.sock"));
    }

    #[test]
    fn the_default_socket_path_names_the_bus() {
        assert!(default_socket_path().ends_with("mokmok/bus.sock"));
    }
}
