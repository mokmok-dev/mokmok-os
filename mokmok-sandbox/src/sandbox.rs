use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::artifact;
use crate::capability::{Capability, Exposure, uncovered};
use crate::config::Config;
use crate::limits::Limits;
use crate::plan;
use crate::request::Request;
use crate::run::{Reporting, Run};
use crate::workspace::Workspace;

/// Starts one execution and hands back the running workload.
pub trait Sandbox {
    /// Starts one execution.
    ///
    /// The plan covers the workload's own file and its workspace; every other path it may reach
    /// must be an exposure covered by an issued capability. Two exposures are implicit and need no
    /// capability: the workload's own file, because a program that cannot be read cannot run, and
    /// the workspace, because that is the sandbox's own scope.
    ///
    /// `Ok` proves the plan was accepted, not that confinement is live yet. The confiner refuses
    /// before `exec` when it cannot enforce the plan, so a workload that runs at all ran under the
    /// plan, and [`Run::wait`] reports that refusal as [`Error::Confine`](crate::Error::Confine).
    ///
    /// The workload's working directory is its workspace, its stdin is `/dev/null`, and
    /// [`Request::env`] is added to the inherited environment.
    ///
    /// # Errors
    ///
    /// Returns a [`Refusal`], having created nothing, when an exposure is uncovered, a declared
    /// artifact is not a plain relative path, the program is not absolute, the execution
    /// directory cannot be prepared, or the child cannot be started.
    fn spawn(
        &self,
        request: &Request,
        issued: &[Capability],
    ) -> Result<Run, Refusal>;
}

/// The enforcement backend: one Landlock-confined execution per spawn.
pub struct Confined {
    config: Config,
}

impl Confined {
    /// Refuses when the confiner binary is missing or this kernel cannot confine.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal::Confiner`] when the configured path is not an executable file, and
    /// [`Refusal::Kernel`] when the kernel cannot build the ruleset the confiner enforces.
    pub fn new(config: Config) -> Result<Self, Refusal> {
        if !is_executable_file(&config.confiner) {
            return Err(Refusal::Confiner(config.confiner));
        }

        if plan::probe(&config.confiner, &config.root).is_err() {
            return Err(Refusal::Kernel);
        }

        Ok(Self { config })
    }
}

impl Sandbox for Confined {
    fn spawn(
        &self,
        request: &Request,
        issued: &[Capability],
    ) -> Result<Run, Refusal> {
        check(request, issued)?;

        let workspace =
            Workspace::create(&self.config.root, request.execution).map_err(|source| {
                Refusal::Setup {
                    path: Workspace::directory_path(&self.config.root, request.execution),
                    source,
                }
            })?;
        let plan_path = workspace.plan();
        let written = plan::Plan::write(&plan_path, &plan::Plan::of(request, workspace.work()));

        if let Err(source) = written {
            let _ = workspace.remove();

            return Err(Refusal::Setup {
                path: plan_path,
                source,
            });
        }

        let mut command = Command::new(&self.config.confiner);
        command.arg(&plan_path);
        configure(&mut command, request, &workspace);
        // The confiner is deliberately left in the supervisor's group: it calls `setsid` itself,
        // and `setsid` fails with `EPERM` for a process that already leads a group. That is what
        // makes the workload's group id the child pid the supervisor holds.
        let child = command.spawn().map_err(|source| {
            let _ = workspace.remove();

            Refusal::Spawn(source)
        })?;

        Run::start(
            child,
            workspace,
            request,
            self.config.grace,
            Reporting::Confiner,
        )
        .map_err(Refusal::Spawn)
    }
}

/// The degraded backend for a host whose kernel cannot confine: it applies the resource limits
/// through a shell preamble and enforces no filesystem exposure at all.
///
/// It runs the same capability gate, because refusing an exposure the issued set does not cover is
/// the fail-closed direction; what it cannot do is enforce any of it.
pub struct LimitsOnly {
    config: Config,
}

impl LimitsOnly {
    #[must_use]
    pub const fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Sandbox for LimitsOnly {
    fn spawn(
        &self,
        request: &Request,
        issued: &[Capability],
    ) -> Result<Run, Refusal> {
        check(request, issued)?;

        let workspace =
            Workspace::create(&self.config.root, request.execution).map_err(|source| {
                Refusal::Setup {
                    path: Workspace::directory_path(&self.config.root, request.execution),
                    source,
                }
            })?;
        let mut command = Command::new(SHELL);
        command
            .arg("-c")
            .arg(preamble(&request.limits))
            .arg(&request.program)
            .args(&request.args)
            // A shell cannot `setsid`, so it is made a group leader and `exec` keeps that group
            // across the replacement.
            .process_group(0);
        configure(&mut command, request, &workspace);
        let child = command.spawn().map_err(|source| {
            let _ = workspace.remove();

            Refusal::Spawn(source)
        })?;

        Run::start(
            child,
            workspace,
            request,
            self.config.grace,
            Reporting::Exit,
        )
        .map_err(Refusal::Spawn)
    }
}

const SHELL: &str = "sh";

/// Why the sandbox declined to start one execution.
///
/// A refusal means no process was created, and that the execution directory was not left behind.
/// A confinement failure after the child started is [`Error::Confine`](crate::Error::Confine)
/// instead, and it means the workload never ran.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("the exposure at {} is not covered by an issued capability", .0.as_path().display())]
    Uncovered(Exposure),
    #[error("the artifact path {} is not a plain path relative to the workspace", .0.display())]
    Artifact(PathBuf),
    #[error("the program {} is not an absolute path", .0.display())]
    Program(PathBuf),
    #[error("the confiner {} is not an executable file", .0.display())]
    Confiner(PathBuf),
    #[error("this kernel cannot enforce a Landlock V3 ruleset")]
    Kernel,
    #[error("{} could not be prepared: {source}", path.display())]
    Setup { path: PathBuf, source: io::Error },
    #[error("the child could not be started: {0}")]
    Spawn(io::Error),
}

/// Everything the two backends refuse the same way, before they create anything.
fn check(
    request: &Request,
    issued: &[Capability],
) -> Result<(), Refusal> {
    if let Some(exposure) = uncovered(&request.exposures, issued) {
        return Err(Refusal::Uncovered(exposure.clone()));
    }

    if let Some(path) = request
        .artifacts
        .iter()
        .find(|path| !artifact::is_declarable(path))
    {
        return Err(Refusal::Artifact(path.clone()));
    }

    if !request.program.is_absolute() {
        return Err(Refusal::Program(request.program.clone()));
    }

    Ok(())
}

fn configure(
    command: &mut Command,
    request: &Request,
    workspace: &Workspace,
) {
    command
        .current_dir(workspace.work())
        .envs(
            request
                .env
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
}

/// The shell the degraded backend runs a request through, in `ulimit` arguments.
///
/// A count of zero means unlimited to `ulimit`, so a limit that rounded down to zero would become
/// no limit at all. `ulimit -v` counts KiB and `ulimit -f` counts 512-byte blocks, so both round
/// up.
fn preamble(limits: &Limits) -> String {
    let mut lines = Vec::new();

    if let Some(bytes) = limits.address_space {
        lines.push(format!("ulimit -v {}", bytes.get().div_ceil(1024)));
    }
    if let Some(seconds) = limits.cpu {
        lines.push(format!("ulimit -t {}", seconds.get()));
    }
    if let Some(bytes) = limits.file_size {
        lines.push(format!("ulimit -f {}", bytes.get().div_ceil(512)));
    }
    if let Some(fds) = limits.open_files {
        lines.push(format!("ulimit -n {}", fds.get()));
    }

    lines.push("exec \"$0\" \"$@\"".to_owned());
    lines.join("\n")
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::preamble;
    use crate::limits::Limits;
    use std::num::{NonZeroU64, NonZeroUsize};

    fn nonzero(value: u64) -> Option<NonZeroU64> {
        NonZeroU64::new(value)
    }

    fn limits(
        address_space: Option<u64>,
        cpu: Option<u64>,
        file_size: Option<u64>,
        open_files: Option<u64>,
    ) -> Limits {
        Limits {
            address_space: address_space.and_then(nonzero),
            cpu: cpu.and_then(nonzero),
            file_size: file_size.and_then(nonzero),
            open_files: open_files.and_then(nonzero),
            output: NonZeroUsize::new(1024).unwrap(),
        }
    }

    #[test]
    fn the_preamble_names_every_limit_it_was_given() {
        assert_eq!(
            preamble(&limits(
                Some(64 * 1024 * 1024),
                Some(1),
                Some(1024 * 1024),
                Some(64)
            )),
            "ulimit -v 65536\nulimit -t 1\nulimit -f 2048\nulimit -n 64\nexec \"$0\" \"$@\"",
        );
    }

    #[test]
    fn the_preamble_leaves_out_what_the_request_left_alone() {
        assert_eq!(
            preamble(&limits(None, None, None, None)),
            "exec \"$0\" \"$@\"",
        );
    }

    #[test]
    fn a_limit_below_one_block_still_names_one() {
        // `ulimit -f 0` and `ulimit -v 0` mean unlimited, so a small limit must not round to zero.
        assert_eq!(
            preamble(&limits(Some(1), None, Some(1), None)),
            "ulimit -v 1\nulimit -f 1\nexec \"$0\" \"$@\"",
        );
    }
}
