//! Business rules and mappings complete native pages and grouped, pure posting-table reads.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub const POSTING_TABLES: &[&str] = &[
    "ACTType",
    "ACTRevision",
    "ABTDocLine",
    "ABTAmount",
    "ABTPostEntity",
    "ABTPostCode",
    "ABTQuery",
    "ABTQParam",
    "ACTBook",
    "BookingRule",
    "BVRule",
    "BROperation",
    "BROperationCustom",
    "BRFunction",
    "BookVar",
    "BRFuncArgs",
    "BRFuncOperation",
];
const FORMS: &[&str] = &["IPForm", "IPFormBPAction", "IPFormCtrl", "IPFormCtrlValues"];
const STRUCTURED: &[&str] = &[
    "RptStructuredOutputDef",
    "RptStructuredOutputFile",
    "RptStructuredOutputFileElement",
    "RptStructuredOutputFileXMLNS",
];
fn error(message: &str) -> Diagnostic {
    Diagnostic::new("BUSINESS_PAGE_INVALID", message)
}
fn selection(state:&Value)->Result<(String,String),Diagnostic> {
    let settings=&state["_pluginSettings"];
    let family=match settings.get("definitionFamily"){None=>"all",Some(Value::String(value)) if matches!(value.as_str(),"all"|"forms"|"structuredOutput"|"postingRules")=>value,_=>return Err(error("Select a declared business definition family."))};
    let prefix=match settings.get("definitionIdPrefix"){None=>"",Some(Value::String(value))=>value,_=>return Err(error("The business definition prefix must be a string."))};Ok((family.into(),prefix.into()))
}
/// The prefix up to its first LIKE wildcard or bracket, for a server-side `LIKE 'stem%'`
/// superset that needs no escape syntax. Non-ASCII stops the stem because the literal is
/// not N-prefixed and could be converted lossily. None when nothing narrows the read.
fn like_stem(prefix: &str) -> Option<&str> {
    let stem = &prefix[..prefix.find(|c: char| matches!(c, '%' | '_' | '[' | ']') || !c.is_ascii()).unwrap_or(prefix.len())];
    (!stem.is_empty()).then_some(stem)
}
fn carry_state(mut state:Value,from:&Value)->Value {if let Some(value)=from.get("_pluginSettings"){if !state.is_object(){state=json!({});}state["_pluginSettings"]=value.clone();}state}
fn carry(reply:GuestReply,from:&Value)->GuestReply {match reply {
    GuestReply::Action {action,state}=>GuestReply::Action {action,state:carry_state(state,from)},
    GuestReply::ReadyRecords {records,state}=>GuestReply::ReadyRecords {records,state:carry_state(state,from)},
    GuestReply::PreparedPage {mut state,next_cursor,next_state}=>{if let Some(items)=state["_hostWorkItems"].as_array_mut(){for item in items {*item=carry_state(item.take(),from);}}GuestReply::PreparedPage {state:carry_state(state,from),next_cursor,next_state:carry_state(next_state,from)}},other=>other,
}}
#[cfg(test)]
thread_local!{static BUILDS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
fn build(kind:&str,data:&mut Value)->Result<(),Diagnostic>{#[cfg(test)]BUILDS.with(|n|n.set(n.get()+1));crate::business_archive::append(kind,data)}
// A family-excluded record is diagnosed exactly as append would, without building its file.
// Posting validation is the XML build itself, so it runs in full. Other kinds fail only
// on non-object data or a non-empty extension array, so append probes just that skeleton.
fn excluded_check(kind:&str,data:&Value)->Result<(),Diagnostic> {
    if kind=="PostingRules" {return build(kind,&mut data.clone());}
    let extensions=match data["herbertNativePageExtensions"].as_array(){Some(rows) if !rows.is_empty()=>json!([rows[0]]),_=>Value::Null};
    let mut probe=if data.is_object(){json!({"herbertNativePageExtensions":extensions})}else{Value::Null};
    crate::business_archive::append(kind,&mut probe)
}
pub(super) fn finish_selected(records:Vec<(Identity,Value)>,next:Option<String>,prepare:bool,independent:bool,state:&Value)->GuestReply {
    let exact=state["_target"].is_object();
    let (family,prefix)=if exact {("all".into(),String::new())}else{match selection(state){Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}}};
    let mut selected=Vec::new();let mut excluded_errors=Vec::new();
    for (id,mut data) in records {
        let (group,key)=match id.kind.as_str(){"BPMDataForm"=>("forms",Some("FormId")),"StructuredOutput"=>("structuredOutput",Some("RptStructuredOutputDefID")),_=>("postingRules",None)};
        let keep=family=="all"||family==group;
        // A prefix-unselected record is neither validated nor reported,
        // including its retained read errors. The exact match stays literal and case-sensitive.
        if keep&&!key.is_none_or(|key|id.keys.get(key).is_some_and(|value|value.starts_with(&prefix))){continue;}
        let mut errors:Vec<Diagnostic>=data.get("_c20ReadDiagnostics").cloned().map(|value|serde_json::from_value(value).unwrap_or_else(|_|vec![error("Saved read diagnostic is invalid.")])).unwrap_or_default();
        // Kept records build their recovery file once, in output(); family-excluded records are only checked.
        if errors.is_empty()&&!keep{if let Err(diagnostic)=excluded_check(&id.kind,&data){errors.push(diagnostic);}}
        for diagnostic in &mut errors {diagnostic.identity=Some(id.clone());}
        if keep {if !errors.is_empty(){data["_c20ReadDiagnostics"]=json!(errors);}selected.push((id,data));}else{excluded_errors.extend(errors);}
    }
    let mut reply=finish_independent(selected,next,prepare,independent);
    let output=match &mut reply {GuestReply::PreparedPage {state,..}=>Some(state),GuestReply::ReadyRecords {state,..}=>Some(state),GuestReply::Inventory {diagnostics,authoritative,..}=>{diagnostics.extend(excluded_errors.clone());*authoritative&=excluded_errors.is_empty();None},_=>None};
    if let Some(output)=output {
        if let Some(items)=output["_hostWorkItems"].as_array_mut(){if items.is_empty()&&!excluded_errors.is_empty(){items.push(json!({"phase":"output","records":[],"index":0,"next":null,"diagnostics":[]}));}if let Some(first)=items.first_mut(){first["diagnostics"].as_array_mut().unwrap().extend(excluded_errors.into_iter().map(|diagnostic|json!(diagnostic)));}}
        else if let Some(errors)=output["diagnostics"].as_array_mut(){errors.extend(excluded_errors.into_iter().map(|diagnostic|json!(diagnostic)));}
    }
    carry(reply,state)
}
pub fn methods() -> Vec<(String, String)> {
    vec![
        ("Ice.BO.InfoPromptFormSvc".into(), "GetRows".into()),
        (
            "Ice.BO.StructuredOutputDefinitionSvc".into(),
            "GetRows".into(),
        ),
        ("Ice.BO.DynamicQuerySvc".into(), "Execute".into()),
        ("Ice.BO.ZDataTableSvc".into(), "GetRows".into()),
    ]
}
pub(super) fn identity(kind: &str, company: &str, field: &str, value: String) -> Identity {
    Identity {
        category: "business-rules-and-mappings".into(),
        kind: kind.into(),
        scope: if company.is_empty() {
            Scope::Global
        } else {
            Scope::Company {
                company: company.into(),
            }
        },
        keys: BTreeMap::from([(field.into(), value)]),
    }
}
fn native_records(
    body: &Value,
    stream: u64,
    company: &str,
) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let list = if stream == 0 { FORMS } else { STRUCTURED };
    let field = if stream == 0 {
        "FormId"
    } else {
        "RptStructuredOutputDefID"
    };
    let kind = if stream == 0 {
        "BPMDataForm"
    } else {
        "StructuredOutput"
    };
    let mut result = BTreeMap::new();
    for row in body[list[0]]
        .as_array()
        .ok_or_else(|| error("Epicor page omitted its root table."))?
    {
        let id = row[field]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Epicor root omitted its identity."))?;
        let owner = if stream == 0 {
            ""
        } else {
            row["Company"]
                .as_str()
                .ok_or_else(|| error("Structured output omitted Company."))?
        };
        if !owner.is_empty() && owner != company {
            return Err(error("Epicor root belongs to another company."));
        }
        if stream == 1 && row["SystemFlag"] != false {
            return Err(error("Structured output returned a system definition."));
        }
        let mut data = json!({"ExtensionTables":[]});
        for t in list {
            data[*t] = json!([]);
        }
        data[list[0]] = json!([row]);
        if result
            .insert(
                id.to_ascii_lowercase(),
                (identity(kind, owner, field, id.into()), data),
            )
            .is_some()
        {
            return Err(error("Epicor page repeats a root identity."));
        }
    }
    for table in &list[1..] {
        let mut seen = BTreeSet::new();
        for row in body[*table]
            .as_array()
            .ok_or_else(|| error("Epicor page omitted a child table."))?
        {
            let key = row[field]
                .as_str()
                .ok_or_else(|| error("Epicor child omitted its owner."))?
                .to_ascii_lowercase();
            let (id, data) = result
                .get_mut(&key)
                .ok_or_else(|| error("Epicor child has no parent in this page."))?;
            if stream == 1 && row["Company"] != data[list[0]][0]["Company"] {
                return Err(error("Epicor child has foreign company ownership."));
            }
            let rid = row["SysRowID"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error("Epicor child omitted SysRowID."))?;
            if !seen.insert(rid.to_ascii_lowercase()) {
                return Err(error("Epicor child identity is duplicated."));
            }
            let _ = id;
            data[*table].as_array_mut().unwrap().push(row.clone());
        }
    }
    // Extension data carries native ownership rather than belonging to every root.
    if body["ExtensionTables"]
        .as_array()
        .is_some_and(|r| !r.is_empty())
    {
        for (_, data) in result.values_mut() {
            data["herbertNativePageExtensions"] = body["ExtensionTables"].clone();
        }
    }
    for (_, data) in result.values() {
        if stream == 0 {
            let controls: BTreeSet<_> = data["IPFormCtrl"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| r["ControlId"].as_str().map(str::to_ascii_lowercase))
                .collect();
            if controls.len() != data["IPFormCtrl"].as_array().unwrap().len()
                || data["IPFormCtrlValues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| {
                        r["ParentId"]
                            .as_str()
                            .is_none_or(|id| !controls.contains(&id.to_ascii_lowercase()))
                    })
            {
                return Err(error(
                    "A form repeats a control key or a control value has no parent control.",
                ));
            }
        } else {
            let files: BTreeSet<_> = data["RptStructuredOutputFile"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|r| r["RptStructuredOutputFileID"].as_str())
                .collect();
            if files.len() != data["RptStructuredOutputFile"].as_array().unwrap().len() {
                return Err(error("Structured output repeats a file key."));
            }
            for table in [
                "RptStructuredOutputFileElement",
                "RptStructuredOutputFileXMLNS",
            ] {
                if data[table].as_array().unwrap().iter().any(|r| {
                    r["RptStructuredOutputFileID"]
                        .as_str()
                        .is_none_or(|id| !files.contains(id))
                }) {
                    return Err(error("A structured output child has no parent file."));
                }
            }
            let mut parents = BTreeMap::new();
            for row in data["RptStructuredOutputFileElement"].as_array().unwrap() {
                let file = row["RptStructuredOutputFileID"].as_str().unwrap();
                let id = row["ElementID"]
                    .as_i64()
                    .ok_or_else(|| error("Structured element omitted integer ElementID."))?;
                let parent = row["ParentElementID"]
                    .as_i64()
                    .ok_or_else(|| error("Structured element omitted its parent ID."))?;
                if parents.insert((file, id), parent).is_some() {
                    return Err(error("Structured output repeats an element key."));
                }
            }
            for (&(file, id), _) in &parents {
                let mut current = id;
                let mut seen = BTreeSet::new();
                while current != 0 {
                    if !seen.insert(current) {
                        return Err(error("Structured output element hierarchy has a cycle."));
                    }
                    current = *parents.get(&(file, current)).ok_or_else(|| {
                        error("Structured output element has no parent in its file.")
                    })?;
                }
            }
        }
    }
    Ok(result.into_values().collect())
}
pub(super) fn finish(
    records: Vec<(Identity, Value)>,
    next: Option<String>,
    prepare: bool,
) -> GuestReply {
    let state = json!({"phase":"output","records":records,"index":0,"next":next,"diagnostics":[]});
    if prepare {
        GuestReply::PreparedPage {
            state,
            next_cursor: next,
            next_state: json!({}),
        }
    } else {
        output(state)
    }
}
pub(super) fn finish_independent(
    records: Vec<(Identity, Value)>,
    next: Option<String>,
    prepare: bool,
    independent: bool,
) -> GuestReply {
    // The host advertises this optional prepared-page protocol. Older hosts
    // continue to use the ordinary ABI-1 continuation without losing data.
    if !prepare || !independent {
        return finish(records, next, prepare);
    }
    let count = records.len();
    let items: Vec<_> = records.into_iter().map(|record|
        json!({"phase":"output","records":[record],"index":0,"next":null,"diagnostics":[]})).collect();
    GuestReply::PreparedPage {
        state: json!({"_hostWorkItems":items,"_hostWorkNextCursor":next,"_progress":{"pageRecords":count}}),
        next_cursor: next,
        next_state: json!({}),
    }
}
fn output(mut state: Value) -> GuestReply {
    let Some(i) = state["index"].as_u64().map(|v| v as usize) else {
        return GuestReply::Failure {
            diagnostic: error("Herbert could not continue this download from its saved progress (output position). Start the download again."),
        };
    };
    let Some(entries) = state["records"].as_array() else {
        return GuestReply::Failure {
            diagnostic: error("Herbert could not continue this download from its saved progress (prepared output). Start the download again."),
        };
    };
    if i == entries.len() {
        let identities = entries
            .iter()
            .filter_map(|r| serde_json::from_value(r[0].clone()).ok())
            .collect();
        return GuestReply::Inventory {
            identities,
            next_cursor: state["next"].as_str().map(str::to_owned),
            authoritative: state["diagnostics"].as_array().is_some_and(Vec::is_empty),
            diagnostics: serde_json::from_value(state["diagnostics"].take()).unwrap_or_default(),
        };
    }
    let Some(entry) = entries.get(i) else {
        return GuestReply::Failure {
            diagnostic: error("Herbert could not continue this download from its saved progress (output position). Start the download again."),
        };
    };
    let id: Identity = match serde_json::from_value(entry[0].clone()) {
        Ok(v) => v,
        Err(_) => {
            return GuestReply::Failure {
                diagnostic: error("Herbert could not continue this download from its saved progress (output item). Start the download again."),
            };
        }
    };
    let mut data = state["records"][i][1].take();
    let mut diagnostics = vec![];
    let retained_diagnostics = data
        .as_object_mut()
        .and_then(|o| o.remove("_c20ReadDiagnostics"));
    if let Some(saved) = retained_diagnostics {
        diagnostics = serde_json::from_value(saved)
            .unwrap_or_else(|_| vec![error("Saved read diagnostic is invalid.")]);
    }
    if diagnostics.is_empty() {
        if let Err(mut d) = build(&id.kind, &mut data) {
            d.identity = Some(id.clone());
            diagnostics.push(d);
        }
    }
    let completeness = if diagnostics.is_empty() {
        Completeness::Collected
    } else {
        Completeness::Partial
    };
    data["herbertDirectOutput"] = json!(true);
    data["herbertCollectionStatus"] =
        json!({"completeness":completeness,"diagnostics":diagnostics});
    state["diagnostics"]
        .as_array_mut()
        .unwrap()
        .extend(diagnostics.iter().map(|d| json!(d)));
    state["records"][i][1] = Value::Null;
    state["index"] = json!(i + 1);
    GuestReply::ReadyRecords {
        records: vec![(id, data)],
        state,
    }
}
fn drive(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if r.state["phase"] == "output" {
        return Ok(output(r.state));
    }
    if let Some(d) = r.error.take() {
        return Err(d);
    }
    let cursor: Value = r
        .cursor
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| error("Herbert could not continue this download from its saved progress (business rules position). Start the download again."))?
        .unwrap_or(json!({"stream":0,"page":1}));
    let stream = cursor["stream"]
        .as_u64()
        .filter(|n| *n <= 2)
        .ok_or_else(|| error("Herbert could not continue this download from its saved progress (business rules section). Start the download again."))?;
    let page = cursor["page"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= i32::MAX as u64)
        .ok_or_else(|| error("Herbert could not continue this download from its saved progress (page position). Start the download again."))?;
    let size = r
        .page_size
        .filter(|n| *n > 0 && *n <= i32::MAX as u32)
        .ok_or_else(|| error("Page size must be a whole number from 1 to 2147483647."))?;
    // Bulk-first: an unselected family's source is not read at all.
    if r.response.is_none() && !r.state["_target"].is_object() {
        let (family, _) = selection(&r.state)?;
        if family != "all" && family != ["forms", "structuredOutput", "postingRules"][stream as usize] {
            return Ok(GuestReply::Inventory {
                identities: vec![],
                next_cursor: (stream < 2).then(|| json!({"stream":stream+1,"page":1}).to_string()),
                authoritative: true,
                diagnostics: vec![],
            });
        }
    }
    if stream == 2 {
        return crate::business_graph::drive(r);
    }
    if stream < 2 {
        if r.response.is_none() {
            let mut args = json!({"pageSize":size,"absolutePage":page});
            let list = if stream == 0 { FORMS } else { STRUCTURED };
            for t in list {
                args[format!("whereClause{t}")] = json!(if stream == 1 && *t == STRUCTURED[0] {
                    format!(
                        "SystemFlag = false AND (Company = '{}' OR Company = '')",
                        r.company.replace('\'', "''")
                    )
                } else {
                    String::new()
                });
            }
            let field = if stream == 0 { "FormId" } else { "RptStructuredOutputDefID" };
            let root = format!("whereClause{}", list[0]);
            if let Some(key) = r.state["_target"]["keys"]
                .as_object()
                .and_then(|k| k.values().next())
                .and_then(Value::as_str)
            {
                let filter = args[&root].as_str().unwrap_or("");
                let exact = format!("{field} = '{}'", key.replace('\'', "''"));
                args[&root] = json!(if filter.is_empty() {
                    exact
                } else {
                    format!("({filter}) AND ({exact})")
                });
            } else if let Some(stem) = like_stem(&selection(&r.state)?.1) {
                // A broad server-side superset of the prefix; the exact,
                // case-sensitive starts_with in finish_selected still decides selection.
                let filter = args[&root].as_str().unwrap_or("");
                let like = format!("{field} LIKE '{}%'", stem.replace('\'', "''"));
                args[&root] = json!(if filter.is_empty() { like } else { format!("{filter} AND {like}") });
            }
            return Ok(crate::native(
                if stream == 0 {
                    "Ice.BO.InfoPromptFormSvc"
                } else {
                    "Ice.BO.StructuredOutputDefinitionSvc"
                },
                "GetRows",
                args,
                false,
                json!({"phase":"native"}),
            ));
        }
        let response = r.response.unwrap();
        let more = response
            .pointer("/parameters/morePages")
            .and_then(Value::as_bool)
            .ok_or_else(|| error("Epicor page omitted morePages."))?;
        let records = native_records(crate::body(&response), stream, &r.company)?;
        if more && records.is_empty() {
            return Err(error("Epicor page did not advance."));
        }
        let next = Some(
            if more {
                json!({"stream":stream,"page":page+1})
            } else {
                json!({"stream":stream+1,"page":1})
            }
            .to_string(),
        );
        return Ok(finish_selected(
            records,
            next,
            r.state["_hostPreparePage"] == true,
            r.state["_hostIndependentWorkItems"] == true,
            &r.state,
        ));
    }
    Err(error("Herbert could not continue this download from its saved progress (business rules section). Start the download again."))
}

pub fn handle(r: GuestRequest) -> GuestReply {
    let from=r.state.clone();if r.operation!="acquire" {if let Err(diagnostic)=selection(&r.state){return GuestReply::Failure {diagnostic};}}
    carry(handle_inner(r),&from)
}
fn handle_inner(r: GuestRequest) -> GuestReply {
    if r.operation == "acquire" {
        return targeted(r);
    }
    if !matches!(r.operation.as_str(), "discover" | "materialize") {
        return crate::collectors::handle(r);
    }
    if r.state["phase"] == "output" {
        // Read-error fallback needs a source snapshot; local output does not.
        return output(r.state);
    }
    let next = r
        .cursor
        .as_deref()
        .and_then(|c| serde_json::from_str::<Value>(c).ok())
        .unwrap_or(json!({"stream":0}))["stream"]
        .as_u64()
        .filter(|s| *s < 2)
        .map(|s| json!({"stream":s+1,"page":1}).to_string());
    let retained = r.state.clone();
    match drive(r) {
        Ok(v) => v,
        Err(d)
            if retained["records"]
                .as_array()
                .is_some_and(|r| !r.is_empty()) =>
        {
            let mut records = retained["records"].clone();
            for entry in records.as_array_mut().unwrap() {
                entry[1]["_c20ReadDiagnostics"] = json!([d]);
            }
            match serde_json::from_value(records) {
                Ok(records)=>finish_selected(records,next,true,retained["_hostIndependentWorkItems"]==true,&retained),
                Err(_)=>GuestReply::Failure {diagnostic:error("Invalid retained business records.")},
            }
        }
        Err(d) => GuestReply::Inventory {
            identities: vec![],
            next_cursor: next,
            authoritative: false,
            diagnostics: vec![d],
        },
    }
}
fn targeted(mut r: GuestRequest) -> GuestReply {
    let Some(id) = r.identity.clone() else {
        return crate::failure("IDENTITY_REQUIRED", "Choose one business rule or mapping to download.");
    };
    let (stream, field) = match id.kind.as_str() {
        "BPMDataForm" => (0, "FormId"),
        "StructuredOutput" => (1, "RptStructuredOutputDefID"),
        "PostingRules" => (2, "ACTTypeUID"),
        _ => return crate::failure("INVALID_IDENTITY", "The selected item is not a business rule or mapping this plugin can download."),
    };
    let scope_ok = matches!(&id.scope, Scope::Global) && stream < 2
        || matches!(&id.scope,Scope::Company{company} if company==&r.company) && stream > 0;
    if id.category != "business-rules-and-mappings"
        || !scope_ok
        || id.keys.len() != 1
        || id.keys.get(field).is_none_or(|s| s.is_empty())
    {
        return crate::failure(
            "INVALID_IDENTITY",
            "The selected business rule or mapping belongs to a different company than the one selected. Choose the item from the selected company.",
        );
    }
    if !r.state.is_object() {
        r.state = json!({});
    }
    r.state["_target"] = json!(id);
    r.state["_hostIndependentWorkItems"] = json!(false);
    r.state["_hostPreparePage"] = json!(true);
    r.operation = "discover".into();
    r.cursor = Some(json!({"stream":stream,"page":1}).to_string());
    let reply = match drive(r) {
        Ok(v) => v,
        Err(d) => return GuestReply::Failure { diagnostic: d },
    };
    let reply = if let GuestReply::PreparedPage { state, .. } = reply {
        output(state)
    } else {
        reply
    };
    match reply {
        GuestReply::ReadyRecords { mut records, .. }
            if records.len() == 1 && records[0].0 == id =>
        {
            let (_, mut data) = records.remove(0);
            let status = data["herbertCollectionStatus"].clone();
            data.as_object_mut().unwrap().remove("herbertDirectOutput");
            data.as_object_mut()
                .unwrap()
                .remove("herbertCollectionStatus");
            GuestReply::Definition {
                identity: id,
                format: "native-recovery-definition".into(),
                definition: data,
                completeness: serde_json::from_value(status["completeness"].clone())
                    .unwrap_or(Completeness::Partial),
                diagnostics: serde_json::from_value(status["diagnostics"].clone())
                    .unwrap_or_default(),
            }
        }
        GuestReply::Inventory { diagnostics, .. } if !diagnostics.is_empty() => {
            GuestReply::Failure {
                diagnostic: diagnostics[0].clone(),
            }
        }
        GuestReply::Inventory { .. } => crate::failure(
            "CUSTOM_ARTIFACT_NOT_FOUND",
            "Epicor did not return the selected business rule or mapping. It may have been deleted, or this account may not be allowed to read it.",
        ),
        GuestReply::Action { action, state } => GuestReply::Action { action, state },
        other => {
            let diagnostic = Diagnostic::new(
                "BUSINESS_TARGET_INVALID",
                "Epicor did not return the selected business rule or mapping in the expected form. Retry the download; if it fails again, download the whole category instead.",
            );
            GuestReply::Failure {
                diagnostic: match other {
                    GuestReply::Failure { diagnostic: inner } => diagnostic
                        .with_context("innerCode", inner.code)
                        .with_context("innerMessage", inner.message),
                    _ => diagnostic,
                },
            }
        }
    }
}
#[cfg(test)]
#[path = "business_tests.rs"]
mod tests;
#[cfg(test)]
mod bulk_first_tests {
    use super::*;
    #[test]
    fn unselected_family_sources_make_no_request() {
        for (family, read) in [("all", [true, true, true]), ("forms", [true, false, false]), ("structuredOutput", [false, true, false]), ("postingRules", [false, false, true])] {
            for stream in 0..3u64 {
                let r = GuestRequest { abi: PLUGIN_ABI, operation: "discover".into(), category: "business-rules-and-mappings".into(), company: "C".into(), identity: None,
                    cursor: Some(json!({"stream":stream,"page":1}).to_string()), page_size: Some(100),
                    state: json!({"_pluginSettings":{"definitionFamily":family,"definitionIdPrefix":""}}), response: None, error: None };
                match handle(r) {
                    GuestReply::Inventory { identities, next_cursor, authoritative: true, diagnostics } => {
                        assert!(!read[stream as usize] && identities.is_empty() && diagnostics.is_empty(), "{family}/{stream}");
                        assert_eq!(next_cursor.map(|cursor| serde_json::from_str::<Value>(&cursor).unwrap()), (stream < 2).then(|| json!({"stream":stream+1,"page":1})));
                    }
                    reply => assert!(read[stream as usize], "{family} read stream {stream}: {reply:?}"),
                }
            }
        }
    }
    #[test]
    fn recovery_files_are_built_once_per_kept_record_and_excluded_diagnostics_match_append() {
        let form = |id: &str, extensions: bool| (identity("BPMDataForm", "", "FormId", id.into()),
            json!({"IPForm":[{"FormId":id}],"IPFormBPAction":[],"IPFormCtrl":[],"IPFormCtrlValues":[],"ExtensionTables":[],"herbertNativePageExtensions":if extensions {json!([{"x":"EXCLUDED_PAYLOAD"}])} else {json!([])}}));
        let records = vec![form("A", false), form("AExt", true), form("B", true), form("C", false)];
        let mut expected = crate::business_archive::append("BPMDataForm", &mut records[2].1.clone()).unwrap_err();
        expected.identity = Some(records[2].0.clone());
        let state = json!({"_pluginSettings":{"definitionFamily":"forms","definitionIdPrefix":"A"}});
        BUILDS.with(|n| n.set(0));
        let GuestReply::PreparedPage { state: prepared, .. } = finish_selected(records, None, true, true, &state) else { panic!() };
        assert_eq!(BUILDS.with(|n| n.get()), 0, "Selection builds no recovery file");
        let (mut kept, mut diagnostics) = (vec![], vec![]);
        for mut item in prepared["_hostWorkItems"].as_array().unwrap().clone() {
            loop { match output(item) {
                GuestReply::ReadyRecords { records, state } => { kept.extend(records); item = state; }
                GuestReply::Inventory { diagnostics: found, .. } => { diagnostics.extend(found); break; }
                other => panic!("{other:?}"),
            } }
        }
        assert_eq!(BUILDS.with(|n| n.get()), 2, "One build per kept record");
        assert_eq!(kept.iter().map(|(id, _)| id.keys["FormId"].as_str()).collect::<Vec<_>>(), ["A", "AExt"]);
        assert_eq!(kept[0].1["herbertCollectionStatus"]["completeness"], "collected");
        assert!(kept[0].1["herbertSources"]["files"].is_array());
        assert_eq!(kept[1].1["herbertCollectionStatus"]["completeness"], "partial");
        assert!(kept[1].1["herbertSources"].is_null());
        assert_eq!(kept[1].1["herbertCollectionStatus"]["diagnostics"][0]["identity"]["keys"]["FormId"], "AExt");
        assert_eq!(diagnostics.len(), 1, "A prefix-excluded record is not validated or reported: {diagnostics:?}");
        assert!(!diagnostics.contains(&expected), "{diagnostics:?}");
        assert_eq!(diagnostics[0].identity.as_ref().unwrap().keys["FormId"], "AExt");
        for kind in ["BPMDataForm", "StructuredOutput", "Other", "PostingRules"] {
            for value in [json!({}), json!({"herbertNativePageExtensions":[]}), json!({"herbertNativePageExtensions":[{"x":1},{"y":2}]}),
                json!({"herbertNativePageExtensions":{"x":1}}), json!({"IPForm":[{"FormId":"A"}]}), Value::Null, json!([1]), json!("text")] {
                assert_eq!(excluded_check(kind, &value), crate::business_archive::append(kind, &mut value.clone()), "{kind} {value}");
            }
        }
    }
    fn request(stream: u64, family: &str, prefix: &str) -> GuestRequest {
        GuestRequest { abi: PLUGIN_ABI, operation: "discover".into(), category: "business-rules-and-mappings".into(), company: "C'o".into(), identity: None,
            cursor: Some(json!({"stream":stream,"page":1}).to_string()), page_size: Some(100),
            state: json!({"_pluginSettings":{"definitionFamily":family,"definitionIdPrefix":prefix}}), response: None, error: None }
    }
    fn arguments(reply: GuestReply) -> Value {
        match reply { GuestReply::Action { action: HostAction::NativeRead { arguments, .. }, .. } => arguments, other => panic!("{other:?}") }
    }
    #[test]
    fn discovery_reads_carry_a_prefix_like_superset_and_targets_stay_exact() {
        let base = "SystemFlag = false AND (Company = 'C''o' OR Company = '')";
        for family in ["all", "forms"] {
            let args = arguments(handle(request(0, family, "Ab'c")));
            assert_eq!(args["whereClauseIPForm"], "FormId LIKE 'Ab''c%'");
            for child in &FORMS[1..] { assert_eq!(args[format!("whereClause{child}")], "", "{child}"); }
        }
        assert_eq!(arguments(handle(request(0, "forms", "AbΩc")))["whereClauseIPForm"], "FormId LIKE 'Ab%'");
        for family in ["all", "structuredOutput"] {
            let args = arguments(handle(request(1, family, "Q'r_1")));
            assert_eq!(args["whereClauseRptStructuredOutputDef"], format!("{base} AND RptStructuredOutputDefID LIKE 'Q''r%'"));
            for child in &STRUCTURED[1..] { assert_eq!(args[format!("whereClause{child}")], "", "{child}"); }
        }
        for prefix in ["", "%x", "_x", "[a", "]a", "Ωa"] {
            assert_eq!(arguments(handle(request(0, "all", prefix)))["whereClauseIPForm"], "", "{prefix}");
            assert_eq!(arguments(handle(request(1, "all", prefix)))["whereClauseRptStructuredOutputDef"], base, "{prefix}");
        }
        for (kind, field, expected) in [("BPMDataForm", "FormId", "FormId = 'A''B'".to_string()),
            ("StructuredOutput", "RptStructuredOutputDefID", format!("({base}) AND (RptStructuredOutputDefID = 'A''B')"))] {
            let mut r = request(0, "all", "Zzz");
            r.operation = "acquire".into(); r.cursor = None; r.identity = Some(identity(kind, "", field, "A'B".into()));
            let args = arguments(handle(r));
            let root = if kind == "BPMDataForm" { "whereClauseIPForm" } else { "whereClauseRptStructuredOutputDef" };
            assert_eq!(args[root], expected.as_str(), "{kind}");
        }
    }
    #[test]
    fn prefix_excluded_records_and_retained_read_errors_stay_silent_and_pages_continue() {
        let body = json!({"IPForm":[{"FormId":"A"},{"FormId":"aZ"},{"FormId":"B"}],"IPFormBPAction":[],"IPFormCtrl":[],"IPFormCtrlValues":[]});
        for independent in [false, true] {
            let mut r = request(0, "forms", "A");
            r.state["_hostPreparePage"] = json!(true); r.state["_hostIndependentWorkItems"] = json!(independent);
            r.response = Some(json!({"returnObj":body,"parameters":{"morePages":true}}));
            let GuestReply::PreparedPage { state, next_cursor, .. } = handle(r.clone()) else { panic!() };
            assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap(), json!({"stream":0,"page":2}));
            let kept = if independent { state["_hostWorkItems"].as_array().unwrap().iter().map(|item| item["records"][0][0]["keys"]["FormId"].clone()).collect::<Vec<_>>() }
                else { state["records"].as_array().unwrap().iter().map(|record| record[0]["keys"]["FormId"].clone()).collect() };
            assert_eq!(kept, [json!("A")], "the local match stays case-sensitive");
            // An empty selected page still advances, here to the next stream.
            r.state["_pluginSettings"]["definitionIdPrefix"] = json!("Missing");
            r.response = Some(json!({"returnObj":body,"parameters":{"morePages":false}}));
            let GuestReply::PreparedPage { state, next_cursor, .. } = handle(r) else { panic!() };
            assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap(), json!({"stream":1,"page":1}));
            if independent { assert!(state["_hostWorkItems"].as_array().unwrap().is_empty()); }
            else { assert!(matches!(output(state), GuestReply::Inventory { authoritative: true, ref identities, ref diagnostics, .. } if identities.is_empty() && diagnostics.is_empty())); }
        }
        // Retained read errors on prefix-unselected rows are not reported; a selected row keeps its error.
        let form = |id: &str| (identity("BPMDataForm", "", "FormId", id.into()), json!({"IPForm":[{"FormId":id}],"IPFormBPAction":[],"IPFormCtrl":[],"IPFormCtrlValues":[],"ExtensionTables":[],"herbertNativePageExtensions":[{"x":1}]}));
        for (prefix, reported) in [("Missing", 0), ("A", 1)] {
            let mut r = request(0, "forms", prefix);
            r.state["records"] = json!([form("A"), form("B")]); r.error = Some(Diagnostic::new("DENIED", "read failed"));
            let GuestReply::PreparedPage { state, .. } = handle(r) else { panic!() };
            let mut item = state;
            let found = loop { match output(item) { GuestReply::ReadyRecords { state, .. } => item = state, GuestReply::Inventory { diagnostics, authoritative, .. } => { assert_eq!(authoritative, reported == 0); break diagnostics; } other => panic!("{other:?}") } };
            assert_eq!(found.len(), reported, "{prefix}: {found:?}");
            assert!(found.iter().all(|d| d.code == "DENIED" && d.identity.as_ref().unwrap().keys["FormId"] == "A"));
        }
    }
}
