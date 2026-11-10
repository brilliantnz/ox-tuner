#![allow(
    clippy::wildcard_imports,
    reason = "the service modules are one implementation split across files"
)]

use super::dispatch::{refuse, send};
use super::*;

/// Starts a CSV session log.
pub(super) fn start_log(
    op: OpId,
    live: &mut Option<Live>,
    events: &EventTx,
    report: &mut CoreReport,
    directory: &std::path::Path,
    log: &LogStatus,
) -> Outcome {
    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "connect to the ECU before logging",
        );
    };
    if connection.logging.logger.is_some() {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "a session log is already running",
        );
    }
    if !connection.connection.connected {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "the link is lost; reconnect before logging",
        );
    }

    let columns = connection
        .telemetry
        .decoder
        .columns()
        .into_iter()
        .map(|(name, units)| LogColumn::new(name, units))
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "the definition declares no realtime channel to log",
        );
    }
    let column_names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let path = directory.join(format!("oxtuner-{stamp}.csv"));
    match Logger::start(&path, columns, log.clone()) {
        Ok(logger) => {
            tracing::info!(%op, path = %logger.path().display(), "session log started");
            connection.logging.log_columns = column_names;
            connection.logging.logger = Some(logger);
            send(
                events,
                Event::LogState {
                    op,
                    running: true,
                    report: None,
                },
            );
        }
        Err(error) => {
            report.failures += 1;
            send(
                events,
                Event::Failed {
                    op,
                    failure: Failure {
                        code: FailureCode::LogFailed,
                        message: format!("{} could not be created: {error}", path.display()),
                    },
                },
            );
        }
    }
    Outcome::Continue
}

/// Stops the session log and flushes it.
pub(super) fn stop_log(
    op: OpId,
    live: &mut Option<Live>,
    events: &EventTx,
    report: &mut CoreReport,
) -> Outcome {
    let Some(connection) = live.as_mut() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "there is no session to stop logging",
        );
    };
    let Some(logger) = connection.logging.logger.take() else {
        return refuse(
            events,
            report,
            op,
            FailureCode::LogFailed,
            "no session log is running",
        );
    };
    match logger.stop() {
        Ok(summary) => {
            tracing::info!(
                %op,
                path = %summary.path.display(),
                frames = summary.frames,
                dropped = summary.dropped,
                "session log stopped"
            );
            send(
                events,
                Event::LogState {
                    op,
                    running: false,
                    report: Some(summary),
                },
            );
        }
        Err(error) => {
            report.failures += 1;
            send(
                events,
                Event::Failed {
                    op,
                    failure: Failure {
                        code: FailureCode::LogFailed,
                        message: error.to_string(),
                    },
                },
            );
        }
    }
    Outcome::Continue
}
