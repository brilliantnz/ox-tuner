//! The session notification inbox.
//!
//! The store is deliberately independent of GPUI: records are plain data, so
//! the aggregation, read and retention rules can be tested without a window.
//! The bell view and the toast layer read it through `notifications`.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// How many records the session inbox keeps.
///
/// At the limit the oldest read record is dropped first, then the oldest
/// unread one. The store is a session convenience, not an audit log; the
/// activity list and the log file keep the durable record.
const LIMIT: usize = 100;

/// How serious a record is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum NotificationSeverity {
    /// Something the user may want to know.
    #[default]
    Info,
    /// A command completed as asked.
    Success,
    /// A condition needs attention, usually soon.
    Warning,
    /// A command failed or a safety rule stopped it.
    Error,
}

/// The key that merges repeated outcomes into one record.
///
/// The command id keeps a retry of the same operation together, and a named
/// key does the same for a condition that is not tied to one command, such as
/// a lost link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotificationKey {
    /// The command that produced the outcome.
    Op(u64),
    /// A stable name for a repeated condition.
    Named(String),
}

impl NotificationKey {
    /// A key for a core operation.
    pub(crate) fn op(op: u64) -> Self {
        Self::Op(op)
    }

    /// A key for a named condition.
    pub(crate) fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }
}

/// Where clicking a record takes the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotificationAction {
    /// Open Diagnostics, optionally with a text filter such as `op=42`.
    OpenDiagnostics {
        /// The filter to put in the Diagnostics search input.
        filter: Option<String>,
    },
}

/// A record before the store gives it an id and a timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationDraft {
    key: Option<NotificationKey>,
    severity: NotificationSeverity,
    title: String,
    body: String,
    action: Option<NotificationAction>,
}

impl NotificationDraft {
    /// A record with no aggregation key and no action.
    pub(crate) fn new(
        severity: NotificationSeverity,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            key: None,
            severity,
            title: title.into(),
            body: body.into(),
            action: None,
        }
    }

    /// Merges this record with an earlier one that shares the key.
    pub(crate) fn key(mut self, key: NotificationKey) -> Self {
        self.key = Some(key);
        self
    }

    /// Adds the surface a click should open.
    pub(crate) fn action(mut self, action: NotificationAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// One record in the session inbox.
#[derive(Debug, Clone)]
pub(crate) struct NotificationRecord {
    id: u64,
    key: Option<NotificationKey>,
    severity: NotificationSeverity,
    title: String,
    body: String,
    created: Instant,
    read: bool,
    action: Option<NotificationAction>,
    count: u32,
}

impl NotificationRecord {
    /// Stable for the whole session; an aggregated record keeps its id.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// The severity the toast and the bell row use.
    pub(crate) fn severity(&self) -> NotificationSeverity {
        self.severity
    }

    /// One short line, never a stack trace.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// One sentence explaining the outcome.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// When the record was created or last aggregated.
    pub(crate) fn created(&self) -> Instant {
        self.created
    }

    /// Whether the user has clicked the record.
    pub(crate) fn read(&self) -> bool {
        self.read
    }

    /// How many outcomes this record represents.
    pub(crate) fn count(&self) -> u32 {
        self.count
    }

    /// The surface a click should open, when there is one.
    pub(crate) fn action(&self) -> Option<&NotificationAction> {
        self.action.as_ref()
    }
}

/// The app-owned session inbox.
#[derive(Debug, Default)]
pub(crate) struct NotificationStore {
    /// Newest first, so the bell reads in the order users expect.
    records: VecDeque<NotificationRecord>,
    next_id: u64,
}

impl NotificationStore {
    /// Adds a record and returns it.
    ///
    /// A record that shares a key with an existing one replaces it: the id is
    /// kept, the count goes up, the time moves to now and the record becomes
    /// unread again. This is what keeps a retried command from stacking.
    pub(crate) fn push(&mut self, draft: NotificationDraft) -> NotificationRecord {
        let mut record = NotificationRecord {
            id: self.next_id,
            key: draft.key.clone(),
            severity: draft.severity,
            title: draft.title,
            body: draft.body,
            created: Instant::now(),
            read: false,
            action: draft.action,
            count: 1,
        };
        self.next_id = self.next_id.wrapping_add(1);

        if let Some(key) = draft.key.as_ref()
            && let Some(index) = self
                .records
                .iter()
                .position(|existing| existing.key.as_ref() == Some(key))
            && let Some(existing) = self.records.remove(index)
        {
            record.id = existing.id;
            record.count = existing.count.saturating_add(1);
        }

        self.records.push_front(record.clone());
        self.drop_older_than_limit();

        record
    }

    /// Every record, newest first.
    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = &NotificationRecord> {
        self.records.iter()
    }

    /// How many records the user has not clicked.
    pub(crate) fn unread(&self) -> usize {
        self.records.iter().filter(|record| !record.read).count()
    }

    /// Marks one record read. Returns whether anything changed.
    pub(crate) fn mark_read(&mut self, id: u64) -> bool {
        let Some(record) = self.records.iter_mut().find(|record| record.id == id) else {
            return false;
        };

        if record.read {
            return false;
        }

        record.read = true;
        true
    }

    /// Removes one record. Returns whether anything changed.
    pub(crate) fn dismiss(&mut self, id: u64) -> bool {
        let Some(index) = self.records.iter().position(|record| record.id == id) else {
            return false;
        };

        self.records.remove(index);
        true
    }

    /// Removes every record. Returns whether anything changed.
    pub(crate) fn clear(&mut self) -> bool {
        if self.records.is_empty() {
            return false;
        }

        self.records.clear();
        true
    }

    /// Keeps the inbox bounded, dropping read records before unread ones.
    fn drop_older_than_limit(&mut self) {
        while self.records.len() > LIMIT {
            let oldest_read = self.records.iter().rposition(|record| record.read);
            match oldest_read {
                Some(index) => {
                    self.records.remove(index);
                }
                None => {
                    self.records.pop_back();
                }
            }
        }
    }
}

/// A compact age label for a record.
///
/// The inbox is session-only, so a relative time reads better than a clock
/// time that cannot be compared with anything from an earlier run.
pub(crate) fn age(created: Instant) -> Duration {
    created.elapsed()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn draft(title: &str) -> NotificationDraft {
        NotificationDraft::new(NotificationSeverity::Info, title, "body")
    }

    #[test]
    fn new_records_go_to_the_front_and_count_as_unread() {
        let mut store = NotificationStore::default();

        let first = store.push(draft("first"));
        let second = store.push(draft("second"));

        let titles: Vec<&str> = store.iter().map(NotificationRecord::title).collect();

        assert_eq!(titles, ["second", "first"]);
        assert_eq!(store.unread(), 2);
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn a_shared_key_updates_one_record_and_marks_it_unread_again() {
        let mut store = NotificationStore::default();
        let key = NotificationKey::op(7);

        let first = store.push(draft("first").key(key.clone()));
        assert!(store.mark_read(first.id()));
        assert_eq!(store.unread(), 0);

        let second = store
            .push(NotificationDraft::new(NotificationSeverity::Error, "second", "again").key(key));

        assert_eq!(first.id(), second.id());
        assert_eq!(store.iter().count(), 1);
        assert_eq!(second.count(), 2);
        assert_eq!(store.unread(), 1);
        assert_eq!(
            store.iter().next().map(NotificationRecord::title),
            Some("second")
        );
    }

    #[test]
    fn clicking_one_record_marks_only_that_record_read() {
        let mut store = NotificationStore::default();
        let first = store.push(draft("first"));
        let second = store.push(draft("second"));

        assert!(store.mark_read(first.id()));
        assert!(!first.read(), "the returned record is a snapshot");
        assert!(
            !store.mark_read(first.id()),
            "a second click changes nothing"
        );

        let read: Vec<(u64, bool)> = store
            .iter()
            .map(|record| (record.id(), record.read()))
            .collect();
        assert!(read.contains(&(first.id(), true)));
        assert!(read.contains(&(second.id(), false)));
    }

    #[test]
    fn dismiss_and_clear_remove_records_only() {
        let mut store = NotificationStore::default();
        let first = store.push(draft("first"));
        let second = store.push(draft("second"));

        assert!(store.dismiss(first.id()));
        assert_eq!(store.iter().count(), 1);
        assert!(!store.dismiss(first.id()));

        assert!(store.clear());
        assert!(store.iter().next().is_none());
        assert_ne!(second.id(), first.id());
        assert!(!store.clear());
    }

    #[test]
    fn the_limit_drops_the_oldest_read_record_first() {
        let mut store = NotificationStore::default();

        for index in 0..LIMIT {
            store.push(draft(&format!("record {index}")));
        }

        let oldest = store.records.back().expect("the inbox is full").id();
        store.mark_read(oldest);
        store.push(draft("newest"));

        assert_eq!(store.iter().count(), LIMIT);
        assert!(
            store.iter().all(|record| record.id() != oldest),
            "the oldest read record should be the one dropped"
        );
    }

    #[test]
    fn the_limit_drops_the_oldest_unread_record_when_nothing_was_read() {
        let mut store = NotificationStore::default();

        for index in 0..LIMIT {
            store.push(draft(&format!("record {index}")));
        }

        let oldest = store.records.back().expect("the inbox is full").id();
        store.push(draft("newest"));

        assert_eq!(store.iter().count(), LIMIT);
        assert!(
            store.iter().all(|record| record.id() != oldest),
            "the oldest unread record should be dropped when nothing is read"
        );
    }

    #[test]
    fn age_is_the_time_since_creation() {
        let record = NotificationRecord {
            id: 1,
            key: None,
            severity: NotificationSeverity::Info,
            title: "title".into(),
            body: "body".into(),
            created: Instant::now()
                .checked_sub(Duration::from_secs(65))
                .expect("the process has been running for a minute"),
            read: false,
            action: None,
            count: 1,
        };

        assert!(age(record.created()) >= Duration::from_secs(65));
    }
}
