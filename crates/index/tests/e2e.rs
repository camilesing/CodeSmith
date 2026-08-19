//! End-to-end smoke: the real tree-sitter backend driven through the
//! `IndexService` over a fixture repository. The `tree-sitter` feature is
//! enabled for this crate's test builds via the self dev-dependency, so
//! `default_registry()` carries the built-in backend here.

use std::path::Path;
use std::time::Duration;

use codesmith_index::{
    FileQuery, IndexBackendConfig, IndexConfig, IndexService, IndexServiceApi, IndexStore,
    Language, OccurrenceRole, RefreshBudget, SymbolKind, SymbolQuery, default_registry,
};

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, content).expect("write");
}

#[tokio::test]
async fn fixture_repo_symbol_navigation_end_to_end() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write(
        tmp.path(),
        "src/lib.rs",
        "pub struct ToolRegistry;\nimpl ToolRegistry {\n    pub fn build(&self) -> u32 { 0 }\n}\npub fn main() {}\n",
    );
    write(
        tmp.path(),
        "src/client.py",
        "class Client:\n    def connect(self):\n        pass\n",
    );
    write(tmp.path(), ".gitignore", "ignored.rs\n");
    write(tmp.path(), "ignored.rs", "fn hidden() {}\n");

    let registry = default_registry();
    let config: IndexConfig = toml::from_str("").expect("default index config");
    config
        .validate(&registry)
        .expect("defaults validate against default registry");

    let backend = registry
        .build(
            config.symbols.backend_id(),
            &IndexBackendConfig {
                workspace_root: tmp.path().to_path_buf(),
                languages: vec![Language::Rust, Language::Python],
            },
        )
        .expect("build tree-sitter backend");
    assert_eq!(backend.id(), "tree-sitter");

    let store = IndexStore::open(tmp.path(), &tmp.path().join(".index/index.db")).expect("store");
    let service = IndexService::new(
        tmp.path(),
        store,
        Some(backend),
        true,
        RefreshBudget {
            max_files: 100,
            max_duration: Duration::from_secs(10),
        },
    );

    // Symbol search: struct found, method attributed to its container.
    let hits = service
        .search_symbols(SymbolQuery {
            query: "registry".into(),
            ..SymbolQuery::default()
        })
        .await
        .expect("search");
    assert!(
        hits.iter()
            .any(|s| s.name == "ToolRegistry" && s.kind == SymbolKind::Struct),
        "{hits:?}"
    );
    let build = service
        .search_symbols(SymbolQuery {
            query: "build".into(),
            ..SymbolQuery::default()
        })
        .await
        .expect("search")
        .into_iter()
        .find(|s| s.name == "build")
        .expect("build symbol");
    assert_eq!(build.kind, SymbolKind::Method);
    assert_eq!(build.container.as_deref(), Some("ToolRegistry"));
    assert_eq!(build.path, "src/lib.rs");

    // Cross-language: python class definition.
    let defs = service.find_definition("Client").await.expect("defs");
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].kind, SymbolKind::Class);
    assert_eq!(defs[0].path, "src/client.py");

    // Lexical references: the `impl ToolRegistry` header is a reference site.
    let refs = service.find_references("ToolRegistry").await.expect("refs");
    assert!(
        refs.iter()
            .any(|r| r.role == OccurrenceRole::Reference && r.path == "src/lib.rs" && r.line == 2),
        "{refs:?}"
    );

    // .gitignore respected: the ignored file never enters the index.
    let hidden = service
        .search_symbols(SymbolQuery {
            query: "hidden".into(),
            ..SymbolQuery::default()
        })
        .await
        .expect("search");
    assert!(hidden.is_empty(), "{hidden:?}");

    // Incremental freshness: editing lib.rs converges on the next query and
    // stats report the backend that produced the data.
    write(
        tmp.path(),
        "src/lib.rs",
        "pub struct ToolRegistry;\nimpl ToolRegistry {\n    pub fn build(&self) -> u32 { 1 }\n}\npub fn main() {}\n// trailing growth for a size change\n",
    );
    let files = service
        .list_files(FileQuery {
            extension: Some("rs".into()),
            ..FileQuery::default_bounded()
        })
        .await
        .expect("files");
    assert!(files.iter().any(|f| f.path == "src/lib.rs"), "{files:?}");
    let stats = service.stats();
    assert_eq!(stats.backend, "tree-sitter");
    assert!(stats.files >= 2, "{stats:?}");
    assert!(
        stats.stale_files == 0,
        "refresh should have caught up: {stats:?}"
    );
}
