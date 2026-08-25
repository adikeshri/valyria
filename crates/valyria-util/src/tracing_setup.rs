//! Tracing/logging setup (observability, §3): structured, `tracing`-based,
//! with a span per step/tool/model call expected from every subsystem
//! above this crate. This module only owns *initialization*; the spans
//! themselves are created where the work happens.

use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable, for interactive terminal use.
    Pretty,
    /// One JSON object per line, for `~/.valyria/logs` (§48) and log
    /// shipping.
    Json,
}

/// Initialize the global tracing subscriber. Safe to call more than once —
/// subsequent calls are no-ops rather than panicking, since tests and
/// embedded-runtime callers may both try to initialize logging.
pub fn init_tracing(format: LogFormat) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let result = match format {
        LogFormat::Pretty => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .try_init(),
        LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_target(true)
            .try_init(),
    };

    if let Err(e) = result {
        tracing::debug!("tracing subscriber already initialized: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        init_tracing(LogFormat::Pretty);
        init_tracing(LogFormat::Pretty); // must not panic
    }
}
