//! Authored automation: complete persisted family graphs, local construction.
//! Each family has native schema and count manifests; no execution-history reads.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const FAMILIES: &[(&str, &str, &[&str])] = &[
    (
        "SystemAgent",
        "Ice",
        &[
            "SysAgent",
            "SysAgentSched",
            "SysAgentTask",
            "SysAgentTaskParam",
        ],
    ),
    (
        "ProcessSet",
        "Ice",
        &["ProcessSet", "ProcessTask", "ProcessTaskParam"],
    ),
    ("MessageTemplate", "Ice", &["MessageTemplate"]),
    ("GlobalAlert", "Erp", &["GlbAlert"]),
];
fn err(s: &str) -> Diagnostic {
    Diagnostic::new("AUTOMATION_GRAPH_INVALID", s)
}

fn family(state:&Value)->Result<&str,Diagnostic>{match state.pointer("/_pluginSettings/automationFamily"){None=>Ok("all"),Some(Value::String(v)) if matches!(v.as_str(),"all"|"systemAgents"|"processSets"|"messageTemplates"|"globalAlerts")=>Ok(v),_=>Err(err("Select a declared automation family."))}}
fn selected_stream(i:usize,state:&Value)->Result<bool,Diagnostic>{let family=family(state)?;Ok(family=="all"||family==["systemAgents","processSets","messageTemplates","globalAlerts"][i])}
fn selected_identity(identity:&Identity,state:&Value)->Result<bool,Diagnostic>{let i=FAMILIES.iter().position(|f|f.0==identity.kind||identity.kind=="NativeAutomationEvidence"&&identity.keys.get("Family").is_some_and(|v|v==f.0)).ok_or_else(||err("Unknown automation output owner."))?;selected_stream(i,state)}
fn native_fallback_admission(native:&Value,i:usize,company:&str)->Result<(),Diagnostic>{
    let tables=FAMILIES[i].2;let root=tables[0];let keys:&[&str]=match i{0=>&["AgentID"],1=>&["Company","SystemCode","ProcessID"],2=>&["Company","MessageTemplateID","LangNameID"],_=>&["Company","Alertnum"]};
    for table in tables.iter().filter(|table|**table!="ProcessTaskParam"){let rows=native[*table].as_array().ok_or_else(||err("Native fallback omitted a required table."))?;let mut ids=BTreeSet::new();for row in rows{let id=row["SysRowID"].as_str().filter(|id|!id.is_empty()).ok_or_else(||err("Native fallback omitted a physical identity."))?;if !ids.insert(id.to_ascii_lowercase()){return Err(err("Native fallback repeats a physical identity."));}}}
    let roots=native[root].as_array().ok_or_else(||err("Native fallback omitted automation roots."))?;let mut seen=BTreeSet::new();
    for row in roots{if !seen.insert(rowkey(row,keys)?){return Err(err("Native fallback repeats an automation root."));}if matches!(i,1|2)&&row["SystemFlag"]!=false{return Err(err("Native fallback contains an unadmitted system root."));}if i!=0{let owner=row["Company"].as_str().ok_or_else(||err("Native fallback omitted company."))?;if owner!=""&&owner!=company&&!(i==2&&matches!(row["CompanyVisibility"].as_u64(),Some(5|10))){return Err(err("Native fallback contains a foreign automation owner."));}}}
    for table in tables.iter().skip(1).filter(|table|**table!="ProcessTaskParam"){
        let rows=native[*table].as_array().ok_or_else(||err("Native fallback omitted automation children."))?;let mut seen=BTreeSet::new();
        for row in rows{let child_keys:&[&str]=match *table{"SysAgentSched"=>&["AgentID","AgentSchedNum"],"SysAgentTask"=>&["AgentID","AgentSchedNum","AgentTaskNum"],"SysAgentTaskParam"=>&["AgentID","AgentSchedNum","AgentTaskNum","ParamName"],_=>&["Company","SystemCode","ProcessID","SysTaskNum"]};if !seen.insert(rowkey(row,child_keys)?){return Err(err("Native fallback repeats an automation child."));}if !roots.iter().any(|root|matches(row,root,keys)){return Err(err("Native fallback child has no admitted root."));}if let Some(owner)=row.get("Company"){if owner!=""&&owner!=company{return Err(err("Native fallback contains a foreign task owner."));}}
            if *table=="ProcessTask"&&(row["SystemFlag"]!=false||row["History"]!=false){return Err(err("Native fallback contains a system/history process task."));}
            if *table=="SysAgentTask"&&(row["IsSystemTask"]!=false||!native["SysAgentSched"].as_array().unwrap().iter().any(|parent|matches(row,parent,&["AgentID","AgentSchedNum"]))){return Err(err("Native fallback task has no admitted schedule."));}
            if *table=="SysAgentTaskParam"&&!native["SysAgentTask"].as_array().unwrap().iter().any(|parent|matches(row,parent,&["AgentID","AgentSchedNum","AgentTaskNum"])){return Err(err("Native fallback parameter has no admitted task."));}
        }
    }
    Ok(())
}

fn selection_settings(mut state:Value,from:&Value,errors:Vec<Diagnostic>)->Value {
    if !state.is_object(){state=json!({});}
    if let Some(settings)=from.get("_pluginSettings"){state["_pluginSettings"]=settings.clone();}
    let mut all:Vec<Value>=state.get("diagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
    for value in from.get("diagnostics").and_then(Value::as_array).into_iter().flatten().cloned().chain(errors.into_iter().map(|d|json!(d))){if !all.contains(&value){all.push(value);}}
    if !all.is_empty(){state["diagnostics"]=json!(all);}
    state
}
fn select_records(records:Vec<(Identity,Value)>,from:&Value)->Result<(Vec<(Identity,Value)>,Vec<Diagnostic>),Diagnostic>{
    let mut selected=vec![];let mut diagnostics=vec![];
    for (identity,data) in records {if selected_identity(&identity,from)? {selected.push((identity,data));} else {for value in data.pointer("/herbertCollectionStatus/diagnostics").and_then(Value::as_array).into_iter().flatten(){let mut d:Diagnostic=serde_json::from_value(value.clone()).map_err(|_|err("Invalid acquired selection diagnostic."))?;d.identity=Some(identity.clone());diagnostics.push(d);}}}
    Ok((selected,diagnostics))
}
fn selected_reply(reply:GuestReply,from:&Value)->Result<GuestReply,Diagnostic>{
    let filter_state=|mut state:Value|->Result<Value,Diagnostic>{if let Some(value)=state.get("records"){let records=serde_json::from_value(value.clone()).map_err(|_|err("Invalid selected output owners."))?;let (records,errors)=select_records(records,from)?;state["records"]=json!(records);state=selection_settings(state,from,errors);}else{state=selection_settings(state,from,vec![]);}Ok(state)};
    Ok(match reply {
        GuestReply::Action{action,state}=>GuestReply::Action{action,state:selection_settings(state,from,vec![])},
        GuestReply::PreparedPage{state,next_cursor,next_state}=>GuestReply::PreparedPage{state:filter_state(state)?,next_cursor,next_state:selection_settings(next_state,from,vec![])},
        GuestReply::ReadyRecords{records,state}=>{let(records,errors)=select_records(records,from)?;GuestReply::ReadyRecords{records,state:selection_settings(state,from,errors)}},
        GuestReply::InventoryData{records,inventory}=>{let(records,errors)=select_records(records,from)?;let state=selection_settings(json!({}),from,errors);GuestReply::InventoryData{records,inventory:Box::new(selected_reply(*inventory,&state)?)}},
        GuestReply::Inventory{identities,next_cursor,authoritative,mut diagnostics}=>{let mut selected=vec![];for identity in identities{if selected_identity(&identity,from)?{selected.push(identity);}}for value in from.get("diagnostics").and_then(Value::as_array).into_iter().flatten(){let d:Diagnostic=serde_json::from_value(value.clone()).map_err(|_|err("Invalid acquired selection diagnostic."))?;if !diagnostics.iter().any(|old|json!(old)==json!(d)){diagnostics.push(d);}}GuestReply::Inventory{identities:selected,next_cursor,authoritative:authoritative&&diagnostics.is_empty(),diagnostics}},
        other=>other,
    })
}

pub fn methods() -> Vec<(String, String)> {
    [
        ("Ice.BO.ZDataTableSvc", "GetRows"),
        ("Ice.BO.DynamicQuerySvc", "Execute"),
        ("Ice.BO.SysAgentSvc", "GetRows"),
        ("Ice.BO.ProcessSetSvc", "GetRows"),
        ("Ice.BO.MessageTemplateSvc", "GetRows"),
        ("Erp.BO.GlbAlertSvc", "GetRows"),
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
            if v.service == "Ice.BO.MessageTemplateSvc" {
                t.unscoped_tables.push("MessageTemplate".into());
            }
        }
    }
    p
}
fn schema(stream: usize) -> Value {
    let (_, system, tables) = FAMILIES[stream];
    let names = tables
        .iter()
        .map(|t| format!("DataTableID = '{t}'"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut a = json!({"whereClauseZDataTable":format!("SystemCode = '{system}' AND ({names})"),"whereClauseZDataField":"","pageSize":tables.len(),"absolutePage":1});
    for t in [
        "ZLookupLink",
        "ZLinkColumn",
        "ZLookupField",
        "ZKey",
        "ZKeyField",
    ] {
        a[format!("whereClause{t}")] = json!("DataTableID = '__HerbertAutomationNoChildren__'");
    }
    a
}
fn metadata(response: &Value, stream: usize) -> Result<Value, Diagnostic> {
    let (_, system, tables) = FAMILIES[stream];
    let b = &response["returnObj"];
    if response["parameters"]["morePages"] != false {
        return Err(err("The automation schema is incomplete."));
    }
    let roots = b["ZDataTable"]
        .as_array()
        .ok_or_else(|| err("The native schema omitted tables."))?;
    let mut found = BTreeSet::new();
    for r in roots {
        let t = r["DataTableID"]
            .as_str()
            .ok_or_else(|| err("A schema table has no name."))?;
        if !tables.contains(&t)
            || !r["SystemCode"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(system))
            || !found.insert(t)
        {
            return Err(err(
                "The automation schema has foreign or duplicate tables.",
            ));
        }
    }
    if found.len() != tables.len() {
        return Err(err("The automation schema omitted a required table."));
    }
    let mut out = json!({});
    for t in tables {
        out[*t] = json!([]);
    }
    for f in b["ZDataField"]
        .as_array()
        .ok_or_else(|| err("The native schema omitted fields."))?
    {
        let t = f["DataTableID"]
            .as_str()
            .ok_or_else(|| err("A schema field has no table."))?;
        if !tables.contains(&t) {
            return Err(err("Foreign schema field."));
        }
        if f["External"] == true {
            continue;
        }
        let n = f["FieldName"]
            .as_str()
            .ok_or_else(|| err("A schema field has no name."))?;
        if f["External"] != false
            || f["DBTableName"] != t
            || !crate::report_query::identifier(n)
            || out[t]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["FieldName"] == n)
        {
            return Err(err("Invalid or duplicate physical schema field."));
        }
        out[t].as_array_mut().unwrap().push(f.clone());
    }
    for t in tables {
        if !out[*t]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["FieldName"] == "SysRowID")
        {
            return Err(err("An automation table has no row identity."));
        }
    }
    Ok(out)
}
fn payload(fs: &Value, alias: &str, filter: bool) -> Result<String, Diagnostic> {
    let mut cells = vec![];
    for f in fs.as_array().ok_or_else(|| err("Invalid graph fields."))? {
        let name = f["FieldName"]
            .as_str()
            .ok_or_else(|| err("Missing graph field name."))?;
        if !crate::report_query::identifier(name) {
            return Err(err("Invalid graph field identifier."));
        }
        let ty = f["DataType"]
            .as_str()
            .ok_or_else(|| err("Missing graph field type."))?;
        let col = format!("{alias}.{name}");
        let value = match ty {
            "nvarchar" | "varchar" | "nchar" | "char" | "xml" | "uniqueidentifier" | "datetime"
            | "datetime2" | "smalldatetime" | "date" | "time" => {
                let s = match ty {
                    "uniqueidentifier" => format!("LOWER(CONVERT(nvarchar(36),{col}))"),
                    "datetime" | "datetime2" | "smalldatetime" | "date" | "time" => {
                        format!("CONVERT(nvarchar(max),{col},126)")
                    }
                    _ => format!("CONVERT(nvarchar(max),{col})"),
                };
                format!("N'\"' + STRING_ESCAPE({s},'json') + N'\"'")
            }
            "bit" => format!("CASE WHEN {col}=1 THEN N'true' ELSE N'false' END"),
            "timestamp" => format!("CONVERT(nvarchar(max),CONVERT(bigint,{col}))"),
            "int" | "bigint" | "smallint" | "tinyint" | "decimal" | "numeric" | "money"
            | "smallmoney" => format!("CONVERT(nvarchar(max),{col})"),
            "float" | "real" => format!("CONVERT(nvarchar(max),{col},3)"),
            _ => {
                return Err(err(
                    "A native graph field type has no lossless JSON projection.",
                ));
            }
        };
        let value = if filter
            && [
                "SysPassWord",
                "SysPassWordMasked",
                "Password",
                "ApiKey",
                "ClientSecret",
                "Secret",
                "ParamToken",
            ]
            .contains(&name)
        {
            "N'null'".to_string()
        } else if filter && name == "ParamCharacter" {
            format!(
                "CASE WHEN LOWER({alias}.ParamName) LIKE '%password%' OR LOWER({alias}.ParamName) LIKE '%secret%' OR LOWER({alias}.ParamName) LIKE '%apikey%' OR LOWER({alias}.ParamName) LIKE '%access[_]token%' THEN N'null' ELSE {value} END"
            )
        } else {
            value
        };
        cells.push(format!(
            "CASE WHEN {col} IS NULL THEN N'null' ELSE {value} END"
        ));
    }
    Ok(format!(
        "CONVERT(nvarchar(max),N'[') + {} + N']'",
        cells.join(" + N',' + ")
    ))
}

fn build(
    company: &str,
    stream: usize,
    meta: &Value,
    filter: bool,
    visible: &Value,
) -> Result<Value, Diagnostic> {
    if company.is_empty() {
        return Err(err("An automation company is required."));
    }
    let (_, system, tables) = FAMILIES[stream];
    let q = format!("HerbertAutomationGraph{stream}");
    let mut ds = json!({"DynamicQuery":[{"Company":company,"QueryID":q,"Description":"Unsaved complete authored automation graph","Updatable":false,"ExtQuery":false,"IsShared":false,"AllCompanies":false,"XCompany":true,"RowMod":"A"}],"QuerySubQuery":[],"QueryTable":[],"QueryField":[],"QueryWhereItem":[],"QuerySortBy":[],"QueryParameter":[],"QueryCtrl":[],"QueryRelation":[],"QueryRelationField":[]});
    for (ti, table) in tables.iter().enumerate() {
        for marker in [false, true] {
            let branch = ti * 2 + usize::from(marker);
            let alias = format!("Source{branch}");
            let task_owner = format!("TaskOwner{branch}");
            let set_owner = format!("SetOwner{branch}");
            let sub = format!("00000000-0000-0000-0000-{:012}", branch + 1);
            let row = |extra: Value| {
                let mut r = json!({"Company":company,"QueryID":q,"SubQueryID":sub,"RowMod":"A"});
                r.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                r
            };
            ds["QuerySubQuery"].as_array_mut().unwrap().push(row(json!({"Name":alias,"Type":if branch==0{"TopLevel"}else{"UnionAll"},"Seq":branch+1})));
            ds["QueryTable"].as_array_mut().unwrap().extend([row(json!({"TableID":alias,"DBSchemaName":system,"DBTableName":table,"TableType":"DB","Seq":1,"IsVisibleInDesigner":true})),row(json!({"TableID":"Calculated","TableType":"TT","Seq":-1,"IsVisibleInDesigner":false}))]);
            let mut criteria: Vec<(String, String, String, String, String, String)> = vec![];
            let mut criterion =
                |a: &str, f: &str, v: String, logic: &str, left: &str, right: &str| {
                    criteria.push((
                        a.into(),
                        f.into(),
                        v,
                        logic.into(),
                        left.into(),
                        right.into(),
                    ))
                };
            // Apply scope and authored/history exclusions before values enter responses.
            let scoped = match *table {
                "SysAgentTask" => Some(alias.clone()),
                "SysAgentTaskParam" => Some(task_owner.clone()),
                "SysAgent" | "SysAgentSched" | "MessageTemplate" => None,
                _ => Some(alias.clone()),
            };
            if let Some(a) = scoped {
                criterion(
                    &a,
                    "Company",
                    crate::report_query::literal(company),
                    "",
                    "(",
                    "",
                );
                criterion(&a, "Company", "N''".into(), "Or", "", ")");
            }
            if matches!(
                *table,
                "ProcessSet" | "ProcessTask" | "ProcessTaskParam" | "MessageTemplate"
            ) {
                criterion(&alias, "SystemFlag", "0".into(), "And", "", "");
            }
            if *table == "ProcessTask" {
                criterion(&alias, "History", "0".into(), "And", "", "");
            }
            if *table == "SysAgentTask" {
                criterion(&alias, "IsSystemTask", "0".into(), "And", "", "");
            }
            let mut joins: Vec<(&str, &str, Vec<&str>)> = vec![];
            if *table == "SysAgentTaskParam" {
                joins.push((
                    "SysAgentTask",
                    task_owner.as_str(),
                    vec!["AgentID", "AgentSchedNum", "AgentTaskNum"],
                ));
                criterion(
                    task_owner.as_str(),
                    "IsSystemTask",
                    "0".into(),
                    "And",
                    "",
                    "",
                );
            }
            if matches!(*table, "ProcessTask" | "ProcessTaskParam") {
                joins.push((
                    "ProcessSet",
                    set_owner.as_str(),
                    vec!["Company", "SystemCode", "ProcessID"],
                ));
                criterion(set_owner.as_str(), "SystemFlag", "0".into(), "And", "", "");
            }
            if *table == "ProcessTaskParam" {
                joins.push((
                    "ProcessTask",
                    task_owner.as_str(),
                    vec!["Company", "SystemCode", "ProcessID", "SysTaskNum"],
                ));
                criterion(task_owner.as_str(), "SystemFlag", "0".into(), "And", "", "");
                criterion(task_owner.as_str(), "History", "0".into(), "And", "", "");
            }
            for (ji, (parent, owner, keys)) in joins.into_iter().enumerate() {
                ds["QueryTable"].as_array_mut().unwrap().push(row(json!({"TableID":owner,"DBSchemaName":system,"DBTableName":parent,"TableType":"DB","Seq":ji+2,"IsVisibleInDesigner":true})));
                let rel = format!("00000000-0000-0001-{:04}-{:012}", branch + 1, ji + 1);
                ds["QueryRelation"].as_array_mut().unwrap().push(row(json!({"RelationID":rel,"ParentTableID":owner,"ChildTableID":alias,"JoinType":"Inner","Seq":ji+1})));
                for (ki, key) in keys.iter().enumerate() {
                    ds["QueryRelationField"].as_array_mut().unwrap().push(row(json!({"RelationID":rel,"ParentFieldName":key,"ChildFieldName":key,"CompOp":"=","AndOr":if ki==0{""}else{"And"},"Seq":ki+1})));
                }
            }

            // Persist only rows admitted by the native service under this caller.
            let (visibility_table, visibility_alias) = if *table == "ProcessTaskParam" {
                ("ProcessTask", task_owner.as_str())
            } else {
                (*table, alias.as_str())
            };
            let admitted = visible[visibility_table]
                .as_array()
                .ok_or_else(|| err("Missing native service visibility contract."))?;
            if admitted.is_empty() {
                criterion(
                    &alias,
                    "SysRowID",
                    "N'00000000-0000-0000-0000-000000000000'".into(),
                    "And",
                    "",
                    "",
                );
            } else {
                for (vi, id) in admitted.iter().enumerate() {
                    let value = id
                        .as_str()
                        .ok_or_else(|| err("Invalid admitted native row identity."))?;
                    criterion(
                        visibility_alias,
                        "SysRowID",
                        crate::report_query::literal(value),
                        if vi == 0 { "And" } else { "Or" },
                        if vi == 0 { "(" } else { "" },
                        if vi + 1 == admitted.len() { ")" } else { "" },
                    );
                }
            }
            for (i, (name, ty, formula)) in [
                (
                    "Kind",
                    "nvarchar",
                    format!("CONVERT(nvarchar(max),N'{table}')"),
                ),
                (
                    "Payload",
                    "nvarchar",
                    if marker {
                        "CONVERT(nvarchar(max),NULL)".into()
                    } else {
                        payload(&meta[*table], &alias, filter)?
                    },
                ),
                (
                    "Total",
                    "bigint",
                    if marker {
                        "COUNT_BIG(*)".into()
                    } else {
                        "CONVERT(bigint,-1)".into()
                    },
                ),
            ]
            .into_iter()
            .enumerate()
            {
                ds["QueryField"].as_array_mut().unwrap().push(row(json!({"TableID":"Calculated","FieldName":name,"Alias":name,"DataType":ty,"FieldFormat":"","FieldLabel":name,"Seq":i+1,"IsCalculated":true,"Formula":formula,"Updatable":false})));
            }
            for (i, (a, f, v, logic, left, right)) in criteria.into_iter().enumerate() {
                ds["QueryWhereItem"].as_array_mut().unwrap().push(row(json!({"CriteriaID":format!("00000000-0000-0002-{:04}-{:012}",branch+1,i+1),"TableID":a,"FieldName":f,"CompOp":"=","RValue":v,"IsConst":true,"CriteriaType":2,"Seq":i+1,"AndOr":if i==0{""}else{logic.as_str()},"Neg":false,"LeftP":left,"RightP":right,"UseHaving":false})));
            }
        }
    }
    Ok(json!({"queryDS":ds,"executionParams":{"ExecutionParameter":[]}}))
}

fn decode(
    response: &Value,
    meta: &Value,
    company: &str,
    stream: usize,
) -> Result<Value, Diagnostic> {
    if response["returnObj"]["Errors"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        return Err(err("The native automation graph returned errors."));
    }
    let tables = FAMILIES[stream].2;
    let rows = response["returnObj"]["Results"]
        .as_array()
        .ok_or_else(|| err("The graph omitted Results."))?;
    let mut graph = json!({});
    for t in tables {
        graph[*t] = json!([]);
    }
    let mut totals = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for wire in rows {
        let table = wire["Kind"]
            .as_str()
            .filter(|t| tables.contains(t))
            .ok_or_else(|| err("Unknown automation graph table."))?;
        let n = wire["Total"]
            .as_i64()
            .ok_or_else(|| err("Missing automation graph total."))?;
        if n >= 0 {
            if !wire["Payload"].is_null() || totals.insert(table, n as usize).is_some() {
                return Err(err("Invalid graph count manifest."));
            }
            continue;
        }
        if n != -1 {
            return Err(err("Invalid graph row marker."));
        }
        let cells: Vec<Value> = serde_json::from_str(
            wire["Payload"]
                .as_str()
                .ok_or_else(|| err("Missing graph payload."))?,
        )
        .map_err(|_| err("Invalid graph JSON."))?;
        let fs = meta[table]
            .as_array()
            .ok_or_else(|| err("Missing graph schema."))?;
        if cells.len() != fs.len() {
            return Err(err("Graph field count differs from native schema."));
        }
        let mut row: Value = Value::Object(
            fs.iter()
                .zip(cells)
                .map(|(f, v)| (f["FieldName"].as_str().unwrap().into(), v))
                .collect(),
        );
        for f in fs {
            if matches!(
                f["DataType"].as_str(),
                Some("datetime" | "datetime2" | "smalldatetime")
            ) {
                let k = f["FieldName"].as_str().unwrap();
                if let Some(v) = row[k].as_str().filter(|v| v.contains('.')) {
                    row[k] = json!(v.trim_end_matches('0').trim_end_matches('.'));
                }
            }
        }
        let id = row["SysRowID"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| err("Missing physical graph identity."))?;
        if !seen.insert((table, id.to_ascii_lowercase())) {
            return Err(err("Duplicate graph identity."));
        }
        if row.get("Company").is_some()
            && row["Company"] != company
            && row["Company"] != ""
            && !(stream == 2 && matches!(row["CompanyVisibility"].as_u64(), Some(5 | 10)))
        {
            return Err(err("Foreign automation company."));
        }
        graph[table].as_array_mut().unwrap().push(row);
    }
    for t in tables {
        if totals.get(t) != Some(&graph[*t].as_array().unwrap().len()) {
            return Err(err("The graph is missing rows or its count manifest."));
        }
        graph[*t]
            .as_array_mut()
            .unwrap()
            .sort_by_key(|r| r["SysRowID"].as_str().unwrap().to_ascii_lowercase());
    }
    Ok(graph)
}
fn matches(a: &Value, b: &Value, keys: &[&str]) -> bool {
    keys.iter().all(|k| match (a.get(*k), b.get(*k)) {
        (Some(Value::String(a)), Some(Value::String(b))) => a.eq_ignore_ascii_case(b),
        (Some(a), Some(b)) => !a.is_null() && a == b,
        _ => false,
    })
}
fn rowkey(row: &Value, keys: &[&str]) -> Result<Vec<String>, Diagnostic> {
    keys.iter()
        .map(|k| {
            let v = row
                .get(*k)
                .ok_or_else(|| err("A native automation key is missing."))?;
            if let Some(s) = v.as_str() {
                Ok(s.to_ascii_lowercase())
            } else if v.is_i64() || v.is_u64() {
                Ok(v.to_string())
            } else {
                Err(err("A native automation key has an invalid type."))
            }
        })
        .collect()
}
fn records(
    graph: &Value,
    meta: &Value,
    stream: usize,
    company: &str,
    filter: bool,
    native: &Value,
) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let (kind, _, tables) = FAMILIES[stream];
    let root_table = tables[0];
    let roots = graph[root_table]
        .as_array()
        .ok_or_else(|| err("Missing automation roots."))?;
    let keys: &[&str] = match stream {
        0 => &["AgentID"],
        1 => &["Company", "SystemCode", "ProcessID"],
        2 => &["Company", "MessageTemplateID", "LangNameID"],
        _ => &["Company", "Alertnum"],
    };
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for root in roots {
        let k = rowkey(root, keys)?;
        if !seen.insert(k) {
            return Err(err("The automation graph repeats a native root identity."));
        }
        if matches!(stream, 1 | 2) && root["SystemFlag"] != false {
            return Err(err(
                "A system-marked definition reached the authored graph.",
            ));
        }
        let owner = if stream == 0 {
            ""
        } else {
            root["Company"]
                .as_str()
                .ok_or_else(|| err("Missing automation company."))?
        };
        if !owner.is_empty()
            && owner != company
            && !(stream == 2
                && matches!(root["CompanyVisibility"].as_u64(), Some(5 | 10))
                && native[root_table]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|n| matches(n, root, &["SysRowID", "Company", "CompanyVisibility"])))
        {
            return Err(err("Foreign automation root company."));
        }
        let identity = Identity {
            category: "C25".into(),
            kind: kind.into(),
            scope: if owner.is_empty() {
                Scope::Global
            } else if owner != company {
                Scope::SharedCompany {
                    company: company.into(),
                    owner_company: owner.into(),
                }
            } else {
                Scope::Company {
                    company: owner.into(),
                }
            },
            keys: keys
                .iter()
                .map(|k| {
                    (
                        (*k).into(),
                        root[*k]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| root[*k].to_string()),
                    )
                })
                .collect(),
        };
        let mut data = json!({});
        data[root_table] = json!([root]);
        for t in &tables[1..] {
            data[*t] = json!(
                graph[*t]
                    .as_array()
                    .ok_or_else(|| err("Missing automation children."))?
                    .iter()
                    .filter(|r| matches(r, root, keys))
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        if stream == 0 {
            let schedules = data["SysAgentSched"].as_array().unwrap();
            let mut sk = BTreeSet::new();
            for s in schedules {
                if !sk.insert(rowkey(s, &["AgentID", "AgentSchedNum"])?) {
                    return Err(err("Duplicate schedule identity."));
                }
            }
            let tasks = data["SysAgentTask"].as_array().unwrap();
            let mut tk = BTreeSet::new();
            for t in tasks {
                if t["IsSystemTask"] != false
                    || !schedules
                        .iter()
                        .any(|s| matches(t, s, &["AgentID", "AgentSchedNum"]))
                    || !tk.insert(rowkey(t, &["AgentID", "AgentSchedNum", "AgentTaskNum"])?)
                {
                    return Err(err("Invalid scheduled task ownership or identity."));
                }
            }
            let mut pk = BTreeSet::new();
            for p in data["SysAgentTaskParam"].as_array().unwrap() {
                if !tasks
                    .iter()
                    .any(|t| matches(p, t, &["AgentID", "AgentSchedNum", "AgentTaskNum"]))
                    || !pk.insert(rowkey(
                        p,
                        &["AgentID", "AgentSchedNum", "AgentTaskNum", "ParamName"],
                    )?)
                {
                    return Err(err("Invalid scheduled parameter ownership or identity."));
                }
            }
        }
        if stream == 1 {
            let tasks = data["ProcessTask"].as_array().unwrap();
            let mut tk = BTreeSet::new();
            for t in tasks {
                if t["SystemFlag"] != false
                    || t["History"] != false
                    || !tk.insert(rowkey(
                        t,
                        &["Company", "SystemCode", "ProcessID", "SysTaskNum"],
                    )?)
                {
                    return Err(err("Invalid process task identity, history or ownership."));
                }
            }
            let mut pk = BTreeSet::new();
            for p in data["ProcessTaskParam"].as_array().unwrap() {
                if p["SystemFlag"] != false
                    || !tasks.iter().any(|t| {
                        matches(p, t, &["Company", "SystemCode", "ProcessID", "SysTaskNum"])
                    })
                    || !pk.insert(rowkey(
                        p,
                        &[
                            "Company",
                            "SystemCode",
                            "ProcessID",
                            "SysTaskNum",
                            "ParamName",
                        ],
                    )?)
                {
                    return Err(err("Invalid process parameter ownership or identity."));
                }
            }
        }
        let mut native_data = json!({});
        for table in tables.iter().filter(|t| **t != "ProcessTaskParam") {
            native_data[*table] = json!(
                native[*table]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|r| matches(r, root, keys))
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        out.push((
            identity,
            complete(data, meta, stream, filter, &native_data)?,
        ));
    }
    for t in &tables[1..] {
        for r in graph[*t].as_array().unwrap() {
            if !roots.iter().any(|root| matches(r, root, keys)) {
                return Err(err("An automation child has no retained parent."));
            }
        }
    }
    Ok(out)
}
fn complete(
    mut data: Value,
    meta: &Value,
    stream: usize,
    filter: bool,
    native: &Value,
) -> Result<Value, Diagnostic> {
    let (kind, system, tables) = FAMILIES[stream];
    let mut omissions = vec![];
    if filter {
        for table in tables {
            for row in data[*table].as_array().unwrap() {
                for field in [
                    "SysPassWord",
                    "SysPassWordMasked",
                    "Password",
                    "ApiKey",
                    "ClientSecret",
                    "Secret",
                    "ParamToken",
                    "ParamCharacter",
                ] {
                    let parameter_sensitive = row["ParamName"].as_str().is_some_and(|n| {
                        let n = n.to_lowercase();
                        ["password", "secret", "apikey", "access_token"]
                            .iter()
                            .any(|s| n.contains(s))
                    });
                    if row.get(field).is_some()
                        && (field != "ParamCharacter" || parameter_sensitive)
                    {
                        if !row[field].is_null() {
                            return Err(err("A filtered sensitive field reached retained output."));
                        }
                        omissions.push(json!({"table":table,"sysRowID":row["SysRowID"],"field":field,"reason":"explicit sensitive-value filter; original value may be null"}));
                    }
                }
            }
        }
    }
    let source = data.clone();
    let mut recovery = json!({});
    let mut source_only_fields = vec![];
    for table in tables {
        if stream == 1 && *table != "ProcessSet" {
            continue;
        }
        let mut rows = data[*table].as_array().unwrap().clone();
        for row in &mut rows {
            if let Some(original) = native[*table]
                .as_array()
                .into_iter()
                .flatten()
                .find(|n| matches(n, row, &["SysRowID"]))
            {
                let unsupported: Vec<String> = row
                    .as_object()
                    .unwrap()
                    .keys()
                    .filter(|k| original.get(k.as_str()).is_none())
                    .cloned()
                    .collect();
                for field in unsupported {
                    source_only_fields.push(json!({"table":table,"sysRowID":row["SysRowID"],"field":field,"reason":"Physical source field is absent from the native typed service row; no native write compatibility is established."}));
                    row.as_object_mut().unwrap().remove(&field);
                }
                for (k, v) in original.as_object().unwrap() {
                    if row.get(k).is_none() {
                        row[k] = v.clone();
                    }
                }
            }
            row["RowMod"] = json!(if stream == 3 || stream == 0 { "U" } else { "A" });
            row.as_object_mut().unwrap().remove("SysRevID");
            row.as_object_mut().unwrap().remove("SysRowID");
        }
        recovery[*table] = json!(rows);
    }
    let service = format!(
        "{system}.BO.{}Svc",
        if stream == 0 {
            "SysAgent"
        } else if stream == 3 {
            "GlbAlert"
        } else {
            kind
        }
    );
    let request =
        json!({"ds":recovery,"continueProcessingOnError":false,"rollbackParentOnChildError":true});
    let policy = json!({"filterSensitiveFields":filter,"omissions":omissions,"systemExclusions":["SystemFlag=true roots and process children","IsSystemTask=true scheduled tasks and their parameters"],"historyExclusions":["ProcessTask.History=true and its parameters"],"acquisition":"native metadata and unsaved complete persisted family graph","nativeOwnerScopePreserved":true,"resources":"all schema-described stored scalar fields; no opaque binary resources declared by these nine native tables"});
    let mut contract = json!({"service":service,"method":"UpdateExt","requestFile":"native-update-ext.json","sourceFile":"automation-source.json","schemaFile":"automation-schema.json","sourceOnlyFields":source_only_fields,"sourceOwnerCompany":source[tables[0]][0].get("Company").cloned().unwrap_or(json!("")),"companyContext":"To preserve a nonempty stored Company, use that authorized native destination company context. SharedCompany identifies visibility from the selected company, not a new owner. Choose and document any destination remapping explicitly.","enablement":"Source values are preserved. Review AutoStart, Started, schedules Enabled and alerts Active before restore; restoration may enable automation.","processParameters":if stream==1{"Restore the ProcessSet root with UpdateExt, then use native-save-process-tasks.json SaveProcessTask requests. The task service is derived from stored native dataset provenance and TaskType; confirm the destination contract. Preserve unmapped/context parameter evidence from source."}else{"not applicable"},"credentialRecovery":"Stored credential fields are preserved by default; destination encryption/masking semantics must be reconciled before reuse."});
    if stream == 1 {
        contract["processTaskRecoveryFile"] = json!("native-save-process-tasks.json");
    }
    if stream == 0 {
        contract["scheduleCreationFile"] = json!("native-create-schedules.json");
    }
    data["herbertSources"] = json!({"files":[{"name":"automation-source.json","text":serde_json::to_string_pretty(&source).unwrap()},{"name":"automation-schema.json","text":serde_json::to_string_pretty(meta).unwrap()},{"name":"native-update-ext.json","text":serde_json::to_string_pretty(&request).unwrap()},{"name":"automation-recovery.json","text":serde_json::to_string_pretty(&contract).unwrap()},{"name":"automation-policy.json","text":serde_json::to_string_pretty(&policy).unwrap()}]});
    if stream == 1 {
        let tasks = process_recovery(&source)?;
        data["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"native-save-process-tasks.json","text":serde_json::to_string_pretty(&tasks).unwrap()}));
    }
    if stream == 0 {
        let mut stages = vec![];
        for schedule in recovery["SysAgentSched"].as_array().unwrap() {
            let source_key =
                json!({"AgentID":schedule["AgentID"],"AgentSchedNum":schedule["AgentSchedNum"]});
            let mut create = schedule.clone();
            create["AgentSchedNum"] = json!(0);
            create["RowMod"] = json!("A");
            let tasks = recovery["SysAgentTask"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| matches(t, schedule, &["AgentID", "AgentSchedNum"]))
                .cloned()
                .collect::<Vec<_>>();
            let params = recovery["SysAgentTaskParam"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|t| matches(t, schedule, &["AgentID", "AgentSchedNum"]))
                .cloned()
                .collect::<Vec<_>>();
            stages.push(json!({"sourceKey":source_key,"service":"Ice.BO.SysAgentSvc","createSchedule":{"method":"Update","arguments":{"ds":{"SysAgentSched":[create]}}},"sourceTasks":tasks,"sourceParameters":params,"requiredBindings":["Read back the newly assigned AgentSchedNum after schedule creation","Create each source task under that destination schedule; map the native AgentTaskNum if regenerated","Create parameters under the mapped schedule/task keys; set RowMod=A only for these new rows"],"enablement":"The original Enabled value is preserved; review it before calling the native method."}));
        }
        data["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"native-create-schedules.json","text":serde_json::to_string_pretty(&json!({"stages":stages,"existingSingletonAgentRequired":true,"inPlaceAlternative":"native-update-ext.json uses U for exact existing keys; create stages use native-assigned new schedule IDs"})).unwrap()}));
    }
    data["automationNative"] = native.clone();
    data["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"automation-native-service.json","text":serde_json::to_string_pretty(native).unwrap()}));
    data["automationPolicy"] = policy;
    data["automationRecovery"] = contract;
    data["herbertDirectOutput"] = json!(true);
    data["herbertCollectionStatus"] = json!({"completeness":"collected","diagnostics":[]});
    Ok(data)
}

fn process_recovery(data: &Value) -> Result<Value, Diagnostic> {
    let mut out = vec![];
    for task in data["ProcessTask"]
        .as_array()
        .ok_or_else(|| err("Missing process tasks."))?
    {
        let params = data["ProcessTaskParam"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| {
                matches(
                    p,
                    task,
                    &["Company", "SystemCode", "ProcessID", "SysTaskNum"],
                )
            })
            .collect::<Vec<_>>();
        let provenance = |name: &str| {
            params
                .iter()
                .find(|p| p["ParamName"] == name)
                .and_then(|p| p["ParamCharacter"].as_str())
        };
        let system = provenance("ReportParameterDataSetSystemCode");
        let dataset = provenance("ReportParameterDataSetId");
        let Some((system, dataset)) = system.zip(dataset).filter(|(s, d)| {
            crate::report_query::identifier(s) && crate::report_query::identifier(d)
        }) else {
            out.push(json!({"task":task,"recovery":"source-only","reason":"Native parameter dataset provenance is missing or invalid.","parameters":params}));
            continue;
        };
        let route = match task["TaskType"].as_str() {
            Some("Report") => "Rpt",
            Some("Process") => "Proc",
            _ => {
                out.push(json!({"task":task,"recovery":"source-only","reason":"TaskType has no established native save-service mapping.","parameters":params}));
                continue;
            }
        };
        let mut row = json!({});
        let mut context = vec![];
        let mut unsupported = vec![];
        for p in &params {
            let name = p["ParamName"]
                .as_str()
                .ok_or_else(|| err("A process parameter has no name."))?;
            if name.starts_with("Cur-")
                || name.starts_with("ReportParameterDataSet")
                || !crate::report_query::identifier(name)
            {
                context.push((*p).clone());
                continue;
            }
            let field = match p["ParamDataType"]
                .as_str()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("string" | "character") => "ParamCharacter",
                Some("int32" | "integer") => "ParamInteger",
                Some("int64" | "long") => "ParamLong",
                Some("decimal") => "ParamDecimal",
                Some("boolean" | "logical") => "ParamLogical",
                Some("datetime" | "date") => "ParamDate",
                _ => {
                    unsupported.push((*p).clone());
                    continue;
                }
            };
            row[name] = p[field].clone();
            if p["ParamToken"].as_str().is_some_and(|v| !v.is_empty()) {
                row[format!("{name}Token")] = p["ParamToken"].clone();
            }
        }
        row["ProcessID"] = task["ProcessID"].clone();
        row["ProcessCompany"] = task["Company"].clone();
        row["ProcessSystemCode"] = task["SystemCode"].clone();
        row["ProcessTaskNum"] = task["SysTaskNum"].clone();
        let mut ds = json!({"ExtensionTables":[]});
        ds[format!("{dataset}Param")] = json!([row]);
        out.push(json!({"sourceTask":task,"service":format!("{system}.{route}.{dataset}Svc"),"method":"SaveProcessTask","arguments":{"maintProgram":task["SourceDLL"],"ds":ds},"recovery":"native definition-only save candidate; destination service contract must be confirmed","contextParameters":context,"unsupportedParameters":unsupported,"identityNormalization":["ProcessID, ProcessCompany, ProcessSystemCode and ProcessTaskNum identify the destination task; SysRowID/SysRevID are regenerated","SaveProcessTask rebuilds TaskDescription and session context; source task metadata remains available for supported native Update afterward"],"execution":"SaveProcessTask persists a definition; never call SubmitToAgent, RunDirect or RunTask to restore a backup"}));
    }
    Ok(json!({"tasks":out,"sourceRemainsAuthoritative":true}))
}

fn output(mut state: Value) -> Result<GuestReply, Diagnostic> {
    // Prepared records already contain their recovery files. A current host can
    // accept them together; legacy checkpoints keep their original continuation.
    if state["bulk"] == true {
        let records: Vec<(Identity, Value)> = serde_json::from_value(state["records"].take())
            .map_err(|_| err("Invalid prepared automation records."))?;
        return Ok(GuestReply::InventoryData {
            inventory: Box::new(GuestReply::Inventory {
                identities: records.iter().map(|(id, _)| id.clone()).collect(),
                next_cursor: state["next"].as_str().map(str::to_owned),
                authoritative: state["diagnostics"].as_array().is_none_or(Vec::is_empty),
                diagnostics: serde_json::from_value(state["diagnostics"].take()).unwrap_or_default(),
            }),
            records,
        });
    }
    let i = state["index"]
        .as_u64()
        .ok_or_else(|| err("Invalid automation output continuation."))? as usize;
    let count = state["records"]
        .as_array()
        .ok_or_else(|| err("Missing prepared records."))?
        .len();
    if i == count {
        return Ok(GuestReply::Inventory {
            identities: vec![],
            next_cursor: state["next"].as_str().map(str::to_owned),
            authoritative: state["diagnostics"].as_array().is_none_or(Vec::is_empty),
            diagnostics: serde_json::from_value(state["diagnostics"].clone()).unwrap_or_default(),
        });
    }
    if i > count {
        return Err(err("Invalid automation output index."));
    }
    let record = serde_json::from_value(state["records"][i].take())
        .map_err(|_| err("Invalid prepared automation record."))?;
    state["index"] = json!(i + 1);
    Ok(GuestReply::ReadyRecords {
        records: vec![record],
        state,
    })
}
fn finish(
    records: Vec<(Identity, Value)>,
    next: Option<String>,
    prepare: bool,
    bulk: bool,
) -> Result<GuestReply, Diagnostic> {
    let state = json!({"phase":"output","records":records,"index":0,"next":next,"bulk":bulk});
    if prepare {
        Ok(GuestReply::PreparedPage {
            state,
            next_cursor: next,
            next_state: json!({}),
        })
    } else {
        output(state)
    }
}
fn stream(r: &GuestRequest) -> Result<usize, Diagnostic> {
    let value = if r.operation == "acquire" {
        let id = r
            .identity
            .as_ref()
            .ok_or_else(|| err("An exact automation identity is required."))?;
        if id.category != "C25" {
            return Err(err("Foreign automation identity."));
        }
        FAMILIES
            .iter()
            .position(|f| f.0 == id.kind)
            .ok_or_else(|| err("Unknown automation identity kind."))?
            .to_string()
    } else {
        r.cursor.clone().unwrap_or_else(|| "0".into())
    };
    let i = value
        .parse::<usize>()
        .map_err(|_| err("Invalid automation source continuation."))?;
    if i >= FAMILIES.len() {
        return Err(err("Unavailable automation source."));
    }
    Ok(i)
}

#[cfg(test)]
mod bulk_output_tests {
    use super::*;
    #[test]
    fn automation_bulk_preserves_original_files_partial_diagnostics_and_cursor() {
        let records:Vec<(Identity,Value)>=(0..37).map(|n|(Identity{category:"C25".into(),kind:"ProcessSet".into(),scope:Scope::Global,keys:BTreeMap::from([("ProcessSetID".into(),n.to_string())])},json!({"herbertSources":{"files":[{"name":"recovery.json","text":"é exact"}]},"sensitive":{"original":"preserve"},"unknown":[n]}))).collect();
        let diagnostics=vec![Diagnostic::new("TEST_PARTIAL","Retain exact coverage.")];
        let mut legacy=json!({"records":records,"index":0,"next":"2","diagnostics":diagnostics});
        let mut expected=Vec::new();
        loop {match output(legacy).unwrap(){
            GuestReply::ReadyRecords{records,state}=>{expected.extend(records);legacy=state;},
            GuestReply::Inventory{diagnostics:ds,authoritative,next_cursor,..}=>{assert!(!authoritative);assert_eq!(json!(ds),json!(diagnostics));assert_eq!(next_cursor.as_deref(),Some("2"));break;},
            other=>panic!("{other:?}"),
        }}
        let GuestReply::InventoryData{records:actual,inventory}=output(json!({"bulk":true,"records":records,"next":"2","diagnostics":diagnostics})).unwrap()else{panic!()};
        assert_eq!(actual,expected);
        assert!(matches!(*inventory,GuestReply::Inventory{authoritative:false,diagnostics:ds,next_cursor:Some(_),..} if json!(ds)==json!(diagnostics)));
        for bulk in [false,true]{
            let GuestReply::PreparedPage{state,next_cursor,..}=finish(records.clone(),Some("2".into()),true,bulk).unwrap()else{panic!()};
            assert_eq!(state["bulk"],bulk);assert_eq!(next_cursor.as_deref(),Some("2"));
            assert_eq!(matches!(output(state).unwrap(),GuestReply::InventoryData{..}),bulk);
        }
        assert!(matches!(finish(vec![],None,false,true).unwrap(),GuestReply::InventoryData{records,..}if records.is_empty()));
    }
}

fn visibility_action(r: &GuestRequest, i: usize, s: Value) -> Result<GuestReply, Diagnostic> {
    let (kind, system, tables) = FAMILIES[i];
    let page = s["page"]
        .as_u64()
        .filter(|p| *p > 0 && *p <= i32::MAX as u64)
        .ok_or_else(|| err("Native visibility page must fit a positive Int32."))?;
    let size = r
        .page_size
        .filter(|s| *s > 0 && *s <= i32::MAX as u32)
        .ok_or_else(|| err("Native visibility page size must fit a positive Int32."))?;
    let mut a = json!({"pageSize":size,"absolutePage":page});
    for t in tables.iter().filter(|t| **t != "ProcessTaskParam") {
        a[format!("whereClause{t}")] = json!("");
    }
    let company = r.company.replace('\'', "''");
    let scope = format!("(Company = '{company}' OR Company = '')");
    if i == 0 {
        a["whereClauseSysAgentTask"] = json!(format!("{scope} AND IsSystemTask = false"));
    }
    if i == 1 {
        a["whereClauseProcessSet"] = json!(format!("{scope} AND SystemFlag = false"));
        a["whereClauseProcessTask"] = json!("SystemFlag = false AND History = false");
    }
    if i == 2 {
        a["whereClauseMessageTemplate"] = json!("SystemFlag = false");
    }
    if i == 3 {
        a["whereClauseGlbAlert"] = json!(scope);
    }
    let service = format!(
        "{system}.BO.{}Svc",
        if i == 0 {
            "SysAgent"
        } else if i == 3 {
            "GlbAlert"
        } else {
            kind
        }
    );
    Ok(crate::native(&service, "GetRows", a, false, s))
}

fn drive(mut r: GuestRequest, i: usize) -> Result<GuestReply, Diagnostic> {
    if r.operation == "materialize" && r.state["phase"] == "output" {
        return output(r.state);
    }
    if !matches!(r.operation.as_str(), "discover" | "acquire") {
        return Err(Diagnostic::new(
            "UNSUPPORTED_OPERATION",
            "C25 supports bulk discovery, exact-identity acquisition and prepared materialization.",
        ));
    }
    if let Some(d) = r.error.take() {
        return Err(d);
    }
    let mut s = r.state.clone();
    if s["phase"].is_null() {
        let filter = crate::response_policy::sensitive_filter_enabled(&GuestRequest {
            state: s.clone(),
            ..r.clone()
        });
        s = json!({"phase":"visibility","stream":i,"page":1,"visible":{},"native":{},"prepare":s["_hostPreparePage"],"bulk":s["_hostIndependentWorkItems"],"filter":filter});
        return visibility_action(&r, i, s);
    }
    if s["stream"].as_u64() != Some(i as u64) {
        return Err(err("Automation response belongs to another source."));
    }
    let response = r
        .response
        .clone()
        .ok_or_else(|| err("Missing automation native response."))?;
    if s["phase"] == "visibility" {
        let tables = FAMILIES[i].2;
        let b = &response["returnObj"];
        let native_tables = if i == 1 { &tables[..2] } else { tables };
        for (name, value) in b
            .as_object()
            .ok_or_else(|| err("Invalid native visibility body."))?
        {
            if let Some(rows) = value.as_array() {
                if !s["native"][name].is_array() {
                    s["native"][name] = json!([]);
                }
                s["native"][name]
                    .as_array_mut()
                    .unwrap()
                    .extend(rows.iter().cloned());
                if !native_tables.contains(&name.as_str()) && !rows.is_empty() {
                    s["unmappedNativeTables"][name] = json!(true);
                }
            } else {
                s["native"][name] = value.clone();
            }
        }
        for t in native_tables {
            let rows = b[*t]
                .as_array()
                .ok_or_else(|| err("The native visibility page omitted a required table."))?;
            if !s["visible"][*t].is_array() {
                s["visible"][*t] = json!([]);
            }
            for row in rows {
                let id = row["SysRowID"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| err("A native visible row omitted SysRowID."))?;
                if s["visible"][*t].as_array().unwrap().iter().any(|v| v == id) {
                    return Err(err("Native visibility repeated a row."));
                }
                s["visible"][*t].as_array_mut().unwrap().push(json!(id));
            }
        }
        let more = response["parameters"]["morePages"]
            .as_bool()
            .ok_or_else(|| err("Native visibility omitted morePages."))?;
        if more {
            if b[tables[0]].as_array().is_none_or(Vec::is_empty) {
                return Err(err("Native visibility page did not advance."));
            }
            s["page"] = json!(s["page"].as_u64().unwrap() + 1);
            return visibility_action(&r, i, s);
        }
        if s["visible"][tables[0]].as_array().unwrap().is_empty() {
            if r.operation == "acquire" {
                return Err(err("The requested automation identity is not visible."));
            }
            return finish(
                vec![],
                (i + 1 < FAMILIES.len()).then(|| (i + 1).to_string()),
                s["prepare"] == true,
                s["bulk"] == true,
            );
        }
        if s["unmappedNativeTables"]
            .as_object()
            .is_some_and(|m| !m.is_empty())
        {
            let partial = GuestRequest {
                state: s,
                ..r.clone()
            };
            return Ok(fallback(&partial,i,err("The native service returned populated tables without an established persisted ownership mapping; native evidence is retained."))
                .unwrap_or_else(||GuestReply::Failure{diagnostic:err("Missing native fallback evidence.")}));
        }
        s["phase"] = json!("schema");
        return Ok(crate::native(
            "Ice.BO.ZDataTableSvc",
            "GetRows",
            schema(i),
            false,
            s,
        ));
    }
    if s["phase"] == "schema" {
        s["metadata"] = metadata(&response, i)?;
        let args = build(
            &r.company,
            i,
            &s["metadata"],
            s["filter"] == true,
            &s["visible"],
        )?;
        s["phase"] = json!("graph");
        return Ok(crate::native(
            "Ice.BO.DynamicQuerySvc",
            "Execute",
            args,
            false,
            s,
        ));
    }
    if s["phase"] != "graph" {
        return Err(err("Unknown automation phase."));
    }
    let g = decode(&response, &s["metadata"], &r.company, i)?;
    for t in FAMILIES[i].2 {
        if *t != "ProcessTaskParam" {
            let actual: BTreeSet<_> = g[*t]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["SysRowID"].as_str().unwrap().to_ascii_lowercase())
                .collect();
            let expected: BTreeSet<_> = s["visible"][*t]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap().to_ascii_lowercase())
                .collect();
            for row in g[*t].as_array().unwrap() {
                let original = s["native"][*t]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|n| matches(n, row, &["SysRowID"]))
                    .ok_or_else(|| err("Missing native source row."))?;
                if original["SysRevID"] != row["SysRevID"] {
                    return Err(err(
                        "Automation source changed between native visibility and persisted acquisition; native evidence is retained.",
                    ));
                }
            }
            if actual != expected {
                return Err(err("Stored and native service-visible membership differ."));
            }
        }
    }
    let records = records(
        &g,
        &s["metadata"],
        i,
        &r.company,
        s["filter"] == true,
        &s["native"],
    )?;
    if r.operation == "acquire" {
        let id = r
            .identity
            .ok_or_else(|| err("Missing automation identity."))?;
        let (_, definition) = records
            .into_iter()
            .find(|(found, _)| *found == id)
            .ok_or_else(|| {
                err("The requested automation identity is absent from native-visible source.")
            })?;
        return Ok(GuestReply::Definition {
            identity: id,
            format: "native-automation-definition".into(),
            definition,
            completeness: Completeness::Collected,
            diagnostics: vec![],
        });
    }
    finish(
        records,
        (i + 1 < FAMILIES.len()).then(|| (i + 1).to_string()),
        s["prepare"] == true,
        s["bulk"] == true,
    )
}
fn fallback_omissions(data: &Value, filter: bool) -> Vec<Value> {
    let mut out = vec![];
    if !filter {
        return out;
    }
    if let Some(tables) = data.as_object() {
        for (table, rows) in tables {
            for row in rows.as_array().into_iter().flatten() {
                let sensitive = row["ParamName"].as_str().is_some_and(|n| {
                    let n = n.to_lowercase();
                    ["password", "secret", "apikey", "access_token"]
                        .iter()
                        .any(|s| n.contains(s))
                });
                if let Some(fields) = row.as_object() {
                    for (field, value) in fields {
                        if [
                            "SysPassWord",
                            "SysPassWordMasked",
                            "Password",
                            "ApiKey",
                            "ClientSecret",
                            "Secret",
                            "ParamToken",
                        ]
                        .iter()
                        .any(|f| f.eq_ignore_ascii_case(field))
                            || (sensitive && field == "ParamCharacter")
                        {
                            out.push(json!({"table":table,"sysRowID":row["SysRowID"],"field":field,"projection":"upstream native sensitive-value policy","projectedNull":value.is_null(),"originalPopulationUnknown":true}));
                        }
                    }
                }
            }
        }
    }
    out
}
fn fallback(r: &GuestRequest, i: usize, diagnostic: Diagnostic) -> Option<GuestReply> {
    let native = &r.state["native"];
    let root = FAMILIES[i].2[0];
    if native[root].as_array().is_none_or(Vec::is_empty) {
        return None;
    }
    if let Err(admission)=native_fallback_admission(native,i,&r.company){return Some(GuestReply::Inventory{identities:vec![],next_cursor:(i+1<FAMILIES.len()).then(||(i+1).to_string()),authoritative:false,diagnostics:vec![diagnostic,admission]});}
    let kind = FAMILIES[i].0;
    let identity = Identity {
        category: "C25".into(),
        kind: "NativeAutomationEvidence".into(),
        scope: Scope::Company {
            company: r.company.clone(),
        },
        keys: BTreeMap::from([("Family".into(), kind.into())]),
    };
    let coverage = json!({"completeness":"partial","representation":"native service source evidence","nativeFamily":kind,"queryCompany":r.company,"ownerScope":"Each row retains its own native Company or global identity; the artifact scope identifies the caller connection","omissions":["Complete persisted fields and independent counts are unavailable","ProcessTaskParam is absent from ProcessSetTableset","No generated recovery payload is claimed for this partial evidence"],"filterSensitiveFields":r.state["filter"]==true,"sensitiveOmissions":fallback_omissions(native,r.state["filter"]==true),"diagnostics":[diagnostic.clone()]});
    let data = json!({"nativeServiceGraph":native,"automationCoverage":coverage,"herbertDirectOutput":true,"herbertCollectionStatus":{"completeness":"partial","diagnostics":[diagnostic.clone()]},"herbertSources":{"files":[{"name":"native-source-only.json","text":serde_json::to_string_pretty(native).unwrap()},{"name":"automation-coverage.json","text":serde_json::to_string_pretty(&coverage).unwrap()}]}});
    let next = (i + 1 < FAMILIES.len()).then(|| (i + 1).to_string());
    let state = json!({"phase":"output","records":[(identity,data)],"index":0,"next":next,"diagnostics":[diagnostic]});
    Some(if r.state["prepare"] == true {
        GuestReply::PreparedPage {
            state,
            next_cursor: next,
            next_state: json!({}),
        }
    } else {
        output(state).unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic })
    })
}
fn handle_inner(r: GuestRequest) -> GuestReply {
    let i = match stream(&r) {
        Ok(i) => i,
        Err(diagnostic) => return GuestReply::Failure { diagnostic },
    };
    let independent = r.operation == "discover";
    match drive(r.clone(), i) {
        Ok(v) => v,
        Err(diagnostic) if independent => {
            fallback(&r, i, diagnostic.clone()).unwrap_or_else(|| GuestReply::Inventory {
                identities: vec![],
                next_cursor: (i + 1 < FAMILIES.len()).then(|| (i + 1).to_string()),
                authoritative: false,
                diagnostics: vec![diagnostic],
            })
        }
        Err(diagnostic) => GuestReply::Failure { diagnostic },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_automation_families_skip_only_unacquired_streams(){
        for (family,wanted) in [("all",None),("systemAgents",Some(0)),("processSets",Some(1)),("messageTemplates",Some(2)),("globalAlerts",Some(3))]{for i in 0..4{let mut r=request();r.cursor=Some(i.to_string());r.state["_pluginSettings"]=json!({"automationFamily":family});let reply=handle(r);if wanted.is_none()||wanted==Some(i){assert!(matches!(reply,GuestReply::Action{state,..}if state["_pluginSettings"]["automationFamily"]==family));}else{assert!(matches!(reply,GuestReply::Inventory{authoritative:true,..}));}}}
        let mut r=request();r.state=json!({"phase":"visibility","stream":0,"_pluginSettings":{"automationFamily":"messageTemplates"}});r.error=Some(Diagnostic::new("DENIED","acquired refusal"));assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,..}if !diagnostics.is_empty()));
    }
    #[test]
    fn selected_family_preserves_complete_graph_and_native_fallback_with_frozen_settings(){
        let all=records(&agent(),&json!({}),0,"C",false,&agent()).unwrap();for bulk in [false,true]{let from=json!({"_pluginSettings":{"automationFamily":"systemAgents"},"diagnostics":[Diagnostic::new("ACQUIRED","retained error")]});let GuestReply::PreparedPage{state,next_state,..}=selected_reply(finish(all.clone(),Some("1".into()),true,bulk).unwrap(),&from).unwrap()else{panic!()};assert!(state.get("_hostWorkItems").is_none());assert_eq!(state["records"],json!(all));assert_eq!(next_state["_pluginSettings"],from["_pluginSettings"]);let mut r=request();r.operation="materialize".into();r.state=state;loop{match handle(r.clone()){GuestReply::ReadyRecords{records,state}=>{assert_eq!(records,all);r.state=state;},GuestReply::InventoryData{records,inventory}=>{assert_eq!(records,all);assert!(matches!(*inventory,GuestReply::Inventory{authoritative:false,..}));break;},GuestReply::Inventory{authoritative:false,diagnostics,..}=>{assert_eq!(diagnostics.len(),1);break;},reply=>panic!("{reply:?}")}}}
        let mut r=request();r.state=json!({"phase":"schema","stream":0,"native":agent(),"prepare":true,"_pluginSettings":{"automationFamily":"systemAgents"}});r.error=Some(Diagnostic::new("DENIED","richer read"));let GuestReply::PreparedPage{state,..}=handle(r)else{panic!()};assert_eq!(state["records"][0][1]["nativeServiceGraph"],agent());assert_eq!(state["records"][0][0]["kind"],"NativeAutomationEvidence");assert!(state.get("_hostWorkItems").is_none());
    }
    #[test]
    fn excluded_acquired_fallback_keeps_error_without_payload_and_rejects_bad_native_owners(){
        let mut r=request();r.state=json!({"phase":"schema","stream":0,"native":agent(),"prepare":true,"_pluginSettings":{"automationFamily":"messageTemplates"}});r.error=Some(Diagnostic::new("DENIED","richer read"));let GuestReply::PreparedPage{state,..}=handle(r.clone())else{panic!()};assert_eq!(state["records"],json!([]));r.operation="materialize".into();r.state=state;r.error=None;assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,..}if !diagnostics.is_empty()));
        for (table,field,value) in [("SysAgentTask","Company",json!("Foreign")),("SysAgentTask","AgentSchedNum",json!(99)),("SysAgentTaskParam","AgentTaskNum",json!(99)),("SysAgent","SysRowID",json!(false))]{let mut r=request();let mut native=agent();native[table][0][field]=value;r.state=json!({"phase":"schema","stream":0,"native":native,"prepare":true,"_pluginSettings":{"automationFamily":"systemAgents"}});r.error=Some(Diagnostic::new("DENIED","richer read"));assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,identities,diagnostics,..}if identities.is_empty()&&diagnostics.len()==2&&diagnostics[0].code=="DENIED"));}
    }
    #[test]
    fn exact_automation_ignores_invalid_discovery_family(){let identity=records(&agent(),&json!({}),0,"C",false,&agent()).unwrap()[0].0.clone();let mut r=request();r.operation="acquire".into();r.identity=Some(identity);let baseline=handle(r.clone());r.state["_pluginSettings"]=json!({"automationFamily":false});let actual=handle(r);assert_eq!(json!(actual),json!(baseline));}
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C25".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(100),
            state: json!({"_hostPreparePage":true}),
            response: None,
            error: None,
        }
    }
    fn agent() -> Value {
        json!({"SysAgent":[{"AgentID":"Agent","SysRowID":"root","UnknownFutureField":"Ω exact","SysPassWord":"synthetic"}],"SysAgentSched":[{"AgentID":"Agent","AgentSchedNum":7,"Enabled":false,"SysRowID":"schedule"}],"SysAgentTask":[{"AgentID":"Agent","AgentSchedNum":7,"AgentTaskNum":1,"Company":"C","IsSystemTask":false,"SysRowID":"task"},{"AgentID":"Agent","AgentSchedNum":7,"AgentTaskNum":2,"Company":"","IsSystemTask":false,"SysRowID":"global-task"}],"SysAgentTaskParam":[{"AgentID":"Agent","AgentSchedNum":7,"AgentTaskNum":1,"ParamName":"ClientSecret","ParamCharacter":"synthetic","ParamToken":"synthetic-token","ParamLong":9007199254740000i64,"SysRowID":"param"}]})
    }
    #[test]
    fn global_agent_company_tasks_full_relationships_and_unknown_values_survive() {
        let out = records(&agent(), &json!({}), 0, "C", false, &agent()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.scope, Scope::Global);
        assert_eq!(out[0].1["SysAgent"][0]["UnknownFutureField"], "Ω exact");
        assert_eq!(out[0].1["SysAgentTask"].as_array().unwrap().len(), 2);
        assert_eq!(
            out[0].1["SysAgentTaskParam"][0]["ParamLong"],
            9007199254740000i64
        );
        assert!(
            out[0].1["herbertSources"]
                .to_string()
                .contains("synthetic-token")
        );
    }
    #[test]
    fn orphan_schedule_task_and_parameter_keys_cannot_complete() {
        for (t, k, v) in [
            ("SysAgentSched", "AgentID", json!("foreign")),
            ("SysAgentTask", "AgentSchedNum", json!(8)),
            ("SysAgentTaskParam", "AgentTaskNum", json!(8)),
            ("SysAgentTask", "IsSystemTask", json!(true)),
        ] {
            let mut g = agent();
            g[t][0][k] = v;
            assert!(records(&g, &json!({}), 0, "C", false, &g).is_err());
        }
    }
    #[test]
    fn optional_sensitive_filter_is_explicit_and_does_not_change_system_exclusions() {
        let mut g = agent();
        assert!(records(&g, &json!({}), 0, "C", true, &g).is_err());
        g["SysAgent"][0]["SysPassWord"] = Value::Null;
        g["SysAgentTaskParam"][0]["ParamCharacter"] = Value::Null;
        g["SysAgentTaskParam"][0]["ParamToken"] = Value::Null;
        let out = records(&g, &json!({}), 0, "C", true, &g).unwrap();
        assert_eq!(out[0].1["automationPolicy"]["filterSensitiveFields"], true);
        assert_eq!(
            out[0].1["automationPolicy"]["omissions"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        g["SysAgentTask"][0]["IsSystemTask"] = json!(true);
        assert!(records(&g, &json!({}), 0, "C", true, &g).is_err());
    }
    #[test]
    fn manifests_detect_truncation_duplicate_and_undeclared_fields() {
        let meta = json!({"MessageTemplate":[{"FieldName":"Company","DataType":"nvarchar"},{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]});
        let mut rows = vec![json!({"Kind":"MessageTemplate","Total":1,"Payload":null})];
        assert!(decode(&json!({"returnObj":{"Results":rows}}), &meta, "C", 2).is_err());
        rows.push(json!({"Kind":"MessageTemplate","Total":-1,"Payload":"[\"C\",\"id\"]"}));
        assert!(decode(&json!({"returnObj":{"Results":rows}}), &meta, "C", 2).is_ok());
        rows.push(rows[1].clone());
        assert!(decode(&json!({"returnObj":{"Results":rows}}), &meta, "C", 2).is_err());
    }
    #[test]
    fn native_service_denial_advances_independent_family() {
        let mut r = request();
        r.error = Some(Diagnostic::new("DENIED", "Native service denied"));
        let GuestReply::Inventory {
            next_cursor,
            authoritative,
            diagnostics,
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(next_cursor.as_deref(), Some("1"));
        assert!(!authoritative);
        assert_eq!(diagnostics[0].code, "DENIED");
    }
    #[test]
    fn visibility_is_native_paged_and_required_before_persisted_read() {
        let r = request();
        let GuestReply::Action {
            action:
                HostAction::NativeRead {
                    service,
                    method,
                    arguments,
                    ..
                },
            state,
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(service, "Ice.BO.SysAgentSvc");
        assert_eq!(method, "GetRows");
        assert_eq!(
            arguments["whereClauseSysAgentTask"],
            "(Company = 'C' OR Company = '') AND IsSystemTask = false"
        );
        assert_eq!(state["phase"], "visibility");
        let mut next = r;
        next.state = state;
        next.response = Some(json!({"returnObj":agent(),"parameters":{"morePages":false}}));
        let GuestReply::Action {
            action: HostAction::NativeRead { service, .. },
            state,
        } = handle(next)
        else {
            panic!()
        };
        assert_eq!(service, "Ice.BO.ZDataTableSvc");
        assert_eq!(
            state["visible"]["SysAgentTask"].as_array().unwrap().len(),
            2
        );
    }
    #[test]
    fn query_has_count_manifest_and_explicit_native_visibility_restrictions() {
        let tables = FAMILIES[0].2;
        let meta: Value = Value::Object(
            tables
                .iter()
                .map(|t| {
                    (
                        (*t).into(),
                        json!([{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]),
                    )
                })
                .collect(),
        );
        let visible: Value = Value::Object(
            tables
                .iter()
                .map(|t| ((*t).into(), json!([format!("id-{t}")])))
                .collect(),
        );
        let q = build("C", 0, &meta, false, &visible).unwrap();
        assert_eq!(q["queryDS"]["QuerySubQuery"].as_array().unwrap().len(), 8);
        for t in tables {
            assert!(q.to_string().contains(&format!("id-{t}")));
        }
        assert!(build("", 0, &meta, false, &visible).is_err());
    }
    #[test]
    fn process_recovery_preserves_typed_values_and_records_context() {
        let task = json!({"Company":"C","SystemCode":"ERP","ProcessID":"P","SysTaskNum":7,"TaskType":"Report","SourceDLL":"Ice.UIRpt.BAQReport"});
        let p = |name: &str, ty: &str, field: &str, value: Value| {
            let mut p = task.clone();
            p["ParamName"] = json!(name);
            p["ParamDataType"] = json!(ty);
            p[field] = value;
            p
        };
        let data = json!({"ProcessTask":[task],"ProcessTaskParam":[p("ReportParameterDataSetSystemCode","String","ParamCharacter",json!("Ice")),p("ReportParameterDataSetId","String","ParamCharacter",json!("BAQReport")),p("Character01","String","ParamCharacter",json!("Ω exact")),p("Date01","DateTime","ParamDate",json!("2099-01-02")),p("Number01","Decimal","ParamDecimal",json!(1234.56789)),p("Cur-Comp","String","ParamCharacter",json!("C"))]});
        let r = process_recovery(&data).unwrap();
        let t = &r["tasks"][0];
        assert_eq!(t["service"], "Ice.Rpt.BAQReportSvc");
        assert_eq!(t["method"], "SaveProcessTask");
        assert_eq!(
            t["arguments"]["ds"]["BAQReportParam"][0]["Character01"],
            "Ω exact"
        );
        assert_eq!(
            t["arguments"]["ds"]["BAQReportParam"][0]["ProcessTaskNum"],
            7
        );
        assert_eq!(t["contextParameters"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn successful_native_graph_survives_denied_richer_read_and_later_family_continues() {
        let mut r = request();
        r.state =
            json!({"phase":"schema","stream":0,"prepare":true,"filter":false,"native":agent()});
        r.error = Some(Diagnostic::new("DENIED", "Dictionary denied"));
        let GuestReply::PreparedPage {
            state, next_cursor, ..
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(next_cursor.as_deref(), Some("1"));
        let mut output_r = r;
        output_r.operation = "materialize".into();
        output_r.error = None;
        output_r.state = state;
        let GuestReply::ReadyRecords { records, .. } = handle(output_r) else {
            panic!()
        };
        assert_eq!(
            records[0].1["herbertCollectionStatus"]["completeness"],
            "partial"
        );
        assert_eq!(
            records[0].1["nativeServiceGraph"]["SysAgentTaskParam"][0]["ParamName"],
            "ClientSecret"
        );
        assert!(
            !records[0].1["herbertSources"]
                .to_string()
                .contains("native-update-ext.json")
        );
        let mut next = request();
        next.cursor = next_cursor;
        let GuestReply::Action {
            action: HostAction::NativeRead { service, .. },
            ..
        } = handle(next)
        else {
            panic!()
        };
        assert_eq!(service, "Ice.BO.ProcessSetSvc");
    }
    #[test]
    fn native_union_parent_aliases_are_unique_across_data_and_count_branches() {
        for stream in [0, 1] {
            let tables = FAMILIES[stream].2;
            let meta: Value = Value::Object(
                tables
                    .iter()
                    .map(|t| {
                        (
                            (*t).into(),
                            json!([{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]),
                        )
                    })
                    .collect(),
            );
            let visible: Value = Value::Object(
                tables
                    .iter()
                    .filter(|t| **t != "ProcessTaskParam")
                    .map(|t| ((*t).into(), json!([format!("id-{t}")])))
                    .collect(),
            );
            let q = build("C", stream, &meta, false, &visible).unwrap();
            let mut names = BTreeSet::new();
            for t in q["queryDS"]["QueryTable"].as_array().unwrap() {
                let name = t["TableID"].as_str().unwrap();
                if name != "Calculated" {
                    assert!(names.insert(name));
                }
            }
        }
    }
    #[test]
    fn standalone_recovery_metadata_matches_the_definition_contract() {
        for stream in [0, 1] {
            let g = if stream == 0 {
                agent()
            } else {
                json!({"ProcessSet":[{"Company":"C","SystemCode":"ERP","ProcessID":"P","SystemFlag":false,"SysRowID":"p"}],"ProcessTask":[],"ProcessTaskParam":[]})
            };
            let d = complete(g.clone(), &json!({}), stream, false, &g).unwrap();
            let file = d["herbertSources"]["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"] == "automation-recovery.json")
                .unwrap();
            let parsed: Value = serde_json::from_str(file["text"].as_str().unwrap()).unwrap();
            assert_eq!(parsed, d["automationRecovery"]);
            if stream == 0 {
                let f = d["herbertSources"]["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|f| f["name"] == "native-create-schedules.json")
                    .unwrap();
                let p: Value = serde_json::from_str(f["text"].as_str().unwrap()).unwrap();
                for stage in p["stages"].as_array().unwrap() {
                    assert_eq!(stage["createSchedule"]["method"], "Update");
                    assert_eq!(
                        stage["createSchedule"]["arguments"]
                            .as_object()
                            .unwrap()
                            .len(),
                        1
                    );
                    assert_eq!(
                        stage["createSchedule"]["arguments"]["ds"]["SysAgentSched"][0]["AgentSchedNum"],
                        0
                    );
                }
            }
        }
    }
    #[test]
    fn native_shared_templates_keep_owner_private_and_unadmitted_rows_fail() {
        let shared = json!({"Company":"ADMIN","CompanyVisibility":10,"MessageTemplateID":"Shared","LangNameID":"enu","SystemFlag":false,"SysRowID":"s","SysRevID":1});
        let g = json!({"MessageTemplate":[shared.clone()]});
        let out = records(&g, &json!({}), 2, "C", false, &g).unwrap();
        assert_eq!(
            out[0].0.scope,
            Scope::SharedCompany {
                company: "C".into(),
                owner_company: "ADMIN".into()
            }
        );
        assert_eq!(out[0].0.keys["Company"], "ADMIN");
        assert!(
            records(
                &g,
                &json!({}),
                2,
                "C",
                false,
                &json!({"MessageTemplate":[]})
            )
            .is_err()
        );
        let mut private = g.clone();
        private["MessageTemplate"][0]["CompanyVisibility"] = json!(0);
        assert!(records(&private, &json!({}), 2, "C", false, &private).is_err());
        let meta =
            json!({"MessageTemplate":[{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]});
        let q = build("C", 2, &meta, false, &json!({"MessageTemplate":["s"]})).unwrap();
        assert!(
            !q["queryDS"]["QueryWhereItem"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["FieldName"] == "Company")
        );
        assert!(
            q["queryDS"]["QueryWhereItem"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["FieldName"] == "SysRowID")
        );
        let GuestReply::Action {
            action: HostAction::NativeRead { arguments, .. },
            ..
        } = visibility_action(&request(), 2, json!({"page":1})).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            arguments["whereClauseMessageTemplate"],
            "SystemFlag = false"
        );
        let p = policies()
            .into_iter()
            .find(|p| p.service == "Ice.BO.MessageTemplateSvc")
            .unwrap();
        assert!(
            p.tables
                .unwrap()
                .unscoped_tables
                .contains(&"MessageTemplate".into())
        );
    }
    #[test]
    fn physical_fields_outside_native_typed_contract_stay_source_only() {
        let g = agent();
        let mut native = g.clone();
        native["SysAgent"][0]
            .as_object_mut()
            .unwrap()
            .remove("UnknownFutureField");
        let d = complete(g, &json!({}), 0, false, &native).unwrap();
        assert_eq!(d["SysAgent"][0]["UnknownFutureField"], "Ω exact");
        let file = d["herbertSources"]["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "native-update-ext.json")
            .unwrap();
        let request: Value = serde_json::from_str(file["text"].as_str().unwrap()).unwrap();
        assert!(
            request["ds"]["SysAgent"][0]
                .get("UnknownFutureField")
                .is_none()
        );
        assert!(
            d["automationRecovery"]["sourceOnlyFields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["table"] == "SysAgent" && f["field"] == "UnknownFutureField")
        );
    }
}

pub fn handle(r:GuestRequest)->GuestReply{
    if r.operation=="acquire"||r.identity.is_some(){return handle_inner(r);}
    let from=r.state.clone();if let Err(d)=family(&from){return GuestReply::Failure{diagnostic:d};}
    if r.operation=="discover"&&from["phase"].is_null()&&r.response.is_none()&&r.error.is_none(){if let Ok(i)=stream(&r){if matches!(selected_stream(i,&from),Ok(false)){return selected_reply(GuestReply::Inventory{identities:vec![],next_cursor:(i+1<FAMILIES.len()).then(||(i+1).to_string()),authoritative:true,diagnostics:vec![]},&from).unwrap_or_else(|diagnostic|GuestReply::Failure{diagnostic});}}}
    match selected_reply(handle_inner(r),&from){Ok(reply)=>reply,Err(diagnostic)=>GuestReply::Failure{diagnostic}}
}
