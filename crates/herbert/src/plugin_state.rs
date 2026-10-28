//! A module owns one portable state file beside its actual Wasm file.
//!
//! The host treats its contents as opaque JSON. It does not interpret settings,
//! remember an expected module roster, or put credentials in this file.
use fs2::FileExt;
use herbert_contract::Diagnostic;
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

fn problem(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn reject_link(path: &Path) -> Result<(), Diagnostic> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(problem(
            "PLUGIN_STATE_PATH_INVALID",
            "The module state path must not be a symbolic link.",
        )),
        Ok(metadata) if !metadata.is_file() => Err(problem(
            "PLUGIN_STATE_PATH_INVALID",
            "The module state path must be an ordinary file.",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(problem(
            "PLUGIN_STATE_UNAVAILABLE",
            "The module state path cannot be inspected.",
        )),
    }
}

pub(crate) struct StateFile {
    path: PathBuf,
    lock: PathBuf,
}
impl StateFile {
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { lock: path.with_extension("lock"), path }
    }
    pub(crate) fn for_module(module: &Path) -> Self {
        Self {
            path: module.with_extension("state.json"),
            lock: module.with_extension("state.lock"),
        }
    }
    pub(crate) fn read(&self) -> Result<Value, Diagnostic> {
        reject_link(&self.path)?;
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
                problem(
                    "PLUGIN_STATE_INVALID",
                    "The module state file contains invalid JSON.",
                )
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
            Err(_) => Err(problem(
                "PLUGIN_STATE_UNAVAILABLE",
                "The module state file cannot be read.",
            )),
        }
    }
    /// The state transition compares the original value under a stable file lock.
    /// If someone edits the config during a call, preserve that edit and report it.
    pub(crate) fn replace(&self, before: &Value, after: &Value) -> Result<(), Diagnostic> {
        reject_link(&self.path)?;
        reject_link(&self.lock)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock)
            .map_err(|_| {
                problem(
                    "PLUGIN_STATE_UNAVAILABLE",
                    "The module state lock cannot be opened.",
                )
            })?;
        lock.try_lock_exclusive().map_err(|_| {
            problem(
                "PLUGIN_STATE_BUSY",
                "Another process owns this module's state update.",
            )
        })?;
        if &self.read()? != before {
            return Err(problem(
                "PLUGIN_STATE_CONFLICT",
                "The module state changed during the call. The external edit was preserved.",
            ));
        }
        let parent = self.path.parent().ok_or_else(|| {
            problem(
                "PLUGIN_STATE_PATH_INVALID",
                "The module state directory is unavailable.",
            )
        })?;
        // tempfile persists through native Windows APIs. Resolve the existing
        // parent to its extended absolute form, as storage::atomic_write does,
        // so valid state paths do not inherit the legacy MAX_PATH boundary.
        // Keep the lock, conflict check and link rejection above unchanged.
        #[cfg(target_os = "windows")]
        let parent = fs::canonicalize(parent).map_err(|_| {
            problem("PLUGIN_STATE_UNAVAILABLE", "The module state directory cannot be resolved.")
        })?;
        #[cfg(target_os = "windows")]
        let destination = parent.join(self.path.file_name().ok_or_else(|| {
            problem("PLUGIN_STATE_PATH_INVALID", "The module state filename is unavailable.")
        })?);
        #[cfg(not(target_os = "windows"))]
        let destination = &self.path;
        let bytes = serde_json::to_vec_pretty(after)
            .map_err(|_| problem("PLUGIN_STATE_INVALID", "The module returned invalid state."))?;
        let mut staged = tempfile::NamedTempFile::new_in(&parent).map_err(|_| {
            problem(
                "PLUGIN_STATE_UNAVAILABLE",
                "The module state cannot be staged.",
            )
        })?;
        staged
            .write_all(&bytes)
            .and_then(|_| staged.write_all(b"\n"))
            .and_then(|_| staged.as_file().sync_all())
            .map_err(|_| {
                problem(
                    "PLUGIN_STATE_UNAVAILABLE",
                    "The module state cannot be written.",
                )
            })?;
        staged.persist(&destination).map_err(|_| {
            problem(
                "PLUGIN_STATE_UNAVAILABLE",
                "The module state cannot be replaced.",
            )
        })?;
        // Independent readback separates a successful write call from actual state.
        if self.read()? != *after {
            return Err(problem(
                "PLUGIN_STATE_READBACK_FAILED",
                "The module state did not match its independent readback.",
            ));
        }
        File::open(&parent).and_then(|file| file.sync_all()).ok(); // Directory flush is platform-dependent.
        Ok(())
    }
}


#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::windows::ffi::OsStrExt;

    #[test]
    fn state_replace_supports_windows_boundary_and_deep_paths() {
        let root = tempfile::tempdir().unwrap();
        for length in [259, 260, 320] {
            let mut parent = root.path().join(format!("case-{length}"));
            let leaf = "decompiler-preferences.json";
            while parent.as_os_str().encode_wide().count() + leaf.len() + 66 < length {
                parent = parent.join("nested-state-directory");
            }
            let padding = length - parent.as_os_str().encode_wide().count() - leaf.len() - 2;
            parent = parent.join("s".repeat(padding));
            fs::create_dir_all(&parent).unwrap();
            let path = parent.join(leaf);
            assert_eq!(path.as_os_str().encode_wide().count(), length);
            let state = StateFile::at(path.clone());
            let first = json!({"revision":1,"value":"preserved"});
            let second = json!({"revision":2,"value":"updated"});
            state.replace(&Value::Null, &first).unwrap();
            assert_eq!(state.read().unwrap(), first);
            state.replace(&first, &second).unwrap();
            assert_eq!(state.read().unwrap(), second);
            assert_eq!(state.replace(&first, &json!({})).unwrap_err().code, "PLUGIN_STATE_CONFLICT");
            assert_eq!(state.read().unwrap(), second);
            let lock = OpenOptions::new().read(true).write(true).open(&state.lock).unwrap();
            lock.try_lock_exclusive().unwrap();
            assert_eq!(state.replace(&second, &first).unwrap_err().code, "PLUGIN_STATE_BUSY");
            drop(lock);
            let extended = StateFile::at(fs::canonicalize(&parent).unwrap().join(leaf));
            extended.replace(&second, &first).unwrap();
            assert_eq!(state.read().unwrap(), first);
            assert_eq!(fs::read_dir(&parent).unwrap().count(), 2, "Only state and stable lock remain");
        }
    }
}
