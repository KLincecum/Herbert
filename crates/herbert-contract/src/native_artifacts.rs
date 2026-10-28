//! Native artifact declarations loaded from a plugin's ordinary sibling manifest.
//! Export files need no author-maintained hashes or signatures.
use crate::Diagnostic;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginManifest {
    #[serde(default)]
    pub artifacts: Vec<NativeArtifactDeclaration>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeArtifactDeclaration {
    #[serde(default)]
    pub purpose: crate::HelperPurpose,
    #[serde(default)]
    pub coverage: crate::HelperCoverage,
    pub alias: String,
    #[serde(rename = "type")]
    pub artifact_type: NativeArtifactType,
    pub file: String,
    pub lifetime: NativeArtifactLifetime,
    #[serde(default)]
    pub target: Option<String>,
    pub policy: NativeArtifactPolicy,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Optional exact native-version overrides. The common file is the fallback.
    #[serde(default)]
    pub version_files: Vec<NativeArtifactVersionFile>,
    /// These declared functions must exist with their exact parameter signatures.
    /// Additional functions in the library are allowed.
    #[serde(default)]
    pub required_functions: Vec<NativeArtifactFunction>,
    #[serde(default)]
    pub required_query_parameters: Vec<NativeArtifactQueryParameter>,
    pub effects: NativeArtifactEffects,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NativeArtifactType {
    FunctionLibrary,
    Baq,
    Ubaq,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NativeArtifactLifetime {
    Temporary,
    Persistent,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NativeArtifactPolicy {
    CreateInstance,
    UpdateOwned,
    ReplaceNamed,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeArtifactVersionFile {
    pub epicor_version: String,
    pub file: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeArtifactFunction {
    pub function: String,
    pub parameters: Vec<NativeArtifactFunctionParameter>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeArtifactFunctionParameter {
    pub name: String,
    pub data_type: String,
    pub direction: NativeArtifactParameterDirection,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NativeArtifactParameterDirection {
    Input,
    Output,
    InputOutput,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeArtifactQueryParameter {
    pub name: String,
    pub data_type: String,
    pub required: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeArtifactEffects {
    pub description: String,
    #[serde(default)]
    pub submits_updates: bool,
    #[serde(default)]
    pub invokes_custom_actions: bool,
}
fn invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("PLUGIN_ARTIFACT_MANIFEST_INVALID", message)
}
fn nonempty(value: &str) -> bool {
    !value.trim().is_empty()
}
impl PluginManifest {
    /// Validate declared semantics and return dependencies before their consumers.
    /// No arbitrary package count, dependency depth or file size limit is applied.
    pub fn ordered_artifacts(&self) -> Result<Vec<NativeArtifactDeclaration>, Diagnostic> {
        let mut aliases = BTreeMap::new();
        for (index, artifact) in self.artifacts.iter().enumerate() {
            if !nonempty(&artifact.alias)
                || aliases.insert(artifact.alias.as_str(), index).is_some()
            {
                return Err(invalid(
                    "Artifact aliases must be nonempty and unique within the plugin.",
                ));
            }
            if !nonempty(&artifact.file) || !nonempty(&artifact.effects.description) {
                return Err(invalid(format!(
                    "Artifact {} requires a file and a readable description of its effects.",
                    artifact.alias
                )));
            }
            if artifact
                .target
                .as_ref()
                .is_some_and(|target| !nonempty(target))
                || (artifact.policy != NativeArtifactPolicy::CreateInstance
                    && artifact.target.is_none())
            {
                return Err(invalid(format!(
                    "Artifact {} requires an exact target for updateOwned or replaceNamed.",
                    artifact.alias
                )));
            }
            let mut versions = BTreeSet::new();
            for file in &artifact.version_files {
                if !nonempty(&file.epicor_version)
                    || !nonempty(&file.file)
                    || !versions.insert(&file.epicor_version)
                {
                    return Err(invalid(format!(
                        "Artifact {} has an empty or duplicate version-file declaration.",
                        artifact.alias
                    )));
                }
            }
            if artifact.artifact_type != NativeArtifactType::FunctionLibrary
                && !artifact.required_functions.is_empty()
                || artifact.artifact_type == NativeArtifactType::FunctionLibrary
                    && !artifact.required_query_parameters.is_empty()
            {
                return Err(invalid(format!(
                    "Artifact {} declares requirements for a different native artifact type.",
                    artifact.alias
                )));
            }
            let mut functions = BTreeSet::new();
            for function in &artifact.required_functions {
                if !nonempty(&function.function) || !functions.insert(&function.function) {
                    return Err(invalid(format!(
                        "Artifact {} has an empty or duplicate required function.",
                        artifact.alias
                    )));
                }
                let mut parameters = BTreeSet::new();
                for parameter in &function.parameters {
                    if !nonempty(&parameter.name)
                        || !nonempty(&parameter.data_type)
                        || match parameter.direction {
                            NativeArtifactParameterDirection::Input => {
                                !parameters.insert((&parameter.name, false))
                            }
                            NativeArtifactParameterDirection::Output => {
                                !parameters.insert((&parameter.name, true))
                            }
                            NativeArtifactParameterDirection::InputOutput => {
                                !parameters.insert((&parameter.name, false))
                                    || !parameters.insert((&parameter.name, true))
                            }
                        }
                    {
                        return Err(invalid(format!(
                            "Artifact {} has an invalid required function signature.",
                            artifact.alias
                        )));
                    }
                }
            }
            let mut parameters = BTreeSet::new();
            for parameter in &artifact.required_query_parameters {
                if !nonempty(&parameter.name)
                    || !nonempty(&parameter.data_type)
                    || !parameters.insert(&parameter.name)
                {
                    return Err(invalid(format!(
                        "Artifact {} has an invalid required query parameter.",
                        artifact.alias
                    )));
                }
            }
        }
        let mut remaining = Vec::new();
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); self.artifacts.len()];
        for (index, artifact) in self.artifacts.iter().enumerate() {
            let mut unique = BTreeSet::new();
            for dependency in &artifact.dependencies {
                let Some(&before) = aliases.get(dependency.as_str()) else {
                    return Err(invalid(format!(
                        "Artifact {} depends on undeclared alias {}.",
                        artifact.alias, dependency
                    )));
                };
                if !unique.insert(before) {
                    return Err(invalid(format!(
                        "Artifact {} repeats dependency {}.",
                        artifact.alias, dependency
                    )));
                }
                dependents[before].push(index);
            }
            remaining.push(unique.len());
        }
        let mut ready: BTreeSet<usize> = remaining
            .iter()
            .enumerate()
            .filter_map(|(index, count)| (*count == 0).then_some(index))
            .collect();
        let mut ordered = Vec::new();
        while let Some(index) = ready.pop_first() {
            ordered.push(self.artifacts[index].clone());
            for &dependent in &dependents[index] {
                remaining[dependent] -= 1;
                if remaining[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
        }
        if ordered.len() != self.artifacts.len() {
            let aliases = self
                .artifacts
                .iter()
                .enumerate()
                .filter(|(i, _)| remaining[*i] != 0)
                .map(|(_, a)| a.alias.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(invalid(format!(
                "Artifact dependency cycle prevents installation: {aliases}."
            )));
        }
        Ok(ordered)
    }
}
impl NativeArtifactDeclaration {
    /// Select only an exact declared override, otherwise the ordinary common file.
    pub fn file_for_version(&self, version: &str) -> &str {
        self.version_files
            .iter()
            .find(|file| file.epicor_version == version)
            .map(|file| file.file.as_str())
            .unwrap_or(&self.file)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn artifact(alias: &str, dependencies: &[&str]) -> serde_json::Value {
        json!({"alias":alias,"type":"baq","file":"artifacts/query.baq","lifetime":"temporary","policy":"createInstance","dependencies":dependencies,"effects":{"description":"Installs a temporary query, reads records, then removes the query."}})
    }
    #[test]
    fn ordinary_manifest_orders_dependencies_without_hash_or_signing_fields() {
        let manifest: PluginManifest = serde_json::from_value(
            json!({"artifacts":[artifact("reader", &["base"]),artifact("base", &[])]}),
        )
        .unwrap();
        let ordered = manifest.ordered_artifacts().unwrap();
        assert_eq!(
            ordered.iter().map(|a| a.alias.as_str()).collect::<Vec<_>>(),
            vec!["base", "reader"]
        );
        assert_eq!(
            ordered[0].file_for_version("unlisted native version"),
            "artifacts/query.baq"
        );
        let mut persistent = manifest;
        persistent.artifacts[0].lifetime = NativeArtifactLifetime::Persistent;
        assert!(persistent.ordered_artifacts().is_ok());
    }
    #[test]
    fn exact_targets_effects_signatures_and_dependency_graph_are_validated() {
        for change in [
            json!({"policy":"replaceNamed"}),
            json!({"effects":{"description":" "}}),
            json!({"dependencies":["missing"]}),
            json!({"dependencies":["a"]}),
            json!({"requiredFunctions":[{"function":"F","parameters":[]}]}),
        ] {
            let mut row = artifact("a", &[]);
            row.as_object_mut()
                .unwrap()
                .extend(change.as_object().unwrap().clone());
            let manifest: PluginManifest =
                serde_json::from_value(json!({"artifacts":[row]})).unwrap();
            assert!(manifest.ordered_artifacts().is_err());
        }
        let mut row = artifact("a", &[]);
        row["policy"] = json!("replaceNamed");
        row["target"] = json!("ExactNativeQuery");
        let manifest: PluginManifest = serde_json::from_value(json!({"artifacts":[row]})).unwrap();
        assert!(manifest.ordered_artifacts().is_ok());
    }
    #[test]
    fn same_name_input_and_output_are_valid_but_duplicate_direction_is_not() {
        let mut row = artifact("a", &[]);
        row["type"] = json!("functionLibrary");
        row["requiredFunctions"] = json!([{"function":"Echo","parameters":[{"name":"value","dataType":"System.String","direction":"input"},{"name":"value","dataType":"System.String","direction":"output"}]}]);
        let mut manifest: PluginManifest =
            serde_json::from_value(json!({"artifacts":[row]})).unwrap();
        assert!(manifest.ordered_artifacts().is_ok());
        manifest.artifacts[0].required_functions[0].parameters[1].direction =
            NativeArtifactParameterDirection::Input;
        assert!(manifest.ordered_artifacts().is_err());
    }
    #[test]
    fn artifact_operations_have_distinct_wire_contracts() {
        let actions = [
            json!({"type":"artifactInstall", "alias":"a"}),
            json!({"type":"artifactFunctionCall", "alias":"a", "function":"F", "arguments":{"value":1}}),
            json!({"type":"artifactQueryExecute", "alias":"a", "parameters":{"p":1}}),
            json!({"type":"artifactQueryUpdate", "alias":"a", "dataset":{"Results":[{"RowMod":"U"}]}}),
            json!({"type":"artifactQueryAction", "alias":"a", "action":"DoWork", "dataset":{"Results":[]}, "parameters":{}}),
        ];
        for value in actions {
            let action: crate::HostAction = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(action).unwrap(), value);
        }
        assert!(
            serde_json::from_value::<crate::HostAction>(
                json!({"type":"artifactInstall","alias":"a","dataset":{}})
            )
            .is_err()
        );
    }
}
