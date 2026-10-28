//! Plugin-supplied native byte fields. This contract contains no collector roster.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BinaryExport {
    pub service: String,
    pub method: String,
    pub pointer: String,
    pub extension: String,
}
