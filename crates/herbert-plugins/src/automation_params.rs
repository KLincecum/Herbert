//! Process-set parameters are persisted outside ProcessSetTableset.
//! Preserve values by default. Opt-in filtering projects credentials as NULL before retention.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const FIELDS: &[&str] = &[
    "Company",
    "SystemCode",
    "ProcessID",
    "SysTaskNum",
    "ParamName",
    "ParamDataType",
    "ParamDate",
    "ParamDecimal",
    "ParamInteger",
    "ParamLong",
    "ParamLabel",
    "ParamLogical",
    "ParamType",
    "SystemFlag",
    "SysRowID",
];
fn query(r: &GuestRequest) -> HostAction {
    let filter_sensitive = crate::response_policy::sensitive_filter_enabled(r);
    let id = r.identity.as_ref().unwrap();
    let mut fields: Vec<_> = FIELDS
        .iter()
        .map(|f| QueryField {
            table: "Parameter".into(),
            field: (*f).into(),
            alias: (*f).into(),
            expression: None,
        })
        .collect();
    if !filter_sensitive {
        for name in ["ParamCharacter", "ParamToken"] {
            fields.push(QueryField {
                table: "Parameter".into(),
                field: name.into(),
                alias: name.into(),
                expression: None,
            });
        }
    }
    for (alias, expression) in [
        (
            "ParamCharacter",
            "CASE WHEN LOWER(Parameter.ParamName) LIKE '%password%' OR LOWER(Parameter.ParamName) LIKE '%secret%' OR LOWER(Parameter.ParamName) LIKE '%apikey%' OR LOWER(Parameter.ParamName) LIKE '%access[_]token%' THEN CAST(NULL AS nvarchar(max)) ELSE Parameter.ParamCharacter END",
        ),
        ("ParamToken", "CAST(NULL AS nvarchar(max))"),
        (
            "SysRevID",
            "CONVERT(nvarchar(max), CONVERT(bigint, Parameter.SysRevID))",
        ),
    ] {
        if !filter_sensitive && alias != "SysRevID" {
            continue;
        }
        fields.push(QueryField {
            table: "Parameter".into(),
            field: "ParamName".into(),
            alias: alias.into(),
            expression: Some(expression.into()),
        });
    }
    let mut parameters = BTreeMap::from([("custom".into(), json!(false))]);
    let mut criteria = Vec::new();
    parameters.insert("Company".into(), json!(id.keys["Company"]));
    criteria.push(QueryCriterion {
        table: "Parameter".into(),
        field: "Company".into(),
        operator: "=".into(),
        parameter: "Company".into(),
    });
    if let Some(after) = r.state["automationAfter"].as_str() {
        parameters.insert("after".into(), json!(after));
        criteria.push(QueryCriterion {
            table: "Parameter".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    for table in ["Parameter", "Task", "Parent"] {
        criteria.push(QueryCriterion {
            table: table.into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "custom".into(),
        });
    }
    let mut joins = Vec::new();
    for (left, right, keys) in [
        (
            "Parameter",
            "Task",
            vec!["Company", "SystemCode", "ProcessID", "SysTaskNum"],
        ),
        ("Task", "Parent", vec!["Company", "SystemCode", "ProcessID"]),
    ] {
        for key in keys {
            joins.push(QueryJoin {
                left_table: left.into(),
                left_field: key.into(),
                right_table: right.into(),
                right_field: key.into(),
                kind: "Inner".into(),
            });
        }
    }
    HostAction::SharedQuery {
        query: QuerySpec {
            tables: [
                ("ProcessTaskParam", "Parameter"),
                ("ProcessTask", "Task"),
                ("ProcessSet", "Parent"),
            ]
            .into_iter()
            .map(|(table, alias)| QueryTable {
                schema: "Ice".into(),
                table: table.into(),
                alias: alias.into(),
                company_field: None,
                metadata: vec![],
            })
            .collect(),
            fields,
            criteria,
            parameters,
            joins,
            order: vec![QueryOrder {
                field: "SysRowID".into(),
                descending: false,
            }],
            page_size: r.page_size,
            page: 1,
        },
    }
}
pub fn start(r: &mut GuestRequest) -> Option<HostAction> {
    r.state["phase"] = json!("automationParameters");
    r.state["automationAfter"] = Value::Null;
    r.state["automationParamIds"] = json!([]);
    r.state["supplements"]["ProcessTaskParam"] = json!([]);
    Some(query(r))
}
fn issue(r: &mut GuestRequest, code: &str, message: &str) {
    r.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(Diagnostic::new(code, message)));
}
pub fn complete(r: &mut GuestRequest) -> Option<HostAction> {
    r.state["phase"] = json!("automationParametersComplete");
    if let Some(e) = r.error.take() {
        r.state["errors"].as_array_mut().unwrap().push(json!(e));
        return None;
    }
    let response = r.response.take().unwrap_or(Value::Null);
    let Some(rows) = response["rows"].as_array() else {
        issue(
            r,
            "PROCESS_PARAMETERS_MISSING",
            "The process-parameter batch omitted its rows.",
        );
        return None;
    };
    let filter_sensitive = crate::response_policy::sensitive_filter_enabled(r);
    let id = r.identity.as_ref().unwrap();
    let tasks = &r.state["definition"]["ProcessTask"];
    let mut keys: BTreeSet<String> =
        serde_json::from_value(r.state["automationParamIds"].clone()).unwrap_or_default();
    let task_ids: BTreeSet<_> = tasks
        .as_array()
        .into_iter()
        .flatten()
        .map(|task| task["SysTaskNum"].to_string())
        .collect();
    let selected: Vec<_> = rows
        .iter()
        .filter(|row| {
            row["Company"] != id.keys["Company"]
                || !row["SystemCode"].is_string()
                || !row["ProcessID"].is_string()
                || (row["SystemCode"] == id.keys["SystemCode"]
                    && row["ProcessID"] == id.keys["ProcessID"])
        })
        .collect();
    let valid: Vec<_> = selected
        .iter()
        .copied()
        .filter(|row| {
            let invalid = ["Company", "SystemCode", "ProcessID"]
                .iter()
                .any(|k| row[*k] != id.keys[*k])
                || row["SystemFlag"] != false
                || !task_ids.contains(&row["SysTaskNum"].to_string())
                || !row["SysRowID"].as_str().is_some_and(|s| !s.is_empty())
                || !keys.insert(row["SysRowID"].to_string())
                || filter_sensitive
                    && (!row["ParamToken"].is_null()
                        || (row["ParamName"].as_str().is_some_and(|n| {
                            let n = n.to_lowercase();
                            ["password", "secret", "apikey", "access_token"]
                                .iter()
                                .any(|x| n.contains(x))
                        }) && !row["ParamCharacter"].is_null()));
            !invalid
        })
        .cloned()
        .collect();
    let invalid = valid.len() != selected.len();
    if invalid {
        issue(
            r,
            "PROCESS_PARAMETERS_UNVERIFIED",
            "A process parameter lacks unique identity, exact ownership, a retained task, custom classification, or the selected sensitive-value policy.",
        );
    }
    if !r.state["supplements"]["ProcessTaskParam"].is_array() {
        r.state["supplements"]["ProcessTaskParam"] = json!([]);
    }
    r.state["supplements"]["ProcessTaskParam"]
        .as_array_mut()
        .unwrap()
        .extend(valid);
    r.state["automationParamIds"] = json!(keys);
    r.state["supplements"]["ProcessParameterPolicy"] = json!({"credentialValuesOmitted":filter_sensitive,"filterSensitiveFields":filter_sensitive,"scope":"exact custom parent and custom tasks","representation":"native query projection"});
    if r.page_size.is_some_and(|size| rows.len() >= size as usize) {
        if let Some(after) = rows
            .last()
            .and_then(|row| row["SysRowID"].as_str())
            .filter(|after| Some(*after) != r.state["automationAfter"].as_str())
        {
            r.state["automationAfter"] = json!(after);
            r.state["phase"] = json!("automationParameters");
            return Some(query(r));
        }
        issue(
            r,
            "PROCESS_PARAMETERS_INCOMPLETE",
            "The process-parameter batch did not provide an advancing row ID.",
        );
    } else if !response["nextCursor"].is_null() {
        issue(
            r,
            "PROCESS_PARAMETERS_INCOMPLETE",
            "The process-parameter response has an unsupported resume point.",
        );
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parameter_batches_are_shared_and_partitioned_locally() {
        let mut r = request();
        let mut other = request();
        other
            .identity
            .as_mut()
            .unwrap()
            .keys
            .insert("ProcessID".into(), "Other".into());
        assert_eq!(
            serde_json::to_value(start(&mut r).unwrap()).unwrap(),
            serde_json::to_value(start(&mut other).unwrap()).unwrap()
        );
        let row = |process: &str, id: &str, value: &str| json!({"Company":"C","SystemCode":"ERP","ProcessID":process,"SysTaskNum":7,"SystemFlag":false,"SysRowID":id,"ParamName":"ClientSecret","ParamCharacter":value,"ParamToken":"keep token"});
        r.response =
            Some(json!({"rows":[row("Owned","one","keep Ω"),row("Other","two","other process")]}));
        let Some(HostAction::SharedQuery { query }) = complete(&mut r) else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "two");
        assert_eq!(
            r.state["supplements"]["ProcessTaskParam"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        r.response = Some(json!({"rows":[row("Owned","three","last value")]}));
        assert!(complete(&mut r).is_none());
        assert!(r.state["errors"].as_array().unwrap().is_empty());
        assert_eq!(
            r.state["supplements"]["ProcessTaskParam"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            r.state["supplements"]["ProcessTaskParam"][0]["ParamCharacter"],
            "keep Ω"
        );
        assert_eq!(
            r.state["supplements"]["ProcessTaskParam"][1]["ParamCharacter"],
            "last value"
        );
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "automation-and-integrations".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "automation-and-integrations".into(),
                kind: "ProcessSet".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([
                    ("Company".into(), "C".into()),
                    ("SystemCode".into(), "ERP".into()),
                    ("ProcessID".into(), "Owned".into()),
                ]),
            }),
            cursor: None,
            page_size: Some(2),
            state: json!({"definition":{"ProcessTask":[{"SysTaskNum":7}]},"supplements":{},"errors":[]}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn shared_custom_parameter_pages_preserve_opt_in_redaction() {
        let mut r = request();
        r.state["_pluginState"] = json!({"config":{"filterSensitiveFields":true}});
        let HostAction::SharedQuery { query: q } = query(&r) else {
            panic!()
        };
        assert_eq!(q.joins.len(), 7);
        assert_eq!(q.criteria.len(), 4);
        assert!(!q.parameters.contains_key("ProcessID"));
        assert!(!q.fields.iter().any(|f| f.field == "*"));
        assert!(
            q.fields
                .iter()
                .find(|f| f.alias == "ParamCharacter")
                .unwrap()
                .expression
                .as_ref()
                .unwrap()
                .contains("THEN CAST(NULL")
        );
        assert_eq!(
            q.fields
                .iter()
                .find(|f| f.alias == "ParamToken")
                .unwrap()
                .expression
                .as_deref(),
            Some("CAST(NULL AS nvarchar(max))")
        );
    }
    #[test]
    fn ordinary_values_preserved_and_bad_ownership_or_secret_rejected() {
        for bad in [false, true] {
            let mut r = request();
            r.state["_pluginState"] = json!({"config":{"filterSensitiveFields":true}});
            r.response = Some(
                json!({"rows":[{"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SysTaskNum":7,"SystemFlag":false,"SysRowID":"row1","ParamName":if bad{"ClientSecret"}else{"Caption"},"ParamCharacter":"Ω exact","ParamToken":null}]}),
            );
            complete(&mut r);
            assert_eq!(r.state["errors"].as_array().unwrap().is_empty(), !bad);
            if bad {
                assert!(
                    r.state["supplements"]["ProcessTaskParam"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                assert!(!r.state["supplements"].to_string().contains("Ω exact"));
            } else {
                assert_eq!(
                    r.state["supplements"]["ProcessTaskParam"][0]["ParamCharacter"],
                    "Ω exact"
                );
            }
        }
    }
    #[test]
    fn missing_rows_or_partial_read_never_claims_complete() {
        let mut r = request();
        r.response = Some(json!({}));
        complete(&mut r);
        assert_eq!(r.state["errors"][0]["code"], "PROCESS_PARAMETERS_MISSING");
        let mut r = request();
        r.response = Some(json!({"rows":[],"nextCursor":"more"}));
        complete(&mut r);
        assert_eq!(
            r.state["errors"][0]["code"],
            "PROCESS_PARAMETERS_INCOMPLETE"
        );
    }

    #[test]
    fn default_and_explicit_off_preserve_sensitive_parameter_values() {
        for configured in [None, Some(false)] {
            let mut r = request();
            if let Some(enabled) = configured {
                r.state["_pluginState"] = json!({"config":{"filterSensitiveFields":enabled}});
            }
            let HostAction::SharedQuery { query: q } = query(&r) else {
                panic!()
            };
            for name in ["ParamCharacter", "ParamToken"] {
                let f = q.fields.iter().find(|f| f.alias == name).unwrap();
                assert_eq!(f.field, name);
                assert!(f.expression.is_none());
            }
            r.response = Some(
                json!({"rows":[{"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SysTaskNum":7,"SystemFlag":false,"SysRowID":"row1","ParamName":"ClientSecret","ParamCharacter":"synthetic Ω value","ParamToken":"synthetic-token"}]}),
            );
            complete(&mut r);
            assert!(r.state["errors"].as_array().unwrap().is_empty());
            assert_eq!(
                r.state["supplements"]["ProcessTaskParam"][0]["ParamCharacter"],
                "synthetic Ω value"
            );
            assert_eq!(
                r.state["supplements"]["ProcessTaskParam"][0]["ParamToken"],
                "synthetic-token"
            );
            assert_eq!(
                r.state["supplements"]["ProcessParameterPolicy"]["credentialValuesOmitted"],
                false
            );
        }
    }
    #[test]
    fn ownership_and_system_exclusions_apply_in_both_filter_modes() {
        for enabled in [false, true] {
            for field in ["Company", "SystemFlag", "SysTaskNum"] {
                let mut r = request();
                r.state["_pluginState"] = json!({"config":{"filterSensitiveFields":enabled}});
                let mut row = json!({"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SysTaskNum":7,"SystemFlag":false,"SysRowID":"row1","ParamName":"Caption","ParamCharacter":"ordinary","ParamToken":null});
                row[field] = match field {
                    "Company" => json!("OTHER"),
                    "SystemFlag" => json!(true),
                    _ => json!(8),
                };
                r.response = Some(json!({"rows":[row]}));
                complete(&mut r);
                assert!(
                    r.state["supplements"]["ProcessTaskParam"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(
                    r.state["errors"][0]["code"],
                    "PROCESS_PARAMETERS_UNVERIFIED"
                );
            }
        }
    }
    #[test]
    fn opt_in_accepts_omitted_sensitive_values() {
        let mut r = request();
        r.state["_pluginState"] = json!({"config":{"filterSensitiveFields":true}});
        r.response = Some(
            json!({"rows":[{"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SysTaskNum":7,"SystemFlag":false,"SysRowID":"row1","ParamName":"ClientSecret","ParamCharacter":null,"ParamToken":null}]}),
        );
        complete(&mut r);
        assert!(r.state["errors"].as_array().unwrap().is_empty());
        assert_eq!(
            r.state["supplements"]["ProcessTaskParam"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            r.state["supplements"]["ProcessParameterPolicy"]["credentialValuesOmitted"],
            true
        );
    }

    #[test]
    fn resolved_run_setting_overrides_saved_filter_choice() {
        for enabled in [false, true] {
            let mut r = request();
            r.state["_pluginState"] = json!({"config":{"filterSensitiveFields":!enabled}});
            r.state["_pluginSettings"] = json!({"filterSensitiveFields":enabled});
            let HostAction::SharedQuery { query: q } = query(&r) else {
                panic!()
            };
            assert_eq!(
                q.fields
                    .iter()
                    .find(|f| f.alias == "ParamToken")
                    .unwrap()
                    .expression
                    .is_some(),
                enabled
            );
            r.response = Some(
                json!({"rows":[{"Company":"C","SystemCode":"ERP","ProcessID":"Owned","SysTaskNum":7,"SystemFlag":false,"SysRowID":"row1","ParamName":"ClientSecret","ParamCharacter":"synthetic","ParamToken":"synthetic-token"}]}),
            );
            complete(&mut r);
            assert_eq!(
                r.state["supplements"]["ProcessParameterPolicy"]["credentialValuesOmitted"],
                enabled
            );
            assert_eq!(
                r.state["supplements"]["ProcessTaskParam"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                enabled
            );
        }
    }
}
