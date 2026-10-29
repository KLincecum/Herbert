//! A custom style may belong to a shipped Report. Discover styles directly.
//! Never fetch the parent report body to establish the style's ownership.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn query(
    table: &str,
    fields: &[&str],
    values: BTreeMap<String, Value>,
    after: Option<Value>,
    size: Option<u32>,
) -> HostAction {
    let mut parameters = values;
    let mut criteria = parameters
        .keys()
        .map(|name| QueryCriterion {
            table: "StyleSource".into(),
            field: name.clone(),
            operator: "=".into(),
            parameter: name.clone(),
        })
        .collect::<Vec<_>>();
    if let Some(after) = after {
        parameters.insert("after".into(), after);
        criteria.push(QueryCriterion {
            table: "StyleSource".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    HostAction::Query {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: table.into(),
                alias: "StyleSource".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields: fields
                .iter()
                .map(|field| QueryField {
                    table: "StyleSource".into(),
                    field: (*field).into(),
                    alias: (*field).into(),
                    expression: None,
                })
                .collect(),
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
fn identity(row: &Value, category: &str, company: &str) -> Result<Identity, GuestReply> {
    let Some(c) = row["Company"]
        .as_str()
        .filter(|c| c.is_empty() || *c == company)
    else {
        return Err(fail(
            "REPORT_STYLE_SCOPE_INVALID",
            "The style belongs to an unavailable company.",
        ));
    };
    let Some(report) = row["ReportID"].as_str().filter(|s| !s.is_empty()) else {
        return Err(fail(
            "REPORT_STYLE_IDENTITY_INVALID",
            "The style omitted ReportID.",
        ));
    };
    let Some(style) = row["StyleNum"]
        .as_i64()
        .filter(|n| i32::try_from(*n).is_ok())
    else {
        return Err(fail(
            "REPORT_STYLE_IDENTITY_INVALID",
            "The report style number in this selection is not a valid whole number. Choose the report style again.",
        ));
    };
    Ok(Identity {
        category: category.into(),
        kind: "ReportStyles".into(),
        scope: if c.is_empty() {
            Scope::Global
        } else {
            Scope::Company { company: c.into() }
        },
        keys: BTreeMap::from([
            ("ReportID".into(), report.into()),
            ("StyleNum".into(), style.to_string()),
        ]),
    })
}
pub fn discover(request: GuestRequest, family: usize, next_family: Option<String>) -> GuestReply {
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => v.unwrap_or(json!({})),
        Err(_) => return fail("INVALID_CURSOR", "The report-style saved position is invalid."),
    };
    let global = cursor["styleGlobal"].as_bool().unwrap_or(false);
    let next_branch = if global {
        next_family.clone()
    } else {
        Some(json!({"family":family,"styleGlobal":true}).to_string())
    };
    let Some(size) = request.page_size.filter(|s| *s > 0) else {
        return fail(
            "PAGE_SIZE_REQUIRED",
            "Supply a positive discovery page size.",
        );
    };
    if let Some(diagnostic) = request.error {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: next_branch,
            authoritative: false,
            diagnostics: vec![diagnostic],
        };
    }
    let Some(response) = request.response else {
        return GuestReply::Action {
            action: query(
                "ReportStyle",
                &["*"],
                BTreeMap::from([
                    (
                        "Company".into(),
                        json!(if global { "" } else { &request.company }),
                    ),
                    ("SystemFlag".into(), json!(false)),
                ]),
                cursor.get("after").cloned(),
                Some(size),
            ),
            state: json!({"phase":"styleDiscover"}),
        };
    };
    let Some(rows) = response["rows"].as_array() else {
        return fail(
            "REPORT_STYLE_INVENTORY_INVALID",
            "The style inventory omitted its rows.",
        );
    };
    let mut identities = vec![];
    let mut seen = BTreeSet::new();
    for row in rows {
        if row["SystemFlag"] != false {
            return fail(
                "REPORT_STYLE_SCOPE_INVALID",
                "The filtered inventory returned a system or unclassified style.",
            );
        }
        let id = match identity(row, &request.category, &request.company) {
            Ok(id) => id,
            Err(error) => return error,
        };
        if !seen.insert(serde_json::to_string(&id).unwrap()) {
            return fail(
                "REPORT_STYLE_DUPLICATE",
                "The style inventory repeated an identity.",
            );
        }
        identities.push(id);
    }
    let next_cursor = if rows.len() == size as usize {
        let Some(after) = rows
            .last()
            .and_then(|r| r["SysRowID"].as_str())
            .filter(|s| !s.is_empty())
        else {
            return fail(
                "REPORT_STYLE_CURSOR_INVALID",
                "The style inventory omitted its resume point key.",
            );
        };
        if cursor["after"] == after {
            return fail(
                "REPORT_STYLE_CURSOR_INVALID",
                "The style inventory did not advance.",
            );
        }
        Some(json!({"family":family,"styleGlobal":global,"after":after}).to_string())
    } else {
        next_branch
    };
    GuestReply::InventoryData {
        records: identities
            .iter()
            .cloned()
            .zip(rows.iter().cloned().map(|row| json!({"style":row})))
            .collect(),
        inventory: Box::new(GuestReply::Inventory {
            identities,
            next_cursor,
            authoritative: true,
            diagnostics: vec![],
        }),
    }
}

pub enum Acquisition {
    Reply(GuestReply),
    Ready(GuestRequest),
}
pub fn acquire(mut request: GuestRequest) -> Acquisition {
    let id = request.identity.as_ref().unwrap().clone();
    let company = match &id.scope {
        Scope::Global => String::new(),
        Scope::Company { company } if company == &request.company => company.clone(),
        _ => {
            return Acquisition::Reply(fail(
                "REPORT_STYLE_SCOPE_INVALID",
                "Select a global or current-company style.",
            ));
        }
    };
    let Some(style) = id.keys.get("StyleNum").and_then(|v| v.parse::<i32>().ok()) else {
        return Acquisition::Reply(fail(
            "REPORT_STYLE_IDENTITY_INVALID",
            "The report style number in this selection is not a valid whole number. Choose the report style again.",
        ));
    };
    let Some(report) = id.keys.get("ReportID").filter(|v| !v.is_empty()) else {
        return Acquisition::Reply(fail(
            "REPORT_STYLE_IDENTITY_INVALID",
            "Choose a report style to download (the report ID is missing).",
        ));
    };
    let Some(size) = request.page_size.filter(|v| *v > 0) else {
        return Acquisition::Reply(fail(
            "PAGE_SIZE_REQUIRED",
            "Supply a positive report batch size.",
        ));
    };
    if let Some(diagnostic) = request.error.take() {
        return Acquisition::Reply(GuestReply::Failure { diagnostic });
    }
    if request.state["phase"] != "styleAcquire" {
        let saved = request.state.pointer("/_inventory/style").cloned();
        request.state = json!({"phase":"styleAcquire","styleIndex":0,"definition":{},"supplements":{},"errors":[],"missingChildren":[]});
        if let Some(row) = saved {
            request.response = Some(json!({"rows":[row]}));
        }
    }
    const TABLES: &[&str] = &[
        "ReportStyle",
        "Report",
        "RptStylePrinters",
        "ReportStyleImage",
        "ReportStyleRule",
    ];
    let mut index = request.state["styleIndex"].as_u64().unwrap_or(0) as usize;
    if let Some(response) = request.response.take() {
        let Some(rows) = response["rows"].as_array() else {
            return Acquisition::Reply(fail(
                "REPORT_STYLE_READ_INVALID",
                "The report batch omitted its rows.",
            ));
        };
        let Some(table) = TABLES.get(index) else {
            return Acquisition::Reply(fail(
                "REPORT_STYLE_STATE_INVALID",
                "The report saved progress is invalid.",
            ));
        };
        if request.state["definition"][*table].is_null() {
            request.state["definition"][*table] = json!([]);
        }
        for row in rows {
            if *table != "Report"
                && (crate::system_artifacts::marked(row)
                    || row["Company"].as_str() != Some(company.as_str()))
            {
                return Acquisition::Reply(fail(
                    "REPORT_STYLE_SCOPE_INVALID",
                    "The report batch returned a system row or another company's body.",
                ));
            }
            if row["ReportID"].as_str() == Some(report)
                && (*table == "Report" || row["StyleNum"].as_i64() == Some(style.into()))
            {
                request.state["definition"][*table]
                    .as_array_mut()
                    .unwrap()
                    .push(row.clone());
            }
        }
        if index == 0 {
            let rows = request.state["definition"]["ReportStyle"]
                .as_array()
                .unwrap();
            if rows.len() != 1 || rows[0]["SystemFlag"] != false {
                return Acquisition::Reply(fail(
                    "CUSTOM_REPORT_STYLE_NOT_FOUND",
                    "No custom style matches the selected identity.",
                ));
            }
        } else if rows.len() == size as usize {
            let Some(after) = rows
                .last()
                .and_then(|r| r["SysRowID"].as_str())
                .filter(|v| !v.is_empty())
            else {
                return Acquisition::Reply(fail(
                    "REPORT_STYLE_CURSOR_INVALID",
                    "The report batch omitted its next row identity.",
                ));
            };
            if request.state["styleAfter"] == after {
                return Acquisition::Reply(fail(
                    "REPORT_STYLE_CURSOR_INVALID",
                    "The report batch did not advance.",
                ));
            }
            request.state["styleAfter"] = json!(after);
            return acquire(request);
        }
        request.state["styleAfter"] = Value::Null;
        if index == 1 {
            let parents = request.state["definition"]["Report"].as_array().unwrap();
            let allowed = parents.len() == 1
                && parents[0]["SystemFlag"] == false
                && parents[0]["SystemRpt"] == false
                && parents[0]["Company"]
                    .as_str()
                    .is_some_and(|c| c.is_empty() || c == request.company);
            request.state["styleNativeExportAllowed"] = json!(allowed);
            if !allowed {
                request.state["definition"]["Report"] = json!([]);
            }
        }
        index += 1;
        request.state["styleIndex"] = json!(index);
    }
    let Some(table) = TABLES.get(index) else {
        request.state["styleScopeComplete"] = json!(true);
        return Acquisition::Ready(request);
    };
    if *table == "ReportStyleRule" {
        return Acquisition::Reply(crate::tables::report_rule_page(
            &request,
            &company,
            request.state["styleAfter"].clone(),
            request.state.clone(),
        ));
    }
    let mut values = BTreeMap::new();
    if index != 1 {
        values.insert("Company".into(), json!(company));
    }
    if index == 0 {
        values.insert("ReportID".into(), json!(report));
        values.insert("StyleNum".into(), json!(style));
    }
    if matches!(*table, "ReportStyle" | "RptStylePrinters") {
        values.insert("SystemFlag".into(), json!(false));
    }
    let fields: &[&str] = if index == 1 {
        &["Company", "ReportID", "SysRowID", "SystemFlag", "SystemRpt"]
    } else {
        &["*"]
    };
    let HostAction::Query { mut query } = query(
        table,
        fields,
        values,
        request
            .state
            .get("styleAfter")
            .filter(|v| !v.is_null())
            .cloned(),
        if index == 0 { None } else { Some(size) },
    ) else {
        unreachable!()
    };
    if index >= 2 {
        query.tables.push(QueryTable {
            schema: "Ice".into(),
            table: "ReportStyle".into(),
            alias: "CustomStyle".into(),
            company_field: None,
            metadata: vec![],
        });
        for key in ["Company", "ReportID", "StyleNum"] {
            query.joins.push(QueryJoin {
                left_table: "StyleSource".into(),
                left_field: key.into(),
                right_table: "CustomStyle".into(),
                right_field: key.into(),
                kind: "Inner".into(),
            });
        }
        query.parameters.insert("customStyle".into(), json!(false));
        query.criteria.push(QueryCriterion {
            table: "CustomStyle".into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "customStyle".into(),
        });
    }
    Acquisition::Reply(GuestReply::Action {
        action: HostAction::SharedQuery { query },
        state: request.state,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn style(n: i32) -> Value {
        json!({"Company":"C","ReportID":"R","StyleNum":n,"SystemFlag":false,"SysRowID":format!("s{n}"),"PrintProgram":"reports/CustomReports/Keep","Description":"Keep Ω"})
    }
    fn req() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "report-styles-and-layouts".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    fn selected(n: i32) -> GuestRequest {
        let mut r = req();
        r.operation = "acquire".into();
        r.identity = Some(identity(&style(n), "report-styles-and-layouts", "C").unwrap());
        r.state = json!({"_inventory":{"style":style(n)}});
        r
    }
    #[test]
    fn full_style_inventory_is_retained_and_system_rows_are_excluded() {
        let GuestReply::Action {
            action: HostAction::Query { query },
            ..
        } = discover(req(), 0, None)
        else {
            panic!()
        };
        assert_eq!(query.parameters["SystemFlag"], false);
        assert_eq!(query.fields[0].field, "*");
        let mut r = req();
        r.response = Some(json!({"rows":[style(1001)]}));
        let GuestReply::InventoryData { records, .. } = discover(r.clone(), 0, None) else {
            panic!()
        };
        assert_eq!(records[0].1["style"]["Description"], "Keep Ω");
        r.response.as_mut().unwrap()["rows"][0]["SystemFlag"] = json!(true);
        assert!(matches!(discover(r, 0, None), GuestReply::Failure { .. }));
    }
    #[test]
    fn styles_share_parent_and_child_pages_and_preserve_their_own_values() {
        let mut actions = vec![];
        for n in [1001, 1002] {
            let mut r = selected(n);
            let mut trace = vec![];
            loop {
                match acquire(r.clone()) {
                    Acquisition::Reply(GuestReply::Action {
                        action: HostAction::SharedQuery { query },
                        state,
                    }) => {
                        let table = query.tables[0].table.clone();
                        assert_ne!(
                            table, "ReportStyle",
                            "Inventory style must not be downloaded again"
                        );
                        trace.push(serde_json::to_value(&query).unwrap());
                        r.state = state;
                        let rows = if table == "Report" {
                            assert!(query.fields.iter().all(|f| f.field != "*"));
                            vec![
                                json!({"Company":"","ReportID":"R","SysRowID":"parent","SystemFlag":false,"SystemRpt":false}),
                            ]
                        } else if query.parameters.contains_key("after") {
                            vec![]
                        } else {
                            let (parameter, alias) = if table == "ReportStyleRule" {
                                ("customRuleStyle", "RuleStyle")
                            } else {
                                ("customStyle", "CustomStyle")
                            };
                            assert_eq!(query.parameters[parameter], false);
                            assert!(query.joins.iter().any(|j| j.right_table == alias));
                            vec![
                                json!({"Company":"C","ReportID":"R","StyleNum":1001,"SysRowID":"a","Value":"One Ω"}),
                                json!({"Company":"C","ReportID":"R","StyleNum":1002,"SysRowID":"b","Value":"Two Ω"}),
                            ]
                        };
                        r.response = Some(json!({"rows":rows}));
                    }
                    Acquisition::Ready(done) => {
                        assert_eq!(done.state["styleNativeExportAllowed"], true);
                        assert_eq!(
                            done.state["definition"]["ReportStyle"][0]["Description"],
                            "Keep Ω"
                        );
                        for table in ["RptStylePrinters", "ReportStyleImage", "ReportStyleRule"] {
                            let rows = done.state["definition"][table].as_array().unwrap();
                            assert_eq!(rows.len(), 1);
                            assert_eq!(rows[0]["Value"], if n == 1001 { "One Ω" } else { "Two Ω" });
                        }
                        break;
                    }
                    _ => panic!(),
                }
            }
            actions.push(trace);
        }
        assert_eq!(
            actions[0], actions[1],
            "Both styles must use identical shared batches"
        );
    }
    #[test]
    fn system_parent_metadata_never_becomes_downloaded_parent_content() {
        let mut r = selected(1001);
        let Acquisition::Reply(GuestReply::Action { state, .. }) = acquire(r.clone()) else {
            panic!()
        };
        r.state = state;
        r.response = Some(
            json!({"rows":[{"Company":"","ReportID":"R","SystemFlag":true,"SystemRpt":true,"SysRowID":"p"}]}),
        );
        let Acquisition::Reply(GuestReply::Action { state, .. }) = acquire(r) else {
            panic!()
        };
        assert_eq!(state["styleNativeExportAllowed"], false);
        assert_eq!(state["definition"]["Report"], json!([]));
    }
}
