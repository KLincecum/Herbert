//! Independent persisted child rows retain source identity and parent linkage.
use herbert_contract::Diagnostic;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub fn validate(
    table: &str,
    rows: &[Value],
    root: &Value,
    links: &BTreeMap<String, String>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        match row.get("SysRowID").and_then(Value::as_str) {
            Some(key) if !key.is_empty() => {
                if !seen.insert(key.to_ascii_lowercase()) {
                    out.push(Diagnostic::new(
                        "CHILD_IDENTITY_DUPLICATE",
                        &format!("The saved {table} response repeats a SysRowID."),
                    ));
                }
            }
            _ => out.push(Diagnostic::new(
                "CHILD_IDENTITY_UNVERIFIED",
                &format!("A saved {table} row does not identify its Epicor SysRowID."),
            )),
        }
        for (field, parent) in links {
            if row.get(field).is_none_or(Value::is_null)
                || root.get(parent).is_none_or(Value::is_null)
                || row.get(field) != root.get(parent)
            {
                out.push(Diagnostic::new("CHILD_IDENTITY_MISMATCH", &format!("A saved {table}.{field} value does not match its full Epicor parent key.")));
            }
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn incomplete_parent_fields_cannot_compare_as_equal_nulls() {
        let links = BTreeMap::from([("Key1".into(), "Key1".into())]);
        assert!(!validate("XXXChunk", &[json!({"SysRowID":"id"})], &json!({}), &links).is_empty());
        let row = json!({"SysRowID":"id","Key1":"form"});
        assert!(validate("XXXChunk", &[row.clone()], &json!({"Key1":"form"}), &links).is_empty());
        assert!(
            validate(
                "XXXChunk",
                &[row.clone(), row],
                &json!({"Key1":"form"}),
                &links
            )
            .iter()
            .any(|d| d.code == "CHILD_IDENTITY_DUPLICATE")
        );
    }
}
