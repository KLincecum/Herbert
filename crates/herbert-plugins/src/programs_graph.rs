//! Complete persisted extension graph, built from native physical field metadata.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const TABLES: &[&str] = &[
    "Extension",
    "ExtensionSetMapping",
    "ExtensionRelation",
    "ExtensionRelationColumn",
    "ZDataTable",
    "ZDataField",
    "ZKey",
    "ZKeyField",
];
fn err(s: &str) -> Diagnostic {
    Diagnostic::new("PROGRAM_GRAPH_INVALID", s)
}
fn payload(fs: &Value, alias: &str) -> Result<String, Diagnostic> {
    let mut cells = vec![];
    for f in fs.as_array().ok_or_else(|| err("Epicor returned a definition in a form Herbert does not recognize, so it was not saved."))? {
        let name = f["FieldName"]
            .as_str()
            .ok_or_else(|| err("Missing record set field name."))?;
        if !crate::report_query::identifier(name) {
            return Err(err("Invalid record set field identifier."));
        }
        let ty = f["DataType"]
            .as_str()
            .ok_or_else(|| err("Missing record set field type."))?;
        let col = format!("{alias}.{name}");
        let value = match ty {
            "sysname" | "nvarchar" | "varchar" | "nchar" | "char" | "xml" | "uniqueidentifier"
            | "datetime" | "datetime2" | "smalldatetime" | "date" | "time" => {
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
                    "An Epicor record set field type has no lossless JSON projection.",
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

pub fn schema() -> Value {
    let names = TABLES
        .iter()
        .map(|t| format!("DataTableID = '{}'", t))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut a = json!({"whereClauseZDataTable":format!("SystemCode = 'Ice' AND ({names})"),"whereClauseZDataField":"","pageSize":0,"absolutePage":1});
    for t in [
        "ZLookupLink",
        "ZLinkColumn",
        "ZLookupField",
        "ZKey",
        "ZKeyField",
    ] {
        a[format!("whereClause{t}")] = json!("1=0");
    }
    a
}
pub fn metadata(r: &Value) -> Result<Value, Diagnostic> {
    if r["parameters"]["morePages"] != false {
        return Err(err("Epicor returned an extension without its full field list, so it was not saved."));
    }
    let fs = r["returnObj"]["ZDataField"]
        .as_array()
        .ok_or_else(|| err("Epicor did not return the database fields for this extension, so it was not saved."))?;
    let mut out = json!({});
    for t in TABLES {
        let rows: Vec<_> = fs
            .iter()
            .filter(|f| f["DataTableID"] == *t && f["External"] == false)
            .cloned()
            .collect();
        let physical = if *t == "Extension" { "ExtensionSet" } else { t };
        if rows.is_empty()
            || rows.iter().any(|f| f["DBTableName"] != *physical)
            || !rows.iter().any(|f| f["FieldName"] == "SysRowID")
        {
            return Err(err("Invalid Epicor physical extension schema."));
        }
        out[*t] = json!(rows);
    }
    Ok(out)
}
pub fn build(
    company: &str,
    meta: &Value,
    roots: &Value,
    refs: &Value,
) -> Result<Value, Diagnostic> {
    if company.is_empty() {
        return Err(err("A company is required."));
    }
    if roots.as_array().is_none_or(Vec::is_empty) {
        return Err(err("The extension record set requires selected Epicor roots."));
    }
    let q = "HerbertProgramsGraph";
    let mut ds = json!({"DynamicQuery":[{"Company":company,"QueryID":q,"Description":"Unsaved complete selected extension graph","Updatable":false,"ExtQuery":false,"IsShared":false,"AllCompanies":false,"XCompany":true,"RowMod":"A"}],"QuerySubQuery":[],"QueryTable":[],"QueryField":[],"QueryWhereItem":[],"QuerySortBy":[],"QueryParameter":[],"QueryCtrl":[],"QueryRelation":[],"QueryRelationField":[]});
    for (ti, table) in TABLES.iter().enumerate() {
        for marker in [false, true] {
            let branch = ti * 2 + usize::from(marker);
            let alias = format!("Source{branch}");
            let sub = format!("00000000-0000-0000-0000-{:012}", branch + 1);
            let row = |extra: Value| {
                let mut r = json!({"Company":company,"QueryID":q,"SubQueryID":sub,"RowMod":"A"});
                r.as_object_mut()
                    .unwrap()
                    .extend(extra.as_object().unwrap().clone());
                r
            };
            ds["QuerySubQuery"].as_array_mut().unwrap().push(row(json!({"Name":alias,"Type":if branch==0{"TopLevel"}else{"UnionAll"},"Seq":branch+1})));
            ds["QueryTable"].as_array_mut().unwrap().extend([row(json!({"TableID":alias,"DBSchemaName":"Ice","DBTableName":if *table=="Extension"{"ExtensionSet"}else{table},"TableType":"DB","Seq":1,"IsVisibleInDesigner":true})),row(json!({"TableID":"Calculated","TableType":"TT","Seq":-1,"IsVisibleInDesigner":false}))]);
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
            let mut criteria: Vec<(&str, String, &str, &str, &str)> = vec![];
            if *table == "ExtensionSetMapping" {
                criteria.push(("Company", crate::report_query::literal(company), "", "", ""));
            } else {
                criteria.push(("SystemFlag", "0".into(), "", "", ""));
            }
            if ti < 4 {
                let ids: Vec<_> = roots
                    .as_array()
                    .ok_or_else(|| err("Epicor returned extension definitions in a form Herbert does not recognize, so this page was not saved."))?
                    .iter()
                    .map(|r| {
                        r["ExtensionSetID"]
                            .as_str()
                            .ok_or_else(|| err("Epicor returned an extension without its ID, so it was not saved."))
                    })
                    .collect::<Result<_, _>>()?;
                for (i, id) in ids.iter().enumerate() {
                    criteria.push((
                        "ExtensionSetID",
                        crate::report_query::literal(id),
                        if i == 0 { "And" } else { "Or" },
                        if i == 0 { "(" } else { "" },
                        if i + 1 == ids.len() { ")" } else { "" },
                    ));
                }
            } else {
                let pairs: BTreeSet<_> = refs
                    .as_array()
                    .ok_or_else(|| err("Epicor returned extension references in a form Herbert does not recognize, so this extension was not saved."))?
                    .iter()
                    .map(|r| {
                        Ok((
                            r["SystemCode"]
                                .as_str()
                                .ok_or_else(|| err("Epicor did not say which system this extension table belongs to, so it was not saved."))?,
                            r["DataTableID"]
                                .as_str()
                                .ok_or_else(|| err("Epicor did not say which table this extension belongs to, so it was not saved."))?,
                        ))
                    })
                    .collect::<Result<_, Diagnostic>>()?;
                if pairs.is_empty() {
                    criteria.push((
                        "SysRowID",
                        crate::report_query::literal("00000000-0000-0000-0000-000000000000"),
                        "And",
                        "",
                        "",
                    ));
                } else {
                    for (i, (system, table)) in pairs.iter().enumerate() {
                        criteria.push((
                            "SystemCode",
                            crate::report_query::literal(system),
                            if i == 0 { "And" } else { "Or" },
                            if i == 0 { "((" } else { "(" },
                            "",
                        ));
                        criteria.push((
                            "DataTableID",
                            crate::report_query::literal(table),
                            "And",
                            "",
                            if i + 1 == pairs.len() { "))" } else { ")" },
                        ));
                    }
                }
            }
            for (i, (field, value, logic, left, right)) in criteria.into_iter().enumerate() {
                ds["QueryWhereItem"].as_array_mut().unwrap().push(row(json!({"CriteriaID":format!("00000000-0000-0002-{:04}-{:012}",branch+1,i+1),"TableID":alias,"FieldName":field,"CompOp":"=","RValue":value,"IsConst":true,"CriteriaType":2,"Seq":i+1,"AndOr":logic,"Neg":false,"LeftP":left,"RightP":right,"UseHaving":false})));
            }
        }
    }
    Ok(json!({"queryDS":ds,"executionParams":{"ExecutionParameter":[]}}))
}
pub fn decode(response: &Value, meta: &Value, company: &str) -> Result<Value, Diagnostic> {
    if response["returnObj"]["Errors"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        return Err(err("The Epicor program record set returned errors."));
    }
    let rows = response["returnObj"]["Results"]
        .as_array()
        .ok_or_else(|| err("The record set omitted Results."))?;
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
            .ok_or_else(|| err("Unknown program record set table."))?;
        let n = wire["Total"]
            .as_i64()
            .ok_or_else(|| err("Missing program record set total."))?;
        if n >= 0 {
            if !wire["Payload"].is_null() || totals.insert(table, n as usize).is_some() {
                return Err(err("Invalid record set count manifest."));
            }
            continue;
        }
        if n != -1 {
            return Err(err("Invalid record set row marker."));
        }
        let cells: Vec<Value> = serde_json::from_str(
            wire["Payload"]
                .as_str()
                .ok_or_else(|| err("Epicor's response did not include the definition data. Nothing was saved for this item."))?,
        )
        .map_err(|_| err("Epicor returned a definition in a form Herbert does not recognize, so it was not saved."))?;
        let fs = meta[table]
            .as_array()
            .ok_or_else(|| err("Epicor's response did not describe the definition's fields. Nothing was saved for this item."))?;
        if cells.len() != fs.len() {
            return Err(err("Record set field count differs from Epicor schema."));
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
            .ok_or_else(|| err("Missing physical record set identity."))?;
        if !seen.insert((table, id.to_ascii_lowercase())) {
            return Err(err("Epicor returned the same record more than once, so Herbert cannot tell which copy is correct. This item was not saved."));
        }
        if table == "SolutionHeader" && row["Company"] != company {
            return Err(err("Epicor returned a program definition that belongs to a different company than the one selected. It was not saved."));
        }
        if table != "ExtensionSetMapping" && row["SystemFlag"] != false {
            return Err(err(
                "The custom record set returned system content or omitted its ownership marker.",
            ));
        }
        graph[table].as_array_mut().unwrap().push(row);
    }
    for t in TABLES {
        if totals.get(t) != Some(&graph[*t].as_array().unwrap().len()) {
            return Err(err("The record set is missing rows or its count manifest."));
        }
        graph[*t]
            .as_array_mut()
            .unwrap()
            .sort_by_key(|r| r["SysRowID"].as_str().unwrap().to_ascii_lowercase());
    }
    Ok(graph)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_graph_has_no_root_or_child_pages() {
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
        let q = build(
            "C",
            &meta,
            &json!([{"ExtensionSetID":"A"},{"ExtensionSetID":"B"}]),
            &json!([]),
        )
        .unwrap();
        assert_eq!(
            q["queryDS"]["QuerySubQuery"].as_array().unwrap().len(),
            TABLES.len() * 2
        );
        assert!(
            q["queryDS"]["QuerySubQuery"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r.get("OrderByFetch").is_none())
        );
    }
}
