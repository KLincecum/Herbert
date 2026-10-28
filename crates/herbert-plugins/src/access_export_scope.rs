//! Share System classification pages across access exports in this run.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub fn start(request: &mut GuestRequest) -> Option<HostAction> {
    request.state["accessExportTables"] =
        json!(["AccessScope", "AccessScopeEntity", "AccessScopeBOMethod"]);
    request.state["accessExportIndex"] = json!(0);
    request.state["accessExportAfter"] = Value::Null;
    request.state["accessExportAllowed"] = json!(true);
    request.response = None;
    request.error = None;
    // Both native exporters resolve GetByID(Session.CompanyID, id), not the selected Company.
    if request.identity.as_ref().unwrap().keys["Company"] != request.company {
        return refuse(
            request,
            Diagnostic::new(
                "ACCESS_EXPORT_COMPANY_UNSUPPORTED",
                "The native exporter only supports the session company. The filtered definition is retained without a mismatched native export.",
            ),
        );
    }
    complete(request)
}
fn refuse(request: &mut GuestRequest, diagnostic: Diagnostic) -> Option<HostAction> {
    request.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(diagnostic));
    request.state["accessExportAllowed"] = json!(false);
    request.state["phase"] = json!("accessExportChecked");
    None
}
pub fn complete(request: &mut GuestRequest) -> Option<HostAction> {
    if let Some(error) = request.error.take() {
        return refuse(request, error);
    }
    let Some(mut index) = request.state["accessExportIndex"]
        .as_u64()
        .map(|v| v as usize)
    else {
        return refuse(
            request,
            Diagnostic::new(
                "ACCESS_EXPORT_SCOPE_UNVERIFIED",
                "The native export scope checkpoint is invalid.",
            ),
        );
    };
    if let Some(response) = request.response.take() {
        let Some(rows) = response["rows"].as_array() else {
            return refuse(
                request,
                Diagnostic::new(
                    "ACCESS_EXPORT_SCOPE_UNVERIFIED",
                    "The native export ownership query omitted its rows.",
                ),
            );
        };
        if rows
            .iter()
            .any(|row| !row["AccessScopeID"].is_string() || !row["SysRowID"].is_string())
        {
            return refuse(
                request,
                Diagnostic::new(
                    "ACCESS_EXPORT_SCOPE_UNVERIFIED",
                    "An access classification row omitted its identity.",
                ),
            );
        }
        if rows.iter().any(|row| {
            row["AccessScopeID"] == request.identity.as_ref().unwrap().keys["AccessScopeID"]
        }) {
            return refuse(
                request,
                Diagnostic::new(
                    "ACCESS_NATIVE_EXPORT_MIXED_SCOPE",
                    "The native access scope export includes system-marked definitions. Only independently filtered custom definitions are retained; the unfiltered native export is unavailable.",
                ),
            );
        }
        if request
            .page_size
            .is_some_and(|size| rows.len() >= size as usize)
        {
            let after = rows.last().unwrap()["SysRowID"].clone();
            if after == request.state["accessExportAfter"] {
                return refuse(
                    request,
                    Diagnostic::new(
                        "ACCESS_EXPORT_SCOPE_UNVERIFIED",
                        "The access classification page did not advance.",
                    ),
                );
            }
            request.state["accessExportAfter"] = after;
        } else {
            index += 1;
            request.state["accessExportIndex"] = json!(index);
            request.state["accessExportAfter"] = Value::Null;
        }
    }
    let Some(table) = request.state["accessExportTables"]
        .get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        request.state["phase"] = json!("accessExportChecked");
        return None;
    };
    let id = request.identity.as_ref().unwrap();
    let mut criteria = vec![
        QueryCriterion {
            table: "AccessDefinition".into(),
            field: "Company".into(),
            operator: "=".into(),
            parameter: "company".into(),
        },
        QueryCriterion {
            table: "AccessDefinition".into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "system".into(),
        },
    ];
    let mut parameters = BTreeMap::from([
        ("company".into(), json!(id.keys["Company"])),
        ("system".into(), json!(true)),
    ]);
    if let Some(after) = request.state["accessExportAfter"].as_str() {
        criteria.push(QueryCriterion {
            table: "AccessDefinition".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
        parameters.insert("after".into(), json!(after));
    }
    request.state["phase"] = json!("accessExportScope");
    Some(HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: table.into(),
                alias: "AccessDefinition".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields: ["SysRowID", "AccessScopeID"]
                .into_iter()
                .map(|field| QueryField {
                    table: "AccessDefinition".into(),
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
            category: "C26".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "C26".into(),
                kind: "AccessScope".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([
                    ("Company".into(), "C".into()),
                    ("AccessScopeID".into(), "R".into()),
                ]),
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
            let Some(HostAction::SharedQuery { query }) = start(&mut r) else {
                panic!()
            };
            assert_eq!(query.fields.len(), 2);
            assert_eq!(query.fields[0].field, "SysRowID");
            assert_eq!(query.parameters["system"], true);
            match case {
                "mixed" => {
                    r.response = Some(json!({"rows":[{"SysRowID":"stock","AccessScopeID":"R"}]}))
                }
                "malformed" => r.response = Some(json!({})),
                _ => r.error = Some(Diagnostic::new("DENIED", "refused")),
            };
            assert!(complete(&mut r).is_none());
            assert_eq!(r.state["accessExportAllowed"], false);
            assert_eq!(r.state["errors"].as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn global_definition_never_exports_session_company() {
        let mut r = request();
        r.identity
            .as_mut()
            .unwrap()
            .keys
            .insert("Company".into(), "".into());
        r.identity.as_mut().unwrap().scope = Scope::Global;
        assert!(start(&mut r).is_none());
        assert_eq!(r.state["accessExportAllowed"], false);
        assert_eq!(
            r.state["errors"][0]["code"],
            "ACCESS_EXPORT_COMPANY_UNSUPPORTED"
        );
    }
    #[test]
    fn all_empty_system_probes_allow_native_export() {
        let mut r = request();
        assert!(start(&mut r).is_some());
        r.response = Some(json!({"rows":[]}));
        assert!(complete(&mut r).is_some());
        r.response = Some(json!({"rows":[]}));
        assert!(complete(&mut r).is_some());
        r.response = Some(json!({"rows":[]}));
        assert!(complete(&mut r).is_none());
        assert_eq!(r.state["accessExportAllowed"], true);
    }
    #[test]
    fn scopes_share_queries_and_unrelated_system_rows_do_not_exclude_custom_exports() {
        let mut a = request();
        a.page_size = Some(2);
        let mut b = a.clone();
        b.identity
            .as_mut()
            .unwrap()
            .keys
            .insert("AccessScopeID".into(), "Other".into());
        let first = start(&mut a).unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(start(&mut b).unwrap()).unwrap()
        );
        assert!(matches!(first, HostAction::SharedQuery { .. }));
        a.response = Some(
            json!({"rows":[{"AccessScopeID":"StockOne","SysRowID":"one"},{"AccessScopeID":"StockTwo","SysRowID":"two"}]}),
        );
        let Some(HostAction::SharedQuery { query }) = complete(&mut a) else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "two");
        assert_eq!(a.state["accessExportIndex"], 0);
        a.response = Some(json!({"rows":[]}));
        let Some(HostAction::SharedQuery { query }) = complete(&mut a) else {
            panic!()
        };
        assert!(!query.parameters.contains_key("after"));
        assert_eq!(a.state["accessExportIndex"], 1);
        assert_eq!(a.state["accessExportAllowed"], true);
    }
}
