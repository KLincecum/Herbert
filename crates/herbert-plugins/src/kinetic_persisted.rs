//! Local extraction from custom persisted Kinetic rows. Native applications and
//! layers have their own download route; do not acquire them again for each row.
use serde_json::{Value, json};

pub fn owns(kind: &str) -> bool {
    matches!(
        kind,
        "PersistedKineticCustomization"
            | "PersistedKineticApplicationFile"
            | "PersistedKineticEventLayout"
            | "PersistedKineticLayerBackup"
    )
}

pub fn append(state: &Value, definition: &mut Value) {
    if state["root"]["TypeCode"] == "KNTCMetaUI" {
        if let Some(text) = state["root"]["Content"].as_str() {
            let name = if serde_json::from_str::<Value>(text).is_ok() {
                "Content.json"
            } else {
                "Content.txt"
            };
            definition["herbertSources"]["files"] = json!([{"name":name,"text":text}]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::*;
    #[test]
    fn downloaded_custom_files_need_no_owner_reacquisition() {
        for (kind, code) in [
            ("PersistedKineticApplicationFile", "KNTCMetaUI"),
            ("PersistedKineticCustomization", "KNTCCustLayer"),
            ("PersistedKineticLayerBackup", "KNTCLayerBackup"),
        ] {
            let row = json!({"Company":"","ProductID":"EP","TypeCode":code,"Key1":"Custom","Key2":"Custom\\layout.jsonc","Key3":"","CGCCode":"","SystemFlag":false,"SysRowID":"row","Content":"{}","SysCharacter01":"","SysCharacter03":"","HerbertContentBytes":"4","HerbertParentsBytes":"0","HerbertDraftBytes":"0"});
            let id = Identity {
                category: "C12".into(),
                kind: kind.into(),
                scope: Scope::Global,
                keys: [
                    "Company",
                    "ProductID",
                    "TypeCode",
                    "Key1",
                    "Key2",
                    "Key3",
                    "CGCCode",
                ]
                .into_iter()
                .map(|key| (key.into(), row[key].as_str().unwrap().into()))
                .collect(),
            };
            let request = GuestRequest {
                abi: PLUGIN_ABI,
                operation: "acquire".into(),
                category: "C12".into(),
                company: "C".into(),
                identity: Some(id),
                cursor: None,
                page_size: Some(200),
                response: None,
                error: None,
                state: json!({"_inventory":{"row":row,"children":{"XXXChunk":[]},"errors":[]}}),
            };
            let GuestReply::Definition {
                definition,
                completeness,
                diagnostics,
                ..
            } = crate::handle("C12", request)
            else {
                panic!("The downloaded row must not start an application or owner download");
            };
            assert_eq!(completeness, Completeness::Collected, "{diagnostics:?}");
            assert_eq!(definition["XXXDef"][0]["Content"], "{}");
            assert_eq!(definition["XXXDef"][0]["TypeCode"], code);
            assert!(definition.get("authoredOwner").is_none());
            if code == "KNTCMetaUI" {
                assert_eq!(definition["herbertSources"]["files"][0]["text"], "{}");
            }
        }
    }
}
