//! Read complete method pages. The host overlaps the next read with this page.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const SERVICE: &str = "Ice.BO.BpMethodSvc";
pub fn methods() -> Vec<(String, String)> {
    let mut methods = crate::collectors::methods("C06");
    methods.extend([
        (SERVICE.into(), "GetRowsEx".into()),
        ("Ice.Lib.SessionModSvc".into(), "GetBaseLevel".into()),
    ]);
    methods.sort();
    methods.dedup();
    methods
}
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("METHOD_PAGE_INVALID", message)
}
fn selection(request: &GuestRequest) -> Result<(bool, bool, String), Diagnostic> {
    let family = match request.state.pointer("/_pluginSettings/directiveFamily") {
        None => "methodAndData",
        Some(Value::String(value)) if matches!(value.as_str(), "methodAndData" | "method" | "data") => value.as_str(),
        Some(_) => return Err(Diagnostic::new("INVALID_SETTING", "Directive family must select method and data directives, method directives, or data directives.")),
    };
    let prefix = match request.state.pointer("/_pluginSettings/methodCodePrefix") {
        None => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(_) => return Err(Diagnostic::new("INVALID_SETTING", "Method or table code starts with must be a string.")),
    };
    Ok((family != "data", family != "method", prefix))
}
fn with_settings(mut state: Value, from: &Value) -> Value {
    if let Some(settings) = from.get("_pluginSettings") {
        state["_pluginSettings"] = settings.clone();
    }
    state
}
pub fn handle(request: GuestRequest) -> GuestReply {
    #[cfg(feature = "ordinary-bpm-compiled")]
    {
        if crate::bpm_compiled::active(&request.state) {
            return crate::bpm_compiled::continue_work(&request)
                .unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic });
        }
        let context = request.clone();
        let reply =
            collect(request).unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic });
        return crate::bpm_compiled::begin(reply, &context)
            .unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic });
    }
    #[cfg(not(feature = "ordinary-bpm-compiled"))]
    collect(request).unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic })
}
fn source(request: &GuestRequest) -> &str {
    request
        .identity
        .as_ref()
        .and_then(|id| id.keys.get("Source").map(String::as_str))
        .or_else(|| {
            request
                .cursor
                .as_deref()
                .and_then(|cursor| cursor.split_once(':').map(|(kind, _)| kind))
        })
        .or_else(|| request.state["source"].as_str())
        .unwrap_or_else(|| if request.state.pointer("/_pluginSettings/directiveFamily").and_then(Value::as_str) == Some("data") { "DB" } else { "BO" })
}
fn kind(source: &str) -> &'static str {
    if source == "DB" {
        "DataDirectiveGroup"
    } else {
        "MethodDirectiveGroup"
    }
}
fn group(source: &str) -> &'static str {
    if source == "DB" {
        "Data directives"
    } else {
        "Method directives"
    }
}
fn collect(mut request: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if !matches!(
        request.operation.as_str(),
        "discover" | "materialize" | "acquire"
    ) {
        return Err(Diagnostic::new(
            "UNSUPPORTED_OPERATION",
            "Select discovery or an exact method identity.",
        ));
    }
    // Exact acquisition is independent of discovery settings. Active compiled
    // continuations have already been handled before this ordinary route.
    let (include_bo, include_db, prefix) = if request.operation == "acquire" {
        (true, true, String::new())
    } else {
        selection(&request)?
    };
    if request.operation != "acquire"
        && (source(&request) == "BO" && !include_bo || source(&request) == "DB" && !include_db)
    {
        return Err(invalid("The method page selects an excluded directive family."));
    }
    if request.state["phase"] == "processing" {
        return process(request);
    }
    if let Some(error) = request.error.take() {
        return Err(error);
    }
    if request.operation == "materialize" {
        request.state["phase"] = json!("processing");
        return process(request);
    }
    match request.state["phase"].as_str().unwrap_or("initial") {
        "initial" => Ok(GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.SessionModSvc".into(),
                method: "GetBaseLevel".into(),
                arguments: json!({}),
                export: false,
            },
            state: with_settings(json!({"phase":"version","_progress":{"request":"Reading the BPM package version",
                "response":"Version received; downloading complete directive definitions"}}), &request.state),
        }),
        "version" => {
            let response = request
                .response
                .as_ref()
                .ok_or_else(|| invalid("The server version is missing."))?;
            let version = ["_Major", "_Minor", "_Build", "_Revision"]
                .into_iter()
                .map(|key| {
                    response["returnObj"][key]
                        .as_u64()
                        .map(|v| v.to_string())
                        .ok_or_else(|| invalid("The server version is invalid."))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(".");
            read_page(&request, &version)
        }
        "nextPage" => read_page(&request, request.state["version"].as_str().unwrap_or("")),
        "rows" => {
            let response = request
                .response
                .as_ref()
                .ok_or_else(|| invalid("The method page is missing."))?;
            let data = &response["returnObj"];
            let more = response["parameters"]["morePages"]
                .as_bool()
                .ok_or_else(|| invalid("The method page omitted its continuation status."))?;
            let mut identities = validate_page(data, &request.company, source(&request))?;
            if more && identities.is_empty() {
                return Err(invalid("The method page did not advance."));
            }
            if request.operation == "acquire"
                && (more
                    || identities.len() != 1
                    || identities.first() != request.identity.as_ref())
            {
                return Err(invalid(
                    "The selected method was not returned with its exact identity.",
                ));
            }
            let page = request.state["page"].as_u64().unwrap_or(1);
            let source = source(&request);
            let next = if request.operation == "acquire" {
                None
            } else if more {
                Some(format!("{source}:{}", page + 1))
            } else if source == "BO" && include_db {
                Some("DB:1".into())
            } else {
                None
            };
            if request.operation != "acquire" {
                identities.retain(|id| id.keys["BpMethodCode"].starts_with(&prefix));
            }
            let mut state = request.state.clone();
            state["dataset"] = data.clone();
            // Full raw ownership/scope validation above must not be hidden by
            // filtering. Queue complete selected graphs, including future tables.
            let selected: BTreeSet<_> = identities.iter().map(|id| id.keys["BpMethodCode"].as_str()).collect();
            for rows in state["dataset"].as_object_mut().unwrap().values_mut() {
                if let Some(rows) = rows.as_array_mut() {
                    rows.retain(|row| row["BpMethodCode"].as_str().is_some_and(|code| selected.contains(code)));
                }
            }
            state["identities"] = json!(identities);
            state["records"] = json!([]);
            state["diagnostics"] = json!([]);
            state["index"] = json!(0);
            state["nextCursor"] = json!(next);
            state["phase"] = json!("processing");
            state["targeted"] = json!(request.operation == "acquire");
            if request.state["_hostPreparePage"] == true && request.operation != "acquire" {
                if request.state["_hostIndependentWorkItems"] == true
                {
                    return Ok(independent_methods(&state, &identities, next));
                }
                return Ok(GuestReply::PreparedPage {
                    next_state: with_settings(json!({"phase":"nextPage","version":state["version"]}), &state),
                    state,
                    next_cursor: next,
                });
            }
            request.state = state;
            request.response = None;
            process(request)
        }
        _ => Err(invalid("The method page checkpoint is invalid.")),
    }
}
fn independent_methods(state: &Value, identities: &[Identity], next: Option<String>) -> GuestReply {
    let mut grouped = BTreeMap::<String, serde_json::Map<String, Value>>::new();
    let tables = state["dataset"].as_object().expect("validated method dataset");
    for identity in identities {
        grouped.insert(identity.keys["BpMethodCode"].clone(), tables.iter()
            .filter(|(_, rows)| rows.is_array()).map(|(table, _)| (table.clone(), json!([]))).collect());
    }
    for (table, rows) in tables {
        for row in rows.as_array().into_iter().flatten() {
            if let Some(owner) = row["BpMethodCode"].as_str().and_then(|code| grouped.get_mut(code)) {
                owner.get_mut(table).unwrap().as_array_mut().unwrap().push(row.clone());
            }
        }
    }
    #[cfg(feature = "ordinary-bpm-compiled")]
    let batch = crate::bpm_compiled::independent_batch(state, &identities.iter().map(|identity|
        (identity.clone(), Value::Object(grouped[&identity.keys["BpMethodCode"]].clone()))).collect::<Vec<_>>());
    let mut items = Vec::new();
    for identity in identities {
        let mut item = json!({"phase":"processing","version":state["version"],"source":state["source"],
            "identities":[identity],"dataset":grouped.remove(&identity.keys["BpMethodCode"]).unwrap(),
            "records":[],"diagnostics":[],"index":0,"nextCursor":null,"targeted":false});
        for (key, value) in state.as_object().unwrap() {
            if key.starts_with('_') && key != "_progress" { item[key] = value.clone(); }
        }
        #[cfg(feature = "ordinary-bpm-compiled")]
        if batch.is_some() { item["_bpmCompiledDeferred"] = json!(true); }
        items.push(item);
    }
    #[cfg(feature = "ordinary-bpm-compiled")]
    if let Some(batch) = batch { items.push(batch); }
    GuestReply::PreparedPage {
        state: json!({"_hostWorkItems":items,"_hostWorkNextCursor":next,"_progress":{"pageRecords":identities.len()}}),
        next_state: with_settings(json!({"phase":"nextPage","version":state["version"]}), state),
        next_cursor: next,
    }
}
fn read_page(request: &GuestRequest, version: &str) -> Result<GuestReply, Diagnostic> {
    let page = request
        .cursor
        .as_deref()
        .unwrap_or("BO:1")
        .rsplit(':')
        .next()
        .unwrap_or("1")
        .parse::<i32>()
        .ok()
        .filter(|page| *page > 0)
        .ok_or_else(|| invalid("The method page number is invalid."))?;
    let source = source(request);
    if !matches!(source, "BO" | "DB") {
        return Err(invalid("Select method or data directives."));
    }
    let size = u64::from(request.page_size.unwrap_or(24));
    if size == 0 || size > i32::MAX as u64 {
        return Err(invalid(
            "The native page size must be a positive Int32 value.",
        ));
    }
    // GetRowsEx builds SQL for its first selection. SQL bit fields require 0;
    // the ordinary GetRows expression `false` fails here in both qualified labs.
    let mut filter = "SystemFlag = 0".to_owned();
    if request.operation == "acquire" {
        let id = request
            .identity
            .as_ref()
            .ok_or_else(|| invalid("Select an exact method identity."))?;
        if id.category != "C06"
            || id.kind != kind(source)
            || id.keys.len() != 2
            || id.keys.get("Source").map(String::as_str) != Some(source)
            || id.scope != Scope::Global
        {
            return Err(invalid(
                "The selected method identity does not match this company.",
            ));
        }
        let code = id
            .keys
            .get("BpMethodCode")
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid("The method code is missing."))?;
        filter.push_str(&format!(
            " AND BpMethodCode = '{}'",
            code.replace('\'', "''")
        ));
    }
    filter.push_str(" BY BpMethodCode");
    Ok(GuestReply::Action {
        action: HostAction::NativeRead {
            service: SERVICE.into(),
            method: "GetRowsEx".into(),
            export: false,
            arguments: json!({"source":source,"whereClauseBpMethod":filter,
                "whereClauseBpDirective":"DirectiveGroup IS NULL OR DirectiveGroup >= ''",
                "pageSize":size,"absolutePage":page}),
        },
        state: with_settings(json!({"phase":"rows","page":page,"source":source,"version":version,
            "_progress":{"request":format!("Downloading {} page {page}", group(source)),
                "response":"Method definitions received; building BPM files and readable code"}}), &request.state),
    })
}

fn validate_page(data: &Value, company: &str, source: &str) -> Result<Vec<Identity>, Diagnostic> {
    for table in ["BpMethod", "BpArgument", "BpDirective"] {
        if !data[table].is_array() {
            return Err(invalid("The method page omitted a required table."));
        }
    }
    let mut methods = BTreeSet::new();
    let mut identities = Vec::new();
    for row in data["BpMethod"].as_array().unwrap() {
        let code = row["BpMethodCode"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid("A method has no code."))?;
        if row["Source"] != source || row["SystemFlag"] != false || !methods.insert(code) {
            return Err(invalid(
                "The page contains a duplicate, system, or non-method definition.",
            ));
        }
        identities.push(Identity {
            category: "C06".into(),
            kind: kind(source).into(),
            scope: Scope::Global,
            keys: BTreeMap::from([
                ("Source".into(), source.into()),
                ("BpMethodCode".into(), code.into()),
            ]),
        });
    }
    for (table, rows) in data
        .as_object()
        .ok_or_else(|| invalid("The method tableset is invalid."))?
    {
        let Some(rows) = rows.as_array() else {
            continue;
        };
        for row in rows {
            if row["Source"] != source
                || row["BpMethodCode"]
                    .as_str()
                    .is_none_or(|code| !methods.contains(code))
            {
                return Err(invalid("A method child row has an unverified parent."));
            }
            if table == "BpDirective"
                && row["Company"]
                    .as_str()
                    .is_none_or(|value| !value.is_empty() && value != company)
            {
                return Err(invalid("A directive belongs to another company."));
            }
        }
    }
    Ok(identities)
}

fn process(mut request: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let identities: Vec<Identity> = serde_json::from_value(request.state["identities"].clone())
        .map_err(|_| invalid("The method page identities are invalid."))?;
    let mut index = request.state["index"].as_u64().unwrap_or(0) as usize;
    while let Some(identity) = identities.get(index) {
        let code = &identity.keys["BpMethodCode"];
        let dataset: Value = Value::Object(
            request.state["dataset"]
                .as_object()
                .unwrap()
                .iter()
                .filter_map(|(table, rows)| {
                    rows.as_array().map(|rows| {
                        (
                            table.clone(),
                            json!(
                                rows.iter()
                                    .filter(|row| row["BpMethodCode"] == *code)
                                    .collect::<Vec<_>>()
                            ),
                        )
                    })
                })
                .collect(),
        );
        let native = request.state["inner"].is_object()
            || crate::method_archive::needs_native_export(&dataset).unwrap_or(true);
        let result = if native {
            // The native dependency needs this method only. Do not copy the
            // complete method page and all completed packages for each call.
            let inner = GuestRequest {
                abi: request.abi,
                operation: "acquire".into(),
                category: request.category.clone(),
                company: request.company.clone(),
                identity: Some(identity.clone()),
                cursor: request.cursor.clone(),
                page_size: request.page_size,
                state: if request.state["inner"].is_object() {
                    request.state["inner"].clone()
                } else {
                    json!({"_inventory":{"response":{"returnObj":dataset}}})
                },
                response: request.response.take(),
                error: request.error.take(),
            };
            match crate::families::handle(inner) {
                GuestReply::Action { action, state } => {
                    request.state["inner"] = state;
                    request.state["_progress"] = json!({"request":format!("Exporting native dependencies for {code}"),
                        "response":"Native package received; preserving its workflow resources"});
                    return Ok(GuestReply::Action {
                        action,
                        state: request.state,
                    });
                }
                GuestReply::Definition {
                    definition,
                    diagnostics,
                    completeness,
                    ..
                } => Ok((definition, diagnostics, completeness)),
                GuestReply::Failure { diagnostic } => Err(diagnostic),
                _ => Err(invalid(
                    "The native dependency exporter returned an invalid reply.",
                )),
            }
        } else {
            local_definition(
                &dataset,
                code,
                request.state["version"].as_str().unwrap_or(""),
            ).map(|(definition, diagnostics)| (definition, diagnostics, Completeness::Collected))
        };
        let (mut definition, mut diagnostics, mut completeness) = match result {
            Ok(result) => result,
            Err(error) => (
                json!({"service":SERVICE,"definition":dataset,"herbertDirectOutput":true}),
                vec![error],
                Completeness::Partial,
            ),
        };
        definition["herbertOutputGroup"] = json!(group(&identity.keys["Source"]));
        // The paged native action ran before a per-artifact acquisition context.
        // Carry its verified file reference into output processing explicitly.
        if let Some(file) = definition
            .pointer("/supplements/directivePackage/returnObj/Data/nativeFile")
            .cloned()
        {
            if !definition["herbertSources"].is_object() {
                definition["herbertSources"] = json!({});
            }
            definition["herbertSources"]["nativeFiles"] =
                json!([{"name":format!("{}.bpm",crate::bpm_code::label(code)),"file":file}]);
        }
        definition["herbertDirectOutput"] = json!(true);
        for diagnostic in &mut diagnostics {
            diagnostic.identity = Some(identity.clone());
        }
        if completeness == Completeness::Collected && !diagnostics.is_empty() {
            completeness = Completeness::Partial;
        }
        if completeness != Completeness::Collected {
            definition["herbertCollectionStatus"] = json!({"completeness":completeness,"diagnostics":diagnostics});
        }
        request.state["records"]
            .as_array_mut()
            .unwrap()
            .push(json!((identity, definition)));
        request.state["diagnostics"]
            .as_array_mut()
            .unwrap()
            .extend(diagnostics.into_iter().map(|d| json!(d)));
        index += 1;
        request.state["index"] = json!(index);
        request.state["inner"] = Value::Null;
        request.response = None;
        request.error = None;
    }
    let mut records: Vec<(Identity, Value)> =
        serde_json::from_value(request.state["records"].clone())
            .map_err(|_| invalid("The method output checkpoint is invalid."))?;
    let diagnostics: Vec<Diagnostic> = serde_json::from_value(request.state["diagnostics"].clone())
        .map_err(|_| invalid("The method diagnostics checkpoint is invalid."))?;
    if request.state["targeted"] == true {
        let (identity, definition) = records
            .pop()
            .ok_or_else(|| invalid("The selected method is missing."))?;
        let completeness = serde_json::from_value(definition["herbertCollectionStatus"]["completeness"].clone())
            .unwrap_or(if diagnostics.is_empty() { Completeness::Collected } else { Completeness::Partial });
        return Ok(GuestReply::Definition {
            identity,
            definition,
            format: "native-method-export".into(),
            completeness,
            diagnostics,
        });
    }
    Ok(GuestReply::InventoryData {
        records,
        inventory: Box::new(GuestReply::Inventory {
            identities,
            authoritative: diagnostics.is_empty(),
            diagnostics,
            next_cursor: request.state["nextCursor"].as_str().map(str::to_owned),
        }),
    })
}
fn local_definition(
    dataset: &Value,
    code: &str,
    version: &str,
) -> Result<(Value, Vec<Diagnostic>), Diagnostic> {
    let bytes = crate::method_archive::build(dataset, version)?;
    let mut definition = json!({"service":SERVICE,"definition":dataset,"herbertDirectOutput":true});
    let diagnostics = crate::bpm_code::attach(&mut definition, dataset);
    if !definition["herbertSources"].is_object() {
        definition["herbertSources"] = json!({});
    }
    definition["herbertSources"]["binaryFiles"] = json!([{"name":format!("{}.bpm",crate::bpm_code::label(code)),
        "bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),
        "base64":base64::engine::general_purpose::STANDARD.encode(bytes)}]);
    Ok((definition, diagnostics))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn two_owner_request() -> GuestRequest {
        let mut r = request();
        let data = &mut r.response.as_mut().unwrap()["returnObj"];
        for table in ["BpMethod", "BpDirective"] {
            let mut other = data[table][0].clone();
            other["BpMethodCode"] = json!("Ice.BO.Y.Update");
            data[table].as_array_mut().unwrap().push(other);
        }
        data["BpDirective"][0]["Body"] = json!("<Workflow><BpmDataFormAction FormId='Prompt'/></Workflow>");
        data["FutureTable"] = json!([{"Source":"BO","BpMethodCode":"Ice.BO.Y.Update","Future":"é"}]);
        r
    }
    fn finish_refused(mut r: GuestRequest, mut reply: GuestReply) -> (Vec<(Identity, Value)>, Vec<Value>) {
        let mut actions = Vec::new();
        loop {
            match reply {
                GuestReply::Action { action, state } => {
                    actions.push(json!(action));
                    r.state = state;
                    if matches!(action, HostAction::SharedQuery { .. }) {
                        r.response = Some(json!({"rows":[]})); r.error = None;
                    } else { r.response = None; r.error = Some(Diagnostic::new("TEST_REFUSAL", "Fixture refusal")); }
                    reply = handle(r.clone());
                }
                GuestReply::InventoryData { records, .. } => return (records, actions),
                other => panic!("{other:?}"),
            }
        }
    }
    #[test]
    fn independent_method_dependencies_match_legacy_actions_and_exact_outputs() {
        let mut legacy = two_owner_request();
        legacy.state["_hostPreparePage"] = json!(false);
        let (expected, actions) = finish_refused(legacy.clone(), handle(legacy));
        let mut r = two_owner_request();
        r.state["_hostIndependentWorkItems"] = json!(true);
        let GuestReply::PreparedPage { state, next_cursor, .. } = handle(r.clone()) else { panic!() };
        assert_eq!(next_cursor.as_deref(), Some("BO:2"));
        assert_eq!(state["_hostWorkNextCursor"], "BO:2");
        assert_eq!(state["_progress"]["pageRecords"], 2);
        let items = state["_hostWorkItems"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let mut actual = Vec::new(); let mut actual_actions = Vec::new();
        for item in items {
            assert_eq!(item["dataset"]["BpMethod"].as_array().unwrap().len(), 1);
            assert!(item["records"].as_array().unwrap().is_empty());
            r.state = item.clone(); r.operation = "materialize".into(); r.response = None; r.error = None;
            let (records, calls) = finish_refused(r.clone(), handle(r.clone()));
            actual.extend(records); actual_actions.extend(calls);
        }
        assert_eq!(json!(actual), json!(expected)); assert_eq!(actual_actions, actions);
    }
    #[cfg(feature = "ordinary-bpm-compiled")]
    #[test]
    fn independent_compiled_batch_keeps_one_read_and_exact_owner_contexts() {
        let mut r = two_owner_request();
        r.state["_hostIndependentWorkItems"] = json!(true);
        r.state["_pluginSettings"] = json!({"downloadCompiledAssemblies":true});
        r.state["_collectionMethod"] = json!("serverAssisted");
        r.state["_hostCapabilities"] = json!({"localDecompilationV1":{"protocolVersion":1,"engineId":MANAGED_ENGINE_ID,"retainedArtifactFunctions":true}});
        let GuestReply::PreparedPage { state, .. } = handle(r.clone()) else { panic!() };
        let items = state["_hostWorkItems"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        let contexts = items[2]["_bpmCompiledOwners"].as_array().unwrap();
        for (index, item) in items[..2].iter().enumerate() {
            r.state = item.clone(); r.operation = "materialize".into(); r.response = None; r.error = None;
            let (records, calls) = finish_refused(r.clone(), handle(r.clone()));
            assert!(calls.iter().all(|call| !call.to_string().contains("ReadCompiled")));
            let files = records[0].1["herbertSources"]["files"].as_array().unwrap();
            let saved = files.iter().find(|f| f["name"] == "compiled-context.json").unwrap();
            assert_eq!(serde_json::from_str::<Value>(saved["text"].as_str().unwrap()).unwrap(), contexts[index][1]);
        }
        assert!(items[2]["dataset"].is_null());
        r.state = items[2].clone(); r.response = None; r.error = None;
        let GuestReply::Action { action: HostAction::RetainedLocalArtifactFunctionCall { function, arguments, .. }, .. } = handle(r) else { panic!() };
        assert_eq!(function, "ReadCompiled");
        assert_eq!(serde_json::from_str::<Vec<Value>>(arguments["iOwners"].as_str().unwrap()).unwrap().len(), 2);
    }
    fn data_request() -> GuestRequest {
        let mut r = request();
        r.state["source"] = json!("DB");
        let ds = &mut r.response.as_mut().unwrap()["returnObj"];
        for table in ["BpMethod", "BpDirective"] {
            for row in ds[table].as_array_mut().unwrap() {
                row["Source"] = json!("DB");
                row["BpMethodCode"] = json!("Ice.UD39.Update");
            }
        }
        ds["BpMethod"][0]["BusinessObject"] = json!("UD39");
        ds["BpMethod"][0]["ObjectNS"] = json!("");
        ds["BpDirective"][0]["DirectiveType"] = json!(0);
        let mut standard = ds["BpDirective"][0].clone();
        standard["DirectiveType"] = json!(1);
        ds["BpDirective"].as_array_mut().unwrap().push(standard);
        r.response.as_mut().unwrap()["parameters"]["morePages"] = json!(false);
        r
    }
    #[test]
    fn empty_method_source_continues_to_data_directives() {
        let mut r = request();
        r.response = Some(
            json!({"returnObj":{"BpMethod":[],"BpArgument":[],"BpDirective":[]},"parameters":{"morePages":false}}),
        );
        let GuestReply::PreparedPage {
            next_state,
            next_cursor,
            ..
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(next_cursor.as_deref(), Some("DB:1"));
        r.state = next_state;
        r.cursor = next_cursor;
        r.response = None;
        let GuestReply::Action {
            action: HostAction::NativeRead { arguments, .. },
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(arguments["source"], "DB");
        assert_eq!(arguments["absolutePage"], 1);
    }
    #[test]
    fn data_page_preserves_both_types_and_finishes_inventory() {
        let mut r = data_request();
        r.state["_hostPreparePage"] = json!(false);
        let GuestReply::InventoryData { records, inventory } = handle(r) else {
            panic!()
        };
        assert_eq!(records[0].0.kind, "DataDirectiveGroup");
        assert_eq!(records[0].0.category, "C06");
        assert_eq!(records[0].1["herbertOutputGroup"], "Data directives");
        assert_eq!(
            records[0].1["definition"]["BpDirective"][0]["DirectiveType"],
            0
        );
        assert_eq!(
            records[0].1["definition"]["BpDirective"][1]["DirectiveType"],
            1
        );
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                next_cursor: None,
                authoritative: true,
                ..
            }
        ));
    }
    #[test]
    fn targeted_data_identity_uses_native_table_source() {
        let mut r = data_request();
        r.operation = "acquire".into();
        r.identity = Some(
            validate_page(&r.response.as_ref().unwrap()["returnObj"], "C", "DB")
                .unwrap()
                .remove(0),
        );
        let GuestReply::Action {
            action: HostAction::NativeRead { arguments, .. },
            ..
        } = read_page(&r, "5.2.100.0").unwrap()
        else {
            panic!()
        };
        assert_eq!(arguments["source"], "DB");
        assert!(
            arguments["whereClauseBpMethod"]
                .as_str()
                .unwrap()
                .contains("Ice.UD39.Update")
        );
        r.identity.as_mut().unwrap().kind = "MethodDirectiveGroup".into();
        assert!(read_page(&r, "5.2.100.0").is_err());
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C06".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(1),
            state: json!({"phase":"rows","version":"5.2.100.0","page":1,"_hostPreparePage":true}),
            response: Some(
                json!({"returnObj":{"BpMethod":[{"Source":"BO","BpMethodCode":"Ice.BO.X.Update","SystemFlag":false,
                "SystemCode":"ICE","ObjectNS":"BO","BusinessObject":"X"}],"BpArgument":[],
                "BpDirective":[{"Source":"BO","BpMethodCode":"Ice.BO.X.Update","Company":"C","Body":"<Workflow><FutureWidget Position='12,34'/></Workflow>"}],
                "ExtensionTables":[]},"parameters":{"morePages":true}}),
            ),
            error: None,
        }
    }
    fn prefix_fixture(source: &str) -> GuestRequest {
        let mut r = if source == "BO" { request() } else { data_request() };
        let data = &mut r.response.as_mut().unwrap()["returnObj"];
        let first = data["BpMethod"][0]["BpMethodCode"].as_str().unwrap().to_owned();
        let peer = if source == "BO" { "Ice.BO.Y.Update" } else { "Ice.UD40.Update" };
        for table in ["BpMethod", "BpDirective"] {
            let mut row = data[table][0].clone(); row["BpMethodCode"] = json!(peer);
            if table == "BpMethod" {row["BusinessObject"] = json!(if source == "BO" {"Y"} else {"UD40"});}
            data[table].as_array_mut().unwrap().push(row);
        }
        data["BpArgument"] = json!([
            {"Source":source,"BpMethodCode":first,"Ordinal":1,"Name":"all arguments","TypeInfo":"original Ω"},
            {"Source":source,"BpMethodCode":peer,"Ordinal":1,"Name":"peer"}]);
        data["FutureTable"] = json!([
            {"Source":source,"BpMethodCode":first,"Original":" Ω%_ '","Nullable":null,"WideInteger":9007199254740993u64},
            {"Source":source,"BpMethodCode":peer,"Original":"peer"}]);
        r.response.as_mut().unwrap()["parameters"]["morePages"] = json!(false);
        r
    }
    fn plain_records(mut r: GuestRequest) -> Vec<(Identity, Value)> {
        r.state["_hostPreparePage"] = json!(false);
        let GuestReply::InventoryData {records, inventory} = handle(r) else {panic!("Expected ordinary definitions")};
        assert!(matches!(*inventory, GuestReply::Inventory {authoritative:true,diagnostics,..} if diagnostics.is_empty()));
        records
    }
    #[test]
    fn prefix_keeps_complete_bo_db_graphs_and_literal_native_codes() {
        for source in ["BO", "DB"] {
            let r = prefix_fixture(source);
            let baseline = plain_records(r.clone()); assert_eq!(baseline.len(), 2);
            let code = baseline[0].0.keys["BpMethodCode"].clone();
            for (prefix, count) in [("".to_owned(),2), (code.clone(),1), (code.to_ascii_lowercase(),0), (format!(" {code}"),0), ("%".to_owned(),0)] {
                let mut selected = r.clone(); selected.state["_pluginSettings"] = json!({"methodCodePrefix":prefix});
                let records = plain_records(selected); assert_eq!(records.len(), count);
                for record in &records {assert_eq!(record, baseline.iter().find(|(id,_)| id == &record.0).unwrap());}
                if count == 1 && source == "DB" {assert_eq!(records[0].1["definition"]["BpDirective"].as_array().unwrap().len(),2);}
            }
            let mut saved = r.clone(); saved.state["_pluginState"] = json!({"config":{"methodCodePrefix":"missing","directiveFamily":"method"}});
            assert_eq!(plain_records(saved), baseline, "Only effective settings are used");
            let mut unicode = r; let data = &mut unicode.response.as_mut().unwrap()["returnObj"];
            for rows in data.as_object_mut().unwrap().values_mut().filter_map(Value::as_array_mut) {
                for row in rows {if row["BpMethodCode"] == code {row["BpMethodCode"] = json!(" Ω%_ '");}}
            }
            unicode.state["_pluginSettings"] = json!({"methodCodePrefix":" Ω%_ '"});
            assert_eq!(plain_records(unicode).len(),1);
        }
    }
    #[test]
    fn prefix_validates_all_raw_owners_before_selection_and_rejects_bad_settings() {
        for source in ["BO", "DB"] {
            for problem in ["orphan", "source", "company", "root"] {
                let mut r = prefix_fixture(source);
                let code = r.response.as_ref().unwrap()["returnObj"]["BpMethod"][0]["BpMethodCode"].clone();
                r.state["_pluginSettings"] = json!({"methodCodePrefix":code});
                let data = &mut r.response.as_mut().unwrap()["returnObj"];
                match problem {
                    "orphan" => data["FutureTable"][1]["BpMethodCode"] = json!("missing"),
                    "source" => data["BpArgument"][1]["Source"] = json!("wrong"),
                    "company" => data["BpDirective"].as_array_mut().unwrap().last_mut().unwrap()["Company"] = json!("foreign"),
                    _ => data["BpMethod"][1]["SystemFlag"] = json!(true),
                }
                assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "METHOD_PAGE_INVALID"),"{source}/{problem}");
            }
        }
        for invalid in [Value::Null,json!(true),json!(4),json!([]),json!({})] {
            let mut r = request(); r.state["_pluginSettings"] = json!({"methodCodePrefix":invalid});
            assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));
        }
        for invalid in [Value::Null,json!("BO"),json!(false)] {
            let mut r = request(); r.state["_pluginSettings"] = json!({"directiveFamily":invalid});
            assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));
        }
    }
    #[test]
    fn prefix_family_and_frozen_settings_survive_version_empty_pages_and_source_transitions() {
        for (family, source, last) in [("methodAndData","BO",Some("DB:1")),("method","BO",None),("data","DB",None)] {
            let settings = json!({"directiveFamily":family,"methodCodePrefix":"Missing Ω"});
            let mut r = request(); r.state = json!({"_pluginSettings":settings}); r.response = None;
            let GuestReply::Action {state,..} = handle(r.clone()) else {panic!()};
            assert_eq!(state["_pluginSettings"],settings);
            r.state = state; r.response = Some(json!({"returnObj":{"_Major":5,"_Minor":2,"_Build":100,"_Revision":0}}));
            let GuestReply::Action {action:HostAction::NativeRead {arguments,..},state} = handle(r.clone()) else {panic!()};
            assert_eq!(arguments["source"],source); assert_eq!(arguments["absolutePage"],1);
            assert_eq!(state["_pluginSettings"],settings);
            r.state = state; r.state["_hostPreparePage"] = json!(true);
            r.response = prefix_fixture(source).response;
            r.response.as_mut().unwrap()["parameters"]["morePages"] = json!(true);
            let GuestReply::PreparedPage {state,next_state,next_cursor} = handle(r.clone()) else {panic!()};
            assert_eq!(next_cursor.as_deref(),Some(format!("{source}:2").as_str()));
            assert_eq!(state["identities"],json!([])); assert_eq!(next_state["_pluginSettings"],settings);
            for rows in state["dataset"].as_object().unwrap().values().filter_map(Value::as_array) {assert!(rows.is_empty());}
            r.state = next_state; r.cursor = next_cursor; r.response = None;
            let GuestReply::Action {action:HostAction::NativeRead {arguments,..},state} = handle(r.clone()) else {panic!()};
            assert_eq!(arguments["source"],source); assert_eq!(arguments["absolutePage"],2);
            r.state = state; r.state["_hostPreparePage"] = json!(true); r.response = prefix_fixture(source).response;
            let GuestReply::PreparedPage {next_state,next_cursor,..} = handle(r.clone()) else {panic!()};
            assert_eq!(next_cursor.as_deref(),last); assert_eq!(next_state["_pluginSettings"],settings);
            if last.is_some() {
                r.state = next_state; r.cursor = next_cursor; r.response = None;
                let GuestReply::Action {action:HostAction::NativeRead {arguments,..},..} = handle(r) else {panic!()};
                assert_eq!(arguments["source"],"DB"); assert_eq!(arguments["absolutePage"],1);
            }
        }
    }
    #[test]
    fn prefix_prepared_and_independent_items_match_whole_selected_legacy_output() {
        for source in ["BO", "DB"] {
            let mut r = prefix_fixture(source); let baseline = plain_records(r.clone());
            r.state["_pluginSettings"] = json!({"methodCodePrefix":baseline[0].0.keys["BpMethodCode"]});
            for independent in [false,true] {
                r.state["_hostIndependentWorkItems"] = json!(independent);
                let GuestReply::PreparedPage {state,..} = handle(r.clone()) else {panic!()};
                let items = if independent {state["_hostWorkItems"].as_array().unwrap().clone()} else {vec![state]};
                assert_eq!(items.len(),1);
                let mut materialize = r.clone(); materialize.operation = "materialize".into(); materialize.state = items[0].clone(); materialize.response = None;
                let GuestReply::InventoryData {records,..} = handle(materialize) else {panic!()};
                assert_eq!(records,vec![baseline[0].clone()]);
            }
        }
    }
    #[test]
    fn prefix_exact_bo_and_db_acquisition_ignore_excluded_family_and_prefix() {
        for source in ["BO", "DB"] {
            let mut r = prefix_fixture(source);
            let identity = validate_page(&r.response.as_ref().unwrap()["returnObj"],"C",source).unwrap().remove(0);
            for rows in r.response.as_mut().unwrap()["returnObj"].as_object_mut().unwrap().values_mut().filter_map(Value::as_array_mut) {
                rows.retain(|row| row["BpMethodCode"] == identity.keys["BpMethodCode"]);
            }
            r.operation = "acquire".into(); r.identity = Some(identity);
            let baseline = serde_json::to_value(handle(r.clone())).unwrap(); assert_eq!(baseline["type"],"definition");
            let args = serde_json::to_value(read_page(&r,"5.2.100.0").unwrap()).unwrap()["action"].clone();
            r.state["_pluginSettings"] = json!({"directiveFamily":if source == "BO" {"data"} else {"method"},"methodCodePrefix":"Missing"});
            assert_eq!(serde_json::to_value(handle(r.clone())).unwrap(),baseline);
            assert_eq!(serde_json::to_value(read_page(&r,"5.2.100.0").unwrap()).unwrap()["action"],args);
        }
    }
    #[test]
    fn prefix_only_selected_owners_initiate_required_native_dependency_fallback() {
        for source in ["BO", "DB"] {
            let mut r = prefix_fixture(source); r.state["_hostPreparePage"] = json!(false);
            let code = r.response.as_ref().unwrap()["returnObj"]["BpMethod"][0]["BpMethodCode"].clone();
            r.state["_pluginSettings"] = json!({"methodCodePrefix":code});
            let data = &mut r.response.as_mut().unwrap()["returnObj"];
            data["BpDirective"].as_array_mut().unwrap().last_mut().unwrap()["Body"] = json!("<Workflow><BpmDataFormAction FormId='Peer'/></Workflow>");
            let ordinary = plain_records(r.clone()); assert_eq!(ordinary.len(),1);
            for directive in r.response.as_mut().unwrap()["returnObj"]["BpDirective"].as_array_mut().unwrap() {
                if directive["BpMethodCode"] == code {directive["Body"] = json!("<Workflow><BpmDataFormAction FormId='Selected'/></Workflow>");}
            }
            let (records,actions) = finish_refused(r.clone(),handle(r)); assert_eq!(records.len(),1);
            assert_eq!(records[0].0.keys["BpMethodCode"],code.as_str().unwrap());
            let exports: Vec<_> = actions.iter().filter(|action| action["method"] == "ExportByService" || action["method"] == "ExportByTable").collect();
            assert_eq!(exports.len(),1);
            assert_eq!(exports[0]["arguments"][if source == "BO" {"serviceName"} else {"tableName"}],if source == "BO" {"X"} else {"UD39"});
        }
    }
    #[cfg(feature = "ordinary-bpm-compiled")]
    #[test]
    fn prefix_optional_compiled_batches_contain_only_selected_bo_db_owners_after_normal_output() {
        for source in ["BO", "DB"] {
            for independent in [false,true] {
                let mut r = prefix_fixture(source);
                let code = r.response.as_ref().unwrap()["returnObj"]["BpMethod"][0]["BpMethodCode"].clone();
                r.state["_hostIndependentWorkItems"] = json!(independent);
                r.state["_pluginSettings"] = json!({"methodCodePrefix":code,"downloadCompiledAssemblies":true});
                r.state["_collectionMethod"] = json!("serverAssisted");
                r.state["_hostCapabilities"] = json!({"localDecompilationV1":{"protocolVersion":1,"engineId":MANAGED_ENGINE_ID,"retainedArtifactFunctions":true}});
                let GuestReply::PreparedPage {state,..} = handle(r.clone()) else {panic!()};
                let mut materialize = r; materialize.operation = "materialize".into(); materialize.response = None;
                if independent {
                    let items = state["_hostWorkItems"].as_array().unwrap(); assert_eq!(items.len(),2);
                    assert_eq!(items[1]["_bpmCompiledOwners"].as_array().unwrap().len(),1);
                    materialize.state = items[0].clone();
                    let GuestReply::InventoryData {records,..} = handle(materialize.clone()) else {panic!()};
                    assert_eq!(records.len(),1); assert_eq!(records[0].0.keys["BpMethodCode"],code.as_str().unwrap());
                    assert!(records[0].1["herbertSources"]["binaryFiles"].is_array());
                    materialize.state = items[1].clone();
                } else {
                    materialize.state = state;
                    let GuestReply::ReadyRecords {records,state} = handle(materialize.clone()) else {panic!()};
                    assert_eq!(records.len(),1); assert_eq!(records[0].0.keys["BpMethodCode"],code.as_str().unwrap());
                    assert!(records[0].1["herbertSources"]["binaryFiles"].is_array()); materialize.state = state;
                }
                let GuestReply::Action {action:HostAction::RetainedLocalArtifactFunctionCall {function,arguments,..},..} = handle(materialize) else {panic!()};
                assert_eq!(function,"ReadCompiled");
                assert_eq!(serde_json::from_str::<Value>(arguments["iOwners"].as_str().unwrap()).unwrap(),json!([{"source":source,"method":code}]));
            }
        }
    }
    #[test]
    fn prepare_page_separates_next_read_from_local_materialization() {
        let r = request();
        let GuestReply::PreparedPage {
            state,
            next_state,
            next_cursor,
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(next_cursor.as_deref(), Some("BO:2"));
        let mut next = r.clone();
        next.state = next_state;
        next.cursor = next_cursor;
        next.response = None;
        let GuestReply::Action {
            action: HostAction::NativeRead {
                method, arguments, ..
            },
            ..
        } = handle(next)
        else {
            panic!()
        };
        assert_eq!(method, "GetRowsEx");
        assert_eq!(arguments["absolutePage"], 2);
        assert_eq!(
            arguments["whereClauseBpMethod"],
            "SystemFlag = 0 BY BpMethodCode"
        );
        let mut materialize = r;
        materialize.state = state;
        materialize.operation = "materialize".into();
        materialize.response = None;
        let GuestReply::InventoryData { records, inventory } = handle(materialize) else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: true,
                ..
            }
        ));
        assert_eq!(
            records[0].1["definition"]["BpDirective"][0]["Body"],
            "<Workflow><FutureWidget Position='12,34'/></Workflow>"
        );
        assert!(
            records[0].1["herbertSources"]["binaryFiles"][0]["name"]
                .as_str()
                .unwrap()
                .ends_with(".bpm")
        );
    }
    #[test]
    fn dependency_export_resumes_and_retains_definition_on_failure() {
        let mut r = request();
        r.state["_hostPreparePage"] = json!(false);
        r.response.as_mut().unwrap()["returnObj"]["BpDirective"][0]["Body"] =
            json!("<Workflow><BpmDataFormAction FormId='Prompt'/></Workflow>");
        let GuestReply::Action {
            action: HostAction::SharedQuery { .. },
            state,
        } = handle(r.clone())
        else {
            panic!()
        };
        r.state = state;
        r.response = Some(json!({"rows":[]}));
        let GuestReply::Action {
            action: HostAction::SharedNativeRead { method, .. },
            state,
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(method, "ExportByService");
        r.state = state;
        r.response = None;
        r.error = Some(Diagnostic::new("TEST_REFUSAL", "Fixture refusal"));
        let GuestReply::InventoryData { records, inventory } = handle(r) else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        assert!(records[0].1["definition"]["BpDirective"].is_array());
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: false,
                ..
            }
        ));
    }
    #[test]
    fn missing_paging_and_foreign_children_cannot_report_complete() {
        let mut r = request();
        r.response.as_mut().unwrap()["parameters"] = json!({});
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
        let mut r = request();
        r.response.as_mut().unwrap()["returnObj"]["BpDirective"][0]["Company"] = json!("OTHER");
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
        let mut r = request();
        r.response.as_mut().unwrap()["returnObj"]["BpArgument"] =
            json!([{"Source":"BO","BpMethodCode":"other"}]);
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
    #[cfg(feature = "ordinary-bpm-compiled")]
    #[test]
    fn compiled_bpm_optional_read_follows_one_normal_materialized_handoff() {
        let mut r = request();
        r.response.as_mut().unwrap()["returnObj"]["BpDirective"][0]["Body"] = json!(
            r#"<DirectiveDefinition2><DirectiveStep DisplayName="Exact"><CustomCodeAction Code="System.GC.KeepAlive(&quot;owned-page-marker&quot;);"/></DirectiveStep></DirectiveDefinition2>"#
        );
        let GuestReply::PreparedPage { state, .. } = handle(r.clone()) else {
            panic!()
        };
        let mut materialize = r;
        materialize.state = state;
        materialize.operation = "materialize".into();
        materialize.response = None;
        let GuestReply::InventoryData {
            records: ordinary, ..
        } = handle(materialize.clone())
        else {
            panic!()
        };
        materialize.state["_pluginSettings"] =
            json!({"downloadCompiledAssemblies":true,"decompileCompiledAssemblies":true});
        materialize.state["_collectionMethod"] = json!("serverAssisted");
        materialize.state["_hostCapabilities"] = json!({"localDecompilationV1":{"protocolVersion":1,"engineId":MANAGED_ENGINE_ID,"retainedArtifactFunctions":true}});
        let GuestReply::ReadyRecords { records, state } = handle(materialize.clone()) else {
            panic!("Native BPM files must enter the writer before optional reads")
        };
        assert_eq!(records.len(), ordinary.len());
        assert_eq!(records[0].1["definition"], ordinary[0].1["definition"]);
        assert_eq!(
            records[0].1["herbertSources"]["binaryFiles"],
            ordinary[0].1["herbertSources"]["binaryFiles"]
        );
        assert_eq!(
            records[0].1["herbertSources"]["files"][0],
            ordinary[0].1["herbertSources"]["files"][0]
        );
        assert!(
            records[0].1["herbertSources"]["files"][0]["text"]
                .as_str()
                .unwrap()
                .contains("owned-page-marker")
        );
        assert!(state["dataset"].is_null());
        assert!(state["records"].is_null());
        assert!(state["inner"].is_null());
        assert_eq!(state["_progress"]["pendingCompiledRead"], true);
        materialize.state = state;
        let GuestReply::Action {
            action: HostAction::RetainedLocalArtifactFunctionCall { .. },
            state,
        } = handle(materialize.clone())
        else {
            panic!()
        };
        materialize.state = state;
        materialize.error = Some(Diagnostic::new(
            "OPTIONAL_READER_REFUSED",
            "The optional reader is unavailable.",
        ));
        let GuestReply::Inventory {
            identities,
            authoritative,
            diagnostics,
            ..
        } = handle(materialize)
        else {
            panic!("Normal records must not be handed off twice")
        };
        assert!(identities.is_empty());
        assert!(authoritative);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].code, "OPTIONAL_READER_REFUSED");
        assert_eq!(diagnostics[1].code, "BPM_COMPILED_UNAVAILABLE");
        for diagnostic in diagnostics {
            assert_eq!(diagnostic.identity, Some(records[0].0.clone()));
            assert_eq!(diagnostic.context.get("processingIncomplete"), Some(&json!(true)));
        }
    }

    #[test]
    fn wave1_protected_dependencies_use_native_route_and_preserve_failed_owner() {
        for source in ["BO","DB"] {
            for body in ["<Workflow><BpmDataFormAction FormId='Prompt'/></Workflow>",
                "<Workflow><AttachHoldAction HoldType='Hold'/></Workflow>"] {
                let mut r=if source=="BO" {request()} else {data_request()};
                r.state["_hostPreparePage"]=json!(false);
                for row in r.response.as_mut().unwrap()["returnObj"]["BpDirective"].as_array_mut().unwrap() {
                    row["IsProtected"]=json!(true);row["Body"]=json!(body);
                }
                let GuestReply::Action {action:HostAction::SharedQuery{..},state}=handle(r.clone()) else {panic!("{source}")};
                r.state=state;r.response=Some(json!({"rows":[]}));
                let GuestReply::Action {action:HostAction::SharedNativeRead{method,..},state}=handle(r.clone()) else {panic!()};
                assert_eq!(method,if source=="BO" {"ExportByService"} else {"ExportByTable"});
                r.state=state;r.response=None;r.error=Some(Diagnostic::new("DENIED","Export refused."));
                let GuestReply::InventoryData {records,inventory}=handle(r) else {panic!()};
                assert_eq!(records.len(),1);assert_eq!(records[0].1["definition"]["BpDirective"][0]["Body"],body);
                let GuestReply::Inventory {authoritative,diagnostics,..}=*inventory else {panic!()};
                assert!(!authoritative);assert!(diagnostics.iter().any(|d|d.code=="DENIED"&&d.identity.as_ref()==Some(&records[0].0)));
            }
            let mut r=if source=="BO" {request()} else {data_request()};r.state["_hostPreparePage"]=json!(false);
            for row in r.response.as_mut().unwrap()["returnObj"]["BpDirective"].as_array_mut().unwrap(){row["IsProtected"]=json!(true);}
            let GuestReply::InventoryData {records,inventory}=handle(r) else {panic!()};
            assert!(records[0].1["herbertSources"]["binaryFiles"].is_array());
            assert!(matches!(*inventory,GuestReply::Inventory{authoritative:true,..}));
        }
    }


    #[test]
    fn wave1_residual_native_refusal_marks_only_failed_method_partial() {
        for source in ["BO", "DB"] {
            let mut r=if source=="BO"{request()}else{data_request()};
            r.state["_hostPreparePage"]=json!(false);r.page_size=Some(2);
            let ds=&mut r.response.as_mut().unwrap()["returnObj"];
            let mut peer=ds["BpMethod"][0].clone();let peer_code=if source=="BO"{"Ice.BO.Y.Update"}else{"Ice.UD40.Update"};
            peer["BpMethodCode"]=json!(peer_code);peer["BusinessObject"]=json!(if source=="BO"{"Y"}else{"UD40"});ds["BpMethod"].as_array_mut().unwrap().push(peer);
            let mut peer_directive=ds["BpDirective"][0].clone();peer_directive["BpMethodCode"]=json!(peer_code);
            for row in ds["BpDirective"].as_array_mut().unwrap(){row["IsProtected"]=json!(true);row["Body"]=json!("<Workflow><BpmDataFormAction FormId='Prompt'/></Workflow>");}
            ds["BpDirective"].as_array_mut().unwrap().push(peer_directive);
            let GuestReply::Action{state,..}=handle(r.clone())else{panic!()};r.state=state;r.response=Some(json!({"rows":[]}));
            let GuestReply::Action{state,action:HostAction::SharedNativeRead{method,..}}=handle(r.clone())else{panic!()};
            assert_eq!(method,if source=="BO"{"ExportByService"}else{"ExportByTable"});r.state=state;r.response=None;
            let mut error=Diagnostic::new("DENIED","Export refused.");error.context.insert("witness".into(),json!("exact native refusal"));r.error=Some(error);
            let GuestReply::InventoryData{records,inventory}=handle(r)else{panic!()};assert_eq!(records.len(),2);
            let failed=records.iter().find(|(id,_)|id.keys["BpMethodCode"]!=peer_code).unwrap();
            let peer=records.iter().find(|(id,_)|id.keys["BpMethodCode"]==peer_code).unwrap();
            let GuestReply::Inventory{diagnostics,authoritative,..}=*inventory else{panic!()};assert!(!authoritative);
            assert_eq!(failed.1["herbertCollectionStatus"]["completeness"],"partial");
            let owner:Vec<_>=diagnostics.iter().filter(|d|d.identity.as_ref()==Some(&failed.0)).collect();
            assert_eq!(failed.1["herbertCollectionStatus"]["diagnostics"],json!(owner));
            assert!(owner.iter().any(|d|d.code=="DENIED"&&d.context["witness"]=="exact native refusal"));
            assert!(peer.1.get("herbertCollectionStatus").is_none());assert!(peer.1["herbertSources"]["binaryFiles"].is_array());
            assert!(failed.1.pointer("/herbertSources/nativeFiles").is_none());
        }
    }

}
