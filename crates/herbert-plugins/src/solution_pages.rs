//! C24: bulk persisted graph and exact opaque retained package bytes.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const TABLES: &[&str] = &[
    "SolutionHeader",
    "SolutionDetail",
    "SolutionTracker",
    "SolutionType",
    "SolutionTypeDetail",
];
fn err(s: &str) -> Diagnostic {
    Diagnostic::new("SOLUTION_GRAPH_INVALID", s)
}
fn selection(state:&Value)->Result<(&str,&str),Diagnostic>{let settings=&state["_pluginSettings"];let family=match settings.get("definitionFamily"){None=>"all",Some(Value::String(value)) if matches!(value.as_str(),"all"|"solutions"|"solutionTypes"|"retainedPackages")=>value,_=>return Err(err("Select a declared solution definition family."))};let prefix=match settings.get("solutionIdPrefix"){None=>"",Some(Value::String(value))=>value,_=>return Err(err("The solution ID prefix must be a string."))};Ok((family,prefix))}
fn selected(identity:&Identity,state:&Value)->Result<bool,Diagnostic>{let (family,prefix)=selection(state)?;let group=match identity.kind.as_str(){"PersistedSolutionHeader"=>"solutions","PersistedSolutionType"=>"solutionTypes","RetainedInstalledSolution"=>"retainedPackages",_=>return Err(err("Unknown solution owner."))};Ok((family=="all"||family==group)&&(group=="solutionTypes"||identity.keys.get("SolutionID").is_some_and(|id|id.starts_with(prefix))))}
fn settings(mut state:Value,from:&Value)->Value{if let Some(value)=from.get("_pluginSettings"){if !state.is_object(){state=json!({});}state["_pluginSettings"]=value.clone();}state}
fn carry(reply:GuestReply,from:&Value)->GuestReply{match reply{GuestReply::Action {action,state}=>GuestReply::Action {action,state:settings(state,from)},GuestReply::ReadyRecords {records,state}=>GuestReply::ReadyRecords {records,state:settings(state,from)},GuestReply::PreparedPage {mut state,next_cursor,next_state}=>{if let Some(items)=state.get_mut("_hostWorkItems").and_then(Value::as_array_mut){for item in items{*item=settings(item.take(),from);}}GuestReply::PreparedPage {state:settings(state,from),next_cursor,next_state:settings(next_state,from)}},other=>other}}
pub fn methods() -> Vec<(String, String)> {
    [
        ("Ice.BO.BAQDesignerSvc", "GetFieldList"),
        ("Ice.BO.DynamicQuerySvc", "Execute"),
        ("Ice.BO.ExportPackageSvc", "GetInstalledSolutions"),
        ("Ice.BO.ExportPackageSvc", "GetInstalledSolution"),
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
fn payload(fs: &Value, alias: &str) -> Result<String, Diagnostic> {
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
        cells.push(format!(
            "CASE WHEN {col} IS NULL THEN N'null' ELSE {value} END"
        ));
    }
    Ok(format!(
        "CONVERT(nvarchar(max),N'[') + {} + N']'",
        cells.join(" + N',' + ")
    ))
}

fn build(company: &str, meta: &Value) -> Result<Value, Diagnostic> {
    if company.is_empty() {
        return Err(err("A company is required."));
    }
    let q = "HerbertSolutionGraph";
    let mut ds = json!({"DynamicQuery":[{"Company":company,"QueryID":q,"Description":"Unsaved complete solution graph","Updatable":false,"ExtQuery":false,"IsShared":false,"AllCompanies":false,"XCompany":true,"RowMod":"A"}],"QuerySubQuery":[],"QueryTable":[],"QueryField":[],"QueryWhereItem":[],"QuerySortBy":[],"QueryParameter":[],"QueryCtrl":[],"QueryRelation":[],"QueryRelationField":[]});
    for (ti, table) in TABLES.iter().enumerate() {
        for marker in [false, true] {
            let branch = ti * 2 + usize::from(marker);
            let alias = format!("Source{branch}");
            let owner = format!("Owner{branch}");
            let sub = format!("00000000-0000-0000-0000-{:012}", branch + 1);
            let row = |extra: Value| {
                let mut r = json!({"Company":company,"QueryID":q,"SubQueryID":sub,"RowMod":"A"});
                r.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                r
            };
            ds["QuerySubQuery"].as_array_mut().unwrap().push(row(json!({"Name":alias,"Type":if branch==0{"TopLevel"}else{"UnionAll"},"Seq":branch+1})));
            ds["QueryTable"].as_array_mut().unwrap().extend([row(json!({"TableID":alias,"DBSchemaName":"Ice","DBTableName":table,"TableType":"DB","Seq":1,"IsVisibleInDesigner":true})),row(json!({"TableID":"Calculated","TableType":"TT","Seq":-1,"IsVisibleInDesigner":false}))]);
            if matches!(*table, "SolutionDetail" | "SolutionTracker") {
                ds["QueryTable"].as_array_mut().unwrap().push(row(json!({"TableID":owner,"DBSchemaName":"Ice","DBTableName":"SolutionHeader","TableType":"DB","Seq":2,"IsVisibleInDesigner":true})));
                let rel = format!("00000000-0000-0001-0000-{:012}", branch + 1);
                ds["QueryRelation"].as_array_mut().unwrap().push(row(json!({"RelationID":rel,"ParentTableID":owner,"ChildTableID":alias,"JoinType":"Inner","Seq":1})));
                ds["QueryRelationField"].as_array_mut().unwrap().push(row(json!({"RelationID":rel,"ParentFieldName":"SolutionID","ChildFieldName":"SolutionID","CompOp":"=","AndOr":"","Seq":1})));
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
                        payload(&meta[*table], &alias)?
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
            if ti <= 2 {
                ds["QueryWhereItem"].as_array_mut().unwrap().push(row(json!({"CriteriaID":format!("00000000-0000-0002-0000-{:012}",branch+1),"TableID":if ti==0{alias.as_str()}else{owner.as_str()},"FieldName":"Company","CompOp":"=","RValue":crate::report_query::literal(company),"IsConst":true,"CriteriaType":2,"Seq":1,"AndOr":"","Neg":false,"LeftP":"","RightP":"","UseHaving":false})));
            }
        }
    }
    Ok(json!({"queryDS":ds,"executionParams":{"ExecutionParameter":[]}}))
}
fn decode(response: &Value, meta: &Value, company: &str) -> Result<Value, Diagnostic> {
    if response["returnObj"]["Errors"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        return Err(err("The native solution graph returned errors."));
    }
    let rows = response["returnObj"]["Results"]
        .as_array()
        .ok_or_else(|| err("The graph omitted Results."))?;
    let mut graph = json!({});
    for t in TABLES {
        graph[*t] = json!([]);
    }
    let mut totals = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for wire in rows {
        let table = wire["Kind"]
            .as_str()
            .filter(|t| TABLES.contains(t))
            .ok_or_else(|| err("Unknown solution graph table."))?;
        let n = wire["Total"]
            .as_i64()
            .ok_or_else(|| err("Missing solution graph total."))?;
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
        if table == "SolutionHeader" && row["Company"] != company {
            return Err(err("Foreign solution company."));
        }
        graph[table].as_array_mut().unwrap().push(row);
    }
    for t in TABLES {
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
fn identity(kind: &str, row: &Value, company: &str, keys: &[&str]) -> Result<Identity, Diagnostic> {
    let keys = keys
        .iter()
        .map(|k| {
            Ok((
                (*k).into(),
                row[*k]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| err("Missing solution key."))?
                    .into(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, Diagnostic>>()?;
    Ok(Identity {
        category: "C24".into(),
        kind: kind.into(),
        scope: if company.is_empty() {
            Scope::Global
        } else {
            Scope::Company {
                company: company.into(),
            }
        },
        keys,
    })
}
fn same_key(a:&Value,b:&Value)->bool {match(a.as_str(),b.as_str()){(Some(a),Some(b))=>a.eq_ignore_ascii_case(b),_=>false}}
fn records(
    graph: &Value,
    meta: &Value,
    company: &str,
) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    let mut out = vec![];
    let mut roots = BTreeSet::new();
    for root in graph["SolutionHeader"].as_array().unwrap() {
        let id = identity(
            "PersistedSolutionHeader",
            root,
            company,
            &["Company", "SolutionID"],
        )?;
        if !roots.insert(root["SolutionID"].as_str().unwrap().to_ascii_lowercase()) {
            return Err(err("Duplicate solution root."));
        }
        let mut ds = json!({"SolutionHeader":[root]});
        for t in ["SolutionDetail", "SolutionTracker"] {
            ds[t] = json!(
                graph[t]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|r| r["SolutionID"].as_str().is_some_and(
                        |s| s.eq_ignore_ascii_case(root["SolutionID"].as_str().unwrap())
                    ))
                    .collect::<Vec<_>>()
            );
        }
        // Retain exact referenced type context. System types are context, not authored roots.
        let typ = &root["SolutionTypeID"];
        ds["SolutionType"] = json!(
            graph["SolutionType"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| same_key(&r["SolutionTypeID"],typ))
                .collect::<Vec<_>>()
        );
        ds["SolutionTypeDetail"] = json!(
            graph["SolutionTypeDetail"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| same_key(&r["SolutionTypeID"],typ))
                .collect::<Vec<_>>()
        );
        out.push((id, complete(ds, meta)));
    }
    for t in ["SolutionDetail", "SolutionTracker"] {
        for row in graph[t].as_array().unwrap() {
            if !row["SolutionID"]
                .as_str()
                .is_some_and(|s| roots.contains(&s.to_ascii_lowercase()))
            {
                return Err(err("A solution child has no scoped parent."));
            }
        }
    }
    let mut types = BTreeSet::new();
    for root in graph["SolutionType"].as_array().unwrap() {
        let key = root["SolutionTypeID"]
            .as_str()
            .ok_or_else(|| err("Missing solution type identity."))?;
        if !types.insert(key.to_ascii_lowercase()) {
            return Err(err("Duplicate solution type."));
        }
        let system = root["SystemFlag"]
            .as_bool()
            .ok_or_else(|| err("Missing solution type ownership."))?;
        let children: Vec<_> = graph["SolutionTypeDetail"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["SolutionTypeID"].as_str().is_some_and(|s|s.eq_ignore_ascii_case(key)))
            .filter(|r| r["SystemFlag"] == false)
            .collect();
        if system && children.is_empty() {
            continue;
        }
        let id = identity("PersistedSolutionType", root, "", &["SolutionTypeID"])?;
        out.push((id,complete(json!({"SolutionType":[if system{json!({"SolutionTypeID":key})}else{root.clone()}],"SolutionTypeDetail":children,"systemParentContext":system}),meta)));
    }
    for row in graph["SolutionTypeDetail"].as_array().unwrap() {
        if !row["SystemFlag"].is_boolean()
            || !row["SolutionTypeID"]
                .as_str()
                .is_some_and(|s| types.contains(&s.to_ascii_lowercase()))
        {
            return Err(err(
                "A solution type detail has invalid ownership or no parent.",
            ));
        }
    }
    Ok(out)
}
fn complete(mut data: Value, meta: &Value) -> Value {
    data["herbertSources"] = json!({"files":[{"name":"solution-graph.json","text":serde_json::to_string_pretty(&data).unwrap()},{"name":"solution-schema.json","text":serde_json::to_string_pretty(meta).unwrap()}]});
    data["herbertDirectOutput"] = json!(true);
    data["herbertCollectionStatus"] = json!({"completeness":"collected","diagnostics":[]});
    data
}
fn output(mut s: Value) -> Result<GuestReply, Diagnostic> {
    // Metadata graphs have no remaining native dependencies. Send ready records
    // once, leaving the older single-record checkpoint format readable below.
    if s["bulk"] == true {
        let records: Vec<(Identity, Value)> = serde_json::from_value(s["records"].take())
            .map_err(|_| err("Invalid prepared solution records."))?;
        return Ok(GuestReply::InventoryData {
            inventory: Box::new(GuestReply::Inventory {
                identities: records.iter().map(|(id, _)| id.clone()).collect(),
                next_cursor: s["next"].as_str().map(str::to_owned),
                authoritative: true,
                diagnostics: vec![],
            }),
            records,
        });
    }
    let i = s["index"].as_u64().unwrap_or(0) as usize;
    if i == s["records"].as_array().map_or(0, Vec::len) {
        return Ok(GuestReply::Inventory {
            identities: vec![],
            next_cursor: s["next"].as_str().map(str::to_owned),
            authoritative: true,
            diagnostics: vec![],
        });
    }
    let record = serde_json::from_value(s["records"][i].take())
        .map_err(|_| err("Invalid prepared solution output."))?;
    s["index"] = json!(i + 1);
    Ok(GuestReply::ReadyRecords {
        records: vec![record],
        state: s,
    })
}
fn finish(
    records: Vec<(Identity, Value)>,
    next: Option<String>,
    s: &Value,
) -> Result<GuestReply, Diagnostic> {
    let mut selected_records=Vec::new();for record in records {if selected(&record.0,s)?{selected_records.push(record);}}let records=selected_records;
    let state = json!({"phase":"output","records":records,"index":0,"next":next,"bulk":s["bulk"]});
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
fn drive(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if r.operation == "materialize" && r.state["phase"] == "output" {
        return output(r.state);
    }
    if r.operation == "acquire" {
        return Ok(crate::collectors::handle(r));
    }
    if !matches!(r.operation.as_str(), "discover" | "materialize") {
        return Err(Diagnostic::new(
            "UNSUPPORTED_OPERATION",
            "C24 supports discovery, acquisition and materialization.",
        ));
    }
    if r.state["phase"] != "package" {
        if let Some(d) = r.error.take() {
            return Err(d);
        }
    }
    let mut s = r.state;
    if s["phase"].is_null() {
        let (family,_)=selection(&s)?;
        if r.response.is_none()&&r.cursor.is_none()&&family=="retainedPackages"{return Ok(GuestReply::Inventory {identities:vec![],next_cursor:Some("retained".into()),authoritative:true,diagnostics:vec![]});}
        if r.response.is_none()&&r.cursor.as_deref()==Some("retained")&&!matches!(family,"all"|"retainedPackages"){return Ok(GuestReply::Inventory {identities:vec![],next_cursor:None,authoritative:true,diagnostics:vec![]});}
        s = json!({"phase":"schema","table":0,"metadata":{},"prepare":s["_hostPreparePage"],"bulk":s["_hostIndependentWorkItems"]});
        if r.cursor.as_deref() == Some("retained") {
            s["phase"] = json!("catalog");
            return Ok(crate::native(
                "Ice.BO.ExportPackageSvc",
                "GetInstalledSolutions",
                json!({}),
                false,
                s,
            ));
        }
        if r.cursor.is_some() {
            return Err(err("Invalid C24 continuation."));
        }
    }
    let phase = s["phase"].as_str().unwrap_or("").to_string();
    if phase == "schema" {
        let mut i = s["table"].as_u64().unwrap_or(0) as usize;
        if let Some(response) = r.response {
            let fs = response["returnObj"]["TableFieldList"]
                .as_array()
                .ok_or_else(|| err("Native solution field metadata is absent."))?;
            let t = TABLES
                .get(i)
                .ok_or_else(|| err("Invalid schema continuation."))?;
            let mut kept = vec![];
            let mut names = BTreeSet::new();
            for f in fs {
                if f["External"] == true {
                    continue;
                }
                let name = f["FieldName"]
                    .as_str()
                    .ok_or_else(|| err("Missing native field name."))?;
                if f["External"] != false
                    || f["DBTableName"] != *t
                    || !crate::report_query::identifier(name)
                    || !names.insert(name)
                {
                    return Err(err("Invalid native solution schema."));
                }
                kept.push(f.clone());
            }
            if !names.contains("SysRowID") {
                return Err(err("Native solution schema has no row identity."));
            }
            s["metadata"][*t] = json!(kept);
            i += 1;
            s["table"] = json!(i);
        }
        if i < TABLES.len() {
            return Ok(crate::native(
                "Ice.BO.BAQDesignerSvc",
                "GetFieldList",
                json!({"schema":"Ice","dataTableID":TABLES[i]}),
                false,
                s,
            ));
        }
        let args = build(&r.company, &s["metadata"])?;
        s["phase"] = json!("graph");
        return Ok(crate::native(
            "Ice.BO.DynamicQuerySvc",
            "Execute",
            args,
            false,
            s,
        ));
    }
    if phase == "graph" {
        let g = decode(
            &r.response
                .ok_or_else(|| err("Missing solution graph response."))?,
            &s["metadata"],
            &r.company,
        )?;
        let records = records(&g, &s["metadata"], &r.company)?;
        return finish(records, Some("retained".into()), &s);
    }
    if phase == "catalog" {
        let response = r
            .response
            .ok_or_else(|| err("Missing retained solution catalog."))?;
        let rows = crate::retained_solutions::catalog(&response, &r.company)?;
        let mut selected_rows=Vec::new();for row in rows {if selected(&row.0,&s)?{selected_rows.push(row);}}let rows=selected_rows;
        if rows.is_empty() {
            return finish(vec![], None, &s);
        }
        // Each opaque package has its own native API; emit its output immediately.
        if s["bulk"] == true {
            let count = rows.len();
            let items: Vec<_> = rows.into_iter().map(|row|
                json!({"phase":"package-request","packages":[row],"packageIndex":0})).collect();
            return Ok(GuestReply::PreparedPage {
                state: json!({"_hostWorkItems":items,"_hostWorkNextCursor":null,"_progress":{"pageRecords":count}}),
                next_cursor: None,
                next_state: json!({}),
            });
        }
        s["packages"] = json!(rows);
        s["packageIndex"] = json!(0);
        s["phase"] = json!("package-request");
        return Ok(GuestReply::PreparedPage {
            state: s,
            next_cursor: None,
            next_state: json!({}),
        });
    }
    if phase == "package-request" {
        let i = s["packageIndex"].as_u64().unwrap() as usize;
        let sid = s["packages"][i][0]["keys"]["SolutionID"].as_str().unwrap();
        let args = json!({"solutionID":sid});
        s["phase"] = json!("package");
        return Ok(crate::native(
            "Ice.BO.ExportPackageSvc",
            "GetInstalledSolution",
            args,
            true,
            s,
        ));
    }
    if phase == "package" {
        let i = s["packageIndex"].as_u64().unwrap() as usize;
        let (id, metadata): (Identity, Value) = serde_json::from_value(s["packages"][i].clone())
            .map_err(|_| err("Invalid retained solution checkpoint."))?;
        let response = r.response.unwrap_or(Value::Null);
        let signature = if let Some(d) = r.error {
            Err(d)
        } else {
            crate::retained_solutions::package_signature(&response)
        };
        let (status, diagnostics) = match signature.as_ref() {
            Ok(_) => ("collected", vec![]),
            Err(e) => ("partial", vec![e.clone()]),
        };
        let mut data = json!({"metadata":metadata,"package":response,"packageSignature":signature.ok(),"herbertCollectionStatus":{"completeness":status,"diagnostics":diagnostics}});
        // Copy exact native bytes through the generic direct-source writer.
        data["herbertSources"] = json!({"files":[{"name":"installed-solution.json","text":serde_json::to_string_pretty(&data).unwrap()}]});
        crate::retained_solutions::declare_package_output(&mut data);
        s["phase"] = json!("next-package");
        return Ok(GuestReply::ReadyRecords {
            records: vec![(id, data)],
            state: s,
        });
    }
    if phase == "next-package" {
        let i = s["packageIndex"].as_u64().unwrap() as usize + 1;
        if i == s["packages"].as_array().unwrap().len() {
            return output(json!({"records":[],"index":0,"next":null}));
        }
        s["packageIndex"] = json!(i);
        s["phase"] = json!("package");
        let sid = s["packages"][i][0]["keys"]["SolutionID"].as_str().unwrap();
        return Ok(crate::native(
            "Ice.BO.ExportPackageSvc",
            "GetInstalledSolution",
            json!({"solutionID":sid}),
            true,
            s,
        ));
    }
    Err(err("Unknown solution collection phase."))
}

#[cfg(test)]
mod bulk_output_tests {
    use super::*;
    #[test]
    fn retained_solution_items_preserve_native_bytes_and_first_ready_output() {
        let catalog:Vec<_>=["A","B"].into_iter().map(|id|json!({"CompanyID":"C","SolutionID":id,"Description":"original","FileName":"original.zip","BuildIteration":1,"SolutionReference":"","ChangedBy":"","ChangedOn":"","Future":"keep"})).collect();
        let r=GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"C24".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state:json!({"phase":"catalog","bulk":true}),response:Some(json!({"returnObj":catalog})),error:None};
        let GuestReply::PreparedPage{state,..}=drive(r.clone()).unwrap()else{panic!()};
        assert_eq!(state["_hostWorkItems"].as_array().unwrap().len(),2);
        for (index,item) in state["_hostWorkItems"].as_array().unwrap().iter().enumerate(){
            assert_eq!(item["packages"].as_array().unwrap().len(),1);
            let mut child=GuestRequest{operation:"materialize".into(),state:item.clone(),response:None,..r.clone()};
            let GuestReply::Action{action:HostAction::NativeRead{method,arguments,..},state}=drive(child.clone()).unwrap()else{panic!()};
            assert_eq!(method,"GetInstalledSolution");assert_eq!(arguments["solutionID"],if index==0{"A"}else{"B"});
            let response=json!({"returnObj":"11111111-1111-1111-1111-111111111111","parameters":{"content":"UEsDBAo="}});
            child.state=state;child.response=Some(response.clone());
            let GuestReply::ReadyRecords{records,state}=drive(child.clone()).unwrap()else{panic!()};
            assert_eq!(records.len(),1);assert_eq!(records[0].1["package"],response);assert_eq!(records[0].1["metadata"]["Future"],"keep");
            child.state=state;child.response=None;
            assert!(matches!(drive(child).unwrap(),GuestReply::Inventory{next_cursor:None,..}));
        }
    }
    #[test]
    fn solution_bulk_preserves_exact_graph_files_and_legacy_output() {
        let records:Vec<(Identity,Value)>=(0..37).map(|n|(Identity{category:"C24".into(),kind:"PersistedSolutionHeader".into(),scope:Scope::Global,keys:BTreeMap::from([("SolutionID".into(),n.to_string())])},json!({"herbertSources":{"files":[{"name":"solution-graph.json","text":"é exact"}]},"unknown":{"original":n}}))).collect();
        let mut legacy=json!({"records":records,"index":0,"next":"retained"});let mut expected=Vec::new();
        loop{match output(legacy).unwrap(){
            GuestReply::ReadyRecords{records,state}=>{expected.extend(records);legacy=state;},
            GuestReply::Inventory{next_cursor,..}=>{assert_eq!(next_cursor.as_deref(),Some("retained"));break;},
            other=>panic!("{other:?}"),
        }}
        for bulk in [false,true]{
            let GuestReply::PreparedPage{state,next_cursor,..}=finish(records.clone(),Some("retained".into()),&json!({"prepare":true,"bulk":bulk})).unwrap()else{panic!()};
            assert_eq!(next_cursor.as_deref(),Some("retained"));
            let result=output(state).unwrap();
            if bulk{let GuestReply::InventoryData{records:actual,..}=result else{panic!()};assert_eq!(actual,expected);}else{assert!(matches!(result,GuestReply::ReadyRecords{..}));}
        }
        assert!(matches!(finish(vec![],None,&json!({"bulk":true})).unwrap(),GuestReply::InventoryData{records,..}if records.is_empty()));
    }
}
pub fn handle(r: GuestRequest) -> GuestReply {
    let from=r.state.clone();if r.operation!="acquire"{if let Err(diagnostic)=selection(&from){return GuestReply::Failure {diagnostic};}}carry(handle_inner(r),&from)
}
fn handle_inner(r: GuestRequest) -> GuestReply {
    let independent = r.operation == "discover" && r.cursor.as_deref() != Some("retained");
    match drive(r) {
        Ok(v) => v,
        Err(diagnostic) if independent => GuestReply::Inventory {
            identities: vec![],
            next_cursor: Some("retained".into()),
            authoritative: false,
            diagnostics: vec![diagnostic],
        },
        Err(diagnostic) => GuestReply::Failure { diagnostic },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph() -> Value {
        json!({"SolutionHeader":[{"Company":"C","SolutionID":"S","SolutionTypeID":"T","SysRowID":"s"}],"SolutionDetail":[{"SolutionID":"S","SysRowID":"d","ForeignSysRowID":"original","TableName":"Ice.UDCodeType"}],"SolutionTracker":[{"SolutionID":"S","SysRowID":"r","LastUpdatedBy":"user"}],"SolutionType":[{"SolutionTypeID":"T","SystemFlag":false,"SysRowID":"t"},{"SolutionTypeID":"Stock","SystemFlag":true,"SysRowID":"stock"}],"SolutionTypeDetail":[{"SolutionTypeID":"T","SystemFlag":false,"SysRowID":"td","ElementHeaderID":"UDCodeType"},{"SolutionTypeID":"Stock","SystemFlag":false,"SysRowID":"custom","ElementHeaderID":"Custom"}]})
    }
    #[test]
    fn closure_and_custom_children_preserved() {
        let r = records(&graph(), &json!({}), "C").unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].1["SolutionDetail"][0]["ForeignSysRowID"], "original");
        assert_eq!(r[0].1["SolutionTracker"][0]["LastUpdatedBy"], "user");
        assert_eq!(
            r[0].1["SolutionTypeDetail"][0]["ElementHeaderID"],
            "UDCodeType"
        );
        let stock = &r[2].1;
        assert_eq!(stock["SolutionType"][0], json!({"SolutionTypeID":"Stock"}));
        assert_eq!(stock["SolutionTypeDetail"][0]["ElementHeaderID"], "Custom");
    }
    #[test]
    fn orphan_members_and_type_children_are_rejected() {
        for (table, key) in [
            ("SolutionDetail", "SolutionID"),
            ("SolutionTracker", "SolutionID"),
            ("SolutionTypeDetail", "SolutionTypeID"),
        ] {
            let mut g = graph();
            g[table][0][key] = json!("Missing");
            assert!(records(&g, &json!({}), "C").is_err());
        }
    }
    #[test]
    fn manifest_detects_missing_extra_and_duplicate_rows() {
        let meta = json!({"SolutionHeader":[{"FieldName":"Company","DataType":"nvarchar"},{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]});
        let mut rows: Vec<Value> = TABLES
            .iter()
            .map(|t| json!({"Kind":t,"Payload":null,"Total":0}))
            .collect();
        assert!(decode(&json!({"returnObj":{"Results":rows}}), &meta, "C").is_ok());
        rows[0]["Total"] = json!(1);
        assert!(decode(&json!({"returnObj":{"Results":rows}}), &meta, "C").is_err());
        rows.push(json!({"Kind":"SolutionHeader","Payload":"[\"C\",\"id\"]","Total":-1}));
        assert!(decode(&json!({"returnObj":{"Results":rows}}), &meta, "C").is_ok());
        rows.push(rows.last().unwrap().clone());
        assert!(decode(&json!({"returnObj":{"Results":rows}}), &meta, "C").is_err());
    }
    #[test]
    fn graph_query_has_one_data_and_count_branch_per_table_and_scoped_children() {
        let meta: Value = Value::Object(
            TABLES
                .iter()
                .map(|t| {
                    (
                        (*t).into(),
                        json!([{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]),
                    )
                })
                .collect(),
        );
        let q = build("C", &meta).unwrap();
        assert_eq!(q["queryDS"]["QuerySubQuery"].as_array().unwrap().len(), 10);
        assert_eq!(q["queryDS"]["QueryRelation"].as_array().unwrap().len(), 4);
        assert_eq!(q["queryDS"]["QueryWhereItem"].as_array().unwrap().len(), 6);
        assert!(build("", &meta).is_err());
    }

    #[test]
    fn wave1_retained_package_arrays_emit_identical_bytes_and_catalog_metadata() {
        let meta=json!({"CompanyID":"C","SolutionID":"S","Description":"D","FileName":"S.zip","BuildIteration":1,"SolutionReference":"R","ChangedBy":"u","ChangedOn":null,"InstallLog":"history","FutureField":{"value":7}});
        let rows=crate::retained_solutions::catalog(&json!({"returnObj":[meta]}),"C").unwrap();
        let mut r=GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"C24".into(),company:"C".into(),identity:None,cursor:None,page_size:None,state:json!({"phase":"package","packageIndex":0,"packages":rows}),response:None,error:None};
        let mut signatures=vec![];
        for payload in [json!("UEs="),json!([80,75])] {
            r.response=Some(json!({"returnObj":"00000000-0000-0000-0000-000000000001","parameters":{"content":payload}}));
            let GuestReply::ReadyRecords{records,..}=handle(r.clone()) else{panic!()};
            assert_eq!(records[0].1["metadata"],meta);
            assert_eq!(records[0].1["herbertDirectOutput"],true);
            assert_eq!(records[0].1["herbertSources"]["binaryFiles"][0]["name"],"package-S.zip");
            assert_eq!(records[0].1["package"]["parameters"]["content"],payload);
            assert_eq!(records[0].1["herbertSources"]["binaryFiles"][0]["base64"],"UEs=");
            assert_eq!(records[0].1["herbertCollectionStatus"]["completeness"],"collected");
            signatures.push(records[0].1["packageSignature"].clone());
        }
        assert_eq!(signatures[0],signatures[1]);
        r.operation="acquire".into();r.identity=Some(rows[0].0.clone());r.state=Value::Null;r.response=None;
        let GuestReply::Action{state,..}=handle(r.clone()) else{panic!()};
        r.state=state;r.response=Some(json!({"returnObj":[meta]}));
        let GuestReply::Action{state,..}=handle(r.clone()) else{panic!()};
        assert_eq!(state["inner"]["metadata"],meta);
        r.state=state;r.response=Some(json!({"returnObj":"00000000-0000-0000-0000-000000000001","parameters":{"content":[80,75]}}));
        let GuestReply::Definition{definition,completeness,..}=handle(r) else{panic!()};
        assert_eq!(completeness,Completeness::Collected);
        assert_eq!(definition["metadata"],meta);
        assert_eq!(definition["packageSignature"],signatures[0]);
    }

}

#[cfg(test)]
mod selection_tests {
    use super::*;
    fn request(state:Value)->GuestRequest{GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"C24".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state,response:None,error:None}}
    fn graph()->Value{json!({"SolutionHeader":[{"Company":"C","SolutionID":"A Ω","SolutionTypeID":"T","SysRowID":"a"},{"Company":"C","SolutionID":"B","SolutionTypeID":"T","SysRowID":"b"}],"SolutionDetail":[{"SolutionID":"A Ω","SysRowID":"da","ForeignSysRowID":"original A"},{"SolutionID":"B","SysRowID":"db","ForeignSysRowID":"original B"}],"SolutionTracker":[{"SolutionID":"A Ω","SysRowID":"ra"},{"SolutionID":"B","SysRowID":"rb"}],"SolutionType":[{"SolutionTypeID":"T","SystemFlag":false,"SysRowID":"t"},{"SolutionTypeID":"Stock","SystemFlag":true,"SysRowID":"stock"}],"SolutionTypeDetail":[{"SolutionTypeID":"T","SystemFlag":false,"SysRowID":"td"},{"SolutionTypeID":"Stock","SystemFlag":false,"SysRowID":"custom"}]})}
    #[test]
    fn whole_solution_closure_and_independent_types_survive_literal_prefix(){
        let all=records(&graph(),&json!({}),"C").unwrap();assert_eq!(all.len(),4);for family in ["all","solutions","solutionTypes","retainedPackages"]{for prefix in ["A Ω","missing",""]{let state=json!({"prepare":true,"_pluginSettings":{"definitionFamily":family,"solutionIdPrefix":prefix}});let GuestReply::PreparedPage{state:out,..}=finish(all.clone(),None,&state).unwrap()else{panic!()};let actual:Vec<(Identity,Value)>=serde_json::from_value(out["records"].clone()).unwrap();let expected:Vec<_>=all.iter().filter(|(id,_)|selected(id,&state).unwrap()).cloned().collect();assert_eq!(actual,expected);for (id,data) in actual{if id.kind=="PersistedSolutionHeader"{assert_eq!(data["SolutionTypeDetail"].as_array().unwrap().len(),1);assert_eq!(data["SolutionDetail"].as_array().unwrap().len(),1);assert_eq!(data["SolutionTracker"].as_array().unwrap().len(),1);}}}}
        let mut invalid=graph();invalid["SolutionDetail"][1]["SolutionID"]=json!("orphan");assert!(records(&invalid,&json!({}),"C").is_err());
    }
    #[test]
    fn full_catalog_validation_precedes_selected_only_original_package_reads(){
        let catalog:Vec<_>=["A Ω","B"].into_iter().map(|id|json!({"CompanyID":"C","SolutionID":id,"Description":"original","FileName":"original.zip","BuildIteration":1,"SolutionReference":"","ChangedBy":"","ChangedOn":"","Future":"keep"})).collect();
        for bulk in [false,true]{let mut r=request(json!({"phase":"catalog","bulk":bulk,"_pluginSettings":{"definitionFamily":"retainedPackages","solutionIdPrefix":"A Ω"}}));r.cursor=Some("retained".into());r.response=Some(json!({"returnObj":catalog}));let GuestReply::PreparedPage{state,next_state,..}=handle(r.clone())else{panic!()};assert_eq!(next_state["_pluginSettings"]["solutionIdPrefix"],"A Ω");if !bulk{assert!(state.get("_hostWorkItems").is_none());}r.state=if bulk{assert_eq!(state["_hostWorkItems"].as_array().unwrap().len(),1);state["_hostWorkItems"][0].clone()}else{state};r.operation="materialize".into();r.response=None;let GuestReply::Action{action:HostAction::NativeRead{arguments,..},state}=handle(r.clone())else{panic!()};assert_eq!(arguments["solutionID"],"A Ω");r.state=state;let response=json!({"returnObj":"11111111-1111-1111-1111-111111111111","parameters":{"content":[80,75,3,4,10]}});r.response=Some(response.clone());let GuestReply::ReadyRecords{records,state}=handle(r)else{panic!()};assert_eq!(records[0].1["package"],response);assert_eq!(records[0].1["metadata"]["Future"],"keep");assert_eq!(state["_pluginSettings"]["solutionIdPrefix"],"A Ω");}
        let mut bad=catalog;bad[1]["CompanyID"]=json!("FOREIGN");let mut r=request(json!({"phase":"catalog","_pluginSettings":{"definitionFamily":"retainedPackages","solutionIdPrefix":"A Ω"}}));r.cursor=Some("retained".into());r.response=Some(json!({"returnObj":bad}));assert!(matches!(handle(r),GuestReply::Failure{..}));
    }
    #[test]
    fn unacquired_families_skip_independently_but_acquired_graph_error_remains(){
        let r=request(json!({"_pluginSettings":{"definitionFamily":"retainedPackages","solutionIdPrefix":""}}));assert!(matches!(handle(r.clone()),GuestReply::Inventory{authoritative:true,next_cursor:Some(next),..}if next=="retained"));let mut failed=r;failed.state["phase"]=json!("graph");failed.error=Some(Diagnostic::new("DENIED","acquired graph"));assert!(matches!(handle(failed),GuestReply::Inventory{authoritative:false,next_cursor:Some(next),diagnostics,..}if next=="retained"&&diagnostics[0].code=="DENIED"));
        let mut r=request(json!({"_pluginSettings":{"definitionFamily":"solutions","solutionIdPrefix":""}}));r.cursor=Some("retained".into());assert!(matches!(handle(r),GuestReply::Inventory{authoritative:true,next_cursor:None,..}));
    }
}
