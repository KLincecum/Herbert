//! Owned backup files, atomic checkpoints and preserved user edits.
//! Keep original acquisitions separate from the source views derived from them.
use fs2::FileExt;
use herbert_contract::{Artifact, Diagnostic, FileRef, Identity, OperatingSettings};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn identity_key(identity: &Identity) -> String {
    sha256(&serde_json::to_vec(identity).expect("Identity serialization is infallible"))
}
fn io_error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Diagnostic> {
    let parent = path
        .parent()
        .ok_or_else(|| io_error("INVALID_PATH", "Output needs a parent directory."))?;
    fs::create_dir_all(parent)
        .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot create the output directory."))?;
    // tempfile uses native Windows file APIs when it persists a file. Give
    // those APIs the extended absolute path returned by canonicalize so valid
    // deep output/state folders do not inherit the legacy MAX_PATH limit.
    #[cfg(target_os = "windows")]
    let parent = fs::canonicalize(parent)
        .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot resolve the output directory."))?;
    #[cfg(target_os = "windows")]
    let destination = parent.join(
        path.file_name()
            .ok_or_else(|| io_error("INVALID_PATH", "Output needs a filename."))?,
    );
    #[cfg(target_os = "windows")]
    let path = destination.as_path();
    let mut tmp = tempfile::NamedTempFile::new_in(&parent).map_err(|_| {
        io_error(
            "OUTPUT_UNAVAILABLE",
            "Cannot create a temporary output file.",
        )
    })?;
    tmp.write_all(bytes)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|_| {
            io_error(
                "OUTPUT_WRITE_FAILED",
                "Cannot write and synchronize output; temporary work remains recoverable.",
            )
        })?;
    // Close the writer before replacing the destination. No caller needs the
    // temporary file handle after its bytes have been synchronized.
    commit_temporary(tmp, path)?;
    #[cfg(not(target_os = "windows"))]
    File::open(parent).and_then(|f| f.sync_all()).map_err(|_| {
        io_error(
            "OUTPUT_COMMIT_UNCERTAIN",
            "File was committed but its directory synchronization failed; inspect it before retry.",
        )
    })?;
    Ok(())
}

/// Keep the complete candidate if either native persistence step fails.
/// `persist` returns an owned TempPath on failure. Dropping that error deletes
/// the only recoverable candidate. Separate keep/rename also identifies which
/// Windows operation failed, without a retry or a permission change.
/// Rust's Windows rename supports a destination held by a delete-shared reader.
/// Tempfile 3.27's MoveFileEx-only persist returns OS error 5 in that case.
fn commit_temporary(tmp: tempfile::NamedTempFile, path: &Path) -> Result<(), Diagnostic> {
    let source = tmp.into_temp_path().keep().map_err(|mut error| {
        error.path.disable_cleanup(true);
        commit_error(
            "prepare temporary attributes",
            &error.path,
            path,
            &error.error,
        )
    })?;
    fs::rename(&source, path)
        .map_err(|error| commit_error("replace destination", &source, path, &error))
}

fn commit_error(
    stage: &str,
    source: &Path,
    destination: &Path,
    error: &std::io::Error,
) -> Diagnostic {
    io_error(
        "OUTPUT_COMMIT_FAILED",
        &format!(
            "Cannot {stage} for '{}'. Complete candidate retained at '{}'. Filesystem error: {} (code {:?}).",
            destination.display(),
            source.display(),
            error,
            error.raw_os_error()
        ),
    )
}

pub fn safe_relative(value: &str) -> Result<PathBuf, Diagnostic> {
    if value.is_empty() || value.contains('\\') || value.contains('\0') || value.contains(':') {
        return Err(io_error(
            "UNSAFE_PATH",
            "Artifact path has an invalid separator, prefix or name.",
        ));
    }
    let path = Path::new(value);
    for component in path.components() {
        match component {
            Component::Normal(name) => {
                let s = name
                    .to_str()
                    .ok_or_else(|| io_error("UNSAFE_PATH", "Artifact names must be UTF-8."))?;
                let stem = s.split('.').next().unwrap_or("").to_uppercase();
                if s.ends_with(['.', ' '])
                    || s.chars().any(|c| c.is_control() || "<>\"|?*".contains(c))
                    || ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                    || (stem.len() == 4
                        && (stem.starts_with("COM") || stem.starts_with("LPT"))
                        && stem.as_bytes()[3].is_ascii_digit()
                        && stem.as_bytes()[3] != b'0')
                {
                    return Err(io_error(
                        "UNSAFE_PATH",
                        "Artifact name is not portable across the supported filesystems.",
                    ));
                }
            }
            _ => {
                return Err(io_error(
                    "UNSAFE_PATH",
                    "Artifact path must stay relative without dot or parent components.",
                ));
            }
        }
    }
    // components normalizes internal '.'. Check spelling too before extraction.
    if value
        .split('/')
        .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(io_error(
            "UNSAFE_PATH",
            "Artifact path contains an empty or dot component.",
        ));
    }
    Ok(path.to_path_buf())
}
pub fn collision_key(path: &str) -> String {
    path.nfc().flat_map(char::to_lowercase).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunJournal {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<crate::input::AuthMode>,
    pub schema_version: u32,
    pub run_id: String,
    pub request_id: String,
    pub endpoint: String,
    pub company: String,
    pub caller: String,
    pub scope: String,
    pub categories: Vec<String>,
    pub plugin_hashes: BTreeMap<String, String>,
    #[serde(default)]
    pub category_names: BTreeMap<String, String>,
    #[serde(default)]
    pub separate_folder: Option<String>,
    pub settings: OperatingSettings,
    pub phase: String,
    /// Timing for the latest execution. Old journals have no timing evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statistics: Option<serde_json::Value>,
    pub artifacts: Vec<Artifact>,
    pub diagnostics: Vec<Diagnostic>,
    pub pending_files: Vec<FileRef>,
    #[serde(default)]
    pub acquisitions: Vec<FileRef>,
    pub cursors: BTreeMap<String, String>,
    #[serde(default)]
    pub discovered: BTreeMap<String, Vec<Identity>>,
    #[serde(default)]
    pub inventory_data: BTreeMap<String, FileRef>,
    #[serde(default)]
    pub discovery_finished: BTreeMap<String, bool>,
    #[serde(default)]
    pub category_outcomes: BTreeMap<String, String>,
    #[serde(default)]
    pub references: Vec<serde_json::Value>,
    #[serde(default)]
    pub definition_views: BTreeMap<String, FileRef>,
    /// Native files shared by plugin-declared output owners within this run.
    #[serde(default)]
    pub shared_exports: BTreeMap<String, Vec<FileRef>>,
    #[serde(default)]
    pub artifact_acquisitions: BTreeMap<String, Vec<FileRef>>,
    #[serde(default)]
    pub targets: Vec<Identity>,
    #[serde(default)]
    pub plugin_settings: BTreeMap<String, serde_json::Value>,
    /// Original plugin-declared filter metadata, including matching-contract help.
    /// Values are frozen separately; older journals have no metadata snapshot.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_selection_contracts: BTreeMap<String, serde_json::Value>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub managed_processing: Option<herbert_contract::ManagedRunMarker>,
    #[serde(default)]
    pub(crate) helper_transfers: Vec<crate::helpers::HelperTransfer>,
    #[serde(default)]
    pub(crate) native_artifacts: Vec<crate::native_artifact_store::OperationRecord>,
    #[serde(default)]
    pub(crate) native_export_files: Vec<crate::application::native_file_export::ExportFile>,
    #[serde(default)]
    pub(crate) host_transfers: Vec<crate::application::host_utility::Transfer>,
    #[serde(default)]
    pub(crate) helper_libraries: Vec<crate::helper_libraries::LibraryLease>,
    #[serde(default)]
    pub temporary_access_enabled: bool,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub(crate) server_change_authority:Option<crate::server_changes::RunAuthority>,
    /// Prior run scopes remain once each for cleanup attribution after renewal.
    #[serde(default,skip_serializing_if="Vec::is_empty")]
    pub(crate) server_change_authority_history:Vec<crate::server_changes::RunAuthority>,
    #[serde(default = "herbert_contract::CollectionMethod::legacy")]
    pub collection_method: herbert_contract::CollectionMethod,
    #[serde(default)]
    pub access_leases: Vec<crate::access::AccessLease>,
}

pub struct Storage {
    root: PathBuf,
    _lock: std::sync::Arc<File>,
    pub(crate) output_names: std::sync::Arc<std::sync::Mutex<crate::output_layout::NameRegistry>>,
    pub(crate) page_owns_derived_files: bool,
    pub journal: RunJournal,
    pub manifest: Manifest,
    checkpoints: std::sync::Mutex<Option<crate::checkpoint_log::Writer>>,
    output_directories: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub endpoint: String,
    pub company: String,
    pub caller: String,
    #[serde(default)]
    pub references: Vec<serde_json::Value>,
    pub artifacts: BTreeMap<String, Artifact>,
    pub retained_files: BTreeMap<String, FileRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<crate::checkpoint_log::Marker>,
}
pub fn separate_folder(journal: &RunJournal) -> String {
    journal
        .separate_folder
        .clone()
        .unwrap_or_else(|| format!("herbert-{}", journal.run_id))
}
fn migrate_legacy_metadata(root: &Path) -> Result<(), Diagnostic> {
    let old = root.join(".hoover");
    let new = root.join("Herbert Data");
    if !old.exists() || new.exists() {
        return Ok(());
    }
    if fs::symlink_metadata(&old)
        .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot read existing download data."))?
        .file_type()
        .is_symlink()
    {
        return Err(io_error(
            "UNSAFE_PATH",
            "Existing download data cannot be a symbolic link.",
        ));
    }
    if fs::symlink_metadata(old.join("destination.lock")).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(io_error(
            "UNSAFE_PATH",
            "Existing download lock cannot be a symbolic link.",
        ));
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(old.join("destination.lock"))
        .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot open existing download lock."))?;
    lock.try_lock_exclusive().map_err(|_| {
        io_error(
            "DESTINATION_BUSY",
            "Another Herbert process owns this download.",
        )
    })?;
    // Windows refuses directory renames with an open child handle. Confirm the
    // legacy writer is absent, close that handle, then rename atomically. The
    // normal open below acquires the relocated lock before any data is changed.
    drop(lock);
    fs::rename(&old, &new).map_err(|error| {
        io_error(
            "OUTPUT_UNAVAILABLE",
            &format!(
                "Cannot rename the existing download data folder; all files are preserved. {error}"
            ),
        )
    })?;
    Ok(())
}
impl Storage {
    pub fn open(
        destination: &Path,
        separate: bool,
        journal: RunJournal,
    ) -> Result<Self, Diagnostic> {
        let chosen = if separate {
            destination.join(separate_folder(&journal))
        } else {
            destination.to_path_buf()
        };
        if separate {
            fs::create_dir_all(destination).map_err(|_| {
                io_error(
                    "OUTPUT_UNAVAILABLE",
                    "Cannot create the selected destination.",
                )
            })?;
            fs::create_dir(&chosen).map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot create a new download folder. Choose an unused destination and try again."))?;
        }
        fs::create_dir_all(&chosen).map_err(|_| {
            io_error(
                "OUTPUT_UNAVAILABLE",
                "Cannot create the selected backup folder.",
            )
        })?;
        let root = chosen.canonicalize().map_err(|_| {
            io_error(
                "OUTPUT_UNAVAILABLE",
                "Cannot resolve the selected backup folder.",
            )
        })?;
        migrate_legacy_metadata(&root)?;
        let internal = root.join("Herbert Data");
        if let Ok(m) = fs::symlink_metadata(&internal) {
            if m.file_type().is_symlink() {
                return Err(io_error(
                    "UNSAFE_PATH",
                    "The journal directory cannot be a symbolic link.",
                ));
            }
        }
        fs::create_dir_all(&internal).map_err(|_| {
            io_error(
                "OUTPUT_UNAVAILABLE",
                "Cannot create backup journal storage.",
            )
        })?;
        let lock_path = internal.join("destination.lock");
        if let Ok(m) = fs::symlink_metadata(&lock_path) {
            if m.file_type().is_symlink() {
                return Err(io_error(
                    "UNSAFE_PATH",
                    "The destination lock cannot be a symbolic link.",
                ));
            }
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot open the destination lock."))?;
        lock.try_lock_exclusive().map_err(|_|io_error("DESTINATION_BUSY","Another host owns this destination. Attach to that run or choose a different folder."))?;
        let internal_manifest = internal.join("manifest.json");
        let manifest_path = if internal_manifest.exists() || !root.join("manifest.json").exists() {
            internal_manifest
        } else {
            root.join("manifest.json")
        };
        if fs::symlink_metadata(&manifest_path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(io_error(
                "UNSAFE_PATH",
                "The stable manifest cannot be a symbolic link.",
            ));
        }
        let manifest = match fs::read(&manifest_path) {
            Ok(bytes) => serde_json::from_slice::<Manifest>(&bytes).map_err(|_| {
                io_error(
                    "MANIFEST_INVALID",
                    "Preserve the invalid manifest; refresh is refused.",
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Manifest {
                schema_version: 1,
                endpoint: journal.endpoint.clone(),
                company: journal.company.clone(),
                caller: journal.caller.clone(),
                references: vec![],
                artifacts: BTreeMap::new(),
                retained_files: BTreeMap::new(),
                checkpoint: None,
            },
            Err(_) => {
                return Err(io_error(
                    "MANIFEST_UNAVAILABLE",
                    "Cannot read the existing manifest.",
                ));
            }
        };
        let mut manifest = crate::checkpoint_log::read_manifest(&root, manifest)?;
        if manifest.schema_version != 1
            || manifest.endpoint != journal.endpoint
            || manifest.company != journal.company
            || manifest.caller != journal.caller
        {
            return Err(io_error(
                "DESTINATION_SCOPE_MISMATCH",
                "This destination belongs to another endpoint, caller or company. Select a matching connection or a separate backup folder.",
            ));
        }
        manifest.checkpoint = Some(crate::checkpoint_log::Marker::new(&journal.run_id));
        let mut storage = Self {
            root,
            _lock: std::sync::Arc::new(lock),
            output_names: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
            page_owns_derived_files: false,
            journal,
            manifest,
            checkpoints: std::sync::Mutex::new(None),
            output_directories: Default::default(),
        };
        if storage.journal.references.is_empty() {
            storage.journal.references = storage.manifest.references.clone();
        }
        storage.checkpoint()?;
        Ok(storage)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn confined(&self, relative: &str) -> Result<PathBuf, Diagnostic> {
        let relative = safe_relative(relative)?;
        let mut current = self.root.clone();
        for c in relative.components() {
            current.push(c);
            if let Ok(m) = fs::symlink_metadata(&current) {
                if m.file_type().is_symlink() {
                    return Err(io_error(
                        "UNSAFE_PATH",
                        "Output path crosses a symbolic link or junction.",
                    ));
                }
                // The root is already canonical. An ordinary child cannot
                // redirect it. Resolve Windows reparse points, including cloud
                // placeholders, but do not open every ordinary ancestor again
                // for every file and receipt. Keep the link check above.
                #[cfg(target_os = "windows")]
                let must_resolve = {
                    use std::os::windows::fs::MetadataExt;
                    m.file_attributes() & 0x400 != 0
                };
                #[cfg(not(target_os = "windows"))]
                let must_resolve = false;
                if must_resolve && !current
                    .canonicalize()
                    .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot resolve output path."))?
                    .starts_with(&self.root)
                {
                    return Err(io_error(
                        "UNSAFE_PATH",
                        "Output path escapes its assigned root.",
                    ));
                }
            }
        }
        Ok(current)
    }
    pub fn checkpoint(&self) -> Result<(), Diagnostic> {
        // Commit page naming before the journal acknowledges its output files.
        self.checkpoint_names()?;
        let mut writer = self.checkpoints.lock().map_err(|_| {
            io_error(
                "CHECKPOINT_IO_UNCERTAIN",
                "The checkpoint writer is unavailable; reopen for reconciliation.",
            )
        })?;
        let first = writer.is_none();
        if first {
            *writer = Some(crate::checkpoint_log::Writer::open(
                &self.root,
                &self.journal,
                &self.manifest,
            )?);
        }
        let writer = writer.as_mut().expect("writer initialized");
        let phase_changed = writer.phase() != self.journal.phase;
        writer.checkpoint(&self.journal, &self.manifest)?;
        if first || phase_changed {
            writer.materialize()?;
        }
        Ok(())
    }
    fn materialize_checkpoint(&self) -> Result<(), Diagnostic> {
        self.checkpoints
            .lock()
            .map_err(|_| {
                io_error(
                    "CHECKPOINT_IO_UNCERTAIN",
                    "The checkpoint writer is unavailable; reopen for reconciliation.",
                )
            })?
            .as_mut()
            .ok_or_else(|| {
                io_error(
                    "CHECKPOINT_IO_UNCERTAIN",
                    "The checkpoint writer was not initialized.",
                )
            })?
            .materialize()
    }
    pub fn record_artifact(&mut self, mut artifact: Artifact) -> Result<(), Diagnostic> {
        crate::processing_status::reconcile(&mut artifact);
        for reference in &artifact.files {
            self.verify(reference)?;
            if reference.role != "native"
                && reference.role != "derivedConflict"
                && reference.role != "processingInput"
            {
                continue;
            }
            let key = sha256(reference.path.as_bytes());
            self.manifest.retained_files.insert(key, reference.clone());
        }
        self.manifest
            .artifacts
            .insert(identity_key(&artifact.identity), artifact.clone());
        self.journal
            .artifacts
            .retain(|previous| previous.identity != artifact.identity);
        self.journal.artifacts.push(artifact);
        self.manifest.references = self.journal.references.clone();
        self.checkpoint()
    }
    /// A page writer reads stable receipts without holding the run mutex.
    /// The owner commits returned artifacts. This view must never checkpoint or
    /// release the destination. Shared names keep concurrent plugin paths unique.
    pub(crate) fn page_output_snapshot<'a>(&self, owners: impl IntoIterator<Item = &'a Identity>) -> Self {
        // Writers need naming/scope, this page's native acquisitions, and prior
        // receipts for conflict detection. Copying all definitions/references
        // for every ready group makes local output grow with the whole backup.
        let mut journal = new_journal(self.journal.request_id.clone());
        journal.run_id = self.journal.run_id.clone();
        journal.company = self.journal.company.clone();
        journal.scope = self.journal.scope.clone();
        journal.category_names = self.journal.category_names.clone();
        let mut artifacts = BTreeMap::new();
        for owner in owners {
            let key = identity_key(owner);
            if let Some(files) = self.journal.artifact_acquisitions.get(&key) {
                journal.artifact_acquisitions.insert(key.clone(), files.clone());
            }
            if let Some(previous) = self.manifest.artifacts.get(&key) {
                artifacts.insert(key, previous.clone());
            }
        }
        let manifest = Manifest { schema_version:self.manifest.schema_version,
            endpoint:self.manifest.endpoint.clone(), company:self.manifest.company.clone(),
            caller:self.manifest.caller.clone(), references:vec![], artifacts,
            retained_files:BTreeMap::new(), checkpoint:self.manifest.checkpoint.clone() };
        Self { root: self.root.clone(), _lock: self._lock.clone(),
            output_names: self.output_names.clone(), page_owns_derived_files: true,
            journal, manifest,
            checkpoints: std::sync::Mutex::new(None), output_directories: Default::default() }
    }
    pub fn release_destination(&mut self) -> Result<(), Diagnostic> {
        self.checkpoint()?;
        self.materialize_checkpoint()?;
        FileExt::unlock(self._lock.as_ref()).map_err(|_| {
            io_error(
                "DESTINATION_RELEASE_UNCERTAIN",
                "The checkpoint was saved but releasing the destination lock failed.",
            )
        })
    }
    pub fn preserve_raw(&mut self, bytes: &[u8], extension: &str) -> Result<FileRef, Diagnostic> {
        self.preserve_immutable(bytes, extension, "native")
    }
    pub fn preserve_for(
        &mut self,
        identity: Option<&Identity>,
        bytes: &[u8],
        extension: &str,
    ) -> Result<FileRef, Diagnostic> {
        let file = self.preserve_raw(bytes, extension)?;
        if let Some(identity) = identity {
            let owned = self
                .journal
                .artifact_acquisitions
                .entry(identity_key(identity))
                .or_default();
            if !owned
                .iter()
                .any(|previous| previous.path == file.path && previous.sha256 == file.sha256)
            {
                owned.push(file.clone());
                self.checkpoint()?;
            }
        }
        Ok(file)
    }
    pub fn assemble(
        &mut self,
        identity: &Identity,
        chunks: &[FileRef],
        bytes: u64,
        expected: &str,
        extension: &str,
    ) -> Result<FileRef, Diagnostic> {
        if extension.is_empty() || !extension.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(io_error(
                "UNSAFE_PATH",
                "The assembled payload extension is invalid.",
            ));
        }
        let owned = self
            .journal
            .artifact_acquisitions
            .get(&identity_key(identity))
            .cloned()
            .unwrap_or_default();
        for chunk in chunks {
            if chunk.role != "native"
                || !owned.iter().any(|file| {
                    file.path == chunk.path
                        && file.sha256 == chunk.sha256
                        && file.bytes == chunk.bytes
                })
            {
                return Err(io_error(
                    "PAYLOAD_OWNERSHIP_DENIED",
                    "Payload assembly can read only native chunks acquired for this exact artifact.",
                ));
            }
            self.verify(chunk)?;
        }
        let staging = self.confined("Herbert Data/staging")?;
        fs::create_dir_all(&staging).map_err(|_| {
            io_error(
                "OUTPUT_UNAVAILABLE",
                "Cannot create the owned transfer staging directory.",
            )
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(&staging)
            .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot create the assembled payload."))?;
        let mut hash = Sha256::new();
        let mut total = 0u64;
        {
            let mut writer = HashingFile {
                file: temporary.as_file_mut(),
                hash: &mut hash,
            };
            for chunk in chunks {
                let mut input = File::open(self.confined(&chunk.path)?)
                    .map_err(|_| io_error("RAW_UNAVAILABLE", "A verified chunk is unavailable."))?;
                total = total
                    .checked_add(std::io::copy(&mut input, &mut writer).map_err(|_| {
                        io_error(
                            "OUTPUT_WRITE_FAILED",
                            "Cannot stream the full assembled payload.",
                        )
                    })?)
                    .ok_or_else(|| {
                        io_error(
                            "PAYLOAD_LENGTH_OVERFLOW",
                            "The assembled byte count exceeds the native u64 representation.",
                        )
                    })?;
            }
        }
        let digest = hex::encode(hash.finalize());
        if total != bytes || digest != expected {
            return Err(io_error(
                "PAYLOAD_INTEGRITY_FAILED",
                "The assembled payload length or SHA256 differs from its native transfer metadata.",
            ));
        }
        temporary.as_file().sync_all().map_err(|_| {
            io_error(
                "OUTPUT_WRITE_FAILED",
                "Cannot synchronize the assembled native payload.",
            )
        })?;
        let file = FileRef {
            path: format!("Herbert Data/raw/{}/{}.{}", &digest[..2], digest, extension),
            sha256: digest,
            bytes: total,
            role: "native".into(),
        };
        let destination = self.confined(&file.path)?;
        self.journal.pending_files.push(file.clone());
        self.checkpoint()?;
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| io_error("INVALID_PATH", "Payload has no parent directory."))?,
        )
        .map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot create the payload directory."))?;
        if destination.exists() {
            self.verify(&file)?;
        } else if let Err(mut error) = temporary.into_temp_path().persist_noclobber(&destination) {
            if error.error.kind() != std::io::ErrorKind::AlreadyExists {
                error.path.disable_cleanup(true);
                return Err(commit_error(
                    "commit immutable payload",
                    &error.path,
                    &destination,
                    &error.error,
                ));
            }
            self.verify(&file)?;
        }
        self.verify(&file)?;
        self.journal
            .pending_files
            .retain(|pending| pending.path != file.path);
        if !self
            .journal
            .acquisitions
            .iter()
            .any(|r| r.path == file.path && r.sha256 == file.sha256)
        {
            self.journal.acquisitions.push(file.clone());
        }
        self.manifest
            .retained_files
            .insert(sha256(file.path.as_bytes()), file.clone());
        let owned = self
            .journal
            .artifact_acquisitions
            .entry(identity_key(identity))
            .or_default();
        if !owned
            .iter()
            .any(|r| r.path == file.path && r.sha256 == file.sha256)
        {
            owned.push(file.clone());
        }
        self.checkpoint()?;
        Ok(file)
    }
    pub fn preserve_immutable(
        &mut self,
        bytes: &[u8],
        extension: &str,
        role: &str,
    ) -> Result<FileRef, Diagnostic> {
        if !extension.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(io_error("UNSAFE_PATH", "Native file extension is invalid."));
        }
        let digest = sha256(bytes);
        let relative = format!("Herbert Data/raw/{}/{}.{}", &digest[..2], digest, extension);
        let reference = FileRef {
            path: relative.clone(),
            sha256: digest,
            bytes: bytes.len() as u64,
            role: role.into(),
        };
        let path = self.confined(&relative)?;
        // A committed file needs verification, not another pending/complete pair.
        if self
            .manifest
            .retained_files
            .get(&sha256(relative.as_bytes()))
            == Some(&reference)
            && self.journal.acquisitions.contains(&reference)
            && !self
                .journal
                .pending_files
                .iter()
                .any(|file| file.path == relative)
        {
            self.verify(&reference)?;
            return Ok(reference);
        }
        self.journal.pending_files.push(reference.clone());
        self.checkpoint()?;
        if path.exists() {
            self.verify(&reference)?;
        } else {
            atomic_write(&path, bytes)?;
            self.verify(&reference)?;
        }
        self.journal
            .pending_files
            .retain(|r| r.path != reference.path);
        if !self
            .journal
            .acquisitions
            .iter()
            .any(|r| r.path == reference.path && r.sha256 == reference.sha256)
        {
            self.journal.acquisitions.push(reference.clone());
        }
        self.manifest
            .retained_files
            .insert(sha256(relative.as_bytes()), reference.clone());
        self.checkpoint()?;
        Ok(reference)
    }
    /// A guest may inspect its own preserved bytes, never an arbitrary path.
    /// Discovery has no artifact identity, so its files must have been issued
    /// by this exact drive operation. Retrieval also supports persisted owners.
    fn read_owned_native(
        &self,
        identity: Option<&Identity>,
        reference: &FileRef,
        issued: &[FileRef],
    ) -> Result<Vec<u8>, Diagnostic> {
        let same = |file: &FileRef| {
            file.role == "native"
                && file.path == reference.path
                && file.sha256 == reference.sha256
                && file.bytes == reference.bytes
        };
        let owned = match identity {
            Some(identity) => self
                .journal
                .artifact_acquisitions
                .get(&identity_key(identity))
                .is_some_and(|files| files.iter().any(same)),
            None => issued.iter().any(same),
        };
        if reference.role != "native" || !owned {
            return Err(io_error(
                "PAYLOAD_OWNERSHIP_DENIED",
                "Projection can read only native bytes acquired for this exact operation.",
            ));
        }
        let bytes = fs::read(self.confined(&reference.path)?).map_err(|_| {
            io_error(
                "RAW_UNAVAILABLE",
                "The acquired native source is unavailable.",
            )
        })?;
        // Verify the same buffer that is parsed, avoiding a second file read
        // between integrity verification and interpretation.
        if bytes.len() as u64 != reference.bytes || sha256(&bytes) != reference.sha256 {
            return Err(io_error(
                "INTEGRITY_FAILED",
                "The acquired native source no longer matches its recorded length and SHA256.",
            ));
        }
        Ok(bytes)
    }
    pub fn read_native_binary(
        &self, identity: Option<&Identity>, reference: &FileRef, issued: &[FileRef],
    ) -> Result<String, Diagnostic> {
        use base64::Engine;
        let bytes = self.read_owned_native(identity, reference, issued)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
    pub fn parse_native_xml(
        &self,
        identity: Option<&Identity>,
        reference: &FileRef,
        issued: &[FileRef],
    ) -> Result<serde_json::Value, Diagnostic> {
        let bytes = self.read_owned_native(identity, reference, issued)?;
        crate::xml_archive::parse_xml(&bytes)
    }
    pub fn parse_native_xml_archive(
        &self,
        identity: Option<&Identity>,
        reference: &FileRef,
        issued: &[FileRef],
        entries: &[String],
    ) -> Result<serde_json::Value, Diagnostic> {
        let bytes = self.read_owned_native(identity, reference, issued)?;
        crate::xml_archive::parse(&bytes, entries)
    }
    pub fn parse_native_json_archive(
        &self,
        identity: Option<&Identity>,
        reference: &FileRef,
        issued: &[FileRef],
        entries: &[String],
    ) -> Result<serde_json::Value, Diagnostic> {
        let bytes = self.read_owned_native(identity, reference, issued)?;
        crate::xml_archive::parse_json(&bytes, entries)
    }
    pub fn parse_native_json(
        &self,
        identity: Option<&Identity>,
        reference: &FileRef,
        issued: &[FileRef],
    ) -> Result<serde_json::Value, Diagnostic> {
        let bytes = self.read_owned_native(identity, reference, issued)?;
        let view = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        serde_json::from_slice(view).map_err(|_| io_error("NATIVE_JSON_INVALID", "The verified native bytes are not a valid UTF-8 JSON document. Original bytes remain preserved."))
    }
    pub fn verify(&self, reference: &FileRef) -> Result<(), Diagnostic> {
        let mut file = File::open(self.confined(&reference.path)?)
            .map_err(|_| io_error("RAW_UNAVAILABLE", "A recorded native file is unavailable."))?;
        let mut hash = Sha256::new();
        let bytes = std::io::copy(&mut file, &mut HashWriter(&mut hash))
            .map_err(|_| io_error("INTEGRITY_FAILED", "Cannot read the full native artifact."))?;
        if bytes != reference.bytes || hex::encode(hash.finalize()) != reference.sha256 {
            return Err(io_error(
                "INTEGRITY_FAILED",
                "Native length or hash does not match its recorded evidence.",
            ));
        }
        Ok(())
    }
    pub fn write_derived(
        &self,
        identity: &Identity,
        name: &str,
        bytes: &[u8],
    ) -> Result<(FileRef, bool), Diagnostic> {
        self.write_derived_stream(identity, name, &mut std::io::Cursor::new(bytes))
    }
    pub fn write_derived_stream(
        &self,
        identity: &Identity,
        name: &str,
        input: &mut impl std::io::Read,
    ) -> Result<(FileRef, bool), Diagnostic> {
        safe_relative(name)?;
        let base = self.artifact_folder(identity)?;
        let portable_name = name
            .split('/')
            .map(|part| crate::output_layout::bounded(part.to_owned()))
            .collect::<Vec<_>>()
            .join("/");
        let name = portable_name.as_str();
        let requested = format!("{base}/{name}");
        let requested_path = self.confined(&requested)?;
        let parent = requested_path.parent().ok_or_else(|| {
            io_error("INVALID_PATH", "An output file has no parent directory.")
        })?;
        {
            // This cache skips repeated directory creation, not path validation.
            // Each file still passes confined() before the directory is used.
            let mut directories = self.output_directories.lock().map_err(|_| {
                io_error("OUTPUT_UNAVAILABLE", "Output directory preparation failed.")
            })?;
            if !directories.contains(parent) {
                fs::create_dir_all(parent).map_err(|_| {
                    io_error("OUTPUT_UNAVAILABLE", "Cannot create the output directory.")
                })?;
                directories.insert(parent.to_owned());
            }
        }
        // Keep the candidate beside its destination. An interrupted write must
        // not leave partial bytes under the completed filename.
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|_| {
            io_error("OUTPUT_UNAVAILABLE", "Cannot create a temporary output file.")
        })?;
        let mut hash = Sha256::new();
        let bytes = {
            let mut writer = HashingFile {
                file: temporary.as_file_mut(),
                hash: &mut hash,
            };
            std::io::copy(input,&mut writer).map_err(|_|io_error("SOURCE_STREAM_FAILED","Cannot read and preserve the complete source view; original native acquisitions remain available."))?
        };
        // Ordinary downloads use the OS write cache. Hash the bytes as they are
        // written; recovery verifies files before it reuses their receipts.
        let digest = hex::encode(hash.finalize());
        let stamp = format!(
            "Herbert Data/derived/{}/{}.json",
            identity_key(identity),
            sha256(name.as_bytes())
        );
        let exists = requested_path.exists();
        // A new destination needs no legacy receipt lookup or content readback.
        let last: Option<FileRef> = if exists {
            self.manifest.artifacts.get(&identity_key(identity))
                .and_then(|artifact| artifact.files.iter().find(|file|
                    file.path == requested && file.role == "derived").cloned())
                .or_else(|| self.confined(&stamp).ok()
                    .and_then(|path| fs::read(path).ok())
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok()))
        } else { None };
        // A process can stop after rename but before the page checkpoint. Reuse
        // an unrecorded file only when its actual bytes match this new download.
        let matching_unrecorded = exists && last.is_none() && self.verify(&FileRef {
            path: requested.clone(), sha256: digest.clone(), bytes, role: "derived".into(),
        }).is_ok();
        let edited = exists && !matching_unrecorded
            && last.as_ref().is_none_or(|file| self.verify(file).is_err());
        let mut relative = if edited {
            format!("Conflicts/{base}/{digest}/{name}")
        } else {
            requested
        };
        let mut file = FileRef {
            path: relative.clone(),
            sha256: digest.clone(),
            bytes,
            role: if edited { "derivedConflict" } else { "derived" }.into(),
        };
        if edited && self.confined(&relative)?.exists() && self.verify(&file).is_err() {
            // A user can edit a prior conflict too. Keep it and commit this server
            // view at a new owned name instead of overwriting the edited file.
            relative = format!("Conflicts/{base}/{digest}/{}/{name}", Uuid::new_v4());
            file.path = relative.clone();
        }
        let destination = if edited { self.confined(&relative)? } else { requested_path };
        let unchanged = if edited {
            destination.exists() && self.verify(&file).is_ok()
        } else {
            matching_unrecorded || (exists && last.as_ref()
                .is_some_and(|old| old.sha256 == digest && old.bytes == bytes))
        };
        if !unchanged {
            if edited {
                fs::create_dir_all(destination.parent().ok_or_else(|| {
                    io_error("INVALID_PATH", "A conflict file has no parent directory.")
                })?).map_err(|_| io_error("OUTPUT_UNAVAILABLE", "Cannot create the conflict directory."))?;
            }
            commit_temporary(temporary, &destination)?;
        }
        if !edited && !self.page_owns_derived_files {
            atomic_write(
                &self.confined(&stamp)?,
                &serde_json::to_vec(&file).map_err(|_| {
                    io_error(
                        "OUTPUT_WRITE_FAILED",
                        "Cannot record source view ownership.",
                    )
                })?,
            )?;
        }
        Ok((file, edited))
    }
    /// Check output only when reopening a run. A saved cursor does not prove
    /// that cached writes survived a machine failure or later file changes.
    pub(crate) fn reconcile_downloads(&mut self) -> Result<(), Diagnostic> {
        let invalid = self.journal.artifacts.iter().filter(|artifact| {
            artifact.files.iter().any(|file| self.verify(file).is_err())
        }).map(|artifact| identity_key(&artifact.identity))
            .collect::<std::collections::HashSet<_>>();
        for artifact in &mut self.journal.artifacts {
            if !invalid.contains(&identity_key(&artifact.identity)) { continue; }
            artifact.acquisition = herbert_contract::Completeness::Partial;
            artifact.processing = "partial".into();
            // Replay the category from its first page. Keep receipts so changed
            // user files are preserved by the normal conflict handling.
            self.journal.cursors.remove(&artifact.identity.category);
            self.journal.discovery_finished.remove(&artifact.identity.category);
            self.journal.category_outcomes.remove(&artifact.identity.category);
            self.manifest.artifacts.insert(identity_key(&artifact.identity), artifact.clone());
        }
        Ok(())
    }

    pub fn reconcile_pending(&mut self) -> Result<(), Diagnostic> {
        let pending = std::mem::take(&mut self.journal.pending_files);
        for file in pending {
            if let Err(d) = self.verify(&file) {
                self.journal.diagnostics.push(d);
            } else {
                self.journal.diagnostics.push(Diagnostic::new(
                    "RECOVERED_FILE",
                    format!("Verified a committed native file: {}", file.path),
                ));
            }
        }
        self.checkpoint()
    }
}
struct HashWriter<'a>(&'a mut Sha256);
struct HashingFile<'a> {
    file: &'a mut File,
    hash: &'a mut Sha256,
}
impl Write for HashingFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.file.write(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
impl Write for HashWriter<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.update(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn new_journal(request_id: String) -> RunJournal {
    RunJournal {
        server_version: None,
        auth_mode: None,
        schema_version: 1,
        run_id: Uuid::new_v4().to_string(),
        request_id,
        endpoint: String::new(),
        company: String::new(),
        caller: String::new(),
        scope: "targeted".into(),
        categories: vec![],
        plugin_hashes: BTreeMap::new(),
        category_names: BTreeMap::new(),
        separate_folder: None,
        settings: Default::default(),
        phase: "preparing".into(),
        statistics: None,
        artifacts: vec![],
        diagnostics: vec![],
        pending_files: vec![],
        acquisitions: vec![],
        cursors: BTreeMap::new(),
        discovered: BTreeMap::new(),
        inventory_data: BTreeMap::new(),
        discovery_finished: BTreeMap::new(),
        category_outcomes: BTreeMap::new(),
        references: vec![],
        definition_views: BTreeMap::new(),
        shared_exports: BTreeMap::new(),
        artifact_acquisitions: BTreeMap::new(),
        targets: vec![],
        plugin_settings: BTreeMap::new(),
        plugin_selection_contracts: BTreeMap::new(),
        managed_processing: None,
        helper_transfers: vec![],
        helper_libraries: vec![],
        native_artifacts: vec![],
        native_export_files: vec![],
        host_transfers: vec![],
        temporary_access_enabled: false,
        server_change_authority:None,
        server_change_authority_history:vec![],
        collection_method: herbert_contract::CollectionMethod::default(),
        access_leases: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::Scope;
    fn id() -> Identity {
        Identity {
            category: "C01".into(),
            kind: "UDCodeType".into(),
            scope: Scope::Company {
                company: "A".into(),
            },
            keys: BTreeMap::from([("CodeTypeID".into(), "../NUL".into())]),
        }
    }
    #[test]
    fn confines_portable_paths_and_collision_equivalence() {
        for p in [
            "../outside",
            "/root",
            "C:/root",
            "x\\y",
            "x/./y",
            "x//y",
            "x/CON.json",
            "a./b",
        ] {
            assert!(safe_relative(p).is_err(), "{p}");
        }
        assert_eq!(collision_key("A/café"), collision_key("a/cafe\u{301}"));
    }
    #[test]
    #[cfg(target_os = "windows")]
    fn atomic_commit_supports_deep_windows_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut parent = dir.path().to_path_buf();
        while parent.as_os_str().len() < 300 {
            parent.push("deep-output-folder");
        }
        let output = parent.join("state.json");
        atomic_write(&output, b"first").unwrap();
        atomic_write(&output, b"second").unwrap();
        assert_eq!(fs::read(output).unwrap(), b"second");
        assert_eq!(fs::read_dir(parent).unwrap().count(), 1);
    }

    #[test]
    fn repeated_atomic_commits_preserve_the_latest_complete_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("manifest.json");
        // A two-row native transfer makes dozens of metadata commits. Exercise
        // that measured pattern with distinct content at every replacement.
        for revision in 0..64 {
            let bytes = format!("{{\"revision\":{revision}}}");
            atomic_write(&output, bytes.as_bytes()).unwrap();
            assert_eq!(fs::read(&output).unwrap(), bytes.as_bytes());
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_commit_preserves_candidate_and_original_destination() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("occupied");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("sentinel"), b"original").unwrap();
        let error = atomic_write(&destination, b"recover this candidate").unwrap_err();
        assert_eq!(error.code, "OUTPUT_COMMIT_FAILED");
        assert!(error.message.contains("replace destination"));
        assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"original");
        let candidates = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().unwrap().is_file())
            .collect::<Vec<_>>();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            fs::read(candidates[0].path()).unwrap(),
            b"recover this candidate"
        );
        assert!(
            error
                .message
                .contains(&candidates[0].file_name().to_string_lossy().to_string())
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn atomic_replacement_supports_an_existing_shared_reader() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("journal.json");
        atomic_write(&output, b"original").unwrap();
        let mut reader = File::open(&output).unwrap();
        atomic_write(&output, b"replacement").unwrap();
        let mut original = Vec::new();
        reader.read_to_end(&mut original).unwrap();
        assert_eq!(original, b"original");
        assert_eq!(fs::read(output).unwrap(), b"replacement");
    }

    #[test]
    fn json_projection_verifies_owner_and_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Storage::open(dir.path(), false, new_journal("json".into())).unwrap();
        let bytes = b"\xef\xbb\xbf{\"apps\":[]}";
        let file = s.preserve_for(Some(&id()), bytes, "json").unwrap();
        assert_eq!(
            s.parse_native_json(Some(&id()), &file, &[]).unwrap(),
            serde_json::json!({"apps":[]})
        );
        assert_eq!(fs::read(s.root().join(&file.path)).unwrap(), bytes);
        let mut other = id();
        other.kind.push_str("Other");
        assert_eq!(
            s.parse_native_json(Some(&other), &file, &[file.clone()])
                .unwrap_err()
                .code,
            "PAYLOAD_OWNERSHIP_DENIED"
        );
        assert_eq!(
            s.parse_native_json(None, &file, &[]).unwrap_err().code,
            "PAYLOAD_OWNERSHIP_DENIED"
        );
        assert!(s.parse_native_json(None, &file, &[file.clone()]).is_ok());
        fs::write(s.root().join(&file.path), b"{\"changed\":true}").unwrap();
        assert_eq!(
            s.parse_native_json(Some(&id()), &file, &[])
                .unwrap_err()
                .code,
            "INTEGRITY_FAILED"
        );
    }

    #[test]
    fn raw_xml_projection_verifies_owner_and_original_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Storage::open(dir.path(), false, new_journal("xml".into())).unwrap();
        let bytes = b"<Report xmlns=\"urn:report\"><Name>Original</Name></Report>";
        let file = s.preserve_for(Some(&id()), bytes, "rdl").unwrap();
        use base64::Engine;
        assert_eq!(s.read_native_binary(Some(&id()), &file, &[]).unwrap(), base64::engine::general_purpose::STANDARD.encode(bytes));
        assert_eq!(s.read_native_binary(None, &file, &[]).unwrap_err().code, "PAYLOAD_OWNERSHIP_DENIED");
        assert!(s.read_native_binary(None, &file, &[file.clone()]).is_ok());
        let xml = s.parse_native_xml(Some(&id()), &file, &[]).unwrap();
        assert_eq!(xml["name"], "Report");
        assert_eq!(xml["namespace"], "urn:report");
        let mut other = id();
        other.kind.push_str("Other");
        assert_eq!(s.read_native_binary(Some(&other), &file, &[file.clone()]).unwrap_err().code, "PAYLOAD_OWNERSHIP_DENIED");
        assert_eq!(
            s.parse_native_xml(Some(&other), &file, &[file.clone()])
                .unwrap_err()
                .code,
            "PAYLOAD_OWNERSHIP_DENIED"
        );
        assert_eq!(fs::read(s.root().join(&file.path)).unwrap(), bytes);
        fs::write(s.root().join(&file.path), b"<Changed/>").unwrap();
        assert_eq!(s.read_native_binary(Some(&id()), &file, &[]).unwrap_err().code, "INTEGRITY_FAILED");
        assert_eq!(
            s.parse_native_xml(Some(&id()), &file, &[])
                .unwrap_err()
                .code,
            "INTEGRITY_FAILED"
        );
    }

    #[test]
    fn reusing_committed_bytes_does_not_write_checkpoints_but_still_checks_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Storage::open(dir.path(), false, new_journal("reuse".into())).unwrap();
        let file = s.preserve_for(Some(&id()), b"original", "bin").unwrap();
        let tip = s.root().join(format!(
            "Herbert Data/runs/{}/checkpoint-v2/tip.json",
            s.journal.run_id
        ));
        let before = fs::read(&tip).unwrap();
        assert_eq!(
            s.preserve_for(Some(&id()), b"original", "bin").unwrap(),
            file
        );
        assert_eq!(fs::read(&tip).unwrap(), before);
        fs::write(s.root().join(&file.path), b"damaged").unwrap();
        assert!(s.preserve_for(Some(&id()), b"original", "bin").is_err());
    }

    #[test]
    fn raw_recovery_locking_and_user_edit_preservation() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Storage::open(dir.path(), false, new_journal("r".into())).unwrap();
        assert!(Storage::open(dir.path(), false, new_journal("r2".into())).is_err());
        let r = s.preserve_raw(b"original", "bin").unwrap();
        s.verify(&r).unwrap();
        s.journal.pending_files.push(r.clone());
        s.checkpoint().unwrap();
        s.reconcile_pending().unwrap();
        let (first, conflict) = s.write_derived(&id(), "definition.json", b"first").unwrap();
        assert!(!conflict);
        fs::write(s.root().join(&first.path), b"user edit").unwrap();
        let (second, conflict) = s
            .write_derived(&id(), "definition.json", b"second")
            .unwrap();
        assert!(conflict);
        assert_eq!(fs::read(s.root().join(&first.path)).unwrap(), b"user edit");
        s.verify(&second).unwrap();
        fs::write(s.root().join(&r.path), b"damaged").unwrap();
        assert!(s.verify(&r).is_err());
    }
    #[test]
    fn manifest_keeps_native_history_and_rejects_another_scope() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = new_journal("one".into());
        journal.endpoint = "https://a.test/".into();
        journal.company = "A".into();
        journal.caller = "User".into();
        let mut s = Storage::open(dir.path(), false, journal.clone()).unwrap();
        let first = s.preserve_raw(b"first", "bin").unwrap();
        let artifact = Artifact {
            identity: id(),
            classification: "selected".into(),
            acquisition: herbert_contract::Completeness::Partial,
            integrity: "hashVerified".into(),
            processing: "pending".into(),
            files: vec![first.clone()],
            dependencies: vec![],
            diagnostics: vec![],
            source_consistency: "nonAtomicUnknown".into(),
        };
        s.record_artifact(artifact.clone()).unwrap();
        drop(s);
        journal.run_id = Uuid::new_v4().to_string();
        let mut s = Storage::open(dir.path(), false, journal.clone()).unwrap();
        let second = s.preserve_raw(b"second", "bin").unwrap();
        let mut updated = artifact;
        updated.files = vec![second];
        s.record_artifact(updated).unwrap();
        assert_eq!(s.manifest.artifacts.len(), 1);
        assert_eq!(s.manifest.retained_files.len(), 2);
        s.verify(&first).unwrap();
        drop(s);
        journal.company = "B".into();
        assert_eq!(
            Storage::open(dir.path(), false, journal)
                .err()
                .unwrap()
                .code,
            "DESTINATION_SCOPE_MISMATCH"
        );
    }
    #[test]
    fn xml_archive_projection_checks_exact_owner_and_parsed_buffer_hash() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = Storage::open(dir.path(), false, new_journal("xml".into())).unwrap();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("data", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"<root><row>body</row></root>").unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        let file = storage.preserve_for(Some(&id()), &bytes, "zip").unwrap();
        let entries = vec!["data".into()];
        assert_eq!(
            storage
                .parse_native_xml_archive(Some(&id()), &file, &[], &entries)
                .unwrap()["documents"]["data"]["xml"]["name"],
            "root"
        );
        let mut other = id();
        other.kind.push_str("Other");
        assert_eq!(
            storage
                .parse_native_xml_archive(Some(&other), &file, &[file.clone()], &entries)
                .unwrap_err()
                .code,
            "PAYLOAD_OWNERSHIP_DENIED"
        );
        fs::write(storage.root().join(&file.path), b"tampered").unwrap();
        assert_eq!(
            storage
                .parse_native_xml_archive(Some(&id()), &file, &[], &entries)
                .unwrap_err()
                .code,
            "INTEGRITY_FAILED"
        );
    }
}
