#![allow(
    clippy::wildcard_imports,
    reason = "the service modules are one implementation split across files"
)]

use super::dispatch::{refuse, send, send_status};
use super::ecu::connection_lost;
use super::tune::{
    ambiguous_write_page, clamp_working_changes, conflicting_value_page, reconcile_groups,
    unresolved_conflicts, unsent_ambiguous_page, unsent_changes, unsent_conflicting_value,
    unsent_edits, unsent_for_pages,
};
use super::*;

#[expect(
    clippy::too_many_arguments,
    reason = "the handler mirrors the fields of one protocol request"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the refusal reasons stay in one place so the safety gate is easy to audit"
)]
pub(super) fn run_controller_command(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    safety: &Safety,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    dialog: &str,
    name: &str,
) -> Outcome {
    let Some(connection) = live.as_ref() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            "connect to the ECU before sending a controller command",
        );
    };
    if connection.txns.write_tx.is_some() || connection.pending_kind(PendingKind::Write) {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            "a write or burn is running; send the command after it finishes",
        );
    }
    if connection.pending_kind(PendingKind::ReadAll) {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            "a page read is running; send the command after it finishes",
        );
    }
    if connection.txns.command_tx.is_some() || connection.pending_kind(PendingKind::Command) {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            "a controller command is already running",
        );
    }

    let Some(project) = session.project() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            "open a project before sending a controller command",
        );
    };
    let Some(controller) = project
        .definition()
        .controller_commands
        .get_ignore_case(name)
    else {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            format!("`{name}` is not declared in [ControllerCommands]"),
        );
    };
    let commands = controller.commands.clone();
    if commands.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            format!("`{name}` declares no command string"),
        );
    }

    let read_only = project.report().read_only_reasons().join("; ");
    if let Err(reason) = safety.gate(
        connection.connection.connected,
        connection.connection.verdict == SignatureVerdict::Match,
        true,
        (!read_only.is_empty()).then_some(read_only.as_str()),
    ) {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            reason.as_str(),
        );
    }

    let Some(journal) = session.journal(journal_dir) else {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            "no write journal can be written for this session; refusing an unjournaled command",
        );
    };
    let begin = JournalEntry {
        at: journal::timestamp(),
        operation: "controller-command".to_owned(),
        phase: "begin".to_owned(),
        port: connection.connection.link_label.clone(),
        signature: connection.connection.signature.clone(),
        pages: "all".to_owned(),
        cells: 0,
        variables: 0,
        result: "pending".to_owned(),
        detail: format!("[ControllerCommands] {name} from dialog {dialog}"),
    };
    if let Err(error) = journal.append(&begin) {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            format!("the write journal could not be written: {error}"),
        );
    }

    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::ControllerActionRefused,
            "the session closed before the command started",
        );
    };
    connection.txns.command_tx = Some(CommandTxn {
        dialog: dialog.to_owned(),
        name: name.to_owned(),
        journal,
    });

    if let Err(error) = connection.request(op, PendingKind::Command, IoOp::Command { commands }) {
        finish_command_failed(connection, "the controller command could not be queued");
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::ControllerActionRefused,
                    message: error.to_string(),
                },
            },
        );
    }

    Outcome::Continue
}

/// Starts a verified RAM write.
#[expect(
    clippy::too_many_arguments,
    reason = "the handler mirrors the fields of one protocol request"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the write path is one linear protocol sequence and reads best in order"
)]
pub(super) fn write_to_ram(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    safety: &Safety,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    requested: Vec<u8>,
) -> Outcome {
    let Some(connection) = live.as_ref() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "connect to the ECU before writing",
        );
    };
    if connection.txns.write_tx.is_some() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "a write or burn is already running",
        );
    }
    if connection.pending_kind(PendingKind::ReadAll) {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "a page read is running; write after it finishes",
        );
    }
    if connection.pending_kind(PendingKind::Command) {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "a controller command is running; write after it finishes",
        );
    }
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "open a project before writing",
        );
    };
    let read_only = project.report().read_only_reasons().join("; ");
    if let Err(reason) = safety.gate(
        connection.connection.connected,
        connection.connection.verdict == SignatureVerdict::Match,
        true,
        (!read_only.is_empty()).then_some(read_only.as_str()),
    ) {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            reason.as_str(),
        );
    }
    if project.images().ecu_ram().is_none() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "read the ECU into the tune image before writing (the session does this after connecting)",
        );
    }
    if !connection.images.pages_complete {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "read every page before writing; an incomplete ECU image cannot be edited safely",
        );
    }
    if let Some(reason) = unresolved_conflicts(project) {
        return refuse(events, report, op, FailureCode::WriteRefused, &reason);
    }
    let clamped = clamp_working_changes(project);
    if clamped > 0 {
        tracing::info!(%op, clamped, "out-of-range values clamped before writing");
    }
    let (changed_variables, changed_pages, _) = unsent_changes(project);
    if changed_variables.is_empty() || changed_pages.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "there are no values that differ from ECU RAM",
        );
    }

    let targets = if requested.is_empty() {
        changed_pages
    } else {
        let mut chosen = Vec::new();
        for page in requested {
            let Some(declared) = project
                .definition()
                .pages
                .iter()
                .find(|entry| entry.number == page)
            else {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::WriteRefused,
                    format!("page {page} is not in the definition"),
                );
            };
            if !declared.is_writable() {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::WriteRefused,
                    format!("page {page} has no write command"),
                );
            }
            if !changed_pages.contains(&page) {
                continue;
            }
            chosen.push(page);
        }
        chosen.sort_unstable();
        chosen.dedup();
        chosen
    };
    if targets.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the selected pages have no values that differ from ECU RAM",
        );
    }
    if let Some(page) = ambiguous_write_page(project, &targets) {
        let (names, total) = project.layout().ambiguous_names(page);
        let listed: Vec<String> = names.into_iter().map(|name| format!("`{name}`")).collect();
        let more = total.saturating_sub(listed.len());
        let unlisted = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };

        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            format!(
                "page {page} has constants whose shared bytes no single value owns: {}{unlisted}; \
                 fix the definition before writing",
                listed.join(", ")
            ),
        );
    }
    if let Some(conflict) = conflicting_value_page(project, &targets) {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            format!("{conflict}; make those values agree before writing"),
        );
    }
    let (changed_variables, cells) = unsent_for_pages(project, &targets);
    if changed_variables.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the selected pages hold no values that still need sending",
        );
    }
    let variables = changed_variables.len();

    let snapshot = match snapshot::write(project, "write") {
        Ok(path) => path,
        Err(error) => {
            return refuse(events, report, op, FailureCode::SnapshotFailed, error);
        }
    };
    let Some(journal) = session.journal(journal_dir) else {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            "no write journal can be written for this session; refusing an unjournaled write",
        );
    };
    let begin = JournalEntry {
        at: journal::timestamp(),
        operation: "write".to_owned(),
        phase: "begin".to_owned(),
        port: connection.connection.link_label.clone(),
        signature: connection.connection.signature.clone(),
        pages: journal::pages_label(&targets),
        cells,
        variables,
        result: "pending".to_owned(),
        detail: format!("snapshot {}", snapshot.display()),
    };
    if let Err(error) = journal.append(&begin) {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            format!("the write journal could not be written: {error}"),
        );
    }

    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::WriteRefused,
            "the session closed before the write started",
        );
    };
    connection.begin_transaction(WriteTxn {
        op,
        kind: TxnKind::Write,
        phase: TxnPhase::ReadBase,
        pages: targets.clone(),
        burned: 0,
        base: BTreeMap::new(),
        expected: BTreeMap::new(),
        readback: BTreeMap::new(),
        changed_variables,
        cells,
        variables,
        snapshot: Some(snapshot),
        journal: Some(journal),
    });
    if let Err(error) =
        connection.request(op, PendingKind::Write, IoOp::ReadPages { pages: targets })
    {
        connection.abort_transaction("the write could not be queued");
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::WriteRefused,
                    message: error.to_string(),
                },
            },
        );
    }
    Outcome::Continue
}

/// Starts a verified burn.
#[expect(
    clippy::too_many_arguments,
    reason = "the handler mirrors the fields of one protocol request"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the burn path is one linear protocol sequence and reads best in order"
)]
pub(super) fn burn_to_flash(
    op: OpId,
    session: &mut Session,
    live: &mut Option<Live>,
    safety: &Safety,
    journal_dir: Option<&std::path::Path>,
    events: &EventTx,
    report: &mut CoreReport,
    requested: Vec<u8>,
) -> Outcome {
    let Some(connection) = live.as_ref() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "connect to the ECU before burning",
        );
    };
    if connection.txns.write_tx.is_some() {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "a write or burn is already running",
        );
    }
    if connection.pending_kind(PendingKind::ReadAll) {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "a page read is running; burn after it finishes",
        );
    }
    if connection.pending_kind(PendingKind::Command) {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "a controller command is running; burn after it finishes",
        );
    }
    let Some(project) = session.project_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "open a project before burning",
        );
    };
    let read_only = project.report().read_only_reasons().join("; ");
    if let Err(reason) = safety.gate(
        connection.connection.connected,
        connection.connection.verdict == SignatureVerdict::Match,
        true,
        (!read_only.is_empty()).then_some(read_only.as_str()),
    ) {
        return refuse(events, report, op, FailureCode::BurnRefused, reason);
    }
    if !connection.images.pages_complete {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "read every page before burning",
        );
    }
    if project.images().ecu_ram().is_none() {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the ECU image is unknown; read it first",
        );
    }
    if let Some(reason) = unresolved_conflicts(project) {
        return refuse(events, report, op, FailureCode::BurnRefused, &reason);
    }
    let (unsent_variables, _, unsent) = unsent_changes(project);
    if unsent > 0 {
        if let Some(page) = unsent_ambiguous_page(project, &unsent_variables) {
            return refuse(
                events,
                report,
                op,
                FailureCode::BurnRefused,
                format!(
                    "page {page} cannot be written because its definition shares bytes with no \
                     owner; revert those values or adopt the ECU tune before burning"
                ),
            );
        }
        if let Some(conflict) = unsent_conflicting_value(project, &unsent_variables) {
            return refuse(
                events,
                report,
                op,
                FailureCode::BurnRefused,
                format!(
                    "{conflict}; revert one of those values or adopt the ECU tune before burning"
                ),
            );
        }

        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the working image has edits that are not in ECU RAM. Write them first; burning would persist the older RAM values",
        );
    }

    let burnable: Vec<u8> = project
        .definition()
        .pages
        .iter()
        .filter(|page| page.is_burnable())
        .map(|page| page.number)
        .collect();
    if burnable.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the definition declares no burnable page",
        );
    }
    let targets = if requested.is_empty() {
        burnable
    } else {
        let mut chosen = Vec::new();
        for page in requested {
            if !burnable.contains(&page) {
                return refuse(
                    events,
                    report,
                    op,
                    FailureCode::BurnRefused,
                    format!("page {page} cannot be burned"),
                );
            }
            chosen.push(page);
        }
        chosen.sort_unstable();
        chosen.dedup();
        chosen
    };

    // A burn makes the tune durable, so the project file records it too.
    // Every check that can refuse the burn has passed; a save that fails stops
    // it instead of leaving the file behind. This is a durable user action, so
    // the header date moves with it.
    let stamp = super::stamp::now();
    if !project.images().status().unsaved.is_empty()
        && let Err(error) = project.save_with_stamp(Some(&stamp))
    {
        return refuse(
            events,
            report,
            op,
            FailureCode::TuneSaveFailed,
            format!("the burn needs the project saved first: {error}"),
        );
    }

    let snapshot = match snapshot::write(project, "burn") {
        Ok(path) => path,
        Err(error) => {
            return refuse(events, report, op, FailureCode::SnapshotFailed, error);
        }
    };
    let Some(journal) = session.journal(journal_dir) else {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            "no write journal can be written for this session",
        );
    };
    let begin = JournalEntry {
        at: journal::timestamp(),
        operation: "burn".to_owned(),
        phase: "begin".to_owned(),
        port: connection.connection.link_label.clone(),
        signature: connection.connection.signature.clone(),
        pages: journal::pages_label(&targets),
        cells: 0,
        variables: 0,
        result: "pending".to_owned(),
        detail: format!("snapshot {}", snapshot.display()),
    };
    if let Err(error) = journal.append(&begin) {
        return refuse(
            events,
            report,
            op,
            FailureCode::JournalFailed,
            format!("the write journal could not be written: {error}"),
        );
    }

    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::BurnRefused,
            "the session closed before the burn started",
        );
    };
    connection.begin_transaction(WriteTxn {
        op,
        kind: TxnKind::Burn,
        phase: TxnPhase::ReadBase,
        pages: targets.clone(),
        burned: 0,
        base: BTreeMap::new(),
        expected: BTreeMap::new(),
        readback: BTreeMap::new(),
        changed_variables: Vec::new(),
        cells: 0,
        variables: 0,
        snapshot: Some(snapshot),
        journal: Some(journal),
    });
    if let Err(error) =
        connection.request(op, PendingKind::Write, IoOp::ReadPages { pages: targets })
    {
        connection.abort_transaction("the burn could not be queued");
        report.failures += 1;
        send(
            events,
            Event::Failed {
                op,
                failure: Failure {
                    code: FailureCode::BurnRefused,
                    message: error.to_string(),
                },
            },
        );
    }
    Outcome::Continue
}

#[expect(
    clippy::too_many_arguments,
    reason = "the handler mirrors the fields of one protocol request"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the write and burn transaction advances through one linear protocol sequence"
)]
pub(super) fn advance_write(
    live: &mut Live,
    op: OpId,
    outcome: IoOutcome,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    latest: &LatestFrame,
    history: &ChartHistory,
    log: &LogStatus,
    report: &mut CoreReport,
) {
    let Some(mut transaction) = live.txns.write_tx.take() else {
        tracing::warn!(%op, "a write reply arrived without a transaction");
        return;
    };
    debug_assert_eq!(
        transaction.op, op,
        "the write transaction belongs to the reply's operation"
    );

    match outcome {
        IoOutcome::Pages { pages } => match transaction.phase {
            TxnPhase::ReadBase => {
                // The bytes the tune image was built from. A fresh read that
                // disagrees with them means the ECU moved since the last read:
                // a read that raced the reboot, or a tune changed behind our
                // back. Patching would put values over a tune we never saw, so
                // stop and let the user read again.
                let drifted: Vec<u8> = pages
                    .iter()
                    .filter(|read| {
                        live.images
                            .page_bytes
                            .get(&read.page)
                            .is_some_and(|known| known != &read.bytes)
                    })
                    .map(|read| read.page)
                    .collect();
                transaction.base = pages
                    .iter()
                    .map(|read| (read.page, read.bytes.clone()))
                    .collect();
                for read in &pages {
                    live.images.page_bytes.insert(read.page, read.bytes.clone());
                }
                if !drifted.is_empty() {
                    let action = match transaction.kind {
                        TxnKind::Write => "writing",
                        TxnKind::Burn => "burning",
                    };
                    finish_refused(
                        live,
                        transaction,
                        events,
                        report,
                        &format!(
                            "page(s) {} changed on the ECU since the last read; read every page \
                             again before {action}",
                            journal::pages_label(&drifted)
                        ),
                    );
                    return;
                }

                match transaction.kind {
                    TxnKind::Write => {
                        let Some(project) = session.project_mut() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the project closed during the write".to_owned(),
                                false,
                            );
                            return;
                        };
                        let Some(mut image) = project.images().ecu_ram().cloned() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the ECU image is unknown; refusing to patch it".to_owned(),
                                false,
                            );
                            return;
                        };
                        let layout = Arc::clone(project.layout());
                        for read in &pages {
                            let Some(page) = IniPage::new(read.page) else {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {} is not a valid page number", read.page),
                                    false,
                                );
                                return;
                            };
                            if let Err(error) = decode_page(&layout, page, &read.bytes, &mut image)
                            {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {} could not be decoded: {error}", read.page),
                                    false,
                                );
                                return;
                            }
                        }
                        // The fresh read is the truth about RAM: use it for the
                        // delta instead of the image from connect time.
                        if let Err(error) = project.images_mut().set_ecu_ram(image) {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                format!("the fresh ECU image could not be stored: {error}"),
                                false,
                            );
                            return;
                        }
                        let (variables, changed_pages, _) = unsent_changes(project);
                        if variables.is_empty() {
                            finish_refused(
                                live,
                                transaction,
                                events,
                                report,
                                "the ECU already holds these values",
                            );
                            return;
                        }
                        let target_set: HashSet<u8> = transaction.pages.iter().copied().collect();
                        let targets: Vec<u8> = changed_pages
                            .into_iter()
                            .filter(|page| target_set.contains(page))
                            .collect();
                        if targets.is_empty() {
                            finish_refused(
                                live,
                                transaction,
                                events,
                                report,
                                "there are no values that differ from ECU RAM on these pages",
                            );
                            return;
                        }
                        let (variables, cells) = unsent_for_pages(project, &targets);
                        if variables.is_empty() {
                            finish_refused(
                                live,
                                transaction,
                                events,
                                report,
                                "the selected pages hold no values that still need sending",
                            );
                            return;
                        }
                        transaction.changed_variables.clone_from(&variables);
                        transaction.cells = cells;
                        transaction.variables = variables.len();
                        transaction.pages.clone_from(&targets);

                        let mut writes = Vec::with_capacity(targets.len());
                        for page in &targets {
                            let Some(base) = transaction.base.get(page) else {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {page} was not read before the write"),
                                    false,
                                );
                                return;
                            };
                            let Some(ini_page) = IniPage::new(*page) else {
                                continue;
                            };
                            match patch_page(
                                &layout,
                                ini_page,
                                base,
                                project.tune(),
                                &transaction.changed_variables,
                            ) {
                                Ok(bytes) => {
                                    transaction.expected.insert(*page, bytes.clone());
                                    writes.push(PageWrite {
                                        page: *page,
                                        data: bytes,
                                    });
                                }
                                Err(error) => {
                                    fail_transaction(
                                        live,
                                        transaction,
                                        session,
                                        safety,
                                        events,
                                        report,
                                        format!("page {page} could not be patched: {error}"),
                                        false,
                                    );
                                    return;
                                }
                            }
                        }

                        transaction.phase = TxnPhase::Write;
                        live.txns.write_tx = Some(transaction);
                        if let Err(error) =
                            live.request(op, PendingKind::Write, IoOp::WritePages { pages: writes })
                        {
                            let Some(transaction) = live.txns.write_tx.take() else {
                                return;
                            };
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                error.to_string(),
                                false,
                            );
                        }
                    }
                    TxnKind::Burn => {
                        let Some(project) = session.project_mut() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the project closed during the burn".to_owned(),
                                false,
                            );
                            return;
                        };
                        let Some(mut image) = project.images().ecu_ram().cloned() else {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the ECU image is unknown; refusing to burn".to_owned(),
                                false,
                            );
                            return;
                        };
                        let layout = Arc::clone(project.layout());
                        let burn_pages = transaction.pages.clone();
                        for page in &burn_pages {
                            let Some(bytes) = transaction.base.get(page) else {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {page} was not read before the burn"),
                                    false,
                                );
                                return;
                            };
                            let Some(ini_page) = IniPage::new(*page) else {
                                continue;
                            };
                            if let Err(error) = decode_page(&layout, ini_page, bytes, &mut image) {
                                fail_transaction(
                                    live,
                                    transaction,
                                    session,
                                    safety,
                                    events,
                                    report,
                                    format!("page {page} could not be decoded: {error}"),
                                    false,
                                );
                                return;
                            }
                        }
                        if let Err(error) = project.images_mut().set_ecu_ram(image) {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                format!("the fresh ECU image could not be stored: {error}"),
                                false,
                            );
                            return;
                        }
                        let (unsent, _) = unsent_edits(project);
                        if unsent > 0 {
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                "the working image has edits that are not in ECU RAM. Write them first; burning would persist the older RAM values".to_owned(),
                                false,
                            );
                            return;
                        }
                        transaction.expected = transaction.base.clone();
                        let pages = if live.can_observe_burn() {
                            // One page per burn, each followed by a check that
                            // the firmware saved it before the next command.
                            transaction.pages.first().copied().into_iter().collect()
                        } else {
                            transaction.pages.clone()
                        };
                        transaction.phase = TxnPhase::Burn;
                        live.txns.write_tx = Some(transaction);
                        if let Err(error) =
                            live.request(op, PendingKind::Write, IoOp::BurnPages { pages })
                        {
                            let Some(transaction) = live.txns.write_tx.take() else {
                                return;
                            };
                            fail_transaction(
                                live,
                                transaction,
                                session,
                                safety,
                                events,
                                report,
                                error.to_string(),
                                false,
                            );
                        }
                    }
                }
            }
            TxnPhase::Verify => {
                if !verify_pages(&mut transaction, &pages) {
                    let reason = verification_message(&transaction);
                    fail_transaction(
                        live,
                        transaction,
                        session,
                        safety,
                        events,
                        report,
                        reason,
                        false,
                    );
                    return;
                }
                succeed_transaction(live, transaction, session, safety, events, report);
            }
            TxnPhase::Write | TxnPhase::Burn | TxnPhase::BurnWait => {
                tracing::warn!(%op, "an unexpected page read arrived during a transaction");
                live.txns.write_tx = Some(transaction);
            }
        },
        IoOutcome::Written { pages, bytes } => {
            tracing::debug!(%op, pages, bytes, "pages written; reading them back");
            start_verify(live, op, transaction, session, safety, events, report);
        }
        IoOutcome::Burned { pages } => {
            tracing::debug!(%op, pages, "pages burned; waiting for the flash write");
            // The firmware answers a burn before it writes EEPROM, so reading
            // the pages back here would only prove RAM. Wait for its queue to
            // empty first, or the link may close over an unfinished save.
            transaction.burned = transaction.burned.saturating_add(pages);
            transaction.phase = TxnPhase::BurnWait;
            live.txns.write_tx = Some(transaction);
            live.begin_burn_wait();
            if live.txns.burn_wait.is_none() {
                // No realtime block to sample: nothing to wait on.
                let Some(transaction) = live.txns.write_tx.take() else {
                    return;
                };
                start_verify(live, op, transaction, session, safety, events, report);
            }
        }
        IoOutcome::Realtime { block } => {
            if transaction.phase == TxnPhase::BurnWait {
                let Some(wait) = live.txns.burn_wait else {
                    tracing::warn!(%op, "a burn check arrived without a wait");
                    live.txns.write_tx = Some(transaction);
                    return;
                };
                let queued = wait.observed
                    && live
                        .telemetry
                        .decoder
                        .channel_raw(&block, BURN_PENDING_CHANNEL)
                        != Some(0.0);
                if queued {
                    if Instant::now() >= wait.deadline {
                        live.txns.burn_wait = None;
                        fail_transaction(
                            live,
                            transaction,
                            session,
                            safety,
                            events,
                            report,
                            format!(
                                "the ECU still reports a pending EEPROM write after {} s. \
                                 Do not close the link: closing resets the board and drops an \
                                 unfinished save",
                                BURN_TIMEOUT.as_secs()
                            ),
                            false,
                        );
                        return;
                    }
                    let mut wait = wait;
                    wait.next_at = Instant::now() + BURN_POLL_INTERVAL;
                    live.txns.burn_wait = Some(wait);
                    live.txns.write_tx = Some(transaction);
                    return;
                }
                live.txns.burn_wait = None;
                if transaction.burned < transaction.pages.len() {
                    // The next page goes out only after this one's save is
                    // accounted for.
                    let page = transaction.pages[transaction.burned];
                    transaction.phase = TxnPhase::Burn;
                    live.txns.write_tx = Some(transaction);
                    if let Err(error) = live.request(
                        op,
                        PendingKind::Write,
                        IoOp::BurnPages { pages: vec![page] },
                    ) {
                        let Some(transaction) = live.txns.write_tx.take() else {
                            return;
                        };
                        fail_transaction(
                            live,
                            transaction,
                            session,
                            safety,
                            events,
                            report,
                            error.to_string(),
                            false,
                        );
                    }
                    return;
                }
                tracing::debug!(%op, "the burn reached flash; reading it back");
                start_verify(live, op, transaction, session, safety, events, report);
            } else {
                tracing::warn!(%op, "an unexpected realtime block arrived during a transaction");
                live.txns.write_tx = Some(transaction);
            }
        }
        IoOutcome::Failed(failure) => {
            let link_lost = matches!(
                failure.kind,
                IoFailureKind::LinkLost | IoFailureKind::Timeout
            );
            fail_transaction(
                live,
                transaction,
                session,
                safety,
                events,
                report,
                failure.message.clone(),
                link_lost,
            );
            if link_lost {
                connection_lost(live, session, events, latest, failure.message);
                history.clear();
                log.mark_stopped();
            }
        }
        other => {
            tracing::warn!(%op, ?other, "an unexpected reply arrived during a transaction");
            live.txns.write_tx = Some(transaction);
        }
    }
}

/// Queues the read-back that ends a write or burn transaction.
pub(super) fn start_verify(
    live: &mut Live,
    op: OpId,
    mut transaction: WriteTxn,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    report: &mut CoreReport,
) {
    transaction.phase = TxnPhase::Verify;
    let pages = transaction.pages.clone();
    live.txns.write_tx = Some(transaction);
    if let Err(error) = live.request(op, PendingKind::Write, IoOp::ReadPages { pages }) {
        let Some(transaction) = live.txns.write_tx.take() else {
            return;
        };
        fail_transaction(
            live,
            transaction,
            session,
            safety,
            events,
            report,
            error.to_string(),
            false,
        );
    }
}

/// Compares each page read back with what the transaction expected.
pub(super) fn verify_pages(transaction: &mut WriteTxn, pages: &[PageRead]) -> bool {
    for read in pages {
        transaction.readback.insert(read.page, read.bytes.clone());
    }
    transaction.pages.iter().all(|page| {
        let expected = transaction.expected.get(page);
        let actual = transaction.readback.get(page);
        expected.is_some() && expected == actual
    })
}

pub(super) fn verification_message(transaction: &WriteTxn) -> String {
    let mismatched: Vec<String> = transaction
        .pages
        .iter()
        .filter(|page| {
            let expected = transaction.expected.get(page);
            let actual = transaction.readback.get(page);
            expected.is_none() || expected != actual
        })
        .map(ToString::to_string)
        .collect();
    format!(
        "the read-back did not match on page {}. The ECU image was reloaded from the read-back; \
         the write path is halted until you review the tune",
        mismatched.join(", ")
    )
}

/// Applies a verified write or burn and reports it.
pub(super) fn succeed_transaction(
    live: &mut Live,
    transaction: WriteTxn,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    report: &mut CoreReport,
) {
    if session.project().is_none() {
        fail_transaction(
            live,
            transaction,
            session,
            safety,
            events,
            report,
            "the project closed during the operation".to_owned(),
            false,
        );
        return;
    }

    let applied = match transaction.kind {
        TxnKind::Write => apply_verified_write(live, &transaction, session),
        TxnKind::Burn => apply_verified_burn(&transaction, session),
    };
    if let Err(reason) = applied {
        fail_transaction(
            live,
            transaction,
            session,
            safety,
            events,
            report,
            reason,
            false,
        );
        return;
    }

    match transaction.kind {
        TxnKind::Write => {
            report.writes = report.writes.saturating_add(1);
            report.verifications = report.verifications.saturating_add(1);
            record_result(&transaction, live, "ok", "read-back matched");
            send(
                events,
                Event::TuneWritten {
                    op: transaction.op,
                    report: WriteReport {
                        pages: transaction.pages.len(),
                        cells: transaction.cells,
                        variables: transaction.variables,
                        verified: true,
                        snapshot: transaction.snapshot.clone(),
                        journal: transaction
                            .journal
                            .as_ref()
                            .map(|journal| journal.path().to_owned()),
                    },
                },
            );
        }
        TxnKind::Burn => {
            report.burns = report.burns.saturating_add(1);
            report.verifications = report.verifications.saturating_add(1);
            record_result(&transaction, live, "ok", "read-back matched");
            send(
                events,
                Event::TuneBurned {
                    op: transaction.op,
                    report: BurnReport {
                        pages: transaction.pages.len(),
                        verified: true,
                        snapshot: transaction.snapshot.clone(),
                        journal: transaction
                            .journal
                            .as_ref()
                            .map(|journal| journal.path().to_owned()),
                    },
                },
            );
        }
    }

    if let Some(project) = session.project() {
        send_status(events, transaction.op, project);
    }
    live.polled();
}

/// Applies the read-back of a verified write to the ECU image.
pub(super) fn apply_verified_write(
    live: &mut Live,
    transaction: &WriteTxn,
    session: &mut Session,
) -> Result<(), String> {
    let Some(project) = session.project_mut() else {
        return Err("the project closed during the write".to_owned());
    };
    let Some(mut image) = project.images().ecu_ram().cloned() else {
        return Err("the ECU image disappeared during the write".to_owned());
    };
    let layout = Arc::clone(project.layout());
    for (page, bytes) in &transaction.readback {
        let Some(ini_page) = IniPage::new(*page) else {
            continue;
        };
        decode_page(&layout, ini_page, bytes, &mut image)
            .map_err(|error| format!("the verified page {page} could not be decoded: {error}"))?;
    }
    project
        .images_mut()
        .set_ecu_ram(image)
        .map_err(|error| format!("the verified ECU image could not be stored: {error}"))?;
    project
        .images_mut()
        .clear_write_failures_for(&transaction.changed_variables);
    project
        .images_mut()
        .clear_resolutions_for(&transaction.changed_variables);
    // The read-back updates the ECU image, but a constant that shares bytes
    // with one that changed still holds whatever the working image said before
    // the write. The working image is what the user asked for and what the
    // write verified, so it is the source here rather than the read-back; only
    // the neighbours are re-derived from it.
    reconcile_groups(project, &transaction.changed_variables, "a verified write");
    live.images.page_bytes.extend(
        transaction
            .readback
            .iter()
            .map(|(page, bytes)| (*page, bytes.clone())),
    );
    Ok(())
}

/// Records that flash now holds the read-back RAM image.
pub(super) fn apply_verified_burn(
    transaction: &WriteTxn,
    session: &mut Session,
) -> Result<(), String> {
    let Some(project) = session.project_mut() else {
        return Err("the project closed during the burn".to_owned());
    };
    let burnable = project
        .definition()
        .pages
        .iter()
        .filter(|page| page.is_burnable())
        .count();
    if transaction.pages.len() < burnable {
        // Only some pages were burned; the flash image of the rest is still
        // whatever it was, so claiming the whole image is verified would be a
        // lie. The cached flash no longer describes the chip either.
        project.images_mut().mark_flash_unknown();
        return Ok(());
    }
    let Some(ram) = project.images().ecu_ram().cloned() else {
        return Err("the ECU image disappeared during the burn".to_owned());
    };
    project
        .images_mut()
        .set_ecu_flash(ram)
        .map_err(|error| format!("the verified flash image could not be stored: {error}"))?;
    Ok(())
}

/// Ends a transaction that failed (or could not be sent), halting writes.
#[expect(
    clippy::too_many_arguments,
    reason = "the handler mirrors the fields of one protocol request"
)]
#[expect(
    clippy::needless_pass_by_value,
    reason = "the failed transaction is finished and dropped by this call"
)]
pub(super) fn fail_transaction(
    live: &mut Live,
    transaction: WriteTxn,
    session: &mut Session,
    safety: &mut Safety,
    events: &EventTx,
    report: &mut CoreReport,
    reason: String,
    link_lost: bool,
) {
    let code = transaction.kind.failure_code();
    if transaction.kind == TxnKind::Write {
        if let Some(project) = session.project_mut() {
            if !transaction.readback.is_empty()
                && let Some(mut image) = project.images().ecu_ram().cloned()
            {
                let layout = Arc::clone(project.layout());
                for (page, bytes) in &transaction.readback {
                    let Some(ini_page) = IniPage::new(*page) else {
                        continue;
                    };
                    let _ = decode_page(&layout, ini_page, bytes, &mut image);
                }
                if let Err(error) = project.images_mut().set_ecu_ram(image) {
                    tracing::warn!(%error, "the failed write's read-back could not be stored");
                }
            }
            mark_failed_cells(project, &transaction.changed_variables);
        }
        if link_lost {
            session.mark_unknown();
        }
    }
    if transaction.kind == TxnKind::Burn
        && (transaction.burned > 0 || transaction.phase == TxnPhase::Burn)
        && let Some(project) = session.project_mut()
    {
        // At least one page may already be in flash, and a failing verify
        // proved some of it differs; nothing about the chip is known now.
        project.images_mut().mark_flash_unknown();
    }
    safety.halt(reason.clone());
    report.failures = report.failures.saturating_add(1);
    if !transaction.expected.is_empty() || !transaction.readback.is_empty() {
        report.verification_failures = report.verification_failures.saturating_add(1);
    }
    tracing::warn!(
        op = %transaction.op,
        operation = transaction.kind.event(),
        %reason,
        link_lost,
        "write transaction failed"
    );
    record_result(&transaction, live, "failed", &reason);
    send(
        events,
        Event::Failed {
            op: transaction.op,
            failure: Failure {
                code,
                message: reason,
            },
        },
    );
    live.polled();
}

/// Marks every cell of the changed variables as a failed write.
pub(super) fn mark_failed_cells(project: &mut Project, names: &[String]) {
    let mut cells: Vec<(String, usize, usize)> = Vec::new();
    for name in names {
        let Some(spec) = project.layout().variable(name) else {
            continue;
        };
        let (columns, rows) = spec.shape.dimensions().unwrap_or((1, 1));
        for row in 0..rows {
            for column in 0..columns {
                cells.push((name.clone(), row, column));
            }
        }
    }
    for (name, row, column) in cells {
        if let Err(error) = project.images_mut().mark_write_failed(&name, row, column) {
            tracing::warn!(%name, %error, "a failed cell could not be marked");
        }
    }
}

/// Records the result line for a transaction's journal, best effort.
pub(super) fn record_result(transaction: &WriteTxn, live: &Live, result: &str, detail: &str) {
    let Some(journal) = &transaction.journal else {
        return;
    };
    let entry = JournalEntry {
        at: journal::timestamp(),
        operation: transaction.kind.journal_name().to_owned(),
        phase: "result".to_owned(),
        port: live.connection.link_label.clone(),
        signature: live.connection.signature.clone(),
        pages: journal::pages_label(&transaction.pages),
        cells: transaction.cells,
        variables: transaction.variables,
        result: result.to_owned(),
        detail: detail.to_owned(),
    };
    if let Err(error) = journal.append(&entry) {
        tracing::warn!(%error, "the journal result could not be written");
    }
}

/// Records the result line for a controller command's journal, best effort.
pub(super) fn record_command_result(
    transaction: &CommandTxn,
    live: &Live,
    result: &str,
    detail: &str,
) {
    let entry = JournalEntry {
        at: journal::timestamp(),
        operation: "controller-command".to_owned(),
        phase: "result".to_owned(),
        port: live.connection.link_label.clone(),
        signature: live.connection.signature.clone(),
        pages: "all".to_owned(),
        cells: 0,
        variables: 0,
        result: result.to_owned(),
        detail: format!(
            "[ControllerCommands] {} from dialog {} ({detail})",
            transaction.name, transaction.dialog
        ),
    };
    if let Err(error) = transaction.journal.append(&entry) {
        tracing::warn!(%error, "the controller command result could not be written");
    }
}

/// Closes a controller command's journal after a failure.
pub(super) fn finish_command_failed(live: &mut Live, detail: &str) {
    let Some(transaction) = live.txns.command_tx.take() else {
        return;
    };
    record_command_result(&transaction, live, "failed", detail);
}

/// Ends a transaction before the wire saw it, without halting.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the refused transaction is finished and dropped by this call"
)]
pub(super) fn finish_refused(
    live: &mut Live,
    transaction: WriteTxn,
    events: &EventTx,
    report: &mut CoreReport,
    reason: &str,
) {
    report.failures = report.failures.saturating_add(1);
    record_result(&transaction, live, "refused", reason);
    send(
        events,
        Event::Failed {
            op: transaction.op,
            failure: Failure {
                code: FailureCode::WriteRefused,
                message: reason.to_owned(),
            },
        },
    );
    live.polled();
}
