//! Disk-based output management with offset tracking for background tasks.
//!
//! Mirrors Claude Code's `TaskOutput` + `outputOffset` pattern:
//! output is written to disk, and the UI reads from the stored offset forward.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
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

    /// Compute the output file path for a task id.
    pub fn output_path_for(&self, task_id: &str) -> PathBuf {
        self.output_dir.join(task_id).join("output.txt")
    }

    /// Write incremental output to a task's output file.
    pub fn append_output(&self, task_id: &str, content: &str) -> Result<()> {
        let path = self.output_path_for(task_id);
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
    /// Returns (content, new_offset).
    pub fn read_from_offset(&self, path: &Path, offset: usize) -> Result<(String, usize)> {
        if !path.exists() {
            return Ok((String::new(), offset));
        }
        let mut file = fs::OpenOptions::new().read(true).open(path)
            .with_context(|| format!("open {:?}", path))?;
        let file_len = file.seek(SeekFrom::End(0))? as usize;
        if offset >= file_len {
            return Ok((String::new(), file_len));
        }
        file.seek(SeekFrom::Start(offset as u64))?;
        let bytes_to_read = file_len - offset;
        let mut buf = vec![0u8; bytes_to_read];
        file.read_exact(&mut buf)?;
        let content = String::from_utf8_lossy(&buf).to_string();
        Ok((content, file_len))
    }

    /// Get the total size of a task's output file.
    pub fn output_size(&self, task_id: &str) -> Result<usize> {
        let path = self.output_path_for(task_id);
        if !path.exists() {
            return Ok(0);
        }
        let meta = fs::metadata(&path).with_context(|| format!("stat {:?}", path))?;
        Ok(meta.len() as usize)
    }

    /// Remove a task's output directory (called during eviction).
    pub fn remove_output(&self, task_id: &str) -> Result<()> {
        let dir = self.output_dir.join(task_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).with_context(|| format!("remove {:?}", dir))?;
        }
        Ok(())
    }
}