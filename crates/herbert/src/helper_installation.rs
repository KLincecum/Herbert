//! Install, qualify and remove exact plugin-supplied native helper instances.
//! Neither the package builder nor EpiDev performs these native writes.
use super::*;
use crate::helper_libraries::{self, LeaseFile};
use base64::Engine as _;

impl Host {
    /// Recover cleanup without a plugin file. Saved runs retain their exact
    /// transfer contracts. The destination lock prevents a second active host.
    pub(super) async fn reconcile_helper_operation(
        &self,
        holder: &str,
    ) -> Result<Value, Diagnostic> {
        let connection = self.current_connection().await?;
        let root = self.state_root.clone();
        let owner = holder.to_string();
        let saved = blocking(move || {
            let path = root.join("runs").join(format!("{owner}.json"));
            if !path.try_exists().map_err(|_| {
                Diagnostic::new(
                    "RUN_RECORD_INVALID",
                    "Cannot check saved operation ownership.",
                )
            })? {
                return Ok(None);
            }
            Ok(Some(crate::recovery::RunIndex::open(&root)?.get(&owner)?))
        })
        .await?;
        let mut record = saved;
        let run = if let Some(record) = &record {
            if record.journal.endpoint != connection.endpoint.as_str()
                || record.journal.company != connection.company
                || Some(record.journal.caller.as_str()) != connection.caller().await.as_deref()
            {
                return Err(Diagnostic::new(
                    "RUN_SCOPE_MISMATCH",
                    "Reconnect as the saved caller to its endpoint and company before cleanup.",
                ));
            }
            let saved = record.clone();
            let storage = blocking(move || {
                let journal = crate::recovery::read_journal(&saved)?;
                Storage::open(Path::new(&saved.destination), false, journal)
            })
            .await?;
            Some(Arc::new(Run {
                storage: Arc::new(std::sync::Mutex::new(storage)),
                cancel: CancellationToken::new(),
                paused: AtomicBool::new(false),
                wake: Notify::new(),
                active: AtomicBool::new(false),
                network: false,
                helper_restore_attempts: Mutex::new(std::collections::BTreeSet::new()),
                activity: Arc::new(Default::default()),
                shared_reads: Default::default(),
                helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
            }))
        } else {
            None
        };
        let _operation = if run.is_none() {
            let root = self.state_root.clone();
            let owner = holder.to_string();
            Some(
                blocking(move || crate::helper_libraries::OperationGuard::acquire(&root, &owner))
                    .await?,
            )
        } else {
            None
        };
        let _transfer = self.helper_transfer_gate.lock().await;
        let mut diagnostics = if let Some(run) = &run {
            self.cleanup_helper_transfers(run, &connection, None).await
        } else {
            vec![]
        };
        diagnostics.extend(
            self.cleanup_helper_libraries(holder, &connection, run.as_ref())
                .await,
        );
        if run.is_none() && diagnostics.is_empty() {
            if let Err(error) = crate::native_access::release(
                &self.access_store,
                &connection,
                holder,
                &CancellationToken::new(),
            )
            .await
            {
                diagnostics.push(error);
            }
        }
        if let Some(run) = &run {
            if let Err(error) = self.cleanup_access(run, &connection).await {
                diagnostics.push(error);
            }
            let mut journal = collection::snapshot(run).await?;
            if !diagnostics.is_empty() {
                journal.phase = "partial".into();
            } else if !matches!(journal.phase.as_str(), "canceled" | "failed") {
                journal.phase = collection::terminal_phase(&journal).into();
            }
            journal.diagnostics.extend(diagnostics.clone());
            let next = journal.clone();
            collection::update(run, move |storage| {
                storage.journal = next;
                Ok(())
            })
            .await?;
            if let Some(record) = &mut record {
                record.journal = journal;
                let saved = record.clone();
                let root = self.state_root.clone();
                blocking(move || crate::recovery::RunIndex::open(&root)?.save(&saved)).await?;
            }
            let storage = run.storage.clone();
            blocking(move || {
                storage
                    .lock()
                    .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                    .release_destination()
            })
            .await?;
            self.runs.lock().await.insert(holder.into(), run.clone());
        }
        Ok(
            json!({"operationId": holder, "cleanupConfirmed": diagnostics.is_empty(), "diagnostics": diagnostics}),
        )
    }

    /// Reuse a ready instance during one execution. Recovery creates a new Run;
    /// cleanup uses its separate fresh ownership checks.
    pub(super) async fn ensure_helper_library(
        &self,
        plugin: &Plugin,
        claim: &HelperContract,
        holder: &str,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
        temporary_access: bool,
        cancel: &CancellationToken,
    ) -> Result<HelperContract, Diagnostic> {
        let method = if let Some(run) = run {
            run.storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                .journal.collection_method
        } else {
            crate::collection_preferences::read(&self.state_root)?.collection_method
        };
        crate::collection_preferences::admit(method, claim.purpose)?;
        let Some(run) = run else {
            return self
                .prepare_helper_library(
                    plugin,
                    claim,
                    holder,
                    connection,
                    None,
                    temporary_access,
                    cancel,
                )
                .await;
        };
        let key = json!([plugin.hash, claim.library_id, claim.global_id]).to_string();
        let mut ready = run.helper_instances.lock().await;
        if let Some(instance) = ready.get(&key) {
            return Ok(instance.clone());
        }
        let instance = self
            .prepare_helper_library(
                plugin,
                claim,
                holder,
                connection,
                Some(run),
                temporary_access,
                cancel,
            )
            .await?;
        if claim.installation.is_some() {
            let identity = crate::native_artifact_backend::probe(
                connection, NativeArtifactType::FunctionLibrary, &instance.library_id, cancel)
                .await?.ok_or_else(|| Diagnostic::new("HELPER_NOT_FOUND",
                    "The installed helper disappeared before lease registration."))?.0;
            self.keep_helper_alive(run, connection, &identity, false, cancel).await?;
        }
        ready.insert(key, instance.clone());
        Ok(instance)
    }

    async fn prepare_helper_library(
        &self,
        plugin: &Plugin,
        claim: &HelperContract,
        holder: &str,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
        temporary_access: bool,
        cancel: &CancellationToken,
    ) -> Result<HelperContract, Diagnostic> {
        let Some(installation) = &claim.installation else {
            return Ok(claim.clone());
        };
        if !installation.supports_instances {
            return Err(Diagnostic::new(
                "HELPER_INSTALLATION_NOT_ADMITTED",
                "Native helper source must support a private library instance.",
            ));
        }

        // A retained resource belongs to its original helper generation. Resolve
        // that generation before open() can reserve a new identity.
        if let Some(run) = run {
            let journal = collection::snapshot(run).await?;
            let outstanding = journal
                .helper_transfers
                .iter()
                .filter(|record| !matches!(record.phase.as_str(), "closed" | "notAdmitted"))
                .collect::<Vec<_>>();
            let retained = outstanding.iter().find(|record| {
                journal.helper_libraries.iter().any(|lease| {
                    lease.template_id == claim.library_id
                        && lease.instance.global_id == record.claim.global_id
                }) || crate::helper_recovery::matches_template(claim, &record.claim)
            });
            if let Some(record) = retained {
                return self
                    .ensure_retained_helper(run, connection, record, Some((plugin, claim)), cancel)
                    .await;
            }
            if outstanding.iter().any(|record| {
                record.identity.category == plugin.category.id
                    && !journal
                        .helper_libraries
                        .iter()
                        .any(|lease| lease.instance.global_id == record.claim.global_id)
            }) {
                return Err(Diagnostic::new(
                    "HELPER_RECOVERY_LEASE_REQUIRED",
                    "An outstanding transfer has no matching saved helper generation. Preserve its staging and ownership evidence.",
                ));
            }
        }
        let _gate = self.helper_library_gate.lock().await;
        let caller = connection.caller().await.ok_or_else(|| {
            Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the caller before installing a helper.",
            )
        })?;
        if temporary_access {
            // Verify the exact native policies for the lifecycle we will use.
            // The admitted Security Manager already has stock EFX admin rights
            // on the qualified targets. Only stored policy can require a group.
            for method in [
                "GetDefaults",
                "IsValidLibraryId",
                "ImportLibrary",
                "ApplyChangesWithDiagnostics",
                "PromoteToProduction",
                "GetLibrary",
                "ExportLibrary",
            ] {
                self.authorize_native_policy(
                    connection,
                    "Ice.Lib.EfxLibraryDesignerSvc",
                    method,
                    holder,
                    run,
                    None,
                    cancel,
                )
                .await?;
            }
        }
        let mut file = LeaseFile::open(
            &self.state_root,
            holder,
            claim,
            &plugin.hash,
            connection,
            &caller,
        )?;
        if file.lease.phase == "removed" {
            if let Some(run) = run {
                if collection::snapshot(run)
                    .await?
                    .helper_transfers
                    .iter()
                    .any(|transfer| {
                        transfer.claim.global_id == file.lease.instance.global_id
                            && !matches!(transfer.phase.as_str(), "closed" | "notAdmitted")
                    })
                {
                    return Err(Diagnostic::new(
                        "HELPER_REMOVAL_WAITING_FOR_TRANSFERS",
                        "Reconcile the old helper's transfers before creating its replacement.",
                    ));
                }
            }
            if helper_libraries::exists(connection, &file.lease.instance.library_id, cancel).await?
            {
                return Err(Diagnostic::new(
                    "HELPER_RECONCILIATION_REQUIRED",
                    "A removed helper identity is present again. Preserve it and reconcile its ownership before continuing.",
                ));
            }
            file.rotate_removed()?;
        }
        self.checkpoint_helper_library(run, &file).await?;
        let result = self
            .provision_helper(&mut file, claim, connection, cancel)
            .await;
        self.checkpoint_helper_library(run, &file).await?;
        result?;
        Ok(file.lease.instance)
    }

    /// Restore an outstanding resource's exact helper, never a successor lease.
    /// Resource callers hold helper_transfer_gate. This retains transfer -> library
    /// lock order. Existing source can be reconciled without the module package.
    pub(super) async fn ensure_retained_helper(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        record: &crate::helpers::HelperTransfer,
        _source: Option<(&Plugin, &HelperContract)>,
        cancel: &CancellationToken,
    ) -> Result<HelperContract, Diagnostic> {
        let _gate = self.helper_library_gate.lock().await;
        let journal = collection::snapshot(run).await?;
        let saved = journal
            .helper_libraries
            .iter()
            .filter(|lease| lease.instance.global_id == record.claim.global_id)
            .collect::<Vec<_>>();
        if saved.is_empty()
            && helper_libraries::exists(connection, &record.claim.library_id, cancel).await?
        {
            // A preinstalled resource helper has no Herbert-owned lease. Preserve
            // its existing cleanup path. Invoke_helper_claim still verifies source.
            return Ok(record.claim.clone());
        }
        if saved.len() != 1 {
            return Err(Diagnostic::new(
                "HELPER_RECOVERY_LEASE_REQUIRED",
                "The retained transfer needs one exact saved helper generation. Preserve its ownership and staging.",
            ));
        }
        let saved = saved[0];
        let mut file = LeaseFile::existing(&self.state_root, &journal.run_id, &saved.template_id)?;
        let caller = connection.caller().await.ok_or_else(|| {
            Diagnostic::new(
                "ADMISSION_REQUIRED",
                "Verify the original caller before helper recovery.",
            )
        })?;
        let retained = journal
            .helper_transfers
            .iter()
            .filter(|transfer| {
                transfer.claim.global_id == record.claim.global_id
                    && !matches!(transfer.phase.as_str(), "closed" | "notAdmitted")
            })
            .collect::<Vec<_>>();
        if !retained
            .iter()
            .any(|transfer| transfer.operation_id == record.operation_id)
        {
            return Err(Diagnostic::new(
                "HELPER_RECOVERY_BINDING_INVALID",
                "The requested transfer is not outstanding in this saved operation.",
            ));
        }
        for transfer in retained {
            crate::helper_recovery::validate_binding(
                &file.lease,
                saved,
                transfer,
                &crate::helper_recovery::RecoveryScope {
                    run_id: &journal.run_id,
                    endpoint: connection.endpoint.as_str(),
                    company: &connection.company,
                    caller: &caller,
                },
            )?;
        }
        if journal.temporary_access_enabled {
            self.authorize_recovery_methods(
                connection,
                run,
                &journal.run_id,
                &["GetLibrary", "ExportLibrary"],
                cancel,
            )
            .await?;
        }
        let library = file.lease.instance.library_id.clone();
        let result = async {
            if helper_libraries::exists(connection, &library, cancel).await? {
                return self.complete_retained_helper(&mut file, connection, run, cancel).await;
            }
            if run.helper_restore_attempts.lock().await.contains(&file.lease.instance.global_id) {
                return Err(Diagnostic::new(
                    "HELPER_RECOVERY_ATTEMPT_UNRESOLVED",
                    "This execution already dispatched restoration for the missing helper. Preserve the original transfer; start an explicit recovery or reconciliation to check again.",
                ));
            }
            let bytes = file.frozen_package()?;
            if journal.temporary_access_enabled {
                self.authorize_recovery_methods(connection, run, &journal.run_id,
                    &["GetDefaults", "IsValidLibraryId", "ImportLibrary",
                      "ApplyChangesWithDiagnostics", "PromoteToProduction"], cancel).await?;
            }
            let defaults = connection.helper_metadata("GetDefaults", json!({}), cancel).await?;
            let version = defaults.value.pointer("/returnObj/EfxLibrary/0/EpicorVersion")
                .and_then(Value::as_str).ok_or_else(|| Diagnostic::new(
                    "HELPER_TARGET_VERSION_UNKNOWN",
                    "The native defaults did not identify the helper package version.",
                ))?;
            let package:Value=serde_json::from_slice(bytes.strip_prefix(&[0xef,0xbb,0xbf]).unwrap_or(&bytes)).map_err(|_|Diagnostic::new("HELPER_RECOVERY_SOURCE_REQUIRED","The retained helper source is malformed."))?;
            if package["Library"]["EpicorVersion"].as_str()!=Some(version) {return Err(Diagnostic::new("HELPER_RECOVERY_VERSION_MISMATCH","The retained helper source targets a different native version."));}
            crate::helper_recovery::verify_bound_bytes(&file.lease, &bytes, true)?;
            self.restore_helper_generation(&mut file, &bytes, connection, run, cancel).await
        }.await;
        self.checkpoint_helper_library(Some(run), &file).await?;
        result?;
        Ok(file.lease.instance)
    }

    async fn authorize_recovery_methods(
        &self,
        connection: &Arc<Connection>,
        run: &Arc<Run>,
        holder: &str,
        methods: &[&str],
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        for method in methods {
            self.authorize_native_policy(
                connection,
                "Ice.Lib.EfxLibraryDesignerSvc",
                method,
                holder,
                Some(run),
                None,
                cancel,
            )
            .await?;
        }
        Ok(())
    }

    /// Native presence after a lost response is reconciled independently. No
    /// installer package is needed when exact retained source already exists.
    async fn complete_retained_helper(
        &self,
        file: &mut LeaseFile,
        connection: &Arc<Connection>,
        run: &Arc<Run>,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let package =
            helper_libraries::export(connection, &file.lease.instance.library_id, cancel).await?;
        file.verify_owned(&package, false)?;
        // Exact native source resolves a previous import outcome. A later,
        // independently observed loss is a new recovery, not an unresolved replay.
        run.helper_restore_attempts
            .lock()
            .await
            .remove(&file.lease.instance.global_id);
        if file.verify_compiled(&package).is_ok() {
            match package["Library"]["Published"].as_bool() {
                Some(true) => {
                    file.verify_owned(&package, true)?;
                    return if file.lease.phase == "ready" {
                        Ok(())
                    } else {
                        file.phase("ready")
                    };
                }
                Some(false) => file.phase("compiled")?,
                None => {
                    return Err(Diagnostic::new(
                        "HELPER_READBACK_INVALID",
                        "The retained helper omitted its publication state.",
                    ));
                }
            }
        } else if package["Library"]["Disabled"].as_bool() == Some(true)
            && package["Library"]["Functions"]
                .as_array()
                .is_some_and(|functions| {
                    functions
                        .iter()
                        .all(|function| function["Disabled"].as_bool() == Some(true))
                })
        {
            file.phase("installed")?;
        } else {
            file.verify_compiled(&package)?;
        }
        let journal = collection::snapshot(run).await?;
        if journal.temporary_access_enabled {
            self.authorize_recovery_methods(
                connection,
                run,
                &journal.run_id,
                &["ApplyChangesWithDiagnostics", "PromoteToProduction"],
                cancel,
            )
            .await?;
        }
        self.checkpoint_helper_library(Some(run), file).await?;
        let claim = file.lease.instance.clone();
        self.provision_helper(file, &claim, connection, cancel)
            .await
    }

    /// Import one exact original package after a fresh absence check. The
    /// execution fence is recorded before dispatch and survives error teardown.
    async fn restore_helper_generation(
        &self,
        file: &mut LeaseFile,
        bytes: &[u8],
        connection: &Arc<Connection>,
        run: &Arc<Run>,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let library = file.lease.instance.library_id.clone();
        let present = helper_libraries::exists(connection, &library, cancel).await?;
        if present {
            return self
                .complete_retained_helper(file, connection, run, cancel)
                .await;
        }
        crate::helper_recovery::verify_bound_bytes(&file.lease, bytes, true)?;
        let valid = connection
            .helper_metadata("IsValidLibraryId", json!({"proposedValue":library}), cancel)
            .await?;
        if valid.value["returnObj"].as_str() != Some("") {
            return Err(Diagnostic::new(
                "HELPER_ID_NOT_SUPPORTED",
                "The target rejected the original helper library identity.",
            ));
        }
        if !run
            .helper_restore_attempts
            .lock()
            .await
            .insert(file.lease.instance.global_id.clone())
        {
            return Err(Diagnostic::new(
                "HELPER_RECOVERY_ATTEMPT_UNRESOLVED",
                "Restoration was already dispatched in this execution. Preserve the transfer and reconcile its outcome.",
            ));
        }
        file.phase("restorePending")?;
        self.checkpoint_helper_library(Some(run), file).await?;
        file.phase("importPending")?;
        self.checkpoint_helper_library(Some(run), file).await?;
        let response = connection
            .owned_write(
                "Ice.Lib.EfxLibraryDesignerSvc",
                "ImportLibrary",
                json!({"libraryPackage":base64::engine::general_purpose::STANDARD.encode(bytes),
                "options":{"NewLibraryId":library,"OverwriteMode":0,"IsProtected":false}}),
                cancel,
            )
            .await;
        let readback = CancellationToken::new();
        if !helper_libraries::exists(connection, &library, &readback).await? {
            // Absence releases no staging ownership and never authorizes rotation.
            file.phase("restorePending")?;
            self.checkpoint_helper_library(Some(run), file).await?;
            return Err(response.err().unwrap_or_else(|| Diagnostic::new(
                "HELPER_IMPORT_UNVERIFIED",
                "Native readback did not find the restored helper. Its original transfer remains owned; explicit recovery can check and retry.",
            )));
        }
        file.verify_owned(
            &helper_libraries::export(connection, &library, &readback).await?,
            false,
        )?;
        run.helper_restore_attempts
            .lock()
            .await
            .remove(&file.lease.instance.global_id);
        file.phase("installed")?;
        self.checkpoint_helper_library(Some(run), file).await?;
        // Complete/read back the imported generation even after a lost response.
        let claim = file.lease.instance.clone();
        let result = self
            .provision_helper(file, &claim, connection, &readback)
            .await;
        self.checkpoint_helper_library(Some(run), file).await?;
        result
    }

    async fn provision_helper(
        &self,
        file: &mut LeaseFile,
        claim: &HelperContract,
        connection: &Arc<Connection>,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let library = file.lease.instance.library_id.clone();
        if file.lease.phase == "ready" {
            file.verify_owned(
                &helper_libraries::export(connection, &library, cancel).await?,
                true,
            )?;
            return Ok(());
        }
        if file.lease.phase == "enablePending" {
            let package = helper_libraries::export(connection, &library, cancel).await?;
            file.verify_owned(&package, false)?;
            if file.verify_compiled(&package).is_ok() {
                file.phase("compiled")?;
            } else if package["Library"]["Disabled"].as_bool() == Some(true)
                && package["Library"]["Functions"]
                    .as_array()
                    .is_some_and(|functions| {
                        functions
                            .iter()
                            .all(|function| function["Disabled"].as_bool() == Some(true))
                    })
            {
                // Readback proved this source is still disabled. Enable from a
                // fresh native tableset, never from a stale mutation payload.
                file.phase("installed")?;
            } else {
                file.verify_compiled(&package)?;
            }
        }
        if file.lease.phase == "activationPending" {
            let package = helper_libraries::export(connection, &library, cancel).await?;
            file.verify_compiled(&package)?;
            if file.verify_owned(&package, true).is_ok() {
                file.phase("ready")?;
            } else if package["Library"]["Published"].as_bool() == Some(false) {
                file.phase("compiled")?;
            } else {
                file.verify_owned(&package, true)?;
            }
        }
        if file.lease.phase == "reserved" {
            let valid = connection
                .helper_metadata(
                    "IsValidLibraryId",
                    json!({"proposedValue": library}),
                    cancel,
                )
                .await?;
            if valid.value["returnObj"].as_str() != Some("") {
                return Err(Diagnostic::new(
                    "HELPER_ID_NOT_SUPPORTED",
                    "The target rejected the reserved helper library identity.",
                ));
            }
            if helper_libraries::exists(connection, &library, cancel).await? {
                return Err(Diagnostic::new(
                    "HELPER_ID_ALREADY_PRESENT",
                    "The reserved native identity already exists. It will not be overwritten.",
                ));
            }
            let defaults = connection
                .helper_metadata("GetDefaults", json!({}), cancel)
                .await?;
            let version = defaults
                .value
                .pointer("/returnObj/EfxLibrary/0/EpicorVersion")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Diagnostic::new(
                        "HELPER_TARGET_VERSION_UNKNOWN",
                        "The native defaults did not identify the helper package version.",
                    )
                })?;
            let installation = claim.installation.as_ref().ok_or_else(|| {
                Diagnostic::new(
                    "HELPER_PACKAGE_MISSING",
                    "The module has no helper package.",
                )
            })?;
            let candidates = installation
                .packages
                .iter()
                .filter(|package| package.epicor_version == version)
                .collect::<Vec<_>>();
            if candidates.len() != 1 {
                return Err(Diagnostic::new(
                    "HELPER_PACKAGE_TARGET_MISMATCH",
                    "The module must supply one exact package for the selected native version.",
                ));
            }
            let (bytes, _) = file.package(&candidates[0].package)?;
            let bytes = file.freeze_package(&bytes)?;
            // The intent and private native ID exist before the import request.
            // Overwrite mode is explicit. Independent absence is checked above.
            file.phase("importPending")?;
            let result = connection.owned_write(
                "Ice.Lib.EfxLibraryDesignerSvc", "ImportLibrary",
                json!({"libraryPackage": base64::engine::general_purpose::STANDARD.encode(bytes), "options": {
                    "NewLibraryId": library, "OverwriteMode": 0, "IsProtected": false
                }}), cancel,
            ).await;
            let present =
                helper_libraries::exists(connection, &library, &CancellationToken::new()).await?;
            if !present {
                // The readback proved no library exists. There is no resource to
                // remove, and the recorded failed attempt must not be replayed.
                file.phase("removed")?;
                return Err(result.err().unwrap_or_else(|| {
                    Diagnostic::new(
                        "HELPER_IMPORT_UNVERIFIED",
                        "Native readback did not find the imported library.",
                    )
                }));
            }
            file.verify_owned(
                &helper_libraries::export(connection, &library, &CancellationToken::new()).await?,
                false,
            )?;
            file.phase("installed")?;
        }
        if file.lease.phase == "importPending" {
            if !helper_libraries::exists(connection, &library, cancel).await? {
                file.phase("removed")?;
                return Err(Diagnostic::new(
                    "HELPER_IMPORT_NOT_PRESENT",
                    "Readback found no library for the interrupted import. Start a new operation to make another attempt.",
                ));
            }
            file.verify_owned(
                &helper_libraries::export(connection, &library, cancel).await?,
                false,
            )?;
            file.phase("installed")?;
        }
        if file.lease.phase == "installed" {
            // Native ImportLibrary creates disabled rows. Enable only this exact
            // owned instance before compilation. Leave all other libraries alone.
            let native = connection
                .native_read(
                    "Ice.Lib.EfxLibraryDesignerSvc",
                    "GetLibrary",
                    json!({"libraryId": library}),
                    false,
                    cancel,
                )
                .await?;
            let mut tableset = native.value["returnObj"].clone();
            for table in ["EfxLibrary", "EfxFunction"] {
                let rows = tableset[table].as_array_mut().ok_or_else(|| {
                    Diagnostic::new(
                        "HELPER_READBACK_INVALID",
                        "The owned helper omitted its library/function rows.",
                    )
                })?;
                for row in rows {
                    if row["LibraryID"].as_str() != Some(library.as_str()) {
                        return Err(Diagnostic::new(
                            "HELPER_OWNER_MISMATCH",
                            "A helper enable row belongs to another library.",
                        ));
                    }
                    row["Disabled"] = json!(false);
                    row["RowMod"] = json!("U");
                }
            }
            file.phase("enablePending")?;
            let enabled = connection
                .owned_write(
                    "Ice.Lib.EfxLibraryDesignerSvc",
                    "ApplyChangesWithDiagnostics",
                    json!({"libraryTableset": tableset}),
                    cancel,
                )
                .await;
            // A lost response can follow a committed compile. Independent readback
            // decides the transition. Never replay the mutation from its error.
            let package =
                helper_libraries::export(connection, &library, &CancellationToken::new()).await?;
            match file.verify_compiled(&package) {
                Ok(()) => file.phase("compiled")?,
                Err(readback) => {
                    if let Ok(response) = &enabled {
                        crate::native_access::verify_update_result(&response.value)?;
                    }
                    return Err(enabled.err().unwrap_or(readback));
                }
            }
        }
        if matches!(file.lease.phase.as_str(), "enabled" | "compilationPending") {
            // ApplyChangesWithDiagnostics compiled the enabled custom functions.
            // RegenerateLibrary is an advertised stub on ICE 5.1.100.29. Verify
            // active, valid native functions instead of calling that stub.
            file.phase("compilationPending")?;
            let package = helper_libraries::export(connection, &library, cancel).await?;
            file.verify_compiled(&package)?;
            file.phase("compiled")?;
        }
        if file.lease.phase == "compiled" {
            file.phase("activationPending")?;
            let result = connection
                .owned_write(
                    "Ice.Lib.EfxLibraryDesignerSvc",
                    "PromoteToProduction",
                    json!({"libraryID": library}),
                    cancel,
                )
                .await;
            let package =
                helper_libraries::export(connection, &library, &CancellationToken::new()).await?;
            if let Err(error) = file.verify_owned(&package, true) {
                return Err(result.err().unwrap_or(error));
            }
            file.phase("ready")?;
        }
        if file.lease.phase != "ready" {
            return Err(Diagnostic::new(
                "HELPER_RECONCILIATION_REQUIRED",
                "The helper's interrupted native transition needs reconciliation before use.",
            ));
        }
        Ok(())
    }

    async fn checkpoint_helper_library(
        &self,
        run: Option<&Arc<Run>>,
        file: &LeaseFile,
    ) -> Result<(), Diagnostic> {
        if let Some(run) = run {
            let lease = file.lease.clone();
            collection::update(run, move |storage| {
                let rows = &mut storage.journal.helper_libraries;
                if let Some(previous) = rows
                    .iter_mut()
                    .find(|previous| previous.instance.global_id == lease.instance.global_id)
                {
                    *previous = lease;
                } else {
                    rows.push(lease);
                }
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    /// Reconcile registered legacy contracts through the shared server lease.
    /// The local record is updated only after independent native absence.
    pub(super) async fn cleanup_leased_contracts(
        &self, run: &Arc<Run>, connection: &Arc<Connection>, library: &str,
    ) -> Vec<Diagnostic> {
        let holder = match collection::snapshot(run).await {
            Ok(journal) => journal.run_id,
            Err(error) => return vec![error],
        };
        let (mut files, mut errors) = match helper_libraries::for_holder(&self.state_root, &holder) {
            Ok(found) => found,
            Err(error) => return vec![error],
        };
        for file in &mut files {
            if file.lease.phase == "removed" { continue; }
            let result = self.cleanup_leased_helper(connection, library,
                &file.lease.instance.library_id, &file.lease.instance.global_id).await;
            match result {
                Ok(true) => {
                    if let Err(error) = file.phase("removed") { errors.push(error); }
                    if let Err(error) = self.checkpoint_helper_library(Some(run), file).await { errors.push(error); }
                }
                Ok(false) => {
                    if let Err(error) = file.phase("released") { errors.push(error); }
                    if let Err(error) = self.checkpoint_helper_library(Some(run), file).await { errors.push(error); }
                },
                Err(error) => errors.push(error),
            }
        }
        errors
    }

    pub(super) async fn cleanup_helper_libraries(
        &self,
        holder: &str,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
    ) -> Vec<Diagnostic> {
        self.cleanup_helper_libraries_for(holder, connection, run, None)
            .await
    }
    pub(super) async fn cleanup_helper_libraries_for(
        &self,
        holder: &str,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
        plugin_hashes: Option<&std::collections::BTreeSet<String>>,
    ) -> Vec<Diagnostic> {
        let _gate = self.helper_library_gate.lock().await;
        if let Err(error) = self.access_store.location().and_then(|root|
            crate::server_collection::ensure_run_settled(&root.join("server-collections"), holder)) {
            return vec![error];
        }
        if let Some(run) = run {
            match collection::snapshot(run).await {
                Ok(journal)
                    if journal.helper_transfers.iter().any(|transfer| {
                        !matches!(transfer.phase.as_str(), "closed" | "notAdmitted")
                    }) =>
                {
                    return vec![Diagnostic::new(
                        "HELPER_REMOVAL_WAITING_FOR_TRANSFERS",
                        "Close the owned server transfers before removing their helper library.",
                    )];
                }
                Err(error) => return vec![error],
                _ => {}
            }
        }
        let (mut files, mut diagnostics) =
            match helper_libraries::for_holder(&self.state_root, holder) {
                Ok(found) => found,
                Err(error) => return vec![error],
            };
        for file in &mut files {
            if plugin_hashes.is_some_and(|selected| !selected.contains(&file.lease.plugin_hash)) {
                continue;
            }
            let result = self.remove_helper_library(file, connection).await;
            if let Err(error) = result {
                diagnostics.push(error);
            }
            if let Err(error) = self.checkpoint_helper_library(run, file).await {
                diagnostics.push(error);
            }
        }
        diagnostics
    }

    async fn remove_helper_library(
        &self,
        file: &mut LeaseFile,
        connection: &Arc<Connection>,
    ) -> Result<(), Diagnostic> {
        if matches!(file.lease.phase.as_str(), "removed" | "released") {
            return Ok(());
        }
        if file.lease.endpoint != connection.endpoint.as_str()
            || file.lease.company != connection.company
            || connection.caller().await.as_deref() != Some(file.lease.caller.as_str())
        {
            return Err(Diagnostic::new(
                "HELPER_OWNER_MISMATCH",
                "Reconnect as the original helper owner before native cleanup.",
            ));
        }
        if self.server_leases.lock().await.values().any(|keeper|
            keeper.contains(&file.lease.instance.global_id)) {
            return Ok(());
        }
        let cancel = CancellationToken::new();
        let library = file.lease.instance.library_id.clone();
        if !helper_libraries::exists(connection, &library, &cancel).await? {
            file.phase("removed")?;
            return Ok(());
        }
        if file.lease.phase == "reserved" {
            return Err(Diagnostic::new(
                "HELPER_NOT_OWNED",
                "A library exists without an import intent. Leave it unchanged.",
            ));
        }
        file.verify_owned(
            &helper_libraries::export(connection, &library, &cancel).await?,
            false,
        )?;
        // ImportLibrary creates a custom library. The native designer deletes
        // custom libraries through ApplyChanges. UninstallLibrary accepts only
        // installed packages. This was checked against ICE 5.1.100.29 source.
        // Obtain a fresh row for its native revision and recheck ownership.
        let native = connection
            .native_read(
                "Ice.Lib.EfxLibraryDesignerSvc",
                "GetLibrary",
                json!({"libraryId": library}),
                false,
                &cancel,
            )
            .await?;
        let rows = native.value["returnObj"]["EfxLibrary"]
            .as_array()
            .ok_or_else(|| {
                Diagnostic::new(
                    "HELPER_READBACK_INVALID",
                    "The helper delete read omitted its library row.",
                )
            })?;
        if rows.len() != 1
            || rows[0]["LibraryID"].as_str() != Some(library.as_str())
            || rows[0]["Owner"].as_str() != Some(file.lease.caller.as_str())
            || rows[0]["Notes"].as_str() != Some(file.lease.owner_marker.as_str())
            || rows[0]["OwnedByCompany"].as_str() != Some(file.lease.company.as_str())
        {
            return Err(Diagnostic::new(
                "HELPER_NOT_OWNED",
                "The fresh helper delete row does not match its recorded ownership.",
            ));
        }
        let mut row = rows[0].clone();
        row["RowMod"] = json!("D");
        file.phase("cleanupPending")?;
        let result = connection
            .owned_write(
                "Ice.Lib.EfxLibraryDesignerSvc",
                "ApplyChangesWithDiagnostics",
                json!({"libraryTableset": {"EfxLibrary": [row]}}),
                &cancel,
            )
            .await;
        if helper_libraries::exists(connection, &library, &cancel).await? {
            return Err(result.err().unwrap_or_else(|| {
                Diagnostic::new(
                    "HELPER_REMOVAL_UNVERIFIED",
                    "Native readback still finds the owned helper library.",
                )
            }));
        }
        file.phase("removed")
    }
}

#[cfg(test)]
#[path = "helper_recovery_tests.rs"]
mod helper_recovery_tests;
