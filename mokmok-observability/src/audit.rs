//! The durable record of a security decision.

use mokmok_protocol::{Moment, PrincipalID};

use crate::error::Error;
use crate::exporter::Exporter;
use crate::metrics::Metrics;

/// The counter a landed entry adds to.
const APPENDED: &str = "audit.appended";

/// The counter a refused export adds to.
const EXPORT_FAILURES: &str = "audit.export_failures";

/// What kind of decision an [`AuditEntry`] records.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum AuditKind {
    /// Who may do what, and with which grant.
    Authorization,
    /// A connection leaving the machine.
    Egress,
    /// A tool or model call.
    Execution,
    /// A change to the rules the process runs under.
    Configuration,
}

/// The decision an [`AuditEntry`] records.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum Outcome {
    /// The action was permitted or completed.
    Allowed,
    /// The action was refused.
    Denied,
    /// The action completed.
    Succeeded,
    /// The action was attempted and did not complete.
    Failed,
}

/// One security decision, as it is written down.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct AuditEntry {
    at: Moment,
    actor: PrincipalID,
    kind: AuditKind,
    outcome: Outcome,
    detail: Option<String>,
}

impl AuditEntry {
    /// An entry with no detail.
    #[must_use]
    pub const fn new(
        at: Moment,
        actor: PrincipalID,
        kind: AuditKind,
        outcome: Outcome,
    ) -> Self {
        Self {
            at,
            actor,
            kind,
            outcome,
            detail: None,
        }
    }

    /// The same entry with `detail` attached, replacing an earlier detail.
    #[must_use]
    pub fn with_detail(
        self,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            detail: Some(detail.into()),
            ..self
        }
    }

    /// The instant the decision was made.
    #[must_use]
    pub const fn at(&self) -> Moment {
        self.at
    }

    /// The principal the decision was made for.
    #[must_use]
    pub const fn actor(&self) -> PrincipalID {
        self.actor
    }

    /// What kind of decision this is.
    #[must_use]
    pub const fn kind(&self) -> AuditKind {
        self.kind
    }

    /// What was decided.
    #[must_use]
    pub const fn outcome(&self) -> Outcome {
        self.outcome
    }

    /// The free-form detail, when the caller supplied one.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// A durable, ordered record of security decisions.
///
/// An implementation returns from [`AuditLog::append`] only once the entry survives a restart:
/// a security fact that lives in a buffer is a fact a crash can take back.
pub trait AuditLog {
    /// Appends `entry` and does not return until it is durable.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry could not be made durable. A returned error means the
    /// entry did not land; a caller may retry it.
    fn append(
        &mut self,
        entry: AuditEntry,
    ) -> Result<(), Error>;

    /// Every entry at or after `since`, in the order it was appended.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Journal`] when the record cannot be read, and [`Error::Decode`] when it
    /// holds a line this build cannot parse.
    fn read(
        &self,
        since: Moment,
    ) -> Result<Vec<AuditEntry>, Error>;
}

/// Records `entry` in `log`, then offers the current metrics to `exporter`.
///
/// The append lands first and the export cannot undo it: the exporter is telemetry, and a
/// security fact must stay recorded when the collector is down. An append failure is returned
/// without calling the exporter at all, so the error says the entry is missing rather than
/// merely that the export failed.
///
/// # Errors
///
/// Returns the append's error when the entry could not be made durable, or [`Error::Export`]
/// when `exporter` refused the snapshot after the entry landed.
pub fn append_and_export(
    log: &mut dyn AuditLog,
    metrics: &mut Metrics,
    exporter: &dyn Exporter,
    entry: AuditEntry,
) -> Result<(), Error> {
    log.append(entry)?;
    metrics.increment(APPENDED);

    if let Err(error) = exporter.export(&metrics.snapshot()) {
        metrics.increment(EXPORT_FAILURES);
        return Err(error);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use mokmok_protocol::Moment;

    use super::{AuditEntry, AuditKind, AuditLog, Outcome, append_and_export};
    use crate::error::Error;
    use crate::exporter::Exporter;
    use crate::journal::Journal;
    use crate::metrics::{Metrics, Snapshot};
    use crate::support::{TempDir, actor};

    /// An exporter whose destination is always down.
    struct Refusing;

    impl Exporter for Refusing {
        fn export(
            &self,
            _snapshot: &Snapshot,
        ) -> Result<(), Error> {
            Err(Error::Export(Box::new(std::io::Error::other(
                "the collector is down",
            ))))
        }
    }

    #[test]
    fn a_failing_exporter_reports_an_error_after_the_entry_landed() {
        let dir = TempDir::new("failing-exporter");
        let path = dir.path().join("audit.jsonl");
        let mut log = Journal::open(&path).unwrap();
        let mut metrics = Metrics::new();
        let entry = AuditEntry::new(
            Moment::from_millis(1_700_000_000_000),
            actor(),
            AuditKind::Egress,
            Outcome::Denied,
        )
        .with_detail("connect api.example.com:443");

        let exported = append_and_export(&mut log, &mut metrics, &Refusing, entry.clone());

        assert!(matches!(exported, Err(Error::Export(_))));
        assert_eq!(metrics.counter("audit.appended"), 1);
        assert_eq!(metrics.counter("audit.export_failures"), 1);

        drop(log);
        let reopened = Journal::open(&path).unwrap();
        assert_eq!(reopened.read(Moment::EPOCH).unwrap(), vec![entry]);
    }
}
