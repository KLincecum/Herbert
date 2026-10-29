//! Collect full native RDD and BAQ-report pages. Build XML locally so collection
//! does not create server files or repeat an export call for each definition.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn family(stream: u64) -> (&'static str, &'static str, &'static str, &'static str) {
    match stream {
        0 => (
            "RptDataDef",
            "RptDataDef",
            "RptDefID",
            "ReportDataDefinition",
        ),
        1 => ("DynamicReport", "BAQReport", "BAQRptID", "BAQReport"),
        _ => unreachable!(),
    }
}
pub fn methods() -> Vec<(String, String)> {
    ["RptDataDef", "DynamicReport"]
        .into_iter()
        .map(|name| (format!("Ice.BO.{name}Svc"), "GetRows".into()))
        .collect()
}
fn failure(message: &str) -> GuestReply {
    crate::failure("REPORT_PAGE_INVALID", message)
}
fn selection(request: &GuestRequest) -> Result<(String,String),Diagnostic> {
    let family=match request.state.pointer("/_pluginSettings/definitionFamily") {
        None=>"reportDataAndBaqReports".to_owned(),Some(Value::String(value)) if matches!(value.as_str(),"reportDataAndBaqReports"|"reportData"|"baqReports")=>value.clone(),
        _=>return Err(Diagnostic::new("INVALID_SETTING","Select a declared report definition family.")),
    };
    Ok((family,crate::report_archive::prefix(request,"definitionIdPrefix")?))
}
/// The prefix's leading run before any LIKE wildcard, bracket or non-ASCII character, for a
/// server-side `LIKE 'stem%'` superset that needs no escape syntax. Non-ASCII stops the stem
/// because the literal is not N-prefixed and could be converted lossily. None when nothing
/// narrows the read.
fn like_stem(prefix: &str) -> Option<&str> {
    let stem = &prefix[..prefix.find(|c: char| matches!(c, '%' | '_' | '[' | ']') || !c.is_ascii()).unwrap_or(prefix.len())];
    (!stem.is_empty()).then_some(stem)
}
fn records(
    stream: u64,
    body: &Value,
    company: &str,
) -> Result<Vec<(Identity, Value)>, String> {
    let (service, root, key, kind) = family(stream);
    let tables = crate::report_archive::schema(service);
    let roots = body[root]
        .as_array()
        .ok_or("The report page omitted its parent table.")?;
    let mut groups = BTreeMap::new();
    for row in roots {
        let id = row[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("A report has no native identity.")?;
        let owner = row["Company"]
            .as_str()
            .ok_or("A report has no company scope.")?;
        if !owner.is_empty() && owner != company {
            return Err("A report belongs to another company.".into());
        }
        if row["SystemFlag"] != false || (stream == 0 && row["SystemRpt"] != false) {
            return Err("The custom report page contains a system definition.".into());
        }
        let identity = Identity {
            category: "report-definitions".into(),
            kind: kind.into(),
            scope: if owner.is_empty() {
                Scope::Global
            } else {
                Scope::Company {
                    company: owner.into(),
                }
            },
            keys: BTreeMap::from([(key.into(), id.into())]),
        };
        let mut data = json!({});
        for table in tables.keys() {
            data[table] = json!([]);
        }
        data[root] = json!([row]);
        if groups.insert(id.to_owned(), (identity, data)).is_some() {
            return Err("A report page repeats its parent identity.".into());
        }
    }
    for table in tables.keys().filter(|t| t.as_str() != root && t.as_str() != "RptDataSourceField") {
        let rows = body[table]
            .as_array()
            .ok_or_else(|| format!("Report page omitted table {table}."))?;
        let mut seen = BTreeSet::new();
        let mut parameter_keys = BTreeMap::<String, &Value>::new();
        for row in rows {
            let parent = row[key]
                .as_str()
                .ok_or_else(|| format!("Table {table} has a row without parent key {key}."))?;
            let id = row["SysRowID"]
                .as_str()
                .ok_or_else(|| format!("Table {table} has a row without SysRowID."))?;
            // Parameters are transient native projections. Their composite key,
            // not SysRowID, establishes ownership; identical occurrences are retained.
            if table != "RptDataSourceParameter" && !seen.insert(id) {
                return Err(format!("Table {table} repeats row {id}."));
            }
            if table == "RptDataSourceParameter" {
                let fields = ["RptDefID","RptCriteriaSetID","RptTableID","ParameterID"];
                let values = fields.iter().map(|field| row[*field].as_str())
                    .collect::<Option<Vec<_>>>().ok_or("Report parameter omitted a native key field.")?;
                let key = serde_json::to_string(&values).unwrap();
                if parameter_keys.insert(key, row).is_some_and(|previous| previous != row) {
                    return Err("Report parameter repeats its native key with different content.".into());
                }
            }
            let (_, data) = groups
                .get_mut(parent)
                .ok_or_else(|| format!("Table {table} references {key} {parent}, which is absent from this page."))?;
            data[table].as_array_mut().unwrap().push(row.clone());
        }
    }
    if stream == 0 {
        distribute_query_fields(body, &mut groups)?;
    }
    Ok(groups.into_values().collect())
}

/// Native SetBAQData creates one field set for each RptTable query reference.
/// These transient rows have QueryID, not RptDefID. Allocate each identical
/// field occurrence to its report through RptTable; do not copy the whole page
/// into every report or depend on native row order. Keep the original row IDs.
fn distribute_query_fields(body: &Value, groups: &mut BTreeMap<String, (Identity, Value)>) -> Result<(), String> {
    let fields = body["RptDataSourceField"].as_array()
        .ok_or("Report page omitted table RptDataSourceField.")?;
    let mut references: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for table in body["RptTable"].as_array().ok_or("Report page omitted table RptTable.")? {
        if let Some(query) = table["QueryID"].as_str().filter(|q| !q.is_empty()) {
            if table["IsValidDataSource"] == true {
                let parent = table["RptDefID"].as_str().ok_or("RptTable omitted RptDefID.")?;
                references.entry(query.to_lowercase()).or_default().push(parent.into());
            }
        }
    }
    let mut occurrences: BTreeMap<(String, String), Vec<&Value>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for row in fields {
        let query = row["QueryID"].as_str().filter(|q| !q.is_empty())
            .ok_or("RptDataSourceField has a row without QueryID.")?;
        let id = row["SysRowID"].as_str().ok_or("RptDataSourceField has a row without SysRowID.")?;
        if !seen.insert(id) { return Err(format!("RptDataSourceField repeats row {id}.")); }
        let mut content = row.clone();
        content.as_object_mut().unwrap().remove("SysRowID");
        occurrences.entry((query.to_lowercase(), content.to_string())).or_default().push(row);
    }
    for ((query, _), rows) in occurrences {
        let parents = references.get(&query).filter(|p| !p.is_empty())
            .ok_or_else(|| format!("RptDataSourceField query {query} has no valid RptTable reference in this page."))?;
        if rows.len() % parents.len() != 0 {
            return Err(format!("RptDataSourceField query {query} has {} matching field rows for {} table references; ownership cannot be reconciled.", rows.len(), parents.len()));
        }
        for (parent, rows) in parents.iter().zip(rows.chunks(rows.len() / parents.len())) {
            let (_, data) = groups.get_mut(parent).ok_or_else(|| format!("Query {query} references missing report {parent}."))?;
            data["RptDataSourceField"].as_array_mut().unwrap().extend(rows.iter().map(|r| (*r).clone()));
        }
    }
    Ok(())
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.operation == "materialize" {
        return materialize(request);
    }
    if !matches!(request.operation.as_str(), "discover" | "acquire") {
        return failure("Reports support discovery and exact collection.");
    }
    let exact = request.operation == "acquire";
    let (selected_family,prefix)=if exact {("reportDataAndBaqReports".into(),String::new())}else {match selection(&request){Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}}};
    if exact
        && request.identity.as_ref().is_none_or(|id| {
            id.category != "report-definitions"
                || id.keys.len() != 1
                || !match &id.scope {
                    Scope::Global => true,
                    Scope::Company { company } => company == &request.company,
                    _ => false,
                }
        })
    {
        return failure("Select a report identity in this company or global scope.");
    }
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(c) => c.unwrap_or(json!({"stream":0,"page":1})),
        Err(_) => return failure("The report resume point is invalid."),
    };
    let stream = if exact {
        match request.identity.as_ref().map(|id| id.kind.as_str()) {
            Some("ReportDataDefinition") => 0,
            Some("BAQReport") => 1,
            _ => return failure("Select an Epicor report definition identity."),
        }
    } else {
        match cursor["stream"].as_u64() {
            Some(v @ 0..=1) => v,
            _ => return failure("The report source is invalid."),
        }
    };
    let (service, root, key, _) = family(stream);
    let family_selected = exact || selected_family=="reportDataAndBaqReports" || selected_family==if stream==0 {"reportData"}else{"baqReports"};
    let next = |more: bool| {
        if exact {
            None
        } else if more {
            Some(json!({"stream":stream,"page":cursor["page"].as_u64().unwrap_or(1)+1}).to_string())
        } else if stream == 0 {
            Some(json!({"stream":1,"page":1}).to_string())
        } else {
            None
        }
    };
    if let Some(error) = request.error.take() {
        return if exact {
            GuestReply::Failure { diagnostic: error }
        } else {
            GuestReply::Inventory {
                identities: vec![],
                next_cursor: next(false),
                authoritative: false,
                diagnostics: vec![error],
            }
        };
    }
    if let Some(response) = &request.response {
        let Some(more) = response
            .pointer("/parameters/morePages")
            .and_then(Value::as_bool)
        else {
            return failure("The report page omitted its resume point flag.");
        };
        let mut rows = match records(stream, crate::body(response), &request.company) {
            Ok(v) => v,
            Err(e) => return failure(&e),
        };
        if more && rows.is_empty() {
            return failure("The report page did not advance.");
        }
        if exact && (rows.len() != 1 || rows[0].0 != *request.identity.as_ref().unwrap()) {
            return failure("The exact report identity was not returned.");
        }
        // The server filter is a superset; the literal, case-sensitive prefix is decided here.
        // Only kept definitions are validated, so excluded ones produce no diagnostics.
        if !exact {rows.retain(|(id,_)|family_selected && id.keys[key].starts_with(&prefix));}
        let mut diagnostics=Vec::new();
        for (identity,data) in &rows {
            diagnostics.extend(crate::report_archive::graph_errors(identity,data));
            if let Err(mut error)=crate::report_archive::xml(service,data) {error.identity=Some(identity.clone());diagnostics.push(error);}
        }
        let state = crate::report_archive::with_settings(json!({"stream":stream,"records":rows,"next":next(more),"exact":exact,"diagnostics":diagnostics}),&request.state);
        if !exact && request.state["_hostPreparePage"] == true {
            return GuestReply::PreparedPage {
                next_cursor: next(more),
                next_state: crate::report_archive::with_settings(json!({}),&request.state),
                state,
            };
        }
        request.state = state;
        return materialize(request);
    }
    // Bulk-first: an unselected family's source is not read at all.
    if !family_selected {
        return GuestReply::Inventory {identities:vec![],next_cursor:next(false),authoritative:true,diagnostics:vec![]};
    }
    let page = cursor["page"].as_u64().unwrap_or(1);
    let size = request.page_size.unwrap_or(50);
    if size == 0 || size > i32::MAX as u32 || page == 0 || page > i32::MAX as u64 {
        return failure("The report page number and page size must each be a whole number from 1 to 2147483647.");
    }
    let mut args = json!({"pageSize":size,"absolutePage":if exact{1}else{page}});
    for table in crate::report_archive::schema(service).keys() {
        args[format!("whereClause{table}")] = json!("");
    }
    let mut filter = if stream == 0 {
        "SystemFlag = false AND SystemRpt = false".to_owned()
    } else {
        "SystemFlag = false".to_owned()
    };
    if exact {
        let id = request.identity.as_ref().unwrap();
        let Some(value) = id.keys.get(key) else {
            return failure("The report identity omitted its Epicor key.");
        };
        filter.push_str(&format!(" AND {key} = '{}'", value.replace('\'', "''")));
    } else if let Some(stem) = like_stem(&prefix) {
        filter.push_str(&format!(" AND {key} LIKE '{}%'", stem.replace('\'', "''")));
    }
    args[format!("whereClause{root}")] = json!(filter);
    crate::native(
        &format!("Ice.BO.{service}Svc"),
        "GetRows",
        args,
        false,
        crate::report_archive::with_settings(json!({"_progress":{"request":format!("Reading complete {service} definitions and children"),"response":"Report batch received; building native XML exports"}}),&request.state),
    )
}
fn materialize(request: GuestRequest) -> GuestReply {
    let Some(stream @ 0..=1) = request.state["stream"].as_u64() else {
        return failure("The prepared report source is invalid.");
    };
    let (service, _, key, _) = family(stream);
    let mut rows: Vec<(Identity, Value)> =
        match serde_json::from_value(request.state["records"].clone()) {
            Ok(v) => v,
            Err(_) => return failure("The prepared report page is invalid."),
        };
    let mut diagnostics:Vec<Diagnostic>=serde_json::from_value(request.state["diagnostics"].clone()).unwrap_or_default();
    for (id, data) in &mut rows {
        diagnostics.extend(crate::report_archive::graph_errors(id,data));
        let Some(name) = id.keys.get(key) else {
            return failure("The prepared report identity omitted its key.");
        };
        if let Err(mut diagnostic) = crate::report_archive::append(service, name, data) {
            diagnostic.identity=Some(id.clone());diagnostics.push(diagnostic);
        }
        let errors:Vec<_>=diagnostics.iter().filter(|error|error.identity.as_ref()==Some(id)).cloned().collect();
        if !errors.is_empty(){data["herbertCollectionStatus"]=json!({"completeness":"partial","diagnostics":errors});}
        data["herbertOutputGroup"] = json!(if stream == 0 {
            "Report data definitions"
        } else {
            "BAQ reports"
        });
    }
    if request.state["exact"] == true {
        if rows.len() != 1 {
            return failure("The exact prepared report is missing or ambiguous.");
        }
        let (identity, definition) = rows.remove(0);
        return GuestReply::Definition {
            identity,
            definition,
            format: "native-report-definition".into(),
            completeness: if diagnostics.is_empty(){Completeness::Collected}else{Completeness::Partial},
            diagnostics,
        };
    }
    GuestReply::InventoryData {
        inventory: Box::new(GuestReply::Inventory {
            identities: rows.iter().map(|(id, _)| id.clone()).collect(),
            next_cursor: request.state["next"].as_str().map(str::to_owned),
            authoritative: diagnostics.is_empty(),
            diagnostics,
        }),
        records: rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selected_request(stream:u64,family:&str,prefix:&str,body:Value,more:bool) -> GuestRequest {
        GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:"report-definitions".into(),company:"C".into(),identity:None,cursor:Some(json!({"stream":stream,"page":1}).to_string()),page_size:Some(50),state:json!({"_hostPreparePage":true,"_pluginSettings":{"definitionFamily":family,"definitionIdPrefix":prefix}}),response:Some(json!({"returnObj":body,"parameters":{"morePages":more}})),error:None}
    }
    fn finish_selected(mut request:GuestRequest)->(Vec<(Identity,Value)>,Vec<Diagnostic>,Option<String>){
        let GuestReply::PreparedPage {state,next_cursor,..}=handle(request.clone())else{panic!()};request.operation="materialize".into();request.state=state;request.response=None;
        let GuestReply::InventoryData {records,inventory}=handle(request)else{panic!()};let GuestReply::Inventory {diagnostics,..}=*inventory else{panic!()};(records,diagnostics,next_cursor)
    }
    #[test]
    fn definition_selection_preserves_full_query_occurrences_and_baq_report_graph_xml() {
        let mut body=data();body["RptDataDef"].as_array_mut().unwrap().push(json!({"Company":"C","RptDefID":"B","SystemFlag":false,"SystemRpt":false}));
        body["RptTable"]=json!([{"RptDefID":"A","RptTableID":"T1","QueryID":"Q","IsValidDataSource":true,"SysRowID":"a1"},{"RptDefID":"A","RptTableID":"T2","QueryID":"Q","IsValidDataSource":true,"SysRowID":"a2"},{"RptDefID":"B","RptTableID":"T3","QueryID":"Q","IsValidDataSource":true,"SysRowID":"b1"}]);
        body["RptDataSourceField"]=json!([{"QueryID":"Q","FieldName":"F","SysRowID":"f3"},{"QueryID":"Q","FieldName":"F","SysRowID":"f1"},{"QueryID":"Q","FieldName":"F","SysRowID":"f2"}]);
        let (all,errors,_)=finish_selected(selected_request(0,"reportDataAndBaqReports","",body.clone(),false));assert!(errors.is_empty());
        let (selected,errors,_)=finish_selected(selected_request(0,"reportData","A",body,false));assert!(errors.is_empty());assert_eq!(selected,all.into_iter().filter(|(id,_)|id.keys["RptDefID"]=="A").collect::<Vec<_>>());assert_eq!(selected[0].1["RptDataSourceField"].as_array().unwrap().len(),2);
        let mut body=json!({});for table in crate::report_archive::schema("DynamicReport").keys(){body[table]=json!([]);}
        body["BAQReport"]=json!([{"Company":"C","BAQRptID":"Alpha","SystemFlag":false},{"Company":"C","BAQRptID":"Beta","SystemFlag":false}]);
        for table in crate::report_archive::schema("DynamicReport").keys().filter(|table|table.as_str()!="BAQReport") {body[table]=json!([{"BAQRptID":"Alpha","SysRowID":format!("{table}-row")}]);}
        let (all,_,_)=finish_selected(selected_request(1,"reportDataAndBaqReports","",body.clone(),false));let (selected,errors,_)=finish_selected(selected_request(1,"baqReports","Alpha",body,false));assert!(errors.is_empty());assert_eq!(selected,all.into_iter().filter(|(id,_)|id.keys["BAQRptID"]=="Alpha").collect::<Vec<_>>());
    }
    #[test]
    fn definition_selection_validates_only_kept_definitions_and_keeps_valid_selected_owner() {
        let mut body=data();body["RptDataDef"].as_array_mut().unwrap().push(json!({"Company":"C","RptDefID":"Other","SystemFlag":false,"SystemRpt":false,"RptDescription":false}));
        let (records,errors,_)=finish_selected(selected_request(0,"reportData","A",body.clone(),false));assert_eq!(records.len(),1);assert!(errors.is_empty(),"an excluded definition is not validated: {errors:?}");
        assert!(!serde_json::to_string(&records).unwrap().contains("Other"));
        let (_,errors,_)=finish_selected(selected_request(0,"reportData","Other",body.clone(),false));assert!(errors.iter().any(|error|error.code=="REPORT_XML_INVALID" && error.identity.as_ref().unwrap().keys["RptDefID"]=="Other"));
        let (records,errors,_)=finish_selected(selected_request(0,"baqReports","Missing",body.clone(),false));assert!(records.is_empty());assert!(errors.is_empty());
        body["RptTable"][0]["RptDefID"]=json!("Unknown");assert!(matches!(handle(selected_request(0,"baqReports","Missing",body,false)),GuestReply::Failure {..}));
        let mut request=selected_request(0,"reportData","A",data(),false);let GuestReply::PreparedPage {mut state,..}=handle(request.clone())else{panic!()};state["records"][0][1]["errors"]=json!([Diagnostic::new("ACQUIRED_CHILD_FAILED","A child read failed.")]);
        request.state=state;request.operation="materialize".into();request.response=None;let GuestReply::InventoryData {records,inventory}=handle(request)else{panic!()};assert_eq!(records.len(),1);assert!(matches!(*inventory,GuestReply::Inventory {authoritative:false,diagnostics,..} if diagnostics.iter().any(|error|error.code=="ACQUIRED_CHILD_FAILED")));
    }
    #[test]
    fn unselected_family_makes_no_request_and_prefix_narrows_the_parent_read() {
        let request=|stream:u64,family:&str,prefix:&str|{let mut r=selected_request(stream,family,prefix,json!({}),false);r.response=None;r};
        let GuestReply::Inventory {identities,next_cursor,authoritative,diagnostics}=handle(request(0,"baqReports",""))else{panic!()};
        assert!(identities.is_empty()&&authoritative&&diagnostics.is_empty());assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap(),json!({"stream":1,"page":1}));
        let GuestReply::Inventory {next_cursor,..}=handle(request(1,"reportData",""))else{panic!()};assert!(next_cursor.is_none());
        let clause=|stream:u64,prefix:&str|{let GuestReply::Action {action:HostAction::NativeRead {arguments,..},..}=handle(request(stream,"reportDataAndBaqReports",prefix))else{panic!()};arguments};
        assert_eq!(clause(0,"Ab'c")["whereClauseRptDataDef"],"SystemFlag = false AND SystemRpt = false AND RptDefID LIKE 'Ab''c%'");
        assert_eq!(clause(1,"Q_1")["whereClauseBAQReport"],"SystemFlag = false AND BAQRptID LIKE 'Q%'");
        for prefix in ["","%x","[a","Ωa"] {assert_eq!(clause(0,prefix)["whereClauseRptDataDef"],"SystemFlag = false AND SystemRpt = false");}
        // A non-ASCII character ends the stem: the literal is not N-prefixed and could convert lossily.
        assert_eq!(clause(0,"AbΩc")["whereClauseRptDataDef"],"SystemFlag = false AND SystemRpt = false AND RptDefID LIKE 'Ab%'");
    }
    #[test]
    fn definition_family_prefix_raw_paging_settings_and_exact_independence_are_preserved() {
        for (family,prefix,count) in [("reportDataAndBaqReports","",1),("reportData","A",1),("baqReports","",0),("reportData","a",0),("reportData"," A",0),("reportData","%",0)] {let (records,errors,_)=finish_selected(selected_request(0,family,prefix,data(),false));assert_eq!(records.len(),count);assert!(errors.is_empty());}
        let request=selected_request(0,"reportData","Missing",data(),true);let GuestReply::PreparedPage {state,next_cursor,next_state}=handle(request.clone())else{panic!()};assert!(state["records"].as_array().unwrap().is_empty());assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap()["page"],2);assert_eq!(next_state["_pluginSettings"],request.state["_pluginSettings"]);
        let mut next_request=request.clone();next_request.state=next_state;next_request.cursor=next_cursor;next_request.response=None;let GuestReply::Action {state,..}=handle(next_request)else{panic!()};assert_eq!(state["_pluginSettings"],request.state["_pluginSettings"]);
        let mut exact=request;exact.operation="acquire".into();exact.identity=Some(records(0,&data(),"C").unwrap()[0].0.clone());exact.response=None;exact.cursor=None;let baseline=serde_json::to_value(handle(exact.clone())).unwrap();exact.state["_pluginSettings"]=json!({"definitionFamily":false,"definitionIdPrefix":false});assert_eq!(serde_json::to_value(handle(exact)).unwrap()["action"],baseline["action"]);
        for value in [Value::Null,json!(false),json!(2),json!([])] {let mut request=selected_request(0,"reportData","",data(),false);request.state["_pluginSettings"]["definitionIdPrefix"]=value;assert!(matches!(handle(request),GuestReply::Failure {diagnostic} if diagnostic.code=="INVALID_SETTING"));}
    }
    fn data() -> Value {
        let mut data = json!({});
        for name in crate::report_archive::schema("RptDataDef").keys() {
            data[name] = json!([]);
        }
        data["RptDataDef"] =
            json!([{"Company":"","RptDefID":"A","SystemFlag":false,"SystemRpt":false}]);
        data["RptTable"] = json!([{"Company":"","RptDefID":"A","SysRowID":"row","RptTableID":"T"}]);
        data
    }
    #[test]
    fn complete_child_graph_and_global_scope_are_retained() {
        let rows = records(0, &data(), "C").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0.scope, Scope::Global);
        assert_eq!(rows[0].1["RptTable"][0]["RptTableID"], "T");
    }
    #[test]
    fn malformed_children_and_foreign_or_system_parents_fail() {
        let mut d = data();
        d.as_object_mut().unwrap().remove("RptTable");
        assert!(records(0, &d, "C").is_err());
        let mut d = data();
        d["RptTable"][0]["RptDefID"] = json!("other");
        assert!(records(0, &d, "C").is_err());
        let mut d = data();
        let row = d["RptTable"][0].clone();
        d["RptTable"].as_array_mut().unwrap().push(row);
        assert!(records(0, &d, "C").is_err());
        let mut d = data();
        d["RptDataDef"][0]["Company"] = json!("foreign");
        assert!(records(0, &d, "C").is_err());
        let mut d = data();
        d["RptDataDef"][0]["SystemRpt"] = json!(true);
        assert!(records(0, &d, "C").is_err());
    }

    #[test]
    fn query_fields_follow_table_references_and_preserve_each_native_occurrence() {
        let mut d = data();
        d["RptDataDef"].as_array_mut().unwrap().push(json!({"Company":"","RptDefID":"B","SystemFlag":false,"SystemRpt":false}));
        d["RptTable"] = json!([
            {"RptDefID":"A","QueryID":"Q","IsValidDataSource":true,"SysRowID":"a1"},
            {"RptDefID":"A","QueryID":"Q","IsValidDataSource":true,"SysRowID":"a2"},
            {"RptDefID":"B","QueryID":"Q","IsValidDataSource":true,"SysRowID":"b1"}]);
        d["RptDataSourceField"] = json!([
            {"QueryID":"Q","FieldName":"F","SysRowID":"f3"},
            {"QueryID":"Q","FieldName":"F","SysRowID":"f1"},
            {"QueryID":"Q","FieldName":"F","SysRowID":"f2"}]);
        let rows = records(0, &d, "C").unwrap();
        assert_eq!(rows[0].1["RptDataSourceField"].as_array().unwrap().len(), 2);
        assert_eq!(rows[1].1["RptDataSourceField"].as_array().unwrap().len(), 1);
        let ids: BTreeSet<_> = rows.iter().flat_map(|(_,d)| d["RptDataSourceField"].as_array().unwrap()).map(|r| r["SysRowID"].as_str().unwrap()).collect();
        assert_eq!(ids, BTreeSet::from(["f1","f2","f3"]));
        d["RptDataSourceField"].as_array_mut().unwrap().pop();
        assert!(records(0, &d, "C").unwrap_err().contains("ownership cannot be reconciled"));
        d["RptDataSourceField"][0]["QueryID"] = json!("unrelated");
        assert!(records(0, &d, "C").is_err());
    }

    #[test]
    fn supplied_native_response_replay() {
        // Private native evidence stays outside the repository and build output.
        let Ok(path) = std::env::var("HERBERT_RDD_REPLAY") else { return; };
        let response: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let body = crate::body(&response);
        let rows = records(0, body, "TESTCO").unwrap();
        assert_eq!(rows.len(), body["RptDataDef"].as_array().unwrap().len());
        for table in crate::report_archive::schema("RptDataDef").keys() {
            let expected = body[table].as_array().unwrap().len();
            let actual: usize = rows.iter().map(|(_,d)| d[table].as_array().unwrap().len()).sum();
            assert_eq!(actual, expected, "{table}");
        }
        for (_, data) in rows { crate::report_archive::xml("RptDataDef", &data).unwrap(); }
    }
    #[test]
    fn native_parameter_projection_repeats_are_preserved_without_weakening_key_conflicts() {
        let mut d=data();
        let a=json!({"RptDefID":"A","RptCriteriaSetID":"Main","RptTableID":"T","ParameterID":"P","SysRowID":"shared","ParamDataType":"character"});
        let mut b=a.clone();b["ParameterID"]=json!("Other");
        d["RptDataSourceParameter"]=json!([a.clone(),a.clone(),b]);
        let rows=records(0,&d,"C").unwrap();assert_eq!(rows[0].1["RptDataSourceParameter"],d["RptDataSourceParameter"]);
        let mut bad=a;bad["ParamDataType"]=json!("integer");d["RptDataSourceParameter"].as_array_mut().unwrap().push(bad);
        assert!(records(0,&d,"C").unwrap_err().contains("different content"));
    }

}
