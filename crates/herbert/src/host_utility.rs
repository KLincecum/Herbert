//! One run-owned generic helper. No plugin exporters, filters or business tables.
#[cfg(test)]
#[path = "host_utility_native_tests.rs"]
mod native_tests;
use super::*;
use crate::native_artifact_store::{ArtifactScope, OperationFile};
use crate::plugin_artifacts::NativeArtifactSnapshot;
use base64::Engine;
use std::io::Read;

pub(super) const OWNER: &str = "$herbert-host";
const ALIAS: &str = "host";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Transfer {
    operation_id: String,
    plugin: String,
    plugin_hash: String,
    identity: Option<Identity>,
    pub(super) phase: String,
    #[serde(default)]
    chunks: Vec<FileRef>,
    #[serde(default)]
    archive: Option<FileRef>,
    #[serde(default)]
    manifest: Option<Value>,
    #[serde(default)]
    individual_inputs: bool,
    #[serde(default)]
    input_downloads: BTreeMap<String, InputDownload>,
    #[serde(default)]
    input_batch_reads: Vec<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputDownload {
    expected: StagedFile,
    identity: Identity,
    chunks: Vec<FileRef>,
    file: Option<FileRef>,
}
fn error(message: &str) -> Diagnostic {
    Diagnostic::new("HOST_TRANSFER_UNVERIFIED", message)
}
fn decode_input_batch(
    response: &Value,
    pending: &[(&StagedInput, u64)],
    chunk_bytes: u32,
) -> Result<Vec<Vec<u8>>, Diagnostic> {
    let rows = response["files"]
        .as_array()
        .ok_or_else(|| error("The server omitted the native batch files."))?;
    if rows.len() != pending.len() {
        return Err(error("The server returned an incomplete native batch."));
    }
    rows.iter()
        .zip(pending)
        .map(|(row, (input, offset))| {
            if row["fileId"] != input.file.file_id {
                return Err(error(
                    "The server returned another file in the native batch.",
                ));
            }
            decode_chunk(
                row,
                *offset,
                input.file.bytes,
                &input.file.sha256,
                chunk_bytes,
            )
        })
        .collect()
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn native_version_selects_matching_embedded_host_package() {
        for (version, file, stamp) in [
            ("11.3.200.39", "HerbertHost-4.3.200.efxj", "4.3.200"),
            ("11.3.200.40", "HerbertHost-4.3.200.efxj", "4.3.200"),
            ("11.3.200", "HerbertHost-4.3.200.efxj", "4.3.200"),
            ("11.3.201.39", "HerbertHost-5.1.100.efxj", "5.1.100"),
            ("12.1.100.29", "HerbertHost-5.1.100.efxj", "5.1.100"),
            ("12.2.100.9", "HerbertHost-5.2.100.efxj", "5.2.100"),
        ] {
            let template = snapshot_for_version(version).unwrap();
            assert_eq!(template.declaration.file_for_version(version), file);
            let bytes = template.files[file].as_slice();
            let package: Value = serde_json::from_slice(
                bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes),
            ).unwrap();
            assert_eq!(package["Version"], stamp);
            assert_eq!(package["Library"]["EpicorVersion"], stamp);
        }
    }

    #[test]
    fn batch_receipts_reject_missing_repeated_reordered_and_changed_files() {
        let inputs = (0..25)
            .map(|i| StagedInput {
                file: StagedFile {
                    file_id: format!("{i:032x}.input"),
                    name: format!("{i}.baq"),
                    bytes: 3,
                    sha256: storage::sha256(b"abc"),
                },
                identity: Identity {
                    category: "fixture".into(),
                    kind: "Native".into(),
                    scope: Scope::Global,
                    keys: BTreeMap::from([("id".into(), i.to_string())]),
                },
            })
            .collect::<Vec<_>>();
        let pending = inputs.iter().map(|input| (input, 0)).collect::<Vec<_>>();
        let response = json!({"files": inputs.iter().map(|input|
            json!({"fileId":input.file.file_id,"offset":0,"total":3,
                "sha256":input.file.sha256,"data":"YWJj"})).collect::<Vec<_>>()});
        assert_eq!(
            decode_input_batch(&response, &pending, 1024).unwrap(),
            vec![b"abc".to_vec(); 25]
        );
        for defect in ["missing", "repeat", "order", "offset", "hash", "bytes"] {
            let mut changed = response.clone();
            match defect {
                "missing" => {
                    changed["files"].as_array_mut().unwrap().pop();
                }
                "repeat" => changed["files"][1] = changed["files"][0].clone(),
                "order" => changed["files"].as_array_mut().unwrap().swap(0, 1),
                "offset" => changed["files"][0]["offset"] = json!(1),
                "hash" => changed["files"][0]["sha256"] = json!("0".repeat(64)),
                _ => changed["files"][0]["data"] = json!("YQ=="),
            }
            assert!(
                decode_input_batch(&changed, &pending, 1024).is_err(),
                "{defect}"
            );
        }
        let resumed = vec![(&inputs[0], 2)];
        let tail = json!({"files":[{"fileId":inputs[0].file.file_id,"offset":2,"total":3,
            "sha256":inputs[0].file.sha256,"data":"Yw=="}]});
        assert_eq!(
            decode_input_batch(&tail, &resumed, 1024).unwrap(),
            vec![b"c".to_vec()]
        );
    }
}
/// Retain unfinished native files. A server receipt alone does not prove delivery.
fn verify_inputs(run: &Arc<Run>, transfer: &Transfer, state: &Value) -> Result<(), Diagnostic> {
    let storage = run
        .storage
        .lock()
        .map_err(|_| error("Run state is unavailable."))?;
    verify_input_receipts(&storage, transfer, state)
}
fn verify_input_receipts(
    storage: &Storage,
    transfer: &Transfer,
    state: &Value,
) -> Result<(), Diagnostic> {
    if !transfer.individual_inputs || state["phase"] != "open" {
        return Ok(());
    }
    let files: Vec<StagedFile> = serde_json::from_value(state["readyInputs"].clone())
        .map_err(|_| error("The server omitted its individual file inventory."))?;
    validate_files(&files)?;
    for expected in files {
        let downloaded = transfer.input_downloads.get(&expected.file_id)
            .filter(|d| d.expected == expected).and_then(|d| d.file.as_ref())
            .ok_or_else(|| error("A staged file remains available for recovery. Download and verify it before cleanup."))?;
        if downloaded.bytes != expected.bytes || downloaded.sha256 != expected.sha256 {
            return Err(error("The local file differs from the server receipt."));
        }
        storage.verify(downloaded)?;
    }
    Ok(())
}
fn snapshot() -> Result<NativeArtifactSnapshot, Diagnostic> {
    let declaration = serde_json::from_value(json!({
        "alias":ALIAS,"purpose":"requiredContent","coverage":"someContent",
        "type":"functionLibrary","file":"HerbertHost-5.1.100.efxj",
        "versionFiles":[{"epicorVersion":"12.2.100.9","file":"HerbertHost-5.2.100.efxj"}],
        "lifetime":"temporary","policy":"createInstance",
        "requiredFunctions":[{"function":"Execute","parameters":[
            {"name":"iRequest","dataType":"System.String","direction":"input"},
            {"name":"oJson","dataType":"System.String","direction":"output"}]},
            {"function":"Lease","parameters":[
            {"name":"iRequest","dataType":"System.String","direction":"input"},
            {"name":"oJson","dataType":"System.String","direction":"output"}]}],
        "effects":{"description":"Stages owned files, builds ZIP packages, transfers bytes, and removes its exact staged resources."}
    })).map_err(|_| error("The built-in host helper declaration is invalid."))?;
    Ok(NativeArtifactSnapshot {
        declaration,
        files: BTreeMap::from([
            (
                "HerbertHost-4.3.200.efxj".into(),
                Arc::new(include_bytes!("../../../helpers/host/HerbertHost-4.3.200.efxj").to_vec()),
            ),
            (
                "HerbertHost-5.1.100.efxj".into(),
                Arc::new(include_bytes!("../../../helpers/host/HerbertHost-5.1.100.efxj").to_vec()),
            ),
            (
                "HerbertHost-5.2.100.efxj".into(),
                Arc::new(include_bytes!("../../../helpers/host/HerbertHost-5.2.100.efxj").to_vec()),
            ),
        ]),
    })
}
// 2024.2 uses application/database release 11.3.200 and native EFx release
// 4.3.200. Its importer rejects the newer 5.1.100 package before compilation.
// Native acceptance was exercised on 11.3.200.39; revision routing follows the
// native release stamp and does not impose that tested revision as a hard cap.
// Keep this built-in-host choice local: plugin versionFiles retain exact matching.
pub(super) fn snapshot_for_version(version: &str) -> Result<NativeArtifactSnapshot, Diagnostic> {
    let mut template = snapshot()?;
    let release = version.rsplit_once('.')
        .filter(|(_, revision)| revision.parse::<u64>().is_ok())
        .map(|(release, _)| release);
    if version == "11.3.200" || release == Some("11.3.200") {
        template.declaration.file = "HerbertHost-4.3.200.efxj".into();
    }
    Ok(template)
}
fn uuid(value: &str) -> Result<String, Diagnostic> {
    uuid::Uuid::parse_str(value)
        .ok()
        .filter(|v| !v.is_nil())
        .map(|v| v.to_string())
        .ok_or_else(|| error("A nonempty operation UUID is required."))
}
pub(super) fn resume_operations(
    journal: &storage::RunJournal,
    plugin: &Plugin,
    identity: Option<&Identity>,
) -> Value {
    json!(
        journal
            .host_transfers
            .iter()
            .filter(|t| t.plugin == plugin.category.id
                && t.plugin_hash == plugin.hash
                && t.identity.as_ref() == identity
                && t.phase != "purged")
            .collect::<Vec<_>>()
    )
}
fn validate_files(files: &[StagedFile]) -> Result<(), Diagnostic> {
    let mut names = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    for file in files {
        storage::safe_relative(&file.name)?;
        let stem = file
            .file_id
            .strip_suffix(".input")
            .ok_or_else(|| error("A staged file ID is invalid."))?;
        if uuid(stem)?.replace('-', "") != stem
            || !ids.insert(&file.file_id)
            || file.sha256.len() != 64
            || !file
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(error("A staged file ID or hash is invalid or repeated."));
        }
        let key = storage::collision_key(&file.name);
        if !names.insert(key) {
            return Err(error("Archive output names collide."));
        }
    }
    for name in &names {
        for (index, _) in name.match_indices('/') {
            if names.contains(&name[..index]) {
                return Err(error("An archive file conflicts with a directory name."));
            }
        }
    }
    Ok(())
}
fn verify_archive(
    storage: &Storage,
    archive: &FileRef,
    files: &[StagedFile],
) -> Result<(), Diagnostic> {
    use sha2::{Digest, Sha256};
    validate_files(files)?;
    storage.verify(archive)?;
    let input = std::fs::File::open(storage.confined(&archive.path)?)
        .map_err(|_| error("The downloaded archive is unavailable."))?;
    let mut zip = zip::ZipArchive::new(input)
        .map_err(|_| error("The downloaded ZIP directory is invalid."))?;
    if zip.len() != files.len() {
        return Err(error(
            "The ZIP does not contain exactly the declared files.",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|_| error("A ZIP member cannot be read."))?;
        let expected = files
            .iter()
            .find(|f| f.name == entry.name())
            .ok_or_else(|| error("The ZIP contains an undeclared name."))?;
        if !seen.insert(expected.name.clone())
            || entry.is_dir()
            || entry.is_symlink()
            || entry.size() != expected.bytes
        {
            return Err(error(
                "A ZIP member has an invalid type, length or duplicate name.",
            ));
        }
        let mut digest = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 8192]; // Streaming buffer, not a supported payload limit.
        loop {
            let n = entry
                .read(&mut buffer)
                .map_err(|_| error("ZIP member decoding or CRC verification failed."))?;
            if n == 0 {
                break;
            }
            total = total
                .checked_add(n as u64)
                .ok_or_else(|| error("ZIP member length exceeds u64."))?;
            if total > expected.bytes {
                return Err(error("A ZIP member exceeds its declared length."));
            }
            digest.update(&buffer[..n]);
        }
        if total != expected.bytes || hex::encode(digest.finalize()) != expected.sha256 {
            return Err(error(
                "A ZIP member differs from its declared SHA256 or length.",
            ));
        }
    }
    Ok(())
}
impl Host {
    pub(super) async fn cleanup_host_transfers(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        selected: Option<&std::collections::BTreeSet<String>>,
    ) -> Vec<Diagnostic> {
        let journal = match collection::snapshot(run).await {
            Ok(j) => j,
            Err(e) => return vec![e],
        };
        if let Err(error) = self.access_store.location().and_then(|root| {
            crate::server_collection::ensure_run_settled(
                &root.join("server-collections"),
                &journal.run_id,
            )
        }) {
            return vec![error];
        }
        let mut errors = vec![];
        for transfer in journal.host_transfers {
            if transfer.phase == "purged" || selected.is_some_and(|p| !p.contains(&transfer.plugin))
            {
                continue;
            }
            let gate = self
                .utility_gates
                .lock()
                .await
                .entry((journal.run_id.clone(), transfer.operation_id.clone()))
                .or_default()
                .clone();
            let _guard = gate.lock().await;
            let result = self.cleanup_host_transfer(run, connection, transfer).await;
            if let Err(e) = result {
                run.activity
                    .note("Host helper", "Staging cleanup needs attention");
                errors.push(e);
            }
        }
        errors
    }
    async fn cleanup_host_transfer(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        mut transfer: Transfer,
    ) -> Result<(), Diagnostic> {
        // A canceled download must still attempt cleanup of its owned resources.
        let cancel = CancellationToken::new();
        let progress = run
            .activity
            .begin("Host helper", None, "Removing staged files");
        let state = self
            .utility_call(
                run,
                connection,
                &transfer.operation_id,
                json!({"action":"inspect"}),
                &cancel,
            )
            .await?;
        if state["exists"] != false {
            verify_inputs(run, &transfer, &state)?;
            let mut close = json!({"action":"close"});
            match state["phase"].as_str() {
                Some("ready") => {
                    let archive = transfer.archive.as_ref().ok_or_else(|| error(
                        "The server archive remains available for recovery. Download and verify it before cleanup."
                    ))?;
                    run.storage
                        .lock()
                        .map_err(|_| error("Run state is unavailable."))?
                        .verify(archive)?;
                    if state["sha256"].as_str() != Some(&archive.sha256) {
                        return Err(error("The archive changed before cleanup."));
                    }
                    close["sha256"] = json!(archive.sha256);
                }
                Some("open" | "closed" | "empty") => {}
                Some("packaging") => {
                    return Err(error(
                        "Packaging stopped before completion. Inspect the staged files before cleanup.",
                    ));
                }
                _ => return Err(error("The helper returned an unknown staging phase.")),
            }
            if state["phase"] != "closed" {
                let response = self
                    .utility_call(run, connection, &transfer.operation_id, close, &cancel)
                    .await?;
                if response["closed"] != true {
                    return Err(error(
                        "The helper did not confirm removal of the staged files.",
                    ));
                }
            }
            transfer.phase = "closed".into();
            self.save_transfer(run, transfer.clone()).await?;
            progress.step("Removing staging directory");
            let response = self
                .utility_call(
                    run,
                    connection,
                    &transfer.operation_id,
                    json!({"action":"purge"}),
                    &cancel,
                )
                .await?;
            if response["purged"] != true && response["exists"] != false {
                return Err(error(
                    "The helper did not confirm removal of the staging directory.",
                ));
            }
        }
        transfer.phase = "purged".into();
        self.save_transfer(run, transfer).await
    }
    pub(super) async fn prepare_host_utility(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let journal = collection::snapshot(run).await?;
        let key = (journal.run_id.clone(), OWNER.to_string());
        let mut operations = self.native_operations.lock().await;
        if operations.contains_key(&key) {
            return Ok(());
        }
        let snapshots = if journal.native_artifacts.iter().any(|r| r.plugin == OWNER) {
            vec![]
        } else {
            vec![snapshot_for_version(
                connection.server_version().await.as_deref().unwrap_or(""),
            )?]
        };
        let scope = ArtifactScope {
            endpoint: journal.endpoint,
            company: journal.company,
            caller: journal.caller,
        };
        let root = self.state_root.clone();
        let holder = journal.run_id;
        let file =
            blocking(move || OperationFile::open(&root, &holder, OWNER, scope, snapshots)).await?;
        // Check a recovered installation once before this process uses it.
        // Normal chunk reads reuse the checked identity.
        for artifact in &file.record.artifacts {
            if artifact.phase == "ready" {
                let expected = artifact
                    .identity
                    .as_ref()
                    .ok_or_else(|| error("The saved helper identity is missing."))?;
                let current = crate::native_artifact_backend::probe(
                    connection,
                    artifact.declaration.artifact_type,
                    &expected.id,
                    cancel,
                )
                .await?
                .ok_or_else(|| error("The saved host helper no longer exists."))?;
                if !crate::native_artifact_backend::owns(&current.0, expected) {
                    return Err(error(
                        "The saved host helper has a different owner or identity.",
                    ));
                }
            }
        }
        self.checkpoint_native_artifacts(run, file.record.clone())
            .await?;
        operations.insert(key, Arc::new(Mutex::new(file)));
        Ok(())
    }
    pub(super) async fn utility_call(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        operation: &str,
        mut request: Value,
        cancel: &CancellationToken,
    ) -> Result<Value, Diagnostic> {
        self.prepare_host_utility(run, connection, cancel).await?;
        let holder = collection::snapshot(run).await?.run_id;
        request["runId"] = json!(holder);
        request["operationId"] = json!(operation);
        // Hold the installation lock only until the helper is ready.
        // Different staging operations can then transfer data concurrently.
        let installation = self
            .native_artifact_action_for(
                OWNER,
                HostAction::ArtifactInstall {
                    alias: ALIAS.into(),
                },
                &holder,
                run,
                connection,
                cancel,
            )
            .await?;
        let library = installation.value["id"]
            .as_str()
            .ok_or_else(|| error("The host helper identity is missing."))?;
        let has_lease = self.server_leases.lock().await.get(&holder)
            .is_some_and(|keeper| keeper.uses_library(library));
        if !has_lease {
            let identity = crate::native_artifact_backend::probe(connection, NativeArtifactType::FunctionLibrary, library, cancel)
                .await?.ok_or_else(|| error("The host helper disappeared before lease registration."))?.0;
            self.keep_helper_alive(run, connection, &identity, true, cancel).await?;
        }
        loop {
        let response = connection
            .artifact_function_call(
                library,
                "Execute",
                json!({"iRequest":request.to_string()}),
                cancel,
            )
            .await.map_err(|mut diagnostic| {
                diagnostic.message = format!("Host helper {}: {}", request["action"].as_str().unwrap_or("request"), diagnostic.message);
                diagnostic
            })?;
        let text = response
            .value
            .get("oJson")
            .or_else(|| response.value.pointer("/returnObj/oJson"))
            .and_then(Value::as_str)
            .ok_or_else(|| error("The host helper omitted its JSON response."))?;
        let value: Value = serde_json::from_str(text).map_err(|_| error("The host helper returned malformed JSON."))?;
        if let Some(details) = value.get("leaseAdmissionError") {
            return Err(error(&format!("Server lease admission failed: {details}")));
        }
        if value["leaseBusy"] != true { return Ok(value); }
        // Retry only explicit admission contention. An uncertain mutation is
        // never replayed by this loop.
        tokio::select! { _ = cancel.cancelled() => return Err(error("The host request was canceled.")),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {} }
        }
    }
    pub(super) async fn host_utility(
        &self,
        plugin: &Plugin,
        operation: &str,
        request: &HostUtilityRequest,
        identity: Option<&Identity>,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        cancel: &CancellationToken,
    ) -> Result<(Value, Vec<FileRef>), Diagnostic> {
        let journal = collection::snapshot(run).await?;
        crate::collection_preferences::admit(journal.collection_method, HelperPurpose::ServerWork)?;
        let operation = uuid(operation)?;
        let gate = self
            .utility_gates
            .lock()
            .await
            .entry((journal.run_id, operation.clone()))
            .or_default()
            .clone();
        let _guard = tokio::select! { _=cancel.cancelled()=>return Err(error("The host utility operation was canceled.")),g=gate.lock()=>g };
        let journal = collection::snapshot(run).await?;
        let saved = journal
            .host_transfers
            .iter()
            .find(|t| t.operation_id == operation);
        if let Some(saved) = saved {
            if saved.plugin != plugin.category.id
                || saved.plugin_hash != plugin.hash
                || (saved.identity.is_some() && saved.identity.as_ref() != identity)
            {
                return Err(error(
                    "The staged operation belongs to different plugin or artifact facts.",
                ));
            }
            if saved.phase == "purged" {
                return Err(error(
                    "The staged operation is already purged. Use a new operation ID.",
                ));
            }
            if saved.phase == "closed"
                && !matches!(
                    request,
                    HostUtilityRequest::Inspect
                        | HostUtilityRequest::Close
                        | HostUtilityRequest::Purge
                )
            {
                return Err(error(
                    "The staged operation is closed. Use a new operation ID for new work.",
                ));
            }
        } else {
            if !matches!(request, HostUtilityRequest::Begin) {
                return Err(error(
                    "Begin an owned operation before using server staging.",
                ));
            }
            let transfer = Transfer {
                operation_id: operation.clone(),
                plugin: plugin.category.id.clone(),
                plugin_hash: plugin.hash.clone(),
                identity: identity.cloned(),
                phase: "admitted".into(),
                chunks: vec![],
                archive: None,
                manifest: None,
                individual_inputs: false,
                input_downloads: BTreeMap::new(),
                input_batch_reads: vec![],
            };
            collection::update(run, move |s| {
                s.journal.host_transfers.push(transfer);
                Ok(())
            })
            .await?;
        }
        if let HostUtilityRequest::Put {
            file_id,
            name,
            offset,
            ..
        } = request
        {
            uuid(file_id)?;
            storage::safe_relative(name)?;
            if *offset > i64::MAX as u64 {
                return Err(error(
                    "The offset exceeds the native Int64 stream position.",
                ));
            }
        }
        if let HostUtilityRequest::Reserve { files } | HostUtilityRequest::ReserveInputs { files } =
            request
        {
            let manifest = files
                .iter()
                .map(|file| {
                    Ok(StagedFile {
                        file_id: format!("{}.input", uuid(&file.file_id)?.replace('-', "")),
                        name: file.name.clone(),
                        bytes: 0,
                        sha256: storage::sha256(b""),
                    })
                })
                .collect::<Result<Vec<_>, Diagnostic>>()?;
            validate_files(&manifest)?;
        }
        if let HostUtilityRequest::DownloadInputs {
            files,
            identity: selected,
            chunk_bytes,
        } = request
        {
            if files.is_empty()
                || !files.iter().any(|file| &file.identity == selected)
                || identity.is_some_and(|owner| owner != selected)
                || files.iter().any(|file| {
                    file.identity.category != plugin.category.id
                        || !super::identity_in_scope(&file.identity, &connection.company)
                })
            {
                return Err(error("The native batch belongs to another artifact scope."));
            }
            return self
                .download_host_inputs(
                    run,
                    connection,
                    &operation,
                    &plugin.category.name,
                    files,
                    selected,
                    *chunk_bytes,
                    cancel,
                )
                .await;
        }
        if let HostUtilityRequest::DownloadInput {
            file,
            identity: owner,
            chunk_bytes,
        } = request
        {
            if owner.category != plugin.category.id
                || !super::identity_in_scope(owner, &connection.company)
                || identity.is_some_and(|id| id != owner)
            {
                return Err(error("The file belongs to another artifact scope."));
            }
            return self
                .download_host_input(
                    run,
                    connection,
                    &operation,
                    owner,
                    file,
                    *chunk_bytes,
                    cancel,
                )
                .await;
        }
        if let HostUtilityRequest::Seal { files, .. } = request {
            validate_files(files)?;
        }
        if let HostUtilityRequest::Download { chunk_bytes } = request {
            let identity =
                identity.ok_or_else(|| error("Archive download requires an artifact identity."))?;
            return self
                .download_host_archive(run, connection, &operation, identity, *chunk_bytes, cancel)
                .await;
        }
        let mut wire =
            serde_json::to_value(request).map_err(|_| error("The utility request is invalid."))?;
        if matches!(request, HostUtilityRequest::ReserveInputs { .. }) {
            // Record the cleanup rule before the server can accept reserved files.
            let mut transfer = collection::snapshot(run)
                .await?
                .host_transfers
                .into_iter()
                .find(|t| t.operation_id == operation)
                .unwrap();
            transfer.individual_inputs = true;
            self.save_transfer(run, transfer).await?;
            wire["action"] = json!("reserve");
        }
        if matches!(request, HostUtilityRequest::Close) {
            let state = self
                .utility_call(
                    run,
                    connection,
                    &operation,
                    json!({"action":"inspect"}),
                    cancel,
                )
                .await?;
            let journal = collection::snapshot(run).await?;
            let transfer = journal
                .host_transfers
                .iter()
                .find(|t| t.operation_id == operation)
                .unwrap();
            verify_inputs(run, transfer, &state)?;
            if state["phase"] == "ready" {
                let journal = collection::snapshot(run).await?;
                let transfer = journal
                    .host_transfers
                    .iter()
                    .find(|t| t.operation_id == operation)
                    .unwrap();
                let archive = transfer.archive.as_ref().ok_or_else(|| {
                    error("Download and verify the ready archive before cleanup.")
                })?;
                run.storage
                    .lock()
                    .map_err(|_| error("Run state is unavailable."))?
                    .verify(archive)?;
                if state["sha256"].as_str() != Some(&archive.sha256) {
                    return Err(error("The server archive changed after verification."));
                }
                wire["sha256"] = json!(archive.sha256);
            }
        }
        // No automatic write replay. The retained operation ID supports explicit inspect/reconciliation.
        let result = self
            .utility_call(run, connection, &operation, wire, cancel)
            .await?;
        let phase = match request {
            HostUtilityRequest::Begin if result["phase"] == "open" => Some("open"),
            HostUtilityRequest::Seal { .. } if result["phase"] == "ready" => Some("ready"),
            HostUtilityRequest::Close if result["closed"] == true || result["exists"] == false => {
                Some("closed")
            }
            HostUtilityRequest::Purge if result["purged"] == true || result["exists"] == false => {
                Some("purged")
            }
            _ => None,
        };
        if let Some(phase) = phase {
            let operation = operation.clone();
            collection::update(run, move |s| {
                s.journal
                    .host_transfers
                    .iter_mut()
                    .find(|t| t.operation_id == operation)
                    .unwrap()
                    .phase = phase.into();
                Ok(())
            })
            .await?;
        }
        Ok((result, vec![]))
    }
    async fn download_host_archive(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        operation: &str,
        identity: &Identity,
        chunk_bytes: u32,
        cancel: &CancellationToken,
    ) -> Result<(Value, Vec<FileRef>), Diagnostic> {
        // Int32 is the actual Function buffer length contract; this is not a workload cap.
        if chunk_bytes == 0 || chunk_bytes > i32::MAX as u32 {
            return Err(error(
                "Chunk length must fit a positive native Int32 buffer length.",
            ));
        }
        let state = self
            .utility_call(
                run,
                connection,
                operation,
                json!({"action":"inspect"}),
                cancel,
            )
            .await?;
        if state["phase"] != "ready" {
            return Err(error("The server archive is not ready."));
        }
        let bytes = state["bytes"]
            .as_u64()
            .ok_or_else(|| error("The archive length is missing."))?;
        let hash = state["sha256"]
            .as_str()
            .ok_or_else(|| error("The archive hash is missing."))?
            .to_string();
        let manifest: Vec<StagedFile> = serde_json::from_value(state["files"].clone())
            .map_err(|_| error("The archive manifest is invalid."))?;
        validate_files(&manifest)?;
        let signature = json!({"bytes":bytes,"sha256":hash,"files":manifest});
        let mut transfer = collection::snapshot(run)
            .await?
            .host_transfers
            .into_iter()
            .find(|t| t.operation_id == operation)
            .unwrap();
        if transfer
            .manifest
            .as_ref()
            .is_some_and(|old| old != &signature)
        {
            return Err(error("The ready archive changed during transfer."));
        }
        transfer.manifest = Some(signature);
        let storage = run.storage.clone();
        let chunks = transfer.chunks.clone();
        let mut offset = blocking(move || {
            let s = storage
                .lock()
                .map_err(|_| error("Run state is unavailable."))?;
            let mut offset = 0u64;
            for chunk in &chunks {
                s.verify(chunk)?;
                offset = offset
                    .checked_add(chunk.bytes)
                    .ok_or_else(|| error("Transfer length exceeds u64."))?;
            }
            Ok(offset)
        })
        .await?;
        if offset > bytes {
            return Err(error("Retained chunks exceed the server archive length."));
        }
        self.save_transfer(run, transfer.clone()).await?;
        while offset < bytes {
            let value = self
                .utility_call(
                    run,
                    connection,
                    operation,
                    json!({"action":"read","offset":offset,"count":chunk_bytes}),
                    cancel,
                )
                .await?;
            let data = decode_chunk(&value, offset, bytes, &hash, chunk_bytes)?;
            let storage = run.storage.clone();
            let owner = identity.clone();
            let chunk = blocking(move || {
                storage
                    .lock()
                    .map_err(|_| error("Run state is unavailable."))?
                    .preserve_for(Some(&owner), &data, "bin")
            })
            .await?;
            offset += chunk.bytes;
            transfer.chunks.push(chunk);
            self.save_transfer(run, transfer.clone()).await?;
        }
        let storage = run.storage.clone();
        let owner = identity.clone();
        let chunks = transfer.chunks.clone();
        let archive = blocking(move || {
            let mut s = storage
                .lock()
                .map_err(|_| error("Run state is unavailable."))?;
            let archive = s.assemble(&owner, &chunks, bytes, &hash, "zip")?;
            verify_archive(&s, &archive, &manifest)?;
            Ok(archive)
        })
        .await?;
        transfer.archive = Some(archive.clone());
        transfer.phase = "verified".into();
        self.save_transfer(run, transfer.clone()).await?;
        Ok((
            json!({"archive":archive,"manifest":transfer.manifest}),
            vec![archive],
        ))
    }
    pub(super) async fn download_host_inputs(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        operation: &str,
        plugin_name: &str,
        files: &[StagedInput],
        selected: &Identity,
        chunk_bytes: u32,
        cancel: &CancellationToken,
    ) -> Result<(Value, Vec<FileRef>), Diagnostic> {
        validate_files(
            &files
                .iter()
                .map(|input| input.file.clone())
                .collect::<Vec<_>>(),
        )?;
        if chunk_bytes == 0 || chunk_bytes > i32::MAX as u32 {
            return Err(error(
                "Chunk length must fit a positive native Int32 buffer length.",
            ));
        }
        let mut transfer = collection::snapshot(run)
            .await?
            .host_transfers
            .into_iter()
            .find(|transfer| transfer.operation_id == operation)
            .unwrap();
        if !transfer.individual_inputs {
            return Err(error(
                "Reserve individual files before downloading a batch.",
            ));
        }
        if files.iter().all(|input| {
            transfer
                .input_downloads
                .get(&input.file.file_id)
                .is_some_and(|saved| {
                    saved.expected == input.file
                        && saved.identity == input.identity
                        && saved.file.is_some()
                })
        }) {
            let input = files
                .iter()
                .find(|input| &input.identity == selected)
                .unwrap();
            return self
                .download_host_input(
                    run,
                    connection,
                    operation,
                    selected,
                    &input.file,
                    chunk_bytes,
                    cancel,
                )
                .await;
        }
        let batch = run.activity.begin_batch(plugin_name, files.len());
        // Validate all retained receipts before making a request. An interrupted
        // batch resumes each file at its own locally verified offset.
        for input in files {
            let download = transfer
                .input_downloads
                .entry(input.file.file_id.clone())
                .or_insert_with(|| InputDownload {
                    expected: input.file.clone(),
                    identity: input.identity.clone(),
                    chunks: vec![],
                    file: None,
                });
            if download.expected != input.file || download.identity != input.identity {
                return Err(error("A native batch receipt changed during transfer."));
            }
            let storage = run
                .storage
                .lock()
                .map_err(|_| error("Run state is unavailable."))?;
            for chunk in &download.chunks {
                storage.verify(chunk)?;
            }
            if let Some(file) = &download.file {
                storage.verify(file)?;
            }
        }
        loop {
            let mut pending = Vec::new();
            for input in files {
                let download = &transfer.input_downloads[&input.file.file_id];
                let offset = download.chunks.iter().try_fold(0u64, |total, chunk| {
                    total
                        .checked_add(chunk.bytes)
                        .ok_or_else(|| error("Transfer length exceeds u64."))
                })?;
                if offset > input.file.bytes {
                    return Err(error("Retained chunks exceed the native file length."));
                }
                if download.file.is_none() && offset < input.file.bytes {
                    pending.push((input, offset));
                }
            }
            if pending.is_empty() {
                break;
            }
            let reads = pending.iter().map(|(input, offset)|
                json!({"fileId":input.file.file_id,"offset":offset,"count":chunk_bytes})).collect::<Vec<_>>();
            let response = self
                .utility_call(
                    run,
                    connection,
                    operation,
                    json!({"action":"readInputs","files":reads}),
                    cancel,
                )
                .await?;
            let chunks = decode_input_batch(&response, &pending, chunk_bytes)?;
            // Commit only after every response member has passed identity, size,
            // offset and hash checks. Never accept a partial or reordered batch.
            for ((input, _), bytes) in pending.iter().zip(chunks) {
                let chunk = run
                    .storage
                    .lock()
                    .map_err(|_| error("Run state is unavailable."))?
                    .preserve_for(Some(&input.identity), &bytes, "bin")?;
                transfer
                    .input_downloads
                    .get_mut(&input.file.file_id)
                    .unwrap()
                    .chunks
                    .push(chunk);
            }
            transfer.input_batch_reads.push(pending.len());
            self.save_transfer(run, transfer.clone()).await?;
        }
        self.save_transfer(run, transfer).await?;
        // Assembly uses the existing native-file verification and receipt path.
        // All bytes are now local, so these calls do not make individual reads.
        let mut result = None;
        batch.update("Verifying", 0);
        for (index, input) in files.iter().enumerate() {
            let verified = self
                .download_host_input(
                    run,
                    connection,
                    operation,
                    &input.identity,
                    &input.file,
                    chunk_bytes,
                    cancel,
                )
                .await?;
            batch.update("Verifying", index + 1);
            if &input.identity == selected {
                result = Some(verified);
            }
        }
        let result = result.ok_or_else(|| error("The selected artifact is missing from its batch."))?;
        batch.complete();
        Ok(result)
    }
    async fn download_host_input(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        operation: &str,
        identity: &Identity,
        expected: &StagedFile,
        chunk_bytes: u32,
        cancel: &CancellationToken,
    ) -> Result<(Value, Vec<FileRef>), Diagnostic> {
        validate_files(std::slice::from_ref(expected))?;
        if chunk_bytes == 0 || chunk_bytes > i32::MAX as u32 {
            return Err(error(
                "Chunk length must fit a positive native Int32 buffer length.",
            ));
        }
        let mut transfer = collection::snapshot(run)
            .await?
            .host_transfers
            .into_iter()
            .find(|t| t.operation_id == operation)
            .unwrap();
        if !transfer.individual_inputs {
            return Err(error("Reserve individual files before downloading them."));
        }
        let mut download = transfer
            .input_downloads
            .get(&expected.file_id)
            .cloned()
            .unwrap_or(InputDownload {
                expected: expected.clone(),
                identity: identity.clone(),
                chunks: vec![],
                file: None,
            });
        if download.expected != *expected || download.identity != *identity {
            return Err(error("The native file receipt changed during transfer."));
        }
        let mut offset = 0u64;
        {
            let storage = run
                .storage
                .lock()
                .map_err(|_| error("Run state is unavailable."))?;
            for chunk in &download.chunks {
                storage.verify(chunk)?;
                offset = offset
                    .checked_add(chunk.bytes)
                    .ok_or_else(|| error("Transfer length exceeds u64."))?;
            }
            if let Some(file) = &download.file {
                storage.verify(file)?;
                return Ok((json!({"nativeFile":file}), vec![file.clone()]));
            }
        }
        if offset > expected.bytes {
            return Err(error("Retained chunks exceed the native file length."));
        }
        while offset < expected.bytes {
            let response = self.utility_call(run, connection, operation,
                json!({"action":"readInput","fileId":expected.file_id,"offset":offset,"count":chunk_bytes}), cancel).await?;
            if response["fileId"] != expected.file_id {
                return Err(error("The server returned another file."));
            }
            let bytes = decode_chunk(
                &response,
                offset,
                expected.bytes,
                &expected.sha256,
                chunk_bytes,
            )?;
            let chunk = run
                .storage
                .lock()
                .map_err(|_| error("Run state is unavailable."))?
                .preserve_for(Some(identity), &bytes, "bin")?;
            offset += chunk.bytes;
            download.chunks.push(chunk);
            transfer
                .input_downloads
                .insert(expected.file_id.clone(), download.clone());
            self.save_transfer(run, transfer.clone()).await?;
        }
        // Keep the native extension so ordinary output processing can name the export.
        let extension = std::path::Path::new(&expected.name)
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric()))
            .unwrap_or("bin");
        let file = run
            .storage
            .lock()
            .map_err(|_| error("Run state is unavailable."))?
            .assemble(
                identity,
                &download.chunks,
                expected.bytes,
                &expected.sha256,
                extension,
            )?;
        download.file = Some(file.clone());
        transfer
            .input_downloads
            .insert(expected.file_id.clone(), download);
        self.save_transfer(run, transfer).await?;
        Ok((json!({"nativeFile":file}), vec![file]))
    }
    async fn save_transfer(&self, run: &Arc<Run>, transfer: Transfer) -> Result<(), Diagnostic> {
        let id = transfer.operation_id.clone();
        collection::update(run, move |s| {
            *s.journal
                .host_transfers
                .iter_mut()
                .find(|t| t.operation_id == id)
                .ok_or_else(|| error("The transfer admission is missing."))? = transfer;
            Ok(())
        })
        .await
    }
}
fn decode_chunk(
    value: &Value,
    offset: u64,
    total: u64,
    hash: &str,
    count: u32,
) -> Result<Vec<u8>, Diagnostic> {
    if offset > total {
        return Err(error("The chunk offset exceeds the archive length."));
    }
    if value["offset"].as_u64() != Some(offset)
        || value["total"].as_u64() != Some(total)
        || value["sha256"].as_str() != Some(hash)
    {
        return Err(error("The chunk belongs to different archive metadata."));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(
            value["data"]
                .as_str()
                .ok_or_else(|| error("The chunk data is missing."))?,
        )
        .map_err(|_| error("The chunk data is not base64."))?;
    if bytes.is_empty() || bytes.len() as u64 != (total - offset).min(count as u64) {
        return Err(error("The chunk length differs from the requested range."));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    fn file(name: &str, data: &[u8]) -> StagedFile {
        StagedFile {
            file_id: format!("{}.input", uuid::Uuid::new_v4().simple()),
            name: name.into(),
            bytes: data.len() as u64,
            sha256: storage::sha256(data),
        }
    }
    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    #[test]
    fn both_embedded_packages_prepare_with_exact_owned_identity_and_signature() {
        let source = snapshot().unwrap();
        PluginManifest {
            artifacts: vec![source.declaration.clone()],
        }
        .ordered_artifacts()
        .unwrap();
        for bytes in source.files.values() {
            let owner = uuid::Uuid::new_v4();
            let prepared = crate::native_artifact_package::prepare(
                &source.declaration,
                bytes,
                "OwnedHostFixture",
                owner,
                "C",
                "caller",
            )
            .unwrap();
            let package: Value = serde_json::from_slice(
                prepared
                    .bytes
                    .strip_prefix(&[0xef, 0xbb, 0xbf])
                    .unwrap_or(&prepared.bytes),
            )
            .unwrap();
            assert_eq!(package["LibraryId"], "OwnedHostFixture");
            assert_eq!(package["Library"]["GlobalID"], owner.to_string());
            assert_eq!(package["Library"]["OwnedByCompany"], "C");
            crate::native_artifact_backend::verify_functions(&source.declaration, &package, false)
                .unwrap();
        }
        let root = tempfile::tempdir().unwrap();
        let run = uuid::Uuid::new_v4().to_string();
        let scope = ArtifactScope {
            endpoint: "https://fixture.invalid/ERP".into(),
            company: "C".into(),
            caller: "caller".into(),
        };
        let first =
            OperationFile::open(root.path(), &run, OWNER, scope.clone(), vec![source]).unwrap();
        let record = first.record.clone();
        drop(first);
        let recovered = OperationFile::open(root.path(), &run, OWNER, scope, vec![]).unwrap();
        assert_eq!(recovered.record, record);
    }
    #[test]
    fn manifest_rejects_traversal_duplicates_and_file_directory_collisions() {
        validate_files(&[file("nested/π.json", b""), file("other", b"x")]).unwrap();
        for names in [
            vec!["../a"],
            vec!["a", "A"],
            vec!["a", "a/b"],
            vec!["a/./b"],
            vec!["NUL.txt"],
        ] {
            assert!(
                validate_files(&names.into_iter().map(|n| file(n, b"")).collect::<Vec<_>>())
                    .is_err()
            );
        }
        let first = file("a", b"");
        let mut second = first.clone();
        second.name = "b".into();
        assert!(validate_files(&[first, second]).is_err());
        let mut wrong = file("a", b"");
        wrong.sha256 = "invalid".into();
        assert!(validate_files(&[wrong]).is_err());
    }
    #[test]
    fn chunks_require_exact_range_metadata_and_data_length() {
        let value = json!({"offset":3,"total":8,"sha256":"hash","data":"YWJj"});
        assert_eq!(decode_chunk(&value, 3, 8, "hash", 3).unwrap(), b"abc");
        for patch in [
            json!({"offset":0}),
            json!({"total":9}),
            json!({"sha256":"changed"}),
            json!({"data":""}),
            json!({"data":"!"}),
            json!({"data":"YWI="}),
        ] {
            let mut changed = value.clone();
            changed
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(decode_chunk(&changed, 3, 8, "hash", 3).is_err());
        }
        assert!(decode_chunk(&value, 9, 8, "hash", 3).is_err());
    }
    #[test]
    fn downloaded_archive_must_match_every_declared_member() {
        let temp = tempfile::tempdir().unwrap();
        let mut s = Storage::open(temp.path(), false, storage::new_journal("test".into())).unwrap();
        let manifest = vec![file("nested/π.json", b"content"), file("empty", b"")];
        let bytes = archive(&[("nested/π.json", b"content"), ("empty", b"")]);
        let good = s.preserve_raw(&bytes, "zip").unwrap();
        verify_archive(&s, &good, &manifest).unwrap();
        for bytes in [
            archive(&[("nested/π.json", b"changed"), ("empty", b"")]),
            archive(&[("nested/π.json", b"content")]),
            archive(&[("../escape", b"content"), ("empty", b"")]),
            b"not a zip".to_vec(),
        ] {
            let bad = s.preserve_raw(&bytes, "zip").unwrap();
            assert!(verify_archive(&s, &bad, &manifest).is_err());
        }
        // Physical local edits cannot authorize server cleanup.
        std::fs::write(s.confined(&good.path).unwrap(), b"tampered").unwrap();
        assert!(verify_archive(&s, &good, &manifest).is_err());
    }
    #[test]
    fn transfer_receipts_survive_restart_without_promoting_legacy_runs() {
        let mut journal = storage::new_journal(uuid::Uuid::new_v4().to_string());
        journal.host_transfers.push(Transfer {
            operation_id: uuid::Uuid::new_v4().to_string(),
            plugin: "fixture".into(),
            plugin_hash: "hash".into(),
            identity: None,
            phase: "ready".into(),
            chunks: vec![],
            archive: None,
            manifest: Some(json!({"sha256":"retained"})),
            individual_inputs: false,
            input_downloads: BTreeMap::new(),
            input_batch_reads: vec![],
        });
        let wire = serde_json::to_vec(&journal).unwrap();
        let restored: storage::RunJournal = serde_json::from_slice(&wire).unwrap();
        assert_eq!(restored.host_transfers, journal.host_transfers);
        let mut legacy = serde_json::to_value(&journal).unwrap();
        legacy.as_object_mut().unwrap().remove("collectionMethod");
        legacy.as_object_mut().unwrap().remove("hostTransfers");
        let restored: storage::RunJournal = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.collection_method, CollectionMethod::ServerAssisted);
        assert!(restored.host_transfers.is_empty());
        // Server Assisted permits required helpers and optional server work.
        assert!(crate::collection_preferences::admit(
            CollectionMethod::ServerAssisted, snapshot().unwrap().declaration.purpose).is_ok());
        assert!(crate::collection_preferences::admit(
            CollectionMethod::DirectOnly, snapshot().unwrap().declaration.purpose).is_err());
        for mode in [CollectionMethod::DirectOnly] {
            assert!(
                crate::collection_preferences::admit(mode, HelperPurpose::ServerWork)
                    .is_err()
            );
        }
    }
    #[test]
    fn individual_file_cleanup_requires_unchanged_local_bytes_for_every_server_file() {
        let root = tempfile::tempdir().unwrap();
        let mut storage =
            Storage::open(root.path(), false, storage::new_journal("inputs".into())).unwrap();
        let expected = file("native.baq", b"native bytes");
        let local = storage.preserve_raw(b"native bytes", "bin").unwrap();
        let identity = Identity {
            category: "fixture".into(),
            kind: "Native".into(),
            scope: Scope::Global,
            keys: BTreeMap::new(),
        };
        let mut transfer = Transfer {
            operation_id: uuid::Uuid::new_v4().to_string(),
            plugin: "fixture".into(),
            plugin_hash: "hash".into(),
            identity: None,
            phase: "open".into(),
            chunks: vec![],
            archive: None,
            manifest: None,
            individual_inputs: true,
            input_downloads: BTreeMap::new(),
            input_batch_reads: vec![],
        };
        let state = json!({"phase":"open","readyInputs":[expected]});
        assert!(verify_input_receipts(&storage, &transfer, &state).is_err());
        transfer.input_downloads.insert(
            expected.file_id.clone(),
            InputDownload {
                expected: expected.clone(),
                identity,
                chunks: vec![local.clone()],
                file: Some(local.clone()),
            },
        );
        verify_input_receipts(&storage, &transfer, &state).unwrap();
        let saved: Transfer =
            serde_json::from_slice(&serde_json::to_vec(&transfer).unwrap()).unwrap();
        assert_eq!(saved, transfer);
        let mut changed = state.clone();
        changed["readyInputs"][0]["sha256"] = json!("b".repeat(64));
        assert!(verify_input_receipts(&storage, &transfer, &changed).is_err());
        assert!(verify_input_receipts(&storage, &transfer, &json!({"phase":"open"})).is_err());
        std::fs::write(storage.confined(&local.path).unwrap(), b"changed").unwrap();
        assert!(verify_input_receipts(&storage, &transfer, &state).is_err());
    }
}
