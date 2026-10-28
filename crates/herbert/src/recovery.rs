//! Secret-free durable run intents and accepted-request reconciliation.
use crate::storage::{RunJournal, atomic_write, sha256};
use fs2::FileExt;
use herbert_contract::Diagnostic;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunRecord {
    pub schema_version: u32,
    pub scope_hash: String,
    pub destination: String,
    pub accepted: bool,
    pub full: bool,
    pub page_size: Option<u32>,
    pub journal: RunJournal,
}
pub struct RunIndex {
    root: PathBuf,
    _lock: File,
}
fn failure(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn owned_file(root: &Path, relative: &str) -> Result<PathBuf, Diagnostic> {
    let relative = crate::storage::safe_relative(relative)?;
    let path = root.join(relative);
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(failure(
            "UNSAFE_PATH",
            "A run run index file cannot be a symbolic link.",
        ));
    }
    Ok(path)
}
impl RunIndex {
    pub fn open(state: &Path) -> Result<Self, Diagnostic> {
        let root = state.join("runs");
        if fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(failure(
                "UNSAFE_PATH",
                "The run run index cannot be a symbolic link.",
            ));
        }
        fs::create_dir_all(&root)
            .map_err(|_| failure("REGISTRY_UNAVAILABLE", "Cannot create durable run state."))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(owned_file(&root, "registry.lock")?)
            .map_err(|_| {
                failure(
                    "REGISTRY_UNAVAILABLE",
                    "Cannot open durable run registration.",
                )
            })?;
        lock.lock_exclusive().map_err(|_| {
            failure(
                "REGISTRY_UNAVAILABLE",
                "Cannot acquire durable run registration.",
            )
        })?;
        Ok(Self { root, _lock: lock })
    }
    fn read(&self, path: &Path) -> Result<RunRecord, Diagnostic> {
        let bytes = fs::read(path)
            .map_err(|_| failure("RUN_NOT_FOUND", "The durable run record is unavailable."))?;
        let record: RunRecord = serde_json::from_slice(&bytes).map_err(|_| {
            failure(
                "RUN_RECORD_INVALID",
                "The durable run record is invalid; preserve it for reconciliation.",
            )
        })?;
        if record.schema_version != 1 || uuid::Uuid::parse_str(&record.journal.run_id).is_err() {
            return Err(failure(
                "RUN_RECORD_INVALID",
                "The durable run identity or schema is invalid.",
            ));
        }
        Ok(record)
    }
    pub fn get(&self, id: &str) -> Result<RunRecord, Diagnostic> {
        uuid::Uuid::parse_str(id)
            .map_err(|_| failure("INVALID_RUN_ID", "Use a native Herbert run UUID."))?;
        let record = self.read(&owned_file(&self.root, &format!("{id}.json"))?)?;
        if record.journal.run_id != id {
            return Err(failure(
                "RUN_RECORD_INVALID",
                "The saved run identity does not match its filename.",
            ));
        }
        Ok(record)
    }
    pub fn request(&self, id: &str) -> Result<Option<RunRecord>, Diagnostic> {
        let path = owned_file(
            &self.root,
            &format!("request-{}.json", sha256(id.as_bytes())),
        )?;
        if path.exists() {
            let record = self.read(&path)?;
            if record.journal.request_id != id {
                return Err(failure(
                    "RUN_RECORD_INVALID",
                    "The saved request identity does not match its index.",
                ));
            }
            return Ok(Some(record));
        }
        // Recover the index after a crash between the two atomic run index writes.
        let mut found = None;
        for entry in fs::read_dir(&self.root).map_err(|_| {
            failure(
                "REGISTRY_UNAVAILABLE",
                "Cannot reconcile durable run intents.",
            )
        })? {
            let entry = entry
                .map_err(|_| failure("REGISTRY_UNAVAILABLE", "Cannot read a durable run entry."))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json")
                || uuid::Uuid::parse_str(name.trim_end_matches(".json")).is_err()
            {
                continue;
            }
            let record = self.read(&owned_file(&self.root, &name)?)?;
            if record.journal.request_id == id {
                if found.is_some() {
                    return Err(failure(
                        "REQUEST_OWNERSHIP_UNCERTAIN",
                        "More than one durable run claims this request; reconcile exact saved records before starting again.",
                    ));
                }
                found = Some(record);
            }
        }
        Ok(found)
    }
    pub fn save(&self, record: &RunRecord) -> Result<(), Diagnostic> {
        let bytes = serde_json::to_vec_pretty(record).map_err(|_| {
            failure(
                "REGISTRY_UNAVAILABLE",
                "Cannot serialize durable run state.",
            )
        })?;
        atomic_write(
            &owned_file(&self.root, &format!("{}.json", record.journal.run_id))?,
            &bytes,
        )?;
        atomic_write(
            &owned_file(
                &self.root,
                &format!(
                    "request-{}.json",
                    sha256(record.journal.request_id.as_bytes())
                ),
            )?,
            &bytes,
        )
    }
    pub fn list(&self) -> Result<Vec<serde_json::Value>, Diagnostic> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root)
            .map_err(|_| failure("RUN_INDEX_UNAVAILABLE", "Cannot read saved run state."))?
        {
            let entry = entry
                .map_err(|_| failure("RUN_INDEX_UNAVAILABLE", "Cannot read a saved run entry."))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json")
                || uuid::Uuid::parse_str(name.trim_end_matches(".json")).is_err()
            {
                continue;
            }
            match self.read(&owned_file(&self.root,&name)?){
                Ok(record)=>{
                    let loaded=read_journal(&record);
                    let (journal,diagnostic)=match loaded{Ok(j)=>(j,None),Err(e)=>(record.journal.clone(),Some(e))};
                    records.push(serde_json::json!({"runId":journal.run_id,"requestId":journal.request_id,"accepted":record.accepted,"destination":record.destination,"endpoint":journal.endpoint,"company":journal.company,"caller":journal.caller,"scope":journal.scope,"phase":journal.phase,"artifacts":journal.artifacts.len(),"diagnostic":diagnostic}));
                },Err(diagnostic)=>records.push(serde_json::json!({"runId":name.trim_end_matches(".json"),"diagnostic":diagnostic})),
            }
        }
        records.sort_by(|a, b| a["runId"].as_str().cmp(&b["runId"].as_str()));
        Ok(records)
    }
}
pub fn read_journal(record: &RunRecord) -> Result<RunJournal, Diagnostic> {
    let root = PathBuf::from(&record.destination)
        .canonicalize()
        .map_err(|_| {
            failure(
                "OUTPUT_UNAVAILABLE",
                "The recorded backup destination is unavailable.",
            )
        })?;
    let folder = if !root.join("Herbert Data").exists() && root.join(".hoover").exists() { ".hoover" } else { "Herbert Data" };
    let relative = format!("{folder}/runs/{}/journal.json", record.journal.run_id);
    let mut path = root.clone();
    for component in crate::storage::safe_relative(&relative)?.components() {
        path.push(component);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(failure(
                "UNSAFE_PATH",
                "The recorded journal crosses a symbolic link or junction.",
            ));
        }
    }
    let journal: RunJournal = if let Some(journal) =
        crate::checkpoint_log::read_journal(&root, &record.journal.run_id)?
    {
        journal
    } else {
        serde_json::from_slice(&fs::read(path).map_err(|_| {
            failure(
                "JOURNAL_UNAVAILABLE",
                "The saved run journal is unavailable.",
            )
        })?)
        .map_err(|_| {
            failure(
                "JOURNAL_INVALID",
                "The saved run journal is invalid; preserve it.",
            )
        })?
    };
    if journal.run_id != record.journal.run_id
        || journal.request_id != record.journal.request_id
        || journal.endpoint != record.journal.endpoint
        || journal.company != record.journal.company
        || journal.caller != record.journal.caller
    {
        return Err(failure(
            "RUN_SCOPE_MISMATCH",
            "The saved journal does not match its accepted endpoint, caller, company and request.",
        ));
    }
    Ok(journal)
}
