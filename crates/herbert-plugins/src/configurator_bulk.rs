//! Read stored configurator graphs for one visible root page, not one request
//! sequence per configurator. The host owns concurrency and response retention.
use herbert_contract::{Diagnostic, HostAction};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("CONFIGURATOR_BULK_INVALID", message)
}
fn tables() -> Vec<String> {
    let schema: Value = serde_json::from_str(include_str!("configurator-dataset.json")).unwrap();
    let mut tables: Vec<String> = serde_json::from_value(schema["order"].clone()).unwrap();
    // Drawing filenames are known only after PcInputs. These reads do not
    // download unrelated FileStore contents.
    tables.retain(|t| t != "FileStore");
    // These authored expressions and audit records are omitted by the native
    // import XML. Retain them in the source graph without adding XML elements.
    tables.extend(["PcDynLstExpr".into(), "PcAudit".into()]);
    tables.push("FileStore".into());
    tables
}
pub enum Step {
    Read { action: HostAction, state: Value },
    Ready { records: BTreeMap<String, Value> },
}
pub fn start(company: &str, roots: &[Value], size: u32) -> Result<Step, Diagnostic> {
    if company.is_empty() || size == 0 || size > i32::MAX as u32 {
        return Err(error(
            "Configurator reads need a selected company and a page size from 1 to 2147483647.",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut records = BTreeMap::new();
    let mut visible = BTreeMap::new();
    for root in roots {
        let id = root["ConfigID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Visible root omitted ConfigID."))?;
        if root["Company"] != company || !ids.insert(id.to_lowercase()) {
            return Err(error(
                "Visible roots have foreign ownership or duplicate IDs.",
            ));
        }
        let row_id = root["SysRowID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Visible root omitted SysRowID."))?;
        visible.insert(id.to_owned(), row_id.to_owned());
        let mut record = json!({});
        for table in tables() {
            record[&table] = json!([]);
        }
        records.insert(id.to_owned(), record);
    }
    if records.is_empty() {
        return Ok(Step::Ready { records });
    }
    next(
        json!({"company":company,"size":size,"index":0,"records":records,"visible":visible,"after":null,"seen":{}}),
    )
}
fn next(mut state: Value) -> Result<Step, Diagnostic> {
    let list = tables();
    let index = state["index"]
        .as_u64()
        .ok_or_else(|| error("Missing bulk table index."))? as usize;
    if index == list.len() {
        let records: BTreeMap<String, Value> = serde_json::from_value(state["records"].clone())
            .map_err(|_| error("Epicor returned configurator records in a form Herbert does not recognize, so this page was not saved."))?;
        for (id, record) in &records {
            if record["PcStatus"].as_array().is_none_or(|r| r.len() != 1) {
                return Err(error(format!(
                    "Visible configurator {id} was not returned exactly once by the stored read."
                )));
            }
            if !record["PcStatus"][0]["SysRowID"]
                .as_str()
                .zip(state["visible"][id].as_str())
                .is_some_and(|(stored, visible)| stored.eq_ignore_ascii_case(visible))
            {
                return Err(error(format!(
                    "Configurator {id} changed identity after Epicor discovery."
                )));
            }
        }
        return Ok(Step::Ready { records });
    }
    let table = list
        .get(index)
        .ok_or_else(|| error("Invalid bulk table index."))?;
    if table == "FileStore" {
        let mut drawings: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, record) in state["records"]
            .as_object()
            .ok_or_else(|| error("Epicor's response contained no configurator records where some were expected. Nothing was saved for this page."))?
        {
            for row in record["PcInputs"]
                .as_array()
                .ok_or_else(|| error("Epicor did not return this configurator's inputs, so it was not saved."))?
            {
                if row["ControlType"].as_str().is_some_and(|s| {
                    s.eq_ignore_ascii_case(
                        "Erp.Lib.Configurator.PcControls.EpiPc2DRunTime, Erp.Lib.Configurator",
                    )
                }) {
                    let input = row["InputName"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| error("Drawing input omitted its name."))?;
                    drawings
                        .entry(format!("{id}_{input}.dat"))
                        .or_default()
                        .push(id.clone());
                }
            }
        }
        state["drawings"] = json!(drawings);
        if drawings.is_empty() {
            state["index"] = json!(index + 1);
            return next(state);
        }
    }
    state["phase"] = json!("dictionary");
    Ok(Step::Read {
        action: HostAction::SharedNativeRead {
            service: "Ice.BO.BAQDesignerSvc".into(),
            method: "GetFieldList".into(),
            arguments: json!({"dataTableID":table,"schema":if table=="FileStore"{"Ice"}else{"Erp"}}),
            export: false,
        },
        state,
    })
}
fn query(mut state: Value) -> Result<Step, Diagnostic> {
    let list = tables();
    let table = list
        .get(
            state["index"]
                .as_u64()
                .ok_or_else(|| error("Herbert could not continue this download from its saved progress (configurator table position). Start the download again."))? as usize,
        )
        .ok_or_else(|| error("Herbert could not continue this download from its saved progress (configurator table position). Start the download again."))?;
    let company = state["company"]
        .as_str()
        .ok_or_else(|| error("Epicor did not say which company these records belong to, so they were not saved."))?;
    let size = state["size"]
        .as_u64()
        .and_then(|s| u32::try_from(s).ok())
        .ok_or_else(|| error("The page size for this download must be a whole number of 1 or more. Correct it in the plugin settings."))?;
    let mut criteria = vec![(
        "Company".into(),
        "=".into(),
        crate::report_query::literal(company),
        "".into(),
        "".into(),
        "".into(),
    )];
    if table != "PcInputVar" {
        let keys = if table == "FileStore" {
            &state["drawings"]
        } else {
            &state["records"]
        };
        let ids: Vec<_> = keys
            .as_object()
            .ok_or_else(|| error("Herbert could not continue this download from its saved progress (selected configurators). Start the download again."))?
            .keys()
            .collect();
        if ids.is_empty() {
            return Err(error("An unscoped child query is not allowed."));
        }
        for (i, id) in ids.iter().enumerate() {
            criteria.push((
                if table == "FileStore" {
                    "FileName"
                } else {
                    "ConfigID"
                }
                .into(),
                "=".into(),
                crate::report_query::literal(id),
                if i == 0 { "AND" } else { "OR" }.into(),
                if i == 0 { "(" } else { "" }.into(),
                if i + 1 == ids.len() { ")" } else { "" }.into(),
            ));
        }
    }
    let mut args = crate::report_query::build(
        company,
        table,
        &state["fields"],
        size,
        &criteria,
        state["after"].as_str(),
    )
    .map_err(error)?;
    args["queryDS"]["QueryTable"][0]["DBSchemaName"] =
        json!(if table == "FileStore" { "Ice" } else { "Erp" });
    for rows in args["queryDS"].as_object_mut().unwrap().values_mut() {
        for row in rows.as_array_mut().into_iter().flatten() {
            if row.get("QueryID").is_some() {
                row["QueryID"] = json!("HerbertConfiguratorRead");
            }
        }
    }
    args["queryDS"]["DynamicQuery"][0]["Description"] =
        json!("Unsaved configurator page backup read");
    state["phase"] = json!("rows");
    let (service, method) = ("Ice.BO.DynamicQuerySvc".to_owned(), "Execute".to_owned());
    // Company-wide PcInputVar is identical for every configurator page (only Company, the shared dictionary
    // fields, page size and keyset position): a SharedNativeRead lets the host serve it once per run.
    let action = if table == "PcInputVar" {
        HostAction::SharedNativeRead { service, method, arguments: args, export: false }
    } else {
        HostAction::NativeRead { service, method, arguments: args, export: false }
    };
    Ok(Step::Read { action, state })
}
pub fn receive(mut state: Value, response: Value) -> Result<Step, Diagnostic> {
    let list = tables();
    let index = state["index"]
        .as_u64()
        .ok_or_else(|| error("Herbert could not continue this download from its saved progress (configurator table position). Start the download again."))? as usize;
    let table = list
        .get(index)
        .ok_or_else(|| error("Herbert could not continue this download from its saved progress (configurator table position). Start the download again."))?;
    if state["phase"] == "dictionary" {
        let fields = response
            .pointer("/returnObj/TableFieldList")
            .and_then(Value::as_array)
            .filter(|f| !f.is_empty())
            .ok_or_else(|| error(format!("Dictionary omitted fields for {table}.")))?;
        let names: BTreeSet<_> = fields
            .iter()
            .filter_map(|f| f["FieldName"].as_str())
            .collect();
        for field in [
            "Company",
            "SysRowID",
            if table == "FileStore" {
                "FileName"
            } else if table == "PcInputVar" {
                "Company"
            } else {
                "ConfigID"
            },
        ] {
            if !names.contains(field) {
                return Err(error(format!("Dictionary for {table} omitted {field}.")));
            }
        }
        if names.len() != fields.len() {
            return Err(error("Dictionary fields are duplicated or unnamed."));
        }
        state["fields"] = json!(fields);
        return query(state);
    }
    if state["phase"] != "rows" {
        return Err(error("Unknown bulk read phase."));
    }
    // DynamicQuery can return HTTP success and a Results table together with
    // native query errors. An empty Results array is not proof of absence then.
    if response.pointer("/returnObj/Errors").is_some_and(|errors| {
        !errors.is_null() && errors.as_array().is_none_or(|rows| !rows.is_empty())
    }) {
        return Err(error(format!(
            "Epicor stored {table} query returned errors."
        )));
    }
    let rows = response
        .pointer("/returnObj/Results")
        .and_then(Value::as_array)
        .ok_or_else(|| error(format!("Stored {table} response omitted Results.")))?;
    let size = state["size"]
        .as_u64()
        .ok_or_else(|| error("No page size was set for this download. Enter a page size of 1 or more in the plugin settings."))? as usize;
    if rows.len() > size {
        return Err(error(
            "Stored page exceeded its requested Epicor page size.",
        ));
    }
    for row in rows {
        let id = row["SysRowID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| error("Stored row omitted SysRowID."))?;
        if row["Company"] != state["company"]
            || state["seen"].get(id.to_ascii_lowercase()).is_some()
        {
            return Err(error("Stored page contains foreign or repeated rows."));
        }
        state["seen"][id.to_ascii_lowercase()] = json!(true);
        let owners: Vec<String> = if table == "PcInputVar" {
            state["records"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect()
        } else if table == "FileStore" {
            serde_json::from_value(
                state["drawings"][row["FileName"]
                    .as_str()
                    .ok_or_else(|| error("Drawing omitted its filename."))?]
                .clone(),
            )
            .map_err(|_| error("An unrelated drawing was returned."))?
        } else {
            let owner = row["ConfigID"]
                .as_str()
                .ok_or_else(|| error("Stored child omitted ConfigID."))?;
            let exact = state["records"]
                .as_object()
                .unwrap()
                .keys()
                .find(|k| k.eq_ignore_ascii_case(owner))
                .ok_or_else(|| {
                    error("Stored row belongs to a configurator outside the visible page.")
                })?;
            vec![exact.clone()]
        };
        for owner in owners {
            state["records"][owner][table]
                .as_array_mut()
                .unwrap()
                .push(row.clone());
        }
    }
    if rows.len() == size {
        let after = rows.last().unwrap()["SysRowID"].clone();
        if after == state["after"] {
            return Err(error("Stored page saved position did not advance."));
        }
        state["after"] = after;
        return query(state);
    }
    state["index"] = json!(index + 1);
    state["after"] = Value::Null;
    state["seen"] = json!({});
    state.as_object_mut().unwrap().remove("fields");
    next(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supplemental_rows_remain_owned_source_outside_the_import_schema() {
        let roots = json!([
            {"Company":"C","ConfigID":"A","SysRowID":"00000000-0000-0000-0000-000000000001"},
            {"Company":"C","ConfigID":"B","SysRowID":"00000000-0000-0000-0000-000000000002"}
        ]);
        for table in ["PcDynLstExpr", "PcAudit"] {
            let Step::Read { mut state, .. } = start("C", roots.as_array().unwrap(), 10).unwrap()
            else {
                panic!()
            };
            state["index"] = json!(tables().iter().position(|t| t == table).unwrap());
            state["phase"] = json!("rows");
            // Ensure the final-table path can validate the already read roots.
            for root in roots.as_array().unwrap() {
                state["records"][root["ConfigID"].as_str().unwrap()]["PcStatus"] = json!([root]);
            }
            let row = json!({"Company":"C","ConfigID":"B","SysRowID":"00000000-0000-0000-0000-000000000003","Source":"Unicode Ω & <source>\r\nsecond line"});
            let records = match receive(state, json!({"returnObj":{"Results":[row]}})).unwrap() {
                Step::Read { state, .. } => state["records"].clone(),
                Step::Ready { records } => json!(records),
            };
            assert_eq!(records["B"][table], json!([row]));
            assert_eq!(records["A"][table], json!([]));
            let schema: Value =
                serde_json::from_str(include_str!("configurator-dataset.json")).unwrap();
            assert!(!schema["order"].as_array().unwrap().contains(&json!(table)));
        }
    }
    #[test]
    fn root_recreation_and_native_query_errors_cannot_claim_complete_data() {
        let root =
            json!({"Company":"C","ConfigID":"A","SysRowID":"00000000-0000-0000-0000-000000000001"});
        let Step::Read { mut state, .. } = start("C", &[root.clone()], 10).unwrap() else {
            panic!()
        };
        state["phase"] = json!("rows");
        for errors in [
            json!([{"ErrorText":"Denied"}]),
            json!({"message":"Malformed"}),
        ] {
            assert!(
                receive(
                    state.clone(),
                    json!({"returnObj":{"Results":[],"Errors":errors}})
                )
                .is_err()
            );
        }
        state["index"] = json!(tables().len());
        state["records"]["A"]["PcStatus"] = json!([root]);
        assert!(matches!(next(state.clone()).unwrap(), Step::Ready { .. }));
        state["records"]["A"]["PcStatus"][0]["SysRowID"] =
            json!("00000000-0000-0000-0000-000000000002");
        assert!(next(state).is_err());
        assert!(start("C", &[json!({"Company":"C","ConfigID":"A"})], 10).is_err());
    }
    #[test]
    fn keyset_pages_group_roots_and_reject_repeated_or_unselected_rows() {
        let a =
            json!({"Company":"C","ConfigID":"A","SysRowID":"00000000-0000-0000-0000-000000000001"});
        let b =
            json!({"Company":"C","ConfigID":"B","SysRowID":"00000000-0000-0000-0000-000000000002"});
        let Step::Read { state, .. } = start("C", &[a.clone(), b.clone()], 1).unwrap() else {
            panic!()
        };
        let fields = json!({"returnObj":{"TableFieldList":[{"FieldName":"Company","DataType":"nvarchar"},{"FieldName":"ConfigID","DataType":"nvarchar"},{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]}});
        let Step::Read { action, state } = receive(state, fields).unwrap() else {
            panic!()
        };
        let HostAction::NativeRead { arguments, .. } = action else {
            panic!()
        };
        assert_eq!(
            arguments["queryDS"]["QueryWhereItem"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let Step::Read { action, state } =
            receive(state, json!({"returnObj":{"Results":[a]}})).unwrap()
        else {
            panic!()
        };
        let HostAction::NativeRead { arguments, .. } = action else {
            panic!()
        };
        assert_eq!(arguments["queryDS"]["QueryWhereItem"][3]["CompOp"], ">");
        assert!(receive(state.clone(), json!({"returnObj":{"Results":[a]}})).is_err());
        let mut foreign = b.clone();
        foreign["ConfigID"] = json!("Other");
        assert!(receive(state.clone(), json!({"returnObj":{"Results":[foreign]}})).is_err());
        let Step::Read { state, .. } =
            receive(state, json!({"returnObj":{"Results":[b]}})).unwrap()
        else {
            panic!()
        };
        let Step::Read { state, .. } = receive(state, json!({"returnObj":{"Results":[]}})).unwrap()
        else {
            panic!()
        };
        assert_eq!(state["index"], 1);
        assert_eq!(state["records"]["A"]["PcStatus"][0], a);
        assert_eq!(state["records"]["B"]["PcStatus"][0], b);
        assert!(receive(state, json!({"returnObj":{}})).is_err());
    }
    #[test]
    fn company_input_vars_are_one_shared_read_identical_across_pages() {
        // Two different configurator pages must ask for byte-identical PcInputVar reads, as a SharedNativeRead,
        // so the host's run cache answers the second page; per-configurator tables stay ordinary NativeReads.
        let fields = json!({"returnObj":{"TableFieldList":[{"FieldName":"Company","DataType":"nvarchar"},{"FieldName":"SysRowID","DataType":"uniqueidentifier"},{"FieldName":"VarName","DataType":"nvarchar"}]}});
        let read = |id: &str| {
            let root = json!({"Company":"C","ConfigID":id,"SysRowID":format!("00000000-0000-0000-0000-00000000000{}", id.len())});
            let Step::Read { mut state, .. } = start("C", &[root], 5).unwrap() else { panic!() };
            state["index"] = json!(tables().iter().position(|t| t == "PcInputVar").unwrap());
            state["phase"] = json!("dictionary");
            let Step::Read { action, .. } = receive(state, fields.clone()).unwrap() else { panic!() };
            action
        };
        let (HostAction::SharedNativeRead { arguments: a, .. }, HostAction::SharedNativeRead { arguments: b, .. }) = (read("A"), read("BB")) else { panic!("PcInputVar must be a SharedNativeRead") };
        assert_eq!(a, b);
        let root = json!({"Company":"C","ConfigID":"A","SysRowID":"00000000-0000-0000-0000-000000000001"});
        let Step::Read { state, .. } = start("C", &[root], 5).unwrap() else { panic!() };
        let Step::Read { action, .. } = receive(state, json!({"returnObj":{"TableFieldList":[{"FieldName":"Company","DataType":"nvarchar"},{"FieldName":"ConfigID","DataType":"nvarchar"},{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]}})).unwrap() else { panic!() };
        assert!(matches!(action, HostAction::NativeRead { .. }));
    }
    #[test]
    fn supplied_two_root_bulk_replay() {
        let Ok(base) = std::env::var("HERBERT_CONFIGURATORS_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let mut stored: Value = serde_json::from_slice(
                &std::fs::read(folder.join("stored/classic-function/rows.json")).unwrap(),
            )
            .unwrap();
            let nic: Value = serde_json::from_slice(
                &std::fs::read(folder.join("fixtures/nic/readback.json")).unwrap(),
            )
            .unwrap();
            let mut dictionary: Value = serde_json::from_slice(
                &std::fs::read(folder.join("dictionary/fields.json")).unwrap(),
            )
            .unwrap();
            let observed: Option<Value> =
                std::fs::read(folder.join("bulk-two-root-complete/rows.json"))
                    .ok()
                    .map(|b| serde_json::from_slice(&b).unwrap());
            let roots = vec![stored["PcStatus"][0].clone(), nic["PcStatus"][0].clone()];
            for table in ["PcDynLstExpr", "PcAudit"] {
                if stored.get(table).is_none() {
                    stored[table] = json!([]);
                }
            }
            let extras: Value = serde_json::from_slice(
                &std::fs::read(folder.join("bulk-supplement-dictionary/fields.json")).unwrap(),
            )
            .unwrap();
            dictionary
                .as_object_mut()
                .unwrap()
                .extend(extras.as_object().unwrap().clone());
            let mut step = start("TESTCO", &roots, 100).unwrap();
            let mut queries = vec![];
            loop {
                match step {
                    Step::Ready { records } => {
                        assert_eq!(records.len(), 2);
                        assert_eq!(records["H19C1024A"], stored);
                        if observed.is_some() {
                            std::fs::write(
                                folder.join("bulk-two-root-complete/grouped.json"),
                                serde_json::to_vec_pretty(&records).unwrap(),
                            )
                            .unwrap();
                        }
                        break;
                    }
                    Step::Read { action, state } => {
                        let table = tables()[state["index"].as_u64().unwrap() as usize].clone();
                        let response = match action {
                            HostAction::SharedNativeRead { .. } => {
                                dictionary[&table]["value"].clone()
                            }
                            HostAction::NativeRead { arguments, .. } => {
                                queries.push(arguments);
                                let rows = if let Some(observed) = &observed {
                                    observed[&table].clone()
                                } else if table == "PcStatus" {
                                    json!(roots)
                                } else {
                                    stored[&table].clone()
                                };
                                json!({"returnObj":{"Results":rows}})
                            }
                            _ => panic!("Unexpected action"),
                        };
                        step = receive(state, response).unwrap();
                    }
                }
            }
            assert_eq!(queries.len(), 33);
            std::fs::write(
                folder.join("bulk-two-root-query-plan.json"),
                serde_json::to_vec_pretty(&queries).unwrap(),
            )
            .unwrap();
        }
    }
    #[test]
    fn supplied_native_bulk_replay() {
        let Ok(base) = std::env::var("HERBERT_CONFIGURATORS_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let mut stored: Value = serde_json::from_slice(
                &std::fs::read(folder.join("stored/classic-function/rows.json")).unwrap(),
            )
            .unwrap();
            let mut dictionary: Value = serde_json::from_slice(
                &std::fs::read(folder.join("dictionary/fields.json")).unwrap(),
            )
            .unwrap();
            for table in ["PcDynLstExpr", "PcAudit"] {
                if stored.get(table).is_none() {
                    stored[table] = json!([]);
                }
            }
            let extras: Value = serde_json::from_slice(
                &std::fs::read(folder.join("bulk-supplement-dictionary/fields.json")).unwrap(),
            )
            .unwrap();
            dictionary
                .as_object_mut()
                .unwrap()
                .extend(extras.as_object().unwrap().clone());
            let mut step = start("TESTCO", stored["PcStatus"].as_array().unwrap(), 100).unwrap();
            let mut queries = vec![];
            loop {
                match step {
                    Step::Ready { records } => {
                        assert_eq!(records["H19C1024A"], stored);
                        break;
                    }
                    Step::Read { action, state } => {
                        let table = tables()[state["index"].as_u64().unwrap() as usize].clone();
                        let response = match action {
                            HostAction::SharedNativeRead { method, .. } => {
                                assert_eq!(method, "GetFieldList");
                                dictionary[&table]["value"].clone()
                            }
                            HostAction::NativeRead {
                                method, arguments, ..
                            } => {
                                assert_eq!(method, "Execute");
                                assert_eq!(
                                    arguments["queryDS"]["QueryTable"][0]["DBSchemaName"],
                                    "Erp"
                                );
                                queries.push(arguments);
                                json!({"returnObj":{"Results":stored[&table]}})
                            }
                            _ => panic!("Bulk reader requested a non-read action"),
                        };
                        step = receive(state, response).unwrap();
                    }
                }
            }
            assert_eq!(queries.len(), 33);
            std::fs::write(
                folder.join("bulk-query-plan.json"),
                serde_json::to_vec_pretty(&queries).unwrap(),
            )
            .unwrap();
        }
    }
}
