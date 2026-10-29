use serde::{Deserialize, Serialize};

/// Helper permission, not a requirement to use a helper.
/// Plugins select direct calls unless a helper is needed or improves collection.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum CollectionMethod {
    #[serde(alias = "direct")]
    DirectOnly,
    #[default]
    #[serde(alias = "hybrid", alias = "directWithHelp", alias = "serverSide", alias = "helper")]
    ServerAssisted,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum HelperPurpose {
    /// Older helper declarations provided collection coverage, not optional acceleration.
    #[default]
    RequiredContent,
    /// Optional server work with a demonstrated collection benefit.
    ServerWork,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum HelperCoverage {
    #[default]
    SomeContent,
    AllContent,
}

impl CollectionMethod {
    pub fn allows(self, _purpose: HelperPurpose) -> bool {
        match self {
            Self::DirectOnly => false,
            Self::ServerAssisted => true,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::DirectOnly => "Direct only",
            Self::ServerAssisted => "Server-assisted",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::DirectOnly => {
                "Use existing Epicor services only. No helpers are installed or invoked."
            }
            Self::ServerAssisted => {
                "Use helpers when needed or when they improve collection. Otherwise use direct calls."
            }
        }
    }
    /// Both helper-enabled modes were merged into Server Assisted on 2026-10-21.
    /// Historical helper-enabled selections remain readable under that policy.
    pub fn legacy() -> Self {
        Self::ServerAssisted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn method_policy_and_legacy_behavior_are_explicit() {
        assert_eq!(CollectionMethod::default(), CollectionMethod::ServerAssisted);
        assert_eq!(CollectionMethod::legacy(), CollectionMethod::ServerAssisted);
        for purpose in [HelperPurpose::RequiredContent, HelperPurpose::ServerWork] {
            assert!(!CollectionMethod::DirectOnly.allows(purpose));
            assert!(CollectionMethod::ServerAssisted.allows(purpose));
        }
        for legacy in ["hybrid", "directWithHelp", "serverSide", "helper", "serverAssisted"] {
            let restored: CollectionMethod = serde_json::from_value(serde_json::json!(legacy)).unwrap();
            assert_eq!(restored, CollectionMethod::ServerAssisted);
            assert_eq!(serde_json::to_value(restored).unwrap(), "serverAssisted");
        }
        assert!(serde_json::from_str::<CollectionMethod>("\"invalid\"").is_err());
        let schema=serde_json::to_value(schemars::schema_for!(CollectionMethod)).unwrap();
        assert_eq!(schema["enum"],serde_json::json!(["directOnly","serverAssisted"]));
    }
}
