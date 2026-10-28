//! Live operations and diagnostic events. Polling never writes a checkpoint.
use herbert_contract::{HostAction, Identity};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    sync::{Arc, Mutex},
    time::Instant,
};
// Scope transport diagnostics to the current run without sharing credentials or
// changing a connection used by another task. Cache hits do not enter this scope.
tokio::task_local! {
    pub static TRANSPORT_LOG: (Arc<Activity>, String, String);
}

pub fn record_transport(attempt: u32, elapsed_ms: u128, code: Option<&str>, delay: Option<u128>) {
    let _ = TRANSPORT_LOG.try_with(|(activity, service, method)|
        activity.transport_attempt(service, method, attempt, elapsed_ms, code, delay));
}

#[derive(Default)]
pub struct Activity {
    meaningful: std::sync::atomic::AtomicU64,
    pending_compiled_pages: std::sync::atomic::AtomicU64,
    managed_jobs: Mutex<BTreeMap<String,String>>,
    work_timings: Mutex<BTreeMap<String, BTreeMap<String, (u64, u128)>>>,
    metrics: Mutex<crate::activity_metrics::Metrics>,
    entries: Mutex<BTreeMap<String, Entry>>,
    plugin_states: Mutex<BTreeMap<String, String>>,
    stages: Mutex<Vec<String>>,
    batches: Mutex<Vec<Value>>,
    pub(crate) progress_summary: Mutex<Option<Value>>,
    diagnostics: Mutex<Option<DiagnosticLog>>,
}
/// One page whose optional compiled capture/enrollment has not settled yet.
pub(crate) struct CompiledPage<'a>(&'a Activity);
impl Drop for CompiledPage<'_>{fn drop(&mut self){self.0.pending_compiled_pages.fetch_sub(1,std::sync::atomic::Ordering::Relaxed);}}
/// Measure one named operation, including its wait for a local worker.
/// These durations can overlap across producer and consumer tasks.
pub struct WorkTimer<'a> {
    activity: &'a Activity,
    category: String,
    operation: &'static str,
    started: Instant,
}
impl Drop for WorkTimer<'_> {
    fn drop(&mut self) {
        let micros = self.started.elapsed().as_micros();
        {
            let mut timings = self.activity.work_timings.lock().unwrap();
            let entry = timings.entry(self.category.clone()).or_default()
                .entry(self.operation.into()).or_default();
            entry.0 += 1;
            entry.1 += micros;
        }
        self.activity.record(json!({"event":"workCompleted","category":self.category,
            "operation":self.operation,"durationMicros":micros}));
    }
}
struct DiagnosticLog {
    file: File,
    path: String,
    started: Instant,
    failed: bool,
}
struct Entry {
    plugin: String,
    identity: Option<Identity>,
    step: String,
    started: Instant,
}
pub struct Guard {
    activity: Arc<Activity>,
    id: String,
}
// A batch begins only after the transfer gate is acquired. Waiting item tasks
// and cache hits must not create batch rows. Keep finished rows for fast reads.
pub struct BatchGuard {
    activity: Arc<Activity>,
    index: usize,
    complete: bool,
}
impl BatchGuard {
    /// Ready groups belong to the same native page. Add them without restarting
    /// progress for records that the output workers have already saved.
    pub fn records_ready(&self, items: usize) {
        let mut batches = self.activity.batches.lock().unwrap();
        let row = &mut batches[self.index];
        let total = row["files"].as_u64().unwrap_or(0) + items as u64;
        row["files"] = json!(total);
        row["verified"] = json!(total);
        row["countKnown"] = json!(true);
        row["phase"] = json!("Collecting and writing files");
    }
    /// The server determines the actual page size. Do not show a guessed total.
    pub fn page_received(&self, items: usize) {
        let mut batches = self.activity.batches.lock().unwrap();
        let row = &mut batches[self.index];
        row["files"] = json!(items);
        row["verified"] = json!(items);
        row["countKnown"] = json!(true);
        row["phase"] = json!("Writing files");
        row["processed"] = json!(0);
    }
    /// Keep visible totals at the last completed batch while files are written.
    pub fn progress_scope(&self, category: &str, completed: usize) {
        let mut batches = self.activity.batches.lock().unwrap();
        batches[self.index]["category"] = json!(category);
        batches[self.index]["completedItems"] = json!(completed);
    }
    pub fn processed(&self, count: usize) {
        let mut batches = self.activity.batches.lock().unwrap();
        batches[self.index]["phase"] = json!("Processing");
        let previous=batches[self.index]["processed"].as_u64().unwrap_or(0) as usize;
        batches[self.index]["processed"] = json!(count.max(previous));
        self.activity.meaningful_work("recordsSaved",count.saturating_sub(previous) as u64);
    }
    pub fn processing_complete(mut self, completed: usize) {
        let mut batches = self.activity.batches.lock().unwrap();
        let row = &mut batches[self.index];
        // Keep indices stable for other active guards. An empty check is not a
        // completed batch and must not trigger progress or a completion sound.
        if row["files"].as_u64() == Some(0) {
            row["phase"] = json!("Checked");
            self.complete = true;
            return;
        }
        row["phase"] = json!("Processed");
        row["processed"] = row["files"].clone();
        row["completedItems"] = json!(completed);
        self.complete = true;
        self.activity.record(json!({"event":"batchProcessed","batch":self.index + 1,"summary":row}));
    }
    pub fn update(&self, phase: &str, verified: usize) {
        let mut batches = self.activity.batches.lock().unwrap();
        batches[self.index]["phase"] = json!(phase);
        batches[self.index]["verified"] = json!(verified);
    }
    pub fn complete(mut self) {
        let mut batches = self.activity.batches.lock().unwrap();
        let row = &mut batches[self.index];
        row["phase"] = json!("Verified");
        row["verified"] = row["files"].clone();
        self.complete = true;
    }
}
impl Drop for BatchGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.activity.batches.lock().unwrap()[self.index]["phase"] = json!("Interrupted");
        }
    }
}
impl Activity {
    pub(crate) fn compiled_page(&self)->CompiledPage<'_>{self.pending_compiled_pages.fetch_add(1,std::sync::atomic::Ordering::Relaxed);CompiledPage(self)}
    pub(crate) fn meaningful_work(&self, kind: &str, count:u64) {
        if count==0{return;}
        self.meaningful.fetch_add(count,std::sync::atomic::Ordering::Relaxed);
        self.record(json!({"event":"meaningfulWork","kind":kind,"count":count}));
    }
    pub(crate) fn managed_job(&self,id:&str,state:&str) {
        self.managed_jobs.lock().unwrap().insert(id.into(),state.into());
        self.meaningful_work("decompilerTransition",1);
        self.record(json!({"event":"decompilerJob","job":id,"state":state}));
    }

    /// Keep scheduler state visible between requests and during local writes.
    /// An empty request list does not mean that a plugin has finished.
    pub fn plugin_state(&self, category: &str, state: &str) {
        self.metrics.lock().unwrap().plugin(category, state);
        self.plugin_states.lock().unwrap().insert(category.to_owned(), state.to_owned());
        self.record(json!({"event":"pluginStateChanged","category":category,"state":state}));
    }

    pub fn plugin_states(&self) -> Value {
        json!(*self.plugin_states.lock().unwrap())
    }

    /// Each execution gets its own log. Recovery preserves the earlier log.
    pub fn for_run(storage: &crate::storage::Storage) -> Result<Self, herbert_contract::Diagnostic> {
        let relative = format!("Herbert Data/diagnostics/{}-{}.jsonl",
            storage.journal.run_id, uuid::Uuid::new_v4());
        let path = storage.confined(&relative)?;
        let unavailable = || herbert_contract::Diagnostic::new(
            "DIAGNOSTICS_UNAVAILABLE", "Cannot create the run diagnostic log.");
        std::fs::create_dir_all(path.parent().ok_or_else(unavailable)?)
            .map_err(|_| unavailable())?;
        let path = storage.confined(&relative)?;
        let file = File::options().write(true).create_new(true).open(&path)
            .map_err(|_| unavailable())?;
        let activity = Self { diagnostics: Mutex::new(Some(DiagnosticLog {
            file, path: relative, started: Instant::now(), failed: false,
        })), ..Self::default() };
        activity.record(json!({"event":"executionStarted","runId":storage.journal.run_id,
            "hostVersion":env!("CARGO_PKG_VERSION")}));
        Ok(activity)
    }

    // Call only with selected metadata. Never record response bodies, credentials,
    // request arguments or server error text. Polls do not call this method.
    pub(crate) fn record(&self, mut event: Value) {
        let mut log = self.diagnostics.lock().unwrap();
        let Some(log) = log.as_mut() else { return; };
        if log.failed { return; }
        event["elapsedMs"] = json!(log.started.elapsed().as_millis());
        event["unixMs"] = json!(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
        let result = serde_json::to_vec(&event).and_then(|mut bytes| {
            bytes.push(b'\n');
            log.file.write_all(&bytes).map_err(serde_json::Error::io)
        });
        log.failed = result.is_err();
    }

    pub fn transport_attempt(&self, service: &str, method: &str, attempt: u32,
        elapsed_ms: u128, code: Option<&str>, retry_delay_ms: Option<u128>) {
        self.record(json!({"event":"transportAttempt","route":[service,method],
            "attempt":attempt,"requestElapsedMs":elapsed_ms,"failureCode":code,
            "retryDelayMs":retry_delay_ms}));
    }

    pub fn work(&self, category: &str, operation: &'static str) -> WorkTimer<'_> {
        WorkTimer { activity: self, category: category.into(), operation, started: Instant::now() }
    }
    pub fn statistics(&self) -> Value {
        let mut value = self.metrics.lock().unwrap().snapshot();
        let timings = self.work_timings.lock().unwrap();
        let work: BTreeMap<_, BTreeMap<_, _>> = timings.iter().map(|(category, operations)| {
            (category, operations.iter().map(|(name, (calls, micros))| {
                (name, json!({"calls":calls,"durationMicros":micros}))
            }).collect())
        }).collect();
        value["workTimings"] = json!(work);
        value["meaningfulWork"] = json!(self.meaningful.load(std::sync::atomic::Ordering::Relaxed));
        let mut jobs=BTreeMap::<String,usize>::new();
        for state in self.managed_jobs.lock().unwrap().values(){*jobs.entry(state.clone()).or_default()+=1;}
        value["decompilerJobs"]=json!(jobs);
        value["pendingCompiledPages"]=json!(self.pending_compiled_pages.load(std::sync::atomic::Ordering::Relaxed));
        value
    }
    pub fn buffer_page(&self, category: &str, bytes: u64, disk: bool, queued: bool) {
        self.metrics.lock().unwrap().buffer(category, bytes, disk, queued);
        self.record(json!({"event":if queued {"pageQueued"} else {"pageDequeued"},
            "category":category,"estimatedBytes":bytes,"storage":if disk {"disk"} else {"memory"}}));
    }
    pub fn buffer_label(&self, category: &str) -> String { self.metrics.lock().unwrap().buffer_label(category) }
    pub fn clear_buffer(&self, category: &str) { self.metrics.lock().unwrap().clear_buffer(category); }
    pub fn finish_statistics(&self) -> Value {
        self.metrics.lock().unwrap().finish();
        self.statistics()
    }

    pub fn diagnostics_status(&self) -> Value {
        let log = self.diagnostics.lock().unwrap();
        match log.as_ref() {
            Some(log) => json!({"path":log.path,"writeFailed":log.failed}),
            None => Value::Null,
        }
    }

    pub fn native_response(&self, plugin: &str, route: Option<&(String, String)>,
        elapsed_ms: u128, bytes: usize, value: &Value) {
        self.metrics.lock().unwrap().response(elapsed_ms, bytes);
        self.meaningful_work("responseReceived",1);
        let dataset = value.get("returnObj").unwrap_or(value);
        let counts: BTreeMap<_, _> = dataset.as_object().into_iter()
            .flat_map(|object| object.iter())
            .filter_map(|(name, rows)| rows.as_array().map(|rows| (name, rows.len())))
            .collect();
        self.record(json!({"event":"responseReceived","plugin":plugin,
            "route":route,"requestElapsedMs":elapsed_ms,"responseBytes":bytes,
            "rowCounts":counts}));
    }

    pub fn cached_response(&self, plugin: &str, route: Option<&(String, String)>, bytes: usize) {
        // Reusing a retained body does not download bytes or spend server time.
        self.record(json!({"event":"responseReused","plugin":plugin,
            "route":route,"retainedResponseBytes":bytes}));
    }

    pub fn request_failed(&self, plugin: &str, route: Option<&(String, String)>, code: &str) {
        self.record(json!({"event":"requestFailed","plugin":plugin,"route":route,"code":code}));
    }

    pub fn page_written(&self, category: &str, records: usize, write_ms: u128, checkpoint_ms: u128) {
        self.metrics.lock().unwrap().page(write_ms, checkpoint_ms);
        self.record(json!({"event":"pageWritten","category":category,"records":records,
            "writeElapsedMs":write_ms,"checkpointElapsedMs":checkpoint_ms}));
    }

    pub fn begin_page(self: &Arc<Self>, plugin: &str, category: &str, completed: usize) -> BatchGuard {
        let guard = self.begin_batch(plugin, 0);
        guard.progress_scope(category, completed);
        let mut batches = self.batches.lock().unwrap();
        batches[guard.index]["kind"] = json!("definitionPage");
        batches[guard.index]["countKnown"] = json!(false);
        drop(batches);
        guard
    }
    pub fn batch_count(&self) -> usize {
        self.batches.lock().unwrap().len()
    }

    /// Continue the exact transfer row through local processing. A recovered
    /// cache hit has no transfer row, so it receives a new processing row.
    pub fn processing_batch(
        self: &Arc<Self>,
        index: usize,
        plugin: &str,
        files: usize,
    ) -> BatchGuard {
        let mut batches = self.batches.lock().unwrap();
        let index = if batches.get(index).is_some_and(|row| {
            row["plugin"] == plugin && row["files"] == files && row["phase"] == "Verified"
        }) {
            index
        } else {
            let index = batches.len();
            batches.push(json!({"plugin":plugin,"files":files,"verified":files}));
            index
        };
        batches[index]["phase"] = json!("Processing");
        batches[index]["processed"] = json!(0);
        BatchGuard {
            activity: self.clone(),
            index,
            complete: false,
        }
    }
    pub fn begin_batch(self: &Arc<Self>, plugin: &str, files: usize) -> BatchGuard {
        let mut batches = self.batches.lock().unwrap();
        let index = batches.len();
        batches.push(json!({"plugin":plugin,"files":files,"verified":0,"phase":"Downloading"}));
        BatchGuard {
            activity: self.clone(),
            index,
            complete: false,
        }
    }
    pub fn batches(&self) -> Value {
        json!(self.batches.lock().unwrap().iter()
            .filter(|row| row["phase"] != "Checked").collect::<Vec<_>>())
    }

    pub fn begin(self: &Arc<Self>, plugin: &str, identity: Option<Identity>, step: &str) -> Guard {
        self.note(plugin, step);
        let id = uuid::Uuid::new_v4().to_string();
        self.entries.lock().unwrap().insert(
            id.clone(),
            Entry {
                plugin: plugin.into(),
                identity,
                step: step.into(),
                started: Instant::now(),
            },
        );
        Guard {
            activity: self.clone(),
            id,
        }
    }
    pub fn snapshot(&self) -> Value {
        json!(self.entries.lock().unwrap().iter().map(|(id,e)| json!({"id":id,"plugin":e.plugin,"identity":e.identity,"step":e.step,"elapsedSeconds":e.started.elapsed().as_secs()})).collect::<Vec<_>>())
    }
    pub fn note(&self, plugin: &str, step: &str) {
        self.record(json!({"event":"stage","plugin":plugin,"stage":step}));
        let message = format!("{plugin}: {step}");
        let mut stages = self.stages.lock().unwrap();
        // Retain each distinct stage once, not one entry per downloaded item.
        if !stages.contains(&message) {
            stages.push(message);
        }
    }
    pub fn history(&self) -> Value {
        json!(*self.stages.lock().unwrap())
    }
}
impl Guard {
    /// Refresh live counters without adding one history entry per poll.
    pub fn status(&self, step: impl Into<String>) {
        if let Some(entry) = self.activity.entries.lock().unwrap().get_mut(&self.id) {
            entry.step = step.into();
        }
    }

    pub fn step(&self, step: impl Into<String>) {
        if let Some(e) = self.activity.entries.lock().unwrap().get_mut(&self.id) {
            e.step = step.into();
            self.activity.note(&e.plugin, &e.step);
            e.started = Instant::now();
        }
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.activity.entries.lock().unwrap().remove(&self.id);
    }
}
pub fn action_label(action: &HostAction) -> String {
    match action {
        HostAction::NativeRead { method, arguments, export, .. }
        | HostAction::SharedNativeRead { method, arguments, export, .. } => {
            if method == "GetList" {
                "Finding items — reading next list page".into()
            } else if method == "GetRows" && arguments["absolutePage"].is_number() {
                format!("Downloading definition page {} (up to {} items)",
                    arguments["absolutePage"], arguments["pageSize"])
            } else if *export {
                format!("Downloading export ({method})")
            } else {
                format!("Reading definition ({method})")
            }
        }
        HostAction::NativeEntityPage { entity, page, .. } => {
            format!("Reading {entity}, page {page}")
        }
        HostAction::Query { query } | HostAction::SharedQuery { query } => format!(
            "Reading {}",
            query
                .tables
                .iter()
                .map(|t| t.table.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        HostAction::QueueManagedDecompilation { .. } | HostAction::QueueLocalDecompilation { .. } => "Queuing local decompiled project work".into(),
        HostAction::RetainedArtifactFunctionCall { .. } | HostAction::RetainedLocalArtifactFunctionCall { .. } => "Reading and retaining original compiled files".into(),
        HostAction::ArtifactFunctionStream { .. } => {
            "Exporting on the server and downloading ready batches".into()
        }
        HostAction::ArtifactInstall { .. } => "Installing temporary helper in Epicor".into(),
        HostAction::SharedArtifactFunctionCall { function, .. }
        | HostAction::ArtifactFunctionCall { function, .. }
        | HostAction::HelperCall { function, .. } => {
            format!("Downloading plugin data ({function})")
        }
        HostAction::ArtifactQueryExecute { .. } => "Reading plugin query results".into(),
        HostAction::ArtifactQueryUpdate { .. } | HostAction::ArtifactQueryAction { .. } => {
            "Running plugin operation".into()
        }
        HostAction::NativeFileExport { .. } => "Downloading native export".into(),
        HostAction::ReadBinary { .. }
        | HostAction::ParseJson { .. }
        | HostAction::ParseXml { .. }
        | HostAction::ParseJsonArchive { .. }
        | HostAction::ParseXmlArchive { .. } => "Reading downloaded export".into(),
        HostAction::Assemble { .. } => "Saving downloaded file".into(),
        HostAction::BuildZip { .. } => "Building export ZIP from downloaded files".into(),
        HostAction::SystemInfo => "Reading system information".into(),
        HostAction::HostUtility { request, .. } => match request {
            herbert_contract::HostUtilityRequest::Begin => "Preparing server staging",
            herbert_contract::HostUtilityRequest::Inspect => "Checking server staging",
            herbert_contract::HostUtilityRequest::Reserve { .. } => "Preparing server files",
            herbert_contract::HostUtilityRequest::ReserveInputs { .. } => {
                "Preparing native server files"
            }
            herbert_contract::HostUtilityRequest::DownloadInput { .. } => {
                "Downloading and verifying native file"
            }
            herbert_contract::HostUtilityRequest::DownloadInputs { .. } => {
                "Downloading and verifying native file batch"
            }
            herbert_contract::HostUtilityRequest::Put { .. } => "Staging file",
            herbert_contract::HostUtilityRequest::Seal { .. } => "Packaging files on the server",
            herbert_contract::HostUtilityRequest::Download { .. } => {
                "Downloading and verifying server package"
            }
            herbert_contract::HostUtilityRequest::Close => "Removing staged files",
            herbert_contract::HostUtilityRequest::Purge => "Removing staging directory",
        }
        .into(),
    }
}

/// Plugins describe the purpose of a request; the host still owns all counters.
/// This text is display-only and must never change routing or permissions.
pub fn request_label(action: &HostAction, state: &Value) -> String {
    state.pointer("/_progress/request").and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty()).map(str::to_owned)
        .unwrap_or_else(|| action_label(action))
}

pub fn response_label(state: &Value) -> &str {
    state.pointer("/_progress/response").and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or("Data received; processing batch locally")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn meaningful_activity_continues_after_definitions_and_polling_does_not_invent_work() {
        let activity=Arc::new(Activity::default());
        let page=activity.begin_page("Functions","C08",0);page.page_received(2);page.processed(2);
        let before=activity.statistics()["meaningfulWork"].as_u64().unwrap();
        page.processed(2);assert_eq!(activity.statistics()["meaningfulWork"],before);
        activity.managed_job("job","running");activity.meaningful_work("decompiledFiles",3);
        let during=activity.statistics()["meaningfulWork"].as_u64().unwrap();assert!(during>before);
        assert_eq!(activity.statistics()["decompilerJobs"]["running"],1);
        for _ in 0..10 {activity.snapshot();activity.batches();assert_eq!(activity.statistics()["meaningfulWork"],during);}
        activity.managed_job("job","finished");assert_eq!(activity.statistics()["decompilerJobs"]["finished"],1);assert!(activity.statistics()["decompilerJobs"]["running"].is_null());
    }
    #[test]
    fn empty_checks_do_not_complete_batches_or_move_other_guards() {
        let activity = Arc::new(Activity::default());
        let empty = activity.begin_page("Reports", "C17", 0);
        let populated = activity.begin_page("Reports", "C17", 0);
        empty.page_received(0);
        empty.processing_complete(0);
        assert_eq!(activity.batches().as_array().unwrap().len(), 1);
        populated.page_received(16);
        populated.processing_complete(16);
        assert_eq!(activity.batches()[0]["phase"], "Processed");
        assert_eq!(activity.batches()[0]["completedItems"], 16);
    }
    #[tokio::test]
    async fn transport_events_are_scoped_to_the_run_and_record_retry_timing() {
        let path = std::env::temp_dir().join(format!("herbert-transport-{}.jsonl", uuid::Uuid::new_v4()));
        let activity = Arc::new(Activity { diagnostics: Mutex::new(Some(DiagnosticLog {
            file: File::options().write(true).create_new(true).open(&path).unwrap(),
            path: "transport.jsonl".into(), started: Instant::now(), failed: false,
        })), ..Activity::default() });
        record_transport(1, 5, None, None);
        TRANSPORT_LOG.scope((activity.clone(), "Service".into(), "Read".into()), async {
            record_transport(1, 25, Some("THROTTLED"), Some(1000));
            record_transport(2, 50, None, None);
        }).await;
        record_transport(1, 5, Some("UNRELATED"), None);
        let text = std::fs::read_to_string(&path).unwrap();
        let events: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["retryDelayMs"], 1000);
        assert_eq!(events[1]["attempt"], 2);
        assert!(events[1]["failureCode"].is_null());
        assert!(!text.contains("UNRELATED"));
        drop(activity);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn diagnostics_keep_counts_without_payloads_and_polling_does_not_write() {
        let path = std::env::temp_dir().join(format!("herbert-events-{}.jsonl", uuid::Uuid::new_v4()));
        let activity = Activity { diagnostics: Mutex::new(Some(DiagnosticLog {
            file: File::options().write(true).create_new(true).open(&path).unwrap(),
            path: "test.jsonl".into(), started: Instant::now(), failed: false,
        })), ..Activity::default() };
        activity.native_response("C04", Some(&("Ice.BO.BAQDesignerSvc".into(), "GetRows".into())),
            123, 456, &json!({"returnObj":{"QueryHdr":[{"secret":"DO_NOT_LOG"}],"token":"DO_NOT_LOG"}}));
        let before = std::fs::read(&path).unwrap();
        activity.snapshot();
        activity.history();
        activity.batches();
        assert_eq!(activity.diagnostics_status()["writeFailed"], false);
        assert_eq!(before, std::fs::read(&path).unwrap());
        let text = String::from_utf8(before).unwrap();
        assert!(!text.contains("DO_NOT_LOG"));
        let event: Value = text.lines().map(|line|serde_json::from_str::<Value>(line).unwrap()).find(|event|event["event"]=="responseReceived").unwrap();
        assert_eq!(event["rowCounts"]["QueryHdr"], 1);
        assert_eq!(event["requestElapsedMs"], 123);
        assert_eq!(event["responseBytes"], 456);
        drop(activity);
        // A failed log write is visible but does not abort the collection.
        let activity = Activity { diagnostics: Mutex::new(Some(DiagnosticLog {
            file: File::open(&path).unwrap(), path: "test.jsonl".into(),
            started: Instant::now(), failed: false,
        })), ..Activity::default() };
        activity.request_failed("C04", None, "TIMEOUT");
        assert_eq!(activity.diagnostics_status()["writeFailed"], true);
        drop(activity);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn definition_page_reports_download_then_real_writes_and_committed_progress() {
        let activity = Arc::new(Activity::default());
        let page = activity.begin_page("BAQs", "C04", 100);
        assert_eq!(activity.batches()[0]["countKnown"], false);
        assert_eq!(activity.batches()[0]["completedItems"], 100);
        page.page_received(24);
        page.processed(7);
        assert_eq!(activity.batches()[0]["files"], 24);
        assert_eq!(activity.batches()[0]["processed"], 7);
        assert_eq!(activity.batches()[0]["completedItems"], 100);
        page.processing_complete(124);
        assert_eq!(activity.batches()[0]["completedItems"], 124);
        assert_eq!(activity.batches()[0]["phase"], "Processed");
    }
    #[test]
    fn batch_counts_survive_completion_and_interruption() {
        let activity = Arc::new(Activity::default());
        let first = activity.begin_batch("C04", 25);
        first.update("Verifying", 7);
        assert_eq!(activity.batches()[0]["verified"], 7);
        first.complete();
        let tail = activity.begin_batch("C04", 15);
        assert_eq!(activity.batches()[1]["files"], 15);
        drop(tail);
        assert_eq!(activity.batches()[0]["phase"], "Verified");
        assert_eq!(activity.batches()[0]["verified"], 25);
        assert_eq!(activity.batches()[1]["phase"], "Interrupted");
    }
    #[test]
    fn concurrent_operations_update_and_clear_independently() {
        let a = Arc::new(Activity::default());
        let x = a.begin("First", None, "Discovering");
        let y = a.begin("Second", None, "Reading");
        assert_eq!(a.snapshot().as_array().unwrap().len(), 2);
        x.step("Saving files");
        drop(y);
        assert_eq!(a.snapshot()[0]["step"], "Saving files");
        drop(x);
        assert_eq!(a.snapshot(), json!([]));
        assert_eq!(
            a.history(),
            json!([
                "First: Discovering",
                "Second: Reading",
                "First: Saving files"
            ])
        );
        a.note("First", "Saving files");
        assert_eq!(a.history().as_array().unwrap().len(), 3);
    }
}

#[cfg(test)]
mod compiled_page_tests {
 use super::*;
 #[test]fn compiled_pages_stay_pending_until_continuations_settle(){let activity=Activity::default();let one=activity.compiled_page();let two=activity.compiled_page();assert_eq!(activity.statistics()["pendingCompiledPages"],2);drop(one);assert_eq!(activity.statistics()["pendingCompiledPages"],1);drop(two);assert_eq!(activity.statistics()["pendingCompiledPages"],0);}
}
