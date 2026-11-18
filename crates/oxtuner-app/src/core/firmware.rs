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
use oxtuner_core::{COMMON_PROBE_BAUD_RATES, FirmwareProbe, OpId, PortChoice};
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
    let Some(published) = crate::core::ports(cx) else {
        crate::core::refresh_ports(cx);
        let status = &mut cx.global_mut::<CoreService>().status;
        clear_detection(status);
        status.firmware_error = Some(t!("app.new_project.scan_ports_pending").to_string());
        cx.refresh_windows();
        return;
    };
    let ports: Vec<String> = published.iter().map(|port| port.name.clone()).collect();

    let status = &mut cx.global_mut::<CoreService>().status;
    clear_detection(status);
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
/// whether the last one found an ECU, found nothing, or never started.
fn clear_detection(status: &mut CoreStatus) {
    status.firmware_scan = None;
    status.definition_download = None;
    status.detected_firmware = None;
}

/// The attempts a scan makes, in order: newest port first, then common baud, then family.
fn attempts_for(ports: &[String]) -> Vec<ProbeAttempt> {
    let mut attempts = Vec::with_capacity(ports.len() * COMMON_PROBE_BAUD_RATES.len() * 2);

    for port in ports.iter().rev() {
        for baud in COMMON_PROBE_BAUD_RATES {
            for probe in FirmwareProbe::ALL {
                attempts.push(ProbeAttempt {
                    port: port.clone(),
                    baud,
                    probe,
                });
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
        return;
    };

    let Some(handle) = CoreService::handle(cx) else {
        fail_scan(cx, t!("app.new_project.scan_core_stopped").as_ref());
        return;
    };

    match handle.probe_firmware(
        PortChoice::serial(attempt.port.clone(), attempt.baud),
        attempt.probe,
    ) {
        Ok(op) => {
            if let Some(scan) = cx.global_mut::<CoreService>().status.firmware_scan.as_mut() {
                scan.op = Some(op);
            }

            tracing::debug!(
                port = %attempt.port,
                baud = attempt.baud,
                probe = ?attempt.probe,
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
fn fail_scan(cx: &mut App, message: &str) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.firmware_scan = None;
    status.firmware_error = Some(message.to_owned());
    cx.refresh_windows();
}

/// Stops a scan the user walked away from.
///
/// A probe already in flight still answers, but its miss is dropped: nothing is
/// left to advance, and a probe miss is never a notification.
pub(crate) fn cancel_scan(cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    if status.firmware_scan.take().is_some() {
        tracing::debug!("the firmware scan was cancelled");
    }
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
    status.definition_download = None;
    status.inspected = None;
    status.downloading_definition = false;
    status.download_progress = None;
    status.test = None;
    status.testing = None;
    status.inspect_serial = status.failure_serial;
    cx.refresh_windows();
}

/// Handles a probe that answered with a signature.
pub(crate) fn probed(
    op: OpId,
    port: String,
    baud: Option<u32>,
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
        signature: signature.to_owned(),
        definition: None,
    });

    tracing::info!(baud, %signature, ?probe, "firmware detected");
    resolve(signature, cx);
    cx.refresh_windows();
}

/// Handles a probe that found nothing.
///
/// A miss that belongs to no scan (cancelled, or a stale op) is still not a
/// visible failure: a scan step is never news on its own.
pub(crate) fn missed(op: OpId, cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    let Some(scan) = status.firmware_scan.as_mut() else {
        return;
    };
    if scan.op != Some(op) {
        return;
    }

    scan.next += 1;
    if scan.next < scan.attempts.len() {
        send_current(cx);
        cx.refresh_windows();
        return;
    }

    let attempts = scan.attempts.len();
    status.firmware_scan = None;
    status.firmware_error = Some(t!("app.new_project.scan_no_answer").to_string());
    tracing::info!(attempts, "the firmware scan found no ECU");
    cx.refresh_windows();
}

/// Resolves a signature through the registry and the local cache.
fn resolve(signature: &str, cx: &mut App) {
    let registry = Registry::with_defaults();
    match registry.resolve(signature) {
        Ok(resolved) => {
            let store = CacheStore::new(AppPaths::get(cx).registry_dir());
            let path = store.definition_ini(&resolved.id);

            if store.verify_file(&path) {
                tracing::debug!(
                    signature,
                    path = %path.display(),
                    "the definition is cached"
                );
                finish(&path, cx);
                return;
            }

            cx.global_mut::<CoreService>().status.definition_download = Some(DefinitionDownload {
                signature: signature.to_owned(),
                url: resolved.ini_url,
                id: resolved.id,
            });
        }
        Err(error) => {
            tracing::warn!(signature, %error, "no definition source for the signature");
            cx.global_mut::<CoreService>().status.firmware_error = Some(error.to_string());
        }
    }
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
                    let status = &mut cx.global_mut::<CoreService>().status;
                    status.downloading_definition = false;
                    status.download_progress = None;
                    finish(&path, cx);
                }
                Err(error) => {
                    let status = &mut cx.global_mut::<CoreService>().status;
                    status.downloading_definition = false;
                    status.download_progress = None;
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

/// Drops a download the user declined.
pub(crate) fn skip_download(cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.definition_download = None;
    status.downloading_definition = false;
    status.download_progress = None;
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

    inspect_definition(path, cx);
    cx.refresh_windows();
}

/// Records that the active definition was just sent for inspect.
///
/// The serial moves with the send: an inspect failure that arrives after this
/// point belongs to this definition, and the dialog compares against it.
pub(crate) fn note_inspect_sent(cx: &mut App) {
    let status = &mut cx.global_mut::<CoreService>().status;
    status.inspect_serial = status.failure_serial;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_tries_every_baud_and_family_per_port() {
        let attempts = attempts_for(&["first".to_owned(), "second".to_owned()]);

        assert_eq!(
            attempts.len(),
            2 * COMMON_PROBE_BAUD_RATES.len() * FirmwareProbe::ALL.len()
        );
        assert_eq!(
            attempts[0],
            ProbeAttempt {
                port: "second".to_owned(),
                baud: 115_200,
                probe: FirmwareProbe::Speeduino,
            }
        );
        assert_eq!(attempts[1].probe, FirmwareProbe::Rusefi);
        assert_eq!(attempts[2].baud, COMMON_PROBE_BAUD_RATES[1]);
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
    fn a_host_is_read_from_the_download_url() {
        assert_eq!(
            host_of("https://speeduino.com/fw/202501.7.ini"),
            "speeduino.com"
        );
        assert_eq!(host_of("not a url"), "not a url");
    }
}
