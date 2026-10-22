//! Opening the project folder.
//!
//! M1 records the chosen project and remembers it for the next launch. The
//! tune/INI reader that consumes the folder arrives with the later milestones, so
//! this module deliberately stops at selecting and persisting the path — it
//! does not claim to have parsed a tune.

use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui_kit::{App, PathPromptOptions};
use rust_i18n::t;

use crate::{settings::preferences::AppSettings, state::session::AppState};

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

/// Makes `path` the active project and remembers it.
///
/// The state write is immediate rather than debounced: choosing a project is a
/// deliberate action and should survive a crash a moment later.
pub(crate) fn open(path: PathBuf, cx: &mut App) {
    let path = match fs::canonicalize(&path) {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not canonicalize project path");
            path
        }
    };

    AppState::update_global(cx, |state| state.last_project = Some(path.clone()));
    AppSettings::remember_project(cx, &path);
    AppState::save(cx);
    // Only the shell draws the active project; the Settings window does not.
    crate::app::window::notify_main(cx);
}

/// The folder name to show in the sidebar and empty state.
pub(crate) fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
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
