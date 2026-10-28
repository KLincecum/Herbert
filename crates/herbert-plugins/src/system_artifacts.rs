//! Release policy: explicitly system-marked roots are not user artifacts.
//! SystemCode (ICE/ERP) is a namespace, never an ownership marker.
use herbert_contract::{Diagnostic, GuestReply};
use serde_json::Value;
pub fn marked(row: &Value) -> bool {
    row.as_object().is_some_and(|fields| fields.iter().any(|(key, value)| {
        (key == "SystemFlag" || key.ends_with("_SystemFlag")) && match value {
            Value::Bool(v) => *v,
            Value::Number(v) => v.as_i64().is_some_and(|v| v != 0),
            Value::String(v) => v.eq_ignore_ascii_case("true") || v == "1",
            _ => false,
        }
    }))
}
pub fn refused() -> GuestReply {
    GuestReply::Failure { diagnostic: Diagnostic::new("SYSTEM_ARTIFACT_EXCLUDED", "This artifact is marked as system and is excluded from downloads.") }
}
#[cfg(test)] mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn explicit_markers_only() {
        for row in [json!({"SystemFlag":true}),json!({"SystemFlag":1}),json!({"EfxLibrary_SystemFlag":"true"})] { assert!(marked(&row)); }
        for row in [json!({"SystemFlag":false}),json!({"SystemFlag":0}),json!({"SystemFlag":"false"}),json!({"SystemCode":"ICE"}),json!({"SystemCode":"ERP"}),json!({"QueryID":"zSystemNamedCustom"})] { assert!(!marked(&row)); }
    }
}
