//! Independent current KNTCMetaUI file acquisition. Native ExportApp reads only
//! the first 100 database rows (retained DBConentLoader.cs:454-487). Page by the
//! physical row key. Preserve all selected/global authored rows and their text.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn push(state: &mut Value, diagnostic: Diagnostic) {
    state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(diagnostic));
}
pub fn policy_start(request: GuestRequest, mut state: Value) -> GuestReply {
    state["phase"] = json!("fileSystemPolicy");
    state["policyBranch"] = json!(0);
    state["policyAfter"] = Value::Null;
    state["systemFileIdentities"] = json!([]);
    policy_query(request, state)
}
fn policy_query(request: GuestRequest, state: Value) -> GuestReply {
    let Some(size) = request.page_size.filter(|n| *n > 0) else {
        return GuestReply::Failure {
            diagnostic: error(
                "PAGE_SIZE_REQUIRED",
                "The authored ownership inventory requires the configured page size.",
            ),
        };
    };
    let branch = state["policyBranch"].as_u64().unwrap_or(0);
    if branch >= 4 {
        if state["systemFileIdentities"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
        {
            let mut state = state;
            state["content"]["nativeExportPolicy"] = json!({"withheld":true,"reason":"The native whole-application export and version service cannot exclude explicitly system-marked child content before retention. Eligible custom current file bodies are preserved independently."});
            state["versionInventory"] = json!({"withheldForSystemChildren":true});
            state["exportWithheldForSystemChildren"] = json!(true);
            push(
                &mut state,
                error(
                    "KINETIC_NATIVE_EXPORT_SYSTEM_CONTENT",
                    "Native application export and retained-version reads were withheld because they include explicitly system-marked child content. Independently measured custom current files are preserved; native export/version completeness is not claimed.",
                ),
            );
            return start(request, state);
        }
        return crate::kinetic::read_versions(request, state);
    }
    let event = branch >= 2;
    let company = if branch % 2 == 0 {
        ""
    } else {
        &request.company
    };
    let mut parameters = BTreeMap::from([
        ("company".into(), json!(company)),
        ("product".into(), json!("EP")),
        (
            "kind".into(),
            json!(if event {
                "KNTCEventLayout"
            } else {
                "KNTCMetaUI"
            }),
        ),
        ("system".into(), json!(true)),
    ]);
    let mut criteria: Vec<_> = [
        ("Company", "company"),
        ("ProductID", "product"),
        ("TypeCode", "kind"),
        ("SystemFlag", "system"),
    ]
    .into_iter()
    .map(|(field, parameter)| QueryCriterion {
        table: "Ownership".into(),
        field: field.into(),
        operator: "=".into(),
        parameter: parameter.into(),
    })
    .collect();
    if !state["policyAfter"].is_null() {
        parameters.insert("after".into(), state["policyAfter"].clone());
        criteria.push(QueryCriterion {
            table: "Ownership".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    let fields = [
        "Company",
        "ProductID",
        "TypeCode",
        "Key1",
        "Key2",
        "Key3",
        "CGCCode",
        "SysRowID",
        "SystemFlag",
    ]
    .into_iter()
    .map(|field| QueryField {
        table: "Ownership".into(),
        field: field.into(),
        alias: field.into(),
        expression: None,
    })
    .collect();
    GuestReply::Action {
        action: HostAction::SharedQuery {
            query: QuerySpec {
                tables: vec![QueryTable {
                    schema: "Ice".into(),
                    table: "XXXDef".into(),
                    alias: "Ownership".into(),
                    company_field: None,
                    metadata: vec![],
                }],
                fields,
                joins: vec![],
                criteria,
                parameters,
                order: vec![QueryOrder {
                    field: "SysRowID".into(),
                    descending: false,
                }],
                page_size: Some(size),
                page: 1,
            },
        },
        state,
    }
}
pub fn policy_handle(request: GuestRequest) -> GuestReply {
    if let Some(diagnostic) = request.error.clone() {
        return GuestReply::Failure { diagnostic };
    }
    let mut state = request.state.clone();
    let Some(rows) = request.response.as_ref().and_then(|v| v["rows"].as_array()) else {
        return GuestReply::Failure {
            diagnostic: error(
                "SYSTEM_POLICY_UNVERIFIED",
                "The authored ownership query omitted its metadata rows.",
            ),
        };
    };
    let Some(size) = request.page_size.filter(|n| *n > 0) else {
        return GuestReply::Failure {
            diagnostic: error(
                "PAGE_SIZE_REQUIRED",
                "The ownership checkpoint has no configured page size.",
            ),
        };
    };
    if rows.len() > size as usize
        || rows.iter().any(|r| {
            !crate::system_artifacts::marked(r) || r["SysRowID"].as_str().is_none_or(str::is_empty)
        })
    {
        return GuestReply::Failure {
            diagnostic: error(
                "SYSTEM_POLICY_UNVERIFIED",
                "The ownership inventory returned invalid metadata or exceeded its requested page size.",
            ),
        };
    }
    let view = &request.identity.as_ref().unwrap().keys["ViewId"];
    state["systemFileIdentities"]
        .as_array_mut()
        .unwrap()
        .extend(
            rows.iter()
                .filter(|row| {
                    let key = if row["TypeCode"] == "KNTCEventLayout" {
                        "Key2"
                    } else {
                        "Key1"
                    };
                    row[key]
                        .as_str()
                        .is_some_and(|value| value.eq_ignore_ascii_case(view))
                })
                .cloned(),
        );
    if rows.len() == size as usize {
        let after = rows.last().unwrap()["SysRowID"].clone();
        if after == state["policyAfter"] {
            return GuestReply::Failure {
                diagnostic: error(
                    "PAGINATION_UNVERIFIED",
                    "The ownership metadata cursor did not advance.",
                ),
            };
        }
        state["policyAfter"] = after;
    } else {
        state["policyBranch"] = json!(state["policyBranch"].as_u64().unwrap_or(0) + 1);
        state["policyAfter"] = Value::Null;
    }
    policy_query(request, state)
}
pub fn start(request: GuestRequest, mut state: Value) -> GuestReply {
    state["phase"] = json!("persistedFiles");
    state["fileCompanyBranch"] = json!(0);
    state["fileAfter"] = Value::Null;
    state["persistedFiles"] = json!([]);
    query(&request, state)
}
fn query(request: &GuestRequest, state: Value) -> GuestReply {
    let company = if state["fileCompanyBranch"].as_u64() == Some(0) {
        ""
    } else {
        &request.company
    };
    crate::tables::kinetic_file_page(request, company, state["fileAfter"].clone(), state)
}
fn next_branch(request: GuestRequest, mut state: Value) -> GuestReply {
    if state["fileCompanyBranch"].as_u64() == Some(0) && !request.company.is_empty() {
        state["fileCompanyBranch"] = json!(1);
        state["fileAfter"] = Value::Null;
        query(&request, state)
    } else {
        let errors = if state["exportWithheldForSystemChildren"] == true {
            vec![]
        } else {
            reconcile(request.identity.as_ref().unwrap(), &mut state)
        };
        for diagnostic in errors {
            push(&mut state, diagnostic);
        }
        crate::kinetic_resources::start(request, state)
    }
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    let mut state = request.state.clone();
    if let Some(diagnostic) = request.error.take() {
        push(&mut state, diagnostic);
        return next_branch(request, state);
    }
    let Some(rows) = request
        .response
        .as_ref()
        .and_then(|v| v.get("rows"))
        .and_then(Value::as_array)
    else {
        push(
            &mut state,
            error(
                "KINETIC_FILES_UNVERIFIED",
                "The independent file response omitted its native rows.",
            ),
        );
        return next_branch(request, state);
    };
    let Some(size) = request.page_size.filter(|n| *n > 0) else {
        return GuestReply::Failure {
            diagnostic: error(
                "PAGE_SIZE_REQUIRED",
                "The persisted-file checkpoint has no configured page size.",
            ),
        };
    };
    let company = if state["fileCompanyBranch"].as_u64() == Some(0) {
        ""
    } else {
        &request.company
    };
    let id = request.identity.as_ref().unwrap();
    let mut seen: BTreeSet<String> = state["persistedFiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["SysRowID"].as_str().map(str::to_ascii_lowercase))
        .collect();
    for row in rows {
        if crate::system_artifacts::marked(row) {
            continue;
        }
        if row["ProductID"] != "EP"
            || row["Key1"]
                .as_str()
                .is_some_and(|v| !v.eq_ignore_ascii_case(&id.keys["ViewId"]))
        {
            continue;
        }
        if row["Company"].as_str() != Some(company)
            || row["ProductID"] != "EP"
            || row["TypeCode"] != "KNTCMetaUI"
            || !row["Key1"]
                .as_str()
                .is_some_and(|v| v.eq_ignore_ascii_case(&id.keys["ViewId"]))
            || row["Key3"] != ""
            || !["Key2", "CGCCode"]
                .iter()
                .all(|f| row.get(*f).is_some_and(Value::is_string))
        {
            push(
                &mut state,
                error(
                    "KINETIC_FILE_IDENTITY_MISMATCH",
                    "An independent file row is outside its exact app, type or company branch.",
                ),
            );
            continue;
        }
        let Some(physical) = row["SysRowID"].as_str().filter(|v| !v.is_empty()) else {
            push(
                &mut state,
                error(
                    "KINETIC_FILE_IDENTITY_UNVERIFIED",
                    "An authored file lacks its native physical row identity.",
                ),
            );
            continue;
        };
        if !seen.insert(physical.to_ascii_lowercase()) {
            push(
                &mut state,
                error(
                    "KINETIC_FILE_DUPLICATE",
                    "The paged file inventory repeated a physical row; source changes or a nonadvancing page prevent completeness.",
                ),
            );
            continue;
        }
        let actual = row["Content"]
            .as_str()
            .map(|s| s.encode_utf16().count() as u64 * 2);
        let measured = row["HerbertContentBytes"]
            .as_u64()
            .or_else(|| row["HerbertContentBytes"].as_str()?.parse().ok());
        if actual.is_none() || actual != measured {
            push(
                &mut state,
                error(
                    "KINETIC_FILE_LENGTH_MISMATCH",
                    "An authored file body is absent or differs from its native UTF-16 byte length.",
                ),
            );
        }
        state["persistedFiles"]
            .as_array_mut()
            .unwrap()
            .push(row.clone());
    }
    if rows.len() > size as usize {
        push(
            &mut state,
            error(
                "PAGINATION_UNVERIFIED",
                "The independent file query exceeded its requested page size.",
            ),
        );
        return next_branch(request, state);
    }
    if rows.len() == size as usize {
        let after = rows
            .last()
            .and_then(|r| r.get("SysRowID"))
            .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            .cloned();
        if let Some(after) = after.filter(|v| *v != state["fileAfter"]) {
            state["fileAfter"] = after;
            return query(&request, state);
        }
        push(
            &mut state,
            error(
                "PAGINATION_UNVERIFIED",
                "The independent file cursor did not advance; preserved pages remain available.",
            ),
        );
    }
    next_branch(request, state)
}
fn relative_path(view: &str, path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let (prefix, relative) = normalized.split_once('/')?;
    if !prefix.eq_ignore_ascii_case(view) || relative.is_empty() {
        return None;
    }
    Some(relative.to_owned())
}
fn reconcile(id: &Identity, state: &mut Value) -> Vec<Diagnostic> {
    let mut errors = Vec::new();
    let Some(rows) = state["persistedFiles"].as_array() else {
        return vec![error(
            "KINETIC_FILES_UNVERIFIED",
            "The persisted file collection is absent.",
        )];
    };
    let mut expected: BTreeMap<String, (&str, &Value)> = BTreeMap::new();
    for row in rows {
        if row["CGCCode"] != "" {
            continue;
        } // Native ExportApp explicitly binds CGCCode empty.
        let Some(path) = row["Key2"]
            .as_str()
            .and_then(|p| relative_path(&id.keys["ViewId"], p))
        else {
            errors.push(error(
                "KINETIC_FILE_PATH_UNVERIFIED",
                "A persisted file does not have the native app-relative path prefix.",
            ));
            continue;
        };
        let normalized = path.to_ascii_lowercase();
        match expected.get(&normalized){
            Some((_,old)) if old["Company"]==row["Company"]=>errors.push(error("KINETIC_FILE_PATH_COLLISION","Two persisted authored files have the same native case-insensitive path in one company.")),
            Some((_,old)) if old["Company"]==""=>(), // Native export selects global content first.
            _=>{expected.insert(normalized,(row["Key2"].as_str().unwrap(),row));}
        }
    }
    let files = state["content"]["export"]
        .get("returnObj")
        .unwrap_or(&state["content"]["export"])
        .get("Files")
        .and_then(Value::as_object);
    let mut exported = BTreeMap::new();
    if let Some(files) = files {
        for (path, value) in files {
            if exported
                .insert(path.replace('\\', "/").to_ascii_lowercase(), value)
                .is_some()
            {
                errors.push(error(
                    "KINETIC_EXPORT_PATH_COLLISION",
                    "The native export repeats a case-insensitive path.",
                ));
            }
        }
    }
    let mut omitted = Vec::new();
    let mut matched = Vec::new();
    for (path, (native_path, _row)) in expected {
        match exported.get(&path) {
            Some(_) => matched.push(native_path.to_owned()),
            None => omitted.push(native_path.to_owned()),
        }
    }
    // Missing export members do not mean missing backup bytes: the complete
    // measured current row bodies are now retained alongside that native export.
    state["fileCoverage"] = json!({"pathsInExport":matched,"additionalPaths":omitted});
    errors
}
#[cfg(test)]
mod tests {
    use super::*;
    fn id() -> Identity {
        Identity {
            category: "C12".into(),
            kind: "MetaFXApplication".into(),
            scope: Scope::Company {
                company: "C".into(),
            },
            keys: BTreeMap::from([
                ("ViewId".into(), "App".into()),
                ("Company".into(), "C".into()),
                ("Type".into(), "view".into()),
                ("SubType".into(), "Dashboard".into()),
                ("DeviceType".into(), "Desktop".into()),
            ]),
        }
    }
    fn row(company: &str, path: &str, text: &str) -> Value {
        json!({"Company":company,"ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":"App","Key2":format!("App\\{path}"),"Key3":"","CGCCode":"","SysRowID":format!("{company}:{path}"),"Content":text,"HerbertContentBytes":text.encode_utf16().count()*2})
    }
    #[test]
    fn export_first_page_omissions_are_recovered_without_losing_drafts() {
        let rows: Vec<_> = (0..151)
            .map(|n| row("", &format!("{n}.jsonc"), "body"))
            .chain([row("", "layout.draft.jsonc", "draft")])
            .collect();
        let files: serde_json::Map<_, _> = (0..100)
            .map(|n| (format!("{n}.jsonc"), json!("body")))
            .collect();
        let mut state =
            json!({"persistedFiles":rows,"content":{"export":{"returnObj":{"Files":files}}}});
        assert!(reconcile(&id(), &mut state).is_empty());
        assert_eq!(
            state["fileCoverage"]["additionalPaths"]
                .as_array()
                .unwrap()
                .len(),
            52
        );
    }
    #[test]
    fn export_and_downloaded_source_are_kept_without_content_comparison() {
        let mut state = json!({"persistedFiles":[row("","layout.jsonc","source")],"content":{"export":{"returnObj":{"Files":{"layout.jsonc":"changed"}}}}});
        assert!(reconcile(&id(), &mut state).is_empty());
        state["persistedFiles"] = json!([
            row("", "layout.jsonc", "source"),
            row("", "LAYOUT.JSONC", "source")
        ]);
        assert!(
            reconcile(&id(), &mut state)
                .iter()
                .any(|d| d.code == "KINETIC_FILE_PATH_COLLISION")
        );
    }
    #[test]
    fn query_preserves_company_branch_and_has_no_export_100_limit() {
        let request = GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "C12".into(),
            company: "C".into(),
            identity: Some(id()),
            cursor: None,
            page_size: Some(7),
            state: json!({}),
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = start(request, json!({"errors":[]}))
        else {
            panic!()
        };
        assert_eq!(query.page_size, Some(7));
        assert_eq!(
            query.parameters[&query
                .criteria
                .iter()
                .find(|c| c.field == "Company")
                .unwrap()
                .parameter],
            ""
        );
        assert_eq!(state["phase"], "persistedFiles");
        assert!(
            query
                .fields
                .iter()
                .any(|f| f.alias == "HerbertContentBytes")
        );
    }
    #[test]
    fn system_child_metadata_prevents_whole_export_and_versions_before_body_reads() {
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "C12".into(),
            company: "C".into(),
            identity: Some(id()),
            cursor: None,
            page_size: Some(1),
            state: Value::Null,
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = policy_start(request.clone(), json!({"content":{},"errors":[]}))
        else {
            panic!()
        };
        assert!(
            query
                .fields
                .iter()
                .all(|f| f.field != "*" && f.field != "Content")
        );
        assert_eq!(query.parameters["system"], true);
        request.state = state;
        request.response = Some(
            json!({"rows":[{"Company":"","ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":"App","Key2":"App\\system.jsonc","Key3":"","CGCCode":"","SystemFlag":true,"SysRowID":"a"}]}),
        );
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = policy_handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "a");
        request.state = state;
        request.response = Some(json!({"rows":[]}));
        for branch in 1..4 {
            let GuestReply::Action {
                action: HostAction::SharedQuery { query },
                state,
            } = policy_handle(request.clone())
            else {
                panic!()
            };
            assert_eq!(state["policyBranch"], branch);
            assert!(query.fields.iter().all(|f| f.field != "Content"));
            request.state = state;
        }
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = policy_handle(request)
        else {
            panic!("whole native export/version read must not run")
        };
        assert_eq!(query.tables[0].table, "XXXDef");
        assert_eq!(
            query.parameters[&query
                .criteria
                .iter()
                .find(|c| c.field == "SystemFlag")
                .unwrap()
                .parameter],
            false
        );
        assert_eq!(state["exportWithheldForSystemChildren"], true);
        assert_eq!(
            state["errors"][0]["code"],
            "KINETIC_NATIVE_EXPORT_SYSTEM_CONTENT"
        );
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "C12".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "C12".into(),
                kind: "MetaFXApplication".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([("ViewId".into(), "App".into())]),
            }),
            cursor: None,
            page_size: Some(1),
            state: json!({}),
            response: None,
            error: None,
        }
    }
    fn row() -> Value {
        json!({"Company":"","ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":"App","Key2":"App/layout.draft.jsonc","Key3":"","CGCCode":"","SysRowID":"a","Content":"{}","HerbertContentBytes":"4"})
    }
    fn state() -> Value {
        json!({"errors":[],"content":{"export":{"returnObj":{"Files":{}}}}})
    }
    #[test]
    fn applications_share_file_pages_and_keep_only_their_own_content() {
        let mut first = request();
        first.page_size = Some(3);
        let mut second = first.clone();
        second
            .identity
            .as_mut()
            .unwrap()
            .keys
            .insert("ViewId".into(), "Other".into());
        let GuestReply::Action {
            action: first_action,
            state: first_state,
        } = start(first.clone(), state())
        else {
            panic!()
        };
        let GuestReply::Action {
            action: second_action,
            ..
        } = start(second, state())
        else {
            panic!()
        };
        assert_eq!(
            serde_json::to_value(first_action).unwrap(),
            serde_json::to_value(second_action).unwrap()
        );
        let mut other = row();
        other["Key1"] = json!("Other");
        other["Key2"] = json!("Other/layout.jsonc");
        other["SysRowID"] = json!("b");
        first.state = first_state;
        first.response = Some(json!({"rows":[row(),other]}));
        let GuestReply::Action { state: next, .. } = handle(first.clone()) else {
            panic!()
        };
        first.state = next;
        first.response = Some(json!({"rows":[]}));
        let GuestReply::Definition {
            definition,
            diagnostics,
            ..
        } = handle(first)
        else {
            panic!()
        };
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(definition["persistedFiles"], json!([row()]));
    }

    #[test]
    fn exact_boundary_requires_empty_page_and_each_company_branch() {
        let mut request = request();
        let GuestReply::Action { state, .. } = start(request.clone(), state()) else {
            panic!()
        };
        request.state = state;
        request.response = Some(json!({"rows":[row()]}));
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "a");
        request.state = state;
        request.response = Some(json!({"rows":[]}));
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(
            query.parameters[&query
                .criteria
                .iter()
                .find(|c| c.field == "Company")
                .unwrap()
                .parameter],
            "C"
        );
        assert!(!query.parameters.contains_key("after"));
        request.state = state;
        let GuestReply::Definition {
            definition,
            diagnostics,
            ..
        } = handle(request)
        else {
            panic!()
        };
        assert_eq!(definition["persistedFiles"].as_array().unwrap().len(), 1);
        assert!(
            !diagnostics
                .iter()
                .any(|d| d.code == "KINETIC_EXPORT_MEMBERSHIP_UNVERIFIED")
        );
        assert!(diagnostics.is_empty(), "{:?}", diagnostics);
        assert_eq!(definition["resourceClosureComplete"], true);
    }
    #[test]
    fn refused_branch_preserves_successful_other_branch_and_diagnostic() {
        let mut request = request();
        let GuestReply::Action { state, .. } = start(request.clone(), state()) else {
            panic!()
        };
        request.state = state;
        request.error = Some(error("NATIVE_REFUSAL", "fixture refusal"));
        let GuestReply::Action { state, .. } = handle(request.clone()) else {
            panic!()
        };
        request.state = state;
        request.error = None;
        request.response = Some(json!({"rows":[]}));
        let GuestReply::Definition {
            completeness,
            diagnostics,
            ..
        } = handle(request)
        else {
            panic!()
        };
        assert_eq!(completeness, Completeness::Partial);
        assert!(diagnostics.iter().any(|d| d.code == "NATIVE_REFUSAL"));
    }
    #[test]
    fn source_length_error_survives_into_final_result() {
        let mut request = request();
        request.page_size = Some(2);
        let GuestReply::Action { state, .. } = start(request.clone(), state()) else {
            panic!()
        };
        request.state = state;
        let mut row = row();
        row["HerbertContentBytes"] = json!("999");
        request.response = Some(json!({"rows":[row]}));
        let GuestReply::Action { state, .. } = handle(request.clone()) else {
            panic!()
        };
        request.state = state;
        request.response = Some(json!({"rows":[]}));
        let GuestReply::Definition { diagnostics, .. } = handle(request) else {
            panic!()
        };
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "KINETIC_FILE_LENGTH_MISMATCH")
        );
    }
}
