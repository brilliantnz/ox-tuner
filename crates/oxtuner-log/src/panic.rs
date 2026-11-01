use std::{
    backtrace::Backtrace,
    panic::{PanicHookInfo, set_hook, take_hook},
};

/// Replaces the panic hook with one that records the panic first.
///
/// The record goes through the same subscriber as every other line, so a
/// release build with no terminal still has the message, location and
/// backtrace on disk. The previous hook runs afterwards, which keeps the usual
/// stderr output for a debug run.
pub(crate) fn install() {
    let previous = take_hook();

    set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("panic");
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_default();
        let backtrace = Backtrace::force_capture();

        tracing::error!(
            panic = true,
            location = %location,
            backtrace = %backtrace,
            "panic: {message}"
        );

        previous(info);
    }));
}
