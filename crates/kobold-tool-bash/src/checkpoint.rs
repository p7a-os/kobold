use std::ffi::CString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("I/O error during checkpoint operation: {0}")]
    Io(#[from] io::Error),

    #[error("checkpoint '{0}' not found")]
    NotFound(String),

    #[error("checkpoint operation failed: {0}")]
    Other(String),
}

/// Manager for sub-second filesystem checkpoints backed by APFS copy-on-write (`clonefile`).
pub struct CheckpointManager {
    workspace_root: PathBuf,
    checkpoints_dir: PathBuf,
}

impl CheckpointManager {
    pub fn new(workspace_root: PathBuf, checkpoints_dir: PathBuf) -> Self {
        Self {
            workspace_root,
            checkpoints_dir,
        }
    }

    /// Default checkpoints directory within `.kobold/checkpoints`.
    pub fn default_for_workspace(workspace_root: &Path) -> Self {
        let checkpoints_dir = workspace_root.join(".kobold").join("checkpoints");
        Self::new(workspace_root.to_path_buf(), checkpoints_dir)
    }

    /// Create an APFS copy-on-write snapshot of the current workspace directory.
    pub fn create_checkpoint(&self, name: &str) -> Result<PathBuf, CheckpointError> {
        if !self.checkpoints_dir.exists() {
            fs::create_dir_all(&self.checkpoints_dir)?;
        }

        let checkpoint_path = self.checkpoints_dir.join(name);
        if checkpoint_path.exists() {
            fs::remove_dir_all(&checkpoint_path)?;
        }

        clone_directory(&self.workspace_root, &checkpoint_path)?;
        Ok(checkpoint_path)
    }

    /// Rewind workspace state to a previously saved checkpoint.
    pub fn rewind_to_checkpoint(&self, name: &str) -> Result<(), CheckpointError> {
        let checkpoint_path = self.checkpoints_dir.join(name);
        if !checkpoint_path.exists() {
            return Err(CheckpointError::NotFound(name.to_string()));
        }

        // Clean out active workspace files while preserving .git and .kobold
        for entry in fs::read_dir(&self.workspace_root)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str == ".git" || name_str == ".kobold" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }

        // Restore files from checkpoint
        for entry in fs::read_dir(&checkpoint_path)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();
            if name_str == ".git" || name_str == ".kobold" {
                continue;
            }
            let src = entry.path();
            let dst = self.workspace_root.join(&file_name);
            clone_directory(&src, &dst)?;
        }

        Ok(())
    }

    /// List all currently stored checkpoint names.
    pub fn list_checkpoints(&self) -> Result<Vec<String>, CheckpointError> {
        if !self.checkpoints_dir.exists() {
            return Ok(Vec::new());
        }

        let mut names = Vec::new();
        for entry in fs::read_dir(&self.checkpoints_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        Ok(names)
    }

    /// Delete a stored checkpoint.
    pub fn delete_checkpoint(&self, name: &str) -> Result<(), CheckpointError> {
        let checkpoint_path = self.checkpoints_dir.join(name);
        if checkpoint_path.exists() {
            fs::remove_dir_all(checkpoint_path)?;
        }
        Ok(())
    }
}

/// Clone file or directory using APFS CoW (`clonefile`) on macOS, or fallback recursive copy.
fn clone_directory(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if let (Ok(src_c), Ok(dst_c)) = (
            CString::new(src.as_os_str().as_encoded_bytes()),
            CString::new(dst.as_os_str().as_encoded_bytes()),
        ) {
            let res = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };
            if res == 0 {
                return Ok(());
            }
        }
    }

    // Fallback: standard recursive copy if clonefile is unavailable or fails
    fallback_copy_recursive(src, dst)
}

fn fallback_copy_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_src = entry.path();
            let file_name = entry.file_name();
            let name_str = file_name.to_string_lossy();

            // Skip checkpoints directory itself if nested inside workspace
            if name_str == ".kobold" {
                continue;
            }

            let child_dst = dst.join(file_name);
            fallback_copy_recursive(&child_src, &child_dst)?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
    }
    Ok(())
}
