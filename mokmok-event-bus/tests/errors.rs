//! Pins the message and the source chain of every error a caller can catch, so a change to how the
//! errors are built cannot reword one or drop a link without a test failing.

use mokmok_event_bus::{Error, InvalidTopic, Payload, PayloadError, Refusal, ServeError, Topic};
use std::error::Error as _;
use std::io;

struct Unserializable;

impl serde::Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(
        &self,
        _: S,
    ) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("no"))
    }
}

#[test]
fn payload_error_names_both_directions_and_carries_serde() {
    let encoded: PayloadError = Payload::json(&Unserializable).unwrap_err();
    let decoded: PayloadError = Payload::json(&1).unwrap().decode::<String>().unwrap_err();

    for error in [&encoded, &decoded] {
        assert_eq!(
            error.to_string(),
            "the value could not be converted to or from JSON",
        );

        let source = error.source().expect("the serde error that failed");
        assert!(source.downcast_ref::<serde_json::Error>().is_some());
    }
}

#[test]
fn invalid_topic_states_the_grammar_and_has_no_source() {
    let error: InvalidTopic = "turn:nope".parse::<Topic>().unwrap_err();

    assert_eq!(error.to_string(), "a topic is `global` or `<kind>:<uuid>`");
    assert!(error.source().is_none());
}

#[test]
fn serve_error_says_which_kind_it_is() {
    let running = ServeError::AlreadyRunning;
    assert_eq!(
        running.to_string(),
        "another process is already serving this socket"
    );
    assert!(running.source().is_none());

    let io = ServeError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
    assert_eq!(io.to_string(), "the socket could not be served: denied");
    assert!(io.source().is_some());
}

#[test]
fn client_error_writes_every_variant_and_keeps_its_debug() {
    let transport = Error::Transport(Box::new(io::Error::new(io::ErrorKind::NotFound, "gone")));
    let protocol = Error::Protocol("the bus sent a frame that is not a response");
    let refused = Error::Refused(Refusal::TopicLimit);
    let closed = Error::Closed;
    let indeterminate = Error::Indeterminate;

    assert_eq!(transport.to_string(), "the socket failed: gone");
    assert_eq!(
        protocol.to_string(),
        "the bus sent a frame that is not a response",
    );
    assert_eq!(
        refused.to_string(),
        "the bus refused: the bus has no room for another topic",
    );
    assert_eq!(closed.to_string(), "the bus closed the connection");
    assert_eq!(
        indeterminate.to_string(),
        "the publish was written but the bus never answered",
    );

    assert!(transport.source().is_some());
    for error in [&protocol, &refused, &closed, &indeterminate] {
        assert!(error.source().is_none());
    }

    assert_eq!(format!("{transport:?}"), "Transport(gone)");
    assert_eq!(
        format!("{protocol:?}"),
        "Protocol(the bus sent a frame that is not a response)",
    );
    assert_eq!(
        format!("{refused:?}"),
        "Refused(the bus has no room for another topic)",
    );
    assert_eq!(format!("{closed:?}"), "Closed");
    assert_eq!(format!("{indeterminate:?}"), "Indeterminate");
}
