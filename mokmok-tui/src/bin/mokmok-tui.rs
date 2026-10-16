//! The conversation at a terminal: the kernel's own flags, and as many sessions as the human holds.

use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;
use mokmok_kernel::{Agent, Args, Command, Config, Run};
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
                  --session names the session the conversation starts in. Its first line opens it.\n\n\
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

fn conversation(cli: Cli) -> Result<(), Error> {
    let args = Args {
        global: cli.global,
        command: Command::Agent(Agent::Run(cli.run.clone())),
    };
    let discovered = mokmok_kernel::config_path();
    let config = Config::resolve(&args, discovered.as_deref())?;
    let at = now();

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
