//! Test-only helpers: a process-wide environment lock so tests that mutate
//! HOME / CARGO_HOME cannot interleave. The engine crate's
//! `test_support::lock_test_env` twin — same shape, per-crate lock (a
//! cross-crate lock would have to live in a production dependency).

pub(crate) fn lock_test_env() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    match LOCK.get_or_init(|| std::sync::Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
