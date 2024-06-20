use foundations::telemetry::metrics::{metrics, Counter, Gauge, HistogramBuilder, TimeHistogram};
use std::sync::Arc;

#[metrics]
pub mod system {
    /// Current system version.
    pub fn version() -> Gauge;

    #[ctor = HistogramBuilder {
        // 1 second to 601 seconds in regular intervals.
        buckets: &[1.0, 38.5, 76.0, 113.5, 151.0, 188.5, 226.0, 263.5, 301.0, 338.5, 376.0, 413.5, 451.0, 488.5, 526.0, 563.5, 601.0],
    }]
    pub fn configuration_download_duration(system_package_id: &Arc<String>) -> TimeHistogram;

    #[ctor = HistogramBuilder {
        // 50 milliseconds to 100 seconds.
        buckets: &[0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0],
    }]
    pub fn configuration_setup_duration(system_package_id: &Arc<String>) -> TimeHistogram;

    #[ctor = HistogramBuilder {
        // 1 second to 601 seconds in regular intervals.
        buckets: &[1.0, 38.5, 76.0, 113.5, 151.0, 188.5, 226.0, 263.5, 301.0, 338.5, 376.0, 413.5, 451.0, 488.5, 526.0, 563.5, 601.0],
    }]
    pub fn configuration_switch_duration(system_package_id: &Arc<String>) -> TimeHistogram;
}

#[metrics]
pub mod requests {
    /// Number of summary requests made to the agent since it started up.
    pub fn summary() -> Counter;

    /// Number of new configuration requests made to the agent since it started up.
    pub fn new_configuration() -> Counter;
}
