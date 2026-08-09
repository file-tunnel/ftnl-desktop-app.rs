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

pub fn event(logger: &Logger, name: &'static str) {
    let _ = logger
        .info(vec![Value::String(name.into())])
        .add_fields(JsonObject::from_iter([
            ("event.name".into(), json!(name)),
            ("data.classification".into(), json!("metadata-free")),
        ]))
        .add_tags(["file-tunnel", "desktop"])
        .send();
}

pub fn event_with_count(logger: &Logger, name: &'static str, count: usize) {
    let _ = logger
        .info(vec![Value::String(name.into())])
        .add_fields(JsonObject::from_iter([
            ("event.name".into(), json!(name)),
            ("item.count".into(), json!(count)),
            ("data.classification".into(), json!("metadata-free")),
        ]))
        .add_tags(["file-tunnel", "desktop"])
        .send();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_accepts_only_constant_test_event() {
        let logger = logger();
        event(&logger, "desktop.test");
        logger.close().unwrap();
    }
}
