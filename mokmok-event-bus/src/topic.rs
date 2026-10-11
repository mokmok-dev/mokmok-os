use std::fmt;
use std::str::FromStr;

use mokmok_protocol::{AgentID, ExecutionID, SessionID};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Topic {
    Agent(AgentID),
    Session(SessionID),
    Execution(ExecutionID),
    Global,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, thiserror::Error)]
#[error("a topic is `global` or `<kind>:<uuid>`")]
pub struct InvalidTopic;

impl fmt::Display for Topic {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::Agent(id) => write!(formatter, "agent:{}", id.as_uuid()),
            Self::Session(id) => write!(formatter, "session:{}", id.as_uuid()),
            Self::Execution(id) => write!(formatter, "execution:{}", id.as_uuid()),
            Self::Global => formatter.write_str("global"),
        }
    }
}

impl FromStr for Topic {
    type Err = InvalidTopic;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "global" {
            return Ok(Self::Global);
        }

        match value.split_once(':') {
            Some(("agent", uuid)) => {
                parse_uuid(uuid).map(|uuid| Self::Agent(AgentID::from_uuid(uuid)))
            },
            Some(("session", uuid)) => {
                parse_uuid(uuid).map(|uuid| Self::Session(SessionID::from_uuid(uuid)))
            },
            Some(("execution", uuid)) => {
                parse_uuid(uuid).map(|uuid| Self::Execution(ExecutionID::from_uuid(uuid)))
            },
            _ => Err(InvalidTopic),
        }
    }
}

fn parse_uuid(value: &str) -> Result<uuid::Uuid, InvalidTopic> {
    value.parse().map_err(|_| InvalidTopic)
}

impl serde::Serialize for Topic {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for Topic {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TopicVisitor;

        impl serde::de::Visitor<'_> for TopicVisitor {
            type Value = Topic;

            fn expecting(
                &self,
                formatter: &mut fmt::Formatter<'_>,
            ) -> fmt::Result {
                formatter.write_str("a topic name")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> Result<Topic, E> {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(TopicVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::Topic;
    use std::str::FromStr;
    use uuid::Uuid;

    const AGENT: &str = "agent:018f2f3a-1b2c-7d3e-9f10-223344556677";
    const SESSION: &str = "session:018f2f3a-1b2c-7d3e-9f10-223344556678";
    const EXECUTION: &str = "execution:018f2f3a-1b2c-7d3e-9f10-223344556679";

    #[test]
    fn display_writes_the_topic_grammar() {
        assert_eq!(
            Topic::from_str(AGENT).unwrap().to_string(),
            "agent:018f2f3a-1b2c-7d3e-9f10-223344556677",
        );
        assert_eq!(
            Topic::from_str(SESSION).unwrap().to_string(),
            "session:018f2f3a-1b2c-7d3e-9f10-223344556678",
        );
        assert_eq!(
            Topic::from_str(EXECUTION).unwrap().to_string(),
            "execution:018f2f3a-1b2c-7d3e-9f10-223344556679",
        );
        assert_eq!(Topic::Global.to_string(), "global");
    }

    #[test]
    fn every_variant_round_trips_through_display_and_from_str() {
        let agent = Topic::from_str(AGENT).unwrap();
        let session = Topic::from_str(SESSION).unwrap();
        let execution = Topic::from_str(EXECUTION).unwrap();

        assert_eq!(Topic::from_str(&agent.to_string()), Ok(agent));
        assert_eq!(Topic::from_str(&session.to_string()), Ok(session));
        assert_eq!(Topic::from_str(&execution.to_string()), Ok(execution));
        assert_eq!(
            Topic::from_str(&Topic::Global.to_string()),
            Ok(Topic::Global)
        );
    }

    #[test]
    fn the_parsed_topic_carries_the_uuid_it_names() {
        let agent = Topic::from_str(AGENT).unwrap();
        let Topic::Agent(id) = agent else {
            panic!("expected an agent topic");
        };
        assert_eq!(
            id.as_uuid(),
            Uuid::from_u128(0x018f_2f3a_1b2c_7d3e_9f10_2233_4455_6677)
        );
    }

    #[test]
    fn a_bad_name_is_an_invalid_topic() {
        for bad in [
            "",
            "agent",
            "agent:",
            "agent:not-a-uuid",
            "agent:018f2f3a-1b2c-7d3e-9f10-223344556677:extra",
            "session",
            "turn:018f2f3a-1b2c-7d3e-9f10-223344556677",
            "global:x",
            "global ",
            "Global",
        ] {
            assert_eq!(Topic::from_str(bad), Err(super::InvalidTopic), "{bad}");
        }
    }

    #[test]
    fn the_wire_form_equals_the_display_form() {
        let topic = Topic::from_str(SESSION).unwrap();

        let encoded = serde_json::to_string(&topic).unwrap();

        assert_eq!(encoded, "\"session:018f2f3a-1b2c-7d3e-9f10-223344556678\"");
        assert_eq!(serde_json::from_str::<Topic>(&encoded).unwrap(), topic);
        assert!(serde_json::from_str::<Topic>("\"session:nope\"").is_err());
        assert!(serde_json::from_str::<Topic>("7").is_err());
    }

    #[test]
    fn a_json_number_names_the_topic_grammar_in_the_error() {
        let error = serde_json::from_str::<Topic>("7").unwrap_err();

        assert_eq!(
            error.to_string(),
            "invalid type: integer `7`, expected a topic name at line 1 column 1",
        );
    }
}
