//! Where each message goes, and which checkpoint it resumes from.

use std::path::Path;

use mokmok_event_bus::{SubscriberId, Topic};
use mokmok_protocol::{AgentID, SessionID};
use uuid::Uuid;

/// Commands travel on the one topic every daemon reads.
#[must_use]
pub const fn command_topic() -> Topic {
    Topic::Global
}

/// One session's durable commits, history replacements, and trouble notices.
#[must_use]
pub const fn session_topic(session: SessionID) -> Topic {
    Topic::Session(session)
}

/// One daemon's roster revisions and streaming prose.
#[must_use]
pub const fn daemon_topic(daemon: AgentID) -> Topic {
    Topic::Agent(daemon)
}

/// The daemon's command checkpoint, derived from the data directory it serves.
///
/// A daemon that restarts with the same data directory resumes the commands it had not yet
/// handled; a second daemon over another directory reads the same topic from its own checkpoint
/// and refuses sessions the first one owns, which is why one bus serves one daemon.
#[must_use]
pub fn daemon_subscriber(data_dir: &Path) -> SubscriberId {
    let path = data_dir.to_string_lossy();

    SubscriberId::from_uuid(Uuid::new_v5(&Uuid::NAMESPACE_OID, path.as_bytes()))
}

/// One process's subscription to a session, so a reconnect resumes and a restart re-bootstraps.
#[must_use]
pub fn session_subscriber(
    nonce: Uuid,
    session: SessionID,
) -> SubscriberId {
    SubscriberId::from_uuid(Uuid::new_v5(&nonce, session.as_uuid().as_bytes()))
}

/// One process's subscription to a daemon's roster and progress.
#[must_use]
pub fn daemon_view_subscriber(
    nonce: Uuid,
    daemon: AgentID,
) -> SubscriberId {
    SubscriberId::from_uuid(Uuid::new_v5(&nonce, daemon.as_uuid().as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mokmok_protocol::SessionID;
    use uuid::Uuid;

    use super::{daemon_subscriber, daemon_view_subscriber, session_subscriber};

    fn session(seed: u128) -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(seed))
    }

    #[test]
    fn a_daemon_resumes_the_same_checkpoint_after_a_restart() {
        let first = daemon_subscriber(Path::new("/var/lib/mokmokd"));

        assert_eq!(first, daemon_subscriber(Path::new("/var/lib/mokmokd")));
        assert_ne!(first, daemon_subscriber(Path::new("/tmp/other")));
    }

    #[test]
    fn two_processes_do_not_share_a_session_checkpoint() {
        let one = Uuid::from_u128(1);
        let two = Uuid::from_u128(2);

        assert_ne!(
            session_subscriber(one, session(3)),
            session_subscriber(two, session(3)),
        );
        assert_ne!(
            session_subscriber(one, session(3)),
            session_subscriber(one, session(4)),
        );
    }

    #[test]
    fn a_daemon_view_is_scoped_by_process_and_daemon() {
        let agent = mokmok_protocol::AgentID::from_uuid(Uuid::from_u128(9));

        assert_eq!(
            daemon_view_subscriber(Uuid::from_u128(1), agent),
            daemon_view_subscriber(Uuid::from_u128(1), agent),
        );
        assert_ne!(
            daemon_view_subscriber(Uuid::from_u128(1), agent),
            daemon_view_subscriber(Uuid::from_u128(2), agent),
        );
    }
}
