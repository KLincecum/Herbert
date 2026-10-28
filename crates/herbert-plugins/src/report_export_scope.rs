//! Share System-child classification pages across native report exports.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub fn start(request: &mut GuestRequest, children: &[String]) -> Option<HostAction> {
    // These two native tableset projections have no SystemFlag. They are not
    // persisted child definitions (verified native schema on both supported lanes).
    request.state["reportExportTables"] = json!(
        children
            .iter()
            .filter(|s| !matches!(s.as_str(), "RptDataSourceField" | "RptDataSourceParameter"))
            .collect::<Vec<_>>()
    );
    request.state["reportExportIndex"] = json!(0);
    request.state["reportExportAfter"] = Value::Null;
    request.state["reportExportAllowed"] = json!(true);
    request.response = None;
    request.error = None;
    complete(request)
}
fn refuse(request: &mut GuestRequest, diagnostic: Diagnostic) -> Option<HostAction> {
    request.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(diagnostic));
    request.state["reportExportAllowed"] = json!(false);
    request.state["phase"] = json!("reportExportChecked");
    None
}
pub fn complete(request: &mut GuestRequest) -> Option<HostAction> {
    let key = if request.identity.as_ref().unwrap().kind == "BAQReport" {
        "BAQRptID"
    } else {
        "RptDefID"
    };
    if let Some(error) = request.error.take() {
        return refuse(request, error);
    }
    let Some(mut index) = request.state["reportExportIndex"]
        .as_u64()
        .map(|v| v as usize)
    else {
        return refuse(
            request,
            Diagnostic::new(
                "REPORT_EXPORT_SCOPE_UNVERIFIED",
                "The native export scope checkpoint is invalid.",
            ),
        );
    };
    if let Some(response) = request.response.take() {
        let Some(rows) = response["rows"].as_array() else {
            return refuse(
                request,
                Diagnostic::new(
                    "REPORT_EXPORT_SCOPE_UNVERIFIED",
                    "The native export ownership query omitted its rows.",
                ),
            );
        };
        if rows
            .iter()
            .any(|row| !row[key].is_string() || !row["SysRowID"].is_string())
        {
            return refuse(
                request,
                Diagnostic::new(
                    "REPORT_EXPORT_SCOPE_UNVERIFIED",
                    "A report classification row omitted its identity.",
                ),
            );
        }
        if rows
            .iter()
            .any(|row| row[key] == request.identity.as_ref().unwrap().keys[key])
        {
            return refuse(
                request,
                Diagnostic::new(
                    "REPORT_NATIVE_EXPORT_MIXED_SCOPE",
                    "The native report export includes system-marked children. Only independently filtered custom definitions are retained; the unfiltered native export is unavailable.",
                ),
            );
        }
        if request
            .page_size
            .is_some_and(|size| rows.len() >= size as usize)
        {
            let after = rows.last().unwrap()["SysRowID"].clone();
            if after == request.state["reportExportAfter"] {
                return refuse(
                    request,
                    Diagnostic::new(
                        "REPORT_EXPORT_SCOPE_UNVERIFIED",
                        "The report classification page did not advance.",
                    ),
                );
            }
            request.state["reportExportAfter"] = after;
        } else {
            index += 1;
            request.state["reportExportIndex"] = json!(index);
            request.state["reportExportAfter"] = Value::Null;
        }
    }
    let Some(table) = request.state["reportExportTables"]
        .get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        request.state["phase"] = json!("reportExportChecked");
        return None;
    };
    let mut criteria = vec![QueryCriterion {
        table: "ReportChild".into(),
        field: "SystemFlag".into(),
        operator: "=".into(),
        parameter: "system".into(),
    }];
    let mut parameters = BTreeMap::from([("system".into(), json!(true))]);
    if let Some(after) = request.state["reportExportAfter"].as_str() {
        criteria.push(QueryCriterion {
            table: "ReportChild".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
        parameters.insert("after".into(), json!(after));
    }
    if key == "BAQRptID" {
        criteria.push(QueryCriterion {
            table: "ReportChild".into(),
            field: "Company".into(),
            operator: "=".into(),
            parameter: "company".into(),
        });
        parameters.insert(
            "company".into(),
            json!(match &request.identity.as_ref().unwrap().scope {
                Scope::Global => "",
                Scope::Company { company } => company.as_str(),
                _ => "",
            }),
        );
    }
    request.state["phase"] = json!("reportExportScope");
    Some(HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: table.into(),
                alias: "ReportChild".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields: ["SysRowID", key]
                .into_iter()
                .map(|field| QueryField {
                    table: "ReportChild".into(),
                    field: field.into(),
                    alias: field.into(),
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
            page_size: request.page_size,
            page: 1,
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "C16".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "C16".into(),
                kind: "ReportDataDefinition".into(),
                scope: Scope::Global,
                keys: BTreeMap::from([("RptDefID".into(), "R".into())]),
            }),
            cursor: None,
            page_size: None,
            state: json!({"errors":[]}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn minimal_scope_probe_refuses_mixed_malformed_and_denied_exports() {
        for case in ["mixed", "malformed", "denied"] {
            let mut r = request();
            let Some(HostAction::SharedQuery { query }) = start(&mut r, &["RptLiterals".into()])
            else {
                panic!()
            };
            assert_eq!(query.fields.len(), 2);
            assert_eq!(query.fields[0].field, "SysRowID");
            assert_eq!(query.parameters["system"], true);
            match case {
                "mixed" => r.response = Some(json!({"rows":[{"SysRowID":"stock","RptDefID":"R"}]})),
                "malformed" => r.response = Some(json!({})),
                _ => r.error = Some(Diagnostic::new("DENIED", "refused")),
            };
            assert!(complete(&mut r).is_none());
            assert_eq!(r.state["reportExportAllowed"], false);
            assert_eq!(r.state["errors"].as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn all_empty_system_probes_allow_native_export() {
        let mut r = request();
        assert!(start(&mut r, &["RptLiterals".into(), "RptTable".into()]).is_some());
        r.response = Some(json!({"rows":[]}));
        assert!(complete(&mut r).is_some());
        r.response = Some(json!({"rows":[]}));
        assert!(complete(&mut r).is_none());
        assert_eq!(r.state["reportExportAllowed"], true);
    }
}
