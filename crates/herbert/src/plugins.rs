//! Discover actual Wasm modules and run ABI 1 in isolated stores.
//! Descriptors request capabilities. The host remains responsible for granting them.
use herbert_contract::*;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use wasmtime::{Engine, Instance, Module, Store};

#[derive(Clone)]
pub struct Plugin {
    pub category: Category,
    pub hash: String,
    /// Compiled on first use when the descriptor came from the descriptor cache.
    runtime: Arc<OnceCell<Arc<Runtime>>>,
    pub methods: Vec<(String, String)>,
    pub entities: Vec<(String, String)>,
    pub binary_exports: Vec<BinaryExport>,
    pub response_policies: Vec<NativeResponsePolicy>,
    state_file: Option<Arc<crate::plugin_state::StateFile>>,
    module_path: Option<PathBuf>,
    shipped: Option<Arc<ShippedPackage>>,
    cached_descriptor: Option<Arc<CachedDescriptor>>,
}

/// The wasmtime release pinned in Cargo.lock. Package engine fingerprints name it, and a test keeps
/// it equal to the lock file.
const WASMTIME_VERSION: &str = "49.0.3";

/// Operating default, not a plugin limit: plugin compilation leaves two logical cores for the window
/// and its event loop. Evidence: compiling the 24 shipped guests at launch kept about 13 cores busy
/// for about 9 s while the first window was shown (gui-polish receipt, "Launch window stays white").
/// Compilation still uses every other core; machines with three or fewer cores compile on one thread.
const UI_RESERVED_CORES: usize = 2;
fn compile_threads(available: usize) -> usize {
    available.saturating_sub(UI_RESERVED_CORES).max(1)
}
fn compile_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let available = std::thread::available_parallelism().map_or(1, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(compile_threads(available))
            .thread_name(|index| format!("herbert-plugin-compile-{index}"))
            .build()
            .ok()
    })
    .as_ref()
}
/// wasmtime's parallel compilation runs on the current rayon pool, so this bounds it.
fn compiling<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    match compile_pool() {
        Some(pool) => pool.install(work),
        None => work(),
    }
}
/// The one engine configuration for compiling, deserializing and `herbert precompile`. Every
/// plugin shares this engine (an `Engine` clone is a reference), so their modules share one
/// code-memory and type registry instead of one per plugin. The configuration, and therefore
/// the fingerprint that precompiled modules are checked against, is unchanged. Epochs are
/// engine-wide: a cancellation's epoch bump runs every live store's deadline callback, and a
/// store stops only when its own operation is canceled.
fn engine() -> Result<Engine, Diagnostic> {
    static ENGINE: OnceLock<Result<Engine, Diagnostic>> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = wasmtime::Config::new();
        config.epoch_interruption(true);
        config.target(env!("HERBERT_NATIVE_TARGET")).map_err(|_| plugin_error("PLUGIN_RUNTIME_UNAVAILABLE", "Cannot select the plugin runtime target."))?;
        Engine::new(&config).map_err(|_| plugin_error("PLUGIN_RUNTIME_UNAVAILABLE", "Cannot initialize the plugin runtime."))
    }).clone()
}
/// `wasmtime-<version>;<target>;<sha256 of the engine's precompile compatibility hash>`. It names
/// no path, host or user. Precompiled modules load only under an identical fingerprint.
pub fn engine_fingerprint() -> Result<String, Diagnostic> {
    struct Digest(sha2::Sha256);
    impl std::hash::Hasher for Digest {
        fn write(&mut self, bytes: &[u8]) {
            sha2::Digest::update(&mut self.0, bytes);
        }
        fn finish(&self) -> u64 {
            0
        }
    }
    let engine = engine()?;
    let mut digest = Digest(<sha2::Sha256 as sha2::Digest>::new());
    std::hash::Hash::hash(&engine.precompile_compatibility_hash(), &mut digest);
    Ok(format!("wasmtime-{WASMTIME_VERSION};{};{}", env!("HERBERT_NATIVE_TARGET"), hex::encode(sha2::Digest::finalize(digest.0))))
}
fn current_fingerprint() -> Option<&'static str> {
    static FINGERPRINT: OnceLock<Option<String>> = OnceLock::new();
    FINGERPRINT.get_or_init(|| engine_fingerprint().ok()).as_deref()
}
/// Build-time only (`herbert precompile`): reads `input`, writes only `output`, creates no state.
pub fn precompile(input: &Path, output: &Path) -> Result<(), Diagnostic> {
    let bytes = std::fs::read(input).map_err(|_| plugin_error("PLUGIN_MISSING", "The plugin file to precompile cannot be read."))?;
    let engine = engine()?;
    let compiled = compiling(|| engine.precompile_module(&bytes))
        .map_err(|_| plugin_error("PLUGIN_INVALID", "The plugin is not valid WebAssembly for this runtime."))?;
    // Ship only modules the host would accept. These bytes were produced in this process.
    let module = unsafe { Module::deserialize(&engine, &compiled) }
        .map_err(|_| plugin_error("PLUGIN_INVALID", "The precompiled plugin could not be loaded back."))?;
    if module.imports().next().is_some() {
        return Err(plugin_error("PLUGIN_CAPABILITY_DENIED", "Plugins must have no ambient system, network, filesystem or environment imports."));
    }
    std::fs::write(output, &compiled).map_err(|_| plugin_error("OUTPUT_WRITE_FAILED", "Cannot write the precompiled plugin file."))
}

/// How a guest's code was obtained. `module_sha256` is the guest `.wasm` sha256, the hash run
/// journals record; precompiled code is used only when the package binds it to that hash.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LoadRecord {
    pub(crate) source: &'static str,
    pub(crate) module_sha256: String,
    pub(crate) reason: Option<&'static str>,
}

/// Herbert's own package. Precompiled code is native code outside the Wasm sandbox, so it is used
/// only from the `plugins` folder beside the running executable, where replacing it is no easier
/// than replacing herbert.exe, and only when package.json binds the guest and its `.cwasm` by sha256.
pub(crate) struct ShippedPackage {
    root: PathBuf,
    files: BTreeMap<String, String>,
    precompiled: BTreeMap<String, (String, String)>,
}
impl ShippedPackage {
    /// `plugin_root` and `executable_folder` must be canonical.
    fn open(plugin_root: &Path, executable_folder: &Path) -> Option<Self> {
        if std::fs::canonicalize(executable_folder.join("plugins")).ok()? != plugin_root {
            return None;
        }
        Self::read(executable_folder)
    }
    fn read(root: &Path) -> Option<Self> {
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("package.json")).ok()?).ok()?;
        let files = manifest["files"].as_array()?.iter()
            .filter_map(|file| Some((file["path"].as_str()?.to_owned(), file["sha256"].as_str()?.to_ascii_lowercase())))
            .collect();
        let precompiled = manifest["precompiledPlugins"].as_object()?.iter()
            .filter_map(|(wasm, entry)| Some((wasm.clone(), (entry["cwasm"].as_str()?.to_owned(), entry["engine"].as_str()?.to_owned()))))
            .collect();
        Some(Self { root: root.to_path_buf(), files, precompiled })
    }
    fn relative(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let parts = relative.components().map(|part| match part {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        }).collect::<Option<Vec<_>>>()?;
        Some(parts.join("/"))
    }
    /// An error is the reason to compile the `.wasm` instead. Never deserializes unverified bytes.
    fn module(&self, path: &Path, guest_sha256: &str, engine: &Engine) -> Result<Module, &'static str> {
        let wasm = self.relative(path).ok_or("notShipped")?;
        let (cwasm, fingerprint) = self.precompiled.get(&wasm).ok_or("notShipped")?;
        // The manifest may only name a .cwasm beside its own .wasm in the plugins folder.
        if !sibling_cwasm(&wasm, cwasm) {
            return Err("notShipped");
        }
        if self.files.get(&wasm).map(String::as_str) != Some(guest_sha256) {
            return Err("hashMismatch");
        }
        let expected = self.files.get(cwasm).ok_or("missing")?;
        if Some(fingerprint.as_str()) != current_fingerprint() {
            return Err("engineMismatch");
        }
        let location = self.root.join(cwasm);
        if std::fs::symlink_metadata(&location).map_or(true, |metadata| !metadata.is_file()) {
            return Err("missing");
        }
        // Read once, verify, then deserialize that same buffer.
        let bytes = std::fs::read(&location).map_err(|_| "missing")?;
        if &crate::storage::sha256(&bytes) != expected {
            return Err("hashMismatch");
        }
        // SAFETY: package.json binds these bytes to this guest's verified hash and this engine, and the
        // package folder is the running executable's own folder (see `ShippedPackage`).
        unsafe { Module::deserialize(engine, &bytes) }.map_err(|_| "engineMismatch")
    }
}

fn sibling_cwasm(wasm: &str, cwasm: &str) -> bool {
    let plain = |path: &str| path.starts_with("plugins/") && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':']));
    let parent = |path: &str| path.rsplit_once('/').map(|(parent, _)| parent.to_owned());
    plain(wasm) && plain(cwasm) && cwasm.ends_with(".cwasm") && parent(wasm) == parent(cwasm)
}
/// A descriptor reply saved after a live describe. It holds data, not code: a tampered entry can
/// only mislabel, and the first call compares it with the compiled module's live descriptor.
pub(crate) struct CachedDescriptor {
    file: PathBuf,
    reply: serde_json::Value,
    state: serde_json::Value,
}
#[derive(Clone)]
pub(crate) struct DescriptorCache {
    root: PathBuf,
}
impl DescriptorCache {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }
    /// Key: guest sha256, plugin-state sha256 and this host build.
    fn key(guest_sha256: &str, state: &serde_json::Value) -> Option<String> {
        static HOST: OnceLock<Option<String>> = OnceLock::new();
        let host = HOST.get_or_init(|| std::env::current_exe().ok().and_then(|exe| std::fs::read(exe).ok()).map(|bytes| crate::storage::sha256(&bytes))).as_deref()?;
        let state = crate::storage::sha256(&serde_json::to_vec(state).ok()?);
        Some(crate::storage::sha256(format!("{guest_sha256};{state};{host}").as_bytes()))
    }
    /// One entry per module path; a new guest version replaces it.
    fn file(&self, module: &Path) -> PathBuf {
        self.root.join(format!("{}.json", crate::storage::sha256(module.to_string_lossy().as_bytes())))
    }
    fn usable_folder(&self) -> bool {
        std::fs::symlink_metadata(&self.root).is_ok_and(|metadata| metadata.is_dir() && !reparse_point(&metadata))
    }
    fn read(&self, module: &Path, guest_sha256: &str, state: &serde_json::Value) -> Option<CachedDescriptor> {
        let key = Self::key(guest_sha256, state)?;
        if !self.usable_folder() {
            return None;
        }
        let file = self.file(module);
        let metadata = std::fs::symlink_metadata(&file).ok()?;
        if !metadata.is_file() || reparse_point(&metadata) {
            return None;
        }
        let entry: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).ok()?).ok()?;
        (entry["key"].as_str() == Some(key.as_str())).then(|| CachedDescriptor { file, reply: entry["reply"].clone(), state: state.clone() })
    }
    fn write(&self, module: &Path, guest_sha256: &str, state: &serde_json::Value, reply: &serde_json::Value) {
        if Self::key(guest_sha256, state).is_none() || std::fs::create_dir_all(&self.root).is_err() || !self.usable_folder() {
            return;
        }
        Self::replace(&self.file(module), guest_sha256, state, reply);
    }
    /// Remove a mismatching entry. If it cannot be removed, replace it with the live descriptor so
    /// the next listing does not refuse again.
    fn discard(file: &Path, guest_sha256: &str, state: &serde_json::Value, live: &serde_json::Value, remove: impl FnOnce(&Path) -> std::io::Result<()>) {
        // An entry that is already gone counts as removed; never recreate it.
        if remove(file).is_err_and(|error| error.kind() != std::io::ErrorKind::NotFound) {
            Self::replace(file, guest_sha256, state, live);
        }
    }
    /// A cache that cannot be written only costs a compile on the next launch.
    fn replace(file: &Path, guest_sha256: &str, state: &serde_json::Value, reply: &serde_json::Value) {
        let Some(key) = Self::key(guest_sha256, state) else { return };
        let _ = crate::storage::atomic_write(file, &serde_json::to_vec(&serde_json::json!({"key":key,"reply":reply})).unwrap_or_default());
    }
}
fn reparse_point(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
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
                "This plugin file is not compatible with this version of Herbert. Reinstall the plugin from the same release as Herbert.",
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
            "This plugin was built for a different version of Herbert. Reinstall the plugin from the same release as Herbert.",
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
    let len=u32::try_from(payload.len()).map_err(|_|plugin_error("PLUGIN_ADDRESS_SPACE","A plugin produced more data than a plugin can return at once. Lower the page size for this plugin, then retry."))?;
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
            "The plugin returned a damaged reply and was stopped. Retry; if it happens again, reinstall the plugin.",
        )
    })?;
    let data = memory.data(&store).get(out_ptr..end).ok_or_else(|| {
        plugin_error(
            "PLUGIN_MEMORY_INVALID",
            "The plugin returned a damaged reply and was stopped. Retry; if it happens again, reinstall the plugin.",
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
    load: LoadRecord,
}
impl Runtime {
    fn source_hash(bytes: &[u8]) -> String { crate::storage::sha256(bytes) }
    fn compile(bytes: &[u8]) -> Result<Self, Diagnostic> {
        Self::prepare(bytes, None, None)
    }
    /// Use the package's precompiled module when it is bound to these bytes; otherwise compile.
    fn prepare(bytes: &[u8], path: Option<&Path>, shipped: Option<&ShippedPackage>) -> Result<Self, Diagnostic> {
        let source_hash = Self::source_hash(bytes);
        let engine = engine()?;
        let precompiled = match (path, shipped) {
            (Some(path), Some(shipped)) => shipped.module(path, &source_hash, &engine),
            _ => Err("notShipped"),
        };
        let (module, load) = match precompiled {
            Ok(module) => (module, LoadRecord { source: "precompiled", module_sha256: source_hash.clone(), reason: None }),
            Err(reason) => (
                compiling(|| Module::new(&engine, bytes)).map_err(|_| plugin_error("PLUGIN_INVALID", "The plugin is not valid WebAssembly for this runtime."))?,
                LoadRecord { source: "compiled", module_sha256: source_hash.clone(), reason: Some(reason) },
            ),
        };
        // Precompiled modules get the same ambient-import check as compiled ones.
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
            load,
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
        .map_err(|_| plugin_error("PLUGIN_TASK_FAILED", "Herbert could not load this plugin file. Reinstall the plugin; if it still fails, the file may be damaged."))??;
    call_runtime(Arc::new(runtime), request, cancel).await
}
async fn read_state(state_file: &Option<Arc<crate::plugin_state::StateFile>>) -> Result<serde_json::Value, Diagnostic> {
    match state_file {
        Some(file) => {
            let file = file.clone();
            tokio::task::spawn_blocking(move || file.read())
                .await
                .map_err(|_| plugin_error("PLUGIN_STATE_UNAVAILABLE", "Module state reading failed."))?
        }
        None => Ok(serde_json::Value::Null),
    }
}
async fn prepare_runtime(bytes: Arc<Vec<u8>>, path: Option<PathBuf>, shipped: Option<Arc<ShippedPackage>>) -> Result<Arc<Runtime>, Diagnostic> {
    Ok(Arc::new(
        tokio::task::spawn_blocking(move || Runtime::prepare(&bytes, path.as_deref(), shipped.as_deref()))
            .await
            .map_err(|_| plugin_error("PLUGIN_TASK_FAILED", "Herbert could not load this plugin file. Reinstall the plugin; if it still fails, the file may be damaged."))??,
    ))
}
async fn describe_reply(runtime: Arc<Runtime>, state: &serde_json::Value, cancel: CancellationToken) -> Result<GuestReply, Diagnostic> {
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
    call_runtime(runtime, request, cancel).await
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
        let state_file = Some(Arc::new(crate::plugin_state::StateFile::for_module(path)));
        let state = read_state(&state_file).await?;
        let runtime = prepare_runtime(bytes.clone(), Some(path.to_path_buf()), None).await?;
        let reply = describe_reply(runtime.clone(), &state, CancellationToken::new()).await?;
        Self::from_descriptor(crate::storage::sha256(&bytes), reply, Arc::new(OnceCell::new_with(Some(runtime))), state_file)?
            .with_artifact_manifest(path)
            .await
    }
    /// Ensure the guest's code is loaded (compiled, or deserialized from the package) and report how.
    /// A descriptor taken from the descriptor cache is checked against the live one here, before any
    /// guest call. A mismatch discards the cache entry and refuses this use; the next listing reads
    /// the live descriptor.
    pub(crate) async fn prepare(&self, cancel: &CancellationToken) -> Result<serde_json::Value, Diagnostic> {
        let runtime = self.runtime(cancel).await?;
        let load = &runtime.load;
        Ok(serde_json::json!({"event":"pluginLoaded","category":self.category.id,"source":load.source,"moduleSha256":load.module_sha256,"reason":load.reason}))
    }
    async fn runtime(&self, cancel: &CancellationToken) -> Result<Arc<Runtime>, Diagnostic> {
        self.runtime.get_or_try_init(|| async {
            let path = self.module_path.clone().ok_or_else(|| plugin_error("PLUGIN_UNAVAILABLE", "An installed module cannot be read."))?;
            let bytes = Arc::new(tokio::fs::read(&path).await.map_err(|_| plugin_error("PLUGIN_UNAVAILABLE", "An installed module cannot be read."))?);
            if crate::storage::sha256(&bytes) != self.hash {
                return Err(plugin_error("PLUGIN_CHANGED", "A plugin file changed after Herbert listed it. Retry to use the current file."));
            }
            let runtime = prepare_runtime(bytes, Some(path), self.shipped.clone()).await?;
            if let Some(cached) = &self.cached_descriptor {
                let live = describe_reply(runtime.clone(), &cached.state, cancel.clone()).await?;
                let live = serde_json::to_value(&live).unwrap_or_default();
                if live != cached.reply {
                    let (file, guest, state) = (cached.file.clone(), self.hash.clone(), cached.state.clone());
                    let _ = tokio::task::spawn_blocking(move || DescriptorCache::discard(&file, &guest, &state, &live, |file| std::fs::remove_file(file))).await;
                    let mut error = plugin_error("PLUGIN_DESCRIPTOR_CHANGED", "A plugin described itself differently from Herbert's saved description. The saved description was discarded. Retry to use the plugin's current description.");
                    error.context.insert("category".into(), serde_json::json!(self.category.id));
                    return Err(error);
                }
            }
            Ok(runtime)
        }).await.cloned()
    }
    fn from_descriptor(
        hash: String,
        reply: GuestReply,
        runtime: Arc<OnceCell<Arc<Runtime>>>,
        state_file: Option<Arc<crate::plugin_state::StateFile>>,
    ) -> Result<Self, Diagnostic> {
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
                return Err(plugin_error("PLUGIN_DESCRIPTOR_INVALID", "Epicor route declarations must contain valid service and operation names."));
            }
            if binary_exports.iter().any(|d| !crate::connection::valid_segment(&d.service) || !crate::connection::valid_segment(&d.method) || !d.pointer.starts_with('/') || d.extension.is_empty() || !d.extension.chars().all(|c| c.is_ascii_alphanumeric())) {
                return Err(plugin_error("PLUGIN_DESCRIPTOR_INVALID", "This plugin's description of the files it downloads is invalid, so Herbert will not load it. Reinstall the plugin from this release."));
            }
            let mut policy_services = std::collections::BTreeSet::new();
            if response_policies.iter().any(|policy| !methods.iter().any(|(service, _)| service == &policy.service) || !policy_services.insert(&policy.service) || !crate::response_policy::valid(policy)) {
                return Err(plugin_error("PLUGIN_DESCRIPTOR_INVALID", "A response policy must name one declared Epicor service and valid field rules."));
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
                hash,
                runtime,
                methods,
                entities,
                binary_exports,
                response_policies,
                state_file,
                module_path: None,
                shipped: None,
                cached_descriptor: None,
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
        let runtime = self.runtime(&cancel).await?;
        let mut reply = call_runtime(runtime, request, cancel).await?;
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
    descriptors: Option<DescriptorCache>,
}
pub async fn discover(root: &Path) -> Result<(Vec<Plugin>, Vec<Diagnostic>), Diagnostic> {
    FolderCache::default()
        .scan(root, &CancellationToken::new())
        .await
}
impl FolderCache {
    /// Listing uses saved descriptors and compiles a guest only when it is first used.
    pub(crate) fn with_descriptor_cache(root: PathBuf) -> Self {
        Self { current: BTreeMap::new(), descriptors: Some(DescriptorCache::new(root)) }
    }
    /// Forget loaded guest code; each plugin loads it again on next use (`Plugin::runtime`).
    /// Plugin clones held elsewhere, such as by an executing run, keep their own runtime.
    pub(crate) fn unload_modules(&mut self) {
        for plugin in self.current.values_mut() {
            plugin.runtime = Arc::new(OnceCell::new());
        }
    }
    /// Cached plugins whose guest code is currently loaded.
    #[cfg(test)]
    pub(crate) fn loaded_modules(&self) -> usize {
        self.current.values().filter(|plugin| plugin.runtime.get().is_some()).count()
    }
    async fn read_plugin(
        &self,
        path: &Path,
        shipped: Option<Arc<ShippedPackage>>,
        cancel: &CancellationToken,
    ) -> Result<Plugin, Diagnostic> {
        let bytes = Arc::new(tokio::fs::read(path).await.map_err(|_| {
            plugin_error("PLUGIN_UNAVAILABLE", "An installed module cannot be read.")
        })?);
        // Reuse compiled modules only while their portable Wasm bytes match.
        let hash = crate::storage::sha256(&bytes);
        let source_hash = Runtime::source_hash(&bytes);
        let matches_source = |plugin: &&Plugin| {
            plugin.hash == hash && plugin.runtime.get().is_some_and(|runtime| runtime.source_hash == source_hash)
        };
        let runtime = self
            .current
            .get(path)
            .filter(matches_source)
            .or_else(|| self.current.values().find(matches_source))
            .and_then(|plugin| plugin.runtime.get().cloned());
        let state_file = Some(Arc::new(crate::plugin_state::StateFile::for_module(path)));
        let state = read_state(&state_file).await?;
        let cached = match (&runtime, &self.descriptors) {
            (None, Some(cache)) => {
                let (cache, module, guest, state) = (cache.clone(), path.to_path_buf(), hash.clone(), state.clone());
                tokio::task::spawn_blocking(move || cache.read(&module, &guest, &state)).await.ok().flatten()
            }
            _ => None,
        };
        let mut plugin = match (runtime, cached) {
            (Some(runtime), _) => {
                let reply = describe_reply(runtime.clone(), &state, cancel.clone()).await?;
                Plugin::from_descriptor(hash, reply, Arc::new(OnceCell::new_with(Some(runtime))), state_file)?
            }
            (None, Some(cached)) => match serde_json::from_value::<GuestReply>(cached.reply.clone()) {
                Ok(reply) => {
                    let mut plugin = Plugin::from_descriptor(hash, reply, Arc::new(OnceCell::new()), state_file)?;
                    plugin.cached_descriptor = Some(Arc::new(cached));
                    plugin
                }
                Err(_) => self.describe_live(path, bytes, hash, shipped.clone(), state_file, &state, cancel).await?,
            },
            (None, None) => self.describe_live(path, bytes, hash, shipped.clone(), state_file, &state, cancel).await?,
        };
        plugin.shipped = shipped;
        plugin.with_artifact_manifest(path).await
    }
    #[allow(clippy::too_many_arguments)]
    async fn describe_live(
        &self,
        path: &Path,
        bytes: Arc<Vec<u8>>,
        hash: String,
        shipped: Option<Arc<ShippedPackage>>,
        state_file: Option<Arc<crate::plugin_state::StateFile>>,
        state: &serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<Plugin, Diagnostic> {
        let runtime = prepare_runtime(bytes, Some(path.to_path_buf()), shipped).await?;
        let reply = describe_reply(runtime.clone(), state, cancel.clone()).await?;
        let saved = serde_json::to_value(&reply).ok();
        let plugin = Plugin::from_descriptor(hash.clone(), reply, Arc::new(OnceCell::new_with(Some(runtime))), state_file)?;
        if let (Some(cache), Some(saved)) = (&self.descriptors, saved) {
            let (cache, module, state) = (cache.clone(), path.to_path_buf(), state.clone());
            let _ = tokio::task::spawn_blocking(move || cache.write(&module, &hash, &state, &saved)).await;
        }
        Ok(plugin)
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
        let shipped = {
            let root = root.clone();
            tokio::task::spawn_blocking(move || {
                let folder = std::fs::canonicalize(std::env::current_exe().ok()?.parent()?).ok()?;
                ShippedPackage::open(&root, &folder)
            })
            .await
            .ok()
            .flatten()
            .map(Arc::new)
        };
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
            let result = self.read_plugin(&path, shipped.clone(), cancel).await;
            if let Ok(plugin) = &result {
                self.current.insert(path.clone(), plugin.clone());
            } else {
                self.current.remove(&path);
            }
            match result {
                Ok(plugin) if crate::legacy_category_ids::refuse_plugin_id(&plugin.category.id).is_err() => {
                    let mut error = crate::legacy_category_ids::refuse_plugin_id(&plugin.category.id).unwrap_err();
                    error.context.insert("file".into(), serde_json::json!(path.file_name().and_then(|n| n.to_str()).unwrap_or("")));
                    diagnostics.push(error);
                }
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
                        diagnostics.push(plugin_error("PLUGIN_ID_CONFLICT","Two installed packages claim the same category ID with different code, declarations or Epicor artifact files; neither can own that ambiguous identity."));
                    } else {
                        loaded.push(plugin);
                    }
                }
                Err(error) => diagnostics.push(error),
            }
        }
        // Capabilities and the GUI list plugins in the established catalog order.
        loaded.sort_by_key(|plugin| crate::legacy_category_ids::catalog_order(&plugin.category.id));
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
            category: "ud-codes".into(),
            company: String::new(),
            identity: None,
            cursor: None,
            page_size: None,
            state: serde_json::Value::Null,
            response: None,
            error: None,
        }
    }
    fn descriptor_module(name: &str) -> Vec<u8> {
        let descriptor = serde_json::to_vec(&serde_json::json!({"type":"descriptor", "abi":1, "category":{"id":"example-native", "name":name, "description":"fixture", "pluginVersion":"1", "available":true, "qualification":"fixture", "roles":["collector"]}, "methods":[["Ice.BO.DynamicQuerySvc","Execute"]]})).unwrap();
        let literal = descriptor.iter().map(|b| format!("\\{b:02x}")).collect::<String>();
        wat::parse_str(format!(r#"(module (memory (export "memory") 1) (data (i32.const 4096) "{literal}") (func (export "hoover_abi") (result i32) i32.const 1) (func (export "hoover_alloc") (param i32) (result i32) i32.const 0) (func (export "hoover_free") (param i32 i32)) (func (export "hoover_call") (param i32 i32) (result i64) i64.const {}))"#, (4096_u64 << 32) | descriptor.len() as u64)).unwrap()
    }
    async fn scan_one(cache: &mut FolderCache, root: &Path) -> Plugin {
        let (plugins, errors) = cache.scan(root, &CancellationToken::new()).await.unwrap();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(plugins.len(), 1);
        plugins.into_iter().next().unwrap()
    }
    #[test]
    fn plugin_compile_runs_in_bounded_pool_and_leaves_ui_cores() {
        assert_eq!(compile_threads(16), 14);
        assert_eq!(compile_threads(4), 2);
        for small in [0, 1, 2, 3] {
            assert_eq!(compile_threads(small), 1);
        }
        let available = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(compiling(rayon::current_num_threads), compile_threads(available));
        assert!(compiling(|| rayon::current_thread_index().is_some()), "compilation must run inside the bounded pool");
    }
    #[test]
    fn engine_fingerprint_names_wasmtime_target_and_compatibility_hash_only() {
        let lock = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock")).unwrap();
        assert!(lock.contains(&format!("name = \"wasmtime\"\nversion = \"{WASMTIME_VERSION}\"\n")), "WASMTIME_VERSION must equal Cargo.lock");
        let fingerprint = engine_fingerprint().unwrap();
        assert_eq!(fingerprint, engine_fingerprint().unwrap());
        let parts = fingerprint.split(';').collect::<Vec<_>>();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], format!("wasmtime-{WASMTIME_VERSION}"));
        assert_eq!(parts[1], env!("HERBERT_NATIVE_TARGET"));
        assert!(parts[2].len() == 64 && parts[2].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(!fingerprint.contains(['\\', '/', ':', ' ']), "{fingerprint}");
    }
    #[tokio::test]
    async fn descriptor_cache_hit_skips_compile_and_matches_fresh_descriptor() {
        let folder = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let plugins = folder.path().join("plugins");
        std::fs::create_dir_all(plugins.join("Example")).unwrap();
        std::fs::write(plugins.join("Example/Example.wasm"), descriptor_module("Example")).unwrap();
        let fresh = scan_one(&mut FolderCache::with_descriptor_cache(state.path().to_path_buf()), &plugins).await;
        assert!(fresh.runtime.get().is_some(), "a cache miss compiles and describes live");
        assert_eq!(std::fs::read_dir(state.path()).unwrap().count(), 1);
        // A new process: the descriptor comes from the cache and nothing is compiled.
        let cached = scan_one(&mut FolderCache::with_descriptor_cache(state.path().to_path_buf()), &plugins).await;
        assert!(cached.runtime.get().is_none());
        assert_eq!(serde_json::to_value(&cached.category).unwrap(), serde_json::to_value(&fresh.category).unwrap());
        assert_eq!((&cached.hash, &cached.methods), (&fresh.hash, &fresh.methods));
        let event = cached.prepare(&CancellationToken::new()).await.unwrap();
        assert_eq!(event, serde_json::json!({"event":"pluginLoaded","category":"example-native","source":"compiled","moduleSha256":fresh.hash,"reason":"notShipped"}));
        assert!(cached.runtime.get().is_some());
        assert!(cached.call(request(), CancellationToken::new()).await.is_ok());
    }
    #[tokio::test]
    async fn descriptor_cache_misses_on_any_byte_or_state_change() {
        let folder = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let plugins = folder.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let module = plugins.join("Example.wasm");
        std::fs::write(&module, descriptor_module("First")).unwrap();
        scan_one(&mut FolderCache::with_descriptor_cache(state.path().to_path_buf()), &plugins).await;
        std::fs::write(&module, descriptor_module("Second")).unwrap();
        let changed = scan_one(&mut FolderCache::with_descriptor_cache(state.path().to_path_buf()), &plugins).await;
        assert!(changed.runtime.get().is_some(), "changed bytes must compile");
        assert_eq!(changed.category.name, "Second");
        let cache = DescriptorCache::new(state.path().to_path_buf());
        let module = std::fs::canonicalize(&module).unwrap();
        let state_value = serde_json::json!({"saved":"a"});
        assert!(cache.read(&module, &changed.hash, &serde_json::Value::Null).is_some());
        assert!(cache.read(&module, &changed.hash, &state_value).is_none(), "a different plugin state must miss");
        assert_ne!(DescriptorCache::key(&changed.hash, &serde_json::Value::Null), DescriptorCache::key(&changed.hash, &state_value));
        assert!(cache.read(&module, &"0".repeat(64), &serde_json::Value::Null).is_none(), "a different guest hash must miss");
    }
    #[tokio::test]
    async fn tampered_descriptor_cache_entry_is_discarded_on_first_use() {
        let folder = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let plugins = folder.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join("Example.wasm"), descriptor_module("Genuine")).unwrap();
        scan_one(&mut FolderCache::with_descriptor_cache(state.path().to_path_buf()), &plugins).await;
        let entry = std::fs::read_dir(state.path()).unwrap().next().unwrap().unwrap().path();
        let mut saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&entry).unwrap()).unwrap();
        saved["reply"]["category"]["name"] = serde_json::json!("Mislabelled");
        std::fs::write(&entry, serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut cache = FolderCache::with_descriptor_cache(state.path().to_path_buf());
        let tampered = scan_one(&mut cache, &plugins).await;
        assert_eq!(tampered.category.name, "Mislabelled");
        let refused = tampered.prepare(&CancellationToken::new()).await.unwrap_err();
        assert_eq!(refused.code, "PLUGIN_DESCRIPTOR_CHANGED");
        assert_eq!(refused.context["category"], "example-native");
        assert_eq!(tampered.call(request(), CancellationToken::new()).await.unwrap_err().code, "PLUGIN_DESCRIPTOR_CHANGED");
        assert!(!entry.exists(), "the mismatching entry is discarded");
        assert_eq!(scan_one(&mut cache, &plugins).await.category.name, "Genuine");
    }
    #[test]
    fn mismatching_entry_that_cannot_be_removed_is_replaced_with_the_live_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let cache = DescriptorCache::new(root.path().to_path_buf());
        let module = root.path().join("Example.wasm");
        let guest = "c".repeat(64);
        cache.write(&module, &guest, &serde_json::Value::Null, &serde_json::json!({"name":"Mislabelled"}));
        let file = cache.file(&module);
        let live = serde_json::json!({"name":"Genuine"});
        DescriptorCache::discard(&file, &guest, &serde_json::Value::Null, &live, |_| Err(std::io::Error::other("held open")));
        assert_eq!(cache.read(&module, &guest, &serde_json::Value::Null).unwrap().reply, live);
        DescriptorCache::discard(&file, &guest, &serde_json::Value::Null, &live, |file| std::fs::remove_file(file));
        assert!(!file.exists(), "a removable entry is removed");
    }
    #[test]
    fn manifest_cwasm_must_be_a_sibling_of_its_wasm_in_the_plugins_folder() {
        assert!(sibling_cwasm("plugins/Example/Example.wasm", "plugins/Example/Example.cwasm"));
        for cwasm in ["plugins/Other/Example.cwasm", "plugins/Example/../Other/Example.cwasm", "herbert.cwasm", "plugins/Example/Example.dll",
            "plugins/Example/sub/Example.cwasm", "plugins/Example\\Example.cwasm", "C:/plugins/Example/Example.cwasm", "plugins//Example.cwasm"] {
            assert!(!sibling_cwasm("plugins/Example/Example.wasm", cwasm), "{cwasm}");
        }
    }
    #[test]
    fn shipped_cwasm_outside_its_wasm_folder_is_not_loaded() {
        let (_guard, root, wasm, mut manifest) = shipped_fixture();
        std::fs::create_dir_all(root.join("plugins/Other")).unwrap();
        std::fs::copy(root.join("plugins/Example/Example.cwasm"), root.join("plugins/Other/Example.cwasm")).unwrap();
        manifest["files"][1]["path"] = serde_json::json!("plugins/Other/Example.cwasm");
        manifest["precompiledPlugins"]["plugins/Example/Example.wasm"]["cwasm"] = serde_json::json!("plugins/Other/Example.cwasm");
        let load = load_with(&root, &wasm, &manifest);
        assert_eq!((load.source, load.reason), ("compiled", Some("notShipped")));
    }
    #[test]
    fn cache_path_symlink_or_reparse_point_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let module = root.path().join("Example.wasm");
        let real = root.path().join("real");
        let cache = DescriptorCache::new(real.clone());
        cache.write(&module, &"a".repeat(64), &serde_json::Value::Null, &serde_json::json!({"type":"descriptor"}));
        assert!(cache.read(&module, &"a".repeat(64), &serde_json::Value::Null).is_some());
        let not_a_folder = root.path().join("file");
        std::fs::write(&not_a_folder, b"x").unwrap();
        assert!(DescriptorCache::new(not_a_folder).read(&module, &"a".repeat(64), &serde_json::Value::Null).is_none());
        // A directory junction is a reparse point that needs no symbolic-link privilege.
        fn link_folder(target: &Path, link: &Path) {
            #[cfg(windows)]
            assert!(std::process::Command::new("cmd").args(["/C", "mklink", "/J"]).arg(link).arg(target).output().unwrap().status.success());
            #[cfg(not(windows))]
            std::os::unix::fs::symlink(target, link).unwrap();
        }
        let link = root.path().join("link");
        link_folder(&real, &link);
        let through_link = DescriptorCache::new(link.clone());
        assert!(through_link.read(&module, &"a".repeat(64), &serde_json::Value::Null).is_none(), "a linked cache folder is refused");
        through_link.write(&module, &"b".repeat(64), &serde_json::Value::Null, &serde_json::json!({}));
        assert!(cache.read(&module, &"b".repeat(64), &serde_json::Value::Null).is_none(), "nothing is written through a link");
        let entry = cache.file(&module);
        std::fs::remove_file(&entry).unwrap();
        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        link_folder(&elsewhere, &entry);
        assert!(cache.read(&module, &"a".repeat(64), &serde_json::Value::Null).is_none(), "a linked entry is refused");
    }
    fn shipped_fixture() -> (tempfile::TempDir, PathBuf, Vec<u8>, serde_json::Value) {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("plugins/Example");
        std::fs::create_dir_all(&folder).unwrap();
        let wasm = descriptor_module("Shipped");
        std::fs::write(folder.join("Example.wasm"), &wasm).unwrap();
        precompile(&folder.join("Example.wasm"), &folder.join("Example.cwasm")).unwrap();
        let cwasm = std::fs::read(folder.join("Example.cwasm")).unwrap();
        let manifest = serde_json::json!({"files":[
            {"path":"plugins/Example/Example.wasm","bytes":wasm.len(),"sha256":crate::storage::sha256(&wasm)},
            {"path":"plugins/Example/Example.cwasm","bytes":cwasm.len(),"sha256":crate::storage::sha256(&cwasm)}],
            "precompiledPlugins":{"plugins/Example/Example.wasm":{"cwasm":"plugins/Example/Example.cwasm","engine":engine_fingerprint().unwrap()}}});
        let root_path = std::fs::canonicalize(root.path()).unwrap();
        (root, root_path, wasm, manifest)
    }
    fn load_with(root: &Path, wasm: &[u8], manifest: &serde_json::Value) -> LoadRecord {
        std::fs::write(root.join("package.json"), serde_json::to_vec(manifest).unwrap()).unwrap();
        let package = ShippedPackage::read(root).unwrap();
        Runtime::prepare(wasm, Some(&root.join("plugins/Example/Example.wasm")), Some(&package)).unwrap().load
    }
    #[tokio::test]
    async fn shipped_cwasm_loads_only_when_the_package_binds_guest_module_and_engine() {
        let (_guard, root, wasm, manifest) = shipped_fixture();
        let load = load_with(&root, &wasm, &manifest);
        assert_eq!((load.source, load.reason, load.module_sha256.as_str()), ("precompiled", None, crate::storage::sha256(&wasm).as_str()));
        std::fs::write(root.join("package.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        let package = ShippedPackage::read(&root).unwrap();
        let runtime = Arc::new(Runtime::prepare(&wasm, Some(&root.join("plugins/Example/Example.wasm")), Some(&package)).unwrap());
        match describe_reply(runtime, &serde_json::Value::Null, CancellationToken::new()).await.unwrap() {
            GuestReply::Descriptor { category, .. } => assert_eq!(category.name, "Shipped"),
            _ => panic!("the precompiled guest must answer its descriptor"),
        }
        // The package folder must be the running executable's own folder.
        let plugins = std::fs::canonicalize(root.join("plugins")).unwrap();
        assert!(ShippedPackage::open(&plugins, &root).is_some());
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(ShippedPackage::open(&plugins, &std::fs::canonicalize(elsewhere.path()).unwrap()).is_none());
    }
    #[test]
    fn shipped_cwasm_with_wrong_package_hash_is_ignored() {
        let (_guard, root, wasm, manifest) = shipped_fixture();
        let reason = |manifest: &serde_json::Value| { let load = load_with(&root, &wasm, manifest); assert_eq!(load.source, "compiled"); load.reason };
        let mut wrong_cwasm = manifest.clone();
        wrong_cwasm["files"][1]["sha256"] = serde_json::json!("0".repeat(64));
        assert_eq!(reason(&wrong_cwasm), Some("hashMismatch"));
        let mut wrong_guest = manifest.clone();
        wrong_guest["files"][0]["sha256"] = serde_json::json!("0".repeat(64));
        assert_eq!(reason(&wrong_guest), Some("hashMismatch"));
        let mut other_engine = manifest.clone();
        other_engine["precompiledPlugins"]["plugins/Example/Example.wasm"]["engine"] = serde_json::json!("wasmtime-0.0.0;other;00");
        assert_eq!(reason(&other_engine), Some("engineMismatch"));
        let mut unlisted = manifest.clone();
        unlisted["files"].as_array_mut().unwrap().pop();
        assert_eq!(reason(&unlisted), Some("missing"));
        let mut unmapped = manifest.clone();
        unmapped["precompiledPlugins"] = serde_json::json!({});
        assert_eq!(reason(&unmapped), Some("notShipped"));
        // A .cwasm altered after packaging fails its hash before any deserialization.
        std::fs::write(root.join("plugins/Example/Example.cwasm"), b"not a module").unwrap();
        assert_eq!(reason(&manifest), Some("hashMismatch"));
        std::fs::remove_file(root.join("plugins/Example/Example.cwasm")).unwrap();
        assert_eq!(reason(&manifest), Some("missing"));
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
    async fn plugins_share_one_engine_and_cancellation_stops_only_its_own_guest() {
        let guest=wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "hoover_abi") (result i32) i32.const 1) (func (export "hoover_alloc") (param i32) (result i32) i32.const 0) (func (export "hoover_free") (param i32 i32)) (func (export "hoover_call") (param i32 i32) (result i64) (loop br 0) i64.const 0))"#).unwrap();
        let (first, second) = (Arc::new(Runtime::compile(&guest).unwrap()), Arc::new(Runtime::compile(&descriptor_module("Other")).unwrap()));
        assert!(Engine::same(&first.engine, &second.engine));
        assert!(Engine::same(&first.engine, &engine().unwrap()));
        // The shared engine keeps the configuration that shipped modules are bound to.
        assert_eq!(engine_fingerprint().unwrap(), current_fingerprint().unwrap());
        let (stop_first, stop_second) = (CancellationToken::new(), CancellationToken::new());
        let a = tokio::spawn(call_runtime(first.clone(), request(), stop_first.clone()));
        let b = tokio::spawn(call_runtime(first, request(), stop_second.clone()));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        stop_first.cancel();
        let wait = std::time::Duration::from_secs(30);
        assert_eq!(tokio::time::timeout(wait, a).await.unwrap().unwrap().err().unwrap().code, "CANCELED");
        // Another plugin's calls complete and bump the shared epoch; the second guest keeps running.
        assert!(call_runtime(second, request(), CancellationToken::new()).await.is_ok());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!b.is_finished(), "an epoch bump from another operation must not stop this guest");
        stop_second.cancel();
        assert_eq!(tokio::time::timeout(wait, b).await.unwrap().unwrap().err().unwrap().code, "CANCELED");
    }
    #[tokio::test]
    async fn unloaded_modules_reload_on_next_use() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(folder.path().join("Example")).unwrap();
        std::fs::write(folder.path().join("Example/Example.wasm"), descriptor_module("Example")).unwrap();
        let mut cache = FolderCache::default();
        let loaded = scan_one(&mut cache, folder.path()).await;
        let before = loaded.runtime.get().cloned().unwrap();
        cache.unload_modules();
        // The executing holder keeps its runtime; the cache's plugins load again on use.
        assert!(Arc::ptr_eq(loaded.runtime.get().unwrap(), &before));
        let listed = cache.current.values().next().unwrap().clone();
        assert!(listed.runtime.get().is_none());
        assert_eq!(listed.prepare(&CancellationToken::new()).await.unwrap()["event"], "pluginLoaded");
        assert!(listed.call(request(), CancellationToken::new()).await.is_ok());
        drop((loaded, before));
        let rescanned = scan_one(&mut cache, folder.path()).await;
        assert!(rescanned.call(request(), CancellationToken::new()).await.is_ok());
    }
    #[tokio::test]
    async fn plugin_declares_new_native_methods_without_host_registration() {
        let descriptor = serde_json::to_vec(&GuestReply::Descriptor {
            binary_exports: vec![],
            response_policies: vec![],
            entities: vec![],
            abi: 1,
            category: Category {
                id: "functions".into(),
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
        assert!(Arc::ptr_eq(first[0].runtime.get().unwrap(), second[0].runtime.get().unwrap()));
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
