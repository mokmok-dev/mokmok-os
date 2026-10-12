//! The supervisor notification protocol: readiness, liveness, and orderly stop.
//!
//! The contract is systemd's and it arrives from outside this program: `NOTIFY_SOCKET` names a
//! datagram socket whose first byte is `@` for the Linux abstract namespace and `/` for a path, the
//! datagram carries this process's credentials for the supervisor to identify it by, and the
//! watchdog variables are inherited by everything the supervisor starts rather than being aimed at
//! this process alone. The process is expected to keep every method working with no supervisor
//! present, because `mokmokd serve` is also run by hand.

use std::io;
use std::num::NonZeroU64;
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::time::Duration;

use tokio::time::Instant;

const SOCKET: &str = "NOTIFY_SOCKET";
const USEC: &str = "WATCHDOG_USEC";
/// The variable that says which process the deadline was set for.
const PID: &str = "WATCHDOG_PID";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Message {
    Ready,
    Alive,
    Stopping,
}

impl Message {
    /// The assignment block the supervisor reads, as `sd_notify` sends it.
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Ready => "READY=1\n",
            Self::Alive => "WATCHDOG=1\n",
            Self::Stopping => "STOPPING=1\n",
        }
    }
}

#[derive(Debug)]
struct Supervisor {
    socket: UnixDatagram,
    address: SocketAddr,
    /// Half the deadline the supervisor enforces; absent when it enforces none.
    ping: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct Notifier {
    supervisor: Option<Supervisor>,
}

impl Notifier {
    pub fn from_env() -> io::Result<Self> {
        let Ok(value) = std::env::var(SOCKET) else {
            return Ok(Self::default());
        };
        let mut notifier = Self::at(&value)?;
        let usec = std::env::var(USEC);
        let pid = std::env::var(PID);
        let every = ping(
            usec.ok().as_deref(),
            pid.ok().as_deref(),
            std::process::id(),
        );
        if let Some(supervisor) = notifier.supervisor.as_mut() {
            supervisor.ping = every;
        }

        Ok(notifier)
    }

    pub fn at(socket: &str) -> io::Result<Self> {
        let address = address(socket)?;
        let datagram = UnixDatagram::unbound()?;
        // A full supervisor queue must cost one dropped report rather than park the executor this
        // pass runs on.
        datagram.set_nonblocking(true)?;

        Ok(Self {
            supervisor: Some(Supervisor {
                socket: datagram,
                address,
                ping: None,
            }),
        })
    }

    #[must_use]
    pub fn ping(&self) -> Option<Duration> {
        self.supervisor
            .as_ref()
            .and_then(|supervisor| supervisor.ping)
    }

    pub fn send(
        &self,
        message: Message,
    ) -> io::Result<()> {
        let Some(supervisor) = &self.supervisor else {
            return Ok(());
        };

        supervisor
            .socket
            .send_to_addr(message.wire().as_bytes(), &supervisor.address)
            .map(|_| ())
    }
}

pub struct Cadence {
    every: Option<Duration>,
    next: Option<Instant>,
}

impl Cadence {
    #[must_use]
    pub fn new(
        every: Option<Duration>,
        now: Instant,
    ) -> Self {
        Self {
            every,
            next: every.map(|every| now + every),
        }
    }

    pub fn due(
        &mut self,
        now: Instant,
    ) -> bool {
        let Some(next) = self.next else {
            return false;
        };
        if now < next {
            return false;
        }
        // The deadline moves from the report rather than from the schedule, so a pass that ran long
        // buys a whole interval before the next report instead of a burst of them.
        self.next = self.every.map(|every| now + every);

        true
    }
}

fn ping(
    usec: Option<&str>,
    pid: Option<&str>,
    ours: u32,
) -> Option<Duration> {
    // A zero deadline is not a deadline; `NonZeroU64` is what rejects it along with every value
    // that is not a number.
    let micros = NonZeroU64::new(usec?.parse::<u64>().ok()?)?;

    // The environment is inherited by everything the supervisor starts, so `WATCHDOG_PID` is what
    // says the value was meant for this process. A value that cannot be read is taken as naming
    // this process, because a keep-alive is only ever harmless.
    let stated = pid.and_then(|value| value.parse::<u32>().ok());
    if stated.is_some_and(|value| value != ours) {
        return None;
    }

    // Half the deadline, because the supervisor ends the process one whole deadline after the last
    // report.
    Some(Duration::from_micros(micros.get() / 2))
}

/// An abstract name and a path are the two forms this process can reach, so a value of any other
/// shape is refused rather than delivered to a path spelled like it: `vsock:2:1234`, which the
/// protocol defines for notifying a virtual machine's host, is a datagram this process has no way
/// to send.
fn address(value: &str) -> io::Result<SocketAddr> {
    if let Some(name) = value.strip_prefix('@') {
        // The abstract namespace's leading NUL is what `from_abstract_name` writes, so the name
        // here is the value without its `@`.
        return SocketAddr::from_abstract_name(name);
    }
    if value.starts_with('/') {
        return SocketAddr::from_pathname(value);
    }

    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::{Cadence, Message, Notifier, address, ping};
    use std::io::ErrorKind;
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    use std::time::Duration;

    use tokio::time::Instant;

    fn datagram(name: &str) -> UnixDatagram {
        let address = SocketAddr::from_abstract_name(name).unwrap();

        UnixDatagram::bind_addr(&address).unwrap()
    }

    fn read_one(receiver: &UnixDatagram) -> String {
        receiver
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut buffer = [0_u8; 64];
        let read = receiver.recv(&mut buffer).unwrap();

        String::from_utf8_lossy(&buffer[..read]).into_owned()
    }

    #[test]
    fn each_message_is_the_one_assignment_the_supervisor_reads() {
        assert_eq!(Message::Ready.wire(), "READY=1\n");
        assert_eq!(Message::Alive.wire(), "WATCHDOG=1\n");
        assert_eq!(Message::Stopping.wire(), "STOPPING=1\n");
    }

    #[test]
    fn a_report_reaches_a_socket_bound_at_an_abstract_name() {
        let pid = std::process::id();
        let name = format!("/mokmok-notify-unit-abstract-{pid}");
        let receiver = datagram(&name);
        let notifier = Notifier::at(&format!("@{name}")).unwrap();

        notifier.send(Message::Ready).unwrap();

        assert_eq!(read_one(&receiver), "READY=1\n");
    }

    #[test]
    fn a_report_reaches_a_socket_bound_at_a_path() {
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("mokmok-notify-unit-path-{pid}"));
        let _ = std::fs::remove_file(&path);
        let receiver = UnixDatagram::bind(&path).unwrap();
        let notifier = Notifier::at(&path.display().to_string()).unwrap();

        notifier.send(Message::Stopping).unwrap();

        assert_eq!(read_one(&receiver), "STOPPING=1\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_value_naming_neither_an_abstract_name_nor_a_path_is_refused() {
        assert_eq!(address("").unwrap_err().kind(), ErrorKind::Unsupported);
        assert_eq!(
            address("vsock:2:1234").unwrap_err().kind(),
            ErrorKind::Unsupported,
        );
        assert_eq!(
            address("mokmok-notify").unwrap_err().kind(),
            ErrorKind::Unsupported,
        );
    }

    #[test]
    fn a_report_without_a_supervisor_writes_nothing_and_succeeds() {
        let notifier = Notifier::default();

        assert!(notifier.send(Message::Alive).is_ok());
        assert_eq!(notifier.ping(), None);
    }

    #[test]
    fn the_keep_alive_is_half_the_deadline_of_this_process_alone() {
        assert_eq!(
            ping(Some("400000"), None, 42),
            Some(Duration::from_millis(200)),
        );
        assert_eq!(
            ping(Some("400000"), Some("42"), 42),
            Some(Duration::from_millis(200)),
        );
        assert_eq!(ping(Some("400000"), Some("1"), 42), None);
        assert_eq!(
            ping(Some("400000"), Some("not-a-pid"), 42),
            Some(Duration::from_millis(200)),
        );
        assert_eq!(ping(Some("0"), None, 42), None);
        assert_eq!(ping(Some("soon"), None, 42), None);
        assert_eq!(ping(None, None, 42), None);
    }

    #[test]
    fn a_schedule_is_due_once_an_interval_has_passed_and_never_without_one() {
        let start = Instant::now();
        let mut cadence = Cadence::new(Some(Duration::from_millis(200)), start);

        assert!(!cadence.due(start));
        assert!(!cadence.due(start + Duration::from_millis(199)));
        assert!(cadence.due(start + Duration::from_millis(200)));
        assert!(!cadence.due(start + Duration::from_millis(399)));
        assert!(cadence.due(start + Duration::from_millis(400)));

        let mut never = Cadence::new(None, start);
        assert!(!never.due(start + Duration::from_secs(3600)));
    }
}
