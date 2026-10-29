//! Routing rules are classified with their custom style during inventory.
//! Export each style once. Extract readable rule text from the saved response.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn push(request: &mut GuestRequest, code: &str, message: &str) {
    request.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(Diagnostic::new(code, message)));
}
fn done(request: &mut GuestRequest) -> Option<HostAction> {
    request.state["phase"] = json!("reportRuleComplete");
    None
}
pub fn start(request: &mut GuestRequest) -> Option<HostAction> {
    request.state["phase"] = json!("reportRuleParents");
    request.state["reportParents"] = json!([]);
    parent_batch(request)
}
fn parent_batch(request: &mut GuestRequest) -> Option<HostAction> {
    let Some(size) = request.page_size.filter(|v| *v > 0) else {
        push(
            request,
            "PAGE_SIZE_REQUIRED",
            "Supply a positive report batch size.",
        );
        return done(request);
    };
    let mut parameters = BTreeMap::new();
    let mut criteria = vec![];
    if let Some(after) = request.state["reportParentAfter"].as_str() {
        parameters.insert("after".into(), json!(after));
        criteria.push(QueryCriterion {
            table: "StyleSource".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    // The exporter selects ReportID without a company. One shared metadata
    // inventory determines which IDs resolve to a custom report in this scope.
    Some(HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: "Report".into(),
                alias: "StyleSource".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields: ["Company", "ReportID", "SysRowID", "SystemFlag", "SystemRpt"]
                .iter()
                .map(|f| QueryField {
                    table: "StyleSource".into(),
                    field: (*f).into(),
                    alias: (*f).into(),
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
            page_size: Some(size),
            page: 1,
        },
    })
}
fn same(row: &Value, expected: &Value, keys: &[&str]) -> bool {
    keys.iter().all(|key| {
        !row[*key].is_null()
            && if *key == "SysRowID" {
                row[*key]
                    .as_str()
                    .zip(expected[*key].as_str())
                    .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            } else {
                row[*key] == expected[*key]
            }
    })
}
pub fn complete(request: &mut GuestRequest) -> Option<HostAction> {
    if let Some(error) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(error));
        return done(request);
    }
    let response = request.response.take().unwrap_or(Value::Null);
    let row = request.state["root"].clone();
    if request.state["phase"] == "reportRuleParents" {
        let Some(rows) = response["rows"].as_array() else {
            push(
                request,
                "REPORT_PARENT_INVENTORY_INVALID",
                "The report inventory omitted its rows.",
            );
            return done(request);
        };
        for parent in rows {
            if parent["ReportID"] == row["ReportID"] {
                request.state["reportParents"]
                    .as_array_mut()
                    .unwrap()
                    .push(parent.clone());
            }
        }
        if rows.len() == request.page_size.unwrap_or(0) as usize {
            let Some(after) = rows
                .last()
                .and_then(|r| r["SysRowID"].as_str())
                .filter(|v| !v.is_empty())
            else {
                push(
                    request,
                    "REPORT_PARENT_CURSOR_INVALID",
                    "The report batch omitted its next row identity.",
                );
                return done(request);
            };
            if request.state["reportParentAfter"] == after {
                push(
                    request,
                    "REPORT_PARENT_CURSOR_INVALID",
                    "The report batch did not advance.",
                );
                return done(request);
            }
            request.state["reportParentAfter"] = json!(after);
            return parent_batch(request);
        }
        let parents = request.state["reportParents"].as_array().unwrap();
        if parents.len() != 1
            || parents[0]["SystemFlag"] != false
            || parents[0]["SystemRpt"] != false
            || !parents[0]["Company"]
                .as_str()
                .is_some_and(|v| v.is_empty() || v == request.company)
        {
            // Keep the custom rule already downloaded. ExportReportStyle would
            // also download the shipped or foreign parent report.
            return done(request);
        }
        request.state["phase"] = json!("reportRuleExport");
        return Some(HostAction::SharedNativeRead {
            service: "Ice.BO.ReportSvc".into(),
            method: "ExportReportStyle".into(),
            arguments: json!({"reportId":row["ReportID"],"styleNumber":row["StyleNum"]}),
            export: true,
        });
    }
    let data = &response["returnObj"];
    let valid_parent = data["Report"].as_array().is_some_and(|v| {
        v.len() == 1
            && same(&v[0], &row, &["ReportID"])
            && v[0]["SystemFlag"] == false
            && v[0]["SystemRpt"] == false
            && v[0]["Company"]
                .as_str()
                .is_some_and(|c| c.is_empty() || c == request.company)
    });
    let valid_style = data["ReportStyle"].as_array().is_some_and(|v| {
        v.len() == 1
            && same(&v[0], &row, &["Company", "ReportID", "StyleNum"])
            && v[0]["SystemFlag"] == false
    });
    let rule = data["ReportStyleRule"]
        .as_array()
        .filter(|v| v.len() == 1)
        .and_then(|v| v.first())
        .filter(|v| same(v, &row, &["Company", "ReportID", "StyleNum", "SysRowID"]));
    let custom = data.as_object().is_some_and(|v| {
        !v.values()
            .filter_map(Value::as_array)
            .flatten()
            .any(crate::system_artifacts::marked)
    });
    let valid_images = data["ReportStyleImage"].as_array().is_some_and(|v| {
        v.iter()
            .all(|i| same(i, &row, &["Company", "ReportID", "StyleNum"]))
    });
    if !valid_parent || !valid_style || !custom || !valid_images || rule.is_none() {
        push(
            request,
            "REPORT_RULE_EXPORT_INVALID",
            "The export did not contain the selected custom report rule and style.",
        );
        return done(request);
    }
    // The export is the download, not a second opinion on an earlier body.
    request.state["tables"]["ReportStyleRule"] = json!([rule.unwrap()]);
    request.state["tables"]["nativeReportStyleExport"] = response;
    done(request)
}
pub fn attach(definition: &mut Value) -> Vec<Diagnostic> {
    let Some(body) = definition
        .pointer("/ReportStyleRule/0/RuleBody")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return vec![Diagnostic::new(
            "REPORT_RULE_BODY_UNVERIFIED",
            "The routing rule omitted its original RuleBody text.",
        )];
    };
    if !definition["herbertSources"].is_object() {
        definition["herbertSources"] = json!({});
    }
    definition["herbertSources"]["files"] = json!([{"name":if body.trim_start().starts_with('<'){"RuleBody.xml"}else{"RuleBody.txt"},"text":body}]);
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req() -> GuestRequest {
        let row = json!({"Company":"C","ReportID":"Owned","StyleNum":1001,"SysRowID":"r","RuleBody":"<Rule>Ω</Rule>","IsEnabled":false});
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "report-behavior".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "report-behavior".into(),
                kind: "PersistedReportStyleRule".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([
                    ("Company".into(), "C".into()),
                    ("SysRowID".into(), "r".into()),
                ]),
            }),
            cursor: None,
            page_size: Some(2),
            state: json!({"root":row,"tables":{"ReportStyleRule":[row]},"errors":[]}),
            response: None,
            error: None,
        }
    }
    fn parent() -> Value {
        json!({"Company":"","ReportID":"Owned","SysRowID":"p","SystemFlag":false,"SystemRpt":false})
    }
    #[test]
    fn rule_inventory_filters_the_style_before_returning_bodies() {
        let mut r = req();
        r.operation = "discover".into();
        r.identity = None;
        r.state = Value::Null;
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            ..
        } = crate::tables::handle(r)
        else {
            panic!()
        };
        assert!(
            query
                .joins
                .iter()
                .any(|j| j.right_table == "RuleStyle" && j.right_field == "StyleNum")
        );
        assert_eq!(query.parameters["customRuleStyle"], false);
        assert!(query.fields.iter().any(|f| f.field == "*"));
    }
    #[test]
    fn different_rules_share_parent_inventory_then_export_once() {
        let mut r = req();
        let first = start(&mut r).unwrap();
        let mut other = req();
        other.state["root"]["ReportID"] = json!("Other");
        let second = start(&mut other).unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(second).unwrap()
        );
        r.response = Some(json!({"rows":[parent()]}));
        let Some(HostAction::SharedNativeRead {
            method, arguments, ..
        }) = complete(&mut r)
        else {
            panic!()
        };
        assert_eq!(method, "ExportReportStyle");
        assert_eq!(arguments, json!({"reportId":"Owned","styleNumber":1001}));
        let mut exported = r.state["root"].clone();
        exported["RuleBody"] = json!("<Rule>Updated Ω</Rule>");
        r.response = Some(
            json!({"returnObj":{"Report":[parent()],"ReportStyle":[{"Company":"C","ReportID":"Owned","StyleNum":1001,"SystemFlag":false}],"ReportStyleRule":[exported],"ReportStyleImage":[]}}),
        );
        assert!(complete(&mut r).is_none());
        assert_eq!(r.state["errors"], json!([]));
        assert!(attach(&mut r.state["tables"]).is_empty());
        assert_eq!(
            r.state["tables"]["herbertSources"]["files"][0]["text"],
            "<Rule>Updated Ω</Rule>"
        );
    }
    #[test]
    fn stock_parent_keeps_custom_rule_without_downloading_stock_export() {
        let mut r = req();
        start(&mut r);
        let mut p = parent();
        p["SystemRpt"] = json!(true);
        r.response = Some(json!({"rows":[p]}));
        assert!(complete(&mut r).is_none());
        assert!(r.state["tables"].get("nativeReportStyleExport").is_none());
        assert_eq!(
            r.state["tables"]["ReportStyleRule"][0]["RuleBody"],
            "<Rule>Ω</Rule>"
        );
    }
    #[test]
    fn refusal_keeps_downloaded_rule_and_does_not_start_more_reads() {
        let mut r = req();
        start(&mut r);
        r.response = Some(json!({"rows":[parent()]}));
        complete(&mut r);
        r.error = Some(Diagnostic::new("ACCESS_DENIED", "Refused."));
        assert!(complete(&mut r).is_none());
        assert_eq!(r.state["errors"][0]["code"], "ACCESS_DENIED");
        assert_eq!(
            r.state["tables"]["ReportStyleRule"][0]["RuleBody"],
            "<Rule>Ω</Rule>"
        );
    }
}
