//! Plugins declare which parts of a native response belong in their download.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeResponsePolicy {
    pub service: String,
    #[serde(default)]
    pub tables: Option<NativeTablePolicy>,
    #[serde(default)]
    pub redact_fields: Vec<String>,
    /// Plugin Boolean setting that enables sensitive-value filtering. Missing means off.
    #[serde(default)]
    pub sensitive_filter_setting: Option<String>,
    #[serde(default)]
    pub redact_parameters: Vec<ParameterRedaction>,
    /// Rules for the saved copy only. The plugin still receives the original rows.
    #[serde(default)]
    pub retention_rules: Vec<NativeRetentionRule>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRetentionRule {
    pub methods: Vec<String>,
    /// Object keys, with "*" to visit each array element.
    pub array_path: Vec<String>,
    pub exclude_when: NativeRowPredicate,
    #[serde(default)]
    pub preserve_children: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum NativeRowPredicate {
    FlagTrue { field: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeTablePolicy {
    pub pointer: String,
    pub fallback_to_root: bool,
    pub company_field: String,
    pub global_values: Vec<String>,
    pub unscoped_tables: Vec<String>,
    pub omitted_tables: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParameterRedaction {
    pub name_field: String,
    pub name_contains: Vec<String>,
    pub value_fields: Vec<String>,
}
