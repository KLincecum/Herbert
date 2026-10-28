//! Exact native user-defined application registration metadata.
//! UserDefinedApplicationService stores EP/KNTCUDApp with Key1 = ViewId.
//! SysCharacter01 is security and SysCharacter02 is creator metadata here.
//! These rows are not Classic XML or application file bodies.
use serde_json::{Value, json};

pub const KIND: &str = "PersistedKineticApplicationRegistration";

pub fn valid_keys(keys: &std::collections::BTreeMap<String, String>) -> bool {
    keys.get("TypeCode")
        .is_some_and(|code| code.eq_ignore_ascii_case("KNTCUDApp"))
        && keys
            .get("ProductID")
            .is_some_and(|product| product.eq_ignore_ascii_case("EP"))
        && keys.get("Key1").is_some_and(|view| !view.is_empty())
}

pub fn append(definition: &mut Value) {
    let view = definition["XXXDef"][0]["Key1"].clone();
    definition["applicationReference"] = json!({"ViewId":view,"registrationOnly":true});
}
