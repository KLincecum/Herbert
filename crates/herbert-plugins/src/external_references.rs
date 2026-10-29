//! Native Externals catalog and exact assembly bytes. No helper, filesystem scan, or decompilation.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const KIND: &str = "ExternalAssemblyReference";
pub fn owns(category: &str, kind: &str) -> bool {
    category == "programs-and-extensions" && kind == KIND
}
pub fn methods() -> Vec<(String, String)> {
    vec![
        ("Ice.BO.BpMethodSvc".into(), "GetAvailableReferences".into()),
        ("Ice.Lib.EcfToolsSvc".into(), "GetAssemblyBytes".into()),
    ]
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn catalog_action() -> GuestReply {
    GuestReply::Action {
        action: HostAction::SharedNativeRead {
            service: "Ice.BO.BpMethodSvc".into(),
            method: "GetAvailableReferences".into(),
            // Native BpMethodSvc selects the Product provider for "Assemblies"
            // and the External provider for "Externals". Product dependencies
            // are not independently authored external programs.
            arguments: json!({"referenceGroup":"Externals"}),
            export: false,
        },
        state: json!({"phase":"catalog"}),
    }
}
fn rows(response: &Value) -> Result<&Vec<Value>, Diagnostic> {
    let rows = response
        .get("returnObj")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Diagnostic::new(
                "REFERENCE_CATALOG_UNVERIFIED",
                "The Epicor reference catalog omitted its array.",
            )
        })?;
    let mut seen = BTreeSet::new();
    for row in rows {
        let name = row
            .get("Name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Diagnostic::new(
                    "REFERENCE_IDENTITY_UNVERIFIED",
                    "An assembly catalog row lacks its exact Name.",
                )
            })?;
        if !seen.insert(name) {
            return Err(Diagnostic::new(
                "DUPLICATE_NATIVE_IDENTITY",
                "The assembly catalog repeats an exact Name.",
            ));
        }
        for key in ["FileName", "Version"] {
            if !row.get(key).is_some_and(|v| v.is_null() || v.is_string()) {
                return Err(Diagnostic::new(
                    "REFERENCE_METADATA_UNVERIFIED",
                    "The Epicor reference metadata has an unexpected shape.",
                ));
            }
        }
    }
    Ok(rows)
}
fn identity(name: &str) -> Identity {
    Identity {
        category: "programs-and-extensions".into(),
        kind: KIND.into(),
        scope: Scope::Global,
        keys: BTreeMap::from([("Name".into(), name.into())]),
    }
}
fn bytes_valid(value: &Value) -> bool {
    if let Some(file) = value.get("nativeFile") {
        return serde_json::from_value::<FileRef>(file.clone())
            .is_ok_and(|f| !f.path.is_empty() && f.sha256.len() == 64 && f.bytes > 0);
    }
    match value {
        Value::String(s) => base64::engine::general_purpose::STANDARD
            .decode(s)
            .is_ok_and(|b| !b.is_empty()),
        Value::Array(a) => !a.is_empty() && a.iter().all(|v| v.as_u64().is_some_and(|n| n <= 255)),
        _ => false,
    }
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.category != "programs-and-extensions" {
        return fail(
            "INVALID_IDENTITY",
            "External assembly references are downloaded by the Programs and extensions plugin. Choose that plugin instead.",
        );
    }
    let discover = request.operation == "discover";
    if !discover && request.operation != "acquire" {
        return fail(
            "UNSUPPORTED_OPERATION",
            "The external reference collector supports discovery and exact download.",
        );
    }
    if !discover {
        let Some(id) = request.identity.as_ref() else {
            return fail(
                "IDENTITY_REQUIRED",
                "Select the exact Epicor assembly Name.",
            );
        };
        let Some(name) = id.keys.get("Name") else {
            return fail("INVALID_IDENTITY", "The exact assembly Name is required.");
        };
        if name.is_empty() || id != &identity(name) {
            return fail(
                "INVALID_IDENTITY",
                "Use the exact assembly identity returned by discovery.",
            );
        }
    }
    if !discover && request.state.get("phase").is_none() {
        if let Some(catalog) = request.state.pointer("/_inventory/catalog").cloned() {
            request.state = json!({"phase":"catalog"});
            request.response = Some(json!({"returnObj":[catalog]}));
        }
    }
    let phase = request
        .state
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("initial");
    if phase == "initial" {
        return catalog_action();
    }
    if let Some(error) = request.error {
        return if discover {
            GuestReply::Inventory {
                identities: vec![],
                next_cursor: None,
                authoritative: false,
                diagnostics: vec![error],
            }
        } else {
            GuestReply::Failure { diagnostic: error }
        };
    }
    let Some(response) = request.response.as_ref() else {
        return fail(
            "NATIVE_RESPONSE_MISSING",
            "The Epicor reference response is absent.",
        );
    };
    if phase == "catalog" {
        let rows = match rows(response) {
            Ok(rows) => rows,
            Err(diagnostic) => {
                return if discover {
                    GuestReply::Inventory {
                        identities: vec![],
                        next_cursor: None,
                        authoritative: false,
                        diagnostics: vec![diagnostic],
                    }
                } else {
                    GuestReply::Failure { diagnostic }
                };
            }
        };
        if discover {
            let records: Vec<_> = rows
                .iter()
                .filter(|r| !crate::system_artifacts::marked(r))
                .map(|r| (identity(r["Name"].as_str().unwrap()), json!({"catalog":r})))
                .collect();
            return GuestReply::InventoryData {
                inventory: Box::new(GuestReply::Inventory {
                    identities: records.iter().map(|(id, _)| id.clone()).collect(),
                    next_cursor: None,
                    authoritative: true,
                    diagnostics: vec![],
                }),
                records,
            };
        }
        let name = &request.identity.as_ref().unwrap().keys["Name"];
        let Some(row) = rows.iter().find(|r| r["Name"].as_str() == Some(name)) else {
            return fail(
                "REFERENCE_DISAPPEARED",
                "The exact reference no longer exists in the Epicor catalog.",
            );
        };
        if crate::system_artifacts::marked(row) {
            return crate::system_artifacts::refused();
        }
        return GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.EcfToolsSvc".into(),
                method: "GetAssemblyBytes".into(),
                arguments: json!({"assemblyId":name}),
                export: true,
            },
            state: json!({"phase":"bytes","catalog":row}),
        };
    }
    if phase == "bytes" && !discover {
        let payload = response.get("returnObj");
        let diagnostics = if payload.is_some_and(Value::is_null) {
            vec![Diagnostic::new(
                "REFERENCE_BYTES_UNAVAILABLE",
                "The Epicor catalog contains this reference, but the Externals byte route returned null.",
            )]
        } else if payload.is_some_and(bytes_valid) {
            vec![]
        } else {
            vec![Diagnostic::new(
                "REFERENCE_BYTES_UNVERIFIED",
                "The Externals route did not return a valid nonempty assembly payload.",
            )]
        };
        return GuestReply::Definition {
            identity: request.identity.unwrap(),
            format: "external-assembly-reference-and-native-bytes-json".into(),
            definition: json!({"catalog":request.state["catalog"],"bytes":response,"sourceKind":"native Externals binary; original source unavailable"}),
            completeness: if diagnostics.is_empty() {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
        };
    }
    fail(
        "INVALID_PLUGIN_STATE",
        "The external-reference download saved progress is invalid.",
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn req() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "programs-and-extensions".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(200),
            state: json!({"phase":"catalog"}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn catalog_exact_case_and_duplicates() {
        let mut r = req();
        r.response = Some(
            json!({"returnObj":[{"Name":"Foo","FileName":"Foo.dll","Version":"1"},{"Name":"foo","FileName":"foo.dll","Version":null}]}),
        );
        let GuestReply::InventoryData { records, inventory } = handle(r.clone()) else {
            panic!()
        };
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: true,
                ..
            }
        ));
        assert_eq!(records.len(), 2);
        let mut selected = req();
        selected.operation = "acquire".into();
        selected.identity = Some(records[0].0.clone());
        selected.state = json!({"_inventory":records[0].1});
        let GuestReply::Action {
            action:
                HostAction::SharedNativeRead {
                    method, arguments, ..
                },
            state,
        } = handle(selected)
        else {
            panic!()
        };
        assert_eq!(
            method, "GetAssemblyBytes",
            "Acquisition must not request the catalog again"
        );
        assert_eq!(arguments["assemblyId"], "Foo");
        assert_eq!(state["catalog"]["FileName"], "Foo.dll");
        r.response.as_mut().unwrap()["returnObj"][1]["Name"] = json!("Foo");
        let GuestReply::Inventory { authoritative, .. } = handle(r) else {
            panic!()
        };
        assert!(!authoritative);
    }
    #[test]
    fn absent_bytes_are_preserved_and_not_complete() {
        let mut r = req();
        r.operation = "acquire".into();
        r.identity = Some(identity("Foo"));
        r.state = json!({"phase":"bytes","catalog":{"Name":"Foo"}});
        r.response = Some(json!({"returnObj":null}));
        let GuestReply::Definition {
            completeness,
            definition,
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(completeness, Completeness::Partial);
        assert!(definition["bytes"]["returnObj"].is_null());
    }
    #[test]
    fn bytes_round_trip_and_scoped_identity() {
        let mut r = req();
        r.operation = "acquire".into();
        r.identity = Some(identity("Foo"));
        r.state = json!({"phase":"bytes","catalog":{"Name":"Foo"}});
        r.response = Some(json!({"returnObj":"TVo="}));
        let GuestReply::Definition { completeness, .. } = handle(r.clone()) else {
            panic!()
        };
        assert_eq!(completeness, Completeness::Collected);
        r.identity.as_mut().unwrap().scope = Scope::Company {
            company: "C".into(),
        };
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
}
