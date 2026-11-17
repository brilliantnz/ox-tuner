//! Showing a folder in the platform's file manager.
//!
//! One place holds the program names, so every "show in folder" button opens
//! the same thing. Failures are logged: the user asked for a convenience, and a
//! file manager that refuses to start is not a reason to interrupt them.

use std::path::Path;
use std::process::Command;

/// Shows `path` in the file manager, selecting nothing.
///
/// The child process is waited on in a detached thread, so a launcher that
/// stays alive (some `xdg-open` setups) neither blocks the UI thread nor leaves
/// a zombie behind.
pub(crate) fn show_in_folder(path: &Path) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };

    match Command::new(program).arg(path).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                if let Err(error) = child.wait() {
                    tracing::debug!(%error, "the file manager exited without a status");
                }
            });
        }
        Err(error) => {
            tracing::warn!(
                %error,
                program,
                path = %path.display(),
                "the file manager could not be started"
            );
        }
    }
}
