use std::path::Path;

use super::{CONFIG_DIR, Project, ProjectError, ProjectKind};

impl Project {
    /// Classifies a path without opening anything.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectError::NotAProject`] when the path is neither a
    /// project folder nor a `.msq` file.
    pub fn detect(path: &Path) -> Result<ProjectKind, ProjectError> {
        if path.is_dir() {
            if path.join(CONFIG_DIR).is_dir() {
                return Ok(ProjectKind::Folder(path.to_owned()));
            }
            return Err(ProjectError::NotAProject {
                path: path.to_owned(),
            });
        }
        let is_msq = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("msq"));
        if is_msq {
            return Ok(ProjectKind::TuneFile(path.to_owned()));
        }
        Err(ProjectError::NotAProject {
            path: path.to_owned(),
        })
    }
}
