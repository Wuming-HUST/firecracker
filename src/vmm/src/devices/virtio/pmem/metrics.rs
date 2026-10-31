// SPDX-License-Identifier: Apache-2.0
use crate::logger::SharedIncMetric;
use serde::Serialize;
#[derive(Debug, Serialize)]
/// Counters for pmem requests and event failures.
pub struct PmemMetrics {
    /// Flushes.
    pub flushes: SharedIncMetric,
    /// Malformed requests.
    pub malformed_requests: SharedIncMetric,
    /// Event errors.
    pub event_errors: SharedIncMetric,
}
/// Global pmem metrics.
pub static METRICS: PmemMetrics = PmemMetrics {
    flushes: SharedIncMetric::new(),
    malformed_requests: SharedIncMetric::new(),
    event_errors: SharedIncMetric::new(),
};
/// Immutable pmem flush metrics<s.
pub fn flush_metrics<S: serde::Serializer>(serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut seq = serializer.serialize_map(Some(1))?;
    seq.serialize_entry("pmem", &METRICS)?;
    seq.end()
}
