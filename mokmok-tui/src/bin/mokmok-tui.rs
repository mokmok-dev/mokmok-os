//! The conversation at a terminal: the kernel's own flags, and as many sessions as the human holds.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;
use mokmok_kernel::{Agent, Args, Command, Config, Global, Run};
use mokmok_protocol::Moment;
use mokmok_tui::{Error, Sessions, Terminal, run};

/// The kernel's own flag declarations, flattened, so no TUI-only flag exists and every rule that
/// refuses a run refuses a conversation the same way.
#[derive(Debug, clap::Parser)]
#[command(
    name = "mokmok-tui",
    version,
    about = "hold a conversation with the agent",
    after_help = "--steps is accepted so a 'mokmokd agent run' command line works unchanged, but this \
                  conversation ignores it: it runs until every session it holds has ended, or until \
                  you leave.\n\n\
                  --session names the session the conversation starts in; without it a fresh one is \
                  minted. Its first line opens it.\n\n\
                  Keys\n  \
                  Tab       show the next session this process holds\n  \
                  Ctrl-O    list the sessions the log holds, and open one of them\n  \
                  Ctrl-N    start a session with a goal you type\n  \
                  Esc       close the list or the prompt\n  \
                  Ctrl-C    leave, with every session open and resumable"
)]
struct Cli {
    #[command(flatten)]
    global: mokmok_kernel::Global,
    #[command(flatten)]
    run: Run,
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    match conversation(cli) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mokmok-tui: {error}");

            std::process::ExitCode::FAILURE
        },
    }
}

fn conversation(mut cli: Cli) -> Result<(), Error> {
    let discovered = mokmok_kernel::config_path();
    let at = now();
    let cwd = std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .map_err(Error::Directory)?;
    let shell = shell();
    defaults(
        &mut cli.global,
        &mut cli.run,
        at,
        discovered.as_deref(),
        &cwd,
        &shell,
    );

    let args = Args {
        global: cli.global,
        command: Command::Agent(Agent::Run(cli.run.clone())),
    };
    let environment = mokmok_kernel::ProcessEnvironment::read();
    let config = Config::resolve(&args, discovered.as_deref(), &environment)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Terminal)?;

    runtime.block_on(async move {
        let world = Sessions::new(config, cli.run, at);
        let first = world.start()?;
        let mut terminal = Terminal::open()?;
        let outcome = run(&world, first, &mut terminal).await;

        // The frame lived on the alternate screen, so the plain replay is printed after the terminal
        // is handed back.
        drop(terminal);

        let outcome = outcome?;
        for left in &outcome.left {
            println!("--- session {}", left.session.as_uuid());
            for line in &left.lines {
                println!("{line}");
            }
            match left.ending {
                Some(ending) => println!("the session ended: {ending:?}"),
                None => println!(
                    "left open; resume with --session {}",
                    left.session.as_uuid(),
                ),
            }
        }

        Ok(())
    })
}

/// The goal a bare start opens its session under.
const GOAL: &str = "Work with me in this checkout. I will type what I want.";

/// Fills every flag the operator did not name, so `mokmok-tui` alone is a conversation in the
/// checkout it was started in.
fn defaults(
    global: &mut Global,
    run: &mut Run,
    at: Moment,
    config_file: Option<&Path>,
    cwd: &Path,
    shell: &Path,
) {
    if global.data_dir.is_relative() {
        // The confiner opens the plan beside the sandbox root, and it does not share this
        // process's working directory, so a relative root would name a plan it cannot find.
        global.data_dir = cwd.join(&global.data_dir);
    }
    if run.session.is_empty() {
        run.session = mokmok_kernel::mint_session(at).as_uuid().to_string();
    }
    if run.goal.is_empty() {
        GOAL.clone_into(&mut run.goal);
    }
    if run.checkout.as_os_str().is_empty() {
        run.checkout = cwd.to_path_buf();
    }
    if global.allow_tool.is_empty() {
        global.allow_tool = vec![shell.to_path_buf()];
    }
    if global.allow_write.is_empty() {
        global.allow_write = vec![cwd.to_path_buf()];
    }
    if global.allow_path.is_empty() {
        let mut paths = vec![cwd.to_path_buf()];
        // A command resolves through PATH, so every directory a system shell may look in is
        // readable and executable. The write grant stays the checkout.
        for directory in [
            "/nix/store",
            "/bin",
            "/usr",
            "/usr/bin",
            "/lib",
            "/lib64",
            "/run/current-system",
        ] {
            let path = PathBuf::from(directory);
            if path.is_dir() && !paths.iter().any(|held| path.starts_with(held)) {
                paths.push(path);
            }
        }
        if let Some(parent) = shell.parent()
            && !paths.iter().any(|path| parent.starts_with(path))
        {
            paths.push(parent.to_path_buf());
        }
        global.allow_path = paths;
    }
    // A search root no grant covers would make the kernel refuse the whole run, so the bare
    // start only adds the working directory when a grant already reaches it.
    let covered = global
        .allow_write
        .iter()
        .chain(&global.allow_path)
        .any(|grant| cwd.starts_with(grant));
    if global.search_root.is_empty() && covered {
        global.search_root = vec![cwd.to_path_buf()];
    }
    if global.skill_root.is_empty() {
        global.skill_root = config_file
            .and_then(Path::parent)
            .map(|config| config.join("skills"))
            .into_iter()
            .chain([cwd.join(".mokmok/skills")])
            .filter(|root| root.is_dir())
            .collect();
    }
}

/// The shell a bare start confines its tools to: `$SHELL` when it is an absolute file, and `/bin/sh`
/// otherwise.
fn shell() -> PathBuf {
    shell_of(std::env::var_os("SHELL").as_deref())
}

fn shell_of(value: Option<&OsStr>) -> PathBuf {
    value
        .map(PathBuf::from)
        .filter(|shell| shell.is_absolute() && shell.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// The one clock reading in this binary; nothing in the library reads a clock.
fn now() -> Moment {
    Moment::from_millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::{GOAL, defaults, shell_of};
    use mokmok_kernel::{Global, ProviderLayer, Run};
    use mokmok_protocol::Moment;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    fn bare() -> (Global, Run) {
        (
            Global {
                data_dir: PathBuf::from("mokmok-data"),
                allow_tool: Vec::new(),
                allow_path: Vec::new(),
                allow_write: Vec::new(),
                search_root: Vec::new(),
                skill_root: Vec::new(),
                tool_env: Vec::new(),
                confiner: None,
                crash_after: None,
                config: None,
                provider: ProviderLayer::default(),
            },
            Run {
                session: String::new(),
                goal: String::new(),
                checkout: PathBuf::new(),
                max_turns: 8,
                token_budget: 400_000,
                context_bytes: 131_072,
                observation_bytes: 4096,
                context_turns: 16,
                tool_deadline_seconds: 120,
                steps: 64,
            },
        )
    }

    #[test]
    fn a_bare_start_names_a_session_and_anchors_every_path_at_the_working_directory() {
        let (mut global, mut run) = bare();
        let cwd = Path::new("/srv/checkout");
        defaults(
            &mut global,
            &mut run,
            Moment::EPOCH,
            Some(Path::new("/srv/config/mokmok/config.toml")),
            cwd,
            Path::new("/bin/sh"),
        );

        assert!(!run.session.is_empty(), "a session is minted");
        assert_eq!(run.goal, GOAL);
        assert_eq!(run.checkout, cwd);
        assert_eq!(global.data_dir, cwd.join("mokmok-data"));
        assert_eq!(global.allow_write, vec![cwd.to_path_buf()]);
        assert_eq!(global.search_root, vec![cwd.to_path_buf()]);
        assert_eq!(global.allow_tool.len(), 1);
        assert!(
            global.allow_path.iter().any(|path| path == cwd),
            "the checkout is readable",
        );
    }

    #[test]
    fn a_named_flag_survives_the_defaults() {
        let (mut global, mut run) = bare();
        run.session = "018f2f1e-0000-7000-8000-000000000000".to_owned();
        run.goal = "my own goal".to_owned();
        run.checkout = PathBuf::from("/srv/other");
        global.data_dir = PathBuf::from("/srv/data");
        global.allow_write = vec![PathBuf::from("/srv/other")];

        defaults(
            &mut global,
            &mut run,
            Moment::EPOCH,
            None,
            Path::new("/srv/checkout"),
            Path::new("/bin/sh"),
        );

        assert_eq!(run.session, "018f2f1e-0000-7000-8000-000000000000");
        assert_eq!(run.goal, "my own goal");
        assert_eq!(run.checkout, Path::new("/srv/other"));
        assert_eq!(global.data_dir, Path::new("/srv/data"));
        assert_eq!(global.allow_write, vec![PathBuf::from("/srv/other")]);
    }

    #[test]
    fn the_path_guards_skip_a_directory_the_working_directory_already_covers() {
        let (mut global, mut run) = bare();
        let cwd = Path::new("/nix");
        let shell = Path::new("/nix/store/a-shell/bin/bash");
        defaults(&mut global, &mut run, Moment::EPOCH, None, cwd, shell);

        assert!(
            !global
                .allow_path
                .iter()
                .any(|path| path == Path::new("/nix/store")),
            "a store already under the checkout is not added twice",
        );
        assert!(
            !global
                .allow_path
                .iter()
                .any(|path| path == shell.parent().unwrap()),
            "a shell directory already under the checkout is not added twice",
        );
    }

    #[test]
    fn the_shell_is_the_absolute_file_or_bin_sh() {
        assert_eq!(
            shell_of(Some(OsStr::new("/bin/sh"))),
            PathBuf::from("/bin/sh"),
        );
        assert_eq!(
            shell_of(Some(OsStr::new("Cargo.toml"))),
            PathBuf::from("/bin/sh"),
            "a relative file is not a shell",
        );
        assert_eq!(
            shell_of(Some(OsStr::new("/definitely/not/a/file"))),
            PathBuf::from("/bin/sh"),
        );
        assert_eq!(shell_of(None), PathBuf::from("/bin/sh"));
    }
}
