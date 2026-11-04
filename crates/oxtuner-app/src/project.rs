//! Opening the project folder.
//!
//! The picker and the recent list send the path to the core, which reads the
//! definition and the tune and owns them (ADR-0007). This module owns the path
//! semantics — canonicalising, naming, remembering — and persists the folder
//! only once the core reports it opened successfully.

use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui_kit::{App, PathPromptOptions};
use rust_i18n::t;

use crate::{core, settings::preferences::AppSettings, state::session::AppState};

/// Asks the OS for a project folder, then opens it.
///
/// The picker is asynchronous; the app-level action returns immediately and the
/// result is applied when the user chooses or cancels.
pub(crate) fn choose(cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some(t!("app.menu.open_project_folder").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| open(path, cx));
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the project folder picker failed"),
        Err(error) => tracing::warn!(%error, "the project folder picker channel closed"),
    })
    .detach();
}

/// Asks the core to open `path`.
///
/// The reply arrives on the event pump: the core module persists the folder and
/// repaints once the core reports success, so a failed open never enters the
/// recent list.
pub(crate) fn open(path: PathBuf, cx: &mut App) {
    let path = match fs::canonicalize(&path) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not canonicalize project path");
            path
        }
    };

    core::open_project(&path, cx);
}

/// Reopens the project the last session left, when the user asked for it.
///
/// Called once at start-up, after the core is running. Gated on the Settings
/// `auto_reopen` switch (off by default): a tune is opened deliberately, never
/// as a side effect of launching. A folder that has since moved is left alone
/// rather than reported as a start-up failure.
pub fn reopen_last(cx: &mut App) {
    if !AppSettings::auto_reopen(cx) {
        return;
    }
    let path = cx
        .global::<AppState>()
        .last_project()
        .map(Path::to_path_buf);

    if let Some(path) = path
        && is_available(&path)
    {
        tracing::debug!(path = %path.display(), "reopening the last project");
        open(path, cx);
    }
}

/// Makes `path` the active project and remembers it.
///
/// Called by the core event pump after a successful open. The state write is
/// immediate rather than debounced: choosing a project is a deliberate action
/// and should survive a crash a moment later.
pub(crate) fn remember(path: &Path, cx: &mut App) {
    AppState::update_global(cx, |state| state.last_project = Some(path.to_path_buf()));
    AppSettings::remember_project(cx, path);
    AppState::save(cx);
    // Only the main layout draws the active project; the Settings window does not.
    crate::app::window::notify_main(cx);
}

/// The folder name to show in the sidebar and empty state.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned)
}

/// Whether the remembered project still exists on disk.
pub(crate) fn is_available(path: &Path) -> bool {
    path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_uses_the_last_component() {
        assert_eq!(display_name(Path::new("/projects/roadster")), "roadster");
        assert_eq!(display_name(Path::new("/")), "/");
    }
}
