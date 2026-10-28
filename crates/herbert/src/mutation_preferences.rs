//! One-time host preference migration. Preferences are never server-change authority.
use crate::plugin_state::StateFile;
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::path::Path;

fn unavailable(message: &str) -> Diagnostic {
    Diagnostic::new("SERVER_CHANGE_MIGRATION_REQUIRED", message)
}

/// Persist the original and intended values before changing either preference file.
/// Each existing StateFile transition compares under its own stable lock and reads
/// back the result. An interrupted migration accepts only its original or intended
/// value; an unrelated edit is preserved and requires reconciliation.
pub(crate) fn initialize(root: &Path) -> Result<Value, Diagnostic> {
    std::fs::create_dir_all(root).map_err(|_| unavailable("Cannot open the host preference directory."))?;
    let state = StateFile::at(root.join("server-change-migration.json"));
    let mut intent = state.read()?;
    if intent.is_null() {
        let mut files = Vec::new();
        for name in ["collection-preferences.json", "gui-preferences.json"] {
            let before = StateFile::at(root.join(name)).read()?;
            if !before.is_null() && !before.is_object() {
                return Err(unavailable("An existing preference file is not an object. Its contents were preserved."));
            }
            let mut after = before.clone();
            if name == "collection-preferences.json" {
                if before.is_null() { after = json!({}); }
                after["collectionMethod"] = json!("directOnly");
            } else if !before.is_null() {
                if before.get("temporaryAccess").is_some_and(|v| !v.is_boolean()) {
                    return Err(unavailable("The saved temporary-access choice is invalid. Its contents were preserved."));
                }
                after["temporaryAccess"] = json!(false);
            }
            files.push(json!({"name":name,"before":before,"after":after}));
        }
        let changed_legacy = files.iter().any(|file| !file["before"].is_null() && file["before"] != file["after"]);
        intent = json!({"schemaVersion":1,"state":"pending","files":files,"changedLegacyPreferences":changed_legacy,
            "notice":"Herbert now defaults to Direct collection without temporary group access. Server changes require explicit authorization for a run, session, or saved workflow. Earlier choices were preserved as history, not authorization."});
        state.replace(&Value::Null, &intent)?;
    }
    if intent["schemaVersion"] != 1 || !matches!(intent["state"].as_str(), Some("pending" | "complete")) {
        return Err(unavailable("The server-change preference migration record is unknown. It was preserved."));
    }
    if intent["state"] == "complete" { return Ok(intent); }
    let files = intent["files"].as_array().ok_or_else(|| unavailable("The preference migration omitted its owned files."))?;
    if files.len() != 2 || files[0]["name"] != "collection-preferences.json" || files[1]["name"] != "gui-preferences.json" {
        return Err(unavailable("The preference migration does not name the two expected host preference files."));
    }
    for file in files {
        let name = file["name"].as_str().ok_or_else(|| unavailable("The migration file identity is invalid."))?;
        let target = StateFile::at(root.join(name));
        let current = target.read()?;
        if current == file["after"] { continue; }
        if current != file["before"] {
            return Err(unavailable("A host preference changed during migration. The external edit and original migration intent were preserved."));
        }
        if !file["after"].is_null() { target.replace(&current, &file["after"])?; }
    }
    let mut complete = intent.clone();
    complete["state"] = json!("complete");
    match state.replace(&intent, &complete) {
        Ok(()) => Ok(complete),
        Err(error) => {
            // Another process may have completed the same pending transition.
            if state.read()? == complete { Ok(complete) } else { Err(error) }
        }
    }
}

pub(crate) fn take_notice(root: &Path) -> Result<Option<String>, Diagnostic> {
    let before = initialize(root)?;
    if before["changedLegacyPreferences"] != true || before["noticeDisplayed"] == true {
        return Ok(None);
    }
    let notice = before["notice"].as_str().ok_or_else(|| unavailable("The migration notice is unavailable."))?.to_owned();
    let mut after = before.clone();
    after["noticeDisplayed"] = json!(true);
    StateFile::at(root.join("server-change-migration.json")).replace(&before, &after)?;
    Ok(Some(notice))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_changes_only_mutation_choices_once_and_preserves_history() {
        let t = tempfile::tempdir().unwrap();
        let method = StateFile::at(t.path().join("collection-preferences.json"));
        let gui = StateFile::at(t.path().join("gui-preferences.json"));
        let old_method = json!({"collectionMethod":"serverAssisted"});
        let old_gui = json!({"schemaVersion":1,"temporaryAccess":true,"destination":"retained","profileId":"kept","settings":{"workers":7}});
        method.replace(&Value::Null, &old_method).unwrap();
        gui.replace(&Value::Null, &old_gui).unwrap();
        let record = initialize(t.path()).unwrap();
        assert_eq!(record["files"][0]["before"], old_method);
        assert_eq!(record["files"][1]["before"], old_gui);
        let mut expected = old_gui; expected["temporaryAccess"] = json!(false);
        assert_eq!(gui.read().unwrap(), expected);
        method.replace(&method.read().unwrap(), &json!({"collectionMethod":"serverAssisted"})).unwrap();
        expected["temporaryAccess"] = json!(true);
        gui.replace(&gui.read().unwrap(), &expected).unwrap();
        initialize(t.path()).unwrap();
        assert_eq!(method.read().unwrap()["collectionMethod"], "serverAssisted");
        assert_eq!(gui.read().unwrap(), expected);
    }

    #[test]
    fn interrupted_migration_resumes_without_overwriting_an_external_edit() {
        let t = tempfile::tempdir().unwrap();
        let record = initialize(t.path()).unwrap();
        let state = StateFile::at(t.path().join("server-change-migration.json"));
        let mut pending = record.clone(); pending["state"] = json!("pending");
        state.replace(&record, &pending).unwrap();
        assert_eq!(initialize(t.path()).unwrap()["state"], "complete");
        state.replace(&state.read().unwrap(), &pending).unwrap();
        let method = StateFile::at(t.path().join("collection-preferences.json"));
        let external = json!({"collectionMethod":"serverAssisted"});
        method.replace(&method.read().unwrap(), &external).unwrap();
        assert!(initialize(t.path()).is_err());
        assert_eq!(method.read().unwrap(), external);
        assert_eq!(state.read().unwrap(), pending);
    }
}
