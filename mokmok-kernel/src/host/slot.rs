//! One session's world on a thread of its own, driven without a client waiting on it.

use std::mem;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use mokmok_agent_runtime::{Error as AgentError, Halt, Move, Progress, Receipt, Transcript, next};
use mokmok_protocol::{CommandID, Moment, SessionID};
use mokmok_session_wire::Refusal;
use mokmok_state_machine::Phase;
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot};

use crate::agent::Driver;
use crate::config::{Config, Run};

/// How much streamed prose is held before it is sent to the daemon's publisher.
const PROSE_BYTES: usize = 4 * 1024;

/// How long streamed prose is held before it is sent, so one token per event does not flood the
/// bus.
const PROSE_TICK: Duration = Duration::from_millis(50);

/// What the host asks one session's thread to do.
pub enum Command {
    Open {
        run: Run,
        reply: oneshot::Sender<Result<(), Refusal>>,
    },
    Speak {
        request: CommandID,
        line: String,
        reply: oneshot::Sender<Result<Receipt, Refusal>>,
    },
}

/// What one session's thread reports without being asked.
#[derive(Debug)]
pub enum Event {
    /// Where the fold stands after a dispatched turn, so the host can refuse a racing line.
    State {
        session: SessionID,
        phase: Phase,
        working: bool,
    },
    /// Prose a step is streaming. Display-only; the settled answer is a record.
    Fragment {
        session: SessionID,
        text: String,
    },
    /// A turn stopped for a reason no record carries.
    Trouble {
        session: SessionID,
        detail: String,
    },
    Stopped {
        session: SessionID,
    },
}

/// The host's handle to one session's thread.
pub struct Handle {
    pub commands: Sender<Command>,
}

/// Starts a session thread. It opens nothing until the host sends [`Command::Open`].
///
/// # Errors
///
/// Returns the thread's failure when it cannot be started.
pub fn spawn(
    config: Config,
    session: SessionID,
    at: Moment,
    events: mpsc::UnboundedSender<Event>,
) -> std::io::Result<Handle> {
    let (commands, command) = std::sync::mpsc::channel();
    let name = format!("mokmok-session-{}", session.as_uuid());
    // The handle is dropped so the thread is not joined on the daemon's stop path: a step in flight
    // is left to finish, and the log already covers a step cut off by the process leaving.
    let _thread = std::thread::Builder::new().name(name).spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            let _ = events.send(Event::Trouble {
                session,
                detail: "the session's runtime could not start".to_owned(),
            });

            return;
        };
        serve(&config, session, at, &command, &events, &runtime);
        let _ = events.send(Event::Stopped { session });
    })?;

    Ok(Handle { commands })
}

fn serve(
    config: &Config,
    session: SessionID,
    at: Moment,
    commands: &Receiver<Command>,
    events: &mpsc::UnboundedSender<Event>,
    runtime: &Runtime,
) {
    let mut driver: Option<Driver> = None;

    while let Ok(command) = commands.recv() {
        match command {
            Command::Open { run, reply } => {
                if driver.is_some() {
                    let _ = reply.send(Ok(()));
                    continue;
                }

                match Driver::open(config, &run, at) {
                    Ok(opened) => {
                        report(&opened, session, events);
                        let _ = reply.send(Ok(()));
                        driver = Some(opened);
                    },
                    Err(error) => {
                        let _ = reply.send(Err(Refusal::Unavailable {
                            detail: error.to_string(),
                        }));
                    },
                }
            },
            Command::Speak {
                request,
                line,
                reply,
            } => {
                let Some(held) = driver.as_mut() else {
                    let _ = reply.send(Err(Refusal::UnknownSession { session }));
                    continue;
                };

                match held.speak_as(&line, request) {
                    Ok(receipt) => {
                        let _ = reply.send(Ok(receipt));
                        run_turn(held, session, events, runtime);
                        report(held, session, events);
                    },
                    Err(error) => {
                        let _ = reply.send(Err(refusal(session, &error)));
                    },
                }
            },
        }
    }
}

/// Steps the session while it owes work, forwarding prose as it streams.
///
/// Two failures are not the end of a turn: a replayed intent is a write that landed without an
/// outcome, so the ledger is reopened and the move is asked again; a provider failure is re-asked
/// under the next attempt, exactly as the conversation's loop does.
fn run_turn(
    driver: &mut Driver,
    session: SessionID,
    events: &mpsc::UnboundedSender<Event>,
    runtime: &Runtime,
) {
    let mut prose = Prose::new(session, events);

    loop {
        let fold = match driver.transcript() {
            Ok(fold) => fold,
            Err(error) => {
                let _ = events.send(Event::Trouble {
                    session,
                    detail: error.to_string(),
                });

                return;
            },
        };
        if !working(&fold) {
            break;
        }

        let result = runtime.block_on(driver.step_watched(&mut |text| prose.observe(text)));
        prose.flush();

        match result {
            Ok(Progress::Yielded | Progress::Advanced(Move::Halt(Halt::Draining))) => {
                if let Err(error) = driver.reopen() {
                    let _ = events.send(Event::Trouble {
                        session,
                        detail: error.to_string(),
                    });

                    return;
                }
            },
            // A provider failure is re-asked under the next attempt by the runtime itself.
            Ok(Progress::Advanced(_)) | Err(AgentError::Provider(_)) => {},
            Ok(Progress::Awaiting | Progress::Terminal(_)) => break,
            Err(error) => {
                let _ = events.send(Event::Trouble {
                    session,
                    detail: error.to_string(),
                });

                return;
            },
        }
    }

    prose.flush();
}

/// Whether the session owes another step, the same question the conversation's loop asks.
pub(super) fn working(fold: &Transcript) -> bool {
    match fold.session.phase() {
        Phase::Closed(_) => false,
        Phase::Draining => true,
        Phase::Unopened | Phase::Accepting => {
            !matches!(next(fold), Move::Begin { .. } | Move::Await)
        },
    }
}

fn report(
    driver: &Driver,
    session: SessionID,
    events: &mpsc::UnboundedSender<Event>,
) {
    let (phase, working) = driver.transcript().map_or((Phase::Unopened, true), |fold| {
        let phase = fold.session.phase();
        let working = working(&fold);

        (phase, working)
    });
    let _ = events.send(Event::State {
        session,
        phase,
        working,
    });
}

fn refusal(
    session: SessionID,
    error: &AgentError,
) -> Refusal {
    match error {
        AgentError::NotAwaiting => Refusal::NotAwaiting { session },
        AgentError::TooMuch { limit, found } => Refusal::TooMuch {
            limit: *limit,
            found: *found,
        },
        other => Refusal::Unavailable {
            detail: other.to_string(),
        },
    }
}

/// Coalesces a stream of model fragments so the bus carries prose, not tokens.
struct Prose<'a> {
    session: SessionID,
    events: &'a mpsc::UnboundedSender<Event>,
    buffer: String,
    at: Instant,
}

impl<'a> Prose<'a> {
    fn new(
        session: SessionID,
        events: &'a mpsc::UnboundedSender<Event>,
    ) -> Self {
        Self {
            session,
            events,
            buffer: String::new(),
            at: Instant::now(),
        }
    }

    fn observe(
        &mut self,
        text: &str,
    ) {
        self.buffer.push_str(text);
        if self.buffer.len() >= PROSE_BYTES || self.at.elapsed() >= PROSE_TICK {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }

        let text = mem::take(&mut self.buffer);
        let _ = self.events.send(Event::Fragment {
            session: self.session,
            text,
        });
        self.at = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mokmok_protocol::SessionID;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::{Event, PROSE_TICK, Prose};

    const CAP: usize = 4096;

    fn session() -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(0x51))
    }

    fn fragment(receiver: &mut mpsc::UnboundedReceiver<Event>) -> String {
        match receiver.try_recv() {
            Ok(Event::Fragment { text, .. }) => text,
            other => panic!("expected one prose fragment, got {other:?}"),
        }
    }

    #[test]
    fn prose_holds_a_fragment_under_its_cap_and_sends_it_at_the_cap() {
        let (events, mut received) = mpsc::unbounded_channel();
        let mut prose = Prose::new(session(), &events);
        let head = "a".repeat(CAP - 1);

        prose.observe(&head);
        assert!(
            received.try_recv().is_err(),
            "a fragment under the cap and inside the tick waits",
        );

        prose.observe("b");
        assert_eq!(fragment(&mut received), format!("{head}b"));

        prose.flush();
        assert!(
            received.try_recv().is_err(),
            "an empty buffer flushes nothing",
        );
    }

    #[test]
    fn prose_sends_a_held_fragment_once_its_tick_elapses() {
        let (events, mut received) = mpsc::unbounded_channel();
        let mut prose = Prose::new(session(), &events);

        std::thread::sleep(PROSE_TICK + Duration::from_millis(10));
        prose.observe("held");

        assert_eq!(fragment(&mut received), "held");
    }
}
