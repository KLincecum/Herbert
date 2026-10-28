//! C22 bulk metadata, locally grouped source, and exact native assembly/symbol bytes.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn err(s: &str) -> Diagnostic {
    Diagnostic::new("PROGRAMS_INVALID", s)
}
fn selection(state:&Value)->Result<(&str,&str),Diagnostic>{
    let options=&state["_pluginSettings"];let family=match options.get("definitionFamily"){None=>"all",Some(Value::String(value)) if matches!(value.as_str(),"all"|"services"|"datasets"|"extensions"|"externals")=>value,_=>return Err(err("Select a declared program definition family."))};
    let prefix=match options.get("definitionIdPrefix"){None=>"",Some(Value::String(value))=>value,_=>return Err(err("The program definition prefix must be a string."))};Ok((family,prefix))
}
fn selected(identity:&Identity,state:&Value)->Result<bool,Diagnostic>{if state["_target"].is_object(){return Ok(true);}let (family,prefix)=selection(state)?;let (group,key)=match identity.kind.as_str(){"ServiceDefinition"=>("services","ClassName"),"DatasetDefinition"=>("datasets","DataSetID"),"ExtensionSet"=>("extensions","ExtensionSetID"),"ExternalAssemblyReference"=>("externals","Name"),_=>return Err(err("Unknown program owner."))};Ok((family=="all"||family==group)&&identity.keys.get(key).is_some_and(|value|value.starts_with(prefix)))}
fn select_records(records:Vec<(Identity,Value)>,state:&Value)->Result<(Vec<(Identity,Value)>,Vec<Diagnostic>),Diagnostic>{
    let mut output=Vec::new();let mut diagnostics=Vec::new();for (identity,data) in records {if selected(&identity,state)?{output.push((identity,data));}else{let errors=data["herbertCollectionStatus"]["diagnostics"].as_array().cloned().unwrap_or_default();let mut errors=errors;if let Some(error)=data.get("hiddenMetadataError").filter(|value|!value.is_null()){errors.push(error.clone());}for error in errors{let mut diagnostic:Diagnostic=serde_json::from_value(error).map_err(|_|err("Invalid acquired program diagnostic."))?;diagnostic.identity=Some(identity.clone());diagnostics.push(diagnostic);}}}Ok((output,diagnostics))
}
fn settings(mut state:Value,from:&Value)->Value{if let Some(value)=from.get("_pluginSettings"){if !state.is_object(){state=json!({});}state["_pluginSettings"]=value.clone();}state}
fn carry(reply:GuestReply,from:&Value)->GuestReply{match reply{GuestReply::Action {action,state}=>GuestReply::Action {action,state:settings(state,from)},GuestReply::ReadyRecords {records,state}=>GuestReply::ReadyRecords {records,state:settings(state,from)},GuestReply::PreparedPage {mut state,next_cursor,next_state}=>{if let Some(items)=state.get_mut("_hostWorkItems").and_then(Value::as_array_mut){for item in items{*item=settings(item.take(),from);}}GuestReply::PreparedPage {state:settings(state,from),next_cursor,next_state:settings(next_state,from)}},other=>other}}
pub fn methods() -> Vec<(String, String)> {
    [
        ("Ice.BO.ZBODefSvc", "GetRows"),
        ("Ice.BO.ZDataSetSvc", "GetRows"),
        ("Ice.BO.ExtensionSvc", "GetList"),
        ("Ice.BO.ExtensionSvc", "GetExtensionSetAssembly"),
        ("Ice.BO.ZDataTableSvc", "GetRows"),
        ("Ice.BO.DynamicQuerySvc", "Execute"),
        ("Ice.BO.BpMethodSvc", "GetAvailableReferences"),
        ("Ice.Lib.EcfToolsSvc", "GetAssemblyBytes"),
    ]
    .into_iter()
    .map(|(s, m)| (s.into(), m.into()))
    .collect()
}
pub fn policies() -> Vec<NativeResponsePolicy> {
    let mut p = crate::response_policy::for_methods(&methods());
    for v in &mut p {
        v.retention_rules.clear();
        if let Some(t) = &mut v.tables {
            t.unscoped_tables.push("Results".into());
        }
    }
    p
}
fn id(kind: &str, row: &Value, keys: &[&str]) -> Result<Identity, Diagnostic> {
    Ok(Identity {
        category: "C22".into(),
        kind: kind.into(),
        scope: Scope::Global,
        keys: keys
            .iter()
            .map(|k| {
                Ok((
                    (*k).into(),
                    row[*k]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| err("Missing native program key."))?
                        .into(),
                ))
            })
            .collect::<Result<_, Diagnostic>>()?,
    })
}
fn eq(a: &Value, b: &Value, k: &str) -> bool {
    match (a[k].as_str(), b[k].as_str()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}
type RowIndex<'a> = BTreeMap<Vec<String>,Vec<(usize,&'a Value)>>;
fn row_index<'a>(rows:&'a [Value],fields:&[&str],fold:bool)->RowIndex<'a>{
    let mut index:RowIndex<'a>=BTreeMap::new();
    for (position,row) in rows.iter().enumerate(){
        let key:Option<Vec<_>>=fields.iter().map(|field|row[*field].as_str().map(|value|if fold{value.to_ascii_lowercase()}else{value.to_owned()})).collect();
        if let Some(key)=key{index.entry(key).or_default().push((position,row));}
    }
    index
}
fn indexed_rows<'a>(index:&RowIndex<'a>,keys:impl IntoIterator<Item=Vec<String>>)->Vec<&'a Value>{
    // Selecting several referenced keys must preserve original row order and
    // must not repeat a row when multiple references name the same key.
    let mut selected=BTreeMap::new();
    for key in keys{if let Some(rows)=index.get(&key){selected.extend(rows.iter().copied());}}
    selected.into_values().collect()
}
fn complete(mut data: Value, files: Vec<Value>, diagnostics: Vec<Diagnostic>) -> Value {
    let text = serde_json::to_string_pretty(&data).unwrap();
    data["herbertSources"] =
        json!({"files":[{"name":"program-definition.json","text":text}],"binaryFiles":files});
    data["herbertDirectOutput"] = json!(true);
    data["herbertCollectionStatus"] = json!({"completeness":if diagnostics.is_empty(){"collected"}else{"partial"},"diagnostics":diagnostics});
    data
}
fn safe_leaf(s: &str) -> String {
    let leaf = s.rsplit(['/', '\\']).next().unwrap_or(s);
    let out = leaf
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._- ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if out.is_empty() || out == "." || out == ".." {
        "assembly.dll".into()
    } else {
        out
    }
}
fn bytes(name: String, v: &Value) -> Result<Option<Value>, Diagnostic> {
    if v.is_null() || v.get("nativeFile").is_some() {
        return Ok(None);
    }
    let encoded = if let Some(s) = v.as_str() {
        s.to_owned()
    } else if let Some(a) = v.as_array() {
        use base64::Engine;
        let b = a
            .iter()
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n <= 255)
                    .map(|n| n as u8)
                    .ok_or_else(|| err("Invalid native assembly byte."))
            })
            .collect::<Result<Vec<_>, _>>()?;
        base64::engine::general_purpose::STANDARD.encode(b)
    } else {
        return Err(err("Invalid native assembly bytes."));
    };
    use base64::Engine;
    let b = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .map_err(|_| err("Invalid native assembly encoding."))?;
    if b.is_empty() {
        return Ok(None);
    }
    Ok(Some(json!({"name":name,"base64":encoded})))
}
fn records(body: &Value, service: bool) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let (root, kind, key, children) = if service {
        (
            "ZBODef",
            "ServiceDefinition",
            "ClassName",
            vec!["ZBODataSets", "ZServiceLicenseType"],
        )
    } else {
        (
            "ZDataSet",
            "DatasetDefinition",
            "DataSetID",
            vec!["ZRelation", "ZRelationField", "ZSubDataSet"],
        )
    };
    let roots = body[root]
        .as_array()
        .ok_or_else(|| err("Missing native program definitions."))?;
    let mut out = vec![];
    let mut seen = BTreeSet::new();
    for r in roots {
        let identity = id(kind, r, &["SystemCode", key])?;
        if r["SystemFlag"] != false || !seen.insert(serde_json::to_string(&identity).unwrap()) {
            return Err(err("Invalid or repeated custom definition."));
        }
        let mut ds = json!({});
        ds[root] = json!([r]);
        for t in &children {
            let rows = body[*t]
                .as_array()
                .ok_or_else(|| err("Missing native program children."))?;
            ds[*t] = json!(
                rows.iter()
                    .filter(|c| eq(c, r, "SystemCode")
                        && if *t == "ZSubDataSet" {
                            c["ParentDataSetID"] == r[key]
                        } else {
                            c[key] == r[key]
                        })
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        out.push((identity,complete(json!({"definition":ds,"coverage":"Native custom metadata; source evidence for SDK regeneration, not a compiled service or native import package."}),vec![],vec![])));
    }
    for t in children {
        let mut ids = BTreeSet::new();
        for c in body[t]
            .as_array()
            .ok_or_else(|| err("Missing program child table."))?
        {
            if c["SystemFlag"].as_bool() == Some(true) {
                return Err(err(
                    "A custom-only native child read returned system content.",
                ));
            }
            let rid = c["SysRowID"]
                .as_str()
                .ok_or_else(|| err("Missing child identity."))?;
            if !ids.insert(rid) {
                return Err(err("Duplicate program child."));
            }
            if !roots.iter().any(|r| {
                eq(c, r, "SystemCode")
                    && if t == "ZSubDataSet" {
                        c["ParentDataSetID"] == r[key]
                    } else {
                        c[key] == r[key]
                    }
            }) {
                return Err(err("Program child has no owner in the complete stream."));
            }
        }
    }
    Ok(out)
}
fn finish(
    records: Vec<(Identity, Value)>,
    next: Option<String>,
    s: &Value,
) -> Result<GuestReply, Diagnostic> {
    if s["_target"].is_object() {
        let target: Identity = serde_json::from_value(s["_target"].clone())
            .map_err(|_| err("Invalid program target."))?;
        let (_, data) = records.into_iter().find(|r| r.0 == target).ok_or_else(|| {
            err("The requested custom program no longer exists in its native scope.")
        })?;
        let completeness =
            serde_json::from_value(data["herbertCollectionStatus"]["completeness"].clone())
                .map_err(|_| err("Invalid target completeness."))?;
        let diagnostics =
            serde_json::from_value(data["herbertCollectionStatus"]["diagnostics"].clone())
                .map_err(|_| err("Invalid target diagnostics."))?;
        return Ok(GuestReply::Definition {
            identity: target,
            format: "programs-and-extensions-json".into(),
            definition: data,
            completeness,
            diagnostics,
        });
    }
    let (records,mut diagnostics)=select_records(records,s)?;
    diagnostics.extend(serde_json::from_value::<Vec<Diagnostic>>(s["selectionDiagnostics"].clone()).unwrap_or_default());

    if s["prepare"] == true && s["independent"] == true {
        let mut items: Vec<_> = records
            .into_iter()
            .map(|r| json!({"phase":"output","records":[r],"index":0,"next":null,"diagnostics":[]}))
            .collect();
        if items.is_empty()&&!diagnostics.is_empty(){items.push(json!({"phase":"output","records":[],"index":0,"next":null,"diagnostics":[]}));}
        if let Some(first)=items.first_mut(){first["diagnostics"]=json!(diagnostics);}
        return Ok(GuestReply::PreparedPage {
            state: json!({"_hostWorkItems":items,"_hostWorkNextCursor":next}),
            next_cursor: next,
            next_state: json!({}),
        });
    }
    let state = json!({"phase":"output","records":records,"index":0,"next":next,"diagnostics":diagnostics});
    if s["prepare"] == true {
        Ok(GuestReply::PreparedPage {
            state,
            next_cursor: next,
            next_state: json!({}),
        })
    } else {
        output(state)
    }
}
fn output(mut s: Value) -> Result<GuestReply, Diagnostic> {
    let i = s["index"].as_u64().unwrap_or(0) as usize;
    if i == s["records"].as_array().map_or(0, Vec::len) {
        return Ok(GuestReply::Inventory {
            identities: vec![],
            next_cursor: s["next"].as_str().map(str::to_owned),
            authoritative: s["diagnostics"].as_array().is_none_or(Vec::is_empty),
            diagnostics: serde_json::from_value(s["diagnostics"].clone()).unwrap_or_default(),
        });
    }
    let record = serde_json::from_value(s["records"][i].take())
        .map_err(|_| err("Invalid prepared program output."))?;
    s["index"] = json!(i + 1);
    Ok(GuestReply::ReadyRecords {
        records: vec![record],
        state: s,
    })
}
fn finish_one(record: (Identity, Value), mut s: Value) -> Result<GuestReply, Diagnostic> {
    if s["_target"].is_object() {
        return finish(vec![record], None, &s);
    }
    if s["prepare"] == true {
        let extension = s["phase"] == "extension-next";
        let next=Some(json!({"stream":if extension{"extensions"}else{"externals"},"after":record.0.keys[if extension{"ExtensionSetID"}else{"Name"}]}).to_string());
        let diagnostics=s["selectionDiagnostics"].take();
        Ok(GuestReply::PreparedPage {
            state: json!({"phase":"output","records":[record],"index":0,"next":next,"diagnostics":diagnostics}),
            next_cursor: next,
            next_state: s,
        })
    } else {
        Ok(GuestReply::ReadyRecords {
            records: vec![record],
            state: s,
        })
    }
}
fn refs(s: Value) -> Result<GuestReply, Diagnostic> {
    let ids = s["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "ExtensionSetID eq '{}'",
                r["ExtensionSetID"].as_str().unwrap().replace('\'', "''")
            )
        })
        .collect::<Vec<_>>()
        .join(" or ");
    Ok(GuestReply::Action {
        action: HostAction::NativeEntityPage {
            service: "Ice.BO.ExtensionSvc".into(),
            entity: "ExtensionTables".into(),
            query: BTreeMap::from([
                (
                    "$select".into(),
                    "ExtensionSetID,SystemCode,DataTableID,DataSetSystemCode,DataSetID,SysRowID"
                        .into(),
                ),
                ("$filter".into(), ids),
                ("$orderby".into(), "SysRowID".into()),
                ("$count".into(), "true".into()),
            ]), // OData $top is Int32; use its supported range rather than a root window.
            page_size: i32::MAX as u32,
            page: s["refPage"].as_u64().unwrap(),
        },
        state: s,
    })
}
fn next_binary(mut s: Value) -> Result<GuestReply, Diagnostic> {
    let i = s["binaryIndex"].as_u64().unwrap() as usize;
    if s["phase"] == "external-next" {
        let a = s["externals"].as_array().unwrap();
        if i == a.len() {
            return finish(vec![], None, &s);
        }
        let name = a[i]["Name"]
            .as_str()
            .ok_or_else(|| err("External reference has no Name."))?
            .to_owned();
        s["phase"] = json!("external-bytes");
        return Ok(crate::native(
            "Ice.Lib.EcfToolsSvc",
            "GetAssemblyBytes",
            json!({"assemblyId":name}),
            false,
            s,
        ));
    }
    if i == s["extensionRecords"].as_array().unwrap().len() {
        return finish(vec![], if s["binaryWorkItem"] == true { None } else { Some("externals".into()) }, &s);
    }
    let id = s["extensionRecords"][i][0]["keys"]["ExtensionSetID"].clone();
    s["phase"] = json!("extension-bytes");
    Ok(crate::native(
        "Ice.BO.ExtensionSvc",
        "GetExtensionSetAssembly",
        json!({"extensionSetID":id}),
        false,
        s,
    ))
}
fn binary_work(records: Vec<Value>, extension: bool) -> GuestReply {
    let count = records.len();
    let items:Vec<_>=records.into_iter().map(|record| {
        let mut state=json!({"phase":if extension{"extension-next"}else{"external-next"},
            "binaryIndex":0,"binaryWorkItem":true});
        state[if extension{"extensionRecords"}else{"externals"}]=json!([record]);
        state
    }).collect();
    let next=extension.then(||"externals".to_owned());
    GuestReply::PreparedPage{
        state:json!({"_hostWorkItems":items,"_hostWorkNextCursor":next,"_progress":{"pageRecords":count}}),
        next_cursor:next,next_state:json!({}),
    }
}
fn selected_binary_work(records:Vec<Value>,extension:bool,state:&Value)->GuestReply{let mut reply=binary_work(records,extension);if let GuestReply::PreparedPage {state:prepared,..}=&mut reply{if let Some(first)=prepared["_hostWorkItems"].as_array_mut().and_then(|items|items.first_mut()){first["selectionDiagnostics"]=state["selectionDiagnostics"].clone();}}reply}
fn drive(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if r.operation == "materialize" && r.state["phase"] == "output" {
        return output(r.state);
    }

    if !matches!(r.operation.as_str(), "discover" | "materialize") {
        return Err(err("Unsupported program operation."));
    }
    let mut s = r.state;
    if s["phase"].is_null() && r.error.is_some() { return Err(r.error.take().unwrap()); }
    if s["phase"].is_null() {
        s = json!({"prepare":s["_hostPreparePage"],"independent":s["_hostIndependentWorkItems"],"_target":s["_target"],"_pluginSettings":s["_pluginSettings"]});
        let resume = r
            .cursor
            .as_deref()
            .filter(|v| v.starts_with('{'))
            .map(serde_json::from_str::<Value>)
            .transpose()
            .map_err(|_| err("Invalid program recovery cursor."))?;
        let stream = resume
            .as_ref()
            .and_then(|v| v["stream"].as_str())
            .map(str::to_owned)
            .or(r.cursor.clone());
        if let Some(v) = resume {
            if !matches!(v["stream"].as_str(), Some("extensions" | "externals"))
                || v.as_object().is_none_or(|o| o.len() != 2)
            {
                return Err(err("Invalid binary recovery stream."));
            }
            let after = v["after"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| err("Recovery cursor omitted its exact identity."))?;
            s["afterIdentity"] = json!(after);
        }
        if !s["_target"].is_object()&&r.response.is_none()&&r.error.is_none(){let (family,_)=selection(&s)?;let current=stream.as_deref().unwrap_or("services");if family!="all"&&family!=current&&matches!(current,"services"|"datasets"|"extensions"|"externals"){let next=match current{"services"=>Some("datasets"),"datasets"=>Some("extensions"),"extensions"=>Some("externals"),_=>None};return Ok(GuestReply::Inventory {identities:vec![],next_cursor:next.map(str::to_owned),authoritative:true,diagnostics:vec![]});}}
        match stream.as_deref() {
            None => {
                s["phase"] = json!("services");
                return Ok(crate::native(
                    "Ice.BO.ZBODefSvc",
                    "GetRows",
                    json!({"whereClauseZBODef":"SystemFlag = false","whereClauseZBODataSets":"SystemFlag = false","whereClauseZServiceLicenseType":"","pageSize":0,"absolutePage":1}),
                    false,
                    s,
                ));
            }
            Some("datasets") => {
                s["phase"] = json!("datasets");
                return Ok(crate::native(
                    "Ice.BO.ZDataSetSvc",
                    "GetRows",
                    json!({"whereClauseZDataSet":"SystemFlag = false","whereClauseZRelation":"SystemFlag = false","whereClauseZRelationField":"SystemFlag = false","whereClauseZSubDataSet":"SystemFlag = false","pageSize":0,"absolutePage":1}),
                    false,
                    s,
                ));
            }
            Some("extensions") => {
                s["phase"] = json!("extension-list");
                return Ok(crate::native(
                    "Ice.BO.ExtensionSvc",
                    "GetList",
                    json!({"whereClause":"SystemFlag = false","pageSize":0,"absolutePage":1}),
                    false,
                    s,
                ));
            }
            Some("externals") => {
                s["phase"] = json!("externals");
                return Ok(crate::native(
                    "Ice.BO.BpMethodSvc",
                    "GetAvailableReferences",
                    json!({"referenceGroup":"Externals"}),
                    false,
                    s,
                ));
            }
            _ => return Err(err("Invalid program cursor.")),
        }
    }
    let phase = s["phase"].as_str().unwrap_or("").to_owned();
    if matches!(phase.as_str(), "extension-next" | "external-next") {
        return next_binary(s);
    }
    if let Some(error) = r.error.take() {
        if matches!(phase.as_str(), "extension-hidden" | "sdk-hidden") {
            s["hiddenError"] = json!(error);
        } else if matches!(phase.as_str(), "extension-bytes" | "external-bytes") {
            s["binaryError"] = json!(error);
        } else {
            return Err(error);
        }
    }
    let response = r.response.unwrap_or(Value::Null);
    if response["parameters"]["morePages"] == true {
        return Err(err("Complete program read reported more pages."));
    }
    match phase.as_str() {
        "services" | "datasets" => {
            let root = if phase == "services" {
                "ZBODef"
            } else {
                "ZDataSet"
            };
            if response["returnObj"][root]
                .as_array()
                .is_some_and(Vec::is_empty)
            {
                return finish(
                    records(&response["returnObj"], phase == "services")?,
                    Some(
                        if phase == "services" {
                            "datasets"
                        } else {
                            "extensions"
                        }
                        .into(),
                    ),
                    &s,
                );
            }
            s["nativeBody"] = response["returnObj"].clone();
            s["nativeKind"] = json!(phase);
            s["phase"] = json!("sdk-hidden");
            return Ok(GuestReply::Action {
                action: HostAction::SharedArtifactFunctionCall {
                    alias: "programs".into(),
                    function: "ReadInternalMetadata".into(),
                    arguments: json!({}),
                },
                state: s,
            });
        }
        "sdk-hidden" => {
            let service = s["nativeKind"] == "services";
            let mut body = s["nativeBody"].take();
            if response["oSuccess"] == true {
                let supplemental: Value = serde_json::from_str(
                    response["oJson"]
                        .as_str()
                        .ok_or_else(|| err("Stored SDK fields omitted JSON."))?,
                )
                .map_err(|_| err("Invalid stored SDK field JSON."))?;
                let (table, section) = if service {
                    ("ZBODataSets", "ServiceDatasetFields")
                } else {
                    ("ZDataSet", "DatasetStoredFields")
                };
                let values = supplemental[section]
                    .as_array()
                    .ok_or_else(|| err("The helper omitted supplemental SDK fields."))?;
                for row in body[table]
                    .as_array_mut()
                    .ok_or_else(|| err("The native SDK table is missing."))?
                {
                    let matches: Vec<_> = values
                        .iter()
                        .filter(|v| v["SysRowID"] == row["SysRowID"])
                        .collect();
                    if matches.len() != 1 {
                        return Err(err(
                            "Stored SDK fields have missing or duplicate native ownership.",
                        ));
                    }
                    for (k, v) in matches[0]
                        .as_object()
                        .ok_or_else(|| err("Invalid SDK supplement."))?
                    {
                        if let Some(original) = row.get(k) {
                            if original != v {
                                return Err(err("SDK supplement disagrees with a native field."));
                            }
                        } else {
                            row[k] = v.clone();
                        }
                    }
                }
            } else if s["hiddenError"].is_null() {
                s["hiddenError"] = json!(Diagnostic::new(
                    "PROGRAM_STORED_SDK_FIELDS_UNAVAILABLE",
                    response["oMessage"]
                        .as_str()
                        .unwrap_or("The helper is unavailable; native SDK fields remain retained.")
                ));
            }
            let mut output = records(&body, service)?;
            if !s["hiddenError"].is_null() {
                for (_, data) in &mut output {
                    data["herbertCollectionStatus"] =
                        json!({"completeness":"partial","diagnostics":[s["hiddenError"]]});
                }
            }
            return finish(
                output,
                Some(if service { "datasets" } else { "extensions" }.into()),
                &s,
            );
        }
        "extension-list" => {
            let roots = response["returnObj"]["ExtensionList"]
                .as_array()
                .ok_or_else(|| err("Missing custom extension inventory."))?;
            let mut seen = BTreeSet::new();
            for row in roots {
                if row["SystemFlag"] == true
                    || !seen.insert(
                        serde_json::to_string(&id("ExtensionSet", row, &["ExtensionSetID"])?)
                            .unwrap(),
                    )
                {
                    return Err(err("Invalid custom extension inventory."));
                }
            }
            if roots.is_empty() {
                return finish(vec![], Some("externals".into()), &s);
            }
            s["roots"] = json!(roots);
            s["refs"] = json!([]);
            s["refPage"] = json!(1);
            s["phase"] = json!("references");
            return refs(s);
        }
        "references" => {
            let rows = response["value"]
                .as_array()
                .ok_or_else(|| err("Missing extension table references."))?;
            let total = response["@odata.count"]
                .as_u64()
                .ok_or_else(|| err("Missing extension reference count."))?;
            let mut seen: BTreeSet<_> = s["refs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["SysRowID"].clone().to_string())
                .collect();
            for row in rows {
                if !s["roots"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| eq(r, row, "ExtensionSetID"))
                    || !seen.insert(row["SysRowID"].to_string())
                {
                    return Err(err("Foreign or repeated extension reference."));
                }
                for key in [
                    "SystemCode",
                    "DataTableID",
                    "DataSetSystemCode",
                    "DataSetID",
                    "SysRowID",
                ] {
                    if !row[key].is_string() {
                        return Err(err("Incomplete native extension reference."));
                    }
                }
                s["refs"].as_array_mut().unwrap().push(row.clone());
            }
            let got = s["refs"].as_array().unwrap().len() as u64;
            if got > total || (rows.is_empty() && got < total) {
                return Err(err("Truncated extension references."));
            }
            if got < total {
                s["refPage"] = json!(s["refPage"].as_u64().unwrap() + 1);
                return refs(s);
            }
            s["phase"] = json!("extension-schema");
            return Ok(crate::native(
                "Ice.BO.ZDataTableSvc",
                "GetRows",
                crate::programs_graph::schema(),
                false,
                s,
            ));
        }
        "extension-schema" => {
            s["metadata"] = crate::programs_graph::metadata(&response)?;
            let a =
                crate::programs_graph::build(&r.company, &s["metadata"], &s["roots"], &s["refs"])?;
            s["phase"] = json!("extension-graph");
            return Ok(crate::native(
                "Ice.BO.DynamicQuerySvc",
                "Execute",
                a,
                false,
                s,
            ));
        }
        "extension-graph" => {
            s["graph"] = crate::programs_graph::decode(&response, &s["metadata"], &r.company)?;
            s["phase"] = json!("extension-hidden");
            return Ok(GuestReply::Action {
                action: HostAction::SharedArtifactFunctionCall {
                    alias: "programs".into(),
                    function: "ReadInternalMetadata".into(),
                    arguments: json!({}),
                },
                state: s,
            });
        }
        "extension-hidden" => {
            let g = s["graph"].take();
            let extension_names:BTreeSet<_>=g["Extension"].as_array().unwrap().iter().filter_map(|r|r["ExtensionSetID"].as_str().map(str::to_ascii_lowercase)).collect();
            let extension_row_ids:BTreeSet<_>=g["Extension"].as_array().unwrap().iter().map(|r|r["SysRowID"].to_string()).collect();
            let visible_extensions:BTreeSet<_>=s["roots"].as_array().unwrap().iter().filter_map(|r|Some((r["ExtensionSetID"].as_str()?.to_ascii_lowercase(),r["SysRowID"].to_string()))).collect();
            let mut hidden = json!({"RuntimeExtension":[],"ExtensionDataSetTable":[],"ZSystem":[]});
            if response["oSuccess"] == true {
                hidden = serde_json::from_str(
                    response["oJson"]
                        .as_str()
                        .ok_or_else(|| err("Hidden metadata omitted JSON."))?,
                )
                .map_err(|_| err("Hidden metadata JSON is invalid."))?;
                for t in ["RuntimeExtension", "ExtensionDataSetTable", "ZSystem"] {
                    let rows = hidden[t]
                        .as_array()
                        .ok_or_else(|| err("Hidden metadata omitted a table."))?;
                    let mut seen = BTreeSet::new();
                    for row in rows {
                        let id = row["SysRowID"]
                            .as_str()
                            .ok_or_else(|| err("Hidden row omitted identity."))?;
                        if !seen.insert(id) {
                            return Err(err("Repeated hidden metadata identity."));
                        }
                        if row["SystemFlag"] == true {
                            return Err(err("Hidden metadata returned system content."));
                        }
                        if t == "RuntimeExtension"
                            && !extension_row_ids.contains(&row["ExtensionSetRowID"].to_string())
                        {
                            return Err(err("Runtime registration has no custom extension owner."));
                        }
                        if t == "ExtensionDataSetTable"
                            && row["ExtensionSetID"].as_str().is_none_or(|id|!extension_names.contains(&id.to_ascii_lowercase()))
                        {
                            return Err(err("Dataset link has no custom extension owner."));
                        }
                    }
                }
            } else {
                if s["hiddenError"].is_null() {
                    s["hiddenError"]=json!(Diagnostic::new("PROGRAM_INTERNAL_METADATA_UNAVAILABLE",response["oMessage"].as_str().unwrap_or("The hidden metadata helper is unavailable; direct definitions and binaries remain collected.")));
                }
            }
            for t in [
                "ExtensionSetMapping",
                "ExtensionRelation",
                "ExtensionRelationColumn",
            ] {
                for row in g[t].as_array().unwrap() {
                    if row["ExtensionSetID"].as_str().is_none_or(|id|!extension_names.contains(&id.to_ascii_lowercase()))
                    {
                        return Err(err(
                            "The extension graph returned an orphaned relationship.",
                        ));
                    }
                }
            }
            let refs_index=row_index(s["refs"].as_array().unwrap(),&["ExtensionSetID"],true);
            let owned_indices:BTreeMap<_,_>=["ExtensionSetMapping","ExtensionRelation","ExtensionRelationColumn"].into_iter().map(|table|(table,row_index(g[table].as_array().unwrap(),&["ExtensionSetID"],true))).collect();
            let dataset_index=row_index(hidden["ExtensionDataSetTable"].as_array().unwrap(),&["ExtensionSetID"],true);
            let system_index=row_index(hidden["ZSystem"].as_array().unwrap(),&["SystemCode"],true);
            let runtime_index=row_index(hidden["RuntimeExtension"].as_array().unwrap(),&["ExtensionSetRowID"],false);
            let schema_indices:BTreeMap<_,_>=["ZDataTable","ZDataField","ZKey","ZKeyField"].into_iter().map(|table|(table,row_index(g[table].as_array().unwrap(),&["SystemCode","DataTableID"],true))).collect();
            let mut out = vec![];
            for root in g["Extension"].as_array().unwrap() {
                if root["ExtensionSetID"].as_str().is_none_or(|id|!visible_extensions.contains(&(id.to_ascii_lowercase(),root["SysRowID"].to_string())))
                {
                    return Err(err(
                        "The graph returned an extension outside native caller-visible scope.",
                    ));
                }
                let id = id("ExtensionSet", root, &["ExtensionSetID"])?;
                let owner=vec![id.keys["ExtensionSetID"].to_ascii_lowercase()];
                let rf=indexed_rows(&refs_index,[owner.clone()]);
                let mut d = json!({"Extension":[root],"ExtensionTableContext":rf,"coverage":"Persisted custom extension metadata and exact readable assembly/symbol bytes. System schema bodies excluded; native context preserves dependency keys. Metadata is source evidence, not an import package."});
                for t in [
                    "ExtensionSetMapping",
                    "ExtensionRelation",
                    "ExtensionRelationColumn",
                ] {
                    d[t] = json!(indexed_rows(&owned_indices[t],[owner.clone()]));
                }
                d["ExtensionDataSetTable"] = json!(indexed_rows(&dataset_index,[owner.clone()]));
                d["ZSystem"] = json!(indexed_rows(&system_index,rf.iter().filter_map(|r|r["SystemCode"].as_str().map(|v|vec![v.to_ascii_lowercase()]))));
                d["hiddenMetadataError"] = s["hiddenError"].clone();
                d["RuntimeExtension"] = json!(indexed_rows(&runtime_index,root["SysRowID"].as_str().map(|v|vec![v.to_owned()])));
                for t in ["ZDataTable", "ZDataField", "ZKey", "ZKeyField"] {
                    d[t] = json!(indexed_rows(&schema_indices[t],rf.iter().filter_map(|r|Some(vec![r["SystemCode"].as_str()?.to_ascii_lowercase(),r["DataTableID"].as_str()?.to_ascii_lowercase()]))));
                }
                out.push((id, d));
            }
            if out.len() != s["roots"].as_array().unwrap().len() {
                return Err(err("Extension graph omitted an inventoried root."));
            }
            out.sort_by(|a, b| a.0.keys["ExtensionSetID"].cmp(&b.0.keys["ExtensionSetID"]));
            if let Some(after) = s["afterIdentity"].as_str() {
                out.retain(|r| r.0.keys["ExtensionSetID"].as_str() > after);
            }
            if s["_target"].is_object() {
                let target: Identity = serde_json::from_value(s["_target"].clone())
                    .map_err(|_| err("Invalid extension target."))?;
                out.retain(|r| r.0 == target);
                if out.is_empty() {
                    return Err(err("The requested extension is no longer visible."));
                }
            }
            let (out,errors)=select_records(out,&s)?;
            s["selectionDiagnostics"]=json!(errors);
            if out.is_empty(){return finish(vec![],Some("externals".into()),&s);}
            if s["prepare"] == true && s["independent"] == true && !s["_target"].is_object() {
                return Ok(selected_binary_work(out.into_iter().map(|record|json!(record)).collect(),true,&s));
            }
            s["extensionRecords"] = json!(out);
            s["binaryIndex"] = json!(0);
            s["phase"] = json!("extension-next");
            return next_binary(s);
        }
        "extension-bytes" => {
            let i = s["binaryIndex"].as_u64().unwrap() as usize;
            let (id, mut d): (Identity, Value) =
                serde_json::from_value(s["extensionRecords"][i].take())
                    .map_err(|_| err("Invalid extension output checkpoint."))?;
            let mut files = vec![];
            let mut native_files = vec![];
            let mut diagnostics = vec![];
            if !d["hiddenMetadataError"].is_null() {
                diagnostics.push(
                    serde_json::from_value(d["hiddenMetadataError"].clone())
                        .map_err(|_| err("Invalid hidden metadata diagnostic."))?,
                );
            }
            if let Some(e) = s.get("binaryError").filter(|v| !v.is_null()) {
                diagnostics.push(
                    serde_json::from_value(e.clone()).map_err(|_| err("Invalid binary error."))?,
                );
            } else {
                let body = &response["returnObj"];
                let mut assembly_retained = false;
                for (field, suffix) in [("Assembly", "dll"), ("Symbols", "pdb")] {
                    let stem = safe_leaf(
                        d["Extension"][0]["Assembly"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .unwrap_or(&id.keys["ExtensionSetID"]),
                    );
                    let stem = stem.strip_suffix(".dll").unwrap_or(&stem);
                    let name = format!("{stem}.{suffix}");
                    if let Some(file) = body[field].get("nativeFile") {
                        if field == "Assembly" {
                            assembly_retained = file["bytes"].as_u64().is_some_and(|n| n > 0);
                        }
                        native_files.push(json!({"name":name,"file":file}));
                    } else if let Some(f) = bytes(name, &body[field])? {
                        assembly_retained |= field == "Assembly";
                        files.push(f);
                    }
                }
                if !assembly_retained
                    && d["Extension"][0]["Assembly"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty())
                {
                    diagnostics.push(Diagnostic::new("EXTENSION_BYTES_UNAVAILABLE","The extension registers an assembly but the native byte route returned no nonempty assembly content."));
                }
                d["nativeAssembly"] = body.clone();
            }
            s["binaryError"] = Value::Null;
            s["binaryIndex"] = json!(i + 1);
            s["phase"] = json!("extension-next");
            let mut data = complete(d, files, diagnostics);
            data["herbertSources"]["nativeFiles"] = json!(native_files);
            return finish_one((id, data), s);
        }
        "externals" => {
            let rows = response["returnObj"]
                .as_array()
                .ok_or_else(|| err("Missing Externals catalog."))?;
            let mut seen = BTreeSet::new();
            for row in rows {
                if ["FileName", "Version"]
                    .iter()
                    .any(|k| !row.get(*k).is_some_and(|v| v.is_null() || v.is_string()))
                {
                    return Err(err(
                        "External reference metadata has an invalid native shape.",
                    ));
                }
                let id = id("ExternalAssemblyReference", row, &["Name"])?;
                if !seen.insert(serde_json::to_string(&id).unwrap()) {
                    return Err(err("Duplicate external assembly reference."));
                }
            }
            let mut rows = rows.clone();
            rows.sort_by_key(|r| r["Name"].as_str().unwrap().to_owned());
            if let Some(after) = s["afterIdentity"].as_str() {
                rows.retain(|r| r["Name"].as_str().unwrap() > after);
            }
            if let Some(name) = s["_target"]["keys"]["Name"].as_str() {
                rows.retain(|r| r["Name"] == name);
                if rows.is_empty() {
                    return Err(err("The requested external assembly is no longer visible."));
                }
            }
            let mut selected_rows=Vec::new();for row in rows {let identity=id("ExternalAssemblyReference",&row,&["Name"])?;if selected(&identity,&s)?{selected_rows.push(row);}}let rows=selected_rows;
            if rows.is_empty(){return finish(vec![],None,&s);}
            if s["prepare"] == true && s["independent"] == true && !s["_target"].is_object() {
                return Ok(selected_binary_work(rows,false,&s));
            }
            s["externals"] = json!(rows);
            s["binaryIndex"] = json!(0);
            s["phase"] = json!("external-next");
            return next_binary(s);
        }
        "external-bytes" => {
            let i = s["binaryIndex"].as_u64().unwrap() as usize;
            let row = &s["externals"][i];
            let id = id("ExternalAssemblyReference", row, &["Name"])?;
            let name = safe_leaf(
                row["FileName"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(&id.keys["Name"]),
            );
            let name = if name.to_ascii_lowercase().ends_with(".dll") {
                name
            } else {
                format!("{name}.dll")
            };
            let mut diagnostics = vec![];
            let mut files = vec![];
            if let Some(e) = s.get("binaryError").filter(|v| !v.is_null()) {
                diagnostics.push(
                    serde_json::from_value(e.clone()).map_err(|_| err("Invalid binary error."))?,
                );
            } else if let Some(file) = bytes(name.clone(), &response["returnObj"])? {
                files.push(file);
            } else if response.pointer("/returnObj/nativeFile").is_none() {
                diagnostics.push(Diagnostic::new("REFERENCE_BYTES_UNAVAILABLE","The Externals catalog lists this reference but the native byte route returned no readable assembly."));
            }
            let mut d = complete(
                json!({"catalog":row,"coverage":"Native Externals reference and exact readable assembly bytes. No original source or separate external symbols API is exposed."}),
                files,
                diagnostics,
            );
            if let Some(file) = response.pointer("/returnObj/nativeFile") {
                d["herbertSources"]["nativeFiles"] = json!([{"name":name,"file":file}]);
            }
            s["binaryError"] = Value::Null;
            s["binaryIndex"] = json!(i + 1);
            s["phase"] = json!("external-next");
            return finish_one((id, d), s);
        }
        _ => Err(err("Unknown program collection phase.")),
    }
}
pub fn handle(r: GuestRequest) -> GuestReply {
    let from=r.state.clone();if r.operation!="acquire"&&!r.state["_target"].is_object(){if let Err(diagnostic)=selection(&r.state){return GuestReply::Failure {diagnostic};}}
    carry(handle_inner(r),&from)
}
fn handle_inner(mut r: GuestRequest) -> GuestReply {
    if r.operation == "acquire" {
        let Some(target) = r.identity.clone() else {
            return GuestReply::Failure {
                diagnostic: err("Select an exact program identity."),
            };
        };
        let valid = target.category == "C22"
            && target.scope == Scope::Global
            && match target.kind.as_str() {
                "ServiceDefinition" => {
                    target.keys.len() == 2
                        && ["SystemCode", "ClassName"]
                            .iter()
                            .all(|k| target.keys.get(*k).is_some_and(|v| !v.is_empty()))
                }
                "DatasetDefinition" => {
                    target.keys.len() == 2
                        && ["SystemCode", "DataSetID"]
                            .iter()
                            .all(|k| target.keys.get(*k).is_some_and(|v| !v.is_empty()))
                }
                "ExtensionSet" => {
                    target.keys.len() == 1
                        && target
                            .keys
                            .get("ExtensionSetID")
                            .is_some_and(|v| !v.is_empty())
                }
                "ExternalAssemblyReference" => {
                    target.keys.len() == 1 && target.keys.get("Name").is_some_and(|v| !v.is_empty())
                }
                _ => false,
            };
        if !valid {
            return GuestReply::Failure {
                diagnostic: err("The program identity has invalid keys or scope."),
            };
        }
        if r.state["phase"].is_null() {
            r.cursor = match target.kind.as_str() {
                "ServiceDefinition" => None,
                "DatasetDefinition" => Some("datasets".into()),
                "ExtensionSet" => Some("extensions".into()),
                _ => Some("externals".into()),
            };
            r.state["_target"] = json!(target);
        }
        r.operation = "discover".into();
    }
    match drive(r) {
        Ok(v) => v,
        Err(diagnostic) => GuestReply::Failure { diagnostic },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_program_children_match_native_case_rules_order_and_reference_union() {
        let rows=vec![json!({"ExtensionSetID":"b","SystemCode":"Erp","DataTableID":"Y","unknown":"é first"}),json!({"ExtensionSetID":"A","SystemCode":"Ice","DataTableID":"X"}),json!({"ExtensionSetID":"B","SystemCode":"erp","DataTableID":"Z"}),json!({"ExtensionSetID":"unrelated","SystemCode":"Other","DataTableID":"X"}),json!({"SystemCode":null})];
        let index=row_index(&rows,&["ExtensionSetID"],true);
        for name in ["a","B","missing"]{
            let owner=json!({"ExtensionSetID":name});
            assert_eq!(json!(indexed_rows(&index,[vec![name.to_ascii_lowercase()]])),json!(rows.iter().filter(|r|eq(r,&owner,"ExtensionSetID")).collect::<Vec<_>>()));
        }
        let refs=vec![json!({"SystemCode":"ERP","DataTableID":"Z"}),json!({"SystemCode":"erp","DataTableID":"Y"}),json!({"SystemCode":"Erp","DataTableID":"Y"})];
        let index=row_index(&rows,&["SystemCode","DataTableID"],true);
        let selected=indexed_rows(&index,refs.iter().map(|r|vec![r["SystemCode"].as_str().unwrap().to_ascii_lowercase(),r["DataTableID"].as_str().unwrap().to_ascii_lowercase()]));
        assert_eq!(json!(selected),json!(rows.iter().filter(|row|refs.iter().any(|r|eq(row,r,"SystemCode")&&eq(row,r,"DataTableID"))).collect::<Vec<_>>()));
        let exact=row_index(&rows,&["ExtensionSetID"],false);
        assert_eq!(indexed_rows(&exact,[vec!["B".into()]]).len(),1);
    }
    #[test]
    fn binary_byte_values_are_preserved_and_invalid_values_refused() {
        assert_eq!(
            bytes("x".into(), &json!([0, 127, 255])).unwrap().unwrap()["base64"],
            "AH//"
        );
        assert!(bytes("x".into(), &json!([256])).is_err());
        assert!(bytes("x".into(), &Value::Null).unwrap().is_none());
    }
    #[test]
    fn file_names_are_confined_to_a_leaf() {
        assert_eq!(safe_leaf("../../a.dll"), "a.dll");
        assert_eq!(safe_leaf(".."), "assembly.dll");
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::*;
    #[test]
    fn independent_binary_owners_preserve_bytes_and_do_not_carry_other_graphs() {
        for extension in [false,true] {
            let entries:Vec<_>=["A","B"].into_iter().map(|name|if extension {
                let identity=id("ExtensionSet",&json!({"ExtensionSetID":name}),&["ExtensionSetID"]).unwrap();
                json!([identity,{"Extension":[{"ExtensionSetID":name,"Assembly":format!("{name}.dll")}],"original":format!("graph {name}")}])
            }else{json!({"Name":name,"FileName":format!("{name}.dll"),"Version":"original"})}).collect();
            let GuestReply::PreparedPage{state,next_cursor,..}=binary_work(entries,extension)else{panic!()};
            assert_eq!(next_cursor,extension.then(||"externals".into()));
            for (index,item) in state["_hostWorkItems"].as_array().unwrap().iter().enumerate(){
                assert_eq!(item[if extension{"extensionRecords"}else{"externals"}].as_array().unwrap().len(),1);
                let mut r=request();r.operation="materialize".into();r.state=item.clone();
                let GuestReply::Action{action:HostAction::NativeRead{method,arguments,..},state}=drive(r.clone()).unwrap()else{panic!()};
                assert_eq!(method,if extension{"GetExtensionSetAssembly"}else{"GetAssemblyBytes"});
                assert_eq!(arguments[if extension{"extensionSetID"}else{"assemblyId"}],if index==0{"A"}else{"B"});
                assert!(!serde_json::to_string(&state).unwrap().contains(if index==0{"graph B"}else{"graph A"}));
                r.state=state;r.response=Some(if extension{json!({"returnObj":{"Assembly":[0,127,255],"Symbols":[1,2]}})}else{json!({"returnObj":[0,127,255]})});
                let GuestReply::ReadyRecords{records,state}=drive(r.clone()).unwrap()else{panic!()};
                assert_eq!(records[0].1["herbertSources"]["binaryFiles"][0]["base64"],"AH//");
                r.state=state;r.response=None;
                assert!(matches!(drive(r).unwrap(),GuestReply::Inventory{next_cursor:None,..}));
            }
        }
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C22".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(1),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    #[test]
    fn cold_binary_cursor_reacquires_bulk_scope_and_materialization_keeps_same_cursor() {
        let identity = id(
            "ExtensionSet",
            &json!({"ExtensionSetID":"ees://Provider/Name"}),
            &["ExtensionSetID"],
        )
        .unwrap();
        let s = json!({"phase":"extension-next","binaryIndex":1,"prepare":true});
        let GuestReply::PreparedPage {
            state,
            next_cursor,
            next_state,
        } = finish_one((identity, json!({"kept":true})), s).unwrap()
        else {
            panic!()
        };
        let mut r = request();
        r.cursor = next_cursor.clone();
        let GuestReply::Action {
            action: HostAction::NativeRead { method, .. },
            state: recovery,
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(method, "GetList");
        assert_eq!(recovery["afterIdentity"], "ees://Provider/Name");
        assert_eq!(next_state["phase"], "extension-next");
        let GuestReply::ReadyRecords { state, .. } = output(state).unwrap() else {
            panic!()
        };
        assert!(
            matches!(output(state).unwrap(),GuestReply::Inventory{next_cursor:actual,..}if actual==next_cursor)
        );
    }
    #[test]
    fn helper_refusal_retains_native_sdk_records_and_marks_only_missing_coverage() {
        let mut r = request();
        r.state = json!({"phase":"sdk-hidden","nativeKind":"services","prepare":true,"nativeBody":{"ZBODef":[{"SystemCode":"Ice","ClassName":"Custom","SystemFlag":false,"SysRowID":"root"}],"ZBODataSets":[],"ZServiceLicenseType":[]}});
        r.error = Some(Diagnostic::new(
            "DIRECT_ONLY",
            "Helpers are disabled for this run.",
        ));
        let GuestReply::PreparedPage { state, .. } = handle(r) else {
            panic!()
        };
        assert_eq!(
            state["records"][0][1]["definition"]["ZBODef"][0]["ClassName"],
            "Custom"
        );
        assert_eq!(
            state["records"][0][1]["herbertCollectionStatus"]["completeness"],
            "partial"
        );
    }
    #[test]
    fn metadata_closure_rejects_orphan_and_duplicate_children() {
        let mut b = json!({"ZDataSet":[{"SystemCode":"I","DataSetID":"D","SystemFlag":false,"SysRowID":"d"}],"ZRelation":[],"ZRelationField":[],"ZSubDataSet":[]});
        b["ZSubDataSet"] =
            json!([{"SystemCode":"I","ParentDataSetID":"Other","SysRowID":"s","SystemFlag":false}]);
        assert!(records(&b, false).is_err());
        b["ZSubDataSet"][0]["ParentDataSetID"] = json!("D");
        assert_eq!(records(&b, false).unwrap().len(), 1);
        let row = b["ZSubDataSet"][0].clone();
        b["ZSubDataSet"].as_array_mut().unwrap().push(row);
        assert!(records(&b, false).is_err());
    }
    #[test]
    fn declared_missing_assembly_preserves_metadata_with_explicit_partial_status() {
        let mut r = request();
        let identity = id(
            "ExtensionSet",
            &json!({"ExtensionSetID":"E"}),
            &["ExtensionSetID"],
        )
        .unwrap();
        r.state = json!({"phase":"extension-bytes","binaryIndex":0,"prepare":true,"extensionRecords":[[identity,{"Extension":[{"Assembly":"ExpectedAssembly"}]}]]});
        r.response = Some(json!({"returnObj":{}}));
        let GuestReply::PreparedPage { state, .. } = handle(r) else {
            panic!()
        };
        assert_eq!(
            state["records"][0][1]["herbertCollectionStatus"]["diagnostics"][0]["code"],
            "EXTENSION_BYTES_UNAVAILABLE"
        );
        assert_eq!(
            state["records"][0][1]["Extension"][0]["Assembly"],
            "ExpectedAssembly"
        );
    }
}

#[cfg(test)]
mod targeted_tests {
    use super::*;
    #[test]
    fn exact_target_uses_bulk_metadata_instead_of_legacy_family_reads() {
        let target = id(
            "DatasetDefinition",
            &json!({"SystemCode":"ICE","DataSetID":"D"}),
            &["SystemCode", "DataSetID"],
        )
        .unwrap();
        let r = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "C22".into(),
            company: "C".into(),
            identity: Some(target.clone()),
            cursor: None,
            page_size: Some(1),
            state: Value::Null,
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::NativeRead {
                method, arguments, ..
            },
            state,
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(method, "GetRows");
        assert_eq!(arguments["pageSize"], 0);
        assert_eq!(state["_target"], json!(target));
    }
    #[test]
    fn target_finish_never_returns_an_unrelated_owner() {
        let target = id(
            "DatasetDefinition",
            &json!({"SystemCode":"ICE","DataSetID":"D"}),
            &["SystemCode", "DataSetID"],
        )
        .unwrap();
        let other = id(
            "DatasetDefinition",
            &json!({"SystemCode":"ICE","DataSetID":"Other"}),
            &["SystemCode", "DataSetID"],
        )
        .unwrap();
        let s = json!({"_target":target});
        assert!(finish(vec![(other, complete(json!({}), vec![], vec![]))], None, &s).is_err());
    }

    #[test]
    fn wave1_symbols_do_not_satisfy_registered_assembly() {
        let identity=id("ExtensionSet",&json!({"ExtensionSetID":"E"}),&["ExtensionSetID"]).unwrap();
        let file=json!({"path":"retained.dll","sha256":"a".repeat(64),"bytes":2,"role":"nativeExport"});
        for (assembly,symbols,registered,partial) in [
            (Value::Null,json!("UEs="),true,true),
            (json!(""),json!("UEs="),true,true),
            (json!({"nativeFile":{ "path":"empty.dll","sha256":"a".repeat(64),"bytes":0,"role":"nativeExport"}}),json!({"nativeFile":file.clone()}),true,true),
            (json!("UEs="),Value::Null,true,false),
            (json!({"nativeFile":file}),Value::Null,true,false),
            (Value::Null,Value::Null,false,false),
        ] {
            let r=GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"C22".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(1),state:json!({"phase":"extension-bytes","binaryIndex":0,"prepare":true,"extensionRecords":[[identity,{"Extension":[{"Assembly":if registered {"Expected"}else{""}}]}]]}),response:Some(json!({"returnObj":{"Assembly":assembly,"Symbols":symbols}})),error:None};
            let GuestReply::PreparedPage{state,..}=handle(r) else {panic!()};
            let data=&state["records"][0][1];
            assert_eq!(data["herbertCollectionStatus"]["completeness"],if partial{"partial"}else{"collected"});
            if partial {assert_eq!(data["herbertCollectionStatus"]["diagnostics"][0]["code"],"EXTENSION_BYTES_UNAVAILABLE");}
            if symbols.is_string(){assert!(data["herbertSources"]["binaryFiles"].as_array().unwrap().iter().any(|f| f["name"].as_str().unwrap().ends_with(".pdb")));}
        }
    }

}

#[cfg(test)]
mod selection_tests {
    use super::*;
    fn request(state:Value)->GuestRequest{GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"C22".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state,response:None,error:None}}
    fn native_services()->Value{json!({"ZBODef":[{"SystemCode":"Ice","ClassName":" Ω%_ ","SystemFlag":false,"SysRowID":"a"},{"SystemCode":"Erp","ClassName":" Ω%_ ","SystemFlag":false,"SysRowID":"b"},{"SystemCode":"Ice","ClassName":"Other","SystemFlag":false,"SysRowID":"c"}],"ZBODataSets":[],"ZServiceLicenseType":[]})}
    #[test]
    fn native_variants_and_literal_selection_preserve_complete_output(){
        let all=records(&native_services(),true).unwrap();assert_eq!(all.len(),3);for prefix in [""," Ω%_ ","Ω","other"]{let state=json!({"_pluginSettings":{"definitionFamily":"services","definitionIdPrefix":prefix}});let (actual,errors)=select_records(all.clone(),&state).unwrap();assert!(errors.is_empty());assert_eq!(actual,all.iter().filter(|(id,_)|id.keys["ClassName"].starts_with(prefix)).cloned().collect::<Vec<_>>());}
        let mut bad=native_services();bad["ZBODef"][2]["ClassName"]=json!(false);assert!(records(&bad,true).is_err());
        let mut r=request(json!({"phase":"sdk-hidden","nativeKind":"services","nativeBody":bad,"_pluginSettings":{"definitionFamily":"externals","definitionIdPrefix":"no match"}}));r.error=Some(Diagnostic::new("DIRECT_ONLY","helper refused"));assert!(matches!(handle(r),GuestReply::Failure{..}));
    }
    #[test]
    fn excluded_helper_failure_survives_empty_prepared_independent_and_resumed_output(){
        for independent in [false,true]{let mut r=request(json!({"phase":"sdk-hidden","nativeKind":"services","nativeBody":native_services(),"prepare":true,"independent":independent,"_pluginSettings":{"definitionFamily":"datasets","definitionIdPrefix":"missing"}}));r.error=Some(Diagnostic::new("DIRECT_ONLY","helper refused"));let GuestReply::PreparedPage{state,next_state,..}=handle(r)else{panic!()};assert_eq!(next_state["_pluginSettings"]["definitionIdPrefix"],"missing");if !independent{assert!(state.get("_hostWorkItems").is_none());}let items=if independent{state["_hostWorkItems"].as_array().unwrap().clone()}else{vec![state]};assert!(!items.is_empty());for state in items{let mut r=request(state);r.operation="materialize".into();assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,..}if !diagnostics.is_empty()&&diagnostics.iter().all(|d|d.code=="DIRECT_ONLY")));}}
    }
    #[test]
    fn full_external_catalog_validates_before_selected_only_binary_reads(){
        let catalog=json!([{"Name":"A","FileName":"A.dll","Version":"1"},{"Name":"B","FileName":"B.dll","Version":"1"}]);for independent in [false,true]{let mut r=request(json!({"phase":"externals","prepare":true,"independent":independent,"_pluginSettings":{"definitionFamily":"externals","definitionIdPrefix":"A"}}));r.response=Some(json!({"returnObj":catalog}));let reply=handle(r.clone());let state=if independent{let GuestReply::PreparedPage{state,..}=reply else{panic!()};assert_eq!(state["_hostWorkItems"].as_array().unwrap().len(),1);state["_hostWorkItems"][0].clone()}else{let GuestReply::Action{action:HostAction::NativeRead{arguments,..},state}=reply else{panic!()};assert_eq!(arguments["assemblyId"],"A");state};r.operation="materialize".into();r.state=state;r.response=None;if independent{let GuestReply::Action{action:HostAction::NativeRead{arguments,..},state}=handle(r.clone())else{panic!()};assert_eq!(arguments["assemblyId"],"A");r.state=state;}r.response=Some(json!({"returnObj":[0,127,255]}));let reply=handle(r);let state=match reply{GuestReply::PreparedPage{state,..}=>state,GuestReply::ReadyRecords{records,state}=>{assert_eq!(records[0].1["herbertSources"]["binaryFiles"][0]["base64"],"AH//");state},other=>panic!("{other:?}")};assert_eq!(state["_pluginSettings"]["definitionIdPrefix"],"A");}
        let mut r=request(json!({"phase":"externals","_pluginSettings":{"definitionFamily":"externals","definitionIdPrefix":"A"}}));let mut bad=catalog;bad[1]["Version"]=json!(false);r.response=Some(json!({"returnObj":bad}));assert!(matches!(handle(r),GuestReply::Failure{..}));
    }
    #[test]
    fn exact_four_native_kinds_ignore_invalid_discovery_settings_across_initial_action(){
        for (kind,keys) in [("ServiceDefinition",json!({"SystemCode":"Ice","ClassName":"A"})),("DatasetDefinition",json!({"SystemCode":"Ice","DataSetID":"A"})),("ExtensionSet",json!({"ExtensionSetID":"A"})),("ExternalAssemblyReference",json!({"Name":"A"}))]{let identity=Identity{category:"C22".into(),kind:kind.into(),scope:Scope::Global,keys:serde_json::from_value(keys).unwrap()};let mut r=request(json!({"_pluginSettings":{"definitionFamily":"invalid","definitionIdPrefix":false}}));r.operation="acquire".into();r.identity=Some(identity.clone());let GuestReply::Action{state,..}=handle(r)else{panic!()};assert_eq!(state["_target"],json!(identity));assert!(selected(&identity,&state).unwrap());}
    }
}
