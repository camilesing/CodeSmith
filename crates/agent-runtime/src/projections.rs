//! Transcript projections: session state rebuilt as a pure fold of the
//! conversation transcript when a session is loaded.
//!
//! Recording-layer role (one of four layers — each owning module states its
//! role): the transcript/event stream is the *authoritative log*; a
//! registered projection holds no authority of its own — every field it
//! keeps is derivable by replaying its rebuild over the transcript. State
//! that cannot be re-folded from the transcript (e.g. the fact ledger,
//! whose source messages compaction drops) is persisted as a snapshot
//! instead and deliberately does not register here.
//!
//! This module owns the *rebuild entry*, not fold semantics: each
//! projection keeps its own fold function (they genuinely differ —
//! last-write-wins, update-in-place, call/result pairing), so onboarding a
//! new projection is one `register` call at the load path instead of
//! another hand-wired rebuild block. This is deliberately not the mod
//! runtime's `MessageProjectionHub` (`codesmith_agent::extension`), which
//! folds incrementally while the session runs, in JSON, for mod-registered
//! projections — different currency, different lifecycle, same
//! transcript-is-the-log principle.
//!
//! Known limitations:
//!
//! * Engine-held working-set state (`WorkingSet`) rebuilds through its own
//!   entry, `Session::rebuild_working_set`, on every engine transcript
//!   replacement (`Op::SyncSession` — which every load path already sends).
//!   Its trigger is transcript *replacement*, not transcript *load*;
//!   registering it here would double-rebuild on every load. The
//!   `workspace` field on [`ProjectionInput`] exists so a future
//!   path-extracting projection can register without a signature change.
//! * Entries rebuild sequentially and a panicking entry aborts the
//!   remaining projections — fail loud: a projection that cannot rebuild is
//!   state the user would otherwise silently lose.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::models::Message;

/// Shared input for one rebuild pass: the loaded transcript (the fold's
/// only source) and the workspace the session ran in.
#[derive(Debug, Clone)]
pub struct ProjectionInput {
    /// Full transcript in order.
    pub messages: Arc<Vec<Message>>,
    /// Workspace root for path-extracting projections.
    pub workspace: PathBuf,
}

/// One projection's rebuild: folds `input.messages` into the projection's
/// live state (the holder captured at registration).
pub type ProjectionRebuild =
    Arc<dyn Fn(ProjectionInput) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Registry of transcript projections rebuilt together on session load.
///
/// Construct at the load path with the live state holders, then call
/// [`rebuild_all`](Self::rebuild_all) once.
#[derive(Default)]
pub struct ProjectionRegistry {
    entries: Vec<(&'static str, ProjectionRebuild)>,
}

impl ProjectionRegistry {
    /// Register one projection; entries rebuild in registration order.
    pub fn register(&mut self, name: &'static str, rebuild: ProjectionRebuild) {
        self.entries.push((name, rebuild));
    }

    /// Registered projection names, in rebuild order — the load path's
    /// contract surface (tests pin the expected set here).
    pub fn names(&self) -> Vec<&'static str> {
        self.entries.iter().map(|(name, _)| *name).collect()
    }

    /// Rebuild every registered projection from the transcript, in order.
    pub async fn rebuild_all(&self, input: ProjectionInput) {
        for (name, rebuild) in &self.entries {
            rebuild(input.clone()).await;
            tracing::debug!(
                target: "codesmith_projections",
                projection = name,
                "rebuilt projection from transcript"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording_entry(
        name: &'static str,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    ) -> ProjectionRebuild {
        Arc::new(move |input| {
            let log = log.clone();
            Box::pin(async move {
                log.lock().unwrap().push(format!(
                    "{name}:{}:{}",
                    input.messages.len(),
                    input.workspace.display()
                ));
            })
        })
    }

    #[test]
    fn names_preserve_registration_order() {
        let mut registry = ProjectionRegistry::default();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        registry.register("todo", recording_entry("todo", log.clone()));
        registry.register("plan", recording_entry("plan", log));
        assert_eq!(registry.names(), vec!["todo", "plan"]);
    }

    #[tokio::test]
    async fn rebuild_all_runs_every_entry_with_shared_input() {
        let mut registry = ProjectionRegistry::default();
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        registry.register("todo", recording_entry("todo", log.clone()));
        registry.register("plan", recording_entry("plan", log.clone()));

        registry
            .rebuild_all(ProjectionInput {
                messages: Arc::new(vec![Message {
                    role: "user".to_string(),
                    content: Vec::new(),
                }]),
                workspace: PathBuf::from("/tmp/ws"),
            })
            .await;

        // Sequential in registration order, same input for every entry.
        assert_eq!(
            *log.lock().unwrap(),
            vec!["todo:1:/tmp/ws".to_string(), "plan:1:/tmp/ws".to_string()]
        );
    }
}
