//! Apply plugin-declared omissions before native responses reach storage.
use herbert_contract::{NativeResponsePolicy, ParameterRedaction};
use serde_json::Value;

pub fn apply(policy: &NativeResponsePolicy, company: &str, settings: &Value, value: &mut Value) -> (bool, bool) {
    fn redact(value: &mut Value, fields: &[String], parameters: &[ParameterRedaction]) -> bool {
        let mut changed = false;
        match value {
            Value::Array(rows) => {
                for row in rows { changed |= redact(row, fields, parameters); }
            }
            Value::Object(row) => {
                for (key, value) in row.iter_mut() {
                    if fields.iter().any(|field| field.eq_ignore_ascii_case(key)) {
                        changed |= !value.is_null();
                        *value = Value::Null;
                    }
                }
                for rule in parameters {
                    let matches = row.get(&rule.name_field).and_then(Value::as_str).is_some_and(|name| {
                        let name = name.to_lowercase();
                        rule.name_contains.iter().any(|part| name.contains(&part.to_lowercase()))
                    });
                    if matches {
                        for field in &rule.value_fields {
                            if let Some(value) = row.get_mut(field) {
                                changed |= !value.is_null();
                                *value = Value::Null;
                            }
                        }
                    }
                }
                for child in row.values_mut() { changed |= redact(child, fields, parameters); }
            }
            _ => {}
        }
        changed
    }
    // Scope rules always apply. Sensitive values change only after explicit plugin opt-in.
    let filter_sensitive = policy.sensitive_filter_setting.as_ref()
        .and_then(|key| settings.get(key)).and_then(Value::as_bool) == Some(true);
    let redacted = filter_sensitive && redact(value, &policy.redact_fields, &policy.redact_parameters);
    let mut scoped = false;
    if let Some(rule) = &policy.tables {
        let pointer = if value.pointer(&rule.pointer).is_none() && rule.fallback_to_root {
            ""
        } else { &rule.pointer };
        if let Some(tables) = value.pointer_mut(pointer).and_then(Value::as_object_mut) {
            for (name, rows) in tables {
                let Some(rows) = rows.as_array_mut() else { continue };
                if rule.omitted_tables.contains(name) {
                    scoped |= !rows.is_empty();
                    rows.clear();
                } else if !rule.unscoped_tables.contains(name) {
                    let before = rows.len();
                    rows.retain(|row| row.get(&rule.company_field).and_then(Value::as_str).is_none_or(|owner| {
                        owner == company || rule.global_values.iter().any(|global| global == owner)
                    }));
                    scoped |= before != rows.len();
                }
            }
        }
    }
    (redacted, scoped)
}

pub fn valid(policy: &NativeResponsePolicy) -> bool {
    let tables_valid = policy.tables.as_ref().is_none_or(|tables| {
        let pointer = &tables.pointer;
        (pointer.is_empty() || pointer.starts_with('/')) && !tables.company_field.is_empty()
    });
    tables_valid && crate::response_retention::valid(policy) && policy.sensitive_filter_setting.as_ref().is_none_or(|key| !key.is_empty())
        && policy.redact_fields.iter().all(|field| !field.is_empty())
        && policy.redact_parameters.iter().all(|rule| {
            !rule.name_field.is_empty() && !rule.name_contains.is_empty()
                && rule.name_contains.iter().all(|part| !part.is_empty())
                && !rule.value_fields.is_empty() && rule.value_fields.iter().all(|field| !field.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::NativeTablePolicy;
    use serde_json::json;

    #[test]
    fn plugin_fields_control_scope_and_redaction_without_host_table_knowledge() {
        let policy = NativeResponsePolicy {
            service: "Example.BO.SourceSvc".into(),
            tables: Some(NativeTablePolicy {
                pointer: "/payload".into(), fallback_to_root: false, company_field: "Owner".into(),
                global_values: vec!["shared".into()], unscoped_tables: vec!["Links".into()], omitted_tables: vec!["History".into()],
            }),
            sensitive_filter_setting: Some("filterPrivateValues".into()),
            redact_fields: vec!["PrivateValue".into()],
            redact_parameters: vec![ParameterRedaction {name_field:"Name".into(),name_contains:vec!["credential".into()],value_fields:vec!["Value".into()]}],
            retention_rules: vec![],
        };
        let mut value = json!({"payload":{"Rows":[{"Owner":"C","PRIVATEVALUE":"secret"},{"Owner":"other"},{"Owner":"shared"}],"Links":[{"Owner":"other"}],"History":[{}]},"parameters":[{"Name":"Credential token","Value":"secret"},{"Name":"Display title","Value":"keep"}]});
        assert!(valid(&policy));
        assert_eq!(apply(&policy,"C",&json!({"filterPrivateValues":true}),&mut value),(true,true));
        assert_eq!(value["payload"]["Rows"],json!([{"Owner":"C","PRIVATEVALUE":null},{"Owner":"shared"}]));
        assert_eq!(value["payload"]["Links"],json!([{"Owner":"other"}]));
        assert_eq!(value["payload"]["History"],json!([]));
        assert_eq!(value["parameters"][0]["Value"],Value::Null);
        assert_eq!(value["parameters"][1]["Value"],"keep");
        assert_eq!(apply(&policy,"C",&json!({"filterPrivateValues":true}),&mut value),(false,false));
    }

    #[test]
    fn absent_policy_preserves_arbitrary_native_fields() {
        let mut value = json!({"returnObj":{"History":[{"Company":"other","Password":"native"}]}});
        let before = value.clone();
        assert_eq!(apply(&NativeResponsePolicy::default(),"C",&json!({}),&mut value),(false,false));
        assert_eq!(value,before);
    }
    #[test]
    fn sensitive_values_require_explicit_plugin_opt_in_and_scope_still_applies() {
        let policy = NativeResponsePolicy {
            service: "Example.BO.SourceSvc".into(),
            sensitive_filter_setting: Some("filterPrivateValues".into()),
            tables: Some(NativeTablePolicy {
                pointer: "/payload".into(), fallback_to_root: false, company_field: "Owner".into(),
                global_values: vec![], unscoped_tables: vec![], omitted_tables: vec![],
            }),
            redact_fields: vec!["Password".into()],
            redact_parameters: vec![ParameterRedaction {
                name_field: "Name".into(), name_contains: vec!["secret".into()], value_fields: vec!["Value".into()],
            }],
            retention_rules: vec![],
        };
        let original = json!({"payload":{"Rows":[{"Owner":"C","Password":"exact Ω value","Parameters":[{"Name":"ClientSecret","Value":"exact parameter"}]},{"Owner":"foreign","Password":"excluded"}]}});
        for settings in [Value::Null, json!({}), json!({"filterPrivateValues":false}), json!({"otherPluginOption":true})] {
            let mut value = original.clone();
            assert_eq!(apply(&policy,"C",&settings,&mut value),(false,true));
            assert_eq!(value["payload"]["Rows"],json!([original["payload"]["Rows"][0].clone()]));
        }
        let mut value = original.clone();
        assert_eq!(apply(&policy,"C",&json!({"filterPrivateValues":true}),&mut value),(true,true));
        assert!(value["payload"]["Rows"][0]["Password"].is_null());
        assert!(value["payload"]["Rows"][0]["Parameters"][0]["Value"].is_null());
        assert_eq!(value["payload"]["Rows"].as_array().unwrap().len(),1);
        // Old descriptors cannot silently opt in just because they list fields.
        let mut legacy = policy.clone();
        legacy.sensitive_filter_setting = None;
        let mut value = original.clone();
        assert_eq!(apply(&legacy,"C",&json!({"filterPrivateValues":true}),&mut value),(false,true));
        assert_eq!(value["payload"]["Rows"][0],original["payload"]["Rows"][0]);
    }

}
