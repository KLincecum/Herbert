//! Durable collection and offline processing over the modules actually present.
use super::*;
#[cfg(test)]
#[path = "continuous_fetch_tests.rs"]
mod continuous_fetch_tests;

fn produces_definition(outcome: &Completeness) -> bool {
    !matches!(
        outcome,
        Completeness::VerifiedAbsent | Completeness::VerifiedNotApplicable | Completeness::Excluded
    )
}
fn definition_views(
    storage: &mut Storage,
    identity: &Identity,
    definition: &Value,
    outcome: &Completeness,
) -> Result<(Vec<FileRef>, Vec<Diagnostic>), Diagnostic> {
    if produces_definition(outcome) {
        crate::processing::write_views(storage, identity, definition)
    } else {
        Ok((Vec::new(), Vec::new()))
    }
}

/// A derived reference must identify the processor's collection and point into
/// the exact preserved source. A guest cannot invent another source or scope.
pub(super) fn valid_reference(
    record: &Value,
    processor: &Plugin,
    source: &Identity,
    definition: &Value,
    company: &str,
) -> bool {
    let Some(identity) = record
        .get("identity")
        .and_then(|value| serde_json::from_value::<Identity>(value.clone()).ok())
    else {
        return false;
    };
    if identity.category != processor.category.id
        || identity.scope != source.scope
        || !super::identity_in_scope(&identity, company)
    {
        return false;
    }
    let Some(occurrences) = record.get("occurrences").and_then(Value::as_array) else {
        return false;
    };
    let source_value = json!(source);
    !occurrences.is_empty() && occurrences.iter().all(|occurrence| {
        let Some(pointer) = occurrence.get("pointer").and_then(Value::as_str) else {
            return false;
        };
        occurrence.get("source") == Some(&source_value) && definition.pointer(pointer).is_some()
    })
}
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::collections::{BTreeSet, HashSet};

pub(super) async fn snapshot(run: &Arc<Run>) -> Result<storage::RunJournal, Diagnostic> {
    let storage = run.storage.clone();
    blocking(move || {
        Ok(storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal
            .clone())
    })
    .await
}
pub(super) async fn update(
    run: &Arc<Run>,
    change: impl FnOnce(&mut Storage) -> Result<(), Diagnostic> + Send + 'static,
) -> Result<(), Diagnostic> {
    let storage = run.storage.clone();
    blocking(move || {
        let mut storage = storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
        change(&mut storage)?;
        storage.checkpoint()
    })
    .await
}
/// `update` for per-artifact records: save on the paced schedule only.
async fn update_paced(
    run: &Arc<Run>,
    change: impl FnOnce(&mut Storage) -> Result<(), Diagnostic> + Send + 'static,
) -> Result<(), Diagnostic> {
    let storage = run.storage.clone();
    blocking(move || {
        let mut storage = storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
        change(&mut storage)?;
        storage.checkpoint_paced()
    })
    .await
}
/// Collected artifacts of one category, counted under the lock. Progress
/// needs only this number; cloning the whole journal for it grew with the run.
pub(super) async fn category_count(run: &Arc<Run>, category: &str) -> Result<usize, Diagnostic> {
    let storage = run.storage.clone();
    let category = category.to_owned();
    blocking(move || {
        Ok(storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal
            .artifacts
            .iter()
            .filter(|a| a.identity.category == category)
            .count())
    })
    .await
}
async fn control(run: &Arc<Run>) -> Result<(), Diagnostic> {
    loop {
        let wake = run.wake.notified();
        if run.cancel.is_cancelled() {
            return Err(Diagnostic::new("CANCELED", "The run was canceled."));
        }
        if !run.paused.load(Ordering::SeqCst) {
            return Ok(());
        }
        tokio::select! {_ = run.cancel.cancelled()=>{},_ = wake=>{}}
    }
}
fn successful(artifact: &Artifact) -> bool {
    matches!(
        artifact.acquisition,
        Completeness::Collected
            | Completeness::VerifiedAbsent
            | Completeness::VerifiedNotApplicable
            | Completeness::Excluded
    ) && artifact.processing == "derived"
}
pub(super) fn terminal_phase(journal: &storage::RunJournal) -> &'static str {
    if journal.managed_processing.is_some() && !cfg!(feature="managed-decompilation") {return "partial";}
    if journal
        .native_artifacts
        .iter()
        .any(|op| op.artifacts.iter().any(|a| a.needs_cleanup()))
    {
        return "partial";
    }
    if journal
        .helper_libraries
        .iter()
        .any(|lease| !matches!(lease.phase.as_str(), "removed" | "released"))
    {
        return "partial";
    }
    if journal.access_leases.iter().any(|lease| {
        matches!(
            lease.state.as_str(),
            "grantPending" | "cleanupPending" | "uncertain"
        )
    }) {
        return "partial";
    }
    if journal
        .helper_transfers
        .iter()
        .any(|transfer| !matches!(transfer.phase.as_str(), "closed" | "notAdmitted"))
    {
        return "partial";
    }
    if journal
        .category_outcomes
        .values()
        .any(|v| matches!(v.as_str(), "discoveryFailed" | "discoveryIncomplete"))
        || journal.artifacts.iter().any(|a| {
            !matches!(
                a.acquisition,
                Completeness::Collected
                    | Completeness::VerifiedAbsent
                    | Completeness::VerifiedNotApplicable
                    | Completeness::Excluded
            ) || a.processing != "derived"
        })
    {
        "partial"
    } else {
        "completed"
    }
}
/// Returns true when the destination was released, so the saved journal is current.
async fn finish(run: &Arc<Run>, result: Result<(), Diagnostic>) -> bool {
    let failed = result.is_err();
    // collect records the failure before teardown so live status can show it.
    let phase = if run.cancel.is_cancelled() {
        "canceled"
    } else if failed {
        "failed"
    } else {
        match snapshot(run).await {
            Ok(journal) => terminal_phase(&journal),
            Err(_) => "failed",
        }
    };
    // Freeze after collection and server cleanup. Persist before releasing the destination.
    let statistics = run.activity.finish_statistics();
    let storage = run.storage.clone();
    let _ = blocking(move || {
        let mut storage = storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
        storage.journal.statistics = Some(statistics);
        Ok(())
    }).await;
    let _ = checkpoint(run, phase).await;
    let storage = run.storage.clone();
    let released = match blocking(move || {
        storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .release_destination()
    })
    .await
    {
        Ok(()) => true,
        Err(error) => {
            let _ = record_diagnostic(run, error).await;
            let _ = checkpoint(run, "destinationReleaseUncertain").await;
            false
        }
    };
    run.active.store(false, Ordering::SeqCst);
    run.wake.notify_waiters();
    released
}

/// Processing configuration shared by the collector and its stream reader.
#[derive(Clone)]
pub(super) struct CollectionContext {
    pub processors: Vec<Plugin>,
    pub page_size: Option<u32>,
    pub full: bool,
    pub processed_inputs: Arc<Mutex<BTreeSet<String>>>,
}

impl Host {
    /// Coordinate the run. Discovery, acquisition and teardown have separate jobs.
    pub(super) async fn collect(
        self: Arc<Self>,
        run: Arc<Run>,
        connection: Arc<Connection>,
        categories: Vec<String>,
        identities: Vec<Identity>,
        full: bool,
        page_size: Option<u32>,
    ) {
        if cfg!(feature="managed-decompilation") {
            if let Err(error)=self.managed.resume(&self.plugin_root,&run).await {let _=record_diagnostic(&run,error).await;}
        } else if run.storage.lock().is_ok_and(|s|s.journal.managed_processing.is_some()) {
            let _=record_diagnostic(&run,Diagnostic::new("MANAGED_HOST_UNAVAILABLE","This download was started by a Herbert build that includes decompiling compiled code. Open it with that build to continue. All saved files were kept.")).await;
        }
        let result = self
            .collect_modules(&run, &connection, &categories, &identities, full, page_size)
            .await;

        if cfg!(feature="managed-decompilation") {
            for diagnostic in self.managed.drain(&run).await {let _=record_diagnostic(&run,diagnostic).await;}
            {let mut storage=run.storage.lock().unwrap_or_else(|e|e.into_inner());if let Err(error)=managed_decompilation::attach_outputs(&mut storage){storage.journal.diagnostics.push(error);}}
        }
        // Show the collection failure before cleanup starts. Cleanup can require
        // several server requests; it must not hide the reason that work stopped.
        if let Err(error) = &result {
            let _ = record_diagnostic(&run, error.clone()).await;
        }

        let _cleanup_activity =
            run.activity
                .begin("Download", None, "Cleaning up temporary helpers and access");
        // A canceled driver can stop between actions. Its ownership journal still
        // identifies exactly which server resources this run must close.
        {
            let _guard = self.helper_transfer_gate.lock().await;
            let cleanup = self.cleanup_helper_transfers(&run, &connection, None).await;
            for diagnostic in cleanup {
                let _ = record_diagnostic(&run, diagnostic).await;
            }
        }
        if let Ok(journal) = snapshot(&run).await {
            for diagnostic in self
                .cleanup_helper_libraries(&journal.run_id, &connection, Some(&run))
                .await
            {
                let _ = record_diagnostic(&run, diagnostic).await;
            }
        }
        self.cleanup_previous_runs(&run, &connection).await;
        for diagnostic in self.cleanup_native_artifacts(&run, &connection).await {
            let _ = record_diagnostic(&run, diagnostic).await;
        }
        for diagnostic in self.cleanup_native_export_files(&run, &connection).await {
            let _ = record_diagnostic(&run, diagnostic).await;
        }
        if let Err(diagnostic) = self.cleanup_access(&run, &connection).await {
            let _ = record_diagnostic(&run, diagnostic).await;
        }
        // Completed, failed and canceled runs all end here.
        if let Ok(journal) = snapshot(&run).await {
            self.forget_borrow_previews(&journal.run_id);
        }
        let released = finish(&run, result).await;
        self.settle_run(&run, released).await;
    }

    /// Discover server activity independently of local run indexes and exports.
    async fn cleanup_previous_runs(&self, current: &Arc<Run>, connection: &Arc<Connection>) {
        if let Err(error) = self.sweep_server_helpers(current, connection).await {
            let _ = record_diagnostic(current, error).await;
        }
    }

    async fn collect_modules(
        self: &Arc<Self>,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        categories: &[String],
        identities: &[Identity],
        full: bool,
        page_size: Option<u32>,
    ) -> Result<(), Diagnostic> {
        let (installed, diagnostics) = self.installed_for(&run.cancel).await?;
        for diagnostic in diagnostics {
            record_diagnostic(run, diagnostic).await?;
        }

        let selected: BTreeSet<_> = categories.iter().cloned().collect();
        // A saved run is bound to its actual modules. This is recovery evidence,
        // not an expected-plugin roster for a new backup.
        let saved = snapshot(run).await?;
        for (category, hash) in &saved.plugin_hashes {
            if selected.contains(category)
                && !installed.iter().any(|plugin| {
                    plugin.category.available
                        && crate::legacy_category_ids::same(&plugin.category.id, category)
                        && plugin.hash == *hash
                })
            {
                // Name the plugin, never a retired code from a Release 1 journal.
                let name = saved
                    .category_names
                    .get(category)
                    .map(String::as_str)
                    .unwrap_or_else(|| crate::legacy_category_ids::current(category));
                return Err(Diagnostic::new(
                    "RESUME_PLUGIN_MISMATCH",
                    format!(
                        "The saved run used a different or unavailable module for {name}. Restore that module to resume, or start a new backup."
                    ),
                ));
            }
        }
        let processors = installed
            .iter()
            .filter(|plugin| plugin.category.available && plugin.has_role("definitionProcessor"))
            .filter(|plugin| !full || selected.contains(&plugin.category.id))
            .cloned()
            .collect::<Vec<_>>();
        let mut present = installed
            .into_iter()
            .filter(|plugin| plugin.category.available && selected.contains(&plugin.category.id))
            .collect::<Vec<_>>();

        let mut artifact_plugins = present.clone();
        for processor in &processors {
            if !artifact_plugins
                .iter()
                .any(|p| p.category.id == processor.category.id)
            {
                artifact_plugins.push(processor.clone());
            }
        }
        // Listing may use saved descriptors; load each plugin this run uses now and log how.
        for plugin in &artifact_plugins {
            run.activity.record(plugin.prepare(&run.cancel).await?);
        }
        let preparation = run
            .activity
            .begin("Download", None, "Checking plugin requirements");
        let refused = self
            .prepare_native_artifacts(run, connection, &artifact_plugins)
            .await;
        drop(preparation);
        let processors = processors
            .into_iter()
            .filter(|p| !refused.contains(&p.category.id))
            .collect::<Vec<_>>();
        present.retain(|p| !refused.contains(&p.category.id));
        *run.collection_context.lock().await = Some(CollectionContext {
            processors: processors.clone(),
            page_size,
            full,
            processed_inputs: Arc::new(Mutex::new(BTreeSet::new())),
        });
        for category in refused {
            update(run, move |storage| {
                storage
                    .journal
                    .category_outcomes
                    .insert(category, "discoveryFailed".into());
                Ok(())
            })
            .await?;
        }

        // Source collectors run before modules that consume their references.
        // Advertised roles determine this order. The host has no category roster.
        present.sort_by_key(|plugin| {
            (
                plugin.has_role("definitionProcessor"),
                crate::legacy_category_ids::catalog_order(&plugin.category.id),
            )
        });

        // Complete source collectors before consumers read their references.
        // Within each group, independent plugins can download together.
        let parallel = snapshot(run).await?.settings.concurrency as usize;
        let mut first_error = None;
        for consumers in [false, true] {
            if consumers { self.process_existing_reference_inputs(run, &processors).await?; }
            let mut group = Vec::new();
            for plugin in &present {
                if plugin.has_role("definitionProcessor") == consumers {
                    group.push(self.collect_plugin(run, connection, plugin, &processors, identities, full, page_size));
                }
            }
            let mut pending = futures_util::stream::iter(group)
                .buffer_unordered(parallel);
            while let Some(result) = pending.next().await {
                if let Err(error) = result {
                    // Drain admitted work before teardown. A failure in one
                    // plugin must not drop another plugin's in-flight request.
                    record_diagnostic(run, error.clone()).await?;
                    if first_error.is_none() { first_error = Some(error); }
                }
            }
            control(run).await?;
        }
        if let Some(error) = first_error { return Err(error); }
        Ok(())
    }

    /// Run one plugin with ordered requests and independent local processing.
    async fn collect_plugin(
        &self, run: &Arc<Run>, connection: &Arc<Connection>, plugin: &Plugin,
        processors: &[Plugin], identities: &[Identity], full: bool, page_size: Option<u32>,
    ) -> Result<(), Diagnostic> {
        run.activity.plugin_state(&plugin.category.id, "running");
        let result = self.collect_plugin_inner(run, connection, plugin, processors, identities, full, page_size).await;
        // Discovery preserves earlier output and records its failure in the
        // journal. An Ok task result means the task drained, not that it succeeded.
        let saved = snapshot(run).await?;
        let state = match &result {
            Ok(()) => plugin_completion_state(&saved, &plugin.category.id),
            Err(error) if error.code == "CANCELED" => "canceled",
            Err(_) => "failed",
        };
        run.activity.plugin_state(&plugin.category.id, state);
        result
    }

    async fn collect_plugin_inner(
        &self, run: &Arc<Run>, connection: &Arc<Connection>, plugin: &Plugin,
        processors: &[Plugin], identities: &[Identity], full: bool, page_size: Option<u32>,
    ) -> Result<(), Diagnostic> {
        // Required helpers share one collection slot. Native plugins continue
        // while this slot is occupied. Native server operation locks still apply.
        let _helper = if !plugin.category.helpers.is_empty() {
            let _waiting = run.activity.begin(&plugin.category.name, None, "Waiting for server helper slot");
            Some(tokio::select! {
                _ = run.cancel.cancelled() => return Err(Diagnostic::new("CANCELED", "The helper collection was canceled.")),
                guard = self.plugin_helper_gate.lock() => guard,
            })
        } else { None };
        control(run).await?;
        let category = plugin.category.id.clone();
        let module_hash = plugin.hash.clone();
        let stamp = category.clone();
        update(run, move |storage| {
            storage.journal.plugin_hashes.insert(stamp, module_hash);
            Ok(())
        })
        .await?;

        let targets = if full {
            self.discover_targets(run, connection, &plugin, &processors, page_size)
                .await?
        } else {
            identities
                .iter()
                .filter(|identity| identity.category == category)
                .cloned()
                .collect()
        };
        // These collectors write complete definitions as each page arrives.
        // Explicit single-item requests retain their direct acquisition route.
        if !full || !plugin.has_role("pagedDefinitions") {
            self.acquire_targets(
                run,
                connection,
                &plugin,
                &processors,
                targets,
                page_size,
                full,
            )
            .await?;
        }
        Ok(())
    }

    /// Keep every verified identity even when a later discovery page fails.
    pub(super) async fn discover_targets(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        plugin: &Plugin,
        processors: &[Plugin],
        page_size: Option<u32>,
    ) -> Result<Vec<Identity>, Diagnostic> {
        checkpoint(run, "discovering").await?;
        let category = plugin.category.id.clone();
        let saved = snapshot(run).await?;
        let root = self.state_root.clone();
        let scope = crate::native_artifact_store::ArtifactScope {
            endpoint: saved.endpoint.clone(),
            company: saved.company.clone(),
            caller: saved.caller.clone(),
        };
        let owned =
            blocking(move || crate::native_artifact_store::pending_operations(&root, &scope))
                .await?;
        let mut targets = saved.discovered.get(&category).cloned().unwrap_or_default();
        let incremental = !plugin.has_role("pagedDefinitions")
            && !(plugin.has_role("prepareAcquisition")
                && saved.collection_method == CollectionMethod::ServerAssisted);
        if incremental {
            self.acquire_targets(
                run,
                connection,
                plugin,
                processors,
                targets.clone(),
                page_size,
                true,
            )
            .await?;
        }
        if saved
            .discovery_finished
            .get(&category)
            .copied()
            .unwrap_or(false)
        {
            return Ok(if incremental { Vec::new() } else { targets });
        }

        let mut cursor = saved.cursors.get(&category).cloned();
        // Built once and kept current, not rebuilt from every target per page.
        let mut known = targets.iter().map(storage::identity_key).collect::<HashSet<_>>();
        let stop = run.cancel.child_token();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let producer = Arc::new(self.page_memory.producer());
        let buffering = run.activity.begin(&plugin.category.name, None, "Fetching pages while processing continues");
        let fetch = self.fetch_pages(run, connection, plugin, page_size, cursor.clone(), sender, &stop, &producer, &buffering);
        let consume = async {
        let mut seen = HashSet::new();
        let mut incomplete = saved
            .category_outcomes
            .get(&category)
            .is_some_and(|outcome| outcome == "discoveryIncomplete");

        loop {
            control(run).await?;
            if !seen.insert(cursor.clone()) {
                self.discovery_failed(run, &category, Diagnostic::new(
                    "CURSOR_CYCLE",
                    "An Epicor saved position repeated. The run retains the identities from earlier pages.",
                )).await?;
                break;
            }

            let context = if plugin.has_role("definitionProcessor") {
                super::reference_processing::current_reference_context(run, &plugin.category.id).await?
            } else {
                Value::Null
            };
            let request = GuestRequest {
                abi: PLUGIN_ABI,
                operation: "discover".into(),
                category: category.clone(),
                company: connection.company.clone(),
                identity: None,
                cursor: cursor.clone(),
                page_size,
                state: context,
                response: None,
                error: None,
            };

            let entry = tokio::select! {
                _ = stop.cancelled() => return Err(Diagnostic::new("CANCELED", "The page pipeline was canceled.")),
                entry = receiver.recv() => entry,
            };
            let Some(entry) = entry else { break; };
            let result = match entry {
                Err(error) => Err(error),
                Ok(entry) => {
                    let spilled = entry.spilled;
                    let bytes = entry.bytes;
                    let loaded = blocking(move || entry.load()).await?;
                    let crate::page_buffer::Page { cursor: received_cursor, reply, files } = loaded.page;
                    if received_cursor != cursor {
                        return Err(Diagnostic::new("PAGE_BUFFER_ORDER", "The buffered page does not match the next recovery saved position."));
                    }
                    run.activity.buffer_page(&category, bytes, spilled, false);
                    buffering.step(run.activity.buffer_label(&category));
                    let mut materialize = request;
                    materialize.operation = "materialize".into();
                    match reply {
                        // Boxed like `drive`: the helper pipeline's future is large (lease gating included)
                        // and must not be inlined into this already large discovery future.
                        GuestReply::PreparedPage { state, .. } => {
                            materialize.state = state;
                            Box::pin(self.drive_with_sources(plugin, materialize, connection, run, &stop, None, files)).await
                        }
                        reply @ GuestReply::InventoryData { .. } => {
                            Box::pin(self.drive_with_sources(plugin, materialize, connection, run, &stop, Some(reply), files)).await
                        }
                        reply => Ok((reply, files)),
                    }
                }
            };
            match result {
                Ok((
                    GuestReply::Inventory {
                        identities,
                        next_cursor,
                        authoritative,
                        diagnostics,
                    },
                    _,
                )) => {
                    incomplete |= !authoritative;
                    for diagnostic in diagnostics {
                        record_page_diagnostic(run, &category, &identities, diagnostic).await?;
                    }
                    let mut received = Vec::new();
                    for identity in identities {
                        // Deferred cleanup must not cause our temporary helpers
                        // to enter the backup. Match exact keys and verify native
                        // ownership; never exclude a name merely by its prefix.
                        let mut helper = false;
                        for operation in &owned {
                            for artifact in &operation.artifacts {
                                if !artifact.needs_cleanup() {
                                    continue;
                                }
                                let key = match artifact.declaration.artifact_type {
                                    herbert_contract::NativeArtifactType::FunctionLibrary => {
                                        "LibraryID"
                                    }
                                    _ => "QueryID",
                                };
                                if identity.keys.get(key) != Some(&artifact.target) {
                                    continue;
                                }
                                if let Some((native, _)) = crate::native_artifact_backend::probe(
                                    connection,
                                    artifact.declaration.artifact_type,
                                    &artifact.target,
                                    &run.cancel,
                                )
                                .await?
                                {
                                    helper |= artifact.matches_import(&operation.scope, &native);
                                }
                            }
                        }
                        if helper {
                            continue;
                        }
                        if known.insert(storage::identity_key(&identity)) {
                            received.push(identity.clone());
                            targets.push(identity);
                        }
                    }

                    // Only this page's new identities; the journal keeps the
                    // cumulative list, and the log saves just its new tail.
                    let page = received.clone();
                    let stamp = category.clone();
                    let next = next_cursor.clone();
                    let outcome = if incomplete {
                        "discoveryIncomplete"
                    } else if targets.is_empty() && next.is_none() {
                        "verifiedAbsent"
                    } else {
                        "discovered"
                    }
                    .to_string();

                    // Commit only consumed pages. Fetched pages never advance recovery.
                    update(run, move |storage| {
                        storage.journal.discovered.entry(stamp.clone()).or_default().extend(page);
                        storage
                            .journal
                            .category_outcomes
                            .insert(stamp.clone(), outcome);
                        storage
                            .journal
                            .discovery_finished
                            .insert(stamp.clone(), next.is_none());
                        if let Some(cursor) = next {
                            storage.journal.cursors.insert(stamp, cursor);
                        } else {
                            storage.journal.cursors.remove(&stamp);
                        }
                        Ok(())
                    })
                    .await?;
                    // Process discovered objects while later inventory pages arrive.
                    if incremental {
                        self.acquire_targets(
                            run, connection, plugin, processors, received, page_size, true,
                        )
                        .await?;
                        checkpoint(run, "discovering").await?;
                    }
                    cursor = next_cursor;
                    if cursor.is_none() {
                        break;
                    }
                }
                Ok((GuestReply::Failure { diagnostic }, _)) | Err(diagnostic) => {
                    if run.cancel.is_cancelled() {
                        return Err(diagnostic);
                    }
                    self.discovery_failed(run, &category, diagnostic).await?;
                    break;
                }
                _ => {
                    self.discovery_failed(run, &category, Diagnostic::new(
                        "PLUGIN_OUTPUT_INVALID",
                        "The plugin did not return an inventory page. Earlier identities remain available.",
                    )).await?;
                    break;
                }
            }
        }
        Ok(if incremental { Vec::new() } else { targets })
        };
        // A consumer failure stops future requests. Join the producer before
        // helper teardown or run completion. Never leave a detached fetch task.
        let consume = async {
            let result = consume.await;
            stop.cancel();
            result
        };
        let (_, result) = tokio::join!(fetch, consume);
        run.activity.clear_buffer(&category);
        result
    }

    /// Fetch until the server ends the stream. Local writers do not gate this loop.
    async fn fetch_pages(
        &self, run: &Arc<Run>, connection: &Arc<Connection>, plugin: &Plugin,
        page_size: Option<u32>, mut cursor: Option<String>,
        sender: tokio::sync::mpsc::UnboundedSender<Result<crate::page_buffer::BufferedPage, Diagnostic>>,
        cancel: &CancellationToken,
        producer: &Arc<crate::page_buffer::Producer>,
        buffering: &crate::activity::Guard,
    ) {
        let directory = self.state_root.join("page-buffers");
        let mut seen = HashSet::new();
        let mut state = Value::Null;
        let result: Result<(), Diagnostic> = async {
            loop {
                if cancel.is_cancelled() { break; }
                if !seen.insert(cursor.clone()) {
                    return Err(Diagnostic::new("CURSOR_CYCLE", "An Epicor saved position repeated. Earlier pages remain available."));
                }
                if plugin.has_role("definitionProcessor") {
                    state = super::reference_processing::current_reference_context(run, &plugin.category.id).await?;
                }
                let request = GuestRequest { abi:PLUGIN_ABI, operation:"discover".into(),
                    category:plugin.category.id.clone(), company:connection.company.clone(),
                    identity:None, cursor:cursor.clone(), page_size, state, response:None, error:None };
                let (reply, files) = self.drive(plugin, request, connection, run, cancel).await?;
                let (next, next_state) = match &reply {
                    GuestReply::PreparedPage { next_cursor, next_state, .. } => (next_cursor.clone(), next_state.clone()),
                    GuestReply::Inventory { next_cursor, .. } => (next_cursor.clone(), Value::Null),
                    GuestReply::InventoryData { inventory, .. } => match inventory.as_ref() {
                        GuestReply::Inventory { next_cursor, .. } => (next_cursor.clone(), Value::Null),
                        _ => return Err(Diagnostic::new("PLUGIN_OUTPUT_INVALID", "The fetched page has no inventory saved position.")),
                    },
                    GuestReply::Failure { diagnostic } => return Err(diagnostic.clone()),
                    _ => return Err(Diagnostic::new("PLUGIN_OUTPUT_INVALID", "The plugin did not return a collection page.")),
                };
                let page = crate::page_buffer::Page { cursor, reply, files };
                let memory = producer.clone();
                let path = directory.clone();
                let buffered = blocking(move || memory.store(page, &path)).await?;
                run.activity.buffer_page(&plugin.category.id, buffered.bytes, buffered.spilled, true);
                buffering.step(run.activity.buffer_label(&plugin.category.id));
                if sender.send(Ok(buffered)).is_err() { break; }
                if next.is_none() { break; }
                cursor = next;
                state = next_state;
            }
            Ok(())
        }.await;
        if let Err(error) = result { let _ = sender.send(Err(error)); }
    }

    async fn discovery_failed(
        &self,
        run: &Arc<Run>,
        category: &str,
        diagnostic: Diagnostic,
    ) -> Result<(), Diagnostic> {
        let category = category.to_string();
        update(run, move |storage| {
            storage
                .journal
                .category_outcomes
                .insert(category, "discoveryFailed".into());
            Ok(())
        })
        .await?;
        record_diagnostic(run, diagnostic).await
    }

    /// Dispatch unfinished objects one at a time (`concurrency` below is fixed
    /// at 1; it does not read the user's settings).
    pub(super) async fn acquire_targets(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        plugin: &Plugin,
        processors: &[Plugin],
        targets: Vec<Identity>,
        page_size: Option<u32>,
        full: bool,
    ) -> Result<(), Diagnostic> {
        checkpoint(run, "acquiring").await?;
        // Read only what this selection needs. Cloning the whole journal and
        // scanning every artifact for every target grew with the run each page.
        let storage = run.storage.clone();
        let target_keys: BTreeSet<_> = targets.iter().map(storage::identity_key).collect();
        let (completed, collection_method, admitted) = blocking(move || {
            let storage = storage
                .lock()
                .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
            let mut completed = HashSet::new();
            for artifact in &storage.journal.artifacts {
                let key = storage::identity_key(&artifact.identity);
                if !successful(artifact) || !target_keys.contains(&key) { continue; }
                // A completed journal entry is not proof that its files survived a
                // restart. Verify immutable inputs before using it to skip acquisition.
                // Derived views can be edited by the user and follow conflict handling.
                for file in artifact.files.iter()
                    .filter(|file| matches!(file.role.as_str(), "native" | "processingInput"))
                {
                    storage.verify(file)?;
                }
                completed.insert(key);
            }
            let admitted = storage.journal.helper_transfers.iter()
                .filter(|transfer| matches!(transfer.phase.as_str(), "admitted" | "open"))
                .map(|transfer| storage::identity_key(&transfer.identity))
                .collect::<HashSet<_>>();
            Ok((completed, storage.journal.collection_method.clone(), admitted))
        })
        .await?;
        let mut seen = BTreeSet::new();
        let mut remaining = targets
            .into_iter()
            .filter(|identity| {
                let key = storage::identity_key(identity);
                seen.insert(key.clone()) && !completed.contains(&key)
            })
            .collect::<Vec<_>>();

        // Let a collector prepare the complete selection once. Client concurrency
        // applies later to file transfers and local artifact processing.
        if !remaining.is_empty()
            && plugin.has_role("prepareAcquisition")
            && collection_method == CollectionMethod::ServerAssisted
        {
            if remaining.iter().any(|id| {
                id.category != plugin.category.id
                    || !super::identity_in_scope(id, &connection.company)
            }) {
                return Err(Diagnostic::new(
                    "SCOPE_MISMATCH",
                    "A selected identity is outside the fixed collection scope.",
                ));
            }
            let request = GuestRequest {
                abi: PLUGIN_ABI,
                operation: "prepareAcquisition".into(),
                category: plugin.category.id.clone(),
                company: connection.company.clone(),
                identity: None,
                cursor: None,
                page_size,
                state: json!({"identities":remaining}),
                response: None,
                error: None,
            };
            match self
                .drive(plugin, request, connection, run, &run.cancel)
                .await?
            {
                (
                    GuestReply::Inventory {
                        identities,
                        authoritative: true,
                        diagnostics,
                        ..
                    },
                    _,
                ) if identities == remaining => {
                    for diagnostic in diagnostics {
                        record_diagnostic(run, diagnostic).await?;
                    }
                }
                (GuestReply::Failure { diagnostic }, _) => return Err(diagnostic),
                _ => {
                    return Err(Diagnostic::new(
                        "PLUGIN_OUTPUT_INVALID",
                        "Selection preparation did not return the exact requested identities.",
                    ));
                }
            }
            // Streamed inputs have already passed through normal processing.
            // Unpublished failures still need the collector's final reply.
            let context = run.collection_context.lock().await.clone();
            if let Some(context) = context {
                let processed = context.processed_inputs.lock().await;
                remaining.retain(|identity| !processed.contains(&storage::identity_key(identity)));
            }
        }

        // Recover an already admitted row before allocating a new staging slot.
        remaining.sort_by_key(|identity| !admitted.contains(&storage::identity_key(identity)));
        let mut remaining = std::collections::VecDeque::from(remaining);
        let mut pending = FuturesUnordered::new();

        while !remaining.is_empty() || !pending.is_empty() {
            control(run).await?;
            let concurrency = 1;
            while pending.len() < concurrency {
                let Some(identity) = remaining.pop_front() else {
                    break;
                };
                pending.push(self.collect_one(
                    run.clone(),
                    connection.clone(),
                    plugin.clone(),
                    processors.to_vec(),
                    identity,
                    page_size,
                    full,
                    None,
                ));
            }
            if let Some(result) = pending.next().await {
                result?;
            }
        }
        // Artifacts are recorded on the paced schedule; save the rest now.
        checkpoint(run, "acquiring").await
    }

    pub(super) async fn collect_one(
        &self,
        run: Arc<Run>,
        connection: Arc<Connection>,
        plugin: Plugin,
        processors: Vec<Plugin>,
        identity: Identity,
        page_size: Option<u32>,
        full: bool,
        stream_input: Option<Value>,
    ) -> Result<(), Diagnostic> {
        control(&run).await?;
        if !super::identity_in_scope(&identity, &connection.company) {
            return self
                .failed_artifact(
                    &run,
                    identity,
                    Diagnostic::new(
                        "SCOPE_MISMATCH",
                        "The selected identity is outside the fixed company scope.",
                    ),
                    Completeness::Inaccessible,
                )
                .await;
        }
        let mut context = if plugin
            .category
            .roles
            .iter()
            .any(|r| r == "definitionProcessor")
        {
            super::reference_processing::exact_reference_context(&run, &plugin.category.id, &identity).await?
        } else {
            Value::Null
        };
        let storage = run.storage.clone();
        let inventory_identity = identity.clone();
        let inventory_data = blocking(move || {
            let storage = storage
                .lock()
                .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
            storage
                .journal
                .inventory_data
                .get(&storage::identity_key(&inventory_identity))
                .map(|file| storage.parse_native_json(Some(&inventory_identity), file, &[]))
                .transpose()
        })
        .await?;
        if let Some(data) = inventory_data {
            if !context.is_object() {
                context = json!({});
            }
            context["_inventory"] = data;
        }
        if let Some(input) = stream_input {
            if !context.is_object() {
                context = json!({});
            }
            context["_streamInput"] = input;
        }
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: plugin.category.id.clone(),
            company: connection.company.clone(),
            identity: Some(identity.clone()),
            cursor: None,
            page_size,
            state: context,
            response: None,
            error: None,
        };
        match self
            .drive(&plugin, request, &connection, &run, &run.cancel)
            .await
        {
            Ok((
                GuestReply::Definition {
                    identity,
                    definition,
                    completeness,
                    diagnostics,
                    ..
                },
                _,
            )) => {
                let _saving = run.activity.begin(
                    &plugin.category.name,
                    Some(identity.clone()),
                    "Writing output files",
                );
                if plugin.has_role("pagedDefinitions") && completeness == Completeness::Collected {
                    let mut prepared=self.prepare_reference_records(vec![(identity,definition)], &processors, &connection.company, &run.cancel).await?;
                    prepared.first_mut().ok_or_else(|| Diagnostic::new("PLUGIN_OUTPUT_INVALID", "The selected definition did not produce its prepared record."))?.extend_diagnostics(diagnostics)?;
                    let storage=run.storage.clone();let writers=self.file_writers.clone();let cancel=run.cancel.clone();
                    return blocking(move||{
                        crate::collection_output::write_shared_page(storage.clone(),std::mem::take(&mut prepared),&writers,&cancel,|_|{})?;
                        storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run state is unavailable."))?.checkpoint_paced()
                    }).await;
                }
                let references = if produces_definition(&completeness) {
                    self.process_references(
                        &processors,
                        &identity,
                        &definition,
                        &connection.company,
                        &run.cancel,
                    )
                    .await
                } else {
                    (Vec::new(), Vec::new(), false)
                };
                let storage = run.storage.clone();
                blocking(move || {
                    let mut s = storage.lock().map_err(|_| {
                        Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                    })?;
                    let mut files = s
                        .journal
                        .artifact_acquisitions
                        .get(&storage::identity_key(&identity))
                        .cloned()
                        .unwrap_or_default();
                    let (views, mut processing_diagnostics) =
                        definition_views(&mut s, &identity, &definition, &completeness)?;
                    files.extend(views);
                    let (records, processor_diagnostics, _reference_complete) = references;
                    processing_diagnostics.extend(processor_diagnostics);
                    let dependencies = records
                        .iter()
                        .filter_map(|record| {
                            record
                                .get("identity")
                                .and_then(|id| serde_json::from_value(id.clone()).ok())
                        })
                        .collect();
                    if !processors.is_empty() {
                        crate::processing::replace_references(&mut s, &identity, records);
                    }
                    let processing = if processing_diagnostics
                        .iter()
                        .any(super::reference_processing::reference_error)
                    {
                        "partial"
                    } else {
                        "derived"
                    }
                    .into();
                    let mut diagnostics = diagnostics;
                    diagnostics.extend(processing_diagnostics);
                    for diagnostic in &mut diagnostics {
                        if diagnostic.identity.is_none() {
                            diagnostic.identity = Some(identity.clone());
                        }
                    }
                    s.record_artifact_paced(Artifact {
                        identity,
                        classification: if full {
                            "discoveredDefinition"
                        } else {
                            "explicitlyRequestedDefinition"
                        }
                        .into(),
                        acquisition: completeness,
                        integrity: "hostHashVerified".into(),
                        processing,
                        files,
                        dependencies,
                        diagnostics,
                        source_consistency: "nonAtomicUnknown".into(),
                    })
                })
                .await
            }
            Ok((GuestReply::Failure { diagnostic }, _)) | Err(diagnostic) => {
                if run.cancel.is_cancelled() {
                    return Err(diagnostic);
                }
                let outcome = match diagnostic.code.as_str() {
                    "COLLECTION_METHOD_UNAVAILABLE" => Completeness::Unsupported,
                    "CATEGORY_UNIMPLEMENTED" => Completeness::Unsupported,
                    "ACCESS_DENIED" | "NATIVE_REFUSAL" | "ADMISSION_REQUIRED" => {
                        Completeness::Inaccessible
                    }
                    _ => Completeness::Partial,
                };
                self.failed_artifact(&run, identity, diagnostic, outcome)
                    .await
            }
            _ => {
                self.failed_artifact(
                    &run,
                    identity,
                    Diagnostic::new(
                        "PLUGIN_OUTPUT_INVALID",
                        "Download did not return the exact requested artifact definition.",
                    ),
                    Completeness::Partial,
                )
                .await
            }
        }
    }

    async fn failed_artifact(
        &self,
        run: &Arc<Run>,
        identity: Identity,
        mut diagnostic: Diagnostic,
        acquisition: Completeness,
    ) -> Result<(), Diagnostic> {
        diagnostic.identity = Some(identity.clone());
        update_paced(run, move |s| {
            let files = s
                .journal
                .artifact_acquisitions
                .get(&storage::identity_key(&identity))
                .cloned()
                .unwrap_or_default();
            s.record_artifact_paced(Artifact {
                identity,
                classification: "requestedDefinition".into(),
                acquisition,
                integrity: "preservedNativeFilesHashVerified".into(),
                processing: "failed".into(),
                files,
                dependencies: Vec::new(),
                diagnostics: vec![diagnostic],
                source_consistency: "notEstablished".into(),
            })
        })
        .await
    }

    pub(super) async fn process_references(
        &self,
        processors: &[Plugin],
        source: &Identity,
        definition: &Value,
        company: &str,
        cancel: &CancellationToken,
    ) -> (Vec<Value>, Vec<Diagnostic>, bool) {
        let mut records = Vec::new();
        let mut diagnostics = Vec::new();
        let mut complete = true;
        for processor in processors {
            if processor.category.id == source.category { continue; }
            let request = GuestRequest {
                abi: PLUGIN_ABI,
                operation: "processDefinition".into(),
                category: processor.category.id.clone(),
                company: company.into(),
                identity: Some(source.clone()),
                cursor: None,
                page_size: None,
                state: json!({"definition":definition,"_hostReferenceDiagnostics":true}),
                response: None,
                error: None,
            };
            match processor.call(request, cancel.clone()).await {
                Ok(GuestReply::References { records: returned, diagnostics: reported }) => {
                    diagnostics.extend(reported.into_iter().map(|mut d|{d.identity=Some(source.clone());d.context.insert("processorCategory".into(),json!(processor.category.id));d.context.insert("processorSha256".into(),json!(processor.hash));d}));
                    let mut rejected = false;
                    for record in returned {
                        if valid_reference(&record,processor,source,definition,company){records.push(record);}else{rejected=true;}
                    }
                    if rejected {
                        complete = false;
                        diagnostics.push(Diagnostic::new(
                            "PROCESSOR_REFERENCE_INVALID",
                            "A processor returned a reference outside its collection or source definition.",
                        ));
                    }
                }
                Ok(GuestReply::Failure { mut diagnostic }) | Err(mut diagnostic) => {
                    complete = false;
                    diagnostic.identity=Some(source.clone());diagnostic.context.insert("processorCategory".into(),json!(processor.category.id));diagnostic.context.insert("processorSha256".into(),json!(processor.hash));
                    diagnostics.push(diagnostic);
                }
                _ => {complete = false; diagnostics.push(Diagnostic::new(
                    "PROCESSOR_OUTPUT_INVALID",
                    "A definition processor must return references without requesting Epicor operations.",
                ));},
            }
        }
        (records, diagnostics, complete)
    }

    pub(super) async fn reprocess(self: Arc<Self>, run: Arc<Run>) {
        if cfg!(feature="managed-decompilation"){if let Err(error)=self.managed.resume(&self.plugin_root,&run).await{let _=record_diagnostic(&run,error).await;}}
        let result = async {
            checkpoint(&run, "reprocessing").await?;
            let (installed, diagnostics) = self.installed_for(&run.cancel).await?;
            for diagnostic in diagnostics {
                record_diagnostic(&run, diagnostic).await?;
            }
            let processors = installed
                .into_iter()
                .filter(|p| {
                    p.category.available
                        && p.category.roles.iter().any(|r| r == "definitionProcessor")
                })
                .collect::<Vec<_>>();
            for plugin in &processors {
                run.activity.record(plugin.prepare(&run.cancel).await?);
            }
            self.invalidate_reference_outputs(&run,&processors).await?;
            self.process_reference_inputs(&run, &processors, true).await?;
            let journal = snapshot(&run).await?;
            for mut artifact in journal.artifacts {
                control(&run).await?;
                if processors.iter().any(|p|p.category.id==artifact.identity.category){continue;}
                if !produces_definition(&artifact.acquisition) {
                    continue;
                }
                let Some(view) = journal
                    .definition_views
                    .get(&storage::identity_key(&artifact.identity))
                    .cloned()
                else {
                    continue;
                };
                let reference_only = view.role == "referenceInput";
                if reference_only { continue; }
                let storage = run.storage.clone();
                let definition = blocking(move || {
                    let s = storage.lock().map_err(|_| {
                        Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                    })?;
                    s.verify(&view)?;
                    let bytes = std::fs::read(s.confined(&view.path)?).map_err(|_| {
                        Diagnostic::new(
                            "PROCESSING_INPUT_UNAVAILABLE",
                            "The immutable processing input is unavailable.",
                        )
                    })?;
                    herbert_contract::decode_local_json::<Value>(&bytes).map_err(|_| {
                        Diagnostic::new(
                            "PROCESSING_INPUT_INVALID",
                            "The preserved processing input is invalid JSON.",
                        )
                    })
                })
                .await;
                let definition = match definition {
                    Ok(value) => value,
                    Err(mut error) => {
                        error.identity = Some(artifact.identity.clone());
                        update(&run, move |s| {
                            // Acquisition remains truthful. Only this processing
                            // attempt failed. A later verified retry can recover.
                            artifact.processing = "failed".into();
                            artifact.diagnostics.push(error.clone());
                            s.journal.diagnostics.push(error);
                            // Keep failed file receipts as evidence; clear unverifiable current references. record_artifact
                            // verifies file bytes and cannot record this known failure.
                            crate::processing::replace_references(s,&artifact.identity,vec![]);artifact.dependencies.clear();
                            s.manifest.artifacts.insert(
                                storage::identity_key(&artifact.identity),
                                artifact.clone(),
                            );
                            s.journal
                                .artifacts
                                .retain(|old| old.identity != artifact.identity);
                            s.journal.artifacts.push(artifact);
                            Ok(())
                        })
                        .await?;
                        continue;
                    }
                };
                let (records, processor_diagnostics, _reference_complete) = self
                    .process_references(
                        &processors,
                        &artifact.identity,
                        &definition,
                        &journal.company,
                        &run.cancel,
                    )
                    .await;
                let storage = run.storage.clone();
                let has_processors = !processors.is_empty();
                let previous = artifact.clone();
                let current_records = records.clone();
                let prior_files = artifact.files.clone();
                let output_result = blocking(move || {
                    let mut s = storage.lock().map_err(|_| {
                        Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                    })?;
                    if !reference_only { artifact.files.retain(|file| file.role == "native"); }
                    for file in &artifact.files {
                        s.verify(file)?;
                    }
                    let (views, mut diagnostics) =
                        if reference_only { (vec![],vec![]) } else { crate::processing::write_views(&mut s, &artifact.identity, &definition)? };
                    let output_failed=diagnostics.iter().any(|d|d.code.starts_with("OUTPUT_"));
                    for diagnostic in &mut diagnostics{diagnostic.context.insert("reprocessOutput".into(),json!(true));}
                    artifact.files.extend(views);
                    if output_failed{for file in prior_files{if !artifact.files.iter().any(|f|f.path==file.path){artifact.files.push(file);}}}
                    diagnostics.extend(processor_diagnostics);
                    if has_processors {
                        artifact.dependencies = records
                            .iter()
                            .filter_map(|record| {
                                record
                                    .get("identity")
                                    .and_then(|id| serde_json::from_value(id.clone()).ok())
                            })
                            .collect();
                        crate::processing::replace_references(&mut s, &artifact.identity, records);
                    }
                    artifact.processing =
                        if diagnostics.iter().any(super::reference_processing::reference_error) {
                            "partial"
                        } else {
                            "derived"
                        }
                        .into();
                    artifact.diagnostics.retain(|d| !d.code.starts_with("REFERENCE_") && !d.code.starts_with("PROCESSOR_") && !d.context.contains_key("processorCategory") && d.context.get("referenceInput")!=Some(&json!(true)));
                    artifact.diagnostics.retain(|d| d.context.get("reprocessOutput") != Some(&json!(true)));
                    artifact.diagnostics.extend(diagnostics);
                    artifact.source_consistency="derivedFromPreservedDefinition".into();
                    artifact.integrity="hostHashVerified".into();
                    if output_failed{
                        artifact.processing="partial".into();artifact.integrity="currentProcessingOutputUnverified".into();artifact.source_consistency="currentDefinitionWithUnverifiedViews".into();
                        let mut failure=Diagnostic::new("REPROCESS_OUTPUT_FAILED","Some producer views could not be saved. Current successful receipts and previous file receipts remain preserved; independent producers continue.");failure.identity=Some(artifact.identity.clone());failure.context.insert("reprocessOutput".into(),json!(true));artifact.diagnostics.push(failure);
                        let key=storage::identity_key(&artifact.identity);s.manifest.artifacts.insert(key,artifact.clone());s.journal.artifacts.retain(|a|a.identity!=artifact.identity);s.journal.artifacts.push(artifact);s.checkpoint()
                    }else{s.record_artifact(artifact)}
                })
                .await;
                if let Err(error) = output_result {
                    let mut artifact = previous;
                    let mut failure = Diagnostic::new("REPROCESS_OUTPUT_FAILED", "This producer's current derived files could not be saved and verified. Preserved file receipts remain available; other producers continue and local reprocessing can retry.");
                    failure.identity=Some(artifact.identity.clone());failure.context.insert("cause".into(),json!(error));failure.context.insert("reprocessOutput".into(),json!(true));
                    update(&run,move|s|{
                        artifact.processing="partial".into();artifact.integrity="currentProcessingOutputUnverified".into();artifact.source_consistency="currentDefinitionWithUnverifiedViews".into();
                        artifact.diagnostics.retain(|d|d.context.get("reprocessOutput")!=Some(&json!(true)));artifact.diagnostics.push(failure);
                        if has_processors{artifact.dependencies=current_records.iter().filter_map(|r|serde_json::from_value(r["identity"].clone()).ok()).collect();crate::processing::replace_references(s,&artifact.identity,current_records);}
                        let key=storage::identity_key(&artifact.identity);s.manifest.artifacts.insert(key,artifact.clone());s.journal.artifacts.retain(|a|a.identity!=artifact.identity);s.journal.artifacts.push(artifact);Ok(())
                    }).await?;
                }
            }
            self.refresh_reference_artifacts(&run,&processors).await?;
            Ok(())
        }
        .await;
        if cfg!(feature="managed-decompilation"){
            for diagnostic in self.managed.drain(&run).await{let _=record_diagnostic(&run,diagnostic).await;}
            let mut store=run.storage.lock().unwrap_or_else(|e|e.into_inner());if let Err(error)=managed_decompilation::attach_outputs(&mut store){store.journal.diagnostics.push(error);}
        }
        let released = finish(&run, result).await;
        self.settle_run(&run, released).await;
    }
}

fn plugin_completion_state(journal: &storage::RunJournal, category: &str) -> &'static str {
    match journal.category_outcomes.get(category).map(String::as_str) {
        Some("discoveryFailed") => return "failed",
        Some("discoveryIncomplete") => return "incomplete",
        _ => {}
    }
    if journal.artifacts.iter().filter(|a| a.identity.category == category).any(|a|
        matches!(a.acquisition, Completeness::Partial | Completeness::Inaccessible
            | Completeness::Unsupported | Completeness::DiscoveryFailed)) {
        "incomplete"
    } else {
        "completed"
    }
}

#[cfg(test)]
mod outcome_view_tests {
    use super::*;
    #[test]
    fn absent_and_excluded_keep_receipts_without_named_views() {
        for outcome in [
            Completeness::VerifiedAbsent,
            Completeness::VerifiedNotApplicable,
            Completeness::Excluded,
            Completeness::Collected,
            Completeness::Partial,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut journal = storage::new_journal("outcome".into());
            journal.company = "C".into();
            let mut store = Storage::open(temp.path(), false, journal).unwrap();
            let identity = Identity {
                category: "collection".into(),
                kind: "Definition".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: std::collections::BTreeMap::from([("ID".into(), "Example".into())]),
            };
            let receipt = store
                .preserve_immutable(b"native receipt", "json", "native")
                .unwrap();
            store
                .journal
                .artifact_acquisitions
                .insert(storage::identity_key(&identity), vec![receipt.clone()]);
            let (files, diagnostics) = definition_views(
                &mut store,
                &identity,
                &json!({"authored":"value"}),
                &outcome,
            )
            .unwrap();
            assert!(diagnostics.is_empty());
            store.verify(&receipt).unwrap();
            assert_eq!(
                store.journal.artifact_acquisitions[&storage::identity_key(&identity)].len(),
                1
            );
            if produces_definition(&outcome) {
                assert!(
                    files
                        .iter()
                        .any(|f| f.role == "derived" && f.path.ends_with(".json"))
                );
            } else {
                assert!(files.is_empty());
                assert!(store.journal.definition_views.is_empty());
            }
        }
    }
}

#[cfg(test)]
mod reprocessing_input_tests {
    use super::*;
    fn run(store: Storage) -> Arc<Run> {
        Arc::new(Run {
            storage: Arc::new(std::sync::Mutex::new(store)),
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
            wake: Notify::new(),
            active: AtomicBool::new(true),
            network: false,
            helper_restore_attempts: Mutex::new(BTreeSet::new()),
            activity: Arc::new(Default::default()),
            shared_reads: Default::default(),
            helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
        })
    }
    #[tokio::test]
    async fn corrupt_or_missing_definition_marks_artifact_failed_then_recovers() {
        for missing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join("output");
            let plugins = root.path().join("plugins");
            std::fs::create_dir(&plugins).unwrap();
            let host = Host::with_roots(
                root.path().join("state"),
                plugins,
                Some(root.path().join("coordination")),
            );
            let mut journal = storage::new_journal("offline-reprocess".into());
            journal.company = "C".into();
            let mut store = Storage::open(&output, false, journal).unwrap();
            let identity = Identity {
                category: "fixture".into(),
                kind: "Definition".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: std::collections::BTreeMap::from([("ID".into(), "Example".into())]),
            };
            let native = store
                .preserve_immutable(b"original native bytes", "bin", "native")
                .unwrap();
            store
                .journal
                .artifact_acquisitions
                .insert(storage::identity_key(&identity), vec![native.clone()]);
            let definition = json!({"authored":"preserved value"});
            let (mut files, diagnostics) =
                crate::processing::write_views(&mut store, &identity, &definition).unwrap();
            assert!(diagnostics.is_empty());
            let view = store.journal.definition_views[&storage::identity_key(&identity)].clone();
            let view_path = output.join(&view.path);
            let bytes = std::fs::read(&view_path).unwrap();
            files.push(native.clone());
            store
                .record_artifact(Artifact {
                    identity: identity.clone(),
                    classification: "fixture".into(),
                    acquisition: Completeness::Collected,
                    integrity: "hostHashVerified".into(),
                    processing: "derived".into(),
                    files,
                    dependencies: vec![],
                    diagnostics: vec![],
                    source_consistency: "fixture".into(),
                })
                .unwrap();
            assert_eq!(terminal_phase(&store.journal), "completed");
            if missing {
                std::fs::remove_file(&view_path).unwrap();
            } else {
                std::fs::write(&view_path, b"corrupt bytes").unwrap();
            }
            let first = run(store);
            host.clone().reprocess(first.clone()).await;
            let failed = snapshot(&first).await.unwrap();
            assert_eq!(failed.phase, "partial", "{:?}", failed.diagnostics);
            assert_eq!(failed.artifacts[0].acquisition, Completeness::Collected);
            assert_eq!(failed.artifacts[0].processing, "failed");
            assert_eq!(
                failed.artifacts[0]
                    .diagnostics
                    .last()
                    .unwrap()
                    .identity
                    .as_ref(),
                Some(&identity)
            );
            assert!(!failed.diagnostics.is_empty());
            first.storage.lock().unwrap().verify(&native).unwrap();
            drop(first);
            std::fs::write(&view_path, bytes).unwrap();
            let recovered = run(Storage::open(&output, false, failed.clone()).unwrap());
            host.clone().reprocess(recovered.clone()).await;
            let complete = snapshot(&recovered).await.unwrap();
            assert_eq!(complete.phase, "completed");
            assert_eq!(complete.artifacts[0].processing, "derived");
            assert_eq!(complete.artifacts[0].acquisition, Completeness::Collected);
            assert_eq!(complete.diagnostics, failed.diagnostics);
            assert!(!complete.artifacts[0].diagnostics.is_empty());
            recovered.storage.lock().unwrap().verify(&native).unwrap();
        }
    }
}
