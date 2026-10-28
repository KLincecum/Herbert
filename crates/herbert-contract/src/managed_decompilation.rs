//! Narrow local managed-code processing. No executable, arguments, environment or
//! arbitrary path can be selected by a guest. FileRef inputs must be run-issued.
use crate::{FileRef, Identity};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MANAGED_DECOMPILATION_PROTOCOL: u32 = 1;
pub const MANAGED_ENGINE_ID: &str = "ilspy-11.1.0.9782-herbert-v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct ManagedStoreSelector {
    pub folder: String,
    pub assembly: String,
    pub version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all="camelCase")]
pub enum ManagedSymbolMode { None, ExternalPortable, EmbeddedPortable }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub enum ManagedBinaryState { Captured, VerifiedAbsent, Inaccessible, SourceChanging, Failed, Unavailable }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct ManagedCapture {
    pub owner: Identity,
    pub selector: Option<ManagedStoreSelector>,
    pub binary_state: ManagedBinaryState,
    pub diagnostics: Vec<crate::Diagnostic>,
    /// Native fields retain their representation; rowversion is not a DLL version.
    pub observation: Value,
    pub definition_sha256: String,
    pub definition_revision: Value,
    pub input: Option<FileRef>,
    pub symbols: Option<FileRef>,
    pub symbol_mode: ManagedSymbolMode,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct ManagedDecompilationRequest {
    pub protocol_version: u32,
    pub request_id: String,
    pub expected_engine: String,
    pub create_project: bool,
    /// Relative placement chosen by the calling plugin, beneath its owning artifact.
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub output_subdirectory: Option<String>,
    pub capture: ManagedCapture,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct ManagedDecompilationReceipt {
    pub capture_id: String,
    pub job_id: Option<String>,
    pub project_state: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_local_request_rejects_command_and_path_overrides() {
        let value=serde_json::json!({"protocolVersion":1,"requestId":"capture","expectedEngine":MANAGED_ENGINE_ID,"createProject":true,"capture":{
            "owner":{"category":"C08","kind":"EfxLibrary","scope":{"type":"global"},"keys":{"LibraryID":"Sample"}},
            "selector":{"folder":":Efx","assembly":"efx.sample","version":""},"binaryState":"captured","diagnostics":[],"observation":{"SysRevID":"AAAA"},"definitionSha256":"a".repeat(64),"definitionRevision":1,
            "input":{"path":"issued.dll","sha256":"b".repeat(64),"bytes":1,"role":"native"},"symbols":null,"symbolMode":"none"}});
        let request:ManagedDecompilationRequest=serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(),value);
        for key in ["command","arguments","environment","inputPath"] {
            let mut injected=value.clone();injected[key]=serde_json::json!("untrusted");
            assert!(serde_json::from_value::<ManagedDecompilationRequest>(injected).is_err());
        }
    }
}

/// Present only after optional work is durably enrolled. Strict older journals
/// reject this marker and preserve the run rather than silently dropping jobs.
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,schemars::JsonSchema)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct ManagedRunMarker {pub protocol_version:u32,pub ledger:String}

/// Category-neutral local work. Native association and capture status belong to
/// the caller. These inputs are immutable FileRefs issued by the current run.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct LocalDecompilationRequest {
    pub protocol_version: u32,
    pub request_id: String,
    pub expected_engine: String,
    pub owner: Identity,
    pub input: FileRef,
    pub symbols: Option<FileRef>,
    pub symbol_mode: ManagedSymbolMode,
    pub output_subdirectory: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct LocalDecompilationReceipt {
    pub enrollment_id: String,
    pub job_id: Option<String>,
    pub project_state: String,
}
