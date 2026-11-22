//! The notification policy.
//!
//! Every notification starts here from a typed core outcome. Views never
//! create records directly, and no log level turns into a record on its own:
//! the match below is the single place that decides what the user must know
//! now. Aggregation keys keep a retried command or a repeated condition in one
//! record instead of stacking copies.
//!
//! The draft carries only plain data, so it can be built without a window and
//! tested from the event values.

use oxtuner_core::{Event, Failure, FailureCode, OpId};

use super::{NotificationAction, NotificationDraft, NotificationKey, NotificationSeverity};
use crate::i18n::t;

/// A record for one core outcome, when that outcome deserves the user's
/// attention.
///
/// `link_was_lost` is the state before the event was folded, so a reconnect is
/// only announced when the link had visibly gone away first. A reconnect after
/// a hidden retry is diagnostic detail, not news.
///
/// A firmware probe that found nothing is not news either. The scan tries many
/// port, baud and family combinations, so a miss is a routine step: the wizard
/// that started it reports once, inline, when every attempt is spent.
pub(crate) fn for_event(event: &Event, link_was_lost: Option<&str>) -> Option<NotificationDraft> {
    match event {
        Event::ProjectOpened { view, .. } if view.read_only => {
            Some(read_only(view.read_only_reason.as_deref()))
        }
        Event::Connected { view, .. } if view.read_only => {
            Some(read_only(view.read_only_reason.as_deref()))
        }
        Event::ConnectionLost { reason } => Some(
            NotificationDraft::new(
                NotificationSeverity::Warning,
                t!("app.notifications.link_lost_title").to_string(),
                reason.clone(),
            )
            .key(NotificationKey::named("connection-lost"))
            .action(NotificationAction::OpenDiagnostics { filter: None }),
        ),
        Event::Reconnected { view } if link_was_lost.is_some() => Some(
            NotificationDraft::new(
                NotificationSeverity::Info,
                t!("app.notifications.link_restored_title").to_string(),
                t!(
                    "app.notifications.link_restored_body",
                    port = &view.port,
                    signature = &view.signature,
                )
                .to_string(),
            )
            .key(NotificationKey::named("connection-restored")),
        ),
        Event::TuneWritten { op, report } if report.verified => Some(
            NotificationDraft::new(
                NotificationSeverity::Success,
                t!("app.notifications.write_done_title").to_string(),
                t!(
                    "app.notifications.write_done_body",
                    pages = report.pages.to_string(),
                    cells = report.cells.to_string(),
                )
                .to_string(),
            )
            .key(NotificationKey::op(op.get())),
        ),
        Event::TuneBurned { op, report } if report.verified => Some(
            NotificationDraft::new(
                NotificationSeverity::Success,
                t!("app.notifications.burn_done_title").to_string(),
                t!(
                    "app.notifications.burn_done_body",
                    pages = report.pages.to_string(),
                )
                .to_string(),
            )
            .key(NotificationKey::op(op.get())),
        ),
        Event::Failed { op, failure } if failure_is_news(failure.code) => {
            Some(failed(*op, failure))
        }
        Event::Rejected { op, .. } => Some(
            NotificationDraft::new(
                NotificationSeverity::Warning,
                t!("app.notifications.rejected_title").to_string(),
                t!("app.notifications.rejected_body").to_string(),
            )
            .key(NotificationKey::op(op.get()))
            .action(details_action(*op)),
        ),
        _ => None,
    }
}

/// Whether a failure is the user's business the moment it arrives.
fn failure_is_news(code: FailureCode) -> bool {
    code != FailureCode::FirmwareNotDetected
}

fn read_only(reason: Option<&str>) -> NotificationDraft {
    let body = match reason {
        Some(reason) => reason.to_string(),
        None => t!("app.notifications.read_only_body").to_string(),
    };

    NotificationDraft::new(
        NotificationSeverity::Warning,
        t!("app.notifications.read_only_title").to_string(),
        body,
    )
    .key(NotificationKey::named("read-only"))
}

fn failed(op: OpId, failure: &Failure) -> NotificationDraft {
    NotificationDraft::new(
        failure_severity(failure.code),
        failure_title(failure.code),
        first_line(&failure.message),
    )
    .key(NotificationKey::op(op.get()))
    .action(details_action(op))
}

/// The toast and the bell carry one line; Diagnostics carries the rest.
fn first_line(message: &str) -> String {
    message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(message)
        .trim()
        .to_string()
}

fn failure_severity(code: FailureCode) -> NotificationSeverity {
    match code {
        FailureCode::ImportNeedsConsent => NotificationSeverity::Warning,
        _ => NotificationSeverity::Error,
    }
}

/// The shortest title that names the outcome. The full message stays in the
/// body and in Diagnostics, so the title never grows into a paragraph.
fn failure_title(code: FailureCode) -> String {
    match code {
        FailureCode::DefinitionRejected => t!("app.notifications.failed_definition"),
        FailureCode::ProjectRejected => t!("app.notifications.failed_project"),
        FailureCode::TuneSaveFailed => t!("app.notifications.failed_save"),
        FailureCode::NothingToSave => t!("app.notifications.failed_nothing_to_save"),
        FailureCode::TableUnavailable => t!("app.notifications.failed_table"),
        FailureCode::TableEditRejected => t!("app.notifications.failed_table_edit"),
        FailureCode::DialogUnavailable => t!("app.notifications.failed_dialog"),
        FailureCode::DialogEditRejected => t!("app.notifications.failed_dialog_edit"),
        FailureCode::ControllerActionRefused => t!("app.notifications.failed_command_refused"),
        FailureCode::ControllerActionFailed => t!("app.notifications.failed_command"),
        FailureCode::ConnectFailed => t!("app.notifications.failed_connect"),
        FailureCode::TestFailed => t!("app.notifications.failed_test"),
        FailureCode::NotConnected => t!("app.notifications.failed_not_connected"),
        FailureCode::ReadFailed => t!("app.notifications.failed_read"),
        FailureCode::AdoptRefused => t!("app.notifications.failed_adopt"),
        FailureCode::TraceFailed => t!("app.notifications.failed_trace"),
        FailureCode::RealtimeRejected => t!("app.notifications.failed_realtime"),
        FailureCode::WriteRefused => t!("app.notifications.failed_write_refused"),
        FailureCode::WriteFailed => t!("app.notifications.failed_write"),
        FailureCode::BurnRefused => t!("app.notifications.failed_burn_refused"),
        FailureCode::BurnFailed => t!("app.notifications.failed_burn"),
        FailureCode::ImportRejected => t!("app.notifications.failed_import"),
        FailureCode::ImportNeedsConsent => t!("app.notifications.failed_import_consent"),
        FailureCode::PickerFailed => t!("app.notifications.failed_picker"),
        FailureCode::SnapshotFailed => t!("app.notifications.failed_snapshot"),
        FailureCode::JournalFailed => t!("app.notifications.failed_journal"),
        FailureCode::LogFailed => t!("app.notifications.failed_log"),
        FailureCode::Busy => t!("app.notifications.failed_busy"),
        _ => t!("app.notifications.failed_generic"),
    }
    .to_string()
}

fn details_action(op: OpId) -> NotificationAction {
    NotificationAction::OpenDiagnostics {
        filter: Some(format!("op={op}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::store::NotificationStore;

    #[test]
    fn an_import_consent_uses_a_warning_not_an_error() {
        assert_eq!(
            failure_severity(FailureCode::ImportNeedsConsent),
            NotificationSeverity::Warning
        );
        assert_eq!(
            failure_severity(FailureCode::WriteRefused),
            NotificationSeverity::Error
        );
    }

    #[test]
    fn a_probe_miss_is_a_scan_step_not_a_notification() {
        assert!(!failure_is_news(FailureCode::FirmwareNotDetected));
        assert!(failure_is_news(FailureCode::TestFailed));
        assert!(failure_is_news(FailureCode::ConnectFailed));
    }

    #[test]
    fn every_failure_code_gets_a_title() {
        let titles = [
            failure_title(FailureCode::DefinitionRejected),
            failure_title(FailureCode::WriteFailed),
            failure_title(FailureCode::Busy),
        ];

        for title in titles {
            assert_ne!(title, "");
        }
    }

    #[test]
    fn repeated_read_only_outcomes_share_one_record() {
        let mut store = NotificationStore::default();

        store.push(read_only(Some("the signatures differ")));
        let record = store.push(read_only(None));

        assert_eq!(store.iter().count(), 1);
        assert_eq!(record.count(), 2);
        assert_eq!(record.severity(), NotificationSeverity::Warning);
    }

    #[test]
    fn a_reconnect_is_only_announced_after_a_visible_loss() {
        let event = Event::Reconnected {
            view: connection_summary(),
        };

        assert!(for_event(&event, None).is_none());
        assert!(for_event(&event, Some("the cable was pulled")).is_some());
    }

    fn connection_summary() -> oxtuner_core::ConnectionView {
        oxtuner_core::ConnectionView {
            port: "/dev/tty.usbmodem1".into(),
            custom: false,
            signature: "speeduino-2024".into(),
            version_text: None,
            definition_signature: "speeduino-2024".into(),
            verdict: oxtuner_core::SignatureVerdict::Match,
            read_only: false,
            read_only_reason: None,
            writes_enabled: false,
            behavior: "speeduino".into(),
            framing: "envelope",
            pages: 4,
            realtime_block: None,
        }
    }
}
