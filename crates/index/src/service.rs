//! Index orchestration: the [`IndexServiceApi`] implementation tying the
//! walk, the backend, and the store together.
//!
//! Freshness model: every query first runs a **lazy incremental refresh**
//! bounded by a [`RefreshBudget`] — walk the workspace, diff against the
//! store on (path, mtime, size), re-extract dirty files, purge deleted
//! ones, and report whatever exceeded the budget as `stale_files`. When a
//! refresh hits its budget, a low-priority background task continues in
//! chunks until the index is fresh, so the agent turn is never blocked by
//! a full build.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use chrono::Utc;
use tokio::task::spawn_blocking;

use crate::backend::{
    Extraction, IndexBackend, IndexBackendConfig, IndexServiceApi, RefreshOutcome,
};
use crate::config::IndexConfig;
use crate::registry::IndexBackendRegistry;
use crate::store::IndexStore;
use crate::types::{
    FileEntry, FileQuery, IndexStats, Language, Occurrence, RefreshBudget, Symbol, SymbolQuery,
};
use crate::walk::{MAX_EXTRACT_BYTES, WalkEntry, live_paths, walk_workspace};

/// Chunk budget used by the background completion task.
const BACKGROUND_BUDGET: RefreshBudget = RefreshBudget {
    max_files: 2048,
    max_duration: Duration::from_secs(15),
};

/// Safety cap on background completion iterations (each chunk walks the
/// workspace once); prevents unbounded churn on constantly-changing trees.
const BACKGROUND_MAX_ITERATIONS: usize = 64;

/// Result of one refresh pass (internal bookkeeping).
struct RefreshReport {
    refreshed: usize,
    stale: usize,
    duration_ms: u64,
}

struct Shared {
    root: PathBuf,
    store: Arc<IndexStore>,
    backend: Option<Arc<dyn IndexBackend>>,
    files_enabled: bool,
    stats: std::sync::Mutex<IndexStats>,
    background_running: AtomicBool,
}

/// The concrete [`IndexServiceApi`]: one instance per workspace root.
pub struct IndexService {
    inner: Arc<Shared>,
    default_budget: RefreshBudget,
}

impl IndexService {
    /// Compose a service from explicit parts (host assembly and tests).
    /// The host normally goes through [`build_service`] instead.
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        store: IndexStore,
        backend: Option<Arc<dyn IndexBackend>>,
        files_enabled: bool,
        default_budget: RefreshBudget,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Shared {
                root: root.into(),
                store: Arc::new(store),
                backend,
                files_enabled,
                stats: std::sync::Mutex::new(IndexStats::default()),
                background_running: AtomicBool::new(false),
            }),
            default_budget,
        })
    }

    async fn refresh_once(&self, budget: RefreshBudget) -> Result<RefreshReport> {
        let inner = self.inner.clone();
        spawn_blocking(move || refresh_blocking(&inner, budget))
            .await
            .context("index refresh task failed")?
    }

    /// Query-path refresh: bounded pass now, background completion later.
    async fn refresh_before_query(&self) -> Result<()> {
        let report = self.refresh_once(self.default_budget).await?;
        if report.stale > 0 {
            spawn_background_completion(self.inner.clone());
        }
        Ok(())
    }
}

/// Build the per-workspace service from config + registry (host assembly).
/// Fails fast when the index is disabled or config does not validate —
/// hosts should not call this for disabled configs.
pub fn build_service(
    workspace_root: &Path,
    cfg: &IndexConfig,
    registry: &IndexBackendRegistry,
) -> Result<Arc<dyn IndexServiceApi>> {
    if !cfg.is_enabled() {
        bail!("index disabled by [index] enabled = false");
    }
    cfg.validate(registry)?;
    let store = IndexStore::open_default(workspace_root)?;
    let backend = if cfg.symbols.is_enabled() {
        let languages: Vec<Language> = Language::all()
            .iter()
            .copied()
            .filter(|lang| cfg.symbols.language_enabled(*lang))
            .collect();
        let backend = registry.build(
            cfg.symbols.backend_id(),
            &IndexBackendConfig {
                workspace_root: workspace_root.to_path_buf(),
                languages,
            },
        )?;
        store.set_backend(backend.id())?;
        Some(backend)
    } else {
        None
    };
    Ok(IndexService::new(
        workspace_root,
        store,
        backend,
        cfg.files.is_enabled(),
        RefreshBudget {
            max_files: 256,
            max_duration: Duration::from_millis(cfg.effective_refresh_budget_ms()),
        },
    ))
}

fn refresh_blocking(shared: &Shared, budget: RefreshBudget) -> Result<RefreshReport> {
    let started = Instant::now();
    let walked = walk_workspace(&shared.root)?;
    let existing: HashMap<String, (i64, u64)> = shared
        .store
        .all_files()?
        .into_iter()
        .map(|record| {
            (
                record.entry.path,
                (record.entry.mtime_ms, record.entry.size),
            )
        })
        .collect();
    shared.store.delete_missing(&live_paths(&walked))?;

    let dirty: Vec<&WalkEntry> = walked
        .iter()
        .filter(|entry| match existing.get(&entry.rel_path) {
            Some((mtime_ms, size)) => *mtime_ms != entry.mtime_ms || *size != entry.size,
            None => true,
        })
        .collect();
    let total_dirty = dirty.len();

    let deadline = started + budget.max_duration;
    let mut refreshed = 0;
    for entry in &dirty {
        if refreshed >= budget.max_files || Instant::now() >= deadline {
            break;
        }
        index_one(shared, entry)?;
        refreshed += 1;
    }

    let stale = total_dirty - refreshed;
    let (files, symbols) = shared.store.counts()?;
    let backend_id = shared
        .backend
        .as_ref()
        .map(|b| b.id().to_string())
        .unwrap_or_default();
    let report = RefreshReport {
        refreshed,
        stale,
        duration_ms: started.elapsed().as_millis() as u64,
    };
    *shared.stats.lock().expect("index stats mutex poisoned") = IndexStats {
        files,
        symbols,
        stale_files: stale as u64,
        last_refresh: Some(Utc::now()),
        backend: backend_id,
    };
    Ok(report)
}

/// Index one file: inventory row always; symbols when a backend supports
/// the language and the file is within the extraction size cap. Extraction
/// failures degrade to inventory-only rows (never fail the whole refresh).
fn index_one(shared: &Shared, entry: &WalkEntry) -> Result<()> {
    let abs = shared.root.join(&entry.rel_path);
    let mut extraction = Extraction::default();
    if let (Some(backend), Some(lang)) = (shared.backend.as_ref(), entry.language)
        && backend.supported_languages().contains(&lang)
        && entry.size <= MAX_EXTRACT_BYTES
    {
        match std::fs::read_to_string(&abs) {
            Ok(source) => match backend.extract(&abs, &source, lang) {
                Ok(ex) => extraction = ex,
                Err(err) => {
                    tracing::warn!(path = %entry.rel_path, %err, "index extraction failed; inventory-only row");
                }
            },
            Err(err) => {
                tracing::debug!(path = %entry.rel_path, %err, "index read failed (binary?); inventory-only row");
            }
        }
    }
    shared.store.replace_file(
        &FileEntry {
            path: entry.rel_path.clone(),
            mtime_ms: entry.mtime_ms,
            size: entry.size,
            language: entry.language,
        },
        &extraction,
    )
}

/// Continue refreshing in background chunks until fresh (or the iteration
/// cap). Only one completion task runs at a time.
fn spawn_background_completion(inner: Arc<Shared>) {
    if inner.background_running.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        for _ in 0..BACKGROUND_MAX_ITERATIONS {
            let chunk_inner = inner.clone();
            let report =
                match spawn_blocking(move || refresh_blocking(&chunk_inner, BACKGROUND_BUDGET))
                    .await
                {
                    Ok(Ok(report)) => report,
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "index background refresh failed");
                        break;
                    }
                    Err(err) => {
                        tracing::warn!(%err, "index background task join failed");
                        break;
                    }
                };
            if report.stale == 0 {
                break;
            }
            // Yield between chunks: the foreground refresh stays responsive
            // and a hot edit loop does not spin the walker.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        inner.background_running.store(false, Ordering::SeqCst);
    });
}

#[async_trait]
impl IndexServiceApi for IndexService {
    async fn search_symbols(&self, query: SymbolQuery) -> Result<Vec<Symbol>> {
        if self.inner.backend.is_none() {
            bail!("symbol index is disabled ([index.symbols] enabled = false)");
        }
        self.refresh_before_query().await?;
        let store = self.inner.store.clone();
        spawn_blocking(move || store.search_symbols(&query))
            .await
            .context("symbol search task failed")?
    }

    async fn find_definition(&self, name: &str) -> Result<Vec<Symbol>> {
        if self.inner.backend.is_none() {
            bail!("symbol index is disabled ([index.symbols] enabled = false)");
        }
        self.refresh_before_query().await?;
        let store = self.inner.store.clone();
        let name = name.to_string();
        spawn_blocking(move || store.find_definition(&name))
            .await
            .context("find definition task failed")?
    }

    async fn find_references(&self, name: &str) -> Result<Vec<Occurrence>> {
        if self.inner.backend.is_none() {
            bail!("symbol index is disabled ([index.symbols] enabled = false)");
        }
        self.refresh_before_query().await?;
        let store = self.inner.store.clone();
        let name = name.to_string();
        spawn_blocking(move || store.find_occurrences(&name))
            .await
            .context("find references task failed")?
    }

    async fn list_files(&self, query: FileQuery) -> Result<Vec<FileEntry>> {
        if !self.inner.files_enabled {
            bail!("file inventory is disabled ([index.files] enabled = false)");
        }
        self.refresh_before_query().await?;
        let store = self.inner.store.clone();
        spawn_blocking(move || store.list_files(&query))
            .await
            .context("list files task failed")?
    }

    async fn refresh(&self, budget: RefreshBudget) -> Result<RefreshOutcome> {
        let report = self.refresh_once(budget).await?;
        if report.stale > 0 {
            spawn_background_completion(self.inner.clone());
        }
        Ok(RefreshOutcome {
            stats: self.stats(),
            refreshed_files: report.refreshed,
            duration_ms: report.duration_ms,
        })
    }

    fn stats(&self) -> IndexStats {
        self.inner
            .stats
            .lock()
            .expect("index stats mutex poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::IndexBackendFactory;
    use crate::types::{Language, Location, SymbolKind};
    use std::sync::atomic::AtomicUsize;

    /// Stub backend: counts extracts, one symbol per file named after the
    /// file stem.
    struct CountingBackend {
        extracts: AtomicUsize,
    }

    struct CountingFactory;

    impl IndexBackendFactory for CountingFactory {
        fn id(&self) -> &str {
            "counting"
        }
        fn capabilities(&self) -> &'static [crate::backend::IndexCapability] {
            &[crate::backend::IndexCapability::Symbols]
        }
        fn build(&self, _cfg: &IndexBackendConfig) -> Result<Arc<dyn IndexBackend>> {
            Ok(Arc::new(CountingBackend {
                extracts: AtomicUsize::new(0),
            }))
        }
    }

    impl CountingBackend {
        fn symbol_for(&self, file: &Path) -> Symbol {
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Symbol {
                name: format!("Stub_{stem}"),
                kind: SymbolKind::Function,
                container: None,
                path: file.to_string_lossy().to_string(),
                location: Location {
                    line: 1,
                    col: 1,
                    end_line: 1,
                    end_col: 2,
                },
                signature: None,
            }
        }
    }

    impl IndexBackend for CountingBackend {
        fn id(&self) -> &str {
            "counting"
        }
        fn supported_languages(&self) -> &[Language] {
            Language::all()
        }
        fn extract(&self, file: &Path, _source: &str, _lang: Language) -> Result<Extraction> {
            self.extracts.fetch_add(1, Ordering::SeqCst);
            let symbol = self.symbol_for(file);
            let name = symbol.name.clone();
            let line = symbol.location.line;
            Ok(Extraction {
                symbols: vec![symbol],
                occurrences: vec![Occurrence {
                    name,
                    role: crate::types::OccurrenceRole::Definition,
                    path: file.to_string_lossy().to_string(),
                    line,
                }],
            })
        }
    }

    fn service_for(tmp: &tempfile::TempDir) -> (Arc<IndexService>, Arc<CountingBackend>) {
        let backend: Arc<CountingBackend> = Arc::new(CountingBackend {
            extracts: AtomicUsize::new(0),
        });
        let store =
            IndexStore::open(tmp.path(), &tmp.path().join(".index/index.db")).expect("store");
        let service = IndexService::new(
            tmp.path(),
            store,
            Some(backend.clone() as Arc<dyn IndexBackend>),
            true,
            RefreshBudget {
                max_files: 4096,
                max_duration: Duration::from_secs(10),
            },
        );
        (service, backend)
    }

    fn write(tmp: &tempfile::TempDir, rel: &str, content: &str) {
        let path = tmp.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, content).expect("write");
    }

    #[tokio::test]
    async fn refresh_indexes_files_and_serves_queries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp, "src/alpha.rs", "fn alpha() {}");
        write(&tmp, "src/beta.py", "def beta(): pass");
        let (service, _backend) = service_for(&tmp);

        let outcome = service
            .refresh(RefreshBudget {
                max_files: 100,
                max_duration: Duration::from_secs(5),
            })
            .await
            .expect("refresh");
        assert_eq!(outcome.refreshed_files, 2);
        assert_eq!(outcome.stats.stale_files, 0);
        assert_eq!(outcome.stats.files, 2);
        assert_eq!(outcome.stats.symbols, 2);

        let hits = service
            .search_symbols(SymbolQuery {
                query: "stub_alpha".into(),
                ..SymbolQuery::default()
            })
            .await
            .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Stub_alpha");

        let defs = service.find_definition("stub_beta").await.expect("defs");
        assert_eq!(defs.len(), 1);

        let files = service
            .list_files(FileQuery {
                extension: Some("rs".into()),
                ..FileQuery::default_bounded()
            })
            .await
            .expect("files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/alpha.rs");
    }

    #[tokio::test]
    async fn editing_one_file_reextracts_only_that_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp, "a.rs", "fn a() {}");
        write(&tmp, "b.rs", "fn b() {}");
        let (service, backend) = service_for(&tmp);

        service
            .refresh(RefreshBudget {
                max_files: 100,
                max_duration: Duration::from_secs(5),
            })
            .await
            .expect("first refresh");
        assert_eq!(backend.extracts.load(Ordering::SeqCst), 2);

        // Grow a.rs (size change marks it dirty even when mtime granularity
        // misses the edit).
        write(&tmp, "a.rs", "fn a() { /* more */ }");
        service
            .refresh(RefreshBudget {
                max_files: 100,
                max_duration: Duration::from_secs(5),
            })
            .await
            .expect("second refresh");
        assert_eq!(
            backend.extracts.load(Ordering::SeqCst),
            3,
            "only the edited file is re-extracted"
        );
    }

    #[tokio::test]
    async fn deleting_a_file_purges_its_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp, "gone.rs", "fn gone() {}");
        write(&tmp, "stay.rs", "fn stay() {}");
        let (service, _backend) = service_for(&tmp);
        service
            .refresh(RefreshBudget {
                max_files: 100,
                max_duration: Duration::from_secs(5),
            })
            .await
            .expect("refresh");
        assert_eq!(service.stats().files, 2);

        std::fs::remove_file(tmp.path().join("gone.rs")).expect("remove");
        service
            .refresh(RefreshBudget {
                max_files: 100,
                max_duration: Duration::from_secs(5),
            })
            .await
            .expect("refresh after delete");
        assert_eq!(service.stats().files, 1);
        assert!(
            service
                .find_definition("stub_gone")
                .await
                .expect("defs")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn budget_truncation_reports_stale_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp, "a.rs", "fn a() {}");
        write(&tmp, "b.rs", "fn b() {}");
        write(&tmp, "c.rs", "fn c() {}");
        let (service, _backend) = service_for(&tmp);

        let outcome = service
            .refresh(RefreshBudget {
                max_files: 1,
                max_duration: Duration::from_secs(5),
            })
            .await
            .expect("refresh");
        assert_eq!(outcome.refreshed_files, 1);
        assert_eq!(outcome.stats.stale_files, 2);
        assert_eq!(outcome.stats.files, 1);

        // A generous budget catches up.
        let outcome = service
            .refresh(RefreshBudget {
                max_files: 100,
                max_duration: Duration::from_secs(5),
            })
            .await
            .expect("catch-up refresh");
        assert_eq!(outcome.stats.stale_files, 0);
        assert_eq!(outcome.stats.files, 3);
    }

    #[tokio::test]
    async fn disabled_symbols_errors_but_inventory_works() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(&tmp, "a.rs", "fn a() {}");
        let store =
            IndexStore::open(tmp.path(), &tmp.path().join(".index/index.db")).expect("store");
        let service = IndexService::new(
            tmp.path(),
            store,
            None,
            true,
            RefreshBudget {
                max_files: 100,
                max_duration: Duration::from_secs(5),
            },
        );

        let err = service
            .search_symbols(SymbolQuery {
                query: "x".into(),
                ..SymbolQuery::default()
            })
            .await
            .expect_err("symbols disabled");
        assert!(err.to_string().contains("disabled"), "{}", err);

        let files = service
            .list_files(FileQuery::default_bounded())
            .await
            .expect("files");
        assert_eq!(files.len(), 1);
    }

    #[tokio::test]
    async fn build_service_fails_fast_on_disabled_or_invalid_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut registry = IndexBackendRegistry::new();
        registry.register(Arc::new(CountingFactory));

        let disabled: IndexConfig = toml::from_str("enabled = false").expect("cfg");
        let err = build_service(tmp.path(), &disabled, &registry)
            .map(|_| ())
            .expect_err("disabled");
        assert!(err.to_string().contains("disabled"), "{}", err);

        let bad_backend: IndexConfig =
            toml::from_str("[symbols]\nbackend = \"nope\"").expect("cfg");
        let err = build_service(tmp.path(), &bad_backend, &registry)
            .map(|_| ())
            .expect_err("unknown backend");
        assert!(err.to_string().contains("nope"), "{}", err);
    }
}
