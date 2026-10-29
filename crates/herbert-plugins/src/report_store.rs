//! Read current packages from the native custom report store.
//! These packages are separate from deployed SSRS resources and retained history.
//! The dedicated custom table establishes provenance even for stock-named paths.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub fn contract() -> HelperContract {
    serde_json::from_str(include_str!(
        "../../../helpers/report-store/helper-contract.json"
    ))
    .expect("Reviewed report helper contract")
}
pub fn owns(category: &str, kind: &str) -> bool {
    category == "report-styles-and-layouts" && kind == "CurrentCustomReportPackage"
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn identity(key: &str) -> Identity {
    Identity {
        category: "report-styles-and-layouts".into(),
        kind: "CurrentCustomReportPackage".into(),
        scope: Scope::Global,
        keys: BTreeMap::from([("PrintProgram".into(), key.into())]),
    }
}
fn call(function: &str, args: Value, state: Value) -> GuestReply {
    GuestReply::Action {
        action: HostAction::HelperCall {
            library: contract().library_id,
            function: function.into(),
            arguments: args,
        },
        state,
    }
}
fn finish(request: &GuestRequest, diagnostic: Option<Diagnostic>) -> GuestReply {
    let id = request.identity.clone().unwrap();
    let key = id.keys["PrintProgram"].as_str();
    let label = key
        .rsplit('/')
        .filter(|s| !s.is_empty())
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" - ");
    // The full native row ID distinguishes identical leaf names across native paths.
    let owner = Identity {
        keys: BTreeMap::from([
            ("Report".into(), label),
            (
                "StoreRowID".into(),
                request.state["metadata"]["SysRowID"]
                    .as_str()
                    .unwrap_or(key)
                    .into(),
            ),
        ]),
        ..id.clone()
    };
    // Expose embedded Code text for reading. The ZIP retains the original RDL.
    // This XML projection is not used to reconstruct or replace its bytes.
    let mut views = vec![];
    if let Some(documents) = request.state["archive"]["documents"].as_object() {
        for (name, doc) in documents {
            if !name.to_ascii_lowercase().ends_with(".rdl") || !doc["diagnostic"].is_null() {
                continue;
            }
            if let Some(content) = doc["xml"]["content"].as_array() {
                for item in content {
                    let node = &item["element"];
                    if node["name"] == "Code" && node["namespace"] == doc["xml"]["namespace"] {
                        let text = node["content"]
                            .as_array()
                            .map(|c| {
                                c.iter()
                                    .filter_map(|v| v["text"].as_str())
                                    .collect::<String>()
                            })
                            .unwrap_or_default();
                        views.push(json!({
                            "pointer": format!(
                                "/archive/documents/{}/Code",
                                name.replace('~', "~0").replace('/', "~1")
                            ),
                            "extension": "vb",
                            "text": text
                        }));
                    }
                }
            }
        }
    }
    GuestReply::Definition {
        identity: id,
        format: "current-custom-report-package".into(),
        definition: json!({
            "sourceTable": "Ice.CustomReportStore",
            "snapshot": "currentStoredPackage",
            "currentDeploymentComparison": false,
            "metadata": request.state["metadata"],
            "sourceFile": request.state["file"],
            "archive": request.state["archive"],
            "herbertExportIdentity": owner,
            "herbertSources": {
                "files": [{
                    "name": "Metadata.json",
                    "text": serde_json::to_string_pretty(&request.state["metadata"]).unwrap()
                }],
                "views": views
            }
        }),
        completeness: if diagnostic.is_none() {
            Completeness::Collected
        } else {
            Completeness::Partial
        },
        diagnostics: diagnostic.into_iter().collect(),
    }
}
pub fn handle(mut r: GuestRequest) -> GuestReply {
    if let Some(error) = r.error.take() {
        // Once bytes are preserved, later failures produce a partial artifact
        // instead of discarding the original package from its definition.
        return if r.state["file"].is_object() {
            finish(&r, Some(error))
        } else {
            GuestReply::Failure { diagnostic: error }
        };
    }
    if r.operation == "discover" {
        if r.response.is_none() {
            let Some(size) = r.page_size.filter(|n| *n > 0 && *n <= i32::MAX as u32) else {
                return fail(
                    "PAGE_SIZE_REQUIRED",
                    "Enter a page size from 1 to 2147483647.",
                );
            };
            return call(
                "Inventory",
                json!({"iCompany":r.company,"iPageSize":size,"iHasCursor":r.cursor.is_some(),"iAfter":r.cursor.unwrap_or_default()}),
                json!({"phase":"inventory"}),
            );
        }
        let body = r.response.unwrap();
        if body["oSuccess"] != true {
            return fail(
                "CURRENT_REPORT_INVENTORY_REFUSED",
                "The Epicor current custom report inventory refused; absence is unverified.",
            );
        }
        let Some(rows) = body.pointer("/oDS/Store").and_then(Value::as_array) else {
            return fail(
                "CURRENT_REPORT_INVENTORY_INVALID",
                "The Epicor inventory omitted its rows.",
            );
        };
        let mut ids = vec![];
        let mut diagnostics = vec![];
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            let Some(key) = row["PrintProgram"].as_str().filter(|s| !s.is_empty()) else {
                diagnostics.push(Diagnostic::new(
                    "CURRENT_REPORT_KEY_INVALID",
                    "A current package omitted its full Epicor key.",
                ));
                continue;
            };
            if !seen.insert(key) {
                diagnostics.push(Diagnostic::new("CURRENT_REPORT_KEY_DUPLICATE", "The inventory repeats a current package identity."));
                continue;
            }
            ids.push(identity(key));
        }
        let incomplete = |ids, mut diagnostics: Vec<Diagnostic>, message| {
            diagnostics.push(Diagnostic::new("CURRENT_REPORT_CURSOR_INVALID", message));
            GuestReply::Inventory { identities:ids, next_cursor:None, authoritative:false, diagnostics }
        };
        let Some(cursor) = body
            .pointer("/oDS/Cursor")
            .and_then(Value::as_array)
            .filter(|v| v.len() == 1)
            .map(|v| &v[0])
        else {
            return incomplete(ids, diagnostics,
                "The native inventory omitted its continuation.",
            );
        };
        let next = match cursor["More"].as_bool() {
            Some(false) => None,
            Some(true) => {
                let Some(after) = cursor["After"].as_str().filter(|s| !s.is_empty()) else {
                    return incomplete(ids, diagnostics,
                        "The continuation omitted its exact native key.",
                    );
                };
                if ids
                    .last()
                    .and_then(|id| id.keys.get("PrintProgram"))
                    .map(String::as_str)
                    != Some(after)
                    || r.cursor.as_deref() == Some(after)
                {
                    return incomplete(ids, diagnostics,
                        "The continuation did not advance through the returned native keys.",
                    );
                };
                Some(after.into())
            }
            None => {
                return incomplete(ids, diagnostics,
                    "The continuation omitted its More flag.",
                );
            }
        };
        return GuestReply::Inventory {
            identities: ids,
            next_cursor: next,
            authoritative: diagnostics.is_empty(),
            diagnostics,
        };
    }
    let Some(id) = r.identity.as_ref().filter(|id| {
        owns(&id.category, &id.kind) && id.scope == Scope::Global && id.keys.len() == 1
    }) else {
        return fail(
            "INVALID_IDENTITY",
            "Select one exact current custom report package.",
        );
    };
    let Some(key) = id.keys.get("PrintProgram").filter(|s| !s.is_empty()) else {
        return fail(
            "INVALID_IDENTITY",
            "The package needs its full stored PrintProgram key.",
        );
    };
    if r.response.is_none() {
        return call(
            "Read",
            json!({"iCompany":r.company,"iPrintProgram":key}),
            json!({"phase":"read"}),
        );
    }
    let body = r.response.take().unwrap();
    if r.state["phase"] == "read" {
        if body["oSuccess"] != true {
            return fail(
                "CURRENT_REPORT_READ_REFUSED",
                "The exact current custom report read refused; its original response is retained.",
            );
        }
        let Some(meta) = body
            .pointer("/oDS/Metadata")
            .and_then(Value::as_array)
            .filter(|v| v.len() == 1)
            .map(|v| v[0].clone())
        else {
            return fail(
                "CURRENT_REPORT_METADATA_INVALID",
                "The current package omitted its exact metadata.",
            );
        };
        if meta["PrintProgram"].as_str() != Some(key)
            || [
                "ContentHash",
                "SysRowID",
                "SysRevID",
                "CreatedBy",
                "CreatedOn",
                "ModifiedBy",
                "ModifiedOn",
            ]
            .iter()
            .any(|k| meta.get(*k).is_none())
        {
            return fail(
                "CURRENT_REPORT_KEY_MISMATCH",
                "The Epicor package metadata differs from its selected key or omits source fields.",
            );
        }
        let Some(file) = body
            .pointer("/oContent/nativeFile")
            .and_then(|v| serde_json::from_value::<FileRef>(v.clone()).ok())
        else {
            return fail(
                "CURRENT_REPORT_BYTES_MISSING",
                "The current package has no preserved original byte file.",
            );
        };
        if meta["Length"].as_u64() != Some(file.bytes)
            || meta["SHA256"].as_str() != Some(file.sha256.as_str())
            || file.bytes == 0
        {
            return fail(
                "CURRENT_REPORT_BYTES_MISMATCH",
                "The original package bytes differ from Epicor length/hash metadata or are empty.",
            );
        }
        r.state = json!({"phase":"assemble","metadata":meta,"file":file});
        return GuestReply::Action {
            action: HostAction::Assemble {
                bytes: file.bytes,
                sha256: file.sha256.clone(),
                chunks: vec![file],
                extension: "zip".into(),
            },
            state: r.state,
        };
    }
    if r.state["phase"] == "assemble" {
        let file = body["assembledFile"].clone();
        let Some(file) = serde_json::from_value::<FileRef>(file).ok().filter(|f| {
            Some(f.bytes) == r.state["file"]["bytes"].as_u64()
                && Some(f.sha256.as_str()) == r.state["file"]["sha256"].as_str()
                && f.path.ends_with(".zip")
        }) else {
            return finish(
                &r,
                Some(Diagnostic::new(
                    "CURRENT_REPORT_ASSEMBLY_INVALID",
                    "The named ZIP differs from the preserved original length/hash or has no ZIP file reference.",
                )),
            );
        };
        r.state["originalFile"] = r.state["file"].clone();
        r.state["file"] = json!(file);
        r.state["phase"] = json!("directory");
        return GuestReply::Action {
            action: HostAction::ParseXmlArchive {
                file,
                entries: vec![],
            },
            state: r.state,
        };
    }
    if body["sourceFile"] != r.state["file"] {
        return finish(
            &r,
            Some(Diagnostic::new(
                "CURRENT_REPORT_ARCHIVE_MISMATCH",
                "The ZIP projection belongs to a different source file.",
            )),
        );
    }
    let archive = &body["parsedXmlArchive"];
    if r.state["phase"] == "directory" {
        let Some(members) = archive["members"].as_array() else {
            return finish(
                &r,
                Some(Diagnostic::new(
                    "CURRENT_REPORT_ARCHIVE_INVALID",
                    "The stored package has no ZIP directory.",
                )),
            );
        };
        let entries = members
            .iter()
            .filter(|m| m["directory"] != true)
            .map(|m| m["name"].as_str().map(String::from))
            .collect::<Option<Vec<_>>>();
        let Some(entries) =
            entries.filter(|e| e.iter().any(|s| s.to_ascii_lowercase().ends_with(".rdl")))
        else {
            return finish(
                &r,
                Some(Diagnostic::new(
                    "CURRENT_REPORT_RDL_MISSING",
                    "The original stored package contains no RDL member.",
                )),
            );
        };
        r.state["entries"] = json!(entries);
        r.state["phase"] = json!("members");
        return GuestReply::Action {
            action: HostAction::ParseXmlArchive {
                file: serde_json::from_value(r.state["file"].clone()).unwrap(),
                entries,
            },
            state: r.state,
        };
    }
    r.state["archive"] = archive.clone();
    let valid = archive["missing"].as_array().is_some_and(|v| v.is_empty())
        && r.state["entries"].as_array().is_some_and(|entries| {
            entries.iter().all(|e| {
                let name = e.as_str().unwrap();
                let doc = &archive["documents"][name];
                doc["bytes"].as_u64().is_some()
                    && doc["sha256"].as_str().is_some()
                    && (!name.to_ascii_lowercase().ends_with(".rdl")
                        || (doc["diagnostic"].is_null()
                            && doc["xml"]["name"] == "Report"
                            && doc["xml"]["namespace"].as_str().is_some_and(|ns| {
                                ns.starts_with("http://schemas.microsoft.com/sqlserver/reporting/")
                                    && ns.ends_with("/reportdefinition")
                            })))
            })
        });
    finish(
        &r,
        if valid {
            None
        } else {
            Some(Diagnostic::new(
                "CURRENT_REPORT_MEMBERS_INVALID",
                "A stored package member is missing or invalid; original package bytes remain preserved.",
            ))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "report-styles-and-layouts".into(),
            company: "C".into(),
            identity: Some(identity("/Root/reports/StockName/Main")),
            cursor: None,
            page_size: Some(1),
            response: None,
            error: None,
            state: json!({}),
        }
    }
    fn native() -> Value {
        json!({"oSuccess":true,"oDS":{"Metadata":[{"PrintProgram":"/Root/reports/StockName/Main","ContentHash":"nativehash","SysRowID":"row","SysRevID":"rev","CreatedBy":"user","CreatedOn":"date","ModifiedBy":"","ModifiedOn":"","Length":123,"SHA256":"abc"}]},"oContent":{"nativeFile":{"path":"native/package.zip","bytes":123,"sha256":"abc","role":"original"}}})
    }
    #[test]
    fn current_report_exact_full_key_stock_named_override_is_not_rewritten() {
        let GuestReply::Action {
            action:
                HostAction::HelperCall {
                    function,
                    arguments,
                    ..
                },
            ..
        } = handle(request())
        else {
            panic!()
        };
        assert_eq!(function, "Read");
        assert_eq!(arguments["iPrintProgram"], "/Root/reports/StockName/Main");
    }
    #[test]
    fn current_report_metadata_inventory_is_paginated_and_empty_authoritative() {
        let mut r = request();
        r.operation = "discover".into();
        r.identity = None;
        let GuestReply::Action {
            action: HostAction::HelperCall { arguments, .. },
            ..
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(arguments["iPageSize"], 1);
        r.response =
            Some(json!({"oSuccess":true,"oDS":{"Store":[],"Cursor":[{"More":false,"After":""}]}}));
        assert!(
            matches!(handle(r),GuestReply::Inventory{authoritative:true,identities,..}if identities.is_empty())
        );
    }
    #[test]
    fn current_report_refusal_never_means_absence() {
        let mut r = request();
        r.operation = "discover".into();
        r.response = Some(json!({"oSuccess":false,"oMessage":"denied"}));
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
    #[test]
    fn current_report_exact_bytes_require_matching_key_length_and_hash() {
        for field in ["PrintProgram", "Length", "SHA256"] {
            let mut r = request();
            r.state = json!({"phase":"read"});
            let mut n = native();
            n["oDS"]["Metadata"][0][field] = json!("mismatch");
            r.response = Some(n);
            assert!(matches!(handle(r), GuestReply::Failure { .. }));
        }
        let mut r = request();
        r.state = json!({"phase":"read"});
        r.response = Some(native());
        assert!(
            matches!(handle(r),GuestReply::Action{action:HostAction::Assemble{extension,bytes:123,..},..}if extension=="zip")
        );
    }
    #[test]
    fn current_report_named_zip_requires_unchanged_bytes_before_parse() {
        let mut r = request();
        r.state = json!({"phase":"read"});
        r.response = Some(native());
        let GuestReply::Action { state, .. } = handle(r.clone()) else {
            panic!()
        };
        r.state = state;
        let file = r.state["file"].clone();
        r.response = Some(json!({"assembledFile":file}));
        assert!(
            matches!(handle(r.clone()),GuestReply::Action{action:HostAction::ParseXmlArchive{file,..},..}if file.path.ends_with(".zip")&&file.bytes==123)
        );
        for field in ["bytes", "sha256", "path"] {
            let mut bad = r.clone();
            bad.response.as_mut().unwrap()["assembledFile"][field] = json!("wrong");
            assert!(
                matches!(handle(bad),GuestReply::Definition{completeness:Completeness::Partial,diagnostics,..}if diagnostics[0].code=="CURRENT_REPORT_ASSEMBLY_INVALID")
            );
        }
    }
    #[test]
    fn current_report_human_identity_retains_collision_identity_and_code_text() {
        let mut r = request();
        let ns = "http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition";
        r.state = json!({"metadata":native()["oDS"]["Metadata"][0],"archive":{"documents":{"Main.rdl":{"xml":{"namespace":ns,"content":[{"element":{"name":"Code","namespace":ns,"content":[{"text":"Function 日本語()\r\n"},{"text":"  Return \"<&>😀\"\nEnd Function"}]}},{"element":{"name":"Code","namespace":"urn:foreign","content":[{"text":"wrong"}]}}]}}}}});
        let GuestReply::Definition { definition: a, .. } = finish(&r, None) else {
            panic!()
        };
        assert_eq!(a["herbertSources"]["views"].as_array().unwrap().len(), 1);
        assert_eq!(
            a["herbertSources"]["views"][0]["text"],
            "Function 日本語()\r\n  Return \"<&>😀\"\nEnd Function"
        );
        assert_eq!(
            a["herbertExportIdentity"]["keys"]["Report"],
            "StockName - Main"
        );
        r.state["metadata"]["SysRowID"] = json!("another-row");
        r.state["archive"] = json!({});
        let GuestReply::Definition { definition: b, .. } = finish(&r, None) else {
            panic!()
        };
        assert_ne!(a["herbertExportIdentity"], b["herbertExportIdentity"]);
        assert_eq!(b["herbertSources"]["views"], json!([]));
    }
    #[test]
    fn current_report_missing_archive_preserves_original_as_partial() {
        let mut r = request();
        r.state = json!({"phase":"read"});
        r.response = Some(native());
        let GuestReply::Action { state, .. } = handle(r.clone()) else {
            panic!()
        };
        r.state = state;
        r.error = Some(Diagnostic::new("INVALID_ZIP", "bad archive"));
        r.response = None;
        assert!(
            matches!(handle(r),GuestReply::Definition{completeness:Completeness::Partial,definition,..}if definition["sourceFile"]["bytes"]==123)
        );
    }
    #[test]
    fn current_report_continuation_must_bind_last_native_key() {
        let mut r = request();
        r.operation = "discover".into();
        r.response = Some(
            json!({"oSuccess":true,"oDS":{"Store":[{"PrintProgram":"/Root/reports/A"}],"Cursor":[{"More":true,"After":"/Other/reports/B"}]}}),
        );
        assert!(matches!(handle(r), GuestReply::Inventory { identities, authoritative:false, diagnostics, .. }
            if identities.len()==1 && diagnostics.iter().any(|d|d.code=="CURRENT_REPORT_CURSOR_INVALID")));
    }
    #[test]
    fn current_report_descriptor_declares_readonly_original_bytes() {
        let c = contract();
        assert_eq!(c.functions.len(), 4);
        assert!(c.functions.iter().all(|f| f.effect == HelperEffect::Read));
        assert_eq!(c.functions.iter().find(|f|f.function_id=="Read").unwrap().binary_outputs, vec!["oContent"]);
        assert!(c.functions.iter().any(|f|f.function_id=="ReadPage"&&f.binary_outputs.is_empty()));
        assert!(c.functions.iter().any(|f|f.function_id=="ReadLayouts"&&f.binary_outputs.is_empty()));
    }
}
