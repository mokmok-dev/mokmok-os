use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread::JoinHandle;

use mokmok_agent_runtime::{Progress, Transcript};
use mokmok_protocol::SessionID;
use tokio::runtime::Runtime;

use crate::Error;
use crate::port::{Engine, Update};

#[derive(Debug)]
enum Command {
    Speak(String),
    Step,
    Reopen,
}

#[derive(Debug)]
enum Message {
    Synced(Transcript),
    Fold(Transcript),
    Fragment(String),
    StepDone(Result<Progress, Error>),
    Failed(Error),
}

/// A session whose world lives on a thread of its own.
///
/// The cached fold is the last one the worker reported, so reading the screen never touches the
/// log. A step is dispatched and returns at once; the worker's reports arrive through
/// [`Engine::drain`].
pub struct Worker {
    session: SessionID,
    commands: Sender<Command>,
    messages: Receiver<Message>,
    fold: Transcript,
    busy: bool,
    /// Kept so the handle is not dropped while the thread runs; dropping the command sender, not
    /// this handle, is what ends the loop.
    _thread: JoinHandle<()>,
}

impl Worker {
    /// Opens the session's world and starts its thread.
    ///
    /// # Errors
    ///
    /// Returns the world's own failure when its first fold cannot be read, and the thread's
    /// failure when it cannot be started.
    pub fn open<E: Engine + Send + 'static>(mut engine: E) -> Result<Self, Error> {
        let session = engine.session();
        let fold = engine.fold()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(Error::WorkerStart)?;
        let (commands, command) = channel();
        let (message, messages) = channel();
        let name = format!("mokmok-session-{}", session.as_uuid());
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || serve(&mut engine, &command, &message, &runtime))
            .map_err(Error::WorkerStart)?;

        Ok(Self {
            session,
            commands,
            messages,
            fold,
            busy: false,
            _thread: thread,
        })
    }

    fn send(
        &self,
        command: Command,
    ) -> Result<(), Error> {
        self.commands.send(command).map_err(|_| Error::Worker)
    }
}

impl Engine for Worker {
    fn session(&self) -> SessionID {
        self.session
    }

    fn fold(&self) -> Result<Transcript, Error> {
        Ok(self.fold.clone())
    }

    fn speak(
        &mut self,
        line: &str,
    ) -> Result<(), Error> {
        self.send(Command::Speak(line.to_owned()))?;
        self.busy = true;

        Ok(())
    }

    fn step(&mut self) -> impl std::future::Future<Output = Result<Progress, Error>> {
        if !self.busy {
            match self.send(Command::Step) {
                Ok(()) => self.busy = true,
                Err(error) => return std::future::ready(Err(error)),
            }
        }

        std::future::ready(Ok(Progress::Awaiting))
    }

    fn reopen(&mut self) -> Result<(), Error> {
        self.send(Command::Reopen)?;
        self.busy = true;

        Ok(())
    }

    fn busy(&self) -> bool {
        self.busy
    }

    fn drain(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();

        loop {
            match self.messages.try_recv() {
                Ok(Message::Synced(fold)) => {
                    self.fold = fold;
                    self.busy = false;
                },
                Ok(Message::Fold(fold)) => self.fold = fold,
                Ok(Message::Fragment(fragment)) => updates.push(Update::Fragment(fragment)),
                Ok(Message::StepDone(result)) => {
                    self.busy = false;
                    updates.push(Update::Stepped(result));
                },
                Ok(Message::Failed(error)) => {
                    self.busy = false;
                    updates.push(Update::Failed(error));
                },
                Err(TryRecvError::Empty) => return updates,
                Err(TryRecvError::Disconnected) => {
                    if self.busy {
                        self.busy = false;
                        updates.push(Update::Failed(Error::Worker));
                    }

                    return updates;
                },
            }
        }
    }
}

fn serve<E: Engine>(
    engine: &mut E,
    commands: &Receiver<Command>,
    messages: &Sender<Message>,
    runtime: &Runtime,
) {
    while let Ok(command) = commands.recv() {
        match command {
            Command::Speak(line) => match engine.speak(&line) {
                Ok(()) => report_synced(engine, messages),
                Err(error) => report(messages, Message::Failed(error)),
            },
            Command::Step => {
                let fragments = messages.clone();
                let result = runtime.block_on(engine.step_watched(&mut |fragment| {
                    report(&fragments, Message::Fragment(fragment.to_owned()));
                }));
                report_fold(engine, messages);
                report(messages, Message::StepDone(result));
            },
            Command::Reopen => match engine.reopen() {
                Ok(()) => report_synced(engine, messages),
                Err(error) => report(messages, Message::Failed(error)),
            },
        }
    }
}

fn report_fold<E: Engine>(
    engine: &E,
    messages: &Sender<Message>,
) {
    match engine.fold() {
        Ok(fold) => report(messages, Message::Fold(fold)),
        Err(error) => report(messages, Message::Failed(error)),
    }
}

fn report_synced<E: Engine>(
    engine: &E,
    messages: &Sender<Message>,
) {
    match engine.fold() {
        Ok(fold) => report(messages, Message::Synced(fold)),
        Err(error) => report(messages, Message::Failed(error)),
    }
}

fn report(
    messages: &Sender<Message>,
    message: Message,
) {
    let _ = messages.send(message);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};

    use mokmok_agent_runtime::{Progress, Transcript};
    use mokmok_protocol::SessionID;

    use super::Worker;
    use crate::Error;
    use crate::port::{Engine, Update};

    fn session() -> SessionID {
        SessionID::from_uuid(uuid::Uuid::from_u128(1))
    }

    struct Fake {
        session: SessionID,
        fold: Transcript,
        fragments: Vec<&'static str>,
        gate: Option<Receiver<()>>,
        started: Option<Sender<()>>,
        spoken: Arc<Mutex<Vec<String>>>,
        reopened: Arc<AtomicUsize>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                session: session(),
                fold: Transcript::new(session()),
                fragments: Vec::new(),
                gate: None,
                started: None,
                spoken: Arc::new(Mutex::new(Vec::new())),
                reopened: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn blocked() -> (Self, Sender<()>, Receiver<()>) {
            let (release, gate) = channel();
            let (started, wait) = channel();

            (
                Self {
                    gate: Some(gate),
                    started: Some(started),
                    ..Self::new()
                },
                release,
                wait,
            )
        }
    }

    impl Engine for Fake {
        fn session(&self) -> SessionID {
            self.session
        }

        fn fold(&self) -> Result<Transcript, Error> {
            Ok(self.fold.clone())
        }

        fn speak(
            &mut self,
            line: &str,
        ) -> Result<(), Error> {
            self.spoken
                .lock()
                .expect("the spoken list is not poisoned")
                .push(line.to_owned());

            Ok(())
        }

        fn step(&mut self) -> impl std::future::Future<Output = Result<Progress, Error>> {
            std::future::ready(Ok(Progress::Awaiting))
        }

        fn step_watched<'a>(
            &'a mut self,
            watch: &'a mut dyn FnMut(&str),
        ) -> impl std::future::Future<Output = Result<Progress, Error>> + 'a {
            if let Some(started) = &self.started {
                let _ = started.send(());
            }
            if let Some(gate) = &self.gate {
                let _ = gate.recv();
            }
            for fragment in &self.fragments {
                watch(fragment);
            }

            std::future::ready(Ok(Progress::Awaiting))
        }

        fn reopen(&mut self) -> Result<(), Error> {
            self.reopened.fetch_add(1, Ordering::SeqCst);

            Ok(())
        }
    }

    fn drain_until_step(worker: &mut Worker) -> Vec<Update> {
        let mut updates = Vec::new();

        for _ in 0..1_000 {
            updates.extend(worker.drain());

            if updates
                .iter()
                .any(|update| matches!(update, Update::Stepped(_)))
            {
                return updates;
            }

            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        panic!("the step never reported back");
    }

    #[tokio::test]
    async fn a_step_reports_its_fragments_and_its_result() {
        let mut fake = Fake::new();
        fake.fragments = vec!["one", "two"];
        let mut worker = Worker::open(fake).expect("the worker starts");

        worker.step().await.expect("the step dispatches");
        assert!(worker.busy(), "the dispatched step is in flight");

        let updates = drain_until_step(&mut worker);
        let fragments: Vec<&str> = updates
            .iter()
            .filter_map(|update| match update {
                Update::Fragment(text) => Some(text.as_str()),
                Update::Stepped(_) | Update::Failed(_) => None,
            })
            .collect();

        assert_eq!(fragments, ["one", "two"]);
        assert!(!worker.busy(), "the step's result ends the dispatch");
    }

    #[tokio::test]
    async fn a_blocked_step_leaves_the_screen_thread_free() {
        let (fake, release, started) = Fake::blocked();
        let mut worker = Worker::open(fake).expect("the worker starts");

        worker.step().await.expect("the step dispatches");
        started
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the step started");

        assert_eq!(worker.session(), session());
        assert!(
            worker.fold().is_ok(),
            "the cached fold answers while the step is parked",
        );
        assert!(worker.busy());
        assert!(
            !drain_until_any(&mut worker),
            "a parked step reports nothing yet",
        );

        release.send(()).expect("the step is released");
        let updates = drain_until_step(&mut worker);

        assert!(
            updates
                .iter()
                .any(|update| matches!(update, Update::Stepped(Ok(Progress::Awaiting)))),
            "the released step reports its result",
        );
        assert!(!worker.busy(), "the result ends the dispatch");
    }

    #[tokio::test]
    async fn a_reopen_reaches_the_engine_and_ends_when_its_fold_comes_back() {
        let fake = Fake::new();
        let reopened = Arc::clone(&fake.reopened);
        let mut worker = Worker::open(fake).expect("the worker starts");

        worker.reopen().expect("the reopen is dispatched");
        assert!(worker.busy(), "the reopen is in flight");

        for _ in 0..1_000 {
            worker.drain();

            if !worker.busy() {
                break;
            }

            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert_eq!(
            reopened.load(Ordering::SeqCst),
            1,
            "the worker reopened the engine",
        );
        assert!(!worker.busy(), "the synced fold ends the reopen");
    }

    fn drain_until_any(worker: &mut Worker) -> bool {
        for _ in 0..20 {
            if !worker.drain().is_empty() {
                return true;
            }

            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        false
    }

    #[tokio::test]
    async fn a_spoken_line_reaches_the_engine_and_its_fold_comes_back() {
        let fake = Fake::new();
        let spoken = Arc::clone(&fake.spoken);
        let mut worker = Worker::open(fake).expect("the worker starts");

        worker.speak("hello").expect("the line is dispatched");
        assert!(worker.busy(), "the line is in flight");

        for _ in 0..1_000 {
            worker.drain();

            if !worker.busy() {
                break;
            }

            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        assert!(
            !worker.busy(),
            "the fold the engine wrote comes back and ends the line's flight",
        );
        assert_eq!(
            spoken
                .lock()
                .expect("the spoken list is not poisoned")
                .as_slice(),
            ["hello"],
            "the worker spoke the line in the engine",
        );
        assert!(
            Worker::drain(&mut worker).is_empty(),
            "a spoken line reports a fold and no step",
        );
    }
}
