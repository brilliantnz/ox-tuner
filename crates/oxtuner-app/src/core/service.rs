//! The process-wide core handle and the tasks that carry its events.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::{App, Global};
use oxtuner_core::{
    ChartHistory, CoreConfig, CoreHandle, CoreParts, CoreThread, Event, EventStream, FrameWatch,
    LatestFrame, LogStatus, RealtimeFrame, SessionLogSnapshot,
};

use super::event;
use super::status::CoreStatus;
use crate::platform::paths::AppPaths;

#[cfg(test)]
use oxtuner_core::{DialogView, Failure, FailureCode, SaveReport, TableView};

/// The core handle, its thread, and the last status.
///
/// One global rather than three: they are only meaningful together, and a view
/// must never render a summary from a different core than the one running.
pub(crate) struct CoreService {
    /// `None` when the core could not be started: the app still runs, and every
    /// command reports that instead of panicking.
    handle: Option<CoreHandle>,
    thread: Option<CoreThread>,
    /// The renderable snapshot, folded from the event stream.
    pub(super) status: CoreStatus,
    /// The freshest realtime frame (ADR-0014).
    latest: LatestFrame,
    /// The bounded realtime history the chart reads (ADR-0014/0015).
    history: ChartHistory,
    /// The live session log counters.
    log_status: LogStatus,
    /// The tune path waiting for the user's import consent (S9).
    pub(super) pending_import: Option<PathBuf>,
    /// `--sim`: connect to the simulator as soon as a definition is loaded.
    pub(super) auto_sim: bool,
}

impl Global for CoreService {}

impl CoreService {
    /// The status to render. Callers run after [`start`], which installs the
    /// global before the first window is created.
    pub(crate) fn status(cx: &App) -> &CoreStatus {
        &cx.global::<Self>().status
    }

    /// The status for the command and event layers to update.
    pub(crate) fn status_mut(cx: &mut App) -> &mut CoreStatus {
        &mut cx.global_mut::<Self>().status
    }

    /// A handle for sending commands, or `None` when the core failed to start.
    ///
    /// The app keeps running without a core (see [`start`]); a command sent
    /// then must report that, not panic.
    pub(crate) fn handle(cx: &App) -> Option<CoreHandle> {
        cx.global::<Self>().handle.clone()
    }

    /// The newest realtime frame, for the live-values view.
    pub(crate) fn latest(cx: &App) -> Option<Arc<RealtimeFrame>> {
        cx.global::<Self>().latest.get()
    }

    /// The bounded realtime history the chart reads on its render tick.
    pub(crate) fn history(cx: &App) -> ChartHistory {
        cx.global::<Self>().history.clone()
    }

    /// The definition targets whose snapshot reports unsaved cells.
    pub(crate) fn dirty(cx: &App) -> &BTreeSet<String> {
        &cx.global::<Self>().status.dirty
    }

    /// The live session log counters.
    pub(crate) fn log(cx: &App) -> SessionLogSnapshot {
        cx.global::<Self>().log_status.snapshot()
    }

    /// Takes the handle and thread out for shutdown.
    ///
    /// The core is stopped once per process, so the thread is consumed rather
    /// than cloned. The handle stays cloneable because commands may still be
    /// in flight while the quit hook runs.
    pub(super) fn take_core(&mut self) -> (Option<CoreHandle>, Option<CoreThread>) {
        (self.handle.clone(), self.thread.take())
    }
}

#[cfg(test)]
impl CoreService {
    /// Marks `target` as holding unsaved cells, for view tests.
    pub(crate) fn mark_dirty(cx: &mut App, target: &str) {
        cx.global_mut::<Self>()
            .status
            .dirty
            .insert(target.to_owned());
    }

    /// Installs a table snapshot, for editor tests.
    pub(crate) fn show_table(cx: &mut App, table: TableView) {
        cx.global_mut::<Self>().status.table = Some(table);
    }

    /// Installs a dialog snapshot, for editor tests.
    pub(crate) fn show_dialog(cx: &mut App, dialog: DialogView) {
        cx.global_mut::<Self>().status.dialog = Some(dialog);
    }

    /// Moves to another definition and drops the open snapshot, the way a
    /// project switch does, for editor tests.
    pub(crate) fn next_definition(cx: &mut App) {
        let status = &mut cx.global_mut::<Self>().status;
        status.definition_revision = status.definition_revision.wrapping_add(1);
        status.table = None;
        status.dialog = None;
    }

    /// Records a landed save, for view tests.
    pub(crate) fn save_for_test(cx: &mut App) {
        let status = &mut cx.global_mut::<Self>().status;
        status.last_save = Some(SaveReport {
            path: "/projects/test/CurrentTune.msq".into(),
            cells: 1,
        });
        status.tune_revision = status.tune_revision.wrapping_add(1);
    }

    /// Records a failed command, for view tests.
    pub(crate) fn fail_for_test(cx: &mut App, code: FailureCode) {
        let status = &mut cx.global_mut::<Self>().status;
        status.failure_serial = status.failure_serial.wrapping_add(1);
        status.failure = Some(Failure {
            code,
            message: "the test command failed".to_owned(),
        });
    }

    /// Installs a service with no core behind it, for view tests.
    ///
    /// A view reads the status and the newest frame on every render, so a test
    /// that draws a window needs the global even when it never talks to a core.
    /// Without it the first frame would panic instead of showing empty state.
    pub(crate) fn install_without_core(cx: &mut App) {
        cx.set_global(Self {
            handle: None,
            thread: None,
            status: CoreStatus::default(),
            latest: LatestFrame::default(),
            history: ChartHistory::default(),
            log_status: LogStatus::new(),
            pending_import: None,
            auto_sim: false,
        });
    }

    /// Installs a service with a live core and no event pumps, for logic tests.
    ///
    /// A test that drives a command through the real sender needs the handle
    /// and nothing else. The event stream is dropped, so whatever the core
    /// answers is discarded; the test calls the handlers directly instead. The
    /// pumps [`start`] spawns cannot run here: the core thread wakes a
    /// foreground task from a thread the headless test scheduler does not own,
    /// and it refuses to continue. The core runs on the default config, with no
    /// journal or definition cache and writes off. The caller keeps the
    /// returned thread, or drops it, which shuts the core down.
    pub(crate) fn install_with_core(cx: &mut App) -> CoreThread {
        let parts = oxtuner_core::start(&CoreConfig::default()).expect("the core starts");

        cx.set_global(Self {
            handle: Some(parts.handle),
            thread: None,
            status: CoreStatus::default(),
            latest: parts.latest,
            history: parts.history,
            log_status: parts.log,
            pending_import: None,
            auto_sim: false,
        });

        parts.thread
    }
}

/// Starts the core and the task that carries its events to the views.
///
/// The global is installed either way: views read it every frame, and a missing
/// core is a status to render, not a reason to panic. When the core does not
/// start, its handle is `None` and every command reports a failure.
pub fn start(cx: &mut App, auto_sim: bool) {
    let writes_enabled = crate::settings::preferences::AppSettings::writes_enabled(cx);
    let (journal_dir, definition_cache_dir) = {
        let paths = AppPaths::get(cx);
        (
            paths.config_dir.join("write-journal.log"),
            paths.definition_cache_dir(),
        )
    };
    let config = CoreConfig {
        writes_enabled,
        journal_dir: Some(journal_dir),
        definition_cache_dir: Some(definition_cache_dir),
        ..CoreConfig::default()
    };
    let (service, pump) = match oxtuner_core::start(&config) {
        Ok(CoreParts {
            handle,
            events,
            latest,
            frames,
            history,
            log,
            thread,
        }) => (
            CoreService {
                handle: Some(handle),
                thread: Some(thread),
                status: CoreStatus {
                    writes_enabled,
                    ..CoreStatus::default()
                },
                latest,
                history,
                log_status: log,
                pending_import: None,
                auto_sim,
            },
            Some((events, frames)),
        ),
        Err(error) => {
            // Without a core the app still runs: the commands report that the
            // core is not running, and nothing claims to be loaded.
            tracing::error!(%error, "the core did not start");
            (
                CoreService {
                    handle: None,
                    thread: None,
                    status: CoreStatus::default(),
                    latest: LatestFrame::default(),
                    history: ChartHistory::default(),
                    log_status: LogStatus::new(),
                    pending_import: None,
                    auto_sim,
                },
                None,
            )
        }
    };

    // Install the global before the pump can deliver its first event: the
    // pump's fold reads the status through it.
    cx.set_global(service);

    if let Some((events, frames)) = pump {
        spawn_event_pump(events, frames, cx);
    }
}

fn spawn_event_pump(events: EventStream, frames: FrameWatch, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Some(event) = events.recv().await {
            let stopped = matches!(event, Event::Stopped);
            cx.update(|cx| event::apply(event, cx));
            if stopped {
                break;
            }
        }
        tracing::debug!("the core event pump finished");
    })
    .detach();

    // Realtime wakes are separate from the event queue and coalesced, so a
    // frame can never queue behind a command answer or block the core
    // (ADR-0021). Repainting reads LatestFrame and the chart history.
    cx.spawn(async move |cx| {
        while frames.changed().await {
            cx.update(gpui_kit::App::refresh_windows);
        }
        tracing::debug!("the realtime wake watch finished");
    })
    .detach();
}
