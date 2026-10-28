//! One producer invocation runs beside a reader of immutable published inputs.
//! The producer stays request-bound. No server background task is created.
use super::*;
use herbert_contract::{StagedFile, StagedInput, StagingIdentity};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("STAGED_STREAM_INVALID", message)
}

async fn run_stream<P, C, T>(producer: P, consumer: C) -> Result<T, Diagnostic>
where
    P: std::future::Future<Output = Result<T, Diagnostic>>,
    C: std::future::Future<Output = Result<(), Diagnostic>>,
{
    let (production, consumption) = tokio::join!(producer, consumer);
    let response = production?;
    consumption?;
    Ok(response)
}

struct PublishedInputs {
    selected: BTreeMap<String, StagingIdentity>,
    ready: BTreeMap<String, StagedFile>,
    downloaded: BTreeSet<String>,
    failed: usize,
    active: usize,
}
impl PublishedInputs {
    fn new(inputs: Vec<StagingIdentity>, batch_size: usize) -> Result<Self, Diagnostic> {
        if inputs.is_empty() || batch_size == 0 {
            return Err(invalid(
                "Supply selected inputs and a positive transfer batch size.",
            ));
        }
        let mut selected = BTreeMap::new();
        let mut identities = BTreeSet::new();
        for input in inputs {
            storage::safe_relative(&input.name)?;
            if !identities.insert(serde_json::to_string(&input.identity).unwrap())
                || selected.insert(input.file_id.clone(), input).is_some()
            {
                return Err(invalid("The stream repeats a file or artifact identity."));
            }
        }
        Ok(Self {
            selected,
            ready: BTreeMap::new(),
            downloaded: BTreeSet::new(),
            failed: 0,
            active: 0,
        })
    }

    fn observe(&mut self, response: Value) -> Result<(), Diagnostic> {
        let files: Vec<StagedFile> = serde_json::from_value(response["files"].clone())
            .map_err(|_| invalid("The publication inventory is malformed."))?;
        let failed = response["failed"]
            .as_u64()
            .ok_or_else(|| invalid("The failure count is missing."))?;
        let active = response["active"]
            .as_u64()
            .ok_or_else(|| invalid("The active count is missing."))?;
        if failed > self.selected.len() as u64
            || active > self.selected.len() as u64
            || files.len() as u64 + failed + active > self.selected.len() as u64
        {
            return Err(invalid(
                "The publication counts exceed the selected inputs.",
            ));
        }
        let mut observed = BTreeMap::new();
        for file in files {
            let selected = self
                .selected
                .get(&file.file_id)
                .ok_or_else(|| invalid("A published file is outside the selection."))?;
            if file.name != selected.name
                || file.sha256.len() != 64
                || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                || self
                    .ready
                    .get(&file.file_id)
                    .is_some_and(|prior| prior != &file)
            {
                return Err(invalid(
                    "A published file changed its reserved identity or receipt.",
                ));
            }
            if observed.insert(file.file_id.clone(), file).is_some() {
                return Err(invalid("The publication inventory repeats a file."));
            }
        }
        if self.ready.keys().any(|key| !observed.contains_key(key)) || failed < self.failed as u64 {
            return Err(invalid("A published result disappeared during collection."));
        }
        self.ready = observed;
        self.failed = failed as usize;
        self.active = active as usize;
        Ok(())
    }

    fn next_batch(&self, size: usize, producer_finished: bool) -> Vec<StagedInput> {
        let pending: Vec<_> = self
            .ready
            .iter()
            .filter(|(key, _)| !self.downloaded.contains(*key))
            .take(size)
            .map(|(key, file)| StagedInput {
                file: file.clone(),
                identity: self.selected[key].identity.clone(),
            })
            .collect();
        if pending.len() == size || producer_finished {
            pending
        } else {
            Vec::new()
        }
    }
    fn accept(&mut self, batch: &[StagedInput]) {
        self.downloaded
            .extend(batch.iter().map(|input| input.file.file_id.clone()));
    }
}

impl Host {
    pub(super) async fn stream_artifact_inputs(
        &self,
        plugin: &Plugin,
        holder: &str,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        cancel: &CancellationToken,
        alias: String,
        function: String,
        arguments: Value,
        operation: String,
        inputs: Vec<StagingIdentity>,
        batch_size: usize,
    ) -> Result<crate::connection::NativeResponse, Diagnostic> {
        let context = run
            .collection_context
            .lock()
            .await
            .clone()
            .ok_or_else(|| invalid("The stream has no active collection context."))?;
        let mut published = PublishedInputs::new(inputs, batch_size)?;
        for input in published.selected.values() {
            if input.identity.category != plugin.category.id
                || !super::identity_in_scope(&input.identity, &connection.company)
            {
                return Err(invalid("A stream input belongs to another artifact scope."));
            }
        }
        // Check reservation ownership before submitting the producer. This uses
        // the normal operation gate and scope checks, without a second export.
        let (reservation, _) = self
            .host_utility(
                plugin,
                &operation,
                &HostUtilityRequest::Inspect,
                None,
                run,
                connection,
                cancel,
            )
            .await?;
        let slots = reservation["inputs"]
            .as_array()
            .ok_or_else(|| invalid("The staged operation has no input reservations."))?;
        for input in published.selected.values() {
            if !slots
                .iter()
                .any(|slot| slot["fileId"] == input.file_id && slot["name"] == input.name)
            {
                return Err(invalid("The stream input does not match a reserved file."));
            }
        }
        let progress = run.activity.begin(
            &plugin.category.name,
            None,
            "Preparing server export and batch downloads",
        );
        let finished = AtomicBool::new(false);
        let producer = async {
            let result = self
                .native_artifact_action(
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
                .await;
            finished.store(true, Ordering::Release);
            result
        };
        let consumer = async {
            loop {
                // Read completion before polling. If it changes during the poll,
                // the next pass reads the final publication inventory.
                let producer_finished = finished.load(Ordering::Acquire);
                let response = self
                    .utility_call(
                        run,
                        connection,
                        &operation,
                        json!({"action":"pollInputs"}),
                        cancel,
                    )
                    .await?;
                published.observe(response)?;
                progress.status(format!(
                    "Server exports: {}/{} ready, {} failed, {} active; {} files processed",
                    published.ready.len(),
                    published.selected.len(),
                    published.failed,
                    published.active,
                    published.downloaded.len()
                ));
                loop {
                    let batch = published.next_batch(batch_size, producer_finished);
                    if batch.is_empty() {
                        break;
                    }
                    let selected = batch[0].identity.clone();
                    let batch_index = run.activity.batch_count();
                    self.host_utility(
                        plugin,
                        &operation,
                        &HostUtilityRequest::DownloadInputs {
                            files: batch.clone(),
                            identity: selected.clone(),
                            chunk_bytes: 1024 * 1024,
                        },
                        Some(&selected),
                        run,
                        connection,
                        cancel,
                    )
                    .await?;
                    // Write normal named outputs before reading another batch.
                    // The producer remains active while local processing runs.
                    let batch_progress = run.activity.processing_batch(
                        batch_index,
                        &plugin.category.name,
                        batch.len(),
                    );
                    let completed = collection::snapshot(run)
                        .await?
                        .artifacts
                        .iter()
                        .filter(|a| a.identity.category == plugin.category.id)
                        .count();
                    batch_progress.progress_scope(&plugin.category.id, completed);
                    for (index, input) in batch.iter().enumerate() {
                        progress.status(format!(
                            "Processing downloaded batch: {} / {} items",
                            index,
                            batch.len()
                        ));
                        Box::pin(self.collect_one(
                            run.clone(),
                            connection.clone(),
                            plugin.clone(),
                            context.processors.clone(),
                            input.identity.clone(),
                            context.page_size,
                            context.full,
                            Some(json!({"operationId":operation,"file":input.file})),
                        ))
                        .await?;
                        context
                            .processed_inputs
                            .lock()
                            .await
                            .insert(storage::identity_key(&input.identity));
                        batch_progress.processed(index + 1);
                    }
                    let completed = collection::snapshot(run)
                        .await?
                        .artifacts
                        .iter()
                        .filter(|a| a.identity.category == plugin.category.id)
                        .count();
                    batch_progress.processing_complete(completed);
                    published.accept(&batch);
                    progress.status(format!(
                        "Server exports: {}/{} ready, {} failed; {} files processed",
                        published.ready.len(),
                        published.selected.len(),
                        published.failed,
                        published.downloaded.len()
                    ));
                }
                if producer_finished {
                    if published.ready.len() + published.failed != published.selected.len() {
                        return Err(invalid(
                            "The producer stopped without publishing every input result.",
                        ));
                    }
                    return Ok::<(), Diagnostic>(());
                }
                // One second bounds normal readiness-display latency without a
                // busy loop. This is a polling interval, not an export timeout.
                tokio::select! {
                    _ = cancel.cancelled() => return Err(invalid("The stream was canceled; unresolved server work is preserved.")),
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                }
            }
        };
        // Poll both futures throughout each batch read. Awaiting the producer
        // first would recreate the original export-all-before-download defect.
        // A reader failure does not silently abandon or replay the producer.
        let response = run_stream(producer, consumer).await?;
        progress.step("Server export and batch processing complete");
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection(count: usize) -> Vec<StagingIdentity> {
        (0..count).map(|n| StagingIdentity {
            file_id: format!("{n:032x}.input"), name: format!("{n}.baq"),
            identity: serde_json::from_value(json!({"category":"C04","kind":"BAQ","scope":{"type":"company","company":"EPIC06"},"keys":{"QueryID":format!("Q{n}"),"Company":"EPIC06"}})).unwrap(),
        }).collect()
    }
    fn snapshot(inputs: &[StagingIdentity]) -> Value {
        json!({"files":inputs.iter().map(|input| json!({"fileId":input.file_id,"name":input.name,"bytes":1,"sha256":"a".repeat(64)})).collect::<Vec<_>>(),"failed":0,"active":0})
    }
    #[test]
    fn full_batches_are_available_before_producer_completion_and_tail_waits() {
        let inputs = selection(53);
        let mut state = PublishedInputs::new(inputs.clone(), 24).unwrap();
        state.observe(snapshot(&inputs[..23])).unwrap();
        assert!(state.next_batch(24, false).is_empty());
        state.observe(snapshot(&inputs[..24])).unwrap();
        let first = state.next_batch(24, false);
        assert_eq!(first.len(), 24);
        state.accept(&first);
        assert!(state.next_batch(24, false).is_empty());
        state.observe(snapshot(&inputs)).unwrap();
        let second = state.next_batch(24, false);
        assert_eq!(second.len(), 24);
        state.accept(&second);
        assert!(state.next_batch(24, false).is_empty());
        assert_eq!(state.next_batch(24, true).len(), 5);
    }
    #[test]
    fn published_receipts_cannot_change_disappear_repeat_or_escape_selection() {
        let inputs = selection(2);
        let mut state = PublishedInputs::new(inputs.clone(), 24).unwrap();
        state.observe(snapshot(&inputs[..1])).unwrap();
        assert!(state.observe(snapshot(&[])).is_err());
        let mut changed = snapshot(&inputs[..1]);
        changed["files"][0]["bytes"] = json!(2);
        assert!(state.observe(changed).is_err());
        assert!(
            state
                .observe(snapshot(&[inputs[0].clone(), inputs[0].clone()]))
                .is_err()
        );
        assert!(state.observe(snapshot(&selection(3))).is_err());
    }

    #[tokio::test]
    async fn one_submission_downloads_two_batches_before_export_finishes() {
        let inputs = selection(53);
        let output = tempfile::tempdir().unwrap();
        let mut journal = storage::new_journal("streamed-output".into());
        journal.company = "EPIC06".into();
        journal.category_names.insert("C04".into(), "BAQs".into());
        let mut storage = Storage::open(output.path(), false, journal).unwrap();
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        let (ack, mut acknowledgments) = tokio::sync::mpsc::channel(1);
        let finished = AtomicBool::new(false);
        let submissions = std::sync::atomic::AtomicUsize::new(0);
        let mut sizes = Vec::new();
        let producer = async {
            submissions.fetch_add(1, Ordering::SeqCst);
            for count in [24, 48, 53] {
                send.send(snapshot(&inputs[..count])).await.unwrap();
                if count < 53 {
                    // This cannot complete if orchestration waits for the whole
                    // export before it starts the download consumer.
                    acknowledgments.recv().await.unwrap();
                }
            }
            finished.store(true, Ordering::Release);
            drop(send);
            Ok(())
        };
        let consumer = async {
            let mut state = PublishedInputs::new(inputs.clone(), 24)?;
            while let Some(receipts) = receive.recv().await {
                state.observe(receipts)?;
                let batch = state.next_batch(24, false);
                if !batch.is_empty() {
                    assert!(!finished.load(Ordering::Acquire));
                    // Exercise the production output writer while the producer
                    // waits for this batch. Cache receipts alone are not proof.
                    for input in &batch {
                        let native = storage.preserve_for(
                            Some(&input.identity),
                            b"native fixture",
                            "baq",
                        )?;
                        storage
                            .journal
                            .artifact_acquisitions
                            .insert(storage::identity_key(&input.identity), vec![native]);
                        let (files, _) = crate::processing::write_views(
                            &mut storage,
                            &input.identity,
                            &json!({"herbertDirectOutput":true}),
                        )?;
                        let named = files
                            .iter()
                            .find(|file| file.role == "derived" && file.path.ends_with(".baq"))
                            .expect("Each received item must produce its named native file");
                        assert!(named.path.starts_with("BAQs/"), "{}", named.path);
                        assert_eq!(
                            std::fs::read(storage.confined(&named.path)?).unwrap(),
                            b"native fixture"
                        );
                    }
                    sizes.push(batch.len());
                    state.accept(&batch);
                    ack.send(()).await.unwrap();
                }
            }
            assert!(finished.load(Ordering::Acquire));
            sizes.push(state.next_batch(24, true).len());
            Ok(())
        };
        tokio::time::timeout(
            // The fixture now synchronizes real output files on Windows.
            // This watchdog detects deadlocks; it is not a production timeout.
            std::time::Duration::from_secs(60),
            run_stream(producer, consumer),
        )
        .await
        .expect("The consumer must run before the producer finishes")
        .unwrap();
        assert_eq!(submissions.load(Ordering::SeqCst), 1);
        assert_eq!(sizes, vec![24, 24, 5]);
    }

    #[tokio::test]
    async fn reader_error_does_not_drop_or_resubmit_producer() {
        let settled = AtomicBool::new(false);
        let producer = async {
            tokio::task::yield_now().await;
            settled.store(true, Ordering::Release);
            Ok(())
        };
        let result = run_stream(producer, async { Err(invalid("Read failed")) }).await;
        assert!(result.is_err());
        assert!(settled.load(Ordering::Acquire));
    }
}
