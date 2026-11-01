use std::sync::Arc;

use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tracing::Level;

/// One captured tracing event.
///
/// The record keeps the formatted message and the structured fields, not the
/// original `tracing` event. It is a plain value with no GPUI dependency, so
/// the view can hold it and a test can build one.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// When the event reached the subscriber.
    pub time: OffsetDateTime,
    /// The same instant as an RFC 3339 UTC string, formatted once.
    pub timestamp: Arc<str>,
    /// Event level.
    pub level: Level,
    /// The `module_path!()` target, or the `log` target through the bridge.
    pub target: Arc<str>,
    /// The name of the thread that emitted the event, when it has one.
    pub thread: Option<Arc<str>>,
    /// The event's message.
    pub message: Arc<str>,
    /// Structured fields, in declaration order, with `op` repeated here.
    pub fields: Vec<(Arc<str>, Arc<str>)>,
    /// The command id when the event carried `op`, for the Diagnostics filter.
    pub op: Option<Arc<str>>,
}

impl LogRecord {
    pub(crate) fn new(
        time: OffsetDateTime,
        level: Level,
        target: &str,
        thread: Option<&str>,
        message: String,
        fields: Vec<(String, String)>,
    ) -> Self {
        let timestamp = time.format(&Rfc3339).unwrap_or_default();
        let op = fields
            .iter()
            .find(|(name, _)| name == "op")
            .map(|(_, value)| Arc::from(value.as_str()));

        Self {
            time,
            timestamp: Arc::from(timestamp),
            level,
            target: Arc::from(target),
            thread: thread.map(Arc::from),
            message: Arc::from(message),
            fields: fields
                .into_iter()
                .map(|(name, value)| (Arc::from(name), Arc::from(value)))
                .collect(),
            op,
        }
    }

    /// One structured field by name.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(field, _)| field.as_ref() == name)
            .map(|(_, value)| value.as_ref())
    }
}

/// A frozen copy of the in-memory ring for one render.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct LogSnapshot {
    /// Records oldest first.
    pub records: Vec<LogRecord>,
    /// Records evicted because the ring was full.
    pub dropped: u64,
    /// Increases with every accepted record; compare to avoid a full repaint.
    pub revision: u64,
}
