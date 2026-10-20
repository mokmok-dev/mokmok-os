//! The conversation at a terminal: one bus, as many sessions as the human holds.
//!
//! The daemon owns the sessions, the provider, the grants, and the log. This binary owns the
//! screen: it publishes what the human types and folds what the daemon records.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;
use mokmok_protocol::{Moment, SessionID};
use mokmok_tui::{BusRoster, Error, Roster, Terminal, run};

/// The goal a bare start opens its session under.
const GOAL: &str = "Work with me in this checkout. I will type what I want.";

#[derive(Debug, clap::Parser)]
#[command(
    name = "mokmok-tui",
    version,
    about = "hold a conversation with the agent",
    after_help = "The daemon owns the sessions, the provider, and the grants. Start it with \
                  'mokmokd serve' and pass its socket with --bus; nothing about the provider \
                  belongs on this command line.\n\n\
                  --session opens a session the log already holds; without it a fresh one is \
                  created. Its first line opens it.\n\n\
                  Keys\n  \
                  Tab       show the next session this process holds\n  \
                  Ctrl-O    list the sessions the log holds, and open one of them\n  \
                  Ctrl-N    start a session with a goal you type\n  \
                  Esc       close the list or the prompt\n  \
                  Ctrl-C    leave, with every session open and resumable"
)]
struct Cli {
    /// The daemon's bus socket. Defaults to the bus's own runtime path.
    #[arg(long, value_name = "PATH")]
    bus: Option<PathBuf>,
    /// The session to open. Without it a fresh one is created.
    #[arg(long)]
    session: Option<String>,
    /// The goal a fresh session is created under.
    #[arg(long)]
    goal: Option<String>,
    /// The checkout a fresh session works in. Defaults to the working directory.
    #[arg(long, value_name = "DIR")]
    checkout: Option<PathBuf>,
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

fn conversation(cli: Cli) -> Result<(), Error> {
    let at = now();
    let cwd = std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .map_err(Error::Directory)?;
    let checkout = cli.checkout.unwrap_or(cwd);
    let socket = cli
        .bus
        .unwrap_or_else(|| mokmok_event_bus::Config::default().path);
    let roster = BusRoster::attach(&socket, at, checkout)?;

    let first = match cli.session {
        Some(value) => {
            let uuid = value
                .parse()
                .map_err(|_| Error::InvalidSession(value.clone()))?;
            roster.start(SessionID::from_uuid(uuid))?
        },
        None => roster.create(cli.goal.as_deref().unwrap_or(GOAL))?,
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Terminal)?;

    runtime.block_on(async move {
        let mut terminal = Terminal::open()?;
        let outcome = run(&roster, first, &mut terminal).await;

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

/// The one clock reading this process makes, for minting a created session's identity.
fn now() -> Moment {
    Moment::from_millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            }),
    )
}
