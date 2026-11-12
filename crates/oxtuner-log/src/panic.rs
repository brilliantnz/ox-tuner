use std::{
    backtrace::Backtrace,
    panic::{PanicHookInfo, set_hook, take_hook},
};

/// Replaces the panic hook with one that records the panic.
///
/// The previous hook runs first, so its stderr output survives even if
/// recording fails, and the file record follows through the same subscriber as
/// every other line. A release build with no terminal still has the message,
/// location and backtrace on disk.
///
/// Recording must not panic: `std` aborts as soon as a panic is raised while a
/// hook is running, before any unwind could be caught.
pub(crate) fn install() {
    let previous = take_hook();

    set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        previous(info);
        record(info);
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
    use std::{
        panic::catch_unwind,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use tracing_subscriber::layer::SubscriberExt as _;

    use super::*;

    type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

    /// Puts the hook from before the test back, even when the test panics.
    struct HookRestore(Option<Hook>);

    impl Drop for HookRestore {
        fn drop(&mut self) {
            if let Some(hook) = self.0.take() {
                set_hook(hook);
            }
        }
    }

    fn records_the_panic(store: &crate::LogStore) -> bool {
        store
            .snapshot()
            .records
            .iter()
            .any(|record| record.message.contains("panic: hook test"))
    }

    #[test]
    fn a_panic_is_recorded_in_the_ring_after_the_previous_hook_runs() {
        let store = crate::LogStore::new(8);
        let filter = crate::filter::default_filter(crate::Profile::Debug, crate::Verbosity::Normal);
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(crate::store::StoreLayer::new(store.clone()));

        // Quiet the inherited hook for this test only, and restore it
        // unconditionally when the guard drops.
        let _restore = HookRestore(Some(take_hook()));

        // The stand-in previous hook sees the ring before this hook records, so
        // it can prove the order.
        let recorded_first = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&recorded_first);
        let hook_store = store.clone();
        set_hook(Box::new(move |_| {
            flag.store(records_the_panic(&hook_store), Ordering::Relaxed);
        }));
        install();

        let panicked = tracing::subscriber::with_default(subscriber, || {
            catch_unwind(|| panic!("hook test")).is_err()
        });

        assert!(panicked);
        assert!(
            !recorded_first.load(Ordering::Relaxed),
            "the previous hook ran before the record was written"
        );
        assert!(records_the_panic(&store), "the record reached the ring");
        let record = store
            .snapshot()
            .records
            .iter()
            .find(|record| record.message.contains("panic: hook test"))
            .expect("the panic is in the ring")
            .clone();
        assert_eq!(record.field("panic"), Some("true"));
    }
}
