use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use time::OffsetDateTime;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::layer::{Context, Layer};

use crate::record::{LogRecord, LogSnapshot};

const DEFAULT_CAPACITY: usize = 5_000;

/// A bounded, thread-safe ring of the newest log records.
///
/// The ring is the live tail the Diagnostics view reads. It is not the file:
/// when the ring is full the oldest record is dropped and counted, while the
/// file keeps every record it received. Cloning the store is cheap; every
/// clone shares the same ring.
#[derive(Clone, Debug)]
pub struct LogStore {
    inner: Arc<Mutex<Ring>>,
}

#[derive(Debug)]
struct Ring {
    records: VecDeque<LogRecord>,
    capacity: usize,
    dropped: u64,
    revision: u64,
}

impl LogStore {
    /// A store that keeps at most `capacity` records.
    ///
    /// A capacity of zero is raised to one, because a ring that can never hold
    /// a record is indistinguishable from a broken view.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Ring {
                records: VecDeque::with_capacity(capacity.clamp(1, 1_024)),
                capacity: capacity.max(1),
                dropped: 0,
                revision: 0,
            })),
        }
    }

    /// The newest records, oldest first, with the drop and revision counters.
    pub fn snapshot(&self) -> LogSnapshot {
        let ring = self.lock();

        LogSnapshot {
            records: ring.records.iter().cloned().collect(),
            dropped: ring.dropped,
            revision: ring.revision,
        }
    }

    /// How many records the ring currently holds.
    pub fn len(&self) -> usize {
        self.lock().records.len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many records were evicted because the ring was full.
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }

    /// The number of accepted records, including evicted ones.
    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    pub(crate) fn push(&self, mut record: LogRecord) {
        let mut ring = self.lock();

        let sequence = ring.revision.saturating_add(1);
        record.sequence = sequence;

        if ring.records.len() >= ring.capacity {
            ring.records.pop_front();
            ring.dropped = ring.dropped.saturating_add(1);
        }

        ring.records.push_back(record);
        ring.revision = sequence;
    }

    fn lock(&self) -> MutexGuard<'_, Ring> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// The subscriber layer that copies events into a [`LogStore`].
pub(crate) struct StoreLayer {
    store: LogStore,
}

impl StoreLayer {
    pub(crate) fn new(store: LogStore) -> Self {
        Self { store }
    }
}

impl<S: Subscriber> Layer<S> for StoreLayer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let thread = std::thread::current();
        let message = visitor.message.unwrap_or_else(|| "(event)".to_owned());

        self.store.push(LogRecord::new(
            OffsetDateTime::now_utc(),
            *metadata.level(),
            metadata.target(),
            thread.name(),
            message,
            visitor.fields,
        ));
    }
}

/// Collects the message and the structured fields of one event.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl FieldVisitor {
    fn record_text(&mut self, field: &Field, value: String) {
        match field.name() {
            "message" | "log.message" => self.message = Some(value),
            // `tracing-log` copies the `log` metadata into fields; the target
            // and level are already on the record, so these would be noise.
            name if name.starts_with("log.") => {}
            name => self.fields.push((name.to_owned(), value)),
        }
    }
}

impl Visit for FieldVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record_text(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_text(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_text(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_text(field, value.to_string());
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_text(field, value.to_owned());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.record_text(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.record_text(field, format!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Profile;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[test]
    fn the_ring_drops_the_oldest_record_and_counts_it() {
        let store = LogStore::new(2);
        for index in 0..3 {
            store.push(LogRecord::new(
                OffsetDateTime::now_utc(),
                tracing::Level::INFO,
                "oxtuner_test",
                None,
                format!("record {index}"),
                Vec::new(),
            ));
        }

        let snapshot = store.snapshot();

        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.dropped, 1);
        assert_eq!(snapshot.revision, 3);
        assert_eq!(snapshot.records[0].message.as_ref(), "record 1");
        assert_eq!(snapshot.records[0].sequence, 2);
        assert_eq!(snapshot.records[1].message.as_ref(), "record 2");
        assert_eq!(snapshot.records[1].sequence, 3);
    }

    #[test]
    fn the_layer_captures_the_message_fields_and_op() {
        let store = LogStore::new(10);
        let filter = crate::filter::default_filter(Profile::Debug, false);
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(StoreLayer::new(store.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "oxtuner_core::service",
                op = "op-7",
                pages = 2,
                "wrote two pages"
            );
        });

        let snapshot = store.snapshot();
        let record = &snapshot.records[0];

        assert_eq!(record.message.as_ref(), "wrote two pages");
        assert_eq!(record.target.as_ref(), "oxtuner_core::service");
        assert_eq!(record.op.as_deref(), Some("op-7"));
        assert_eq!(record.field("pages"), Some("2"));
        assert_eq!(record.level, tracing::Level::INFO);
        assert!(!record.local_timestamp.is_empty());
    }

    #[test]
    fn the_default_filter_keeps_dependency_records_out_of_the_ring() {
        let store = LogStore::new(10);
        let filter = crate::filter::default_filter(Profile::Debug, false);
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(StoreLayer::new(store.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "oxtuner_core::service", "workspace");
            tracing::debug!(target: "serial2::port", "dependency");
        });

        let snapshot = store.snapshot();

        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].message.as_ref(), "workspace");
    }
}
