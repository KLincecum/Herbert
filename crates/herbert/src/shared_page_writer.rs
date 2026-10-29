//! Convert a received page to file jobs. The host pool owns all write threads.
use super::*;
use crate::file_writers::{FILES_PER_CHUNK, Job, WriterPool};
use herbert_contract::FileRef;
use std::sync::{Arc, Condvar, Mutex, mpsc};

enum Content {
    Memory(Vec<u8>),
    Native(FileRef),
}
struct FileWork {
    record: usize,
    sequence: usize,
    name: String,
    content: Content,
}
struct RecordWork {
    artifact: Mutex<Artifact>,
    // Only duplicate destination names need ordered writes. Independent files
    // from one large artifact can use all four workers.
    ordered: bool,
    receipts: Mutex<std::collections::BTreeMap<usize, FileRef>>,
    next: Mutex<usize>,
    ready: Condvar,
    expected: usize,
}
struct Advance<'a> {
    next: std::sync::MutexGuard<'a, usize>,
    ready: &'a Condvar,
}
impl Drop for Advance<'_> {
    fn drop(&mut self) {
        *self.next += 1;
        self.ready.notify_all();
    }
}
impl RecordWork {
    fn enter(&self, sequence: usize) -> Advance<'_> {
        let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
        while *next != sequence {
            next = self.ready.wait(next).unwrap_or_else(|e| e.into_inner());
        }
        Advance {
            next,
            ready: &self.ready,
        }
    }
}
enum Event {
    Done(Result<(), Diagnostic>, Vec<Artifact>),
}
struct Completion {
    send: mpsc::Sender<Event>,
    done: bool,
}
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.send.send(Event::Done(Err(Diagnostic::new(
                "OUTPUT_WORKER_FAILED",
                "A file writer stopped before its chunk completed.",
            )), vec![]));
        }
    }
}

pub(crate) fn write_shared_page(
    shared: Arc<Mutex<Storage>>,
    records: Vec<PreparedRecord>,
    pool: &WriterPool,
    cancel: &tokio_util::sync::CancellationToken,
    progress: impl Fn(usize),
) -> Result<(), Diagnostic> {
    let unavailable = || Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.");
    let snapshot = {
        let mut storage = shared.lock().map_err(|_| unavailable())?;
        storage.page_owns_derived_files = true;
        let result = (|| {
            for record in &records {
                if let Some(group) = record.definition["herbertOutputGroup"].as_str() {
                    storage.group_artifact(&record.identity, group)?;
                } else if !record.definition["herbertOutputGroup"].is_null() {
                    return Err(Diagnostic::new(
                        "OUTPUT_INVALID",
                        "The output group must be text.",
                    ));
                }
                storage.artifact_folder(&record.identity)?;
            }
            Ok(Arc::new(storage.page_output_snapshot(records.iter().map(|record| &record.identity))))
        })();
        storage.page_owns_derived_files = false;
        result?
    };
    let mut states = Vec::new();
    let mut reference_updates = std::collections::BTreeMap::new();
    let mut work = Vec::new();
    for (index, record) in records.into_iter().enumerate() {
        let identity = record.identity.clone();
        if let Some(refs) = &record.reference_update { reference_updates.insert(identity_key(&identity), refs.clone()); }
        let outcome = record.outcome.clone();
        let (outputs, diagnostics) = outputs(&snapshot, record)?;
        let mut completed_artifact = artifact(&snapshot, identity, vec![], diagnostics);
        apply_outcome(&mut completed_artifact, outcome);
        let count = outputs.len();
        let mut names = std::collections::BTreeSet::new();
        let ordered = outputs
            .iter()
            .any(|(name, _)| !names.insert(name.to_lowercase()));
        for (sequence, (name, content)) in outputs.into_iter().enumerate() {
            work.push(FileWork {
                record: index,
                sequence,
                name,
                content,
            });
        }
        states.push(RecordWork {
            artifact: Mutex::new(completed_artifact),
            next: Mutex::new(0),
            ready: Condvar::new(),
            expected: count,
            ordered,
            receipts: Mutex::new(std::collections::BTreeMap::new()),
        });
    }
    let states = Arc::new(states);
    let (send, receive) = mpsc::channel();
    let mut work = work.into_iter();
    let mut jobs: Vec<Job> = Vec::new();
    loop {
        let chunk = work.by_ref().take(FILES_PER_CHUNK).collect::<Vec<_>>();
        if chunk.is_empty() {
            break;
        }
        let (states, snapshot, send, cancel) = (
            states.clone(),
            snapshot.clone(),
            send.clone(),
            cancel.clone(),
        );
        jobs.push(Box::new(move || {
            let mut completion = Completion { send:send.clone(), done:false };
            let mut error = None;
            let mut completed = Vec::new();
            for file in chunk {
                let state = &states[file.record];
                let sequence = file.sequence;
                let _order = state.ordered.then(|| state.enter(sequence));
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| if cancel.is_cancelled() {
                    Err(Diagnostic::new("CANCELED", "File writing was canceled."))
                } else {
                    let identity = state.artifact.lock().unwrap_or_else(|e| e.into_inner()).identity.clone();
                    match file.content {
                        Content::Memory(bytes) => snapshot.write_derived(&identity, &file.name, &bytes),
                        Content::Native(source) => (|| {
                            snapshot.verify(&source)?;
                            let mut input = std::fs::File::open(snapshot.confined(&source.path)?)
                                .map_err(|_| Diagnostic::new("RAW_UNAVAILABLE", "Epicor's export file is unavailable."))?;
                            snapshot.write_derived_stream(&identity, &file.name, &mut input)
                        })(),
                    }
                })).unwrap_or_else(|_| Err(Diagnostic::new("OUTPUT_WORKER_FAILED",
                    "A file writer stopped while writing a file.")));
                match result {
                    Ok((file, conflict)) => {
                        let mut artifact = state.artifact.lock().unwrap_or_else(|e| e.into_inner());
                        if conflict { artifact.diagnostics.push(Diagnostic::new("USER_EDIT_CONFLICT",
                            "Different content already occupies this output path. The existing file is preserved; the current source version is in Conflicts.")); }
                        let mut receipts = state.receipts.lock().unwrap_or_else(|e| e.into_inner());
                        receipts.insert(sequence, file);
                        // Return receipts once per 100-file assignment. The
                        // owner updates status without a lock for each file.
                        if receipts.len() == state.expected {
                            artifact.files = receipts.values().cloned().collect();
                            completed.push(artifact.clone());
                        }
                    }
                    Err(diagnostic) => { if error.is_none() { error = Some(diagnostic); } }
                }
            }
            let _ = send.send(Event::Done(error.map_or(Ok(()), Err), completed));
            completion.done = true;
        }));
    }
    let chunks = jobs.len();
    pool.submit_page(jobs);
    drop(send);
    let mut finished = 0;
    let mut completed = 0;
    let mut error = None;
    for event in receive {
        match event {
            Event::Done(result, artifacts) => {
                completed += artifacts.len();
                {
                    match shared.lock() {
                        Ok(mut storage) => commit_batch(&mut storage, artifacts, &reference_updates),
                        Err(_) => { if error.is_none() { error = Some(unavailable()); } }
                    }
                }
                progress(completed);
                finished += 1;
                if let Err(diagnostic) = result {
                    if error.is_none() {
                        error = Some(diagnostic);
                    }
                }
            }
        }
    }
    if finished != chunks && error.is_none() {
        error = Some(Diagnostic::new(
            "OUTPUT_WORKER_FAILED",
            "A file chunk did not return its result.",
        ));
    }
    // All writers have drained. Keep completed write receipts even when cancellation
    // stops between files of one artifact. Otherwise recovery mistakes our own
    // unrecorded output for a user file and creates a false conflict.
    // An incomplete artifact must never be reported as collected.
    let mut artifacts = Vec::with_capacity(states.len());
    for state in states.iter() {
        let mut artifact = state
            .artifact
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let receipts = state.receipts.lock().unwrap_or_else(|e| e.into_inner());
        if receipts.len() != state.expected {
            if receipts.is_empty() {
                continue;
            }
            artifact.files = receipts.values().cloned().collect();
            artifact.acquisition = Completeness::Partial;
            artifact.processing = "partial".into();
            artifact.diagnostics.push(Diagnostic::new(
                "OUTPUT_INCOMPLETE",
                "Some files were saved before writing stopped. Resume the run to finish this artifact.",
            ));
        }
        artifacts.push(artifact);
    }
    let mut storage = shared.lock().map_err(|_| unavailable())?;
    commit_batch(&mut storage, artifacts, &reference_updates);
    error.map_or(Ok(()), Err)
}

fn commit_batch(storage: &mut Storage, mut artifacts: Vec<Artifact>, updates: &std::collections::BTreeMap<String,ReferenceUpdate>) {
    for artifact in &mut artifacts {
        if let Some(update) = updates.get(&identity_key(&artifact.identity)) {
            let records=&update.records;
            let key=identity_key(&artifact.identity);
            if let Some(file)=artifact.files.iter().find(|f|f.sha256==update.core_sha256 && matches!(f.role.as_str(),"derived"|"derivedConflict")) {
                // The named core has an actual completed writer receipt, even
                // when an independent native/resource file failed. Its references
                // remain useful but the parent artifact stays explicitly partial.
                if storage.verify(file).is_ok(){
                    artifact.dependencies=records.iter().filter_map(|r|serde_json::from_value(r["identity"].clone()).ok()).collect();
                    crate::processing::replace_references(storage,&artifact.identity,records.clone());
                    let mut input=file.clone();input.role="referenceInput".into();storage.journal.definition_views.insert(key,input);
                    continue;
                }
            }
            artifact.processing="partial".into();
            artifact.diagnostics.push(Diagnostic::new("REFERENCE_INPUT_UNVERIFIED","No current core JSON receipt could be verified. Independent source references remain available."));
            // No verified current core exists. Do not leave stale occurrences
            // bound to a source path that this attempted write may have changed.
            artifact.dependencies.clear();crate::processing::replace_references(storage,&artifact.identity,vec![]);
            storage.journal.definition_views.remove(&key);
        }
    }
    let keys = artifacts.iter().map(|a| identity_key(&a.identity))
        .collect::<std::collections::HashSet<_>>();
    storage.journal.artifacts.retain(|old| !keys.contains(&identity_key(&old.identity)));
    for artifact in artifacts {
        storage.manifest.artifacts.insert(identity_key(&artifact.identity), artifact.clone());
        storage.journal.artifacts.push(artifact);
    }
}

fn outputs(
    storage: &Storage,
    record: PreparedRecord,
) -> Result<(Vec<(String, Content)>, Vec<Diagnostic>), Diagnostic> {
    let Some((sources, diagnostics)) = record.sources else {
        let bytes = serde_json::to_vec_pretty(&record.definition).map_err(|_| {
            Diagnostic::new(
                "OUTPUT_INVALID",
                "Cannot serialize the collected definition.",
            )
        })?;
        return Ok((
            vec![(
                format!(
                    "{}.json",
                    crate::output_layout::artifact_name(&record.identity)
                ),
                Content::Memory(bytes),
            )],
            vec![],
        ));
    };
    let native = storage
        .journal
        .artifact_acquisitions
        .get(&identity_key(&record.identity))
        .cloned()
        .unwrap_or_default();
    let (archive, archive_diagnostics) =
        crate::archives::prepare_views(storage, &record.identity, &native);
    let mut diagnostics = diagnostics;
    diagnostics.extend(archive_diagnostics);
    let mut outputs = archive
        .into_iter()
        .map(|s| (s.name, Content::Memory(s.bytes)))
        .collect::<Vec<_>>();
    let mut declared_native = std::collections::BTreeSet::new();
    if let Some(declared) = record.definition.pointer("/herbertSources/nativeFiles") {
        for entry in declared
            .as_array()
            .ok_or_else(|| Diagnostic::new("OUTPUT_INVALID", "Epicor files must be a list."))?
        {
            let file: FileRef = serde_json::from_value(entry["file"].clone()).map_err(|_| {
                Diagnostic::new("OUTPUT_INVALID", "The Epicor file reference is invalid.")
            })?;
            let name = entry["name"].as_str().ok_or_else(|| {
                Diagnostic::new("OUTPUT_INVALID", "The Epicor filename is missing.")
            })?;
            crate::storage::safe_relative(name)?;
            declared_native.insert((file.path.clone(),file.sha256.clone()));
            outputs.push((name.into(), Content::Native(file)));
        }
    }
    let mut extensions = std::collections::BTreeMap::<String, usize>::new();
    let name = crate::output_layout::artifact_name(&record.identity);
    for file in native {
        if declared_native.contains(&(file.path.clone(),file.sha256.clone())) { continue; }
        let extension = file.path.rsplit('.').next().unwrap_or("");
        if !matches!(
            extension,
            "baq"
                | "bpm"
                | "efxj"
                | "efxb"
                | "zip"
                | "xml"
                | "jsonc"
                | "rdl"
                | "png"
                | "jpg"
                | "dll"
        ) {
            continue;
        }
        let count = extensions.entry(extension.into()).or_default();
        *count += 1;
        let filename = if *count == 1 {
            format!("{name}.{extension}")
        } else {
            format!("{name} - {count}.{extension}")
        };
        outputs.push((filename, Content::Native(file)));
    }
    outputs.extend(
        sources
            .into_iter()
            .map(|s| (s.name, Content::Memory(s.bytes))),
    );
    Ok((outputs, diagnostics))
}
