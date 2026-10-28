//! Application-level actions: commands that are not owned by a single view.
//!
//! They are bound from the table in `shortcuts.rs` and referenced by the menu
//! in `menu.rs`. A view that triggers one (a menu item, a toolbar button)
//! emits the same action, so the keyboard and pointer paths cannot drift apart.

gpui_kit::actions!(
    oxtuner,
    [
        Quit,
        About,
        OpenDefinition,
        OpenProject,
        ManageProjects,
        CloseProject,
        SaveTune,
        OpenHomepage,
        Settings,
        ShowDisclaimer,
        TogglePrimarySidebar,
        ToggleSecondarySidebar,
        ToggleBottomPanel,
        ConnectEcu,
        DisconnectEcu,
        SaveSessionTrace,
        ShowTune,
        ShowCharts,
        WriteToRam,
        BurnToFlash,
        RevertTune,
        EnableWrites,
        ImportTune,
        WriteJournal,
        OpenSnapshots
    ]
);
