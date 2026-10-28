//! Versioned, credential-free application and guest contracts.
use serde::{Deserialize, Serialize};
use serde_json::Value;
mod response_policy;
pub use response_policy::*;
mod binary_exports;
pub use binary_exports::*;
mod native_artifacts;
pub use native_artifacts::*;
mod collection_method;
pub use collection_method::*;
mod managed_decompilation;
pub use managed_decompilation::*;
mod host_utility;
pub use host_utility::*;
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 1;
pub const PLUGIN_ABI: u32 = 1;

/// Local ABI and saved definitions need no arbitrary nesting cutoff. The
/// decoder still requires one complete document and reports syntax errors.
/// Actual runtime memory/stack availability remains a resource boundary.
pub fn decode_local_json<T:serde::de::DeserializeOwned>(bytes:&[u8])->Result<T,serde_json::Error>{
    let mut decoder=serde_json::Deserializer::from_slice(bytes);decoder.disable_recursion_limit();
    let value=T::deserialize(&mut decoder)?;decoder.end()?;Ok(value)
}

#[cfg(test)]mod local_json_depth_tests{
    #[test]fn deep_local_definition_is_not_limited_by_the_json_default(){
        let mut value=serde_json::json!({"DBSchemaName":"Erp","DBTableName":"Part"});
        for _ in 0..180{value=serde_json::json!({"child":value});}
        let bytes=serde_json::to_vec(&value).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err());
        assert_eq!(super::decode_local_json::<serde_json::Value>(&bytes).unwrap(),value);
        assert!(super::decode_local_json::<serde_json::Value>(b"{} {}").is_err());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub category: String,
    pub kind: String,
    pub scope: Scope,
    pub keys: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Scope {
    Global,
    Company { company: String },
    /// A definition owned elsewhere but shared with the selected company.
    /// Keep its physical Company key; do not relabel the stored owner.
    SharedCompany {
        company: String,
        #[serde(rename = "ownerCompany")]
        owner_company: String,
    },
    Personal { company: String, owner: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Completeness {
    Collected,
    Partial,
    Inaccessible,
    Unsupported,
    DiscoveryFailed,
    VerifiedAbsent,
    VerifiedNotApplicable,
    Excluded,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub description: String,
    pub plugin_version: Option<String>,
    pub available: bool,
    /// Plugin-owned inclusion preference, returned from its portable state.
    #[serde(default = "selected_by_default")]
    pub selected: bool,
    pub qualification: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub helpers: Vec<HelperContract>,
    /// Supplied by the sibling plugin.json manifest. No native files are embedded.
    #[serde(default)]
    pub artifacts: Vec<NativeArtifactDeclaration>,
    #[serde(default)]
    pub settings: Vec<PluginSetting>,
    /// Help travels with the module. The host does not maintain a plugin roster.
    #[serde(default)]
    pub help: Vec<HelpSection>,
}
fn selected_by_default() -> bool {
    true
}

/// Plain text for a help page. Paragraphs remain readable in every frontend.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpSection {
    pub title: String,
    pub paragraphs: Vec<String>,
}

/// Presentation and value shape only. A plugin owns keys, defaults and semantics.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PluginSetting {
    Integer {
        key: String,
        label: String,
        description: String,
        default: u32,
        minimum: u32,
        maximum: u32,
        #[serde(default)]
        demo_only: bool,
    },
    Checkbox {
        key: String,
        label: String,
        description: String,
        default: bool,
        #[serde(default)]
        demo_only: bool,
    },
    Choice {
        key: String,
        label: String,
        description: String,
        default: Value,
        options: Vec<PluginSettingOption>,
        #[serde(default)]
        demo_only: bool,
    },
    Filter {
        key: String,
        label: String,
        description: String,
        default: String,
        #[serde(default)]
        demo_only: bool,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginSettingOption {
    pub label: String,
    pub value: Value,
}
impl PluginSetting {
    /// Bounds must follow the plugin's documented native contract.
    pub fn integer(key: &str, label: &str, default: u32, minimum: u32, maximum: u32) -> Self {
        Self::Integer {key:key.into(),label:label.into(),description:String::new(),default,minimum,maximum,demo_only:false}
    }
    /// Declare a choice without exposing wire-format construction to a plugin author.
    pub fn choice(
        key: &str,
        label: &str,
        default: impl Into<Value>,
        options: impl IntoIterator<Item = PluginSettingOption>,
    ) -> Self {
        Self::Choice {
            key: key.into(),
            label: label.into(),
            description: String::new(),
            default: default.into(),
            options: options.into_iter().collect(),
            demo_only: false,
        }
    }

    pub fn checkbox(key: &str, label: &str, default: bool) -> Self {
        Self::Checkbox {
            key: key.into(),
            label: label.into(),
            description: String::new(),
            default,
            demo_only: false,
        }
    }

    pub fn filter(key: &str, label: &str, default: &str) -> Self {
        Self::Filter {
            key: key.into(),
            label: label.into(),
            description: String::new(),
            default: default.into(),
            demo_only: false,
        }
    }

    pub fn description(mut self, text: &str) -> Self {
        match &mut self {
            Self::Integer { description, .. }
            | Self::Checkbox { description, .. }
            | Self::Choice { description, .. }
            | Self::Filter { description, .. } => *description = text.into(),
        }
        self
    }

    /// Demo controls store their values but intentionally do not change behavior.
    pub fn demo(mut self) -> Self {
        match &mut self {
            Self::Integer { demo_only, .. }
            | Self::Checkbox { demo_only, .. }
            | Self::Choice { demo_only, .. }
            | Self::Filter { demo_only, .. } => *demo_only = true,
        }
        self
    }
    pub fn demo_only(&self) -> bool {
        match self {
            Self::Integer { demo_only, .. }
            | Self::Checkbox { demo_only, .. }
            | Self::Choice { demo_only, .. }
            | Self::Filter { demo_only, .. } => *demo_only,
        }
    }
    pub fn key(&self) -> &str {
        match self {
            Self::Integer { key, .. } | Self::Checkbox { key, .. } | Self::Choice { key, .. } | Self::Filter { key, .. } => key,
        }
    }
    pub fn default_value(&self) -> Value {
        match self {
            Self::Integer { default, .. } => Value::from(*default),
            Self::Checkbox { default, .. } => Value::Bool(*default),
            Self::Choice { default, .. } => default.clone(),
            Self::Filter { default, .. } => Value::String(default.clone()),
        }
    }
    pub fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Integer { minimum, maximum, .. } => value.as_u64().is_some_and(|n| n >= *minimum as u64 && n <= *maximum as u64),
            Self::Checkbox { .. } => value.is_boolean(),
            Self::Filter { .. } => value.is_string(),
            Self::Choice { options, .. } => options.iter().any(|option| &option.value == value),
        }
    }
}

pub fn option(label: &str, value: impl Into<Value>) -> PluginSettingOption {
    PluginSettingOption {
        label: label.into(),
        value: value.into(),
    }
}
pub fn resolve_plugin_settings(
    declarations: &[PluginSetting],
    overrides: &Value,
) -> Result<Value, Diagnostic> {
    let mut result = serde_json::Map::new();
    let supplied = if overrides.is_null() {
        None
    } else {
        Some(overrides.as_object().ok_or_else(|| {
            Diagnostic::new(
                "INVALID_PLUGIN_SETTINGS",
                "Plugin setting values must be an object.",
            )
        })?)
    };
    if supplied.is_some_and(|values| {
        values
            .keys()
            .any(|key| !declarations.iter().any(|setting| setting.key() == key))
    }) {
        return Err(Diagnostic::new(
            "INVALID_PLUGIN_SETTINGS",
            "A setting is not declared by the available plugin.",
        ));
    }
    for setting in declarations {
        if setting.key().is_empty() || result.contains_key(setting.key()) {
            return Err(Diagnostic::new(
                "PLUGIN_DESCRIPTOR_INVALID",
                "Plugin setting keys must be nonempty and unique.",
            ));
        }
        let value = supplied
            .and_then(|values| values.get(setting.key()))
            .cloned()
            .unwrap_or_else(|| setting.default_value());
        if !setting.accepts(&setting.default_value()) || !setting.accepts(&value) {
            return Err(Diagnostic::new(
                "INVALID_PLUGIN_SETTINGS",
                "A setting value does not match its plugin declaration.",
            ));
        }
        result.insert(setting.key().into(), value);
    }
    Ok(Value::Object(result))
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperContract {
    #[serde(default)]
    pub purpose: HelperPurpose,
    #[serde(default)]
    pub coverage: HelperCoverage,
    pub library_id: String,
    pub global_id: String,
    pub company_argument: String,
    pub functions: Vec<HelperFunction>,
    /// Optional reviewed native packages supplied by the module itself.
    #[serde(default)]
    pub installation: Option<HelperInstallation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperInstallation {
    /// The source must work when the host gives each run its own library ID.
    pub supports_instances: bool,
    pub packages: Vec<HelperPackage>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperPackage {
    pub epicor_version: String,
    pub package: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperFunction {
    pub function_id: String,
    /// Legacy journal compatibility only. Never required or used to admit code.
    #[serde(default, skip_serializing)]
    pub body_sha256: String,
    pub binary_outputs: Vec<String>,
    #[serde(default)]
    pub effect: HelperEffect,
    #[serde(default)]
    pub resource: Option<HelperResourceContract>,
}
#[derive(
    Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum HelperEffect {
    #[default]
    Read,
    OwnedTemporaryState,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperResourceContract {
    pub operation_argument: String,
    pub operation_id_argument: String,
    pub begin_operation: String,
    pub status_operation: String,
    pub close_operation: String,
    /// Optional final removal of closure records, after local close evidence is durable.
    #[serde(default)]
    pub acknowledge_operation: Option<String>,
    pub closed_pointer: String,
    pub admission_rejected_pointer: String,
    pub binding_arguments: Vec<String>,
    #[serde(default)]
    pub observations: Vec<HelperArgumentObservation>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelperArgumentObservation {
    pub argument: String,
    pub pointer: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Diagnostic {
    /// Structured local debugging facts. Support exports apply their own allowlist.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, Value>,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    pub retryable: bool,
    /// Native cause for an in-process ownership adapter. Never send server text
    /// to a frontend, a guest, ordinary logs or the durable journal.
    #[serde(skip)]
    #[schemars(skip)]
    pub native_details: Option<Value>,
}

impl Diagnostic {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            context: BTreeMap::new(),
            code: code.into(),
            message: message.into(),
            identity: None,
            retryable: false,
            native_details: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Artifact {
    pub identity: Identity,
    pub classification: String,
    pub acquisition: Completeness,
    pub integrity: String,
    pub processing: String,
    pub files: Vec<FileRef>,
    pub dependencies: Vec<Identity>,
    pub diagnostics: Vec<Diagnostic>,
    pub source_consistency: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatingSettings {
    /// Simultaneous plugins. Keep the wire name for existing saved settings.
    /// Each plugin issues one request at a time; local processing can overlap.
    pub concurrency: u8,
    /// Parallel work inside one server collection. Kevin selected 4 on 2026-10-20.
    #[serde(default = "default_server_concurrency")]
    pub server_concurrency: u32,
    /// Shared local decompilation budget; default four is measured, not a cap.
    #[serde(default = "default_local_decompiler_workers")]
    pub local_decompiler_workers: u32,
    pub connection_timeout_ms: Option<u64>,
    pub service_timeout_ms: Option<u64>,
    pub export_timeout_ms: Option<u64>,
    pub inactivity_timeout_ms: Option<u64>,
    pub minimum_request_interval_ms: Option<u64>,
    pub retry_attempts: u32,
    pub retry_backoff_ms: Option<u64>,
    pub page_size: Option<u32>,
}

pub fn default_server_concurrency() -> u32 { 4 }
pub fn default_local_decompiler_workers() -> u32 { 4 }

#[cfg(test)]
mod server_concurrency_tests {
    use super::*;
    #[test]
    fn older_settings_default_server_work_to_four_and_keep_request_choice() {
        let mut value = serde_json::to_value(OperatingSettings::default()).unwrap();
        value["concurrency"] = serde_json::json!(8);
        value.as_object_mut().unwrap().remove("serverConcurrency");
        let settings: OperatingSettings = serde_json::from_value(value).unwrap();
        assert_eq!(settings.concurrency, 8);
        assert_eq!(settings.server_concurrency, 4);
    }
    #[test]
    fn server_parallelism_is_independent_and_has_only_the_native_integer_boundary() {
        for count in [1, 4, 9, 25, i32::MAX as u32] {
            let settings = OperatingSettings { concurrency: 2, server_concurrency: count, ..Default::default() };
            settings.validate().unwrap();
            let restored: OperatingSettings = serde_json::from_value(serde_json::to_value(&settings).unwrap()).unwrap();
            assert_eq!(restored, settings);
        }
        for count in [0, i32::MAX as u32 + 1] {
            assert!(OperatingSettings { server_concurrency: count, ..Default::default() }.validate().is_err());
        }
    }
}

impl Default for OperatingSettings {
    fn default() -> Self {
        // Concurrency uses the selected default. Other defaults need evidence.
        // Plugin-specific choices belong to descriptors, not this host contract.
        Self {
            // Kevin selected four plugins and the 2/4/6/8 choices on 2026-10-21.
            // This is a user policy, not a server capacity limit.
            concurrency: 4,
            server_concurrency: default_server_concurrency(),
            local_decompiler_workers: default_local_decompiler_workers(),
            connection_timeout_ms: None,
            service_timeout_ms: None,
            export_timeout_ms: None,
            inactivity_timeout_ms: None,
            minimum_request_interval_ms: None,
            retry_attempts: 0,
            retry_backoff_ms: None,
            // Lab dictionary tests returned the same items at 100, 200, 500 and
            // 1000 rows. At 500, requests fell from 26 to 11 versus 200, with
            // responses below 191 KB. This is an
            // editable default, not a limit on the total number of results.
            page_size: Some(500),
        }
    }
}

impl OperatingSettings {
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.local_decompiler_workers == 0 {
            return Err(Diagnostic::new("INVALID_DECOMPILER_WORKERS", "Local decompiler workers must be a positive whole number."));
        }
        // .NET parallel execution accepts a positive Int32 degree of parallelism.
        if self.server_concurrency == 0 || self.server_concurrency > i32::MAX as u32 {
            return Err(Diagnostic::new("INVALID_SERVER_CONCURRENCY",
                "Server concurrency must fit a positive Int32 value."));
        }
        if ![2, 4, 6, 8].contains(&self.concurrency) {
            return Err(Diagnostic::new(
                "INVALID_CONCURRENCY",
                "Select 2, 4, 6, or 8 simultaneous plugins.",
            ));
        }
        for (name, value) in [
            ("connectionTimeoutMs", self.connection_timeout_ms),
            ("serviceTimeoutMs", self.service_timeout_ms),
            ("exportTimeoutMs", self.export_timeout_ms),
            ("inactivityTimeoutMs", self.inactivity_timeout_ms),
        ] {
            if value == Some(0) {
                return Err(Diagnostic::new(
                    "INVALID_SETTING",
                    format!("{name} must be positive or null."),
                ));
            }
        }
        if self.page_size == Some(0) {
            return Err(Diagnostic::new(
                "INVALID_SETTING",
                "pageSize must be positive or null.",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuerySpec {
    pub tables: Vec<QueryTable>,
    pub fields: Vec<QueryField>,
    #[serde(default)]
    pub joins: Vec<QueryJoin>,
    #[serde(default)]
    pub criteria: Vec<QueryCriterion>,
    #[serde(default)]
    pub parameters: BTreeMap<String, Value>,
    #[serde(default)]
    pub order: Vec<QueryOrder>,
    pub page_size: Option<u32>,
    pub page: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryTable {
    pub schema: String,
    pub table: String,
    pub alias: String,
    pub company_field: Option<String>,
    #[serde(default)]
    pub metadata: Vec<QueryColumn>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryColumn {
    pub name: String,
    pub data_type: String,
    pub format: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryField {
    pub table: String,
    pub field: String,
    pub alias: String,
    pub expression: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryJoin {
    pub left_table: String,
    pub left_field: String,
    pub right_table: String,
    pub right_field: String,
    pub kind: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryCriterion {
    pub table: String,
    pub field: String,
    pub operator: String,
    pub parameter: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QueryOrder {
    pub field: String,
    pub descending: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum HostAction {
    /// Shared host utility; admitted only in Server side mode.
    HostUtility {
        #[serde(rename = "operationId")]
        operation_id: String,
        request: HostUtilityRequest,
    },
    /// Negotiated helper read: decode declared binaries once at the host boundary.
    RetainedArtifactFunctionCall { alias:String, function:String, arguments:Value },
    /// Enroll run-issued immutable inputs; the host owns engine/process/output.
    QueueManagedDecompilation { request: ManagedDecompilationRequest },
    /// Retain declared binary helper output before returning its FileRefs.
    /// Native association remains the calling plugin's responsibility.
    RetainedLocalArtifactFunctionCall { alias:String, function:String, arguments:Value },
    /// Category-neutral project enrollment beneath a validated owning artifact.
    QueueLocalDecompilation { request: LocalDecompilationRequest },
    /// Read a plugin-selected native entity page.
    NativeEntityPage {
        service: String,
        entity: String,
        query: BTreeMap<String, String>,
        page_size: u32,
        page: u64,
    },
    ArtifactInstall {
        alias: String,
    },
    /// Reuse a successful read-only function result within this run and plugin.
    SharedArtifactFunctionCall {
        alias: String,
        function: String,
        arguments: Value,
    },
    ArtifactFunctionCall {
        alias: String,
        function: String,
        arguments: Value,
    },
    /// Submit one producer and download its published inputs while it runs.
    ArtifactFunctionStream {
        alias: String,
        function: String,
        arguments: Value,
        operation_id: String,
        inputs: Vec<host_utility::StagingIdentity>,
        batch_size: usize,
    },
    ArtifactQueryExecute {
        alias: String,
        parameters: Value,
    },
    ArtifactQueryUpdate {
        alias: String,
        dataset: Value,
    },
    ArtifactQueryAction {
        alias: String,
        action: String,
        dataset: Value,
        parameters: Value,
    },
    /// Read a curated host snapshot. No environment variables or credentials.
    SystemInfo,
    HelperCall {
        library: String,
        function: String,
        arguments: Value,
    },
    Assemble {
        chunks: Vec<FileRef>,
        bytes: u64,
        sha256: String,
        extension: String,
    },
    /// Read verified native bytes as base64 for a native container file.
    /// The file must belong to this operation and match its recorded hash.
    ReadBinary {
        file: FileRef,
    },
    /// Build a ZIP from retained host files. Bodies do not enter guest memory.
    BuildZip {
        entries: Vec<(String, FileRef)>,
    },
    /// Parse a verified native XML file already acquired for this operation.
    /// The original bytes remain immutable. No external entities are resolved.
    ParseXml {
        file: FileRef,
    },
    /// Project selected XML members from an owned native ZIP.
    ParseXmlArchive {
        file: FileRef,
        entries: Vec<String>,
    },
    /// Read selected JSON members from a downloaded ZIP without extracting files.
    ParseJsonArchive {
        file: FileRef,
        entries: Vec<String>,
    },
    /// Parse a verified native JSON file already acquired for this operation.
    /// The original bytes remain immutable. The guest owns its data schema.
    ParseJson {
        file: FileRef,
    },
    /// Export to a host-owned server filename, preserve its bytes, then remove it.
    /// The guest chooses the declared exporter, never the cleanup filename.
    NativeFileExport {
        /// Explicit owner for a record materialized within a page. The host
        /// validates category/company and forbids replacing an exact request owner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        identity: Option<Identity>,
        service: String,
        method: String,
        arguments: Value,
        filename_argument: String,
        folder: i32,
        extension: String,
        /// Additional required files written with the same generated filename stem.
        #[serde(default)]
        companion_extensions: Vec<String>,
    },
    /// Reuse an identical plugin-declared read within this run.
    SharedNativeRead {
        service: String,
        method: String,
        arguments: Value,
        export: bool,
    },
    NativeRead {
        service: String,
        method: String,
        arguments: Value,
        export: bool,
    },
    Query {
        query: QuerySpec,
    },
    /// Reuse this exact read-only query within the current run.
    SharedQuery {
        query: QuerySpec,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuestRequest {
    pub abi: u32,
    pub operation: String,
    pub category: String,
    pub company: String,
    pub identity: Option<Identity>,
    pub cursor: Option<String>,
    pub page_size: Option<u32>,
    pub state: Value,
    pub response: Option<Value>,
    pub error: Option<Diagnostic>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum GuestReply {
    /// Write ready records and resume this page. The discovery cursor advances
    /// only after the final Inventory reply, so recovery can replay a partial page.
    ReadyRecords {
        records: Vec<(Identity, Value)>,
        state: Value,
    },
    /// Native page data is ready. Build it locally while the host fetches the
    /// next page. State is opaque to the host and is not a committed cursor.
    /// Optional state._progress.pageRecords reports the actual output record
    /// count for progress. It must not be the requested maximum page size.
    PreparedPage {
        state: Value,
        next_cursor: Option<String>,
        next_state: Value,
    },
    /// Retain plugin-owned data from discovery for the later acquisition call.
    /// The host does not interpret its contents or make collection decisions.
    /// Collectors declaring `pagedDefinitions` instead supply final JSON values:
    /// the host writes those values immediately and skips the acquisition pass.
    InventoryData {
        records: Vec<(Identity, Value)>,
        inventory: Box<GuestReply>,
    },
    /// Persist opaque state beside this module, then process the enclosed reply.
    /// The host supplies the file path. A guest cannot choose another destination.
    State {
        plugin_state: Value,
        reply: Box<GuestReply>,
    },
    References {
        records: Vec<Value>,
        /// Optional local scan coverage and partial-input diagnostics.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        diagnostics: Vec<Diagnostic>,
    },
    Descriptor {
        abi: u32,
        category: Category,
        methods: Vec<(String, String)>,
        #[serde(default)]
        entities: Vec<(String, String)>,
        #[serde(default)]
        binary_exports: Vec<BinaryExport>,
        #[serde(default)]
        response_policies: Vec<NativeResponsePolicy>,
    },
    Action {
        action: HostAction,
        state: Value,
    },
    Inventory {
        identities: Vec<Identity>,
        next_cursor: Option<String>,
        authoritative: bool,
        diagnostics: Vec<Diagnostic>,
    },
    Definition {
        identity: Identity,
        format: String,
        definition: Value,
        completeness: Completeness,
        diagnostics: Vec<Diagnostic>,
    },
    Failure {
        diagnostic: Diagnostic,
    },
}
