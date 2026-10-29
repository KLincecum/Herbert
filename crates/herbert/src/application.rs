//! Shared application operations for GUI, CLI and MCP.
//! This module owns admitted connections and fixed run scope. Frontends supply commands.
use crate::{
    connection::Connection,
    input::*,
    plugins::Plugin,
    profiles::{self, Profiles},
    scheduler::Budget,
    storage::{self, Storage},
};
use herbert_contract::*;
#[cfg(test)]
#[path = "ubaq_compiled_native_tests.rs"]
mod ubaq_compiled_native_tests;
#[path = "managed_decompilation.rs"]
mod managed_decompilation;
#[path = "artifact_lifecycle.rs"]
mod artifact_lifecycle;
#[path = "host_utility.rs"]
pub(crate) mod host_utility;
#[path = "server_leases.rs"]
mod server_leases;
#[path = "streamed_inputs.rs"]
mod streamed_inputs;
#[path = "collection.rs"]
mod collection;
#[path = "reference_processing.rs"]
mod reference_processing;
#[path = "reference_consumers.rs"]
mod reference_consumers;
#[path = "ready_output.rs"]
pub(crate) mod ready_output;
#[path = "helper_installation.rs"]
mod helper_installation;
#[path = "helper_transfers.rs"]
mod helper_transfers;
#[path = "native_file_export.rs"]
pub(crate) mod native_file_export;
#[path = "native_permissions.rs"]
pub(crate) mod native_permissions;
#[path = "server_change_operations.rs"]
mod server_change_operations;
#[path="server_change_dispatch.rs"]
mod server_change_dispatch;
#[cfg(test)]
#[path="server_change_api_tests.rs"]
pub(crate) mod server_change_api_tests;
#[cfg(test)]
#[path="connection_test_tests.rs"]
mod test_connection_tests;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;

// Host metadata added to sanitized or plugin-scoped native responses saved in the backup.
// 0.2.0 renamed these keys. 0.1.x backups keep their saved bytes: nothing parses the
// keys back, and saved responses are verified by their recorded hashes.
pub(crate) const ACQUISITION_METADATA_KEY: &str = "_herbertAcquisition";
pub(crate) const RETENTION_METADATA_KEY: &str = "_herbertRetention";

fn mark_sanitized_response(value: &mut Value, secrets_omitted: bool, scope_omitted: bool) {
    value[ACQUISITION_METADATA_KEY] = json!({"secretValuesOmitted":secrets_omitted,"outOfScopeRowsOrHistoryOmitted":scope_omitted,"representation":"sanitized native definition response"});
}

fn mark_scoped_response(saved: &mut serde_json::Map<String, Value>) {
    saved.insert(RETENTION_METADATA_KEY.into(), json!({"outOfScopeRowsOmitted":true,"representation":"plugin-scoped native response"}));
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Command {
    pub schema_version: u32,
    pub method: String,
    #[serde(default)]
    pub arguments: Value,
}
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Reply {
    pub schema_version: u32,
    pub ok: bool,
    pub result: Option<Value>,
    pub error: Option<Diagnostic>,
}
impl Reply {
    fn from_result(value: Result<Value, Diagnostic>) -> Self {
        match value {
            Ok(result) => Self {
                schema_version: 1,
                ok: true,
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                schema_version: 1,
                ok: false,
                result: None,
                error: Some(error),
            },
        }
    }
}

pub struct Host {
    // Weak ownership lets a joined native task retain this exact host without a cycle.
    self_owner: std::sync::Weak<Host>,
    pub state_root: PathBuf,
    pub plugin_root: PathBuf,
    access_store: crate::access_store::AccessStore,
    plugin_cache: Mutex<crate::plugins::FolderCache>,
    settings: RwLock<OperatingSettings>,
    decompiler_settings_ready: Mutex<bool>,
    budget: Arc<Budget>,
    connection: RwLock<Option<Arc<Connection>>>,
    last_diagnostic: Mutex<Option<Diagnostic>>,
    runs: Mutex<BTreeMap<String, Arc<Run>>>,
    /// In-memory status fields of runs this host settled (see settle_run). The journal
    /// itself is read from disk, so a settled run keeps no storage, cache or plugins.
    settled_runs: Mutex<BTreeMap<String, Value>>,
    requests: Mutex<BTreeMap<String, (String, String)>>,
    native_operations:
        Mutex<BTreeMap<(String, String), Arc<Mutex<crate::native_artifact_store::OperationFile>>>>,
    managed: managed_decompilation::Supervisor,
    raw_decompiler: Arc<crate::raw_decompiler::Service>,
    helper_transfer_gate: Mutex<()>,
    helper_library_gate: Mutex<()>,
    plugin_request_gates: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    file_writers: Arc<crate::file_writers::WriterPool>,
    page_memory: Arc<crate::page_buffer::BufferMemory>,
    plugin_helper_gate: Mutex<()>,
    utility_gates: Mutex<BTreeMap<(String, String), Arc<Mutex<()>>>>,
    server_leases: Mutex<BTreeMap<String, server_leases::Keeper>>,
    operations: RwLock<()>,
    operation_cancel: CancellationToken,
    // Host connection-session identity, never an Epicor token or login secret.
    authorization_session: RwLock<String>,
    server_change_previews: Mutex<BTreeMap<String, server_change_operations::IssuedPreview>>,
    // Per run: method/unheld-membership keys whose policy preview had no borrow
    // effect. Only that negative answer is reused; see borrow_method_access.
    borrow_preview_negative: std::sync::Mutex<BTreeMap<String, std::collections::BTreeSet<String>>>,
}
struct Run {
    storage: Arc<std::sync::Mutex<Storage>>,
    cancel: CancellationToken,
    paused: AtomicBool,
    wake: Notify,
    active: AtomicBool,
    network: bool,
    // Unresolved restoration intents in this execution. Exact owned native
    // readback clears one. A new explicit recovery creates a fresh context.
    helper_restore_attempts: Mutex<std::collections::BTreeSet<String>>,
    activity: Arc<crate::activity::Activity>,
    shared_reads: crate::shared_reads::ReadCache,
    helper_instances: Mutex<BTreeMap<String, HelperContract>>,
    collection_context: Mutex<Option<collection::CollectionContext>>,
}
impl Host {
    pub async fn shutdown(&self) -> Vec<Diagnostic> {
        // Stop configuration calls and guests, then wait for accepted API calls to
        // settle. No call can register a new run after the shutdown snapshot.
        self.operation_cancel.cancel();
        let _stopping = self.operations.write().await;
        let runs = self.runs.lock().await.values().cloned().collect::<Vec<_>>();
        for run in &runs {
            if run.active.load(Ordering::SeqCst) {
                run.cancel.cancel();
                run.paused.store(false, Ordering::SeqCst);
                run.wake.notify_waiters();
            }
        }
        for run in runs {
            loop {
                let finished = run.wake.notified();
                if !run.active.load(Ordering::SeqCst) {
                    break;
                }
                finished.await;
            }
        }
        self.raw_decompiler.shutdown().await;
        let mut diagnostics = Vec::new();
        if let Some(connection) = self.connection.write().await.take() {
            if let Err(error) = connection.logout(&CancellationToken::new()).await {
                diagnostics.push(error);
            }
        }
        diagnostics
    }
    pub fn new(state_root: PathBuf, plugin_root: PathBuf) -> Arc<Self> {
        Self::with_roots(state_root, plugin_root, None)
    }
    /// Temporary memberships share ownership independently of output and profile
    /// directories. Multiple machines must select the same locking filesystem.
    pub fn with_roots(
        state_root: PathBuf,
        plugin_root: PathBuf,
        coordination_root: Option<PathBuf>,
    ) -> Arc<Self> {
        let access_store = match coordination_root {
            Some(root) => crate::access_store::AccessStore::explicit(root, state_root.clone()),
            None => crate::access_store::AccessStore::default_location(state_root.clone()),
        };
        let settings = OperatingSettings::default();
        let budget = Budget::new(settings.concurrency).expect("Default concurrency must be valid");
        // Descriptors are data, so this cache may live in the user-writable state folder.
        let plugin_cache = crate::plugins::FolderCache::with_descriptor_cache(state_root.join("plugin-descriptors"));
        Arc::new_cyclic(|owner| Self {
            self_owner: owner.clone(),
            state_root,
            plugin_root,
            access_store,
            plugin_cache: Mutex::new(plugin_cache),
            settings: RwLock::new(settings),
            decompiler_settings_ready: Mutex::new(false),
            budget,
            connection: RwLock::new(None),
            last_diagnostic: Mutex::new(None),
            runs: Mutex::new(BTreeMap::new()),
            settled_runs: Mutex::new(BTreeMap::new()),
            requests: Mutex::new(BTreeMap::new()),
            native_operations: Mutex::new(BTreeMap::new()),
            managed: managed_decompilation::Supervisor::default(),
            raw_decompiler: Arc::new(crate::raw_decompiler::Service::default()),
            helper_transfer_gate: Mutex::new(()),
            helper_library_gate: Mutex::new(()),
            plugin_request_gates: Mutex::new(BTreeMap::new()),
            file_writers: Arc::new(crate::file_writers::WriterPool::default()),
            page_memory: Arc::new(crate::page_buffer::BufferMemory::default()),
            plugin_helper_gate: Mutex::new(()),
            utility_gates: Mutex::new(BTreeMap::new()),
            server_leases: Mutex::new(BTreeMap::new()),
            operations: RwLock::new(()),
            operation_cancel: CancellationToken::new(),
            authorization_session: RwLock::new(uuid::Uuid::new_v4().to_string()),
            server_change_previews: Mutex::new(BTreeMap::new()),
            borrow_preview_negative: std::sync::Mutex::new(BTreeMap::new()),
        })
    }
    pub async fn execute(self: &Arc<Self>, command: Command) -> Reply {
        // Disconnect must wait for accepted connection operations and must not
        // race a command that is registering a new network run.
        let (_read, _write) = if command.method == "connection.disconnect" {
            (None, Some(self.operations.write().await))
        } else {
            (Some(self.operations.read().await), None)
        };
        if self.operation_cancel.is_cancelled() {
            return Reply::from_result(Err(Diagnostic::new(
                "HOST_CLOSED",
                "This host is shutting down. Start a new host to continue.",
            )));
        }
        if command.schema_version != 1 {
            return Reply::from_result(Err(Diagnostic::new(
                "UNKNOWN_SCHEMA",
                "The command schemaVersion must be 1.",
            )));
        }
        // Retired Release 1 category codes are refused with their replacement before any work.
        let mut result=match crate::legacy_category_ids::refuse_retired_arguments(&command.arguments) {
            Err(diagnostic)=>Err(diagnostic),
            Ok(())=>self.operation(&command.method, command.arguments).await,
        };
        if command.method != "diagnostics.export" {
            if let Err(diagnostic)=&mut result {
                diagnostic.context.entry("operation".into()).or_insert(json!(command.method));
                *self.last_diagnostic.lock().await=Some(diagnostic.clone());
            }
        }
        Reply::from_result(result)
    }
    async fn initialize_decompiler_settings(&self) -> Result<(),Diagnostic> {
        let mut ready=self.decompiler_settings_ready.lock().await;
        if !*ready {
            let root=self.plugin_root.clone();
            let paths=blocking(move || crate::decompiler_settings::legacy_candidates(&root)).await?;
            let mut legacy=None;
            for path in paths {
                let plugin=Plugin::load(&path).await?;
                if plugin.category.id=="functions" {
                    if legacy.is_some() {return Err(Diagnostic::new("SETTINGS_CONFLICT","Multiple Functions copies contain a legacy worker preference. Preserve them and select the intended plugin copy."));}
                    legacy=Some(plugin);
                }
            }
            let workers=crate::decompiler_settings::initialize(&self.state_root,legacy.as_ref().and_then(Plugin::portable_state))?;
            self.settings.write().await.local_decompiler_workers=workers;
            *ready=true;
        }
        Ok(())
    }
    async fn operation(self: &Arc<Self>, method: &str, args: Value) -> Result<Value, Diagnostic> {
        // Validate empty commands too, so ignored input cannot contradict the
        // schema or turn a malformed request into an unrelated state error.
        if matches!(
            method,
            "schema"
                | "profiles.list"
                | "settings.inspect"
                | "collection.preferences.get"
                | "serverChanges.list"
                | "capabilities"
                | "connection.disconnect"
                | "companies.list"
        ) {
            let _: NoArguments = decode(args.clone())?;
        }
        if matches!(method,"settings.inspect"|"settings.update"|"plugin.configure"|"retrieve.start"|"decompiler.start"|"decompiler.recover") { self.initialize_decompiler_settings().await?; }
        match method {
            "serverChanges.preview"=>self.server_change_preview(decode(args)?).await,
            "serverChanges.authorize"=>self.authorize_server_changes(decode(args)?,"explicitLocalClient").await,
            "serverChanges.list"=>Ok(json!(server_change_operations::AuthorizationList{authorizations:crate::server_changes::list(&self.state_root)?})),
            "serverChanges.inspect"=>{let input:IdInput=decode(args)?;Ok(json!(crate::server_changes::get(&self.state_root,&input.id)?))},
            "serverChanges.revoke"=>{let input:IdInput=decode(args)?;Ok(json!(crate::server_changes::revoke(&self.state_root,&input.id)?))},
            "decompiler.start" => self.raw_decompiler.start(self.state_root.clone(),self.plugin_root.clone(),decode(args)?,self.settings.read().await.local_decompiler_workers).await,
            "decompiler.status" => {let input:IdInput=decode(args)?;self.raw_decompiler.status(&self.state_root,&input.id).await},
            "decompiler.cancel" => {let input:IdInput=decode(args)?;self.raw_decompiler.cancel(&input.id).await},
            "decompiler.recover" => {let input:IdInput=decode(args)?;self.raw_decompiler.recover(self.state_root.clone(),self.plugin_root.clone(),&input.id,self.settings.read().await.local_decompiler_workers).await},
            "schema" => Ok(operation_schema()),
            "run.list" => {
                let _: NoArguments = decode(args)?;
                let root = self.state_root.clone();
                blocking(move || Ok(json!(crate::recovery::RunIndex::open(&root)?.list()?))).await
            }
            "profiles.list" => {
                let root = self.state_root.clone();
                blocking(move || Ok(json!(Profiles::new(&root).list()?))).await
            }
            "profiles.upsert" => {
                let mut profile: Profile = decode(args)?;
                // An explicitly empty company means "not chosen", the same as omitted.
                if profile.defaults.company.as_deref() == Some("") { profile.defaults.company = None; }
                let warnings = if profile.defaults.company.is_none() {
                    vec![Diagnostic::new("COMPANY_NOT_SET", "No company is chosen for this profile. Choose one before downloading; Test connection lists the companies this account can use.")]
                } else { vec![] };
                let root = self.state_root.clone();
                blocking(move || {
                    Profiles::new(&root).upsert(profile)?;
                    Ok(if warnings.is_empty() { json!({"saved":true}) } else { json!({"saved":true,"warnings":warnings}) })
                })
                .await
            }
            "profiles.remove" => {
                let input: IdInput = decode(args)?;
                let root = self.state_root.clone();
                blocking(move || {
                    Profiles::new(&root).remove(&input.id)?;
                    Ok(json!({
                        "removed": true,
                        "credentialCleanup": "Use explicit forget before profile removal to remove its endpoint-bound credential."
                    }))
                }).await
            }
            "profiles.rename" => {
                let input: RenameInput = decode(args)?;
                let root = self.state_root.clone();
                blocking(move || {
                    Profiles::new(&root)
                        .edit(&input.id, |profile| profile.name = Some(input.name))?;
                    Ok(json!({"renamed":true}))
                })
                .await
            }
            "profiles.associateFile" => {
                let input: AssociateFileInput = decode(args)?;
                let _validated =
                    read_environment_file(Path::new(&input.path), &input.environment_id).await?;
                let root = self.state_root.clone();
                blocking(move || {
                    let profiles = Profiles::new(&root);
                    profiles.edit(&input.id, |profile| {
                        profile.user_file = Some(UserFileRef {
                            path: input.path,
                            environment_id: input.environment_id,
                        })
                    })?;
                    Ok(json!({"associated":true,"inputFileChanged":false}))
                })
                .await
            }
            "profiles.removeFile" => {
                let input: IdInput = decode(args)?;
                let root = self.state_root.clone();
                blocking(move || {
                    let profiles = Profiles::new(&root);
                    profiles.edit(&input.id, |profile| profile.user_file = None)?;
                    Ok(json!({"removedReference":true,"inputFileChanged":false}))
                })
                .await
            }
            "credentials.status" | "credentials.forget" => {
                let input: IdInput = decode(args)?;
                let root = self.state_root.clone();
                let forget = method.ends_with("forget");
                blocking(move || {
                    let p = Profiles::new(&root).get(&input.id)?;
                    let endpoint = endpoint(&p.url)?;
                    if forget {
                        profiles::secure_forget(endpoint.as_str(), &p.id)?;
                        Ok(json!({"forgotten":true}))
                    } else {
                        Ok(json!({"status":profiles::secure_status(endpoint.as_str(),&p.id)?}))
                    }
                })
                .await
            }
            "credentials.save" => {
                let input: SaveCredentialInput = decode(args)?;
                let root = self.state_root.clone();
                blocking(move || {
                    let p = Profiles::new(&root).get(&input.id)?;
                    input.credentials.validate(p.auth_mode)?;
                    profiles::secure_save(endpoint(&p.url)?.as_str(), &p.id, &input.credentials)?;
                    Ok(json!({"saved":true}))
                })
                .await
            }
            "settings.inspect" => {
                let mut effective = self.settings.read().await.clone();
                effective.concurrency = self.budget.ceiling();
                Ok(
                    json!({"effective":effective,"timing":"Plugin concurrency applies to new runs. Request pacing applies to new dispatch. Server concurrency and local decompiler workers apply to new runs. Raw decompiler worker changes apply when the current batch is idle. Connection timers and sizes apply to new connections. Run scope remains fixed.","measurement":"Defaults are operating choices, not supported workload limits. Adjust them for the target server."}),
                )
            }
            "collection.preferences.get" => {
                Ok(json!(crate::collection_preferences::read(&self.state_root)?))
            }
            "collection.preferences.set" => {
                let input: crate::collection_preferences::Preferences = decode(args)?;
                crate::collection_preferences::write(&self.state_root, &input)?;
                Ok(json!(input))
            }
            "settings.update" => {
                let mut args=args;
                if let Some(object)=args.as_object_mut() { if !object.contains_key("localDecompilerWorkers") { object.insert("localDecompilerWorkers".into(),json!(self.settings.read().await.local_decompiler_workers)); } }
                let input: OperatingSettings = decode(args)?;
                input.validate()?;
                crate::decompiler_settings::write(&self.state_root,input.local_decompiler_workers)?;
                self.budget.configure_operating(input.concurrency,input.minimum_request_interval_ms)?;
                *self.settings.write().await = input;
                Ok(
                    json!({"updated":true,"effectiveTiming":"Plugin concurrency applies to new runs. Request pacing applies to new dispatch. Server concurrency and local decompiler workers apply to new runs. Raw decompiler worker changes apply when the current batch is idle. Existing responses retain their slots until consumed; other settings apply to new connections."}),
                )
            }
            "serverCollection.inspect" => {
                let _: NoArguments = decode(args)?;
                crate::server_collection::inspect(&self.access_store.location()?.join("server-collections"))
            }
            "serverCollection.reconcile" => {
                let input = decode(args)?;
                crate::server_collection::reconcile(&self.access_store.location()?.join("server-collections"), input)
            }
            "capabilities" => self.capabilities().await,
            "help.read" => {
                let _: NoArguments = decode(args)?;
                Ok(json!({"documents": crate::help::documents()?}))
            }
            "about" => {
                let _: NoArguments = decode(args)?;
                Ok(json!(crate::about::information()))
            }
            "helpers.reconcile" => {
                let input: IdInput = decode(args)?;
                if uuid::Uuid::parse_str(&input.id).is_err() {
                    return Err(Diagnostic::new(
                        "INVALID_OPERATION_ID",
                        "Supply the exact helper operation ID.",
                    ));
                }
                if self
                    .runs
                    .lock()
                    .await
                    .get(&input.id)
                    .is_some_and(|run| run.active.load(Ordering::SeqCst))
                {
                    return Err(Diagnostic::new(
                        "RUN_ACTIVE",
                        "This download is still running. Cancel it, or wait for it to finish, before Herbert cleans up its changes in Epicor.",
                    ));
                }
                self.reconcile_helper_operation(&input.id).await
            }
            "connection.test" => {
                let input: ConnectionInput = decode(args)?;
                let mut active = self.connection.write().await;
                if active.is_some() {
                    return Err(Diagnostic::new(
                        "CONNECTION_ALREADY_ACTIVE",
                        "Disconnect the current connection before you test another connection.",
                    ));
                }
                let connection = self.resolve_connection_for(input, true).await?;
                let cancel = self.operation_cancel.child_token();
                let mut result = connection.connect(&cancel).await;
                if let Ok(output) = &mut result {
                    // Read the company list in the same session, then always sign out.
                    let (companies, source, diagnostic) = connection.available_companies(&cancel).await;
                    let selected = (!connection.is_company_discovery()).then(|| json!({"id":connection.company,"inList":companies.iter().any(|row|row["id"]==connection.company.as_str())}));
                    output["availableCompanies"] = json!(companies);
                    output["companySource"] = json!(source);
                    output["selectedCompany"] = json!(selected);
                    output["companyListDiagnostic"] = json!(diagnostic);
                    output["transportTrust"] = json!(connection.transport_trust());
                    if connection.is_company_discovery() { output["company"] = Value::Null; }
                }
                let cleanup = connection.logout(&CancellationToken::new()).await;
                // A retained test is held only for disconnect: never a confirmed connect, so no
                // authorization-session rotation or preview clear, and no other work may use it.
                if cleanup.is_err() { connection.mark_test_only(); }
                match (result,cleanup) {
                    (Ok(mut output),Ok(())) => { output["cleanup"]=Value::Null; Ok(output) }
                    (Err(error),Ok(())) => Err(error),
                    (Ok(output),Err(mut cleanup)) => {
                        cleanup.context.insert("admission".into(),output);
                        retain_uncertain_connection(&mut cleanup,&mut active,connection);
                        Err(cleanup)
                    }
                    (Err(mut error),Err(cleanup)) => {
                        error.context.insert("authMode".into(),json!(connection.mode));
                        error.context.insert("cleanup".into(),json!(cleanup));
                        retain_uncertain_connection(&mut error,&mut active,connection);
                        Err(error)
                    }
                }
            }
            "connection.connect" => {
                let input: ConnectionInput = decode(args)?;
                // Reserve the host's single connection slot before native Login.
                let mut active = self.connection.write().await;
                if active.is_some() {
                    return Err(Diagnostic::new(
                        "CONNECTION_ALREADY_ACTIVE",
                        "Disconnect the current connection before replacing its fixed scope.",
                    ));
                }
                let connection = self.resolve_connection(input).await?;
                let result = match connection
                    .connect(&self.operation_cancel.child_token())
                    .await
                {
                    Ok(mut result) => { result["transportTrust"] = json!(connection.transport_trust()); result },
                    Err(mut error) => {
                        error.context.insert("authMode".into(),json!(connection.mode));
                        if let Err(cleanup)=connection.logout(&CancellationToken::new()).await {
                            error.context.insert("cleanup".into(),json!(cleanup));
                            retain_uncertain_connection(&mut error,&mut active,connection);
                        }
                        return Err(error);
                    }
                };
                *active = Some(connection);
                *self.authorization_session.write().await = uuid::Uuid::new_v4().to_string();
                self.server_change_previews.lock().await.clear();
                Ok(result)
            }
            "connection.disconnect" => {
                if self
                    .runs
                    .lock()
                    .await
                    .values()
                    .any(|run| run.network && run.active.load(Ordering::SeqCst))
                {
                    return Err(Diagnostic::new(
                        "RUN_ACTIVE",
                        "Cancel or finish active network runs before you disconnect. Their cleanup still needs this connection.",
                    ));
                }
                let mut active = self.connection.write().await;
                let connection = active.as_ref().ok_or_else(|| {
                    Diagnostic::new("CONNECTION_REQUIRED", "No active connection exists.")
                })?;
                connection.logout(&CancellationToken::new()).await?;
                active.take();
                *self.authorization_session.write().await = uuid::Uuid::new_v4().to_string();
                self.server_change_previews.lock().await.clear();
                // Compiled plugin code is reloaded on next use. Runs that still execute
                // (local reprocessing) keep their own references.
                self.plugin_cache.lock().await.unload_modules();
                Ok(json!({"disconnected":true}))
            }
            "companies.list" => {
                let connection = self.current_connection().await?;
                let native = connection
                    .native_read(
                        "Ice.BO.CompanySvc",
                        "GetList",
                        json!({"whereClause":"","pageSize":0,"absolutePage":0}),
                        false,
                        &self.operation_cancel.child_token(),
                    )
                    .await?;
                let mut available = native.value.pointer("/returnObj/CompanyList").and_then(Value::as_array).into_iter().flatten()
                    .filter_map(|row| Some(json!({"id":row["Company"].as_str().filter(|id|!id.is_empty())?,"name":row["Name"].as_str().unwrap_or("")})))
                    .collect::<Vec<_>>();
                available.sort_by(|a,b|a["id"].as_str().cmp(&b["id"].as_str()));
                Ok(
                    json!({"companies":native.value,"availableCompanies":available,"scope":"Authorized discovery only. Each run still selects one company."}),
                )
            }
            "plugin.configure" => {
                let input: PluginInput = decode(args)?;
                let plugin = self.plugin(&input.category).await?;
                let values = resolve_plugin_settings(&plugin.category.settings, &input.values)?;
                let reply = plugin
                    .call(
                        GuestRequest {
                            abi: PLUGIN_ABI,
                            operation: "configure".into(),
                            category: plugin.category.id.clone(),
                            company: String::new(),
                            identity: None,
                            cursor: None,
                            page_size: None,
                            state: json!({"values": values,"selected":input.selected}),
                            response: None,
                            error: None,
                        },
                        self.operation_cancel.child_token(),
                    )
                    .await?;
                match reply {
                    GuestReply::Definition { ref identity, .. }
                        if identity.category != plugin.category.id
                            || identity.scope != Scope::Global =>
                    {
                        Err(Diagnostic::new(
                            "PLUGIN_IDENTITY_DENIED",
                            "Plugin configuration must belong to its declared collection and global scope.",
                        ))
                    }
                    GuestReply::Definition { .. } => Ok(json!(reply)),
                    GuestReply::Failure { diagnostic } => Err(diagnostic),
                    GuestReply::Action { .. } => Err(Diagnostic::new(
                        "PLUGIN_CAPABILITY_DENIED",
                        "Saving plugin options cannot invoke host capabilities.",
                    )),
                    _ => Err(Diagnostic::new(
                        "PLUGIN_OUTPUT_INVALID",
                        "Plugin configuration must return a definition or failure.",
                    )),
                }
            }
            "retrieve.start" | "backup.start" => {
                let input: StartInput = decode(args)?;
                self.start(input, method == "backup.start").await
            }
            "run.status" => {
                let input: StatusInput = decode(args)?;
                let summary = input.summary;
                let live = self.runs.lock().await.get(&input.id).cloned();
                let Some(run) = live else {
                    // settle_run records a settled run here before it leaves `runs`.
                    let settled = self.settled_runs.lock().await.get(&input.id).cloned();
                    let root = self.state_root.clone();
                    let id = input.id;
                    return blocking(move || {
                        let record = crate::recovery::RunIndex::open(&root)?.get(&id)?;
                        let journal = crate::recovery::read_journal(&record)?;
                        let mut status = if summary {
                            crate::progress_snapshot::summary(&journal)
                        } else {
                            crate::progress_snapshot::full(&journal)
                        };
                        status["discoveryMessages"] = crate::progress_snapshot::discovery_messages(&status);
                        status["executionActive"] = json!(false);
                        if let Some(Value::Object(fields)) = settled {
                            for (key, value) in fields { status[key] = value; }
                        }
                        Ok(status)
                    })
                    .await;
                };
                // Capture execution first: a terminal journal can precede final
                // destination cleanup. A stale true requires one extra poll.
                // False must describe a snapshot taken after execution ended.
                let execution_active = run.active.load(Ordering::SeqCst);
                let pause_requested = execution_active && run.paused.load(Ordering::SeqCst);
                let cancel_requested = execution_active && run.cancel.is_cancelled();
                let activity = run.activity.snapshot();
                let stages = run.activity.history();
                let batches = run.activity.batches();
                let storage = run.storage.clone();
                let progress = run.activity.clone();
                blocking(move || {
                    let mut status = if summary && execution_active {
                        crate::progress_snapshot::active_summary(
                            &storage, &progress.progress_summary, &input.id,
                        )?
                    } else {
                        // Terminal status and explicit full reads require fresh
                        // journal evidence, never cached progress counts.
                        let storage = storage.lock().map_err(|_| {
                            Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                        })?;
                        if summary { crate::progress_snapshot::summary(&storage.journal) }
                        else { crate::progress_snapshot::full(&storage.journal) }
                    };
                    status["discoveryMessages"] = crate::progress_snapshot::discovery_messages(&status);
                    status["executionActive"] = json!(execution_active);
                    status["activeOperations"] = activity;
                    status["pluginStates"] = progress.plugin_states();
                    status["activityStages"] = stages;
                    status["transferBatches"] = batches;
                    status["diagnosticLog"] = progress.diagnostics_status();
                    status["statistics"] = progress.statistics();
                    status["pauseRequested"] = json!(pause_requested);
                    status["cancelRequested"] = json!(cancel_requested);
                    Ok(status)
                })
                .await
            }
            "diagnostics.export" => {
                let input: SupportExportInput = decode(args)?;
                let live = if let Some(id)=&input.id {self.runs.lock().await.get(id).cloned()} else {None};
                let (journal,root) = if let Some(run)=live {
                    let statistics=run.activity.statistics();
                    let storage=run.storage.clone();
                    blocking(move || {
                        let storage=storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run state is unavailable."))?;
                        let mut journal=json!(storage.journal);journal["statistics"]=statistics;
                        Ok((journal,storage.root().to_path_buf()))
                    }).await?
                } else if let Some(id)=input.id {
                    let state=self.state_root.clone();
                    blocking(move || {let record=crate::recovery::RunIndex::open(&state)?.get(&id)?;
                        Ok((json!(crate::recovery::read_journal(&record)?),std::path::PathBuf::from(record.destination)))
                    }).await?
                } else {
                    let diagnostic=self.last_diagnostic.lock().await.clone();
                    (json!({"phase":"beforeRun","diagnostics":diagnostic.into_iter().collect::<Vec<_>>()}),self.state_root.clone())
                };
                blocking(move ||crate::support_packet::export(journal,&root,std::path::Path::new(&input.path))).await
            }
            "run.pause" | "run.resume" | "run.cancel" => {
                let input: IdInput = decode(args)?;
                let run = match self.run(&input.id).await {
                    Ok(run) => Some(run),
                    Err(_) if self.settled_runs.lock().await.contains_key(&input.id) => None,
                    Err(error) => return Err(error),
                };
                let Some(run) = run.filter(|run| run.active.load(Ordering::SeqCst)) else {
                    return Ok(
                        json!({"accepted":false,"runId":input.id,"effect":"The run is terminal; use recovery or retry to acquire a new execution lease."}),
                    );
                };
                match method {
                    "run.pause" => run.paused.store(true, Ordering::SeqCst),
                    "run.resume" => {
                        run.paused.store(false, Ordering::SeqCst);
                        run.wake.notify_waiters();
                    }
                    _ => {
                        run.cancel.cancel();
                        run.wake.notify_waiters();
                    }
                }
                Ok(
                    json!({"accepted":true,"runId":input.id,"effect":"Pause stops new dispatch; cancel preserves verified files."}),
                )
            }
            "run.recover" | "run.retryFailed" | "run.reprocess" => {
                let input: IdInput = decode(args)?;
                self.recover(&input.id, method).await
            }
            _ => Err(Diagnostic::new(
                "UNKNOWN_OPERATION",
                "The operation is unknown. Use schema to inspect the shared command surface.",
            )),
        }
    }
    /// Every operation except connect, test and disconnect reaches the server
    /// connection here. A connection.test connection (with or without a company)
    /// can be held only because its logout was uncertain; it stays held so
    /// connection.disconnect can retry the logout and free the Epicor session,
    /// but it is never usable for any other work.
    async fn current_connection(&self) -> Result<Arc<Connection>, Diagnostic> {
        let connection = self.connection.read().await.clone().ok_or_else(|| {
            Diagnostic::new(
                "CONNECTION_REQUIRED",
                "Connect to Epicor first, so Herbert can run its Security Manager sign-in check.",
            )
        })?;
        if connection.is_company_discovery() {
            let mut error = Diagnostic::new("CONNECTION_REQUIRED", "The only held connection is a company-list test whose sign-out could not be confirmed. Disconnect to retry the sign-out, then connect with a company.");
            error.context.insert("recoveryOperation".into(), json!("connection.disconnect"));
            return Err(error);
        }
        if connection.is_test_only() {
            let mut error = Diagnostic::new("CONNECTION_REQUIRED", "The only held connection is a connection test whose sign-out could not be confirmed. Disconnect to retry the sign-out, then connect.");
            error.context.insert("recoveryOperation".into(), json!("connection.disconnect"));
            return Err(error);
        }
        Ok(connection)
    }
    async fn resolve_connection(
        &self,
        input: ConnectionInput,
    ) -> Result<Arc<Connection>, Diagnostic> {
        self.resolve_connection_for(input, false).await
    }
    /// `allow_discovery`: connection.test may run without a company to list them.
    async fn resolve_connection_for(
        &self,
        input: ConnectionInput,
        allow_discovery: bool,
    ) -> Result<Arc<Connection>, Diagnostic> {
        let profile = if let Some(id) = &input.profile_id {
            let root = self.state_root.clone();
            let id = id.clone();
            Some(blocking(move || Profiles::new(&root).get(&id)).await?)
        } else {
            None
        };
        let file = if let CredentialSource::UserFile {
            path,
            environment_id,
        } = &input.credential_source
        {
            Some(read_environment_file(Path::new(path), environment_id).await?)
        } else {
            None
        };
        let url = input
            .url
            .as_deref()
            .or_else(|| profile.as_ref().map(|p| p.url.as_str()))
            .or_else(|| file.as_ref().map(|f| f.url.as_str()))
            .ok_or_else(|| {
                Diagnostic::new(
                    "ENDPOINT_REQUIRED",
                    "Supply an endpoint or an exact saved environment selection.",
                )
            })?;
        let endpoint = endpoint(url)?;
        if let Some(f) = &file {
            if endpoint != crate::input::endpoint(&f.url)? {
                return Err(Diagnostic::new(
                    "CREDENTIAL_ENDPOINT_MISMATCH",
                    "The user-file credentials belong to a different endpoint. Select a matching source explicitly.",
                ));
            }
        }
        let mode = input
            .auth_mode
            .or_else(|| profile.as_ref().map(|p| p.auth_mode))
            .or_else(|| file.as_ref().map(|f| f.auth_mode))
            .unwrap_or_default();
        let company = input
            .company
            .clone()
            .filter(|c| !c.is_empty())
            .or_else(|| profile.as_ref().and_then(|p| p.defaults.company.clone()).filter(|c| !c.is_empty()))
            .or_else(|| file.as_ref().and_then(|f| f.defaults.company.clone()).filter(|c| !c.is_empty()));
        if company.is_none() && !allow_discovery {
            return Err(crate::connection::company_required());
        }
        // A saved profile's or file's certificate choice never follows an overridden endpoint.
        let same_endpoint = |url: &str| crate::input::endpoint(url).is_ok_and(|saved| saved == endpoint);
        let allow_untrusted_certificate = input.allow_untrusted_certificate
            .or_else(|| profile.as_ref().filter(|p| same_endpoint(&p.url)).map(|p| p.allow_untrusted_certificate))
            .or_else(|| file.as_ref().filter(|f| same_endpoint(&f.url)).map(|f| f.allow_untrusted_certificate))
            .unwrap_or(false);
        let explicit_settings = input.settings.is_some();
        let settings = match input.settings {
            Some(settings) => settings,
            None => {
                let mut settings = self.settings.read().await.clone();
                if let Some(concurrency) = profile
                    .as_ref()
                    .and_then(|p| p.defaults.concurrency)
                    .or_else(|| file.as_ref().and_then(|f| f.defaults.concurrency))
                {
                    settings.concurrency = concurrency;
                }
                settings
            }
        };
        settings.validate()?;
        let credentials = match input.credential_source {
            CredentialSource::Inline { credentials } => credentials,
            CredentialSource::UserFile { .. } => file.unwrap().credentials,
            CredentialSource::SecureStore => {
                let p = profile.as_ref().ok_or_else(|| {
                    Diagnostic::new(
                        "PROFILE_REQUIRED",
                        "A saved profile is required for secure-store selection.",
                    )
                })?;
                if endpoint != crate::input::endpoint(&p.url)? {
                    return Err(Diagnostic::new(
                        "CREDENTIAL_ENDPOINT_MISMATCH",
                        "Saved credentials cannot follow an overridden endpoint.",
                    ));
                }
                let url = endpoint.as_str().to_string();
                let id = p.id.clone();
                blocking(move || profiles::secure_read(&url, &id)).await?
            }
        };
        let connection=Connection::with_options(
            endpoint,
            company,
            mode,
            credentials,
            settings,
            self.budget.clone(),
            crate::connection::ConnectionOptions { allow_untrusted_certificate },
        )?;
        self.budget.configure_operating(connection.settings().concurrency,connection.settings().minimum_request_interval_ms)?;
        if explicit_settings {
            // Honor an explicit connection setting for new runs. Later settings.update
            // can change this value without reconnecting or changing an active run.
            self.settings.write().await.server_concurrency = connection.settings().server_concurrency;
        }
        Ok(connection)
    }
    async fn installed(&self) -> Result<(Vec<Plugin>, Vec<Diagnostic>), Diagnostic> {
        self.installed_for(&self.operation_cancel.child_token())
            .await
    }
    async fn installed_for(
        &self,
        cancel: &CancellationToken,
    ) -> Result<(Vec<Plugin>, Vec<Diagnostic>), Diagnostic> {
        let mut cache = tokio::select! {_ = cancel.cancelled()=>return Err(Diagnostic::new("CANCELED","Collection discovery was canceled.")),cache=self.plugin_cache.lock()=>cache};
        cache.scan(&self.plugin_root, cancel).await
    }
    async fn plugin(&self, category: &str) -> Result<Plugin, Diagnostic> {
        self.installed().await?.0.into_iter().find(|p|p.category.id==category)
            .ok_or_else(||Diagnostic::new("COLLECTION_UNAVAILABLE","Choose a collection declared by a module currently present in the selected folder."))
    }
    async fn capabilities(&self) -> Result<Value, Diagnostic> {
        let (plugins, diagnostics) = self.installed().await?;
        Ok(
            json!({"schemaVersion":1,"categories":plugins.into_iter().map(|p|p.category).collect::<Vec<_>>(),"diagnostics":diagnostics,
            "discovery":"The selected folder is authoritative. Only present module descriptors appear; no expected roster or plugin registry exists.",
            "targets":["2026.100","2025.2","2024.2"],"qualification":"Review each artifact result and diagnostic. Server contracts and available source determine completeness.",
            "temporaryAccess":{"default":default_temporary_access(),"nativeGrant":"Native policy checks determine required groups. Grants use recorded ownership.","nativeCleanup":"Remove only owned additions and verify the resulting memberships."},
            "connection":{"allowUntrustedCertificate":true,"companyDiscovery":true}}),
        )
    }
    async fn drive_inner(
        &self,
        plugin: &Plugin,
        mut request: GuestRequest,
        connection: &Arc<Connection>,
        run: &Arc<Run>,
        holder: &str,
        temporary_access: bool,
        cancel: &CancellationToken,
        mut buffered_reply: Option<GuestReply>,
        output: Option<&ready_output::OutputQueue>,
        issued: Vec<FileRef>,
    ) -> Result<(GuestReply, Vec<FileRef>), Diagnostic> {
        let direct_page = request.operation == "materialize"
            || (request.operation == "discover" && plugin.has_role("pagedDefinitions")
                && !plugin.has_role("prefetchPages"));
        let mut page_batch = if direct_page && output.is_none() {
            let completed = run.storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                .journal.artifacts.iter().filter(|a| a.identity.category == plugin.category.id
                    && a.acquisition == Completeness::Collected).count();
            Some(run.activity.begin_page(&plugin.category.name, &plugin.category.id, completed))
        } else { None };
        let activity = run.activity.begin(
            &plugin.category.name,
            request.identity.clone(),
            if request.operation == "materialize" {
                "Building exports from downloaded data"
            } else if direct_page {
                "Downloading definition page"
            } else if request.operation == "discover" {
                "Finding items"
            } else {
                "Preparing download"
            },
        );
        let collection_method = run.storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal.collection_method;
        let server_concurrency = run.storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal.settings.server_concurrency;
        let overrides = run
            .storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal
            .plugin_settings
            .get(&plugin.category.id)
            .cloned()
            .unwrap_or(Value::Null);
        let plugin_settings = resolve_frozen_plugin_settings(&plugin.category.settings, &overrides)?;
        let retained_transfer = request
            .state
            .get("_hostTransfer")
            .cloned()
            .unwrap_or(Value::Null);
        let transfer_id = retained_transfer
            .get("operationId")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        // A prepared page carries files issued by its fetch operation. This
        // host-owned handoff is separate from the untrusted guest state.
        let mut files = issued;
        let mut ready_identities = Vec::new();
        let mut compiled_page = (request.state.pointer("/_progress/pendingCompiledRead") == Some(&json!(true)))
            .then(|| run.activity.compiled_page());
        loop {
            // No blanket borrowing: a boolean cannot extend every active group.
            // native_read_with_access borrows per dispatched service/method, only
            // through an exact PolicyAuthority for a selected owned membership.
            self.checkpoint_access(run, connection).await?;
            if !request.state.is_object() {
                request.state = json!({});
            }
            request.state["_hostCapabilities"] = json!({});
            if cfg!(feature="function-compiled"){request.state["_hostCapabilities"]["managedDecompilationV1"]=json!({"protocolVersion":1,"engineId":MANAGED_ENGINE_ID,"retainedArtifactFunctions":true});}
            if cfg!(feature="managed-decompilation"){request.state["_hostCapabilities"]["localDecompilationV1"]=json!({"protocolVersion":1,"engineId":MANAGED_ENGINE_ID,"retainedArtifactFunctions":true});}
            request.state["_hostCaller"] = json!(connection.caller().await);
            // Native formats can differ by Epicor version (for example app page path separators).
            request.state["_hostServerVersion"] = json!(connection.server_version().await);
            request.state["_hostRunId"] = json!(holder);
            request.state["_hostSettings"] = json!(connection.settings());
            request.state["_hostSettings"]["serverConcurrency"] = json!(server_concurrency);
            request.state["_serverConcurrency"] = json!(server_concurrency);
            request.state["_collectionMethod"] = json!(collection_method);
            request.state["_hostUtilityOperations"] = host_utility::resume_operations(
                &run.storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?.journal,
                plugin, request.identity.as_ref());
            request.state["_pluginSettings"] = plugin_settings.clone();
            request.state["_hostPreparePage"] = json!(
                request.operation == "discover" && plugin.has_role("prefetchPages"));
            request.state["_hostIndependentWorkItems"] = json!(true);
            request.state["_hostTransferId"] = json!(transfer_id);
            request.state["_hostTransfer"] = retained_transfer.clone();
            while run.paused.load(Ordering::SeqCst) {
                tokio::select! {_ = cancel.cancelled()=>return Err(Diagnostic::new("CANCELED","The run was canceled.")),_ = run.wake.notified()=>{}}
            }
            if direct_page && request.response.is_some() {
                activity.step(crate::activity::response_label(&request.state));
                if let Some(batch) = &page_batch {
                    batch.update(crate::activity::response_label(&request.state), 0);
                }
            }
            let mut reply = match buffered_reply.take() {
                Some(reply) => reply,
                None => {
                    let _work = run.activity.work(&plugin.category.id, "pluginCall");
                    plugin.call(request.clone(), cancel.clone()).await?
                },
            };
            crate::support_packet::annotate(&mut reply,&plugin.category.id,&request.operation,request.identity.as_ref(),request.response.as_ref());
            // Keep ready identities until the final inventory commits the cursor.
            // The output pipeline joins its writers before this reply can escape.
            let mut resume_page = None;
            if let GuestReply::ReadyRecords { records, state } = reply {
                if !matches!(request.operation.as_str(), "materialize" | "acquire") || !plugin.has_role("pagedDefinitions") {
                    return Err(Diagnostic::new("PLUGIN_OUTPUT_INVALID", "Ready records require a created page or selected definition."));
                }
                if request.operation=="acquire" && records.iter().any(|(owner,_)|request.identity.as_ref()!=Some(owner)) {
                    return Err(Diagnostic::new("PLUGIN_IDENTITY_DENIED","Ready records must belong to the selected definition."));
                }
                if compiled_page.is_none() && state.pointer("/_progress/pendingCompiledRead")==Some(&json!(true)){compiled_page=Some(run.activity.compiled_page());}
                let identities = records.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
                ready_identities.extend(identities.clone());
                resume_page = Some(state);
                reply = GuestReply::InventoryData { records, inventory: Box::new(GuestReply::Inventory {
                    identities, next_cursor: None, authoritative: true, diagnostics: vec![],
                }) };
            }
            // Simple paged plugins return their complete records directly. Queue
            // them before writing, just like plugins that return a PreparedPage.
            if request.operation == "discover" && plugin.has_role("prefetchPages")
                && matches!(reply, GuestReply::InventoryData { .. }) {
                return Ok((reply, files));
            }
            if let GuestReply::InventoryData { records, inventory } = reply {
                let GuestReply::Inventory { identities, .. } = inventory.as_ref() else {
                    return Err(Diagnostic::new(
                        "PLUGIN_OUTPUT_INVALID",
                        "Inventory data needs an inventory reply.",
                    ));
                };
                ready_output::validate_inventory_records(&request, &connection.company,
                    &records, identities, output.map(|queue| &queue.owners))?;
                let storage = run.storage.clone();
                let direct = plugin.has_role("pagedDefinitions");
                let batch = page_batch.take();
                let page_category = plugin.category.id.clone();
                if direct && output.is_none() {
                    activity.step("Writing downloaded files");
                    if let Some(batch) = &batch { batch.page_received(records.len()); }
                }
                if let Some(output) = output {
                    output.enqueue(records, run, &plugin.category.id).await?;
                } else if direct {
                    let write_started = std::time::Instant::now();
                    let record_count = records.len();
                    activity.step("Preparing page files in memory");
                    let prepared = self.prepare_reference_page(run, records, cancel).await?;
                    activity.step(&format!("Writing prepared file batch ({} local writers)",
                        crate::file_writers::WRITERS));
                    let writer_storage = storage.clone();
                    let writers = self.file_writers.clone();
                    let writer_cancel = cancel.clone();
                    let batch = blocking(move || {
                        // Local file concurrency is independent of server request slots.
                        // This is the selected operating default.
                        crate::collection_output::write_shared_page(writer_storage, prepared,
                            &writers,
                            &writer_cancel, |count| { if let Some(batch) = &batch { batch.processed(count); } })?;
                        Ok(batch)
                    }).await?;
                    let write_ms = write_started.elapsed().as_millis();
                    let checkpoint_started = std::time::Instant::now();
                    let completed = blocking(move || {
                        let storage = storage.lock().map_err(|_| {
                            Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                        })?;
                        storage.checkpoint()?;
                        Ok(storage.journal.artifacts.iter()
                            .filter(|a| a.identity.category == page_category).count())
                    }).await?;
                    run.activity.page_written(&plugin.category.id, record_count, write_ms,
                        checkpoint_started.elapsed().as_millis());
                    if let Some(batch) = batch { batch.processing_complete(completed); }
                } else {
                    blocking(move || {
                        let mut storage = storage.lock().map_err(|_| {
                            Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                        })?;
                    for (identity, data) in records {
                        let bytes = serde_json::to_vec(&data).map_err(|_| {
                            Diagnostic::new("PLUGIN_OUTPUT_INVALID", "Inventory data is invalid.")
                        })?;
                        let file = storage.preserve_raw(&bytes, "json")?;
                        let key = storage::identity_key(&identity);
                        let owned = storage
                            .journal
                            .artifact_acquisitions
                            .entry(key.clone())
                            .or_default();
                        if !owned.iter().any(|previous| previous == &file) {
                            owned.push(file.clone());
                        }
                        storage.journal.inventory_data.insert(key, file);
                    }
                    storage.checkpoint()
                    }).await?;
                }
                reply = *inventory;
            }
            if let Some(state) = resume_page {
                request.state = state;
                request.response = None;
                request.error = None;
                if output.is_none() {
                let completed = run.storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                    .journal.artifacts.iter().filter(|a| a.identity.category == plugin.category.id).count();
                page_batch = Some(run.activity.begin_page(&plugin.category.name, &plugin.category.id, completed));
                }
                continue;
            }
            if let GuestReply::Inventory { identities, .. } = &mut reply {
                identities.splice(0..0, ready_identities.drain(..));
            }
            match reply {
                GuestReply::Action { action, state } => {
                    // Serialize actions from this plugin across page fetches and
                    // targeted calls. Local page processing does not hold this gate.
                    let local = matches!(&action, HostAction::BuildZip { .. }
                        | HostAction::ReadBinary { .. } | HostAction::ParseXml { .. }
                        | HostAction::ParseXmlArchive { .. } | HostAction::ParseJsonArchive { .. }
                        | HostAction::ParseJson { .. } | HostAction::Assemble { .. }
                        | HostAction::QueueManagedDecompilation { .. } | HostAction::QueueLocalDecompilation { .. });
                    let mut _request = if local { None } else {
                        let gate = self.plugin_request_gates.lock().await
                            .entry(plugin.category.id.clone())
                            .or_insert_with(|| Arc::new(Mutex::new(()))).clone();
                        let _wait = run.activity.work(&plugin.category.id, "requestGateWait");
                        Some(tokio::select! {
                            _ = cancel.cancelled() => return Err(Diagnostic::new("CANCELED", "The plugin request was canceled.")),
                            guard = gate.lock_owned() => guard,
                        })
                    };
                    activity.step(crate::activity::request_label(&action, &state));
                    if let Some(batch) = &page_batch {
                        batch.update(&crate::activity::request_label(&action, &state), 0);
                    }
                    request.state = state;
                    if !cfg!(feature="function-compiled") && matches!(&action,HostAction::QueueManagedDecompilation{..}|HostAction::RetainedArtifactFunctionCall{..}) {request.response=None;request.error=Some(Diagnostic::new("MANAGED_HOST_UNAVAILABLE","This host package does not enable managed decompilation."));continue;}
                    if !cfg!(feature="managed-decompilation")&&matches!(&action,HostAction::QueueLocalDecompilation{..}|HostAction::RetainedLocalArtifactFunctionCall{..}){request.response=None;request.error=Some(Diagnostic::new("MANAGED_HOST_UNAVAILABLE","This host package does not enable local decompilation."));continue;}
                    if let HostAction::QueueManagedDecompilation { request: job } = &action {
                        // ReadyRecords have passed page identity/scope validation. Their
                        // writer joins before the cursor can advance, while local jobs may
                        // enroll immediately against these host-validated page owners.
                        let mut allowed_owner=request.identity.as_ref().or_else(||ready_identities.iter().find(|owner|**owner==job.capture.owner)).cloned();
                        if allowed_owner.is_none() { if let Some(queue)=output { allowed_owner=queue.owners.find(&job.capture.owner)?; } }
                        match self.managed.enqueue(&self.plugin_root, run, &plugin.category.id, allowed_owner.as_ref(), &files, job.clone()).await {
                            Ok(receipt) => { request.response=Some(serde_json::to_value(receipt).map_err(|_|crate::processing::output_error())?); request.error=None; },
                            Err(error) => { request.response=None; request.error=Some(error); }
                        }
                        continue;
                    }
                    if let HostAction::QueueLocalDecompilation { request: job } = &action {
                        let mut allowed_owner=request.identity.as_ref().or_else(||ready_identities.iter().find(|owner|**owner==job.owner)).cloned();
                        if allowed_owner.is_none() { if let Some(queue)=output { allowed_owner=queue.owners.find(&job.owner)?; } }
                        match self.managed.enqueue_local(&self.plugin_root,run,&plugin.category.id,allowed_owner.as_ref(),&files,job.clone()).await {
                            Ok(receipt)=>{request.response=Some(serde_json::to_value(receipt).map_err(|_|crate::processing::output_error())?);request.error=None;},
                            Err(error)=>{request.response=None;request.error=Some(error);}
                        }
                        continue;
                    }
                    if let HostAction::RetainedLocalArtifactFunctionCall { alias,function,arguments } = &action {
                        match Box::pin(managed_decompilation::read_local_artifact(self,plugin,alias,function,arguments,holder,run,connection,cancel,_request.take())).await {
                            Ok((value,acquired))=>{files.extend(acquired);request.response=Some(value);request.error=None;},
                            Err(error)=>{request.response=None;request.error=Some(error);}
                        }
                        continue;
                    }
                    if let HostAction::RetainedArtifactFunctionCall { alias, function, arguments } = &action {
                        let result = async {
                            if !plugin.binary_exports.iter().any(|d|d.service==*alias && d.method==*function) {
                                return Err(Diagnostic::new("MANAGED_BINARY_DECLARATION_REQUIRED","The retained helper read has no declared binary output paths."));
                            }
                            if plugin.category.id!="functions" || alias!="compiled_functions" || function!="ReadCompiled" {return Err(Diagnostic::new("MANAGED_INPUT_OWNERSHIP_DENIED","This retained Epicor route is reserved for the declared Function reader."));}
                            let mut arguments=arguments.clone();
                            {let storage=run.storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run storage is unavailable."))?;
                                arguments["iCaller"]=json!(storage.journal.caller);arguments["iCompany"]=json!(storage.journal.company);
                                arguments["iKnownCaptures"]=json!(serde_json::to_string(&managed_decompilation::known_captures(&storage,&arguments)?).map_err(|_|crate::processing::output_error())?);
                            }
                            let mut native=self.native_artifact_action(plugin,HostAction::ArtifactFunctionCall{alias:alias.clone(),function:function.clone(),arguments:arguments.clone()},holder,run,connection,cancel).await?;
                            run.activity.native_response(&plugin.category.id,None,native.elapsed_ms,native.raw.len(),&native.value);
                            {let storage=run.storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run storage is unavailable."))?;managed_decompilation::validate_reader_scope(&storage,&native.value)?;}
                            let blobs=crate::native_export::declared_blobs(&plugin.binary_exports,alias,function,&native.value)?;
                            for blob in blobs {
                                let file={let mut storage=run.storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run storage is unavailable."))?;storage.preserve_for(None,&blob.bytes,&blob.extension)?};
                                *native.value.pointer_mut(&blob.pointer).ok_or_else(||Diagnostic::new("INVALID_NATIVE_BYTES","A declared Epicor binary pointer disappeared."))?=json!({"nativeFile":file,"representation":"decoded native bytes"});
                                files.push(file);
                            }
                            {let storage=run.storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run storage is unavailable."))?;managed_decompilation::restore_retained_rows(&storage,&mut native.value,&mut files)?;}
                            zeroize::Zeroize::zeroize(&mut native.raw);
                            let metadata=serde_json::to_vec(&native.value).map_err(|_|crate::processing::output_error())?;
                            let file={let mut storage=run.storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run storage is unavailable."))?;let file=storage.preserve_for(None,&metadata,"json")?;managed_decompilation::record_retained_read(&mut storage,&native.value,&arguments,&files)?;file};files.push(file);
                            Ok::<_,Diagnostic>(native.value)
                        }.await;
                        match result { Ok(value)=>{request.response=Some(value);request.error=None;},Err(error)=>{request.response=None;request.error=Some(error);} }
                        continue;
                    }
                    if let HostAction::BuildZip { entries } = &action {
                        let _work = run.activity.work(&plugin.category.id, "buildZip");
                        let storage = run.storage.clone();
                        let entries = entries.clone();
                        let result = blocking(move || {
                            let mut storage = storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
                            crate::native_zip::build(&mut storage, entries)
                        }).await;
                        match result {
                            Ok(file) => { files.push(file.clone()); request.response = Some(json!({"file":file})); request.error = None; }
                            Err(error) => { request.response = None; request.error = Some(error); }
                        }
                        continue;
                    }
                    if let HostAction::HostUtility { operation_id, request: utility } = &action {
                        match self.host_utility(plugin, operation_id, utility, request.identity.as_ref(), run, connection, cancel).await {
                            Ok((value, acquired)) => {
                                files.extend(acquired);
                                request.response = Some(value);
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    if let HostAction::NativeFileExport {
                        identity: export_identity,
                        service,
                        method,
                        arguments,
                        filename_argument,
                        folder,
                        extension,
                        companion_extensions,
                    } = &action
                    {
                        let owner = match native_file_export::resolve_export_owner(request.identity.as_ref(),export_identity.as_ref(),&plugin.category.id,&connection.company) {
                            Ok(owner)=>owner,
                            Err(error)=>{request.response=None;request.error=Some(error);continue;}
                        };
                        match self
                            .export_native_file(
                                plugin,
                                run,
                                connection,
                                Some(&owner),
                                service,
                                method,
                                arguments.clone(),
                                filename_argument,
                                *folder,
                                extension,
                                companion_extensions,
                                cancel,
                            )
                            .await
                        {
                            Ok(exported) => {
                                files.extend(exported.clone());
                                request.response = Some(
                                    json!({"returnObj":{"nativeFile":exported.first(),"nativeFiles":exported,"representation":"native server export bytes"}}),
                                );
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    if let HostAction::ReadBinary { file } = &action {
                        let storage = run.storage.clone();
                        let identity = request.identity.clone();
                        let issued = files.clone();
                        let owned_file = file.clone();
                        let result = blocking(move || {
                            storage
                                .lock()
                                .map_err(|_| {
                                    Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                                })?
                                .read_native_binary(identity.as_ref(), &owned_file, &issued)
                        })
                        .await;
                        match result {
                            Ok(value) => {
                                request.response =
                                    Some(json!({"base64":value,"sourceFile":file}));
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    if let HostAction::ParseXml { file } = &action {
                        let _work = run.activity.work(&plugin.category.id, "parseXml");
                        let storage = run.storage.clone();
                        let identity = request.identity.clone();
                        let issued = files.clone();
                        let owned_file = file.clone();
                        let result = blocking(move || {
                            storage
                                .lock()
                                .map_err(|_| {
                                    Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                                })?
                                .parse_native_xml(identity.as_ref(), &owned_file, &issued)
                        })
                        .await;
                        match result {
                            Ok(value) => {
                                request.response =
                                    Some(json!({"parsedXml":value,"sourceFile":file}));
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    if let HostAction::ParseXmlArchive { file, entries } = &action {
                        let storage = run.storage.clone();
                        let identity = request.identity.clone();
                        let issued = files.clone();
                        let owned_file = file.clone();
                        let entries = entries.clone();
                        let result = blocking(move || {
                            storage
                                .lock()
                                .map_err(|_| {
                                    Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                                })?
                                .parse_native_xml_archive(
                                    identity.as_ref(),
                                    &owned_file,
                                    &issued,
                                    &entries,
                                )
                        })
                        .await;
                        match result {
                            Ok(value) => {
                                request.response =
                                    Some(json!({"parsedXmlArchive":value,"sourceFile":file}));
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    if let HostAction::ParseJsonArchive { file, entries } = &action {
                        let storage = run.storage.clone();
                        let identity = request.identity.clone();
                        let issued = files.clone();
                        let owned_file = file.clone();
                        let entries = entries.clone();
                        let result = blocking(move || {
                            storage
                                .lock()
                                .map_err(|_| {
                                    Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                                })?
                                .parse_native_json_archive(
                                    identity.as_ref(),
                                    &owned_file,
                                    &issued,
                                    &entries,
                                )
                        })
                        .await;
                        match result {
                            Ok(value) => {
                                request.response =
                                    Some(json!({"parsedJsonArchive":value,"sourceFile":file}));
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    if let HostAction::ParseJson { file } = &action {
                        let result = {
                            let storage = run.storage.clone();
                            let identity = request.identity.clone();
                            let issued = files.clone();
                            let file = file.clone();
                            blocking(move || {
                                storage
                                    .lock()
                                    .map_err(|_| {
                                        Diagnostic::new(
                                            "RUN_STATE_FAILED",
                                            "Run state is unavailable.",
                                        )
                                    })?
                                    .parse_native_json(identity.as_ref(), &file, &issued)
                            })
                            .await
                        };
                        match result {
                            Ok(value) => {
                                request.response =
                                    Some(json!({"parsedJson":value,"sourceFile":file}));
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    if let HostAction::Assemble {
                        chunks,
                        bytes,
                        sha256,
                        extension,
                    } = &action
                    {
                        let result = if let Some(identity) = request.identity.clone() {
                            let storage = run.storage.clone();
                            let chunks = chunks.clone();
                            let bytes = *bytes;
                            let sha256 = sha256.clone();
                            let extension = extension.clone();
                            blocking(move || {
                                storage
                                    .lock()
                                    .map_err(|_| {
                                        Diagnostic::new(
                                            "RUN_STATE_FAILED",
                                            "Run state is unavailable.",
                                        )
                                    })?
                                    .assemble(&identity, &chunks, bytes, &sha256, &extension)
                            })
                            .await
                        } else {
                            Err(Diagnostic::new(
                                "RETRIEVAL_REQUIRED",
                                "Payload assembly requires an accepted durable retrieval run and its exact artifact identity.",
                            ))
                        };
                        match result {
                            Ok(file) => {
                                files.push(file.clone());
                                request.response = Some(json!({"assembledFile":file}));
                                request.error = None;
                            }
                            Err(error) => {
                                request.response = None;
                                request.error = Some(error);
                            }
                        }
                        continue;
                    }
                    let shared_native = matches!(&action, HostAction::SharedNativeRead { .. });
                    let shared_query = matches!(&action, HostAction::SharedQuery { .. });
                    let mut query_binary_columns = Vec::new();
                    let native_route = match &action {
                        HostAction::NativeRead {
                            service, method, ..
                        }
                        | HostAction::SharedNativeRead {
                            service, method, ..
                        } => Some((service.clone(), method.clone())),
                        _ => None,
                    };
                    // Helper tables use the same declared byte paths as native
                    // responses. Retain bodies before sending rows to a guest.
                    let binary_route = match &action {
                        HostAction::HelperCall { library, function, .. } => {
                            Some((library.clone(), function.clone()))
                        }
                        _ => native_route.clone(),
                    };
                    let helper_outputs = match &action {
                        HostAction::HelperCall {
                            library, function, ..
                        } => plugin
                            .category
                            .helpers
                            .iter()
                            .find(|h| h.library_id == *library)
                            .and_then(|h| h.functions.iter().find(|f| f.function_id == *function))
                            .map(|f| f.binary_outputs.clone())
                            .unwrap_or_default(),
                        _ => vec![],
                    };
                    let mut cached_response = false;
                    let result = match action {
                        HostAction::HostUtility { .. } | HostAction::QueueManagedDecompilation { .. } | HostAction::RetainedArtifactFunctionCall { .. } | HostAction::QueueLocalDecompilation { .. } | HostAction::RetainedLocalArtifactFunctionCall { .. } => unreachable!(),
                        HostAction::SharedArtifactFunctionCall {
                            alias,
                            function,
                            arguments,
                        } => {
                            let key = serde_json::to_string(&json!([
                                plugin.hash,
                                alias,
                                function,
                                arguments
                            ]))
                            .unwrap();
                            run.shared_reads
                                .read_with_source(key, cancel, || {
                                    self.native_artifact_action(
                                        plugin,
                                        HostAction::ArtifactFunctionCall {
                                            alias,
                                            function,
                                            arguments,
                                        },
                                        holder,
                                        run,
                                        connection,
                                        cancel,
                                    )
                                })
                                .await.map(|(response, reused)| { cached_response = reused; response })
                        }
                        HostAction::ArtifactFunctionStream { alias, function, arguments, operation_id, inputs, batch_size } => {
                            self.stream_artifact_inputs(plugin, holder, run, connection, cancel,
                                alias, function, arguments, operation_id, inputs, batch_size).await
                        }
                        action @ (HostAction::ArtifactInstall { .. }
                        | HostAction::ArtifactFunctionCall { .. }
                        | HostAction::ArtifactQueryExecute { .. }
                        | HostAction::ArtifactQueryUpdate { .. }
                        | HostAction::ArtifactQueryAction { .. }) => {
                            self.native_artifact_action(
                                plugin, action, holder, run, connection, cancel,
                            )
                            .await
                        }
                        HostAction::NativeEntityPage {
                            service,
                            entity,
                            query,
                            page_size,
                            page,
                        } => {
                            if !plugin.entities.contains(&(service.clone(), entity.clone())) {
                                return Err(Diagnostic::new(
                                    "PLUGIN_CAPABILITY_DENIED",
                                    "The plugin does not declare Epicor schema discovery.",
                                ));
                            }
                            // Not cached: the run cache keeps every body until the run
                            // ends, and a paged entity read is consumed once by the
                            // plugin that requested it (there is no shared form).
                            connection
                                .native_entity_page(&service, &entity, &query, page_size, page, cancel)
                                .await
                        }
                        HostAction::SystemInfo => {
                            let started = std::time::Instant::now();
                            let value = crate::system_info::snapshot();
                            let raw = serde_json::to_vec(&value)
                                .map_err(|_| crate::processing::output_error())?;
                            Ok(crate::connection::NativeResponse {
                                value,
                                raw,
                                elapsed_ms: started.elapsed().as_millis(),
                            })
                        }
                        HostAction::HelperCall {
                            library,
                            function,
                            arguments,
                        } => {
                            self.helper_action(
                                plugin,
                                &library,
                                &function,
                                arguments,
                                holder,
                                temporary_access,
                                connection,
                                Some(run),
                                request.identity.as_ref(),
                                cancel,
                            )
                            .await
                        }
                        HostAction::NativeFileExport { .. }
                        | HostAction::Assemble { .. }
                        | HostAction::ReadBinary { .. }
                        | HostAction::BuildZip { .. }
                        | HostAction::ParseJson { .. }
                        | HostAction::ParseXmlArchive { .. }
                        | HostAction::ParseJsonArchive { .. }
                        | HostAction::ParseXml { .. } => Err(Diagnostic::new(
                            "INVALID_ACTION",
                            "Payload assembly was not dispatched.",
                        )),
                        HostAction::NativeRead {
                            service,
                            method,
                            arguments,
                            export,
                        }
                        | HostAction::SharedNativeRead {
                            service,
                            method,
                            arguments,
                            export,
                        } => {
                            if !plugin.methods.contains(&(service.clone(), method.clone())) {
                                return Err(Diagnostic::new(
                                    "PLUGIN_CAPABILITY_DENIED",
                                    "The plugin requested an Epicor method absent from its descriptor.",
                                ));
                            }
                            let read = || {
                                self.native_read_with_access(
                                    connection,
                                    &service,
                                    &method,
                                    arguments.clone(),
                                    export,
                                    Some(run),
                                    request.identity.as_ref(),
                                    holder,
                                    temporary_access,
                                    cancel,
                                )
                            };
                            if shared_native {
                                // The cache belongs to this run and connection. Each plugin
                                // passes its capability check before it can reuse a response.
                                let key = json!([
                                    "native",
                                    connection.company,
                                    service,
                                    method,
                                    arguments,
                                    export
                                ])
                                .to_string();
                                run.shared_reads.read_with_source(key, cancel, read).await.map(|(response, reused)| { cached_response = reused; response })
                            } else {
                                read().await
                            }
                        }
                        HostAction::Query { mut query } | HostAction::SharedQuery { mut query } => {
                            if let Err(error) = self
                                .query_metadata(
                                    &mut query,
                                    connection,
                                    Some(run),
                                    request.identity.as_ref(),
                                    holder,
                                    temporary_access,
                                    cancel,
                                )
                                .await
                            {
                                // Let the plugin skip this failed branch and continue
                                // with independent inventory or download work.
                                request.response = None;
                                request.error = Some(error);
                                continue;
                            }
                            query_binary_columns =
                                crate::native_export::query_binary_columns(&query);
                            match crate::baq::build(&query, &connection.company) {
                                Err(error) => Err(error),
                                Ok(arguments) => {
                                    let read = || async {
                                        let native = self
                                            .native_read_with_access(
                                                connection,
                                                "Ice.BO.DynamicQuerySvc",
                                                "Execute",
                                                arguments.clone(),
                                                false,
                                                Some(run),
                                                request.identity.as_ref(),
                                                holder,
                                                temporary_access,
                                                cancel,
                                            )
                                            .await?;
                                        let result =
                                            native.value.get("returnObj").unwrap_or(&native.value);
                                        if result
                                            .get("Errors")
                                            .and_then(Value::as_array)
                                            .is_some_and(|errors| !errors.is_empty())
                                            || !result.get("Results").is_some_and(Value::is_array)
                                        {
                                            return Err(Diagnostic::new(
                                                "QUERY_REFUSED",
                                                "Epicor returned errors or no result rows for this unsaved query. Check the query, then try again.",
                                            ));
                                        }
                                        Ok(native)
                                    };
                                    let result = if shared_query {
                                        // Generated BAQ and relation IDs change on every build.
                                        // Cache the requested query, not those temporary wire IDs.
                                        let key =
                                            json!(["query", connection.company, query]).to_string();
                                        run.shared_reads.read_with_source(key, cancel, read).await.map(|(response, reused)| { cached_response = reused; response })
                                    } else {
                                        read().await
                                    };
                                    match result {
                                        Err(error) => Err(error),
                                        Ok(mut native) => {
                                            let result = native
                                                .value
                                                .get("returnObj")
                                                .unwrap_or(&native.value);
                                            let errors =
                                                result.get("Errors").and_then(Value::as_array);
                                            if errors.is_some_and(|e| !e.is_empty()) {
                                                Err(Diagnostic::new(
                                                    "QUERY_REFUSED",
                                                    "Epicor returned errors for this unsaved query. Check the query, then try again. The error text is not included in routine diagnostics.",
                                                ))
                                            } else if let Some(rows) =
                                                result.get("Results").and_then(Value::as_array)
                                            {
                                                let next = if query
                                                    .page_size
                                                    .is_some_and(|size| rows.len() == size as usize)
                                                {
                                                    rows.last()
                                                        .and_then(|row| {
                                                            query.order.first().and_then(|sort| {
                                                                row.get(&sort.field)
                                                            })
                                                        })
                                                        .and_then(Value::as_str)
                                                        .map(|last| {
                                                            json!({"after":last}).to_string()
                                                        })
                                                } else {
                                                    None
                                                };
                                                native.value =
                                                    json!({"rows":rows,"nextCursor":next});
                                                Ok(native)
                                            } else {
                                                Err(Diagnostic::new(
                                                    "QUERY_RESULT_UNVERIFIED",
                                                    "Epicor's response to this unsaved query had no Results table.",
                                                ))
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    };
                    match result {
                        Ok(mut native) => {
                            let _work = run.activity.work(&plugin.category.id, "retainResponse");
                            if cached_response {
                                run.activity.cached_response(&plugin.category.id, native_route.as_ref(), native.raw.len());
                            } else {
                                run.activity.native_response(&plugin.category.id, native_route.as_ref(),
                                    native.elapsed_ms, native.raw.len(), &native.value);
                            }
                            let mut retention_omissions = std::collections::BTreeSet::new();
                            if let Some((service, _)) = &native_route {
                                let (secrets_omitted, scope_omitted) = plugin
                                    .response_policies
                                    .iter()
                                    .find(|policy| &policy.service == service)
                                    .map(|policy| {
                                        crate::response_policy::apply(
                                            policy,
                                            &connection.company,
                                            &plugin_settings,
                                            &mut native.value,
                                        )
                                    })
                                    .unwrap_or((false, false));
                                if secrets_omitted || scope_omitted {
                                    mark_sanitized_response(&mut native.value, secrets_omitted, scope_omitted);
                                    zeroize::Zeroize::zeroize(&mut native.raw);
                                    native.raw = serde_json::to_vec(&native.value)
                                        .map_err(|_| crate::processing::output_error())?;
                                }
                            }
                            if let Some((service, method)) = &native_route {
                                if let Some(policy) = plugin
                                    .response_policies
                                    .iter()
                                    .find(|policy| &policy.service == service)
                                {
                                    let projection = (|| -> Result<Option<(Vec<u8>, std::collections::BTreeSet<String>)>, Diagnostic> {
                                        let Some(mut saved) = crate::response_retention::project_with_omissions(policy, method, &native.value)? else { return Ok(None); };
                                        mark_scoped_response(saved.value.as_object_mut().ok_or_else(|| Diagnostic::new("INVALID_RETENTION_RESPONSE", "The filtered Epicor response must be an object. The original response was not saved."))?);
                                        Ok(Some((serde_json::to_vec(&saved.value).map_err(|_| crate::processing::output_error())?, saved.omitted)))
                                    })();
                                    match projection {
                                        Ok(Some((raw, omitted))) => {
                                            retention_omissions = omitted;
                                            zeroize::Zeroize::zeroize(&mut native.raw);
                                            native.raw = raw;
                                        }
                                        Ok(None) => {}
                                        Err(error) => {
                                            // Let the plugin continue independent sources without saving this response.
                                            request.response = None;
                                            request.error = Some(error);
                                            continue;
                                        }
                                    }
                                }
                            }
                            {
                                let storage = run.storage.clone();
                                let raw = native.raw;
                                let owner = request.identity.clone();
                                files.push(
                                    blocking(move || {
                                        storage
                                            .lock()
                                            .map_err(|_| {
                                                Diagnostic::new(
                                                    "RUN_STATE_FAILED",
                                                    "Run state is unavailable.",
                                                )
                                            })?
                                            .preserve_for_paced(owner.as_ref(), &raw, "json")
                                    })
                                    .await?,
                                );
                            }
                            match crate::native_export::query_blobs(
                                &query_binary_columns,
                                &native.value,
                            ) {
                                Ok(blobs) => {
                                    let mut binary_files = Vec::new();
                                    for blob in blobs {
                                        let storage = run.storage.clone();
                                        let owner = request.identity.clone();
                                        let bytes = blob.bytes;
                                        let file = blocking(move || {
                                            storage
                                                .lock()
                                                .map_err(|_| {
                                                    Diagnostic::new(
                                                        "RUN_STATE_FAILED",
                                                        "Run state is unavailable.",
                                                    )
                                                })?
                                                .preserve_for_paced(owner.as_ref(), &bytes, "bin")
                                        })
                                        .await?;
                                        binary_files
                                            .push(json!({"pointer":blob.pointer,"file":file}));
                                        files.push(file);
                                    }
                                    if !query_binary_columns.is_empty() {
                                        native.value["binaryFiles"] = json!(binary_files);
                                    }
                                }
                                Err(error) => {
                                    request.response = None;
                                    request.error = Some(error);
                                    continue;
                                }
                            }
                            if let Some((service, method)) = &binary_route {
                                match crate::native_export::declared_blobs_where(
                                    &plugin.binary_exports,
                                    service,
                                    method,
                                    &native.value,
                                    |pointer| {
                                        crate::response_retention::pointer_retained(
                                            &retention_omissions,
                                            pointer,
                                        )
                                    },
                                ) {
                                    Ok(blobs) => {
                                        for blob in blobs {
                                            {
                                                let storage = run.storage.clone();
                                                let bytes = blob.bytes;
                                                let extension = blob.extension;
                                                let owner = request.identity.clone();
                                                let file = blocking(move || {
                                                    storage
                                                        .lock()
                                                        .map_err(|_| {
                                                            Diagnostic::new(
                                                                "RUN_STATE_FAILED",
                                                                "Run state is unavailable.",
                                                            )
                                                        })?
                                                        .preserve_for_paced(
                                                            owner.as_ref(),
                                                            &bytes,
                                                            &extension,
                                                        )
                                                })
                                                .await?;
                                                if let Some(value) =
                                                    native.value.pointer_mut(&blob.pointer)
                                                {
                                                    *value = json!({"nativeFile":file,"representation":"decoded native bytes"});
                                                }
                                                files.push(file);
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        request.response = None;
                                        request.error = Some(error);
                                        continue;
                                    }
                                }
                            }
                            for output in helper_outputs {
                                let encoded = native.value.get(&output).ok_or_else(|| {
                                    Diagnostic::new(
                                        "HELPER_OUTPUT_INVALID",
                                        "The helper omitted a verified byte output.",
                                    )
                                })?;
                                let bytes = crate::native_export::decode_bytes(encoded)?;
                                {
                                    let storage = run.storage.clone();
                                    let owner = request.identity.clone();
                                    let file = blocking(move || {
                                        storage
                                            .lock()
                                            .map_err(|_| {
                                                Diagnostic::new(
                                                    "RUN_STATE_FAILED",
                                                    "Run state is unavailable.",
                                                )
                                            })?
                                            .preserve_for_paced(owner.as_ref(), &bytes, "bin")
                                    })
                                    .await?;
                                    native.value[&output] = json!({"nativeFile":file});
                                    files.push(file);
                                }
                            }
                            request.response = Some(native.value);
                            request.error = None;
                        }
                        Err(error) => {
                            request.response = None;
                            run.activity.request_failed(&plugin.category.id, native_route.as_ref(), &error.code);
                            request.error = Some(error);
                        }
                    }
                }
                other => {
                    match &other {
                        GuestReply::Inventory { identities, .. } => {
                            for id in identities {
                                if id.category != request.category
                                    || !identity_in_scope(id, &connection.company)
                                {
                                    return Err(Diagnostic::new(
                                        "PLUGIN_IDENTITY_DENIED",
                                        "The plugin inventory returned an identity outside its category or run scope.",
                                    ));
                                }
                            }
                        }
                        GuestReply::Definition { identity, .. } => {
                            if request.identity.as_ref() != Some(identity)
                                || identity.category != request.category
                                || !identity_in_scope(identity, &connection.company)
                            {
                                return Err(Diagnostic::new(
                                    "PLUGIN_IDENTITY_DENIED",
                                    "The plugin definition differs from the exact requested identity or run scope.",
                                ));
                            }
                        }
                        _ => {}
                    }
                    return Ok((other, files));
                }
            }
        }
    }
    async fn helper_action(
        &self,
        plugin: &Plugin,
        library: &str,
        function: &str,
        arguments: Value,
        holder: &str,
        temporary_access: bool,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
        identity: Option<&Identity>,
        cancel: &CancellationToken,
    ) -> Result<crate::connection::NativeResponse, Diagnostic> {
        let claim = plugin
            .category
            .helpers
            .iter()
            .find(|h| {
                h.library_id == library && h.functions.iter().any(|f| f.function_id == function)
            })
            .ok_or_else(|| {
                Diagnostic::new(
                    "PLUGIN_CAPABILITY_DENIED",
                    "The installed module does not declare this exact helper source contract.",
                )
            })?;
        let instance = self
            .ensure_helper_library(
                plugin,
                claim,
                holder,
                connection,
                run,
                temporary_access,
                cancel,
            )
            .await?;
        let verified = self
            .verify_helper_claim(&instance, function, connection, run, identity, cancel, true)
            .await?;
        self.admit_helper_transfer(plugin, &instance, function, &arguments, run, identity)
            .await?;
        let collection_permit = crate::server_collection::acquire(
            &self.access_store.location()?.join("server-collections"), holder, &plugin.category.id, cancel).await?;
        let lost = match run { Some(run) => self.lease_gate(run).await?, None => None };
        let result = server_leases::after_leased_call(&lost, connection
            .helper_call(&verified, arguments.clone(), cancel)
            .await);
        collection_permit.settle(&result)?;
        if let (Some(run), Ok(response)) = (run, &result) {
            self.observe_helper_transfer(&instance, function, &arguments, &response.value, run)
                .await?;
        }
        result
    }
    async fn invoke_helper_claim(
        &self,
        claim: &HelperContract,
        function: &str,
        arguments: Value,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
        identity: Option<&Identity>,
        cancel: &CancellationToken,
    ) -> Result<crate::connection::NativeResponse, Diagnostic> {
        let verified = self
            .verify_helper_claim(claim, function, connection, run, identity, cancel, false)
            .await?;
        if let Some(run) = run {
            // F2/F3: each newly initiated call into an installed private instance
            // re-checks the live grant for its legacyHelper effect. Owned cleanup, under
            // its original reference or of this run's own reference-less records,
            // is not new work and is not checked here.
            if server_change_dispatch::cleanup_reference().is_none() && !server_leases::in_cleanup() {
                let journal = collection::snapshot(run).await?;
                if let Some(lease) = journal.helper_libraries.iter().find(|lease| lease.instance.global_id == claim.global_id) {
                    let category = journal.plugin_hashes.iter().find(|(_, hash)| **hash == lease.plugin_hash).map(|(category, _)| category.clone())
                        .ok_or_else(|| Diagnostic::new("SERVER_CHANGE_AUTHORIZATION_REQUIRED", "The helper's module is not part of this run."))?;
                    self.require_server_change(run, connection, &category, &format!("legacyHelper:{}", lease.template_id), None, &json!({"kind":"legacyHelper"}), None).await?;
                }
            }
        }
        let lost = match run { Some(run) => self.lease_gate(run).await?, None => None };
        server_leases::after_leased_call(&lost, connection.helper_call(&verified, arguments, cancel).await)
    }
    async fn verify_helper_claim(
        &self,
        claim: &HelperContract,
        function: &str,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
        _identity: Option<&Identity>,
        cancel: &CancellationToken,
        reuse_in_run: bool,
    ) -> Result<crate::helpers::VerifiedHelper, Diagnostic> {
        let library = &claim.library_id;
        let read = || {
            connection.native_read("Ice.Lib.EfxLibraryDesignerSvc","ExportLibrary",
            json!({"libraryID":library,"options":{"Mode":0,"Format":1,"Package":"","PackageVersion":"","Publisher":"","InstallAsHidden":false}}),true,cancel)
        };
        let native = if let Some(run) = run.filter(|_| reuse_in_run) {
            let key = json!(["helperDefinition", library, claim.global_id]).to_string();
            run.shared_reads.read(key, cancel, read).await?
        } else {
            // Cleanup and recovery verify the current server object before use.
            read().await?
        };
        if let Some(run) = run {
            let storage = run.storage.clone();
            let raw = native.raw;
            blocking(move || {
                storage
                    .lock()
                    .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                    .preserve_immutable(&raw, "json", "helperVerification")
            })
            .await?;
        }
        let (bytes, package) =
            crate::native_artifact_backend::library_backup(&native.value, library)?;
        if let Some(run) = run {
            let storage = run.storage.clone();
            blocking(move || {
                storage
                    .lock()
                    .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                    .preserve_immutable(&bytes, "efxj", "helperVerification")
            })
            .await?;
        }
        crate::helpers::verify(claim, function, &package, &connection.company)
    }
    async fn query_metadata(
        &self,
        query: &mut QuerySpec,
        connection: &Arc<Connection>,
        run: Option<&Arc<Run>>,
        identity: Option<&Identity>,
        holder: &str,
        temporary_access: bool,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        for table in &mut query.tables {
            // Dictionary metadata is fixed for this run's connection. Reuse it
            // across query pages and artifacts instead of fetching it per row.
            let read = || {
                self.native_read_with_access(
                    connection,
                    "Ice.BO.BAQDesignerSvc",
                    "GetFieldList",
                    json!({"dataTableID":table.table,"schema":table.schema}),
                    false,
                    run,
                    identity,
                    holder,
                    temporary_access,
                    cancel,
                )
            };
            let native = if let Some(run) = run {
                let key = json!(["dictionary", table.schema, table.table]).to_string();
                run.shared_reads.read(key, cancel, read).await?
            } else {
                read().await?
            };
            if let Some(run) = run {
                let storage = run.storage.clone();
                let raw = native.raw;
                let owner = identity.cloned();
                blocking(move || {
                    storage
                        .lock()
                        .map_err(|_| {
                            Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                        })?
                        .preserve_for(owner.as_ref(), &raw, "json")
                })
                .await?;
            }
            let rows = native
                .value
                .pointer("/returnObj/TableFieldList")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    Diagnostic::new(
                        "FIELD_METADATA_REQUIRED",
                        "Epicor's data dictionary returned no field list.",
                    )
                })?;
            let mut metadata = Vec::new();
            for row in rows {
                if row.get("DBSchemaName").and_then(Value::as_str) != Some(table.schema.as_str())
                    || row.get("DBTableName").and_then(Value::as_str) != Some(table.table.as_str())
                {
                    continue;
                }
                let text = |name: &str| {
                    row.get(name)
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                };
                if let (Some(name), Some(data_type), Some(format)) = (
                    text("DBFieldName"),
                    text("DataType"),
                    row.get("FieldFormat").and_then(Value::as_str),
                ) {
                    metadata.push(QueryColumn {
                        name: name.into(),
                        data_type: data_type.into(),
                        format: format.into(),
                    });
                } else {
                    return Err(Diagnostic::new(
                        "FIELD_METADATA_REQUIRED",
                        "A field in Epicor's data dictionary has no name or type.",
                    ));
                }
            }
            if metadata.is_empty() {
                return Err(Diagnostic::new(
                    "FIELD_METADATA_REQUIRED",
                    "Epicor's data dictionary has no fields for the requested table. Check the table name, then try again.",
                ));
            }
            table.metadata = metadata;
        }
        Ok(())
    }
    async fn run(&self, id: &str) -> Result<Arc<Run>, Diagnostic> {
        self.runs.lock().await.get(id).cloned().ok_or_else(||Diagnostic::new("RUN_NOT_FOUND","This host does not own that run. Durable restart attachment is still under implementation."))
    }
    /// Release a run that has stopped executing. Its retained responses and plugin references
    /// go at once. When `released` (the destination was released after the final checkpoint,
    /// so the saved journal is current), the run also leaves `runs`: run.status then reads the
    /// saved journal plus the few live-only fields kept here, and the storage, activity and
    /// diagnostic log are dropped with the last reference. Recovery always opens a new run
    /// from the saved journal, so nothing released here is needed to resume. When the release
    /// is uncertain the run stays in `runs` and keeps serving its in-memory journal.
    async fn settle_run(&self, run: &Arc<Run>, released: bool) {
        if run.active.load(Ordering::SeqCst) { return; }
        run.shared_reads.clear().await;
        *run.collection_context.lock().await = None;
        if !released { return; }
        let Ok(id) = run.storage.lock().map(|storage| storage.journal.run_id.clone()) else { return; };
        let fields = json!({
            "activeOperations": [],
            "pluginStates": run.activity.plugin_states(),
            "activityStages": run.activity.history(),
            "transferBatches": run.activity.batches(),
            "diagnosticLog": run.activity.diagnostics_status(),
            "statistics": run.activity.statistics(),
            "pauseRequested": false,
            "cancelRequested": false,
        });
        let mut runs = self.runs.lock().await;
        // A recovery started since this run stopped owns the entry now; leave it alone.
        if runs.get(&id).is_some_and(|current| Arc::ptr_eq(current, run)) {
            self.settled_runs.lock().await.insert(id.clone(), fields);
            runs.remove(&id);
            drop(runs);
            // Teardown removes these on its normal path; an early return there would otherwise
            // keep the operation (its artifact snapshots and operation lock) until the host
            // exits. Recovery reopens operations from disk.
            self.native_operations.lock().await.retain(|(holder, _), _| holder != &id);
            self.unload_idle_plugin_modules().await;
        }
    }
    /// Release cached plugin code once no run is executing. Measured 2026-10-28: memory held
    /// between runs on one connection (about +175 MB per run) was released only by
    /// disconnect, whose one plugin-side step is this unload. It is safe at any time: a run,
    /// discovery or MCP call that is using a plugin holds its own clone of that plugin's
    /// runtime, which this leaves untouched, and the cache reloads lazily on next use (cheap
    /// from a shipped .cwasm). The cache lock waits for any scan in progress.
    async fn unload_idle_plugin_modules(&self) {
        if self.runs.lock().await.values().any(|run| run.active.load(Ordering::SeqCst)) {
            return;
        }
        self.plugin_cache.lock().await.unload_modules();
    }
    async fn recover(self: &Arc<Self>, id: &str, operation: &str) -> Result<Value, Diagnostic> {
        if self
            .runs
            .lock()
            .await
            .get(id)
            .is_some_and(|run| run.active.load(Ordering::SeqCst))
        {
            return Err(Diagnostic::new(
                "RUN_ACTIVE",
                "The current host is still executing this run. Pause or cancel it before recovery.",
            ));
        }
        // A recovered execution re-evaluates every borrow preview.
        self.forget_borrow_previews(id);
        let reprocess = operation == "run.reprocess";
        let connection = if reprocess {
            None
        } else {
            Some(self.current_connection().await?)
        };
        let root = self.state_root.clone();
        let saved_id = id.to_string();
        let mut record =
            blocking(move || crate::recovery::RunIndex::open(&root)?.get(&saved_id)).await?;
        if let Some(connection) = &connection {
            if record.journal.endpoint != connection.endpoint.as_str()
                || record.journal.company != connection.company
                || Some(record.journal.caller.as_str()) != connection.caller().await.as_deref()
            {
                return Err(Diagnostic::new(
                    "RUN_SCOPE_MISMATCH",
                    "To recover this download, reconnect as the Epicor user who started it, to the same server and company.",
                ));
            }
        }
        let loaded = record.clone();
        let mut journal = blocking(move || match crate::recovery::read_journal(&loaded) {
            Ok(journal) => Ok(journal),
            Err(error) if !loaded.accepted && error.code == "JOURNAL_UNAVAILABLE" => {
                Ok(loaded.journal)
            }
            Err(error) => Err(error),
        })
        .await?;
        if !cfg!(feature="managed-decompilation") && journal.managed_processing.is_some(){return Err(Diagnostic::new("MANAGED_HOST_UNAVAILABLE","This run contains managed processing state. Open it with a Herbert build that includes managed decompilation; all saved state and output were preserved."));}
        if !reprocess
            && journal.access_leases.iter().any(|lease| {
                !matches!(
                    lease.state.as_str(),
                    "released" | "preexisting" | "notGranted"
                )
            })
        {
            return Err(Diagnostic::new(
                "ACCESS_RECONCILIATION_REQUIRED",
                "Reconcile the saved temporary-membership intents and cleanup before resuming network collection.",
            ));
        }
        if operation == "run.retryFailed" {
            let failed: Vec<_> = journal
                .artifacts
                .iter()
                .filter(|a| {
                    matches!(
                        a.acquisition,
                        Completeness::Partial
                            | Completeness::Inaccessible
                            | Completeness::DiscoveryFailed
                            | Completeness::Unsupported
                    ) || a.processing == "failed" || cfg!(feature="managed-decompilation") && managed_decompilation::native_retry_required(a)
                })
                .map(|a| a.identity.clone())
                .collect();
            if record.full {
                journal.artifacts.retain(|a| !failed.contains(&a.identity));
                restart_incomplete_discovery(&mut journal);
            } else {
                journal.targets = failed.clone();
                record.journal.targets = failed;
            }
        }
        journal.phase = if reprocess {
            "reprocessing"
        } else {
            "recovering"
        }
        .into();
        let destination = PathBuf::from(&record.destination);
        let operation_retry_managed=operation=="run.retryFailed";
        let store = blocking(move || {
            let mut storage = Storage::open(&destination, false, journal)?;
            if !reprocess {
                storage.reconcile_downloads()?;
            }
            storage.reconcile_pending()?;
            if cfg!(feature="managed-decompilation") && (reprocess || operation_retry_managed) {managed_decompilation::retry_failed(&mut storage)?;}
            Ok(storage)
        })
        .await?;
        let activity = Arc::new(crate::activity::Activity::for_run(&store)?);
        record.accepted = true;
        record.journal = store.journal.clone();
        let saved = record.clone();
        let state = self.state_root.clone();
        blocking(move || crate::recovery::RunIndex::open(&state)?.save(&saved)).await?;
        let run = Arc::new(Run {
            storage: Arc::new(std::sync::Mutex::new(store)),
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
            wake: Notify::new(),
            active: AtomicBool::new(true),
            network: !reprocess,
            helper_restore_attempts: Mutex::new(std::collections::BTreeSet::new()),
            activity,
            shared_reads: Default::default(),
            helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
        });
        self.runs.lock().await.insert(id.into(), run.clone());
        let host = self.clone();
        let output = record.destination.clone();
        tokio::spawn(async move {
            if reprocess {
                host.reprocess(run).await;
            } else {
                host.collect(
                    run,
                    connection.unwrap(),
                    record.journal.categories,
                    record.journal.targets,
                    record.full,
                    record.page_size,
                )
                .await;
            }
        });
        Ok(json!({"runId":id,"accepted":true,"operation":operation,"destination":output}))
    }
    async fn saved_journal(&self, id: &str) -> Result<storage::RunJournal, Diagnostic> {
        if let Some(run) = self.runs.lock().await.get(id).cloned() {
            return Ok(run
                .storage
                .lock()
                .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                .journal
                .clone());
        }
        let root = self.state_root.clone();
        let id = id.to_string();
        blocking(move || {
            let record = crate::recovery::RunIndex::open(&root)?.get(&id)?;
            crate::recovery::read_journal(&record)
        })
        .await
    }
    async fn start(self: &Arc<Self>, input: StartInput, full: bool) -> Result<Value, Diagnostic> {
        crate::mutation_preferences::initialize(&self.state_root)?;
        if input.request_id.is_empty() {
            return Err(Diagnostic::new(
                "REQUEST_ID_REQUIRED",
                "Supply a stable requestId to reconcile an uncertain start response.",
            ));
        }
        if !full && input.identities.is_empty() {
            return Err(Diagnostic::new(
                "IDENTITY_REQUIRED",
                "To download selected items, give at least one item's exact Epicor ID.",
            ));
        }
        let connection = self.current_connection().await?;
        let page_size = input.page_size.or(connection.settings().page_size);
        let (installed, discovery_diagnostics) = self.installed().await?;
        let available = installed
            .iter()
            .filter(|p| p.category.available)
            .map(|p| p.category.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let mut categories = if full {
            select_full_categories(&available,input.categories.clone(),&discovery_diagnostics)?
        } else {
            let requested = input
                .identities
                .iter()
                .map(|id| id.category.clone())
                .collect::<std::collections::BTreeSet<_>>();
            if requested.iter().any(|id| !available.contains(id)) {
                return Err(Diagnostic::new(
                    "COLLECTION_UNAVAILABLE",
                    "A targeted identity is not handled by an available installed module.",
                ));
            }
            requested.into_iter().collect()
        };
        // Run journals and the run monitor list categories in the established catalog order.
        categories.sort_by_key(|id| crate::legacy_category_ids::catalog_order(id));
        categories.dedup();
        let mut plugin_settings = BTreeMap::new();
        let mut plugin_selection_contracts = BTreeMap::new();
        for plugin in installed
            .iter()
            .filter(|plugin| categories.contains(&plugin.category.id))
        {
            let filters = selection_contract_metadata(&plugin.category.settings);
            if !filters.is_empty() {
                plugin_selection_contracts.insert(plugin.category.id.clone(), json!(filters));
            }
            plugin_settings.insert(
                plugin.category.id.clone(),
                resolve_plugin_settings(
                    &plugin.category.settings,
                    input
                        .plugin_settings
                        .get(&plugin.category.id)
                        .unwrap_or(&Value::Null),
                )?,
            );
        }
        // Saved UI preference is not API authority. A workflow sends its explicit
        // configuration; omitted fields always retain the non-helper default.
        let collection_method = input.collection_method.unwrap_or(CollectionMethod::DirectOnly);
        let mut scope = json!({
            "collectionMethod": collection_method,
            "destination": input.destination,
            "mode": input.output_mode,
            "categories": categories,
            "identities": input.identities,
            "endpoint": connection.endpoint.as_str(),
            "company": connection.company,
            "caller": connection.caller().await,
            "full": full,
            "temporaryAccess": input.temporary_access,
            "contextRunId": input.context_run_id,
            "pluginSettings": plugin_settings
        });
        if !plugin_selection_contracts.is_empty() {
            scope["pluginSelectionContracts"] = json!(plugin_selection_contracts);
        }
        let scope_hash = storage::sha256(&serde_json::to_vec(&scope).unwrap());
        let mut requests = self.requests.lock().await;
        if let Some((id, previous)) = requests.get(&input.request_id) {
            if previous != &scope_hash {
                return Err(Diagnostic::new(
                    "REQUEST_ID_REUSED",
                    "This requestId already belongs to a different run scope.",
                ));
            }
            return Ok(json!({"runId":id,"accepted":true,"reconciled":true}));
        }
        // A repeated accepted request is a read-only reconciliation, even if
        // its grant has since expired/revoked. It must not become a new run.
        let state=self.state_root.clone();let request_id=input.request_id.clone();
        if let Some(previous)=blocking(move||crate::recovery::RunIndex::open(&state)?.request(&request_id)).await? {
            if previous.scope_hash!=scope_hash {return Err(Diagnostic::new("REQUEST_ID_REUSED","The durable request belongs to another run scope."));}
            return Ok(json!({"runId":previous.journal.run_id,"accepted":previous.accepted,"destination":previous.destination,"reconciled":true,"execution":"Inspect status; recover the original owned run after an interrupted host."}));
        }
        let mut material_input=input.clone();
        material_input.collection_method=Some(collection_method);
        material_input.plugin_settings=plugin_settings.clone();
        let server_change_authority=self.admit_server_changes(&material_input,full).await?;
        if collection_method == herbert_contract::CollectionMethod::ServerAssisted {
            crate::server_collection::check_unresolved(
                &self.access_store.location()?.join("server-collections"),
            )?;
        }
        let mut journal = storage::new_journal(input.request_id.clone());
        journal.server_version = connection.server_version().await;
        journal.auth_mode = Some(connection.mode);
        journal.transport_trust = Some(connection.transport_trust());
        journal.endpoint = connection.endpoint.to_string();
        journal.company = connection.company.clone();
        journal.caller = connection.caller().await.unwrap_or_default();
        journal.scope = if full { "fullBackup" } else { "targeted" }.into();
        journal.categories = categories.clone();
        journal.category_names = installed
            .iter()
            .filter(|p| categories.contains(&p.category.id))
            .map(|p| (p.category.id.clone(), p.category.name.clone()))
            .collect();
        journal.targets = input.identities.clone();
        journal.plugin_hashes=installed.iter().filter(|plugin|categories.contains(&plugin.category.id)).map(|plugin|(plugin.category.id.clone(),plugin.hash.clone())).collect();
        journal.plugin_settings = plugin_settings;
        journal.plugin_selection_contracts = plugin_selection_contracts;
        if let Some(context) = &input.context_run_id {
            let saved = self.saved_journal(context).await?;
            if saved.endpoint != connection.endpoint.as_str()
                || saved.company != connection.company
                || Some(saved.caller.as_str()) != connection.caller().await.as_deref()
            {
                return Err(Diagnostic::new(
                    "RUN_SCOPE_MISMATCH",
                    "This reference data belongs to a different server, company or Epicor user than the current connection.",
                ));
            }
            journal.references = saved.references;
        }
        journal.server_change_authority=server_change_authority;
        journal.temporary_access_enabled = input.temporary_access;
        journal.collection_method = collection_method;
        journal.settings = connection.settings().clone();
        // The request budget can change after connection. Freeze the selected
        // plugin count for this run, rather than the connection's earlier value.
        journal.settings.concurrency = self.budget.ceiling();
        // Server parallelism belongs to this run, not the connection's lifetime.
        journal.settings.server_concurrency = self.settings.read().await.server_concurrency;
        journal.settings.local_decompiler_workers = self.settings.read().await.local_decompiler_workers;
        let run_id = journal.run_id.clone();
        let destination = PathBuf::from(&input.destination);
        let separate = input.output_mode == OutputMode::Separate;
        if separate {
            let timestamp = httpdate::fmt_http_date(std::time::SystemTime::now()).replace(':', "-");
            let timestamp = timestamp
                .split_once(", ")
                .map(|(_, tail)| tail)
                .unwrap_or(&timestamp);
            let label = format!(
                "Herbert - {} - {}",
                crate::output_layout::component(&journal.company),
                timestamp
            );
            let mut folder = label.clone();
            let mut duplicate = 2_u64;
            while destination.join(&folder).exists() {
                folder = format!("{label} ({duplicate})");
                duplicate += 1;
            }
            journal.separate_folder = Some(folder);
        }
        let expected = if separate {
            destination.join(storage::separate_folder(&journal))
        } else {
            destination.clone()
        };
        let mut record = crate::recovery::RunRecord {
            schema_version: 1,
            scope_hash: scope_hash.clone(),
            destination: std::path::absolute(expected)
                .map_err(|_| {
                    Diagnostic::new(
                        "INVALID_DESTINATION",
                        "Cannot resolve the chosen destination.",
                    )
                })?
                .to_string_lossy()
                .into_owned(),
            accepted: false,
            full,
            page_size,
            journal: journal.clone(),
        };
        let state = self.state_root.clone();
        let intent = record.clone();
        let previous = blocking(move || {
            let run_index = crate::recovery::RunIndex::open(&state)?;
            if let Some(previous) = run_index.request(&intent.journal.request_id)? {
                return Ok(Some(previous));
            }
            run_index.save(&intent)?;
            Ok(None)
        })
        .await?;
        if let Some(previous) = previous {
            if previous.scope_hash != scope_hash {
                return Err(Diagnostic::new(
                    "REQUEST_ID_REUSED",
                    "The durable request belongs to another run scope.",
                ));
            }
            return Ok(
                json!({"runId":previous.journal.run_id,"accepted":previous.accepted,"destination":previous.destination,"reconciled":true,"execution":"Inspect status; use run.recover to obtain an exclusive execution lease after an interrupted host."}),
            );
        }
        let store = blocking(move || Storage::open(&destination, separate, journal)).await?;
        let activity = Arc::new(crate::activity::Activity::for_run(&store)?);
        let output = store.root().to_string_lossy().to_string();
        record.accepted = true;
        record.destination = output.clone();
        record.journal = store.journal.clone();
        let state = self.state_root.clone();
        blocking(move || crate::recovery::RunIndex::open(&state)?.save(&record)).await?;
        let run = Arc::new(Run {
            storage: Arc::new(std::sync::Mutex::new(store)),
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
            wake: Notify::new(),
            active: AtomicBool::new(true),
            network: true,
            helper_restore_attempts: Mutex::new(std::collections::BTreeSet::new()),
            activity,
            shared_reads: Default::default(),
            helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
        });
        self.runs.lock().await.insert(run_id.clone(), run.clone());
        requests.insert(input.request_id, (run_id.clone(), scope_hash));
        drop(requests);
        let host = self.clone();
        tokio::spawn(async move {
            host.collect(
                run,
                connection,
                categories,
                input.identities,
                full,
                page_size,
            )
            .await;
        });
        Ok(
            json!({"runId":run_id,"accepted":true,"destination":output,"scope":if full{"fullBackup"}else{"targeted"}}),
        )
    }
}
fn retain_uncertain_connection(
    diagnostic:&mut Diagnostic,
    active:&mut Option<Arc<Connection>>,
    connection:Arc<Connection>,
) {
    diagnostic.context.insert("connectionRetained".into(),json!(true));
    diagnostic.context.insert("recoveryOperation".into(),json!("connection.disconnect"));
    *active=Some(connection);
}
/// Frozen selection must never borrow a filter from today's portable options.
/// Older journals can omit settings. Without an original filter value the host
/// cannot infer the plugin's matching semantics; refuse instead of changing scope.
pub(crate) fn resolve_frozen_plugin_settings(
    declarations: &[PluginSetting],
    frozen: &Value,
) -> Result<Value, Diagnostic> {
    if declarations.iter().any(|setting| {
        matches!(setting, PluginSetting::Filter { .. })
            && !setting.demo_only()
            && frozen.get(setting.key()).is_none()
    }) {
        return Err(Diagnostic::new(
            "FROZEN_SELECTION_REQUIRED",
            "This saved run has no frozen value for a declared filter. Restore its original compatible module or start a new run with an explicit selection. Saved output is preserved.",
        ));
    }
    resolve_plugin_settings(declarations, frozen)
}

/// One reviewed classification serves authorization material facts and Advanced UI.
pub(crate) fn is_tuning_setting(category:&str,setting:&PluginSetting)->bool {server_change_operations::tuning(category,setting)}

fn selection_contract_metadata(declarations: &[PluginSetting]) -> Vec<Value> {
    declarations.iter()
        .filter(|setting| matches!(setting, PluginSetting::Filter { .. }) && !setting.demo_only())
        .map(|setting| {
            let mut metadata = json!(setting);
            // Descriptor defaults are portable saved preferences, not matching
            // semantics. Effective values already participate in request identity.
            metadata.as_object_mut().unwrap().remove("default");
            metadata
        })
        .collect()
}

#[cfg(test)]
mod output_metadata_key_tests {
    use super::*;

    #[test]
    fn new_backups_write_only_product_named_metadata_keys() {
        let mut sanitized = json!({"returnObj":{}});
        mark_sanitized_response(&mut sanitized, true, false);
        assert_eq!(sanitized[ACQUISITION_METADATA_KEY]["secretValuesOmitted"], true);
        let mut scoped = serde_json::Map::new();
        mark_scoped_response(&mut scoped);
        assert_eq!(scoped[RETENTION_METADATA_KEY]["outOfScopeRowsOmitted"], true);
        let written = format!("{sanitized}{}", Value::Object(scoped)).to_ascii_lowercase();
        assert!(written.contains("_herbertacquisition") && written.contains("_herbertretention"));
        assert!(!written.contains("hoover"), "{written}");
    }

    #[test]
    fn existing_0_1_backup_responses_with_old_metadata_keys_still_read() {
        // A 0.1.x saved response carries the old key spelling (the one remaining
        // legacy literal, kept only as this compatibility fixture).
        let saved = br#"{"returnObj":{"Rows":[]},"_hooverAcquisition":{"secretValuesOmitted":true,"outOfScopeRowsOrHistoryOmitted":false,"representation":"sanitized native definition response"}}"#;
        let dir = tempfile::tempdir().unwrap();
        let mut storage = crate::storage::Storage::open(dir.path(), false, crate::storage::new_journal("old-keys".into())).unwrap();
        let file = storage.preserve_raw(saved, "json").unwrap();
        storage.verify(&file).unwrap();
        let read = storage.parse_native_json(None, &file, std::slice::from_ref(&file)).unwrap();
        assert_eq!(serde_json::to_vec(&read).unwrap(), serde_json::to_vec(&serde_json::from_slice::<Value>(saved).unwrap()).unwrap());
        assert_eq!(std::fs::read(dir.path().join(&file.path)).unwrap(), saved);
    }
}

#[cfg(test)]
mod frozen_selection_tests {
    use super::*;

    #[test]
    fn explicit_selection_identity_ignores_changed_saved_defaults() {
        let declarations = |saved: &str| vec![PluginSetting::filter("prefix", "ID starts with", saved)
            .description("Literal matching; contract fixture-v1")];
        let scope = |saved: &str, selected: &str| {
            let settings = declarations(saved);
            json!({"pluginSettings":resolve_plugin_settings(&settings, &json!({"prefix":selected})).unwrap(),
                "pluginSelectionContracts":selection_contract_metadata(&settings)})
        };
        assert_eq!(scope("old saved", "explicit"), scope("new saved", "explicit"));
        assert_ne!(scope("old saved", "explicit"), scope("old saved", "changed selection"));
        assert_eq!(selection_contract_metadata(&declarations("saved"))[0]["description"],
            "Literal matching; contract fixture-v1");
        assert!(selection_contract_metadata(&declarations("saved"))[0].get("default").is_none());
    }

    #[test]
    fn frozen_selection_never_inherits_current_saved_filter() {
        let declarations = vec![PluginSetting::filter("prefix", "ID starts with", "new saved value")];
        for value in ["", "original ", "λ'*%"] {
            let frozen = json!({"prefix":value});
            assert_eq!(resolve_frozen_plugin_settings(&declarations, &frozen).unwrap(), frozen);
        }
        for legacy in [Value::Null, json!({})] {
            assert_eq!(resolve_frozen_plugin_settings(&declarations, &legacy).unwrap_err().code,
                "FROZEN_SELECTION_REQUIRED");
        }
        assert_eq!(resolve_frozen_plugin_settings(&declarations, &json!({"prefix":false})).unwrap_err().code,
            "INVALID_PLUGIN_SETTINGS");
        assert_eq!(resolve_frozen_plugin_settings(&declarations, &json!({"prefix":"", "unknown":1})).unwrap_err().code,
            "INVALID_PLUGIN_SETTINGS");
    }

    #[test]
    fn legacy_nonselection_defaults_remain_compatible() {
        let declarations = vec![PluginSetting::checkbox("processing", "Processing", false)];
        assert_eq!(resolve_frozen_plugin_settings(&declarations, &Value::Null).unwrap(),
            json!({"processing":false}));
    }
}

fn select_full_categories(
    available:&std::collections::BTreeSet<String>,
    requested:Option<Vec<String>>,
    diagnostics:&[Diagnostic],
) -> Result<Vec<String>,Diagnostic> {
    let catalog_ordered=|ids:Vec<String>|{let mut ids=ids;ids.sort_by_key(|id|crate::legacy_category_ids::catalog_order(id));ids};
    let Some(requested)=requested else { return Ok(catalog_ordered(available.iter().cloned().collect())); };
    if requested.is_empty() {
        return Err(Diagnostic::new("CATEGORY_SELECTION_EMPTY","Select at least one category for an explicit full backup."));
    }
    let requested=requested.into_iter().collect::<std::collections::BTreeSet<_>>();
    let unavailable=requested.iter().filter(|id|!available.contains(*id)).cloned().collect::<Vec<_>>();
    if !unavailable.is_empty() {
        let mut error=Diagnostic::new("COLLECTION_UNAVAILABLE","One or more requested categories are not available.");
        error.context.insert("requestedCategories".into(),json!(catalog_ordered(requested.into_iter().collect())));
        error.context.insert("unavailableCategories".into(),json!(unavailable));
        error.context.insert("discoveryDiagnostics".into(),json!(diagnostics));
        return Err(error);
    }
    Ok(catalog_ordered(requested.into_iter().collect()))
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Diagnostic> + Send + 'static,
) -> Result<T, Diagnostic> {
    tokio::task::spawn_blocking(f).await.map_err(|_| {
        Diagnostic::new(
            "LOCAL_TASK_FAILED",
            "A local processing step failed. Existing output was kept, and the download's saved progress shows where it stopped.",
        )
    })?
}
pub(crate) fn identity_in_scope(identity: &Identity, company: &str) -> bool {
    if identity.category.is_empty()
        || identity.kind.is_empty()
        || identity.keys.keys().any(|key| key.is_empty())
    {
        return false;
    }
    match &identity.scope {
        Scope::Global => identity
            .keys
            .get("Company")
            .is_none_or(|value| value.is_empty()),
        Scope::Company { company: owner } => {
            owner == company
                && identity
                    .keys
                    .get("Company")
                    .is_none_or(|value| value == company)
        }
        Scope::SharedCompany { company: selected, owner_company } => {
            selected == company
                && !owner_company.is_empty()
                && owner_company != selected
                && identity.keys.get("Company") == Some(owner_company)
        }
        Scope::Personal {
            company: owner,
            owner: user,
        } => {
            !user.is_empty()
                && (owner.is_empty() || owner == company)
                && identity
                    .keys
                    .get("Company")
                    .is_none_or(|value| value == owner)
        }
    }
}

#[cfg(test)]
mod shared_company_scope_tests {
    use super::*;
    #[test]
    fn shared_identity_keeps_owner_and_is_bound_to_the_selected_company() {
        let mut id=Identity{category:"menus-and-launch-definitions".into(),kind:"PersistedMenu".into(),
            scope:Scope::SharedCompany{company:"C".into(),owner_company:"Peer".into()},
            keys:std::collections::BTreeMap::from([("Company".into(),"Peer".into()),("MenuID".into(),"M".into())])};
        assert!(identity_in_scope(&id,"C"));
        assert!(!identity_in_scope(&id,"Other"));
        id.keys.insert("Company".into(),"DifferentOwner".into());
        assert!(!identity_in_scope(&id,"C"));
        id.scope=Scope::Company{company:"Peer".into()};
        assert!(!identity_in_scope(&id,"C"));
    }
}
async fn checkpoint(run: &Arc<Run>, phase: &str) -> Result<(), Diagnostic> {
    let storage = run.storage.clone();
    let phase = phase.to_string();
    blocking(move || {
        let mut s = storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
        s.journal.phase = phase;
        s.checkpoint_all()
    })
    .await
}
// An explicit retry starts discovery from the beginning. Keep collected identities,
// but do not carry an earlier incomplete verdict into the new attempt.
fn restart_incomplete_discovery(journal: &mut storage::RunJournal) {
    let categories: Vec<_> = journal
        .category_outcomes
        .iter()
        .filter(|(_, outcome)| {
            matches!(outcome.as_str(), "discoveryFailed" | "discoveryIncomplete")
        })
        .map(|(category, _)| category.clone())
        .collect();
    for category in categories {
        journal.discovery_finished.remove(&category);
        journal.cursors.remove(&category);
        journal.category_outcomes.remove(&category);
    }
}


// Add next to record_diagnostic; called only for final Inventory page diagnostics.
async fn record_page_diagnostic(run:&Arc<Run>,category:&str,owners:&[Identity],diagnostic:Diagnostic)->Result<(),Diagnostic>{
 let Some(owner)=crate::processing_status::page_owner(category,owners,&diagnostic)? else{return record_diagnostic(run,diagnostic).await;};
 let storage=run.storage.clone();
 blocking(move||{
  let mut s=storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run state is unavailable."))?;
  let mut artifact=s.manifest.artifacts.get(&storage::identity_key(&owner)).cloned().ok_or_else(||Diagnostic::new("PLUGIN_OUTPUT_INVALID","An incomplete processing result has no saved artifact for this page owner."))?;
  if !artifact.diagnostics.contains(&diagnostic){artifact.diagnostics.push(diagnostic.clone());}
  crate::processing_status::reconcile(&mut artifact);
  if !s.journal.diagnostics.contains(&diagnostic){s.journal.diagnostics.push(diagnostic);}
  s.record_artifact(artifact)
 }).await
}

async fn record_diagnostic(run: &Arc<Run>, diagnostic: Diagnostic) -> Result<(), Diagnostic> {
    // Canceling several plugins is one user action, not several failures.
    // Keep other diagnostics, including uncertain server cleanup.
    if run.cancel.is_cancelled() && diagnostic.code == "CANCELED" {
        return Ok(());
    }
    let storage = run.storage.clone();
    blocking(move || {
        let mut s = storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
        if s.journal.diagnostics.contains(&diagnostic) {
            return Ok(());
        }
        s.journal.diagnostics.push(diagnostic);
        s.checkpoint()
    })
    .await
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdInput {
    id: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct StatusInput {
    id: String,
    #[serde(default)]
    summary: bool,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenameInput {
    id: String,
    name: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SaveCredentialInput {
    id: String,
    credentials: Credentials,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssociateFileInput {
    id: String,
    path: String,
    environment_id: String,
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoArguments {}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginInput {
    category: String,
    selected: Option<bool>,
    #[serde(default)]
    values: Value,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
enum OutputMode {
    Refresh,
    Separate,
}
// Shared by fresh GUI preferences and omitted CLI/MCP download options.
// A saved workflow supplies explicit run configuration, never omission escalation.
pub(crate) fn default_temporary_access() -> bool {
    false
}

#[cfg(test)]
mod temporary_access_default_tests {
    use super::*;

    #[test]
    fn omitted_download_option_disables_access_and_preserves_explicit_choice() {
        let mut value =
            json!({"requestId":"fixture", "destination":"fixture", "outputMode":"separate"});
        let omitted: StartInput = serde_json::from_value(value.clone()).unwrap();
        assert!(!omitted.temporary_access);
        for enabled in [false, true] {
            value["temporaryAccess"] = json!(enabled);
            let explicit: StartInput = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(explicit.temporary_access, enabled);
        }
        let schema = serde_json::to_value(schemars::schema_for!(StartInput)).unwrap();
        assert_eq!(schema["properties"]["temporaryAccess"]["default"], false);
    }
}

#[derive(Clone,Serialize,Deserialize,schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartInput {
    collection_method: Option<CollectionMethod>,
    /// Explicit reference to a separately created server-change grant.
    #[serde(skip_serializing_if="Option::is_none")]
    authorization_id:Option<String>,
    /// Named unattended workflow context; it never activates a schedule.
    #[serde(skip_serializing_if="Option::is_none")]
    workflow_id:Option<String>,
    request_id: String,
    destination: String,
    output_mode: OutputMode,
    categories: Option<Vec<String>>,
    #[serde(default)]
    identities: Vec<Identity>,
    page_size: Option<u32>,
    #[serde(default = "default_temporary_access")]
    temporary_access: bool,
    context_run_id: Option<String>,
    /// Per-category run overrides. Omitted values inherit saved plugin options;
    /// explicit blank filters clear this run's selection without saving options.
    #[serde(default)]
    plugin_settings: BTreeMap<String, Value>,
}
pub const OPERATIONS: &[&str] = &[
    "serverChanges.preview", "serverChanges.authorize", "serverChanges.list", "serverChanges.inspect", "serverChanges.revoke",
    "decompiler.start", "decompiler.status", "decompiler.cancel", "decompiler.recover",
    "collection.preferences.get",
    "collection.preferences.set",
    "schema",
    "help.read",
    "about",
    "helpers.reconcile",
    "plugin.configure",
    "profiles.list",
    "profiles.upsert",
    "profiles.rename",
    "profiles.remove",
    "profiles.associateFile",
    "profiles.removeFile",
    "credentials.status",
    "credentials.save",
    "credentials.forget",
    "settings.inspect",
    "settings.update",
    "serverCollection.inspect",
    "serverCollection.reconcile",
    "capabilities",
    "connection.test",
    "connection.connect",
    "connection.disconnect",
    "companies.list",
    "retrieve.start",
    "backup.start",
    "run.status",
    "diagnostics.export",
    "run.list",
    "run.pause",
    "run.resume",
    "run.cancel",
    "run.recover",
    "run.retryFailed",
    "run.reprocess",
];
pub fn operation_schema() -> Value {
    let operations = OPERATIONS
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "description": crate::stdio::description(name),
                "inputSchema": operation_input_schema(name),
                "outputSchema": operation_output_schema(name)
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schemaVersion": 1,
        "framing": "One JSON command and response per UTF-8 line. Secret-bearing input belongs on stdin.",
        "operations": operations,
        "command": {"schemaVersion": 1, "method": "OPERATION", "arguments": {}},
        "credentials": "Write-only input; never included in replies.",
        "runLifetime": "The owning host executes accepted runs. Host exit interrupts execution; journals preserve state. Saved run IDs permit status reads and exclusive recovery as the original caller. Offline source reprocessing needs no connection."
    })
}
pub fn operation_input_schema(name: &str) -> Value {
    let schema = match name {
        "serverChanges.preview"=>serde_json::to_value(schemars::schema_for!(server_change_operations::PreviewInput)),
        "serverChanges.authorize"=>serde_json::to_value(schemars::schema_for!(server_change_operations::AuthorizeInput)),
        "serverChanges.inspect"|"serverChanges.revoke"=>serde_json::to_value(schemars::schema_for!(IdInput)),
        "decompiler.start" => serde_json::to_value(schemars::schema_for!(crate::raw_decompiler::Start)),
        "decompiler.status"|"decompiler.cancel"|"decompiler.recover" => serde_json::to_value(schemars::schema_for!(IdInput)),
        "collection.preferences.set" => serde_json::to_value(schemars::schema_for!(crate::collection_preferences::Preferences)),
        "profiles.upsert" => serde_json::to_value(schemars::schema_for!(Profile)),
        "profiles.rename" => serde_json::to_value(schemars::schema_for!(RenameInput)),
        "profiles.associateFile" => serde_json::to_value(schemars::schema_for!(AssociateFileInput)),
        "credentials.save" => serde_json::to_value(schemars::schema_for!(SaveCredentialInput)),
        "plugin.configure" => serde_json::to_value(schemars::schema_for!(PluginInput)),
        "settings.update" => serde_json::to_value(schemars::schema_for!(OperatingSettings)),
        "serverCollection.reconcile" => serde_json::to_value(schemars::schema_for!(crate::server_collection::ReconcileInput)),
        "connection.connect" | "connection.test" => {
            serde_json::to_value(schemars::schema_for!(ConnectionInput))
        }
        "backup.start" | "retrieve.start" => {
            serde_json::to_value(schemars::schema_for!(StartInput))
        }
        "run.status" => serde_json::to_value(schemars::schema_for!(StatusInput)),
        "diagnostics.export" => serde_json::to_value(schemars::schema_for!(SupportExportInput)),
        "profiles.remove"
        | "helpers.reconcile"
        | "profiles.removeFile"
        | "credentials.status"
        | "credentials.forget"
        | "run.pause"
        | "run.resume"
        | "run.cancel"
        | "run.recover"
        | "run.retryFailed"
        | "run.reprocess" => serde_json::to_value(schemars::schema_for!(IdInput)),
        _ => serde_json::to_value(schemars::schema_for!(NoArguments)),
    };
    let mut schema = schema.expect("Static schema serialization is infallible");
    if let Some(credentials) = schema.pointer_mut("/$defs/Credentials") {
        credentials["writeOnly"] = json!(true);
    }
    schema
}

fn schema<T:schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("Static schema serialization is infallible")
}
fn object_schema(required:&[&str],properties:Value,additional_properties:bool)->Value {
    json!({"type":"object","required":required,"properties":properties,"additionalProperties":additional_properties})
}
pub(crate) fn operation_result_schema(name:&str)->Value {
    let string=||json!({"type":"string"});
    let boolean=||json!({"type":"boolean"});
    let integer=||json!({"type":"integer"});
    let array=|items:Value|json!({"type":"array","items":items});
    let open=|description:&str|json!({"type":"object","description":description,"additionalProperties":true});
    match name {
        "serverChanges.preview"=>schema::<server_change_operations::PreviewResult>(),
        "serverChanges.authorize"=>schema::<server_change_operations::AuthorizationResult>(),
        "serverChanges.list"=>schema::<server_change_operations::AuthorizationList>(),
        "serverChanges.inspect"|"serverChanges.revoke"=>schema::<crate::server_changes::Grant>(),
        "decompiler.start"=>object_schema(&["jobId","accepted"],json!({"jobId":string(),"accepted":{"const":true}}),false),
        "decompiler.status"=>object_schema(&["engineSha256","id","output","input","symbolMode","state","active"],json!({
            "engineSha256":string(),"id":string(),"output":string(),"input":open("Retained input file metadata."),
            "symbols":{"oneOf":[open("Retained symbol file metadata."),{"type":"null"}]},"symbolMode":string(),"state":string(),
            "result":{"type":["object","null"],"additionalProperties":true},"diagnostic":{"oneOf":[schema::<Diagnostic>(),{"type":"null"}]},
            "active":boolean(),"statistics":{"type":"object","additionalProperties":true}
        }),false),
        "decompiler.cancel"=>object_schema(&["cancelRequested"],json!({"cancelRequested":{"const":true}}),false),
        "decompiler.recover"=>json!({"oneOf":[
            object_schema(&["jobId","alreadyFinished"],json!({"jobId":string(),"alreadyFinished":{"const":true}}),false),
            object_schema(&["jobId","recoveredCommit"],json!({"jobId":string(),"recoveredCommit":{"const":true}}),false),
            object_schema(&["jobId","accepted"],json!({"jobId":string(),"accepted":{"const":true}}),false)
        ]}),
        "collection.preferences.get"|"collection.preferences.set"=>schema::<crate::collection_preferences::Preferences>(),
        "schema"=>object_schema(&["schemaVersion","framing","operations","command","credentials","runLifetime"],json!({
            "schemaVersion":{"const":1},"framing":string(),"operations":array(object_schema(&["name","description","inputSchema","outputSchema"],json!({
                "name":string(),"description":string(),"inputSchema":open("JSON Schema for operation arguments."),"outputSchema":open("JSON Schema for structured content.")
            }),false)),"command":open("CLI command envelope."),"credentials":string(),"runLifetime":string()
        }),false),
        "help.read"=>object_schema(&["documents"],json!({"documents":array(schema::<crate::help::HelpDocument>())}),false),
        "about"=>schema::<crate::about::About>(),
        "helpers.reconcile"=>open("Native helper reconciliation result and cleanup evidence."),
        "plugin.configure"=>schema::<GuestReply>(),
        "profiles.list"=>array(schema::<Profile>()),
        "profiles.upsert"=>object_schema(&["saved"],json!({"saved":{"const":true},"warnings":{"type":"array","items":schema::<Diagnostic>(),"description":"For example COMPANY_NOT_SET. Membership is checked by connection.test and connect."}}),false),
        "credentials.save"=>object_schema(&["saved"],json!({"saved":{"const":true}}),false),
        "profiles.rename"=>object_schema(&["renamed"],json!({"renamed":{"const":true}}),false),
        "profiles.remove"=>object_schema(&["removed","credentialCleanup"],json!({"removed":{"const":true},"credentialCleanup":string()}),false),
        "profiles.associateFile"=>object_schema(&["associated","inputFileChanged"],json!({"associated":{"const":true},"inputFileChanged":{"const":false}}),false),
        "profiles.removeFile"=>object_schema(&["removedReference","inputFileChanged"],json!({"removedReference":{"const":true},"inputFileChanged":{"const":false}}),false),
        "credentials.status"=>object_schema(&["status"],json!({"status":{"type":"string","enum":["saved","notSaved"]}}),false),
        "credentials.forget"=>object_schema(&["forgotten"],json!({"forgotten":{"const":true}}),false),
        "settings.inspect"=>object_schema(&["effective","timing","measurement"],json!({"effective":schema::<OperatingSettings>(),"timing":string(),"measurement":string()}),false),
        "settings.update"=>object_schema(&["updated","effectiveTiming"],json!({"updated":{"const":true},"effectiveTiming":string()}),false),
        "serverCollection.inspect"=>json!({"oneOf":[{"type":"null"},object_schema(&["schemaVersion","invocationId","runId","plugin","state"],json!({
            "schemaVersion":{"const":1},"invocationId":string(),"runId":string(),"plugin":string(),"state":string(),"evidence":string()
        }),false)]}),
        "serverCollection.reconcile"=>object_schema(&["schemaVersion","invocationId","runId","plugin","state","evidence"],json!({
            "schemaVersion":{"const":1},"invocationId":string(),"runId":string(),"plugin":string(),"state":{"const":"operatorConfirmedStopped"},"evidence":string()
        }),false),
        "capabilities"=>{
            let mut category=schema::<Category>();
            // Host-side description only; the shared contract bytes stay frozen.
            if let Some(field)=category.pointer_mut("/properties/qualification") {field["description"]=json!("The plugin's plain statement of how to judge its results, or why it is unavailable.");}
            object_schema(&["schemaVersion","categories","diagnostics","discovery","targets","qualification","temporaryAccess","connection"],json!({
            "schemaVersion":{"const":1},"categories":array(category),"diagnostics":array(schema::<Diagnostic>()),"discovery":string(),
            "targets":array(string()),"qualification":{"type":"string","description":"How to judge completeness: check each artifact's result and diagnostics."},
            "temporaryAccess":object_schema(&["default","nativeGrant","nativeCleanup"],json!({
                "default":boolean(),"nativeGrant":string(),"nativeCleanup":string()
            }),false),
            "connection":object_schema(&["allowUntrustedCertificate","companyDiscovery"],json!({
                "allowUntrustedCertificate":{"type":"boolean","description":"Connections accept a per-profile allowUntrustedCertificate option. It relaxes trust in the certificate issuer only; the server name and validity dates are still checked."},
                "companyDiscovery":{"type":"boolean","description":"connection.test without a company lists the companies the account can use, then signs out."}
            }),false)
        }),false)},
        "connection.test"|"connection.connect"=>{
            let company_row=object_schema(&["id","name"],json!({"id":string(),"name":string()}),false);
            object_schema(&["endpoint","company","caller","authMode","admission","version","transportTrust"],json!({
            "endpoint":string(),"company":{"type":["string","null"],"description":"Null when connection.test ran without a company."},"caller":string(),"authMode":string(),"admission":{"const":"verifiedSecurityManager"},"version":{},
            "cleanup":{"type":"null","description":"connection.test only. Null means logout was confirmed."},
            "transportTrust":schema::<crate::transport_trust::TransportTrust>(),
            "availableCompanies":{"type":"array","items":company_row,"description":"connection.test only. Companies this account can use, sorted by id."},
            "companySource":{"type":"string","enum":["userAccess","companyList","unavailable"],"description":"connection.test only. userAccess: the account's own company access; companyList: all companies, access is confirmed when you connect; unavailable: the list could not be read."},
            "selectedCompany":{"oneOf":[object_schema(&["id","inList"],json!({"id":string(),"inList":boolean()}),false),{"type":"null"}],"description":"connection.test only. The requested company and whether it is in availableCompanies."},
            "companyListDiagnostic":{"oneOf":[schema::<Diagnostic>(),{"type":"null"}],"description":"connection.test only. Why the company list could not be read."}
        }),false)},
        "connection.disconnect"=>object_schema(&["disconnected"],json!({"disconnected":{"const":true}}),false),
        "companies.list"=>object_schema(&["companies","availableCompanies","scope"],json!({"companies":open("Native CompanySvc GetList response."),
            "availableCompanies":array(object_schema(&["id","name"],json!({"id":string(),"name":string()}),false)),"scope":string()}),false),
        "retrieve.start"|"backup.start"=>object_schema(&["runId","accepted"],json!({
            "runId":string(),"accepted":{"const":true},"destination":string(),"scope":string(),"reconciled":boolean(),"execution":string()
        }),false),
        "run.recover"|"run.retryFailed"|"run.reprocess"=>object_schema(&["runId","accepted","operation","destination"],json!({
            "runId":string(),"accepted":{"const":true},"operation":string(),"destination":string()
        }),false),
        "run.status"=>object_schema(&["runId","phase","executionActive"],json!({
            "serverVersion":{"type":["string","null"]},"authMode":{"type":["string","null"]},"schemaVersion":integer(),"runId":string(),"requestId":string(),
            "endpoint":string(),"company":string(),"caller":string(),"scope":string(),"categories":array(string()),"pluginHashes":open("Category hash map."),
            "categoryNames":open("Category name map."),"separateFolder":{"type":["string","null"]},"settings":schema::<OperatingSettings>(),"phase":string(),
            "statistics":{"type":["object","null"],"additionalProperties":true},"artifacts":array(schema::<Artifact>()),"diagnostics":array(schema::<Diagnostic>()),
            "pendingFiles":array(schema::<FileRef>()),"acquisitions":array(schema::<FileRef>()),"cursors":open("Saved cursor map."),"discovered":open("Native identity map."),
            "inventoryData":open("Retained inventory file map."),"discoveryFinished":open("Discovery completion map."),"categoryOutcomes":open("Category outcome map."),
            "references":array(json!({})),"definitionViews":open("Definition view map."),"sharedExports":open("Shared export map."),"artifactAcquisitions":open("Artifact acquisition map."),
            "targets":array(schema::<Identity>()),"pluginSettings":open("Frozen plugin setting map."),"pluginSelectionContracts":open("Frozen plugin-declared filter metadata; matching help belongs to the plugin."),"discoveryMessages":open("Successful empty-selection messages after complete discovery."),"managedProcessing":{"type":["object","null"],"additionalProperties":true},
            "temporaryAccessEnabled":boolean(),"collectionMethod":string(),"executionActive":boolean(),"activeOperations":array(json!({})),"pluginStates":open("Transient plugin state."),
            "activityStages":array(json!({})),"transferBatches":array(json!({})),"diagnosticLog":{"oneOf":[{"type":"null"},object_schema(&["path","writeFailed"],json!({"path":string(),"writeFailed":boolean()}),false)]},"pauseRequested":boolean(),"cancelRequested":boolean(),
            "fileCount":integer(),"fileBytes":integer(),"countsPending":boolean(),
            "serverChangeAuthority":{"oneOf":[schema::<crate::server_changes::RunAuthority>(),{"type":"null"}],"description":"The explicit server-change authorization this run uses, or null for a run that may change nothing on the server."},
            "transportTrust":{"oneOf":[schema::<crate::transport_trust::TransportTrust>(),{"type":"null"}],"description":"How the server certificate was trusted for this run."},
            "serverChanges":object_schema(&["leaseLost","resources","foreignPending"],json!({
                "leaseLost":{"type":"boolean","description":"This run's server lease for its temporary helpers was lost. New helper work stopped; Direct reads and cleanup continued."},
                "resources":array(object_schema(&["code","outcome"],json!({"code":string(),"identity":{"oneOf":[schema::<Identity>(),{"type":"null"}]},"context":open("Codes and identities only."),"outcome":schema::<crate::progress_snapshot::ServerChangeOutcome>()}),false)),
                "foreignPending":array(schema::<crate::storage::ForeignPendingHelper>())
            }),false),
            "foreignPendingHelpers":{"type":"array","items":schema::<crate::storage::ForeignPendingHelper>(),"description":"Internal recovery evidence; use serverChanges."}
        }),true),
        "diagnostics.export"=>object_schema(&["saved","path","sanitized","schemaVersion","logs","events","omittedInvalidEvents"],json!({
            "saved":{"const":true},"path":string(),"sanitized":{"const":true},"schemaVersion":{"const":1},"logs":integer(),"events":integer(),"omittedInvalidEvents":integer()
        }),false),
        "run.list"=>array(json!({"oneOf":[object_schema(&["runId","requestId","accepted","destination","endpoint","company","caller","scope","phase","artifacts","diagnostic"],json!({
            "runId":string(),"requestId":string(),"accepted":boolean(),"destination":string(),"endpoint":string(),"company":string(),"caller":string(),"scope":string(),
            "phase":string(),"artifacts":integer(),"diagnostic":{"oneOf":[schema::<Diagnostic>(),{"type":"null"}]}
        }),false),object_schema(&["runId","diagnostic"],json!({"runId":string(),"diagnostic":schema::<Diagnostic>()}),false)]})),
        "run.pause"|"run.resume"|"run.cancel"=>object_schema(&["accepted","runId","effect"],json!({"accepted":boolean(),"runId":string(),"effect":string()}),false),
        _=>unreachable!("Unknown shared operation: {name}"),
    }
}
pub fn operation_output_schema(name:&str)->Value {
    let result=operation_result_schema(name);
    let error=schema::<Diagnostic>();
    let mut output = json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","oneOf":[
        object_schema(&["schemaVersion","ok","result","error"],json!({"schemaVersion":{"const":1},"ok":{"const":true},"result":result,"error":{"type":"null"}}),false),
        object_schema(&["schemaVersion","ok","result","error"],json!({"schemaVersion":{"const":1},"ok":{"const":false},"result":{"type":"null"},"error":error}),false)
    ]});
    rebase_schema_references(&mut output, "", "");
    output
}

// Generated schemas retain their own definitions when embedded. Rebase their
// resource-relative pointers to their exact location in the advertised schema.
fn rebase_schema_references(value: &mut Value, path: &str, resource: &str) {
    match value {
        Value::Object(object) => {
            let resource = if object.contains_key("$schema") { path } else { resource };
            if let Some(Value::String(reference)) = object.get_mut("$ref") {
                if let Some(local) = reference.strip_prefix('#') {
                    *reference = format!("#{resource}{local}");
                }
            }
            for (key, child) in object {
                let key = key.replace('~', "~0").replace('/', "~1");
                rebase_schema_references(child, &format!("{path}/{key}"), resource);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                rebase_schema_references(child, &format!("{path}/{index}"), resource);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod download_surface_tests {
    use super::*;

    #[tokio::test]
    async fn fresh_host_reports_the_default_plugin_concurrency_before_connection() {
        let root = tempfile::tempdir().unwrap();
        let host = Host::with_roots(
            root.path().join("state"),
            root.path().join("plugins"),
            Some(root.path().join("coordination")),
        );
        let settings = host.operation("settings.inspect", json!({})).await.unwrap();
        assert_eq!(settings["effective"]["concurrency"], 4);
        assert_eq!(host.budget.ceiling(), host.settings.read().await.concurrency);
        assert!(host.shutdown().await.is_empty());
    }

    #[test]
    fn explicit_backup_selection_rejects_missing_and_empty_categories() {
        let available=["ud-codes".to_string(),"ud-schema".to_string()].into_iter().collect();
        // A vendor-style unknown ID: requested categories are reported in sorted ID order.
        let mixed=select_full_categories(&available,Some(vec!["ud-codes".into(),"vendor.unknown".into()]),&[
            Diagnostic::new("PLUGIN_INVALID","A plugin failed discovery."),
        ]).unwrap_err();
        assert_eq!(mixed.code,"COLLECTION_UNAVAILABLE");
        assert_eq!(mixed.context["unavailableCategories"],json!(["vendor.unknown"]));
        assert_eq!(mixed.context["requestedCategories"],json!(["ud-codes","vendor.unknown"]));
        assert_eq!(mixed.context["discoveryDiagnostics"][0]["code"],"PLUGIN_INVALID");
        assert_eq!(select_full_categories(&available,None,&[]).unwrap(),vec!["ud-codes","ud-schema"]);
        assert_eq!(select_full_categories(&available,Some(Vec::new()),&[]).unwrap_err().code,"CATEGORY_SELECTION_EMPTY");
    }

    #[tokio::test]
    async fn rejected_connection_input_does_not_change_shared_settings() {
        let root=tempfile::tempdir().unwrap();
        let host=Host::new(root.path().join("state"),root.path().join("plugins"));
        let mut requested=OperatingSettings::default();
        requested.concurrency=6;
        requested.server_concurrency=0;
        let invalid=ConnectionInput {
            profile_id:None,url:Some("https://example.invalid/instance".into()),
            auth_mode:Some(AuthMode::BasicApiKey),company:Some("FIXTURE".into()),
            credential_source:CredentialSource::Inline { credentials:Credentials {
                username:"fixture".into(),password:"fixture".into(),api_key:Some("fixture".into()),
            }},settings:Some(requested),allow_untrusted_certificate:None,
        };
        let error=match host.resolve_connection(invalid).await { Ok(_)=>panic!("invalid connection settings were accepted"),Err(error)=>error };
        assert_eq!(error.code,"INVALID_SERVER_CONCURRENCY");
        assert_eq!(host.budget.ceiling(),4);
        assert_eq!(host.settings.read().await.server_concurrency,4);

        let mut valid_settings=OperatingSettings::default();
        valid_settings.concurrency=6;
        let missing_profile=ConnectionInput {
            profile_id:None,url:Some("https://example.invalid/instance".into()),
            auth_mode:Some(AuthMode::Session),company:Some("FIXTURE".into()),
            credential_source:CredentialSource::SecureStore,settings:Some(valid_settings),allow_untrusted_certificate:None,
        };
        let error=match host.resolve_connection(missing_profile).await { Ok(_)=>panic!("missing secure-store profile was accepted"),Err(error)=>error };
        assert_eq!(error.code,"PROFILE_REQUIRED");
        assert_eq!(host.budget.ceiling(),4);
        assert_eq!(host.settings.read().await.server_concurrency,4);
        assert!(host.shutdown().await.is_empty());
    }

    #[test]
    fn full_retry_clears_old_discovery_verdict_and_keeps_completed_categories() {
        let mut journal = storage::new_journal("retry-discovery".into());
        for (category, outcome) in [
            ("first", "discoveryIncomplete"),
            ("second", "discoveryFailed"),
            ("complete", "verifiedAbsent"),
        ] {
            journal
                .category_outcomes
                .insert(category.into(), outcome.into());
            journal.discovery_finished.insert(category.into(), true);
            journal.cursors.insert(category.into(), "saved-page".into());
        }
        let identity: Identity = serde_json::from_value(json!({"category":"first","kind":"Fixture","scope":{"type":"company","company":"Fixture"},"keys":{"ID":"retained"}})).unwrap();
        journal
            .discovered
            .insert("first".into(), vec![identity.clone()]);
        restart_incomplete_discovery(&mut journal);
        for category in ["first", "second"] {
            assert!(!journal.category_outcomes.contains_key(category));
            assert!(!journal.discovery_finished.contains_key(category));
            assert!(!journal.cursors.contains_key(category));
        }
        assert_eq!(journal.discovered["first"], vec![identity]);
        assert_eq!(journal.category_outcomes["complete"], "verifiedAbsent");
        assert!(journal.discovery_finished["complete"]);
        assert_eq!(journal.cursors["complete"], "saved-page");
    }

    #[tokio::test]
    async fn empty_commands_reject_unknown_input_before_state_checks() {
        let root = tempfile::tempdir().unwrap();
        let host = Host::new(root.path().join("state"), root.path().join("plugins"));
        for method in [
            "schema",
            "profiles.list",
            "settings.inspect",
            "capabilities",
            "connection.disconnect",
            "companies.list",
        ] {
            let reply = host
                .execute(Command {
                    schema_version: 1,
                    method: method.into(),
                    arguments: json!({"unexpected":"private-input"}),
                })
                .await;
            assert!(!reply.ok, "{method}");
            assert_eq!(reply.error.unwrap().code, "INVALID_INPUT", "{method}");
        }
        assert!(host.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn removed_inspectors_are_not_executable_and_download_options_remain() {
        let root = tempfile::tempdir().unwrap();
        let host = Host::with_roots(
            root.path().join("state"),
            root.path().join("plugins"),
            Some(root.path().join("coordination")),
        );
        for removed in [
            "inventory.list",
            "artifact.inspect",
            "access.inspect",
            "plugin.status",
            "plugin.run",
        ] {
            assert!(!OPERATIONS.contains(&removed));
            let reply = host
                .execute(Command {
                    schema_version: 1,
                    method: removed.into(),
                    arguments: json!({"category":"ud-codes","temporaryAccess":true}),
                })
                .await;
            assert!(!reply.ok);
            assert_eq!(reply.error.unwrap().code, "UNKNOWN_OPERATION");
        }
        for retained in [
            "plugin.configure",
            "capabilities",
            "settings.inspect",
            "settings.update",
            "backup.start",
            "retrieve.start",
            "run.status",
            "run.pause",
            "run.resume",
            "run.cancel",
            "run.recover",
            "helpers.reconcile",
        ] {
            assert!(OPERATIONS.contains(&retained));
        }
        let options = operation_input_schema("backup.start");
        for field in [
            "categories",
            "pluginSettings",
            "temporaryAccess",
            "outputMode",
            "pageSize",
        ] {
            assert!(options["properties"].get(field).is_some());
        }
        assert!(host.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn cached_plugin_modules_unload_when_the_last_active_run_settles_and_reload_on_use() {
        let root = tempfile::tempdir().unwrap();
        let (state, plugins) = (root.path().join("state"), root.path().join("plugins"));
        server_change_api_tests::module(&plugins, "Loaded");
        let host = Host::with_roots(state.clone(), plugins.clone(), Some(root.path().join("coordination")));
        let in_use = host.plugin_cache.lock().await.scan(&plugins, &CancellationToken::new()).await.unwrap().0;
        assert_eq!(in_use.len(), 1);
        assert_eq!(host.plugin_cache.lock().await.loaded_modules(), 1, "a first scan describes the plugin live");
        let run = |name: &str, active: bool| {
            let mut journal = storage::new_journal(name.into());
            journal.phase = "completed".into();
            let store = Storage::open(&root.path().join(name), false, journal.clone()).unwrap();
            crate::recovery::RunIndex::open(&state).unwrap().save(&crate::recovery::RunRecord {
                schema_version: 1, scope_hash: "test".into(), destination: store.root().to_string_lossy().to_string(),
                accepted: true, full: false, page_size: None, journal: journal.clone(),
            }).unwrap();
            (journal.run_id, Arc::new(Run {
                storage: Arc::new(std::sync::Mutex::new(store)), cancel: CancellationToken::new(),
                paused: AtomicBool::new(false), wake: Notify::new(), active: AtomicBool::new(active), network: false,
                helper_restore_attempts: Mutex::new(std::collections::BTreeSet::new()), activity: Arc::new(Default::default()),
                shared_reads: Default::default(), helper_instances: Mutex::new(BTreeMap::new()), collection_context: Mutex::new(None),
            }))
        };
        // Another run still executing keeps the modules loaded when one run settles.
        let (busy_id, busy) = run("busy-output", true);
        let (done_id, done) = run("done-output", true);
        host.runs.lock().await.insert(busy_id.clone(), busy.clone());
        host.runs.lock().await.insert(done_id.clone(), done.clone());
        done.storage.lock().unwrap().release_destination().unwrap();
        done.active.store(false, Ordering::SeqCst);
        host.settle_run(&done, true).await;
        assert_eq!(host.plugin_cache.lock().await.loaded_modules(), 1, "an active run keeps cached modules");
        // The last active run settling unloads them.
        busy.storage.lock().unwrap().release_destination().unwrap();
        busy.active.store(false, Ordering::SeqCst);
        host.settle_run(&busy, true).await;
        assert_eq!(host.plugin_cache.lock().await.loaded_modules(), 0, "no run active: cached modules are released");
        // A holder that was using the plugin keeps its own runtime and still works.
        assert_eq!(in_use[0].prepare(&CancellationToken::new()).await.unwrap()["event"], "pluginLoaded");
        // The next use reloads lazily from the cache.
        let reloaded = host.plugin_cache.lock().await.scan(&plugins, &CancellationToken::new()).await.unwrap().0;
        assert_eq!(reloaded[0].prepare(&CancellationToken::new()).await.unwrap()["event"], "pluginLoaded");
        assert_eq!(host.plugin_cache.lock().await.loaded_modules(), 1);
        assert!(host.shutdown().await.is_empty());
    }
    #[tokio::test]
    async fn settled_run_releases_cache_context_and_storage_and_status_still_answers() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let host = Host::with_roots(state.clone(), root.path().join("plugins"), Some(root.path().join("coordination")));
        let fixture = |name: &str| {
            let mut journal = storage::new_journal(name.into());
            journal.phase = "completed".into();
            let store = Storage::open(&root.path().join(name), false, journal.clone()).unwrap();
            crate::recovery::RunIndex::open(&state).unwrap().save(&crate::recovery::RunRecord {
                schema_version: 1, scope_hash: "test".into(), destination: store.root().to_string_lossy().to_string(),
                accepted: true, full: false, page_size: None, journal: journal.clone(),
            }).unwrap();
            (journal.run_id, Arc::new(Run {
                storage: Arc::new(std::sync::Mutex::new(store)), cancel: CancellationToken::new(),
                paused: AtomicBool::new(false), wake: Notify::new(), active: AtomicBool::new(true), network: false,
                helper_restore_attempts: Mutex::new(std::collections::BTreeSet::new()), activity: Arc::new(Default::default()),
                shared_reads: Default::default(), helper_instances: Mutex::new(BTreeMap::new()), collection_context: Mutex::new(None),
            }))
        };
        let retain = |run: Arc<Run>| async move {
            run.shared_reads.read("body".into(), &CancellationToken::new(), || async {
                Ok(crate::connection::NativeResponse { raw: vec![0; 1024], value: json!({}), elapsed_ms: 0 })
            }).await.unwrap();
            *run.collection_context.lock().await = Some(collection::CollectionContext { processors: vec![], page_size: None, full: true, processed_inputs: Default::default() });
        };

        // Released: the host drops the run and answers status from the saved journal.
        let (run_id, run) = fixture("settled-output");
        host.runs.lock().await.insert(run_id.clone(), run.clone());
        retain(run.clone()).await;
        run.activity.plugin_state("fixture-plugin", "finished");
        run.activity.begin_batch("fixture-plugin", 3).complete();
        run.storage.lock().unwrap().release_destination().unwrap();
        run.active.store(false, Ordering::SeqCst);
        host.settle_run(&run, true).await;
        assert!(run.shared_reads.is_empty().await);
        assert!(run.collection_context.lock().await.is_none());
        assert!(!host.runs.lock().await.contains_key(&run_id));
        assert_eq!(Arc::strong_count(&run), 1, "the host keeps no reference to a settled run");
        drop(run);
        for summary in [false, true] {
            let status = host.operation("run.status", json!({"id":run_id,"summary":summary})).await.unwrap();
            assert_eq!(status["phase"], "completed");
            assert_eq!(status["executionActive"], false);
            assert_eq!(status["pluginStates"]["fixture-plugin"], "finished");
            assert_eq!(status["transferBatches"][0]["phase"], "Verified");
            assert_eq!(status["activeOperations"], json!([]));
            assert!(status["statistics"].is_object());
        }
        assert_eq!(host.operation("run.pause", json!({"id":run_id})).await.unwrap()["accepted"], false);

        // Uncertain release: the retained cache and plugins still go, the run stays.
        let (kept_id, kept) = fixture("uncertain-output");
        host.runs.lock().await.insert(kept_id.clone(), kept.clone());
        retain(kept.clone()).await;
        kept.active.store(false, Ordering::SeqCst);
        host.settle_run(&kept, false).await;
        assert!(kept.shared_reads.is_empty().await);
        assert!(kept.collection_context.lock().await.is_none());
        assert!(host.runs.lock().await.contains_key(&kept_id));
        assert_eq!(host.operation("run.status", json!({"id":kept_id})).await.unwrap()["executionActive"], false);

        // A recovery that replaced the entry keeps it.
        let (raced_id, raced) = fixture("raced-output");
        let (_, newer) = fixture("newer-output");
        newer.active.store(false, Ordering::SeqCst);
        host.runs.lock().await.insert(raced_id.clone(), newer.clone());
        raced.storage.lock().unwrap().release_destination().unwrap();
        raced.active.store(false, Ordering::SeqCst);
        host.settle_run(&raced, true).await;
        assert!(Arc::ptr_eq(host.runs.lock().await.get(&raced_id).unwrap(), &newer));
        assert!(!host.settled_runs.lock().await.contains_key(&raced_id));
        assert!(host.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn terminal_status_preserves_execution_until_cleanup_and_saved_status_is_inactive() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let plugins = root.path().join("plugins");
        let coordination = root.path().join("coordination");
        let host = Host::with_roots(state.clone(), plugins.clone(), Some(coordination.clone()));
        let mut journal = storage::new_journal("execution-projection".into());
        journal.phase = "completed".into();
        let run_id = journal.run_id.clone();
        let store = Storage::open(&root.path().join("output"), false, journal.clone()).unwrap();
        crate::recovery::RunIndex::open(&state)
            .unwrap()
            .save(&crate::recovery::RunRecord {
                schema_version: 1,
                scope_hash: "test".into(),
                destination: store.root().to_string_lossy().to_string(),
                accepted: true,
                full: false,
                page_size: None,
                journal,
            })
            .unwrap();
        let run = Arc::new(Run {
            storage: Arc::new(std::sync::Mutex::new(store)),
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
            wake: Notify::new(),
            active: AtomicBool::new(true),
            network: false,
            helper_restore_attempts: Mutex::new(std::collections::BTreeSet::new()),
            activity: Arc::new(Default::default()),
            shared_reads: Default::default(),
            helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
        });
        host.runs.lock().await.insert(run_id.clone(), run.clone());
        let pending = host
            .operation("run.status", json!({"id":run_id}))
            .await
            .unwrap();
        assert_eq!(pending["phase"], "completed");
        assert_eq!(pending["executionActive"], true);

        // The terminal phase alone does not mean the destination lease is free.
        run.storage.lock().unwrap().release_destination().unwrap();
        run.active.store(false, Ordering::SeqCst);
        let finished = host
            .operation("run.status", json!({"id":run_id}))
            .await
            .unwrap();
        assert_eq!(finished["executionActive"], false);

        // Execution ownership belongs to this host, never to a saved journal.
        let saved_host = Host::with_roots(state.clone(), plugins, Some(coordination));
        let saved = saved_host
            .operation("run.status", json!({"id":run_id}))
            .await
            .unwrap();
        assert_eq!(saved["phase"], "completed");
        assert_eq!(saved["executionActive"], false);
        let record = crate::recovery::RunIndex::open(&state)
            .unwrap()
            .get(&run_id)
            .unwrap();
        assert!(
            json!(crate::recovery::read_journal(&record).unwrap())
                .get("executionActive")
                .is_none()
        );
        assert!(host.shutdown().await.is_empty());
        assert!(saved_host.shutdown().await.is_empty());
    }
}


#[cfg(test)]
mod concurrent_cancel_tests {
    use super::*;

    #[tokio::test]
    async fn intentional_cancel_keeps_cleanup_failures_without_duplicate_warnings() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path(), false, storage::new_journal("cancel".into())).unwrap();
        let run = Arc::new(Run {
            storage: Arc::new(std::sync::Mutex::new(store)),
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
            wake: Notify::new(),
            active: AtomicBool::new(false),
            network: false,
            helper_restore_attempts: Mutex::new(Default::default()),
            activity: Arc::new(Default::default()),
            shared_reads: Default::default(),
            helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
        });
        run.cancel.cancel();
        record_diagnostic(&run, Diagnostic::new("CANCELED", "The run was canceled.")).await.unwrap();
        let failure = Diagnostic::new("CLEANUP_UNCERTAIN", "The helper removal is not verified.");
        record_diagnostic(&run, failure.clone()).await.unwrap();
        record_diagnostic(&run, failure.clone()).await.unwrap();
        assert_eq!(run.storage.lock().unwrap().journal.diagnostics, vec![failure]);
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SupportExportInput { #[serde(default)] id:Option<String>, path:String }

#[cfg(test)]
mod support_export_api_tests {
    use super::*;
    #[tokio::test]
    async fn connection_failure_packet_needs_no_run_or_network_and_preserves_existing_files() {
        let temp=tempfile::tempdir().unwrap();
        let host=Host::with_roots(temp.path().join("state"),temp.path().join("plugins"),Some(temp.path().join("coordination")));
        let call=|method:&str,arguments|Command{schema_version:1,method:method.into(),arguments};
        let reply=host.execute(call("connection.inspect",json!({}))).await;
        assert!(!reply.ok);
        let target=temp.path().join("support.zip");
        let packet=host.execute(call("diagnostics.export",json!({"path":target}))).await;
        assert!(packet.ok);
        let before=std::fs::read(&target).unwrap();
        let second=host.execute(call("diagnostics.export",json!({"path":target}))).await;
        assert!(!second.ok);assert_eq!(std::fs::read(target).unwrap(),before);
        assert!(host.runs.lock().await.is_empty());assert!(host.connection.read().await.is_none());
    }
}
