//! The confinement engine, run as its own binary so the workload it replaces is the process the
//! kernel confines.
//!
//! `<plan.json>` is the only argument, and the status file beside the plan is the whole reporting
//! protocol. Standard error carries only the workload's own bytes.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use landlock::RulesetStatus;
use nix::sys::resource::{Resource, setrlimit};
use nix::unistd::{execvp, setsid};

use crate::limits::Limits;
use crate::plan::text::Text;
use crate::plan::{self, Plan, StatusFile};
use crate::run::Failure;

const REFUSED: u8 = 125;

/// The confiner's whole algorithm, reading `std::env::args_os`.
///
/// This is what the packaged `mokmok-sandbox-confine` binary is three lines around, so an embedder
/// that ships its own multicall binary can call the same function.
pub fn run() -> ExitCode {
    let mut arguments = std::env::args_os();
    let _ = arguments.next();

    let Some(plan_path) = arguments.next() else {
        tracing::error!("the confiner needs the path of a plan file");

        return ExitCode::from(REFUSED);
    };

    let failure = confine(Path::new(&plan_path));
    tracing::error!(%failure, "the confiner refused the workload");

    ExitCode::from(REFUSED)
}

/// Returns only when the workload did not run.
fn confine(plan_path: &Path) -> Failure {
    let status_path = status_path(plan_path);
    let plan = match Plan::load(plan_path) {
        Ok(plan) => plan,
        Err(error) => {
            // The status path comes from the plan file's own path rather than from anything
            // inside the plan, so a plan that does not parse can still be reported.
            let failure = Failure::Plan(error.to_string());

            if let Ok(mut status) = StatusFile::create(&status_path) {
                let _ = status.refuse(&failure);
            }

            return failure;
        },
    };
    let mut status = match StatusFile::create(&status_path) {
        Ok(status) => status,
        Err(error) => return Failure::Unknown(format!("{}: {error}", status_path.display())),
    };

    if let Err(error) = status.prepared() {
        return Failure::Unknown(format!("{}: {error}", status_path.display()));
    }

    if let Err(error) = setsid() {
        return refuse(&mut status, Failure::Session(error.to_string()));
    }

    if let Err(error) = apply_limits(&plan.limits) {
        return refuse(&mut status, Failure::Limits(error));
    }

    let ruleset = match plan::ruleset(&plan) {
        Ok(ruleset) => ruleset,
        Err(failure) => return refuse(&mut status, failure),
    };

    match ruleset.restrict_self() {
        Ok(enforced) if enforced.ruleset == RulesetStatus::FullyEnforced => {
            if let Err(error) = status.confined() {
                return Failure::Unknown(format!("{}: {error}", status_path.display()));
            }
        },
        Ok(enforced) => {
            return refuse(
                &mut status,
                Failure::Ruleset(format!("the ruleset reported {:?}", enforced.ruleset)),
            );
        },
        Err(error) => return refuse(&mut status, Failure::Ruleset(error.to_string())),
    }

    // A successful exec replaces this process, so this is the last record the supervisor can see.
    if let Err(error) = status.attempted() {
        return Failure::Unknown(format!("{}: {error}", status_path.display()));
    }

    // `exec` only returns when it failed, and this is the one refusal written after the ruleset is
    // enforced, which is what the fixed-size status record is for.
    refuse(&mut status, exec(&plan))
}

/// Writes through the descriptor held across `restrict_self`, which is why a refusal is still
/// reportable once the ruleset is enforced.
fn refuse(
    status: &mut StatusFile,
    failure: Failure,
) -> Failure {
    let _ = status.refuse(&failure);

    failure
}

fn status_path(plan_path: &Path) -> PathBuf {
    plan_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("status.json")
}

/// Applies the rlimits the request named, and drops core dumps.
///
/// `RLIMIT_NPROC` is deliberately absent: it is accounted per real uid, so on a shared build uid it
/// would count unrelated processes. `RLIMIT_CORE` is zeroed unconditionally.
fn apply_limits(limits: &Limits) -> Result<(), String> {
    let named = [
        (Resource::RLIMIT_AS, limits.address_space),
        (Resource::RLIMIT_CPU, limits.cpu),
        (Resource::RLIMIT_FSIZE, limits.file_size),
        (Resource::RLIMIT_NOFILE, limits.open_files),
    ];

    for (resource, value) in named {
        if let Some(value) = value {
            let value = value.get();

            setrlimit(resource, value, value).map_err(|error| format!("{resource:?}: {error}"))?;
        }
    }

    setrlimit(Resource::RLIMIT_CORE, 0, 0).map_err(|error| format!("RLIMIT_CORE: {error}"))
}

fn exec(plan: &Plan) -> Failure {
    let program = plan.program.to_path_buf();
    let encoded = std::iter::once(plan.program.os())
        .chain(plan.args.iter().map(Text::os))
        .map(|value| CString::new(value.as_bytes()))
        .collect::<Result<Vec<_>, _>>();

    let Ok(arguments) = encoded else {
        return Failure::Exec {
            program,
            message: "the program or an argument contains a NUL byte".to_owned(),
        };
    };

    match execvp(&arguments[0], &arguments) {
        Ok(never) => match never {},
        Err(error) => Failure::Exec {
            program,
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::status_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_status_file_sits_beside_the_plan() {
        assert_eq!(
            status_path(Path::new("/run/mokmok/sandbox/id/ctl/plan.json")),
            PathBuf::from("/run/mokmok/sandbox/id/ctl/status.json"),
        );
    }
}
