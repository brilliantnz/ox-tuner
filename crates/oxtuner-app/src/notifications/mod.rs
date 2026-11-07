//! The app-owned notification surface.
//!
//! A notification is created by [`policy`] from a typed core outcome, never by
//! a view and never by a log level. The [`store`] keeps the session inbox that
//! the bell draws; a new record also shows a gpui-kit toast so the user does
//! not have to watch the bell, unless the inbox is already open. A toast that
//! hides or is dismissed leaves its record behind, because the bell is the
//! history.
//!
//! Records are session-only. Clearing them never touches Activity, the write
//! journal or the log file.

pub(crate) mod bell;
mod policy;
mod store;
pub(crate) mod toast;

use std::time::Instant;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, Theme, WindowExt as _};
use gpui_kit::{Anchor, App, Global, SharedString, Styled as _, Window, px};

pub(crate) use self::store::{
    NotificationAction, NotificationDraft, NotificationKey, NotificationRecord,
    NotificationSeverity,
};
use self::store::{NotificationStore, age};
use crate::app;
use crate::i18n::t;

/// The session notification inbox.
#[derive(Debug, Default)]
pub(crate) struct Notifications {
    store: NotificationStore,
    /// The notification center is open, so new records belong in the list and
    /// must not raise a toast behind the popover.
    inbox_open: bool,
}

impl Global for Notifications {}

/// Installs the inbox and places gpui-kit toasts above the status bar.
pub(crate) fn init(cx: &mut App) {
    if cx.try_global::<Notifications>().is_none() {
        cx.set_global(Notifications::default());
    }

    Theme::update(cx, |theme| {
        theme.notification.placement = Anchor::BottomRight;
        theme.notification.margins.right = px(8.);
        theme.notification.margins.bottom = px(35.);
    });
}

/// Every record, newest first.
pub(crate) fn records(cx: &App) -> Vec<NotificationRecord> {
    cx.global::<Notifications>().store.iter().cloned().collect()
}

/// How many records the user has not clicked.
pub(crate) fn unread(cx: &App) -> usize {
    cx.global::<Notifications>().store.unread()
}

/// Adds one policy draft to the inbox and shows its toast.
///
/// The toast and the bell row are the same record: a retry that aggregates
/// keeps the record id, so pushing it again replaces the visible toast instead
/// of stacking a second one.
pub(crate) fn publish(draft: NotificationDraft, cx: &mut App) {
    let record = cx.global_mut::<Notifications>().store.push(draft);
    let inbox_open = cx.global::<Notifications>().inbox_open;

    if !inbox_open && let Some(window) = app::window::main(cx) {
        let notification = toast(&record);
        window
            .update(cx, |_, window, cx| {
                window.push_notification(notification, cx);
            })
            .ok();
    }

    app::window::notify_main(cx);
}

/// Runs the policy for one core event and publishes the result, if any.
pub(crate) fn publish_event(
    event: &oxtuner_core::Event,
    link_was_lost: Option<&str>,
    cx: &mut App,
) {
    if let Some(draft) = policy::for_event(event, link_was_lost) {
        publish(draft, cx);
    }
}

/// Marks one record read and runs its action.
///
/// This is the click path for both a bell row and a toast. Only the clicked
/// record changes, so opening the popover cannot clear the badge by accident.
pub(crate) fn activate(id: u64, window: &mut Window, cx: &mut App) {
    let action = {
        let store = &mut cx.global_mut::<Notifications>().store;
        store.mark_read(id);

        store
            .iter()
            .find(|record| record.id() == id)
            .and_then(|record| record.action().cloned())
    };

    app::window::notify_main(cx);

    if let Some(action) = action {
        run_action(action, window, cx);
    }
}

/// Removes one record and its toast.
pub(crate) fn dismiss(id: u64, window: &mut Window, cx: &mut App) {
    let changed = cx.global_mut::<Notifications>().store.dismiss(id);

    if changed {
        window.remove_notification1::<Notifications>(id, cx);
        app::window::notify_main(cx);
    }
}

/// Removes every record and every toast.
pub(crate) fn clear(window: &mut Window, cx: &mut App) {
    let ids: Vec<u64> = cx
        .global::<Notifications>()
        .store
        .iter()
        .map(NotificationRecord::id)
        .collect();

    let changed = cx.global_mut::<Notifications>().store.clear();

    if changed {
        for id in ids {
            window.remove_notification1::<Notifications>(id, cx);
        }

        app::window::notify_main(cx);
    }
}

/// Removes every visible toast but leaves the inbox untouched.
///
/// The notification center calls this when it opens, so a toast underneath
/// cannot keep taking pointer hover through the overlay.
pub(crate) fn hide_toasts(window: &mut Window, cx: &mut App) {
    let ids: Vec<u64> = cx
        .global::<Notifications>()
        .store
        .iter()
        .map(NotificationRecord::id)
        .collect();

    for id in ids {
        window.remove_notification1::<Notifications>(id, cx);
    }
}

/// Tracks whether the notification center is open.
///
/// Opening clears the visible toasts; while it stays open, new records only
/// join the list. A toast behind the popover would otherwise take hover
/// through the overlay.
pub(crate) fn set_inbox_open(open: bool, window: &mut Window, cx: &mut App) {
    cx.global_mut::<Notifications>().inbox_open = open;

    if open {
        hide_toasts(window, cx);
    }
}

/// Runs a record's action.
pub(crate) fn run_action(action: NotificationAction, window: &mut Window, cx: &mut App) {
    match action {
        NotificationAction::OpenDiagnostics { filter } => {
            crate::layout::show_diagnostics(window, cx, filter);
        }
    }
}

/// A relative label for a record's age. The inbox is session-only, so "4 min
/// ago" is more useful than a clock time from a run the user cannot compare
/// with anything.
pub(crate) fn time_label(created: Instant) -> SharedString {
    let elapsed = age(created);
    let seconds = elapsed.as_secs();

    match seconds {
        0..=59 => t!("app.notifications.time_now"),
        60..=3_599 => t!(
            "app.notifications.time_minutes",
            count = (seconds / 60).to_string(),
        ),
        _ => t!(
            "app.notifications.time_hours",
            count = (seconds / 3_600).to_string(),
        ),
    }
}

/// The icon a record's severity uses. A read record is dimmed.
pub(crate) fn severity_icon(record: &NotificationRecord, cx: &App) -> Icon {
    let theme = cx.theme();
    let color = if record.read() {
        theme.muted_foreground
    } else {
        match record.severity() {
            NotificationSeverity::Info => theme.info,
            NotificationSeverity::Success => theme.success,
            NotificationSeverity::Warning => theme.warning,
            NotificationSeverity::Error => theme.danger,
        }
    };
    let icon = match record.severity() {
        NotificationSeverity::Info => IconName::Info,
        NotificationSeverity::Success => IconName::CircleCheck,
        NotificationSeverity::Warning => IconName::TriangleAlert,
        NotificationSeverity::Error => IconName::CircleX,
    };

    Icon::new(icon).with_size(px(18.)).text_color(color)
}

fn toast(record: &NotificationRecord) -> Notification {
    let id = record.id();
    let content_record = record.clone();

    Notification::new()
        .id1::<Notifications>(id)
        .placement(Anchor::BottomRight)
        .on_click(move |_, window, cx| activate(id, window, cx))
        .content(move |_, _window, cx| toast::content(&content_record, cx))
}
