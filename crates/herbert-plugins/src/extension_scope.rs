//! ExtensionSvc.AfterGetRows appends unfiltered schema rows. Read persisted
//! custom rows independently, using only projected native table identities.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn fail(message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new("EXTENSION_SCOPE_UNVERIFIED", message),
    }
}
fn job(table: &str, output: &str, values: Value, probe: bool) -> Value {
    json!({"table":table,"output":output,"values":values,"probe":probe})
}
fn query(j: &Value, size: Option<u32>, after: &Value) -> HostAction {
    let mut parameters: BTreeMap<String, Value> = j["values"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut criteria: Vec<_> = parameters
        .keys()
        .map(|key| QueryCriterion {
            table: "ExtensionSource".into(),
            field: key.clone(),
            operator: "=".into(),
            parameter: key.clone(),
        })
        .collect();
    if !after.is_null() {
        parameters.insert("after".into(), after.clone());
        criteria.push(QueryCriterion {
            table: "ExtensionSource".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: j["table"].as_str().unwrap().into(),
                alias: "ExtensionSource".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields: vec![QueryField {
                table: "ExtensionSource".into(),
                field: if j["probe"] == true { "SysRowID" } else { "*" }.into(),
                alias: if j["probe"] == true { "SysRowID" } else { "*" }.into(),
                expression: None,
            }],
            joins: vec![],
            criteria,
            parameters,
            order: vec![QueryOrder {
                field: "SysRowID".into(),
                descending: false,
            }],
            page_size: size,
            page: 1,
        },
    }
}
fn table_page(r: &GuestRequest) -> GuestReply {
    let Some(size) = r.page_size.filter(|n| *n > 0) else {
        return fail("Supply the run's positive page size for Epicor extension table discovery.");
    };
    let keys = r.state["step"] == "keys";
    GuestReply::Action {
        action: HostAction::NativeEntityPage {
            service: "Ice.BO.ExtensionSvc".into(),
            entity: if keys {
                "ExtensionTableKeys"
            } else {
                "ExtensionTables"
            }
            .into(),
            query: BTreeMap::from([
                (
                    "$select".into(),
                    if keys {
                        "ExtensionSetID,SystemCode,DataTableID,FieldName,Seq,SysRowID"
                    } else {
                        "ExtensionSetID,SystemCode,DataTableID,DataSetSystemCode,DataSetID,SysRowID"
                    }
                    .into(),
                ),
                ("$orderby".into(), "SysRowID".into()),
                ("$count".into(), "true".into()),
            ]),
            page_size: size,
            page: r.state["page"].as_u64().unwrap(),
        },
        state: r.state.clone(),
    }
}
pub fn acquire(mut r: GuestRequest) -> GuestReply {
    if let Some(diagnostic) = r.error.take() {
        return GuestReply::Failure { diagnostic };
    }
    let id = r.identity.as_ref().unwrap().keys["ExtensionSetID"].clone();
    if r.state["phase"] != "extensionAcquire" {
        r.state = json!({"phase":"extensionAcquire","step":"root","definition":{},"jobs":[],"index":0,"exportAllowed":true,"page":1,"tableRefs":[],"keyRefs":[],"pageRows":0});
        return GuestReply::Action {
            action: query(
                &job(
                    "ExtensionSet",
                    "Extension",
                    json!({"ExtensionSetID":id,"SystemFlag":false}),
                    false,
                ),
                r.page_size,
                &Value::Null,
            ),
            state: r.state,
        };
    }
    let Some(response) = r.response.take() else {
        return fail("The extension resume point omitted its response.");
    };
    match r.state["step"].as_str().unwrap_or("") {
        "root" => {
            let Some(rows) = response["rows"].as_array().filter(|rows| rows.len() == 1) else {
                return fail("No single custom extension matches this identity.");
            };
            if rows[0]["ExtensionSetID"] != id || rows[0]["SystemFlag"] != false {
                return fail("The extension root identity or custom marker differs.");
            }
            r.state["definition"]["Extension"] = json!(rows);
            r.state["step"] = json!("tables");
            return table_page(&r);
        }
        "tables" | "keys" => {
            let keys = r.state["step"] == "keys";
            let refs = if keys { "keyRefs" } else { "tableRefs" };
            let Some(rows) = response["value"].as_array() else {
                return fail("The Epicor extension table projection omitted its rows.");
            };
            let Some(total) = response["@odata.count"].as_u64() else {
                return fail("The Epicor extension table projection omitted its total count.");
            };
            r.state["tableTotal"] = json!(total);
            let mut seen: BTreeSet<String> = r.state[refs]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["SysRowID"].as_str().unwrap().into())
                .collect();
            for row in rows {
                if row["ExtensionSetID"] != id {
                    continue;
                }
                if ["SystemCode", "DataTableID", "SysRowID"]
                    .iter()
                    .any(|k| !row[*k].as_str().is_some_and(|s| !s.is_empty()))
                {
                    return fail(
                        "An extension table identity is missing or belongs to another extension.",
                    );
                }
                if !seen.insert(row["SysRowID"].as_str().unwrap().into()) {
                    return fail("The extension table inventory repeats a row.");
                }
                if row.as_object().unwrap().keys().any(|k| {
                    !matches!(
                        k.as_str(),
                        "ExtensionSetID"
                            | "SystemCode"
                            | "DataTableID"
                            | "DataSetSystemCode"
                            | "DataSetID"
                            | "SysRowID"
                            | "FieldName"
                            | "Seq"
                            | "@odata.etag"
                    )
                }) {
                    return fail("The Epicor table projection returned undeclared body fields.");
                }
                r.state[refs].as_array_mut().unwrap().push(row.clone());
            }
            let count = r.state["pageRows"].as_u64().unwrap_or(0) + rows.len() as u64;
            r.state["pageRows"] = json!(count);
            if count > total || (rows.is_empty() && count < total) {
                return fail("The extension table pagination is inconsistent.");
            }
            if count < total {
                r.state["page"] = json!(r.state["page"].as_u64().unwrap() + 1);
                return table_page(&r);
            }
            if !keys {
                r.state["step"] = json!("keys");
                r.state["page"] = json!(1);
                r.state["pageRows"] = json!(0);
                r.state["tableTotal"] = Value::Null;
                return table_page(&r);
            }
            r.state["definition"]["ExtensionTableContext"] = r.state["tableRefs"].clone();
            r.state["definition"]["ExtensionKeyContext"] = r.state["keyRefs"].clone();
            let mut jobs = vec![job(
                "ExtensionSetMapping",
                "ExtensionSetMapping",
                json!({"ExtensionSetID":id,"Company":r.company}),
                false,
            )];
            for table in ["ExtensionRelation", "ExtensionRelationColumn"] {
                jobs.push(job(
                    table,
                    table,
                    json!({"ExtensionSetID":id,"SystemFlag":false}),
                    false,
                ));
                jobs.push(job(
                    table,
                    table,
                    json!({"ExtensionSetID":id,"SystemFlag":true}),
                    true,
                ));
            }
            let mut tables = BTreeSet::new();
            for row in r.state["tableRefs"].as_array().unwrap() {
                tables.insert((
                    row["SystemCode"].as_str().unwrap().to_owned(),
                    row["DataTableID"].as_str().unwrap().to_owned(),
                ));
            }
            for (system, table) in tables {
                for source in ["ZDataTable", "ZDataField", "ZKey", "ZKeyField"] {
                    jobs.push(job(
                        source,
                        source,
                        json!({"SystemCode":system,"DataTableID":table,"SystemFlag":false}),
                        false,
                    ));
                    if matches!(source, "ZDataTable" | "ZDataField") {
                        jobs.push(job(
                            source,
                            source,
                            json!({"SystemCode":system,"DataTableID":table,"SystemFlag":true}),
                            true,
                        ));
                    }
                }
            }
            // Native export includes only its projected primary-key fields, not
            // every key definition. Probe those exact row identities.
            for row in r.state["keyRefs"].as_array().unwrap() {
                jobs.push(job(
                    "ZKeyField",
                    "ZKeyField",
                    json!({"SysRowID":row["SysRowID"],"SystemFlag":true}),
                    true,
                ));
            }
            for name in [
                "ExtensionSetMapping",
                "ExtensionRelation",
                "ExtensionRelationColumn",
                "ZDataTable",
                "ZDataField",
                "ZKey",
                "ZKeyField",
            ] {
                r.state["definition"][name] = json!([]);
            }
            jobs.sort_by_key(|j| j["probe"] != true);
            r.state["jobs"] = json!(jobs);
            r.state["step"] = json!("rows");
        }
        "rows" => {
            let index = r.state["index"].as_u64().unwrap() as usize;
            let j = r.state["jobs"][index].clone();
            let Some(rows) = response["rows"].as_array() else {
                return fail("A saved extension query omitted its rows.");
            };
            if j["probe"] == true {
                if !rows.is_empty() {
                    r.state["exportAllowed"] = json!(false);
                }
            } else {
                let mut seen = BTreeSet::new();
                for row in rows {
                    for (key, value) in j["values"].as_object().unwrap() {
                        if row.get(key) != Some(value) {
                            return fail(
                                "A saved extension row violates its ownership predicate.",
                            );
                        }
                    }
                    let Some(rowid) = row["SysRowID"].as_str().filter(|v| !v.is_empty()) else {
                        return fail("A saved extension row omitted its identity.");
                    };
                    if !seen.insert(rowid) {
                        return fail("A saved extension query repeated a row.");
                    }
                }
                r.state["definition"][j["output"].as_str().unwrap()]
                    .as_array_mut()
                    .unwrap()
                    .extend(rows.iter().cloned());
            }
            if j["probe"] != true && rows.len() == r.page_size.unwrap_or(0) as usize {
                let after = rows
                    .last()
                    .and_then(|row| row.get("SysRowID"))
                    .filter(|v| v.as_str().is_some_and(|v| !v.is_empty()))
                    .cloned()
                    .ok_or_else(|| fail("The extension page omitted its next row identity."));
                let after = match after {
                    Ok(v) => v,
                    Err(failure) => return failure,
                };
                if after == r.state["jobAfter"] {
                    return fail("The extension page saved position did not advance.");
                }
                r.state["jobAfter"] = after;
                return GuestReply::Action {
                    action: query(&j, r.page_size, &r.state["jobAfter"]),
                    state: r.state,
                };
            }
            r.state["jobAfter"] = Value::Null;
            r.state["index"] = json!(index + 1);
        }
        _ => return fail("The extension resume point step is invalid."),
    }
    let mut index = r.state["index"].as_u64().unwrap() as usize;
    while let Some(j) = r.state["jobs"].get(index) {
        let packaged = matches!(
            j["table"].as_str(),
            Some("ExtensionRelation" | "ExtensionRelationColumn" | "ZDataTable" | "ZDataField")
        );
        if r.state["exportAllowed"] == true && j["probe"] != true && packaged {
            index += 1;
            r.state["index"] = json!(index);
            continue;
        }
        return GuestReply::Action {
            action: query(j, r.page_size, &r.state["jobAfter"]),
            state: r.state,
        };
    }
    let allowed = r.state["exportAllowed"].clone();
    let definition = r.state["definition"].clone();
    r.state = json!({"phase":"root","extensionExportAllowed":allowed});
    r.response = Some(json!({"returnObj":definition}));
    crate::families::handle(r)
}

// The ZIP includes the extension tables and compiled assembly. Company mappings
// and secondary key definitions remain separate because the exporter omits them.
pub fn package_start(r: &mut GuestRequest) -> Option<HostAction> {
    r.state["extensionPackageComplete"] = json!(true);
    let Some(value) = r
        .state
        .pointer("/supplements/ExportExtensionSet/returnObj/nativeFile")
    else {
        return None;
    };
    let Ok(file) = serde_json::from_value::<FileRef>(value.clone()) else {
        return None;
    };
    r.state["extensionPackageFile"] = json!(file);
    r.state["phase"] = json!("extensionPackage");
    Some(HostAction::ParseXmlArchive {
        file,
        entries: vec!["ExtensionTableset.xml".into()],
    })
}
fn package_tables(xml: &Value) -> Result<Value, Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            "EXTENSION_PACKAGE_INVALID",
            "The Epicor extension XML has an invalid table or row structure.",
        )
    };
    if xml["name"] != "ExtensionTableset" {
        return Err(invalid());
    }
    fn elements(node: &Value) -> impl Iterator<Item = &Value> {
        node["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c.get("element"))
    }
    let mut data = serde_json::Map::new();
    for table in elements(xml) {
        let name = table["name"].as_str().ok_or_else(invalid)?;
        if name == "ExtensionTables" {
            continue;
        }
        let mut rows = vec![];
        for row in elements(table) {
            if row["name"].as_str() != Some(&format!("{name}Row")) {
                return Err(invalid());
            }
            let mut fields = serde_json::Map::new();
            for cell in elements(row) {
                let field = cell["name"].as_str().ok_or_else(invalid)?;
                let nil = cell["attributes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|a| {
                        a["name"] == "nil"
                            && a["namespace"] == "http://www.w3.org/2001/XMLSchema-instance"
                            && a["value"] == "true"
                    });
                let value = if nil {
                    Value::Null
                } else if elements(cell).next().is_some() {
                    json!({"xml":cell})
                } else {
                    json!(
                        cell["content"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v["text"].as_str())
                            .collect::<String>()
                    )
                };
                if fields.insert(field.into(), value).is_some() {
                    return Err(invalid());
                }
            }
            rows.push(Value::Object(fields));
        }
        if data.insert(name.into(), json!(rows)).is_some() {
            return Err(invalid());
        }
    }
    Ok(Value::Object(data))
}
pub fn package_complete(r: &mut GuestRequest) {
    let result = (|| -> Result<Value, Diagnostic> {
        if let Some(error) = r.error.take() {
            return Err(error);
        }
        let response = r.response.take().unwrap_or(Value::Null);
        if response["sourceFile"] != r.state["extensionPackageFile"] {
            return Err(Diagnostic::new(
                "EXTENSION_PACKAGE_INVALID",
                "The parsed package does not match the downloaded file.",
            ));
        }
        let data = package_tables(
            &response["parsedXmlArchive"]["documents"]["ExtensionTableset.xml"]["xml"],
        )?;
        let rows = data["Extension"]
            .as_array()
            .filter(|rows| rows.len() == 1)
            .ok_or_else(|| {
                Diagnostic::new(
                    "EXTENSION_PACKAGE_INVALID",
                    "The extension package must contain one root.",
                )
            })?;
        if rows[0]["ExtensionSetID"].as_str()
            != Some(&r.identity.as_ref().unwrap().keys["ExtensionSetID"])
            || data
                .as_object()
                .unwrap()
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .any(crate::system_artifacts::marked)
        {
            return Err(Diagnostic::new(
                "EXTENSION_PACKAGE_INVALID",
                "The package contains another extension or System-marked content.",
            ));
        }
        Ok(data)
    })();
    match result {
        Ok(data) => {
            for (table, rows) in data.as_object().unwrap() {
                if table != "ExtensionSetMapping" {
                    r.state["definition"][table] = rows.clone();
                }
            }
        }
        Err(error) => r.state["errors"].as_array_mut().unwrap().push(json!(error)),
    }
    r.state["phase"] = json!("extensionPackageComplete");
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "programs-and-extensions".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "programs-and-extensions".into(),
                kind: "ExtensionSet".into(),
                scope: Scope::Global,
                keys: BTreeMap::from([("ExtensionSetID".into(), "Owned's extension".into())]),
            }),
            cursor: None,
            page_size: Some(1),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    #[test]
    fn membership_pages_are_shared_and_filter_each_extension_locally() {
        let mut r = request();
        r.state = json!({"phase":"extensionAcquire","step":"tables","page":1,"tableRefs":[],"keyRefs":[],"pageRows":0});
        let mut other = r.clone();
        other
            .identity
            .as_mut()
            .unwrap()
            .keys
            .insert("ExtensionSetID".into(), "Another".into());
        let GuestReply::Action { action: a, .. } = table_page(&r) else {
            panic!()
        };
        let GuestReply::Action { action: b, .. } = table_page(&other) else {
            panic!()
        };
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        r.response = Some(
            json!({"value":[{"ExtensionSetID":"Another","SystemCode":"ICE","DataTableID":"Other","SysRowID":"a"}],"@odata.count":2}),
        );
        let GuestReply::Action {
            action: HostAction::NativeEntityPage { page, .. },
            state,
        } = acquire(r.clone())
        else {
            panic!()
        };
        assert_eq!(page, 2);
        assert!(state["tableRefs"].as_array().unwrap().is_empty());
        r.state = state;
        r.response = Some(
            json!({"value":[{"ExtensionSetID":"Owned's extension","SystemCode":"ICE","DataTableID":"Own","SysRowID":"b"}],"@odata.count":2}),
        );
        let GuestReply::Action {
            action: HostAction::NativeEntityPage { entity, page, .. },
            state,
        } = acquire(r)
        else {
            panic!()
        };
        assert_eq!(entity, "ExtensionTableKeys");
        assert_eq!(page, 1);
        assert_eq!(state["tableRefs"].as_array().unwrap().len(), 1);
        assert_eq!(state["tableRefs"][0]["DataTableID"], "Own");
    }
    #[test]
    fn extension_custom_siblings_survive_mixed_system_export_refusal() {
        for mixed in [false, true] {
            let mut r = request();
            let mut reply = acquire(r.clone());
            loop {
                let GuestReply::Action { action, state } = reply else {
                    panic!("Unexpected terminal reply")
                };
                r.state = state;
                match action {
                    HostAction::SharedNativeRead { method, .. } => {
                        assert!(!mixed);
                        assert_eq!(method, "ExportExtensionSet");
                        break;
                    }
                    HostAction::NativeEntityPage { query, .. } => {
                        assert!(!query.contains_key("$filter"));
                        assert!(!query["$select"].contains("Description"));
                        r.response = Some(json!({"value":[],"@odata.count":0}));
                    }
                    HostAction::SharedQuery { query } => {
                        let table = &query.tables[0].table;
                        let rows = if query.parameters.contains_key("after") {
                            vec![]
                        } else if table == "ExtensionSet" {
                            vec![
                                json!({"ExtensionSetID":"Owned's extension","SystemFlag":false,"SysRowID":"root","Disabled":false,"Provider":"","Level":0}),
                            ]
                        } else if query.parameters.get("SystemFlag") == Some(&json!(true)) {
                            assert_eq!(query.fields[0].field, "SysRowID");
                            if mixed {
                                vec![json!({"SysRowID":"system-only-key"})]
                            } else {
                                vec![]
                            }
                        } else if table == "ExtensionRelation" {
                            vec![
                                json!({"ExtensionSetID":"Owned's extension","SystemCode":"ICE","DataSetID":"D","RelationID":"R","SystemFlag":false,"SysRowID":"custom","Description":"Keep exact Ω","Type":0}),
                            ]
                        } else {
                            vec![]
                        };
                        r.response = Some(json!({"rows":rows}));
                    }
                    _ => panic!("Unexpected action"),
                }
                reply = acquire(r.clone());
                if let GuestReply::Definition {
                    definition,
                    diagnostics,
                    ..
                } = &reply
                {
                    assert!(mixed);
                    assert_eq!(
                        definition["definition"]["ExtensionRelation"][0]["Description"],
                        "Keep exact Ω"
                    );
                    assert_eq!(definition["definition"]["Extension"][0]["Disabled"], false);
                    assert!(
                        diagnostics
                            .iter()
                            .any(|d| d.code == "EXTENSION_EXPORT_MIXED_SYSTEM_SCOPE")
                    );
                    break;
                }
            }
        }
    }
    #[test]
    fn extension_refuses_malformed_or_foreign_root_before_body_export() {
        for row in [
            json!({"ExtensionSetID":"other","SystemFlag":false}),
            json!({"ExtensionSetID":"Owned's extension","SystemFlag":true}),
        ] {
            let mut r = request();
            let GuestReply::Action { state, .. } = acquire(r.clone()) else {
                panic!()
            };
            r.state = state;
            r.response = Some(json!({"rows":[row]}));
            assert!(matches!(acquire(r), GuestReply::Failure { .. }));
        }
    }
    #[test]
    fn extension_table_inventory_checks_count_and_never_accepts_body_fields() {
        for response in [
            json!({"value":[]}),
            json!({"value":[],"@odata.count":1}),
            json!({"value":[{"ExtensionSetID":"Owned's extension","SystemCode":"ICE","DataTableID":"D","SysRowID":"row","Description":"unexpected body"}],"@odata.count":1}),
        ] {
            let mut r = request();
            r.state = json!({"phase":"extensionAcquire","step":"tables","tableRefs":[],"page":1});
            r.response = Some(response);
            assert!(matches!(acquire(r), GuestReply::Failure { .. }));
        }
    }
}
