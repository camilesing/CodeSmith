//! Disk-based output management with offset tracking for background tasks.
//!
//! Mirrors Claude Code's `TaskOutput` + `outputOffset` pattern:
//! output is written to disk, and the UI reads from the stored offset forward.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};

/// Read the single byte at `offset` (seeking) for UTF-8 boundary probing.
fn buf_probe(file: &mut std::fs::File, offset: usize) -> std::io::Result<u8> {
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte)?;
    Ok(byte[0])
}
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Manages on-disk output files for background tasks with offset tracking.
pub struct BackgroundTaskOutputManager {
    output_dir: PathBuf,
}

impl BackgroundTaskOutputManager {
    /// Create a new output manager rooted at `output_dir`.
    pub fn new(output_dir: PathBuf) -> Self {
        Self { output_dir }
    }

    /// Compute the output file path for a task id. Single choke point for
    /// the traversal guard: the id is joined into a path here, so callers
    /// cannot get a path for an unchecked id — siblings below must not
    /// join the id themselves (the one exception is `remove_output`,
    /// which owns its own guarded join).
    pub fn output_path_for(&self, task_id: &str) -> Result<PathBuf> {
        codesmith_agent_runtime::utils::require_safe_path_component(task_id)?;
        Ok(self.output_dir.join(task_id).join("output.txt"))
    }

    /// Write incremental output to a task's output file.
    #[allow(dead_code)]
    pub fn append_output(&self, task_id: &str, content: &str) -> Result<()> {
        let path = self.output_path_for(task_id)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create dir {:?}", parent))?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {:?}", path))?;
        file.write_all(content.as_bytes())
            .with_context(|| format!("write to {:?}", path))?;
        Ok(())
    }

    /// Read output from a file starting at the given offset.
    /// Returns (content, new_offset). The path must come from
    /// [`Self::output_path_for`] (the validated join) — this helper
    /// takes it pre-built so the UI can hold one path across polls.
    pub fn read_from_offset(&self, path: &Path, offset: usize) -> Result<(String, usize)> {
        if !path.exists() {
            return Ok((String::new(), offset));
        }
        let mut file = fs::OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("open {:?}", path))?;
        let file_len = file.seek(SeekFrom::End(0))? as usize;
        if offset >= file_len {
            return Ok((String::new(), file_len));
        }
        // A caller offset landing mid-character (stored across polls)
        // would start the returned slice on a UTF-8 continuation byte;
        // skip up to 3 leading continuation bytes defensively.
        let mut start = offset;
        while start < file_len && (buf_probe(&mut file, start)? & 0xC0) == 0x80 {
            start += 1;
        }
        file.seek(SeekFrom::Start(start as u64))?;
        let bytes_to_read = file_len - start;
        let mut buf = vec![0u8; bytes_to_read];
        file.read_exact(&mut buf)?;
        // Torn append at EOF: this is a concurrent read-while-append flow,
        // so the tail can end mid-character. `from_utf8_lossy` would bake a
        // permanent U+FFFD into the content and the returned offset would
        // advance past the torn bytes. Stop at the last complete character
        // instead so the incomplete tail is re-read on the next poll.
        match String::from_utf8(buf) {
            Ok(content) => Ok((content, file_len)),
            Err(err) => {
                let valid = err.utf8_error().valid_up_to();
                let content = String::from_utf8_lossy(&err.as_bytes()[..valid]).into_owned();
                Ok((content, start + valid))
            }
        }
    }

    /// Get the total size of a task's output file.
    #[allow(dead_code)]
    pub fn output_size(&self, task_id: &str) -> Result<usize> {
        let path = self.output_path_for(task_id)?;
        if !path.exists() {
            return Ok(0);
        }
        let meta = fs::metadata(&path).with_context(|| format!("stat {:?}", path))?;
        Ok(meta.len() as usize)
    }

    /// Remove a task's output directory (called during eviction).
    pub fn remove_output(&self, task_id: &str) -> Result<()> {
        codesmith_agent_runtime::utils::require_safe_path_component(task_id)?;
        let dir = self.output_dir.join(task_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("remove {:?}", dir))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn output_path_for_constructs_correct_path() {
        let mgr = BackgroundTaskOutputManager::new(PathBuf::from("/tmp/bg"));
        let path = mgr.output_path_for("task-1").expect("path");
        assert_eq!(path, PathBuf::from("/tmp/bg/task-1/output.txt"));
    }

    #[test]
    fn append_output_creates_file_and_writes() {
        let tmp = tempdir().expect("tempdir");
        let mgr = BackgroundTaskOutputManager::new(tmp.path().to_path_buf());
        mgr.append_output("t1", "hello").expect("append");
        let path = mgr.output_path_for("t1").expect("path");
        let content = std::fs::read_to_string(&path).expect("read");
        assert_eq!(content, "hello");
    }

    #[test]
    fn append_output_appends_to_existing() {
        let tmp = tempdir().expect("tempdir");
        let mgr = BackgroundTaskOutputManager::new(tmp.path().to_path_buf());
        mgr.append_output("t1", "hello").expect("append1");
        mgr.append_output("t1", " world").expect("append2");
        let path = mgr.output_path_for("t1").expect("path");
        let content = std::fs::read_to_string(&path).expect("read");
        assert_eq!(content, "hello world");
    }

    #[test]
    fn read_from_offset_reads_from_position() {
        let tmp = tempdir().expect("tempdir");
        let mgr = BackgroundTaskOutputManager::new(tmp.path().to_path_buf());
        mgr.append_output("t1", "0123456789").expect("write");
        let path = mgr.output_path_for("t1").expect("path");
        let (content, new_offset) = mgr.read_from_offset(&path, 5).expect("read");
        assert_eq!(content, "56789");
        assert_eq!(new_offset, 10);
    }

    #[test]
    fn read_from_offset_returns_empty_for_missing_file() {
        let tmp = tempdir().expect("tempdir");
        let mgr = BackgroundTaskOutputManager::new(tmp.path().to_path_buf());
        let path = mgr.output_path_for("nonexistent").expect("path");
        let (content, offset) = mgr.read_from_offset(&path, 0).expect("read");
        assert_eq!(content, "");
        assert_eq!(offset, 0);
    }

    #[test]
    fn read_from_offset_returns_empty_when_offset_at_end() {
        let tmp = tempdir().expect("tempdir");
        let mgr = BackgroundTaskOutputManager::new(tmp.path().to_path_buf());
        mgr.append_output("t1", "short").expect("write");
        let path = mgr.output_path_for("t1").expect("path");
        let (content, _) = mgr.read_from_offset(&path, 5).expect("read");
        assert_eq!(content, "");
    }

    #[test]
    fn output_size_returns_zero_for_missing() {
        let tmp = tempdir().expect("tempdir");
        let mgr = BackgroundTaskOutputManager::new(tmp.path().to_path_buf());
        assert_eq!(mgr.output_size("nonexistent").expect("size"), 0);
    }

    #[test]
    fn remove_output_deletes_directory() {
        let tmp = tempdir().expect("tempdir");
        let mgr = BackgroundTaskOutputManager::new(tmp.path().to_path_buf());
        mgr.append_output("t1", "data").expect("write");
        assert!(mgr.output_path_for("t1").expect("path").exists());
        mgr.remove_output("t1").expect("remove");
        assert!(
            !mgr.output_path_for("t1")
                .expect("path")
                .parent()
                .unwrap()
                .exists()
        );
    }
}
