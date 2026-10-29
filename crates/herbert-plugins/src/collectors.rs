//! Compose independently recoverable native and persisted-definition branches.
use herbert_contract::*;
use serde_json::{Value, json};

pub fn supported(category: &str) -> bool {
    super::families::supported(category)
        || super::tables::supported(category)
        || matches!(category, "kinetic-applications-and-layers" | "kinetic-personalizations" | "configuration-references")
}

#[cfg(test)]
mod report_discovery_continuation_tests {
    use super::*;
    #[test]
    fn kinetic_persisted_source_failure_does_not_suppress_native_application_discovery() {
        let mut r = request();
        r.category = "kinetic-applications-and-layers".into();
        r.cursor = Some(json!({"source":"tables","cursor":null}).to_string());
        r.response = Some(json!({"malformed":"no inventory rows"}));
        let reply = handle(r.clone());
        if let GuestReply::Inventory { diagnostics, .. } = &reply {
            assert_eq!(diagnostics[0].code, "INVALID_NATIVE_RESPONSE");
        }
        let next = next_source(reply, "kinetic");
        r.cursor = Some(next);
        r.response = None;
        r.state = json!({});
        let GuestReply::Action {
            action: HostAction::SharedNativeRead { method, .. },
            state,
        } = handle(r)
        else {
            panic!("native Kinetic discovery was suppressed")
        };
        assert_eq!(method, "GetApplications");
        assert_eq!(state["source"], "kinetic");
    }
    #[test]
    fn invalid_outer_cursor_and_source_do_not_become_valid_continuations() {
        let mut r = request();
        r.cursor = Some("invalid JSON".into());
        assert!(
            matches!(handle(r),GuestReply::Failure{diagnostic} if diagnostic.code=="INVALID_CURSOR")
        );
        let mut r = request();
        r.cursor = Some(json!({"source":"unknown","cursor":null}).to_string());
        assert!(
            matches!(handle(r),GuestReply::Failure{diagnostic} if diagnostic.code=="INVALID_IDENTITY")
        );
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "report-styles-and-layouts".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: json!({}),
            response: None,
            error: None,
        }
    }
    fn next_source(reply: GuestReply, expected: &str) -> String {
        let GuestReply::Inventory {
            authoritative,
            diagnostics,
            next_cursor: Some(next),
            ..
        } = reply
        else {
            panic!("missing continuation")
        };
        assert!(!authoritative);
        assert!(!diagnostics.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&next).unwrap()["source"],
            expected
        );
        next
    }
    #[test]
    fn report_refusal_preserves_diagnostic_and_allows_native_then_history_sources() {
        let mut r = request();
        r.error = Some(Diagnostic::new(
            "ACCESS_DENIED",
            "Synthetic report refusal.",
        ));
        let reply = handle(r.clone());
        if let GuestReply::Inventory { diagnostics, .. } = &reply {
            assert_eq!(diagnostics[0].code, "ACCESS_DENIED");
            assert_eq!(diagnostics[0].message, "Synthetic report refusal.");
        }
        let next = next_source(reply, "native");
        r.cursor = Some(next);
        r.error = None;
        let mut visited = std::collections::BTreeSet::new();
        loop {
            // Report-style discovery has independent company/global branches.
            // A refused company read must still try global before history.
            assert!(
                visited.insert(r.cursor.clone()),
                "native discovery repeated its cursor"
            );
            let GuestReply::Action { state, .. } = handle(r.clone()) else {
                panic!("native discovery was suppressed")
            };
            assert_eq!(state["source"], "native");
            r.state = state;
            r.error = Some(Diagnostic::new("NATIVE_DENIED", "Synthetic style refusal."));
            let GuestReply::Inventory {
                authoritative,
                diagnostics,
                next_cursor: Some(next),
                ..
            } = handle(r.clone())
            else {
                panic!("refusal suppressed the next independent branch")
            };
            assert!(!authoritative);
            assert!(diagnostics.iter().any(|d| d.code == "NATIVE_DENIED"));
            let cursor: Value = serde_json::from_str(&next).unwrap();
            r.cursor = Some(next);
            r.state = json!({});
            r.error = None;
            if cursor["source"] == "tables" {
                break;
            }
            assert_eq!(cursor["source"], "native");
        }
        let GuestReply::Action {
            action: HostAction::Query { .. },
            state,
        } = handle(r)
        else {
            panic!("history discovery was suppressed")
        };
        assert_eq!(state["source"], "tables");
    }
    #[test]
    fn report_bad_page_keeps_valid_ids_and_continues_without_claiming_absence() {
        for cursor in [json!([]), json!([{"More":true,"After":"wrong"}])] {
            let mut r = request();
            r.response = Some(
                json!({"oSuccess":true,"oDS":{"Store":[{"PrintProgram":"/Reports/Example"},{}],"Cursor":cursor}}),
            );
            let reply = handle(r);
            if let GuestReply::Inventory {
                identities,
                diagnostics,
                ..
            } = &reply
            {
                assert_eq!(identities.len(), 1);
                assert_eq!(identities[0].keys["PrintProgram"], "/Reports/Example");
                assert!(
                    diagnostics
                        .iter()
                        .any(|d| d.code == "CURRENT_REPORT_KEY_INVALID")
                );
                assert!(
                    diagnostics
                        .iter()
                        .any(|d| d.code == "CURRENT_REPORT_CURSOR_INVALID")
                );
            } else {
                panic!()
            }
            next_source(reply, "native");
        }
    }
    #[test]
    fn report_empty_verified_page_is_authoritative_but_acquisition_failure_stays_failure() {
        let mut r = request();
        r.response =
            Some(json!({"oSuccess":true,"oDS":{"Store":[],"Cursor":[{"More":false,"After":""}]}}));
        assert!(
            matches!(handle(r),GuestReply::Inventory{authoritative:true,identities,next_cursor:Some(_),diagnostics} if identities.is_empty()&&diagnostics.is_empty())
        );
        let mut r = request();
        r.operation = "acquire".into();
        r.identity = Some(Identity {
            category: "report-styles-and-layouts".into(),
            kind: "CurrentCustomReportPackage".into(),
            scope: Scope::Global,
            keys: std::collections::BTreeMap::from([(
                "PrintProgram".into(),
                "/Reports/Example".into(),
            )]),
        });
        r.error = Some(Diagnostic::new(
            "ACCESS_DENIED",
            "Synthetic acquisition refusal.",
        ));
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
}
pub fn methods(category: &str) -> Vec<(String, String)> {
    let mut reads = super::families::methods(category);
    if category == "configurators" {
        reads.extend([
            ("Ice.Lib.SessionModSvc".into(), "GetLicensedModuleAccess".into()),
            ("Erp.BO.ErpMetaFxSvc".into(), "ExportApp".into()),
            ("Ice.BO.GenXDataSvc".into(), "GetList".into()),
            ("Ice.BO.BAQDesignerSvc".into(), "GetFieldList".into()),
            ("Ice.BO.DynamicQuerySvc".into(), "Execute".into()),
        ]);
    }
    if category == "extended-properties" {
        // Direct only reads ZDataField overrides with an unsaved DynamicQuery (authored_schema::direct_action).
        reads.push(("Ice.BO.DynamicQuerySvc".into(), "Execute".into()));
    }
    if category == "report-behavior" {
        reads
            .extend(["ExportReportStyle"].map(|method| ("Ice.BO.ReportSvc".into(), method.into())));
    }
    if matches!(category, "kinetic-applications-and-layers" | "kinetic-personalizations") {
        reads.extend(super::kinetic::methods());
    }
    if category == "programs-and-extensions" {
        reads.extend(super::external_references::methods());
    }
    if category == "custom-resources" {
        reads.extend(super::file_store_resources::methods());
    }
    if category == "solutions-and-packages" {
        reads.extend(super::retained_solutions::methods());
    }
    reads.sort();
    reads.dedup();
    reads
}
fn sources(category: &str) -> Vec<&'static str> {
    let mut sources = Vec::new();
    if category == "report-styles-and-layouts" {
        sources.push("currentReports");
    }
    if super::families::supported(category) {
        sources.push("native");
    }
    if super::tables::supported(category) {
        sources.push("tables");
    }
    if matches!(category, "kinetic-applications-and-layers" | "kinetic-personalizations") {
        sources.push("kinetic");
    }
    if category == "programs-and-extensions" {
        sources.push("externals");
    }
    if category == "custom-resources" {
        sources.push("resources");
    }
    if category == "solutions-and-packages" {
        sources.push("retainedSolutions");
    }
    sources
}
/// Host-supplied context carried into a collector's inner continuation. A key missing here is silently
/// dropped for every inner request (the 2024.2 configurator separator depended on `_hostServerVersion`).
const INNER_HOST_CONTEXT: [&str; 12] = ["_pluginSettings", "_pluginState", "_inventory", "_streamInput", "_collectionMethod",
    "_hostSettings", "_serverConcurrency", "_hostRunId", "_hostTransferId", "_hostUtilityOperations", "_hostPreparePage",
    "_hostServerVersion"];
/// The collector's inner continuation state, keeping the host capability context.
fn continuation_state(state: &Value) -> Value {
    let mut inner = state.get("inner").cloned().unwrap_or(Value::Null);
    for key in INNER_HOST_CONTEXT {
        if let Some(value) = state.get(key) {
            if !inner.is_object() {
                inner = json!({});
            }
            inner[key] = value.clone();
        }
    }
    inner
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    if crate::category_enabled("configuration-references") && request.category == "configuration-references" {
        return super::references::handle(request);
    }
    let selected = sources(&request.category);
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(c) => c.unwrap_or(json!({"source":selected.first(),"cursor":null})),
        Err(_) => return super::failure("INVALID_CURSOR", "The category resume point is invalid."),
    };
    let source = if request.operation == "materialize" {
        request.state.get("source").and_then(Value::as_str).unwrap_or("")
    } else if request.operation == "discover" {
        cursor.get("source").and_then(Value::as_str).unwrap_or("")
    } else if request
        .identity
        .as_ref()
        .is_some_and(|id| super::report_store::owns(&request.category, &id.kind))
    {
        "currentReports"
    } else if request
        .identity
        .as_ref()
        .is_some_and(|id| super::file_store_resources::owns(&request.category, &id.kind))
    {
        "resources"
    } else if request
        .identity
        .as_ref()
        .is_some_and(|id| super::retained_solutions::owns(&request.category, &id.kind))
    {
        "retainedSolutions"
    } else if request
        .identity
        .as_ref()
        .is_some_and(|id| super::external_references::owns(&request.category, &id.kind))
    {
        "externals"
    } else if request
        .identity
        .as_ref()
        .is_some_and(|id| super::kinetic::owns(&request.category, &id.kind))
    {
        "kinetic"
    } else if request
        .identity
        .as_ref()
        .is_some_and(|id| super::tables::owns(&request.category, &id.kind))
    {
        "tables"
    } else {
        "native"
    };
    let source_owned = source.to_owned();
    let source = source_owned.as_str();
    if !selected.contains(&source) {
        return super::failure(
            "INVALID_IDENTITY",
            "This category does not implement the requested source family.",
        );
    }
    request.cursor = cursor
        .get("cursor")
        .and_then(Value::as_str)
        .map(String::from);
    let caller = request
        .state
        .get("_hostCaller")
        .cloned()
        .unwrap_or(Value::Null);
    request.state = continuation_state(&request.state);
    if source == "kinetic"
        || (source == "tables" && matches!(request.category.as_str(), "kinetic-applications-and-layers" | "kinetic-personalizations"))
    {
        if !request.state.is_object() {
            request.state = json!({});
        }
        request.state["_hostCaller"] = caller;
    }
    let discovery = request.operation == "discover";
    let reply = match source {
        "currentReports" if crate::category_enabled("report-styles-and-layouts") => super::report_store::handle(request),
        "native" => super::families::handle(request),
        "tables" => super::tables::handle(request),
        "kinetic" if crate::category_enabled("kinetic-applications-and-layers") || crate::category_enabled("kinetic-personalizations") => {
            super::kinetic::handle(request)
        }
        "externals" if crate::category_enabled("programs-and-extensions") => {
            super::external_references::handle(request)
        }
        "resources" if crate::category_enabled("custom-resources") => {
            super::file_store_resources::handle(request)
        }
        "retainedSolutions" if crate::category_enabled("solutions-and-packages") => {
            super::retained_solutions::handle(request)
        }
        _ => unreachable!(),
    };
    wrap_reply(reply, &selected, source, discovery)
}

fn wrap_reply(reply: GuestReply, selected: &[&str], source: &str, discovery: bool) -> GuestReply {
    let wrap_cursor = |cursor: Option<String>| {
        cursor.map(|inner| json!({"source":source,"cursor":inner}).to_string())
            .or_else(|| selected.iter().position(|s| s == &source)
                .and_then(|i| selected.get(i + 1))
                .map(|next| json!({"source":next,"cursor":null}).to_string()))
    };
    let (reply, records) = match reply {
        GuestReply::InventoryData { records, inventory } => (*inventory, Some(records)),
        other => (other, None),
    };
    let reply = match reply {
        // A discovery source can fail without invalidating independent sources.
        // Keep the failure visible, never infer
        // absence, and move to the next source. Acquisition semantics stay intact.
        GuestReply::Failure { diagnostic } if discovery => GuestReply::Inventory {
            identities: vec![],
            next_cursor: selected
                .iter()
                .position(|s| s == &source)
                .and_then(|i| selected.get(i + 1))
                .map(|next| json!({"source":next,"cursor":null}).to_string()),
            authoritative: false,
            diagnostics: vec![diagnostic],
        },
        GuestReply::Action { action, state } => GuestReply::Action {
            action,
            state: json!({"source":source,"inner":state}),
        },
        GuestReply::PreparedPage { state, next_cursor, next_state } => {
            // Materialization resumes this source, independently of the cursor
            // already handed to discovery. Do not carry its state into the next
            // independent source when this source has reached its end.
            let has_inner_page = next_cursor.is_some();
            let progress = state.get("_progress").cloned();
            let mut wrapped = json!({"source":source,"inner":state});
            if let Some(progress) = progress { wrapped["_progress"] = progress; }
            GuestReply::PreparedPage {
                state: wrapped,
                next_cursor: wrap_cursor(next_cursor),
                next_state: if has_inner_page {
                    json!({"source":source,"inner":next_state})
                } else { json!({}) },
            }
        },
        GuestReply::Inventory {
            identities,
            next_cursor,
            authoritative,
            diagnostics,
        } => {
            let next = if let Some(inner) = next_cursor {
                Some(json!({"source":source,"cursor":inner}).to_string())
            } else {
                selected
                    .iter()
                    .position(|s| s == &source)
                    .and_then(|i| selected.get(i + 1))
                    .map(|next| json!({"source":next,"cursor":null}).to_string())
            };
            GuestReply::Inventory {
                identities,
                next_cursor: next,
                authoritative,
                diagnostics,
            }
        }
        other => other,
    };
    if let Some(records) = records {
        GuestReply::InventoryData {
            records,
            inventory: Box::new(reply),
        }
    } else {
        reply
    }
}

#[cfg(test)]
mod prepared_source_tests {
    use super::*;
    #[test]
    fn inner_continuations_keep_the_server_version_for_version_specific_native_forms() {
        // Configurators writes 2024.2's native "pages\X" app keys only if this reaches the inner acquisition.
        let state = json!({"inner":{"phase":"configLocalApp"},"_hostServerVersion":"11.3.200.39","_pluginSettings":{},"source":"native"});
        let inner = continuation_state(&state);
        assert_eq!(inner, json!({"phase":"configLocalApp","_hostServerVersion":"11.3.200.39","_pluginSettings":{}}));
        assert_eq!(crate::configurator_app::native_separator(inner["_hostServerVersion"].as_str()), '\\');
        // A first step (no saved continuation yet) still receives it.
        assert_eq!(continuation_state(&json!({"_hostServerVersion":"12.2.100.9"}))["_hostServerVersion"], "12.2.100.9");
    }
    #[test]
    fn prepared_page_keeps_materialization_source_and_separates_next_source_state() {
        for next in [None, Some("page-two".to_string())] {
            let reply = wrap_reply(GuestReply::PreparedPage {
                state: json!({"rows":[1],"_progress":{"pageRecords":1}}),
                next_cursor: next.clone(),
                next_state: json!({"dictionary":"retained"}),
            }, &["native", "tables"], "native", true);
            let GuestReply::PreparedPage { state, next_cursor, next_state } = reply else { panic!() };
            assert_eq!(state["source"], "native");
            assert_eq!(state["inner"]["rows"], json!([1]));
            assert_eq!(state["_progress"]["pageRecords"], 1);
            let cursor: Value = serde_json::from_str(&next_cursor.unwrap()).unwrap();
            if next.is_some() {
                assert_eq!(cursor["source"], "native");
                assert_eq!(cursor["cursor"], "page-two");
                assert_eq!(next_state["inner"]["dictionary"], "retained");
            } else {
                assert_eq!(cursor["source"], "tables");
                assert_eq!(next_state, json!({}));
            }
        }
    }
    #[test]
    fn materialization_without_a_known_saved_source_is_rejected() {
        let request = GuestRequest {
            abi: PLUGIN_ABI, operation: "materialize".into(), category: "configurators".into(),
            company: "C".into(), identity: None, cursor: None, page_size: Some(10),
            state: json!({"source":"unknown"}), response: None, error: None,
        };
        assert!(matches!(handle(request), GuestReply::Failure { diagnostic } if diagnostic.code == "INVALID_IDENTITY"));
    }
}

#[cfg(test)]
mod posting_read_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "business-rules-and-mappings".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "business-rules-and-mappings".into(),
                kind: "PostingRules".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([
                    ("Company".into(), "C".into()),
                    ("ACTTypeUID".into(), "7".into()),
                ]),
            }),
            cursor: None,
            page_size: Some(20),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    fn collect(bad_table: Option<&str>, foreign: bool) -> GuestReply {
        let mut req = request();
        loop {
            match handle(req.clone()) {
                GuestReply::Action {
                    action: HostAction::Query { query },
                    state,
                } => {
                    assert_eq!(query.tables.len(), 1);
                    assert_eq!(query.tables[0].schema, "Erp");
                    let table = query.tables[0].table.as_str();
                    for (field, expected) in [("Company", json!("C")), ("ACTTypeUID", json!("7"))] {
                        let criterion = query
                            .criteria
                            .iter()
                            .find(|c| c.field == field)
                            .expect("full parent binding");
                        let value = &query.parameters[&criterion.parameter];
                        assert!(
                            value == &expected || (field == "ACTTypeUID" && value == &json!(7))
                        );
                    }
                    assert!(
                        query
                            .criteria
                            .iter()
                            .all(|c| c.field == "Company" || c.field == "ACTTypeUID")
                    );
                    // The old BO read changes IsLog for this root. A query preserves it.
                    let rows = match table {
                        "ACTType" => {
                            json!([{"Company":"C","ACTTypeUID":7,"SysRowID":"root","IsLog":true,"LogUntil":"2000-01-01T00:00:00"}])
                        }
                        "ACTRevision" => json!([
                            {"Company":"C","ACTTypeUID":7,"ACTRevisionUID":1,"RevisionStatus":"Active","SysRowID":"revision-active"},
                            {"Company":"C","ACTTypeUID":7,"ACTRevisionUID":2,"RevisionStatus":"Blocked","SysRowID":"revision-blocked"},
                            {"Company":"C","ACTTypeUID":7,"ACTRevisionUID":3,"RevisionStatus":"Draft","SysRowID":"revision-draft"}]),
                        "ACTBook" => json!([
                            {"Company":"C","ACTTypeUID":7,"ACTRevisionUID":1,"BookID":"A","DevelopMode":false,"SysRowID":"book-a"},
                            {"Company":"C","ACTTypeUID":7,"ACTRevisionUID":2,"BookID":"B","DevelopMode":true,"PartiallyUpdated":true,"SysRowID":"book-b"}]),
                        _ => {
                            json!([{"Company":"C","ACTTypeUID":7,"ACTRevisionUID":2,"BookID":"B","SysRowID":format!("{table}-source"),"AuthoredSource":"retained"}])
                        }
                    };
                    req.state = state;
                    req.response = if bad_table == Some(table) && !foreign {
                        Some(json!({}))
                    } else if bad_table == Some(table) {
                        let mut changed = rows;
                        changed[0]["Company"] = json!("Other");
                        Some(json!({"rows":changed}))
                    } else {
                        Some(json!({"rows":rows}))
                    };
                }
                GuestReply::Action { .. } => {
                    panic!("Posting acquisition dispatched a BO or export")
                }
                final_reply => return final_reply,
            }
        }
    }
    #[test]
    fn posting_uses_pure_queries_and_keeps_all_revisions_books_and_raw_source() {
        assert!(
            methods("business-rules-and-mappings")
                .iter()
                .all(|(service, _)| service != "Erp.BO.ACTTypeSvc")
        );
        let GuestReply::Definition {
            completeness,
            diagnostics,
            definition,
            ..
        } = collect(None, false)
        else {
            panic!()
        };
        assert_eq!(completeness, Completeness::Collected);
        assert!(diagnostics.is_empty());
        assert_eq!(definition["ACTRevision"].as_array().unwrap().len(), 3);
        assert_eq!(definition["ACTBook"].as_array().unwrap().len(), 2);
        assert_eq!(definition["ACTType"][0]["IsLog"], true);
        assert_eq!(definition["ACTBook"][1]["PartiallyUpdated"], true);
        assert_eq!(definition["BVRule"][0]["AuthoredSource"], "retained");
        assert_eq!(definition["BRFuncArgs"][0]["AuthoredSource"], "retained");
        assert!(definition.get("BookValRule").is_none());
    }
    #[test]
    fn missing_or_foreign_posting_child_stays_partial() {
        for (table, foreign, code) in [
            ("BVRule", false, "CHILDREN_UNVERIFIED"),
            ("BRFuncArgs", true, "CHILD_IDENTITY_MISMATCH"),
        ] {
            let GuestReply::Definition {
                completeness,
                diagnostics,
                ..
            } = collect(Some(table), foreign)
            else {
                panic!()
            };
            assert_eq!(completeness, Completeness::Partial);
            assert!(diagnostics.iter().any(|d| d.code == code));
        }
    }
}

#[cfg(test)]
mod structured_output_read_tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    const ID: &str = "EFileAP1099CE_MISC";
    const SETTINGS: &str = "<settings>\r\n  Ω &amp; value\r\n</settings>";
    fn request(company: &str) -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "business-rules-and-mappings".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "business-rules-and-mappings".into(),
                kind: "StructuredOutput".into(),
                scope: if company.is_empty() {
                    Scope::Global
                } else {
                    Scope::Company {
                        company: company.into(),
                    }
                },
                keys: BTreeMap::from([("RptStructuredOutputDefID".into(), ID.into())]),
            }),
            cursor: None,
            page_size: Some(20),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    fn collect(company: &str, bad: Option<(&str, &str)>) -> GuestReply {
        let mut req = request(company);
        let mut visited = BTreeSet::new();
        loop {
            match handle(req.clone()) {
                GuestReply::Action {
                    action: HostAction::Query { query },
                    state,
                } => {
                    assert_eq!(query.tables.len(), 1);
                    assert_eq!(query.tables[0].schema, "Ice");
                    let table = query.tables[0].table.as_str();
                    assert!(
                        visited.insert(table.to_string()),
                        "unexpected repeated read"
                    );
                    assert_eq!(query.criteria.len(), 2);
                    if table != "RptStructuredOutputDef" {
                        let marker = query
                            .criteria
                            .iter()
                            .find(|c| c.field == "SystemFlag")
                            .expect("system children excluded before body retention");
                        assert_eq!(marker.operator, "=");
                        assert_eq!(query.parameters[&marker.parameter], json!(false));
                    }
                    for (field, expected) in
                        [("Company", company), ("RptStructuredOutputDefID", ID)]
                    {
                        if field == "Company" && table != "RptStructuredOutputDef" {
                            continue;
                        }
                        let c = query
                            .criteria
                            .iter()
                            .find(|c| c.field == field)
                            .expect("complete scope and parent binding");
                        assert_eq!(query.parameters[&c.parameter], expected);
                    }
                    assert!(query.fields.iter().any(|f| f.field == "*"));
                    let mut row = json!({"Company":company,"RptStructuredOutputDefID":ID,"SysRowID":format!("{table}-row"),"SysRevID":17});
                    match table {
                        "RptStructuredOutputDef" => {
                            // The old service read enters EI compilation for this report.
                            row["RptDefID"] = json!("EI-746");
                            row["PrePostProcessingEFTHeadCompany"] = json!("EICompany");
                            row["PrePostProcessingEFTHeadUID"] = json!(746);
                            row["ComplianceRule"] = json!(SETTINGS);
                            row["AdditionalSettings"] = json!(SETTINGS);
                        }
                        "RptStructuredOutputFile" => {
                            row["RptStructuredOutputFileID"] = json!("Xml");
                            row["AdditionalSettings"] = json!(SETTINGS);
                            row["Enabled"] = json!(false);
                        }
                        "RptStructuredOutputFileElement" => {
                            row["RptStructuredOutputFileID"] = json!("Xml");
                            row["ElementID"] = json!(3);
                            row["ParentElementID"] = json!(1);
                            row["ConstantValue"] = json!(SETTINGS);
                            row["AdditionalSettings"] = json!(SETTINGS);
                            row["CalculationEFTHeadCompany"] = json!("EICompany");
                            row["CalculationEFTHeadUID"] = json!(731);
                            row["CalculationName"] = json!("Calculate");
                        }
                        "RptStructuredOutputFileXMLNamespace" => {
                            row["RptStructuredOutputFileID"] = json!("Xml");
                            row["RptStructuredOutputFileNamespaceID"] = json!(2);
                            row["Namespace"] = json!("urn:authored:namespace");
                            row["Prefix"] = json!("a");
                        }
                        _ => panic!("unexpected table {table}"),
                    }
                    if table != "RptStructuredOutputDef" {
                        // Native tenant children may belong to the current company beneath a global root.
                        row["Company"] = json!("C");
                    }
                    let mut rows = vec![row];
                    req.response = if let Some((bad_table, mode)) = bad.filter(|(t, _)| *t == table)
                    {
                        let _ = bad_table;
                        match mode {
                            "missing" => Some(json!({})),
                            "foreign" => {
                                rows[0]["RptStructuredOutputDefID"] = json!("Other");
                                Some(json!({"rows":rows}))
                            }
                            "duplicate" => {
                                rows.push(rows[0].clone());
                                Some(json!({"rows":rows}))
                            }
                            _ => panic!(),
                        }
                    } else {
                        Some(json!({"rows":rows}))
                    };
                    req.state = state;
                }
                GuestReply::Action { .. } => {
                    panic!("StructuredOutput dispatched an unsafe BO read/export")
                }
                reply => {
                    assert_eq!(visited.len(), 4);
                    return reply;
                }
            }
        }
    }
    #[test]
    fn structured_output_preserves_global_and_company_source_without_read_hooks() {
        assert!(
            methods("business-rules-and-mappings")
                .iter()
                .all(|(s, _)| s != "Ice.BO.StructuredOutputDefinitionSvc")
        );
        assert!(
            methods("business-rules-and-mappings")
                .iter()
                .any(|(s, m)| s == "Ice.BO.InfoPromptFormSvc" && m == "GetByID")
        );
        for company in ["", "C"] {
            let GuestReply::Definition {
                identity,
                definition,
                completeness,
                diagnostics,
                format,
            } = collect(company, None)
            else {
                panic!()
            };
            assert_eq!(identity, request(company).identity.unwrap());
            assert_eq!(format, "persisted-definition-tables-json");
            assert_eq!(completeness, Completeness::Collected);
            assert!(diagnostics.is_empty());
            assert_eq!(
                definition["RptStructuredOutputDef"][0]["ComplianceRule"],
                SETTINGS
            );
            assert_eq!(
                definition["RptStructuredOutputDef"][0]["AdditionalSettings"],
                SETTINGS
            );
            assert_eq!(
                definition["RptStructuredOutputFile"][0]["AdditionalSettings"],
                SETTINGS
            );
            assert_eq!(definition["RptStructuredOutputFile"][0]["Enabled"], false);
            assert_eq!(definition["RptStructuredOutputFile"][0]["Company"], "C");
            assert_eq!(
                definition["RptStructuredOutputFileElement"][0]["ConstantValue"],
                SETTINGS
            );
            assert_eq!(
                definition["RptStructuredOutputFileElement"][0]["CalculationEFTHeadUID"],
                731
            );
            assert_eq!(
                definition["RptStructuredOutputFileXMLNamespace"][0]["Namespace"],
                "urn:authored:namespace"
            );
            assert!(definition.get("RptStructuredOutputFileXMLNS").is_none());
        }
    }
    #[test]
    fn structured_output_missing_foreign_or_duplicate_children_stay_partial() {
        for (table, mode, code) in [
            ("RptStructuredOutputFile", "missing", "CHILDREN_UNVERIFIED"),
            (
                "RptStructuredOutputFileElement",
                "foreign",
                "CHILD_IDENTITY_MISMATCH",
            ),
            (
                "RptStructuredOutputFileXMLNamespace",
                "duplicate",
                "CHILD_IDENTITY_DUPLICATE",
            ),
        ] {
            let GuestReply::Definition {
                completeness,
                diagnostics,
                ..
            } = collect("", Some((table, mode)))
            else {
                panic!()
            };
            assert_eq!(completeness, Completeness::Partial);
            assert!(
                diagnostics.iter().any(|d| d.code == code),
                "{diagnostics:?}"
            );
        }
    }
}

#[cfg(test)]
mod automation_filter_continuation_tests {
 use super::*;
 use std::collections::BTreeMap;
 #[test]
 fn public_dispatch_keeps_effective_filter_through_native_and_parameter_phases(){
  for enabled in [false,true]{
   let mut r=GuestRequest{abi:PLUGIN_ABI,operation:"acquire".into(),category:"automation-and-integrations".into(),company:"C".into(),identity:Some(Identity{category:"automation-and-integrations".into(),kind:"ProcessSet".into(),scope:Scope::Company{company:"C".into()},keys:BTreeMap::from([("Company".into(),"C".into()),("SystemCode".into(),"ERP".into()),("ProcessID".into(),"Owned".into())])}),cursor:None,page_size:Some(2),state:json!({}),response:None,error:None};
   let call=|r:&mut GuestRequest|{r.state["_pluginSettings"]=json!({"filterSensitiveFields":enabled});r.state["_pluginState"]=json!({"config":{"filterSensitiveFields":!enabled}});serde_json::from_slice::<GuestReply>(&crate::dispatch("automation-and-integrations",&serde_json::to_vec(r).unwrap())).unwrap()};
   let GuestReply::Action{action:HostAction::NativeRead{service,..},state}=call(&mut r)else{panic!("expected native visibility read")};assert_eq!(service,"Ice.BO.ProcessSetSvc");assert_eq!(state["filter"],enabled);r.state=state;
   let parent=json!({"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SystemFlag":false,"SysRowID":"root","SysRevID":1});
   let task=json!({"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SysTaskNum":7,"History":false,"SystemFlag":false,"SysRowID":"task","SysRevID":2});
   r.response=Some(json!({"returnObj":{"ProcessSet":[parent.clone()],"ProcessTask":[task.clone()],"ExtensionTables":[]},"parameters":{"morePages":false}}));
   let GuestReply::Action{action:HostAction::NativeRead{service,..},state}=call(&mut r)else{panic!("expected complete field dictionary")};assert_eq!(service,"Ice.BO.ZDataTableSvc");assert_eq!(state["filter"],enabled);r.state=state;
   let common=[("Company","nvarchar"),("SystemCode","nvarchar"),("ProcessID","nvarchar"),("SystemFlag","bit"),("SysRowID","uniqueidentifier"),("SysRevID","timestamp")];
   let fields=BTreeMap::from([("ProcessSet",common.to_vec()),("ProcessTask",common.into_iter().chain([("SysTaskNum","int"),("History","bit")]).collect()),("ProcessTaskParam",common.into_iter().chain([("SysTaskNum","int"),("ParamName","nvarchar"),("ParamCharacter","nvarchar"),("ParamToken","nvarchar"),("ParamDataType","nvarchar")]).collect())]);
   let schema=fields.iter().flat_map(|(table,cols)|cols.iter().map(move|(name,ty)|json!({"DataTableID":table,"DBTableName":table,"FieldName":name,"DataType":ty,"External":false}))).collect::<Vec<_>>();
   r.response=Some(json!({"returnObj":{"ZDataTable":fields.keys().map(|t|json!({"SystemCode":"Ice","DataTableID":t})).collect::<Vec<_>>(),"ZDataField":schema},"parameters":{"morePages":false}}));
   let GuestReply::Action{action:HostAction::NativeRead{service,method,arguments,..},state}=call(&mut r)else{panic!("expected bulk persisted parameter graph")};assert_eq!(service,"Ice.BO.DynamicQuerySvc");assert_eq!(method,"Execute");assert_eq!(state["filter"],enabled);
   let formula=arguments["queryDS"]["QueryField"].as_array().unwrap().iter().find(|f|f["FieldName"]=="Payload"&&f["Formula"].as_str().is_some_and(|s|s.contains("Source4"))).unwrap()["Formula"].as_str().unwrap();
   assert_eq!(formula.contains("LOWER(Source4.ParamName)"),enabled);assert_eq!(formula.contains("STRING_ESCAPE(CONVERT(nvarchar(max),Source4.ParamToken)"),!enabled);
   r.state=state;
   let parameter=json!({"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SysTaskNum":7,"SystemFlag":false,"SysRowID":"parameter","SysRevID":3,"ParamName":"ClientSecret","ParamDataType":"String","ParamCharacter":if enabled{Value::Null}else{json!("synthetic Ω")},"ParamToken":if enabled{Value::Null}else{json!("synthetic token")}});
   let mut wire=vec![];for(table,row)in[("ProcessSet",parent),("ProcessTask",task),("ProcessTaskParam",parameter)]{let values=fields[table].iter().map(|(name,_)|row[*name].clone()).collect::<Vec<_>>();wire.push(json!({"Kind":table,"Payload":serde_json::to_string(&values).unwrap(),"Total":-1}));wire.push(json!({"Kind":table,"Payload":null,"Total":1}));}
   r.response=Some(json!({"returnObj":{"Results":wire,"Errors":[]}}));
   let GuestReply::Definition{definition,completeness,diagnostics,..}=call(&mut r)else{panic!("expected completed exact native identity")};assert_eq!(completeness,Completeness::Collected);assert!(diagnostics.is_empty());assert_eq!(definition["automationPolicy"]["filterSensitiveFields"],enabled);assert_eq!(definition["ProcessTaskParam"][0]["ParamToken"],if enabled{Value::Null}else{json!("synthetic token")});assert_eq!(definition.to_string().contains("synthetic token"),!enabled);
  }
 }
}
