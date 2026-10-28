//! Acquire a complete selected posting graph before local serialization.
//! One schema read and one complete company graph, including linked-map source.
//! There are no root windows or independent child-table pages.
use crate::business_pages::{POSTING_TABLES, finish_selected, identity};
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn err(s: &str) -> Diagnostic {
    Diagnostic::new("BUSINESS_GRAPH_INVALID", s)
}
fn tables() -> impl Iterator<Item = &'static str> {
    POSTING_TABLES.iter().copied().chain(["COAMap"])
}
fn fields<'a>(state: &'a Value, table: &str) -> Result<&'a Value, Diagnostic> {
    state["metadata"]
        .get(table)
        .ok_or_else(|| err("The graph schema omitted a required table."))
}
fn schema() -> Value {
    let names = POSTING_TABLES
        .iter()
        .copied()
        .chain(["COAMap"])
        .map(|t| format!("DataTableID = '{t}'"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut args = json!({"whereClauseZDataTable":format!("SystemCode = 'Erp' AND ({names})"),"whereClauseZDataField":"","pageSize":POSTING_TABLES.len()+1,"absolutePage":1});
    for t in [
        "ZLookupLink",
        "ZLinkColumn",
        "ZLookupField",
        "ZKey",
        "ZKeyField",
    ] {
        args[format!("whereClause{t}")] = json!("DataTableID = '__HerbertGraphNoChildren__'");
    }
    args
}
fn metadata(response: &Value) -> Result<Value, Diagnostic> {
    let body = &response["returnObj"];
    if response["parameters"]["morePages"] != false {
        return Err(err("The native graph schema was incomplete."));
    }
    let roots = body["ZDataTable"]
        .as_array()
        .ok_or_else(|| err("The native graph schema omitted tables."))?;
    let expected: BTreeSet<_> = POSTING_TABLES.iter().copied().chain(["COAMap"]).collect();
    let mut found = BTreeSet::new();
    for r in roots {
        let name = r["DataTableID"]
            .as_str()
            .ok_or_else(|| err("A schema table has no identity."))?;
        if !expected.contains(name)
            || !r["SystemCode"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("Erp"))
            || !found.insert(name)
        {
            return Err(err("The graph schema has foreign or repeated tables."));
        }
    }
    if found != expected {
        return Err(err("The graph schema is missing tables."));
    }
    let mut result = json!({});
    for t in &expected {
        result[*t] = json!([]);
    }
    for f in body["ZDataField"]
        .as_array()
        .ok_or_else(|| err("The graph schema omitted fields."))?
    {
        let table = f["DataTableID"]
            .as_str()
            .ok_or_else(|| err("A graph field has no table."))?;
        if !expected.contains(table) {
            return Err(err("A graph field belongs to a foreign table."));
        }
        if f["External"] == true {
            continue;
        }
        if f["External"] != false {
            return Err(err(
                "A graph field has no physical/external classification.",
            ));
        }
        let name = f["FieldName"]
            .as_str()
            .ok_or_else(|| err("A graph field has no name."))?;
        if !crate::report_query::identifier(name) || f["DBTableName"] != table {
            return Err(err("A graph field has an invalid physical mapping."));
        }
        if result[table]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["FieldName"] == name)
        {
            return Err(err("The graph schema repeats a field."));
        }
        result[table].as_array_mut().unwrap().push(f.clone());
    }
    for t in expected {
        let fs = result[t].as_array().unwrap();
        for name in [
            "Company",
            "SysRowID",
            if t == "COAMap" {
                "COAMapUID"
            } else {
                "ACTTypeUID"
            },
        ] {
            if !fs.iter().any(|f| f["FieldName"] == name) {
                return Err(err("The graph schema lacks ownership or row identity."));
            }
        }
    }
    Ok(result)
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
pub(super) fn build(company: &str, metadata: &Value, uids: &[i64]) -> Result<Value, Diagnostic> {
    if company.is_empty() {
        return Err(err("An unscoped company graph read is prohibited."));
    }
    let mut ds = json!({"DynamicQuery":[{"Company":company,"QueryID":"HerbertPostingGraph","Description":"Unsaved complete selected posting graph","Updatable":false,"ExtQuery":false,"IsShared":false,"AllCompanies":false,"XCompany":true,"RowMod":"A"}],"QuerySubQuery":[],"QueryTable":[],"QueryField":[],"QueryWhereItem":[],"QuerySortBy":[],"QueryParameter":[],"QueryCtrl":[],"QueryRelation":[],"QueryRelationField":[]});
    for (ti, table) in tables().enumerate() {
        for marker in [false, true] {
            let branch = ti * 2 + usize::from(marker);
            let alias = format!("Source{branch}");
            let sub = format!("00000000-0000-0000-0000-{:012}", branch + 1);
            let row = |extra: Value| {
                let mut r = json!({"Company":company,"QueryID":"HerbertPostingGraph","SubQueryID":sub,"RowMod":"A"});
                r.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                r
            };
            ds["QuerySubQuery"].as_array_mut().unwrap().push(row(json!({"Name":alias,"Type":if branch==0{"TopLevel"}else{"UnionAll"},"Seq":branch+1})));
            ds["QueryTable"].as_array_mut().unwrap().extend([row(json!({"TableID":alias,"DBSchemaName":"Erp","DBTableName":table,"TableType":"DB","Seq":1,"IsVisibleInDesigner":true})),row(json!({"TableID":"Calculated","TableType":"TT","Seq":-1,"IsVisibleInDesigner":false}))]);
            let fs = metadata
                .get(table)
                .ok_or_else(|| err("A graph table has no schema."))?;
            let projected = [
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
                        payload(fs, &alias)?
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
            ];
            for (i, (name, ty, formula)) in projected.into_iter().enumerate() {
                ds["QueryField"].as_array_mut().unwrap().push(row(json!({"TableID":"Calculated","FieldName":name,"Alias":name,"DataType":ty,"FieldFormat":"","FieldLabel":name,"Seq":i+1,"IsCalculated":true,"Formula":formula,"Updatable":false})));
            }
            let mut criteria = vec![("Company", crate::report_query::literal(company), "", "", "")];
            // ACTTypeUID=0 identifies standalone GL-book validation defaults,
            // not a posting-type child. Those belong to GL-book configuration.
            if table == "BVRule" {
                criteria.push(("ACTTypeUID", "0".into(), "And", "", ""));
            }
            for (i, uid) in uids.iter().enumerate().filter(|_| table != "COAMap") {
                criteria.push((
                    "ACTTypeUID",
                    uid.to_string(),
                    if i == 0 { "And" } else { "Or" },
                    if i == 0 { "(" } else { "" },
                    if i + 1 == uids.len() { ")" } else { "" },
                ));
            }
            for (i, (field, value, logic, left, right)) in criteria.into_iter().enumerate() {
                ds["QueryWhereItem"].as_array_mut().unwrap().push(row(json!({"CriteriaID":format!("00000000-0000-0000-{:04}-{:012}",branch+1,i+1),"TableID":alias,"FieldName":field,"CompOp":if table=="BVRule"&&field=="ACTTypeUID"&&value=="0"{">"}else{"="},"RValue":value,"IsConst":true,"CriteriaType":2,"Seq":i+1,"AndOr":logic,"Neg":false,"LeftP":left,"RightP":right,"UseHaving":false})));
            }
        }
    }
    Ok(json!({"queryDS":ds,"executionParams":{"ExecutionParameter":[]}}))
}
fn results(response: &Value) -> Result<&Vec<Value>, Diagnostic> {
    if response["returnObj"]["Errors"]
        .as_array()
        .is_some_and(|r| !r.is_empty())
    {
        return Err(err("The native graph query returned errors."));
    }
    response["returnObj"]["Results"]
        .as_array()
        .ok_or_else(|| err("The native graph query omitted Results."))
}
pub(super) fn drive(r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if let Some(d) = r.error {
        return Err(d);
    }
    let mut state = r.state;
    if state["phase"].is_null() {
        state = json!({"phase":"graph-schema","company":r.company,"records":[],"prepare":state["_hostPreparePage"],"independent":state["_hostIndependentWorkItems"],"_target":state["_target"]});
        return Ok(GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.BO.ZDataTableSvc".into(),
                method: "GetRows".into(),
                arguments: schema(),
                export: false,
            },
            state,
        });
    }
    let response = r.response.ok_or_else(|| err("Missing graph response."))?;
    if state["phase"] == "graph-schema" {
        state["metadata"] = metadata(&response)?;
        let uids = if let Some(s) = state["_target"]["keys"]["ACTTypeUID"].as_str() {
            vec![
                s.parse::<i32>()
                    .map_err(|_| err("Target posting UID is not an integer."))?
                    as i64,
            ]
        } else {
            vec![]
        };
        let args = build(
            state["company"].as_str().unwrap(),
            &state["metadata"],
            &uids,
        )?;
        state["phase"] = json!("graph-rows");
        return Ok(crate::native(
            "Ice.BO.DynamicQuerySvc",
            "Execute",
            args,
            false,
            state,
        ));
    }
    if state["phase"] != "graph-rows" {
        return Err(err("Unknown graph collection phase."));
    }
    let graph = decode(results(&response)?, &state)?;
    let records = group_records(&graph, &state)?;
    Ok(finish_selected(records, None, state["prepare"] == true, state["independent"] == true,&state))
}
fn group_records(graph: &BTreeMap<String, Vec<Value>>, state: &Value) -> Result<Vec<(Identity, Value)>, Diagnostic> {
    // Index once, preserving each table's native order within an owner.
    let indexed:BTreeMap<_,BTreeMap<i64,Vec<&Value>>>=POSTING_TABLES.iter().map(|table|{
        let mut owners:BTreeMap<i64,Vec<&Value>>=BTreeMap::new();
        for row in &graph[*table] {
            if let Some(uid)=row["ACTTypeUID"].as_i64(){owners.entry(uid).or_default().push(row);}
        }
        (*table,owners)
    }).collect();
    let mut map_index:BTreeMap<i64,Vec<(usize,&Value)>>=BTreeMap::new();
    for (position,row) in graph["COAMap"].iter().enumerate(){
        if let Some(uid)=row["COAMapUID"].as_i64(){map_index.entry(uid).or_default().push((position,row));}
    }
    let mut records = vec![];
    for root in &graph["ACTType"] {
        let uid = root["ACTTypeUID"]
            .as_i64()
            .ok_or_else(|| err("Posting root omitted integer identity."))?;
        if let Some(target) = state["_target"]["keys"]["ACTTypeUID"].as_str() {
            if target != uid.to_string() {
                return Err(err("The targeted graph returned a different root."));
            }
        }
        let mut data = json!({});
        for t in POSTING_TABLES {
            data[*t] = json!(indexed[*t].get(&uid).cloned().unwrap_or_default());
        }
        let ids: BTreeSet<_> = data["ACTBook"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b["COAMapUID"].as_i64())
            .filter(|n| *n > 0)
            .collect();
        let mut maps:Vec<_> = ids.iter().filter_map(|uid|map_index.get(uid)).flatten().copied().collect();
        maps.sort_by_key(|(position,_)|*position);
        if maps.len() != ids.len() {
            return Err(err(
                "A referenced COA map is missing from the complete graph.",
            ));
        }
        data["COAMap"] = json!(maps.into_iter().map(|(_,row)|row).collect::<Vec<_>>());
        records.push((
            identity(
                "PostingRules",
                state["company"].as_str().unwrap(),
                "ACTTypeUID",
                uid.to_string(),
            ),
            data,
        ));
    }
    Ok(records)
}

#[cfg(test)]
mod grouping_tests {
    use super::*;
    #[test]
    fn indexed_posting_groups_match_scan_order_unknown_fields_and_map_checks() {
        let mut graph:BTreeMap<String,Vec<Value>>=tables().map(|table|(table.into(),vec![])).collect();
        graph.insert("ACTType".into(),vec![json!({"ACTTypeUID":2,"unknown":"second"}),json!({"ACTTypeUID":1,"unknown":"first"})]);
        for table in &POSTING_TABLES[1..]{graph.insert((*table).into(),vec![json!({"ACTTypeUID":1,"ordinal":0,"COAMapUID":8}),json!({"ACTTypeUID":2,"ordinal":1,"COAMapUID":7}),json!({"ACTTypeUID":1,"ordinal":2,"COAMapUID":7}),json!({"ACTTypeUID":99,"unrelated":true})]);}
        graph.insert("COAMap".into(),vec![json!({"COAMapUID":8,"unknown":"é"}),json!({"COAMapUID":99,"unrelated":true}),json!({"COAMapUID":7})]);
        let actual=group_records(&graph,&json!({"company":"C"})).unwrap();
        for (id,data) in actual{
            let uid=id.keys["ACTTypeUID"].parse::<i64>().unwrap();
            for table in POSTING_TABLES{assert_eq!(data[*table],json!(graph[*table].iter().filter(|row|row["ACTTypeUID"]==uid).collect::<Vec<_>>()));}
            let ids:BTreeSet<_>=data["ACTBook"].as_array().unwrap().iter().filter_map(|row|row["COAMapUID"].as_i64()).collect();
            assert_eq!(data["COAMap"],json!(graph["COAMap"].iter().filter(|row|row["COAMapUID"].as_i64().is_some_and(|id|ids.contains(&id))).collect::<Vec<_>>()));
        }
        graph.get_mut("COAMap").unwrap().retain(|row|row["COAMapUID"]!=7);
        assert!(group_records(&graph,&json!({"company":"C"})).is_err());
        assert!(group_records(&graph,&json!({"company":"C","_target":{"keys":{"ACTTypeUID":"1"}}})).is_err());
    }
}
pub(super) fn decode(
    rows: &[Value],
    state: &Value,
) -> Result<BTreeMap<String, Vec<Value>>, Diagnostic> {
    let mut graph: BTreeMap<String, Vec<Value>> =
        tables().map(|t| (t.to_string(), vec![])).collect();
    let mut totals = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for wire in rows {
        let table = wire["Kind"]
            .as_str()
            .filter(|t| POSTING_TABLES.contains(t) || *t == "COAMap")
            .ok_or_else(|| err("The bulk graph returned an unknown table."))?;
        let total = wire["Total"]
            .as_i64()
            .ok_or_else(|| err("A graph row has no validation total."))?;
        if total >= 0 {
            if !wire["Payload"].is_null()
                || !wire["RowId"].is_null()
                || totals.insert(table, total as usize).is_some()
            {
                return Err(err("The bulk graph repeats or corrupts a table manifest."));
            }
            continue;
        }
        if total != -1 {
            return Err(err("Invalid graph row marker."));
        }
        let values: Value = serde_json::from_str(
            wire["Payload"]
                .as_str()
                .ok_or_else(|| err("A graph row omitted its payload."))?,
        )
        .map_err(|_| err("A graph payload is invalid JSON."))?;
        let fs = fields(state, table)?.as_array().unwrap();
        if values.as_array().is_none_or(|a| a.len() != fs.len()) {
            return Err(err("A graph payload omitted or added native fields."));
        }
        let mut data: Value = Value::Object(
            fs.iter()
                .zip(values.as_array().unwrap())
                .map(|(f, v)| (f["FieldName"].as_str().unwrap().to_owned(), v.clone()))
                .collect(),
        );
        // Match the native JSON DateTime spelling after SQL style 126. This
        // applies only to typed date/time columns, never authored string values.
        for f in fs {
            if matches!(
                f["DataType"].as_str(),
                Some("datetime" | "datetime2" | "smalldatetime")
            ) {
                let name = f["FieldName"].as_str().unwrap();
                if let Some(s) = data[name].as_str().filter(|s| s.contains('.')) {
                    data[name] = json!(s.trim_end_matches('0').trim_end_matches('.'));
                }
            }
        }
        let rid = data["SysRowID"]
            .as_str()
            .ok_or_else(|| err("A graph payload omitted SysRowID."))?;
        if !seen.insert((table, rid.to_ascii_lowercase())) || data["Company"] != state["company"] {
            return Err(err("A graph row repeats or has foreign ownership."));
        }
        graph.get_mut(table).unwrap().push(data);
    }
    for t in tables() {
        if totals.get(t).copied() != Some(graph[t].len()) {
            return Err(err(
                "The bulk graph is truncated or missing a table manifest.",
            ));
        }
    }
    let mut uids = BTreeSet::new();
    for r in &graph["ACTType"] {
        if !uids.insert(
            r["ACTTypeUID"]
                .as_i64()
                .ok_or_else(|| err("A posting root omitted integer identity."))?,
        ) {
            return Err(err("The complete graph repeats a posting identity."));
        }
    }
    for t in POSTING_TABLES {
        for r in &graph[*t] {
            if !r["ACTTypeUID"].as_i64().is_some_and(|n| uids.contains(&n)) {
                return Err(err("A posting child has no root in the complete graph."));
            }
        }
    }

    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> (Value, Vec<Value>) {
        let root = json!({"Company":"TESTCO","ACTTypeUID":7,"SysRowID":"12345678-1234-1234-1234-123456789abc"});
        let mut meta = json!({});
        let mut wire = vec![];
        for t in tables() {
            meta[t] = json!([{"FieldName":"Company","DataType":"nvarchar"},{"FieldName":"ACTTypeUID","DataType":"int"},{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]);
            wire.push(
                json!({"Kind":t,"RowId":null,"Payload":null,"Total":if t=="ACTType"{1}else{0}}),
            );
        }
        wire.push(json!({"Kind":"ACTType","RowId":root["SysRowID"],"Payload":json!(["TESTCO",7,"12345678-1234-1234-1234-123456789abc"]).to_string(),"Total":-1}));
        (
            json!({"company":"TESTCO","metadata":meta,"records":[[{}, {"ACTType":[root]}]]}),
            wire,
        )
    }
    #[test]
    fn manifests_detect_truncation_omitted_empty_tables_duplicate_and_foreign_rows() {
        let (s, w) = sample();
        assert_eq!(decode(&w, &s).unwrap()["ACTType"].len(), 1);
        let mut bad = w.clone();
        bad.pop();
        assert!(decode(&bad, &s).is_err());
        let mut bad = w.clone();
        bad.remove(3);
        assert!(decode(&bad, &s).is_err());
        let mut bad = w.clone();
        bad.push(w.last().unwrap().clone());
        assert!(decode(&bad, &s).is_err());
        let mut bad = w.clone();
        bad[0]["Total"] = json!(2);
        assert!(decode(&bad, &s).is_err());
        let mut bad = w.clone();
        bad.last_mut().unwrap()["Payload"] = json!(
            "{\"Company\":\"FOREIGN\",\"ACTTypeUID\":7,\"SysRowID\":\"12345678-1234-1234-1234-123456789abc\"}"
        );
        assert!(decode(&bad, &s).is_err());
        let mut bad = w.clone();
        bad.last_mut().unwrap()["Payload"] = json!("[\"TESTCO\",7]");
        assert!(decode(&bad, &s).is_err());
    }
    #[test]
    fn graph_is_one_scoped_union_with_server_manifests_and_no_child_page_limit() {
        let (s, _) = sample();
        assert!(build("", &s["metadata"], &[]).is_err());
        let args = build("TESTCO", &s["metadata"], &[7, 8]).unwrap();
        let ds = &args["queryDS"];
        assert_eq!(
            ds["QuerySubQuery"].as_array().unwrap().len(),
            (POSTING_TABLES.len() + 1) * 2
        );
        assert!(
            ds["QuerySubQuery"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s.get("OrderByFetch").is_none())
        );
        assert_eq!(
            ds["QueryField"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| f["Formula"] == "COUNT_BIG(*)")
                .count(),
            POSTING_TABLES.len() + 1
        );
        assert_eq!(
            ds["QueryWhereItem"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|f| f["FieldName"] == "Company" && f["RValue"] == "N'TESTCO'")
                .count(),
            (POSTING_TABLES.len() + 1) * 2
        );
        assert!(!args.to_string().contains("ACTTypeSvc"));
    }
}
