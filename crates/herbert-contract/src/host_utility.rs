//! Generic server-side staging. Collection rules remain in plugins.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagedFile {
    /// The helper returns this opaque leaf after accepting a file.
    pub file_id: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagingSlot {
    pub file_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagedInput {
    pub file: StagedFile,
    pub identity: crate::Identity,
}

/// A reserved file and the artifact that owns its downloaded bytes.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StagingIdentity {
    pub file_id: String,
    pub name: String,
    pub identity: crate::Identity,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HostUtilityRequest {
    Begin,
    Inspect,
    /// Reserve exact server files for a plugin helper. No file bytes cross the host.
    Reserve {
        files: Vec<StagingSlot>,
    },
    /// Reserve files that must each be verified locally before cleanup.
    ReserveInputs {
        files: Vec<StagingSlot>,
    },
    Put {
        file_id: String,
        name: String,
        offset: u64,
        data: String,
    },
    Seal {
        files: Vec<StagedFile>,
        compression: ArchiveCompression,
    },
    /// Buffer size is selected by the caller, not a file-size limit.
    Download {
        chunk_bytes: u32,
    },
    /// Download one native file without an outer archive. Verify its receipt.
    DownloadInput {
        file: StagedFile,
        identity: crate::Identity,
        chunk_bytes: u32,
    },
    /// Transfer a group of native files together. Return the selected artifact's file.
    DownloadInputs {
        files: Vec<StagedInput>,
        identity: crate::Identity,
        chunk_bytes: u32,
    },
    /// Cleanup requires a locally verified archive when the server result is ready.
    Close,
    Purge,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveCompression {
    Store,
    Deflate,
}
