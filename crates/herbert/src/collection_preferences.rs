//! One shared preference for GUI, CLI and MCP. A run freezes its own copy.
use herbert_contract::{CollectionMethod, Diagnostic};
use serde::{Deserialize, Serialize};
use std::path::Path;
pub fn admit(
    method: CollectionMethod,
    purpose: herbert_contract::HelperPurpose,
) -> Result<(), Diagnostic> {
    if method.allows(purpose) {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "COLLECTION_METHOD_UNAVAILABLE",
            format!(
                "This helper operation is unavailable in {}. The selected collection method was not changed.",
                method.label()
            ),
        ))
    }
}

#[derive(Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Preferences {
    pub collection_method: CollectionMethod,
}
impl Default for Preferences {
    fn default() -> Self { Self { collection_method: CollectionMethod::DirectOnly } }
}
pub fn read(root: &Path) -> Result<Preferences, Diagnostic> {
    crate::mutation_preferences::initialize(root)?;
    match std::fs::read(root.join("collection-preferences.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
            Diagnostic::new(
                "COLLECTION_PREFERENCE_INVALID",
                "The saved collection method is invalid. Its file has been preserved.",
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
        Err(_) => Err(Diagnostic::new(
            "COLLECTION_PREFERENCE_UNAVAILABLE",
            "Cannot read the saved collection method.",
        )),
    }
}
pub fn write(root: &Path, preferences: &Preferences) -> Result<(), Diagnostic> {
    read(root)?;
    crate::storage::atomic_write(
        &root.join("collection-preferences.json"),
        &serde_json::to_vec_pretty(preferences).expect("serializable preferences"),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_helper_choices_read_as_server_assisted_without_rewriting_the_file() {
        let temp = tempfile::tempdir().unwrap();
        crate::mutation_preferences::initialize(temp.path()).unwrap();
        let path = temp.path().join("collection-preferences.json");
        for old in ["hybrid", "serverSide", "directWithHelp", "helper"] {
            let bytes = serde_json::to_vec(&serde_json::json!({"collectionMethod":old})).unwrap();
            std::fs::write(&path,&bytes).unwrap();
            let preference = read(temp.path()).unwrap();
            assert_eq!(preference.collection_method,CollectionMethod::ServerAssisted);
            assert_eq!(std::fs::read(&path).unwrap(),bytes);
            write(temp.path(),&preference).unwrap();
            let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["collectionMethod"],"serverAssisted");
        }
    }
    #[test]
    fn defaults_roundtrip_and_invalid_file_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            read(temp.path()).unwrap().collection_method,
            CollectionMethod::DirectOnly
        );
        for collection_method in [
            CollectionMethod::DirectOnly,
            CollectionMethod::ServerAssisted,
        ] {
            write(temp.path(), &Preferences { collection_method }).unwrap();
            assert_eq!(
                read(temp.path()).unwrap().collection_method,
                collection_method
            );
        }
        let p = temp.path().join("collection-preferences.json");
        std::fs::write(&p, b"invalid").unwrap();
        assert!(write(temp.path(), &Preferences::default()).is_err());
        assert_eq!(std::fs::read(p).unwrap(), b"invalid");
    }
}
