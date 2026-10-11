//! A crash helper for `tests/crash.rs`. It reaches a named point in a real store, prints one
//! marker line, and parks.
//!
//! The test kills it with `SIGKILL`, so nothing drops the `Store` and no open transaction
//! commits. This binary therefore never calls `process::exit`, which would unwind through the
//! `Store` and roll the open transaction back instead of leaving it for WAL recovery.

use core::time::Duration;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;

use mokmok_event_store::{
    Append, CommandId, Committed, ConsumerId, Error, Expected, Kind, NewEvent, Outcome, Payload,
    Store, StreamId,
};

fn main() -> ExitCode {
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(reason) => {
            eprintln!("mokmok-event-store-crash: {reason}");
            return ExitCode::FAILURE;
        },
    };

    match fault(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("mokmok-event-store-crash: {reason}");
            ExitCode::FAILURE
        },
    }
}

struct Options {
    db: PathBuf,
    point: String,
    stream: StreamId,
    commands: usize,
    appends: usize,
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut db = None;
        let mut point = None;
        let mut stream = None;
        let mut commands = 1_usize;
        let mut appends = 0_usize;

        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| format!("{flag} takes a value"))?;
            match flag.as_str() {
                "--db" => db = Some(PathBuf::from(value)),
                "--point" => point = Some(value),
                "--stream" => {
                    stream = Some(
                        value
                            .parse::<StreamId>()
                            .map_err(|_| format!("--stream {value} is not a uuid"))?,
                    );
                },
                "--commands" => {
                    commands = value
                        .parse::<usize>()
                        .map_err(|_| format!("--commands {value} is not a count"))?;
                },
                "--appends" => {
                    appends = value
                        .parse::<usize>()
                        .map_err(|_| format!("--appends {value} is not a count"))?;
                },
                other => return Err(format!("unknown flag {other}")),
            }
        }

        Ok(Self {
            db: db.ok_or("--db is required")?,
            point: point.ok_or("--point is required")?,
            stream: stream.unwrap_or_else(StreamId::new),
            commands: commands.max(1),
            appends,
        })
    }
}

fn fault(options: &Options) -> Result<(), String> {
    let mut store = Store::open(&options.db).map_err(|error| format!("{error:?}"))?;

    match options.point.as_str() {
        "before-append" => marker("READY"),
        "mid-append" => mid_append(&mut store, options)?,
        "after-append" => after_append(&mut store, options)?,
        "loop" => return loop_append(&mut store, options),
        other => return Err(format!("unknown point {other}")),
    }

    park()
}

/// Blocks without returning, so a kill lands with the process's frames still on the stack.
fn park() -> ! {
    loop {
        thread::park_timeout(Duration::from_secs(3600));
    }
}

fn marker(line: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// The stream id doubles as the command id, so the test can retry the append the helper made
/// without a second flag to describe it.
const fn command_for(stream: StreamId) -> CommandId {
    CommandId::from_uuid(stream.as_uuid())
}

fn new_event(index: usize) -> Result<NewEvent, String> {
    let kind = Kind::new(format!("crash.{index}")).map_err(|error| error.to_string())?;
    let payload =
        Payload::json(&serde_json::json!({"n": index})).map_err(|error| error.to_string())?;

    Ok(NewEvent::new(kind, payload))
}

fn build(
    command: CommandId,
    stream: StreamId,
    expected: Expected,
    events: usize,
) -> Result<Append, String> {
    let mut append = Append::new(command, stream, expected, new_event(0)?);
    for index in 1..events {
        append = append.event(new_event(index)?);
    }

    Ok(append)
}

fn append_one(
    store: &mut Store,
    options: &Options,
) -> Result<Committed, String> {
    let append = build(
        command_for(options.stream),
        options.stream,
        Expected::Any,
        options.commands,
    )?;

    match store.append(&append).map_err(|error| error.to_string())? {
        Outcome::Committed(committed) => Ok(committed),
        other => Err(format!("a fresh append returned {other:?}")),
    }
}

fn after_append(
    store: &mut Store,
    options: &Options,
) -> Result<(), String> {
    let committed = append_one(store, options)?;
    marker(&format!("COMMITTED {}", committed.position().get()));

    Ok(())
}

fn mid_append(
    store: &mut Store,
    options: &Options,
) -> Result<(), String> {
    let outer = append_one(store, options)?;
    let consumer = ConsumerId::from_uuid(options.stream.as_uuid());
    let inner = build(
        CommandId::new(),
        options.stream,
        Expected::At(outer.version()),
        options.commands,
    )?;

    let _ = store.consume(consumer, &outer, |tx| -> Result<(), Error> {
        let _ = tx.append(&inner)?;
        marker("IN-TRANSACTION");
        park();
    });

    Ok(())
}

/// Appends until the caller kills it, or `--appends` times and then returns. A bounded run lets a
/// test put several writers on one stream at once without measuring which of them wins the write
/// lock: every writer finishes, and the log is what gets checked.
fn loop_append(
    store: &mut Store,
    options: &Options,
) -> Result<(), String> {
    let mut written = 0;
    loop {
        let committed = append_once(store, options)?;
        marker(&format!("COMMITTED {}", committed.position().get()));

        written += 1;
        if options.appends != 0 && written >= options.appends {
            return Ok(());
        }
    }
}

/// One commit per call, asking for no particular version. A retry that kept the version it read
/// earlier would spin instead under contention, because every attempt re-reads a version another
/// writer moved in the meantime.
fn append_once(
    store: &mut Store,
    options: &Options,
) -> Result<Committed, String> {
    let append = build(
        CommandId::new(),
        options.stream,
        Expected::Any,
        options.commands,
    )?;

    match store
        .append(&append)
        .map_err(|error| format!("{error:?}"))?
    {
        Outcome::Committed(committed) | Outcome::Replayed(committed) => Ok(committed),
        Outcome::Conflicted { expected, found } => Err(format!(
            "an unversioned append conflicted on {expected:?} at {found:?}"
        )),
    }
}
