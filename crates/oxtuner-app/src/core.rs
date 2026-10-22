//! The headless core, as the app sees it.
//!
//! `oxtuner-core` owns the work; this module owns the *relationship*: it starts
//! the core once per process, keeps the handle and the thread, and folds
//! incoming events into a small global the views can render.
//!
//! ```text
//! File ▸ Open ECU Definition…  ──send──▶  core thread  ──event──▶  CoreService
//!                                                                       │
//!                                                       Output panel ───┘
//! ```
//!
//! The event loop runs on GPUI's foreground executor, awaiting the crossfire
//! async receiver: no polling tick, no second runtime, and no `Send` bound on
//! the task (ADR-0005). The `gpui*` → core direction is a plain channel send,
//! so nothing core-side knows a UI exists.

use gpui_kit::{App, Global, PathPromptOptions};
use oxtuner_core::{
    CoreConfig, CoreHandle, CoreParts, CoreThread, DefinitionSummary, Event, EventStream,
};
use rust_i18n::t;

/// What the views show about the core. Small and `Clone`: rendering reads it
/// every frame.
#[derive(Debug, Clone, Default)]
pub(crate) struct CoreStatus {
    /// The definition the core currently owns.
    pub(crate) summary: Option<DefinitionSummary>,
    /// The last load failure, kept until a load succeeds.
    pub(crate) failure: Option<String>,
}

/// The core handle, its thread, and the last status.
///
/// One global rather than three: they are only meaningful together, and a view
/// must never render a summary from a different core than the one running.
pub(crate) struct CoreService {
    handle: CoreHandle,
    thread: Option<CoreThread>,
    status: CoreStatus,
}

impl Global for CoreService {}

impl CoreService {
    /// The status to render. Callers run after [`start`], which installs the
    /// global before the first window is created.
    pub(crate) fn status(cx: &App) -> &CoreStatus {
        &cx.global::<Self>().status
    }

    /// A handle for sending commands.
    pub(crate) fn handle(cx: &App) -> CoreHandle {
        cx.global::<Self>().handle.clone()
    }
}

/// Starts the core and the task that carries its events to the views.
pub fn start(cx: &mut App) {
    let parts = match oxtuner_core::start(CoreConfig::default()) {
        Ok(parts) => parts,
        Err(error) => {
            // Without a core the app still runs: the definition commands
            // refuse, and the output panel says nothing was loaded.
            tracing::error!(%error, "the core did not start");
            return;
        }
    };

    let CoreParts {
        handle,
        events,
        thread,
    } = parts;

    cx.set_global(CoreService {
        handle,
        thread: Some(thread),
        status: CoreStatus::default(),
    });

    spawn_event_pump(events, cx);
}

fn spawn_event_pump(events: EventStream, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Some(event) = events.recv().await {
            let stopped = matches!(event, Event::Stopped);
            cx.update(|cx| apply(event, cx));
            if stopped {
                break;
            }
        }
        tracing::debug!("the core event pump finished");
    })
    .detach();
}

/// Folds one event into the status and repaints.
fn apply(event: Event, cx: &mut App) {
    let (changed, failure) = match event {
        Event::DefinitionLoaded { summary, .. } => {
            tracing::info!(signature = %summary.signature, "definition loaded");
            (true, Update::Loaded(summary))
        }
        Event::DefinitionUnloaded { .. } => (true, Update::Unloaded),
        Event::Failed { failure, .. } => {
            tracing::warn!(code = failure.code.as_str(), "definition load failed");
            (true, Update::Failed(failure.message))
        }
        Event::Rejected { .. } | Event::Stopped => (false, Update::Nothing),
    };

    if changed {
        cx.global_mut::<CoreService>().status.apply(failure);
        // Only the shell shows the core status; the Settings window does not.
        cx.refresh_windows();
    }
}

enum Update {
    Loaded(DefinitionSummary),
    Unloaded,
    Failed(String),
    Nothing,
}

impl CoreStatus {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Loaded(summary) => {
                self.summary = Some(summary);
                self.failure = None;
            }
            Update::Unloaded => {
                self.summary = None;
                self.failure = None;
            }
            Update::Failed(message) => self.failure = Some(message),
            Update::Nothing => {}
        }
    }
}

/// Asks the OS for an ECU definition file, reads it, and hands it to the core.
pub(crate) fn choose_definition(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("app.menu.open_definition").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| load(path, cx));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the definition picker failed"),
        Err(error) => tracing::warn!(%error, "the definition picker channel closed"),
    })
    .detach();
}

/// Reads `path` and loads it in the core.
///
/// The read happens here, on the UI thread, because it is user-initiated and one
/// shot; the parsing it feeds is already off this thread.
fn load(path: std::path::PathBuf, cx: &mut App) {
    let label = path
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned);

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not read the definition");
            cx.global_mut::<CoreService>()
                .status
                .apply(Update::Failed(error.to_string()));
            cx.refresh_windows();
            return;
        }
    };

    if let Err(error) = CoreService::handle(cx).load_definition(label, bytes) {
        tracing::warn!(%error, "the core refused the definition");
        cx.global_mut::<CoreService>()
            .status
            .apply(Update::Failed(error.to_string()));
        cx.refresh_windows();
    }
}

/// Tells the core to stop and waits for its thread.
///
/// Joining here rather than relying on drop order means the core is gone before
/// the app's own state starts being torn down, and the wait is bounded by the
/// core's own shutdown path.
pub(crate) fn request_shutdown(cx: &mut App) {
    if !cx.has_global::<CoreService>() {
        return;
    }
    let service = cx.global_mut::<CoreService>();
    if let Err(error) = service.handle.shutdown() {
        tracing::debug!(%error, "the core was already stopping");
    }
    if let Some(thread) = service.thread.take() {
        let report = thread.join();
        tracing::debug!(
            handled = report.handled,
            rejected = report.rejected,
            "the core stopped"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> DefinitionSummary {
        DefinitionSummary {
            label: "speeduino.ini".to_owned(),
            signature: "speeduino 202501".to_owned(),
            spec_version: Some(3.78),
            pages: 15,
            constants: 817,
            pc_variables: 50,
            channels: 229,
            tables: 21,
            curves: 34,
            gauges: 90,
            dialogs: 239,
            coverage_percent: 97.95,
            errors: 0,
            warnings: 0,
            active_flags: Vec::new(),
        }
    }

    #[test]
    fn a_load_replaces_any_previous_failure() {
        let mut status = CoreStatus {
            summary: None,
            failure: Some("previous".to_owned()),
        };

        status.apply(Update::Loaded(summary()));

        assert!(status.summary.is_some());
        assert_eq!(status.failure, None);
    }

    #[test]
    fn a_failure_keeps_the_definition_that_is_still_loaded() {
        let mut status = CoreStatus::default();
        status.apply(Update::Loaded(summary()));

        status.apply(Update::Failed("parse error".to_owned()));

        assert!(
            status.summary.is_some(),
            "the old definition is still owned"
        );
        assert_eq!(status.failure.as_deref(), Some("parse error"));
    }

    #[test]
    fn unloading_clears_both_fields() {
        let mut status = CoreStatus::default();
        status.apply(Update::Loaded(summary()));
        status.apply(Update::Failed("parse error".to_owned()));

        status.apply(Update::Unloaded);

        assert!(status.summary.is_none());
        assert!(status.failure.is_none());
    }
}
