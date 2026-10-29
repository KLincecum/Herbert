//! Filter the saved response copy using rules supplied by its plugin.
use herbert_contract::{Diagnostic, NativeResponsePolicy, NativeRetentionRule, NativeRowPredicate};
use serde_json::{Map, Value};

fn invalid() -> Diagnostic {
    Diagnostic::new("INVALID_RETENTION_RESPONSE", "The Epicor response does not match the plugin's retention rule. The original response was not saved.")
}

fn matches(predicate: &NativeRowPredicate, row: &Map<String, Value>) -> bool {
    match predicate {
        NativeRowPredicate::FlagTrue { field } => row.get(field).is_some_and(|value| {
            value.as_bool() == Some(true)
                || value.as_i64().is_some_and(|number| number != 0)
                || value.as_u64().is_some_and(|number| number != 0)
                || value.as_str().is_some_and(|text| text.eq_ignore_ascii_case("true") || text == "1")
        }),
    }
}

fn child_path(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}

fn collect_omissions(value: &Value, path: &[String], pointer: &str, rule: &NativeRetentionRule, omitted: &mut std::collections::BTreeSet<String>) -> Result<(), Diagnostic> {
    if let Some((segment, rest)) = path.split_first() {
        if segment == "*" {
            for (index, row) in value.as_array().ok_or_else(invalid)?.iter().enumerate() {
                collect_omissions(row, rest, &child_path(pointer, &index.to_string()), rule, omitted)?;
            }
        } else if let Some(child) = value.as_object().ok_or_else(invalid)?.get(segment) {
            collect_omissions(child, rest, &child_path(pointer, segment), rule, omitted)?;
        }
        return Ok(());
    }
    for (index, row) in value.as_array().ok_or_else(invalid)?.iter().enumerate() {
        let object = row.as_object().ok_or_else(invalid)?;
        if !matches(&rule.exclude_when, object) { continue; }
        let row_path = child_path(pointer, &index.to_string());
        let mut children = std::collections::BTreeSet::new();
        for name in &rule.preserve_children {
            if let Some(child) = object.get(name) {
                if !child.as_array().ok_or_else(invalid)?.is_empty() { children.insert(name); }
            }
        }
        if children.is_empty() { omitted.insert(row_path); }
        else {
            for name in object.keys().filter(|name| !children.contains(name)) {
                omitted.insert(child_path(&row_path, name));
            }
        }
    }
    Ok(())
}

pub fn pointer_retained(omitted: &std::collections::BTreeSet<String>, pointer: &str) -> bool {
    !omitted.iter().any(|path| pointer == path || pointer.strip_prefix(path).is_some_and(|suffix| suffix.starts_with('/')))
}

fn prune(value: &Value, pointer: &str, omitted: &std::collections::BTreeSet<String>) -> Option<Value> {
    if !pointer_retained(omitted, pointer) { return None; }
    Some(match value {
        Value::Array(rows) => Value::Array(rows.iter().enumerate().filter_map(|(index, row)| prune(row, &child_path(pointer, &index.to_string()), omitted)).collect()),
        Value::Object(fields) => Value::Object(fields.iter().filter_map(|(name, child)| prune(child, &child_path(pointer, name), omitted).map(|value| (name.clone(), value))).collect()),
        other => other.clone(),
    })
}

pub struct Projection {
    pub value: Value,
    pub omitted: std::collections::BTreeSet<String>,
}

pub fn project_with_omissions(policy: &NativeResponsePolicy, method: &str, value: &Value) -> Result<Option<Projection>, Diagnostic> {
    let mut omitted = std::collections::BTreeSet::new();
    // All decisions use original coordinates. Removing one row must not change a later row's identity.
    for rule in policy.retention_rules.iter().filter(|rule| rule.methods.iter().any(|name| name == method)) {
        collect_omissions(value, &rule.array_path, "", rule, &mut omitted)?;
    }
    if omitted.is_empty() { return Ok(None); }
    Ok(Some(Projection { value: prune(value, "", &omitted).ok_or_else(invalid)?, omitted }))
}

#[cfg(test)]
fn project(policy: &NativeResponsePolicy, method: &str, value: &Value) -> Result<Option<Value>, Diagnostic> {
    Ok(project_with_omissions(policy, method, value)?.map(|projection| projection.value))
}

pub fn valid(policy: &NativeResponsePolicy) -> bool {
    policy.retention_rules.iter().all(|rule| {
        !rule.methods.is_empty() && rule.methods.iter().all(|name| !name.is_empty())
            && !rule.array_path.is_empty() && rule.array_path.iter().all(|part| !part.is_empty())
            && rule.preserve_children.iter().all(|name| !name.is_empty())
            && match &rule.exclude_when { NativeRowPredicate::FlagTrue { field } => !field.is_empty() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> NativeResponsePolicy {
        let rule = |path: Vec<&str>, children: Vec<&str>| NativeRetentionRule {
            methods: vec!["List".into()], array_path: path.into_iter().map(String::from).collect(),
            exclude_when: NativeRowPredicate::FlagTrue { field: "Excluded".into() },
            preserve_children: children.into_iter().map(String::from).collect(),
        };
        NativeResponsePolicy { service: "Example.Service".into(), retention_rules: vec![
            rule(vec!["rows", "*", "children"], vec![]),
            rule(vec!["rows"], vec!["children"]),
        ], ..Default::default() }
    }

    #[test]
    fn storage_copy_excludes_parents_but_preserves_exact_eligible_children() {
        let custom = json!({"Excluded":false,"Name":"custom Ω","Password":"preserve exactly","children":[{"Excluded":false,"Code":"unchanged"}]});
        let child = json!({"Excluded":false,"Name":"custom child","Creator":"exact owner"});
        let original = json!({"rows":[custom.clone(),{"Excluded":true,"Name":"excluded parent","children":[child.clone(),{"Excluded":true,"Name":"excluded child"}]},{"Excluded":true,"Name":"excluded alone"}]});
        let before = original.clone();
        let saved = project(&policy(), "List", &original).unwrap().unwrap();
        assert_eq!(saved, json!({"rows":[custom,{"children":[child]}]}));
        assert_eq!(original, before, "Ownership checks must retain the original response");
        let bytes = serde_json::to_vec(&saved).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("excluded"));
    }

    #[test]
    fn native_flag_encodings_are_consistent_and_false_values_are_unchanged() {
        for flag in [json!(true),json!(1),json!(-1),json!(u64::MAX),json!("true"),json!("TRUE"),json!("1")] {
            assert_eq!(project(&policy(),"List",&json!({"rows":[{"Excluded":flag}]})).unwrap(),Some(json!({"rows":[]})));
        }
        for flag in [json!(false),json!(0),json!("false"),Value::Null] {
            assert!(project(&policy(),"List",&json!({"rows":[{"Excluded":flag}]})).unwrap().is_none());
        }
    }

    #[test]
    fn malformed_declared_rows_fail_without_altering_original() {
        for original in [json!({"rows":{}}),json!({"rows":[1]}),json!({"rows":[{"children":null}]}),json!({"rows":[{"children":{}}]})] {
            let before = original.clone();
            assert_eq!(project(&policy(),"List",&original).unwrap_err().code,"INVALID_RETENTION_RESPONSE");
            assert_eq!(original,before);
        }
    }

    #[test]
    fn unrelated_methods_and_legacy_plugins_keep_original_bytes() {
        let original=json!({"rows":[{"Excluded":true}]});
        assert!(project(&policy(),"Other",&original).unwrap().is_none());
        assert!(project(&NativeResponsePolicy::default(),"List",&original).unwrap().is_none());
        assert!(project(&policy(),"List",&json!({"rows":[{"Name":"optional children absent"}]})).unwrap().is_none());
        assert!(valid(&policy()));
        let mut bad=policy(); bad.retention_rules[0].methods.clear(); assert!(!valid(&bad));
    }
    #[test]
    fn omitted_binary_fields_are_skipped_before_decoding_with_original_indices() {
        let original = json!({"rows":[{"Excluded":true,"Blob":"not base64"},{"Excluded":false,"Blob":"b2s="}]});
        let projected = project_with_omissions(&policy(), "List", &original).unwrap().unwrap();
        let declarations = vec![herbert_contract::BinaryExport { service:"Example.Service".into(), method:"List".into(), pointer:"/rows/*/Blob".into(), extension:"bin".into() }];
        let blobs = crate::native_export::declared_blobs_where(&declarations, "Example.Service", "List", &original, |pointer| pointer_retained(&projected.omitted, pointer)).unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].pointer, "/rows/1/Blob");
        assert_eq!(blobs[0].bytes, b"ok");
        assert_eq!(original.pointer(&blobs[0].pointer), Some(&json!("b2s=")));
        assert_eq!(projected.value, json!({"rows":[{"Excluded":false,"Blob":"b2s="}]}));
    }

    #[test]
    fn omitted_pointer_boundaries_and_escaped_fields_are_exact() {
        let omitted = std::collections::BTreeSet::from(["/rows/1".into(), "/a~1b/~0secret".into()]);
        assert!(!pointer_retained(&omitted, "/rows/1/Blob"));
        assert!(pointer_retained(&omitted, "/rows/10/Blob"));
        assert!(!pointer_retained(&omitted, "/a~1b/~0secret"));
        assert!(pointer_retained(&omitted, "/a~1b/~0secretOther"));
        assert_eq!(child_path("/a~1b", "~secret"), "/a~1b/~0secret");
    }

}
