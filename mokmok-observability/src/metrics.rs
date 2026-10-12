//! Counters and gauges a caller can read back and hand to an exporter.

use std::collections::BTreeMap;

/// A registry of counters and gauges.
///
/// Counters only ever grow; gauges hold whatever was set last. Both are keyed by a name, and a
/// name nothing has written reads as zero rather than as an error, so a dashboard never has to
/// distinguish "no reading" from "zero readings".
///
/// `#[derive(Default)]` gives an empty registry.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metrics {
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, i64>,
}

impl Metrics {
    /// An empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            counters: BTreeMap::new(),
            gauges: BTreeMap::new(),
        }
    }

    /// Adds one to the counter named `name`.
    pub fn increment(
        &mut self,
        name: &str,
    ) {
        self.add(name, 1);
    }

    /// Adds `by` to the counter named `name`, saturating rather than wrapping at `u64::MAX`.
    pub fn add(
        &mut self,
        name: &str,
        by: u64,
    ) {
        let counter = self.counters.entry(name.to_owned()).or_default();
        *counter = counter.saturating_add(by);
    }

    /// Sets the gauge named `name` to `value`, replacing whatever it held.
    pub fn set_gauge(
        &mut self,
        name: &str,
        value: i64,
    ) {
        self.gauges.insert(name.to_owned(), value);
    }

    /// The counter named `name`, or zero when nothing has counted it.
    #[must_use]
    pub fn counter(
        &self,
        name: &str,
    ) -> u64 {
        self.counters.get(name).copied().unwrap_or_default()
    }

    /// The gauge named `name`, or zero when nothing has set it.
    #[must_use]
    pub fn gauge(
        &self,
        name: &str,
    ) -> i64 {
        self.gauges.get(name).copied().unwrap_or_default()
    }

    /// Every counter and gauge as it stands now, ordered by name so that two exports of the same
    /// readings are the same bytes.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            counters: self.counters.clone(),
            gauges: self.gauges.clone(),
        }
    }
}

/// The counters and gauges one [`Metrics::snapshot`] saw, detached from the registry.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Snapshot {
    counters: BTreeMap<String, u64>,
    gauges: BTreeMap<String, i64>,
}

impl Snapshot {
    /// Every counter, ordered by name.
    #[must_use]
    pub const fn counters(&self) -> &BTreeMap<String, u64> {
        &self.counters
    }

    /// Every gauge, ordered by name.
    #[must_use]
    pub const fn gauges(&self) -> &BTreeMap<String, i64> {
        &self.gauges
    }

    /// The counter named `name`, or zero when nothing has counted it.
    #[must_use]
    pub fn counter(
        &self,
        name: &str,
    ) -> u64 {
        self.counters.get(name).copied().unwrap_or_default()
    }

    /// The gauge named `name`, or zero when nothing has set it.
    #[must_use]
    pub fn gauge(
        &self,
        name: &str,
    ) -> i64 {
        self.gauges.get(name).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::Metrics;

    #[test]
    fn a_counter_incremented_three_times_reads_three() {
        let mut metrics = Metrics::new();

        metrics.increment("turns.started");
        metrics.increment("turns.started");
        metrics.increment("turns.started");

        assert_eq!(metrics.counter("turns.started"), 3);
        assert_eq!(metrics.snapshot().counter("turns.started"), 3);
    }

    #[test]
    fn a_gauge_set_twice_reads_the_second_value() {
        let mut metrics = Metrics::new();

        metrics.set_gauge("sandbox.busy", 4);
        metrics.set_gauge("sandbox.busy", 7);

        assert_eq!(metrics.gauge("sandbox.busy"), 7);
        assert_eq!(metrics.snapshot().gauge("sandbox.busy"), 7);
    }
}
