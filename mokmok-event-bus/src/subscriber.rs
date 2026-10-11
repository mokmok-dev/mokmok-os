use std::fmt;
use std::str::FromStr;

/// Names one subscriber across reconnects, so the bus can hold its checkpoint.
///
/// A fixed-size UUID is deliberate: the checkpoint key must not be an unbounded caller-chosen
/// string. Persist the [`Display`](fmt::Display) form and rebuild it with [`FromStr`] to resume.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct SubscriberId(uuid::Uuid);

impl SubscriberId {
    /// Mints a fresh identity. A caller that drops it loses the checkpoint it belongs to.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub(crate) const fn as_uuid(self) -> uuid::Uuid {
        self.0
    }
}

impl Default for SubscriberId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SubscriberId {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for SubscriberId {
    type Err = InvalidSubscriberId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self).map_err(|_| InvalidSubscriberId)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, thiserror::Error)]
#[error("a subscriber id is a UUID")]
pub struct InvalidSubscriberId;

#[cfg(test)]
mod tests {
    use super::{InvalidSubscriberId, SubscriberId};

    #[test]
    fn new_mints_a_version_4_uuid() {
        assert_eq!(SubscriberId::new().as_uuid().get_version_num(), 4);
    }

    #[test]
    fn two_minted_ids_differ() {
        assert_ne!(SubscriberId::new(), SubscriberId::new());
    }

    #[test]
    fn display_round_trips_through_from_str() {
        let id = SubscriberId::new();

        assert_eq!(id.to_string().parse::<SubscriberId>(), Ok(id));
    }

    #[test]
    fn the_wire_form_equals_the_display_form() {
        let id = SubscriberId::new();

        let encoded = serde_json::to_string(&id).unwrap();

        assert_eq!(encoded, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<SubscriberId>(&encoded).unwrap(), id);
    }

    #[test]
    fn a_bad_id_is_an_invalid_subscriber_id() {
        assert_eq!(
            "not-a-uuid".parse::<SubscriberId>(),
            Err(InvalidSubscriberId)
        );
    }
}
