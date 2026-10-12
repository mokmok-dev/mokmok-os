//! The kernel's thin binary: read the command line, run the kernel, report what it did.
//!
//! The clock is read here and nowhere else. Everything below takes the instant it was handed, which
//! is what makes a rule over instants reproducible in a test and on replay.

use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser as _;
use mokmok_kernel::{
    Args, Command, Config, Error, ExecutionResult, Report, Submitted, reconcile, serve, status,
    submit,
};
use mokmok_protocol::{Moment, SessionID};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args = Args::parse();

    match run(args).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mokmokd: {error}");

            std::process::ExitCode::FAILURE
        },
    }
}

async fn run(args: Args) -> Result<(), Error> {
    let config = Config::resolve(&args)?;
    let at = now();

    match &args.command {
        Command::Submit(command) => {
            print_submitted(&submit(&config, command, at).await?);
        },
        Command::Status { session } => {
            print_report(&status(&config, session_id(session)?)?);
        },
        Command::Reconcile { session } => {
            for execution in reconcile(&config, session_id(session)?, at)? {
                println!(
                    "recovered execution={} resolution=Unknown",
                    execution.as_uuid(),
                );
            }
        },
        Command::Serve { socket, once } => {
            let socket = socket.clone().unwrap_or_else(|| config.socket());
            let published = serve(&config, &socket, *once, at).await?;
            println!("published={published} socket={}", socket.display());
        },
    }

    Ok(())
}

/// The one clock reading in the workspace.
fn now() -> Moment {
    Moment::from_millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
            }),
    )
}

fn session_id(value: &str) -> Result<SessionID, Error> {
    Ok(SessionID::from_uuid(mokmok_kernel::uuid_of(
        value, "session",
    )?))
}

fn print_submitted(submitted: &Submitted) {
    println!("session={}", submitted.session.as_uuid());
    println!("replayed={}", submitted.replayed);
    println!("phase={:?}", submitted.phase);
    if let Some(turn) = submitted.turn {
        println!(
            "turn={} number={} outcome={:?}",
            turn.as_uuid(),
            submitted
                .number
                .map_or_else(|| "none".to_owned(), |number| number.to_string()),
            submitted.outcome,
        );
    }
    if let Some(execution) = submitted.execution {
        println!(
            "execution={} resolution={:?}",
            execution.as_uuid(),
            submitted.resolution
        );
    }
    if let Some(result) = &submitted.result {
        print_result(result);
    }
}

fn print_report(report: &Report) {
    println!("session={}", report.session.as_uuid());
    println!("phase={:?}", report.phase);
    for (turn, outcome) in &report.turns {
        println!("turn={} outcome={outcome:?}", turn.as_uuid());
    }
    for (execution, state) in &report.executions {
        println!("execution={} state={state:?}", execution.as_uuid());
    }
    for (execution, result) in &report.results {
        println!("result execution={}", execution.as_uuid());
        print_result(result);
    }
}

fn print_result(result: &ExecutionResult) {
    println!(
        "stdout={} bytes={} truncated={}",
        result.stdout.hash, result.stdout.bytes, result.stdout.truncated,
    );
    println!(
        "stderr={} bytes={} truncated={}",
        result.stderr.hash, result.stderr.bytes, result.stderr.truncated,
    );
    for (path, blob) in &result.artifacts {
        println!("artifact={path} hash={} bytes={}", blob.hash, blob.bytes);
    }
}
