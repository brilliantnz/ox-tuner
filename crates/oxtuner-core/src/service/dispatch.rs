#![allow(
    clippy::wildcard_imports,
    reason = "the service modules are one implementation split across files"
)]

use super::ecu::*;
use super::log::*;
use super::transfer::*;
use super::tune::*;
use super::*;

#[expect(
    clippy::too_many_arguments,
    reason = "the dispatcher mirrors the command queue fields"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive match over every UI command reads as a single dispatcher"
)]
pub(super) fn handle(
    envelope: Envelope,
    session: &mut Session,
    live: &mut Option<Live>,
    safety: &mut Safety,
    journal_dir: Option<&std::path::Path>,
    definition_cache_dir: Option<&std::path::Path>,
    events: &EventTx,
    latest: &LatestFrame,
    history: &ChartHistory,
    log: &LogStatus,
    report: &mut CoreReport,
    last_menu: &mut Vec<MenuNode>,
    menu_dirty: &mut bool,
) -> Outcome {
    let Envelope { op, command } = envelope;

    match command {
        CommandRequest::Shutdown => {
            tracing::debug!(%op, "shutdown requested");
            Outcome::Stop
        }
        CommandRequest::ListPorts => {
            match io_thread::list_ports() {
                Ok(ports) => {
                    let summary: Vec<PortView> = ports
                        .into_iter()
                        .map(|port| PortView {
                            label: port.label().to_owned(),
                            name: port.name,
                        })
                        .collect();
                    tracing::debug!(%op, ports = summary.len(), "serial ports listed");
                    send(events, Event::PortsListed { op, ports: summary });
                }
                Err(error) => {
                    tracing::warn!(%op, %error, "serial ports could not be listed");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::ConnectFailed,
                                message: error.to_string(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        CommandRequest::Connect { port, framing } => {
            if let Some(definition) = session.definition() {
                configure_history(history, definition);
            }
            connect(op, session, live, events, report, &port, framing, false)
        }
        CommandRequest::TestConnection { port, framing } => {
            connect(op, session, live, events, report, &port, framing, true)
        }
        CommandRequest::Disconnect => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no connection to close".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if connection.txns.write_tx.is_some()
                || connection.pending_kind(PendingKind::Write)
                || connection.pending_kind(PendingKind::Command)
            {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::Busy,
                            message: "a write, burn or controller command is still running; \
                                      closing now resets the board and could drop it"
                                .to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if let Err(error) = connection.request(op, PendingKind::Disconnect, IoOp::Close) {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: error.to_string(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        CommandRequest::ReadAll => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no connection to read from".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if !connection.connection.connected {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "the link is lost; waiting for it to come back".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if connection.pending_kind(PendingKind::ReadAll) {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::ReadFailed,
                            message: "a page read is already running".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if connection.txns.write_tx.is_some()
                || connection.pending_kind(PendingKind::Write)
                || connection.pending_kind(PendingKind::Command)
            {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::ReadFailed,
                            message: "a write, burn or controller command is running; read the \
                                      pages after it finishes"
                                .to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if let Err(error) = connection.request(op, PendingKind::ReadAll, IoOp::ReadAll) {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::ReadFailed,
                            message: error.to_string(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        CommandRequest::AdoptEcuImage => {
            adopt_ecu_image(op, session, events, report);
            *menu_dirty = true;
            Outcome::Continue
        }
        CommandRequest::StartRealtime { interval_ms } => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::RealtimeRejected,
                            message: "there is no connection to poll".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if !connection.connection.connected {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::RealtimeRejected,
                            message: "the link is lost; waiting for it to come back".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if connection.connection.realtime_block.is_none() {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::RealtimeRejected,
                            message: "the definition declares no realtime block".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            connection.connection.auto_poll = true;
            connection.start_polling(interval_ms);
            send(events, Event::RealtimeState { op, running: true });
            *menu_dirty = true;
            Outcome::Continue
        }
        CommandRequest::StopRealtime => {
            let running = live.as_mut().is_some_and(|connection| {
                connection.connection.auto_poll = false;
                connection.stop_polling();
                true
            });
            if running {
                send(events, Event::RealtimeState { op, running: false });
                *menu_dirty = true;
            } else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no connection to stop".to_owned(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        CommandRequest::SetWritesEnabled { enabled } => {
            safety.set_writes_enabled(enabled);
            let connection = live
                .as_ref()
                .map(|connection| connection.summary(safety.writes_enabled()));
            tracing::info!(%op, enabled, "global write switch changed");
            send(
                events,
                Event::WriteAccessChanged {
                    op,
                    writes_enabled: enabled,
                    connection,
                },
            );
            Outcome::Continue
        }
        CommandRequest::AcknowledgeWriteFailure => {
            let was_halted = safety.halted().is_some();
            safety.clear_halt();
            tracing::info!(%op, was_halted, "write failure acknowledged");
            send(events, Event::WriteFailureAcknowledged { op });
            Outcome::Continue
        }
        CommandRequest::RefreshTuneStatus => {
            match session.tune_status() {
                Some(status) => {
                    send(events, Event::TuneStatusRead { op, status });
                }
                None => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NothingToSave,
                            message: "no tune is open".to_owned(),
                        },
                    },
                ),
            }
            Outcome::Continue
        }
        CommandRequest::ReadJournal => {
            let Some(journal) = session.journal(journal_dir) else {
                send(
                    events,
                    Event::JournalRead {
                        op,
                        path: None,
                        entries: Vec::new(),
                    },
                );
                return Outcome::Continue;
            };
            match journal.read(journal::JOURNAL_READ_LIMIT) {
                Ok(entries) => send(
                    events,
                    Event::JournalRead {
                        op,
                        path: Some(journal.path().to_owned()),
                        entries,
                    },
                ),
                Err(error) => {
                    tracing::warn!(%op, %error, "the write journal could not be read");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::JournalFailed,
                                message: error.to_string(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        CommandRequest::WriteToRam { pages } => write_to_ram(
            op,
            session,
            live,
            safety,
            journal_dir,
            events,
            report,
            pages,
        ),
        CommandRequest::BurnToFlash { pages } => burn_to_flash(
            op,
            session,
            live,
            safety,
            journal_dir,
            events,
            report,
            pages,
        ),
        CommandRequest::RevertTune => {
            let outcome = revert_tune(op, session, journal_dir, events, report);
            *menu_dirty = true;
            outcome
        }
        CommandRequest::RevertTarget { name } => {
            let outcome = revert_target(op, session, journal_dir, events, report, latest, &name);
            *menu_dirty = true;
            outcome
        }
        CommandRequest::ImportTune { path, consented } => {
            let outcome = import_tune(
                op,
                session,
                journal_dir,
                events,
                report,
                &path,
                consented,
                "import",
            );
            *menu_dirty = true;
            outcome
        }
        CommandRequest::RestoreSnapshot { path } => {
            let outcome = import_tune(
                op,
                session,
                journal_dir,
                events,
                report,
                &path,
                true,
                "restore",
            );
            *menu_dirty = true;
            outcome
        }
        CommandRequest::ListSnapshots => {
            let Some(project) = session.project() else {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::SnapshotFailed,
                    "no project is open",
                );
            };
            let paths = snapshot::list(project.root());
            send(events, Event::SnapshotsListed { op, paths });
            Outcome::Continue
        }
        CommandRequest::StartLog { directory } => {
            start_log(op, live, events, report, &directory, log)
        }
        CommandRequest::StopLog => stop_log(op, live, events, report),
        CommandRequest::SaveTrace { path } => {
            let Some(connection) = live.as_mut() else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::NotConnected,
                            message: "there is no session trace to save".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };
            if !connection.connection.connected {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TraceFailed,
                            message: "there is no session to save while the link is lost"
                                .to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            }
            if let Err(error) = connection.request(op, PendingKind::Trace, IoOp::SaveTrace { path })
            {
                report.failures += 1;
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TraceFailed,
                            message: error.to_string(),
                        },
                    },
                );
            }
            Outcome::Continue
        }
        CommandRequest::LoadDefinition {
            label,
            bytes,
            settings,
        } => {
            match oxtuner_ini::parse_bytes(&bytes, &settings) {
                Ok(parsed) => {
                    let summary = DefinitionView::new(label, &parsed);
                    tracing::info!(
                        %op,
                        label = %summary.label,
                        signature = %summary.signature,
                        constants = summary.constants,
                        coverage = summary.coverage_percent,
                        "definition loaded"
                    );
                    // The core is the only owner: the previous session is
                    // dropped here, never handed back to a caller. A live
                    // connection belongs to the definition it was built from,
                    // so it is torn down first.
                    teardown(
                        live,
                        session,
                        safety,
                        journal_dir,
                        latest,
                        events,
                        history,
                        log,
                    );
                    configure_history(history, &parsed);
                    *session = Session::Definition(Box::new(parsed));
                    last_menu.clone_from(&summary.menu);
                    report.loads += 1;
                    send(events, Event::DefinitionLoaded { op, view: summary });
                }
                Err(error) => {
                    tracing::warn!(%op, %label, %error, "definition rejected");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::DefinitionRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        CommandRequest::OpenProject { path } => {
            // Reading and parsing happen here, on the core thread: the UI must
            // never wait on a project's INI and `.msq`.
            let _span =
                tracing::debug_span!("core_open_project", %op, path = %path.display()).entered();
            match Project::open_with_cache(&path, definition_cache_dir) {
                Ok(project) => {
                    let cache_hit = project.definition_source() == DefinitionSource::Cache;
                    // The old session's frame belongs to another definition and
                    // must be gone before the new menu is evaluated.
                    teardown(
                        live,
                        session,
                        safety,
                        journal_dir,
                        latest,
                        events,
                        history,
                        log,
                    );
                    let summary = ProjectView::from_project(&project, latest.get().as_deref());
                    tracing::info!(
                        %op,
                        root = %summary.root.display(),
                        signature = %summary.definition.signature,
                        loaded = summary.loaded,
                        skipped = summary.skipped,
                        warnings = summary.warnings.len(),
                        read_only = summary.read_only,
                        cache_hit,
                        "project opened"
                    );
                    configure_history(history, project.definition());
                    *session = Session::Project(Box::new(project));
                    last_menu.clone_from(&summary.definition.menu);
                    report.projects_opened += 1;
                    send(events, Event::ProjectOpened { op, view: summary });
                }
                Err(error) => {
                    tracing::warn!(%op, path = %path.display(), %error, "project rejected");
                    report.failures += 1;
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::ProjectRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        CommandRequest::SaveTune => {
            match session {
                Session::Project(project) => match project.save() {
                    Ok(outcome) => {
                        tracing::info!(
                            %op,
                            path = %outcome.path.display(),
                            cells = outcome.cells,
                            "tune saved"
                        );
                        report.tune_saves += 1;
                        send(
                            events,
                            Event::TuneSaved {
                                op,
                                outcome: SaveReport {
                                    path: outcome.path,
                                    cells: outcome.cells,
                                },
                            },
                        );
                    }
                    Err(error) => {
                        tracing::warn!(%op, %error, "tune save failed");
                        report.failures += 1;
                        send(
                            events,
                            Event::Failed {
                                op,
                                failure: Failure {
                                    code: FailureCode::TuneSaveFailed,
                                    message: error.to_string(),
                                },
                            },
                        );
                    }
                },
                Session::Definition(_) | Session::Empty => {
                    tracing::debug!(%op, "nothing to save");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::NothingToSave,
                                message: "no tune is open; open a project folder first".to_owned(),
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        CommandRequest::ReadTable { name } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableUnavailable,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            match TableView::from_project(project, &name) {
                Ok(table) => {
                    tracing::debug!(
                        %op,
                        table = %table.id,
                        rows = table.rows,
                        columns = table.cols,
                        "table read"
                    );
                    send(events, Event::TableRead { op, table });
                }
                Err(message) => {
                    tracing::debug!(%op, table = %name, %message, "table unavailable");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableUnavailable,
                                message,
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        CommandRequest::EditTable { name, mut edits } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            if let Err(message) = validate_edits(project, &name, &edits) {
                tracing::debug!(%op, table = %name, %message, "table edit rejected");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                );
                return Outcome::Continue;
            }

            for edit in &mut edits {
                if let Some(spec) = project.layout().variable(&name) {
                    let (clamped, _) = clamp_user(spec, edit.value);
                    edit.value = clamped;
                }
                // Validation above covers every error `set_cell_user` can
                // report, so a failure here would be a bug; report it rather
                // than swallow it, and keep the snapshot truthful.
                if let Err(error) =
                    project
                        .tune_mut()
                        .set_cell_user(&name, edit.row, edit.column, edit.value)
                {
                    tracing::error!(%op, table = %name, %error, "a validated edit failed");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableEditRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                    return Outcome::Continue;
                }
            }

            match TableView::from_project(project, &name) {
                Ok(table) => {
                    tracing::debug!(
                        %op,
                        table = %table.id,
                        cells = edits.len(),
                        "table edited"
                    );
                    send(events, Event::TableEdited { op, table });
                }
                Err(message) => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                ),
            }
            *menu_dirty = true;
            Outcome::Continue
        }
        CommandRequest::EditAxis {
            table,
            axis,
            mut edits,
        } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            let variable = match TableView::axis_variable(project, &table, axis) {
                Ok(variable) => variable,
                Err(message) => {
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableEditRejected,
                                message,
                            },
                        },
                    );
                    return Outcome::Continue;
                }
            };

            if let Err(message) = validate_axis_edits(project, &variable, &edits) {
                tracing::debug!(%op, table = %table, axis = axis.as_str(), %message, "axis edit rejected");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                );
                return Outcome::Continue;
            }

            for edit in &mut edits {
                if let Some(spec) = project.layout().variable(&variable) {
                    let (clamped, _) = clamp_user(spec, edit.value);
                    edit.value = clamped;
                }
                if let Err(error) = project
                    .tune_mut()
                    .set_user_at(&variable, edit.index, edit.value)
                {
                    tracing::error!(%op, table = %table, %error, "a validated axis edit failed");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::TableEditRejected,
                                message: error.to_string(),
                            },
                        },
                    );
                    return Outcome::Continue;
                }
            }

            match TableView::from_project(project, &table) {
                Ok(table) => {
                    tracing::debug!(
                        %op,
                        table = %table.id,
                        axis = axis.as_str(),
                        bins = edits.len(),
                        "axis edited"
                    );
                    send(events, Event::TableEdited { op, table });
                }
                Err(message) => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::TableEditRejected,
                            message,
                        },
                    },
                ),
            }
            *menu_dirty = true;
            Outcome::Continue
        }
        CommandRequest::ReadDialog { name } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogUnavailable,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            let frame = latest.get();
            match DialogView::from_project(project, &name, frame.as_deref()) {
                Ok(dialog) => {
                    tracing::debug!(
                        %op,
                        dialog = %dialog.id,
                        fields = dialog.fields.len(),
                        "dialog read"
                    );
                    send(events, Event::DialogRead { op, dialog });
                }
                Err(message) => {
                    tracing::debug!(%op, table = %name, %message, "dialog unavailable");
                    send(
                        events,
                        Event::Failed {
                            op,
                            failure: Failure {
                                code: FailureCode::DialogUnavailable,
                                message,
                            },
                        },
                    );
                }
            }
            Outcome::Continue
        }
        CommandRequest::EditScalar {
            dialog,
            name,
            mut value,
        } => {
            let Session::Project(project) = session else {
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message: "no tune is open; open a project folder first".to_owned(),
                        },
                    },
                );
                return Outcome::Continue;
            };

            if let Err(message) = validate_scalar_edit(project, &name, value) {
                tracing::debug!(%op, variable = %name, %message, "scalar edit rejected");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message,
                        },
                    },
                );
                return Outcome::Continue;
            }

            if let Some(spec) = project.layout().variable(&name) {
                let (clamped, _) = clamp_user(spec, value);
                value = clamped;
            }

            if let Err(error) = project.tune_mut().set_user_at(&name, 0, value) {
                tracing::error!(%op, variable = %name, %error, "a validated scalar edit failed");
                send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message: error.to_string(),
                        },
                    },
                );
                return Outcome::Continue;
            }

            let frame = latest.get();
            match DialogView::from_project(project, &dialog, frame.as_deref()) {
                Ok(dialog) => {
                    tracing::debug!(
                        %op,
                        dialog = %dialog.id,
                        variable = %name,
                        "scalar edited"
                    );
                    send(events, Event::DialogEdited { op, dialog });
                }
                Err(message) => send(
                    events,
                    Event::Failed {
                        op,
                        failure: Failure {
                            code: FailureCode::DialogEditRejected,
                            message,
                        },
                    },
                ),
            }
            *menu_dirty = true;
            Outcome::Continue
        }
        CommandRequest::RunControllerCommand { dialog, command } => {
            let outcome = run_controller_command(
                op,
                session,
                live,
                safety,
                journal_dir,
                events,
                report,
                &dialog,
                &command,
            );
            *menu_dirty = true;
            outcome
        }
        CommandRequest::UnloadDefinition => {
            let had_session = !matches!(session, Session::Empty);
            teardown(
                live,
                session,
                safety,
                journal_dir,
                latest,
                events,
                history,
                log,
            );
            *session = Session::Empty;
            last_menu.clear();
            history.configure(Vec::new());
            tracing::debug!(%op, had_session, "session unloaded");
            send(events, Event::DefinitionUnloaded { op });
            Outcome::Continue
        }
    }
}

pub(super) fn refuse(
    events: &EventTx,
    report: &mut CoreReport,
    op: OpId,
    code: FailureCode,
    message: &str,
) -> Outcome {
    tracing::warn!(%op, code = code.as_str(), %message, "command refused before the wire");
    report.failures += 1;
    send(
        events,
        Event::Failed {
            op,
            failure: Failure {
                code,
                message: message.to_owned(),
            },
        },
    );
    Outcome::Continue
}

/// Re-evaluates the menu and publishes it only when the tree changed.
///
/// Menu gates read the working tune and the live frame. A tune edit or a
/// connection change can therefore change the sidebar; evaluating on every
/// command instead would add work to reads that cannot affect it.
pub(super) fn refresh_menu(
    events: &EventTx,
    session: &Session,
    latest: &LatestFrame,
    last_menu: &mut Vec<MenuNode>,
) {
    let Some(project) = session.project() else {
        return;
    };

    let mut menu = crate::menu::build(project.definition());
    let frame = latest.get();
    let mut evaluator = GateEvaluator::new(project.definition(), project.tune(), frame.as_deref());
    crate::menu::evaluate(&mut menu, &mut evaluator);

    if menu == *last_menu {
        return;
    }

    let unresolved: Vec<&str> = evaluator.unresolved().collect();
    if !unresolved.is_empty() {
        tracing::debug!(
            unresolved = ?unresolved,
            "menu gates could not be evaluated"
        );
    }

    last_menu.clone_from(&menu);
    send(events, Event::MenuChanged { menu });
}

/// Sends the derived tune status after a change.
pub(super) fn send_status(events: &EventTx, op: OpId, project: &Project) {
    send(
        events,
        Event::TuneStatusRead {
            op,
            status: project.images().status(),
        },
    );
}

/// Appends a local (non-ECU) operation's result to the journal, best effort.
///
/// Writes and burns fail closed when the journal cannot be written; revert,
/// import and restore have already changed only the working image and a
/// snapshot exists, so a journal failure is logged instead of turning a
/// completed operation into an error.
pub(super) fn record_local_journal(
    session: &Session,
    journal_dir: Option<&std::path::Path>,
    operation: &str,
    cells: usize,
    variables: usize,
    result: &str,
    detail: &str,
) {
    let Some(journal) = session.journal(journal_dir) else {
        return;
    };
    let entry = JournalEntry {
        at: journal::timestamp(),
        operation: operation.to_owned(),
        phase: "result".to_owned(),
        port: "local".to_owned(),
        signature: session
            .definition()
            .map_or_else(String::new, |definition| definition.signature.clone()),
        pages: String::new(),
        cells,
        variables,
        result: result.to_owned(),
        detail: detail.to_owned(),
    };
    if let Err(error) = journal.append(&entry) {
        tracing::warn!(%error, "the local journal result could not be written");
    }
}

/// Checks every edit before one is stored, so a rejected request leaves the tune
/// exactly as it was.
pub(super) fn validate_edits(
    project: &Project,
    name: &str,
    edits: &[CellEdit],
) -> Result<(), String> {
    if project.is_read_only() {
        return Err(format!(
            "the tune is read-only: {}",
            project.report().read_only_reasons().join("; ")
        ));
    }

    let Some(spec) = project.layout().variable(name) else {
        return Err(format!("the tune has no variable named `{name}`"));
    };
    if spec.is_controller_owned() {
        return Err(format!(
            "`{name}` is owned by the controller and cannot be edited"
        ));
    }
    if !spec.shape.is_numeric() {
        return Err(format!("`{name}` holds text, not numbers"));
    }
    if spec.scale.is_none() || spec.translate.is_none() {
        return Err(format!(
            "`{name}` has an unresolved scale or translate, so a value cannot be converted"
        ));
    }

    for edit in edits {
        if !edit.value.is_finite() {
            return Err(format!(
                "the value for ({}, {}) is not a finite number",
                edit.row, edit.column
            ));
        }
        if spec.shape.offset(edit.row, edit.column).is_none() {
            return Err(format!(
                "`{name}` has no cell ({}, {})",
                edit.row, edit.column
            ));
        }
    }

    Ok(())
}

/// Checks every axis bin edit before one is stored.
pub(super) fn validate_axis_edits(
    project: &Project,
    name: &str,
    edits: &[AxisEdit],
) -> Result<(), String> {
    if project.is_read_only() {
        return Err(format!(
            "the tune is read-only: {}",
            project.report().read_only_reasons().join("; ")
        ));
    }

    let Some(spec) = project.layout().variable(name) else {
        return Err(format!("the tune has no variable named `{name}`"));
    };
    if spec.is_controller_owned() {
        return Err(format!(
            "`{name}` is owned by the controller and cannot be edited"
        ));
    }
    if !spec.shape.is_numeric() {
        return Err(format!("`{name}` holds text, not numbers"));
    }
    if spec.scale.is_none() || spec.translate.is_none() {
        return Err(format!(
            "`{name}` has an unresolved scale or translate, so a value cannot be converted"
        ));
    }
    let len = spec.shape.element_count();
    if edits.is_empty() {
        return Err("no axis bins to write".to_owned());
    }

    for edit in edits {
        if !edit.value.is_finite() {
            return Err(format!(
                "the value for bin {} is not a finite number",
                edit.index
            ));
        }
        if edit.index >= len {
            return Err(format!("`{name}` has no bin {}", edit.index));
        }
    }

    Ok(())
}

/// Checks a scalar edit before it is stored.
pub(super) fn validate_scalar_edit(
    project: &Project,
    name: &str,
    value: f64,
) -> Result<(), String> {
    if project.is_read_only() {
        return Err(format!(
            "the tune is read-only: {}",
            project.report().read_only_reasons().join("; ")
        ));
    }

    let Some(spec) = project.layout().variable(name) else {
        return Err(format!("the tune has no variable named `{name}`"));
    };
    if spec.is_controller_owned() {
        return Err(format!(
            "`{name}` is owned by the controller and cannot be edited"
        ));
    }
    if !spec.shape.is_numeric() {
        return Err(format!("`{name}` holds text, not numbers"));
    }
    if !spec.shape.is_scalar() {
        return Err(format!(
            "`{name}` is an array; edit it from its table or curve"
        ));
    }
    if spec.scale.is_none() || spec.translate.is_none() {
        return Err(format!(
            "`{name}` has an unresolved scale or translate, so a value cannot be converted"
        ));
    }
    if !value.is_finite() {
        return Err(format!("the value for `{name}` is not a finite number"));
    }

    Ok(())
}

/// Sends an event, logging why it did not arrive.
///
/// A closed channel means the UI is gone, which also ends the command loop on
/// its own. A drop means shutdown was in progress and the consumer had stopped
/// reading; it is counted in the report so a lost answer is never silent.
pub(super) fn send(events: &EventTx, event: Event) {
    match events.send(event) {
        Ok(()) | Err(SendFailure::Dropped) => {}
        Err(SendFailure::Closed) => tracing::debug!("the events channel is closed"),
    }
}
