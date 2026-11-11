//! The `directoryPicker` remote — the vendored workspace picker's in-app
//! directory browser (`client-ui-directory-picker-browse`).
//!
//! The vendor package renders the dialog and owns the flow; the bridge only
//! answers its two primitives: `list` (one absolute directory level plus its
//! breadcrumb ancestry) and `createDirectory` (one child of an existing
//! directory). Paths cross the wire absolute — the client never joins
//! segments itself — and every level lists child *directories* only, hidden
//! rows included for the dialog's own show-hidden toggle.
//!
//! `pick` is the native-picker half (`client-ui-directory-picker-native`),
//! which this desktop does not compose: it answers
//! `directory-picker/unavailable` instead of pretending a chooser opened.
//!
//! Known limitations: the listing is a point-in-time snapshot (no watch);
//! `hidden` follows the POSIX dot convention on every platform; a level
//! larger than [`MAX_ENTRIES`] renders its name-sorted prefix with
//! `truncated: true`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{rpc_error, rpc_ok};

/// Complete rows one listing serves; hidden rows count toward the bound.
const MAX_ENTRIES: usize = 2_000;

/// One directory row: a listing child or a breadcrumb ancestor.
fn entry(name: String, path: PathBuf, hidden: bool) -> Value {
    json!({
        "name": name,
        "path": path.to_string_lossy(),
        "hidden": hidden,
    })
}

/// Is this a directory row worth listing (a directory, or a symlink to one)?
fn directory_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// One listing level plus its ancestry, built from a canonical directory.
fn listing_of(dir: &Path, home: &Path) -> Result<Value, (String, String)> {
    let read = std::fs::read_dir(dir).map_err(|err| {
        (
            "directory-picker/unreadable".to_string(),
            format!("read {}: {err}", dir.display()),
        )
    })?;
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for child in read {
        let child = child.map_err(|err| {
            (
                "directory-picker/unreadable".to_string(),
                format!("read {}: {err}", dir.display()),
            )
        })?;
        // Follow symlinks: a symlink to a directory is a jump target, the
        // same way the sidebar's files tree lists it by its target type.
        let is_dir = std::fs::metadata(child.path())
            .map(|meta| meta.is_dir())
            .unwrap_or(false);
        if !is_dir {
            continue;
        }
        if let Some(name) = directory_name(&child.path()) {
            entries.push((name, child.path()));
        }
    }
    let mut truncated = false;
    if entries.len() > MAX_ENTRIES {
        entries.truncate(MAX_ENTRIES);
        truncated = true;
    }
    entries.sort_by(|left, right| {
        left.0
            .to_lowercase()
            .cmp(&right.0.to_lowercase())
            .then_with(|| left.0.cmp(&right.0))
    });
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|(name, path)| {
            let hidden = name.starts_with('.');
            entry(name, path, hidden)
        })
        .collect();

    // Ancestor chain from the filesystem root to this directory inclusive.
    let mut crumbs: Vec<Value> = Vec::new();
    let mut current = PathBuf::new();
    for component in dir.components() {
        current.push(component.as_os_str());
        let name = match component {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                current.to_string_lossy().into_owned()
            }
            std::path::Component::Normal(name) => name.to_string_lossy().into_owned(),
            _ => continue,
        };
        crumbs.push(entry(name, current.clone(), false));
    }
    if crumbs.is_empty() {
        crumbs.push(entry(
            dir.to_string_lossy().into_owned(),
            dir.to_path_buf(),
            false,
        ));
    }

    Ok(json!({
        "path": dir.to_string_lossy(),
        "home": home.to_string_lossy(),
        "crumbs": crumbs,
        "entries": entries,
        "truncated": truncated,
    }))
}

/// The host account's home directory; the dialog's "Home" crumb.
fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Canonicalize one requested level. Absent names the home directory; a
/// relative or missing path is unreadable, never silently re-rooted.
fn canonical_level(path: Option<&str>) -> Result<PathBuf, (String, String)> {
    let requested = match path {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => home_dir(),
    };
    if !requested.is_absolute() {
        return Err((
            "directory-picker/unreadable".to_string(),
            format!("{} is not an absolute path", requested.display()),
        ));
    }
    let canonical = std::fs::canonicalize(&requested).map_err(|err| {
        (
            "directory-picker/unreadable".to_string(),
            format!("open {}: {err}", requested.display()),
        )
    })?;
    if !canonical.is_dir() {
        return Err((
            "directory-picker/unreadable".to_string(),
            format!("{} is not a directory", canonical.display()),
        ));
    }
    Ok(canonical)
}

/// `directoryPicker/list` — one absolute level; absent path names home.
pub async fn list(args: &Value) -> Value {
    let args = args.get("request").unwrap_or(args);
    let requested = args.get("path").and_then(Value::as_str).map(str::to_string);
    let home = home_dir();
    match tokio::task::spawn_blocking(move || {
        let dir = canonical_level(requested.as_deref())?;
        listing_of(&dir, &home)
    })
    .await
    {
        Ok(Ok(listing)) => rpc_ok(listing),
        Ok(Err((code, message))) => rpc_error(&code, message),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("directoryPicker/list join failed: {err}"),
        ),
    }
}

/// One child name must be exactly one path component.
fn valid_child_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// `directoryPicker/createDirectory` — create one child directory and return
/// its absolute path.
pub async fn create_directory(args: &Value) -> Value {
    let args = args.get("request").unwrap_or(args);
    let Some(parent) = args.get("path").and_then(Value::as_str) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "directoryPicker/createDirectory without path",
        );
    };
    let Some(name) = args.get("name").and_then(Value::as_str) else {
        return rpc_error(
            "gateway/arguments-invalid",
            "directoryPicker/createDirectory without name",
        );
    };
    if !valid_child_name(name) {
        return rpc_error(
            "directory-picker/create-failed",
            format!("{name:?} is not one directory name"),
        );
    }
    let parent = parent.to_string();
    let name = name.to_string();
    match tokio::task::spawn_blocking(move || {
        let parent = canonical_level(Some(&parent))?;
        let target = parent.join(&name);
        match std::fs::create_dir(&target) {
            Ok(()) => Ok(target),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Err((
                "directory-picker/exists".to_string(),
                format!("{} already exists", target.display()),
            )),
            Err(err) => Err((
                "directory-picker/create-failed".to_string(),
                format!("create {}: {err}", target.display()),
            )),
        }
    })
    .await
    {
        Ok(Ok(target)) => rpc_ok(json!(target.to_string_lossy())),
        Ok(Err((code, message))) => rpc_error(&code, message),
        Err(err) => rpc_error(
            "gateway/internal",
            format!("directoryPicker/createDirectory join failed: {err}"),
        ),
    }
}

/// `directoryPicker/pick` — the native chooser is not composed in this shell.
pub fn pick() -> Value {
    rpc_error(
        "directory-picker/unavailable",
        "this desktop composes the in-app directory browser, not a native chooser",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cds-directories-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn listing_lists_directories_only_with_hidden_flags_and_crumbs() {
        let dir = temp_dir("list");
        std::fs::create_dir(dir.join("beta")).unwrap();
        std::fs::create_dir(dir.join("Alpha")).unwrap();
        std::fs::create_dir(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("file.txt"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("beta"), dir.join("link")).unwrap();

        let listing = listing_of(&dir, &dir).unwrap();
        let names: Vec<&str> = listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect();
        // Case-insensitive name order; files are not rows; hidden rows ARE.
        #[cfg(unix)]
        assert_eq!(names, vec![".hidden", "Alpha", "beta", "link"]);
        #[cfg(not(unix))]
        assert_eq!(names, vec![".hidden", "Alpha", "beta"]);
        assert_eq!(listing["entries"][0]["hidden"], true);
        assert_eq!(listing["entries"][1]["hidden"], false);
        // The last crumb is the listed directory itself.
        let crumbs = listing["crumbs"].as_array().unwrap();
        assert_eq!(
            crumbs.last().unwrap()["path"],
            dir.to_string_lossy().as_ref()
        );
        assert_eq!(crumbs[0]["path"], "/");
        assert_eq!(listing["home"], dir.to_string_lossy().as_ref());
        assert_eq!(listing["truncated"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_marks_truncation_at_the_cap() {
        let dir = temp_dir("truncated");
        for index in 0..(MAX_ENTRIES + 2) {
            std::fs::create_dir(dir.join(format!("d{index:05}"))).unwrap();
        }
        let listing = listing_of(&dir, &dir).unwrap();
        assert_eq!(listing["truncated"], true);
        assert_eq!(
            listing["entries"].as_array().unwrap().len(),
            MAX_ENTRIES,
            "the bound is complete rows, not a page"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn child_names_must_be_one_component() {
        assert!(valid_child_name("new-folder"));
        assert!(!valid_child_name(""));
        assert!(!valid_child_name("."));
        assert!(!valid_child_name(".."));
        assert!(!valid_child_name("a/b"));
        assert!(!valid_child_name("a\\b"));
    }

    #[test]
    fn canonical_level_requires_an_absolute_existing_directory() {
        let dir = temp_dir("canonical");
        let canonical = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(
            canonical_level(Some(dir.to_str().unwrap())).unwrap(),
            canonical
        );
        let file = dir.join("file");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            canonical_level(Some(file.to_str().unwrap())).unwrap_err().0,
            "directory-picker/unreadable"
        );
        assert_eq!(
            canonical_level(Some("relative")).unwrap_err().0,
            "directory-picker/unreadable"
        );
        assert_eq!(
            canonical_level(Some("/definitely/not/here")).unwrap_err().0,
            "directory-picker/unreadable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn create_directory_answers_absolute_path_and_refuses_duplicates() {
        let dir = temp_dir("create");
        let args = json!({ "path": dir.to_string_lossy(), "name": "made" });
        let answer = create_directory(&args).await;
        assert_eq!(answer["ok"], true, "{answer}");
        // macOS resolves /var to /private/var: the answer is the canonical
        // parent's child.
        let made = std::fs::canonicalize(&dir).unwrap().join("made");
        assert!(made.is_dir());
        assert_eq!(answer["value"], made.to_string_lossy().as_ref());

        let duplicate = create_directory(&args).await;
        assert_eq!(duplicate["ok"], false);
        assert_eq!(duplicate["error"]["code"], "directory-picker/exists");

        let traversal = create_directory(&json!({
            "path": dir.to_string_lossy(), "name": "../escape"
        }))
        .await;
        assert_eq!(traversal["error"]["code"], "directory-picker/create-failed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn list_answers_the_home_level_when_path_is_absent() {
        let listing = list(&json!({})).await;
        assert_eq!(listing["ok"], true, "{listing}");
        assert_eq!(
            listing["value"]["path"],
            home_dir().to_string_lossy().as_ref()
        );
    }
}
