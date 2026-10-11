use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the sandbox keeps its execution directories, which confiner runs them, and how long a
/// workload that was sent `SIGTERM` has before it is sent `SIGKILL`.
#[derive(Clone, Debug)]
pub struct Config {
    /// Parent of every execution directory.
    pub root: PathBuf,
    pub confiner: PathBuf,
    /// How long a workload that was sent `SIGTERM` has before it is sent `SIGKILL`.
    pub grace: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: default_root(),
            confiner: default_confiner(),
            grace: Duration::from_secs(2),
        }
    }
}

fn default_root() -> PathBuf {
    root_for(std::env::var_os("XDG_RUNTIME_DIR").as_deref())
}

/// Decides the root from a runtime directory alone, so the fallback rules are reachable from a
/// test even though `set_var` is unsafe and denied here.
fn root_for(xdg: Option<&OsStr>) -> PathBuf {
    xdg.filter(|dir| !dir.is_empty()).map_or_else(
        || PathBuf::from("/tmp/mokmok/sandbox"),
        |dir| PathBuf::from(dir).join("mokmok/sandbox"),
    )
}

fn default_confiner() -> PathBuf {
    confiner_for(std::env::current_exe().ok().as_deref())
}

/// A missing `current_exe` leaves an empty path; [`Confined::new`](crate::Confined::new) is what
/// refuses it.
fn confiner_for(exe: Option<&Path>) -> PathBuf {
    exe.and_then(Path::parent)
        .map_or_else(PathBuf::new, |dir| dir.join(confiner_name()))
}

const fn confiner_name() -> &'static str {
    "mokmok-sandbox-confine"
}

#[cfg(test)]
mod tests {
    use super::{Config, confiner_for, root_for};
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_runtime_directory_scopes_the_root() {
        assert_eq!(
            root_for(Some(OsStr::new("/run/user/1000"))),
            PathBuf::from("/run/user/1000/mokmok/sandbox"),
        );
    }

    #[test]
    fn an_empty_runtime_directory_falls_back_to_tmp() {
        assert_eq!(
            root_for(Some(OsStr::new(""))),
            PathBuf::from("/tmp/mokmok/sandbox"),
        );
    }

    #[test]
    fn a_missing_runtime_directory_falls_back_to_tmp() {
        assert_eq!(root_for(None), PathBuf::from("/tmp/mokmok/sandbox"));
    }

    #[test]
    fn the_confiner_is_the_sibling_of_the_running_executable() {
        assert_eq!(
            confiner_for(Some(Path::new("/usr/libexec/mokmok-bus"))),
            PathBuf::from("/usr/libexec/mokmok-sandbox-confine"),
        );

        let exe = std::env::current_exe().unwrap();
        assert_eq!(Config::default().confiner, confiner_for(Some(&exe)));
        assert!(
            Config::default()
                .confiner
                .ends_with("mokmok-sandbox-confine")
        );
    }

    #[test]
    fn a_missing_executable_leaves_an_empty_confiner_path() {
        assert_eq!(confiner_for(None), PathBuf::new());
    }
}
