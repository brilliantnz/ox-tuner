//! Firmware detection and definition lookup for the new-project wizard.
//!
//! The scan sends one probe at a time through the core and advances when the
//! port stays silent or answers with text that is not a firmware signature, so
//! the UI stays responsive and can show progress. A signature that resolves in
//! the registry is served from the cache; a miss waits for the user's consent
//! before the background download starts.

use std::cmp::Reverse;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui_kit::{App, AppContext as _, AsyncApp};
use oxtuner_core::{COMMON_PROBE_BAUD_RATES, FirmwareProbe, LinkMode, OpId, PortChoice, PortView};
use oxtuner_registry::{BlockingClient, CacheStore, ErrorKind, Registry, ResolvedDefinition};
use rust_i18n::t;

use super::status::{
    CoreStatus, DefinitionDownload, DetectedFirmware, DownloadProgress, FirmwareScan, ProbeAttempt,
};
use super::{CoreService, inspect_definition};
use crate::platform::paths::AppPaths;

/// How long one download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the dialog repaints while a definition downloads.
///
/// The fetch runs on a background executor, so nothing else tells the UI a
/// new byte count arrived; the tick stops with the download.
const PROGRESS_TICK: Duration = Duration::from_millis(100);

/// How long one attempt may stay unanswered before the scan moves on.
///
/// This bounds the UI, not the core: the core opens the port without a timeout
/// and joins a wedged probe thread, so no number here can bound an attempt
/// precisely. After this the attempt counts as a miss and the scan tries the
/// next one; a late answer is ignored by the op check.
const PROBE_DEADLINE: Duration = Duration::from_secs(10);

/// The host a download prompt names, for the consent text.
pub(crate) fn host_of(url: &str) -> String {
    url.split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or(url)
        .to_owned()
}

/// Starts a scan across the ports the core last published, newest appearance first.
///
/// The core publishes its list sorted by name, so the order says nothing about
/// when a device was plugged in. This keeps the appearance order the port
/// events carry and sorts it newest first: the ECU the user just connected is
/// usually the newest port, and meeting it first is what keeps a scan short on
/// a machine with several serial adapters. The first sample carries no
/// appearance history yet, so its order is the published name order and the
/// scan walks it from the last name to the first. The list is the core's
/// pushed snapshot; when no sample arrived yet the scan waits for it instead of
/// reporting a portless machine. The request hurries the baseline along, and
/// the next Detect press scans it.
pub(crate) fn start_scan(cx: &mut App) {
    // A scan already owns the core's probe path; a second one would drop the
    // op of the attempt in flight and leave its answer unattributed.
    if cx.global::<CoreService>().status.firmware_scan.is_some() {
        return;
    }

    let Some(published) = crate::core::ports(cx) else {
        crate::core::refresh_ports(cx);
        let status = &mut cx.global_mut::<CoreService>().status;
        clear_detection(status);
        status.scan_after_ports = true;
        status.firmware_error = Some(t!("app.new_project.scan_ports_pending").to_string());
        cx.refresh_windows();
        return;
    };
    let ports = scan_order(published, &cx.global::<CoreService>().status.port_order);

    let status = &mut cx.global_mut::<CoreService>().status;
    clear_detection(status);
    status.scan_after_ports = false;
    status.firmware_error = None;

    if ports.is_empty() {
        status.firmware_error = Some(t!("app.new_project.scan_no_ports").to_string());
        cx.refresh_windows();
        return;
    }

    let attempts = attempts_for(&ports);
    let total = attempts.len();
    status.firmware_scan = Some(FirmwareScan {
        attempts,
        next: 0,
        op: None,
    });
    tracing::info!(
        ports = ports.len(),
        attempts = total,
        "firmware scan started"
    );

    send_current(cx);
    cx.refresh_windows();
}

/// The order a scan walks the ports in: newest appearance first, then name.
///
/// The appearance order itself is kept by [`CoreStatus::note_ports`]. A port
/// the order does not know sorts as the oldest, which is only reachable when a
/// list arrived without passing through that method.
fn scan_order(ports: &[PortView], appeared: &[String]) -> Vec<String> {
    let mut names: Vec<&str> = ports.iter().map(|port| port.name.as_str()).collect();
    names.sort_by_cached_key(|name| (Reverse(rank(appeared, name)), *name));

    names.into_iter().map(str::to_owned).collect()
}

/// How recently a port appeared: a later index is newer.
fn rank(appeared: &[String], name: &str) -> Option<usize> {
    appeared.iter().position(|seen| seen == name)
}

/// Clears the answers a scan produced, so starting over never shows them.
///
/// Both branches of [`start_scan`] run this: a new scan is a new attempt,
/// whether the last one found an ECU, found nothing, or never started. An
/// in-flight download is dropped with it: its progress handle is what decides
/// whether the answer still belongs to a live detection.
fn clear_detection(status: &mut CoreStatus) {
    status.firmware_scan = None;
    status.detected_firmware = None;
    status.download_declined = None;
    status.clear_download();
}

/// The framings a scan tries, in order.
///
/// Both shipped definitions declare `msEnvelope_1.0`, so the modern framing is
/// asked first and the legacy query second. A family that answers only the
/// plain query costs one extra settle and timeout per port and rate, never a
/// miss.
const PROBE_FRAMINGS: [LinkMode; 2] = [LinkMode::Envelope, LinkMode::Legacy];

/// The attempts a scan makes, in order: the ports as given, then common baud,
/// then family, then framing.
///
/// The caller passes the ports already in scan order, so this stays a pure
/// expansion and the ordering rule lives in one place.
fn attempts_for(ports: &[String]) -> Vec<ProbeAttempt> {
    let mut attempts = Vec::with_capacity(
        ports.len()
            * COMMON_PROBE_BAUD_RATES.len()
            * FirmwareProbe::ALL.len()
            * PROBE_FRAMINGS.len(),
    );

    for port in ports {
        for baud in COMMON_PROBE_BAUD_RATES {
            for probe in FirmwareProbe::ALL {
                for framing in PROBE_FRAMINGS {
                    attempts.push(ProbeAttempt {
                        port: port.clone(),
                        baud,
                        probe,
                        framing,
                    });
                }
            }
        }
    }

    attempts
}

/// Sends the attempt at `scan.next`.
fn send_current(cx: &mut App) {
    let Some(attempt) = ({
        let status = &cx.global::<CoreService>().status;
        status
            .firmware_scan
            .as_ref()
            .and_then(|scan| scan.attempts.get(scan.next).cloned())
    }) else {
        // The scan has no attempt at this index; leaving it set would disable
        // Detect with no timer armed to advance it.
        fail_scan(cx, t!("app.new_project.scan_no_attempt").as_ref());
        return;
    };

    let Some(handle) = CoreService::handle(cx) else {
        fail_scan(cx, t!("app.new_project.scan_core_stopped").as_ref());
        return;
    };

    match handle.probe_firmware(
        PortChoice::serial(attempt.port.clone(), attempt.baud),
        attempt.framing,
        attempt.probe,
    ) {
        Ok(op) => {
            if let Some(scan) = cx.global_mut::<CoreService>().status.firmware_scan.as_mut() {
                scan.op = Some(op);
            }

            // An answer that never arrives must not leave the step scanning
            // forever. The deadline calls the same miss path, so a real answer
            // that already advanced the scan makes it a no-op.
            cx.spawn(async move |cx| {
                cx.background_executor().timer(PROBE_DEADLINE).await;
                cx.update(|cx| {
                    crate::core::firmware::missed(op, Miss::Expired, cx);
                });
            })
            .detach();

            tracing::debug!(
                port = %attempt.port,
                baud = attempt.baud,
                probe = ?attempt.probe,
                framing = ?attempt.framing,
                %op,
                "firmware probe sent"
            );
        }
        Err(error) => {
            tracing::warn!(%error, "the core refused the firmware probe");
            fail_scan(
                cx,
                t!(
                    "app.new_project.scan_probe_refused",
                    message = error.to_string()
                )
                .as_ref(),
            );
        }
    }
}

/// Ends the scan with a visible reason.
pub(crate) fn fail_scan(cx: &mut App, message: &str) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.firmware_scan = None;
    status.scan_after_ports = false;
    status.firmware_error = Some(message.to_owned());
    cx.refresh_windows();
}

/// Stops a scan the user walked away from.
///
/// A probe already in flight still answers, but its miss is dropped: nothing is
/// left to advance, and a probe miss is never a notification. A Detect press
/// still waiting for the first port sample is cancelled with it, so closing the
/// dialog cannot start a scan nobody is watching.
pub(crate) fn cancel_scan(cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.scan_after_ports = false;

    if status.firmware_scan.take().is_some() {
        tracing::debug!("the firmware scan was cancelled");
    }

    cx.refresh_windows();
}

/// Clears every detection answer the wizard rendered.
///
/// The dialog owns no detection state itself: the scan, the signature, the
/// download prompt and inspected preview, and the last connection test all
/// live in the core status, so closing or reopening the wizard must wipe them
/// or the next open shows the previous session. The inspect serial is reset
/// to the failure serial with them: with no definition pending, no stored
/// failure may read as this session's answer. An in-flight download still
/// finishes, but without a stored download it cannot attach to a detection
/// and its answer is dropped.
pub(crate) fn reset_detection(cx: &mut App) {
    cancel_scan(cx);
    let status = &mut cx.global_mut::<CoreService>().status;
    status.detected_firmware = None;
    status.firmware_error = None;
    status.inspected = None;
    status.clear_download();
    status.test = None;
    status.testing = None;
    status.inspect_serial = status.failure_serial;
    status.inspect_pending = None;
    cx.refresh_windows();
}

/// Handles a probe that answered with a signature.
///
/// The core reports any answer long enough to be a signature, so the registry
/// decides what the text is before the scan acts on it. A served definition is
/// the detection, a family no source serves stops the scan for a manual INI,
/// and anything the registry does not recognise is a miss that leaves the scan
/// looking.
pub(crate) fn probed(
    op: OpId,
    port: String,
    baud: Option<u32>,
    framing: LinkMode,
    probe: FirmwareProbe,
    signature: &str,
    cx: &mut App,
) {
    let in_flight = cx
        .global::<CoreService>()
        .status
        .firmware_scan
        .as_ref()
        .is_some_and(|scan| scan.op == Some(op));
    if !in_flight {
        return;
    }

    match classify(signature) {
        Verdict::Noise => {
            // Nothing was detected, so the scan is still the one owning this
            // op: its next attempt is the answer to this sentence.
            missed(op, Miss::Noise, cx);
        }
        Verdict::Unserved(reason) => {
            // Real firmware nobody serves a definition for. Scanning on would
            // replace a known answer with a worse one, so the detection stays
            // and the user supplies the INI by hand.
            let status = &mut cx.global_mut::<CoreService>().status;
            status.firmware_scan = None;
            status.detected_firmware = Some(detected(port, baud, framing, signature));
            status.firmware_error = Some(unsupported_message(signature));

            tracing::info!(baud, ?framing, %signature, %reason, "firmware detected without a definition source");
            cx.refresh_windows();
        }
        Verdict::Served(resolved) => {
            let status = &mut cx.global_mut::<CoreService>().status;
            status.firmware_scan = None;
            status.firmware_error = None;
            status.detected_firmware = Some(detected(port, baud, framing, signature));

            tracing::info!(baud, ?framing, %signature, ?probe, "firmware detected");
            serve(resolved, signature, cx);
            cx.refresh_windows();
        }
    }
}

/// The detection one probe answer describes, before its definition is found.
fn detected(
    port: String,
    baud: Option<u32>,
    framing: LinkMode,
    signature: &str,
) -> DetectedFirmware {
    DetectedFirmware {
        port,
        baud: baud.unwrap_or(0),
        framing,
        signature: signature.to_owned(),
        definition: None,
    }
}

/// What the wizard says about firmware no source serves.
fn unsupported_message(signature: &str) -> String {
    t!(
        "app.new_project.scan_unsupported",
        signature = signature.to_owned()
    )
    .to_string()
}

/// What the registry makes of a probed signature.
#[derive(Debug)]
enum Verdict {
    /// The registry serves a definition for this ECU.
    Served(ResolvedDefinition),
    /// The text names a firmware family no source serves yet.
    Unserved(String),
    /// The text is not a firmware signature at all.
    Noise,
}

/// Asks the registry what a probe answer is.
///
/// The core cannot tell an ECU from a chatty adapter, an echoed query or an
/// NMEA sentence: any text long enough to be a signature is reported as one.
/// This is the lookup that tells them apart, and it is pure, so the scan can
/// ask it before it acts on an answer.
fn classify(signature: &str) -> Verdict {
    match Registry::with_defaults().resolve(signature) {
        Ok(resolved) => Verdict::Served(resolved),
        Err(error) if error.kind() == ErrorKind::Unsupported => {
            Verdict::Unserved(error.to_string())
        }
        Err(error) => {
            // `unsupported` is the only verdict that says the firmware is real
            // but has no source. An invalid signature names no firmware, and
            // any other failure leaves the answer unexplained, so the scan
            // keeps looking either way.
            tracing::debug!(%signature, reason = %error, "the probe answer did not resolve");
            Verdict::Noise
        }
    }
}

/// Why an attempt produced no usable detection.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Miss<'a> {
    /// The port stayed silent.
    Silent,
    /// Something answered, but the text was not a firmware signature.
    Noise,
    /// The core refused to run the probe; the message says why.
    Refused(&'a str),
    /// No answer arrived before the deadline.
    Expired,
    /// The core stopped before answering.
    Stopped,
}

/// Handles a probe that did not detect firmware.
///
/// A miss that belongs to no scan (cancelled, or a stale op) is still not a
/// visible failure: a scan step is never news on its own. Any failure that
/// answers the attempt in flight counts as a miss, so a refusal such as "a
/// session is connected" cannot leave the scan spinning on the same step.
///
/// Returns whether the failure belonged to the scan.
pub(crate) fn missed(op: OpId, miss: Miss<'_>, cx: &mut App) -> bool {
    let status = &mut cx.global_mut::<CoreService>().status;
    let Some(scan) = status.firmware_scan.as_mut() else {
        return false;
    };
    if scan.op != Some(op) {
        return false;
    }

    scan.next += 1;
    if scan.next < scan.attempts.len() {
        send_current(cx);
        cx.refresh_windows();
        return true;
    }

    let attempts = scan.attempts.len();
    status.firmware_scan = None;
    status.firmware_error = Some(miss_message(miss));
    tracing::info!(attempts, "the firmware scan found no ECU");
    cx.refresh_windows();

    true
}

/// The sentence a scan that found no ECU ends with.
///
/// A refusal keeps the core's own reason; everything else is the UI's own
/// wording for what happened.
fn miss_message(miss: Miss<'_>) -> String {
    match miss {
        Miss::Refused(reason) => reason.to_owned(),
        Miss::Silent => t!("app.new_project.scan_no_answer").to_string(),
        Miss::Noise => t!("app.new_project.scan_not_ecu").to_string(),
        Miss::Expired => t!("app.new_project.scan_timed_out").to_string(),
        Miss::Stopped => t!("app.new_project.scan_core_stopped").to_string(),
    }
}

/// Re-offers the definition for a signature the scan already detected.
///
/// Used when the user returns to Auto: the detection is on screen, but the
/// download prompt was dropped while the manual tab was active. An unserved
/// detection comes back as the same upload hint the scan showed.
fn resolve(signature: &str, cx: &mut App) {
    match classify(signature) {
        Verdict::Served(resolved) => serve(resolved, signature, cx),
        Verdict::Unserved(reason) => {
            // The detection outlives the scan, so the round trip through the
            // Manual tab must land on the same instruction the scan showed.
            tracing::warn!(signature, reason, "no definition source for the signature");
            fail_detection(unsupported_message(signature), cx);
        }
        Verdict::Noise => {
            // Nothing set a detection without classifying it first, so this is
            // only reachable when the registry changed underneath one. Drop the
            // detection with the message, or the step would show an ECU and
            // "not an ECU" at the same time.
            cx.global_mut::<CoreService>().status.detected_firmware = None;
            fail_detection(t!("app.new_project.scan_not_ecu").to_string(), cx);
        }
    }
}

/// Ends detection with a visible reason.
fn fail_detection(message: String, cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.firmware_error = Some(message);
    cx.refresh_windows();
}

/// Serves a resolved signature from the cache, or offers to download it.
///
/// The cache lookup hashes the definition, which can be a megabyte, so it runs
/// on the background executor and the answer is applied only while the same
/// detection is still on screen.
fn serve(resolved: ResolvedDefinition, signature: &str, cx: &mut App) {
    let directory = AppPaths::get(cx).registry_dir();
    let store = CacheStore::new(&directory);
    let path = store.definition_ini(&resolved.id);
    let verified = path.clone();
    let work =
        cx.background_spawn(async move { CacheStore::new(&directory).verify_file(&verified) });

    let signature = signature.to_owned();
    cx.spawn(async move |cx| {
        let cached = work.await;

        cx.update(|cx| {
            let current = cx
                .global::<CoreService>()
                .status
                .detected_firmware
                .as_ref()
                .is_some_and(|detected| detected.signature == signature);
            if !current {
                // A new scan or a manual pick replaced this detection while
                // the hash ran.
                return;
            }

            if cached {
                tracing::debug!(
                    signature,
                    path = %path.display(),
                    "the definition is cached"
                );
                finish(&path, cx);
                return;
            }

            cx.global_mut::<CoreService>().status.definition_download = Some(DefinitionDownload {
                signature,
                url: resolved.ini_url,
                id: resolved.id,
            });
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Starts the consented download on the background executor.
///
/// The fetch reports byte counts into shared progress state while a ticker
/// repaints the dialog; the completion applies only if no reset or newer
/// download replaced the progress handle meanwhile.
pub(crate) fn download(cx: &mut App) {
    let Some(prompt) = cx
        .global_mut::<CoreService>()
        .status
        .definition_download
        .take()
    else {
        return;
    };

    let progress = Arc::new(DownloadProgress::default());
    {
        let status = &mut cx.global_mut::<CoreService>().status;
        status.downloading_definition = true;
        status.download_progress = Some(Arc::clone(&progress));
        status.firmware_error = None;
    }

    let directory = AppPaths::get(cx).registry_dir();
    tracing::info!(
        signature = %prompt.signature,
        url = %prompt.url,
        "definition download started"
    );

    let fetch_progress = Arc::clone(&progress);
    let work = cx.background_spawn(async move {
        let client = BlockingClient::new(DOWNLOAD_TIMEOUT)?;
        let store = CacheStore::new(&directory);
        let registry = Registry::with_defaults();

        registry.ensure_definition(&client, &store, &prompt.id, &mut |done, total| {
            // The fetch runs on a background thread: only atomics cross over,
            // the UI reads them back on its own tick.
            fetch_progress.done.store(done, Ordering::Relaxed);
            fetch_progress
                .total
                .store(total.unwrap_or(0), Ordering::Relaxed);
        })
    });

    let tick_progress = Arc::clone(&progress);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(PROGRESS_TICK).await;
            // The ticker follows the same handle the completion does: a
            // second download sets its own progress, and the old ticker must
            // not repaint for it.
            let running = cx.update(|cx| {
                let status = &cx.global::<CoreService>().status;
                let running = status.downloading_definition
                    && status
                        .download_progress
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &tick_progress));
                if running {
                    cx.refresh_windows();
                }
                running
            });
            if !running {
                break;
            }
        }
    })
    .detach();

    cx.spawn(async move |cx: &mut AsyncApp| {
        let result = work.await;
        cx.update(|cx| {
            let current = cx.global::<CoreService>().status.download_progress.clone();

            // A reset or a newer download replaced the handle: this answer
            // belongs to a session that is already gone, so drop it.
            let fresh = match current {
                Some(current) => Arc::ptr_eq(&current, &progress),
                None => false,
            };
            if !fresh {
                return;
            }

            match result {
                Ok(path) => {
                    cx.global_mut::<CoreService>().status.clear_download();
                    finish(&path, cx);
                }
                Err(error) => {
                    let status = &mut cx.global_mut::<CoreService>().status;
                    status.clear_download();
                    status.firmware_error = Some(
                        t!(
                            "app.new_project.download_failed",
                            message = error.to_string()
                        )
                        .to_string(),
                    );
                    cx.refresh_windows();
                }
            }
        });
    })
    .detach();

    cx.refresh_windows();
}

/// Points the wizard back at the detected definition.
///
/// The manual pick replaces the inspected file, so switching back to Auto has
/// to inspect the detected one again or the step would show a preview that
/// belongs to the other source. Nothing happens when the current preview
/// already is the detected file, or when detection has not resolved one yet.
pub(crate) fn inspect_detected(cx: &mut App) {
    let (path, signature, may_offer) = {
        let status = &cx.global::<CoreService>().status;
        let Some(detected) = status.detected_firmware.as_ref() else {
            return;
        };

        (
            detected.definition.clone(),
            detected.signature.clone(),
            status.definition_download.is_none()
                && !status.downloading_definition
                && status.download_declined.as_deref() != Some(detected.signature.as_str()),
        )
    };

    let Some(path) = path else {
        // Leaving Auto cleared the prompt; returning must offer it again
        // instead of leaving the step with nothing to press.
        if may_offer {
            resolve(&signature, cx);
        }

        return;
    };

    let already_known = {
        let status = &cx.global::<CoreService>().status;

        status
            .inspected
            .as_ref()
            .is_some_and(|inspected| inspected.path == path)
            || status.inspect_pending.as_deref() == Some(path.as_path())
    };
    if already_known {
        return;
    }

    note_inspect_sent(&path, cx);
    inspect_definition(&path, cx);
}

/// Drops the prompt because the user switched source, not because they said no.
///
/// Looking at the Manual tab must not count as declining: returning to Auto
/// still offers the download the detection found.
pub(crate) fn drop_download(cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.clear_download();
    cx.refresh_windows();
}

/// Drops a download the user declined.
pub(crate) fn skip_download(cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    if let Some(prompt) = status.definition_download.take() {
        // A declined prompt stays declined while the same detection is on
        // screen; leaving Auto and coming back must not ask again.
        status.download_declined = Some(prompt.signature);
    }
    status.clear_download();
    cx.refresh_windows();
}

/// Hands the cached definition to the core and the wizard.
///
/// The serial moves with the send: an inspect failure that arrives after this
/// point belongs to this definition, and the render compares against it.
fn finish(path: &Path, cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    if let Some(detected) = status.detected_firmware.as_mut() {
        detected.definition = Some(path.to_path_buf());
    }
    status.firmware_error = None;
    status.inspect_serial = status.failure_serial;
    status.inspect_pending = Some(path.to_path_buf());

    inspect_definition(path, cx);
    cx.refresh_windows();
}

/// Records that `path` was just sent for inspect.
///
/// The serial moves with the send: an inspect failure that arrives after this
/// point belongs to this definition, and the dialog compares against it. The
/// path is recorded too, because the last *answer* may describe another file
/// while this one is still in flight.
pub(crate) fn note_inspect_sent(path: &Path, cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.inspect_serial = status.failure_serial;
    status.inspect_pending = Some(path.to_path_buf());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_tries_every_baud_and_family_per_port() {
        let attempts = attempts_for(&["first".to_owned(), "second".to_owned()]);

        assert_eq!(
            attempts.len(),
            2 * COMMON_PROBE_BAUD_RATES.len() * FirmwareProbe::ALL.len() * 2
        );
        assert_eq!(
            attempts[0],
            ProbeAttempt {
                port: "first".to_owned(),
                baud: 115_200,
                probe: FirmwareProbe::Speeduino,
                framing: LinkMode::Envelope,
            }
        );
        assert_eq!(attempts[1].framing, LinkMode::Legacy);
        assert_eq!(attempts[2].probe, FirmwareProbe::Rusefi);
        assert_eq!(attempts[2].framing, LinkMode::Envelope);
        assert_eq!(attempts[4].baud, COMMON_PROBE_BAUD_RATES[1]);
        assert_eq!(attempts.last().expect("attempts").port, "second");
    }

    /// A published port row, with the label the UI shows.
    fn port(name: &str) -> PortView {
        PortView {
            name: name.to_owned(),
            label: name.to_owned(),
            vid: None,
            pid: None,
            serial: None,
        }
    }

    #[test]
    fn the_scan_starts_with_the_newest_port_not_the_last_name() {
        // Neither ascending nor descending: only the appearance order produces
        // this sequence, so a name sort cannot pass in its place.
        let ports = [
            port("/dev/cu.aaa"),
            port("/dev/cu.mmm"),
            port("/dev/cu.zzz"),
        ];
        let appeared = ["/dev/cu.zzz", "/dev/cu.aaa", "/dev/cu.mmm"].map(str::to_owned);

        assert_eq!(
            scan_order(&ports, &appeared),
            ["/dev/cu.mmm", "/dev/cu.aaa", "/dev/cu.zzz"]
        );
    }

    #[test]
    fn a_port_the_order_does_not_know_is_scanned_after_a_known_one() {
        let ports = [port("/dev/cu.aaa"), port("/dev/cu.zzz")];

        assert_eq!(
            scan_order(&ports, &["/dev/cu.zzz".to_owned()]),
            ["/dev/cu.zzz", "/dev/cu.aaa"]
        );
        assert_eq!(
            scan_order(&ports, &[]),
            ["/dev/cu.aaa", "/dev/cu.zzz"],
            "with no appearance at all the name keeps the order stable"
        );
    }

    #[test]
    fn the_published_lists_decide_the_first_attempt() {
        // The seam a Detect press runs: record the samples, then expand the
        // ordered ports. The port that appeared between two samples is asked
        // first, even though it is not the last name or the first one.
        let ports = [
            port("/dev/cu.aaa"),
            port("/dev/cu.mmm"),
            port("/dev/cu.zzz"),
        ];
        let mut status = CoreStatus::default();
        status.note_ports(&[port("/dev/cu.aaa"), port("/dev/cu.zzz")]);
        status.note_ports(&ports);

        let attempts = attempts_for(&scan_order(&ports, &status.port_order));

        assert_eq!(
            attempts[0].port, "/dev/cu.mmm",
            "the just-plugged port is tried first"
        );
        assert_eq!(attempts[0].baud, COMMON_PROBE_BAUD_RATES[0]);
        assert_eq!(attempts.last().expect("attempts").port, "/dev/cu.aaa");
    }

    #[test]
    fn a_lost_attempt_keeps_its_own_verdict() {
        assert_eq!(
            miss_message(Miss::Silent),
            t!("app.new_project.scan_no_answer").to_string()
        );
        assert_eq!(
            miss_message(Miss::Noise),
            t!("app.new_project.scan_not_ecu").to_string()
        );
        assert_eq!(
            miss_message(Miss::Expired),
            t!("app.new_project.scan_timed_out").to_string()
        );
        assert_eq!(
            miss_message(Miss::Stopped),
            t!("app.new_project.scan_core_stopped").to_string()
        );
        assert_eq!(miss_message(Miss::Refused("port busy")), "port busy");
    }

    #[test]
    fn a_host_is_read_from_the_download_url() {
        assert_eq!(
            host_of("https://speeduino.com/fw/202501.7.ini"),
            "speeduino.com"
        );
        assert_eq!(host_of("not a url"), "not a url");
    }

    /// An NMEA sentence: long enough for the core's signature floor, and not
    /// firmware text.
    const NMEA: &str = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";

    /// A signature the registry models but no source serves yet.
    const FOME: &str = "rusEFI (FOME) .2026.06.03.proteus_h7.3416487136";

    #[test]
    fn a_probe_answer_is_served_unsupported_or_noise() {
        assert!(matches!(classify(NMEA), Verdict::Noise));
        assert!(matches!(
            classify("garbage from a chatty adapter"),
            Verdict::Noise
        ));

        let Verdict::Served(resolved) = classify("speeduino 202501.7") else {
            panic!("a shipped definition is served");
        };
        assert_eq!(resolved.id.family(), oxtuner_registry::Family::Speeduino);

        let Verdict::Unserved(_) = classify(FOME) else {
            panic!("a modeled family without a source is unsupported");
        };
    }

    /// Installs a core for the scan to send through, and the paths it reads.
    fn start_core(cx: &mut gpui_kit::TestAppContext) -> oxtuner_core::CoreThread {
        // A counter as well as the pid: plain `cargo test` runs every test in
        // one process, and a future test that seeds a cache file here must not
        // read what another one wrote.
        static NEXT_ROOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "oxtuner-probe-scan-{}-{serial}",
            std::process::id()
        ));

        cx.update(|cx| {
            cx.set_global(crate::platform::paths::AppPaths {
                config_dir: root.join("config"),
                cache_dir: root.join("cache"),
                logs_dir: root.join("logs"),
                projects_dir: root.join("OxTunerProjects"),
            });
            CoreService::install_with_core(cx)
        })
    }

    #[gpui_kit::test]
    fn a_noisy_port_does_not_end_the_scan(cx: &mut gpui_kit::TestAppContext) {
        let core = start_core(cx);

        // Send the first attempt the way Detect does, so the scan owns a real
        // op id to answer. The scan walks this list in order, so the noisy
        // port comes first and the ECU port is what it must still reach.
        cx.update(|cx| {
            cx.global_mut::<CoreService>().status.firmware_scan = Some(FirmwareScan {
                attempts: attempts_for(&["/dev/noise".to_owned(), "/dev/ecu".to_owned()]),
                next: 0,
                op: None,
            });
            send_current(cx);
        });

        let noisy_attempts = cx.update(|cx| {
            cx.global::<CoreService>()
                .status
                .firmware_scan
                .as_ref()
                .expect("the scan runs")
                .attempts
                .iter()
                .filter(|attempt| attempt.port == "/dev/noise")
                .count()
        });
        assert!(noisy_attempts > 0, "the noisy port has attempts to answer");

        // Port 1 answers every attempt with an NMEA sentence. The core reports
        // any long enough answer as a signature, so the scan is what must
        // reject it and work through the rest of the port's attempts.
        let mut noise_answers = 0;
        loop {
            let attempt = cx.update(|cx| {
                let scan = cx
                    .global::<CoreService>()
                    .status
                    .firmware_scan
                    .as_ref()
                    .expect("the scan runs");
                let attempt = scan
                    .attempts
                    .get(scan.next)
                    .cloned()
                    .expect("the scan never left the noisy port");
                (attempt, scan.op.expect("an attempt is in flight"))
            });

            let (attempt, op) = attempt;
            if attempt.port != "/dev/noise" {
                break;
            }

            cx.update(|cx| {
                probed(
                    op,
                    attempt.port.clone(),
                    Some(attempt.baud),
                    attempt.framing,
                    attempt.probe,
                    NMEA,
                    cx,
                );
            });

            noise_answers += 1;
            assert!(
                noise_answers <= noisy_attempts,
                "the scan never left the noisy port"
            );
        }

        let (ecu, ecu_op) = cx.update(|cx| {
            let status = &cx.global::<CoreService>().status;
            assert!(
                status.detected_firmware.is_none(),
                "noise is not a detection"
            );
            assert!(status.firmware_error.is_none(), "a miss is not an error");

            let scan = status.firmware_scan.as_ref().expect("the scan continues");
            let attempt = scan
                .attempts
                .get(scan.next)
                .cloned()
                .expect("the scan continues past the noisy port");

            (attempt, scan.op.expect("the second port is being probed"))
        });
        assert_eq!(
            noise_answers, noisy_attempts,
            "every attempt on the noisy port ran"
        );
        assert_eq!(ecu.port, "/dev/ecu");

        // Port 2 answers with a Speeduino signature: this is ECU #2.
        cx.update(|cx| {
            probed(
                ecu_op,
                ecu.port.clone(),
                Some(ecu.baud),
                ecu.framing,
                ecu.probe,
                "speeduino 202501.7",
                cx,
            );
        });

        cx.update(|cx| {
            let status = &cx.global::<CoreService>().status;
            assert!(status.firmware_scan.is_none(), "the scan found its ECU");
            assert!(status.firmware_error.is_none());

            let detected = status.detected_firmware.as_ref().expect("ECU #2");
            assert_eq!(detected.port, "/dev/ecu");
            assert_eq!(detected.signature, "speeduino 202501.7");
        });

        core.join();
    }

    #[gpui_kit::test]
    fn a_scan_that_only_heard_noise_ends_not_an_ecu(cx: &mut gpui_kit::TestAppContext) {
        let core = start_core(cx);

        // One attempt on one port, so answering it with noise exhausts the scan
        // and runs the terminal path the advance test never reaches.
        let op = cx.update(|cx| {
            let mut attempts = attempts_for(&["/dev/noise".to_owned()]);
            attempts.truncate(1);

            cx.global_mut::<CoreService>().status.firmware_scan = Some(FirmwareScan {
                attempts,
                next: 0,
                op: None,
            });
            send_current(cx);

            cx.global::<CoreService>()
                .status
                .firmware_scan
                .as_ref()
                .and_then(|scan| scan.op)
                .expect("the only attempt was sent")
        });

        cx.update(|cx| {
            probed(
                op,
                "/dev/noise".to_owned(),
                Some(115_200),
                LinkMode::Envelope,
                FirmwareProbe::Speeduino,
                NMEA,
                cx,
            );
        });

        cx.update(|cx| {
            let status = &cx.global::<CoreService>().status;
            assert!(
                status.firmware_scan.is_none(),
                "the scan has nowhere left to go"
            );
            assert!(
                status.detected_firmware.is_none(),
                "noise is not a detection"
            );
            assert_eq!(
                status.firmware_error.clone(),
                Some(t!("app.new_project.scan_not_ecu").to_string())
            );
        });

        core.join();
    }

    #[gpui_kit::test]
    fn an_unserved_ecu_stops_the_scan_and_asks_for_the_ini(cx: &mut gpui_kit::TestAppContext) {
        let core = start_core(cx);
        let signature = FOME;

        // Answer the first attempt the scan sends, so the verdict runs through
        // the path a real probe takes, not a hand-built status.
        let op = cx.update(|cx| {
            cx.global_mut::<CoreService>().status.firmware_scan = Some(FirmwareScan {
                attempts: attempts_for(&["/dev/ecu".to_owned()]),
                next: 0,
                op: None,
            });
            send_current(cx);

            cx.global::<CoreService>()
                .status
                .firmware_scan
                .as_ref()
                .and_then(|scan| scan.op)
                .expect("the first attempt was sent")
        });

        cx.update(|cx| {
            probed(
                op,
                "/dev/ecu".to_owned(),
                Some(115_200),
                LinkMode::Envelope,
                FirmwareProbe::Rusefi,
                signature,
                cx,
            );
        });

        cx.update(|cx| {
            let status = &cx.global::<CoreService>().status;
            assert!(status.firmware_scan.is_none(), "a real ECU ends the scan");
            assert_eq!(
                status.firmware_error.clone(),
                Some(unsupported_message(signature))
            );

            let detected = status
                .detected_firmware
                .as_ref()
                .expect("the ECU is remembered");
            assert_eq!(detected.port, "/dev/ecu");
            assert_eq!(detected.signature, signature);
            assert!(
                detected.definition.is_none(),
                "no definition exists for this firmware"
            );
            assert!(
                status.definition_download.is_none(),
                "the registry has nothing to offer"
            );
        });

        core.join();
    }

    #[gpui_kit::test]
    fn an_unserved_detection_keeps_the_upload_hint(cx: &mut gpui_kit::TestAppContext) {
        let signature = FOME;

        cx.update(|cx| {
            CoreService::install_without_core(cx);

            cx.global_mut::<CoreService>().status.detected_firmware = Some(DetectedFirmware {
                port: "/dev/ecu".to_owned(),
                baud: 115_200,
                framing: LinkMode::Envelope,
                signature: signature.to_owned(),
                definition: None,
            });

            // `inspect_detected` is what returning to Auto runs: the detection
            // never had a definition, so re-offering it must land on the same
            // upload hint rather than the registry's internal text.
            inspect_detected(cx);
        });

        cx.update(|cx| {
            assert_eq!(
                cx.global::<CoreService>().status.firmware_error.clone(),
                Some(
                    t!(
                        "app.new_project.scan_unsupported",
                        signature = signature.to_owned()
                    )
                    .to_string()
                )
            );
        });
    }
}
