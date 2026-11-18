//! Firmware detection and definition lookup for the new-project wizard.
//!
//! The scan sends one probe at a time through the core and advances when the
//! port stays silent, so the UI stays responsive and can show progress. A
//! signature that resolves in the registry is served from the cache; a miss
//! waits for the user's consent before the background download starts.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use gpui_kit::{App, AppContext as _, AsyncApp};
use oxtuner_core::{COMMON_PROBE_BAUD_RATES, FirmwareProbe, LinkMode, OpId, PortChoice};
use oxtuner_registry::{BlockingClient, CacheStore, Registry};
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

/// Starts a scan across the ports the core last published, newest last-listed first.
///
/// A freshly plugged device lands at the end of the port list, so the scan
/// walks that list back to front to meet it sooner. The list is the core's
/// pushed snapshot; when no sample arrived yet the scan waits for it instead
/// of reporting a portless machine. The request hurries the baseline along,
/// and the next Detect press scans it.
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
    let ports: Vec<String> = published.iter().map(|port| port.name.clone()).collect();

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

/// The attempts a scan makes, in order: newest port first, then common baud,
/// then family, then framing.
fn attempts_for(ports: &[String]) -> Vec<ProbeAttempt> {
    let mut attempts = Vec::with_capacity(
        ports.len()
            * COMMON_PROBE_BAUD_RATES.len()
            * FirmwareProbe::ALL.len()
            * PROBE_FRAMINGS.len(),
    );

    for port in ports.iter().rev() {
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
pub(crate) fn probed(
    op: OpId,
    port: String,
    baud: Option<u32>,
    framing: LinkMode,
    probe: FirmwareProbe,
    signature: &str,
    cx: &mut App,
) {
    let status = &mut cx.global_mut::<CoreService>().status;
    let Some(scan) = status.firmware_scan.as_ref() else {
        return;
    };
    if scan.op != Some(op) {
        return;
    }

    status.firmware_scan = None;
    status.firmware_error = None;
    status.detected_firmware = Some(DetectedFirmware {
        port,
        baud: baud.unwrap_or(0),
        framing,
        signature: signature.to_owned(),
        definition: None,
    });

    tracing::info!(baud, ?framing, %signature, ?probe, "firmware detected");
    resolve(signature, cx);
    cx.refresh_windows();
}

/// Why an attempt did not produce a signature.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Miss<'a> {
    /// The port stayed silent.
    Silent,
    /// The core refused to run the probe; the message says why.
    Refused(&'a str),
    /// No answer arrived before the deadline.
    Expired,
    /// The core stopped before answering.
    Stopped,
}

/// Handles a probe that found nothing.
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

/// The sentence an unanswered scan ends with.
///
/// A refusal keeps the core's own reason; everything else is the UI's own
/// wording for what happened.
fn miss_message(miss: Miss<'_>) -> String {
    match miss {
        Miss::Refused(reason) => reason.to_owned(),
        Miss::Silent => t!("app.new_project.scan_no_answer").to_string(),
        Miss::Expired => t!("app.new_project.scan_timed_out").to_string(),
        Miss::Stopped => t!("app.new_project.scan_core_stopped").to_string(),
    }
}

/// Resolves a signature through the registry and the local cache.
///
/// The cache lookup hashes the definition, which can be a megabyte, so it runs
/// on the background executor and the answer is applied only while the same
/// detection is still on screen.
fn resolve(signature: &str, cx: &mut App) {
    let registry = Registry::with_defaults();
    let resolved = match registry.resolve(signature) {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(signature, %error, "no definition source for the signature");
            cx.global_mut::<CoreService>().status.firmware_error = Some(error.to_string());
            cx.refresh_windows();
            return;
        }
    };

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
                port: "second".to_owned(),
                baud: 115_200,
                probe: FirmwareProbe::Speeduino,
                framing: LinkMode::Envelope,
            }
        );
        assert_eq!(attempts[1].framing, LinkMode::Legacy);
        assert_eq!(attempts[2].probe, FirmwareProbe::Rusefi);
        assert_eq!(attempts[2].framing, LinkMode::Envelope);
        assert_eq!(attempts[4].baud, COMMON_PROBE_BAUD_RATES[1]);
        assert_eq!(attempts.last().expect("attempts").port, "first");
    }

    #[test]
    fn the_scan_starts_from_the_last_listed_port() {
        let attempts = attempts_for(&[
            "/dev/cu.built-in".to_owned(),
            "/dev/cu.usbmodem-new".to_owned(),
        ]);

        assert_eq!(attempts[0].port, "/dev/cu.usbmodem-new");
        assert_eq!(attempts.last().expect("attempts").port, "/dev/cu.built-in");
    }

    #[test]
    fn a_lost_attempt_keeps_its_own_verdict() {
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
}
