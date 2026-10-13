//! Reads the command line into the one configuration the kernel runs on.
//!
//! The command line is a system boundary, so every field is checked here and the rest of the
//! kernel trusts [`Config`]. A program must be absolute and present, because the sandbox resolves
//! a name before it gates and refuses what it cannot resolve. A directory named as reachable must
//! be one. Nothing is allowed by default: an empty allow list yields an empty policy, and an empty
//! policy denies every request.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use uuid::Uuid;

/// Where a run stops short of finishing, so that recovery can be exercised against a real crash.
///
/// Each point sits between two durable commits, so the process dies with no transaction open and
/// the log holds exactly what the boundary says it holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CrashPoint {
    /// After the turn is committed and before authority is asked: the work is recorded and
    /// nothing has run.
    BeforeDispatch,
    /// After authority allowed the execution and before its outcome is committed: the workload
    /// may have run and no result is recorded.
    AfterDispatch,
}

/// Which provider answers a session's model calls.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ProviderKind {
    /// Answers from a JSON file, so no network is reached.
    Scripted,
    /// Calls an OpenAI-compatible chat-completions endpoint over TLS.
    Ollama,
}

/// The CLI surface of `mokmokd`.
#[derive(Debug, Parser)]
#[command(name = "mokmokd", version, about = "the mokmok OS composition root")]
pub struct Args {
    /// Where the event log, the artifacts, and the bus socket live.
    #[arg(long, global = true, value_name = "DIR", default_value = "mokmok-data")]
    pub data_dir: PathBuf,
    /// A tool program the agent principal may invoke. Repeatable; nothing is allowed by default.
    #[arg(long = "allow-tool", global = true, value_name = "PROGRAM")]
    pub allow_tool: Vec<PathBuf>,
    /// A directory the workload may read and execute in. Repeatable.
    #[arg(long = "allow-path", global = true, value_name = "DIR")]
    pub allow_path: Vec<PathBuf>,
    /// A directory the workload may also write in. Repeatable.
    #[arg(long = "allow-write", global = true, value_name = "DIR")]
    pub allow_write: Vec<PathBuf>,
    /// A `NAME=VALUE` pair the confined workload is given. Repeatable, in order.
    ///
    /// The workload's environment is exactly these pairs plus `PATH`, taken from this process, and
    /// nothing else the daemon holds. A later pair naming a variable replaces an earlier one.
    #[arg(long = "tool-env", global = true, value_name = "NAME=VALUE")]
    pub tool_env: Vec<String>,
    /// The confiner binary the sandbox supervises with. Defaults to the sibling of this
    /// executable, which is where the workspace's own build puts it.
    #[arg(long, global = true, value_name = "PATH", hide = true)]
    pub confiner: Option<PathBuf>,
    /// Stop at this boundary instead of finishing. It exists for the crash-injection tests.
    ///
    /// The flag alone does nothing: the environment must also carry `MOKMOKD_CRASH=1`, and that
    /// second signal is what makes the stop deliberate, so an ordinary invocation cannot end the
    /// daemon between two commits by passing it.
    #[arg(long, value_enum, global = true, hide = true)]
    pub crash_after: Option<CrashPoint>,
    /// Which provider answers a session's model calls.
    #[arg(long, global = true, value_enum, default_value = "scripted")]
    pub provider: ProviderKind,
    /// The scripted provider's answers, as a JSON file.
    #[arg(long, global = true, value_name = "PATH")]
    pub script: Option<PathBuf>,
    /// The model id the endpoint is asked for. Required for `--provider ollama`.
    #[arg(long, global = true, value_name = "MODEL")]
    pub model: Option<String>,
    /// The OpenAI-compatible endpoint root.
    #[arg(
        long,
        global = true,
        default_value = "https://ollama.com/v1",
        value_name = "URL"
    )]
    pub endpoint: String,
    /// A file holding the endpoint credential, read at open and never from the environment.
    #[arg(long, global = true, value_name = "PATH")]
    pub provider_credential_file: Option<PathBuf>,
    /// How many answer tokens one model call may spend.
    #[arg(long, global = true, default_value_t = 4096)]
    pub max_answer_tokens: u64,
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// One invocation of the composition root.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Commit a turn and run one tool under the authority the policy issues.
    Submit(Submit),
    /// Report what the log records for one session.
    Status {
        /// The session to report on.
        #[arg(long)]
        session: String,
    },
    /// Record an unknown outcome for every execution the session left unsettled.
    Reconcile {
        /// The session to reconcile.
        #[arg(long)]
        session: String,
    },
    /// Serve the event bus and relay committed events to it.
    Serve {
        /// Where the bus listens. Defaults to a socket inside the data directory.
        #[arg(long, value_name = "PATH")]
        socket: Option<PathBuf>,
        /// Relay what is pending and exit, instead of serving until interrupted.
        #[arg(long)]
        once: bool,
    },
    /// Drive a session with a model, one bounded step at a time.
    #[command(subcommand)]
    Agent(Agent),
}

/// One agent invocation.
#[derive(Debug, Subcommand)]
pub enum Agent {
    /// Run the loop until the session closes or reaches a limit.
    Run(Run),
    /// Record a cancel, so the next run closes the session cancelled.
    Cancel {
        /// The session to cancel.
        #[arg(long)]
        session: String,
    },
}

/// One `agent run`.
#[derive(Debug, clap::Args)]
pub struct Run {
    /// The session to work in. Its first run opens it.
    #[arg(long)]
    pub session: String,
    /// What the agent is asked to do.
    #[arg(long)]
    pub goal: String,
    /// The scratch checkout the goal lives in. A `--allow-write` grant must cover it.
    #[arg(long, value_name = "DIR")]
    pub checkout: PathBuf,
    /// How many turns the session may open.
    #[arg(long, default_value_t = 8)]
    pub max_turns: u32,
    /// How many tokens the session may spend; zero means unbounded.
    #[arg(long, default_value_t = 400_000)]
    pub token_budget: u64,
    /// How many bytes of context the model is shown in total; zero means unbounded.
    #[arg(long, default_value_t = 131_072)]
    pub context_bytes: u64,
    /// How many bytes of one observation the model is shown; zero means unbounded.
    #[arg(long, default_value_t = 4096)]
    pub observation_bytes: u64,
    /// How many settled turns the model is shown; zero means all of them.
    #[arg(long, default_value_t = 16)]
    pub context_turns: u32,
    /// Seconds one command may run before the sandbox stops it.
    #[arg(long, default_value_t = 120)]
    pub tool_deadline_seconds: u64,
    /// How many steps this invocation performs before it returns.
    #[arg(long, default_value_t = 64)]
    pub steps: usize,
}

/// One `submit`.
#[derive(Debug, clap::Args)]
pub struct Submit {
    /// The session to work in. Its first `submit` opens it.
    #[arg(long)]
    pub session: String,
    /// The idempotency identity. Two submits that name the same key run the tool once.
    #[arg(long)]
    pub idempotency_key: String,
    /// Absolute path of the program to run.
    #[arg(long, value_name = "PROGRAM")]
    pub program: PathBuf,
    /// One argument of the program. Repeatable, in order.
    ///
    /// A workload's own arguments begin with `-` often enough that refusing them here would make
    /// the CLI useless for a shell, so the value after `--arg` is taken as it is written.
    #[arg(long = "arg", value_name = "ARG", allow_hyphen_values = true)]
    pub args: Vec<String>,
    /// A file the workload writes in its own workspace, to be kept as an artifact. Repeatable, and
    /// relative to that workspace.
    #[arg(long = "artifact", value_name = "PATH")]
    pub artifacts: Vec<String>,
    /// Seconds the workload may run before the sandbox stops it.
    #[arg(long, default_value_t = 30, value_name = "SECONDS")]
    pub deadline_seconds: u64,
}

/// The validated configuration of one run.
#[derive(Clone, Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    /// The event log: one SQLite file holding the commit log and everything the kernel records.
    pub events: PathBuf,
    pub artifacts: PathBuf,
    /// Parent of the sandbox's execution directories.
    pub sandbox_root: PathBuf,
    /// Programs the agent principal may invoke.
    pub allow_tool: Vec<PathBuf>,
    /// Directories the workload may read and execute in.
    pub allow_path: Vec<PathBuf>,
    /// Directories the workload may also write in.
    pub allow_write: Vec<PathBuf>,
    /// The exact environment a confined tool is started with: `PATH` from this process, then each
    /// `--tool-env` entry in order. It replaces the daemon's environment rather than adding to it.
    pub tool_env: Vec<(OsString, OsString)>,
    /// The confiner the sandbox supervises with.
    pub confiner: PathBuf,
    pub crash_after: Option<CrashPoint>,
    /// Which provider answers the model calls.
    pub provider: ProviderKind,
    /// The scripted provider's answers, when it is the one selected.
    pub script: Option<PathBuf>,
    /// The model id the provider asks for.
    pub model: Option<String>,
    /// The provider endpoint root.
    pub endpoint: String,
    /// The path of the credential file, never the credential itself.
    pub provider_credential_file: Option<PathBuf>,
    /// How many answer tokens one model call may spend.
    pub max_answer_tokens: u64,
}

impl Config {
    /// Turns parsed arguments into a configuration, creating the directories it names.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when a directory cannot be created, a program is not absolute or
    /// not a runnable file, or a named path is not a directory.
    pub fn resolve(args: &Args) -> Result<Self, ConfigError> {
        create_directory(&args.data_dir)?;

        let artifacts = args.data_dir.join("artifacts");
        let sandbox_root = args.data_dir.join("sandbox");
        for directory in [&artifacts, &sandbox_root] {
            create_directory(directory)?;
        }

        let allow_tool = args
            .allow_tool
            .iter()
            .map(|program| runnable(program))
            .collect::<Result<Vec<_>, _>>()?;
        let allow_path = args
            .allow_path
            .iter()
            .map(|path| reachable_directory(path))
            .collect::<Result<Vec<_>, _>>()?;
        let allow_write = args
            .allow_write
            .iter()
            .map(|path| reachable_directory(path))
            .collect::<Result<Vec<_>, _>>()?;
        // The checkout is reached only through a write grant, so a run whose checkout no grant
        // covers would be refused by the sandbox halfway through; refusing it here names the path.
        if let Command::Agent(Agent::Run(run)) = &args.command {
            // A second grant would be dropped rather than becoming a second tool, which is the
            // kind of silent narrowing an operator will not notice.
            if allow_tool.len() > 1 {
                return Err(ConfigError::MultipleTools {
                    count: allow_tool.len(),
                });
            }
            if !allow_write
                .iter()
                .any(|grant| run.checkout.starts_with(grant))
            {
                return Err(ConfigError::CheckoutUngranted {
                    checkout: run.checkout.clone(),
                });
            }

            match args.provider {
                ProviderKind::Scripted => {
                    if args.script.is_none() {
                        return Err(ConfigError::NoScript);
                    }
                },
                ProviderKind::Ollama => {
                    if args.model.as_deref().unwrap_or_default().is_empty() {
                        return Err(ConfigError::NoModel);
                    }
                    let Some(credential) = args.provider_credential_file.as_deref() else {
                        return Err(ConfigError::NoCredentialFile);
                    };
                    private_credential(credential, &allow_write)?;
                },
            }
        }
        let confiner = args
            .confiner
            .clone()
            .unwrap_or_else(|| mokmok_sandbox::Config::default().confiner);

        Ok(Self {
            data_dir: args.data_dir.clone(),
            events: args.data_dir.join("events.sqlite"),
            artifacts,
            sandbox_root,
            allow_tool,
            allow_path,
            allow_write,
            tool_env: environment(&args.tool_env)?,
            confiner: runnable(&confiner)?,
            crash_after: args.crash_after,
            provider: args.provider,
            script: args.script.clone(),
            model: args.model.clone(),
            endpoint: args.endpoint.clone(),
            provider_credential_file: args.provider_credential_file.clone(),
            max_answer_tokens: args.max_answer_tokens,
        })
    }

    /// The bus socket when the caller named none.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.data_dir.join("bus.sock")
    }
}

/// Stops the process at a boundary between two durable commits when the run asked for it.
///
/// `--crash-after` alone is not enough: the environment must also carry `MOKMOKD_CRASH=1`, so only
/// a caller that means to end the daemon between two commits gets a stop. The code is distinct so
/// a test can tell a deliberate stop from a failure, and the process dies with no transaction
/// open, which is what makes the boundary the one it names.
pub fn crash(
    config: &Config,
    point: CrashPoint,
) {
    if config.crash_after == Some(point) && deliberate() {
        std::process::exit(97);
    }
}

/// Whether the crash knob was unlocked for this process.
fn deliberate() -> bool {
    std::env::var("MOKMOKD_CRASH").is_ok_and(|value| value == "1")
}

/// Reads a value the command line carried as a session, a turn, or an idempotency identity.
///
/// # Errors
///
/// Returns [`ConfigError::NotAUuid`] when the value is not a UUID, because a name would not
/// survive the trip through the log as an identity.
pub fn uuid_of(
    value: &str,
    what: &'static str,
) -> Result<Uuid, ConfigError> {
    value.parse::<Uuid>().map_err(|_| ConfigError::NotAUuid {
        what,
        value: value.to_owned(),
    })
}

/// Reads a value the command line carried as a path inside the workload's own workspace.
///
/// The sandbox registers an artifact only when it is a plain relative path, and it refuses the run
/// otherwise, so a path that climbs out is stopped here rather than turning into a refusal of the
/// whole execution halfway through.
///
/// # Errors
///
/// Returns [`ConfigError::NotRelative`] when the value is absolute or names a parent directory.
pub fn relative_path(value: &str) -> Result<PathBuf, ConfigError> {
    let path = Path::new(value);
    let climbs = path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    });

    if path.is_absolute() || climbs || value.is_empty() {
        return Err(ConfigError::NotRelative(value.to_owned()));
    }

    Ok(path.to_path_buf())
}

fn create_directory(path: &Path) -> Result<(), ConfigError> {
    std::fs::create_dir_all(path).map_err(|source| ConfigError::Data {
        path: path.to_path_buf(),
        source,
    })
}

/// Refuses a credential file the interface promises is a `0600` file: its mode must admit no group
/// or other access, and no `--allow-write` grant may cover it, because a model run inside such a
/// grant could rewrite the file the next invocation reads.
fn private_credential(
    path: &Path,
    writable: &[PathBuf],
) -> Result<(), ConfigError> {
    let metadata = std::fs::metadata(path).map_err(|source| ConfigError::CredentialUnreadable {
        path: path.to_path_buf(),
        source,
    })?;
    let mode = metadata.permissions().mode() & 0o777;

    if mode & 0o077 != 0 {
        return Err(ConfigError::CredentialNotPrivate {
            path: path.to_path_buf(),
            mode,
        });
    }

    if let Some(grant) = writable.iter().find(|grant| path.starts_with(grant)) {
        return Err(ConfigError::CredentialInWriteGrant {
            path: path.to_path_buf(),
            grant: grant.clone(),
        });
    }

    Ok(())
}

fn runnable(program: &Path) -> Result<PathBuf, ConfigError> {
    if !program.is_absolute() {
        return Err(ConfigError::NotAbsolute(program.to_path_buf()));
    }

    if !program.is_file() {
        return Err(ConfigError::NotRunnable(program.to_path_buf()));
    }

    Ok(program.to_path_buf())
}

fn reachable_directory(path: &Path) -> Result<PathBuf, ConfigError> {
    if !path.is_absolute() {
        return Err(ConfigError::NotAbsolute(path.to_path_buf()));
    }

    if !path.is_dir() {
        return Err(ConfigError::NotADirectory(path.to_path_buf()));
    }

    Ok(path.to_path_buf())
}

/// The exact environment a confined tool is started with: `PATH` from this process's own
/// environment, then each `--tool-env` entry in order, with a later entry replacing an earlier one
/// of the same name.
///
/// # Errors
///
/// Returns [`ConfigError::NotAnAssignment`] when an entry carries no `=`.
fn environment(entries: &[String]) -> Result<Vec<(OsString, OsString)>, ConfigError> {
    let mut environment: Vec<(OsString, OsString)> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        environment.push((OsString::from("PATH"), path));
    }

    for entry in entries {
        let Some((name, value)) = entry.split_once('=') else {
            return Err(ConfigError::NotAnAssignment(entry.clone()));
        };
        let name = OsString::from(name);

        match environment
            .iter_mut()
            .find(|(existing, _)| existing.as_os_str() == name.as_os_str())
        {
            Some(slot) => slot.1 = OsString::from(value),
            None => environment.push((name, OsString::from(value))),
        }
    }

    Ok(environment)
}

/// A configuration the kernel refuses to run on.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("the data directory {path} could not be created")]
    Data {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not absolute, and the sandbox resolves a path before it gates on it")]
    NotAbsolute(PathBuf),
    #[error("{0} is not a file a workload could run")]
    NotRunnable(PathBuf),
    #[error("{0} is not a directory a workload could read")]
    NotADirectory(PathBuf),
    #[error("{value} is not a uuid, so it cannot be the {what} it is meant to be")]
    NotAUuid { what: &'static str, value: String },
    #[error("{0} is not a plain relative path inside the workload's workspace")]
    NotRelative(String),
    #[error("{0} is not a NAME=VALUE pair, so it cannot set an environment variable")]
    NotAnAssignment(String),
    #[error("the checkout {checkout} is not covered by any --allow-write grant")]
    CheckoutUngranted { checkout: PathBuf },
    #[error("no --allow-tool program is named, so the agent has nothing to run")]
    NoTool,
    #[error("no --script file is named, and the scripted provider has nothing to answer with")]
    NoScript,
    #[error("no --model is named, and the ollama provider has nothing to ask")]
    NoModel,
    #[error("no --provider-credential-file is named, and the ollama provider has no credential")]
    NoCredentialFile,
    #[error("the credential file {path} could not be examined")]
    CredentialUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "the credential file {path} is mode {mode:o}, which lets group or others read it, and the \
         credential must be 0600"
    )]
    CredentialNotPrivate { path: PathBuf, mode: u32 },
    #[error(
        "the credential file {path} lies inside the --allow-write grant {grant}, so an earlier \
         model run could rewrite it before the next invocation reads it"
    )]
    CredentialInWriteGrant { path: PathBuf, grant: PathBuf },
    #[error("the agent runs one program, but {count} --allow-tool grants are named")]
    MultipleTools { count: usize },
}

#[cfg(test)]
mod tests {
    use super::{Agent, Args, Command, Config, ConfigError, ProviderKind, Submit, uuid_of};
    use clap::Parser as _;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "mokmok-kernel-config-{name}-{}",
                std::process::id(),
            ));
            let _ = std::fs::remove_dir_all(&path);

            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn args(data_dir: &Path) -> Args {
        Args {
            data_dir: data_dir.to_path_buf(),
            allow_tool: vec![PathBuf::from("/bin/sh")],
            allow_path: vec![PathBuf::from("/bin")],
            allow_write: Vec::new(),
            tool_env: Vec::new(),
            confiner: Some(PathBuf::from("/bin/sh")),
            crash_after: None,
            provider: ProviderKind::Scripted,
            script: None,
            model: None,
            endpoint: "https://ollama.com/v1".to_owned(),
            provider_credential_file: None,
            max_answer_tokens: 4096,
            command: Command::Submit(Submit {
                session: "018f2f1e-0000-7000-8000-000000000000".to_owned(),
                idempotency_key: "018f2f1e-0000-7000-8000-000000000001".to_owned(),
                program: PathBuf::from("/bin/sh"),
                args: vec!["-c".to_owned(), "true".to_owned()],
                artifacts: vec!["out.txt".to_owned()],
                deadline_seconds: 5,
            }),
        }
    }

    #[test]
    fn a_resolved_configuration_creates_the_directories_it_names() {
        let scratch = Scratch::new("resolve");
        let config = Config::resolve(&args(&scratch.0)).unwrap();

        assert!(config.data_dir.is_dir());
        assert!(config.artifacts.is_dir());
        assert!(config.sandbox_root.is_dir());
        assert_eq!(config.events, config.data_dir.join("events.sqlite"));
        assert_eq!(config.socket(), config.data_dir.join("bus.sock"));
    }

    #[test]
    fn a_relative_program_is_refused() {
        let scratch = Scratch::new("relative");
        let mut args = args(&scratch.0);
        args.allow_tool = vec![PathBuf::from("sh")];

        assert!(matches!(
            Config::resolve(&args),
            Err(ConfigError::NotAbsolute(_)),
        ));
    }

    #[test]
    fn a_program_that_is_not_a_file_is_refused() {
        let scratch = Scratch::new("absent");
        let mut args = args(&scratch.0);
        args.allow_tool = vec![PathBuf::from("/bin/no-such-tool-mokmok")];

        assert!(matches!(
            Config::resolve(&args),
            Err(ConfigError::NotRunnable(_)),
        ));
    }

    #[test]
    fn a_reachable_path_that_is_not_a_directory_is_refused() {
        let scratch = Scratch::new("file-path");
        let mut args = args(&scratch.0);
        args.allow_path = vec![PathBuf::from("/bin/sh")];

        assert!(matches!(
            Config::resolve(&args),
            Err(ConfigError::NotADirectory(_)),
        ));
    }

    #[test]
    fn an_identity_that_is_not_a_uuid_is_refused() {
        assert!(uuid_of("session-one", "session").is_err());
        assert_eq!(
            uuid_of("018f2f1e-0000-7000-8000-000000000000", "session").unwrap(),
            uuid_of("018f2f1e-0000-7000-8000-000000000000", "session").unwrap(),
        );
    }

    #[test]
    fn a_tool_environment_entry_that_is_not_an_assignment_is_refused() {
        let scratch = Scratch::new("tool-env-shape");
        let mut args = args(&scratch.0);
        args.tool_env = vec!["LANG".to_owned()];

        assert!(matches!(
            Config::resolve(&args),
            Err(ConfigError::NotAnAssignment(entry)) if entry == "LANG",
        ));
    }

    #[test]
    fn the_tool_environment_is_the_operators_entries_and_only_those() {
        fn value(
            environment: &[(OsString, OsString)],
            name: &str,
        ) -> Option<String> {
            environment
                .iter()
                .rev()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
        }

        let scratch = Scratch::new("tool-env-resolve");
        let mut args = args(&scratch.0);
        args.tool_env = vec!["LANG=C".to_owned(), "PATH=/operator".to_owned()];

        let config = Config::resolve(&args).unwrap();

        assert_eq!(value(&config.tool_env, "LANG").as_deref(), Some("C"));
        assert_eq!(
            value(&config.tool_env, "PATH").as_deref(),
            Some("/operator")
        );
        assert_eq!(
            config
                .tool_env
                .iter()
                .filter(|(key, _)| key == "PATH")
                .count(),
            1,
            "an entry naming PATH replaces the inherited one rather than joining it",
        );
        assert_eq!(value(&config.tool_env, "HOME"), None);
    }

    #[test]
    fn the_parser_reads_the_shape_the_command_line_uses() {
        let parsed = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            "/tmp/mokmok-kernel-parse",
            "--allow-tool",
            "/bin/sh",
            "submit",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--idempotency-key",
            "018f2f1e-0000-7000-8000-000000000001",
            "--program",
            "/bin/sh",
            "--arg",
            "-c",
            "--arg",
            "true",
        ])
        .unwrap();

        assert_eq!(parsed.allow_tool, vec![PathBuf::from("/bin/sh")]);
        let Command::Submit(submit) = parsed.command else {
            panic!("the command line named a submit");
        };
        assert_eq!(submit.args, vec!["-c".to_owned(), "true".to_owned()]);
        assert_eq!(submit.deadline_seconds, 30);
    }

    /// The parsed shape of one `agent run`, over the scratch directories.
    fn agent_args(data: &Path) -> Args {
        Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            data.to_str().unwrap(),
            "--allow-tool",
            "/bin/sh",
            "--allow-path",
            "/bin",
            "--allow-write",
            "/bin",
            "agent",
            "run",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--goal",
            "goal",
            "--checkout",
            "/bin",
            "--script",
            "/bin/sh",
        ])
        .unwrap()
    }

    #[test]
    fn the_parser_reads_an_agent_run_and_its_cancel() {
        let scratch = Scratch::new("agent-parse");
        let parsed = agent_args(&scratch.0);

        let Command::Agent(Agent::Run(run)) = &parsed.command else {
            panic!("the command line named an agent run");
        };
        assert_eq!(run.goal, "goal");
        assert_eq!(run.checkout, PathBuf::from("/bin"));
        assert_eq!(run.max_turns, 8);
        assert_eq!(run.steps, 64);
        assert_eq!(run.tool_deadline_seconds, 120);
        assert_eq!(run.token_budget, 400_000);
        assert_eq!(run.context_turns, 16);
        assert_eq!(run.context_bytes, 131_072);
        assert_eq!(run.observation_bytes, 4096);
        assert_eq!(parsed.provider, ProviderKind::Scripted);
        assert_eq!(parsed.script, Some(PathBuf::from("/bin/sh")));
        assert_eq!(parsed.endpoint, "https://ollama.com/v1");
        assert_eq!(parsed.max_answer_tokens, 4096);
        assert_eq!(parsed.model, None);
        assert_eq!(parsed.provider_credential_file, None);

        let cancel = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            scratch.0.to_str().unwrap(),
            "agent",
            "cancel",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
        ])
        .unwrap();
        assert!(matches!(
            cancel.command,
            Command::Agent(Agent::Cancel { ref session })
                if session == "018f2f1e-0000-7000-8000-000000000000",
        ));
    }

    #[test]
    fn a_checkout_no_write_grant_covers_is_refused_by_name() {
        let scratch = Scratch::new("checkout");
        let mut parsed = agent_args(&scratch.0);
        let Command::Agent(Agent::Run(run)) = &mut parsed.command else {
            panic!("the command line named an agent run");
        };
        run.checkout = PathBuf::from("/srv/elsewhere");

        let refusal = Config::resolve(&parsed).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::CheckoutUngranted { checkout }
                    if checkout == &PathBuf::from("/srv/elsewhere"),
            ),
            "{refusal}",
        );
        assert!(refusal.to_string().contains("/srv/elsewhere"), "{refusal}");
    }

    /// The parsed shape of one `--provider ollama` run, with `extra` appended.
    fn ollama_args(
        data: &Path,
        extra: &[&str],
    ) -> Args {
        let mut args = vec![
            "mokmokd",
            "--data-dir",
            data.to_str().unwrap(),
            "--allow-tool",
            "/bin/sh",
            "--allow-write",
            "/bin",
            "--confiner",
            "/bin/sh",
            "agent",
            "run",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--goal",
            "goal",
            "--checkout",
            "/bin",
            "--provider",
            "ollama",
        ];
        args.extend_from_slice(extra);

        Args::try_parse_from(args).unwrap()
    }

    #[test]
    fn a_scripted_run_without_a_script_is_refused() {
        let scratch = Scratch::new("no-script");
        let parsed = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            scratch.0.to_str().unwrap(),
            "--allow-tool",
            "/bin/sh",
            "--allow-write",
            "/bin",
            "--confiner",
            "/bin/sh",
            "agent",
            "run",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--goal",
            "goal",
            "--checkout",
            "/bin",
        ])
        .unwrap();

        assert!(matches!(
            Config::resolve(&parsed),
            Err(ConfigError::NoScript),
        ));
    }

    #[test]
    fn an_ollama_run_without_a_model_or_a_credential_is_refused() {
        let scratch = Scratch::new("no-model");

        assert!(matches!(
            Config::resolve(&ollama_args(&scratch.0, &[])),
            Err(ConfigError::NoModel),
        ));
        assert!(matches!(
            Config::resolve(&ollama_args(&scratch.0, &["--model", "kimi-k2.7-code"])),
            Err(ConfigError::NoCredentialFile),
        ));
    }

    /// Writes a credential file with `mode`, returning the path it landed at.
    fn credential(
        path: &Path,
        mode: u32,
    ) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "s3cr3t").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();

        path.to_path_buf()
    }

    #[test]
    fn a_credential_file_group_or_others_can_read_is_refused_by_path_and_mode() {
        let scratch = Scratch::new("cred-mode");
        let path = credential(&scratch.0.join("credential"), 0o644);

        let refusal = Config::resolve(&ollama_args(
            &scratch.0,
            &[
                "--model",
                "kimi-k2.7-code",
                "--provider-credential-file",
                path.to_str().unwrap(),
            ],
        ))
        .unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::CredentialNotPrivate { path: held, mode }
                    if held == &path && *mode == 0o644,
            ),
            "{refusal}",
        );
        assert!(
            refusal.to_string().contains(&path.display().to_string()),
            "{refusal}",
        );
        assert!(refusal.to_string().contains("644"), "{refusal}");
    }

    #[test]
    fn a_credential_file_inside_a_write_grant_is_refused_by_both_names() {
        let scratch = Scratch::new("cred-write-grant");
        let data = scratch.0.join("data");
        let granted = scratch.0.join("granted");
        let checkout = scratch.0.join("checkout");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        let path = credential(&granted.join("credential"), 0o600);

        let parsed = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            data.to_str().unwrap(),
            "--allow-tool",
            "/bin/sh",
            "--allow-write",
            granted.to_str().unwrap(),
            "--allow-write",
            checkout.to_str().unwrap(),
            "--confiner",
            "/bin/sh",
            "agent",
            "run",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--goal",
            "goal",
            "--checkout",
            checkout.to_str().unwrap(),
            "--provider",
            "ollama",
            "--model",
            "kimi-k2.7-code",
            "--provider-credential-file",
            path.to_str().unwrap(),
        ])
        .unwrap();

        let refusal = Config::resolve(&parsed).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::CredentialInWriteGrant { path: held, grant }
                    if held == &path && grant == &granted,
            ),
            "{refusal}",
        );
        assert!(
            refusal.to_string().contains(&path.display().to_string()),
            "{refusal}",
        );
        assert!(
            refusal.to_string().contains(&granted.display().to_string()),
            "{refusal}",
        );
    }

    #[test]
    fn an_agent_run_with_more_than_one_tool_grant_is_refused() {
        let scratch = Scratch::new("many-tools");
        let parsed = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            scratch.0.to_str().unwrap(),
            "--allow-tool",
            "/bin/sh",
            "--allow-tool",
            "/bin/sh",
            "--allow-write",
            "/bin",
            "--confiner",
            "/bin/sh",
            "agent",
            "run",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--goal",
            "goal",
            "--checkout",
            "/bin",
            "--script",
            "/bin/sh",
        ])
        .unwrap();

        assert!(matches!(
            Config::resolve(&parsed),
            Err(ConfigError::MultipleTools { count: 2 }),
        ));
    }
}
