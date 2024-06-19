use foundations::telemetry::metrics::{metrics, Gauge};

#[metrics]
pub mod system {
    /// Current system version.
    pub fn version() -> Gauge;
}
