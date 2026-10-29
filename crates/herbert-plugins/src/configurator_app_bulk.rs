//! Shared app acquisition for a selected page of Kinetic configurator roots.
use herbert_contract::{Diagnostic, HostAction};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("CONFIGURATOR_APP_BULK_INVALID", message)
}
pub enum Step {
    Read { action: HostAction, state: Value },
    Ready { apps: BTreeMap<String, Value> },
}
pub fn start(company: &str, roots: &[Value], size: u32) -> Result<Step, Diagnostic> {
    if company.is_empty() || size == 0 || size > i32::MAX as u32 {
        return Err(error(
            "Configurator application reads need a selected company and a page size from 1 to 2147483647.",
        ));
    }
    let mut views = BTreeMap::new();
    let mut apps = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        if root["Kinetic"] != true {
            continue;
        }
        let id = root["ConfigID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Kinetic root omitted ConfigID."))?;
        if root["Company"] != company || !seen.insert(id.to_lowercase()) {
            return Err(error("Kinetic roots have foreign or duplicate identities."));
        }
        views.insert(format!("Erp.UICfg.{id}_{company}"), id.to_owned());
        apps.insert(
            id.to_owned(),
            json!({"rows":[],"systemRows":[],"visibleRows":[],"complete":false}),
        );
    }
    if apps.is_empty() {
        return Ok(Step::Ready { apps });
    }
    Ok(Step::Read {
        action: HostAction::SharedNativeRead {
            service: "Ice.BO.BAQDesignerSvc".into(),
            method: "GetFieldList".into(),
            arguments: json!({"dataTableID":"XXXDef","schema":"Ice"}),
            export: false,
        },
        state: json!({"company":company,"size":size,"views":views,"apps":apps,"phase":"dictionary","branch":0,"after":null,"seen":{}}),
    })
}
fn query(mut state: Value) -> Result<Step, Diagnostic> {
    let branch = state["branch"]
        .as_u64()
        .ok_or_else(|| error("Herbert could not continue this download from its saved progress (configurator application step). Start the download again."))?;
    if branch == 6 {
        state["phase"] = json!("visible");
        state["page"] = json!(1);
        state["seen"] = json!({});
        return visible_query(state);
    }
    let company = state["company"]
        .as_str()
        .ok_or_else(|| error("Epicor did not say which company this configurator application belongs to, so it was not saved."))?;
    let event = branch == 2 || branch == 3;
    let body = branch >= 4;
    let owner = if branch % 2 == 0 { "" } else { company };
    let mut criteria = Vec::new();
    for (field, value) in [
        ("Company", crate::report_query::literal(owner)),
        ("ProductID", crate::report_query::literal("EP")),
        (
            "TypeCode",
            crate::report_query::literal(if event {
                "KNTCEventLayout"
            } else {
                "KNTCMetaUI"
            }),
        ),
        ("SystemFlag", if body { "0" } else { "1" }.into()),
    ] {
        criteria.push((
            field.into(),
            "=".into(),
            value,
            if criteria.is_empty() { "" } else { "AND" }.into(),
            "".into(),
            "".into(),
        ));
    }
    if body {
        criteria.push((
            "Key3".into(),
            "=".into(),
            crate::report_query::literal(""),
            "AND".into(),
            "".into(),
            "".into(),
        ));
    }
    let views: Vec<_> = state["views"]
        .as_object()
        .ok_or_else(|| error("Missing selected app views."))?
        .keys()
        .collect();
    if views.is_empty() {
        return Err(error("Unscoped app reads are prohibited."));
    }
    for (i, view) in views.iter().enumerate() {
        criteria.push((
            if event { "Key2" } else { "Key1" }.into(),
            "=".into(),
            crate::report_query::literal(view),
            if i == 0 { "AND" } else { "OR" }.into(),
            if i == 0 { "(" } else { "" }.into(),
            if i + 1 == views.len() { ")" } else { "" }.into(),
        ));
    }
    let fields: Vec<_> = state["fields"]
        .as_array()
        .ok_or_else(|| error("Epicor did not return the configurator's application records, so it was not saved."))?
        .iter()
        .filter(|f| {
            body || [
                "Company",
                "ProductID",
                "TypeCode",
                "CGCCode",
                "Key1",
                "Key2",
                "Key3",
                "SystemFlag",
                "SysRowID",
                "SysRevID",
            ]
            .contains(&f["FieldName"].as_str().unwrap_or(""))
        })
        .cloned()
        .collect();
    let mut args = crate::report_query::build(
        company,
        "XXXDef",
        &json!(fields),
        state["size"].as_u64().unwrap() as u32,
        &criteria,
        state["after"].as_str(),
    )
    .map_err(error)?;
    args["queryDS"]["DynamicQuery"][0]["Description"] =
        json!("Unsaved selected configurator app page read");
    if body {
        let mut field = args["queryDS"]["QueryField"][0].clone();
        let count = args["queryDS"]["QueryField"].as_array().unwrap().len();
        for key in ["FieldName", "Alias", "FieldLabel"] {
            field[key] = json!("HerbertContentBytes");
        }
        field["DataType"] = json!("bigint");
        field["Seq"] = json!(count + 1);
        field["Formula"] = json!("DATALENGTH(R.Content)");
        args["queryDS"]["QueryField"]
            .as_array_mut()
            .unwrap()
            .push(field);
    }
    state["phase"] = json!("rows");
    Ok(Step::Read {
        action: HostAction::NativeRead {
            service: "Ice.BO.DynamicQuerySvc".into(),
            method: "Execute".into(),
            arguments: args,
            export: false,
        },
        state,
    })
}
fn visible_query(state: Value) -> Result<Step, Diagnostic> {
    let views = state["views"]
        .as_object()
        .ok_or_else(|| error("Herbert could not continue this download from its saved progress (selected configurator views). Start the download again."))?;
    let clauses: Vec<_> = views
        .keys()
        .map(|view| format!("Key1 = '{}'", view.replace('\'', "''")))
        .collect();
    if clauses.is_empty() {
        return Err(error("Unscoped Epicor app list is prohibited."));
    }
    Ok(Step::Read {
        action: HostAction::NativeRead {
            service: "Ice.BO.GenXDataSvc".into(),
            method: "GetList".into(),
            arguments: json!({"whereClause":format!("({}) AND Key3 = ''",clauses.join(" OR ")),"pageSize":state["size"],"absolutePage":state["page"]}),
            export: false,
        },
        state,
    })
}
fn visible_receive(mut state: Value, response: Value) -> Result<Step, Diagnostic> {
    let rows = response
        .pointer("/returnObj/XXXDefList")
        .and_then(Value::as_array)
        .ok_or_else(|| error("Epicor app list omitted rows."))?;
    let more = response
        .pointer("/parameters/morePages")
        .and_then(Value::as_bool)
        .ok_or_else(|| error("Epicor app list omitted pagination status."))?;
    if rows.len() > state["size"].as_u64().unwrap() as usize {
        return Err(error("Epicor app list exceeded its requested page size."));
    }
    for row in rows {
        let view = row["Key1"]
            .as_str()
            .ok_or_else(|| error("Epicor app list omitted view."))?;
        let config = state["views"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(view))
            .and_then(|(_, v)| v.as_str())
            .ok_or_else(|| error("Epicor app list returned an unrelated view."))?
            .to_owned();
        let id = row["SysRowID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Epicor app list omitted row identity."))?
            .to_ascii_lowercase();
        if row.get("Content").is_some()
            || row["Key3"] != ""
            || row["SysRevID"].is_null()
            || state["seen"].get(&id).is_some()
        {
            return Err(error(
                "Epicor app metadata contains a body, revision mismatch or repeated identity.",
            ));
        }
        state["seen"][id] = json!(true);
        state["apps"][config]["visibleRows"]
            .as_array_mut()
            .unwrap()
            .push(row.clone());
    }
    if more {
        // GenXData applies access filtering after paging; a page can be empty.
        let page = state["page"]
            .as_u64()
            .unwrap()
            .checked_add(1)
            .filter(|p| *p <= i32::MAX as u64)
            .ok_or_else(|| error("This download has more configurator application pages than Epicor can address. Use a larger page size, then start again."))?;
        state["page"] = json!(page);
        return visible_query(state);
    }
    let mut apps: BTreeMap<String, Value> =
        serde_json::from_value(state["apps"].take()).map_err(|_| error("Epicor returned configurator application data in a form Herbert does not recognize, so it was not saved."))?;
    for app in apps.values_mut() {
        app["complete"] = json!(true);
    }
    Ok(Step::Ready { apps })
}
pub fn receive(mut state: Value, response: Value) -> Result<Step, Diagnostic> {
    if state["phase"] == "visible" {
        return visible_receive(state, response);
    }
    if state["phase"] == "dictionary" {
        let fields = response
            .pointer("/returnObj/TableFieldList")
            .and_then(Value::as_array)
            .ok_or_else(|| error("Epicor did not return the configurator's application layout records, so it was not saved."))?;
        let names: BTreeSet<_> = fields
            .iter()
            .filter_map(|f| f["FieldName"].as_str())
            .collect();
        if names.len() != fields.len() {
            return Err(error("Unnamed or duplicate app fields."));
        }
        for name in [
            "Company",
            "ProductID",
            "TypeCode",
            "CGCCode",
            "Key1",
            "Key2",
            "Key3",
            "SystemFlag",
            "SysRowID",
            "SysRevID",
            "Content",
        ] {
            if !names.contains(name) {
                return Err(error(format!("App dictionary omitted {name}.")));
            }
        }
        state["fields"] = json!(fields);
        return query(state);
    }
    if state["phase"] != "rows" {
        return Err(error("Invalid app read phase."));
    }
    if response
        .pointer("/returnObj/Errors")
        .is_some_and(|v| !v.is_null() && v.as_array().is_none_or(|r| !r.is_empty()))
    {
        return Err(error("Epicor app query returned errors."));
    }
    let rows = response
        .pointer("/returnObj/Results")
        .and_then(Value::as_array)
        .ok_or_else(|| error("Epicor app query omitted Results."))?;
    let size = state["size"].as_u64().unwrap() as usize;
    if rows.len() > size {
        return Err(error("App response exceeded its requested page size."));
    }
    let branch = state["branch"].as_u64().unwrap();
    let body = branch >= 4;
    let event = branch == 2 || branch == 3;
    let owner = if branch % 2 == 0 {
        String::new()
    } else {
        state["company"].as_str().unwrap().into()
    };
    for row in rows {
        let view = row[if event { "Key2" } else { "Key1" }]
            .as_str()
            .ok_or_else(|| error("App row omitted its view identity."))?;
        let config = state["views"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(view))
            .and_then(|(_, v)| v.as_str())
            .ok_or_else(|| error("App row belongs outside the selected page."))?
            .to_owned();
        let id = row["SysRowID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("App row omitted SysRowID."))?;
        if row["Company"] != owner
            || row["ProductID"] != "EP"
            || row["TypeCode"]
                != if event {
                    "KNTCEventLayout"
                } else {
                    "KNTCMetaUI"
                }
            || row["SystemFlag"] != !body
            || row["SysRevID"].is_null()
            || state["seen"].get(id.to_ascii_lowercase()).is_some()
        {
            return Err(error(
                "App response has foreign ownership or duplicate identities.",
            ));
        }
        if body {
            if row["Key3"] != "" {
                return Err(error(
                    "Current app query returned a cached or revision row.",
                ));
            }
            let text = row["Content"]
                .as_str()
                .ok_or_else(|| error("App body omitted Content."))?;
            let measured = row["HerbertContentBytes"]
                .as_u64()
                .or_else(|| row["HerbertContentBytes"].as_str()?.parse().ok());
            if measured != Some(text.encode_utf16().count() as u64 * 2) {
                return Err(error("App body failed its measured length check."));
            }
        } else if row.get("Content").is_some() {
            return Err(error("System metadata query unexpectedly returned a body."));
        }
        state["seen"][id.to_ascii_lowercase()] = json!(true);
        state["apps"][config][if body { "rows" } else { "systemRows" }]
            .as_array_mut()
            .unwrap()
            .push(row.clone());
    }
    if rows.len() == size {
        let after = rows.last().unwrap()["SysRowID"].clone();
        if after == state["after"] {
            return Err(error("App saved position did not advance."));
        }
        state["after"] = after;
        return query(state);
    }
    state["branch"] = json!(branch + 1);
    state["after"] = Value::Null;
    state["seen"] = json!({});
    query(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplied_selected_page_app_read_replay() {
        let Ok(base) = std::env::var("HERBERT_CONFIGURATORS_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            for size in [100u32, 2u32] {
                let folder = std::path::Path::new(&base).join(lane);
                let suffix = if size == 100 { "" } else { "-paged" };
                let read = |p: &str| -> Value {
                    serde_json::from_slice(&std::fs::read(folder.join(p)).unwrap()).unwrap()
                };
                let root = read("stored/kinetic/rows.json")["PcStatus"][0].clone();
                let groups = read("kinetic-app-rows-complete/result.json");
                let native =
                    std::fs::read(folder.join(format!("app-bulk-native{suffix}/responses.json")))
                        .ok()
                        .map(|b| serde_json::from_slice::<Vec<Value>>(&b).unwrap());
                let mut step = start("TESTCO", &[root], size).unwrap();
                let mut queries = Vec::new();
                loop {
                    match step {
                        Step::Read {
                            action: HostAction::SharedNativeRead { method, .. },
                            state,
                        } => {
                            assert_eq!(method, "GetFieldList");
                            step =
                                receive(state, read("app-dictionary/fields.json")["value"].clone())
                                    .unwrap();
                        }
                        Step::Read {
                            action: HostAction::NativeRead { method, .. },
                            state,
                        } if method == "GetList" => {
                            let responses = read("app-visible-list/responses.json");
                            let pages = responses.as_array().unwrap();
                            let response = if size == 2 {
                                pages[state["page"].as_u64().unwrap() as usize - 1].clone()
                            } else {
                                json!({"returnObj":{"XXXDefList":pages.iter().flat_map(|p|p["returnObj"]["XXXDefList"].as_array().unwrap().iter().cloned()).collect::<Vec<_>>()} ,"parameters":{"morePages":false}})
                            };
                            step = receive(state, response).unwrap();
                        }
                        Step::Read {
                            action: HostAction::NativeRead { arguments, .. },
                            state,
                        } => {
                            let branch = state["branch"].as_u64().unwrap() as usize;
                            let fields = arguments["queryDS"]["QueryField"].as_array().unwrap();
                            assert_eq!(
                                fields.iter().any(|f| f["FieldName"] == "Content"),
                                branch >= 4
                            );
                            let clauses =
                                arguments["queryDS"]["QueryWhereItem"].as_array().unwrap();
                            assert!(
                                clauses
                                    .iter()
                                    .any(|c| c["RValue"] == "N'Erp.UICfg.H19K1024A_TESTCO'")
                            );
                            let response = native.as_ref().map(|v| v[queries.len()].clone()).unwrap_or_else(|| {
                            let rows = if branch < 4 { Vec::new() } else { groups[branch % 2]["rows"].as_array().unwrap().clone() };
                            let offset = state["after"].as_str().map(|id| rows.iter().position(|r| r["SysRowID"] == id).unwrap() + 1).unwrap_or(0);
                            json!({"returnObj":{"Results":rows.into_iter().skip(offset).take(size as usize).collect::<Vec<_>>()}})
                        });
                            queries.push(arguments);
                            step = receive(state, response).unwrap();
                        }
                        Step::Ready { apps } => {
                            assert_eq!(queries.len(), if size == 100 { 6 } else { 10 });
                            let app = &apps["H19K1024A"];
                            assert_eq!(app["complete"], true);
                            assert_eq!(
                                crate::configurator_app::from_visible("TESTCO", "H19K1024A", app)
                                    .unwrap(),
                                read("oracle/kinetic/app.json")
                            );
                            assert!(app["systemRows"].as_array().unwrap().is_empty());
                            assert_eq!(
                                crate::configurator_app::from_rows(
                                    "TESTCO",
                                    "H19K1024A",
                                    app["rows"].as_array().unwrap()
                                )
                                .unwrap(),
                                read("oracle/kinetic/app.json")
                            );
                            std::fs::write(
                                folder.join(format!("app-bulk-plan{suffix}.json")),
                                serde_json::to_vec_pretty(&queries).unwrap(),
                            )
                            .unwrap();
                            break;
                        }
                        _ => panic!("Unexpected app action"),
                    }
                }
            }
        }
    }
    #[test]
    fn app_queries_group_selected_roots_and_reject_missing_or_foreign_results() {
        let roots = json!([{"Company":"C","ConfigID":"A","Kinetic":true},{"Company":"C","ConfigID":"B","Kinetic":true}]);
        let Step::Read { state, .. } = start("C", roots.as_array().unwrap(), 2).unwrap() else {
            panic!()
        };
        let names = [
            "Company",
            "ProductID",
            "TypeCode",
            "CGCCode",
            "Key1",
            "Key2",
            "Key3",
            "SystemFlag",
            "SysRowID",
            "SysRevID",
            "Content",
        ];
        let fields: Vec<_> = names
            .iter()
            .map(|n| json!({"FieldName":n,"DataType":"nvarchar"}))
            .collect();
        let Step::Read {
            action: HostAction::NativeRead { arguments, .. },
            state,
        } = receive(state, json!({"returnObj":{"TableFieldList":fields}})).unwrap()
        else {
            panic!()
        };
        let clauses = arguments["queryDS"]["QueryWhereItem"].as_array().unwrap();
        assert!(clauses.iter().any(|c| c["RValue"] == "N'Erp.UICfg.A_C'"));
        assert!(
            clauses
                .iter()
                .any(|c| c["RValue"] == "N'Erp.UICfg.B_C'" && c["AndOr"] == "Or")
        );
        assert!(receive(state.clone(), json!({})).is_err());
        assert!(
            receive(
                state.clone(),
                json!({"returnObj":{"Results":[],"Errors":[{"message":"failed"}]}})
            )
            .is_err()
        );
        let row = json!({"Company":"","ProductID":"EP","TypeCode":"KNTCMetaUI","SystemFlag":true,"Key1":"Other","SysRowID":"00000000-0000-0000-0000-000000000001","SysRevID":1});
        assert!(receive(state, json!({"returnObj":{"Results":[row]}})).is_err());
    }
}
