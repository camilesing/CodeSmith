use std::fs::OpenOptions;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use fd_lock::RwLock;
use serde_json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AmendError {
    #[error("prefix rule requires at least one token")]
    EmptyPrefix,
    #[error("policy path has no parent: {path}")]
    MissingParent { path: PathBuf },
    #[error("failed to create policy directory {dir}: {source}")]
    CreatePolicyDir {
        dir: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to format prefix tokens: {source}")]
    SerializePrefix { source: serde_json::Error },
    #[error("generated rule failed to round-trip the policy parser (not written): {source}")]
    RuleRoundTrip { source: super::error::Error },
    #[error("failed to open policy file {path}: {source}")]
    OpenPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write to policy file {path}: {source}")]
    WritePolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to lock policy file {path}: {source}")]
    LockPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to seek policy file {path}: {source}")]
    SeekPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read policy file {path}: {source}")]
    ReadPolicyFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read metadata for policy file {path}: {source}")]
    PolicyMetadata {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Note this thread uses advisory file locking and performs blocking I/O, so it should be used with
/// [`tokio::task::spawn_blocking`] when called from an async context.
pub fn blocking_append_allow_prefix_rule(
    policy_path: &Path,
    prefix: &[String],
) -> Result<(), AmendError> {
    // An EMPTY first token (empty slice, `["", "flag"]`) only fails at
    // eval time inside `PrefixPattern::new` — reject it up front.
    if prefix.first().is_none_or(String::is_empty) {
        return Err(AmendError::EmptyPrefix);
    }

    let tokens = prefix
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| AmendError::SerializePrefix { source })?;
    let pattern = format!("[{}]", tokens.join(", "));
    let rule = format!(r#"prefix_rule(pattern={pattern}, decision="allow")"#);

    // Fail fast: the whole policy file is one Starlark module, so one
    // unparseable appended line (serde_json emits `\u0001`-style escapes
    // for control characters, which Starlark's lexer rejects) would brick
    // every previously-accumulated rule at load time. Round-trip the
    // generated line through the parser before it reaches disk.
    super::parser::PolicyParser::new()
        .parse("amend-roundtrip", &rule)
        .map_err(|source| AmendError::RuleRoundTrip { source })?;

    let dir = policy_path
        .parent()
        .ok_or_else(|| AmendError::MissingParent {
            path: policy_path.to_path_buf(),
        })?;
    match std::fs::create_dir(dir) {
        Ok(()) => {}
        Err(ref source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(AmendError::CreatePolicyDir {
                dir: dir.to_path_buf(),
                source,
            });
        }
    }
    append_locked_line(policy_path, &rule)
}

fn append_locked_line(policy_path: &Path, line: &str) -> Result<(), AmendError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(policy_path)
        .map_err(|source| AmendError::OpenPolicyFile {
            path: policy_path.to_path_buf(),
            source,
        })?;
    let mut file = RwLock::new(file);
    let mut file = file.write().map_err(|source| AmendError::LockPolicyFile {
        path: policy_path.to_path_buf(),
        source,
    })?;

    let len = file
        .metadata()
        .map_err(|source| AmendError::PolicyMetadata {
            path: policy_path.to_path_buf(),
            source,
        })?
        .len();

    // Ensure file ends in a newline before appending.
    if len > 0 {
        file.seek(SeekFrom::End(-1))
            .map_err(|source| AmendError::SeekPolicyFile {
                path: policy_path.to_path_buf(),
                source,
            })?;
        let mut last = [0; 1];
        file.read_exact(&mut last)
            .map_err(|source| AmendError::ReadPolicyFile {
                path: policy_path.to_path_buf(),
                source,
            })?;

        if last[0] != b'\n' {
            file.write_all(b"\n")
                .map_err(|source| AmendError::WritePolicyFile {
                    path: policy_path.to_path_buf(),
                    source,
                })?;
        }
    }

    file.write_all(format!("{line}\n").as_bytes())
        .map_err(|source| AmendError::WritePolicyFile {
            path: policy_path.to_path_buf(),
            source,
        })?;
    // Durability: without the fsync a crash can silently lose the appended
    // rule the user just approved into the policy.
    file.sync_all()
        .map_err(|source| AmendError::WritePolicyFile {
            path: policy_path.to_path_buf(),
            source,
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::PolicyParser;
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn appends_rule_and_creates_directories() {
        let tmp = tempdir().expect("create temp dir");
        let policy_path = tmp.path().join("rules").join("default.rules");

        blocking_append_allow_prefix_rule(
            &policy_path,
            &[String::from("echo"), String::from("Hello, world!")],
        )
        .expect("append rule");

        let contents = std::fs::read_to_string(&policy_path).expect("default.rules should exist");
        assert_eq!(
            contents,
            r#"prefix_rule(pattern=["echo", "Hello, world!"], decision="allow")
"#
        );
    }

    #[test]
    fn rejects_empty_and_empty_first_token_prefixes() {
        let tmp = tempdir().expect("create temp dir");
        let policy_path = tmp.path().join("rules").join("default.rules");

        for prefix in [
            vec![],
            vec![String::new()],
            vec![String::new(), String::from("flag")],
        ] {
            let err = blocking_append_allow_prefix_rule(&policy_path, &prefix)
                .expect_err("empty prefix must be rejected");
            assert!(
                matches!(err, AmendError::EmptyPrefix),
                "expected EmptyPrefix, got {err:?}"
            );
        }
        assert!(
            !policy_path.exists(),
            "nothing may be written for a rejected prefix"
        );
    }

    #[test]
    fn written_rules_stay_parseable_with_pathological_tokens() {
        // The round-trip gate runs every generated line through the policy
        // parser before it reaches disk, so the file can never accumulate
        // an unparseable line. Control characters (serde-escaped as
        // `\u0001`-style escapes), quotes, backslashes, and non-ASCII must
        // either append cleanly — leaving the whole file loadable — or be
        // rejected with `RuleRoundTrip` before any write.
        let tmp = tempdir().expect("create temp dir");
        let policy_path = tmp.path().join("rules").join("default.rules");
        std::fs::create_dir_all(policy_path.parent().unwrap()).expect("policy dir");
        std::fs::write(&policy_path, "").expect("seed empty file");

        let tricky = vec![
            String::from("git"),
            String::from("quote\"back\\slash"),
            String::from("bad\u{1}token"),
            String::from("中文"),
        ];
        blocking_append_allow_prefix_rule(&policy_path, &tricky)
            .expect("tricky tokens round-trip under the extended dialect");

        // The load-time invariant: the whole file (seed + append) parses.
        let contents = std::fs::read_to_string(&policy_path).unwrap();
        PolicyParser::new()
            .parse("amend-test", &contents)
            .expect("accumulated policy file stays parseable");
        assert!(contents.contains("git"), "{contents}");
    }

    #[test]
    fn appends_rule_without_duplicate_newline() {
        let tmp = tempdir().expect("create temp dir");
        let policy_path = tmp.path().join("rules").join("default.rules");
        std::fs::create_dir_all(policy_path.parent().unwrap()).expect("create policy dir");
        std::fs::write(
            &policy_path,
            r#"prefix_rule(pattern=["ls"], decision="allow")
"#,
        )
        .expect("write seed rule");

        blocking_append_allow_prefix_rule(
            &policy_path,
            &[String::from("echo"), String::from("Hello, world!")],
        )
        .expect("append rule");

        let contents = std::fs::read_to_string(&policy_path).expect("read policy");
        assert_eq!(
            contents,
            r#"prefix_rule(pattern=["ls"], decision="allow")
prefix_rule(pattern=["echo", "Hello, world!"], decision="allow")
"#
        );
    }

    #[test]
    fn inserts_newline_when_missing_before_append() {
        let tmp = tempdir().expect("create temp dir");
        let policy_path = tmp.path().join("rules").join("default.rules");
        std::fs::create_dir_all(policy_path.parent().unwrap()).expect("create policy dir");
        std::fs::write(
            &policy_path,
            r#"prefix_rule(pattern=["ls"], decision="allow")"#,
        )
        .expect("write seed rule without newline");

        blocking_append_allow_prefix_rule(
            &policy_path,
            &[String::from("echo"), String::from("Hello, world!")],
        )
        .expect("append rule");

        let contents = std::fs::read_to_string(&policy_path).expect("read policy");
        assert_eq!(
            contents,
            r#"prefix_rule(pattern=["ls"], decision="allow")
prefix_rule(pattern=["echo", "Hello, world!"], decision="allow")
"#
        );
    }
}
