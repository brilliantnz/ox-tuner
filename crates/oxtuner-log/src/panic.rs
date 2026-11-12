use std::{
    backtrace::Backtrace,
    panic::{AssertUnwindSafe, PanicHookInfo, catch_unwind, set_hook, take_hook},
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
        // Recording is best effort: a panic inside the hook would double-panic
        // and abort before either the file record or the previous hook ran.
        let _ = catch_unwind(AssertUnwindSafe(|| record(info)));

        previous(info);
    }));
}

/// Writes one panic through the subscriber.
fn record(info: &PanicHookInfo<'_>) {
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
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    #[test]
    fn a_panic_is_recorded_in_the_ring_before_the_previous_hook_runs() {
        let store = crate::LogStore::new(8);
        let filter = crate::filter::default_filter(crate::Profile::Debug, false);
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(crate::store::StoreLayer::new(store.clone()));

        // Quiet the inherited hook so the test output keeps only failures.
        let original = take_hook();
        set_hook(Box::new(|_| {}));
        install();

        let panicked = tracing::subscriber::with_default(subscriber, || {
            catch_unwind(|| panic!("hook test")).is_err()
        });
        set_hook(original);

        assert!(panicked);
        let snapshot = store.snapshot();
        let record = snapshot
            .records
            .iter()
            .find(|record| record.message.contains("panic: hook test"))
            .expect("the panic is in the ring");
        assert_eq!(record.field("panic"), Some("true"));
    }
}
