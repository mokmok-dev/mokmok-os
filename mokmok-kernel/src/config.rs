//! Reads the command line and the operator's configuration file into the one configuration the
//! kernel runs on.
//!
//! The command line is a system boundary, so every field is checked here and the rest of the
//! kernel trusts [`Config`]. A program must be absolute and present, because the sandbox resolves
//! a name before it gates and refuses what it cannot resolve. A directory named as reachable must
//! be one. Nothing is allowed by default: an empty allow list yields an empty policy, and an empty
//! policy denies every request.
//!
//! The file carries the provider group and nothing else: the authority grants, the data directory,
//! and the agent-run budgets stay on the command line, where an edited file cannot widen them.
//!
//! A value the file writes may name a variable of the process, and the environment that answers it
//! is read once by a binary and handed to resolution as data.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use serde::Deserialize;
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
///
/// The file and the command line write this the same way, because serde reproduces the kebab case
/// `ValueEnum` already uses. `every_provider_spelling_parses_from_both_surfaces` holds the two
/// together.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// Answers from a JSON file, so no network is reached.
    Scripted,
    /// Calls an OpenAI-compatible chat-completions endpoint over TLS.
    Ollama,
}

/// One layer of the provider group: the flags, and the keys `config.toml` may set.
///
/// Every key is optional, so a layer says only what it set. `deny_unknown_fields` refuses a key the
/// group does not own, which is what keeps an authority grant out of the file. One struct serves
/// both surfaces, so a key the file accepts is a key the command line accepts and the two cannot
/// drift apart.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, clap::Args)]
#[serde(deny_unknown_fields)]
pub struct ProviderLayer {
    /// Which provider answers a session's model calls. Defaults to `scripted`.
    #[arg(long = "provider", global = true, value_enum, value_name = "PROVIDER")]
    #[serde(rename = "provider")]
    pub kind: Option<ProviderKind>,
    /// The model id the endpoint is asked for. Required for `--provider ollama`.
    #[arg(long, global = true, value_name = "MODEL")]
    pub model: Option<String>,
    /// The OpenAI-compatible endpoint root. Defaults to `https://ollama.com/v1`.
    #[arg(long, global = true, value_name = "URL")]
    pub endpoint: Option<String>,
    /// The scripted provider's answers, as a JSON file. A path in the file is relative to it.
    #[arg(long, global = true, value_name = "PATH")]
    pub script: Option<PathBuf>,
    /// A file holding the endpoint credential, read at open and never from the environment. A path
    /// in the file is relative to it.
    #[arg(long = "provider-credential-file", global = true, value_name = "PATH")]
    #[serde(rename = "provider_credential_file")]
    pub credential_file: Option<PathBuf>,
    /// How many answer tokens one model call may spend. Defaults to `4096`.
    #[arg(long, global = true)]
    pub max_answer_tokens: Option<u64>,
}

/// The provider group once every layer has spoken: every key holds a value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSettings {
    pub kind: ProviderKind,
    pub model: Option<String>,
    pub endpoint: String,
    pub script: Option<PathBuf>,
    pub credential_file: Option<PathBuf>,
    pub max_answer_tokens: u64,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Scripted,
            model: None,
            endpoint: "https://ollama.com/v1".to_owned(),
            script: None,
            credential_file: None,
            max_answer_tokens: 4096,
        }
    }
}

impl ProviderSettings {
    /// The whole precedence rule in one place: the flag, then the file, then the built-in default.
    #[must_use]
    pub fn merge(
        flags: &ProviderLayer,
        file: &ProviderLayer,
    ) -> Self {
        let default = Self::default();

        Self {
            kind: flags.kind.or(file.kind).unwrap_or(default.kind),
            model: flags.model.clone().or_else(|| file.model.clone()),
            endpoint: flags
                .endpoint
                .clone()
                .or_else(|| file.endpoint.clone())
                .unwrap_or(default.endpoint),
            script: flags.script.clone().or_else(|| file.script.clone()),
            credential_file: flags
                .credential_file
                .clone()
                .or_else(|| file.credential_file.clone()),
            max_answer_tokens: flags
                .max_answer_tokens
                .or(file.max_answer_tokens)
                .unwrap_or(default.max_answer_tokens),
        }
    }
}

/// The options every invocation shares, whichever subcommand runs. Split out of [`Args`] so a
/// second binary can flatten exactly these flags and inherit exactly these rules.
#[derive(Debug, clap::Args)]
pub struct Global {
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
    #[arg(long = "search-root", global = true, value_name = "DIR")]
    pub search_root: Vec<PathBuf>,
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
    /// The provider settings in a file, instead of the one discovery finds.
    ///
    /// The file is `<XDG_CONFIG_HOME>/mokmok/config.toml`, or `$HOME/.config/mokmok/config.toml`
    /// when `XDG_CONFIG_HOME` is unset. A flag beats the file, and the file beats the built-in
    /// default. A value in the file may name a variable of the process, written `$NAME` or
    /// `${NAME}`, answered from the environment the binary was started with.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// The provider group: the flags, and the keys the configuration file may set.
    #[command(flatten)]
    pub provider: ProviderLayer,
}

/// The CLI surface of `mokmokd`.
#[derive(Debug, Parser)]
#[command(name = "mokmokd", version, about = "the mokmok OS composition root")]
pub struct Args {
    #[command(flatten)]
    pub global: Global,
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
#[derive(Clone, Debug, clap::Args)]
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
    pub search_root: Vec<PathBuf>,
    pub search_index: PathBuf,
    /// The exact environment a confined tool is started with: `PATH` from this process, then each
    /// `--tool-env` entry in order. It replaces the daemon's environment rather than adding to it.
    pub tool_env: Vec<(OsString, OsString)>,
    /// The confiner the sandbox supervises with.
    pub confiner: PathBuf,
    pub crash_after: Option<CrashPoint>,
    /// The provider group every invocation carries; only an agent run asks it a question.
    pub provider: ProviderSettings,
}

impl Config {
    /// Turns parsed arguments into a configuration, creating the directories it names.
    ///
    /// The file is read before the subcommand is matched, so one file is valid or invalid for every
    /// command rather than for a subset, and a broken file is reported on the first command the
    /// operator types. `environment` is the process's own environment, read by the caller: a value
    /// the file writes as `$NAME` or `${NAME}` is answered from it and from nowhere else.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when a directory cannot be created, a program is not absolute or
    /// not a runnable file, a named path is not a directory, a search root no grant covers, or the
    /// configuration file is missing, unreadable, invalid, or holds a path key that leaves it, or a
    /// reference in it names a variable the environment does not answer, is not a reference at all,
    /// or expands to a path that leaves the file's directory.
    pub fn resolve(
        args: &Args,
        discovered: Option<&Path>,
        environment: &ProcessEnvironment,
    ) -> Result<Self, ConfigError> {
        let global = &args.global;
        let file = file_layer(args, discovered, environment)?;
        let provider = ProviderSettings::merge(&global.provider, &file);

        create_directory(&global.data_dir)?;

        let artifacts = global.data_dir.join("artifacts");
        let sandbox_root = global.data_dir.join("sandbox");
        for directory in [&artifacts, &sandbox_root] {
            create_directory(directory)?;
        }

        let allow_tool = global
            .allow_tool
            .iter()
            .map(|program| runnable(program))
            .collect::<Result<Vec<_>, _>>()?;
        let allow_path = global
            .allow_path
            .iter()
            .map(|path| reachable_directory(path))
            .collect::<Result<Vec<_>, _>>()?;
        let allow_write = global
            .allow_write
            .iter()
            .map(|path| reachable_directory(path))
            .collect::<Result<Vec<_>, _>>()?;
        let search_root = global
            .search_root
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
            // A root no grant covers reads bytes no grant lets the workload write, and a find
            // would hand the model text the operator never opened to it, so the run is refused
            // here, before any index or log exists.
            if let Some(root) = search_root.iter().find(|root| {
                !allow_path.iter().any(|grant| root.starts_with(grant))
                    && !allow_write.iter().any(|grant| root.starts_with(grant))
            }) {
                return Err(ConfigError::SearchUngranted { path: root.clone() });
            }

            match provider.kind {
                ProviderKind::Scripted => {
                    if provider.script.is_none() {
                        return Err(ConfigError::NoScript);
                    }
                },
                ProviderKind::Ollama => {
                    if provider.model.as_deref().unwrap_or_default().is_empty() {
                        return Err(ConfigError::NoModel);
                    }
                    let Some(credential) = provider.credential_file.as_deref() else {
                        return Err(ConfigError::NoCredentialFile);
                    };
                    private_credential(credential, &allow_write)?;
                },
            }
        }
        let confiner = global
            .confiner
            .clone()
            .unwrap_or_else(|| mokmok_sandbox::Config::default().confiner);

        Ok(Self {
            data_dir: global.data_dir.clone(),
            events: global.data_dir.join("events.sqlite"),
            artifacts,
            sandbox_root,
            allow_tool,
            allow_path,
            allow_write,
            search_root,
            search_index: global.data_dir.join("search"),
            tool_env: tool_environment(&global.tool_env)?,
            confiner: runnable(&confiner)?,
            crash_after: global.crash_after,
            provider,
        })
    }

    /// The bus socket when the caller named none.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.data_dir.join("bus.sock")
    }
}

/// Where the operator's configuration file is, under the XDG rules.
///
/// `None` when neither root is set: a service has no home, and a file with no place to live is not a
/// missing file. Only the two binaries call it; every test hands [`Config::resolve`] its own path
/// instead, so the operator's own file cannot reach a test.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    config_path_for(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Decides the file location from the two roots alone, so the fallback is reachable from a test even
/// though `set_var` is unsafe and denied here.
fn config_path_for(
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    // An XDG variable counts only when it names something, which is what the spec says about empty.
    let root = |value: Option<&OsStr>| value.filter(|value| !value.is_empty()).map(PathBuf::from);

    root(xdg)
        .or_else(|| root(home).map(|home| home.join(".config")))
        .map(|root| root.join("mokmok/config.toml"))
}

/// The environment this process was started with, read once by a binary and handed to resolution
/// as data.
///
/// The file layer consults it for one thing: the values an operator's file writes as `$NAME` or
/// `${NAME}`. Nothing else reads it, and [`Config`] does not keep it, so an answer cannot reach a
/// later invocation.
///
/// `Default` is the empty environment, which is what a case hands in: a file that names a variable
/// and a caller that offers no environment is refused by name, rather than answered from whatever
/// the developer's shell happens to hold.
///
/// It is not `Debug`, because it holds every value this process was given, secrets included.
#[derive(Default)]
pub struct ProcessEnvironment(BTreeMap<OsString, OsString>);

impl ProcessEnvironment {
    /// The one read of this process's own environment for the file layer.
    #[must_use]
    pub fn read() -> Self {
        Self(std::env::vars_os().collect())
    }

    /// The value `name` holds, or `None` when the environment holds no value for it.
    ///
    /// Compared as bytes, so a name that is not UTF-8 simply never matches a reference.
    fn get(
        &self,
        name: &str,
    ) -> Option<&OsStr> {
        self.0.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    /// The environment one case pins, so the file it writes is answered from the case's own data.
    #[cfg(test)]
    fn of(pairs: &[(&str, &OsStr)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(name, value)| (OsString::from(name), value.to_os_string()))
                .collect(),
        )
    }
}

/// What a value the file wrote could not be answered, before a file and a key are named.
#[derive(Debug, Eq, PartialEq)]
enum Unanswered {
    /// The environment holds no value for the name, or holds an empty one.
    Unset { name: String },
    /// A dollar that opens neither a name nor a brace, as the operator wrote it.
    Malformed { reference: String },
    /// The name is set, and its value is not text a value of a text file can carry.
    NotText { name: String },
}

impl Unanswered {
    /// The refusal this becomes: the file, the key, and the value as the operator wrote it.
    fn in_file(
        self,
        file: &Path,
        key: &'static str,
        written: &str,
    ) -> ConfigError {
        match self {
            Self::Unset { name } => ConfigError::ConfigFileVariableUnset {
                path: file.to_path_buf(),
                key,
                value: written.to_owned(),
                name,
            },
            Self::Malformed { reference } => ConfigError::ConfigFileReferenceMalformed {
                path: file.to_path_buf(),
                key,
                value: written.to_owned(),
                reference,
            },
            Self::NotText { name } => ConfigError::ConfigFileVariableNotText {
                path: file.to_path_buf(),
                key,
                value: written.to_owned(),
                name,
            },
        }
    }
}

/// The whole rule a value the file writes follows, in one place.
///
/// A reference is `$NAME` or `${NAME}`; a name starts with an ASCII letter or `_` and goes on with
/// ASCII letters, digits, and `_`. `$$` writes one literal `$`, which is how an operator keeps a
/// dollar the environment must not answer. Any other `$` is refused rather than passed through,
/// and a name the environment does not hold, or holds empty, is refused rather than becoming
/// nothing: no key silently becomes blank, and no mistyped reference silently stays text.
///
/// An answer is appended exactly as it came and is never scanned again, so no value the environment
/// carries can open a second reference or reach a second key.
///
/// It knows of no file and no key: [`Unanswered`] says what was wrong, and the layer says where.
fn expand(
    written: &str,
    environment: &ProcessEnvironment,
) -> Result<String, Unanswered> {
    let mut expanded = String::with_capacity(written.len());
    let mut rest = written;

    // `rest` shrinks by at least the dollar every turn, so the loop ends.
    while let Some(dollar) = rest.find('$') {
        expanded.push_str(&rest[..dollar]);
        let after = &rest[dollar + 1..];

        match after.as_bytes().first() {
            // The one way to write a dollar the environment does not answer.
            Some(b'$') => {
                expanded.push('$');
                rest = &after[1..];
            },
            Some(b'{') => {
                // The name is what lies up to the first `}`. A group that never closes, or whose
                // name is not a name, is refused as the operator wrote it, because the brace is
                // part of the reference they typed and its absence is the mistake.
                let Some(close) = after.find('}') else {
                    let end = 1 + name_run(&after[1..]);

                    return Err(Unanswered::Malformed {
                        reference: format!("${}", &after[..end]),
                    });
                };
                let name = &after[1..close];
                if !variable_name(name) {
                    return Err(Unanswered::Malformed {
                        reference: format!("${}", &after[..=close]),
                    });
                }
                expanded.push_str(answered(name, environment)?);
                rest = &after[close + 1..];
            },
            // `$NAME`, and the lone `$` at the end, which takes this arm with an empty run: a run
            // that is empty or starts with a digit opens no name the environment can hold.
            _ => {
                let end = name_run(after);
                let name = &after[..end];
                if !variable_name(name) {
                    return Err(Unanswered::Malformed {
                        reference: format!("${}", &after[..end]),
                    });
                }
                expanded.push_str(answered(name, environment)?);
                rest = &after[end..];
            },
        }
    }
    expanded.push_str(rest);

    Ok(expanded)
}

/// The text `name` answers with, or what the environment's silence costs.
fn answered<'a>(
    name: &str,
    environment: &'a ProcessEnvironment,
) -> Result<&'a str, Unanswered> {
    match environment.get(name) {
        Some(value) if !value.is_empty() => value.to_str().ok_or_else(|| Unanswered::NotText {
            name: name.to_owned(),
        }),
        _ => Err(Unanswered::Unset {
            name: name.to_owned(),
        }),
    }
}

/// Whether `name` is a name the environment can hold: a letter or `_`, then letters, digits, and
/// `_`. The empty text is not one, which is what makes a lone `$` and a `$2` refusals rather than
/// silence.
fn variable_name(name: &str) -> bool {
    let mut characters = name.chars();

    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => characters.all(name_char),
        _ => false,
    }
}

/// Whether `character` continues a name.
const fn name_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// How many bytes of `text` begin the run of name characters. Bytes, because the caller slices with
/// it, and every name character is ASCII.
fn name_run(text: &str) -> usize {
    text.find(|character: char| !name_char(character))
        .unwrap_or(text.len())
}

/// The layer the configuration file contributes, or an empty one when there is no file to read.
fn file_layer(
    args: &Args,
    discovered: Option<&Path>,
    environment: &ProcessEnvironment,
) -> Result<ProviderLayer, ConfigError> {
    let named = args.global.config.as_deref();
    let Some(path) = named
        .map(Path::to_path_buf)
        .or_else(|| discovered.map(Path::to_path_buf))
    else {
        return Ok(ProviderLayer::default());
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // A file nobody named is simply not there. One the operator named and that is not there is
        // a typo, and every other refusal in this function names its path.
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            if named.is_some() {
                return Err(ConfigError::ConfigFileMissing { path });
            }

            return Ok(ProviderLayer::default());
        },
        Err(source) => return Err(ConfigError::ConfigFileUnreadable { path, source }),
    };

    let layer: ProviderLayer =
        toml::from_str(&text).map_err(|source| ConfigError::ConfigFileInvalid {
            path: path.clone(),
            source,
        })?;

    answered_layer(layer, &path, environment)
}

/// The layer the file contributes once the environment has answered it.
///
/// Serde has already refused a key the group does not own, a value of the wrong type, and a
/// provider the binary does not have, so this stage sees only the keys the file owns, each already
/// of its own type. The keys are answered in the struct's own field order, which is the order a
/// file with two bad references names them in.
fn answered_layer(
    layer: ProviderLayer,
    file: &Path,
    environment: &ProcessEnvironment,
) -> Result<ProviderLayer, ConfigError> {
    let base = file.parent().unwrap_or_else(|| Path::new("."));

    Ok(ProviderLayer {
        model: answered_text(layer.model, "model", file, environment)?,
        endpoint: answered_text(layer.endpoint, "endpoint", file, environment)?,
        script: answered_path(layer.script, "script", base, file, environment)?,
        credential_file: answered_path(
            layer.credential_file,
            "provider_credential_file",
            base,
            file,
            environment,
        )?,
        ..layer
    })
}

/// One text key: the environment answers every reference the operator wrote in it.
fn answered_text(
    written: Option<String>,
    key: &'static str,
    file: &Path,
    environment: &ProcessEnvironment,
) -> Result<Option<String>, ConfigError> {
    written
        .map(|written| {
            expand(&written, environment).map_err(|cost| cost.in_file(file, key, &written))
        })
        .transpose()
}

/// One path key: the environment answers it first, and the answer is then held to the same
/// plain-relative rule a written path follows and joined below the file's own directory.
fn answered_path(
    written: Option<PathBuf>,
    key: &'static str,
    base: &Path,
    file: &Path,
    environment: &ProcessEnvironment,
) -> Result<Option<PathBuf>, ConfigError> {
    let Some(written) = written else {
        return Ok(None);
    };
    let written = written.to_string_lossy().into_owned();
    let text = expand(&written, environment).map_err(|cost| cost.in_file(file, key, &written))?;

    match relative_path(&text) {
        Ok(relative) => Ok(Some(base.join(relative))),
        // A written path keeps its own message; only an answer gets the sentence that quotes both.
        // A path that carries no `$` expands to itself, so the two texts differ exactly when the
        // environment changed the value.
        Err(_) if text == written => Err(ConfigError::ConfigFileNotRelative {
            path: file.to_path_buf(),
            key,
            value: written,
        }),
        Err(_) => Err(ConfigError::ConfigFileNotRelativeAfterExpansion {
            path: file.to_path_buf(),
            key,
            value: written,
            expanded: text,
        }),
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
fn tool_environment(entries: &[String]) -> Result<Vec<(OsString, OsString)>, ConfigError> {
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
    #[error(
        "the search root {path} is not covered by any --allow-path or --allow-write grant, so the \
         finder would read what the operator never opened"
    )]
    SearchUngranted { path: PathBuf },
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
    #[error("--config names {path}, and no file is there to read")]
    ConfigFileMissing { path: PathBuf },
    #[error("the configuration file {path} could not be read: {source}")]
    ConfigFileUnreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the configuration file {path} is not one mokmok can read: {source}")]
    ConfigFileInvalid {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error(
        "the configuration file {path} sets {key} to {value}, and a path in the file is a plain \
         relative path inside the file's own directory"
    )]
    ConfigFileNotRelative {
        path: PathBuf,
        key: &'static str,
        value: String,
    },
    #[error(
        "the configuration file {path} sets {key} to {value}, and {name} is unset or empty in the \
         environment this process was started with"
    )]
    ConfigFileVariableUnset {
        path: PathBuf,
        key: &'static str,
        value: String,
        name: String,
    },
    #[error(
        "the configuration file {path} sets {key} to {value}, and {reference} is not a reference: \
         a reference is $NAME or ${{NAME}}, and a literal dollar is written $$"
    )]
    ConfigFileReferenceMalformed {
        path: PathBuf,
        key: &'static str,
        value: String,
        reference: String,
    },
    #[error(
        "the configuration file {path} sets {key} to {value}, and {name} is set to bytes that are \
         not text"
    )]
    ConfigFileVariableNotText {
        path: PathBuf,
        key: &'static str,
        value: String,
        name: String,
    },
    #[error(
        "the configuration file {path} sets {key} to {value}, which the environment expands to \
         {expanded}, and a path in the file is a plain relative path inside the file's own directory"
    )]
    ConfigFileNotRelativeAfterExpansion {
        path: PathBuf,
        key: &'static str,
        value: String,
        expanded: String,
    },
    #[error("the agent runs one program, but {count} --allow-tool grants are named")]
    MultipleTools { count: usize },
}

#[cfg(test)]
mod tests {
    use super::{
        Agent, Args, Command, Config, ConfigError, Global, ProcessEnvironment, ProviderKind,
        ProviderLayer, Submit, config_path_for, relative_path, uuid_of,
    };
    use clap::{CommandFactory as _, Parser as _, ValueEnum as _};
    use std::ffi::{OsStr, OsString};
    use std::os::unix::ffi::OsStrExt as _;
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
            global: Global {
                data_dir: data_dir.to_path_buf(),
                allow_tool: vec![PathBuf::from("/bin/sh")],
                allow_path: vec![PathBuf::from("/bin")],
                allow_write: Vec::new(),
                search_root: Vec::new(),
                tool_env: Vec::new(),
                confiner: Some(PathBuf::from("/bin/sh")),
                crash_after: None,
                config: None,
                provider: ProviderLayer::default(),
            },
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
        let config =
            Config::resolve(&args(&scratch.0), None, &ProcessEnvironment::default()).unwrap();

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
        args.global.allow_tool = vec![PathBuf::from("sh")];

        assert!(matches!(
            Config::resolve(&args, None, &ProcessEnvironment::default()),
            Err(ConfigError::NotAbsolute(_)),
        ));
    }

    #[test]
    fn a_program_that_is_not_a_file_is_refused() {
        let scratch = Scratch::new("absent");
        let mut args = args(&scratch.0);
        args.global.allow_tool = vec![PathBuf::from("/bin/no-such-tool-mokmok")];

        assert!(matches!(
            Config::resolve(&args, None, &ProcessEnvironment::default()),
            Err(ConfigError::NotRunnable(_)),
        ));
    }

    #[test]
    fn a_reachable_path_that_is_not_a_directory_is_refused() {
        let scratch = Scratch::new("file-path");
        let mut args = args(&scratch.0);
        args.global.allow_path = vec![PathBuf::from("/bin/sh")];

        assert!(matches!(
            Config::resolve(&args, None, &ProcessEnvironment::default()),
            Err(ConfigError::NotADirectory(_)),
        ));
    }

    #[test]
    fn a_path_inside_the_workload_workspace_is_relative_and_one_that_leaves_it_is_not() {
        assert!(matches!(
            relative_path("../escape"),
            Err(ConfigError::NotRelative(value)) if value == "../escape",
        ));
        assert!(matches!(
            relative_path(""),
            Err(ConfigError::NotRelative(value)) if value.is_empty(),
        ));
        // The path the sandbox registers, which the two refusals above are read against.
        assert_eq!(relative_path("out.txt").unwrap(), PathBuf::from("out.txt"));
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
        args.global.tool_env = vec!["LANG".to_owned()];

        assert!(matches!(
            Config::resolve(&args, None, &ProcessEnvironment::default()),
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
        args.global.tool_env = vec!["LANG=C".to_owned(), "PATH=/operator".to_owned()];

        let config = Config::resolve(&args, None, &ProcessEnvironment::default()).unwrap();

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

        assert_eq!(parsed.global.allow_tool, vec![PathBuf::from("/bin/sh")]);
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
        assert_eq!(parsed.global.provider.kind, None);
        assert_eq!(
            parsed.global.provider.script,
            Some(PathBuf::from("/bin/sh"))
        );
        assert_eq!(parsed.global.provider.endpoint, None);
        assert_eq!(parsed.global.provider.max_answer_tokens, None);
        assert_eq!(parsed.global.provider.model, None);
        assert_eq!(parsed.global.provider.credential_file, None);
        assert_eq!(parsed.global.config, None);

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

        let refusal = Config::resolve(&parsed, None, &ProcessEnvironment::default()).unwrap_err();

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

    #[test]
    fn a_search_root_no_grant_covers_is_refused_by_path() {
        let scratch = Scratch::new("search-ungranted");
        let elsewhere = scratch.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let mut parsed = agent_args(&scratch.0);
        parsed.global.search_root = vec![elsewhere.clone()];

        let refusal = Config::resolve(&parsed, None, &ProcessEnvironment::default()).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::SearchUngranted { path } if path == &elsewhere,
            ),
            "{refusal}",
        );
        assert!(
            refusal
                .to_string()
                .contains(&elsewhere.display().to_string()),
            "{refusal}",
        );
        assert!(refusal.to_string().contains("grant"), "{refusal}");
    }

    #[test]
    fn a_search_root_inside_a_read_grant_is_accepted() {
        let scratch = Scratch::new("search-granted");
        let granted = scratch.0.join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        let mut parsed = agent_args(&scratch.0);
        parsed.global.allow_path.push(granted.clone());
        parsed.global.search_root = vec![granted.clone()];
        parsed.global.confiner = Some(PathBuf::from("/bin/sh"));

        let config = Config::resolve(&parsed, None, &ProcessEnvironment::default()).unwrap();

        assert_eq!(config.search_root, vec![granted]);
        assert_eq!(config.search_index, config.data_dir.join("search"));
    }

    #[test]
    fn a_search_root_that_is_not_a_directory_is_refused() {
        let scratch = Scratch::new("search-not-a-dir");
        let mut parsed = agent_args(&scratch.0);
        parsed.global.search_root = vec![PathBuf::from("/bin/sh")];

        match Config::resolve(&parsed, None, &ProcessEnvironment::default()) {
            Err(ConfigError::NotADirectory(path)) => assert_eq!(path, PathBuf::from("/bin/sh")),
            other => panic!("a file named as a search root is refused: {other:?}"),
        }
    }

    #[test]
    fn a_run_naming_no_search_root_creates_no_search_directory() {
        let scratch = Scratch::new("search-absent");
        let mut parsed = agent_args(&scratch.0);
        parsed.global.confiner = Some(PathBuf::from("/bin/sh"));

        let config = Config::resolve(&parsed, None, &ProcessEnvironment::default()).unwrap();

        assert_eq!(config.search_root, Vec::<PathBuf>::new());
        assert_eq!(config.search_index, config.data_dir.join("search"));
        assert!(
            !config.search_index.exists(),
            "a session with no root opens no index and creates nothing",
        );
    }

    #[test]
    fn the_parser_reads_a_repeated_search_root() {
        let scratch = Scratch::new("search-parse");
        let parsed = Args::try_parse_from([
            "mokmokd",
            "--data-dir",
            scratch.0.to_str().unwrap(),
            "--allow-tool",
            "/bin/sh",
            "--search-root",
            "/bin",
            "--search-root",
            "/usr",
            "submit",
            "--session",
            "018f2f1e-0000-7000-8000-000000000000",
            "--idempotency-key",
            "018f2f1e-0000-7000-8000-000000000001",
            "--program",
            "/bin/sh",
        ])
        .unwrap();

        assert_eq!(
            parsed.global.search_root,
            vec![PathBuf::from("/bin"), PathBuf::from("/usr")],
        );
    }

    /// The parsed shape of one `agent run` over `data`, with `extra` appended.
    fn run_args(
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
        ];
        args.extend_from_slice(extra);

        Args::try_parse_from(args).unwrap()
    }

    /// The same run with `--provider ollama` named, which is the shape the credential cases use.
    fn ollama_args(
        data: &Path,
        extra: &[&str],
    ) -> Args {
        let mut named = vec!["--provider", "ollama"];
        named.extend_from_slice(extra);

        run_args(data, &named)
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
            Config::resolve(&parsed, None, &ProcessEnvironment::default()),
            Err(ConfigError::NoScript),
        ));
    }

    #[test]
    fn an_ollama_run_without_a_model_or_a_credential_is_refused() {
        let scratch = Scratch::new("no-model");

        assert!(matches!(
            Config::resolve(
                &ollama_args(&scratch.0, &[]),
                None,
                &ProcessEnvironment::default()
            ),
            Err(ConfigError::NoModel),
        ));
        assert!(matches!(
            Config::resolve(
                &ollama_args(&scratch.0, &["--model", "kimi-k2.7-code"]),
                None,
                &ProcessEnvironment::default()
            ),
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

        let refusal = Config::resolve(
            &ollama_args(
                &scratch.0,
                &[
                    "--model",
                    "kimi-k2.7-code",
                    "--provider-credential-file",
                    path.to_str().unwrap(),
                ],
            ),
            None,
            &ProcessEnvironment::default(),
        )
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

        let refusal = Config::resolve(&parsed, None, &ProcessEnvironment::default()).unwrap_err();

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
            Config::resolve(&parsed, None, &ProcessEnvironment::default()),
            Err(ConfigError::MultipleTools { count: 2 }),
        ));
    }

    /// Writes `body` as `<scratch>/config.toml`, returning its path.
    fn config_file(
        scratch: &Scratch,
        body: &str,
    ) -> PathBuf {
        std::fs::create_dir_all(&scratch.0).unwrap();
        let path = scratch.0.join("config.toml");
        std::fs::write(&path, body).unwrap();

        path
    }

    #[test]
    fn the_given_config_root_scopes_the_file() {
        assert_eq!(
            config_path_for(Some(OsStr::new("/x/cfg")), Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/x/cfg/mokmok/config.toml")),
        );
    }

    #[test]
    fn an_empty_config_root_falls_back_to_the_home_directory() {
        assert_eq!(
            config_path_for(Some(OsStr::new("")), Some(OsStr::new("/home/me"))),
            Some(PathBuf::from("/home/me/.config/mokmok/config.toml")),
        );
    }

    #[test]
    fn neither_root_names_no_file() {
        assert_eq!(config_path_for(None, None), None);
        assert_eq!(config_path_for(Some(OsStr::new("")), None), None);
    }

    #[test]
    fn a_file_fills_every_key_the_flags_left_out() {
        let scratch = Scratch::new("file-every-key");
        let credential = credential(&scratch.0.join("credential"), 0o600);
        let file = config_file(
            &scratch,
            "provider = \"ollama\"\n\
             model = \"kimi-k2.7-code\"\n\
             endpoint = \"https://example.test/v1\"\n\
             script = \"answers.json\"\n\
             provider_credential_file = \"credential\"\n\
             max_answer_tokens = 8192\n",
        );

        let config = Config::resolve(
            &run_args(&scratch.0, &[]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap();

        assert_eq!(config.provider.kind, ProviderKind::Ollama);
        assert_eq!(config.provider.model.as_deref(), Some("kimi-k2.7-code"));
        assert_eq!(config.provider.endpoint, "https://example.test/v1");
        assert_eq!(config.provider.script, Some(scratch.0.join("answers.json")));
        assert_eq!(config.provider.credential_file, Some(credential));
        assert_eq!(config.provider.max_answer_tokens, 8192);
    }

    #[test]
    fn a_flag_beats_the_file_for_the_key_it_names() {
        let scratch = Scratch::new("flag-beats-file");
        credential(&scratch.0.join("credential"), 0o600);
        let file = config_file(
            &scratch,
            "provider = \"ollama\"\n\
             model = \"from-the-file\"\n\
             endpoint = \"https://from-the-file.test/v1\"\n\
             provider_credential_file = \"credential\"\n",
        );

        let config = Config::resolve(
            &run_args(&scratch.0, &["--endpoint", "https://from-the-flag.test/v1"]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap();

        assert_eq!(config.provider.endpoint, "https://from-the-flag.test/v1");
        assert_eq!(config.provider.model.as_deref(), Some("from-the-file"));
    }

    #[test]
    fn a_file_config_named_beats_the_discovered_one() {
        let scratch = Scratch::new("named-beats-discovered");
        let discovered = config_file(&scratch, "max_answer_tokens = 111\n");
        let named = scratch.0.join("named.toml");
        std::fs::write(&named, "max_answer_tokens = 222\n").unwrap();
        let args = run_args(
            &scratch.0,
            &["--script", "/bin/sh", "--config", named.to_str().unwrap()],
        );

        let config =
            Config::resolve(&args, Some(&discovered), &ProcessEnvironment::default()).unwrap();

        assert_eq!(config.provider.max_answer_tokens, 222);
    }

    #[test]
    fn no_file_leaves_the_built_in_provider_group() {
        let scratch = Scratch::new("no-file");
        let config = Config::resolve(
            &run_args(&scratch.0, &["--script", "/bin/sh"]),
            None,
            &ProcessEnvironment::default(),
        )
        .unwrap();

        assert_eq!(
            (
                config.provider.kind,
                config.provider.endpoint.as_str(),
                config.provider.max_answer_tokens,
            ),
            (ProviderKind::Scripted, "https://ollama.com/v1", 4096),
        );
    }

    #[test]
    fn a_discovered_file_that_is_not_there_is_not_read() {
        let scratch = Scratch::new("discovered-absent");
        let absent = scratch.0.join("absent.toml");

        let config = Config::resolve(
            &run_args(&scratch.0, &["--script", "/bin/sh"]),
            Some(&absent),
            &ProcessEnvironment::default(),
        )
        .unwrap();

        assert_eq!(config.provider.max_answer_tokens, 4096);
    }

    #[test]
    fn a_file_config_named_and_not_there_is_refused_by_path() {
        let scratch = Scratch::new("named-absent");
        let named = scratch.0.join("absent.toml");
        let args = run_args(
            &scratch.0,
            &["--script", "/bin/sh", "--config", named.to_str().unwrap()],
        );

        let refusal = Config::resolve(&args, None, &ProcessEnvironment::default()).unwrap_err();

        assert!(
            matches!(&refusal, ConfigError::ConfigFileMissing { path } if path == &named),
            "{refusal}",
        );
    }

    #[test]
    fn a_config_path_that_is_a_directory_is_refused_by_path() {
        let scratch = Scratch::new("config-is-dir");
        let directory = scratch.0.join("config");
        std::fs::create_dir_all(&directory).unwrap();
        let args = run_args(
            &scratch.0,
            &[
                "--script",
                "/bin/sh",
                "--config",
                directory.to_str().unwrap(),
            ],
        );

        let refusal = Config::resolve(&args, None, &ProcessEnvironment::default()).unwrap_err();

        assert!(
            matches!(&refusal, ConfigError::ConfigFileUnreadable { path, .. } if path == &directory),
            "{refusal}",
        );
    }

    #[test]
    fn text_that_is_not_toml_is_refused_by_path() {
        let scratch = Scratch::new("not-toml");
        let wanted = config_file(&scratch, "provider = \n");
        let args = run_args(
            &scratch.0,
            &["--script", "/bin/sh", "--config", wanted.to_str().unwrap()],
        );

        let refusal = Config::resolve(&args, None, &ProcessEnvironment::default()).unwrap_err();

        assert!(
            matches!(&refusal, ConfigError::ConfigFileInvalid { path, .. } if path == &wanted),
            "{refusal}",
        );
    }

    #[test]
    fn a_key_the_file_does_not_own_is_refused_by_name() {
        let scratch = Scratch::new("unknown-key");
        let file = config_file(&scratch, "allow_write = [\"/\"]\n");

        let refusal = Config::resolve(
            &run_args(&scratch.0, &["--script", "/bin/sh"]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap_err();

        assert!(
            matches!(refusal, ConfigError::ConfigFileInvalid { .. }),
            "{refusal}",
        );
        assert!(refusal.to_string().contains("allow_write"), "{refusal}");
    }

    #[test]
    fn a_key_of_the_wrong_type_is_refused_by_name() {
        let scratch = Scratch::new("wrong-type");
        let file = config_file(&scratch, "max_answer_tokens = \"many\"\n");

        let refusal = Config::resolve(
            &run_args(&scratch.0, &["--script", "/bin/sh"]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap_err();

        assert!(
            refusal.to_string().contains("max_answer_tokens"),
            "{refusal}"
        );
    }

    #[test]
    fn a_provider_the_file_does_not_have_is_refused() {
        let scratch = Scratch::new("unknown-provider");
        let file = config_file(&scratch, "provider = \"openai\"\n");

        assert!(matches!(
            Config::resolve(
                &run_args(&scratch.0, &["--script", "/bin/sh"]),
                Some(&file),
                &ProcessEnvironment::default()
            ),
            Err(ConfigError::ConfigFileInvalid { .. }),
        ));
    }

    #[test]
    fn an_absolute_script_in_the_file_is_refused_by_key() {
        let scratch = Scratch::new("absolute-script");
        let file = config_file(&scratch, "script = \"/bin/sh\"\n");

        let refusal = Config::resolve(
            &run_args(&scratch.0, &[]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap_err();

        assert!(
            matches!(&refusal, ConfigError::ConfigFileNotRelative { key, .. } if *key == "script"),
            "{refusal}",
        );
    }

    #[test]
    fn a_climbing_credential_in_the_file_is_refused_by_key() {
        let scratch = Scratch::new("climbing-credential");
        let file = config_file(&scratch, "provider_credential_file = \"../credential\"\n");

        let refusal = Config::resolve(
            &run_args(&scratch.0, &[]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::ConfigFileNotRelative { key, .. } if *key == "provider_credential_file"
            ),
            "{refusal}",
        );
    }

    #[test]
    fn a_credential_the_file_names_inside_a_write_grant_is_refused_by_both_names() {
        let scratch = Scratch::new("file-credential-grant");
        let granted = scratch.0.join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        let credential = credential(&granted.join("credential"), 0o600);
        let file = config_file(
            &scratch,
            "provider = \"ollama\"\n\
             model = \"kimi-k2.7-code\"\n\
             provider_credential_file = \"granted/credential\"\n",
        );
        let mut args = run_args(&scratch.0, &[]);
        args.global.allow_write.push(granted.clone());

        let refusal =
            Config::resolve(&args, Some(&file), &ProcessEnvironment::default()).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::CredentialInWriteGrant { path, grant }
                    if path == &credential && grant == &granted,
            ),
            "{refusal}",
        );
    }

    #[test]
    fn a_path_in_the_file_resolves_beside_the_file() {
        let scratch = Scratch::new("beside-the-file");
        let file = config_file(
            &scratch,
            "provider = \"scripted\"\nscript = \"answers.json\"\n",
        );

        let config = Config::resolve(
            &run_args(&scratch.0, &[]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap();

        assert_eq!(config.provider.script, Some(scratch.0.join("answers.json")));
    }

    #[test]
    fn a_file_that_names_ollama_still_refuses_without_a_model() {
        let scratch = Scratch::new("file-no-model");
        let file = config_file(&scratch, "provider = \"ollama\"\n");

        assert!(matches!(
            Config::resolve(
                &run_args(&scratch.0, &[]),
                Some(&file),
                &ProcessEnvironment::default()
            ),
            Err(ConfigError::NoModel),
        ));
    }

    #[test]
    fn a_scripted_file_that_names_no_script_still_refuses() {
        let scratch = Scratch::new("file-no-script");
        let file = config_file(&scratch, "provider = \"scripted\"\n");

        assert!(matches!(
            Config::resolve(
                &run_args(&scratch.0, &[]),
                Some(&file),
                &ProcessEnvironment::default()
            ),
            Err(ConfigError::NoScript),
        ));
    }

    #[test]
    fn a_malformed_file_refuses_a_command_that_asks_no_model() {
        let scratch = Scratch::new("malformed-submit");
        let file = config_file(&scratch, "model =\n");
        let mut args = args(&scratch.0);
        args.global.config = Some(file.clone());

        let refusal = Config::resolve(&args, None, &ProcessEnvironment::default()).unwrap_err();

        assert!(
            matches!(&refusal, ConfigError::ConfigFileInvalid { path, .. } if path == &file),
            "{refusal}",
        );
    }

    /// The environment one case pins: the names its file references, and nothing the developer's
    /// shell happens to hold.
    fn pinned(pairs: &[(&str, &str)]) -> ProcessEnvironment {
        let pairs: Vec<(&str, &OsStr)> = pairs
            .iter()
            .map(|(name, value)| (*name, OsStr::new(*value)))
            .collect();

        ProcessEnvironment::of(&pairs)
    }

    #[test]
    fn a_reference_is_answered_from_the_environment_the_caller_hands_in() {
        let scratch = Scratch::new("reference-answered");
        let file = config_file(
            &scratch,
            "model = \"${MOKMOK_MODEL}\"\n\
             endpoint = \"$MOKMOK_ENDPOINT\"\n\
             script = \"answers.json\"\n",
        );
        let environment = pinned(&[
            ("MOKMOK_MODEL", "kimi-k2.7-code"),
            ("MOKMOK_ENDPOINT", "https://example.test/v1"),
        ]);

        let config =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap();

        assert_eq!(config.provider.model.as_deref(), Some("kimi-k2.7-code"));
        assert_eq!(config.provider.endpoint, "https://example.test/v1");
        assert_eq!(config.provider.script, Some(scratch.0.join("answers.json")));
    }

    #[test]
    fn a_bare_reference_ends_at_the_first_character_that_cannot_continue_a_name() {
        let scratch = Scratch::new("reference-bare-run");
        let file = config_file(
            &scratch,
            "endpoint = \"$MOKMOK_ENDPOINT/v1\"\nscript = \"answers.json\"\n",
        );
        let environment = pinned(&[("MOKMOK_ENDPOINT", "https://example.test")]);

        let config =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap();

        assert_eq!(config.provider.endpoint, "https://example.test/v1");
    }

    #[test]
    fn a_second_dollar_writes_the_dollar_the_environment_does_not_answer() {
        let scratch = Scratch::new("reference-dollar-dollar");
        let file = config_file(
            &scratch,
            "endpoint = \"https://host/v1?token=$$raw\"\nscript = \"answers.json\"\n",
        );
        let environment = pinned(&[("raw", "answered")]);

        let config =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap();

        assert_eq!(config.provider.endpoint, "https://host/v1?token=$raw");
    }

    #[test]
    fn an_answer_the_environment_gives_is_not_scanned_again() {
        let scratch = Scratch::new("reference-not-rescanned");
        let file = config_file(
            &scratch,
            "model = \"${MOKMOK_MODEL}\"\nscript = \"answers.json\"\n",
        );
        let environment = pinned(&[
            ("MOKMOK_MODEL", "$MOKMOK_ENDPOINT"),
            ("MOKMOK_ENDPOINT", "https://elsewhere.test/v1"),
        ]);

        let config =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap();

        assert_eq!(config.provider.model.as_deref(), Some("$MOKMOK_ENDPOINT"));
        assert_eq!(config.provider.endpoint, "https://ollama.com/v1");
    }

    #[test]
    fn a_value_with_no_dollar_is_left_exactly_as_the_file_wrote_it() {
        let scratch = Scratch::new("reference-absent");
        let file = config_file(
            &scratch,
            "model = \"literal-model\"\nscript = \"answers.json\"\n",
        );
        let environment = pinned(&[("MOKMOK_MODEL", "from-the-environment")]);

        let config =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap();

        assert_eq!(config.provider.model.as_deref(), Some("literal-model"));
    }

    #[test]
    fn an_unset_variable_is_refused_by_file_key_and_name() {
        let scratch = Scratch::new("reference-unset");
        let file = config_file(
            &scratch,
            "model = \"${MOKMOK_MODEL}\"\nscript = \"answers.json\"\n",
        );

        let refusal = Config::resolve(
            &run_args(&scratch.0, &[]),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::ConfigFileVariableUnset { path, key, value, name }
                    if path == &file
                        && *key == "model"
                        && value == "${MOKMOK_MODEL}"
                        && name == "MOKMOK_MODEL",
            ),
            "{refusal}",
        );
        for expected in [
            file.display().to_string(),
            "model".to_owned(),
            "MOKMOK_MODEL".to_owned(),
        ] {
            assert!(refusal.to_string().contains(&expected), "{refusal}");
        }
    }

    #[test]
    fn an_empty_variable_is_refused_like_an_unset_one() {
        let scratch = Scratch::new("reference-empty");
        let file = config_file(
            &scratch,
            "endpoint = \"${MOKMOK_ENDPOINT}\"\nscript = \"answers.json\"\n",
        );
        let environment = pinned(&[("MOKMOK_ENDPOINT", "")]);

        let refusal =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::ConfigFileVariableUnset { key, name, .. }
                    if *key == "endpoint" && name == "MOKMOK_ENDPOINT",
            ),
            "{refusal}",
        );
    }

    #[test]
    fn a_dollar_that_opens_no_name_is_refused_as_written() {
        let scratch = Scratch::new("reference-malformed");

        for (body, reference) in [("$2", "$2"), ("trailing$", "$")] {
            let file = config_file(
                &scratch,
                &format!("endpoint = \"{body}\"\nscript = \"answers.json\"\n"),
            );
            let refusal = Config::resolve(
                &run_args(&scratch.0, &[]),
                Some(&file),
                &ProcessEnvironment::default(),
            )
            .unwrap_err();

            assert!(
                matches!(
                    &refusal,
                    ConfigError::ConfigFileReferenceMalformed { reference: held, key, .. }
                        if held == reference && *key == "endpoint",
                ),
                "{refusal}",
            );
            assert!(refusal.to_string().contains("$NAME"), "{refusal}");
            assert!(refusal.to_string().contains("$$"), "{refusal}");
        }
    }

    #[test]
    fn a_braced_reference_that_names_nothing_is_refused_as_written() {
        let scratch = Scratch::new("reference-braced-empty");

        for (body, reference) in [("${}", "${}"), ("${MOKMOK_MODEL", "${MOKMOK_MODEL")] {
            let file = config_file(
                &scratch,
                &format!("model = \"{body}\"\nscript = \"answers.json\"\n"),
            );
            let refusal = Config::resolve(
                &run_args(&scratch.0, &[]),
                Some(&file),
                &ProcessEnvironment::default(),
            )
            .unwrap_err();

            assert!(
                matches!(
                    &refusal,
                    ConfigError::ConfigFileReferenceMalformed { reference: held, key, .. }
                        if held == reference && *key == "model",
                ),
                "{refusal}",
            );
        }
    }

    #[test]
    fn a_variable_whose_value_is_not_text_is_refused_by_name() {
        let scratch = Scratch::new("reference-not-text");
        let file = config_file(
            &scratch,
            "model = \"${MOKMOK_MODEL}\"\nscript = \"answers.json\"\n",
        );
        let environment = ProcessEnvironment::of(&[("MOKMOK_MODEL", OsStr::from_bytes(b"\xff"))]);

        let refusal =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::ConfigFileVariableNotText { key, name, .. }
                    if *key == "model" && name == "MOKMOK_MODEL",
            ),
            "{refusal}",
        );
        assert!(refusal.to_string().contains("not text"), "{refusal}");
    }

    #[test]
    fn a_reference_in_a_path_key_resolves_beside_the_file() {
        let scratch = Scratch::new("reference-path");
        let file = config_file(&scratch, "script = \"${MOKMOK_SCRIPT}\"\n");
        let environment = pinned(&[("MOKMOK_SCRIPT", "answers.json")]);

        let config =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap();

        assert_eq!(config.provider.script, Some(scratch.0.join("answers.json")));
    }

    #[test]
    fn an_expanded_path_that_leaves_the_file_directory_is_refused_by_key_and_variable() {
        let scratch = Scratch::new("reference-path-escape");
        let file = config_file(
            &scratch,
            "provider_credential_file = \"${MOKMOK_CREDENTIAL}\"\n",
        );
        let environment = pinned(&[("MOKMOK_CREDENTIAL", "/run/secrets/ollama")]);

        let refusal =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::ConfigFileNotRelativeAfterExpansion { key, value, expanded, .. }
                    if *key == "provider_credential_file"
                        && value == "${MOKMOK_CREDENTIAL}"
                        && expanded == "/run/secrets/ollama",
            ),
            "{refusal}",
        );
        for expected in [
            "provider_credential_file",
            "${MOKMOK_CREDENTIAL}",
            "/run/secrets/ollama",
        ] {
            assert!(refusal.to_string().contains(expected), "{refusal}");
        }
    }

    #[test]
    fn a_written_path_with_no_reference_keeps_its_existing_refusal() {
        let scratch = Scratch::new("reference-path-written");
        let file = config_file(&scratch, "script = \"/bin/sh\"\n");
        let environment = pinned(&[("MOKMOK_SCRIPT", "answers.json")]);

        let refusal =
            Config::resolve(&run_args(&scratch.0, &[]), Some(&file), &environment).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::ConfigFileNotRelative { key, value, .. }
                    if *key == "script" && value == "/bin/sh",
            ),
            "{refusal}",
        );
    }

    #[test]
    fn a_reference_in_the_provider_or_the_token_ceiling_is_refused_by_the_parse() {
        let scratch = Scratch::new("reference-wrong-key");

        for (body, key) in [
            ("provider = \"${MOKMOK_PROVIDER}\"\n", "provider"),
            (
                "max_answer_tokens = \"${MOKMOK_TOKENS}\"\n",
                "max_answer_tokens",
            ),
        ] {
            let file = config_file(&scratch, body);
            let refusal = Config::resolve(
                &run_args(&scratch.0, &["--script", "/bin/sh"]),
                Some(&file),
                &ProcessEnvironment::default(),
            )
            .unwrap_err();

            assert!(
                matches!(refusal, ConfigError::ConfigFileInvalid { .. }),
                "{refusal}",
            );
            assert!(refusal.to_string().contains(key), "{refusal}");
        }
    }

    #[test]
    fn a_flag_keeps_its_dollar() {
        let scratch = Scratch::new("reference-flag-dollar");
        let file = config_file(&scratch, "endpoint = \"https://file.test/v1\"\n");

        let config = Config::resolve(
            &run_args(
                &scratch.0,
                &["--endpoint", "https://flag.test/$2", "--script", "/bin/sh"],
            ),
            Some(&file),
            &ProcessEnvironment::default(),
        )
        .unwrap();

        assert_eq!(config.provider.endpoint, "https://flag.test/$2");
    }

    #[test]
    fn an_unset_variable_refuses_a_command_that_asks_no_model() {
        let scratch = Scratch::new("reference-submit");
        let file = config_file(&scratch, "model = \"${MOKMOK_MODEL}\"\n");
        let mut args = args(&scratch.0);
        args.global.config = Some(file.clone());

        let refusal = Config::resolve(&args, None, &ProcessEnvironment::default()).unwrap_err();

        assert!(
            matches!(
                &refusal,
                ConfigError::ConfigFileVariableUnset { path, key, name, .. }
                    if path == &file && *key == "model" && name == "MOKMOK_MODEL",
            ),
            "{refusal}",
        );
    }

    #[test]
    fn a_flag_still_beats_a_value_the_environment_answered() {
        let scratch = Scratch::new("reference-flag-beats");
        let file = config_file(
            &scratch,
            "model = \"${MOKMOK_MODEL}\"\nscript = \"answers.json\"\n",
        );
        let environment = pinned(&[("MOKMOK_MODEL", "from-the-environment")]);

        let config = Config::resolve(
            &run_args(&scratch.0, &["--model", "from-the-flag"]),
            Some(&file),
            &environment,
        )
        .unwrap();

        assert_eq!(config.provider.model.as_deref(), Some("from-the-flag"));
    }

    #[test]
    fn the_help_says_a_value_may_name_a_variable() {
        let help = Args::command().render_long_help().to_string();

        assert!(
            help.contains("$NAME") && help.contains("${NAME}"),
            "the help omits the reference syntax:\n{help}",
        );
    }

    #[test]
    fn every_provider_spelling_parses_from_both_surfaces() {
        for kind in ProviderKind::value_variants() {
            let spelling = kind.to_possible_value().unwrap().get_name().to_owned();
            let layer: ProviderLayer =
                toml::from_str(&format!("provider = \"{spelling}\"")).unwrap();

            assert_eq!(layer.kind, Some(*kind), "{spelling}");
        }
    }

    #[test]
    fn the_help_names_the_built_in_defaults() {
        let help = Args::command().render_help().to_string();

        for expected in ["scripted", "https://ollama.com/v1", "4096"] {
            assert!(
                help.contains(expected),
                "the help omits {expected}:\n{help}"
            );
        }
    }
}
