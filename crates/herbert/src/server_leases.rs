//! Server activity leases. Local files are not evidence that a helper is active.
use super::*;
use std::collections::BTreeSet;
use std::time::Duration;
// tokio's Instant: the real clock in production, simulated time in paused-clock tests.
use tokio::time::Instant;
#[path = "known_helpers.rs"]
mod known_helpers;

const HEARTBEAT: Duration = Duration::from_secs(30);
const EXPIRY: Duration = Duration::from_secs(300);
/// Stop treating the lease as held this long before it can expire on the server: covers the delay between
/// lease_gate admitting a call and that call reaching the server, and clock-rate drift. 5% of EXPIRY.
const MARGIN: Duration = Duration::from_secs(15);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_identity_ignores_creator_but_requires_exact_brand_and_generation() {
        let generation = uuid::Uuid::new_v4();
        let mut identity = crate::native_artifact_store::NativeIdentity {
            id: format!("H{}", &generation.simple().to_string()[..29]),
            company: "FIXTURE".into(), row_id: uuid::Uuid::new_v4().to_string(),
            global_id: Some(generation.to_string()), owner: Some("another-user".into()),
            updatable: None, marker: format!("Herbert artifact {generation}"),
        };
        assert!(resource(&identity, "plugin").is_ok());
        identity.marker = format!("Herbert temporary helper; lease {generation}");
        assert!(resource(&identity, "plugin").is_ok());
        identity.marker.push_str(" modified");
        assert!(resource(&identity, "plugin").is_err());
        identity.marker = format!("Herbert artifact {generation}");
        identity.global_id = Some(uuid::Uuid::new_v4().to_string());
        assert!(resource(&identity, "plugin").is_err());
    }
}

pub(super) struct Keeper {
    pub library: String,
    resources: BTreeSet<String>,
    /// Set once the lease expired or renewal failed terminally. New helper
    /// work stops; Direct reads and owned cleanup continue.
    lost: Arc<std::sync::atomic::AtomicBool>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
tokio::task_local! { static CLEANUP_WORK: (); }
/// Mark cleanup of a resource this run's own journal records but that has no
/// authorization reference (records from earlier versions). Like owned cleanup
/// under `with_owned_cleanup`, it is not new helper work for `lease_gate`.
pub(super) async fn as_cleanup<T>(work: impl std::future::Future<Output = T>) -> T {
    CLEANUP_WORK.scope((), work).await
}
/// True inside `as_cleanup`: owned cleanup of this run's own resources, not new helper work.
pub(super) fn in_cleanup() -> bool {
    CLEANUP_WORK.try_with(|_| ()).is_ok()
}
pub(super) fn lease_lost() -> Diagnostic {
    Diagnostic::new("SERVER_LEASE_LOST", "This run's server lease for its temporary helpers was lost. New helper work stopped and affected items are incomplete. Direct reads continue, and cleanup still runs.")
}
/// Result of a helper call that may have been in flight when the lease was lost.
/// Such a call is uncertain: its result is not used and it is never retried.
pub(super) fn after_leased_call<T>(lost: &Option<Arc<std::sync::atomic::AtomicBool>>, result: Result<T, Diagnostic>) -> Result<T, Diagnostic> {
    if lost.as_ref().is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
        let mut error = Diagnostic::new("SERVER_LEASE_LOST_DURING_CALL", "The server lease was lost while this helper call was running. Its result is uncertain and was not used; the item is incomplete.");
        if let Err(inner) = &result {
            error.context.insert("callResult".into(), json!(inner.code));
        }
        return Err(error);
    }
    result
}
/// Helper generations a journal owns for its own scope, with the authorization
/// reference recorded at creation (None for records from earlier versions).
fn journal_generations(journal: &crate::storage::RunJournal) -> std::collections::BTreeMap<uuid::Uuid, Option<crate::server_changes::AuthorizationRef>> {
    fn add(record: &crate::native_artifact_store::ArtifactRecord, out: &mut std::collections::BTreeMap<uuid::Uuid, Option<crate::server_changes::AuthorizationRef>>) {
        if let Some(generation) = record.identity.as_ref().and_then(|identity| identity.global_id.as_deref()).and_then(|id| uuid::Uuid::parse_str(id).ok()) {
            out.insert(generation, record.authorization.clone());
        }
        for previous in &record.previous_generations {
            add(previous, out);
        }
    }
    let mut out = std::collections::BTreeMap::new();
    for operation in journal.native_artifacts.iter().filter(|operation| operation.run_id == journal.run_id
        && operation.scope.endpoint == journal.endpoint && operation.scope.company == journal.company
        && operation.scope.caller == journal.caller) {
        for artifact in &operation.artifacts {
            add(artifact, &mut out);
        }
    }
    out
}
/// Split discovered helpers into those this run may reap (its own generations,
/// or exact own-journal matches) and read-only foreign-pending reports.
fn partition_discovered(
    discovered: Vec<(uuid::Uuid, Value)>,
    own: &std::collections::BTreeMap<uuid::Uuid, Option<crate::server_changes::AuthorizationRef>>,
    journaled: &BTreeSet<uuid::Uuid>,
) -> (Vec<(uuid::Uuid, Value)>, Vec<crate::storage::ForeignPendingHelper>) {
    let mut reap = Vec::new();
    let mut foreign = Vec::new();
    for (generation, item) in discovered {
        if own.contains_key(&generation) || journaled.contains(&generation) {
            reap.push((generation, item));
        } else {
            foreign.push(crate::storage::ForeignPendingHelper {
                library_id: item["libraryId"].as_str().unwrap_or_default().into(),
                global_id: generation.to_string(),
                role: item["role"].as_str().unwrap_or_default().into(),
                state: "unknown".into(),
            });
        }
    }
    (reap, foreign)
}
/// Drop this run's own helpers from the foreign list: plugin helpers its helper-library records hold
/// (matched by library ID or generation) and its own lease host. Their own cleanup owns them; a
/// sweep that runs while the run's lease is still held must not report them as someone else's.
fn exclude_own(foreign: Vec<crate::storage::ForeignPendingHelper>, own: &BTreeSet<String>, host: Option<&str>)
    -> Vec<crate::storage::ForeignPendingHelper> {
    let generation = |id: &str| uuid::Uuid::parse_str(id).map(|id| id.to_string()).unwrap_or_else(|_| id.to_ascii_lowercase());
    foreign.into_iter().filter(|helper| Some(helper.library_id.as_str()) != host
        && !own.contains(&helper.library_id) && !own.contains(&generation(&helper.global_id))).collect()
}
/// Fingerprint of a helper library package: for each function, sorted by ID, "<FunctionID>\t<sha256 of its
/// code>" (Body.Code when Body is JSON text with a Code string, else Body), joined with "\n", then sha256.
/// Installation rewrites identity and scope metadata only, so an installed helper keeps its package's
/// fingerprint. known_helpers.rs lists every released Herbert helper's fingerprint.
fn helper_fingerprint(package: &[u8]) -> Option<String> {
    let package: Value = serde_json::from_slice(package.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(package)).ok()?;
    let functions = package["Library"]["Functions"].as_array().filter(|functions| !functions.is_empty())?;
    let mut lines = functions.iter().map(|function| {
        let body = function["Body"].as_str().unwrap_or("");
        let code = serde_json::from_str::<Value>(body).ok().and_then(|parsed| parsed["Code"].as_str().map(str::to_owned)).unwrap_or_else(|| body.to_owned());
        Some(format!("{}\t{}", function["FunctionID"].as_str()?, crate::storage::sha256(code.as_bytes())))
    }).collect::<Option<Vec<_>>>()?;
    lines.sort();
    Some(crate::storage::sha256(lines.join("\n").as_bytes()))
}
fn known_helper(package: &[u8]) -> bool {
    helper_fingerprint(package).is_some_and(|fingerprint| known_helpers::KNOWN_HELPER_FINGERPRINTS.contains(&fingerprint.as_str()))
}
impl Keeper {
    pub(super) fn contains(&self, generation: &str) -> bool {
        self.resources.contains(generation)
    }
    pub(super) fn uses_library(&self, library: &str) -> bool {
        self.resources.iter().any(|generation| {
            uuid::Uuid::parse_str(generation).is_ok_and(|id|
                library == format!("H{}", &id.simple().to_string()[..29]))
        })
    }
}
impl Drop for Keeper {
    fn drop(&mut self) {
        self.stop.cancel();
        self.task.abort();
    }
}
fn issue(message: &str) -> Diagnostic {
    Diagnostic::new("SERVER_LEASE_FAILED", message)
}

/// The run a lease heartbeat reports to. The spawned heartbeat holds it weakly, so a keeper that
/// outlives its run (teardown could not release the lease) never keeps that run's storage,
/// activity and caches alive. Tests pass the run itself.
trait LeaseRun {
    fn run(&self) -> Option<Arc<Run>>;
}
impl LeaseRun for Arc<Run> {
    fn run(&self) -> Option<Arc<Run>> { Some(self.clone()) }
}
impl LeaseRun for std::sync::Weak<Run> {
    fn run(&self) -> Option<Arc<Run>> { self.upgrade() }
}
#[allow(clippy::too_many_arguments)]
async fn heartbeat(connection: Arc<Connection>, run: std::sync::Weak<Run>, library: String, operation: String,
    token: CancellationToken, lost: Arc<std::sync::atomic::AtomicBool>, interval: Duration, expiry: Duration, acquired: Instant) {
    let renew = |token: CancellationToken| {
        let (connection, library, operation) = (connection.clone(), library.clone(), operation.clone());
        async move { connection.lease_call(&library, json!({"action":"renew","operationId":operation}), &token).await }
    };
    keep_renewing(renew, &run, &token, &lost, interval, expiry, MARGIN, acquired).await;
}

/// Renew until stopped. A failed renewal is retried within the lease window;
/// an explicit refusal, or reaching the window's end without a confirmed
/// renewal, marks the lease lost. Losing it never cancels the run: new helper
/// work stops at `lease_gate`, Direct reads and cleanup go on.
///
/// The window is measured from when the acquire or last accepted renewal was sent (the
/// server cannot have renewed earlier) and ends `margin` before it can expire.
/// The loop never sleeps past that end, so `lost` is set on time.
async fn keep_renewing<F, Fut, R: LeaseRun>(mut renew: F, run: &R, token: &CancellationToken,
    lost: &std::sync::atomic::AtomicBool, interval: Duration, expiry: Duration, margin: Duration, acquired: Instant)
where F: FnMut(CancellationToken) -> Fut, Fut: std::future::Future<Output = Result<Value, Diagnostic>> {
    let mut held_from = acquired;
    let mut failure = None;
    loop {
        let deadline = held_from + expiry.saturating_sub(margin);
        let wait = interval.min(deadline.saturating_duration_since(Instant::now()));
        tokio::select! { _ = token.cancelled() => break, _ = tokio::time::sleep(wait) => {} }
        if Instant::now() >= deadline {
            mark_lost(run, lost, failure.take()).await;
            break;
        }
        // Bound a renewal by the remaining window. A lost or late response
        // never extends the client's assurance.
        let sent = Instant::now();
        match tokio::time::timeout(deadline.saturating_duration_since(sent), renew(token.clone())).await {
            Ok(Ok(value)) if value["accepted"] == true => { held_from = sent; failure = None; }
            Ok(Ok(_)) | Err(_) => {
                mark_lost(run, lost, None).await;
                break;
            }
            Ok(Err(error)) => {
                if let Some(run) = run.run() {
                    run.activity.note("Server lease", "Heartbeat failed; retrying before lease expiry");
                }
                failure = Some(error);
            }
        }
    }
}

async fn mark_lost(run: &impl LeaseRun, lost: &std::sync::atomic::AtomicBool, cause: Option<Diagnostic>) {
    lost.store(true, std::sync::atomic::Ordering::SeqCst);
    let mut diagnostic = lease_lost();
    if let Some(cause) = cause {
        diagnostic.context.insert("cause".into(), json!(cause.code));
    }
    // A run already released has no journal left to record into.
    if let Some(run) = run.run() {
        let _ = record_diagnostic(&run, diagnostic).await;
    }
}
fn resource(
    identity: &crate::native_artifact_store::NativeIdentity,
    role: &str,
) -> Result<Value, Diagnostic> {
    let id = identity
        .global_id
        .as_deref()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or_else(|| issue("The temporary helper has no generation ID."))?;
    if identity.id != format!("H{}", &id.simple().to_string()[..29])
        || !(identity
            .marker
            .lines()
            .any(|line| line == format!("Herbert artifact {id}"))
            || identity.marker == format!("Herbert temporary helper; lease {id}"))
    {
        return Err(issue(
            "The Epicor object is not an identified Herbert temporary helper.",
        ));
    }
    Ok(json!({"libraryId":identity.id,"globalId":id,"role":role}))
}

impl Host {
    /// Completion sweep. Discovery reads the native host marker only. This run
    /// claims and reaps just the generations its own journal records, plus
    /// generations without an authorization reference that a local journal for
    /// the same caller, endpoint and company records exactly. It
    /// installs no host, calls no other run's host and never observes or claims
    /// a foreign or unknown generation: those are reported as foreign-pending.
    pub(super) async fn sweep_server_helpers(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
    ) -> Result<(), Diagnostic> {
        let journal = collection::snapshot(run).await?;
        let run_id = journal.run_id.clone();
        let cancel = CancellationToken::new();
        let mut query: QuerySpec = serde_json::from_value(json!({
            "tables":[{"schema":"Ecf","table":"EfxLibrary","alias":"Library","companyField":null}],
            "fields":[{"table":"Library","field":"LibraryID","alias":"LibraryID"},
                {"table":"Library","field":"GlobalID","alias":"GlobalID"},
                {"table":"Library","field":"Notes","alias":"Notes"}],
            "criteria":[{"table":"Library","field":"Notes","operator":"LIKE","parameter":"marker"},
                {"table":"Library","field":"OwnedByCompany","operator":"=","parameter":"company"}],
            "parameters":{"marker":"%Herbert%","company":connection.company},
            "order":[{"field":"LibraryID","descending":false}],"pageSize":24,"page":1
        }))
        .map_err(|_| issue("The server helper discovery query is invalid."))?;
        self.query_metadata(
            &mut query,
            connection,
            Some(run),
            None,
            &run_id,
            false,
            &cancel,
        )
        .await?;
        let mut discovered = Vec::new();
        loop {
            let arguments = crate::baq::build(&query, &connection.company)?;
            let response = connection
                .native_read(
                    "Ice.BO.DynamicQuerySvc",
                    "Execute",
                    arguments,
                    false,
                    &cancel,
                )
                .await?;
            let result = response.value.get("returnObj").unwrap_or(&response.value);
            if result["Errors"]
                .as_array()
                .is_some_and(|errors| !errors.is_empty())
            {
                return Err(issue("Epicor helper discovery returned errors."));
            }
            let rows = result["Results"]
                .as_array()
                .ok_or_else(|| issue("Epicor helper discovery omitted its result rows."))?;
            for row in rows {
                let id = row["LibraryID"]
                    .as_str()
                    .ok_or_else(|| issue("A helper discovery row has no identity."))?;
                let generation = row["GlobalID"]
                    .as_str()
                    .and_then(|s| uuid::Uuid::parse_str(s).ok())
                    .ok_or_else(|| issue("A helper discovery row has no generation."))?;
                let notes = row["Notes"].as_str().unwrap_or("");
                if id == format!("H{}", &generation.simple().to_string()[..29])
                    && (notes
                        .lines()
                        .any(|line| line == format!("Herbert artifact {generation}"))
                        || notes == format!("Herbert temporary helper; lease {generation}"))
                {
                    let role = if notes.starts_with("Herbert lease host v1") {
                        "host"
                    } else {
                        "plugin"
                    };
                    discovered.push((generation, json!({"libraryId":id,"globalId":generation,"role":role})));
                }
            }
            if rows.len() < 24 {
                break;
            }
            query.page += 1;
        }
        let own = journal_generations(&journal);
        // A resource without an authorization reference is attributable only
        // through an exact local journal record with the same caller, endpoint
        // and company. Records that carry a reference belong to their own run's
        // recovery, not to this run.
        let mut journaled = BTreeSet::new();
        if discovered.iter().any(|(generation, _)| !own.contains_key(generation)) {
            let root = self.state_root.clone();
            let others = blocking(move || crate::recovery::RunIndex::open(&root)?.journals()).await.unwrap_or_default();
            for other in others.iter().filter(|other| other.run_id != run_id && other.endpoint == journal.endpoint
                && other.company == journal.company && other.caller == journal.caller) {
                journaled.extend(journal_generations(other).into_iter().filter(|(_, reference)| reference.is_none()).map(|(generation, _)| generation));
            }
        }
        let (mut reap, foreign) = partition_discovered(discovered, &own, &journaled);
        let root = self.state_root.clone();
        let holder = run_id.clone();
        let own_helpers = blocking(move || crate::helper_libraries::for_holder(&root, &holder)).await
            .map(|(files, _)| files.into_iter().flat_map(|file| {
                let generation = uuid::Uuid::parse_str(&file.lease.instance.global_id)
                    .map(|id| id.to_string()).unwrap_or_else(|_| file.lease.instance.global_id.to_ascii_lowercase());
                [file.lease.instance.library_id, generation]
            }).collect::<BTreeSet<_>>())
            .unwrap_or_default();
        // Only this run's own host may claim. The sweep installs none and never
        // borrows another run's host; without one, owned items stay pending.
        let host = self.server_leases.lock().await.get(&run_id).map(|keeper| keeper.library.clone());
        // Leftovers of earlier Herbert versions are removed when their code is a released Herbert helper;
        // anything that only looks like one is kept and reported.
        let foreign = exclude_own(foreign, &own_helpers, host.as_deref());
        let foreign = self.reap_leftovers(run, connection, host.as_deref(), foreign, &cancel).await;
        self.record_foreign_pending(run, foreign).await?;
        if reap.is_empty() {
            return Ok(());
        }
        let Some(library) = host else {
            let mut error = Diagnostic::new("SERVER_CLEANUP_PENDING", "Temporary helpers owned by this run remain on the server, and this run has no active host to clean them. Recover this run to finish cleanup.");
            error.context.insert("helpers".into(), json!(reap.len()));
            return Err(error);
        };
        let library = library.as_str();
        let _progress =
            run.activity
                .begin("Server cleanup", None, "Checking expired helper leases");
        // Register only owned generations, so an unregistered leftover of this
        // run can be claimed. Nothing foreign is sent to the registry.
        connection
            .lease_call(
                library,
                json!({"action":"observe","resources":reap.iter().map(|(_, item)| item.clone()).collect::<Vec<_>>()}),
                &cancel,
            )
            .await?;
        // Keep the responding host until the other cleanup acknowledgements finish.
        reap.sort_by_key(|(_, item)| item["libraryId"].as_str() == Some(library));
        let mut errors = Vec::new();
        for (generation, item) in reap {
            if item["libraryId"].as_str() == Some(library) && !errors.is_empty() {
                continue;
            }
            let id = item["libraryId"].as_str().unwrap_or_default().to_owned();
            let target = generation.to_string();
            // Owned items are reaped under their original reference; reference-less items
            // (no reference) under the exact journal match established above.
            let result = match own.get(&generation).cloned().flatten() {
                Some(reference) => self.with_owned_cleanup(run, connection, reference,
                    self.cleanup_leased_helper(connection, library, &id, &target)).await,
                None => self.cleanup_leased_helper(connection, library, &id, &target).await,
            };
            if let Err(error) = result {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(issue(&format!(
                "{} helper cleanup operation(s) remain pending. {}",
                errors.len(),
                errors
                    .iter()
                    .map(|error| error.message.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            )))
        }
    }

    /// Replace this run's read-only foreign-pending list (latest sweep wins).
    async fn record_foreign_pending(&self, run: &Arc<Run>, foreign: Vec<crate::storage::ForeignPendingHelper>) -> Result<(), Diagnostic> {
        let storage = run.storage.clone();
        blocking(move || {
            let mut storage = storage.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
            if storage.journal.foreign_pending_helpers != foreign {
                storage.journal.foreign_pending_helpers = foreign;
                storage.checkpoint()?;
            }
            Ok(())
        }).await
    }

    /// Helpers that are not this run's (or an exact own-journal match) but look like Herbert's: remove each one
    /// whose function code is a released Herbert helper and that no running Herbert holds (the lease claim refuses
    /// a live lease). Keep and report everything else. Returns the kept helpers for the foreign-pending report.
    pub(super) async fn reap_leftovers(&self, run: &Arc<Run>, connection: &Connection, host: Option<&str>,
        foreign: Vec<crate::storage::ForeignPendingHelper>, cancel: &CancellationToken) -> Vec<crate::storage::ForeignPendingHelper> {
        let mut kept = Vec::new();
        let mut removed = 0;
        for mut helper in foreign {
            let recognized = crate::native_artifact_backend::export(connection, NativeArtifactType::FunctionLibrary, &helper.library_id, cancel)
                .await.is_ok_and(|package| known_helper(&package));
            if !recognized {
                helper.state = "unrecognized".into();
                let mut diagnostic = Diagnostic::new("CLEANUP_AUTHORITY_REQUIRED", "A helper on the Epicor server looks like Herbert's but does not match a released Herbert helper, so Herbert kept it. Check it in Epicor, and remove it there if nothing uses it.");
                diagnostic.context.insert("libraryId".into(), json!(helper.library_id));
                let _ = record_diagnostic(run, diagnostic).await;
                kept.push(helper);
                continue;
            }
            let Some(host) = host else {
                helper.state = "recognized".into();
                kept.push(helper);
                continue;
            };
            let item = json!({"libraryId":helper.library_id,"globalId":helper.global_id,"role":helper.role});
            let observed = connection.lease_call(host, json!({"action":"observe","resources":[item]}), cancel).await;
            match observed {
                Ok(_) => match self.cleanup_leased_helper(connection, host, &helper.library_id, &helper.global_id).await {
                    Ok(true) => {
                        removed += 1;
                        run.activity.record(json!({"event":"leftoverHelperRemoved","libraryId":helper.library_id}));
                    }
                    Ok(false) => { helper.state = "active".into(); kept.push(helper); }
                    Err(_) => { helper.state = "recognized".into(); kept.push(helper); }
                },
                Err(_) => { helper.state = "recognized".into(); kept.push(helper); }
            }
        }
        if removed > 0 {
            run.activity.note("Server cleanup", &format!("Removed {removed} helper(s) left on the server by an earlier Herbert version"));
        }
        kept
    }

    /// D2 gate before newly initiated helper work (helper call, transfer, lease
    /// acquire). Refuses after this run's lease was lost; otherwise returns the
    /// flag to check again when the call returns. Owned cleanup is not gated.
    pub(super) async fn lease_gate(&self, run: &Arc<Run>) -> Result<Option<Arc<std::sync::atomic::AtomicBool>>, Diagnostic> {
        if super::server_change_dispatch::cleanup_reference().is_some() || CLEANUP_WORK.try_with(|_| ()).is_ok() {
            return Ok(None);
        }
        let run_id = collection::snapshot(run).await?.run_id;
        match self.server_leases.lock().await.get(&run_id) {
            Some(keeper) if keeper.lost.load(std::sync::atomic::Ordering::SeqCst) => Err(lease_lost()),
            Some(keeper) => Ok(Some(keeper.lost.clone())),
            None => Ok(None),
        }
    }

    pub(super) async fn cleanup_leased_helper(
        &self,
        connection: &Connection,
        library: &str,
        id: &str,
        generation: &str,
    ) -> Result<bool, Diagnostic> {
        let cancel = CancellationToken::new();
        // An absent target needs no delete. This also reconciles an uncertain
        // native delete without repeating it against another generation.
        let absent = crate::native_artifact_backend::probe(
            connection,
            NativeArtifactType::FunctionLibrary,
            id,
            &cancel,
        )
        .await?
        .is_none();
        let result = connection
            .lease_call(
                library,
                json!({"action":"claim","globalId":generation}),
                &cancel,
            )
            .await?;
        let claim = result["resources"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["globalId"] == generation));
        let Some(claim) = claim else {
            if absent {
                return Ok(true);
            }
            let observed = connection.lease_call(library, json!({"action":"inspect"}), &cancel).await?;
            if observed["resources"].as_array().is_some_and(|items| items.iter().any(|item|
                item["globalId"] == generation && (item["active"] == true || item["state"] == "claimed"))) {
                return Ok(false);
            }
            return Err(issue(
                "The helper is active, already claimed, or not registered. Cleanup preserved it.",
            ));
        };
        self.remove_claimed_helper(connection, library, claim, &cancel)
            .await?;
        Ok(true)
    }

    async fn remove_claimed_helper(
        &self,
        connection: &Connection,
        library: &str,
        claim: &Value,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        use crate::native_artifact_backend as backend;
        let id = claim["libraryId"]
            .as_str()
            .ok_or_else(|| issue("The cleanup claim omitted its helper ID."))?;
        let generation = claim["globalId"]
            .as_str()
            .ok_or_else(|| issue("The cleanup claim omitted its generation."))?;
        connection
            .lease_call(
                library,
                json!({"action":"reap","globalId":generation,"claimId":claim["claimId"]}),
                cancel,
            )
            .await?;
        if let Some((identity, _)) =
            backend::probe(connection, NativeArtifactType::FunctionLibrary, id, cancel).await?
        {
            let identified = resource(&identity, claim["role"].as_str().unwrap_or(""))?;
            if identified["globalId"] != generation {
                return Err(issue(
                    "The helper generation changed; cleanup preserved it.",
                ));
            }
            backend::delete(
                connection,
                NativeArtifactType::FunctionLibrary,
                &identity,
                cancel,
            )
            .await?;
        }
        // A removed host cannot acknowledge its own deletion. Its claimed record
        // remains discoverable by the next host; no client journal is required.
        if id != library {
            connection
                .lease_call(
                    library,
                    json!({"action":"complete","globalId":generation,"claimId":claim["claimId"]}),
                    cancel,
                )
                .await?;
        }
        Ok(())
    }
    pub(super) async fn keep_helper_alive(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        identity: &crate::native_artifact_store::NativeIdentity,
        host_helper: bool,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let run_id = collection::snapshot(run).await?.run_id;
        let mut keepers = self.server_leases.lock().await;
        let item = resource(identity, if host_helper { "host" } else { "plugin" })?;
        let generation = item["globalId"].as_str().unwrap().to_owned();
        if let Some(keeper) = keepers.get_mut(&run_id) {
            if keeper.resources.contains(&generation) {
                return Ok(());
            }
            // D2: no new lease acquisition after this run's lease was lost.
            if keeper.lost.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(lease_lost());
            }
            let result = connection
                .lease_call(
                    &keeper.library,
                    json!({"action":"acquire",
                "operationId":run_id,"resources":[item]}),
                    cancel,
                )
                .await?;
            if result["accepted"] != true {
                return Err(issue("The server refused the helper lease."));
            }
            keeper.resources.insert(generation);
            return Ok(());
        }
        let host_identity = if host_helper {
            identity.clone()
        } else {
            self.prepare_host_utility(run, connection, cancel).await?;
            let installed = Box::pin(self.native_artifact_action_for(
                super::host_utility::OWNER,
                HostAction::ArtifactInstall {
                    alias: "host".into(),
                },
                &run_id,
                run,
                connection,
                cancel,
            ))
            .await?;
            let id = installed.value["id"]
                .as_str()
                .ok_or_else(|| issue("The host helper identity is absent."))?;
            crate::native_artifact_backend::probe(
                connection,
                NativeArtifactType::FunctionLibrary,
                id,
                cancel,
            )
            .await?
            .ok_or_else(|| issue("The host helper disappeared before lease registration."))?
            .0
        };
        let host_resource = resource(&host_identity, "host")?;
        let host_generation = host_resource["globalId"].as_str().unwrap().to_owned();
        // A host cannot acknowledge its own deletion. Recovery uses a new host
        // to finish those receipts before acquiring the same durable run ID.
        // Native absence is required. Do not revive leases or remove staging.
        let journal = collection::snapshot(run).await?;
        if journal.native_artifacts.iter().any(|operation| operation.artifacts.iter()
            .any(|artifact| !artifact.previous_generations.is_empty())) {
            self.acknowledge_removed_lease_resources(connection, &host_identity.id,
                &run_id, cancel).await?;
        }
        let acquire_sent = Instant::now();
        let result = connection
            .lease_call(
                &host_identity.id,
                json!({"action":"acquire",
            "operationId":run_id,"resources":[host_resource,item]}),
                cancel,
            )
            .await?;
        if result["accepted"] != true {
            return Err(issue("The server refused the operation lease."));
        }
        let stop = CancellationToken::new();
        let token = stop.clone();
        let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let library = host_identity.id.clone();
        let operation = run_id.clone();
        let connection = connection.clone();
        let run = Arc::downgrade(run);
        let flag = lost.clone();
        let task = tokio::spawn(heartbeat(connection, run, library, operation, token, flag, HEARTBEAT, EXPIRY, acquire_sent));
        keepers.insert(
            run_id,
            Keeper {
                library: host_identity.id,
                resources: BTreeSet::from([generation, host_generation]),
                lost,
                stop,
                task,
            },
        );
        Ok(())
    }

    async fn acknowledge_removed_lease_resources(
        &self, connection: &Connection, library: &str, run_id: &str,
        cancel: &CancellationToken,
    ) -> Result<(), Diagnostic> {
        let observed = connection.lease_call(library, json!({"action":"inspect"}), cancel).await?;
        let rows = observed["resources"].as_array()
            .ok_or_else(|| issue("The server omitted its lease resources."))?;
        for row in rows.iter().filter(|row| removed_receipt_for_run(row, run_id)) {
            let id = row["libraryId"].as_str()
                .ok_or_else(|| issue("A cleanup receipt omitted its helper ID."))?;
            if crate::native_artifact_backend::probe(connection,
                NativeArtifactType::FunctionLibrary, id, cancel).await?.is_none() {
                connection.lease_call(library, json!({"action":"complete",
                    "globalId":row["globalId"],"claimId":row["claimId"]}), cancel).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn release_server_lease(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
    ) -> Result<(), Diagnostic> {
        let run_id = collection::snapshot(run).await?.run_id;
        if let Some(keeper) = self.server_leases.lock().await.remove(&run_id) {
            keeper.stop.cancel();
            let library = keeper.library.clone();
            drop(keeper);
            let response = connection
                .lease_call(
                    &library,
                    json!({"action":"release","operationId":run_id}),
                    &CancellationToken::new(),
                )
                .await?;
            if response["accepted"] != true {
                return Err(issue(
                    "The operation lease was already expired or claimed for cleanup.",
                ));
            }
        }
        Ok(())
    }
}

fn removed_receipt_for_run(row: &Value, run_id: &str) -> bool {
    row["active"] == false && row["state"] == "claimed"
        && row["claimId"].as_str().is_some()
        && row["runs"].as_array().is_some_and(|runs| runs.iter().any(|id| id == run_id))
}

/// Heartbeat loss, sweep attribution and reference-less own-journal cleanup.
#[cfg(test)]
mod lease_batch_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct Case { _root: tempfile::TempDir, host: Arc<Host>, run: Arc<Run> }
    fn case() -> Case {
        let root = tempfile::tempdir().unwrap();
        let host = Host::new(root.path().join("state"), root.path().join("plugins"));
        let mut journal = storage::new_journal("lease-request".into());
        journal.endpoint = "https://erp.example.invalid/K/".into(); journal.company = "C".into(); journal.caller = "manager".into();
        let store = Storage::open(root.path().join("output").as_path(), false, journal).unwrap();
        let run = Arc::new(Run { storage: Arc::new(std::sync::Mutex::new(store)), cancel: CancellationToken::new(), paused: AtomicBool::new(false),
            wake: Notify::new(), active: AtomicBool::new(true), network: true, helper_restore_attempts: Mutex::new(Default::default()),
            activity: Arc::new(Default::default()), shared_reads: Default::default(), helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None) });
        Case { _root: root, host, run }
    }
    fn run_id(case: &Case) -> String { case.run.storage.lock().unwrap().journal.run_id.clone() }
    async fn keeper(case: &Case, lost: bool) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(lost));
        case.host.server_leases.lock().await.insert(run_id(case), Keeper { library: "HOST".into(), resources: BTreeSet::new(),
            lost: flag.clone(), stop: CancellationToken::new(), task: tokio::spawn(async {}) });
        flag
    }
    fn diagnostics(case: &Case, code: &str) -> Vec<Diagnostic> {
        case.run.storage.lock().unwrap().journal.diagnostics.iter().filter(|d| d.code == code).cloned().collect()
    }

    #[tokio::test]
    async fn heartbeat_retries_until_expiry_before_marking_lost() {
        let case = case();
        let lost = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let started = Instant::now();
        let renew = |_token: CancellationToken| { calls.fetch_add(1, Ordering::SeqCst); async { Err(Diagnostic::new("TRANSPORT_FAILED", "fixture")) } };
        keep_renewing(renew, &case.run, &CancellationToken::new(), &lost, Duration::from_millis(10), Duration::from_millis(80), Duration::ZERO, Instant::now()).await;
        assert!(lost.load(Ordering::SeqCst));
        assert!(started.elapsed() >= Duration::from_millis(80), "not marked lost before the lease window ends");
        assert!(calls.load(Ordering::SeqCst) >= 2, "failed renewals are retried within the window");
        let lost_diagnostics = diagnostics(&case, "SERVER_LEASE_LOST");
        assert_eq!(lost_diagnostics.len(), 1);
        assert_eq!(lost_diagnostics[0].context["cause"], "TRANSPORT_FAILED");
        assert!(!case.run.cancel.is_cancelled(), "losing the lease never cancels the run");
        // A renewal that succeeds within the window keeps the lease.
        let kept = AtomicBool::new(false);
        let stop = CancellationToken::new();
        let count = AtomicUsize::new(0);
        let renew = |_token: CancellationToken| { let n = count.fetch_add(1, Ordering::SeqCst); if n >= 3 { stop.cancel(); }
            async move { if n == 0 { Err(Diagnostic::new("TRANSPORT_FAILED", "fixture")) } else { Ok(json!({"accepted":true})) } } };
        keep_renewing(renew, &case.run, &stop, &kept, Duration::from_millis(10), Duration::from_millis(500), Duration::ZERO, Instant::now()).await;
        assert!(!kept.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn lease_is_lost_by_sent_time_plus_expiry_minus_margin_without_oversleeping() {
        let case = case();
        // A heartbeat interval longer than the window must not delay the loss.
        let lost = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let started = Instant::now();
        let renew = |_token: CancellationToken| { calls.fetch_add(1, Ordering::SeqCst); async { Ok(json!({"accepted":true})) } };
        keep_renewing(renew, &case.run, &CancellationToken::new(), &lost, Duration::from_secs(30), Duration::from_millis(100), Duration::from_millis(40), started).await;
        assert!(lost.load(Ordering::SeqCst));
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(60) && elapsed < Duration::from_secs(5), "lost at expiry minus margin, not after the 30 s interval: {elapsed:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        // An accepted renewal counts from when it was sent: a slow response cannot extend the lease past
        // sent + expiry - margin, and a response that would arrive after that point is not waited for.
        let lost = AtomicBool::new(false);
        let started = Instant::now();
        let renew = |_token: CancellationToken| async { tokio::time::sleep(Duration::from_millis(150)).await; Ok(json!({"accepted":true})) };
        keep_renewing(renew, &case.run, &CancellationToken::new(), &lost, Duration::from_millis(10), Duration::from_millis(200), Duration::from_millis(40), started).await;
        assert!(lost.load(Ordering::SeqCst));
        assert!(started.elapsed() < Duration::from_millis(330), "first renewal sent at ~10 ms is held only until ~170 ms; the second cannot finish in time: {:?}", started.elapsed());
    }

    #[tokio::test]
    async fn heartbeat_loss_stops_new_helper_use_and_continues_direct_reads() {
        let case = case();
        assert!(case.host.lease_gate(&case.run).await.unwrap().is_none(), "a Direct run has no lease and is never gated");
        let flag = keeper(&case, false).await;
        assert!(case.host.lease_gate(&case.run).await.unwrap().is_some());
        flag.store(true, Ordering::SeqCst);
        assert_eq!(case.host.lease_gate(&case.run).await.unwrap_err().code, "SERVER_LEASE_LOST");
        // Cleanup of journal-owned resources is not new helper work.
        assert!(as_cleanup(case.host.lease_gate(&case.run)).await.unwrap().is_none());
        // New lease acquisition is refused through the existing keeper path.
        let generation = uuid::Uuid::new_v4();
        let identity = crate::native_artifact_store::NativeIdentity { id: format!("H{}", &generation.simple().to_string()[..29]), company: "C".into(),
            row_id: uuid::Uuid::new_v4().to_string(), global_id: Some(generation.to_string()), owner: None, updatable: None, marker: format!("Herbert artifact {generation}") };
        let connection = Arc::new(Connection::new(url::Url::parse("https://erp.example.invalid/K/").unwrap(), "C".into(), AuthMode::BasicApiKey,
            crate::input::Credentials { username: "manager".into(), password: "fixture".into(), api_key: Some("fixture".into()) }, OperatingSettings::default(), Budget::new(1).unwrap()).unwrap());
        assert_eq!(case.host.keep_helper_alive(&case.run, &connection, &identity, false, &CancellationToken::new()).await.unwrap_err().code, "SERVER_LEASE_LOST");
        assert!(!case.run.cancel.is_cancelled(), "the run continues: Direct reads do not consult the lease");
    }

    #[test]
    fn call_in_flight_at_lease_loss_is_recorded_uncertain_and_not_retried() {
        let flag = Some(Arc::new(AtomicBool::new(false)));
        assert_eq!(after_leased_call(&flag, Ok::<_, Diagnostic>(7)).unwrap(), 7);
        flag.as_ref().unwrap().store(true, Ordering::SeqCst);
        let error = after_leased_call(&flag, Ok::<_, Diagnostic>(7)).unwrap_err();
        assert_eq!(error.code, "SERVER_LEASE_LOST_DURING_CALL");
        assert!(!error.retryable, "an uncertain helper call is never replayed");
        let failed = after_leased_call(&flag, Err::<(), _>(Diagnostic::new("TRANSPORT_FAILED", "fixture"))).unwrap_err();
        assert_eq!(failed.code, "SERVER_LEASE_LOST_DURING_CALL");
        assert_eq!(failed.context["callResult"], "TRANSPORT_FAILED");
        assert!(after_leased_call(&None, Ok::<_, Diagnostic>(1)).is_ok(), "no lease, no gate");
    }

    fn generation_record(generation: uuid::Uuid, reference: Option<crate::server_changes::AuthorizationRef>) -> crate::native_artifact_store::ArtifactRecord {
        serde_json::from_value(json!({"declaration":{"alias":"reader","type":"functionLibrary","file":"reader.efxj","lifetime":"temporary","policy":"createInstance","effects":{"description":"Fixture"}},
            "authorization":reference,"target":"T","owner":uuid::Uuid::new_v4(),"files":{},"phase":"installed",
            "identity":{"id":format!("H{}", &generation.simple().to_string()[..29]),"company":"C","rowId":"row","globalId":generation.to_string(),"owner":"manager","updatable":null,"marker":format!("Herbert artifact {generation}")},
            "replacedIdentity":null,"backupFile":null,"diagnostic":null})).unwrap()
    }
    fn journal_with(run_id: &str, caller: &str, artifacts: Vec<crate::native_artifact_store::ArtifactRecord>) -> crate::storage::RunJournal {
        let mut journal = storage::new_journal("r".into());
        journal.run_id = run_id.into(); journal.endpoint = "https://erp.example.invalid/K/".into(); journal.company = "C".into(); journal.caller = caller.into();
        journal.native_artifacts.push(serde_json::from_value(json!({"schemaVersion":1,"runId":run_id,"plugin":"functions",
            "scope":{"endpoint":"https://erp.example.invalid/K/","company":"C","caller":caller},"artifacts":artifacts})).unwrap());
        journal
    }
    fn item(generation: uuid::Uuid) -> (uuid::Uuid, Value) {
        (generation, json!({"libraryId":format!("H{}", &generation.simple().to_string()[..29]),"globalId":generation,"role":"plugin"}))
    }

    #[test]
    fn completion_sweep_reaps_only_this_runs_generations_and_reports_foreign_pending() {
        let (mine, previous, foreign) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let reference = crate::server_changes::AuthorizationRef { grant_id: "g".into(), material_digest: "d".into(), request_id: "r".into() };
        let mut current = generation_record(mine, Some(reference.clone()));
        current.previous_generations.push(generation_record(previous, Some(reference.clone())));
        let journal = journal_with("run-a", "manager", vec![current]);
        let own = journal_generations(&journal);
        assert_eq!(own.get(&mine), Some(&Some(reference.clone())));
        assert!(own.contains_key(&previous), "earlier generations of this run's capability are its own");
        let (reap, pending) = partition_discovered(vec![item(mine), item(previous), item(foreign)], &own, &BTreeSet::new());
        assert_eq!(reap.iter().map(|(g, _)| *g).collect::<Vec<_>>(), vec![mine, previous]);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].global_id, foreign.to_string());
        assert_eq!(pending[0].state, "unknown", "activity cannot be known without the foreign host");
        // Another run's operation inside this journal file is not this run's.
        let mut mixed = journal.clone();
        mixed.native_artifacts[0].run_id = "run-b".into();
        assert!(journal_generations(&mixed).is_empty());
    }

    #[test]
    fn none_reference_resource_with_exact_journal_match_is_cleaned() {
        let legacy = uuid::Uuid::new_v4();
        let other = journal_with("run-old", "manager", vec![generation_record(legacy, None)]);
        let journaled = journal_generations(&other).into_iter().filter(|(_, r)| r.is_none()).map(|(g, _)| g).collect::<BTreeSet<_>>();
        let (reap, pending) = partition_discovered(vec![item(legacy)], &Default::default(), &journaled);
        assert_eq!(reap.len(), 1);
        assert!(pending.is_empty());
    }

    #[test]
    fn none_reference_resource_with_generation_caller_endpoint_or_company_mismatch_is_preserved() {
        let legacy = uuid::Uuid::new_v4();
        // Recorded for another caller: its operation scope does not match its journal, or
        // the sweep's caller/endpoint/company filter excludes the journal entirely.
        let mut other = journal_with("run-old", "someone-else", vec![generation_record(legacy, None)]);
        other.native_artifacts[0].scope.caller = "manager".into();
        assert!(journal_generations(&other).is_empty(), "operation scope must match its own journal");
        // A referenced record from another run is that run's recovery, not an own-journal match.
        let referenced = journal_with("run-old", "manager", vec![generation_record(legacy, Some(crate::server_changes::AuthorizationRef { grant_id: "g".into(), material_digest: "d".into(), request_id: "r".into() }))]);
        let journaled = journal_generations(&referenced).into_iter().filter(|(_, r)| r.is_none()).map(|(g, _)| g).collect::<BTreeSet<_>>();
        assert!(journaled.is_empty());
        // A different generation of the same library is not a match.
        let (reap, pending) = partition_discovered(vec![item(uuid::Uuid::new_v4())], &Default::default(), &BTreeSet::from([legacy]));
        assert!(reap.is_empty());
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn none_reference_resource_without_journal_is_preserved_and_reported_foreign_pending() {
        let unknown = uuid::Uuid::new_v4();
        let (reap, pending) = partition_discovered(vec![item(unknown)], &Default::default(), &BTreeSet::new());
        assert!(reap.is_empty());
        assert_eq!(pending[0].global_id, unknown.to_string());
        assert_eq!(pending[0].role, "plugin");
    }

    #[tokio::test]
    async fn foreign_pending_is_recorded_durably_and_replaced_by_the_latest_sweep() {
        let case = case();
        let report = crate::storage::ForeignPendingHelper { library_id: "HX".into(), global_id: uuid::Uuid::new_v4().to_string(), role: "host".into(), state: "unknown".into() };
        case.host.record_foreign_pending(&case.run, vec![report.clone()]).await.unwrap();
        assert_eq!(case.run.storage.lock().unwrap().journal.foreign_pending_helpers, vec![report]);
        let status = crate::progress_snapshot::full(&case.run.storage.lock().unwrap().journal);
        assert_eq!(status["serverChanges"]["foreignPending"][0]["state"], "unknown");
        assert!(status["serverChanges"]["resources"].as_array().unwrap().is_empty(), "foreign-pending items never appear in resources");
        case.host.record_foreign_pending(&case.run, vec![]).await.unwrap();
        assert!(case.run.storage.lock().unwrap().journal.foreign_pending_helpers.is_empty());
    }
}

#[cfg(test)]
mod recovery_receipt_tests {
    use super::*;
    #[test]
    fn recovery_only_acknowledges_inactive_claimed_resources_for_its_run() {
        let row = json!({"active":false,"state":"claimed","claimId":"claim","runs":["run"]});
        assert!(removed_receipt_for_run(&row, "run"));
        assert!(!removed_receipt_for_run(&row, "other"));
        for (field, value) in [("active", json!(true)), ("state", json!("active")),
            ("claimId", Value::Null)] {
            let mut changed = row.clone();
            changed[field] = value;
            assert!(!removed_receipt_for_run(&changed, "run"));
        }
    }
}

#[cfg(test)]
#[path = "lease_scenario_tests.rs"]
mod lease_scenario_tests;
