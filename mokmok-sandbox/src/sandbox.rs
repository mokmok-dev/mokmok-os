use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use mokmok_protocol::ExecutionID;
use tokio::process::Command;

use crate::artifact;
use crate::capability::{self, Capability, Exposure, uncovered_index};
use crate::config::Config;
use crate::limits::Limits;
use crate::plan;
use crate::request::{Environment, Request};
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
    /// Both backends resolve the exposures and the issued capabilities before they gate, and the
    /// plan records the resolved exposures, so the gate and the rule the confiner builds name the
    /// same object. An exposure that cannot be resolved is refused, not dropped.
    ///
    /// `Ok` proves the plan was accepted, not that confinement is live yet. The confiner refuses
    /// before `exec` when it cannot enforce the plan, so a workload that runs at all ran under the
    /// plan, and [`Run::wait`] reports that refusal as [`Error::Confine`](crate::Error::Confine).
    /// An `Ok` means the workload possibly started; only such a confinement failure means it did
    /// not run.
    ///
    /// The workload's working directory is [`Request::workdir`] when the request names one, and
    /// its workspace when it does not; its stdin is `/dev/null`, and its environment is built as
    /// [`Request::environment`] says.
    ///
    /// # Errors
    ///
    /// Returns a [`Refusal`], having created nothing, when an exposure is uncovered or cannot be
    /// resolved, a declared artifact is not a plain relative path, the program is not absolute,
    /// the workload directory named in [`Request::workdir`] is not absolute, is not a directory,
    /// or lies outside every exposure, the execution id is already in use, the execution
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

        // Every rule the probe builds is opened without following a symlink, so its workspace has
        // to be a directory that is there. The root is not created until the first execution, and
        // the confiner's own directory is one the check above proved exists.
        let probed = config
            .confiner
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("/"));

        if plan::probe(&config.confiner, probed).is_err() {
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
        let checked = check(request, issued)?;
        let workspace = prepare_workspace(&self.config.root, request.execution)?;
        let work = resolve_work(&workspace)?;
        let directory = checked.workdir.unwrap_or_else(|| work.clone());
        let plan_path = workspace.plan();
        let written = plan::Plan::write(
            &plan_path,
            &plan::Plan::of(
                request,
                &expose_of(&request.program),
                &work,
                &checked.exposures,
            ),
        );

        if let Err(source) = written {
            let _ = workspace.remove();

            return Err(Refusal::Setup {
                path: plan_path,
                source,
            });
        }

        let mut command = Command::new(&self.config.confiner);
        command.arg(&plan_path);
        configure(&mut command, request, &directory);
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
            directory,
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
        let checked = check(request, issued)?;

        let workspace = prepare_workspace(&self.config.root, request.execution)?;
        let directory = checked
            .workdir
            .unwrap_or_else(|| workspace.work().to_path_buf());
        let mut command = Command::new(SHELL);
        command
            .arg("-c")
            .arg(preamble(&request.limits))
            .arg(&request.program)
            .args(&request.args)
            // A shell cannot `setsid`, so it is made a group leader and `exec` keeps that group
            // across the replacement.
            .process_group(0);
        configure(&mut command, request, &directory);
        let child = command.spawn().map_err(|source| {
            let _ = workspace.remove();

            Refusal::Spawn(source)
        })?;

        Run::start(
            child,
            workspace,
            directory,
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
    #[error(
        "the exposure at {} is not covered by an issued capability: it resolves to {}",
        .0.as_path().display(),
        .1.display()
    )]
    Uncovered(Exposure, PathBuf),
    #[error("the exposure at {} could not be resolved: {source}", .exposure.as_path().display())]
    Unresolved {
        exposure: Exposure,
        source: io::Error,
    },
    #[error("the execution {} is already in use", .0.as_uuid())]
    Exists(ExecutionID),
    #[error("the artifact path {} is not a plain path relative to the workspace", .0.display())]
    Artifact(PathBuf),
    #[error("the workload directory {} cannot be used: {message}", path.display())]
    Workdir { path: PathBuf, message: String },
    #[error("the program {} is not an absolute path", .0.display())]
    Program(PathBuf),
    #[error("the confiner {} is not an executable file", .0.display())]
    Confiner(PathBuf),
    #[error(
        "this kernel cannot enforce the Landlock ruleset the confiner needs: Linux 6.7 or newer is required"
    )]
    Kernel,
    #[error("{} could not be prepared: {source}", path.display())]
    Setup { path: PathBuf, source: io::Error },
    #[error("the child could not be started: {0}")]
    Spawn(io::Error),
}

/// Everything the two backends do the same way, before they create anything.
///
/// The exposures are resolved, which is what the plan records: the gate, the plan, and the rule
/// the confiner builds then name the same object.
struct Checked {
    exposures: Vec<Exposure>,
    workdir: Option<PathBuf>,
}

fn check(
    request: &Request,
    issued: &[Capability],
) -> Result<Checked, Refusal> {
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

    let mut resolved = Vec::with_capacity(request.exposures.len());
    for exposure in &request.exposures {
        match capability::resolve(exposure.as_path()) {
            Ok(path) => resolved.push(Exposure::path(path, exposure.access())),
            Err(source) => {
                return Err(Refusal::Unresolved {
                    exposure: exposure.clone(),
                    source,
                });
            },
        }
    }

    let resolved_issued: Vec<Capability> = issued
        .iter()
        .filter_map(
            |capability| match capability::resolve(capability.as_path()) {
                Ok(path) => Some(Capability::issued(path, capability.access())),
                Err(error) => {
                    // A capability nobody can resolve names no object, so it covers nothing. The
                    // exposure that needed it is refused below instead of being passed here.
                    tracing::debug!(
                        path = %capability.as_path().display(),
                        %error,
                        "an issued capability that cannot be resolved covers nothing",
                    );

                    None
                },
            },
        )
        .collect();

    if let Some(index) = uncovered_index(&resolved, &resolved_issued) {
        return Err(Refusal::Uncovered(
            request.exposures[index].clone(),
            resolved[index].as_path().to_path_buf(),
        ));
    }

    let workdir = workdir_of(request.workdir.as_deref(), &resolved)?;

    Ok(Checked {
        exposures: resolved,
        workdir,
    })
}

/// Validates the workload directory against the resolved exposures, and resolves it so the cwd
/// and the rules name the same object.
fn workdir_of(
    workdir: Option<&Path>,
    exposures: &[Exposure],
) -> Result<Option<PathBuf>, Refusal> {
    let Some(path) = workdir else {
        return Ok(None);
    };

    if !path.is_absolute() {
        return Err(Refusal::Workdir {
            path: path.to_path_buf(),
            message: "it is not an absolute path".to_owned(),
        });
    }

    let resolved = match capability::resolve(path) {
        Ok(resolved) if resolved.is_dir() => resolved,
        Ok(_) | Err(_) => {
            return Err(Refusal::Workdir {
                path: path.to_path_buf(),
                message: "it is not a directory".to_owned(),
            });
        },
    };

    if !exposures
        .iter()
        .any(|exposure| resolved.starts_with(exposure.as_path()))
    {
        return Err(Refusal::Workdir {
            path: path.to_path_buf(),
            message: "it is outside every exposure".to_owned(),
        });
    }

    Ok(Some(resolved))
}

/// Creates this execution's own directory, mapping an id already in use to its own refusal so a
/// durable-execution caller can tell a live id from a filesystem refusal.
fn prepare_workspace(
    root: &Path,
    execution: ExecutionID,
) -> Result<Workspace, Refusal> {
    Workspace::create(root, execution).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            Refusal::Exists(execution)
        } else {
            Refusal::Setup {
                path: Workspace::directory_path(root, execution),
                source,
            }
        }
    })
}

/// The workspace directory exists by the time the plan is written, so a resolution failure is a
/// setup failure.
fn resolve_work(workspace: &Workspace) -> Result<PathBuf, Refusal> {
    capability::resolve(workspace.work()).map_err(|source| Refusal::Setup {
        path: workspace.work().to_path_buf(),
        source,
    })
}

/// The resolved program, or the path as given when it cannot be resolved.
///
/// A program that is not there must still reach the confiner, which reports it, so a resolution
/// failure is not a refusal here.
fn expose_of(program: &Path) -> PathBuf {
    capability::resolve(program).unwrap_or_else(|_| program.to_path_buf())
}

fn configure(
    command: &mut Command,
    request: &Request,
    directory: &Path,
) {
    command
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    match &request.environment {
        Environment::Extend(variables) => {
            command.envs(variables.iter().cloned());
        },
        Environment::Exact(variables) => {
            command.env_clear().envs(variables.iter().cloned());
        },
    }
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
