//! The domain-neutral event envelope and the ports that persist it.
//!
//! An adapter stores [`CloudEvent`] records under a [`StreamKey`]. Nothing here
//! knows which domain produced the data, so one store serves every event-sourced
//! subsystem. [`record`] owns the `JSONL` byte format those adapters share.

mod cloud_event;
mod port;
mod record;
mod stream;

pub use cloud_event::{CloudEvent, EventType, SOURCE, SequenceType, SpecVersion};
pub use port::{
    EnumerableStore, EventStore, StoreError, StreamKey, check_append, validate_history,
};
pub use record::{
    OBJECT_SUFFIX, canonical_keys, complete_prefix, encode_records, object_name, parse_records,
};
pub use stream::Version;
