//! Supervises one execution of one command under capabilities issued elsewhere.
//!
//! The sandbox enforces what a [`Request`] asks for and never widens it. The caller names the
//! paths it wants in [`Request::exposures`], a Capability Authority issues [`Capability`] values,
//! and [`uncovered`] is the mechanical gate the sandbox runs before it creates anything. There is
//! no default grant: the only paths the plan adds without a capability are the workload's own
//! file and the execution workspace.
//!
//! Three properties hold, and the integration tests pin each one:
//!
//! - A [`Refusal`] means no process was created. A confinement failure after the confiner
//!   started is an [`Error::Confine`], and it means the workload never ran.
//! - `Ok(Run)` from [`Sandbox::spawn`] proves the plan was accepted, not that confinement is live
//!   yet. The confiner refuses before `exec` when it cannot enforce the plan, so a workload that
//!   runs at all ran under the plan; [`Run::wait`] reports that refusal as [`Error::Confine`].
//! - A [`Completed`] means the workload possibly started. Only an [`Error::Confine`] means it did
//!   not run; [`Completed::started`] reports what the confiner said about the exec attempt.
//! - The process tree is **not** contained against a workload that calls `setsid` itself. The
//!   syscall needs no binary, and the child it spawns leaves the group every signal here goes to.
//!   A pid namespace (`unshare(CLONE_NEWUSER | CLONE_NEWPID)` in the confiner, plus an init that
//!   forwards signals) is the fix, and it is future work.
//!
//! The plan names resolved paths, so the gate and the ruleset mean the same object. A real
//! directory swapped into a path between the supervisor's resolution and the confiner's open is
//! outside this contract: closing it would need a file descriptor pinned across the two processes.
//!
//! A confined workload has no TCP. The ruleset handles the network access set and adds no network
//! rule, so `BindTcp` and `ConnectTcp` are denied to everything the workload runs. `AccessNet` has
//! no UDP right and no `AF_UNIX` right, so a workload can still send UDP to a literal address and
//! reach a Unix socket. The floor is Linux 6.7: on an older kernel the sandbox refuses rather than
//! confine a workload that could still dial out.
//!
//! Resource limits are the sandbox's own per-execution [`Limits`] and [`Request::deadline`]. The
//! agent's execution budget is a different responsibility and lives with the agent runtime.
//!
//! ```no_run
//! use std::path::PathBuf;
//! use std::time::Duration;
//!
//! use mokmok_protocol::ExecutionID;
//! use mokmok_sandbox::{
//!     Access, Capability, Completed, Config, Confined, Environment, Exposure, Request, Sandbox,
//! };
//! use uuid::timestamp::{Timestamp, context::NoContext};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let execution = ExecutionID::new(Timestamp::now(NoContext));
//! let sandbox = Confined::new(Config::default())?;
//!
//! let mut request = Request::new(execution, "/bin/sh");
//! request.args = vec!["-c".into(), "printf hello > out.txt".into()];
//! request.environment = Environment::Extend(vec![("LANG".into(), "C".into())]);
//! request.exposures = vec![
//!     Exposure::read_execute("/nix/store"),
//!     Exposure::read_write("/dev/null"),
//! ];
//! request.artifacts = vec![PathBuf::from("out.txt")];
//! request.deadline = Some(Duration::from_secs(30));
//!
//! // Issued separately from the request, and by whoever owns the authority: the gate refuses an
//! // exposure this set does not cover.
//! let issued = [
//!     Capability::issued("/nix/store", Access::READ_EXECUTE),
//!     Capability::issued("/dev/null", Access::READ_WRITE),
//! ];
//!
//! let run = sandbox.spawn(&request, &issued)?;
//! let completed: Completed = run.wait().await?;
//!
//! println!("{} ended as {:?}", completed.id().as_uuid(), completed.outcome());
//! print!("{}", completed.stdout().text());
//! for artifact in completed.artifacts() {
//!     println!("{} is {} bytes", artifact.path().display(), artifact.bytes());
//! }
//!
//! completed.cleanup()?;
//! # Ok(())
//! # }
//! ```

// Internal types are `pub`, not `pub(crate)`: the workspace denies
// `clippy::redundant_pub_crate`, and a private module already keeps them crate-local.
mod artifact;
mod capability;
mod config;
mod confine;
mod limits;
mod output;
mod plan;
mod request;
mod run;
mod sandbox;
mod workspace;

pub use artifact::Artifact;
pub use capability::{Access, Capability, Exposure, resolve, uncovered};
pub use config::Config;
pub use confine::run;
pub use limits::Limits;
pub use output::Captured;
pub use plan::{Reached, open_resolved, open_resolved_dir};
pub use request::{Environment, Request};
pub use run::{Completed, Error, Failure, Outcome, Run, Signal, Started};
pub use sandbox::{Confined, LimitsOnly, Refusal, Sandbox};
