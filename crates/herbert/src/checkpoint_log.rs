//! Synchronous incremental checkpoints. The tip is the acknowledgement boundary.
//! Full JSON files are materialized views. Recovery always replays committed state.
use crate::storage::{Manifest, RunJournal, atomic_write, sha256};
use herbert_contract::{Artifact, Diagnostic, FileRef, Identity, OperatingSettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const FORMAT: u32 = 2;
const MAGIC: &[u8; 8] = b"HBTCKP02";
const HEADER: usize = 80; // magic + u64 length + payload SHA256 + header SHA256
const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn invalid() -> Diagnostic {
    error(
        "CHECKPOINT_INVALID",
        "The committed checkpoint chain is invalid. Preserve its files; recovery refused.",
    )
}
fn io_error() -> Diagnostic {
    error(
        "CHECKPOINT_IO_UNCERTAIN",
        "Checkpoint persistence failed. Preserve its files and reopen to reconcile before further work.",
    )
}
fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>, Diagnostic> {
    serde_json::to_vec(value).map_err(|_| invalid())
}

/// Unknown to v1 readers, whose deny_unknown_fields prevents stale-view writes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Marker {
    pub format: u32,
    pub run_id: String,
}
impl Marker {
    pub fn new(run_id: &str) -> Self {
        Self {
            format: FORMAT,
            run_id: run_id.into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Snapshot {
    journal: RunJournal,
    manifest: Manifest,
}
impl Snapshot {
    fn validate(&self) -> Result<(), Diagnostic> {
        if self.journal.schema_version != 1
            || self.manifest.schema_version != 1
            || self.journal.endpoint != self.manifest.endpoint
            || self.journal.company != self.manifest.company
            || self.journal.caller != self.manifest.caller
            || self.manifest.checkpoint.as_ref() != Some(&Marker::new(&self.journal.run_id))
        {
            return Err(invalid());
        }
        canonical_id(&self.journal.run_id)?;
        Ok(())
    }
    fn same_scope(&self, journal: &RunJournal) -> bool {
        self.journal.run_id == journal.run_id
            && self.journal.request_id == journal.request_id
            && self.journal.endpoint == journal.endpoint
            && self.journal.company == journal.company
            && self.journal.caller == journal.caller
            && self.journal.scope == journal.scope
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replace<T> {
    value: T,
}
impl<T: Clone + PartialEq> Replace<T> {
    fn between(before: &T, after: &T) -> Option<Self> {
        (before != after).then(|| Self {
            value: after.clone(),
        })
    }
    fn apply(self, target: &mut T) -> Result<(), Diagnostic> {
        *target = self.value;
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Sequence<T> {
    before_len: usize,
    start: usize,
    remove: usize,
    insert: Vec<T>,
}
impl<T: Clone + PartialEq> Sequence<T> {
    fn between(before: &Vec<T>, after: &Vec<T>) -> Option<Self> {
        let start = before.iter().zip(after).take_while(|(a, b)| a == b).count();
        if start == before.len() && start == after.len() {
            return None;
        }
        let suffix = before[start..]
            .iter()
            .rev()
            .zip(after[start..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        Some(Self {
            before_len: before.len(),
            start,
            remove: before.len() - start - suffix,
            insert: after[start..after.len() - suffix].to_vec(),
        })
    }
    fn apply(self, target: &mut Vec<T>) -> Result<(), Diagnostic> {
        if target.len() != self.before_len
            || self.start > target.len()
            || self.remove > target.len() - self.start
        {
            return Err(invalid());
        }
        target.splice(self.start..self.start + self.remove, self.insert);
        Ok(())
    }
}
// References grow by adding occurrences to existing identities. Save those
// additions instead of rewriting every earlier occurrence between changed rows.
// The legacy variant keeps existing checkpoint logs readable.
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ReferenceChanges<T = Value> {
    Legacy(Sequence<T>),
    Incremental(ReferenceAppend),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReferenceAppend {
    reference_count: usize,
    occurrences: BTreeMap<String, Sequence<Value>>,
    append: Vec<Value>,
}
impl ReferenceChanges<Value> {
    fn between(before: &Vec<Value>, after: &Vec<Value>) -> Option<Self> {
        let compatible = before.len() <= after.len()
            && before
                .iter()
                .zip(after)
                .all(|(a, b)| match (a.as_object(), b.as_object()) {
                    (Some(a), Some(b)) => {
                        a.get("occurrences").is_some_and(Value::is_array)
                            && b.get("occurrences").is_some_and(Value::is_array)
                            && a.len() == b.len()
                            && a.iter()
                                .filter(|(key, _)| key.as_str() != "occurrences")
                                .all(|(key, value)| b.get(key) == Some(value))
                    }
                    _ => false,
                });
        if !compatible {
            return Sequence::between(before, after).map(Self::Legacy);
        }
        let occurrences = before
            .iter()
            .zip(after)
            .enumerate()
            .filter_map(|(index, (a, b))| {
                Sequence::between(
                    a["occurrences"].as_array().unwrap(),
                    b["occurrences"].as_array().unwrap(),
                )
                .map(|change| (index.to_string(), change))
            })
            .collect::<BTreeMap<_, _>>();
        let append = after[before.len()..].to_vec();
        if occurrences.is_empty() && append.is_empty() {
            None
        } else {
            Some(Self::Incremental(ReferenceAppend {
                reference_count: before.len(),
                occurrences,
                append,
            }))
        }
    }
    fn apply(self, target: &mut Vec<Value>) -> Result<(), Diagnostic> {
        match self {
            Self::Legacy(change) => change.apply(target),
            Self::Incremental(change) => {
                if target.len() != change.reference_count {
                    return Err(invalid());
                }
                for (index, patch) in change.occurrences {
                    let index = index.parse::<usize>().map_err(|_| invalid())?;
                    let values = target
                        .get_mut(index)
                        .and_then(|v| v.get_mut("occurrences"))
                        .and_then(Value::as_array_mut)
                        .ok_or_else(invalid)?;
                    patch.apply(values)?;
                }
                target.extend(change.append);
                Ok(())
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mapping<T> {
    remove: Vec<String>,
    put: BTreeMap<String, T>,
}
impl<T: Clone + PartialEq> Mapping<T> {
    fn between(before: &BTreeMap<String, T>, after: &BTreeMap<String, T>) -> Option<Self> {
        let remove = before
            .keys()
            .filter(|key| !after.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        let put = after
            .iter()
            .filter(|(key, value)| before.get(*key) != Some(*value))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        (!remove.is_empty() || !put.is_empty()).then_some(Self { remove, put })
    }
    fn apply(self, target: &mut BTreeMap<String, T>) -> Result<(), Diagnostic> {
        for key in self.remove {
            if target.remove(&key).is_none() {
                return Err(invalid());
            }
        }
        target.extend(self.put);
        Ok(())
    }
}

// Explicit fields make additions fail review/compile rather than silently hiding
// an arbitrary JSON snapshot/diff in the hot path. Only changed values are cloned.
macro_rules! delta {
    ($name:ident, $target:ident, { $( $field:ident : $patch:ident<$value:ty> ),* $(,)? }) => {
        #[derive(Clone, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct $name {
            $( #[serde(default, skip_serializing_if = "Option::is_none")]
               $field: Option<$patch<$value>>, )*
        }
        impl $name {
            fn between(before: &$target, after: &$target) -> Self {
                // Require every persisted field here. New fields must not vanish during replay.
                let $target { $( $field: _, )* } = after;
                Self { $( $field: $patch::between(&before.$field, &after.$field), )* }
            }
            fn is_empty(&self) -> bool { true $( && self.$field.is_none() )* }
            fn apply(self, target: &mut $target) -> Result<(), Diagnostic> {
                $( if let Some(change) = self.$field { change.apply(&mut target.$field)?; } )*
                Ok(())
            }
        }
    };
}
delta!(JournalDelta, RunJournal, {
    server_version: Replace<Option<String>>, auth_mode: Replace<Option<crate::input::AuthMode>>,
    schema_version: Replace<u32>, run_id: Replace<String>, request_id: Replace<String>,
    endpoint: Replace<String>, company: Replace<String>, caller: Replace<String>,
    scope: Replace<String>, categories: Sequence<String>, plugin_hashes: Mapping<String>, category_names: Mapping<String>, separate_folder: Replace<Option<String>>,
    settings: Replace<OperatingSettings>, phase: Replace<String>, statistics: Replace<Option<Value>>,
    artifacts: Sequence<Artifact>, diagnostics: Sequence<Diagnostic>,
    pending_files: Sequence<FileRef>, acquisitions: Sequence<FileRef>,
    cursors: Mapping<String>, discovered: Mapping<Vec<Identity>>, inventory_data: Mapping<FileRef>,
    discovery_finished: Mapping<bool>, category_outcomes: Mapping<String>,
    references: ReferenceChanges<Value>, definition_views: Mapping<FileRef>, shared_exports: Mapping<Vec<FileRef>>,
    artifact_acquisitions: Mapping<Vec<FileRef>>, targets: Sequence<Identity>,
    plugin_settings: Mapping<Value>, plugin_selection_contracts: Mapping<Value>, managed_processing: Replace<Option<herbert_contract::ManagedRunMarker>>, helper_transfers: Sequence<crate::helpers::HelperTransfer>,
    helper_libraries: Sequence<crate::helper_libraries::LibraryLease>,
    native_artifacts: Sequence<crate::native_artifact_store::OperationRecord>,
    native_export_files: Sequence<crate::application::native_file_export::ExportFile>,
    host_transfers: Sequence<crate::application::host_utility::Transfer>,
    collection_method: Replace<herbert_contract::CollectionMethod>,
    temporary_access_enabled: Replace<bool>, access_leases: Sequence<crate::access::AccessLease>,
    server_change_authority: Replace<Option<crate::server_changes::RunAuthority>>,
    server_change_authority_history: Sequence<crate::server_changes::RunAuthority>,
});
delta!(ManifestDelta, Manifest, {
    schema_version: Replace<u32>, endpoint: Replace<String>, company: Replace<String>,
    caller: Replace<String>, references: ReferenceChanges<Value>, artifacts: Mapping<Artifact>,
    retained_files: Mapping<FileRef>, checkpoint: Replace<Option<Marker>>,
});
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Changes {
    journal: JournalDelta,
    manifest: ManifestDelta,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Record {
    Base { snapshot: Snapshot },
    Changes { changes: Changes },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    format: u32,
    sequence: u64,
    previous_sha256: String,
    record: Record,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Tip {
    format: u32,
    run_id: String,
    sequence: u64,
    end: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Views {
    format: u32,
    sequence: u64,
    sha256: String,
    journal_sha256: String,
    manifest_sha256: String,
}

fn canonical_id(id: &str) -> Result<(), Diagnostic> {
    if uuid::Uuid::parse_str(id)
        .map(|v| v.to_string())
        .ok()
        .as_deref()
        != Some(id)
    {
        return Err(invalid());
    }
    Ok(())
}
fn confined(root: &Path, relative: &str) -> Result<PathBuf, Diagnostic> {
    let root = root.canonicalize().map_err(|_| io_error())?;
    let relative = if !root.join("Herbert Data").exists() && root.join(".hoover").exists() {
        relative.replacen("Herbert Data/", ".hoover/", 1)
    } else {
        relative.to_owned()
    };
    let mut path = root.clone();
    for component in crate::storage::safe_relative(&relative)?.components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink()
                    || !path
                        .canonicalize()
                        .map_err(|_| io_error())?
                        .starts_with(&root)
                {
                    return Err(error(
                        "UNSAFE_PATH",
                        "Checkpoint paths cannot cross a symbolic link or junction.",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(io_error()),
        }
    }
    Ok(path)
}
fn directory(root: &Path, id: &str) -> Result<PathBuf, Diagnostic> {
    canonical_id(id)?;
    confined(root, &format!("Herbert Data/runs/{id}"))
}
fn paths(root: &Path, id: &str) -> Result<(PathBuf, PathBuf, PathBuf), Diagnostic> {
    canonical_id(id)?;
    Ok((
        confined(
            root,
            &format!("Herbert Data/runs/{id}/checkpoint-v2/state.log"),
        )?,
        confined(
            root,
            &format!("Herbert Data/runs/{id}/checkpoint-v2/tip.json"),
        )?,
        confined(
            root,
            &format!("Herbert Data/runs/{id}/checkpoint-v2/views.json"),
        )?,
    ))
}
fn frame(envelope: &Envelope) -> Result<(Vec<u8>, String), Diagnostic> {
    let payload = encoded(envelope)?;
    let digest = sha256(&payload);
    let length = u64::try_from(payload.len()).map_err(|_| invalid())?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&hex::decode(&digest).map_err(|_| invalid())?);
    let header_digest = sha256(&bytes);
    bytes.extend_from_slice(&hex::decode(header_digest).map_err(|_| invalid())?);
    bytes.extend_from_slice(&payload);
    Ok((bytes, digest))
}

struct Replayed {
    snapshot: Snapshot,
    tip: Tip,
    trailing: u64,
}
fn replay(root: &Path, id: &str) -> Result<Option<Replayed>, Diagnostic> {
    let (log, tip_path, _) = paths(root, id)?;
    let tip_bytes = match fs::read(&tip_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !log.exists() => return Ok(None),
        Err(_) => return Err(invalid()),
    };
    let tip: Tip = serde_json::from_slice(&tip_bytes).map_err(|_| invalid())?;
    if tip.format != FORMAT || tip.run_id != id || tip.sequence == 0 {
        return Err(invalid());
    }
    let mut input = File::open(log).map_err(|_| invalid())?;
    let length = input.metadata().map_err(|_| io_error())?.len();
    if length < tip.end {
        return Err(invalid());
    }
    let mut offset = 0u64;
    let mut sequence = 0u64;
    let mut previous = ZERO.to_string();
    let mut snapshot: Option<Snapshot> = None;
    while offset < tip.end {
        if tip.end - offset < HEADER as u64 {
            return Err(invalid());
        }
        let mut header = [0u8; HEADER];
        input.read_exact(&mut header).map_err(|_| invalid())?;
        if &header[..8] != MAGIC || sha256(&header[..48]) != hex::encode(&header[48..80]) {
            return Err(invalid());
        }
        let count = u64::from_le_bytes(header[8..16].try_into().map_err(|_| invalid())?);
        if count > tip.end - offset - HEADER as u64 {
            return Err(invalid());
        }
        let size = usize::try_from(count).map_err(|_| invalid())?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).map_err(|_| io_error())?;
        bytes.resize(size, 0);
        input.read_exact(&mut bytes).map_err(|_| invalid())?;
        let digest = sha256(&bytes);
        if digest != hex::encode(&header[16..48]) {
            return Err(invalid());
        }
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        sequence = sequence.checked_add(1).ok_or_else(invalid)?;
        if envelope.format != FORMAT
            || envelope.sequence != sequence
            || envelope.previous_sha256 != previous
        {
            return Err(invalid());
        }
        match envelope.record {
            Record::Base { snapshot: base } if sequence == 1 => {
                base.validate()?;
                if base.journal.run_id != id {
                    return Err(invalid());
                }
                snapshot = Some(base);
            }
            Record::Changes { changes } if sequence > 1 => {
                let state = snapshot.as_mut().ok_or_else(invalid)?;
                let scope = (
                    state.journal.run_id.clone(),
                    state.journal.request_id.clone(),
                    state.journal.endpoint.clone(),
                    state.journal.company.clone(),
                    state.journal.caller.clone(),
                    state.journal.scope.clone(),
                );
                changes.journal.apply(&mut state.journal)?;
                changes.manifest.apply(&mut state.manifest)?;
                state.validate()?;
                if scope
                    != (
                        state.journal.run_id.clone(),
                        state.journal.request_id.clone(),
                        state.journal.endpoint.clone(),
                        state.journal.company.clone(),
                        state.journal.caller.clone(),
                        state.journal.scope.clone(),
                    )
                {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
        previous = digest;
        offset = offset
            .checked_add(HEADER as u64)
            .and_then(|n| n.checked_add(count))
            .ok_or_else(invalid)?;
    }
    if offset != tip.end || sequence != tip.sequence || previous != tip.sha256 {
        return Err(invalid());
    }
    Ok(Some(Replayed {
        snapshot: snapshot.ok_or_else(invalid)?,
        tip,
        trailing: length - offset,
    }))
}

pub(crate) fn read_journal(root: &Path, id: &str) -> Result<Option<RunJournal>, Diagnostic> {
    let Some(mut state) = replay(root, id)? else {
        return Ok(None);
    };
    if terminal(&state.snapshot.journal.phase) && !views_current(root, id, &state.tip)? {
        state.snapshot.journal.phase = "destinationReleaseUncertain".into();
        state.snapshot.journal.diagnostics.push(error(
            "CHECKPOINT_FINALIZATION_PENDING",
            "Committed acquisitions are recoverable, but the final journal/manifest views were not sealed. Resume finalization.",
        ));
    }
    Ok(Some(state.snapshot.journal))
}
pub(crate) fn read_manifest(root: &Path, manifest: Manifest) -> Result<Manifest, Diagnostic> {
    let Some(marker) = &manifest.checkpoint else {
        return Ok(manifest);
    };
    if marker.format != FORMAT {
        return Err(invalid());
    }
    let recovered = replay(root, &marker.run_id)?
        .ok_or_else(invalid)?
        .snapshot
        .manifest;
    if recovered.endpoint != manifest.endpoint
        || recovered.company != manifest.company
        || recovered.caller != manifest.caller
    {
        return Err(invalid());
    }
    Ok(recovered)
}
fn terminal(phase: &str) -> bool {
    matches!(
        phase,
        "completed" | "partial" | "failed" | "canceled" | "destinationReleaseUncertain"
    )
}
fn views_current(root: &Path, id: &str, tip: &Tip) -> Result<bool, Diagnostic> {
    let (_, _, path) = paths(root, id)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(io_error()),
    };
    let views: Views = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if views.format != FORMAT || views.sequence != tip.sequence || views.sha256 != tip.sha256 {
        return Ok(false);
    }
    let journal = confined(root, &format!("Herbert Data/runs/{id}/journal.json"))?;
    // Later legitimate refreshes replace the destination manifest. The sealed
    // per-run view keeps earlier completed runs independently verifiable.
    let manifest = confined(root, &format!("Herbert Data/runs/{id}/manifest.json"))?;
    Ok(fs::read(journal)
        .ok()
        .is_some_and(|b| sha256(&b) == views.journal_sha256)
        && fs::read(manifest)
            .ok()
            .is_some_and(|b| sha256(&b) == views.manifest_sha256))
}

pub(crate) struct Writer {
    root: PathBuf,
    log: File,
    tip_path: PathBuf,
    views_path: PathBuf,
    state: Snapshot,
    tip: Tip,
    poisoned: bool,
    #[cfg(test)]
    fail: Option<&'static str>,
}
impl Writer {
    pub(crate) fn open(
        root: &Path,
        journal: &RunJournal,
        manifest: &Manifest,
    ) -> Result<Self, Diagnostic> {
        let (log_path, tip_path, views_path) = paths(root, &journal.run_id)?;
        fs::create_dir_all(directory(root, &journal.run_id)?).map_err(|_| io_error())?;
        let recovered = match replay(root, &journal.run_id)? {
            Some(state) => state,
            None => {
                let snapshot = Snapshot {
                    journal: journal.clone(),
                    manifest: manifest.clone(),
                };
                snapshot.validate()?;
                let (bytes, digest) = frame(&Envelope {
                    format: FORMAT,
                    sequence: 1,
                    previous_sha256: ZERO.into(),
                    record: Record::Base {
                        snapshot: snapshot.clone(),
                    },
                })?;
                let tip = Tip {
                    format: FORMAT,
                    run_id: journal.run_id.clone(),
                    sequence: 1,
                    end: bytes.len() as u64,
                    sha256: digest,
                };
                // Publish the initial log and acknowledgement together. A crash
                // before this directory rename leaves the legacy checkpoint
                // authoritative and an isolated staging directory to inspect.
                let run_dir = directory(root, &journal.run_id)?;
                let destination = log_path.parent().ok_or_else(invalid)?;
                if destination.exists() {
                    return Err(invalid());
                }
                let staged = tempfile::Builder::new()
                    .prefix(".checkpoint-v2-stage-")
                    .tempdir_in(&run_dir)
                    .map_err(|_| io_error())?;
                atomic_write(&staged.path().join("state.log"), &bytes)?;
                atomic_write(&staged.path().join("tip.json"), &encoded(&tip)?)?;
                let staged = staged.keep();
                fs::rename(&staged, destination).map_err(|_| io_error())?;
                #[cfg(not(target_os = "windows"))]
                File::open(&run_dir)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| io_error())?;
                Replayed {
                    snapshot,
                    tip,
                    trailing: 0,
                }
            }
        };
        if !recovered.snapshot.same_scope(journal) {
            return Err(invalid());
        }
        let mut log = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&log_path)
            .map_err(|_| io_error())?;
        if recovered.trailing != 0 {
            // Only bytes beyond the independently synchronized acknowledgement tip
            // are uncommitted. Preserve them before removing them from this stream.
            log.seek(SeekFrom::Start(recovered.tip.end))
                .map_err(|_| io_error())?;
            let mut abandoned = Vec::new();
            log.read_to_end(&mut abandoned).map_err(|_| io_error())?;
            let tail = confined(
                root,
                &format!(
                    "Herbert Data/runs/{}/uncommitted-{}.bin",
                    journal.run_id,
                    sha256(&abandoned)
                ),
            )?;
            atomic_write(&tail, &abandoned)?;
            log.set_len(recovered.tip.end)
                .and_then(|_| log.sync_all())
                .map_err(|_| io_error())?;
        }
        log.seek(SeekFrom::End(0)).map_err(|_| io_error())?;
        Ok(Self {
            root: root.to_path_buf(),
            log,
            tip_path,
            views_path,
            state: recovered.snapshot,
            tip: recovered.tip,
            poisoned: false,
            #[cfg(test)]
            fail: None,
        })
    }
    fn point(&mut self, _name: &'static str) -> Result<(), Diagnostic> {
        #[cfg(test)]
        if self.fail == Some(_name) {
            self.fail = None;
            return Err(io_error());
        }
        Ok(())
    }
    pub(crate) fn checkpoint(
        &mut self,
        journal: &RunJournal,
        manifest: &Manifest,
    ) -> Result<(), Diagnostic> {
        if self.poisoned {
            return Err(io_error());
        }
        if !self.state.same_scope(journal) {
            return Err(invalid());
        }
        let changes = Changes {
            journal: JournalDelta::between(&self.state.journal, journal),
            manifest: ManifestDelta::between(&self.state.manifest, manifest),
        };
        if changes.journal.is_empty() && changes.manifest.is_empty() {
            return Ok(());
        }
        // Validate proposed scope/format before bytes are committed, without
        // cloning the growing state. The only fields checked here are fixed scope.
        if manifest.checkpoint.as_ref() != Some(&Marker::new(&journal.run_id))
            || manifest.schema_version != 1
            || journal.schema_version != 1
            || manifest.endpoint != journal.endpoint
            || manifest.company != journal.company
            || manifest.caller != journal.caller
        {
            return Err(invalid());
        }
        let sequence = self.tip.sequence.checked_add(1).ok_or_else(invalid)?;
        let (bytes, digest) = frame(&Envelope {
            format: FORMAT,
            sequence,
            previous_sha256: self.tip.sha256.clone(),
            record: Record::Changes {
                changes: changes.clone(),
            },
        })?;
        let tip = Tip {
            format: FORMAT,
            run_id: journal.run_id.clone(),
            sequence,
            end: self
                .tip
                .end
                .checked_add(bytes.len() as u64)
                .ok_or_else(invalid)?,
            sha256: digest,
        };
        self.poisoned = true;
        self.log
            .write_all(&bytes)
            .and_then(|_| self.log.sync_all())
            .map_err(|_| io_error())?;
        self.point("afterLogSync")?;
        atomic_write(&self.tip_path, &encoded(&tip)?)?;
        self.point("afterTipCommit")?;
        changes.journal.apply(&mut self.state.journal)?;
        changes.manifest.apply(&mut self.state.manifest)?;
        self.tip = tip;
        self.poisoned = false;
        Ok(())
    }
    pub(crate) fn phase(&self) -> &str {
        &self.state.journal.phase
    }
    pub(crate) fn materialize(&mut self) -> Result<(), Diagnostic> {
        if self.poisoned {
            return Err(io_error());
        }
        let journal = serde_json::to_vec_pretty(&self.state.journal).map_err(|_| invalid())?;
        let manifest = serde_json::to_vec_pretty(&self.state.manifest).map_err(|_| invalid())?;
        let path = confined(
            &self.root,
            &format!("Herbert Data/runs/{}/journal.json", self.tip.run_id),
        )?;
        atomic_write(&path, &journal)?;
        self.point("afterJournalView")?;
        let run_manifest = confined(
            &self.root,
            &format!("Herbert Data/runs/{}/manifest.json", self.tip.run_id),
        )?;
        atomic_write(&run_manifest, &manifest)?;
        atomic_write(
            &confined(&self.root, "Herbert Data/manifest.json")?,
            &manifest,
        )?;
        // Older downloads retain their existing manifest entry point. New downloads expose no technical manifest at the root.
        if self.root.join("manifest.json").exists() {
            atomic_write(&confined(&self.root, "manifest.json")?, &manifest)?;
        }
        self.point("afterManifestView")?;
        let views = Views {
            format: FORMAT,
            sequence: self.tip.sequence,
            sha256: self.tip.sha256.clone(),
            journal_sha256: sha256(&journal),
            manifest_sha256: sha256(&manifest),
        };
        atomic_write(&self.views_path, &encoded(&views)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use herbert_contract::Completeness;
    use std::time::Instant;

    fn journal() -> RunJournal {
        serde_json::from_value(serde_json::json!({
            "schemaVersion":1,"runId":"a016b898-83c7-42a5-8566-a3d4d21b0db6",
            "requestId":"checkpoint-fixture","endpoint":"https://fixture.invalid/",
            "company":"fixture","caller":"fixture","scope":"fullBackup","categories":["C02"],
            "pluginHashes":{},"settings":OperatingSettings::default(),"phase":"acquiring",
            "artifacts":[],"diagnostics":[],"pendingFiles":[],"cursors":{}
        }))
        .unwrap()
    }
    fn manifest(journal: &RunJournal) -> Manifest {
        Manifest {
            schema_version: 1,
            endpoint: journal.endpoint.clone(),
            company: journal.company.clone(),
            caller: journal.caller.clone(),
            references: vec![],
            artifacts: BTreeMap::new(),
            retained_files: BTreeMap::new(),
            checkpoint: Some(Marker::new(&journal.run_id)),
        }
    }
    fn artifact(index: usize) -> Artifact {
        Artifact {
            identity: Identity {
                category: "C02".into(),
                kind: "fixture".into(),
                scope: herbert_contract::Scope::Company {
                    company: "fixture".into(),
                },
                keys: BTreeMap::from([("id".into(), format!("fixture-{index:06}"))]),
            },
            classification: "fixture".into(),
            acquisition: Completeness::Collected,
            integrity: "verified".into(),
            processing: "derived".into(),
            files: vec![],
            dependencies: vec![],
            diagnostics: vec![],
            source_consistency: "nonAtomicUnknown".into(),
        }
    }
    fn fixture() -> (tempfile::TempDir, RunJournal, Manifest, Writer) {
        let dir = tempfile::tempdir().unwrap();
        let j = journal();
        let m = manifest(&j);
        let mut writer = Writer::open(dir.path(), &j, &m).unwrap();
        writer.materialize().unwrap();
        (dir, j, m, writer)
    }
    fn add(j: &mut RunJournal, m: &mut Manifest, index: usize) {
        let a = artifact(index);
        m.artifacts
            .insert(crate::storage::identity_key(&a.identity), a.clone());
        j.artifacts.push(a);
    }
    fn loaded(root: &Path, id: &str) -> Snapshot {
        replay(root, id).unwrap().unwrap().snapshot
    }

    #[test]
    fn sequence_and_mapping_changes_preserve_replacements_removals_and_suffixes() {
        for (before, after) in [
            (vec![1, 2, 3], vec![1, 9, 3]),
            (vec![1, 2, 3], vec![2, 3, 1]),
            (vec![1, 2, 3], vec![1, 2]),
            (vec![1], vec![1, 2, 3]),
            (vec![1, 2], vec![]),
        ] {
            let mut actual = before.clone();
            Sequence::between(&before, &after)
                .unwrap()
                .apply(&mut actual)
                .unwrap();
            assert_eq!(actual, after);
        }
        let before = BTreeMap::from([("a".into(), 1), ("b".into(), 2)]);
        let after = BTreeMap::from([("b".into(), 3), ("c".into(), 4)]);
        let mut actual = before.clone();
        Mapping::between(&before, &after)
            .unwrap()
            .apply(&mut actual)
            .unwrap();
        assert_eq!(actual, after);
        assert!(Sequence::<i32>::between(&vec![1], &vec![1]).is_none());
    }

    #[test]
    fn accepted_run_recovery_reads_legacy_then_prefers_committed_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let j = journal();
        let run_dir = directory(dir.path(), &j.run_id).unwrap();
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("journal.json"), encoded(&j).unwrap()).unwrap();
        let record = crate::recovery::RunRecord {
            schema_version: 1,
            scope_hash: "fixture".into(),
            destination: dir.path().to_string_lossy().into_owned(),
            accepted: true,
            full: true,
            page_size: None,
            journal: j.clone(),
        };
        assert_eq!(
            crate::recovery::read_journal(&record)
                .unwrap()
                .artifacts
                .len(),
            0
        );
        let mut storage = Storage::open(dir.path(), false, j).unwrap();
        storage.record_artifact(artifact(1)).unwrap();
        assert_eq!(
            crate::recovery::read_journal(&record)
                .unwrap()
                .artifacts
                .len(),
            1
        );
        let mut wrong = record;
        wrong.journal.company = "other".into();
        assert_eq!(
            crate::recovery::read_journal(&wrong).unwrap_err().code,
            "RUN_SCOPE_MISMATCH"
        );
    }

    #[test]
    fn commit_replays_both_structures_without_materializing_growing_views() {
        let (dir, mut j, mut m, mut writer) = fixture();
        let journal_path = dir
            .path()
            .join(format!("Herbert Data/runs/{}/journal.json", j.run_id));
        let before = fs::read(&journal_path).unwrap();
        add(&mut j, &mut m, 1);
        j.discovered
            .insert("C02".into(), vec![j.artifacts[0].identity.clone()]);
        j.cursors.insert("C02".into(), "next-page".into());
        j.discovery_finished.insert("C02".into(), false);
        writer.checkpoint(&j, &m).unwrap();
        assert!(
            loaded(dir.path(), &j.run_id)
                == Snapshot {
                    journal: j.clone(),
                    manifest: m.clone()
                }
        );
        assert_eq!(fs::read(journal_path).unwrap(), before);
        let tip_before = writer.tip.end;
        writer.checkpoint(&j, &m).unwrap();
        assert_eq!(
            writer.tip.end, tip_before,
            "No-op checkpoints must not append"
        );
        writer.materialize().unwrap();
        assert!(views_current(dir.path(), &j.run_id, &writer.tip).unwrap());
    }

    #[test]
    fn every_truncation_of_an_acknowledged_frame_is_refused() {
        let (dir, mut j, mut m, mut writer) = fixture();
        let start = writer.tip.end;
        add(&mut j, &mut m, 1);
        writer.checkpoint(&j, &m).unwrap();
        let (log, _, _) = paths(dir.path(), &j.run_id).unwrap();
        let bytes = fs::read(&log).unwrap();
        drop(writer);
        for end in start..bytes.len() as u64 {
            fs::write(&log, &bytes[..end as usize]).unwrap();
            assert!(
                replay(dir.path(), &j.run_id).is_err(),
                "accepted committed truncation at {end}"
            );
        }
        fs::write(&log, &bytes).unwrap();
        assert_eq!(loaded(dir.path(), &j.run_id).journal.artifacts.len(), 1);
    }

    #[test]
    fn unacknowledged_tail_is_ignored_preserved_then_reconciled_before_append() {
        let (dir, mut j, mut m, mut writer) = fixture();
        let old = writer.state.clone();
        let end = writer.tip.end;
        add(&mut j, &mut m, 1);
        writer.fail = Some("afterLogSync");
        assert!(writer.checkpoint(&j, &m).is_err());
        assert!(
            writer.checkpoint(&j, &m).is_err(),
            "uncertain writer must remain poisoned"
        );
        let (log, _, _) = paths(dir.path(), &j.run_id).unwrap();
        let bytes = fs::read(&log).unwrap();
        drop(writer);
        for cut in end..=bytes.len() as u64 {
            fs::write(&log, &bytes[..cut as usize]).unwrap();
            assert!(loaded(dir.path(), &j.run_id) == old);
        }
        fs::write(&log, &bytes).unwrap();
        let mut reopened = Writer::open(dir.path(), &old.journal, &old.manifest).unwrap();
        assert_eq!(fs::metadata(&log).unwrap().len(), end);
        let preserved = directory(dir.path(), &j.run_id).unwrap().join(format!(
            "uncommitted-{}.bin",
            sha256(&bytes[end as usize..])
        ));
        assert_eq!(fs::read(preserved).unwrap(), bytes[end as usize..]);
        reopened.checkpoint(&j, &m).unwrap();
        assert_eq!(loaded(dir.path(), &j.run_id).journal.artifacts.len(), 1);
    }

    #[test]
    fn lost_acknowledgement_after_tip_commit_recovers_the_committed_state() {
        let (dir, mut j, mut m, mut writer) = fixture();
        add(&mut j, &mut m, 1);
        writer.fail = Some("afterTipCommit");
        assert!(writer.checkpoint(&j, &m).is_err());
        assert!(writer.poisoned);
        assert!(
            loaded(dir.path(), &j.run_id)
                == Snapshot {
                    journal: j.clone(),
                    manifest: m.clone()
                }
        );
        drop(writer);
        let reopened = Writer::open(dir.path(), &j, &m).unwrap();
        assert_eq!(reopened.state.journal.artifacts.len(), 1);
    }

    #[test]
    fn header_length_payload_and_tip_tampering_are_refused() {
        let (dir, mut j, mut m, mut writer) = fixture();
        let start = writer.tip.end as usize;
        add(&mut j, &mut m, 1);
        writer.checkpoint(&j, &m).unwrap();
        let (log, tip_path, _) = paths(dir.path(), &j.run_id).unwrap();
        let bytes = fs::read(&log).unwrap();
        let tip = fs::read(&tip_path).unwrap();
        drop(writer);
        for at in [
            0,
            8,
            48,
            start,
            start + 8,
            start + 48,
            start + HEADER,
            bytes.len() - 1,
        ] {
            let mut altered = bytes.clone();
            altered[at] ^= 1;
            fs::write(&log, altered).unwrap();
            assert!(
                replay(dir.path(), &j.run_id).is_err(),
                "accepted tamper at {at}"
            );
        }
        fs::write(&log, &bytes).unwrap();
        let mut altered: Tip = serde_json::from_slice(&tip).unwrap();
        altered.sequence += 1;
        fs::write(&tip_path, encoded(&altered).unwrap()).unwrap();
        assert!(replay(dir.path(), &j.run_id).is_err());
        fs::write(&tip_path, tip).unwrap();
        assert!(replay(dir.path(), &j.run_id).unwrap().is_some());
    }

    #[test]
    fn server_change_authority_survives_incremental_replay_and_legacy_absence() {
        let (dir,mut journal,manifest,mut writer)=fixture();
        let legacy=serde_json::to_value(&journal).unwrap();assert!(legacy.get("serverChangeAuthority").is_none());assert!(serde_json::from_value::<RunJournal>(legacy).unwrap().server_change_authority.is_none());
        let scope=crate::server_changes::MaterialScope{endpoint:journal.endpoint.clone(),company:journal.company.clone(),principal:journal.caller.clone(),capabilities:BTreeMap::new()};
        journal.server_change_authority=Some(crate::server_changes::RunAuthority{grant_id:uuid::Uuid::new_v4().to_string(),material_digest:scope.digest(),material_scope:scope,request_id:journal.request_id.clone(),original_session:"verified-host-session".into(),workflow_id:Some("explicit-workflow".into())});
        writer.checkpoint(&journal,&manifest).unwrap();
        journal.server_change_authority_history.push(journal.server_change_authority.take().unwrap());
        writer.checkpoint(&journal,&manifest).unwrap();drop(writer);
        let restored=loaded(dir.path(),&journal.run_id).journal;assert_eq!(restored.server_change_authority,journal.server_change_authority);assert_eq!(restored.server_change_authority_history,journal.server_change_authority_history);
    }

    #[test]
    fn host_transfer_and_method_survive_incremental_replay() {
        let (dir, mut journal, manifest, mut writer) = fixture();
        journal.collection_method = herbert_contract::CollectionMethod::DirectOnly;
        journal.host_transfers.push(
            serde_json::from_value(serde_json::json!({
                "operationId":"d37ed51b-378e-4701-888d-77dfb386db17",
                "plugin":"fixture","pluginHash":"exact-plugin","identity":null,
                "phase":"ready","chunks":[{"path":"chunk.bin","bytes":3,
                    "sha256":sha256(b"abc"),"role":"native"}],
                "archive":null,"manifest":{"sha256":"server-archive"}
            }))
            .unwrap(),
        );
        writer.checkpoint(&journal, &manifest).unwrap();
        drop(writer);
        let restored = loaded(dir.path(), &journal.run_id).journal;
        assert_eq!(restored.collection_method, journal.collection_method);
        assert_eq!(restored.host_transfers, journal.host_transfers);
    }

    #[test]
    fn scope_changes_and_invalid_sequence_patch_never_become_committed() {
        let (dir, mut j, m, mut writer) = fixture();
        let before = writer.tip.end;
        j.company = "other".into();
        assert!(writer.checkpoint(&j, &m).is_err());
        assert_eq!(writer.tip.end, before);
        assert_eq!(
            loaded(dir.path(), &m.checkpoint.as_ref().unwrap().run_id)
                .journal
                .company,
            "fixture"
        );
        let bad = Sequence {
            before_len: 5,
            start: 0,
            remove: 1,
            insert: vec![2],
        };
        assert!(bad.apply(&mut vec![1]).is_err());
    }

    #[test]
    fn finalization_requires_both_views_and_the_matching_seal() {
        let (dir, mut j, mut m, mut writer) = fixture();
        add(&mut j, &mut m, 1);
        j.phase = "completed".into();
        writer.checkpoint(&j, &m).unwrap();
        assert_eq!(
            read_journal(dir.path(), &j.run_id).unwrap().unwrap().phase,
            "destinationReleaseUncertain"
        );
        writer.fail = Some("afterJournalView");
        assert!(writer.materialize().is_err());
        let recovered = read_journal(dir.path(), &j.run_id).unwrap().unwrap();
        assert_eq!(recovered.phase, "destinationReleaseUncertain");
        assert_eq!(recovered.artifacts.len(), 1);
        writer.materialize().unwrap();
        assert_eq!(
            read_journal(dir.path(), &j.run_id).unwrap().unwrap().phase,
            "completed"
        );
        fs::write(
            dir.path()
                .join(format!("Herbert Data/runs/{}/manifest.json", j.run_id)),
            b"tampered view",
        )
        .unwrap();
        assert_eq!(
            read_journal(dir.path(), &j.run_id).unwrap().unwrap().phase,
            "destinationReleaseUncertain"
        );
        writer.materialize().unwrap();
        assert_eq!(
            read_journal(dir.path(), &j.run_id).unwrap().unwrap().phase,
            "completed"
        );
    }

    #[test]
    fn legacy_manifest_migrates_and_refresh_uses_unmaterialized_committed_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let j = journal();
        let mut legacy = manifest(&j);
        legacy.checkpoint = None;
        let old = artifact(0);
        legacy
            .artifacts
            .insert(crate::storage::identity_key(&old.identity), old);
        fs::write(dir.path().join("manifest.json"), encoded(&legacy).unwrap()).unwrap();
        assert!(read_journal(dir.path(), &j.run_id).unwrap().is_none());
        {
            let mut storage = Storage::open(dir.path(), false, j.clone()).unwrap();
            assert_eq!(storage.manifest.artifacts.len(), 1);
            storage.record_artifact(artifact(1)).unwrap();
            // Simulate process exit before public view materialization.
        }
        let mut next = j.clone();
        next.run_id = "0d9585d7-1b0d-4eec-87a7-3ec7ffb21e28".into();
        next.request_id = "refresh".into();
        let mut storage = Storage::open(dir.path(), false, next).unwrap();
        assert_eq!(storage.manifest.artifacts.len(), 2);
        storage.release_destination().unwrap();
        let public: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join("manifest.json")).unwrap()).unwrap();
        assert_eq!(public.artifacts.len(), 2);
        assert_eq!(public.checkpoint.as_ref().unwrap().format, FORMAT);
    }

    #[test]
    fn later_refresh_does_not_invalidate_the_prior_completed_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut j = journal();
        j.phase = "completed".into();
        {
            let mut storage = Storage::open(dir.path(), false, j.clone()).unwrap();
            storage.record_artifact(artifact(0)).unwrap();
            storage.release_destination().unwrap();
        }
        assert_eq!(
            read_journal(dir.path(), &j.run_id).unwrap().unwrap().phase,
            "completed"
        );
        let mut next = journal();
        next.run_id = "0d9585d7-1b0d-4eec-87a7-3ec7ffb21e28".into();
        next.request_id = "refresh".into();
        let mut storage = Storage::open(dir.path(), false, next).unwrap();
        storage.record_artifact(artifact(1)).unwrap();
        storage.release_destination().unwrap();
        assert_eq!(
            read_journal(dir.path(), &j.run_id).unwrap().unwrap().phase,
            "completed"
        );
    }

    #[test]
    fn operational_intents_and_all_collection_shapes_replay_without_native_details() {
        let (dir, mut j, mut m, mut writer) = fixture();
        let id = artifact(0).identity;
        let claim: herbert_contract::HelperContract = serde_json::from_value(serde_json::json!({
            "libraryId":"fixture","globalId":"1d90be3e-1948-43e5-97bb-063eb48da6d3",
            "companyArgument":"Company","functions":[]
        }))
        .unwrap();
        j.helper_transfers.push(crate::helpers::HelperTransfer {
            operation_id: "owned-operation".into(),
            identity: id.clone(),
            claim: claim.clone(),
            function: "Get".into(),
            close_arguments: serde_json::json!({"OperationId":"owned-operation"}),
            phase: "open".into(),
            plugin_hash: "fixture".into(),
            dispatch_count: 1,
        });
        j.helper_libraries
            .push(crate::helper_libraries::LibraryLease {
                schema_version: 1,
                holder: "fixture-holder".into(),
                endpoint: j.endpoint.clone(),
                company: j.company.clone(),
                caller: j.caller.clone(),
                template_id: "fixture".into(),
                plugin_hash: "fixture".into(),
                instance: claim,
                owner_marker: "fixture-owner".into(),
                phase: "installPending".into(),
                transitions: vec!["reserved".into()],
                package_sha256: None,
                frozen_package: None,
            });
        j.access_leases.push(crate::access::AccessLease {
            schema_version: 1,
            lease_id: "fixture-lease".into(),
            endpoint: j.endpoint.clone(),
            company: j.company.clone(),
            caller: j.caller.clone(),
            holder: "fixture".into(),
            artifact: id.clone(),
            group: "owned-group".into(),
            native_policy: "fixture".into(),
            state: "grantPending".into(),
            existed_before: false,
            membership_added: false,
            diagnostics: vec![],
        });
        j.temporary_access_enabled = true;
        j.native_artifacts
            .push(crate::native_artifact_store::OperationRecord {
                schema_version: 1,
                run_id: j.run_id.clone(),
                plugin: "fixture".into(),
                scope: crate::native_artifact_store::ArtifactScope {
                    endpoint: j.endpoint.clone(),
                    company: j.company.clone(),
                    caller: j.caller.clone(),
                },
                artifacts: vec![],
            });
        j.shared_exports.insert(
            "shared-native-file".into(),
            vec![herbert_contract::FileRef {
                path: "native.zip".into(),
                sha256: "fixture".into(),
                bytes: 7,
                role: "derived".into(),
            }],
        );
        j.plugin_hashes.insert("C02".into(), "fixture-hash".into());
        j.plugin_settings
            .insert("C02".into(), serde_json::json!({"enabled":true}));
        j.plugin_selection_contracts.insert("C02".into(), serde_json::json!([
            {"kind":"filter","key":"prefix","description":"Literal matching contract fixture-v1"}
        ]));
        j.discovered.insert("C02".into(), vec![id.clone()]);
        j.targets.push(id.clone());
        j.references
            .push(serde_json::json!({"source":id,"reference":"fixture"}));
        m.references = j.references.clone();
        j.category_outcomes
            .insert("C02".into(), "discovered".into());
        j.discovery_finished.insert("C02".into(), true);
        j.settings.concurrency = 2;
        let mut diagnostic = error("FIXTURE", "Safe public explanation");
        diagnostic.native_details = Some(serde_json::json!({"secret":"must-not-persist-fixture"}));
        j.diagnostics.push(diagnostic);
        writer.checkpoint(&j, &m).unwrap();
        let restored = loaded(dir.path(), &j.run_id);
        assert_eq!(encoded(&restored.journal).unwrap(), encoded(&j).unwrap());
        assert!(restored.journal.diagnostics[0].native_details.is_none());
        assert!(
            !String::from_utf8_lossy(&fs::read(paths(dir.path(), &j.run_id).unwrap().0).unwrap())
                .contains("must-not-persist-fixture")
        );
        j.helper_transfers[0].phase = "closeUncertain".into();
        j.helper_transfers[0].dispatch_count = 2;
        j.access_leases[0].state = "uncertain".into();
        j.helper_libraries[0]
            .transitions
            .push("installUncertain".into());
        j.references.clear();
        m.references.clear();
        j.targets.clear();
        j.native_artifacts[0].plugin = "updated-fixture".into();
        j.shared_exports.clear();
        writer.checkpoint(&j, &m).unwrap();
        assert_eq!(
            encoded(&loaded(dir.path(), &j.run_id).journal).unwrap(),
            encoded(&j).unwrap()
        );
    }

    #[test]
    fn malformed_owner_and_missing_commit_tip_fail_closed() {
        let (dir, j, _, writer) = fixture();
        assert!(read_journal(dir.path(), "../../another-run").is_err());
        let (_, tip, _) = paths(dir.path(), &j.run_id).unwrap();
        drop(writer);
        fs::remove_file(tip).unwrap();
        assert!(read_journal(dir.path(), &j.run_id).is_err());
    }

    #[test]
    fn interrupted_initial_staging_does_not_replace_legacy_or_publish_half_a_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let j = journal();
        let run_dir = directory(dir.path(), &j.run_id).unwrap();
        fs::create_dir_all(&run_dir).unwrap();
        let legacy = encoded(&j).unwrap();
        fs::write(run_dir.join("journal.json"), &legacy).unwrap();
        let staged = run_dir.join(".checkpoint-v2-stage-interrupted");
        fs::create_dir(&staged).unwrap();
        fs::write(
            staged.join("state.log"),
            b"incomplete uncommitted initialization",
        )
        .unwrap();
        assert!(read_journal(dir.path(), &j.run_id).unwrap().is_none());
        assert_eq!(fs::read(run_dir.join("journal.json")).unwrap(), legacy);
        let m = manifest(&j);
        let writer = Writer::open(dir.path(), &j, &m).unwrap();
        assert_eq!(writer.tip.sequence, 1);
        assert!(
            staged.join("state.log").is_file(),
            "abandoned initialization evidence preserved"
        );
        let (log, tip, _) = paths(dir.path(), &j.run_id).unwrap();
        assert!(log.is_file() && tip.is_file());
    }

    #[test]
    fn abrupt_exit_child_fixture() {
        let Some(root) = std::env::var_os("HERBERT_CHECKPOINT_TEST_ROOT") else {
            return;
        };
        let stage = std::env::var("HERBERT_CHECKPOINT_TEST_STAGE").unwrap();
        let root = PathBuf::from(root);
        let mut storage = Storage::open(&root, false, journal()).unwrap();
        let bytes = b"owned abrupt-exit native content";
        let digest = sha256(bytes);
        let pending = FileRef {
            path: format!("Herbert Data/raw/{}/{}.json", &digest[..2], digest),
            sha256: digest,
            bytes: bytes.len() as u64,
            role: "native".into(),
        };
        storage.journal.pending_files.push(pending.clone());
        storage.checkpoint().unwrap();
        if stage != "intent" {
            atomic_write(&storage.confined(&pending.path).unwrap(), bytes).unwrap();
        }
        if stage == "committed" {
            storage
                .preserve_for(Some(&artifact(0).identity), bytes, "json")
                .unwrap();
        }
        // Explicitly bypass Storage/Writer destructors and all normal finish
        // paths, without producing a Windows crash-report UI.
        std::process::exit(73);
    }

    #[test]
    fn process_exit_at_intent_content_and_ownership_boundaries_recovers_exact_state() {
        for stage in ["intent", "content", "committed"] {
            let dir = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "checkpoint_log::tests::abrupt_exit_child_fixture",
                    "--nocapture",
                ])
                .env("HERBERT_CHECKPOINT_TEST_ROOT", dir.path())
                .env("HERBERT_CHECKPOINT_TEST_STAGE", stage)
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(73),
                "child: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let restored = read_journal(dir.path(), &journal().run_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                restored.pending_files.len(),
                usize::from(stage != "committed")
            );
            assert_eq!(
                restored.acquisitions.len(),
                usize::from(stage == "committed")
            );
            let reference = restored
                .pending_files
                .first()
                .or(restored.acquisitions.first())
                .unwrap();
            let content = fs::read(dir.path().join(&reference.path));
            if stage == "intent" {
                assert_eq!(content.unwrap_err().kind(), std::io::ErrorKind::NotFound);
            } else {
                assert_eq!(sha256(&content.unwrap()), reference.sha256);
            }
            if stage == "committed" {
                assert!(
                    restored.artifact_acquisitions
                        [&crate::storage::identity_key(&artifact(0).identity)]
                        .contains(reference)
                );
            }
        }
    }

    #[test]
    fn immutable_intent_survives_without_file_and_owned_completion_survives_without_views() {
        let dir = tempfile::tempdir().unwrap();
        let j = journal();
        let mut storage = Storage::open(dir.path(), false, j.clone()).unwrap();
        let bytes = b"exact preserved native bytes";
        let digest = sha256(bytes);
        let pending = FileRef {
            path: format!("Herbert Data/raw/{}/{}.json", &digest[..2], digest),
            sha256: digest,
            bytes: bytes.len() as u64,
            role: "native".into(),
        };
        storage.journal.pending_files.push(pending.clone());
        storage.checkpoint().unwrap();
        let restored = read_journal(dir.path(), &j.run_id).unwrap().unwrap();
        assert_eq!(restored.pending_files, vec![pending]);
        let native = storage
            .preserve_for(Some(&artifact(0).identity), bytes, "json")
            .unwrap();
        drop(storage);
        let restored = read_journal(dir.path(), &j.run_id).unwrap().unwrap();
        assert!(restored.pending_files.is_empty());
        assert!(restored.acquisitions.contains(&native));
        assert!(
            restored.artifact_acquisitions[&crate::storage::identity_key(&artifact(0).identity)]
                .contains(&native)
        );
        assert_eq!(fs::read(dir.path().join(&native.path)).unwrap(), bytes);
    }

    #[test]
    fn append_bytes_scale_with_new_records_not_prior_history() {
        let (dir, mut j, mut m, mut writer) = fixture();
        let base = writer.tip.end;
        let mut halfway = 0;
        for n in 0..512 {
            add(&mut j, &mut m, n);
            writer.checkpoint(&j, &m).unwrap();
            if n == 255 {
                halfway = writer.tip.end;
            }
        }
        let first = halfway - base;
        let second = writer.tip.end - halfway;
        // Same-size fixture records, with only sequence/end digit growth.
        // This is a test of emitted representation, never a supported-count cap.
        assert!(
            second - first < 256 * 16,
            "later transactions repeated prior records"
        );
        assert!(
            loaded(dir.path(), &j.run_id)
                == Snapshot {
                    journal: j,
                    manifest: m
                }
        );
    }

    #[test]
    #[ignore = "Explicit measured workload from the native 5192-root run; not a product limit"]
    fn measured_5192_artifact_checkpoints() {
        let (dir, mut j, mut m, mut writer) = fixture();
        let start = Instant::now();
        for n in 0..5192 {
            add(&mut j, &mut m, n);
            writer.checkpoint(&j, &m).unwrap();
        }
        let commit_seconds = start.elapsed().as_secs_f64();
        let before = Instant::now();
        writer.materialize().unwrap();
        let materialize_seconds = before.elapsed().as_secs_f64();
        let before = Instant::now();
        let recovered = loaded(dir.path(), &j.run_id);
        let replay_seconds = before.elapsed().as_secs_f64();
        assert!(
            recovered
                == Snapshot {
                    journal: j,
                    manifest: m
                }
        );
        println!(
            "{}",
            serde_json::json!({"artifacts":5192,"commitSeconds":commit_seconds,
            "logBytes":writer.tip.end,"materializeSeconds":materialize_seconds,
            "replaySeconds":replay_seconds,"transactions":writer.tip.sequence})
        );
    }
}

#[cfg(test)]
mod reference_delta_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn reference_growth_saves_only_new_occurrences_and_replays_exactly() {
        let before=(0..100).map(|i|json!({"identity":i,"occurrences":(0..100).map(|n|json!({"source":n,"path":"retained source reference"})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let mut after = before.clone();
        after[0]["occurrences"]
            .as_array_mut()
            .unwrap()
            .push(json!({"source":101}));
        after[99]["occurrences"]
            .as_array_mut()
            .unwrap()
            .push(json!({"source":101}));
        after.push(json!({"identity":100,"occurrences":[{"source":101}]}));
        let patch = ReferenceChanges::between(&before, &after).unwrap();
        let bytes = encoded(&patch).unwrap();
        assert!(
            bytes.len() < 1000,
            "Only the three new occurrences and their positions should be stored"
        );
        let mut replay = before;
        serde_json::from_slice::<ReferenceChanges>(&bytes)
            .unwrap()
            .apply(&mut replay)
            .unwrap();
        assert_eq!(replay, after);
        assert!(ReferenceChanges::between(&after, &after).is_none());
    }
    #[test]
    fn reference_deletion_and_legacy_logs_remain_readable_and_bad_counts_fail() {
        let before = vec![
            json!({"identity":"a","occurrences":[1,2]}),
            json!({"identity":"b","occurrences":[3]}),
        ];
        let after = vec![before[1].clone()];
        let legacy = encoded(&Sequence::between(&before, &after).unwrap()).unwrap();
        let mut replay = before.clone();
        serde_json::from_slice::<ReferenceChanges>(&legacy)
            .unwrap()
            .apply(&mut replay)
            .unwrap();
        assert_eq!(replay, after);
        let mut replay = before.clone();
        ReferenceChanges::between(&before, &after)
            .unwrap()
            .apply(&mut replay)
            .unwrap();
        assert_eq!(replay, after);
        let bad = ReferenceChanges::Incremental(ReferenceAppend {
            reference_count: 3,
            occurrences: BTreeMap::new(),
            append: vec![],
        });
        assert!(bad.apply(&mut replay).is_err());
    }
}
