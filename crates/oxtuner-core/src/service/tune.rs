#![allow(
    clippy::wildcard_imports,
    reason = "the service modules are one implementation split across files"
)]

use super::dispatch::{record_local_journal, refuse, send, send_status};
use super::*;

/// working image. Used for an imported tune, whose layout is a separate
/// allocation but describes the same definition.
pub(super) fn changed_against(project: &Project, other: &Tune) -> (usize, usize) {
    let mut cells = 0usize;
    let mut variables = 0usize;
    for spec in project.layout().variables() {
        let Some(current) = project.tune().value(&spec.name) else {
            continue;
        };
        let Some(imported) = other.value(&spec.name) else {
            continue;
        };
        let changed = current.changed_cells(imported);
        if changed > 0 {
            cells += changed;
            variables += 1;
        }
    }
    (cells, variables)
}

/// Clamps every non-controller tune value that is about to be written.
///
/// Edits are already clamped when they enter the core. An imported tune or a
/// file written by another tool can still carry an out-of-range value, and the
/// write path must not send those blindly. A value the controller already
/// holds is left alone: it is not part of the write plan, so clamping it would
/// turn "leave the ECU's tune alone" into a write of a different number — a
/// definition's range is not the firmware's limit. Controller-owned values are
/// not part of the write plan either. Returns how many cells changed.
#[expect(
    clippy::float_cmp,
    reason = "a value needs sending only when the ECU's stored raw value differs exactly"
)]
pub(super) fn clamp_working_changes(project: &mut Project) -> usize {
    let mut clamped: Vec<(String, usize, f64)> = Vec::new();
    {
        let images = project.images();
        let Some(ram) = images.ecu_ram() else {
            return 0;
        };
        for spec in project.layout().variables() {
            if spec.is_controller_owned() {
                continue;
            }
            for index in 0..spec.element_count() {
                let Ok(value) = images.working().user_at(&spec.name, index) else {
                    continue;
                };
                if ram
                    .user_at(&spec.name, index)
                    .is_ok_and(|sent| sent == value)
                {
                    // The ECU holds this value; there is nothing to send.
                    continue;
                }
                let (wanted, changed) = clamp_user(spec, value);
                if changed {
                    clamped.push((spec.name.clone(), index, wanted));
                }
            }
        }
    }
    let count = clamped.len();
    for (name, index, value) in clamped {
        if let Err(error) = project.tune_mut().set_user_at(&name, index, value) {
            tracing::warn!(%name, %error, "a clamped value could not be stored");
        }
    }
    count
}

/// The values that differ from ECU RAM, in layout order.
///
/// This is a tune **sync**, not a replay of edit timestamps: it survives saving
/// the `.msq` (which moves the file baseline) and an ECU that holds different
/// values from the file. Controller-owned values are excluded — the ECU owns
/// them, and writing a default over one is exactly the kind of damage the
/// write guardrails exist to prevent.
pub(super) fn unsent_changes(project: &Project) -> (Vec<String>, Vec<u8>, usize) {
    let working = project.images().working();
    let Some(ram) = project.images().ecu_ram() else {
        return (Vec::new(), Vec::new(), 0);
    };
    let mut variables = Vec::new();
    let mut pages = std::collections::BTreeSet::new();
    let mut cells = 0usize;
    for spec in project.layout().variables() {
        if spec.is_controller_owned() {
            continue;
        }
        let Some(page) = spec.ini_page() else {
            // PC-side values are stored in the tune file, never sent to the ECU.
            continue;
        };
        if spec.address.is_none() {
            // An unresolved address cannot be placed in a page image; skipping
            // it is honest, writing a guessed offset would not be.
            continue;
        }
        if !spec.bit_field_round_trips() {
            // The codec refuses this bit field, so a write would report success
            // without sending it. Leave it out of the plan instead.
            continue;
        }
        let (Some(after), Some(sent)) = (working.value(&spec.name), ram.value(&spec.name)) else {
            continue;
        };
        let remaining = after.changed_cells(sent);
        if remaining == 0 {
            continue;
        }
        variables.push(spec.name.clone());
        cells += remaining;
        pages.insert(page.get());
    }
    (variables, pages.into_iter().collect(), cells)
}

/// The unsent variables and cells on `pages`.
pub(super) fn unsent_for_pages(project: &Project, pages: &[u8]) -> (Vec<String>, usize) {
    let (variables, _, _) = unsent_changes(project);
    let working = project.images().working();
    let Some(ram) = project.images().ecu_ram() else {
        return (Vec::new(), 0);
    };
    let mut kept = Vec::new();
    let mut cells = 0usize;
    for name in variables {
        let Some(spec) = project.layout().variable(&name) else {
            continue;
        };
        if spec
            .ini_page()
            .is_none_or(|page| !pages.contains(&page.get()))
        {
            continue;
        }
        let (Some(after), Some(sent)) = (working.value(&name), ram.value(&name)) else {
            continue;
        };
        let remaining = after.changed_cells(sent);
        if remaining == 0 {
            continue;
        }
        cells += remaining;
        kept.push(name);
    }
    (kept, cells)
}

/// Counts edits that have not reached ECU RAM.
///
/// A burn persists whatever RAM holds, so an edit that is only in the working
/// image would be silently left behind. The comparison is working → RAM: a
/// value the ECU already holds is not an unsent edit.
pub(super) fn unsent_edits(project: &Project) -> (usize, usize) {
    let (variables, _, cells) = unsent_changes(project);
    (cells, variables.len())
}

/// Makes the ECU image the working tune.
///
/// Reading pages fills the ECU image but leaves the project file's values on
/// screen, so an unsaved edit is never lost. This is the explicit way to start
/// from what the controller holds instead. The file baseline does not move, so
/// the adopted values show as unsaved changes and a later write is only the
/// delta from the ECU's own tune.
pub(super) fn adopt_ecu_image(
    op: OpId,
    session: &mut Session,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::AdoptRefused,
            "open a project before taking the ECU's tune",
        );
    };
    let Some(ram) = project.images().ecu_ram().cloned() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::AdoptRefused,
            "read the ECU pages before taking its tune",
        );
    };
    if let Err(error) = project.images_mut().import(ram) {
        return refuse(
            events,
            report,
            op,
            FailureCode::AdoptRefused,
            &error.to_string(),
        );
    }
    tracing::info!("the ECU image became the working tune");
    send(events, Event::EcuAdopted { op });
    Outcome::Continue
}

/// Resolves cells that differ between the working image and the ECU.
pub(super) fn resolve_conflicts(
    op: OpId,
    session: &mut Session,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    resolution: ConflictResolution,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "no tune is open",
        );
    };
    let Some(ram) = project.images().ecu_ram().cloned() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "read the ECU before resolving a conflict",
        );
    };

    match resolution {
        ConflictResolution::UseEcu => {
            let snapshot = match snapshot::write(project, "conflict") {
                Ok(path) => path,
                Err(message) => {
                    return refuse(events, report, op, FailureCode::SnapshotFailed, &message);
                }
            };
            if let Err(error) = project.images_mut().import(ram) {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::WriteRefused,
                    &error.to_string(),
                );
            }
            project.images_mut().clear_conflicts();
            record_local_journal(
                session,
                journal_dir,
                "conflict",
                0,
                0,
                "ok",
                &format!("kept ECU values; snapshot {}", snapshot.display()),
            );
        }
        ConflictResolution::KeepWorking => {
            project.images_mut().clear_conflicts();
            record_local_journal(
                session,
                journal_dir,
                "conflict",
                0,
                0,
                "ok",
                "kept working values",
            );
        }
    }

    send(events, Event::ConflictsResolved { op });
    Outcome::Continue
}

/// Discards local edits after snapshotting the working image.
pub(super) fn revert_tune(
    op: OpId,
    session: &mut Session,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "no tune is open",
        );
    };
    let Ok(diff) = TuneDiff::between(project.images().working(), project.images().baseline())
    else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the tune images do not share a layout",
        );
    };
    let snapshot = match snapshot::write(project, "revert") {
        Ok(path) => path,
        Err(message) => return refuse(events, report, op, FailureCode::SnapshotFailed, &message),
    };
    project.images_mut().revert_to_baseline();
    tracing::info!(%op, cells = diff.cells, "tune reverted to the file baseline");
    let snapshot_detail = format!("snapshot {}", snapshot.display());
    send(
        events,
        Event::TuneReverted {
            op,
            report: RevertReport {
                cells: diff.cells,
                snapshot: Some(snapshot),
            },
        },
    );
    send_status(events, op, project);
    record_local_journal(
        session,
        journal_dir,
        "revert",
        diff.cells,
        0,
        "ok",
        &snapshot_detail,
    );
    Outcome::Continue
}

/// Discards the edits one table, curve or dialog owns.
pub(super) fn revert_target(
    op: OpId,
    session: &mut Session,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    latest: &LatestFrame,
    name: &str,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "no tune is open",
        );
    };

    let variables = match target_variables(project, name) {
        Ok(variables) => variables,
        Err(message) => {
            return refuse(events, report, op, FailureCode::WriteRefused, &message);
        }
    };

    // Discarding is irreversible from the tune's side, so the working image is
    // snapshotted first, exactly as a whole-tune revert does.
    let snapshot = match snapshot::write(project, "revert") {
        Ok(path) => path,
        Err(message) => return refuse(events, report, op, FailureCode::SnapshotFailed, &message),
    };

    let reverted = project.images_mut().revert_variables(&variables);
    tracing::info!(
        %op,
        target = %name,
        variables = variables.len(),
        cells = reverted,
        "target reverted to the file baseline"
    );

    // Echo every face the name has, so an open editor and the unsaved marks
    // both see the baseline again: a dialog carries its constants and the map
    // it embeds, and each is reported separately.
    if let Ok(table) = TableView::from_project(project, name) {
        send(events, Event::TableEdited { op, table });
    }
    if let Ok(dialog) = DialogView::from_project(project, name, latest.get().as_deref()) {
        send(events, Event::DialogEdited { op, dialog });
    }
    send_status(events, op, project);

    let detail = format!("snapshot {}", snapshot.display());
    record_local_journal(
        session,
        journal_dir,
        "revert-target",
        reverted,
        variables.len(),
        "ok",
        &detail,
    );
    Outcome::Continue
}

/// The variables one table, curve or dialog owns.
///
/// A table owns its cells and both axis bins. A dialog owns its constant
/// variables, plus the table it embeds when it has one. The two lookups are not
/// alternatives: [`TableView::from_project`] resolves a dialog name to the
/// map inside it, so a dialog that embeds a table matches both and must give up
/// both sets of variables.
///
/// The list is sorted and deduplicated so a variable shared by two rows is
/// reverted once. A name that matches nothing, or one that owns no variables,
/// is an error rather than an empty revert.
pub(super) fn target_variables(project: &Project, name: &str) -> Result<Vec<String>, String> {
    let mut variables = Vec::new();

    if let Ok(table) = TableView::from_project(project, name) {
        variables.extend(table_variables(&table));
    }

    if let Ok(dialog) = DialogView::from_project(project, name, None) {
        variables.extend(
            dialog
                .fields
                .iter()
                .map(|field| field.variable.clone())
                .filter(|variable| !variable.is_empty()),
        );

        if let Some(embedded) =
            crate::menu::embedded_target(&dialog.id, project.definition(), &mut HashSet::new())
            && let Ok(table) = TableView::from_project(project, &embedded.id)
        {
            variables.extend(table_variables(&table));
        }
    }

    if variables.is_empty() {
        return Err(format!(
            "the definition has no table, curve or dialog named `{name}` with variables to discard"
        ));
    }

    variables.sort_unstable();
    variables.dedup();
    Ok(variables)
}

/// The cell variable and axis bins one table summary reads from.
pub(super) fn table_variables(table: &TableView) -> Vec<String> {
    [&table.name, &table.x_axis.name, &table.y_axis.name]
        .into_iter()
        .filter(|variable| !variable.is_empty())
        .cloned()
        .collect()
}

/// Imports one `.msq` into the working image.
#[expect(
    clippy::too_many_arguments,
    reason = "the handler mirrors the fields of one protocol request"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the import path joins several consent and layout checks into one report"
)]
pub(super) fn import_tune(
    op: OpId,
    session: &mut Session,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    path: &std::path::Path,
    consented: bool,
    operation: &str,
) -> Outcome {
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::ImportRejected,
            "no tune is open",
        );
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return refuse(
                events,
                report,
                op,
                FailureCode::ImportRejected,
                &format!("{} could not be read: {error}", path.display()),
            );
        }
    };
    let msq = match oxtuner_project::Msq::parse(&bytes) {
        Ok(msq) => msq,
        Err(error) => {
            return refuse(
                events,
                report,
                op,
                FailureCode::ImportRejected,
                &format!("{} is not a readable .msq: {error}", path.display()),
            );
        }
    };
    let check = oxtuner_project::check(&msq, project.definition());
    let signature_mismatch = check
        .blockers
        .iter()
        .any(|issue| issue.kind == oxtuner_project::IssueKind::SignatureMismatch);
    let other_blocker = check
        .blockers
        .iter()
        .find(|issue| issue.kind != oxtuner_project::IssueKind::SignatureMismatch);
    if let Some(blocker) = other_blocker {
        return refuse(
            events,
            report,
            op,
            FailureCode::ImportRejected,
            &format!("the tune cannot be imported: {}", blocker.message),
        );
    }
    if signature_mismatch && !consented {
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::ImportNeedsConsent,
                    message: format!(
                        "the tune in {} was written for a different ECU signature. Importing it \
                         needs explicit consent",
                        path.display()
                    ),
                },
            },
        );
        return Outcome::Continue;
    }

    let loaded = oxtuner_project::load::load(&msq, project.definition());
    let (cells, variables) = changed_against(project, &loaded.tune);
    let snapshot = match snapshot::write(project, operation) {
        Ok(path) => path,
        Err(message) => return refuse(events, report, op, FailureCode::SnapshotFailed, &message),
    };
    if let Err(error) = project.images_mut().import(loaded.tune) {
        return refuse(
            events,
            report,
            op,
            FailureCode::ImportRejected,
            &error.to_string(),
        );
    }
    tracing::info!(
        %op,
        path = %path.display(),
        cells,
        consented = signature_mismatch && consented,
        "tune imported"
    );
    let snapshot_detail = format!("snapshot {}", snapshot.display());
    send(
        events,
        Event::TuneImported {
            op,
            report: ImportReport {
                path: path.to_owned(),
                cells,
                variables,
                consented: signature_mismatch && consented,
                snapshot: Some(snapshot),
            },
        },
    );
    send_status(events, op, project);
    record_local_journal(
        session,
        journal_dir,
        operation,
        cells,
        variables,
        "ok",
        &snapshot_detail,
    );
    Outcome::Continue
}
