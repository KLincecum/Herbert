//! Page persisted identities, read native child tables, and write native DBD files.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const SERVICE: &str = "Ice.BO.DashBoardSvc";
const KEYS: [&str; 5] = [
    "Company",
    "ProductID",
    "GlbCompany",
    "CGCCode",
    "DefinitionID",
];
const TABLES: [&str; 4] = ["DashBdDef", "DashBdBAQ", "DashBdChunk", "DashBdLike"];
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("DASHBOARD_PAGE_INVALID", message)
}
fn prefix(request: &GuestRequest) -> Result<String, Diagnostic> {
    match request.state.pointer("/_pluginSettings/dashboardIdPrefix") {
        None => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(Diagnostic::new("INVALID_SETTING", "Dashboard ID starts with must be a string.")),
    }
}
fn with_settings(mut state: Value, from: &Value) -> Value {
    if let Some(settings) = from.get("_pluginSettings") {state["_pluginSettings"] = settings.clone();}
    state
}
pub fn methods() -> Vec<(String, String)> {
    vec![
        (SERVICE.into(), "GetRows".into()),
        (SERVICE.into(), "GetByID".into()),
        ("Ice.BO.BAQDesignerSvc".into(), "ExportBaq".into()),
    ]
}
pub fn handle(request: GuestRequest) -> GuestReply {
    collect(request).unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic })
}
fn identity(row: &Value) -> Result<Identity, Diagnostic> {
    let mut keys = BTreeMap::<String, String>::new();
    for key in KEYS {
        keys.insert(
            key.into(),
            row[key]
                .as_str()
                .ok_or_else(|| invalid("A dashboard identity is incomplete."))?
                .into(),
        );
    }
    Ok(Identity {
        category: "classic-dashboards".into(),
        kind: "PersistedDashBdDef".into(),
        scope: if keys["Company"].is_empty() {
            Scope::Global
        } else {
            Scope::Company {
                company: keys["Company"].clone(),
            }
        },
        keys,
    })
}
fn action(state: Value, method: &str, arguments: Value) -> GuestReply {
    GuestReply::Action {
        action: HostAction::NativeRead {
            service: SERVICE.into(),
            method: method.into(),
            arguments,
            export: false,
        },
        state,
    }
}
fn collect(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if !matches!(r.operation.as_str(), "discover" | "acquire" | "materialize") {
        return Err(invalid(
            "Select dashboard discovery or an exact dashboard identity.",
        ));
    }
    let prefix = if r.operation == "acquire" {String::new()} else {prefix(&r)?};
    if r.operation == "materialize" {
        return process(r);
    }
    let phase = r.state["phase"].as_str().unwrap_or("initial").to_owned();
    if let Some(mut error) = r.error.take() {
        if matches!(phase.as_str(), "baq" | "binary") {
            let cause = format!("{}: {}", error.code, error.message);
            error.context.insert("query".into(), json!(r.state["query"].as_str().unwrap_or("")));
            error.context.insert("cause".into(), json!(cause));
            error.message = "A BAQ used by this dashboard could not be included in the dashboard file. The dashboard was still saved; download the BAQ separately or make sure it exists in the target system before importing.".into();
            r.state["errors"].as_array_mut().unwrap().push(json!(error));
            return next_baq(r);
        }
        return Err(error);
    }
    match phase.as_str() {
        "initial" | "nextPage" => {
            let cursor: Value = r
                .cursor
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| invalid("The dashboard saved position is invalid."))?
                .unwrap_or(json!({"version":1,"branch":0}));
            if cursor["version"] != 1 {
                return Err(invalid(
                    "Restart dashboard discovery with the current saved position format.",
                ));
            }
            let branch = cursor["branch"]
                .as_u64()
                .ok_or_else(|| invalid("The dashboard page branch is missing."))?;
            if branch > 1 {
                return Err(invalid("The dashboard page scope is invalid."));
            }
            let company = if branch == 0 {
                r.company.clone()
            } else {
                String::new()
            };
            if let Some(id) = &r.identity {
                if id.category != "classic-dashboards"
                    || id.kind != "PersistedDashBdDef"
                    || id.keys.len() != 5
                    || KEYS.iter().any(|k| !id.keys.contains_key(*k))
                {
                    return Err(invalid("Select an exact dashboard identity."));
                }
                if id.keys["Company"] != r.company && !id.keys["Company"].is_empty() {
                    return Err(invalid(
                        "The dashboard is outside the selected company scope.",
                    ));
                }
            }
            r.page_size = Some(r.page_size.unwrap_or(16));
            if r.page_size == Some(0) {
                return Err(invalid("Dashboard page size must be positive."));
            }
            Ok(crate::tables::dashboard_page(
                &r,
                &company,
                cursor["after"].clone(),
                with_settings(json!({"phase":"inventory","branch":branch,"company":company,"after":cursor["after"],"_progress":{"request":"Finding dashboard variants","response":"Dashboard identities received; reading complete native definitions"}}), &r.state),
                &prefix,
            ))
        }
        "inventory" => {
            let rows = r
                .response
                .as_ref()
                .and_then(|v| v["rows"].as_array())
                .ok_or_else(|| invalid("Dashboard inventory rows are missing."))?;
            let size = r.page_size.unwrap_or(16) as usize;
            if rows.len() > size {
                return Err(invalid(
                    "Dashboard inventory exceeded its requested page size.",
                ));
            }
            let mut ids = Vec::new();
            let mut seen = BTreeSet::new();
            for row in rows {
                let id = identity(row)?;
                let company = r
                    .identity
                    .as_ref()
                    .map(|id| id.keys["Company"].as_str())
                    .unwrap_or(r.state["company"].as_str().unwrap_or(""));
                if id.keys["Company"] != company
                    || row["SystemFlag"] != false
                    || !seen.insert(serde_json::to_string(&id).unwrap())
                {
                    return Err(invalid(
                        "Dashboard inventory contains a duplicate or an out-of-scope row.",
                    ));
                }
                ids.push(id);
            }
            if r.operation == "acquire" && (ids.len() != 1 || ids.first() != r.identity.as_ref()) {
                return Err(invalid("The exact dashboard was not returned."));
            }
            let next = if r.operation == "acquire" {
                None
            } else if rows.len() == size {
                let after = rows
                    .last()
                    .and_then(|v| v["SysRowID"].as_str())
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| invalid("The dashboard page has no saved position."))?;
                if r.state["after"] == after {
                    return Err(invalid("The dashboard saved position did not advance."));
                }
                Some(json!({"version":1,"branch":r.state["branch"],"after":after}).to_string())
            } else if r.state["branch"] == 0 {
                Some(json!({"version":1,"branch":1}).to_string())
            } else {
                None
            };
            if r.operation != "acquire" {ids.retain(|id| id.keys["DefinitionID"].starts_with(&prefix));}
            r.state["ids"] = json!(ids);
            r.state["nextCursor"] = json!(next);
            r.state["data"] =
                json!({"DashBdDef":[],"DashBdBAQ":[],"DashBdChunk":[],"DashBdLike":[]});
            r.state["errors"] = json!([]);
            r.state["packages"] = json!({});
            if ids.is_empty() {
                return prepare(r);
            }
            let clause = ids
                .iter()
                .map(|id| {
                    format!(
                        "({})",
                        KEYS.iter()
                            .map(|k| format!("{k} = '{}'", id.keys[*k].replace('\'', "''")))
                            .collect::<Vec<_>>()
                            .join(" AND ")
                    )
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            r.state["phase"] = json!("native");
            r.state["_progress"] = json!({"request":format!("Downloading {} complete dashboards",ids.len()),"response":"Dashboard batch received; preparing native files"});
            Ok(action(
                r.state,
                "GetRows",
                json!({"whereClauseDashBdDef":clause,"whereClauseDashBdBAQ":"","whereClauseDashBdChunk":"","whereClauseDashBdLike":"","pageSize":ids.len(),"absolutePage":1}),
            ))
        }
        "native" | "exact" => {
            let response = r
                .response
                .as_ref()
                .ok_or_else(|| invalid("The Epicor dashboard response is missing."))?;
            if phase == "native" && response["parameters"]["morePages"] != false {
                return Err(invalid(
                    "The Epicor dashboard response did not establish a complete requested page.",
                ));
            }
            let data = &response["returnObj"];
            let ids: Vec<Identity> = serde_json::from_value(r.state["ids"].clone())
                .map_err(|_| invalid("Dashboard identities are invalid."))?;
            let mut roots = BTreeSet::new();
            for table in TABLES {
                let rows = data[table]
                    .as_array()
                    .ok_or_else(|| invalid("An Epicor dashboard table is missing."))?;
                for row in rows {
                    let id = identity(row)?;
                    if !ids.contains(&id) || (phase == "exact" && json!(id) != r.state["exactId"]) {
                        return Err(invalid(
                            "The Epicor dashboard response contains an unrequested identity.",
                        ));
                    }
                    if table == "DashBdDef"
                        && (row["SystemFlag"] != false
                            || !roots.insert(serde_json::to_string(&id).unwrap()))
                    {
                        return Err(invalid(
                            "An Epicor dashboard root is duplicated or is a system definition.",
                        ));
                    }
                    let array = r.state["data"]
                        .as_object_mut()
                        .unwrap()
                        .entry(table)
                        .or_insert(json!([]));
                    array.as_array_mut().unwrap().push(row.clone());
                }
            }
            if phase == "exact" && roots.len() != 1 {
                return Err(invalid(
                    "The exact dashboard fallback did not return one root.",
                ));
            }
            for id in &ids {
                let found = r.state["data"]["DashBdDef"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| identity(row).as_ref() == Ok(id));
                if !found {
                    r.state["phase"] = json!("exact");
                    r.state["exactId"] = json!(id);
                    r.state["_progress"] = json!({"request":format!("Reading dashboard variant {} ({})",id.keys["DefinitionID"],id.keys["CGCCode"]),"response":"Dashboard variant received"});
                    return Ok(action(
                        r.state,
                        "GetByID",
                        json!({"company":id.keys["Company"],"productID":id.keys["ProductID"],"glbCompany":id.keys["GlbCompany"],"cgCCode":id.keys["CGCCode"],"definitionID":id.keys["DefinitionID"]}),
                    ));
                }
            }
            let mut queries = BTreeSet::new();
            let mut schemas = Vec::new();
            for id in &ids {
                match schema(&r.state["data"], id).and_then(|xml| {
                    let referenced = crate::dashboard_archive::queries(&xml)?;
                    Ok((xml, referenced))
                }) {
                    Ok((xml, referenced)) => {
                        queries.extend(referenced);
                        schemas.push(json!(xml));
                    }
                    Err(mut error) => {
                        error.identity = Some(id.clone());
                        r.state["errors"].as_array_mut().unwrap().push(json!(error));
                        schemas.push(Value::Null);
                    }
                }
            }
            r.state["schemas"] = json!(schemas);
            if r.state
                .pointer("/_pluginSettings/includeReferencedBAQs")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                r.state["queries"] = json!(queries);
            } else {
                r.state["queries"] = json!([]);
            }
            next_baq(r)
        }
        "baq" => {
            let file: FileRef = serde_json::from_value(
                r.response
                    .as_ref()
                    .and_then(|v| v.pointer("/returnObj/nativeFile"))
                    .cloned()
                    .ok_or_else(|| invalid("The BAQ export has no Epicor file."))?,
            )
            .map_err(|_| invalid("The BAQ file reference is invalid."))?;
            r.state["phase"] = json!("binary");
            Ok(GuestReply::Action {
                action: HostAction::ReadBinary { file },
                state: r.state,
            })
        }
        "binary" => {
            let bytes = r
                .response
                .as_ref()
                .and_then(|v| v["base64"].as_str())
                .ok_or_else(|| invalid("The verified BAQ bytes are missing."))?;
            let query = r.state["query"].as_str().unwrap().to_owned();
            r.state["packages"][query] = json!(bytes);
            next_baq(r)
        }
        _ => Err(invalid("The dashboard saved progress is invalid.")),
    }
}
fn schema(data: &Value, id: &Identity) -> Result<String, Diagnostic> {
    let root = data["DashBdDef"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| identity(row).as_ref() == Ok(id)))
        .ok_or_else(|| invalid("The dashboard root is missing."))?;
    let xml = root["DashboardSchema"]
        .as_str()
        .ok_or_else(|| invalid("The dashboard schema field is missing."))?;
    if !xml.is_empty() {
        return Ok(xml.into());
    }
    let mut chunks = data["DashBdChunk"]
        .as_array()
        .ok_or_else(|| invalid("Dashboard chunks are missing."))?
        .iter()
        .filter(|row| identity(row).as_ref() == Ok(id))
        .collect::<Vec<_>>();
    chunks.sort_by_key(|row| row["SeqNum"].as_i64());
    let mut xml = String::new();
    let mut seen = BTreeSet::new();
    for row in chunks {
        let seq = row["SeqNum"]
            .as_i64()
            .ok_or_else(|| invalid("A dashboard chunk has no sequence."))?;
        if !seen.insert(seq) {
            return Err(invalid("Dashboard chunk sequences are duplicated."));
        }
        xml.push_str(
            row["Chunk"]
                .as_str()
                .ok_or_else(|| invalid("A dashboard chunk has no text."))?,
        );
    }
    if xml.is_empty() {
        return Err(invalid("The dashboard has no schema or chunks."));
    }
    Ok(xml.replace('\n', "\r\n").replace("\r\r\n", "\r\n"))
}
fn next_baq(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let queries = r.state["queries"]
        .as_array_mut()
        .ok_or_else(|| invalid("The dashboard BAQ list is missing."))?;
    if queries.is_empty() {
        return prepare(r);
    }
    let query = queries.remove(0);
    r.state["phase"] = json!("baq");
    r.state["query"] = query.clone();
    r.state["_progress"] = json!({"request":format!("Embedding dashboard BAQ {}",query.as_str().unwrap()),"response":"Native BAQ received; assembling dashboard package"});
    Ok(GuestReply::Action {
        action: HostAction::SharedNativeRead {
            service: "Ice.BO.BAQDesignerSvc".into(),
            method: "ExportBaq".into(),
            arguments: json!({"queryId":query,"options":{}}),
            export: true,
        },
        state: r.state,
    })
}
fn prepare(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    r.state["targeted"] = json!(r.operation == "acquire");
    if r.state["_hostPreparePage"] == true && r.operation != "acquire" {
        return Ok(GuestReply::PreparedPage {
            next_cursor: r.state["nextCursor"].as_str().map(str::to_owned),
            next_state: with_settings(json!({"phase":"nextPage"}), &r.state),
            state: r.state,
        });
    }
    process(r)
}
fn process(r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let ids: Vec<Identity> = serde_json::from_value(r.state["ids"].clone())
        .map_err(|_| invalid("Dashboard identities are missing."))?;
    let packages: BTreeMap<String, String> = serde_json::from_value(r.state["packages"].clone())
        .map_err(|_| invalid("Dashboard packages are invalid."))?;
    let mut diagnostics: Vec<Diagnostic> = serde_json::from_value(r.state["errors"].clone())
        .map_err(|_| invalid("Dashboard diagnostics are invalid."))?;
    let mut records = Vec::new();
    for (index, id) in ids.iter().enumerate() {
        let mut definition = json!({"herbertDirectOutput":true,"herbertSources":{"files":[]}});
        for table in TABLES {
            definition[table] = json!(
                r.state["data"][table]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|row| identity(row).as_ref() == Ok(id))
                    .collect::<Vec<_>>()
            );
        }
        // The native client stores a ZIP as base64, separate from the DBD.
        if let Some(encoded) = definition["DashBdDef"][0]["DashboardAssembly"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            use base64::Engine;
            use sha2::{Digest, Sha256};
            match base64::engine::general_purpose::STANDARD.decode(encoded) {
                Ok(bytes) => {
                    definition["herbertSources"]["binaryFiles"] = json!([{
                        "name": crate::bpm_code::label(&format!("{}.assembly.zip", id.keys["DefinitionID"])),
                        "bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(&bytes)),
                        "base64": encoded,
                    }])
                }
                Err(_) => {
                    let mut error = invalid(
                        "The deployed dashboard assembly is not valid base64. Its original value is retained.",
                    );
                    error.identity = Some(id.clone());
                    diagnostics.push(error);
                }
            }
        }
        let Some(xml) = r.state["schemas"][index].as_str() else {
            // Keep the source and continue other dashboards after a bad schema.
            records.push((id.clone(), definition));
            continue;
        };
        let wanted = crate::dashboard_archive::queries(xml)?;
        let selected = packages
            .iter()
            .filter(|(id, _)| wanted.contains(*id))
            .map(|(id, bytes)| (id.clone(), bytes.clone()))
            .collect();
        match crate::dashboard_archive::build(xml,&selected) {
            Ok(text)=>definition["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":crate::bpm_code::label(&format!("{}.dbd",id.keys["DefinitionID"])),"text":text})),
            Err(mut error)=>{error.identity=Some(id.clone());diagnostics.push(error);}
        }
        records.push((id.clone(), definition));
    }
    if r.state["targeted"] == true {
        let (identity, definition) = records
            .pop()
            .ok_or_else(|| invalid("The selected dashboard is missing."))?;
        return Ok(GuestReply::Definition {
            identity,
            definition,
            format: "native-classic-dashboard".into(),
            completeness: if diagnostics.is_empty() {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
        });
    }
    Ok(GuestReply::InventoryData {
        records,
        inventory: Box::new(GuestReply::Inventory {
            identities: ids,
            next_cursor: r.state["nextCursor"].as_str().map(str::to_owned),
            authoritative: diagnostics.is_empty(),
            diagnostics,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "classic-dashboards".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(16),
            state: json!({}),
            response: None,
            error: None,
        }
    }
    fn row(country: &str) -> Value {
        json!({"Company":"C","ProductID":"EP","GlbCompany":"","CGCCode":country,"DefinitionID":"D","SystemFlag":false,"SysRowID":format!("row-{country}"),"DashboardSchema":"<DashboardDataSet xmlns=\"http://tempuri.org/DashboardDataSet.xsd\"><DashboardDefinition><DefinitionID>D</DefinitionID></DashboardDefinition></DashboardDataSet>"})
    }
    fn prefix_roots() -> Vec<Value> {
        [("Z","US"),("Z","ZZ"),("Other","US")].into_iter().map(|(name,country)| {
            let mut root = row(country);root["DefinitionID"] = json!(name);root["SysRowID"] = json!(format!("{name}-{country}"));
            let queries = if name == "Other" {vec!["QPeer".to_owned()]} else {vec!["QShared".to_owned(),format!("Q{country}")]};
            root["DashboardSchema"] = json!(format!("<DashboardDataSet xmlns=\"http://tempuri.org/DashboardDataSet.xsd\"><DashboardDefinition><DefinitionID>{name}</DefinitionID></DashboardDefinition>{}</DashboardDataSet>",queries.iter().map(|q|format!("<DashboardQuery><DynamicQueryID>{q}</DynamicQueryID></DashboardQuery>")).collect::<String>()));
            root["DashboardAssembly"] = json!("UEsDBA==");root["SysRevID"] = json!(9007199254740993u64);root
        }).collect()
    }
    fn prefix_inventory(prefix: &str, include: bool) -> GuestRequest {
        let mut r = request();r.page_size = Some(3);
        r.state = json!({"phase":"inventory","branch":0,"company":"C","_hostPreparePage":true,
            "_pluginSettings":{"dashboardIdPrefix":prefix,"includeReferencedBAQs":include}});
        r.response = Some(json!({"rows":prefix_roots()}));r
    }
    fn complete_graph(roots: Vec<Value>) -> Value {
        let mut data = json!({"DashBdDef":roots,"DashBdBAQ":[],"DashBdChunk":[],"DashBdLike":[]});
        let roots = data["DashBdDef"].as_array().unwrap().clone();
        for table in ["DashBdBAQ","DashBdChunk","DashBdLike"] {
            for root in &roots {
                let mut row = root.clone();row["SeqNum"] = json!(1);row["Chunk"] = json!("Original Ω");row["NativeValue"] = json!(format!("{table} retained"));
                data[table].as_array_mut().unwrap().push(row);
            }
        }
        data
    }
    fn finish_prefix(mut r: GuestRequest, omit_variant: bool, fail_query: Option<&str>) -> (Vec<(Identity,Value)>,Vec<Diagnostic>,Vec<Value>) {
        use base64::Engine;
        use sha2::{Digest,Sha256};
        let mut calls = Vec::new();let mut reply = handle(r.clone());
        loop {
            match reply {
                GuestReply::Action {action,state} => {
                    calls.push(json!(action));r.state = state;r.error = None;
                    match action {
                        HostAction::NativeRead {method,arguments,..} if method == "GetRows" => {
                            let ids: Vec<Identity> = serde_json::from_value(r.state["ids"].clone()).unwrap();
                            let roots = prefix_roots().into_iter().filter(|row| ids.contains(&identity(row).unwrap()) && (!omit_variant || row["CGCCode"] != "ZZ")).collect();
                            r.response = Some(json!({"returnObj":complete_graph(roots),"parameters":{"morePages":false}}));
                            let clause = arguments["whereClauseDashBdDef"].as_str().unwrap();
                            if r.state.pointer("/_pluginSettings/dashboardIdPrefix").and_then(Value::as_str) == Some("Z") {assert!(!clause.contains("Other"));}
                        }
                        HostAction::NativeRead {method,arguments,..} if method == "GetByID" => {
                            assert_eq!(arguments["cgCCode"],"ZZ");assert_eq!(arguments["definitionID"],"Z");
                            r.response = Some(json!({"returnObj":complete_graph(vec![prefix_roots()[1].clone()])}));
                        }
                        HostAction::SharedNativeRead {method,arguments,..} if method == "ExportBaq" => {
                            let query = arguments["queryId"].as_str().unwrap();
                            if fail_query == Some(query) {r.error = Some(Diagnostic::new("NATIVE_REFUSED","Selected BAQ unavailable"));r.response = None;}
                            else {
                                let bytes = format!("original-native-{query}").into_bytes();
                                r.response = Some(json!({"returnObj":{"nativeFile":{"path":format!("raw/{query}.baq"),"sha256":format!("{:x}",Sha256::digest(&bytes)),"bytes":bytes.len(),"role":"native"}}}));
                            }
                        }
                        HostAction::ReadBinary {..} => {
                            let query = r.state["query"].as_str().unwrap();
                            r.response = Some(json!({"base64":base64::engine::general_purpose::STANDARD.encode(format!("original-native-{query}").as_bytes())}));
                        }
                        other => panic!("Unexpected {other:?}"),
                    }
                    reply = handle(r.clone());
                }
                GuestReply::PreparedPage {state,next_state,..} => {
                    assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);
                    r.state = state;r.operation = "materialize".into();r.response = None;reply = handle(r.clone());
                }
                GuestReply::InventoryData {records,inventory} => {
                    let GuestReply::Inventory {diagnostics,..} = *inventory else {panic!()};return (records,diagnostics,calls);
                }
                other => panic!("Unexpected {other:?}"),
            }
        }
    }
    #[test]
    fn prefix_retains_selected_composite_graphs_fallback_and_exact_native_baq_bytes() {
        let (baseline,diagnostics,_) = finish_prefix(prefix_inventory("",true),false,None);assert!(diagnostics.is_empty());
        let (selected,diagnostics,calls) = finish_prefix(prefix_inventory("Z",true),true,None);assert!(diagnostics.is_empty());assert_eq!(selected.len(),2);
        assert_eq!(calls.iter().filter(|c|c["method"] == "GetByID").count(),1);
        let queries: BTreeSet<_> = calls.iter().filter(|c|c["method"] == "ExportBaq").map(|c|c["arguments"]["queryId"].as_str().unwrap()).collect();
        assert_eq!(queries,BTreeSet::from(["QShared","QUS","QZZ"]));
        for record in &selected {assert_eq!(record,baseline.iter().find(|(id,_)| id == &record.0).unwrap());}
        assert_ne!(selected[0].0,selected[1].0);
        let (definitions,diagnostics,calls) = finish_prefix(prefix_inventory("Z",false),false,None);
        assert_eq!(definitions.len(),2);assert!(diagnostics.is_empty());assert!(!calls.iter().any(|c|c["method"] == "ExportBaq"));
        for (_,definition) in definitions {assert_eq!(definition["DashBdChunk"].as_array().unwrap().len(),1);assert_eq!(definition["herbertSources"]["binaryFiles"][0]["base64"],"UEsDBA==");}
    }
    #[test]
    fn prefix_validates_all_inventory_and_continues_empty_selection_without_definition_reads() {
        let mut r = prefix_inventory("Z",true);
        for row in r.response.as_mut().unwrap()["rows"].as_array_mut().unwrap() {if row["DefinitionID"] == "Z" {row["DefinitionID"] = json!("Early");}}
        let GuestReply::PreparedPage {state,next_state,next_cursor} = handle(r.clone()) else {panic!()};
        assert_eq!(state["ids"],json!([]));assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);
        assert_eq!(serde_json::from_str::<Value>(next_cursor.as_ref().unwrap()).unwrap()["after"],"Other-US");
        r.state = next_state;r.cursor = next_cursor;r.response = None;
        let GuestReply::Action {state,..} = handle(r.clone()) else {panic!()};assert_eq!(state["_pluginSettings"],r.state["_pluginSettings"]);
        r.state = state;r.response = Some(json!({"rows":[prefix_roots()[0].clone()]}));
        let GuestReply::Action {action:HostAction::NativeRead {method,..},..} = handle(r.clone()) else {panic!()};assert_eq!(method,"GetRows");
        for field in ["Company","SystemFlag","GlbCompany"] {
            let mut r = prefix_inventory("Z",true);r.response.as_mut().unwrap()["rows"][2][field] = if field == "SystemFlag" {json!(true)} else {Value::Null};
            assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "DASHBOARD_PAGE_INVALID"));
        }
        for invalid in [Value::Null,json!(true),json!(5),json!([])] {
            let mut r = prefix_inventory("",true);r.state["_pluginSettings"]["dashboardIdPrefix"] = invalid;
            assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));
        }
    }
    #[test]
    fn prefix_preserves_selected_dependency_failure_and_exact_acquisition_independence() {
        let (records,diagnostics,calls) = finish_prefix(prefix_inventory("Z",true),false,Some("QShared"));
        assert_eq!(records.len(),2);assert!(diagnostics.iter().any(|d|d.code == "NATIVE_REFUSED"));
        assert!(!calls.iter().any(|c|c["arguments"]["queryId"] == "QPeer"));
        let root = prefix_roots()[0].clone();let mut r = prefix_inventory("Missing",false);
        r.identity = Some(identity(&root).unwrap());r.operation = "acquire".into();r.response = Some(json!({"rows":[root]}));
        let GuestReply::Action {action:HostAction::NativeRead {method,arguments,..},..} = handle(r.clone()) else {panic!()};
        assert_eq!(method,"GetRows");assert!(arguments["whereClauseDashBdDef"].as_str().unwrap().contains("DefinitionID = 'Z'"));
        let baseline = serde_json::to_value(handle(r.clone())).unwrap()["action"].clone();
        r.state["_pluginSettings"]["dashboardIdPrefix"] = json!("Other");assert_eq!(serde_json::to_value(handle(r)).unwrap()["action"],baseline);
    }
    #[test]
    fn omitted_country_variant_uses_exact_key_then_prepares_local_work() {
        let mut r = request();
        r.state = json!({"phase":"inventory","branch":0,"company":"C","_hostPreparePage":true});
        r.response = Some(json!({"rows":[row(""),row("ZZ")]}));
        let GuestReply::Action {
            action: HostAction::NativeRead {
                method, arguments, ..
            },
            mut state,
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(method, "GetRows");
        assert_eq!(arguments["pageSize"], 2);
        state["_hostPreparePage"] = json!(true);
        r.state = state;
        r.response = Some(
            json!({"returnObj":{"DashBdDef":[row("")],"DashBdBAQ":[],"DashBdChunk":[],"DashBdLike":[]},"parameters":{"morePages":false}}),
        );
        let GuestReply::Action {
            action: HostAction::NativeRead {
                method, arguments, ..
            },
            state,
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(method, "GetByID");
        assert_eq!(arguments["cgCCode"], "ZZ");
        r.state = state;
        r.response = Some(
            json!({"returnObj":{"DashBdDef":[row("ZZ")],"DashBdBAQ":[],"DashBdChunk":[],"DashBdLike":[]}}),
        );
        let GuestReply::PreparedPage { state, .. } = handle(r.clone()) else {
            panic!()
        };
        r.operation = "materialize".into();
        r.state = state;
        r.response = None;
        let GuestReply::InventoryData { records, .. } = handle(r) else {
            panic!()
        };
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].0, records[1].0);
        assert!(
            records
                .iter()
                .all(|(_, d)| d["herbertSources"]["files"][0]["name"] == "D.dbd")
        );
    }
    #[test]
    fn empty_inventory_completes_and_bad_scope_is_rejected() {
        let mut r = request();
        r.state = json!({"phase":"inventory","branch":1,"company":""});
        r.response = Some(json!({"rows":[]}));
        assert!(matches!(
            handle(r.clone()),
            GuestReply::InventoryData { .. }
        ));
        r.response = Some(json!({"rows":[row("")]}));
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
    #[test]
    fn chunks_use_native_sequence_order_and_reject_duplicates() {
        let mut root = row("");
        root["DashboardSchema"] = json!("");
        let id = identity(&root).unwrap();
        let mut a = root.clone();
        a["SeqNum"] = json!(1);
        a["Chunk"] = json!("<DashboardDataSet>");
        let mut b = root.clone();
        b["SeqNum"] = json!(2);
        b["Chunk"] = json!("</DashboardDataSet>");
        let mut data = json!({"DashBdDef":[root],"DashBdChunk":[b,a]});
        assert_eq!(
            schema(&data, &id).unwrap(),
            "<DashboardDataSet></DashboardDataSet>"
        );
        data["DashBdChunk"][0]["SeqNum"] = json!(1);
        assert!(schema(&data, &id).is_err());
    }
    #[test]
    fn failed_baq_keeps_the_dashboard_and_reports_the_dependency() {
        let mut r = request();
        let root = row("");
        r.state = json!({
            "phase":"baq", "query":"MissingQuery", "queries":[], "packages":{},
            "errors":[], "ids":[identity(&root).unwrap()], "schemas":[root["DashboardSchema"]],
            "data":{"DashBdDef":[root],"DashBdBAQ":[],"DashBdChunk":[],"DashBdLike":[]}
        });
        r.error = Some(Diagnostic::new(
            "NATIVE_REFUSED",
            "The query export failed.",
        ));
        let GuestReply::InventoryData { records, inventory } = handle(r) else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].1["herbertSources"]["files"][0]["name"], "D.dbd");
        let GuestReply::Inventory {
            authoritative,
            diagnostics,
            ..
        } = *inventory
        else {
            panic!()
        };
        assert!(!authoritative);
        assert_eq!(diagnostics[0].context["query"], "MissingQuery");
        assert_eq!(diagnostics[0].context["cause"], "NATIVE_REFUSED: The query export failed.");
        assert!(diagnostics[0].message.starts_with("A BAQ used by this dashboard"));
    }
}
