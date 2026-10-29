//! Installation, execution and teardown of a plugin's frozen native artifacts.
use super::*;
use crate::{
    native_artifact_backend as backend, native_artifact_package as package,
    native_artifact_store::{ArtifactScope, OperationFile, OperationRecord},
};
use std::collections::BTreeSet;

fn issue(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn helper_setup_transient(error: &Diagnostic) -> bool {
    matches!(error.code.as_str(), "TRANSPORT_FAILED" | "TRANSFER_STALLED" | "TRANSFER_TRUNCATED" | "SERVICE_UNAVAILABLE" | "THROTTLED")
        || matches!(error.context.get("httpStatus").and_then(Value::as_u64), Some(408 | 429 | 500 | 502 | 503 | 504))
}
fn response(value: Value) -> Result<crate::connection::NativeResponse, Diagnostic> {
    let raw = serde_json::to_vec(&value).map_err(|_| {
        issue(
            "ARTIFACT_RESPONSE_INVALID",
            "Cannot serialize the resolved artifact identity.",
        )
    })?;
    Ok(crate::connection::NativeResponse {
        value,
        raw,
        elapsed_ms: 0,
    })
}
impl Host {
    pub(super) async fn prepare_native_artifacts(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        plugins: &[Plugin],
    ) -> BTreeSet<String> {
        let mut refused = BTreeSet::new();
        let journal = match collection::snapshot(run).await {
            Ok(j) => j,
            Err(_) => return plugins.iter().map(|p| p.category.id.clone()).collect(),
        };
        // Read every selected plugin before native discovery starts. No running
        // action later consults mutable files in the author's package directory.
        let mut frozen = Vec::new();
        for plugin in plugins {
            let existing = journal
                .native_artifacts
                .iter()
                .find(|r| crate::legacy_category_ids::same(&r.plugin, &plugin.category.id));
            let snapshots = if existing.is_some() {
                Ok(Vec::new())
            } else {
                plugin.snapshot_artifacts().await
            };
            match snapshots {
                Ok(snapshots) if !snapshots.is_empty() || existing.is_some() => {
                    frozen.push((plugin.category.id.clone(), snapshots))
                }
                Ok(_) => {}
                Err(error) => {
                    refused.insert(plugin.category.id.clone());
                    let _ = super::record_diagnostic(run, error).await;
                }
            }
        }
        for (plugin, snapshots) in frozen {
            let scope = ArtifactScope {
                endpoint: journal.endpoint.clone(),
                company: journal.company.clone(),
                caller: journal.caller.clone(),
            };
            let root = self.state_root.clone();
            let holder = journal.run_id.clone();
            let category = plugin.clone();
            let opened =
                blocking(move || OperationFile::open(&root, &holder, &category, scope, snapshots))
                    .await;
            match opened {
                Ok(file) => {
                    let record = file.record.clone();
                    self.native_operations.lock().await.insert(
                        (journal.run_id.clone(), plugin.clone()),
                        Arc::new(Mutex::new(file)),
                    );
                    if let Err(error) = self.checkpoint_native_artifacts(run, record).await {
                        refused.insert(plugin);
                        let _ = super::record_diagnostic(run, error).await;
                    }
                }
                Err(error) => {
                    refused.insert(plugin);
                    let _ = super::record_diagnostic(run, error).await;
                }
            }
        }
        let _ = connection;
        refused
    }
    pub(super) async fn checkpoint_native_artifacts(
        &self,
        run: &Arc<Run>,
        record: OperationRecord,
    ) -> Result<(), Diagnostic> {
        collection::update(run, move |storage| {
            if let Some(old) = storage
                .journal
                .native_artifacts
                .iter_mut()
                .find(|r| crate::legacy_category_ids::same(&r.plugin, &record.plugin))
            {
                *old = record;
            } else {
                storage.journal.native_artifacts.push(record);
            }
            Ok(())
        })
        .await
    }
    pub(super) async fn native_artifact_action(
        &self,
        plugin: &Plugin,
        action: HostAction,
        holder: &str,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        cancel: &CancellationToken,
    ) -> Result<crate::connection::NativeResponse, Diagnostic> {
        self.native_artifact_action_for(&plugin.category.id, action, holder, run, connection, cancel).await
    }
    pub(super) async fn native_artifact_action_for(
        &self,
        owner: &str,
        action: HostAction,
        holder: &str,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        cancel: &CancellationToken,
    ) -> Result<crate::connection::NativeResponse, Diagnostic> {
        let alias = match &action {
            HostAction::ArtifactInstall { alias }
            | HostAction::ArtifactFunctionCall { alias, .. }
            | HostAction::ArtifactQueryExecute { alias, .. }
            | HostAction::ArtifactQueryUpdate { alias, .. }
            | HostAction::ArtifactQueryAction { alias, .. } => alias.clone(),
            _ => {
                return Err(issue(
                    "INVALID_ACTION",
                    "This action is not an Epicor artifact operation.",
                ));
            }
        };
        let operation = self
            .native_operations
            .lock()
            .await
            .get(&(holder.into(), owner.to_string()))
            .cloned()
            .ok_or_else(|| {
                issue(
                    "ARTIFACT_NOT_DECLARED",
                    "This operation has no frozen Epicor artifacts for the plugin.",
                )
            })?;
        let mut file = operation.lock().await;
        let previous_record = file.record.clone();
        let index = file
            .record
            .artifacts
            .iter()
            .position(|r| r.declaration.alias == alias)
            .ok_or_else(|| {
                issue(
                    "ARTIFACT_NOT_DECLARED",
                    format!("Artifact alias {alias} is not declared by this plugin."),
                )
            })?;
        let result=async {
            let version=connection.server_version().await.unwrap_or_default();
            let source=file.source(index,&version)?;
            self.require_artifact_change(run,connection,owner,&file.record.artifacts[index].declaration,&source,&version).await?;
            let mut required=BTreeSet::from([index]);let mut pending=vec![index];
            while let Some(i)=pending.pop() {for alias in &file.record.artifacts[i].declaration.dependencies {
                let dependency=file.record.artifacts.iter().position(|r|&r.declaration.alias==alias).ok_or_else(||issue("ARTIFACT_DEPENDENCY_MISSING",format!("Declared dependency {alias} is absent from this operation.")))?;
                if required.insert(dependency) {pending.push(dependency);}
            }}
            for i in required {self.install_native_artifact(&mut file,i,run,connection,cancel).await?;}
            let record=&file.record.artifacts[index];
            let identity=record.identity.clone().ok_or_else(||issue("ARTIFACT_IDENTITY_UNVERIFIED","The installed artifact has no verified Epicor ID."))?;
            let declaration=record.declaration.clone();
            // The private host instance was installed and verified for this run.
            // Chunk reads reuse that exact identity instead of adding a metadata
            // request to every transfer request. Teardown still verifies ownership.
            if owner != super::host_utility::OWNER {
                let current=backend::probe(connection,declaration.artifact_type,&identity.id,cancel).await?.ok_or_else(||issue("ARTIFACT_NOT_FOUND","The operation's Epicor instance was removed before use."))?;
                if !backend::owns(&current.0,&identity) {return Err(issue("ARTIFACT_OWNER_MISMATCH","The installed artifact changed Epicor ID or ownership. It will not be executed."));}
            }
            // Reject unsupported actions before recording a server invocation.
            match &action {
                HostAction::ArtifactFunctionCall { .. } if declaration.artifact_type != NativeArtifactType::FunctionLibrary => return Err(issue("ARTIFACT_TYPE_MISMATCH", "A Function call requires a Function-library artifact.")),
                HostAction::ArtifactQueryExecute { .. } if declaration.artifact_type == NativeArtifactType::FunctionLibrary => return Err(issue("ARTIFACT_TYPE_MISMATCH", "Query execution requires a BAQ or UBAQ artifact.")),
                HostAction::ArtifactQueryUpdate { .. } if declaration.artifact_type != NativeArtifactType::Ubaq || !declaration.effects.submits_updates => return Err(issue("ARTIFACT_UPDATE_NOT_DECLARED", "This plugin must declare UBAQ update effects before submitting updates.")),
                HostAction::ArtifactQueryAction { parameters, .. } => {
                    if declaration.artifact_type != NativeArtifactType::Ubaq || !declaration.effects.invokes_custom_actions { return Err(issue("ARTIFACT_ACTION_NOT_DECLARED", "This plugin must declare UBAQ custom-action effects before invoking an action.")); }
                    if !parameters.is_null() && !parameters.as_object().is_some_and(|p| p.is_empty()) { return Err(issue("ARTIFACT_ACTION_PARAMETERS_UNSUPPORTED", "Put action inputs in the Epicor result dataset. Separate parameters are not supported.")); }
                }
                _ => {}
            }
            let execution = if let HostAction::ArtifactQueryExecute { parameters, .. } = &action {
                Some(native_query_parameters(connection, &identity.id, parameters.clone(), cancel).await?)
            } else { None };
            // The collection slot is separate from ordinary request slots.
            // Installation and the host's file-transfer utility do not collect plugin data.
            let collection_permit = if owner != super::host_utility::OWNER && !matches!(&action, HostAction::ArtifactInstall { .. }) {
                Some(crate::server_collection::acquire(&self.access_store.location()?.join("server-collections"), holder, owner, cancel).await?)
            } else { None };
            let source=file.source(index,&version)?;
            self.require_artifact_change(run,connection,owner,&declaration,&source,&version).await?;
            // D2: after this run's server lease is lost, no new helper invocation
            // starts; one that was in flight at the loss is uncertain.
            let lost=if matches!(&action,HostAction::ArtifactInstall{..}) {None} else {self.lease_gate(run).await?};
            let result = match action {
                HostAction::ArtifactInstall {..}=>response(json!({"alias":alias,"id":identity.id,"type":declaration.artifact_type,"company":identity.company})),
                HostAction::ArtifactFunctionCall {function,arguments,..}=>{
                    if declaration.artifact_type!=NativeArtifactType::FunctionLibrary {return Err(issue("ARTIFACT_TYPE_MISMATCH","A Function call requires a Function-library artifact."));}
                    connection.artifact_function_call(&identity.id,&function,arguments,cancel).await
                }
                HostAction::ArtifactQueryExecute {parameters,..}=>{
                    if declaration.artifact_type==NativeArtifactType::FunctionLibrary {return Err(issue("ARTIFACT_TYPE_MISMATCH","Query execution requires a BAQ or UBAQ artifact."));}
                    let _ = parameters;
                    let execution=execution.expect("validated query execution parameters");
                    connection.artifact_call(backend::QUERY,"ExecuteByID",json!({"queryID":identity.id,"executionParams":execution}),cancel).await
                }
                HostAction::ArtifactQueryUpdate {dataset,..}=>{
                    if declaration.artifact_type!=NativeArtifactType::Ubaq||!declaration.effects.submits_updates {return Err(issue("ARTIFACT_UPDATE_NOT_DECLARED","This plugin must declare UBAQ update effects before submitting updates."));}
                    connection.artifact_call(backend::QUERY,"UpdateByID",json!({"queryID":identity.id,"queryResultDataset":dataset}),cancel).await
                }
                HostAction::ArtifactQueryAction {action,dataset,parameters,..}=>{
                    if declaration.artifact_type!=NativeArtifactType::Ubaq||!declaration.effects.invokes_custom_actions {return Err(issue("ARTIFACT_ACTION_NOT_DECLARED","This plugin must declare UBAQ custom-action effects before invoking an action."));}
                    if !parameters.is_null()&&!parameters.as_object().is_some_and(|p|p.is_empty()) {return Err(issue("ARTIFACT_ACTION_PARAMETERS_UNSUPPORTED","RunCustomActionByID accepts its action ID and Epicor result dataset. Put action inputs in that dataset; separate parameters are not supported by this Epicor route."));}
                    connection.artifact_call(backend::QUERY,"RunCustomActionByID",json!({"queryID":identity.id,"actionID":action,"queryResultDataset":dataset}),cancel).await
                }
                _=>unreachable!(),
            };
            let result=super::server_leases::after_leased_call(&lost,result);
            if let Some(permit) = collection_permit { permit.settle(&result)?; }
            result
        }.await;
        if let Err(error) = &result {
            file.record.artifacts[index].diagnostic = Some(error.clone());
            file.checkpoint()?;
        }
        if file.record != previous_record {
            self.checkpoint_native_artifacts(run, file.record.clone()).await?;
        }
        result
    }
    // Three retries after the initial attempt (selected 2026-10-25).
    // This is a helper setup policy, not a blanket replay policy for plugin calls.
    async fn install_native_artifact(
        &self, file: &mut OperationFile, index: usize, run: &Arc<Run>,
        connection: &Arc<Connection>, cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let helper = &file.record.artifacts[index].declaration;
        let retry_safe = helper.lifetime == NativeArtifactLifetime::Temporary
            && helper.policy == NativeArtifactPolicy::CreateInstance && helper.target.is_none();
        if let Some(error) = &file.record.artifacts[index].diagnostic {
            if error.context.get("helperSetupRetries").and_then(Value::as_u64) == Some(3) { return Err(error.clone()); }
        }
        let mut retries = 0;
        let mut reconcile = false;
        loop {
            if cancel.is_cancelled() { return Err(issue("CANCELED", "Helper setup was canceled.")); }
            let result = if reconcile {
                match self.reconcile_helper_retry(file, index, connection, cancel).await {
                    Ok(()) => self.install_native_artifact_once(file, index, run, connection, cancel).await,
                    Err(error) => Err(error),
                }
            } else {
                self.install_native_artifact_once(file, index, run, connection, cancel).await
            };
            match result {
                Ok(()) => { file.record.artifacts[index].diagnostic = None; file.checkpoint()?; return Ok(()); }
                Err(mut error) => {
                    if !retry_safe || !helper_setup_transient(&error) || cancel.is_cancelled() { return Err(error); }
                    error.context.insert("helperSetupRetries".into(), json!(retries));
                    error.retryable = true;
                    file.record.artifacts[index].diagnostic = Some(error.clone());
                    file.checkpoint()?;
                    self.checkpoint_native_artifacts(run, file.record.clone()).await?;
                    if retries == 3 {
                        // The message stays the helper's static sentence; the retry outcome is detail.
                        error.context.insert("attempts".into(), serde_json::json!(retries));
                        error.context.insert("recoveryRecordKept".into(), serde_json::json!(true));
                        return Err(error);
                    }
                    // Exponential pacing prevents immediate repeated pressure on a failing server.
                    // The existing configured backoff can request a longer delay.
                    let configured = run.storage.lock().map_err(|_| issue("RUN_STATE_FAILED", "Run state is unavailable."))?.journal.settings.retry_backoff_ms.unwrap_or(0);
                    let delay = std::time::Duration::from_millis(configured.max(1000u64 << retries));
                    retries += 1;
                    let name = format!("Helper {}", file.record.artifacts[index].declaration.alias);
                    let activity = run.activity.begin(&name, None, &format!("Helper setup retry {retries}/3; waiting before checking server state"));
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(issue("CANCELED", "Helper setup was canceled during retry wait.")),
                        _ = tokio::time::sleep(delay) => {}
                    }
                    activity.step("Reconciling helper identity before retry");
                    reconcile = true;
                }
            }
        }
    }
    async fn reconcile_helper_retry(
        &self, file: &mut OperationFile, index: usize,
        connection: &Connection, cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let record = file.record.artifacts[index].clone();
        if !matches!(record.phase.as_str(), "importPending" | "importAbsent" | "compilePending" | "publishPending") { return Ok(()); }
        let current = backend::probe(connection, record.declaration.artifact_type, &record.target, cancel).await?;
        match current {
            None if matches!(record.phase.as_str(), "importPending" | "importAbsent") => {
                // Reuse the frozen ID/marker. CreateInstance imports use overwrite=false,
                // so a late server commit cannot be overwritten by this retry.
                file.phase(index, "reserved")
            }
            None => Err(issue("ARTIFACT_NOT_FOUND", "The helper disappeared during setup reconciliation.")),
            Some((identity, readback)) => {
                if !record.matches_import(&file.record.scope, &identity)
                    || record.identity.as_ref().is_some_and(|expected| !backend::owns(&identity, expected)) {
                    return Err(issue("ARTIFACT_OWNER_MISMATCH", "Retry found a different helper identity or owner; it was preserved."));
                }
                file.record.artifacts[index].identity = Some(identity);
                if matches!(record.phase.as_str(), "importPending" | "importAbsent") {
                    if record.declaration.artifact_type != NativeArtifactType::FunctionLibrary { backend::verify_query(&record.declaration, &readback)?; }
                    return file.phase(index, "installed");
                }
                // Read the committed state before repeating an owned enable/publish.
                let bytes = backend::export(connection, record.declaration.artifact_type, &record.target, cancel).await?;
                let native = decode_function(&bytes)?;
                if record.phase == "publishPending" {
                    if backend::verify_functions(&record.declaration, &native, true).is_ok() {
                        file.phase(index, "ready")
                    } else { file.phase(index, "compiled") }
                } else {
                    // The normal enable path reads and verifies the same owner again.
                    file.phase(index, "installed")
                }
            }
        }
    }
    async fn install_native_artifact_once(
        &self,
        file: &mut OperationFile,
        index: usize,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        if file.renew_closed(index)? {
            self.checkpoint_native_artifacts(run, file.record.clone()).await?;
        }
        let record = file.record.artifacts[index].clone();
        // A confirmed absent import is terminal for this generation. Preserve the
        // original refusal; another page must not promote it to ready or replay it.
        if record.phase == "importAbsent" {
            return Err(record.diagnostic.clone().unwrap_or_else(|| issue(
                "ARTIFACT_IMPORT_UNRESOLVED",
                "Epicor readback found no imported artifact. Start a new operation; this import will not be replayed.",
            )));
        }
        let method = run.storage.lock().map_err(|_| issue("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal.collection_method;
        crate::collection_preferences::admit(method, record.declaration.purpose)?;
        if record.phase == "ready" {
        if file.record.plugin != super::host_utility::OWNER &&
            record.declaration.artifact_type == NativeArtifactType::FunctionLibrary &&
            record.declaration.lifetime == NativeArtifactLifetime::Temporary &&
            record.declaration.target.is_none() {
            let identity = file.record.artifacts[index].identity.as_ref()
                .ok_or_else(|| issue("ARTIFACT_IDENTITY_UNVERIFIED", "The helper identity is absent."))?;
            Box::pin(self.keep_helper_alive(run, connection, identity, false, cancel)).await?;
        }
            return Ok(());
        }
        if matches!(
            record.phase.as_str(),
            "deletePending" | "restorePending" | "restoreCompilePending" | "restorePublishPending"
        ) {
            return Err(issue(
                "ARTIFACT_CLEANUP_PENDING",
                "This Epicor instance is being restored or removed. Finish cleanup before starting a new operation.",
            ));
        }
        if matches!(record.phase.as_str(), "removed" | "restored") {
            return Err(issue(
                "ARTIFACT_OPERATION_FINISHED",
                "The temporary artifact was already cleaned up. Start a new operation for a new instance.",
            ));
        }
        super::checkpoint(run, "installingArtifacts").await?;
        let progress_name = format!("Helper {}", record.declaration.alias);
        let _installing = run.activity.begin(&progress_name, None, "Installing helper in Epicor");
        _installing.step("Checking server version and preparing helper package");
        let version = if record.declaration.version_files.is_empty()
            && !file.record.artifacts.iter().any(|other| {
                record
                    .declaration
                    .dependencies
                    .contains(&other.declaration.alias)
                    && !other.declaration.version_files.is_empty()
            }) {
            String::new()
        } else {
            let response = connection
                .native_read(
                    "Ice.Lib.SessionModSvc",
                    "GetDatabaseVersion",
                    json!({}),
                    false,
                    cancel,
                )
                .await?;
            database_version(&response.value)?
        };
        _installing.step("Server version verified; preparing the scoped helper package");
        let source = file.source(index, &version)?;
        let authorization=self.require_artifact_change(run,connection,&file.record.plugin,&record.declaration,&source,&version).await?;
        let prepared = package::prepare(
            &record.declaration,
            &source,
            &record.target,
            record.owner,
            &file.record.scope.company,
            &file.record.scope.caller,
        )?;
        _installing.step("Helper package prepared; checking declared dependencies");
        if record.declaration.artifact_type == NativeArtifactType::FunctionLibrary {
            backend::verify_functions(&record.declaration, &prepared.metadata, false)?;
        }
        if record.declaration.artifact_type == NativeArtifactType::FunctionLibrary {
            for dependency in &record.declaration.dependencies {
                let (other_index, other) = file
                    .record
                    .artifacts
                    .iter()
                    .enumerate()
                    .find(|(_, r)| &r.declaration.alias == dependency)
                    .ok_or_else(|| {
                        issue(
                            "ARTIFACT_DEPENDENCY_MISSING",
                            format!("Dependency {dependency} is missing."),
                        )
                    })?;
                if other.declaration.artifact_type != NativeArtifactType::FunctionLibrary {
                    continue;
                }
                let other_source = file.source(other_index, &version)?;
                let other_prepared = package::prepare(
                    &other.declaration,
                    &other_source,
                    &other.target,
                    other.owner,
                    &file.record.scope.company,
                    &file.record.scope.caller,
                )?;
                if other_prepared.source_id != other.target
                    && prepared.metadata["Library"]["LibraryReferences"]
                        .as_array()
                        .is_some_and(|refs| {
                            refs.iter().any(|r| {
                                r["ReferenceType"].as_u64() == Some(3)
                                    && r["ReferenceID"].as_str()
                                        == Some(other_prepared.source_id.as_str())
                            })
                        })
                {
                    return Err(issue(
                        "ARTIFACT_REFERENCE_TARGET_REQUIRED",
                        format!(
                            "Artifact {} references dependency {} by Epicor library ID {}. Set that dependency's explicit target to {} or update the exported reference. Its generated target {} cannot satisfy the reference; custom code was not rewritten.",
                            record.declaration.alias,
                            dependency,
                            other_prepared.source_id,
                            other_prepared.source_id,
                            other.target
                        ),
                    ));
                }
            }
        }
        if record.phase == "reserved" {
            _installing.step("Checking the reserved helper identity in Epicor");
            let existing = backend::probe(
                connection,
                record.declaration.artifact_type,
                &record.target,
                cancel,
            )
            .await?;
            if let Some((identity, _)) = &existing {
                if identity.company != file.record.scope.company {
                    return Err(issue(
                        "ARTIFACT_COMPANY_MISMATCH",
                        "This target belongs to another company or shared scope. It was preserved.",
                    ));
                }
                if record.declaration.lifetime == NativeArtifactLifetime::Temporary
                    && identity.owner.as_deref() != Some(file.record.scope.caller.as_str())
                {
                    return Err(issue(
                        "ARTIFACT_RESTORE_OWNER_UNSUPPORTED",
                        "The Epicor importer rewrites ownership to the allowed to start Epicor user. A temporary replacement cannot preserve this other owner; use a separate instance.",
                    ));
                }
                match record.declaration.policy {
                    NativeArtifactPolicy::CreateInstance => {
                        return Err(issue(
                            "ARTIFACT_TARGET_EXISTS",
                            format!(
                                "Epicor target {} already exists. No replacement was authorized.",
                                record.target
                            ),
                        ));
                    }
                    NativeArtifactPolicy::UpdateOwned => {
                        let old = file.installed(index)?.ok_or_else(|| {
                            issue(
                                "ARTIFACT_NOT_OWNED",
                                "The named artifact has no Herbert ownership record.",
                            )
                        })?;
                        if old.scope != file.record.scope
                            || !crate::legacy_category_ids::same(&old.plugin, &file.record.plugin)
                            || old.alias != record.declaration.alias
                            || old.artifact.phase != "ready"
                            || old.artifact.identity.as_ref() != Some(identity)
                        {
                            return Err(issue(
                                "ARTIFACT_OWNER_MISMATCH",
                                "The named artifact does not match Herbert's verified installation for this plugin and Epicor user.",
                            ));
                        }
                    }
                    NativeArtifactPolicy::ReplaceNamed => {}
                }
                if record.declaration.lifetime == NativeArtifactLifetime::Temporary {
                    if let Some(previous) = file.installed(index)? {
                        if previous.artifact.identity.as_ref() == Some(identity) {
                            file.record.artifacts[index].previous_installation =
                                Some(Box::new(previous));
                        }
                    }
                }
                let backup = backend::export(
                    connection,
                    record.declaration.artifact_type,
                    &record.target,
                    cancel,
                )
                .await?;
                file.backup(index, &backup)?;
                let fresh = backend::probe(
                    connection,
                    record.declaration.artifact_type,
                    &record.target,
                    cancel,
                )
                .await?
                .ok_or_else(|| {
                    issue(
                        "ARTIFACT_CHANGED",
                        "The replacement target disappeared while its backup was exported.",
                    )
                })?;
                if !backend::owns(&fresh.0, identity) {
                    return Err(issue(
                        "ARTIFACT_CHANGED",
                        "The replacement target changed identity while its backup was exported.",
                    ));
                }
                file.record.artifacts[index].replaced_identity = Some(identity.clone());
            }
            file.phase(index, "importPending")?;
            // The durable target, owner marker and backup exist before import.
            file.retain_installation(index)?;
            _installing.step("Uploading and importing helper package");
            if let Err(error)=self.require_artifact_change(run,connection,&file.record.plugin,&record.declaration,&source,&version).await {
                file.phase(index,"reserved")?;return Err(error);
            }
            if file.record.artifacts[index].authorization.is_none() {
                file.record.artifacts[index].authorization=Some(authorization.reference());
                file.checkpoint()?;
                self.checkpoint_native_artifacts(run,file.record.clone()).await?;
            }
            if let Err(error)=self.require_artifact_change(run,connection,&file.record.plugin,&record.declaration,&source,&version).await {
                file.phase(index,"reserved")?;return Err(error);
            }
            let imported = backend::import(
                connection,
                record.declaration.artifact_type,
                &record.target,
                &prepared.bytes,
                existing.is_some(),
                cancel,
            )
            .await;
            _installing.step("Reading back helper identity and ownership");
            let observed = backend::probe(
                connection,
                record.declaration.artifact_type,
                &record.target,
                &CancellationToken::new(),
            )
            .await?;
            let Some((identity, readback)) = observed else {
                file.phase(index, "importAbsent")?;
                return Err(imported.err().unwrap_or_else(||issue("ARTIFACT_IMPORT_UNVERIFIED","Epicor readback found no imported artifact. This import will not be replayed.")));
            };
            if !record.matches_import(&file.record.scope, &identity) {
                return Err(imported.err().unwrap_or_else(||issue("ARTIFACT_OWNER_MISMATCH","Epicor readback did not prove ownership of the imported artifact. It was preserved.")));
            }
            file.record.artifacts[index].identity = Some(identity);
            file.phase(index, "installed")?;
            if record.declaration.artifact_type != NativeArtifactType::FunctionLibrary {
                backend::verify_query(&record.declaration, &readback)?;
            }
            // Even a successful native readback cannot erase an explicit importer
            // failure such as a UBAQ downgrade or reported compilation diagnostic.
            imported?;
        } else if record.phase == "importPending" {
            let Some((identity, readback)) = backend::probe(
                connection,
                record.declaration.artifact_type,
                &record.target,
                cancel,
            )
            .await?
            else {
                return Err(issue(
                    "ARTIFACT_IMPORT_UNRESOLVED",
                    "The interrupted import is absent. Start a new operation; its write will not be replayed.",
                ));
            };
            if !record.matches_import(&file.record.scope, &identity) {
                return Err(issue(
                    "ARTIFACT_OWNER_MISMATCH",
                    "The interrupted import target has different ownership.",
                ));
            }
            file.record.artifacts[index].identity = Some(identity);
            file.phase(index, "installed")?;
            if record.declaration.artifact_type != NativeArtifactType::FunctionLibrary {
                backend::verify_query(&record.declaration, &readback)?;
            }
        }
        if record.declaration.artifact_type == NativeArtifactType::FunctionLibrary {
            if file.record.artifacts[index].phase == "installed" {
                let identity = file.record.artifacts[index]
                    .identity
                    .clone()
                    .ok_or_else(|| {
                        issue(
                            "ARTIFACT_IDENTITY_UNVERIFIED",
                            "The imported Function identity is absent.",
                        )
                    })?;
                _installing.step("Preparing native helper compilation rows");
                let arguments=backend::enable_arguments(connection,&record.target,&identity,&prepared.metadata,cancel).await?;
                self.require_artifact_change(run,connection,&file.record.plugin,&record.declaration,&source,&version).await?;
                file.phase(index,"compilePending")?;
                if let Err(error)=self.require_artifact_change(run,connection,&file.record.plugin,&record.declaration,&source,&version).await {
                    // The final check ran before any enable write was initiated.
                    file.phase(index,"installed")?;return Err(error);
                }
                _installing.step("Compiling and enabling helper");
                backend::apply_enable(connection,arguments,cancel).await?;
                file.phase(index, "compiled")?;
            }
            if file.record.artifacts[index].phase == "compilePending" {
                let bytes = backend::export(
                    connection,
                    record.declaration.artifact_type,
                    &record.target,
                    cancel,
                )
                .await?;
                let native = decode_function(&bytes)?;
                backend::verify_compiled(&native, &prepared.metadata)?;
                file.phase(index, "compiled")?;
            }
            if file.record.artifacts[index].phase == "compiled" {
                file.phase(index, "publishPending")?;
                _installing.step("Publishing helper and verifying its functions");
                if let Err(error)=self.require_artifact_change(run,connection,&file.record.plugin,&record.declaration,&source,&version).await {
                    file.phase(index,"compiled")?;return Err(error);
                }
                let result = backend::publish(connection, &record.target, cancel).await;
                let bytes = backend::export(
                    connection,
                    record.declaration.artifact_type,
                    &record.target,
                    &CancellationToken::new(),
                )
                .await?;
                let package: Value = serde_json::from_slice(
                    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes),
                )
                .map_err(|_| {
                    issue(
                        "ARTIFACT_READBACK_INVALID",
                        "The published Function export is malformed.",
                    )
                })?;
                backend::verify_functions(&record.declaration, &package, true)
                    .map_err(|e| result.err().unwrap_or(e))?;
            }
            if file.record.artifacts[index].phase == "publishPending" {
                let bytes = backend::export(
                    connection,
                    record.declaration.artifact_type,
                    &record.target,
                    cancel,
                )
                .await?;
                let package: Value = serde_json::from_slice(
                    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes),
                )
                .map_err(|_| {
                    issue(
                        "ARTIFACT_READBACK_INVALID",
                        "The Epicor Function export is malformed.",
                    )
                })?;
                backend::verify_functions(&record.declaration, &package, true)?;
            }
        } else if file.record.artifacts[index].phase != "installed" {
            return Err(issue(
                "ARTIFACT_IMPORT_UNRESOLVED",
                "The Epicor query installation is not verified.",
            ));
        }
        if record.declaration.artifact_type != NativeArtifactType::FunctionLibrary {
            let (_, native) = backend::probe(
                connection,
                record.declaration.artifact_type,
                &record.target,
                cancel,
            )
            .await?
            .ok_or_else(|| issue("ARTIFACT_NOT_FOUND", "The imported query is absent."))?;
            backend::verify_query(&record.declaration, &native)?;
            if record.declaration.artifact_type == NativeArtifactType::Ubaq {
                package::verify_update_configuration(&prepared.metadata, &native)?;
                if package::has_bpm(&source)? {
                    let exported = backend::export(
                        connection,
                        record.declaration.artifact_type,
                        &record.target,
                        cancel,
                    )
                    .await?;
                    package::verify_bpm(&source, &exported)?;
                }
            }
        }
        file.phase(index, "ready")?;
        if file.record.plugin != super::host_utility::OWNER &&
            record.declaration.artifact_type == NativeArtifactType::FunctionLibrary &&
            record.declaration.lifetime == NativeArtifactLifetime::Temporary &&
            record.declaration.target.is_none() {
            let identity = file.record.artifacts[index].identity.as_ref()
                .ok_or_else(|| issue("ARTIFACT_IDENTITY_UNVERIFIED", "The helper identity is absent."))?;
            Box::pin(self.keep_helper_alive(run, connection, identity, false, cancel)).await?;
        }
        run.activity.note(&progress_name, "Helper installed and ready");
        file.retain_installation(index)?;
        self.checkpoint_native_artifacts(run, file.record.clone())
            .await?;
        super::checkpoint(run, "acquiring").await
    }
}
async fn native_query_parameters(
    connection: &Connection,
    id: &str,
    provided: Value,
    cancel: &CancellationToken,
) -> Result<Value, Diagnostic> {
    if provided.get("ExecutionParameter").is_some() {
        return Ok(provided);
    }
    let supplied = provided.as_object().ok_or_else(|| {
        issue(
            "ARTIFACT_PARAMETERS_INVALID",
            "Query parameters must be a named object or an Epicor ExecutionParameter tableset.",
        )
    })?;
    let response = connection
        .artifact_call(
            backend::QUERY,
            "GetQueryExecutionParametersByID",
            json!({"queryID":id}),
            cancel,
        )
        .await?;
    let mut tableset = response.value["returnObj"].clone();
    let rows = tableset["ExecutionParameter"]
        .as_array_mut()
        .ok_or_else(|| {
            issue(
                "ARTIFACT_PARAMETERS_INVALID",
                "The Epicor query omitted its execution-parameter table.",
            )
        })?;
    for (name, value) in supplied {
        let row = rows
            .iter_mut()
            .find(|r| r["ParameterID"].as_str() == Some(name))
            .ok_or_else(|| {
                issue(
                    "ARTIFACT_PARAMETER_UNKNOWN",
                    format!("Query {id} has no parameter named {name}."),
                )
            })?;
        row["ParameterValue"] =
            json!(
                value
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| if value.is_null() {
                        String::new()
                    } else {
                        value.to_string()
                    })
            );
        row["IsEmpty"] = json!(value.is_null());
        row["RowMod"] = json!("A");
    }
    Ok(tableset)
}

fn decode_function(bytes: &[u8]) -> Result<Value, Diagnostic> {
    serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)).map_err(|_| {
        issue(
            "ARTIFACT_READBACK_INVALID",
            "The Epicor Function export is malformed.",
        )
    })
}

impl Host {
    pub(super) async fn cleanup_native_artifacts(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
    ) -> Vec<Diagnostic> {
        self.cleanup_native_artifacts_for(run, connection, None).await
    }
    pub(super) async fn cleanup_native_artifacts_for(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        plugins: Option<&BTreeSet<String>>,
    ) -> Vec<Diagnostic> {
        let journal = match collection::snapshot(run).await {
            Ok(journal) => journal,
            Err(error) => return vec![error],
        };
        if let Err(error) = self.access_store.location().and_then(|root|
            crate::server_collection::ensure_run_settled(&root.join("server-collections"), &journal.run_id)) {
            return vec![error];
        }
        // The shared host utility spans category batches. Keep it and resumable
        // server results until the run-level teardown; category startup must not
        // destroy a valid staged result before the plugin can resume its download.
        let mut errors = if plugins.is_none() {
            self.cleanup_host_transfers(run, connection, None).await
        } else { Vec::new() };
        let journal = match collection::snapshot(run).await {
            Ok(j) => j,
            Err(e) => return vec![e],
        };
        let lease_host = self.server_leases.lock().await.get(&journal.run_id)
            .map(|keeper| keeper.library.clone());
        if plugins.is_none() {
            if let Err(error) = self.release_server_lease(run, connection).await {
                errors.push(error);
            }
        }
        if plugins.is_none() {
            if let Some(library) = lease_host.as_deref() {
                errors.extend(self.cleanup_leased_contracts(run, connection, library).await);
            }
        }
        // Delete the control host last so it can acknowledge plugin cleanup.
        let mut saved_operations = journal.native_artifacts.iter().collect::<Vec<_>>();
        saved_operations.sort_by_key(|saved| saved.plugin == super::host_utility::OWNER);
        for saved in saved_operations {
            if plugins.is_some_and(|selected| !selected.contains(&saved.plugin)) { continue; }
            if saved.plugin == super::host_utility::OWNER && journal.host_transfers.iter().any(|t| t.phase != "purged") {
                errors.push(issue("HOST_STAGING_RETAINED", "The host helper remains installed because server staging still needs cleanup."));
                continue;
            }
            let key = (journal.run_id.clone(), saved.plugin.clone());
            let existing = self.native_operations.lock().await.get(&key).cloned();
            let operation = match existing {
                Some(file) => file,
                None => {
                    let root = self.state_root.clone();
                    let saved = saved.clone();
                    match blocking(move || {
                        OperationFile::open(
                            &root,
                            &saved.run_id,
                            &saved.plugin,
                            saved.scope,
                            Vec::new(),
                        )
                    })
                    .await
                    {
                        Ok(file) => Arc::new(Mutex::new(file)),
                        Err(e) => {
                            errors.push(e);
                            continue;
                        }
                    }
                }
            };
            let mut file = operation.lock().await;
            for index in (0..file.record.artifacts.len()).rev() {
                if !file.record.artifacts[index].needs_cleanup() {
                    continue;
                }
                let artifact = &file.record.artifacts[index];
                let leased_helper = artifact.declaration.artifact_type == NativeArtifactType::FunctionLibrary
                    && artifact.declaration.lifetime == NativeArtifactLifetime::Temporary
                    && artifact.declaration.target.is_none();
                // A run lease spans category batches. Category cleanup must not
                // remove a helper while that lease still advertises active use.
                if leased_helper && plugins.is_some() { continue; }
                let _ = super::checkpoint(run, "cleaningArtifacts").await;
                let alias = file.record.artifacts[index].declaration.alias.clone();
                let progress_name = format!("Helper {alias}");
                let _removing = run.activity.begin(&progress_name, None, "Cleaning up temporary helper");
                let dependent = file.record.artifacts.iter().find(|other| {
                    other.declaration.dependencies.contains(&alias)
                        && !matches!(other.phase.as_str(), "reserved" | "removed" | "restored")
                }).map(|other|other.declaration.alias.clone());
                let reference=file.record.artifacts[index].authorization.clone();
                let result=match reference {
                    Some(reference)=>self.with_owned_cleanup(run,connection,reference,async {
                        if let Some(other) = dependent {
                            Err(issue(
                                "ARTIFACT_DEPENDENCY_CLEANUP_PENDING",
                                format!(
                                    "Dependency {alias} remains installed because {} still depends on it. Finish that artifact's cleanup before removing this dependency.",
                                    other
                                ),
                            ))
                        } else if leased_helper {
                            let artifact = &file.record.artifacts[index];
                            match (lease_host.as_deref(), artifact.identity.as_ref().and_then(|id| id.global_id.as_deref())) {
                                (Some(host), Some(generation)) => {
                                    match self.cleanup_leased_helper(connection, host, &artifact.target, generation).await {
                                        Ok(true) => file.phase(index, "removed"),
                                        Ok(false) => {
                                            run.activity.note(&progress_name, "Lease released; another run still uses the helper");
                                            file.phase(index, "released")
                                        },
                                        Err(error) => Err(error),
                                    }
                                }
                                _ => Err(issue("SERVER_LEASE_FAILED", "The helper has no verified server lease. Cleanup preserved it for server reconciliation.")),
                            }
                        } else {
                            cleanup_one(&mut file, index, connection).await
                        }
                    }).await,
                    None=>Err(issue("SERVER_CHANGE_AUTHORIZATION_REQUIRED","This resource has no original authorization reference. Preserve its ownership evidence and explicitly authorize scoped recovery.")),
                };
                if result.is_ok() {
                    let phase = file.record.artifacts[index].phase.as_str();
                    if phase == "removed" {
                        run.activity.note(&progress_name, "Temporary helper removed");
                    } else if phase == "restored" {
                        run.activity.note(&progress_name, "Previous helper restored");
                    }
                }
                if let Err(mut error) = result {
                    run.activity.note(&progress_name, "Helper cleanup needs attention");
                    error.context.insert("plugin".into(), serde_json::json!(file.record.plugin));
                    error.context.insert("helper".into(), serde_json::json!(file.record.artifacts[index].declaration.alias));
                    error.context.insert("serverObject".into(), serde_json::json!(file.record.artifacts[index].target));
                    file.record.artifacts[index].diagnostic = Some(error.clone());
                    errors.push(error);
                }
                if let Err(e) = file.checkpoint() {
                    errors.push(e);
                }
                if let Err(e) = file.retain_installation(index) {
                    errors.push(e);
                }
                if let Err(e) = self
                    .checkpoint_native_artifacts(run, file.record.clone())
                    .await
                {
                    errors.push(e);
                }
            }
            drop(file);
            if plugins.is_none() {
                self.native_operations.lock().await.remove(&key);
            }
        }
        if plugins.is_none() && journal.host_transfers.iter().all(|transfer| transfer.phase == "purged") {
            if let Some(library) = lease_host.as_deref() {
                let cancel = CancellationToken::new();
                match backend::probe(connection, NativeArtifactType::FunctionLibrary, library, &cancel).await {
                    Ok(Some((identity, _))) => {
                        if let Some(generation) = identity.global_id.as_deref() {
                            let reference=journal.native_artifacts.iter().flat_map(|operation|operation.artifacts.iter()).find(|artifact|artifact.target==library).and_then(|artifact|artifact.authorization.clone());
                            let result=match reference {
                                Some(reference)=>self.with_owned_cleanup(run,connection,reference,self.cleanup_leased_helper(connection,library,library,generation)).await.map(|_|()),
                                None=>Err(issue("SERVER_CHANGE_AUTHORIZATION_REQUIRED","The control helper has no original authorization reference. Preserve it for explicit scoped recovery.")),
                            };
                            if let Err(error)=result {errors.push(error);}
                        }
                    }
                    Ok(None) => (),
                    Err(error) => errors.push(error),
                }
            }
        }
        errors
    }
}
fn restored_identity(
    current: &crate::native_artifact_store::NativeIdentity,
    original: &crate::native_artifact_store::NativeIdentity,
) -> bool {
    // Native import can assign a new row UUID. Match the preserved semantic
    // identity and ownership instead of requiring the deleted row UUID.
    current.id == original.id
        && current.company == original.company
        && current.global_id == original.global_id
        && current.owner == original.owner
        && current.updatable == original.updatable
        && current.marker == original.marker
}
async fn cleanup_one(
    file: &mut OperationFile,
    index: usize,
    connection: &Connection,
) -> Result<(), Diagnostic> {
    let cancel = CancellationToken::new();
    let record = file.record.artifacts[index].clone();
    let kind = record.declaration.artifact_type;
    let observed = backend::probe(connection, kind, &record.target, &cancel).await?;
    let restoring = record.phase.starts_with("restore");
    if let Some((identity, _)) = &observed {
        if restoring {
            let original = record.replaced_identity.as_ref().ok_or_else(|| {
                issue(
                    "ARTIFACT_RESTORE_UNVERIFIED",
                    "Restoration has no preserved original identity.",
                )
            })?;
            if !restored_identity(identity, original) {
                return Err(issue(
                    "ARTIFACT_RESTORE_UNVERIFIED",
                    "The interrupted restoration does not match its original ownership. No write was replayed.",
                ));
            }
        } else if let Some(expected) = &record.identity {
            if !backend::owns(identity, expected) {
                return Err(issue(
                    "ARTIFACT_OWNER_MISMATCH",
                    "Cleanup found a changed Epicor ID. It was preserved.",
                ));
            }
        } else if !record.matches_import(&file.record.scope, identity) {
            // A rejected import may leave its original untouched.
            if record
                .replaced_identity
                .as_ref()
                .is_some_and(|original| backend::owns(identity, original))
            {
                file.phase(index, "restored")?;
                return Ok(());
            }
            return Err(issue(
                "ARTIFACT_OWNER_MISMATCH",
                "The import did not establish owned Epicor ID. Cleanup preserved the target.",
            ));
        }
    }
    let Some(original) = record.replaced_identity.as_ref() else {
        if let Some((identity, _)) = observed {
            if record.phase == "deletePending" {
                return Err(issue(
                    "ARTIFACT_CLEANUP_PENDING",
                    "An interrupted delete still has an Epicor target. Its write was not replayed.",
                ));
            }
            file.record.artifacts[index].identity = Some(identity.clone());
            file.phase(index, "deletePending")?;
            backend::delete(connection, kind, &identity, &cancel).await?;
        }
        file.phase(index, "removed")?;
        return Ok(());
    };
    let backup = file.backup_bytes(index)?;
    if !restoring {
        file.phase(index, "restorePending")?;
        let result = backend::import(
            connection,
            kind,
            &record.target,
            &backup,
            observed.is_some(),
            &cancel,
        )
        .await;
        let current = backend::probe(connection, kind, &record.target, &cancel)
            .await?
            .ok_or_else(|| {
                issue(
                    "ARTIFACT_RESTORE_UNVERIFIED",
                    "Epicor restoration did not produce a target. Its backup is retained.",
                )
            })?;
        if !restored_identity(&current.0, original) {
            return Err(result.err().unwrap_or_else(|| {
                issue(
                    "ARTIFACT_RESTORE_UNVERIFIED",
                    "The restored Epicor target does not match the original ownership.",
                )
            }));
        }
        result?;
    } else if observed.is_none() {
        return Err(issue(
            "ARTIFACT_RESTORE_UNVERIFIED",
            "The interrupted restoration is absent. Its write was not replayed.",
        ));
    }
    if kind == NativeArtifactType::FunctionLibrary {
        let source = decode_function(&backup)?;
        if file.record.artifacts[index].phase == "restorePending" {
            let current = backend::probe(connection, kind, &record.target, &cancel)
                .await?
                .ok_or_else(|| {
                    issue(
                        "ARTIFACT_RESTORE_UNVERIFIED",
                        "The restored library disappeared.",
                    )
                })?;
            file.phase(index, "restoreCompilePending")?;
            backend::enable(connection, &record.target, &current.0, &source, &cancel).await?;
        }
        let current =
            decode_function(&backend::export(connection, kind, &record.target, &cancel).await?)?;
        backend::verify_compiled(&current, &source)?;
        if source["Library"]["Published"].as_bool() == Some(true) {
            if file.record.artifacts[index].phase != "restorePublishPending" {
                file.phase(index, "restorePublishPending")?;
                let result = backend::publish(connection, &record.target, &cancel).await;
                let readback = decode_function(
                    &backend::export(connection, kind, &record.target, &cancel).await?,
                )?;
                if readback["Library"]["Published"].as_bool() != Some(true) {
                    return Err(result.err().unwrap_or_else(|| {
                        issue(
                            "ARTIFACT_RESTORE_UNVERIFIED",
                            "The restored library was not republished.",
                        )
                    }));
                }
            } else if current["Library"]["Published"].as_bool() != Some(true) {
                return Err(issue(
                    "ARTIFACT_RESTORE_UNVERIFIED",
                    "The interrupted restoration is not published. Its write was not replayed.",
                ));
            }
        }
    }
    let restored = backend::probe(connection, kind, &record.target, &cancel)
        .await?
        .ok_or_else(|| {
            issue(
                "ARTIFACT_RESTORE_UNVERIFIED",
                "The restored target disappeared before final readback.",
            )
        })?;
    if !restored_identity(&restored.0, original) {
        return Err(issue(
            "ARTIFACT_RESTORE_UNVERIFIED",
            "The restored target changed identity before final readback.",
        ));
    }
    file.record.artifacts[index].identity = Some(restored.0);
    file.phase(index, "restored")
}

/// Epicor returns a version string on some lanes and a four-part object on others.
/// Preserve all native components. Package manifests select exact versions.
fn database_version(response: &Value) -> Result<String, Diagnostic> {
    let value=&response["returnObj"];
    if let Some(text)=value.as_str().filter(|text|!text.trim().is_empty()) {return Ok(text.to_owned());}
    let parts=["_Major","_Minor","_Build","_Revision"].iter().map(|key|value[*key].as_u64().map(|v|v.to_string())).collect::<Option<Vec<_>>>();
    parts.map(|parts|parts.join(".")).ok_or_else(||issue("ARTIFACT_VERSION_UNAVAILABLE","The Epicor database version was missing or malformed."))
}
#[cfg(test)]
mod version_tests {
    use super::*;
    #[test] fn native_artifact_version_accepts_string_and_native_object() {
        assert_eq!(database_version(&json!({"returnObj":"5.2.100"})).unwrap(),"5.2.100");
        assert_eq!(database_version(&json!({"returnObj":{"_Major":12,"_Minor":2,"_Build":100,"_Revision":9}})).unwrap(),"12.2.100.9");
    }
    #[test] fn native_artifact_version_rejects_incomplete_or_mistyped_values() {
        for value in [json!(null),json!(""),json!({"_Major":12,"_Minor":2,"_Build":100}),json!({"_Major":12,"_Minor":2,"_Build":100,"_Revision":"9"})] {assert!(database_version(&json!({"returnObj":value})).is_err());}
    }
}

#[cfg(test)] mod failed_import_tests {
 use super::*;
 #[tokio::test] async fn later_page_preserves_absent_import_and_original_refusal_without_network(){
  let root=tempfile::tempdir().unwrap();let host=Host::new(root.path().join("state"),root.path().join("plugins"));
  let connection=Connection::new(url::Url::parse("http://127.0.0.1:1/fixture/").unwrap(),"FIXTURE".into(),AuthMode::BasicApiKey,Credentials{username:"fixture".into(),password:"fixture".into(),api_key:Some("fixture".into())},OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
  let store=Storage::open(root.path().join("output").as_path(),false,storage::new_journal("failed-import".into())).unwrap();
  let run=Arc::new(Run{storage:Arc::new(std::sync::Mutex::new(store)),cancel:CancellationToken::new(),paused:AtomicBool::new(false),wake:Notify::new(),active:AtomicBool::new(true),network:false,helper_restore_attempts:Mutex::new(BTreeSet::new()),activity:Arc::new(Default::default()),shared_reads:Default::default(),helper_instances:Mutex::new(BTreeMap::new()),collection_context:Mutex::new(None)});
  let declaration=serde_json::from_value(json!({"alias":"reader","type":"functionLibrary","file":"reader.efxj","lifetime":"temporary","policy":"createInstance","effects":{"description":"Fixture"}})).unwrap();
  let snapshot=crate::plugin_artifacts::NativeArtifactSnapshot{declaration,files:BTreeMap::from([("reader.efxj".into(),Arc::new(b"never parsed".to_vec()))])};
  let mut file=OperationFile::open(&host.state_root,"failed-import","fixture",ArtifactScope{endpoint:connection.endpoint.to_string(),company:"FIXTURE".into(),caller:"fixture".into()},vec![snapshot]).unwrap();
  file.phase(0,"importAbsent").unwrap();let original=Diagnostic::new("NATIVE_REFUSAL","Import returned HTTP 500");file.record.artifacts[0].diagnostic=Some(original.clone());
  for _ in 0..2 {let got=host.install_native_artifact(&mut file,0,&run,&connection,&CancellationToken::new()).await.unwrap_err();assert_eq!(got.code,original.code);assert_eq!(got.message,original.message);assert_eq!(file.record.artifacts[0].phase,"importAbsent");assert!(file.record.artifacts[0].identity.is_none());}
  assert!(connection.qualification_http.lock().unwrap().is_empty());assert!(host.shutdown().await.is_empty());
 }
}

#[cfg(test)]
#[path = "artifact_retry_tests.rs"]
mod retry_tests;
