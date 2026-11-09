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
//!
//! The module is split by concern: `status` is the renderable snapshot and its
//! fold, `service` owns the process-wide handle, `event` is the boundary from
//! core events to status updates and follow-up commands, and `commands` holds
//! the senders grouped by the part of the session they act on.

mod commands;
mod event;
mod service;
mod status;

pub(crate) use commands::{
    acknowledge_write_failure, adopt_ecu_tune, burn_to_flash, choose_definition, choose_import,
    close_project, connect, disconnect, edit_axis, edit_scalar, edit_table, import_tune,
    list_snapshots, open_project, read_dialog, read_journal, read_table, refresh_ports,
    refresh_project_availability, request_connect, request_shutdown, restore_snapshot,
    revert_target, revert_tune, run_controller_command, save_trace, save_tune, set_writes_enabled,
    start_log, stop_log, test_connection, write_to_ram,
};
pub(crate) use service::CoreService;
pub use service::start;
#[cfg(test)]
pub(crate) use status::PendingProject;
pub(crate) use status::TestResult;
