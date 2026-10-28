//! Discover actual Wasm modules and run ABI 1 in isolated stores.
//! Descriptors request capabilities. The host remains responsible for granting them.
use herbert_contract::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;
use wasmtime::{Engine, Instance, Module, Store};

#[derive(Clone)]
pub struct Plugin {
    pub category: Category,
    pub hash: String,
    runtime: Arc<Runtime>,
    pub methods: Vec<(String, String)>,
    pub entities: Vec<(String, String)>,
    pub binary_exports: Vec<BinaryExport>,
    pub response_policies: Vec<NativeResponsePolicy>,
    state_file: Option<Arc<crate::plugin_state::StateFile>>,
    module_path: Option<PathBuf>,
}
fn plugin_error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn invoke(
    runtime: &Runtime,
    input: &GuestRequest,
    cancel: &CancellationToken,
) -> Result<GuestReply, Diagnostic> {
    let engine = &runtime.engine;
    let module = &runtime.module;
    let mut store = Store::new(engine, ());
    store.set_epoch_deadline(1);
    let operation_cancel = cancel.clone();
    store.epoch_deadline_callback(move |_| {
        Ok(if operation_cancel.is_cancelled() {
            wasmtime::UpdateDeadline::Interrupt
        } else {
            wasmtime::UpdateDeadline::Continue(1)
        })
    });
    if cancel.is_cancelled() {
        return Err(plugin_error(
            "CANCELED",
            "The plugin operation was canceled.",
        ));
    }
    let instance = Instance::new(&mut store, module, &[])
        .map_err(|_| plugin_error("PLUGIN_TRAPPED", "The plugin failed during initialization."))?;
    let abi = instance
        .get_typed_func::<(), u32>(&mut store, "hoover_abi")
        .map_err(|_| {
            plugin_error(
                "PLUGIN_INCOMPATIBLE",
                "The ABI export is missing or incompatible.",
            )
        })?;
    if abi.call(&mut store, ()).map_err(|_| {
        plugin_error(
            "PLUGIN_TRAPPED",
            "The plugin trapped during compatibility inspection.",
        )
    })? != PLUGIN_ABI
    {
        return Err(plugin_error(
            "PLUGIN_INCOMPATIBLE",
            "The plugin ABI version does not match the host.",
        ));
    }
    let alloc = instance
        .get_typed_func::<u32, u32>(&mut store, "hoover_alloc")
        .map_err(|_| plugin_error("PLUGIN_INCOMPATIBLE", "The allocation export is missing."))?;
    let call = instance
        .get_typed_func::<(u32, u32), u64>(&mut store, "hoover_call")
        .map_err(|_| plugin_error("PLUGIN_INCOMPATIBLE", "The invocation export is missing."))?;
    let free = instance
        .get_typed_func::<(u32, u32), ()>(&mut store, "hoover_free")
        .map_err(|_| plugin_error("PLUGIN_INCOMPATIBLE", "The release export is missing."))?;
    let memory = instance.get_memory(&mut store, "memory").ok_or_else(|| {
        plugin_error(
            "PLUGIN_INCOMPATIBLE",
            "The plugin linear-memory export is missing.",
        )
    })?;
    let payload = serde_json::to_vec(input)
        .map_err(|_| plugin_error("PLUGIN_INPUT_FAILED", "Cannot serialize plugin input."))?;
    // wasm32's address space is an external format constraint, not an artifact-size
    // cap. Large native bytes remain host files, outside guest memory.
    let len=u32::try_from(payload.len()).map_err(|_|plugin_error("PLUGIN_ADDRESS_SPACE","The guest message exceeds the wasm32 address space; keep native payloads in host files."))?;
    let ptr = alloc.call(&mut store, len).map_err(|_| {
        plugin_error(
            "PLUGIN_TRAPPED",
            "The plugin trapped while allocating input.",
        )
    })?;
    memory
        .write(&mut store, ptr as usize, &payload)
        .map_err(|_| {
            plugin_error(
                "PLUGIN_MEMORY_INVALID",
                "The plugin supplied an invalid input pointer.",
            )
        })?;
    let packed = call.call(&mut store, (ptr, len)).map_err(|_| {
        plugin_error(
            "PLUGIN_TRAPPED",
            "The plugin trapped during this operation. The host remains available.",
        )
    })?;
    free.call(&mut store, (ptr, len)).map_err(|_| {
        plugin_error(
            "PLUGIN_TRAPPED",
            "The plugin trapped while releasing input.",
        )
    })?;
    let out_ptr = (packed >> 32) as usize;
    let out_len = (packed & 0xffff_ffff) as usize;
    let end = out_ptr.checked_add(out_len).ok_or_else(|| {
        plugin_error(
            "PLUGIN_MEMORY_INVALID",
            "The guest output pointer overflowed.",
        )
    })?;
    let data = memory.data(&store).get(out_ptr..end).ok_or_else(|| {
        plugin_error(
            "PLUGIN_MEMORY_INVALID",
            "The guest output lies outside its allocated memory.",
        )
    })?;
    let result = herbert_contract::decode_local_json::<GuestReply>(data).map_err(|_| {
        plugin_error(
            "PLUGIN_OUTPUT_INVALID",
            "The plugin output does not match the versioned contract.",
        )
    })?;
    free.call(&mut store, (out_ptr as u32, out_len as u32))
        .map_err(|_| {
            plugin_error(
                "PLUGIN_TRAPPED",
                "The plugin trapped while releasing output.",
            )
        })?;
    Ok(result)
}
struct Runtime {
    engine: Engine,
    module: Module,
    source_hash: String,
}
impl Runtime {
    fn source_hash(bytes: &[u8]) -> String { crate::storage::sha256(bytes) }
    fn compile(bytes: &[u8]) -> Result<Self, Diagnostic> {
        let source_hash = Self::source_hash(bytes);
        let mut config = wasmtime::Config::new();
        config.epoch_interruption(true);
        config.target(env!("HERBERT_NATIVE_TARGET")).map_err(|_| plugin_error("PLUGIN_RUNTIME_UNAVAILABLE", "Cannot select the plugin runtime target."))?;
        let engine = Engine::new(&config).map_err(|_| plugin_error("PLUGIN_RUNTIME_UNAVAILABLE", "Cannot initialize the plugin runtime."))?;
        let module = Module::new(&engine, bytes).map_err(|_| plugin_error("PLUGIN_INVALID", "The plugin is not valid WebAssembly for this runtime."))?;
        if module.imports().next().is_some() {
            return Err(plugin_error(
                "PLUGIN_CAPABILITY_DENIED",
                "Plugins must have no ambient system, network, filesystem or environment imports.",
            ));
        }
        Ok(Self {
            engine,
            module,
            source_hash,
        })
    }
}
struct InvocationGuard {
    cancel: CancellationToken,
    engine: Engine,
    interrupt: tokio::task::JoinHandle<()>,
}
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.engine.increment_epoch();
        self.interrupt.abort();
    }
}
async fn call_runtime(
    runtime: Arc<Runtime>,
    request: GuestRequest,
    cancel: CancellationToken,
) -> Result<GuestReply, Diagnostic> {
    if cancel.is_cancelled() {
        return Err(plugin_error(
            "CANCELED",
            "The plugin operation was canceled.",
        ));
    }
    let operation = cancel.child_token();
    let stop = operation.clone();
    let interrupt_engine = runtime.engine.clone();
    let interrupt = tokio::spawn(async move {
        stop.cancelled().await;
        interrupt_engine.increment_epoch();
    });
    let _guard = InvocationGuard {
        cancel: operation.clone(),
        engine: runtime.engine.clone(),
        interrupt,
    };
    let worker = operation.clone();
    let result = tokio::task::spawn_blocking(move || invoke(&runtime, &request, &worker))
        .await
        .map_err(|_| plugin_error("PLUGIN_TASK_FAILED", "The plugin execution task failed."))?;
    if operation.is_cancelled() {
        return Err(plugin_error(
            "CANCELED",
            "The plugin operation was canceled.",
        ));
    }
    result
}
pub async fn call_bytes(
    bytes: Arc<Vec<u8>>,
    request: GuestRequest,
    cancel: CancellationToken,
) -> Result<GuestReply, Diagnostic> {
    let runtime = tokio::task::spawn_blocking(move || Runtime::compile(&bytes))
        .await
        .map_err(|_| plugin_error("PLUGIN_TASK_FAILED", "Plugin compilation failed."))??;
    call_runtime(Arc::new(runtime), request, cancel).await
}
impl Plugin {
    pub(crate) fn portable_state(&self) -> Option<&crate::plugin_state::StateFile> { self.state_file.as_deref() }
    /// Refresh native export bytes once at the beginning of a new operation.
    /// The caller retains these snapshots for the entire operation and recovery.
    pub(crate) async fn snapshot_artifacts(
        &self,
    ) -> Result<Vec<crate::plugin_artifacts::NativeArtifactSnapshot>, Diagnostic> {
        match &self.module_path {
            Some(module) => crate::plugin_artifacts::snapshot(module).await,
            None => Ok(Vec::new()),
        }
    }
    async fn with_artifact_manifest(mut self, path: &Path) -> Result<Self, Diagnostic> {
        self.category.artifacts = crate::plugin_artifacts::manifest(path).await?.artifacts;
        self.module_path = Some(path.to_path_buf());
        Ok(self)
    }

    pub(crate) fn has_role(&self, role: &str) -> bool {
        self.category.roles.iter().any(|declared| declared == role)
    }

    pub async fn load(path: &Path) -> Result<Self, Diagnostic> {
        let bytes = Arc::new(tokio::fs::read(path).await.map_err(|_| {
            plugin_error("PLUGIN_MISSING", "The category plugin file is unavailable.")
        })?);
        Self::from_bytes(
            bytes,
            Some(Arc::new(crate::plugin_state::StateFile::for_module(path))),
            CancellationToken::new(),
        )
        .await?
        .with_artifact_manifest(path)
        .await
    }
    async fn from_bytes(
        bytes: Arc<Vec<u8>>,
        state_file: Option<Arc<crate::plugin_state::StateFile>>,
        cancel: CancellationToken,
    ) -> Result<Self, Diagnostic> {
        let compilation = bytes.clone();
        let runtime = Arc::new(
            tokio::task::spawn_blocking(move || Runtime::compile(&compilation))
                .await
                .map_err(|_| plugin_error("PLUGIN_TASK_FAILED", "Plugin compilation failed."))??,
        );
        Self::describe(bytes, runtime, state_file, cancel).await
    }
    async fn describe(
        bytes: Arc<Vec<u8>>,
        runtime: Arc<Runtime>,
        state_file: Option<Arc<crate::plugin_state::StateFile>>,
        cancel: CancellationToken,
    ) -> Result<Self, Diagnostic> {
        let state = if let Some(file) = &state_file {
            let file = file.clone();
            tokio::task::spawn_blocking(move || file.read())
                .await
                .map_err(|_| {
                    plugin_error("PLUGIN_STATE_UNAVAILABLE", "Module state reading failed.")
                })??
        } else {
            serde_json::Value::Null
        };
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "descriptor".into(),
            category: String::new(),
            company: String::new(),
            identity: None,
            cursor: None,
            page_size: None,
            state: serde_json::json!({"_pluginState":state}),
            response: None,
            error: None,
        };
        let reply = call_runtime(runtime.clone(), request, cancel).await?;
        if let GuestReply::Descriptor {
            abi,
            category: descriptor,
            methods,
            entities,
            binary_exports,
            response_policies,
        } = reply
        {
            if abi != PLUGIN_ABI
                || descriptor.id.is_empty()
                || crate::storage::safe_relative(&descriptor.id).is_err()
                || descriptor.id.contains('/')
            {
                return Err(plugin_error(
                    "PLUGIN_INCOMPATIBLE",
                    "The plugin descriptor does not match the selected category.",
                ));
            }
            if methods.iter().chain(entities.iter()).any(|(service, operation)| !crate::connection::valid_segment(service) || !crate::connection::valid_segment(operation)) {
                return Err(plugin_error("PLUGIN_DESCRIPTOR_INVALID", "Native route declarations must contain valid service and operation names."));
            }
            if binary_exports.iter().any(|d| !crate::connection::valid_segment(&d.service) || !crate::connection::valid_segment(&d.method) || !d.pointer.starts_with('/') || d.extension.is_empty() || !d.extension.chars().all(|c| c.is_ascii_alphanumeric())) {
                return Err(plugin_error("PLUGIN_DESCRIPTOR_INVALID", "A native byte-field declaration has an invalid route, JSON pointer, or file extension."));
            }
            let mut policy_services = std::collections::BTreeSet::new();
            if response_policies.iter().any(|policy| !methods.iter().any(|(service, _)| service == &policy.service) || !policy_services.insert(&policy.service) || !crate::response_policy::valid(policy)) {
                return Err(plugin_error("PLUGIN_DESCRIPTOR_INVALID", "A response policy must name one declared native service and valid field rules."));
            }
            let mut libraries = std::collections::BTreeSet::new();
            for helper in &descriptor.helpers {
                let mut functions = std::collections::BTreeSet::new();
                if helper.library_id.is_empty()
                    || helper.company_argument.is_empty()
                    || uuid::Uuid::parse_str(&helper.global_id).is_err()
                    || !libraries.insert(&helper.library_id)
                    || helper.functions.is_empty()
                {
                    return Err(plugin_error(
                        "PLUGIN_DESCRIPTOR_INVALID",
                        "An installed helper source contract has an invalid identity.",
                    ));
                }
                for function in &helper.functions {
                    if function.function_id.is_empty() || !functions.insert(&function.function_id) {
                        return Err(plugin_error(
                            "PLUGIN_DESCRIPTOR_INVALID",
                            "An installed helper contract has invalid or duplicate Function source claims.",
                        ));
                    }
                }
            }
            herbert_contract::resolve_plugin_settings(
                &descriptor.settings,
                &serde_json::Value::Null,
            )?;
            Ok(Self {
                category: descriptor,
                hash: crate::storage::sha256(&bytes),
                runtime,
                methods,
                entities,
                binary_exports,
                response_policies,
                state_file,
                module_path: None,
            })
        } else {
            Err(plugin_error(
                "PLUGIN_OUTPUT_INVALID",
                "The plugin did not return its descriptor.",
            ))
        }
    }
    pub async fn call(
        &self,
        mut request: GuestRequest,
        cancel: CancellationToken,
    ) -> Result<GuestReply, Diagnostic> {
        let before = if let Some(file) = &self.state_file {
            let file = file.clone();
            tokio::task::spawn_blocking(move || file.read())
                .await
                .map_err(|_| {
                    plugin_error("PLUGIN_STATE_UNAVAILABLE", "Module state reading failed.")
                })??
        } else {
            serde_json::Value::Null
        };
        if !request.state.is_object() {
            request.state = serde_json::json!({});
        }
        request.state["_pluginState"] = before.clone();
        let mut reply = call_runtime(self.runtime.clone(), request, cancel).await?;
        let mut after = before.clone();
        while let GuestReply::State {
            plugin_state,
            reply: next,
        } = reply
        {
            after = plugin_state;
            reply = *next;
        }
        if after != before {
            let file = self
                .state_file
                .as_ref()
                .ok_or_else(|| {
                    plugin_error(
                        "PLUGIN_STATE_UNAVAILABLE",
                        "This in-memory module has no portable state destination.",
                    )
                })?
                .clone();
            tokio::task::spawn_blocking(move || file.replace(&before, &after))
                .await
                .map_err(|_| {
                    plugin_error("PLUGIN_STATE_UNAVAILABLE", "Module state writing failed.")
                })??;
        }
        Ok(reply)
    }
}
/// The folder is authoritative. No filename, category ID or expected roster is
/// registered in the host. Nested package folders are discovered without a cap.
#[derive(Default)]
pub struct FolderCache {
    current: BTreeMap<PathBuf, Plugin>,
}
pub async fn discover(root: &Path) -> Result<(Vec<Plugin>, Vec<Diagnostic>), Diagnostic> {
    FolderCache::default()
        .scan(root, &CancellationToken::new())
        .await
}
impl FolderCache {
    async fn read_plugin(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> Result<Plugin, Diagnostic> {
        let bytes = tokio::fs::read(path).await.map_err(|_| {
            plugin_error("PLUGIN_UNAVAILABLE", "An installed module cannot be read.")
        })?;
        // Reuse compiled modules only while their portable Wasm bytes match.
        let hash = crate::storage::sha256(&bytes);
        let source_hash = Runtime::source_hash(&bytes);
        let matches_source =
            |plugin: &&Plugin| plugin.hash == hash && plugin.runtime.source_hash == source_hash;
        let runtime = self
            .current
            .get(path)
            .filter(matches_source)
            .or_else(|| self.current.values().find(matches_source))
            .map(|plugin| plugin.runtime.clone());
        let state_file = Some(Arc::new(crate::plugin_state::StateFile::for_module(path)));
        let plugin = match runtime {
            Some(runtime) => {
                Plugin::describe(Arc::new(bytes), runtime, state_file, cancel.clone()).await
            }
            None => Plugin::from_bytes(Arc::new(bytes), state_file, cancel.clone()).await,
        }?;
        plugin.with_artifact_manifest(path).await
    }
    pub async fn scan(
        &mut self,
        root: &Path,
        cancel: &CancellationToken,
    ) -> Result<(Vec<Plugin>, Vec<Diagnostic>), Diagnostic> {
        let root = match tokio::fs::canonicalize(root).await {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.current.clear();
                return Ok((vec![], vec![]));
            }
            Err(_) => {
                return Err(plugin_error(
                    "PLUGIN_FOLDER_UNAVAILABLE",
                    "Cannot read the selected plugin folder.",
                ));
            }
        };
        let mut folders = vec![root.clone()];
        let mut paths = Vec::new();
        while let Some(folder) = folders.pop() {
            if cancel.is_cancelled() {
                return Err(plugin_error(
                    "CANCELED",
                    "Available collection discovery was canceled.",
                ));
            }
            let mut entries = tokio::fs::read_dir(&folder).await.map_err(|_| {
                plugin_error(
                    "PLUGIN_FOLDER_UNAVAILABLE",
                    "Cannot enumerate an installed plugin folder.",
                )
            })?;
            while let Some(entry) = entries.next_entry().await.map_err(|_| {
                plugin_error(
                    "PLUGIN_FOLDER_UNAVAILABLE",
                    "Cannot read an installed plugin entry.",
                )
            })? {
                let kind = entry.file_type().await.map_err(|_| {
                    plugin_error(
                        "PLUGIN_FOLDER_UNAVAILABLE",
                        "Cannot inspect an installed plugin entry.",
                    )
                })?;
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    folders.push(entry.path());
                } else if kind.is_file()
                    && entry.path().extension().is_some_and(|s| {
                        s.to_str()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
                    })
                {
                    paths.push(entry.path());
                }
            }
        }
        paths.sort();
        self.current.retain(|path, _| paths.contains(path));
        let mut loaded = Vec::<Plugin>::new();
        let mut diagnostics = Vec::new();
        let mut conflicts = std::collections::BTreeSet::new();
        for path in paths {
            if cancel.is_cancelled() {
                return Err(plugin_error(
                    "CANCELED",
                    "Available collection discovery was canceled.",
                ));
            }
            let result = self.read_plugin(&path, cancel).await;
            if let Ok(plugin) = &result {
                self.current.insert(path.clone(), plugin.clone());
            } else {
                self.current.remove(&path);
            }
            match result {
                Ok(plugin) => {
                    if conflicts.contains(&plugin.category.id) {
                        continue;
                    }
                    if let Some(previous) =
                        loaded.iter().find(|p| p.category.id == plugin.category.id)
                    {
                        if previous.hash == plugin.hash
                            && serde_json::to_value(&previous.category).ok()
                                == serde_json::to_value(&plugin.category).ok()
                        {
                            let same_artifacts = if plugin.category.artifacts.is_empty() {
                                true
                            } else {
                                match (
                                    previous.snapshot_artifacts().await,
                                    plugin.snapshot_artifacts().await,
                                ) {
                                    (Ok(left), Ok(right)) => {
                                        left.len() == right.len()
                                            && left.iter().zip(&right).all(|(a, b)| {
                                                a.declaration == b.declaration && a.files == b.files
                                            })
                                    }
                                    (left, right) => {
                                        for error in [left.err(), right.err()].into_iter().flatten()
                                        {
                                            diagnostics.push(error);
                                        }
                                        false
                                    }
                                }
                            };
                            if same_artifacts {
                                continue;
                            }
                        }
                        conflicts.insert(plugin.category.id.clone());
                        loaded.retain(|p| p.category.id != plugin.category.id);
                        diagnostics.push(plugin_error("PLUGIN_ID_CONFLICT","Two installed packages claim the same category ID with different code, declarations or native artifact files; neither can own that ambiguous identity."));
                    } else {
                        loaded.push(plugin);
                    }
                }
                Err(error) => diagnostics.push(error),
            }
        }
        loaded.sort_by(|a, b| a.category.id.cmp(&b.category.id));
        Ok((loaded, diagnostics))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "descriptor".into(),
            category: "C01".into(),
            company: String::new(),
            identity: None,
            cursor: None,
            page_size: None,
            state: serde_json::Value::Null,
            response: None,
            error: None,
        }
    }
    #[tokio::test]
    async fn trap_and_ambient_import_do_not_crash_host() {
        let guest=wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "hoover_abi") (result i32) i32.const 1) (func (export "hoover_alloc") (param i32) (result i32) i32.const 0) (func (export "hoover_free") (param i32 i32)) (func (export "hoover_call") (param i32 i32) (result i64) unreachable))"#).unwrap();
        assert_eq!(
            call_bytes(Arc::new(guest), request(), CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code,
            "PLUGIN_TRAPPED"
        );
        let imported = wat::parse_str(r#"(module (import "env" "network" (func)))"#).unwrap();
        assert_eq!(
            call_bytes(Arc::new(imported), request(), CancellationToken::new())
                .await
                .err()
                .unwrap()
                .code,
            "PLUGIN_CAPABILITY_DENIED"
        );
    }
    #[tokio::test]
    async fn infinite_guest_is_interruptible_without_duration_cap() {
        let guest=wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "hoover_abi") (result i32) i32.const 1) (func (export "hoover_alloc") (param i32) (result i32) i32.const 0) (func (export "hoover_free") (param i32 i32)) (func (export "hoover_call") (param i32 i32) (result i64) (loop br 0) i64.const 0))"#).unwrap();
        let cancel = CancellationToken::new();
        // Finish compilation before starting the invocation. Canceling a queued
        // compilation would not establish that an executing loop is interruptible.
        let runtime = Arc::new(Runtime::compile(&guest).unwrap());
        let task = tokio::spawn(call_runtime(runtime, request(), cancel.clone()));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel.cancel();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(30), task)
                .await
                .unwrap()
                .unwrap()
                .err()
                .unwrap()
                .code,
            "CANCELED"
        );
    }
    #[tokio::test]
    async fn plugin_declares_new_native_methods_without_host_registration() {
        let descriptor = serde_json::to_vec(&GuestReply::Descriptor {
            binary_exports: vec![],
            response_policies: vec![],
            entities: vec![],
            abi: 1,
            category: Category {
                id: "C08".into(),
                name: "Independent plugin".into(),
                selected: true,
                description: "fixture".into(),
                plugin_version: Some("test".into()),
                available: true,
                qualification: "fixture".into(),
                roles: vec![],
                helpers: vec![],
                artifacts: vec![],
                settings: vec![],
                help: vec![],
            },
            methods: vec![("Vendor.BO.NewDefinitionSvc".into(), "ReadCustomDefinition".into()), ("Ice.BO.DynamicQuerySvc".into(), "Execute".into())],
        })
        .unwrap();
        let literal = descriptor
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        let module=wat::parse_str(format!(r#"(module (memory (export "memory") 1) (data (i32.const 4096) "{literal}") (func (export "hoover_abi") (result i32) i32.const 1) (func (export "hoover_alloc") (param i32) (result i32) i32.const 0) (func (export "hoover_free") (param i32 i32)) (func (export "hoover_call") (param i32 i32) (result i64) i64.const {}))"#, (4096_u64<<32)|descriptor.len() as u64)).unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("forged.wasm");
        std::fs::write(&path, module).unwrap();
        let loaded = Plugin::load(&path).await.unwrap();
        assert!(loaded.methods.contains(&("Vendor.BO.NewDefinitionSvc".into(), "ReadCustomDefinition".into())));
        assert!(loaded.methods.contains(&("Ice.BO.DynamicQuerySvc".into(), "Execute".into())));
    }
    #[tokio::test]
    async fn plain_module_manifest_loads_without_an_author_hash_and_refreshes_on_warm_scan() {
        let folder = tempfile::tempdir().unwrap();
        let package = folder.path().join("ordinary-plugin");
        tokio::fs::create_dir(&package).await.unwrap();
        let descriptor = serde_json::json!({"type":"descriptor", "abi":1, "category":{"id":"example-native", "name":"Native artifacts", "description":"fixture", "pluginVersion":"1", "available":true, "qualification":"fixture", "roles":["collector"]}, "methods":[]});
        let descriptor = serde_json::to_vec(&descriptor).unwrap();
        let literal = descriptor
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        let wasm = wat::parse_str(format!(r#"(module (memory (export "memory") 1) (data (i32.const 4096) "{literal}") (func (export "hoover_abi") (result i32) i32.const 1) (func (export "hoover_alloc") (param i32) (result i32) i32.const 0) (func (export "hoover_free") (param i32 i32)) (func (export "hoover_call") (param i32 i32) (result i64) i64.const {}))"#, (4096_u64 << 32) | descriptor.len() as u64)).unwrap();
        let module = package.join("plugin.wasm");
        tokio::fs::write(&module, wasm).await.unwrap();
        tokio::fs::write(package.join("query.baq"), b"first native export")
            .await
            .unwrap();
        let mut manifest = serde_json::json!({"artifacts":[{"alias":"query", "type":"baq", "file":"query.baq", "lifetime":"temporary", "policy":"createInstance", "effects":{"description":"Installs a temporary query, downloads records and removes it."}}]});
        tokio::fs::write(
            package.join("plugin.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .await
        .unwrap();
        let mut cache = FolderCache::default();
        let (first, errors) = cache
            .scan(folder.path(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].category.artifacts[0].alias, "query");
        let active = first[0].snapshot_artifacts().await.unwrap();
        tokio::fs::write(package.join("query.baq"), b"second native export")
            .await
            .unwrap();
        manifest["artifacts"][0]["effects"]["description"] =
            serde_json::json!("Updated visible effects.");
        tokio::fs::write(
            package.join("plugin.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .await
        .unwrap();
        let (second, errors) = cache
            .scan(folder.path(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            second[0].category.artifacts[0].effects.description,
            "Updated visible effects."
        );
        assert!(Arc::ptr_eq(&first[0].runtime, &second[0].runtime));
        assert_eq!(
            active[0].bytes_for_version("").unwrap().as_slice(),
            b"first native export"
        );
        assert_eq!(
            second[0].snapshot_artifacts().await.unwrap()[0]
                .bytes_for_version("")
                .unwrap()
                .as_slice(),
            b"second native export"
        );
        let copy = folder.path().join("other-copy");
        tokio::fs::create_dir(&copy).await.unwrap();
        for name in ["plugin.wasm", "plugin.json", "query.baq"] {
            tokio::fs::write(
                copy.join(name),
                tokio::fs::read(package.join(name)).await.unwrap(),
            )
            .await
            .unwrap();
        }
        let (identical, errors) = cache
            .scan(folder.path(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(identical.len(), 1);
        tokio::fs::write(copy.join("query.baq"), b"different supplied business logic")
            .await
            .unwrap();
        let (ambiguous, errors) = cache
            .scan(folder.path(), &CancellationToken::new())
            .await
            .unwrap();
        assert!(ambiguous.is_empty());
        assert!(
            errors
                .iter()
                .any(|error| error.code == "PLUGIN_ID_CONFLICT")
        );
    }
}
