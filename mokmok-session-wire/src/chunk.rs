//! Splitting a replacement snapshot into payloads the bus will carry.

use mokmok_agent_runtime::{Record, TaskRecord};
use mokmok_event_store::{Payload, PayloadError};
use mokmok_protocol::{AgentID, CommandID, SessionID};

use crate::{DaemonEvent, HistoryChunk, RosterChunk, RosterEntry, SessionEvent};

/// The most records one history payload carries. Well under the bus's one MiB frame, which leaves
/// room for the envelope and keeps one slow reader from holding the whole snapshot in credit.
pub const HISTORY_BYTES: usize = 256 * 1024;

/// The most listing one roster payload carries, for the same reason.
pub const ROSTER_BYTES: usize = 256 * 1024;

/// The bus's default frame ceiling, repeated here so the chunker can refuse a payload the socket
/// would close over before it is published.
const FRAME_BYTES: usize = 1024 * 1024;

/// Why a snapshot could not be split.
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    #[error(transparent)]
    Payload(#[from] PayloadError),
    #[error("one record is {found} bytes and a bus frame holds {limit}")]
    RecordTooLarge { limit: usize, found: usize },
}

/// Splits a session's whole history into ordered replacements.
///
/// The last chunk carries the seed a created session has but its log does not. An empty history is
/// one chunk with no records, so a reader never has to infer "nothing to read" from silence.
///
/// # Errors
///
/// Returns [`ChunkError::RecordTooLarge`] when one record cannot fit a bus frame, and
/// [`ChunkError::Payload`] when a record cannot be encoded.
pub fn history_chunks(
    session: SessionID,
    request: CommandID,
    seed: Option<TaskRecord>,
    records: Vec<Record>,
) -> Result<Vec<SessionEvent>, ChunkError> {
    let mut chunks: Vec<HistoryChunk> = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0usize;

    for record in records {
        let size = size_of(&record)?;
        if !current.is_empty() && bytes + size > HISTORY_BYTES {
            chunks.push(chunk(session, request, current, false, None));
            current = Vec::new();
            bytes = 0;
        }
        current.push(record);
        bytes += size;
    }

    let last = chunks.len();
    chunks.push(chunk(session, request, current, true, seed));

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, mut chunk)| {
            chunk.index = u32::try_from(index).unwrap_or(u32::MAX);
            chunk.last = index == last;
            let event = SessionEvent::History(chunk);
            refuse_oversized(&event)?;

            Ok(event)
        })
        .collect()
}

/// Splits the listing into ordered revisions.
///
/// # Errors
///
/// Returns [`ChunkError::RecordTooLarge`] when one entry cannot fit a bus frame, and
/// [`ChunkError::Payload`] when an entry cannot be encoded.
pub fn roster_chunks(
    daemon: AgentID,
    revision: u64,
    entries: Vec<RosterEntry>,
) -> Result<Vec<DaemonEvent>, ChunkError> {
    let mut chunks: Vec<RosterChunk> = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0usize;

    for entry in entries {
        let size = size_of(&entry)?;
        if !current.is_empty() && bytes + size > ROSTER_BYTES {
            chunks.push(roster(daemon, revision, current, false));
            current = Vec::new();
            bytes = 0;
        }
        current.push(entry);
        bytes += size;
    }

    let last = chunks.len();
    chunks.push(roster(daemon, revision, current, true));

    chunks
        .into_iter()
        .enumerate()
        .map(|(index, mut chunk)| {
            chunk.index = u32::try_from(index).unwrap_or(u32::MAX);
            chunk.last = index == last;
            let event = DaemonEvent::Roster(chunk);
            refuse_oversized(&event)?;

            Ok(event)
        })
        .collect()
}

const fn chunk(
    session: SessionID,
    request: CommandID,
    records: Vec<Record>,
    last: bool,
    seed: Option<TaskRecord>,
) -> HistoryChunk {
    HistoryChunk {
        request,
        session,
        index: 0,
        last,
        seed,
        records,
    }
}

const fn roster(
    daemon: AgentID,
    revision: u64,
    entries: Vec<RosterEntry>,
    last: bool,
) -> RosterChunk {
    RosterChunk {
        daemon,
        revision,
        index: 0,
        last,
        entries,
    }
}

fn size_of<T: serde::Serialize + ?Sized>(value: &T) -> Result<usize, PayloadError> {
    Ok(Payload::json(value)?.as_str().len())
}

fn refuse_oversized<T: serde::Serialize + ?Sized>(value: &T) -> Result<(), ChunkError> {
    let found = size_of(value)?;

    if found > FRAME_BYTES {
        return Err(ChunkError::RecordTooLarge {
            limit: FRAME_BYTES,
            found,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use mokmok_agent_runtime::Record;
    use mokmok_protocol::{AgentID, CommandID, SessionID};
    use mokmok_state_machine::{Ending, Phase};
    use uuid::Uuid;

    use super::{HISTORY_BYTES, history_chunks, roster_chunks};
    use crate::{DaemonEvent, RosterEntry, SessionEvent};

    fn session() -> SessionID {
        SessionID::from_uuid(Uuid::from_u128(1))
    }

    fn request() -> CommandID {
        CommandID::from_uuid(Uuid::from_u128(2))
    }

    fn agent() -> AgentID {
        AgentID::from_uuid(Uuid::from_u128(3))
    }

    fn speech(text: &str) -> Record {
        Record::Speech(mokmok_agent_runtime::SpeechRecord {
            turn: mokmok_protocol::TurnID::from_uuid(Uuid::from_u128(4)),
            text: text.to_owned(),
        })
    }

    #[test]
    fn an_empty_history_is_one_empty_chunk() {
        let chunks = history_chunks(session(), request(), None, Vec::new()).unwrap();

        assert_eq!(chunks.len(), 1);
        let SessionEvent::History(chunk) = &chunks[0] else {
            panic!("a history request answers a history chunk");
        };
        assert_eq!(chunk.index, 0);
        assert!(chunk.last);
        assert_eq!(chunk.records, Vec::new());
        assert!(chunk.seed.is_none());
    }

    #[test]
    fn history_keeps_order_and_marks_only_the_last_chunk() {
        let records = (0..40)
            .map(|index| speech(&"x".repeat(1_024 + index)))
            .collect();

        let chunks = history_chunks(session(), request(), None, records).unwrap();

        let mut seen = Vec::new();
        for (index, event) in chunks.iter().enumerate() {
            let SessionEvent::History(chunk) = event else {
                panic!("a history request answers a history chunk");
            };
            assert_eq!(chunk.index, u32::try_from(index).unwrap());
            assert_eq!(chunk.last, index + 1 == chunks.len());
            seen.extend(chunk.records.iter().cloned());
        }
        assert_eq!(seen.len(), 40);
        assert_eq!(seen.first(), Some(&speech(&"x".repeat(1_024))));
    }

    #[test]
    fn a_chunk_stays_under_its_ceiling_or_holds_one_record() {
        let records = (0..40).map(|_| speech(&"x".repeat(16 * 1024))).collect();

        let chunks = history_chunks(session(), request(), None, records).unwrap();

        for event in chunks {
            let size = mokmok_event_store::Payload::json(&event)
                .unwrap()
                .as_str()
                .len();
            assert!(size <= HISTORY_BYTES + 64 * 1024, "{size}");
        }
    }

    #[test]
    fn a_roster_splits_and_marks_its_last_chunk() {
        let entries = (0..60)
            .map(|index| RosterEntry {
                session: SessionID::from_uuid(Uuid::from_u128(index)),
                phase: Phase::Closed(Ending::Completed),
                goal: Some("goal".to_owned()),
                checkout: Some(PathBuf::from("/tmp/checkout")),
                turns: u64::try_from(index).unwrap(),
            })
            .collect();

        let chunks = roster_chunks(agent(), 7, entries).unwrap();

        let mut seen = 0;
        for (index, event) in chunks.iter().enumerate() {
            let DaemonEvent::Roster(chunk) = event else {
                panic!("a roster request answers a roster chunk");
            };
            assert_eq!(chunk.index, u32::try_from(index).unwrap());
            assert_eq!(chunk.last, index + 1 == chunks.len());
            seen += chunk.entries.len();
        }
        assert_eq!(seen, 60);
    }
}
