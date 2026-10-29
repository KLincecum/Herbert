//! Neutral guest enrollment over the existing managed job lifecycle.
//! No native selector, definition revision, or category association policy lives here.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Enrollment {
    request: LocalDecompilationRequest,
    job_ids: Vec<String>,
    current_job_id: Option<String>,
    project_state: String,
    diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    history: Vec<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Read {
    category: String,
    alias: String,
    function: String,
    arguments_sha256: String,
    metadata: FileRef,
    files: Vec<FileRef>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct OwnerState {
    baseline_processing: String,
    added_diagnostics: Vec<Diagnostic>,
    file_paths: BTreeSet<String>,
}

fn verify_inputs(store: &Storage, request: &LocalDecompilationRequest) -> Result<(), Diagnostic> {
    store.verify(&request.input)?;
    if let Some(symbols) = &request.symbols {
        store.verify(symbols)?;
    }
    Ok(())
}
fn ensure_local_job(ledger: &mut Ledger, record: &mut Enrollment, engine: &Engine) {
    let id = ensure_job_for(
        ledger,
        &record.request.input,
        record.request.symbols.as_ref(),
        &record.request.symbol_mode,
        engine,
    );
    if !record.diagnostics.is_empty()
        || record.current_job_id.as_ref().is_some_and(|old| old != &id)
    {
        record.history.push(json!({"projectState":record.project_state,"jobId":record.current_job_id,"diagnostics":record.diagnostics}));
    }
    if !record.job_ids.contains(&id) {
        record.job_ids.push(id.clone());
    }
    record.project_state = ledger.jobs[&id].state.clone();
    record.current_job_id = Some(id);
    record.diagnostics.clear();
}
impl Supervisor {
    pub(in crate::application) async fn enqueue_local(
        &self,
        plugin_root: &Path,
        run: &Arc<Run>,
        category: &str,
        target: Option<&Identity>,
        issued: &[FileRef],
        request: LocalDecompilationRequest,
    ) -> Result<LocalDecompilationReceipt, Diagnostic> {
        storage::safe_relative(&request.output_subdirectory)?;
        if request.protocol_version != MANAGED_DECOMPILATION_PROTOCOL
            || request.expected_engine != MANAGED_ENGINE_ID
            || request.request_id.is_empty()
            || request.owner.category != category
            || target != Some(&request.owner)
        {
            return Err(failure(
                "The local request requires the exact current target or a host-validated page owner.",
            ));
        }
        for file in std::iter::once(&request.input).chain(request.symbols.iter()) {
            if file.role != "native" || !issued.contains(file) {
                return Err(issue(
                    "MANAGED_INPUT_OWNERSHIP_DENIED",
                    "Local processing accepts only immutable Epicor files issued to this operation.",
                ));
            }
        }
        if request.symbol_mode == ManagedSymbolMode::ExternalPortable && request.symbols.is_none() {
            return Err(failure(
                "External-symbol processing needs an issued symbol receipt.",
            ));
        }
        // Validate scope and receipts before touching the optional engine.
        {
            let store = run.storage.lock().map_err(|_| lock_error())?;
            verify_inputs(&store, &request)?;
        }
        let engine = self.engine(plugin_root, run).await;
        let receipt = {
            let mut store = run.storage.lock().map_err(|_| lock_error())?;
            verify_inputs(&store, &request)?;
            let id = key(&json!([
                store.journal.endpoint,
                store.journal.company,
                store.journal.caller,
                store.journal.run_id,
                request
            ]));
            let mut ledger = load(&store)?;
            if ledger.local_enrollments.iter().any(|(old_id, record)| {
                record.request.owner.category == category
                    && record.request.request_id == request.request_id
                    && *old_id != id
            }) {
                return Err(failure("A local request ID was reused for different work."));
            }
            let mut record = ledger
                .local_enrollments
                .get(&id)
                .cloned()
                .unwrap_or_else(|| Enrollment {
                    request: request.clone(),
                    job_ids: vec![],
                    current_job_id: None,
                    project_state: "unavailable".into(),
                    diagnostics: vec![],
                    history: vec![],
                });
            match &engine {
                Ok(engine) => ensure_local_job(&mut ledger, &mut record, engine),
                Err(error) => {
                    record.project_state = "unavailable".into();
                    if !record.diagnostics.contains(error) {
                        record.diagnostics.push(error.clone());
                    }
                }
            }
            let receipt = LocalDecompilationReceipt {
                enrollment_id: id.clone(),
                job_id: record.current_job_id.clone(),
                project_state: record.project_state.clone(),
            };
            ledger.local_enrollments.insert(id, record);
            let owned = store
                .journal
                .artifact_acquisitions
                .entry(storage::identity_key(&request.owner))
                .or_default();
            for file in std::iter::once(&request.input).chain(request.symbols.iter()) {
                if !owned.contains(file) {
                    owned.push(file.clone());
                }
            }
            if store.journal.managed_processing.is_none() {
                store.journal.managed_processing = Some(ManagedRunMarker {
                    protocol_version: 1,
                    ledger: STATE.into(),
                });
                store.checkpoint()?;
            }
            save(&store, &ledger)?;
            store.checkpoint()?;
            qualification_boundary("enrollment-before-ack")?;
            receipt
        };
        if let (Some(id), Ok(engine)) = (&receipt.job_id, engine) {
            self.schedule(run.clone(), engine, id.clone()).await?;
        }
        Ok(receipt)
    }
}

pub(in crate::application) fn validate_local_reader_scope(
    store: &Storage,
    value: &Value,
) -> Result<(), Diagnostic> {
    let rows = value
        .pointer("/oDS/Scope")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("The retained reader omitted Epicor user/company scope."))?;
    if rows.len() != 1
        || rows[0]["Caller"] != store.journal.caller
        || rows[0]["Company"] != store.journal.company
    {
        return Err(issue(
            "MANAGED_INPUT_OWNERSHIP_DENIED",
            "The retained reader scope differs from the allowed to start Epicor user/company.",
        ));
    }
    Ok(())
}
pub(in crate::application) fn record_local_read(
    store: &mut Storage,
    category: &str,
    alias: &str,
    function: &str,
    arguments: &Value,
    value: &Value,
    files: &[FileRef],
) -> Result<FileRef, Diagnostic> {
    validate_local_reader_scope(store, value)?;
    for file in files {
        if file.role != "native" {
            return Err(failure(
                "A retained read requires immutable Epicor receipts.",
            ));
        }
        store.verify(file)?;
    }
    let metadata = store.preserve_raw(
        &serde_json::to_vec(value)
            .map_err(|_| failure("Cannot encode retained reader metadata."))?,
        "json",
    )?;
    let record = Read {
        category: category.into(),
        alias: alias.into(),
        function: function.into(),
        arguments_sha256: key(arguments),
        metadata: metadata.clone(),
        files: files.to_vec(),
    };
    let id = key(&serde_json::to_value(&record)
        .map_err(|_| failure("Cannot encode retained reader receipt."))?);
    let mut ledger = load(store)?;
    ledger.local_reads.insert(id, record);
    if store.journal.managed_processing.is_none() {
        store.journal.managed_processing = Some(ManagedRunMarker {
            protocol_version: 1,
            ledger: STATE.into(),
        });
        store.checkpoint()?;
    }
    save(store, &ledger)?;
    store.checkpoint()?;
    Ok(metadata)
}

/// Keep this new acquisition branch outside the large guest-driver poll frame.
/// The debug x64 driver allocated 429304 stack bytes before this extraction;
/// the full CLI helper path overflowed before its first version read completed.
/// Poll the native request without the caller's collector/output poll frames.
/// A matching-symbol live trace measured cumulative exhaustion of the 2MiB
/// Windows debug worker stack across driver + reader + nested helper install.
/// The existing owned request-gate permit moves into the task, so admission
/// cannot overlap while an aborted request still unwinds. No concurrency,
/// defaults or queue policy changes. Drop requests cancellation; the task's
/// permit remains held until the native future and task are actually dropped.
pub(in crate::application) async fn read_local_artifact(
    host: &Host,
    plugin: &Plugin,
    alias: &str,
    function: &str,
    arguments: &Value,
    holder: &str,
    run: &Arc<Run>,
    connection: &Arc<Connection>,
    cancel: &CancellationToken,
    permit: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<(Value, Vec<FileRef>), Diagnostic> {
    let host = host
        .self_owner
        .upgrade()
        .ok_or_else(|| issue("HOST_CLOSED", "The Epicor reader host is no longer owned."))?;
    let plugin = plugin.clone();
    let alias = alias.to_owned();
    let function = function.to_owned();
    let arguments = arguments.clone();
    let holder = holder.to_owned();
    let run = run.clone();
    let connection = connection.clone();
    let cancel = cancel.clone();
    join_native_reader(
        async move {
            read_local_artifact_inner(
                &host,
                &plugin,
                &alias,
                &function,
                &arguments,
                &holder,
                &run,
                &connection,
                &cancel,
            )
            .await
        },
        permit,
    )
    .await
}
async fn join_native_reader(
    future: impl std::future::Future<Output = Result<(Value, Vec<FileRef>), Diagnostic>>
    + Send
    + 'static,
    permit: Option<tokio::sync::OwnedMutexGuard<()>>,
) -> Result<(Value, Vec<FileRef>), Diagnostic> {
    struct AbortRequest(tokio::task::AbortHandle);
    impl Drop for AbortRequest {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let task = tokio::spawn(async move {
        let _permit = permit;
        future.await
    });
    let _abort = AbortRequest(task.abort_handle());
    task.await.map_err(|_|issue("MANAGED_READER_TASK_FAILED","The retained Epicor reader task stopped before returning. Preserved inputs and Epicor operation ownership remain available for recovery."))?
}
async fn read_local_artifact_inner(
    host: &Host,
    plugin: &Plugin,
    alias: &str,
    function: &str,
    arguments: &Value,
    holder: &str,
    run: &Arc<Run>,
    connection: &Arc<Connection>,
    cancel: &CancellationToken,
) -> Result<(Value, Vec<FileRef>), Diagnostic> {
    if !arguments.is_object() {
        return Err(issue(
            "MANAGED_READER_ARGUMENTS_INVALID",
            "The declared retained reader requires an argument object.",
        ));
    }
    if !plugin
        .binary_exports
        .iter()
        .any(|d| d.service == alias && d.method == function)
    {
        return Err(issue(
            "MANAGED_BINARY_DECLARATION_REQUIRED",
            "The retained helper read has no declared binary output paths.",
        ));
    }
    let mut arguments = arguments.clone();
    {
        let store = run.storage.lock().map_err(|_| lock_error())?;
        arguments["iCaller"] = json!(store.journal.caller);
        arguments["iCompany"] = json!(store.journal.company);
    }
    let mut native = Box::pin(host.native_artifact_action(
        plugin,
        HostAction::ArtifactFunctionCall {
            alias: alias.into(),
            function: function.into(),
            arguments: arguments.clone(),
        },
        holder,
        run,
        connection,
        cancel,
    ))
    .await?;
    run.activity.native_response(
        &plugin.category.id,
        None,
        native.elapsed_ms,
        native.raw.len(),
        &native.value,
    );
    {
        let store = run.storage.lock().map_err(|_| lock_error())?;
        validate_local_reader_scope(&store, &native.value)?;
    }
    let blobs = crate::native_export::declared_blobs(
        &plugin.binary_exports,
        alias,
        function,
        &native.value,
    )?;
    let mut acquired = vec![];
    for blob in blobs {
        let file = {
            let mut store = run.storage.lock().map_err(|_| lock_error())?;
            store.preserve_for(None, &blob.bytes, &blob.extension)?
        };
        *native.value.pointer_mut(&blob.pointer).ok_or_else(|| {
            issue(
                "INVALID_NATIVE_BYTES",
                "A declared binary pointer disappeared.",
            )
        })? = json!({"nativeFile":file,"representation":"decoded native bytes"});
        acquired.push(file);
    }
    zeroize::Zeroize::zeroize(&mut native.raw);
    {
        let mut store = run.storage.lock().map_err(|_| lock_error())?;
        acquired.push(record_local_read(
            &mut store,
            &plugin.category.id,
            alias,
            function,
            &arguments,
            &native.value,
            &acquired,
        )?);
    }
    Ok((native.value, acquired))
}

pub(super) fn resume_enrollments(
    store: &Storage,
    ledger: &mut Ledger,
    engine: &Engine,
) -> Result<(), Diagnostic> {
    for id in ledger.local_enrollments.keys().cloned().collect::<Vec<_>>() {
        let mut record = ledger.local_enrollments[&id].clone();
        match verify_inputs(store, &record.request) {
            Ok(()) => ensure_local_job(ledger, &mut record, engine),
            Err(error) => {
                record.project_state = "unavailable".into();
                let diagnostic = issue(
                    "MANAGED_INPUT_UNAVAILABLE",
                    "A file this decompile job depends on is missing or has changed since it was downloaded. Put the original file back, or download it again, then retry.",
                )
                .with_cause(&error.message);
                if !record.diagnostics.contains(&diagnostic) {
                    record.diagnostics.push(diagnostic.clone());
                }
                if let Some(job) = record
                    .current_job_id
                    .as_ref()
                    .and_then(|id| ledger.jobs.get_mut(id))
                {
                    job.state = "recovery-required".into();
                    if !job.diagnostics.contains(&diagnostic) {
                        job.diagnostics.push(diagnostic);
                    }
                }
            }
        }
        ledger.local_enrollments.insert(id, record);
    }
    // ensure_job_for never resets existing failed/cancelled states. The existing
    // explicit retry path alone returns those jobs to queued.
    Ok(())
}
pub(super) fn output_binding(ledger: &Ledger, id: &str) -> Result<(Identity, String), Diagnostic> {
    if let Some(binding) = ledger
        .captures
        .values()
        .find(|c| c.job_ids.iter().any(|j| j == id))
    {
        return Ok((
            binding.request.capture.owner.clone(),
            binding
                .request
                .output_subdirectory
                .clone()
                .unwrap_or_else(|| "Decompiled".into()),
        ));
    }
    let binding = ledger
        .local_enrollments
        .values()
        .find(|record| record.job_ids.iter().any(|j| j == id))
        .ok_or_else(|| failure("The Epicor user output binding is absent."))?;
    Ok((
        binding.request.owner.clone(),
        binding.request.output_subdirectory.clone(),
    ))
}
pub(super) fn project_identity(ledger: &Ledger, id: &str) -> Result<Identity, Diagnostic> {
    if ledger
        .captures
        .values()
        .any(|c| c.job_ids.iter().any(|j| j == id))
    {
        return Ok(super::project_identity(id));
    }
    let (owner, _) = output_binding(ledger, id)?;
    Ok(Identity {
        category: owner.category,
        kind: "LocalManagedProject".into(),
        scope: owner.scope,
        keys: BTreeMap::from([("Project".into(), id.into())]),
    })
}
pub(super) fn attach_outputs(store: &mut Storage, ledger: &mut Ledger) -> Result<(), Diagnostic> {
    let owners = ledger
        .local_enrollments
        .values()
        .map(|record| {
            (
                storage::identity_key(&record.request.owner),
                record.request.owner.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (owner_key, owner) in owners {
        let mut files = vec![];
        let mut diagnostics = vec![];
        let mut receipts = vec![];
        for (id, record) in ledger
            .local_enrollments
            .iter()
            .filter(|(_, r)| r.request.owner == owner)
        {
            let mut placements = vec![];
            for job in record
                .job_ids
                .iter()
                .filter_map(|id| ledger.jobs.get(id))
                .filter(|job| matches!(job.state.as_str(), "complete" | "partial"))
            {
                let placed =
                    owner_project(store, &owner, job, &record.request.output_subdirectory)?;
                for file in &placed {
                    if !files.contains(file) {
                        files.push(file.clone());
                    }
                }
                placements.push(json!({"jobId":job.id,"files":placed}));
            }
            let current = record
                .current_job_id
                .as_ref()
                .and_then(|id| ledger.jobs.get(id));
            diagnostics.extend(record.diagnostics.clone());
            if let Some(job) = current {
                diagnostics.extend(job_run_diagnostics(store, job));
                if !matches!(job.state.as_str(), "complete" | "partial") {
                    diagnostics.push(issue(
                        "MANAGED_PROJECT_INCOMPLETE",
                        format!(
                            "Local project {} is {}. Original files remain available.",
                            job.id, job.state
                        ),
                    ));
                }
            }
            receipts.push(json!({"enrollmentId":id,"request":record.request,"projectState":current.map(|job|job.state.as_str()).unwrap_or(&record.project_state),"currentJobId":record.current_job_id,"jobs":record.job_ids.iter().filter_map(|id|ledger.jobs.get(id)).collect::<Vec<_>>(),"placements":placements,"diagnostics":record.diagnostics,"history":record.history}));
        }
        let(file,conflict)=store.write_derived(&owner,"local-decompilation.json",&serde_json::to_vec_pretty(&json!({"schemaVersion":1,"enrollments":receipts,"scope":"Mechanical local processing receipts; native association and fidelity are declared by the calling plugin."})).map_err(|_|failure("Cannot encode local processing receipts."))?)?;
        if conflict {
            diagnostics.push(issue("MANAGED_OWNER_MANIFEST_CONFLICT","Your edited local processing manifest was preserved; the new receipt is in Conflicts."));
        }
        let nominal = format!(
            "{}/local-decompilation.json",
            store.artifact_folder(&owner)?
        );
        let mut current = files
            .into_iter()
            .filter(|f| store.verify(f).is_ok())
            .collect::<Vec<_>>();
        current.push(file);
        let paths = current
            .iter()
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        if let Some(artifact) = store.manifest.artifacts.get_mut(&owner_key) {
            let previous = ledger.local_owner_state.remove(&owner_key);
            let baseline = if let Some(previous) = &previous {
                artifact
                    .diagnostics
                    .retain(|d| !previous.added_diagnostics.contains(d));
                if artifact.processing == "partial"
                    && !previous.added_diagnostics.is_empty()
                    && artifact.diagnostics.is_empty()
                {
                    previous.baseline_processing.clone()
                } else {
                    artifact.processing.clone()
                }
            } else {
                artifact.processing.clone()
            };
            artifact.processing = baseline.clone();
            artifact.files.retain(|old| {
                old.path != nominal
                    && !paths.contains(&old.path)
                    && !previous
                        .as_ref()
                        .is_some_and(|state| state.file_paths.contains(&old.path))
            });
            for file in current {
                if !artifact.files.contains(&file) {
                    artifact.files.push(file);
                }
            }
            // Remove only diagnostics this bridge actually added last time.
            // Producer diagnostics and historical enrollment evidence stay intact.
            let mut added = vec![];
            if !diagnostics.is_empty() {
                artifact.processing = "partial".into();
                for diagnostic in diagnostics {
                    if !artifact.diagnostics.contains(&diagnostic) {
                        artifact.diagnostics.push(diagnostic.clone());
                        added.push(diagnostic);
                    }
                }
            }
            ledger.local_owner_state.insert(
                owner_key,
                OwnerState {
                    baseline_processing: baseline,
                    added_diagnostics: added,
                    file_paths: paths,
                },
            );
            let saved = artifact.clone();
            if let Some(existing) = store
                .journal
                .artifacts
                .iter_mut()
                .find(|a| a.identity == owner)
            {
                *existing = saved;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod frame_qualification {
    use super::*;
    fn future_bytes<F, T>(_: F) -> usize
    where
        F: FnOnce() -> T,
    {
        std::mem::size_of::<T>()
    }
    #[tokio::test]
    #[ignore = "Offline async type-size measurement with an explicit BPM directives test module"]
    async fn local_native_async_frame_sizes() {
        let root = tempfile::tempdir().unwrap();
        let package = PathBuf::from(std::env::var("HERBERT_BPM_TEST_PACKAGE").unwrap());
        let host = Host::with_roots(
            root.path().join("state"),
            package.join("plugins"),
            Some(root.path().join("coordination")),
        );
        let connection = Connection::new(
            crate::input::endpoint("https://invalid.example/").unwrap(),
            "LAB".into(),
            crate::input::AuthMode::BasicApiKey,
            crate::input::Credentials {
                username: "offline".into(),
                password: String::new(),
                api_key: Some("offline".into()),
            },
            OperatingSettings::default(),
            Budget::new(4).unwrap(),
        )
        .unwrap();
        let run = super::super::tests::run(
            Storage::open(
                &root.path().join("run"),
                false,
                storage::new_journal("frame-sizes".into()),
            )
            .unwrap(),
        );
        let cancel = CancellationToken::new();
        let plugin = Plugin::load(&package.join("plugins/BPM directives/BPM directives.wasm"))
            .await
            .unwrap();
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "materialize".into(),
            category: "bpm-directives".into(),
            company: "LAB".into(),
            identity: None,
            cursor: None,
            page_size: Some(1),
            state: json!({}),
            response: None,
            error: None,
        };
        let artifact = future_bytes(|| {
            host.native_artifact_action_for(
                "bpm-directives",
                HostAction::ArtifactFunctionCall {
                    alias: "compiled_bpm".into(),
                    function: "ReadCompiled".into(),
                    arguments: json!({}),
                },
                "holder",
                &run,
                &connection,
                &cancel,
            )
        });
        let drive = future_bytes(|| {
            host.drive_inner(
                &plugin,
                request,
                &connection,
                &run,
                "holder",
                false,
                &cancel,
                None,
                None,
                vec![],
            )
        });
        let collect = future_bytes(|| {
            host.clone().collect(
                run.clone(),
                connection.clone(),
                vec!["bpm-directives".into()],
                vec![],
                true,
                Some(1),
            )
        });
        let receipt = json!({"artifactActionFutureBytes":artifact,"driveFutureBytes":drive,"collectFutureBytes":collect,"method":"size_of inferred async output types; factories never invoked, no ERP call or oversized future constructed"});
        fs::write(
            std::env::var("HERBERT_FRAME_RECEIPT").unwrap(),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
        println!("{receipt}");
    }
    #[test]
    fn ordinary_bpm_helper_preparation_preserves_scoped_reader_code() {
        let manifest: Value =
            serde_json::from_str(include_str!("../../../helpers/compiled-bpm/plugin.json"))
                .unwrap();
        let declaration: NativeArtifactDeclaration =
            serde_json::from_value(manifest["artifacts"][0].clone()).unwrap();
        for source in [
            include_bytes!("../../../helpers/compiled-bpm/HerbertBpmCompiled-5.1.100.efxj")
                .as_slice(),
            include_bytes!("../../../helpers/compiled-bpm/HerbertBpmCompiled-5.2.100.efxj")
                .as_slice(),
        ] {
            let before: Value =
                serde_json::from_slice(source.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap()).unwrap();
            let prepared = crate::native_artifact_package::prepare(
                &declaration,
                source,
                "HownedBpmReader",
                uuid::Uuid::new_v4(),
                "LAB",
                "offline",
            )
            .unwrap();
            assert_eq!(
                prepared.metadata["Library"]["Functions"],
                before["Library"]["Functions"]
            );
            assert_eq!(prepared.metadata["LibraryId"], "HownedBpmReader");
            assert_eq!(prepared.metadata["Library"]["Owner"], "offline");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn owner() -> Identity {
        Identity {
            category: "bpm-directives".into(),
            kind: "MethodDirectiveGroup".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([
                ("Source".into(), "BO".into()),
                ("BpMethodCode".into(), "Ice.BO.UD39.GetNewUD39".into()),
            ]),
        }
    }
    fn request(input: FileRef) -> LocalDecompilationRequest {
        LocalDecompilationRequest {
            protocol_version: 1,
            request_id: "local-job".into(),
            expected_engine: MANAGED_ENGINE_ID.into(),
            owner: owner(),
            input,
            symbols: None,
            symbol_mode: ManagedSymbolMode::None,
            output_subdirectory: "Decompiled/assembly".into(),
        }
    }

    #[test]
    fn expected_limits_stay_in_manifest_but_real_failures_affect_run() {
        for failed in [false, true] {
            for category in ["baqs-and-ubaqs", "bpm-directives", "functions"] {
                let root=tempfile::tempdir().unwrap();
                let mut store=Storage::open(root.path(),false,storage::new_journal("result-policy".into())).unwrap();
                let mut owner=owner(); owner.category=category.into();
                crate::collection_output::write_record(&mut store,owner.clone(),json!({"herbertDirectOutput":true,"definition":{"id":"fixture"}})).unwrap();
                let input=store.preserve_raw(b"synthetic input","dll").unwrap();
                let mut req=request(input.clone());req.owner=owner.clone();
                let engine=Engine{path:PathBuf::new(),manifest:EngineManifest{protocol_version:1,engine_id:MANAGED_ENGINE_ID.into(),target_rid:rid().unwrap().into(),executable:"unused".into(),sha256:"a".repeat(64),bytes:0,cancel_grace_milliseconds:1}};
                let mut ledger=load(&store).unwrap();
                let mut record=Enrollment{request:req,job_ids:vec![],current_job_id:None,project_state:"queued".into(),diagnostics:vec![],history:vec![]};
                ensure_local_job(&mut ledger,&mut record,&engine);
                let id=record.current_job_id.clone().unwrap();
                ledger.local_enrollments.insert("fixture".into(),record);
                save(&store,&ledger).unwrap();
                let out=root.path().join("output");fs::create_dir(&out).unwrap();
                let project=b"<Project />";fs::write(out.join("Reconstructed.csproj"),project).unwrap();
                let coverage=json!([{"kind":"type","outcome":if failed{"failed"}else{"partial"},"detail":"fixture"},{"kind":"resource","outcome":"unavailable","detail":"External resource is not downloaded"}]);
                let result=json!({"protocolVersion":1,"jobId":id,"engineId":MANAGED_ENGINE_ID,"inputSha256":input.sha256,"outcome":"partial","symbolDisposition":"none","symbolSha256":null,"unresolvedReferences":["External.Dependency"],"coverage":coverage,"files":[{"path":"Reconstructed.csproj","bytes":project.len(),"sha256":storage::sha256(project),"kind":"project","logicalName":null}],"diagnostics":["Reconstruction fidelity note"],"elapsedMilliseconds":1});
                fs::write(out.join("result.json"),serde_json::to_vec(&result).unwrap()).unwrap();
                let run=super::super::tests::run(store);
                commit_result(&run,&engine,&id,&out,&out,&ledger.jobs[&id]).unwrap();
                let mut store=run.storage.lock().unwrap();let mut ledger=load(&store).unwrap();
                attach_outputs(&mut store,&mut ledger).unwrap();
                let artifact=&store.manifest.artifacts[&storage::identity_key(&owner)];
                assert_eq!(artifact.processing,if failed{"partial"}else{"derived"});
                assert_eq!(artifact.diagnostics.len(),usize::from(failed));
                if failed { assert_eq!(artifact.diagnostics[0].code,"MANAGED_PROJECT_GENERATION_FAILED"); }
                assert_eq!(crate::application::collection::terminal_phase(&store.journal),if failed{"partial"}else{"completed"});
                let file=ledger.jobs[&id].files.iter().find(|f|f.path.ends_with("/result.json")).unwrap().clone();
                let committed:Value=serde_json::from_slice(&fs::read(store.confined(&file.path).unwrap()).unwrap()).unwrap();
                assert_eq!(committed["coverage"],coverage);assert_eq!(committed["diagnostics"],result["diagnostics"]);assert_eq!(committed["unresolvedReferences"],result["unresolvedReferences"]);
                // Older ledgers must be reclassified from verified evidence, not
                // by blindly hiding their broad partial diagnostic.
                ledger.jobs.get_mut(&id).unwrap().diagnostics=vec![issue("MANAGED_PROJECT_PARTIAL","legacy")];
                attach_outputs(&mut store,&mut ledger).unwrap();
                assert_eq!(store.manifest.artifacts[&storage::identity_key(&owner)].processing,if failed{"partial"}else{"derived"});
                let mut broken=ledger.jobs[&id].clone();broken.diagnostics.push(issue("MANAGED_INPUT_UNAVAILABLE","missing original"));
                assert!(job_run_diagnostics(&store,&broken).iter().any(|d|d.code=="MANAGED_INPUT_UNAVAILABLE"));
                fs::write(store.confined(&file.path).unwrap(),b"changed").unwrap();
                assert!(job_run_diagnostics(&store,&broken).iter().any(|d|d.code=="MANAGED_PROJECT_PARTIAL"));
            }
        }
    }

    #[tokio::test]
    async fn neutral_enrollment_requires_current_owner_and_issued_input() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Storage::open(
            root.path(),
            false,
            storage::new_journal("neutral-ownership".into()),
        )
        .unwrap();
        let input = store.preserve_raw(b"original", "dll").unwrap();
        let run = super::super::tests::run(store);
        let supervisor = Supervisor::default();
        let job = request(input.clone());
        assert!(
            supervisor
                .enqueue_local(
                    &root.path().join("plugins"),
                    &run,
                    "bpm-directives",
                    None,
                    &[input.clone()],
                    job.clone()
                )
                .await
                .is_err()
        );
        assert!(
            supervisor
                .enqueue_local(
                    &root.path().join("plugins"),
                    &run,
                    "baqs-and-ubaqs",
                    Some(&owner()),
                    &[input.clone()],
                    job.clone()
                )
                .await
                .is_err()
        );
        assert!(
            supervisor
                .enqueue_local(
                    &root.path().join("plugins"),
                    &run,
                    "bpm-directives",
                    Some(&owner()),
                    &[],
                    job.clone()
                )
                .await
                .is_err()
        );
        let receipt = supervisor
            .enqueue_local(
                &root.path().join("plugins"),
                &run,
                "bpm-directives",
                Some(&owner()),
                &[input.clone()],
                job.clone(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.project_state, "unavailable");
        let again = supervisor
            .enqueue_local(
                &root.path().join("plugins"),
                &run,
                "bpm-directives",
                Some(&owner()),
                &[input.clone()],
                job,
            )
            .await
            .unwrap();
        assert_eq!(receipt, again);
        let store = run.storage.lock().unwrap();
        let ledger = load(&store).unwrap();
        assert_eq!(ledger.local_enrollments.len(), 1);
        assert!(ledger.captures.is_empty());
        assert!(ledger.current_snapshots.is_empty());
        assert!(store.journal.managed_processing.is_some());
        store.verify(&input).unwrap();
        let mut forged = request(input.clone());
        forged.output_subdirectory = "../escape".into();
        drop(store);
        assert!(
            supervisor
                .enqueue_local(
                    &root.path().join("plugins"),
                    &run,
                    "bpm-directives",
                    Some(&owner()),
                    &[input],
                    forged
                )
                .await
                .is_err()
        );
    }
    #[test]
    fn neutral_reader_scope_and_receipts_survive_reload() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Storage::open(
            root.path(),
            false,
            storage::new_journal("neutral-reader".into()),
        )
        .unwrap();
        let input = store.preserve_raw(b"original", "dll").unwrap();
        let value = json!({"oDS":{"Scope":[{"Caller":store.journal.caller,"Company":store.journal.company}],"Assemblies":[{"Data":{"nativeFile":input}}]}});
        let metadata = record_local_read(
            &mut store,
            "bpm-directives",
            "compiled_bpm",
            "ReadCompiled",
            &json!({"iOwners":"[]"}),
            &value,
            &[input.clone()],
        )
        .unwrap();
        let ledger = load(&store).unwrap();
        assert_eq!(ledger.local_reads.len(), 1);
        store.verify(&metadata).unwrap();
        store.verify(&input).unwrap();
        let mut wrong = value;
        wrong["oDS"]["Scope"][0]["Caller"] = json!("other");
        assert!(
            record_local_read(
                &mut store,
                "bpm-directives",
                "compiled_bpm",
                "ReadCompiled",
                &json!({}),
                &wrong,
                &[input]
            )
            .is_err()
        );
    }
    fn write_owner(store: &mut Storage, owner: &Identity) {
        crate::collection_output::write_record(store,owner.clone(),json!({"herbertDirectOutput":true,"definition":{"nativeOwner":owner},"herbertSources":{"files":[{"name":"authored.cs","text":"original authored source"}]},"herbertCollectionStatus":{"completeness":"collected","diagnostics":[Diagnostic::new("BPM_PRODUCER_NOTE","Preserved producer observation")]}})).unwrap();
        store.checkpoint().unwrap();
    }
    #[tokio::test]
    #[ignore = "Actual pinned AOT companion and control assembly are required"]
    async fn actual_neutral_function_coexistence_and_full_placement_edit_recovery() {
        let package = PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());
        let root = tempfile::tempdir().unwrap();
        let mut store = Storage::open(
            root.path(),
            false,
            storage::new_journal("neutral-coexistence".into()),
        )
        .unwrap();
        let ordinary = owner();
        let ubaq = Identity {
            category: "baqs-and-ubaqs".into(),
            kind: "Baq".into(),
            scope: Scope::Company {
                company: "TESTCO".into(),
            },
            keys: BTreeMap::from([("QueryID".into(), "Sample".into())]),
        };
        let function = super::super::tests::owner("Sample");
        for owner in [&ordinary, &ubaq, &function] {
            write_owner(&mut store, owner);
        }
        let input = store
            .preserve_raw(
                &fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap(),
                "dll",
            )
            .unwrap();
        let run = super::super::tests::run(store);
        let supervisor = Supervisor::default();
        let capture = ManagedCapture {
            owner: function.clone(),
            selector: Some(ManagedStoreSelector {
                folder: ":Efx".into(),
                assembly: "efx.sample".into(),
                version: String::new(),
            }),
            binary_state: ManagedBinaryState::Captured,
            diagnostics: vec![],
            observation: json!({"freshness":"unknown"}),
            definition_sha256: "a".repeat(64),
            definition_revision: json!(1),
            input: Some(input.clone()),
            symbols: None,
            symbol_mode: ManagedSymbolMode::None,
        };
        let function_receipt = supervisor
            .enqueue(
                &package.join("plugins"),
                &run,
                "functions",
                Some(&function),
                &[input.clone()],
                ManagedDecompilationRequest {
                    protocol_version: 1,
                    request_id: "function".into(),
                    expected_engine: MANAGED_ENGINE_ID.into(),
                    create_project: true,
                    output_subdirectory: Some("Decompiled".into()),
                    capture,
                },
            )
            .await
            .unwrap();
        let mut receipts = vec![];
        for (owner, directory, id) in [
            (&ordinary, "Compiled/A/Decompiled", "ordinary-A"),
            (&ordinary, "Compiled/B/Decompiled", "ordinary-B"),
            (&ubaq, "BPM/Update/Compiled/Decompiled", "ubaq"),
        ] {
            let mut job = request(input.clone());
            job.owner = owner.clone();
            job.request_id = id.into();
            job.output_subdirectory = directory.into();
            receipts.push(
                supervisor
                    .enqueue_local(
                        &package.join("plugins"),
                        &run,
                        &owner.category,
                        Some(owner),
                        &[input.clone()],
                        job,
                    )
                    .await
                    .unwrap(),
            );
        }
        assert!(supervisor.drain(&run).await.is_empty());
        let mut store = run.storage.lock().unwrap();
        super::super::attach_outputs(&mut store).unwrap();
        let ledger = load(&store).unwrap();
        assert_eq!(ledger.jobs.len(), 1);
        assert_eq!(ledger.local_enrollments.len(), 3);
        assert_eq!(ledger.captures.len(), 1);
        assert!(receipts.iter().all(|r| r.job_id == function_receipt.job_id));
        for owner in [&ordinary, &ubaq, &function] {
            let artifact=&store.manifest.artifacts[&storage::identity_key(owner)];
            assert_eq!(artifact.processing,"derived");
            assert!(!artifact.diagnostics.iter().any(|d|d.code=="MANAGED_PROJECT_PARTIAL" || d.code=="MANAGED_PROJECT_GENERATION_FAILED"));
        }
        let ordinary_files = store.manifest.artifacts[&storage::identity_key(&ordinary)]
            .files
            .clone();
        let folder = store.artifact_folder(&ordinary).unwrap();
        for directory in ["Compiled/A/Decompiled", "Compiled/B/Decompiled"] {
            assert!(
                ordinary_files
                    .iter()
                    .any(|f| f.path.starts_with(&format!("{folder}/{directory}/"))
                        && f.path.ends_with(".csproj"))
            );
        }
        let edited = ordinary_files
            .iter()
            .find(|f| {
                f.path
                    .starts_with(&format!("{folder}/Compiled/A/Decompiled/"))
                    && f.path.ends_with(".cs")
            })
            .unwrap()
            .path
            .clone();
        fs::write(
            store.confined(&edited).unwrap(),
            b"preserved placement edit",
        )
        .unwrap();
        let sibling = ordinary_files
            .iter()
            .filter(|f| {
                f.path
                    .starts_with(&format!("{folder}/Compiled/B/Decompiled/"))
            })
            .cloned()
            .collect::<Vec<_>>();
        super::super::attach_outputs(&mut store).unwrap();
        assert_eq!(
            fs::read(store.confined(&edited).unwrap()).unwrap(),
            b"preserved placement edit"
        );
        for file in sibling {
            store.verify(&file).unwrap();
            assert!(
                store.manifest.artifacts[&storage::identity_key(&ordinary)]
                    .files
                    .contains(&file)
            );
        }
        for artifact in store.manifest.artifacts.values() {
            let mut paths = BTreeSet::new();
            for file in &artifact.files {
                assert!(paths.insert(&file.path), "one current receipt per path");
                store.verify(file).unwrap();
            }
        }
        assert!(
            store.manifest.artifacts[&storage::identity_key(&ordinary)]
                .diagnostics
                .iter()
                .any(|d| d.code == "BPM_PRODUCER_NOTE")
        );
    }
    #[tokio::test]
    #[ignore = "Actual pinned AOT companion and control assembly are required"]
    async fn actual_neutral_missing_engine_recovers_without_stale_current_diagnostics() {
        let package = PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());
        let root = tempfile::tempdir().unwrap();
        let mut store = Storage::open(
            root.path(),
            false,
            storage::new_journal("neutral-repaired-engine".into()),
        )
        .unwrap();
        let owner = owner();
        write_owner(&mut store, &owner);
        let input = store
            .preserve_raw(
                &fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap(),
                "dll",
            )
            .unwrap();
        let run = super::super::tests::run(store);
        let supervisor = Supervisor::default();
        let receipt = supervisor
            .enqueue_local(
                &root.path().join("missing/plugins"),
                &run,
                "bpm-directives",
                Some(&owner),
                &[input.clone()],
                request(input.clone()),
            )
            .await
            .unwrap();
        assert_eq!(receipt.project_state, "unavailable");
        let stale = {
            let mut store = run.storage.lock().unwrap();
            super::super::attach_outputs(&mut store).unwrap();
            let ledger = load(&store).unwrap();
            ledger.local_owner_state[&storage::identity_key(&owner)]
                .added_diagnostics
                .clone()
        };
        assert!(!stale.is_empty());
        supervisor
            .resume(&package.join("plugins"), &run)
            .await
            .unwrap();
        assert!(supervisor.drain(&run).await.is_empty());
        let mut store = run.storage.lock().unwrap();
        super::super::attach_outputs(&mut store).unwrap();
        let ledger = load(&store).unwrap();
        let current = &ledger.local_enrollments[&receipt.enrollment_id];
        assert!(current.diagnostics.is_empty());
        assert!(!current.history.is_empty());
        let artifact = &store.manifest.artifacts[&storage::identity_key(&owner)];
        assert!(stale.iter().all(|d| !artifact.diagnostics.contains(d)));
        assert!(
            artifact
                .diagnostics
                .iter()
                .any(|d| d.code == "BPM_PRODUCER_NOTE")
        );
        for file in &artifact.files {
            store.verify(file).unwrap();
        }
    }
    #[tokio::test]
    #[ignore = "Actual pinned AOT companion is required"]
    async fn actual_neutral_cancelled_job_requires_explicit_retry() {
        let package = PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());
        let root = tempfile::tempdir().unwrap();
        let mut store = Storage::open(
            root.path(),
            false,
            storage::new_journal("neutral-cancelled".into()),
        )
        .unwrap();
        let owner = owner();
        write_owner(&mut store, &owner);
        let input = store.preserve_raw(b"not admitted", "dll").unwrap();
        let run = super::super::tests::run(store);
        let supervisor = Supervisor::with_workers(1);
        let held = supervisor.workers.acquire().await.unwrap();
        let receipt = supervisor
            .enqueue_local(
                &package.join("plugins"),
                &run,
                "bpm-directives",
                Some(&owner),
                &[input.clone()],
                request(input),
            )
            .await
            .unwrap();
        run.cancel.cancel();
        assert_eq!(supervisor.drain(&run).await.len(), 1);
        drop(held);
        let id = receipt.job_id.unwrap();
        assert_eq!(
            load(&run.storage.lock().unwrap()).unwrap().jobs[&id].state,
            "cancelled"
        );
        supervisor
            .resume(&package.join("plugins"), &run)
            .await
            .unwrap();
        assert!(supervisor.drain(&run).await.is_empty());
        let store = run.storage.lock().unwrap();
        let ledger = load(&store).unwrap();
        assert_eq!(ledger.jobs[&id].state, "cancelled");
        assert!(ledger.jobs[&id].attempt.is_none());
    }
}

#[cfg(test)]
mod reader_task_boundary_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    // Two test workers allow caller cancellation to progress while the native
    // destructor deliberately blocks one; this is not a production worker setting.
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }
    #[test]
    fn host_weak_handle_preserves_exact_identity_without_a_strong_cycle() {
        let root = tempfile::tempdir().unwrap();
        let host = Host::new(root.path().join("state"), root.path().join("plugins"));
        let weak = host.self_owner.clone();
        let owned = weak.upgrade().unwrap();
        assert!(Arc::ptr_eq(&host, &owned));
        drop(owned);
        drop(host);
        assert!(weak.upgrade().is_none());
    }
    #[test]
    fn aborted_caller_keeps_request_gate_until_native_unwind_finishes() {
        let rt = runtime();
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let permit = rt.block_on(gate.clone().lock_owned());
        let active = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (drop_tx, drop_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        struct NativeUnwind {
            began: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
            finished: mpsc::Sender<()>,
            active: Arc<AtomicBool>,
        }
        impl Drop for NativeUnwind {
            fn drop(&mut self) {
                let _ = self.began.send(());
                let _ = self.release.recv();
                self.active.store(false, Ordering::SeqCst);
                let _ = self.finished.send(());
            }
        }
        let native_active = active.clone();
        let outer = rt.spawn(join_native_reader(
            async move {
                let _unwind = NativeUnwind {
                    began: drop_tx,
                    release: release_rx,
                    finished: finished_tx,
                    active: native_active.clone(),
                };
                native_active.store(true, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                std::future::pending::<()>().await;
                Ok((json!({}), vec![]))
            },
            Some(permit),
        ));
        entered_rx.recv().unwrap();
        outer.abort();
        assert!(rt.block_on(outer).unwrap_err().is_cancelled());
        drop_rx.recv().unwrap();
        let native_still_active = active.load(Ordering::SeqCst);
        let admission_held = gate.clone().try_lock_owned().is_err();
        release_tx.send(()).unwrap();
        finished_rx.recv().unwrap();
        let _next = rt.block_on(gate.clone().lock_owned());
        assert!(native_still_active);
        assert!(
            admission_held,
            "The caller must not release admission while native work unwinds"
        );
        assert!(
            !active.load(Ordering::SeqCst),
            "No orphan native work survives the released gate"
        );
    }
    #[test]
    fn normal_native_request_is_joined_and_releases_its_existing_gate() {
        let rt = runtime();
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let permit = rt.block_on(gate.clone().lock_owned());
        let reply = rt
            .block_on(join_native_reader(
                async { Ok((json!({"complete":true}), vec![])) },
                Some(permit),
            ))
            .unwrap();
        assert_eq!(reply.0["complete"], true);
        assert!(gate.try_lock_owned().is_ok());
    }
    #[test]
    fn native_task_panic_returns_failure_and_releases_the_gate() {
        let rt = runtime();
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let permit = rt.block_on(gate.clone().lock_owned());
        let reply = rt.block_on(join_native_reader(
            async {
                panic!("intentional reader task control");
                #[allow(unreachable_code)]
                Ok((json!({}), vec![]))
            },
            Some(permit),
        ));
        assert_eq!(reply.unwrap_err().code, "MANAGED_READER_TASK_FAILED");
        assert!(gate.try_lock_owned().is_ok());
    }
}
