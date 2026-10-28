//! Server activity leases. Local files are not evidence that a helper is active.
use super::*;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const HEARTBEAT: Duration = Duration::from_secs(30);
const EXPIRY: Duration = Duration::from_secs(300);

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
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
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
            "The native object is not an identified Herbert temporary helper.",
        ));
    }
    Ok(json!({"libraryId":identity.id,"globalId":id,"role":role}))
}

impl Host {
    /// Read the native host marker. No local journal or creator filter is used.
    pub(super) async fn sweep_server_helpers(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
    ) -> Result<(), Diagnostic> {
        let run_id = collection::snapshot(run).await?.run_id;
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
        let mut libraries = Vec::new();
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
                return Err(issue("Native helper discovery returned errors."));
            }
            let rows = result["Results"]
                .as_array()
                .ok_or_else(|| issue("Native helper discovery omitted its result rows."))?;
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
                    if role == "host" {
                        libraries.push(id.to_owned());
                    }
                    discovered.push(json!({"libraryId":id,"globalId":generation,"role":role}));
                }
            }
            if rows.len() < 24 {
                break;
            }
            query.page += 1;
        }
        if discovered.is_empty() {
            return Ok(());
        }
        let mut selected = self.server_leases.lock().await.get(&run_id).map(|keeper| keeper.library.clone());
        if selected.is_none() {
            for library in &libraries {
                if let Some((identity, _)) = crate::native_artifact_backend::probe(
                    connection, NativeArtifactType::FunctionLibrary, library, &cancel).await? {
                    // A cleanup host is active use too. Register it before using
                    // its registry so another cleaner cannot remove it mid-call.
                    if self.keep_helper_alive(run, connection, &identity, true, &cancel).await.is_ok() {
                        selected = Some(library.clone());
                        break;
                    }
                }
            }
        }
        let library = if let Some(library) = selected { library } else {
            // The normal mode gate still applies: Direct Only never installs
            // a host, even when older helpers need cleanup.
            self.prepare_host_utility(run, connection, &cancel).await?;
            let installed = Box::pin(self.native_artifact_action_for(
                super::host_utility::OWNER,
                HostAction::ArtifactInstall {
                    alias: "host".into(),
                },
                &run_id,
                run,
                connection,
                &cancel,
            ))
            .await?;
            let id = installed.value["id"]
                .as_str()
                .ok_or_else(|| issue("The cleanup host identity is absent."))?;
            let identity = crate::native_artifact_backend::probe(
                connection,
                NativeArtifactType::FunctionLibrary,
                id,
                &cancel,
            )
            .await?
            .ok_or_else(|| issue("The cleanup host disappeared."))?
            .0;
            self.keep_helper_alive(run, connection, &identity, true, &cancel)
                .await?;
            id.to_owned()
        };
        let library = library.as_str();
        let _progress =
            run.activity
                .begin("Server cleanup", None, "Checking expired helper leases");
        connection
            .lease_call(
                library,
                json!({"action":"observe","resources":discovered}),
                &cancel,
            )
            .await?;
        let claims = connection
            .lease_call(library, json!({"action":"claim"}), &cancel)
            .await?;
        let mut resources = claims["resources"]
            .as_array()
            .ok_or_else(|| issue("The server omitted its cleanup claims."))?
            .clone();
        // Keep the responding host until the other cleanup acknowledgements finish.
        resources.sort_by_key(|r| r["libraryId"].as_str() == Some(library));
        let mut errors = Vec::new();
        for item in resources {
            if item["libraryId"].as_str() == Some(library) && !errors.is_empty() {
                continue;
            }
            if let Err(error) = self
                .remove_claimed_helper(connection, library, &item, &cancel)
                .await
            {
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
        let library = host_identity.id.clone();
        let operation = run_id.clone();
        let connection = connection.clone();
        let run = run.clone();
        let task = tokio::spawn(async move {
            let mut last_success = Instant::now();
            loop {
                tokio::select! { _ = token.cancelled() => break, _ = tokio::time::sleep(HEARTBEAT) => {} }
                // Bound a renewal by the remaining lease window, not an export
                // timeout. A lost response never extends the client's assurance.
                let remaining = EXPIRY.saturating_sub(last_success.elapsed());
                let result = tokio::time::timeout(
                    remaining,
                    connection.lease_call(
                        &library,
                        json!({"action":"renew","operationId":operation}),
                        &token,
                    ),
                )
                .await;
                match result {
                    Ok(Ok(value)) if value["accepted"] == true => last_success = Instant::now(),
                    Ok(Ok(_)) | Err(_) => {
                        let _ = record_diagnostic(
                            &run,
                            issue("The server lease expired. Collection stopped; start a new run."),
                        )
                        .await;
                        run.cancel.cancel();
                        break;
                    }
                    Ok(Err(error)) => {
                        run.activity.note(
                            "Server lease",
                            "Heartbeat failed; retrying before lease expiry",
                        );
                        if last_success.elapsed() >= EXPIRY {
                            let _ = record_diagnostic(&run, error).await;
                            run.cancel.cancel();
                            break;
                        }
                    }
                }
            }
        });
        keepers.insert(
            run_id,
            Keeper {
                library: host_identity.id,
                resources: BTreeSet::from([generation, host_generation]),
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
