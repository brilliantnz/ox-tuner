use std::path::Path;
use std::sync::Arc;

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_tune::{Tune, TuneImages, TuneLayout};

use crate::check::LoadReport;
use crate::definition_cache::DefinitionSource;

use super::Project;

impl Project {
    /// The project folder, or the tune file's folder in standalone mode.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The definition that was loaded.
    pub fn definition(&self) -> &Definition {
        &self.definition
    }

    /// Whether this open parsed the definition or loaded the cache.
    pub fn definition_source(&self) -> DefinitionSource {
        self.definition_source
    }

    /// Path of the definition file that was loaded.
    pub fn definition_path(&self) -> &Path {
        &self.definition_path
    }

    /// Path of the tune file.
    pub fn tune_path(&self) -> &Path {
        &self.tune_path
    }

    /// Whether this is a project folder (as opposed to standalone).
    pub fn is_in_project(&self) -> bool {
        self.in_folder
    }

    /// The settings context the definition was parsed with.
    pub fn settings(&self) -> &SettingsContext {
        &self.settings
    }

    /// The layout every value is interpreted through.
    pub fn layout(&self) -> &Arc<TuneLayout> {
        self.images.layout()
    }

    /// The four tune images.
    pub fn images(&self) -> &TuneImages {
        &self.images
    }

    /// The images, mutably — edits go through here.
    pub fn images_mut(&mut self) -> &mut TuneImages {
        &mut self.images
    }

    /// The working tune.
    pub fn tune(&self) -> &Tune {
        self.images.working()
    }

    /// The working tune, mutably (an alias for [`Self::images_mut`] use).
    pub fn tune_mut(&mut self) -> &mut Tune {
        self.images.working_mut()
    }

    /// The load gate's findings.
    pub fn report(&self) -> &LoadReport {
        &self.report
    }

    /// Whether the tune must not be saved (S1).
    pub fn is_read_only(&self) -> bool {
        self.report.is_read_only()
    }
}
