//! Ready exports flow to the shared writers while the plugin continues its page.
//! A final inventory cannot leave this pipeline until all accepted writes finish.
use super::*;
use crate::{activity::BatchGuard, page_buffer::{BufferedPage, Page, Producer}};

/// Negotiated prepared-page protocol. Each item is a complete materialization
/// state, so unrelated definitions do not accompany another item's continuation.
pub(crate) fn independent_work(request: &mut GuestRequest)
    -> Result<Option<(Vec<Value>, Option<String>)>, Diagnostic>
{
    let Some(items) = request.state.as_object_mut().and_then(|s| s.remove("_hostWorkItems")) else { return Ok(None); };
    let Value::Array(items) = items else { return Err(Diagnostic::new("PLUGIN_OUTPUT_INVALID", "Independent work items must be an array.")); };
    if items.iter().any(|item| !item.is_object() || item.get("_hostWorkItems").is_some()) {
        return Err(Diagnostic::new("PLUGIN_OUTPUT_INVALID", "Each independent work item must be a non-nested materialization state."));
    }
    let next = request.state.get("_hostWorkNextCursor").cloned().unwrap_or(Value::Null);
    if !next.is_null() && !next.is_string() { return Err(Diagnostic::new("PLUGIN_OUTPUT_INVALID", "The independent work resume point must be a saved position or null.")); }
    Ok(Some((items, next.as_str().map(str::to_owned))))
}

#[cfg(test)]
mod independent_work_tests {
    use super::*;
    fn owner(name: &str) -> Identity {
        Identity { category:"bpm-directives".into(),kind:"MethodDirectiveGroup".into(),scope:Scope::Global,
            keys:std::collections::BTreeMap::from([("Source".into(),"BO".into()),("BpMethodCode".into(),name.into())]) }
    }
    #[test]
    fn later_work_item_can_use_only_records_validated_in_this_page() {
        let accepted = ValidatedPageOwners::default();
        let first = owner("Ice.BO.A.Update"); let second = owner("Ice.BO.B.Update");
        let mut request=GuestRequest{abi:PLUGIN_ABI,operation:"materialize".into(),category:"bpm-directives".into(),company:"C".into(),identity:None,cursor:None,page_size:None,state:json!({"_hostValidatedOwners":[second]}),response:None,error:None};
        assert!(accepted.find(&second).unwrap().is_none());
        // An inventory-only sentinel and arbitrary guest state grant no ownership.
        validate_inventory_records(&request,"C",&[],&[second.clone()],Some(&accepted)).unwrap();
        assert!(accepted.find(&second).unwrap().is_none());
        validate_inventory_records(&request,"C",&[(first.clone(),json!({"original":"bytes"}))],&[first.clone()],Some(&accepted)).unwrap();
        request.state=json!({"_bpmCompiledStart":true});
        assert_eq!(accepted.find(&first).unwrap(),Some(first.clone()));
        assert!(accepted.find(&second).unwrap().is_none());
        // A malformed later record must not partially authorize earlier records
        // in that same rejected batch.
        let mut foreign=second.clone();foreign.scope=Scope::Company{company:"OTHER".into()};
        for (records,ids) in [
            (vec![(second.clone(),json!({})),(foreign.clone(),json!({}))],vec![second.clone(),foreign]),
            (vec![(second.clone(),json!({}))],vec![]),
            (vec![(second.clone(),json!({})),(second.clone(),json!({}))],vec![second.clone()]),
        ] {
            assert!(validate_inventory_records(&request,"C",&records,&ids,Some(&accepted)).is_err());
            assert!(accepted.find(&second).unwrap().is_none());
        }
        let mut other_category=second.clone();other_category.category="functions".into();
        assert!(validate_inventory_records(&request,"C",&[(other_category.clone(),json!({}))],&[other_category],Some(&accepted)).is_err());
        request.operation="acquire".into();request.identity=Some(first.clone());
        assert!(validate_inventory_records(&request,"C",&[(second.clone(),json!({}))],&[second.clone()],Some(&accepted)).is_err());
        // A resumed/new page starts empty; accepting a prior page does not grant
        // rights on the next page or require a completed writer to authorize it.
        assert!(ValidatedPageOwners::default().find(&first).unwrap().is_none());
    }
    #[test]
    fn rejects_malformed_nested_items_and_preserves_cursor_and_progress() {
        let mut request=GuestRequest{abi:PLUGIN_ABI,operation:"materialize".into(),category:"fixture".into(),company:"C".into(),identity:None,cursor:None,page_size:None,state:json!({}),response:None,error:None};
        assert!(independent_work(&mut request).unwrap().is_none());
        for state in [json!({"_hostWorkItems":{}}),json!({"_hostWorkItems":[null]}),json!({"_hostWorkItems":[{"_hostWorkItems":[]}]}),json!({"_hostWorkItems":[],"_hostWorkNextCursor":4})]{request.state=state;assert!(independent_work(&mut request).is_err());}
        request.state=json!({"_hostWorkItems":[{"one":1},{"two":2}],"_hostWorkNextCursor":"next","_progress":{"pageRecords":2}});
        let(items,next)=independent_work(&mut request).unwrap().unwrap();assert_eq!(items,vec![json!({"one":1}),json!({"two":2})]);assert_eq!(next.as_deref(),Some("next"));assert_eq!(request.state["_progress"]["pageRecords"],2);assert!(request.state.get("_hostWorkItems").is_none());
    }
}

/// Ready groups save progress at most this often, and always once the page's
/// writer drains. Saving after every record made a large personalization
/// run spend as long saving progress as writing files, and rewrote the
/// growing name registry each time. Files written since the last save are
/// recovered by readback after a crash (`write_derived_stream` reuses an
/// unrecorded file whose bytes match), so this bounds repeated work, not
/// correctness. Operating default, not a platform limit.
const CHECKPOINT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// Merge queued groups up to one full chunk for every writer.
const MERGED_RECORDS: usize = crate::file_writers::WRITERS * crate::file_writers::FILES_PER_CHUNK;

async fn save(run: &Arc<Run>) -> Result<(), Diagnostic> {
    let storage = run.storage.clone();
    blocking(move || storage.lock().map_err(|_| Diagnostic::new(
        "RUN_STATE_FAILED", "Run state is unavailable."))?.checkpoint()).await
}

pub(super) struct OutputQueue {
    pub(super) owners: ValidatedPageOwners,
    sender: tokio::sync::mpsc::UnboundedSender<BufferedPage>,
    producer: Arc<Producer>,
    directory: PathBuf,
    batch: Arc<BatchGuard>,
    expected: Option<usize>,
}

/// Host-owned identities accepted by this output pipeline only. Guest checkpoint
/// fields never populate this registry, and it does not outlive the page.
#[derive(Default)]
pub(super) struct ValidatedPageOwners(std::sync::Mutex<std::collections::BTreeMap<String, Identity>>);
impl ValidatedPageOwners {
    pub(super) fn find(&self, owner: &Identity) -> Result<Option<Identity>, Diagnostic> {
        let owners = self.0.lock().map_err(|_| Diagnostic::new("OUTPUT_QUEUE_FAILED", "Validated page ownership is unavailable."))?;
        Ok(owners.get(&storage::identity_key(owner)).filter(|known| *known == owner).cloned())
    }
}

pub(super) fn validate_inventory_records(request: &GuestRequest, company: &str,
    records: &[(Identity, Value)], identities: &[Identity], owners: Option<&ValidatedPageOwners>) -> Result<(), Diagnostic>
{
    let page_keys: std::collections::BTreeSet<_> = identities.iter().map(storage::identity_key).collect();
    let mut record_keys = std::collections::BTreeSet::new();
    if !matches!(request.operation.as_str(), "discover" | "materialize" | "prepareAcquisition" | "acquire")
        || records.iter().any(|(id, _)| {
            let key = storage::identity_key(id);
            id.category != request.category
                || (request.operation == "acquire" && request.identity.as_ref() != Some(id))
                || !identity_in_scope(id, company)
                || !page_keys.contains(&key)
                || !record_keys.insert(key)
        })
    {
        return Err(Diagnostic::new("PLUGIN_IDENTITY_DENIED", "Inventory data must belong to an identity on this page."));
    }
    // Register only after the entire emitted batch has passed the same checks
    // used by the writer. Inventory-only or guest-supplied identities confer no rights.
    if let Some(owners) = owners {
        let mut accepted = owners.0.lock().map_err(|_| Diagnostic::new("OUTPUT_QUEUE_FAILED", "Validated page ownership is unavailable."))?;
        for (identity, _) in records { accepted.insert(storage::identity_key(identity), identity.clone()); }
    }
    Ok(())
}

impl OutputQueue {
    pub async fn enqueue(&self, records: Vec<(Identity, Value)>, run: &Arc<Run>, category: &str)
        -> Result<(), Diagnostic>
    {
        if records.is_empty() { return Ok(()); }
        let count = records.len();
        let page = Page { cursor: None, files: vec![], reply: GuestReply::InventoryData {
            records, inventory: Box::new(GuestReply::Inventory { identities: vec![],
                next_cursor: None, authoritative: true, diagnostics: vec![] }),
        }};
        let producer = self.producer.clone();
        let directory = self.directory.clone();
        let buffered = blocking(move || producer.store(page, &directory)).await?;
        run.activity.buffer_page(category, buffered.bytes, buffered.spilled, true);
        if self.expected.is_none() { self.batch.records_ready(count); }
        self.sender.send(buffered).map_err(|_| Diagnostic::new(
            "OUTPUT_QUEUE_CLOSED", "The output writer stopped before this page finished."))
    }
}

impl Host {
    pub(super) async fn drive_output_pipeline(
        &self, plugin: &Plugin, mut request: GuestRequest, connection: &Arc<Connection>,
        run: &Arc<Run>, holder: &str, temporary_access: bool,
        cancel: &CancellationToken, reply: Option<GuestReply>,
        issued: Vec<FileRef>,
    ) -> Result<(GuestReply, Vec<FileRef>), Diagnostic> {
        if !matches!(request.operation.as_str(), "materialize" | "acquire") || !plugin.has_role("pagedDefinitions") {
            return Box::pin(self.drive_inner(plugin, request, connection, run, holder,
                temporary_access, cancel, reply, None, issued)).await;
        }
        let independent = independent_work(&mut request)?;
        let completed = run.storage.lock().map_err(|_| Diagnostic::new(
            "RUN_STATE_FAILED", "Run state is unavailable."))?.journal.artifacts.iter()
            .filter(|artifact| artifact.identity.category == plugin.category.id
                && artifact.acquisition == Completeness::Collected).count();
        let batch = Arc::new(run.activity.begin_page(&plugin.category.name,
            &plugin.category.id, completed));
        // The guest can report the actual number of records in its prepared
        // page. Never substitute the requested page size for this count.
        let expected = request.state.pointer("/_progress/pageRecords")
            .and_then(Value::as_u64).and_then(|count| usize::try_from(count).ok());
        if let Some(count) = expected { batch.page_received(count); }
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let queue = OutputQueue { owners: ValidatedPageOwners::default(), sender, producer: Arc::new(self.page_memory.producer()),
            directory: self.state_root.join("page-buffers"), batch: batch.clone(), expected };
        let stop = cancel.child_token();
        let produce = async {
            let result = if let Some((items, next_cursor)) = independent {
                let mut identities = Vec::new();
                let mut diagnostics = Vec::new();
                let mut authoritative = true;
                let mut files = issued;
                let mut failure = None;
                // Each ready definition reaches the existing writer consumer
                // immediately while later items are still materializing.
                for state in items {
                    let mut item = request.clone();
                    item.state = state;
                    item.response = None;
                    match Box::pin(self.drive_inner(plugin, item, connection, run, holder,
                        temporary_access, &stop, None, Some(&queue), files.clone())).await {
                        Ok((GuestReply::Inventory { identities: ids, next_cursor: None,
                            authoritative: complete, diagnostics: ds }, retained)) => {
                            identities.extend(ids); diagnostics.extend(ds);
                            authoritative &= complete; files = retained;
                        }
                        Ok((GuestReply::Failure { diagnostic }, _)) | Err(diagnostic) => {
                            failure = Some(diagnostic); break;
                        }
                        _ => { failure = Some(Diagnostic::new("PLUGIN_OUTPUT_INVALID",
                            "An independent item did not finish its local inventory.")); break; }
                    }
                }
                if let Some(error) = failure { Err(error) } else {
                    Ok((GuestReply::Inventory { identities, next_cursor, authoritative, diagnostics }, files))
                }
            } else {
                Box::pin(self.drive_inner(plugin, request, connection, run, holder,
                    temporary_access, &stop, reply, Some(&queue), issued)).await
            };
            // Close the channel on success and error. Never leave a consumer waiting
            // for a sender that is retained by the outer join future.
            drop(queue);
            result
        };
        let consume = async {
            let load = |buffered: BufferedPage| async move {
                let bytes = buffered.bytes;
                let spilled = buffered.spilled;
                let loaded = blocking(move || buffered.load()).await?;
                run.activity.buffer_page(&plugin.category.id, bytes, spilled, false);
                let GuestReply::InventoryData { records, .. } = loaded.page.reply else {
                    return Err(Diagnostic::new("OUTPUT_QUEUE_INVALID", "The output queue contains an invalid record group."));
                };
                Ok(records)
            };
            let mut unsaved = false;
            let mut written = 0;
            let mut write_total = 0;
            let mut checkpoint_total = 0;
            let result: Result<(), Diagnostic> = async {
                let mut carry = None;
                let mut saved_at = std::time::Instant::now();
                loop {
                    let mut records = match carry.take() {
                        Some(records) => records,
                        None => match receiver.recv().await { Some(buffered) => load(buffered).await?, None => break },
                    };
                    // Merge groups that are already queued, so one write uses the whole
                    // writer pool instead of one thread per record. Stop once every
                    // writer has a full chunk; more adds no parallelism and would pull
                    // spilled groups into memory early. Never merge a repeated identity.
                    let mut keys = records.iter().map(|(id, _)| storage::identity_key(id))
                        .collect::<std::collections::HashSet<_>>();
                    while records.len() < MERGED_RECORDS {
                        let Ok(buffered) = receiver.try_recv() else { break };
                        let more = load(buffered).await?;
                        if more.iter().any(|(id, _)| keys.contains(&storage::identity_key(id))) {
                            carry = Some(more);
                            break;
                        }
                        keys.extend(more.iter().map(|(id, _)| storage::identity_key(id)));
                        records.extend(more);
                    }
                    let count = records.len();
                    let started = std::time::Instant::now();
                    let prepared = self.prepare_reference_page(run, records, &stop).await?;
                    let storage = run.storage.clone();
                    let writers = self.file_writers.clone();
                    let writer_cancel = stop.clone();
                    let progress = batch.clone();
                    blocking(move || crate::collection_output::write_shared_page(storage,
                        prepared, &writers, &writer_cancel,
                        |count| progress.processed(written + count))).await?;
                    written += count;
                    unsaved = true;
                    // Written groups are recoverable. The page cursor is still
                    // withheld until both sides finish successfully.
                    let write_ms = started.elapsed().as_millis();
                    let mut checkpoint_ms = 0;
                    if saved_at.elapsed() >= CHECKPOINT_INTERVAL {
                        let checkpoint_started = std::time::Instant::now();
                        save(run).await?;
                        unsaved = false;
                        saved_at = std::time::Instant::now();
                        checkpoint_ms = checkpoint_started.elapsed().as_millis();
                    }
                    write_total += write_ms;
                    checkpoint_total += checkpoint_ms;
                    run.activity.record(json!({"event":"readyGroupWritten",
                        "category":plugin.category.id,"records":count,
                        "writeElapsedMs":write_ms,"checkpointElapsedMs":checkpoint_ms}));
                }
                Ok(())
            }.await;
            // Save what was written even when a later group failed.
            let saved = if unsaved {
                let checkpoint_started = std::time::Instant::now();
                let saved = save(run).await;
                checkpoint_total += checkpoint_started.elapsed().as_millis();
                saved
            } else { Ok(()) };
            let result = result.and(saved).map(|()| written);
            if result.is_ok() && written > 0 {
                run.activity.page_written(&plugin.category.id, written, write_total, checkpoint_total);
            }
            if result.is_err() { stop.cancel(); }
            result
        };
        // Join, rather than race, the futures. A failure must not detach an active
        // blocking writer or let recovery start while it is still changing files.
        let (result, written) = tokio::join!(produce, consume);
        let written = written?;
        let result = result?;
        if matches!(&result.0, GuestReply::Failure { .. }) { return Ok(result); }
        if expected.is_some_and(|count| count != written) {
            return Err(Diagnostic::new("PAGE_RECORD_COUNT_MISMATCH",
                "The prepared page count does not match its written records. Saved files remain available; the page saved position was not advanced."));
        }
        let completed = run.storage.lock().map_err(|_| Diagnostic::new(
            "RUN_STATE_FAILED", "Run state is unavailable."))?.journal.artifacts.iter()
            .filter(|artifact| artifact.identity.category == plugin.category.id).count();
        let batch = Arc::try_unwrap(batch).map_err(|_| Diagnostic::new(
            "OUTPUT_QUEUE_ACTIVE", "An output worker is still using the page progress."))?;
        batch.processing_complete(completed);
        Ok(result)
    }
}
