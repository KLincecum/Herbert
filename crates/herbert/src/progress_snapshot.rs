//! Compact progress excludes recovery data and repeated per-artifact file lists.
use crate::storage::RunJournal;
use serde_json::{Value, json};
use std::sync::{Mutex, TryLockError};

/// Do not make the progress screen wait for file writes or checkpoints.
/// Counts can use the previous snapshot. Activity and batches are read separately.
pub(crate) fn active_summary(
    storage: &Mutex<crate::storage::Storage>,
    cache: &Mutex<Option<Value>>,
    run_id: &str,
) -> Result<Value, herbert_contract::Diagnostic> {
    let unavailable = || herbert_contract::Diagnostic::new(
        "RUN_STATE_FAILED", "Run state is unavailable.",
    );
    match storage.try_lock() {
        Ok(storage) => {
            let status = summary(&storage.journal);
            *cache.lock().map_err(|_| unavailable())? = Some(status.clone());
            Ok(status)
        }
        Err(TryLockError::WouldBlock) => {
            let mut status = cache.lock().map_err(|_| unavailable())?.clone()
                .unwrap_or_else(|| json!({"runId":run_id,"phase":"preparing"}));
            status["countsPending"] = json!(true);
            Ok(status)
        }
        Err(TryLockError::Poisoned(_)) => Err(unavailable()),
    }
}
pub fn summary(j: &RunJournal) -> Value {
    let (file_count, file_bytes) = file_totals(j);
    json!({"runId":j.run_id,"phase":j.phase,"scope":j.scope,"collectionMethod":j.collection_method,"serverChangeAuthority":j.server_change_authority,"categories":j.categories,"categoryNames":j.category_names,"categoryOutcomes":j.category_outcomes,"discoveryFinished":j.discovery_finished,"discovered":j.discovered,"targets":j.targets,"diagnostics":j.diagnostics,"pluginSettings":j.plugin_settings,"pluginHashes":j.plugin_hashes,"pluginSelectionContracts":j.plugin_selection_contracts,
 "artifacts":j.artifacts.iter().map(|a| {
     // Issue review needs actual file evidence. Keep clean-item polls compact;
     // include paths only for items with a diagnostic or incomplete processing.
     let affected = a.diagnostics.iter().any(|d|d.code != "REFERENCE_SCAN_COVERAGE")
         || j.diagnostics.iter().any(|d| d.code != "REFERENCE_SCAN_COVERAGE" && d.identity.as_ref()==Some(&a.identity))
         || !matches!(a.acquisition, herbert_contract::Completeness::Collected
             | herbert_contract::Completeness::VerifiedAbsent
             | herbert_contract::Completeness::VerifiedNotApplicable
             | herbert_contract::Completeness::Excluded)
         || a.processing != "derived";
     let mut value=json!({"identity":a.identity,"acquisition":a.acquisition,"processing":a.processing,"diagnostics":a.diagnostics.iter().filter(|d|d.code != "REFERENCE_SCAN_COVERAGE").collect::<Vec<_>>(),"referenceCoverage":a.diagnostics.iter().filter(|d|d.code == "REFERENCE_SCAN_COVERAGE").collect::<Vec<_>>()});
     if affected { value["files"]=json!(a.files); }
     value
 }).collect::<Vec<_>>(),
 "acquisitions":j.acquisitions.last().into_iter().collect::<Vec<_>>(),
 "statistics":j.statistics,"fileCount":file_count,"fileBytes":file_bytes})
}
/// Success presentation is derived from the committed overall inventory. An
/// empty page, failed branch or exact retrieval must not become a zero-match claim.
pub(crate) fn discovery_messages(status: &Value) -> Value {
    let mut messages = serde_json::Map::new();
    if status["scope"] == "fullBackup" {
        for category in status["categories"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if status["discoveryFinished"][category] == true
                && status["categoryOutcomes"][category] == "verifiedAbsent"
                && status["discovered"][category].as_array().is_some_and(Vec::is_empty)
            {
                messages.insert(category.into(), json!("No eligible items matched the run selection."));
            }
        }
    }
    Value::Object(messages)
}
/// Count each saved path once, including readable output beside native files.
fn file_totals(j: &RunJournal) -> (usize, u64) {
    let files = j
        .acquisitions
        .iter()
        .chain(j.artifacts.iter().flat_map(|a| a.files.iter()))
        .map(|file| (file.path.as_str(), file.bytes))
        .collect::<std::collections::BTreeMap<_, _>>();
    (files.len(), files.values().sum())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_selection_message_requires_complete_successful_overall_inventory() {
        let mut status = json!({"scope":"fullBackup","categories":["fixture"],
            "discoveryFinished":{"fixture":false},"categoryOutcomes":{"fixture":"verifiedAbsent"},
            "discovered":{"fixture":[]}});
        assert_eq!(discovery_messages(&status), json!({}));
        status["discoveryFinished"]["fixture"] = json!(true);
        assert!(discovery_messages(&status)["fixture"].is_string());
        // A final empty page must not erase a root selected on an earlier page.
        status["discovered"]["fixture"] = json!([{"earlierPageRoot":true}]);
        assert_eq!(discovery_messages(&status), json!({}));
        status["discovered"]["fixture"] = json!([]);
        for outcome in ["discoveryIncomplete", "discoveryFailed", "discovered"] {
            status["categoryOutcomes"]["fixture"] = json!(outcome);
            assert_eq!(discovery_messages(&status), json!({}));
        }
        status["categoryOutcomes"]["fixture"] = json!("verifiedAbsent");
        status["scope"] = json!("targeted");
        assert_eq!(discovery_messages(&status), json!({}));
    }
    #[test]
    fn active_counts_do_not_wait_for_the_writer_and_refresh_after_release() {
        let directory = tempfile::tempdir().unwrap();
        let journal = crate::storage::new_journal("progress".into());
        let storage = Mutex::new(crate::storage::Storage::open(directory.path(), false, journal).unwrap());
        let cache = Mutex::new(None);
        let initial = active_summary(&storage, &cache, "progress").unwrap();
        let mut writer = storage.lock().unwrap();
        writer.journal.phase = "cleaningArtifacts".into();
        let pending = active_summary(&storage, &cache, "progress").unwrap();
        assert_eq!(pending["phase"], initial["phase"]);
        assert_eq!(pending["countsPending"], true);
        // A first poll is also nonblocking when no prior counts exist.
        assert_eq!(active_summary(&storage, &Mutex::new(None), "progress").unwrap()["countsPending"], true);
        drop(writer);
        let refreshed = active_summary(&storage, &cache, "progress").unwrap();
        assert_eq!(refreshed["phase"], "cleaningArtifacts");
        assert!(refreshed.get("countsPending").is_none());
    }
    #[test]
    fn summary_keeps_counts_and_diagnostics_without_recovery_payload() {
        let mut j = crate::storage::new_journal("progress".into());
        j.plugin_settings.insert("fixture".into(), json!({"prefix":" private selection "}));
        j.plugin_hashes.insert("fixture".into(), "exact-module".into());
        j.references
            .push(json!({"largePrivateRecoveryBody":"must not be polled"}));
        j.acquisitions.push(herbert_contract::FileRef {
            path: "one.json".into(),
            sha256: "hash".into(),
            bytes: 42,
            role: "native".into(),
        });
        j.artifacts.push(serde_json::from_value(json!({
            "identity":{"category":"fixture","kind":"fixture","scope":{"type":"company","company":"Test"},"keys":{}},
            "classification":"selected","acquisition":"collected","integrity":"verified","processing":"derived",
            "files":[j.acquisitions[0],{"path":"Readable/output.txt","sha256":"other","bytes":58,"role":"derived"}],
            "dependencies":[],"diagnostics":[],"sourceConsistency":"snapshot"
        })).unwrap());
        j.statistics = Some(json!({"elapsedMs":1234,"finished":true}));
        let restored: RunJournal = serde_json::from_value(serde_json::to_value(&j).unwrap()).unwrap();
        assert_eq!(summary(&restored)["statistics"]["elapsedMs"], 1234);
        let s = summary(&j);
        assert_eq!(s["pluginSettings"]["fixture"]["prefix"], " private selection ");
        assert_eq!(s["pluginHashes"]["fixture"], "exact-module");
        assert_eq!(s["fileCount"], 2);
        assert_eq!(s["fileBytes"], 100);
        assert_eq!(s["acquisitions"][0]["path"], "one.json");
        assert!(s.get("references").is_none());
        assert!(s.get("nativeArtifacts").is_none());
        assert!(s["artifacts"][0].get("files").is_none());
        let mut diagnostic=herbert_contract::Diagnostic::new("MISSING_DEPENDENCY","A dependency was not returned.");
        diagnostic.identity=Some(j.artifacts[0].identity.clone());
        j.diagnostics.push(diagnostic);
        let with_issue=summary(&j);
        assert_eq!(with_issue["artifacts"][0]["files"].as_array().unwrap().len(),2);
        assert!(with_issue.get("references").is_none());
    }
}
