//! Test-only helpers (per-crate twins of the engine's test support).

/// The process-wide test env lock shared with every other crate
/// (`codesmith_agent_runtime::test_support`): a crate-local lock let
/// env-mutating tests in other binaries' crates flip CODESMITH_HOME/HOME
/// mid-test, which once made auth-cache `mark`/`load` resolve different
/// paths.
#[cfg(test)]
pub(crate) fn lock_test_env() -> std::sync::MutexGuard<'static, ()> {
    codesmith_agent_runtime::test_support::lock_test_env()
}

#[cfg(test)]
pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: callers hold the process-wide test env mutex.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: callers hold the test env mutex until after this drop.
        unsafe {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}
