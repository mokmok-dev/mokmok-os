use std::ffi::OsStr;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Config {
    pub path: PathBuf,
    /// The database every event is committed to before a subscriber can see it. Kept apart from
    /// `path`: the socket lives in `XDG_RUNTIME_DIR`, a tmpfs that loses everything on reboot,
    /// which is exactly the loss durability must survive.
    pub store: PathBuf,
    pub limits: Limits,
    pub retention: Retention,
    pub timeouts: Timeouts,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub connections: NonZeroUsize,
    pub topics: NonZeroUsize,
    pub batch: NonZeroUsize,
    /// The most events one subscriber may hold un-ACKed. A pull that reaches this ceiling parks
    /// until an [`Ack`](crate::Subscription::ack) frees credit; when the checkpoint is also behind
    /// the retained floor, the pull answers the lapse alone instead and spends no credit on it.
    pub in_flight: NonZeroUsize,
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
            store: default_store_path(),
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
            in_flight: nonzero(256),
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

fn default_store_path() -> PathBuf {
    store_path_for(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn store_path_for(
    xdg_data: Option<&OsStr>,
    home: Option<&OsStr>,
) -> PathBuf {
    let home_fallback = || {
        home.filter(|dir| !dir.is_empty()).map_or_else(
            || PathBuf::from("/tmp/mokmok/bus.db"),
            |dir| PathBuf::from(dir).join(".local/share/mokmok/bus.db"),
        )
    };

    xdg_data
        .filter(|dir| !dir.is_empty())
        .map_or_else(home_fallback, |dir| {
            PathBuf::from(dir).join("mokmok/bus.db")
        })
}

#[cfg(test)]
mod tests {
    use super::{default_socket_path, default_store_path, socket_path_for, store_path_for};
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

    #[test]
    fn the_data_directory_scopes_the_database() {
        assert_eq!(
            store_path_for(Some(OsStr::new("/home/user/.local/share")), None),
            PathBuf::from("/home/user/.local/share/mokmok/bus.db"),
        );
    }

    #[test]
    fn the_data_directory_wins_over_the_home_directory() {
        assert_eq!(
            store_path_for(Some(OsStr::new("/data")), Some(OsStr::new("/home/user"))),
            PathBuf::from("/data/mokmok/bus.db"),
        );
    }

    #[test]
    fn a_missing_data_directory_falls_back_to_the_home_directory() {
        assert_eq!(
            store_path_for(None, Some(OsStr::new("/home/user"))),
            PathBuf::from("/home/user/.local/share/mokmok/bus.db"),
        );
    }

    #[test]
    fn an_empty_data_directory_falls_back_to_the_home_directory() {
        assert_eq!(
            store_path_for(Some(OsStr::new("")), Some(OsStr::new("/home/user"))),
            PathBuf::from("/home/user/.local/share/mokmok/bus.db"),
        );
    }

    #[test]
    fn a_missing_home_directory_falls_back_to_tmp() {
        assert_eq!(
            store_path_for(None, None),
            PathBuf::from("/tmp/mokmok/bus.db")
        );
    }

    #[test]
    fn an_empty_home_directory_falls_back_to_tmp() {
        assert_eq!(
            store_path_for(None, Some(OsStr::new(""))),
            PathBuf::from("/tmp/mokmok/bus.db"),
        );
    }

    #[test]
    fn the_default_store_path_names_the_database() {
        assert!(default_store_path().ends_with("mokmok/bus.db"));
    }
}
