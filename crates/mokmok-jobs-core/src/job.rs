//! The job state machine: states, events, commands, and the decisions between them.

use std::fmt;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use crate::port::Envelope;
use crate::stream::Version;

/// Where one job stands, derived only from its event stream.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum JobState {
    #[default]
    New,
    /// `attempt` is the attempt that will run next, starting at 1.
    Queued {
        input: Vec<u8>,
        attempt: u32,
        max_attempts: NonZeroU32,
    },
    Running {
        input: Vec<u8>,
        attempt: u32,
        max_attempts: NonZeroU32,
    },
    Succeeded {
        output: Vec<u8>,
    },
    Failed {
        reason: String,
    },
    Cancelled {
        reason: String,
    },
}

/// One committed fact about a job.
#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobEvent {
    Submitted {
        input: Vec<u8>,
        max_attempts: NonZeroU32,
    },
    Started {
        attempt: u32,
    },
    RetryScheduled {
        attempt: u32,
        reason: String,
    },
    Succeeded {
        output: Vec<u8>,
    },
    Failed {
        reason: String,
    },
    Cancelled {
        reason: String,
    },
}

/// A request to change a job, before it becomes an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobCommand {
    Submit {
        input: Vec<u8>,
        max_attempts: NonZeroU32,
    },
    Start,
    Succeed {
        output: Vec<u8>,
    },
    Fail {
        reason: String,
        retryable: bool,
    },
    Cancel {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobError {
    InvalidJobId(String),
    AlreadySubmitted,
    NotSubmitted,
    NotQueued,
    NotRunning,
    Terminal,
    CorruptHistory(String),
}

impl fmt::Display for JobError {
    fn fmt(
        &self,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        match self {
            Self::InvalidJobId(raw) => write!(formatter, "invalid job id {raw:?}"),
            Self::AlreadySubmitted => formatter.write_str("job was already submitted"),
            Self::NotSubmitted => formatter.write_str("job was not submitted"),
            Self::NotQueued => formatter.write_str("job is not queued"),
            Self::NotRunning => formatter.write_str("job is not running"),
            Self::Terminal => formatter.write_str("job is in a terminal state"),
            Self::CorruptHistory(message) => write!(formatter, "corrupt history: {message}"),
        }
    }
}

impl std::error::Error for JobError {}

impl JobState {
    const fn kind(&self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Queued { .. } => "queued",
            Self::Running { .. } => "running",
            Self::Succeeded { .. } => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Cancelled { .. } => "cancelled",
        }
    }
}

impl JobEvent {
    const fn kind(&self) -> &'static str {
        match self {
            Self::Submitted { .. } => "submitted",
            Self::Started { .. } => "started",
            Self::RetryScheduled { .. } => "retry_scheduled",
            Self::Succeeded { .. } => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Cancelled { .. } => "cancelled",
        }
    }
}

/// Decides the events a command commits, or why it is rejected.
///
/// # Errors
///
/// Returns the [`JobError`] naming the state the command does not fit.
pub fn decide(
    state: &JobState,
    command: JobCommand,
) -> Result<Vec<JobEvent>, JobError> {
    match state {
        JobState::New => match command {
            JobCommand::Submit {
                input,
                max_attempts,
            } => Ok(vec![JobEvent::Submitted {
                input,
                max_attempts,
            }]),
            JobCommand::Start
            | JobCommand::Succeed { .. }
            | JobCommand::Fail { .. }
            | JobCommand::Cancel { .. } => Err(JobError::NotSubmitted),
        },
        JobState::Queued { attempt, .. } => match command {
            JobCommand::Start => Ok(vec![JobEvent::Started { attempt: *attempt }]),
            JobCommand::Cancel { reason } => Ok(vec![JobEvent::Cancelled { reason }]),
            JobCommand::Submit { .. } => Err(JobError::AlreadySubmitted),
            JobCommand::Succeed { .. } | JobCommand::Fail { .. } => Err(JobError::NotRunning),
        },
        JobState::Running {
            attempt,
            max_attempts,
            ..
        } => match command {
            JobCommand::Succeed { output } => Ok(vec![JobEvent::Succeeded { output }]),
            JobCommand::Fail { reason, retryable } => {
                if retryable && *attempt < max_attempts.get() {
                    Ok(vec![JobEvent::RetryScheduled {
                        attempt: attempt + 1,
                        reason,
                    }])
                } else {
                    Ok(vec![JobEvent::Failed { reason }])
                }
            },
            JobCommand::Cancel { reason } => Ok(vec![JobEvent::Cancelled { reason }]),
            JobCommand::Submit { .. } => Err(JobError::AlreadySubmitted),
            JobCommand::Start => Err(JobError::NotQueued),
        },
        JobState::Succeeded { .. } | JobState::Failed { .. } | JobState::Cancelled { .. } => {
            Err(JobError::Terminal)
        },
    }
}

/// Applies one committed event to a state, rejecting a pair no decision produces.
///
/// # Errors
///
/// Returns [`JobError::CorruptHistory`] when the event does not fit the state.
pub fn evolve(
    state: &mut JobState,
    event: &JobEvent,
) -> Result<(), JobError> {
    let next = match (&*state, event) {
        (
            JobState::New,
            JobEvent::Submitted {
                input,
                max_attempts,
            },
        ) => JobState::Queued {
            input: input.clone(),
            attempt: 1,
            max_attempts: *max_attempts,
        },
        (
            JobState::Queued {
                input,
                attempt,
                max_attempts,
            },
            JobEvent::Started { attempt: started },
        ) if attempt == started => JobState::Running {
            input: input.clone(),
            attempt: *attempt,
            max_attempts: *max_attempts,
        },
        (
            JobState::Running {
                input,
                attempt,
                max_attempts,
            },
            JobEvent::RetryScheduled {
                attempt: next_attempt,
                ..
            },
        ) if *next_attempt == attempt + 1 && *next_attempt <= max_attempts.get() => {
            JobState::Queued {
                input: input.clone(),
                attempt: *next_attempt,
                max_attempts: *max_attempts,
            }
        },
        (JobState::Running { .. }, JobEvent::Succeeded { output }) => JobState::Succeeded {
            output: output.clone(),
        },
        (JobState::Running { .. }, JobEvent::Failed { reason }) => JobState::Failed {
            reason: reason.clone(),
        },
        (JobState::Queued { .. } | JobState::Running { .. }, JobEvent::Cancelled { reason }) => {
            JobState::Cancelled {
                reason: reason.clone(),
            }
        },
        _ => {
            return Err(JobError::CorruptHistory(format!(
                "state {} cannot take event {}",
                state.kind(),
                event.kind()
            )));
        },
    };
    *state = next;
    Ok(())
}

impl JobState {
    /// Rebuilds the state of one stream, enforcing identity and consecutive versions.
    ///
    /// # Errors
    ///
    /// Returns [`JobError::CorruptHistory`] when the envelopes mix job ids, skip a
    /// version, or carry an event the preceding state cannot take.
    pub fn replay(events: &[Envelope]) -> Result<Self, JobError> {
        let Some(first) = events.first() else {
            return Ok(Self::New);
        };
        let mut state = Self::New;
        let mut version = Version::new(1);
        for envelope in events {
            if envelope.job_id != first.job_id {
                return Err(JobError::CorruptHistory(format!(
                    "stream changed from {} to {}",
                    first.job_id.as_str(),
                    envelope.job_id.as_str()
                )));
            }
            if envelope.version != version {
                return Err(JobError::CorruptHistory(format!(
                    "expected version {} but found {}",
                    version.get(),
                    envelope.version.get()
                )));
            }
            evolve(&mut state, &envelope.event)?;
            version = version.next();
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::JobId;
    use proptest::prelude::*;

    fn attempts(count: u32) -> NonZeroU32 {
        NonZeroU32::new(count).expect("test attempts are nonzero")
    }

    fn input() -> Vec<u8> {
        b"payload".to_vec()
    }

    fn submitted() -> JobEvent {
        JobEvent::Submitted {
            input: input(),
            max_attempts: attempts(2),
        }
    }

    #[test]
    fn decide_accepts_submit_only_on_new() {
        assert_eq!(
            decide(
                &JobState::New,
                JobCommand::Submit {
                    input: input(),
                    max_attempts: attempts(2),
                }
            ),
            Ok(vec![submitted()])
        );
        for command in [
            JobCommand::Start,
            JobCommand::Succeed {
                output: b"out".to_vec(),
            },
            JobCommand::Fail {
                reason: "boom".to_owned(),
                retryable: true,
            },
            JobCommand::Cancel {
                reason: "stop".to_owned(),
            },
        ] {
            assert_eq!(decide(&JobState::New, command), Err(JobError::NotSubmitted));
        }
    }

    #[test]
    fn decide_queued_state() {
        let queued = JobState::Queued {
            input: input(),
            attempt: 3,
            max_attempts: attempts(4),
        };
        assert_eq!(
            decide(&queued, JobCommand::Start),
            Ok(vec![JobEvent::Started { attempt: 3 }])
        );
        assert_eq!(
            decide(
                &queued,
                JobCommand::Cancel {
                    reason: "stop".to_owned(),
                }
            ),
            Ok(vec![JobEvent::Cancelled {
                reason: "stop".to_owned(),
            }])
        );
        assert_eq!(
            decide(
                &queued,
                JobCommand::Submit {
                    input: input(),
                    max_attempts: attempts(2),
                }
            ),
            Err(JobError::AlreadySubmitted)
        );
        assert_eq!(
            decide(
                &queued,
                JobCommand::Succeed {
                    output: b"out".to_vec(),
                }
            ),
            Err(JobError::NotRunning)
        );
        assert_eq!(
            decide(
                &queued,
                JobCommand::Fail {
                    reason: "boom".to_owned(),
                    retryable: false,
                }
            ),
            Err(JobError::NotRunning)
        );
    }

    #[test]
    fn decide_running_state() {
        let running = JobState::Running {
            input: input(),
            attempt: 1,
            max_attempts: attempts(3),
        };
        assert_eq!(
            decide(
                &running,
                JobCommand::Succeed {
                    output: b"out".to_vec(),
                }
            ),
            Ok(vec![JobEvent::Succeeded {
                output: b"out".to_vec(),
            }])
        );
        assert_eq!(
            decide(
                &running,
                JobCommand::Fail {
                    reason: "flaky".to_owned(),
                    retryable: true,
                }
            ),
            Ok(vec![JobEvent::RetryScheduled {
                attempt: 2,
                reason: "flaky".to_owned(),
            }])
        );
        assert_eq!(
            decide(
                &running,
                JobCommand::Fail {
                    reason: "broken".to_owned(),
                    retryable: false,
                }
            ),
            Ok(vec![JobEvent::Failed {
                reason: "broken".to_owned(),
            }])
        );
        let last = JobState::Running {
            input: input(),
            attempt: 3,
            max_attempts: attempts(3),
        };
        assert_eq!(
            decide(
                &last,
                JobCommand::Fail {
                    reason: "flaky".to_owned(),
                    retryable: true,
                }
            ),
            Ok(vec![JobEvent::Failed {
                reason: "flaky".to_owned(),
            }])
        );
        assert_eq!(
            decide(&running, JobCommand::Start),
            Err(JobError::NotQueued)
        );
        assert_eq!(
            decide(
                &running,
                JobCommand::Submit {
                    input: input(),
                    max_attempts: attempts(2),
                }
            ),
            Err(JobError::AlreadySubmitted)
        );
    }

    #[test]
    fn decide_terminal_states_reject_every_command() {
        let terminal = [
            JobState::Succeeded {
                output: b"out".to_vec(),
            },
            JobState::Failed {
                reason: "broken".to_owned(),
            },
            JobState::Cancelled {
                reason: "stop".to_owned(),
            },
        ];
        for state in terminal {
            for command in every_command() {
                assert_eq!(decide(&state, command), Err(JobError::Terminal));
            }
        }
    }

    #[test]
    fn evolve_applies_every_legal_transition() {
        let mut state = JobState::New;
        evolve(&mut state, &submitted()).expect("submitted follows new");
        assert_eq!(
            state,
            JobState::Queued {
                input: input(),
                attempt: 1,
                max_attempts: attempts(2),
            }
        );
        evolve(&mut state, &JobEvent::Started { attempt: 1 }).expect("started follows queued");
        assert_eq!(
            state,
            JobState::Running {
                input: input(),
                attempt: 1,
                max_attempts: attempts(2),
            }
        );
        evolve(
            &mut state,
            &JobEvent::RetryScheduled {
                attempt: 2,
                reason: "flaky".to_owned(),
            },
        )
        .expect("retry follows running");
        assert_eq!(
            state,
            JobState::Queued {
                input: input(),
                attempt: 2,
                max_attempts: attempts(2),
            }
        );
        evolve(&mut state, &JobEvent::Started { attempt: 2 }).expect("started follows queued");
        evolve(
            &mut state,
            &JobEvent::Succeeded {
                output: b"out".to_vec(),
            },
        )
        .expect("succeeded follows running");
        assert_eq!(
            state,
            JobState::Succeeded {
                output: b"out".to_vec(),
            }
        );
    }

    #[test]
    fn evolve_applies_failure_and_cancellation() {
        let mut running = JobState::Running {
            input: input(),
            attempt: 1,
            max_attempts: attempts(1),
        };
        evolve(
            &mut running,
            &JobEvent::Failed {
                reason: "broken".to_owned(),
            },
        )
        .expect("failed follows running");
        assert_eq!(
            running,
            JobState::Failed {
                reason: "broken".to_owned(),
            }
        );

        let mut queued = JobState::Queued {
            input: input(),
            attempt: 1,
            max_attempts: attempts(1),
        };
        evolve(
            &mut queued,
            &JobEvent::Cancelled {
                reason: "stop".to_owned(),
            },
        )
        .expect("cancelled follows queued");
        assert_eq!(
            queued,
            JobState::Cancelled {
                reason: "stop".to_owned(),
            }
        );

        let mut running = JobState::Running {
            input: input(),
            attempt: 1,
            max_attempts: attempts(1),
        };
        evolve(
            &mut running,
            &JobEvent::Cancelled {
                reason: "stop".to_owned(),
            },
        )
        .expect("cancelled follows running");
        assert!(matches!(running, JobState::Cancelled { .. }));
    }

    #[test]
    fn evolve_rejects_every_illegal_pair() {
        let queued = || JobState::Queued {
            input: input(),
            attempt: 1,
            max_attempts: attempts(2),
        };
        let running = || JobState::Running {
            input: input(),
            attempt: 1,
            max_attempts: attempts(2),
        };
        let illegal = [
            (JobState::New, JobEvent::Started { attempt: 1 }),
            (
                JobState::New,
                JobEvent::Cancelled {
                    reason: "stop".to_owned(),
                },
            ),
            (queued(), JobEvent::Started { attempt: 2 }),
            (
                queued(),
                JobEvent::Succeeded {
                    output: b"out".to_vec(),
                },
            ),
            (queued(), submitted()),
            (running(), JobEvent::Started { attempt: 1 }),
            (
                running(),
                JobEvent::RetryScheduled {
                    attempt: 1,
                    reason: "flaky".to_owned(),
                },
            ),
            (
                running(),
                JobEvent::RetryScheduled {
                    attempt: 3,
                    reason: "flaky".to_owned(),
                },
            ),
            (
                JobState::Succeeded {
                    output: b"out".to_vec(),
                },
                JobEvent::Cancelled {
                    reason: "stop".to_owned(),
                },
            ),
        ];
        for (mut state, event) in illegal {
            let before = state.clone();
            assert!(matches!(
                evolve(&mut state, &event),
                Err(JobError::CorruptHistory(_))
            ));
            assert_eq!(state, before);
        }
    }

    #[test]
    fn every_job_error_has_a_message() {
        let cases = [
            (JobError::InvalidJobId("bad id".to_owned()), "bad id"),
            (JobError::AlreadySubmitted, "already submitted"),
            (JobError::NotSubmitted, "not submitted"),
            (JobError::NotQueued, "not queued"),
            (JobError::NotRunning, "not running"),
            (JobError::Terminal, "terminal"),
            (JobError::CorruptHistory("stale".to_owned()), "stale"),
        ];
        for (error, expected) in cases {
            assert!(error.to_string().contains(expected));
        }
    }

    #[test]
    fn corrupt_history_names_the_state_and_the_event() {
        let mut state = JobState::New;
        let error = evolve(&mut state, &JobEvent::Started { attempt: 1 })
            .expect_err("started cannot follow new");
        let message = error.to_string();
        assert!(message.contains("new"), "{message}");
        assert!(message.contains("started"), "{message}");
    }

    #[test]
    fn replay_of_an_empty_stream_is_new() {
        assert_eq!(JobState::replay(&[]), Ok(JobState::New));
    }

    #[test]
    fn replay_rejects_skipped_versions() {
        let id = JobId::parse("replay").expect("literal is valid");
        let events = vec![
            Envelope {
                job_id: id.clone(),
                version: Version::new(1),
                event: submitted(),
            },
            Envelope {
                job_id: id,
                version: Version::new(3),
                event: JobEvent::Started { attempt: 1 },
            },
        ];
        assert!(matches!(
            JobState::replay(&events),
            Err(JobError::CorruptHistory(_))
        ));
    }

    #[test]
    fn replay_rejects_mixed_streams() {
        let first = JobId::parse("first").expect("literal is valid");
        let second = JobId::parse("second").expect("literal is valid");
        let events = vec![
            Envelope {
                job_id: first,
                version: Version::new(1),
                event: submitted(),
            },
            Envelope {
                job_id: second,
                version: Version::new(2),
                event: JobEvent::Started { attempt: 1 },
            },
        ];
        assert!(matches!(
            JobState::replay(&events),
            Err(JobError::CorruptHistory(_))
        ));
    }

    fn every_command() -> Vec<JobCommand> {
        vec![
            JobCommand::Submit {
                input: input(),
                max_attempts: attempts(2),
            },
            JobCommand::Start,
            JobCommand::Succeed {
                output: b"out".to_vec(),
            },
            JobCommand::Fail {
                reason: "boom".to_owned(),
                retryable: true,
            },
            JobCommand::Cancel {
                reason: "stop".to_owned(),
            },
        ]
    }

    fn command_strategy() -> impl Strategy<Value = JobCommand> {
        prop_oneof![
            (prop::collection::vec(any::<u8>(), 0..8), 1u32..5).prop_map(
                |(input, max_attempts)| {
                    JobCommand::Submit {
                        input,
                        max_attempts: NonZeroU32::new(max_attempts).expect("range excludes zero"),
                    }
                }
            ),
            Just(JobCommand::Start),
            prop::collection::vec(any::<u8>(), 0..8)
                .prop_map(|output| JobCommand::Succeed { output }),
            any::<bool>().prop_map(|retryable| JobCommand::Fail {
                reason: "boom".to_owned(),
                retryable,
            }),
            Just(JobCommand::Cancel {
                reason: "stop".to_owned(),
            }),
        ]
    }

    proptest! {
        #[test]
        fn replay_matches_the_decisions_that_wrote_it(
            commands in prop::collection::vec(command_strategy(), 0..24),
        ) {
            let id = JobId::parse("proptest").expect("literal is valid");
            let mut state = JobState::New;
            let mut log: Vec<Envelope> = Vec::new();
            for command in commands {
                let Ok(events) = decide(&state, command) else {
                    continue;
                };
                for event in events {
                    let position = u64::try_from(log.len()).expect("log length fits u64");
                    let envelope = Envelope {
                        job_id: id.clone(),
                        version: Version::new(position + 1),
                        event,
                    };
                    evolve(&mut state, &envelope.event).expect("decide produced a legal event");
                    log.push(envelope);
                }
            }
            let replayed = JobState::replay(&log).expect("log is consecutive");
            prop_assert_eq!(&replayed, &state);
            if let JobState::Running { attempt, max_attempts, .. } = &state {
                prop_assert!(attempt <= &max_attempts.get());
            }
            if matches!(
                &state,
                JobState::Succeeded { .. } | JobState::Failed { .. } | JobState::Cancelled { .. }
            ) {
                for command in every_command() {
                    prop_assert_eq!(decide(&state, command), Err(JobError::Terminal));
                }
            }
        }
    }
}
