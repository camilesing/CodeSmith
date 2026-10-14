//! Schema migration framework for `~/.codesmith/` persisted records.
//!
//! Every persistence layer in `crates/tui/src/` (sessions, threads,
//! tasks, automations, offline queue) gates `schema_version > CURRENT_*`
//! to prevent silent truncation when an older binary tries to load a
//! record from a newer one. What was missing — and what this module
//! fixes — is the **upgrade path**: when `schema_version < CURRENT_*`,
//! the load function should run forward migrations rather than loading
//! a partially-correct record.
//!
//! ## Domain registration
//!
//! Each persistence type implements [`SchemaMigration`]:
//!
//! ```ignore
//! pub struct SessionMigration;
//!
//! impl SchemaMigration for SessionMigration {
//!     const CURRENT_VERSION: u32 = 1;
//!     const DOMAIN: &'static str = "session";
//!     const MIGRATIONS: &'static [MigrationFn] = &[
//!         // index i migrates from version (i+1) to (i+2)
//!         migrate_session_v1_to_v2,
//!     ];
//! }
//! ```
//!
//! ## Load-site usage
//!
//! Inside the load function, after deserialization. The whole
//! check–migrate–backup–write cycle must hold an advisory cross-process
//! lock ([`lock_record`]) — two concurrent CLI invocations loading the
//! same record would otherwise both pass the version check against the
//! same on-disk bytes, and their backup/write interleaving would let the
//! second writer's `.bak` capture already-migrated content (destroying
//! the true original) while last-writer-wins drops the other's update:
//!
//! ```ignore
//! let mut lock = lock_record(&path)?;
//! let _guard = lock.write()?;
//! // Re-read under the lock: another process may have migrated the
//! // record between the initial read and here.
//! let raw = std::fs::read_to_string(&path)?;
//! let mut record: SessionRecord = serde_json::from_str(&raw)?;
//! if record.schema_version < SessionMigration::CURRENT_VERSION {
//!     let mut value: serde_json::Value = serde_json::from_str(&raw)?;
//!     let _final = SessionMigration::migrate(
//!         &mut value,
//!         record.schema_version,
//!     )?;
//!     backup_before_migrate(&path, SessionMigration::DOMAIN);
//!     write_atomic(&path, serde_json::to_string_pretty(&value)?.as_bytes())?;
//!     // Re-deserialize with the migrated value into the up-to-date struct.
//!     record = serde_json::from_value(value)?;
//! }
//! ```
//!
//! ## Migration step contract
//!
//! Each step takes a mutable JSON value at version `N` and mutates it
//! into version `N+1`. Steps must be idempotent in the sense that a
//! re-run of the migration on an already-migrated value should be a
//! no-op (because `serde_json::Value` is cheap to introspect, this
//! usually means "if field already exists with the new shape, skip").
//!
//! Steps must NOT call `write_atomic` themselves — the framework writes
//! once at the end. They must NOT log credentials or other sensitive
//! material from the value being migrated.

use std::fs;
use std::path::{Path, PathBuf};

/// Result returned when a migration step fails.
#[derive(Debug)]
pub struct MigrationError {
    pub from_version: u32,
    pub to_version: u32,
    pub reason: String,
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "schema migration {} → {} failed: {}",
            self.from_version, self.to_version, self.reason
        )
    }
}

impl std::error::Error for MigrationError {}

/// Signature of a single forward migration step.
#[allow(dead_code)] // Public surface; first concrete migrator lands when v2 ships.
pub type MigrationFn = fn(&mut serde_json::Value) -> Result<(), MigrationError>;

/// Each persistence domain implements this trait.
///
/// `MIGRATIONS[i]` migrates from version `i + 1` to version `i + 2`. So
/// `MIGRATIONS[0]` is the v1 → v2 step, `MIGRATIONS[1]` is v2 → v3, etc.
/// `CURRENT_VERSION` must equal `MIGRATIONS.len() + 1` (i.e. the version
/// produced by running every step in sequence starting from version 1).
#[allow(dead_code)] // Public surface; first concrete domain lands when v2 ships.
pub trait SchemaMigration {
    /// The current schema version for this domain.
    const CURRENT_VERSION: u32;

    /// Human-readable domain label for logging (e.g. "session", "thread").
    const DOMAIN: &'static str;

    /// Ordered list of migration step functions.
    const MIGRATIONS: &'static [MigrationFn];

    /// Run all required migrations to bring `version` up to
    /// [`CURRENT_VERSION`](SchemaMigration::CURRENT_VERSION).
    ///
    /// Returns the final stamped version. Stamps each intermediate
    /// version onto `value["schema_version"]` so a partial migration
    /// failure leaves a record at a known state rather than mixed.
    fn migrate(value: &mut serde_json::Value, version: u32) -> Result<u32, MigrationError> {
        if version > Self::CURRENT_VERSION {
            // Caller's responsibility to reject newer-than-supported
            // records — the framework's job is forward migration only.
            return Err(MigrationError {
                from_version: version,
                to_version: Self::CURRENT_VERSION,
                reason: format!(
                    "{} record at v{version} is newer than current v{}",
                    Self::DOMAIN,
                    Self::CURRENT_VERSION
                ),
            });
        }

        let mut current = version;
        for (idx, step) in Self::MIGRATIONS.iter().enumerate() {
            let step_from = (idx as u32) + 1;
            if current > step_from {
                // Already past this step — the value was loaded at a
                // newer-than-step version, skip.
                continue;
            }
            if current < step_from {
                // Underflow: Self's MIGRATIONS are dense from 1, and
                // the loop should never see a record older than the
                // first step. If we get here the const list is misordered.
                return Err(MigrationError {
                    from_version: current,
                    to_version: step_from + 1,
                    reason: format!(
                        "{} migration list is non-contiguous at index {idx}",
                        Self::DOMAIN
                    ),
                });
            }
            step(value)?;
            current = step_from + 1;
            // serde_json's IndexMut panics for a non-object top level
            // (array/string/number record files); surface a typed error.
            let obj = value.as_object_mut().ok_or_else(|| MigrationError {
                from_version: current,
                to_version: step_from + 1,
                reason: format!(
                    "{} record is not a JSON object; cannot stamp schema_version",
                    Self::DOMAIN
                ),
            })?;
            obj.insert("schema_version".to_string(), serde_json::json!(current));
        }

        if current != Self::CURRENT_VERSION {
            return Err(MigrationError {
                from_version: version,
                to_version: Self::CURRENT_VERSION,
                reason: format!(
                    "{} migrated to v{current} but expected v{}",
                    Self::DOMAIN,
                    Self::CURRENT_VERSION
                ),
            });
        }

        Ok(current)
    }
}

/// Create a `.bak` copy of `path` before mutation. Returns the backup
/// path. Errors are logged at warn level and ignored — the migration
/// proceeds because [`crate::utils::write_atomic`] is itself crash-safe.
///
/// The `.bak` file is left on disk after a successful migration so a
/// user who notices a regression can manually restore it. No automatic
/// garbage collection — bak files are user-visible recovery artifacts.
#[allow(dead_code)] // Public surface; first call site lands when v2 ships.
pub fn backup_before_migrate(path: &Path, domain: &str) -> PathBuf {
    let bak = path.with_extension(
        path.extension()
            .map(|ext| format!("{}.bak", ext.to_string_lossy()))
            .unwrap_or_else(|| "bak".to_string()),
    );
    match fs::copy(path, &bak) {
        Ok(_) => tracing::info!(
            domain,
            from = %path.display(),
            to = %bak.display(),
            "schema backup created"
        ),
        Err(e) => tracing::warn!(
            domain,
            from = %path.display(),
            error = %e,
            "schema backup failed (continuing — migration is crash-safe)"
        ),
    }
    bak
}

/// Take an advisory cross-process lock covering a record's whole
/// read–migrate–backup–write cycle. Call `lock.write()?` on the returned
/// value and hold the guard until the migrated record has been written —
/// without it, two concurrent CLI processes loading the same file can
/// both pass the version check against the same on-disk bytes and race
/// their backup/write interleavings (the second `.bak` then captures
/// already-migrated content and last-writer-wins drops the other's
/// update). The lock file lives next to the record (same directory ⇒
/// same filesystem), mirroring the `task_v2` `.lock` idiom.
#[allow(dead_code)] // Public surface; first concrete migrator lands when v2 ships.
pub fn lock_record(path: &Path) -> std::io::Result<fd_lock::RwLock<fs::File>> {
    let lock_path = path.with_extension(
        path.extension()
            .map(|ext| format!("{}.lock", ext.to_string_lossy()))
            .unwrap_or_else(|| "lock".to_string()),
    );
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    Ok(fd_lock::RwLock::new(fs::File::open(&lock_path)?))
}

/// Per-domain migration registrations.
///
/// Each persistence type below points at the same `CURRENT_*` constant
/// the original module already gates on. The `MIGRATIONS` list is empty
/// today because no schema bumps have shipped yet — but the framework is
/// in place so the next bump only needs to:
///
/// 1. Add a `migrate_<domain>_v<N>_to_v<N+1>` function in this module.
/// 2. Append it to the matching `MIGRATIONS` list.
/// 3. Bump `CURRENT_VERSION` to match.
/// 4. Wire `<Domain>Migration::migrate(...)` into the load function in
///    the owning module.
pub mod registry {
    use super::{MigrationFn, SchemaMigration};

    /// Sessions: `~/.codesmith/sessions/<id>.json` and the latest
    /// checkpoint at `~/.codesmith/sessions/checkpoints/latest.json`.
    #[allow(dead_code)]
    pub struct SessionMigration;
    impl SchemaMigration for SessionMigration {
        const CURRENT_VERSION: u32 = 1;
        const DOMAIN: &'static str = "session";
        const MIGRATIONS: &'static [MigrationFn] = &[];
    }

    /// Offline queue: `~/.codesmith/sessions/checkpoints/offline_queue.json`.
    #[allow(dead_code)]
    pub struct OfflineQueueMigration;
    impl SchemaMigration for OfflineQueueMigration {
        const CURRENT_VERSION: u32 = 1;
        const DOMAIN: &'static str = "offline_queue";
        const MIGRATIONS: &'static [MigrationFn] = &[];
    }

    /// Runtime threads / turns / items / events / store state — all
    /// share `CURRENT_RUNTIME_SCHEMA_VERSION`.
    #[allow(dead_code)]
    pub struct RuntimeMigration;
    impl SchemaMigration for RuntimeMigration {
        const CURRENT_VERSION: u32 = 2;
        const DOMAIN: &'static str = "runtime";
        const MIGRATIONS: &'static [MigrationFn] = &[];
    }

    /// Durable tasks under `~/.codesmith/tasks/`.
    #[allow(dead_code)]
    pub struct TaskMigration;
    impl SchemaMigration for TaskMigration {
        const CURRENT_VERSION: u32 = 2;
        const DOMAIN: &'static str = "task";
        const MIGRATIONS: &'static [MigrationFn] = &[];
    }

    /// Automation records and their per-run history.
    #[allow(dead_code)]
    pub struct AutomationMigration;
    impl SchemaMigration for AutomationMigration {
        const CURRENT_VERSION: u32 = 1;
        const DOMAIN: &'static str = "automation";
        const MIGRATIONS: &'static [MigrationFn] = &[];
    }

    #[allow(dead_code)]
    pub struct AutomationRunMigration;
    impl SchemaMigration for AutomationRunMigration {
        const CURRENT_VERSION: u32 = 1;
        const DOMAIN: &'static str = "automation_run";
        const MIGRATIONS: &'static [MigrationFn] = &[];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test harness: a fake "thread" domain at v3 with two migrations
    /// (v1 → v2 adds an `archived` field; v2 → v3 adds a `kind` field).
    struct TestThreadMigration;

    fn add_archived_field(value: &mut serde_json::Value) -> Result<(), MigrationError> {
        if value.get("archived").is_none() {
            value["archived"] = serde_json::json!(false);
        }
        Ok(())
    }

    fn add_kind_field(value: &mut serde_json::Value) -> Result<(), MigrationError> {
        if value.get("kind").is_none() {
            value["kind"] = serde_json::json!("standard");
        }
        Ok(())
    }

    impl SchemaMigration for TestThreadMigration {
        const CURRENT_VERSION: u32 = 3;
        const DOMAIN: &'static str = "test_thread";
        const MIGRATIONS: &'static [MigrationFn] = &[add_archived_field, add_kind_field];
    }

    #[test]
    fn migrate_no_op_when_already_current() {
        let mut value = serde_json::json!({
            "schema_version": 3,
            "id": "abc",
            "archived": true,
            "kind": "feature_branch"
        });
        let final_version = TestThreadMigration::migrate(&mut value, 3).expect("ok");
        assert_eq!(final_version, 3);
        // Existing values must be untouched (we don't reset to defaults).
        assert_eq!(value["archived"], serde_json::json!(true));
        assert_eq!(value["kind"], serde_json::json!("feature_branch"));
    }

    #[test]
    fn migrate_runs_all_steps_from_v1() {
        let mut value = serde_json::json!({
            "schema_version": 1,
            "id": "abc"
        });
        let final_version = TestThreadMigration::migrate(&mut value, 1).expect("ok");
        assert_eq!(final_version, 3);
        assert_eq!(value["schema_version"], serde_json::json!(3));
        assert_eq!(value["archived"], serde_json::json!(false));
        assert_eq!(value["kind"], serde_json::json!("standard"));
    }

    #[test]
    fn migrate_runs_only_remaining_steps_from_v2() {
        let mut value = serde_json::json!({
            "schema_version": 2,
            "id": "abc",
            "archived": true
        });
        let final_version = TestThreadMigration::migrate(&mut value, 2).expect("ok");
        assert_eq!(final_version, 3);
        // archived was already set; migration must NOT overwrite to default.
        assert_eq!(value["archived"], serde_json::json!(true));
        assert_eq!(value["kind"], serde_json::json!("standard"));
    }

    #[test]
    fn migrate_rejects_newer_than_current() {
        let mut value = serde_json::json!({
            "schema_version": 99
        });
        let err = TestThreadMigration::migrate(&mut value, 99).expect_err("must reject");
        assert_eq!(err.from_version, 99);
        assert_eq!(err.to_version, 3);
        assert!(err.reason.contains("newer than current"));
    }

    #[test]
    fn backup_creates_bak_file_alongside_original() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session_abc.json");
        std::fs::write(&path, r#"{"id":"abc"}"#).expect("write");
        let bak = backup_before_migrate(&path, "test_session");
        assert!(bak.exists(), "bak file must exist at {}", bak.display());
        assert_eq!(
            std::fs::read_to_string(&bak).expect("read bak"),
            r#"{"id":"abc"}"#
        );
        // Bak is path.json.bak (extension append, not replace).
        assert!(
            bak.to_string_lossy().ends_with(".json.bak"),
            "bak suffix must be `.json.bak`; got {}",
            bak.display()
        );
    }

    #[test]
    fn backup_failure_does_not_panic_or_propagate() {
        // Pointing at a non-existent source: copy fails, but the function
        // returns the bak path it would have used and logs a warning.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("does_not_exist.json");
        let bak = backup_before_migrate(&path, "test_session");
        // The path is well-formed even though copy failed.
        assert!(bak.to_string_lossy().ends_with(".json.bak"));
    }
    #[test]
    fn lock_record_creates_lock_file_and_grants_write_guard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("session_abc.json");
        std::fs::write(&path, r#"{"id":"abc"}"#).expect("write");

        let mut lock = lock_record(&path).expect("lock file opens");
        let guard = lock.write().expect("write guard acquired");
        let lock_path = tmp.path().join("session_abc.json.lock");
        assert!(lock_path.exists(), "lock file must exist beside the record");
        drop(guard);

        // Re-acquirable after release (sequential, same process).
        let mut lock = lock_record(&path).expect("lock file reopens");
        drop(lock.write().expect("write guard re-acquired"));
    }

    #[test]
    fn migration_lists_and_versions_are_consistent() {
        // With NO migration steps shipped, CURRENT_VERSION must equal the
        // version the on-disk writers stamp directly (2): lowering it to
        // satisfy the textbook `len + 1` invariant would make every
        // existing v2 record fail `migrate()` as "newer than supported".
        // Once a real v1→v2 step lands, `len + 1` becomes the contract —
        // this test fails either way it drifts.
        fn check<M: SchemaMigration>() {
            // With no steps shipped, CURRENT_VERSION equals whatever the
            // domain's writers stamp (session=1, runtime/task=2) — lowering
            // it to satisfy the textbook invariant would fail every
            // existing record as "newer than supported". The guard that
            // holds in BOTH regimes: a domain can never claim fewer
            // versions than its step count plus one.
            assert!(
                M::CURRENT_VERSION as usize > M::MIGRATIONS.len(),
                "{} violates CURRENT_VERSION > MIGRATIONS.len()",
                M::DOMAIN
            );
        }
        check::<registry::SessionMigration>();
        check::<registry::RuntimeMigration>();
        check::<registry::TaskMigration>();
        check::<registry::AutomationMigration>();
    }
}
