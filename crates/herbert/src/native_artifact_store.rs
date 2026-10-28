//! Durable native installation ownership and immutable operation sources.
//! Digests below name local records from identity tuples. They never authorize code.
use crate::{plugin_artifacts::NativeArtifactSnapshot, storage};
use fs2::FileExt;
use herbert_contract::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtifactScope {
    pub endpoint: String,
    pub company: String,
    pub caller: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> ArtifactScope {
        ArtifactScope {
            endpoint: "https://example.invalid/ERP".into(),
            company: "C".into(),
            caller: "owner".into(),
        }
    }
    fn snapshot(bytes: &[u8]) -> NativeArtifactSnapshot {
        let declaration=serde_json::from_value(serde_json::json!({"alias":"query","type":"baq","file":"artifacts/query.baq","lifetime":"persistent","policy":"updateOwned","target":"HerbertQuery","effects":{"description":"Reads source definitions."}})).unwrap();
        NativeArtifactSnapshot {
            declaration,
            files: BTreeMap::from([("artifacts/query.baq".into(), Arc::new(bytes.to_vec()))]),
        }
    }
    #[test]
    fn pending_cleanup_does_not_lock_a_new_temporary_instance() {
        let root = tempfile::tempdir().unwrap();
        let mut source = snapshot(b"source");
        source.declaration.lifetime = NativeArtifactLifetime::Temporary;
        source.declaration.target = None;
        let mut old = OperationFile::open(
            root.path(), "old", "plugin", scope(), vec![source.clone()],
        ).unwrap();
        old.phase(0, "deletePending").unwrap();
        let fresh = OperationFile::open(
            root.path(), "fresh", "plugin", scope(), vec![source],
        ).unwrap();
        assert_ne!(old.record.artifacts[0].target, fresh.record.artifacts[0].target);
        let pending = pending_operations(root.path(), &scope()).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].run_id, "old");
        for field in ["endpoint", "company", "caller"] {
            let mut other = scope();
            match field {
                "endpoint" => other.endpoint.push_str("/other"),
                "company" => other.company = "other".into(),
                _ => other.caller = "other".into(),
            }
            assert!(pending_operations(root.path(), &other).unwrap().is_empty());
        }
        old.phase(0, "removed").unwrap();
        assert!(pending_operations(root.path(), &scope()).unwrap().is_empty());
    }
    #[test]
    fn operation_retains_sources_and_locks_until_finished() {
        let root = tempfile::tempdir().unwrap();
        let first = OperationFile::open(
            root.path(),
            "one",
            "plugin",
            scope(),
            vec![snapshot(b"first")],
        )
        .unwrap();
        assert_eq!(first.source(0, "").unwrap().as_slice(), b"first");
        assert!(
            OperationFile::open(
                root.path(),
                "two",
                "plugin",
                scope(),
                vec![snapshot(b"second")]
            )
            .is_err()
        );
        drop(first);
        let recovered = OperationFile::open(
            root.path(),
            "one",
            "plugin",
            scope(),
            vec![snapshot(b"edited")],
        )
        .unwrap();
        assert_eq!(recovered.source(0, "").unwrap().as_slice(), b"first");
        drop(recovered);
        let next = OperationFile::open(
            root.path(),
            "three",
            "plugin",
            scope(),
            vec![snapshot(b"second")],
        )
        .unwrap();
        assert_eq!(next.source(0, "").unwrap().as_slice(), b"second");
    }
    #[test]
    fn closed_generations_keep_receipts_and_frozen_sources_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut source = snapshot(b"frozen source");
        source.declaration.lifetime = NativeArtifactLifetime::Temporary;
        source.declaration.target = None;
        let mut file = OperationFile::open(root.path(), "run", "plugin", scope(), vec![source]).unwrap();
        let first = file.record.artifacts[0].clone();
        file.backup(0, b"original export").unwrap();
        file.phase(0, "removed").unwrap();
        assert!(file.renew_closed(0).unwrap());
        assert_ne!(file.record.artifacts[0].owner, first.owner);
        assert_ne!(file.record.artifacts[0].target, first.target);
        assert_eq!(file.record.artifacts[0].previous_generations[0].phase, "removed");
        assert!(file.record.artifacts[0].previous_generations[0].backup_file.is_some());
        assert_eq!(file.source(0, "").unwrap().as_slice(), b"frozen source");
        file.phase(0, "restored").unwrap();
        drop(file);
        let mut file = OperationFile::open(root.path(), "run", "plugin", scope(), vec![snapshot(b"changed package")]).unwrap();
        assert!(file.renew_closed(0).unwrap());
        assert_eq!(file.record.artifacts[0].previous_generations.len(), 2);
        assert!(file.record.artifacts[0].previous_generations.iter().all(|r| r.previous_generations.is_empty()));
        assert_eq!(file.source(0, "").unwrap().as_slice(), b"frozen source");
    }
    #[test]
    fn renewal_never_resets_unfinished_cleanup_or_persistent_ownership() {
        let root = tempfile::tempdir().unwrap();
        let mut file = OperationFile::open(root.path(), "run", "plugin", scope(), vec![snapshot(b"source")]).unwrap();
        for phase in ["reserved", "ready", "deletePending", "restorePending", "importPending"] {
            file.phase(0, phase).unwrap();
            let before = file.record.clone();
            assert!(!file.renew_closed(0).unwrap());
            assert_eq!(file.record, before);
        }
        file.phase(0, "removed").unwrap();
        assert!(file.renew_closed(0).is_err());
    }
    #[test]
    fn renewed_fixed_target_keeps_target_and_replaces_only_generation() {
        let root = tempfile::tempdir().unwrap();
        let mut source = snapshot(b"source");
        source.declaration.lifetime = NativeArtifactLifetime::Temporary;
        let mut file = OperationFile::open(root.path(), "run", "plugin", scope(), vec![source]).unwrap();
        let old = file.record.artifacts[0].clone();
        file.phase(0, "restored").unwrap();
        assert!(file.renew_closed(0).unwrap());
        assert_eq!(file.record.artifacts[0].target, old.target);
        assert_ne!(file.record.artifacts[0].owner, old.owner);
        assert_eq!(file.record.artifacts[0].phase, "reserved");
    }
    #[test]
    fn ownership_and_backup_survive_reopen_without_author_package() {
        let root = tempfile::tempdir().unwrap();
        let mut first = OperationFile::open(
            root.path(),
            "one",
            "plugin",
            scope(),
            vec![snapshot(b"source")],
        )
        .unwrap();
        first.backup(0, b"original native export").unwrap();
        first.phase(0, "importPending").unwrap();
        first.retain_installation(0).unwrap();
        let owner = first.record.artifacts[0].owner;
        drop(first);
        let recovered = OperationFile::open(root.path(), "one", "plugin", scope(), vec![]).unwrap();
        assert_eq!(
            recovered.backup_bytes(0).unwrap(),
            b"original native export"
        );
        assert_eq!(
            recovered.installed(0).unwrap().unwrap().artifact.owner,
            owner
        );
        assert_eq!(recovered.record.artifacts[0].phase, "importPending");
        drop(recovered);
        let mut other = scope();
        other.caller = "someone else".into();
        assert!(OperationFile::open(root.path(), "one", "plugin", other, vec![]).is_err());
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeIdentity {
    pub id: String,
    pub company: String,
    pub row_id: String,
    pub global_id: Option<String>,
    pub owner: Option<String>,
    pub updatable: Option<bool>,
    pub marker: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArtifactRecord {
    pub declaration: NativeArtifactDeclaration,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub authorization:Option<crate::server_changes::AuthorizationRef>,
    pub target: String,
    pub owner: uuid::Uuid,
    pub files: BTreeMap<String, String>,
    pub phase: String,
    pub identity: Option<NativeIdentity>,
    pub replaced_identity: Option<NativeIdentity>,
    pub backup_file: Option<String>,
    #[serde(default)]
    pub previous_installation: Option<Box<InstalledRecord>>,
    pub diagnostic: Option<Diagnostic>,
    /// Terminal ownership receipts from earlier uses of this frozen capability.
    #[serde(default)]
    pub previous_generations: Vec<ArtifactRecord>,
}
impl ArtifactRecord {
    pub fn matches_import(&self, scope: &ArtifactScope, identity: &NativeIdentity) -> bool {
        identity.id == self.target
            && identity.company == scope.company
            && identity.owner.as_deref() == Some(scope.caller.as_str())
            && identity.marker.lines().any(|line| line == self.marker())
            && match self.declaration.artifact_type {
                NativeArtifactType::FunctionLibrary => {
                    identity
                        .global_id
                        .as_deref()
                        .and_then(|id| uuid::Uuid::parse_str(id).ok())
                        == Some(self.owner)
                }
                NativeArtifactType::Baq | NativeArtifactType::Ubaq => identity.updatable.is_some(),
            }
    }
    pub fn marker(&self) -> String {
        format!("Herbert artifact {}", self.owner)
    }
    pub fn needs_cleanup(&self) -> bool {
        self.declaration.lifetime == NativeArtifactLifetime::Temporary
            && !matches!(self.phase.as_str(), "reserved" | "removed" | "restored" | "released")
    }
}

/// Read pending ownership records for this exact endpoint, company and caller.
/// Startup only reads these records. Remote cleanup runs after collection.
pub(crate) fn pending_operations(root: &Path, scope: &ArtifactScope) -> Result<Vec<OperationRecord>, Diagnostic> {
    let directory = root.join("native-artifacts").join("operations");
    if !directory.exists() { return Ok(Vec::new()); }
    reject_link(&directory)?;
    let mut records = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|_| state_error("Cannot read pending helper ownership."))? {
        let entry = entry.map_err(|_| state_error("Cannot read a pending helper entry."))?;
        reject_link(&entry.path())?;
        if !entry.file_type().map_err(|_| state_error("Cannot inspect a pending helper entry."))?.is_dir() { continue; }
        let path = entry.path().join("operation.json");
        reject_link(&path)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(state_error("Cannot read pending helper ownership.")),
        };
        let record: OperationRecord = serde_json::from_slice(&bytes)
            .map_err(|_| state_error("A pending helper ownership record is invalid."))?;
        if record.schema_version == 1 && &record.scope == scope && record.artifacts.iter().any(ArtifactRecord::needs_cleanup) {
            records.push(record);
        }
    }
    Ok(records)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OperationRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub plugin: String,
    pub scope: ArtifactScope,
    pub artifacts: Vec<ArtifactRecord>,
}
pub(crate) struct OperationFile {
    pub directory: PathBuf,
    pub record: OperationRecord,
    _lock: File,
    target_locks: Vec<File>,
    registry_root: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledRecord {
    pub scope: ArtifactScope,
    pub plugin: String,
    pub alias: String,
    pub operation: String,
    pub artifact: ArtifactRecord,
}
fn state_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("NATIVE_ARTIFACT_STATE_FAILED", message)
}
fn reject_link(path: &Path) -> Result<(), Diagnostic> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => Err(state_error(
            "Native artifact state must not use a link or junction.",
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(state_error("Cannot inspect native artifact state.")),
    }
}
fn checked_dir(path: &Path) -> Result<(), Diagnostic> {
    reject_link(path)?;
    fs::create_dir_all(path)
        .map_err(|_| state_error("Cannot create native artifact state directory."))
}
fn key(value: &impl Serialize) -> Result<String, Diagnostic> {
    Ok(storage::sha256(&serde_json::to_vec(value).map_err(
        |_| state_error("Cannot encode artifact identity."),
    )?))
}
fn lock(path: &Path) -> Result<File, Diagnostic> {
    reject_link(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| state_error("Cannot open native artifact ownership lock."))?;
    file.try_lock_exclusive().map_err(|_|Diagnostic::new("NATIVE_ARTIFACT_IN_USE","Another operation owns this native installation. Let that operation finish before updating or removing it."))?;
    Ok(file)
}
impl OperationFile {
    pub fn open(
        root: &Path,
        run_id: &str,
        plugin: &str,
        scope: ArtifactScope,
        snapshots: Vec<NativeArtifactSnapshot>,
    ) -> Result<Self, Diagnostic> {
        let base = root.join("native-artifacts");
        checked_dir(&base)?;
        let operations = base.join("operations");
        checked_dir(&operations)?;
        let registry_root = base.join("installed");
        checked_dir(&registry_root)?;
        let directory = operations.join(key(&(run_id, plugin))?);
        checked_dir(&directory)?;
        let operation_lock = lock(&directory.join("operation.lock"))?;
        let record_path = directory.join("operation.json");
        reject_link(&record_path)?;
        let record = match fs::read(&record_path) {
            Ok(bytes) => {
                let record: OperationRecord = serde_json::from_slice(&bytes)
                    .map_err(|_| state_error("The saved artifact operation is malformed."))?;
                if record.schema_version != 1
                    || record.run_id != run_id
                    || record.plugin != plugin
                    || record.scope != scope
                {
                    return Err(Diagnostic::new(
                        "NATIVE_ARTIFACT_OWNER_MISMATCH",
                        "The saved artifact operation belongs to a different run, plugin or admitted caller.",
                    ));
                }
                record
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut artifacts = Vec::new();
                for (index, snapshot) in snapshots.into_iter().enumerate() {
                    let owner = uuid::Uuid::new_v4();
                    // EFX library IDs have a native 30-character constraint on the
                    // qualified runtime. Keep the full UUID as ownership metadata.
                    let target = snapshot
                        .declaration
                        .target
                        .clone()
                        .unwrap_or_else(|| format!("H{}", &owner.simple().to_string()[..29]));
                    let mut files = BTreeMap::new();
                    for (file_index, (name, bytes)) in snapshot.files.into_iter().enumerate() {
                        let relative =
                            format!("source-{index}-{file_index}-{}.bin", uuid::Uuid::new_v4());
                        storage::atomic_write(&directory.join(&relative), &bytes)?;
                        files.insert(name, relative);
                    }
                    artifacts.push(ArtifactRecord {
                        authorization:None,
                        declaration: snapshot.declaration,
                        target,
                        owner,
                        files,
                        phase: "reserved".into(),
                        identity: None,
                        replaced_identity: None,
                        backup_file: None,
                        previous_installation: None,
                        diagnostic: None,
                        previous_generations: Vec::new(),
                    });
                }
                OperationRecord {
                    schema_version: 1,
                    run_id: run_id.into(),
                    plugin: plugin.into(),
                    scope,
                    artifacts,
                }
            }
            Err(_) => return Err(state_error("Cannot read the saved artifact operation.")),
        };
        let mut file = Self {
            directory,
            record,
            _lock: operation_lock,
            target_locks: Vec::new(),
            registry_root,
        };
        // Acquire all target locks in deterministic identity order, and retain them
        // through use and teardown. No later run can update an active instance.
        let mut targets = file
            .record
            .artifacts
            .iter()
            .map(|a| file.target_key(a))
            .collect::<Result<Vec<_>, _>>()?;
        targets.sort();
        targets.dedup();
        for target in targets {
            file.target_locks
                .push(lock(&file.registry_root.join(format!("{target}.lock")))?);
        }
        file.checkpoint()?;
        Ok(file)
    }
    /// Start another use only after verified cleanup. Source paths remain frozen.
    pub fn renew_closed(&mut self, index: usize) -> Result<bool, Diagnostic> {
        let old = &self.record.artifacts[index];
        if !matches!(old.phase.as_str(), "removed" | "restored" | "released") {
            return Ok(false);
        }
        if old.declaration.lifetime != NativeArtifactLifetime::Temporary {
            return Err(state_error("A persistent installation cannot rotate as a temporary capability."));
        }
        let mut next = old.clone();
        let mut receipt = old.clone();
        receipt.previous_generations.clear();
        next.previous_generations.push(receipt);
        next.owner = uuid::Uuid::new_v4();
        next.target = next.declaration.target.clone().unwrap_or_else(||
            format!("H{}", &next.owner.simple().to_string()[..29]));
        // Fixed targets already hold this lock. New instance names get a new lock
        // before their import intent can be committed.
        if next.target != old.target {
            self.target_locks.push(lock(&self.registry_root.join(format!("{}.lock", self.target_key(&next)?)))?);
        }
        next.phase = "reserved".into();
        next.authorization=None;
        next.identity = None;
        next.replaced_identity = None;
        next.backup_file = None;
        next.previous_installation = None;
        next.diagnostic = None;
        let old = std::mem::replace(&mut self.record.artifacts[index], next);
        if let Err(error) = self.checkpoint() {
            self.record.artifacts[index] = old;
            return Err(error);
        }
        Ok(true)
    }
    pub fn checkpoint(&self) -> Result<(), Diagnostic> {
        storage::atomic_write(
            &self.directory.join("operation.json"),
            &serde_json::to_vec_pretty(&self.record)
                .map_err(|_| state_error("Cannot encode artifact operation."))?,
        )
    }
    pub fn source(&self, index: usize, version: &str) -> Result<Arc<Vec<u8>>, Diagnostic> {
        let record = &self.record.artifacts[index];
        let name = record.declaration.file_for_version(version);
        let relative = record.files.get(name).ok_or_else(|| {
            state_error("The operation has no frozen source for this native version.")
        })?;
        let path = self.directory.join(storage::safe_relative(relative)?);
        reject_link(&path)?;
        Ok(Arc::new(fs::read(path).map_err(|_|state_error("The retained native source is unavailable. Preserve the operation and reconnect after restoring its source file."))?))
    }
    pub fn backup(&mut self, index: usize, bytes: &[u8]) -> Result<(), Diagnostic> {
        let relative = format!("backup-{index}-{}.bin", uuid::Uuid::new_v4());
        storage::atomic_write(&self.directory.join(&relative), bytes)?;
        self.record.artifacts[index].backup_file = Some(relative);
        self.checkpoint()
    }
    pub fn backup_bytes(&self, index: usize) -> Result<Vec<u8>, Diagnostic> {
        let relative = self.record.artifacts[index]
            .backup_file
            .as_ref()
            .ok_or_else(|| state_error("The replacement backup is absent."))?;
        let path = self.directory.join(storage::safe_relative(relative)?);
        reject_link(&path)?;
        fs::read(path)
            .map_err(|_| state_error("Cannot read the preserved native replacement backup."))
    }
    pub fn phase(&mut self, index: usize, phase: &str) -> Result<(), Diagnostic> {
        self.record.artifacts[index].phase = phase.into();
        self.checkpoint()
    }
    fn target_key(&self, record: &ArtifactRecord) -> Result<String, Diagnostic> {
        let namespace = if record.declaration.artifact_type == NativeArtifactType::FunctionLibrary {
            "function"
        } else {
            "query"
        };
        // Query names can resolve to shared/global rows across companies. Lock
        // that native namespace across companies instead of racing a global row.
        key(&(&self.record.scope.endpoint, namespace, &record.target))
    }
    pub fn installed(&self, index: usize) -> Result<Option<InstalledRecord>, Diagnostic> {
        let path = self.registry_root.join(format!(
            "{}.json",
            self.target_key(&self.record.artifacts[index])?
        ));
        reject_link(&path)?;
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| state_error("The native installation ownership record is malformed.")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(state_error("Cannot read native installation ownership.")),
        }
    }
    pub fn retain_installation(&self, index: usize) -> Result<(), Diagnostic> {
        let artifact = self.record.artifacts[index].clone();
        let path = self
            .registry_root
            .join(format!("{}.json", self.target_key(&artifact)?));
        reject_link(&path)?;
        let mut installed = InstalledRecord {
            scope: self.record.scope.clone(),
            plugin: self.record.plugin.clone(),
            alias: artifact.declaration.alias.clone(),
            operation: self.record.run_id.clone(),
            artifact,
        };
        if installed.artifact.phase == "restored" {
            if let Some(previous) = installed.artifact.previous_installation.clone() {
                let restored = installed.artifact.identity.clone();
                installed = *previous;
                if let Some(identity) = restored {
                    installed.artifact.identity = Some(identity);
                }
            }
        }
        storage::atomic_write(
            &path,
            &serde_json::to_vec_pretty(&installed)
                .map_err(|_| state_error("Cannot serialize native installation ownership."))?,
        )
    }
}
