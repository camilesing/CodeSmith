//! Test-only resolution of the fixture cdylib path (§F5b).
//!
//! `build.rs` emits `<target>/<profile>/libextensions_fixture_dylib.<ext>` —
//! the un-hashed export link. Cargo creates that link only when the fixture
//! is built as a direct target; under `cargo test --workspace` the fixture is
//! built purely as a dev-dependency, the cdylib lands in
//! `<target>/<profile>/deps/` (un-hashed copy and/or content-hashed artifact)
//! and no export link is created. Fresh environments (CI, new clones) never
//! have the export link, so stale local artifacts must not be load-bearing:
//! the hint and the `deps/` candidates compete on mtime and the newest wins.

/// Locate the fixture cdylib. Panics when neither the un-hashed export link
/// nor a `deps/` artifact exists.
pub(crate) fn fixture_dylib_path() -> std::path::PathBuf {
    let hinted = std::path::PathBuf::from(env!("CODESMITH_FIXTURE_DYLIB"));

    let deps_dir = hinted
        .parent()
        .expect("CODESMITH_FIXTURE_DYLIB has a parent directory")
        .join("deps");
    // Match both `libextensions_fixture_dylib.dylib` and the content-hashed
    // `libextensions_fixture_dylib-<hash>.dylib` spellings (`.so`/`.dll`
    // likewise), skipping dep-info/obj files via the suffix filter.
    let stem = format!("{}extensions_fixture_dylib", std::env::consts::DLL_PREFIX);
    let suffix = std::env::consts::DLL_SUFFIX;

    let mut candidates: Vec<(std::path::PathBuf, std::time::SystemTime)> =
        std::fs::read_dir(&deps_dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with(&stem)
                            && name.ends_with(suffix)
                            && entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                    })
                    .filter_map(|entry| {
                        let modified = entry.metadata().ok()?.modified().ok()?;
                        Some((entry.path(), modified))
                    })
                    .collect()
            })
            .unwrap_or_default();
    // The hint competes on mtime too: a direct build at rev A leaves the
    // export link behind, and preferring it unconditionally would shadow
    // the fresh content-hashed artifact a later dev-dep build produced.
    if let Ok(modified) = std::fs::metadata(&hinted).and_then(|m| m.modified()) {
        candidates.push((hinted.clone(), modified));
    }
    candidates.sort_by_key(|(_, modified)| *modified);

    candidates
        .pop()
        .map(|(path, _)| path)
        .unwrap_or_else(|| panic!("fixture cdylib not found: tried {hinted:?} and {deps_dir:?}"))
}
