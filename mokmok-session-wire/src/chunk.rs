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

    use mokmok_agent_runtime::{Budget, Record, Setting, Task, TaskRecord};
    use mokmok_event_store::Payload;
    use mokmok_protocol::{AgentID, CommandID, SessionID};
    use mokmok_state_machine::{Ending, Phase};
    use uuid::Uuid;

    use super::{ChunkError, HISTORY_BYTES, ROSTER_BYTES, history_chunks, roster_chunks};
    use crate::{DaemonEvent, HistoryChunk, RosterEntry, SessionEvent};

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

    fn speech_of_size(bytes: usize) -> Record {
        let empty = Payload::json(&speech("")).unwrap().as_str().len();

        speech(&"x".repeat(bytes - empty))
    }

    fn entry_of_size(
        index: u64,
        bytes: usize,
    ) -> RosterEntry {
        let base = RosterEntry {
            session: SessionID::from_uuid(Uuid::from_u128(100 + u128::from(index))),
            phase: Phase::Closed(Ending::Completed),
            goal: Some(String::new()),
            checkout: None,
            turns: index,
        };
        let empty = Payload::json(&base).unwrap().as_str().len();

        RosterEntry {
            goal: Some("x".repeat(bytes - empty)),
            ..base
        }
    }

    fn task() -> TaskRecord {
        TaskRecord {
            task: Task {
                goal: "hold a conversation".to_owned(),
                setting: Setting {
                    checkout: "/checkout".into(),
                    readable: Vec::new(),
                    writable: Vec::new(),
                    program: "/bin/sh".into(),
                    deadline: std::time::Duration::from_secs(120),
                    search: Vec::new(),
                    skills: Vec::new(),
                    delegate: false,
                },
                budget: Budget::default(),
            },
        }
    }

    fn record_bytes(record: &Record) -> usize {
        Payload::json(record).unwrap().as_str().len()
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

    #[test]
    fn history_fills_each_chunk_to_its_ceiling_and_seeds_only_the_last() {
        let records: Vec<Record> = (0..9).map(|_| speech_of_size(64 * 1024)).collect();
        let seed = task();

        let chunks =
            history_chunks(session(), request(), Some(seed.clone()), records.clone()).unwrap();

        assert_eq!(chunks.len(), 3);
        let mut collected = Vec::new();
        let mut held = Vec::new();
        for (index, event) in chunks.iter().enumerate() {
            let SessionEvent::History(chunk) = event else {
                panic!("a history request answers a history chunk");
            };
            assert_eq!(chunk.request, request());
            assert_eq!(chunk.session, session());
            assert_eq!(chunk.index, u32::try_from(index).unwrap());
            assert_eq!(chunk.last, index + 1 == chunks.len());
            held.push(chunk.records.len());
            collected.extend(chunk.records.iter().cloned());
            if chunk.last {
                assert_eq!(chunk.seed.as_ref(), Some(&seed));
            } else {
                assert!(chunk.seed.is_none());
            }
        }
        assert_eq!(held, [4, 4, 1]);
        assert_eq!(collected, records);
        assert_eq!(
            [0, 1]
                .iter()
                .map(|index| {
                    let SessionEvent::History(chunk) = &chunks[*index] else {
                        panic!("a history request answers a history chunk");
                    };
                    chunk.records.iter().map(record_bytes).sum::<usize>()
                })
                .collect::<Vec<_>>(),
            [HISTORY_BYTES, HISTORY_BYTES],
        );
    }

    #[test]
    fn history_keeps_a_record_bigger_than_a_chunk_in_one_chunk() {
        let record = speech_of_size(HISTORY_BYTES + 4096);

        let chunks = history_chunks(session(), request(), None, vec![record.clone()]).unwrap();

        assert_eq!(chunks.len(), 1);
        let SessionEvent::History(chunk) = &chunks[0] else {
            panic!("a history request answers a history chunk");
        };
        assert_eq!(chunk.index, 0);
        assert!(chunk.last);
        assert_eq!(chunk.records, vec![record]);
    }

    #[test]
    fn history_refuses_a_record_that_exceeds_the_bus_frame() {
        let record = speech_of_size(1024 * 1024 + 4096);

        let error = history_chunks(session(), request(), None, vec![record]).unwrap_err();

        match error {
            ChunkError::RecordTooLarge { limit, found } => {
                assert_eq!(limit, 1024 * 1024);
                assert!(found > limit, "{found}");
            },
            other @ ChunkError::Payload(_) => {
                panic!("an oversized record is refused, not {other:?}");
            },
        }
    }

    #[test]
    fn a_history_payload_exactly_at_the_frame_ceiling_is_published() {
        let ceiling = 1024 * 1024;
        let probe = speech_of_size(ceiling);
        let payload = |record: &Record| {
            Payload::json(&SessionEvent::History(HistoryChunk {
                request: request(),
                session: session(),
                index: 0,
                last: true,
                seed: None,
                records: vec![record.clone()],
            }))
            .unwrap()
            .as_str()
            .len()
        };
        let overhead = payload(&probe) - record_bytes(&probe);
        let record = speech_of_size(ceiling - overhead);
        assert_eq!(payload(&record), ceiling);

        let chunks = history_chunks(session(), request(), None, vec![record]).unwrap();

        assert_eq!(chunks.len(), 1);
        assert_eq!(Payload::json(&chunks[0]).unwrap().as_str().len(), ceiling);
    }

    #[test]
    fn roster_fills_each_chunk_to_its_ceiling_and_marks_only_the_last() {
        let entries: Vec<RosterEntry> = (0..9)
            .map(|index| entry_of_size(index, 64 * 1024))
            .collect();

        let chunks = roster_chunks(agent(), 7, entries.clone()).unwrap();

        assert_eq!(chunks.len(), 3);
        let mut seen = Vec::new();
        let mut held = Vec::new();
        for (index, event) in chunks.iter().enumerate() {
            let DaemonEvent::Roster(chunk) = event else {
                panic!("a roster request answers a roster chunk");
            };
            assert_eq!(chunk.daemon, agent());
            assert_eq!(chunk.revision, 7);
            assert_eq!(chunk.index, u32::try_from(index).unwrap());
            assert_eq!(chunk.last, index + 1 == chunks.len());
            held.push(chunk.entries.len());
            seen.extend(chunk.entries.iter().cloned());
        }
        assert_eq!(held, [4, 4, 1]);
        assert_eq!(seen, entries);
        assert_eq!(
            [0, 1]
                .iter()
                .map(|index| {
                    let DaemonEvent::Roster(chunk) = &chunks[*index] else {
                        panic!("a roster request answers a roster chunk");
                    };
                    chunk
                        .entries
                        .iter()
                        .map(|entry| Payload::json(entry).unwrap().as_str().len())
                        .sum::<usize>()
                })
                .collect::<Vec<_>>(),
            [ROSTER_BYTES, ROSTER_BYTES],
        );
    }

    #[test]
    fn roster_refuses_an_entry_that_exceeds_the_bus_frame() {
        let entry = entry_of_size(0, 1024 * 1024 + 4096);

        let error = roster_chunks(agent(), 7, vec![entry]).unwrap_err();

        match error {
            ChunkError::RecordTooLarge { limit, found } => {
                assert_eq!(limit, 1024 * 1024);
                assert!(found > limit, "{found}");
            },
            other @ ChunkError::Payload(_) => {
                panic!("an oversized entry is refused, not {other:?}");
            },
        }
    }

    #[test]
    fn roster_keeps_an_entry_bigger_than_a_chunk_in_one_chunk() {
        let entry = entry_of_size(0, ROSTER_BYTES + 4096);

        let chunks = roster_chunks(agent(), 7, vec![entry.clone()]).unwrap();

        assert_eq!(chunks.len(), 1);
        let DaemonEvent::Roster(chunk) = &chunks[0] else {
            panic!("a roster request answers a roster chunk");
        };
        assert_eq!(chunk.index, 0);
        assert!(chunk.last);
        assert_eq!(chunk.entries, vec![entry]);
    }
}
