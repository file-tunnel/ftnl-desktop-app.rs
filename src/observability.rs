//! ORES OpenTelemetry-compatible structured logging.
//!
//! Only constant event names and bounded counters enter records. Pairing URIs,
//! capabilities, tickets, filenames, file IDs, paths, and file bytes must never
//! be passed to this module.

use std::sync::Arc;

use next_loggers::{json, JsonObject, Logger, LoggerError, OpenTelemetryTransport, Options, Value};

pub fn logger() -> Logger {
    let transport = Arc::new(OpenTelemetryTransport::new(|record| {
        let encoded = serde_json::to_string(&record)
            .map_err(|error| LoggerError(format!("cannot encode OTEL log record: {error}")))?;
        eprintln!("{encoded}");
        Ok(())
    }));
    let mut options = Options::default().with_transport(transport);
    options.app_name = "ftnl-desktop-app".into();
    options.name = Some("desktop".into());
    options.console = false;
    Logger::new(options)
}

/// `trace_id` and `routine_id` are always inline `ores-trace-` /
/// `ores-routine-` literals supplied by the call site. They are correlation
/// identifiers only: nothing derived from a pairing URI, capability, ticket,
/// filename, file id, path or file byte ever reaches this module.
pub fn event(logger: &Logger, name: &'static str, trace_id: &'static str, routine_id: &'static str) {
    let _ = logger
        .info(vec![Value::String(name.into())])
        .add_fields(JsonObject::from_iter([
            ("event.name".into(), json!(name)),
            ("data.classification".into(), json!("metadata-free")),
        ]))
        .add_tags(["file-tunnel", "desktop"])
        .add_trace(trace_id, false)
        .add_routine_id(routine_id)
        .send();
}

/// Error-level counterpart to [`event`]. Callers log and then return the
/// failure they already intended to return; this never changes an outcome.
pub fn event_error(
    logger: &Logger,
    name: &'static str,
    trace_id: &'static str,
    routine_id: &'static str,
) {
    let _ = logger
        .error(vec![Value::String(name.into())])
        .add_fields(JsonObject::from_iter([
            ("event.name".into(), json!(name)),
            ("data.classification".into(), json!("metadata-free")),
        ]))
        .add_tags(["file-tunnel", "desktop"])
        .add_trace(trace_id, false)
        .add_routine_id(routine_id)
        .send();
}

pub fn event_with_count(
    logger: &Logger,
    name: &'static str,
    count: usize,
    trace_id: &'static str,
    routine_id: &'static str,
) {
    let _ = logger
        .info(vec![Value::String(name.into())])
        .add_fields(JsonObject::from_iter([
            ("event.name".into(), json!(name)),
            ("item.count".into(), json!(count)),
            ("data.classification".into(), json!("metadata-free")),
        ]))
        .add_tags(["file-tunnel", "desktop"])
        .add_trace(trace_id, false)
        .add_routine_id(routine_id)
        .send();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_accepts_only_constant_test_event() {
        const ROUTINE_ID: &str = "ores-routine-C2ijuxjmSR17WC6jtqL6F";
        let logger = logger();
        event(
            &logger,
            "desktop.test",
            "ores-trace-LgjrIJvkaY_a3WbH1J05C",
            ROUTINE_ID,
        );
        event_error(
            &logger,
            "desktop.test.failed",
            "ores-trace-3aoDMThTzw_J6i0lMNDq4",
            ROUTINE_ID,
        );
        logger.close().unwrap();
    }
}
