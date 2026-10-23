//! The headless core, as the app sees it.
//!
//! `oxtuner-core` owns the work; this module owns the *relationship*: it starts
//! the core once per process, keeps the handle and the thread, and folds
//! incoming events into a small global the views can render.
//!
//! ```text
//! File ▸ Open Project…  ──send──▶  core thread  ──event──▶  CoreService
//! File ▸ Open ECU Definition…                                    │
//! File ▸ Save Tune…                                              │
//!                                                    Output panel ┘
//! ```
//!
//! The event loop runs on GPUI's foreground executor, awaiting the crossfire
//! async receiver: no polling tick, no second runtime, and no `Send` bound on
//! the task (ADR-0005). The `gpui*` → core direction is a plain channel send,
//! so nothing core-side knows a UI exists.

use std::path::Path;

use gpui_kit::{App, Global, PathPromptOptions};
use oxtuner_core::{
    CoreConfig, CoreHandle, CoreParts, CoreThread, DefinitionSummary, Event, EventStream, Failure,
    FailureCode, ProjectSummary, SaveSummary,
};
use rust_i18n::t;

/// What the views show about the core. Small and `Clone`: rendering reads it
/// every frame.
#[derive(Debug, Clone, Default)]
pub(crate) struct CoreStatus {
    /// A definition loaded on its own (no tune file).
    pub(crate) definition: Option<DefinitionSummary>,
    /// The project the core currently owns, when one is open.
    pub(crate) project: Option<ProjectSummary>,
    /// The last successful save, shown until the next open or failure.
    pub(crate) last_save: Option<SaveSummary>,
    /// The last command failure, kept until a command succeeds.
    pub(crate) failure: Option<Failure>,
}

impl CoreStatus {
    /// The definition to name, whether it came from a project or on its own.
    pub(crate) fn definition(&self) -> Option<&DefinitionSummary> {
        self.project
            .as_ref()
            .map(|project| &project.definition)
            .or(self.definition.as_ref())
    }
}

/// The core handle, its thread, and the last status.
///
/// One global rather than three: they are only meaningful together, and a view
/// must never render a summary from a different core than the one running.
pub(crate) struct CoreService {
    /// `None` when the core could not be started: the app still runs, and every
    /// command reports that instead of panicking.
    handle: Option<CoreHandle>,
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

    /// A handle for sending commands, or `None` when the core failed to start.
    ///
    /// The app keeps running without a core (see [`start`]); a command sent
    /// then must report that, not panic.
    pub(crate) fn handle(cx: &App) -> Option<CoreHandle> {
        cx.global::<Self>().handle.clone()
    }
}

/// Starts the core and the task that carries its events to the views.
///
/// The global is installed either way: views read it every frame, and a missing
/// core is a status to render, not a reason to panic. When the core does not
/// start, its handle is `None` and every command reports a failure.
pub fn start(cx: &mut App) {
    let service = match oxtuner_core::start(CoreConfig::default()) {
        Ok(CoreParts {
            handle,
            events,
            thread,
        }) => {
            spawn_event_pump(events, cx);
            CoreService {
                handle: Some(handle),
                thread: Some(thread),
                status: CoreStatus::default(),
            }
        }
        Err(error) => {
            // Without a core the app still runs: the commands report that the
            // core is not running, and nothing claims to be loaded.
            tracing::error!(%error, "the core did not start");
            CoreService {
                handle: None,
                thread: None,
                status: CoreStatus::default(),
            }
        }
    };

    cx.set_global(service);
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
    let (update, remember) = match event {
        Event::DefinitionLoaded { summary, .. } => {
            tracing::info!(signature = %summary.signature, "definition loaded");
            (Update::Loaded(summary), None)
        }
        Event::ProjectOpened { summary, .. } => {
            tracing::info!(
                root = %summary.root.display(),
                signature = %summary.definition.signature,
                read_only = summary.read_only,
                "project opened"
            );
            // Remember the folder only after the core opened it: the recent
            // list must not gain paths that turned out not to be projects.
            let root = summary.root.clone();
            (Update::Opened(summary), Some(root))
        }
        Event::TuneSaved { outcome, .. } => {
            tracing::info!(
                path = %outcome.path.display(),
                cells = outcome.cells,
                "tune saved"
            );
            (Update::Saved(outcome), None)
        }
        Event::DefinitionUnloaded { .. } => (Update::Unloaded, None),
        Event::Failed { failure, .. } => {
            tracing::warn!(code = failure.code.as_str(), "the core reported a failure");
            (Update::Failed(failure), None)
        }
        Event::Rejected { .. } | Event::Stopped => (Update::Nothing, None),
    };

    if !matches!(&update, Update::Nothing) {
        cx.global_mut::<CoreService>().status.apply(update);
        if let Some(root) = remember {
            crate::project::remember(&root, cx);
        }
        // Only the shell shows the core status; the Settings window does not.
        cx.refresh_windows();
    }
}

enum Update {
    Loaded(DefinitionSummary),
    Opened(ProjectSummary),
    Saved(SaveSummary),
    Unloaded,
    Failed(Failure),
    Nothing,
}

impl CoreStatus {
    fn apply(&mut self, update: Update) {
        match update {
            Update::Loaded(summary) => {
                self.definition = Some(summary);
                self.project = None;
                self.last_save = None;
                self.failure = None;
            }
            Update::Opened(project) => {
                self.project = Some(project);
                self.definition = None;
                self.last_save = None;
                self.failure = None;
            }
            Update::Saved(outcome) => {
                self.last_save = Some(outcome);
                self.failure = None;
            }
            Update::Unloaded => *self = Self::default(),
            Update::Failed(failure) => self.failure = Some(failure),
            Update::Nothing => {}
        }
    }
}

/// Asks the core to open a project folder.
///
/// The read and parse happen on the core thread; the reply is
/// [`Event::ProjectOpened`] or [`Event::Failed`]. A refused command (a full
/// queue, or a stopped core) produces no event, so the failure is shown here.
pub(crate) fn open_project(path: &Path, cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::ProjectRejected) else {
        return;
    };
    if let Err(error) = handle.open_project(path) {
        tracing::warn!(%error, path = %path.display(), "the core refused the project");
        report_failure(cx, FailureCode::ProjectRejected, error.to_string());
    }
}

/// Asks the core to write the open project's tune file.
pub(crate) fn save_tune(cx: &mut App) {
    let Some(handle) = handle_or_report(cx, FailureCode::TuneSaveFailed) else {
        return;
    };
    if let Err(error) = handle.save_tune() {
        tracing::warn!(%error, "the core refused the save");
        report_failure(cx, FailureCode::TuneSaveFailed, error.to_string());
    }
}

/// The core handle, or a recorded failure when the core never started.
fn handle_or_report(cx: &mut App, code: FailureCode) -> Option<CoreHandle> {
    match CoreService::handle(cx) {
        Some(handle) => Some(handle),
        None => {
            tracing::error!("the core is not running");
            report_failure(cx, code, "the core is not running".to_owned());
            None
        }
    }
}

/// Shows a failure that never reached the core.
///
/// The global is installed by [`start`] before the first window exists, so this
/// normally has somewhere to put the message; the guard is for the boot order,
/// where a failure can only be logged.
pub(crate) fn report_failure(cx: &mut App, code: FailureCode, message: String) {
    if !cx.has_global::<CoreService>() {
        tracing::error!(code = code.as_str(), %message, "failure before the core was installed");
        return;
    }

    cx.global_mut::<CoreService>().status.failure = Some(Failure { code, message });
    cx.refresh_windows();
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
            report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
            return;
        }
    };

    let Some(handle) = handle_or_report(cx, FailureCode::DefinitionRejected) else {
        return;
    };
    if let Err(error) = handle.load_definition(label, bytes) {
        tracing::warn!(%error, "the core refused the definition");
        report_failure(cx, FailureCode::DefinitionRejected, error.to_string());
    }
}

/// Tells the core to stop and returns the future that resolves once it is gone.
///
/// Every blocking step runs on the background pool: the foreground executor has
/// to keep running the event pump until [`Event::Stopped`], or the core's last
/// sends block on a full events queue and the join would never return. The
/// returned future is awaited by the quit hook (`on_app_quit`), so quitting is
/// deferred until the core's thread is joined — the core is gone before the
/// app's own state starts being torn down.
pub(crate) fn request_shutdown(cx: &mut App) -> impl Future<Output = ()> + use<> {
    let core = {
        let service = cx.global_mut::<CoreService>();
        (service.handle.clone(), service.thread.take())
    };

    let stopped = cx.background_executor().spawn(async move {
        let (handle, thread) = core;
        // Bounded: a shutdown that cannot be queued is reported, never waited
        // on forever.
        if let Some(handle) = handle
            && let Err(error) = handle.shutdown()
        {
            tracing::debug!(%error, "the core was already stopping");
        }
        if let Some(thread) = thread {
            let report = thread.join();
            tracing::debug!(
                handled = report.handled,
                rejected = report.rejected,
                "the core stopped"
            );
        }
    });

    async move {
        stopped.await;
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
            menu: Vec::new(),
            coverage_percent: 97.95,
            errors: 0,
            warnings: 0,
            active_flags: Vec::new(),
        }
    }

    fn project() -> ProjectSummary {
        ProjectSummary {
            root: std::path::PathBuf::from("/projects/roadster"),
            tune_path: std::path::PathBuf::from("/projects/roadster/CurrentTune.msq"),
            definition_path: std::path::PathBuf::from(
                "/projects/roadster/projectCfg/mainController.ini",
            ),
            in_project: true,
            definition: summary(),
            read_only: false,
            read_only_reasons: Vec::new(),
            loaded: 817,
            skipped: 0,
            warnings: Vec::new(),
        }
    }

    fn failure(code: FailureCode) -> Failure {
        Failure {
            code,
            message: "boom".to_owned(),
        }
    }

    #[test]
    fn a_load_replaces_any_previous_failure() {
        let mut status = CoreStatus {
            failure: Some(failure(FailureCode::DefinitionRejected)),
            ..CoreStatus::default()
        };

        status.apply(Update::Loaded(summary()));

        assert!(status.definition.is_some());
        assert_eq!(status.failure, None);
    }

    #[test]
    fn a_project_and_a_definition_are_mutually_exclusive() {
        let mut status = CoreStatus::default();
        status.apply(Update::Loaded(summary()));

        status.apply(Update::Opened(project()));

        assert!(status.project.is_some());
        assert!(
            status.definition.is_none(),
            "the project owns the definition now"
        );
        assert_eq!(status.definition().map(|summary| summary.pages), Some(15));
    }

    #[test]
    fn a_save_is_shown_until_the_next_failure() {
        let mut status = CoreStatus::default();
        status.apply(Update::Opened(project()));

        status.apply(Update::Saved(SaveSummary {
            path: std::path::PathBuf::from("/projects/roadster/CurrentTune.msq"),
            cells: 3,
        }));

        assert_eq!(status.last_save.as_ref().map(|save| save.cells), Some(3));
        assert_eq!(status.failure, None);

        status.apply(Update::Failed(failure(FailureCode::TuneSaveFailed)));

        assert_eq!(
            status.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::TuneSaveFailed)
        );
    }

    #[test]
    fn a_failure_keeps_the_project_that_is_still_open() {
        let mut status = CoreStatus::default();
        status.apply(Update::Opened(project()));

        status.apply(Update::Failed(failure(FailureCode::ProjectRejected)));

        assert!(status.project.is_some(), "the old project is still owned");
        assert_eq!(
            status.failure.as_ref().map(|failure| failure.code),
            Some(FailureCode::ProjectRejected)
        );
    }

    #[test]
    fn unloading_clears_every_field() {
        let mut status = CoreStatus::default();
        status.apply(Update::Opened(project()));
        status.apply(Update::Failed(failure(FailureCode::ProjectRejected)));

        status.apply(Update::Unloaded);

        assert!(status.project.is_none());
        assert!(status.definition.is_none());
        assert!(status.last_save.is_none());
        assert!(status.failure.is_none());
    }
}
