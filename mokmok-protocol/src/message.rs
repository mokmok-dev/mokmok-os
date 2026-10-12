//! The message taxonomy and the one extra field a command carries.

use alloc::vec::Vec;

use crate::envelope::Envelope;
use crate::error::ProtocolError;
use crate::id::{CommandID, PrincipalID};

/// What a message means, which decides how its consumer treats it.
///
/// An `Event` is a fact the log already holds; the other kinds describe an intent, and become facts
/// only once the kernel records them.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum MessageKind {
    /// A requested operation.
    Command,
    /// A fact that has occurred.
    Event,
    /// A request for state or history.
    Query,
    /// Subscription, acknowledgement, or replay coordination.
    Control,
    /// Transient progress or measurement.
    Telemetry,
}

/// A command: the envelope that describes it plus the identity a retry can reuse.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Command {
    /// The idempotency identity: a retry that reuses it replays the recorded outcome, which is what
    /// the store's dedup row is keyed on.
    pub id: CommandID,
    pub issuer: PrincipalID,
    pub envelope: Envelope,
}

impl Command {
    /// # Errors
    ///
    /// Returns the same field errors as [`Envelope::from_json`] when `envelope` breaks a contract
    /// rule, so a caller cannot wrap a message its reader would refuse.
    pub fn new(
        id: CommandID,
        issuer: PrincipalID,
        envelope: Envelope,
    ) -> Result<Self, ProtocolError> {
        crate::envelope::validate(&envelope)?;
        Ok(Self {
            id,
            issuer,
            envelope,
        })
    }

    /// # Errors
    ///
    /// Returns [`ProtocolError::Malformed`] when the command cannot be encoded as JSON.
    pub fn to_json(&self) -> Result<Vec<u8>, ProtocolError> {
        serde_json::to_vec(self).map_err(ProtocolError::Malformed)
    }

    /// Parses `bytes` with `serde` and then validates the command's envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Malformed`] when `bytes` is not a command, and the same field
    /// errors as [`Envelope::from_json`] when the command's envelope breaks a contract rule.
    pub fn from_json(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let command: Self = serde_json::from_slice(bytes).map_err(ProtocolError::Malformed)?;
        crate::envelope::validate(&command.envelope)?;
        Ok(command)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use super::{Command, MessageKind};
    use crate::envelope::{Data, Envelope, Source, Subject};
    use crate::error::ProtocolError;
    use crate::id::{CommandID, MessageID, PrincipalID, SessionID};
    use crate::moment::Moment;
    use crate::version::SchemaVersion;

    fn envelope() -> Envelope {
        Envelope::new(
            MessageID::from_uuid(Uuid::from_u128(11)),
            Source::new("agent://planner").unwrap(),
            MessageKind::Command,
            Subject::Session(SessionID::from_uuid(Uuid::from_u128(12))),
            Moment::from_millis(1_700_000_000_000),
            Data::json(&"open the turn").unwrap(),
        )
    }

    #[test]
    fn a_command_round_trips_and_keeps_its_two_identities_apart() {
        let command = Command::new(
            CommandID::from_uuid(Uuid::from_u128(1)),
            PrincipalID::from_uuid(Uuid::from_u128(2)),
            envelope(),
        )
        .unwrap();

        let decoded = Command::from_json(&command.to_json().unwrap()).unwrap();

        assert_eq!(decoded, command);
        assert_eq!(decoded.id, CommandID::from_uuid(Uuid::from_u128(1)));
        assert_eq!(
            decoded.envelope.id,
            MessageID::from_uuid(Uuid::from_u128(11)),
        );
        assert_ne!(decoded.id.as_uuid(), decoded.envelope.id.as_uuid());
    }

    #[test]
    fn command_new_validates_the_envelope_it_wraps() {
        let mut envelope = envelope();
        envelope.version = SchemaVersion::new(2, 0);

        assert_eq!(
            Command::new(
                CommandID::from_uuid(Uuid::from_u128(1)),
                PrincipalID::from_uuid(Uuid::from_u128(2)),
                envelope,
            ),
            Err(ProtocolError::UnsupportedVersion {
                seen: SchemaVersion::new(2, 0),
                supported: SchemaVersion::CURRENT,
            }),
        );
    }

    #[test]
    fn command_from_json_validates_the_envelope_it_parsed() {
        let command = Command::new(
            CommandID::from_uuid(Uuid::from_u128(1)),
            PrincipalID::from_uuid(Uuid::from_u128(2)),
            envelope(),
        )
        .unwrap();
        let mut value = serde_json::to_value(command).unwrap();
        value["envelope"]["data"]["bytes"] = json!([]);

        assert_eq!(
            Command::from_json(&serde_json::to_vec(&value).unwrap()),
            Err(ProtocolError::EmptyPayload),
        );
    }
}
