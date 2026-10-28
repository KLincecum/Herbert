//! Serialize plugin server work independently of the client request budget.
//! An uncertain call retains its intent. A timeout is not server completion.
use fs2::FileExt;
use herbert_contract::Diagnostic;
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReconcileInput {
    pub invocation_id: String,
    pub confirmed_server_stopped: bool,
    pub evidence: String,
}

pub(crate) fn inspect(root: &Path) -> Result<serde_json::Value, Diagnostic> {
    match std::fs::read(root.join("server-collection-active.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| error("The server collection intent is invalid.")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Value::Null),
        Err(_) => Err(error("Cannot read the server collection intent.")),
    }
}

/// Refuse a stranded invocation before discovery or helper installation.
/// A live owner retains the lock; normal collection admission still waits for it.
pub(crate) fn check_unresolved(root: &Path) -> Result<(), Diagnostic> {
    let lock = match OpenOptions::new().read(true).write(true)
        .open(root.join("server-collection.lock")) {
        Ok(lock) => lock,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(error("Cannot inspect the server collection lock.")),
    };
    match lock.try_lock_exclusive() {
        Ok(()) => {
            let intent = inspect(root)?;
            if !intent.is_null() {
                return Err(error(&format!(
                    "Server collection {} needs recovery. Its server outcome is unresolved. No new helpers were installed. Inspect and reconcile that run before starting another server collection.",
                    intent["runId"].as_str().unwrap_or("unknown")
                )));
            }
            Ok(())
        }
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(()),
        Err(_) => Err(error("Cannot inspect the server collection lock.")),
    }
}

/// A remote timeout has no general cancellation API. Require an explicit,
/// recorded operator confirmation after server-side completion or recovery.
pub(crate) fn reconcile(
    root: &Path,
    input: ReconcileInput,
) -> Result<serde_json::Value, Diagnostic> {
    if !input.confirmed_server_stopped || input.evidence.trim().is_empty() {
        return Err(error(
            "Confirm that the server invocation has stopped and record the evidence.",
        ));
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("server-collection.lock"))
        .map_err(|_| error("No server collection lock is available for reconciliation."))?;
    lock.try_lock_exclusive()
        .map_err(|_| error("The owning host is still awaiting the server response."))?;
    let mut intent = inspect(root)?;
    if intent.is_null() || intent["invocationId"] != input.invocation_id {
        return Err(error(
            "The recorded invocation does not match. Inspect the current collection before reconciliation.",
        ));
    }
    intent["state"] = json!("operatorConfirmedStopped");
    intent["evidence"] = json!(input.evidence);
    crate::storage::atomic_write(
        &root.join("server-collection-reconciled.json"),
        &serde_json::to_vec_pretty(&intent).expect("serializable reconciliation"),
    )?;
    std::fs::remove_file(root.join("server-collection-active.json"))
        .map_err(|_| error("Cannot clear the reconciled server collection intent."))?;
    Ok(intent)
}

pub(crate) struct Permit {
    _lock: File,
    intent: PathBuf,
}
fn error(message: &str) -> Diagnostic {
    Diagnostic::new("SERVER_COLLECTION_UNRESOLVED", message)
}
/// Keep resources available while their server invocation can still be active.
pub(crate) fn ensure_run_settled(root: &Path, run: &str) -> Result<(), Diagnostic> {
    let path = root.join("server-collection-active.json");
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(error("Cannot read the server collection intent.")),
    };
    let intent: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| error("The server collection intent is invalid. Preserve it for recovery."))?;
    if intent["runId"] == run {
        return Err(error(
            "This run has an active or uncertain server collection. Confirm completion before removing its helpers or files.",
        ));
    }
    Ok(())
}
pub(crate) async fn acquire(
    root: &Path,
    run: &str,
    plugin: &str,
    cancel: &CancellationToken,
) -> Result<Permit, Diagnostic> {
    std::fs::create_dir_all(root)
        .map_err(|_| error("Cannot open server collection coordination."))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("server-collection.lock"))
        .map_err(|_| error("Cannot open the server collection lock."))?;
    loop {
        if cancel.is_cancelled() {
            return Err(Diagnostic::new(
                "CANCELED",
                "Canceled while waiting for the server collection slot.",
            ));
        }
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                // File locks have no asynchronous notification API. This polling
                // interval controls admission latency, not workload or wait duration.
                tokio::select! {
                    _ = cancel.cancelled() => {},
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
                }
            }
            Err(_) => return Err(error("Cannot acquire the server collection lock.")),
        }
    }
    let intent = root.join("server-collection-active.json");
    if intent.exists() {
        return Err(error(
            "A prior server collection has an uncertain outcome. Reconcile its recorded run before starting another server collection.",
        ));
    }
    crate::storage::atomic_write(&intent, &serde_json::to_vec_pretty(&json!({
        "schemaVersion":1, "invocationId":uuid::Uuid::new_v4().to_string(), "runId":run, "plugin":plugin,
        "state":"awaitingServerResponse"
    })).expect("serializable collection intent"))?;
    Ok(Permit {
        _lock: lock,
        intent,
    })
}
impl Permit {
    pub(crate) fn settle<T>(self, result: &Result<T, Diagnostic>) -> Result<(), Diagnostic> {
        // Explicit admission refusals prove that the collection did not start.
        if result.is_ok()
            || matches!(result, Err(e) if matches!(e.code.as_str(), "ACCESS_DENIED" | "NOT_FOUND" | "THROTTLED"))
        {
            self.completed()?;
        }
        Ok(())
    }
    /// Only a completed native response permits the next server collection.
    /// Dropping a permit after cancellation or an error preserves the intent.
    pub(crate) fn completed(self) -> Result<(), Diagnostic> {
        std::fs::remove_file(&self.intent).map_err(|_| {
            error("The server replied, but its collection intent could not be cleared.")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn different_plugins_wait_and_canceled_waiters_do_not_remove_active_intent() {
        let root = tempfile::tempdir().unwrap();
        let first = acquire(root.path(), "run-a", "C03", &CancellationToken::new())
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let path = root.path().to_path_buf();
        let child_cancel = cancel.clone();
        let waiting =
            tokio::spawn(async move { acquire(&path, "run-b", "C04", &child_cancel).await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        cancel.cancel();
        assert!(matches!(waiting.await.unwrap(), Err(e) if e.code == "CANCELED"));
        assert!(root.path().join("server-collection-active.json").exists());
        first.completed().unwrap();
        acquire(root.path(), "run-b", "C04", &CancellationToken::new())
            .await
            .unwrap()
            .completed()
            .unwrap();
        assert!(!root.path().join("server-collection-active.json").exists());
    }
    #[tokio::test]
    async fn uncertain_response_survives_permit_drop_and_blocks_other_plugins() {
        let root = tempfile::tempdir().unwrap();
        assert!(check_unresolved(root.path()).is_ok());
        drop(
            acquire(root.path(), "run-a", "C04", &CancellationToken::new())
                .await
                .unwrap(),
        );
        assert!(matches!(check_unresolved(root.path()), Err(e) if e.code == "SERVER_COLLECTION_UNRESOLVED" && e.message.contains("run-a")));
        assert!(
            matches!(acquire(root.path(), "run-b", "C03", &CancellationToken::new()).await,
            Err(e) if e.code == "SERVER_COLLECTION_UNRESOLVED")
        );
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join("server-collection-active.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(saved["runId"], "run-a");
        assert!(ensure_run_settled(root.path(), "run-a").is_err());
        assert!(ensure_run_settled(root.path(), "unrelated-run").is_ok());
        let id = saved["invocationId"].as_str().unwrap().to_string();
        assert!(
            reconcile(
                root.path(),
                ReconcileInput {
                    invocation_id: "wrong".into(),
                    confirmed_server_stopped: true,
                    evidence: "Test evidence".into()
                }
            )
            .is_err()
        );
        assert!(
            reconcile(
                root.path(),
                ReconcileInput {
                    invocation_id: id.clone(),
                    confirmed_server_stopped: false,
                    evidence: "Test evidence".into()
                }
            )
            .is_err()
        );
        reconcile(
            root.path(),
            ReconcileInput {
                invocation_id: id,
                confirmed_server_stopped: true,
                evidence: "Native completion confirmed in test".into(),
            },
        )
        .unwrap();
        assert!(ensure_run_settled(root.path(), "run-a").is_ok());
        assert!(inspect(root.path()).unwrap().is_null());
        assert!(check_unresolved(root.path()).is_ok());
    }
    #[tokio::test]
    async fn completed_collection_admits_waiting_plugin_and_active_owner_cannot_be_reconciled() {
        let root = tempfile::tempdir().unwrap();
        let first = acquire(root.path(), "run-a", "C03", &CancellationToken::new())
            .await
            .unwrap();
        assert!(check_unresolved(root.path()).is_ok());
        let id = inspect(root.path()).unwrap()["invocationId"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            reconcile(
                root.path(),
                ReconcileInput {
                    invocation_id: id,
                    confirmed_server_stopped: true,
                    evidence: "Cannot override an active owner".into()
                }
            )
            .is_err()
        );
        let path = root.path().to_path_buf();
        let second = tokio::spawn(async move {
            acquire(&path, "run-b", "C04", &CancellationToken::new())
                .await
                .unwrap()
                .completed()
                .unwrap();
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        first.completed().unwrap();
        second.await.unwrap();
        assert!(inspect(root.path()).unwrap().is_null());
    }
}
