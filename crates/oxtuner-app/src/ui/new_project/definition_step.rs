//! Step 2: detect the ECU, download its definition, or upload one by hand.

use std::path::Path;

use gpui_kit::assets::IconName;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ExternalPaths, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, PathPromptOptions, StatefulInteractiveElement as _, Styled as _, div,
};
use oxtuner_core::{LinkMode, accepts_definition};
use rust_i18n::t;

use super::shared::section_title;
use super::state::{DefinitionMode, SharedWizard};
use crate::core::{self, ProbeAttempt, firmware};

/// Step 2: detect the ECU, download its definition when the registry has it,
/// or upload the file by hand.
///
/// The mode picker decides which half shows: auto-detect probes the ECU,
/// manual shows the file upload. The reading indicators below belong to both,
/// since either source ends in the same inspect round trip.
#[expect(
    clippy::too_many_arguments,
    reason = "the step reads one part of the wizard and core status each"
)]
pub(super) fn definition_body(
    model: &SharedWizard,
    definition: Option<&Path>,
    definition_error: Option<String>,
    inspect_error: Option<&str>,
    preview_ready: bool,
    status: &crate::core::CoreStatus,
    cx: &App,
    creating: bool,
) -> AnyElement {
    let mode = model.borrow().definition_mode;
    // A running download keeps the source, the tabs and every control fixed
    // until it finishes; only the progress and the outcome are actionable.
    let locked = creating || status.downloading_definition;
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;
    let mut body = v_flex()
        .gap_3()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .child(section_title(
                    t!("app.new_project.definition_source").to_string(),
                    cx,
                ))
                .child(section_title(
                    match mode {
                        DefinitionMode::Auto => {
                            t!("app.new_project.detect_firmware_label").to_string()
                        }
                        DefinitionMode::Manual => {
                            t!("app.new_project.definition_label").to_string()
                        }
                    },
                    cx,
                )),
        )
        .child(mode_tabs(model, locked));

    body = match mode {
        DefinitionMode::Auto => body.child(auto_detect_section(model, status, cx, locked)),
        DefinitionMode::Manual => body.child(manual_upload_section(
            model,
            definition,
            definition_error,
            cx,
            locked,
        )),
    };

    body.when_some(inspect_error.map(str::to_owned), |this, message| {
        this.child(
            div()
                .text_xs()
                .text_color(danger)
                .child(t!("app.new_project.inspect_failed", message = message)),
        )
    })
    .when(
        definition.is_some() && !preview_ready && inspect_error.is_none() && !creating,
        |this| {
            this.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(t!("app.new_project.inspecting")),
            )
        },
    )
    .into_any_element()
}

/// The mode tabs: auto-detect or manual upload, never both.
///
/// A segmented control that fills the width; the active tab is the wizard's
/// source. Each mode owns its source, so toggling keeps the other side's
/// answers around and switching back restores them.
fn mode_tabs(model: &SharedWizard, locked: bool) -> AnyElement {
    let selected = match model.borrow().definition_mode {
        DefinitionMode::Auto => 0,
        DefinitionMode::Manual => 1,
    };
    let picker_model = std::rc::Rc::clone(model);

    TabBar::new("new-project-definition-mode")
        .segmented()
        .w_full()
        .selected_index(selected)
        .child(
            Tab::new()
                .flex_1()
                .label(t!("app.new_project.definition_auto").to_string())
                .disabled(locked),
        )
        .child(
            Tab::new()
                .flex_1()
                .label(t!("app.new_project.definition_manual").to_string())
                .disabled(locked),
        )
        .on_click(move |index, _, cx| {
            set_definition_mode(
                &picker_model,
                if *index == 0 {
                    DefinitionMode::Auto
                } else {
                    DefinitionMode::Manual
                },
                cx,
            );
        })
        .into_any_element()
}

/// Switches the definition source.
///
/// Toggling never clears either side's answer: the scan's detection and the
/// picked file both stay, and the step, the review and Create read only the
/// active one. Leaving Auto stops the scan and drops the download it prompted
/// for, so a late answer cannot fill the step after the user chose the file
/// instead. Returning to Auto re-inspects the detected definition when the
/// manual pick replaced the inspected one.
fn set_definition_mode(model: &SharedWizard, mode: DefinitionMode, cx: &mut App) {
    if model.borrow().definition_mode == mode {
        return;
    }

    model.borrow_mut().definition_mode = mode;

    match mode {
        DefinitionMode::Manual => {
            firmware::cancel_scan(cx);
            firmware::drop_download(cx);
        }
        DefinitionMode::Auto => firmware::inspect_detected(cx),
    }

    cx.refresh_windows();
}

/// The Detect button, with Stop beside it while a scan runs.
///
/// A scan of several ports can take a while with every other control disabled,
/// so the running state offers a way out, except while a download holds the
/// step locked.
fn detect_controls(status: &crate::core::CoreStatus, locked: bool) -> AnyElement {
    h_flex()
        .gap_2()
        .child(
            Button::new("new-project-detect-firmware")
                .label(t!("app.new_project.detect_firmware_button"))
                .icon(IconName::Cpu)
                .outline()
                .disabled(locked || status.firmware_scan.is_some() || status.connection.is_some())
                .on_click(|_, _, cx| firmware::start_scan(cx)),
        )
        .when(status.firmware_scan.is_some(), |row| {
            row.child(
                Button::new("new-project-stop-scan")
                    .label(t!("app.new_project.stop_scan"))
                    .outline()
                    .disabled(locked)
                    .on_click(|_, _, cx| firmware::cancel_scan(cx)),
            )
        })
        .into_any_element()
}

/// The line that says which attempt is in flight.
fn scan_line(attempt: &ProbeAttempt, number: usize, total: usize) -> String {
    t!(
        "app.new_project.scanning_firmware",
        port = attempt.port.clone(),
        baud = attempt.baud,
        family = attempt.probe.label(),
        framing = framing_label(attempt.framing),
        attempt = number,
        total = total
    )
    .to_string()
}

/// The framing as the firmware names it, for the scan line and the detection.
fn framing_label(framing: LinkMode) -> &'static str {
    match framing {
        LinkMode::Envelope => "msEnvelope_1.0",
        LinkMode::Legacy => "legacy",
    }
}

/// Step 2, first section: probe the ECU and fetch its definition.
fn auto_detect_section(
    model: &SharedWizard,
    status: &crate::core::CoreStatus,
    cx: &App,
    locked: bool,
) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let danger = cx.theme().danger;
    let mut body = v_flex().gap_3().child(
        div()
            .text_xs()
            .text_color(muted)
            .child(t!("app.new_project.detect_firmware_hint")),
    );

    body = body.child(detect_controls(status, locked));

    if status.connection.is_some() {
        body = body.child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.new_project.detect_while_connected")),
        );
    }

    if let Some(scan) = &status.firmware_scan
        && let Some(attempt) = scan.attempts.get(scan.next)
    {
        body = body.child(div().text_xs().text_color(muted).child(scan_line(
            attempt,
            scan.next + 1,
            scan.attempts.len(),
        )));
    } else if let Some(detected) = &status.detected_firmware {
        body = body.child(div().text_xs().child(t!(
            "app.new_project.firmware_detected",
            signature = detected.signature.clone(),
            port = detected.port.clone(),
            baud = detected.baud,
            framing = framing_label(detected.framing)
        )));
    }

    if let Some(error) = &status.firmware_error {
        body = body.child(div().text_xs().text_color(danger).child(error.clone()));
    }

    let skip_model = std::rc::Rc::clone(model);

    if status.downloading_definition {
        body = body.child(download_progress(status, cx));
    }

    if let Some(prompt) = &status.definition_download {
        body = body.child(
            v_flex()
                .gap_2()
                .child(div().text_xs().text_color(muted).child(t!(
                    "app.new_project.download_prompt",
                    signature = prompt.signature.clone(),
                    host = firmware::host_of(&prompt.url)
                )))
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("new-project-skip-download")
                                .label(t!("app.new_project.skip"))
                                .outline()
                                .disabled(locked)
                                .on_click(move |_, _, cx| {
                                    // Skipping means the file comes by hand:
                                    // move to the manual tab instead of
                                    // stranding the step without a definition.
                                    skip_model.borrow_mut().definition_mode =
                                        DefinitionMode::Manual;
                                    firmware::skip_download(cx);
                                }),
                        )
                        .child(
                            Button::new("new-project-download-definition")
                                .label(t!("app.new_project.download"))
                                .primary()
                                .disabled(locked)
                                .on_click(|_, _, cx| firmware::download(cx)),
                        ),
                ),
        );
    }

    body.into_any_element()
}

/// The in-flight download, as a progress bar instead of static text.
///
/// The bar is determinate once the response headers name a total, and an
/// indeterminate shimmer while the size is still unknown. The row below the
/// bar keeps the byte counts on the left and the total percent on the right.
fn download_progress(status: &crate::core::CoreStatus, cx: &App) -> AnyElement {
    use std::sync::atomic::Ordering;

    let (done, total) = status
        .download_progress
        .as_ref()
        .map_or((0, 0), |progress| {
            (
                progress.done.load(Ordering::Relaxed),
                progress.total.load(Ordering::Relaxed),
            )
        });
    let fraction = status
        .download_progress
        .as_ref()
        .and_then(|progress| progress.fraction());

    let mut bar = gpui_kit::component::progress::Progress::new("new-project-definition-download")
        .w_full()
        .color(cx.theme().primary)
        .accessibility_label(t!("app.new_project.downloading_definition").to_string());

    if let Some(fraction) = fraction {
        bar = bar.value(fraction * 100.0);
    } else {
        bar = bar.loading(true);
    }

    let muted = cx.theme().muted_foreground;
    let size = if total > 0 {
        t!(
            "app.new_project.downloaded_of_total",
            downloaded = format_bytes(done),
            total = format_bytes(total)
        )
    } else {
        t!(
            "app.new_project.downloaded_bytes",
            downloaded = format_bytes(done)
        )
    };
    let percent = fraction.map_or_else(String::new, |fraction| format!("{:.0}%", fraction * 100.0));

    v_flex()
        .gap_1()
        .child(bar)
        .child(
            h_flex()
                .justify_between()
                .child(div().text_xs().text_color(muted).child(size))
                .child(div().text_xs().text_color(muted).child(percent)),
        )
        .into_any_element()
}

/// Bytes as the tuner reads them: `48 KB`, never a raw count.
///
/// Rounds to the nearest unit, so a half-downloaded file never reads as whole
/// kilobytes, while a file just under a megabyte still reads as kilobytes.
fn format_bytes(bytes: u64) -> String {
    const UNIT: u64 = 1024;

    if bytes < UNIT {
        format!("{bytes} B")
    } else if bytes < UNIT * UNIT {
        format!("{} KB", (bytes + UNIT / 2) / UNIT)
    } else {
        format!("{} MB", (bytes + UNIT * UNIT / 2) / (UNIT * UNIT))
    }
}

/// Step 2, second section: drop a definition file or pick one by hand.
///
/// The whole zone takes a file drop on every platform and a click anywhere
/// opens the native picker. The native picker cannot filter by extension, so
/// anything can be picked and only `.ini` files are accepted; anything else
/// explains itself below the file name instead of silently doing nothing.
fn manual_upload_section(
    model: &SharedWizard,
    definition: Option<&Path>,
    error: Option<String>,
    cx: &App,
    locked: bool,
) -> AnyElement {
    let chosen = definition.map_or_else(
        || t!("app.new_project.definition_none").to_string(),
        |path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        },
    );
    let danger = cx.theme().danger;

    v_flex()
        .gap_2()
        .child(definition_dropzone(model, cx, locked))
        .child(div().flex_1().min_w_0().truncate().text_sm().child(chosen))
        .when_some(error, |this, message| {
            this.child(div().text_xs().text_color(danger).child(message))
        })
        .into_any_element()
}

/// The dashed drop target: a file drop lands here on every platform and a
/// click anywhere opens the native picker.
fn definition_dropzone(model: &SharedWizard, cx: &App, locked: bool) -> AnyElement {
    let click_model = std::rc::Rc::clone(model);
    let drop_model = std::rc::Rc::clone(model);
    let muted = cx.theme().muted_foreground;

    div()
        .id("new-project-definition-dropzone")
        .w_full()
        .rounded(cx.theme().radius)
        .border_1()
        .border_dashed()
        .border_color(cx.theme().border)
        .px_4()
        .py_4()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            if locked {
                return;
            }
            choose_definition(std::rc::Rc::clone(&click_model), cx);
        })
        .on_drop::<ExternalPaths>(move |paths, _, cx| {
            if locked {
                return;
            }
            if let Some(path) = paths.paths().first() {
                use_definition(path, &drop_model, cx);
            }
        })
        .child(
            Icon::new(IconName::ArrowUpFromLine)
                .large()
                .text_color(muted),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(t!("app.new_project.definition_drop_title")),
        )
        .child(
            div()
                .text_xs()
                .text_color(muted)
                .child(t!("app.new_project.definition_drop_hint")),
        )
        .into_any_element()
}

/// Asks for the definition file.
fn choose_definition(model: SharedWizard, cx: &mut App) {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(t!("app.new_project.choose_definition").to_string().into()),
    });

    cx.spawn(async move |cx| match receiver.await {
        Ok(Ok(Some(paths))) => {
            if let Some(path) = paths.into_iter().next() {
                cx.update(|cx| {
                    // A pick that lands after close belongs to no dialog.
                    if !model.borrow().closed {
                        use_definition(&path, &model, cx);
                    }
                });
            }
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => tracing::warn!(%error, "the definition picker failed"),
        Err(error) => tracing::warn!(%error, "the definition picker channel closed"),
    })
    .detach();
}

/// Keeps `path` as the wizard's definition.
///
/// Reading it is the core's job: Create carries the path, and a file that
/// cannot be read fails there with the same pipeline every other create error
/// uses. The extension rule is shared with the core so a wrong pick is still
/// explained immediately.
fn use_definition(path: &Path, model: &SharedWizard, cx: &mut App) {
    if !accepts_definition(path) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().to_string(),
        );
        let mut wizard = model.borrow_mut();
        wizard.definition = None;
        wizard.definition_error =
            Some(t!("app.new_project.definition_not_ini", name = name).to_string());
        drop(wizard);
        cx.refresh_windows();
        return;
    }

    let mut wizard = model.borrow_mut();
    if wizard.closed {
        return;
    }
    wizard.definition = Some(path.to_path_buf());
    drop(wizard);

    // The serial moves with the send: an inspect failure that arrives after
    // this point belongs to this definition.
    crate::core::firmware::note_inspect_sent(path, cx);
    model.borrow_mut().retry_definition();

    // The core reads and parses the file; the answer carries the signature and
    // the setting groups the next two steps render.
    core::inspect_definition(path, cx);
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use oxtuner_core::FirmwareProbe;

    use super::*;

    #[test]
    fn the_scan_line_fills_every_placeholder() {
        let attempt = ProbeAttempt {
            port: "/dev/cu.usbmodem".to_owned(),
            baud: 115_200,
            probe: FirmwareProbe::Speeduino,
            framing: LinkMode::Envelope,
        };

        let line = super::scan_line(&attempt, 2, 16);

        assert!(!line.contains("%{"), "{line}");
        assert!(line.contains("/dev/cu.usbmodem"), "{line}");
        assert!(line.contains("Speeduino"), "{line}");
        assert!(line.contains("msEnvelope_1.0"), "{line}");
        assert!(line.contains("2 of 16"), "{line}");
    }

    #[test]
    fn byte_counts_read_as_units() {
        assert_eq!(super::format_bytes(0), "0 B");
        assert_eq!(super::format_bytes(512), "512 B");
        assert_eq!(super::format_bytes(1024), "1 KB");
        assert_eq!(super::format_bytes(1025), "1 KB");
        assert_eq!(super::format_bytes(1536), "2 KB");
        assert_eq!(super::format_bytes(48 * 1024), "48 KB");
        assert_eq!(super::format_bytes(1024 * 1024 - 1), "1024 KB");
        assert_eq!(super::format_bytes(3 * 1024 * 1024 / 2), "2 MB");
        assert_eq!(super::format_bytes(3 * 1024 * 1024), "3 MB");
    }
}
