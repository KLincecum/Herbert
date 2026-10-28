//! Generic owned-resource lifecycle over exact helper contracts. No module roster.
use super::*;

impl Host {
    pub(super) async fn drive(
        &self,
        plugin: &Plugin,
        request: GuestRequest,
        connection: &Arc<Connection>,
        run: &Arc<Run>,
        cancel: &CancellationToken,
    ) -> Result<(GuestReply, Vec<FileRef>), Diagnostic> {
        Box::pin(self.drive_with_reply(plugin, request, connection, run, cancel, None)).await
    }

    pub(super) async fn drive_with_reply(
        &self,
        plugin: &Plugin,
        request: GuestRequest,
        connection: &Arc<Connection>,
        run: &Arc<Run>,
        cancel: &CancellationToken,
        reply: Option<GuestReply>,
    ) -> Result<(GuestReply, Vec<FileRef>), Diagnostic> {
        Box::pin(self.drive_with_sources(plugin, request, connection, run, cancel, reply, vec![])).await
    }

    pub(super) async fn drive_with_sources(
        &self,
        plugin: &Plugin,
        mut request: GuestRequest,
        connection: &Arc<Connection>,
        run: &Arc<Run>,
        cancel: &CancellationToken,
        reply: Option<GuestReply>,
        issued: Vec<FileRef>,
    ) -> Result<(GuestReply, Vec<FileRef>), Diagnostic> {
        // Authority and ownership come from the accepted download, never guest state.
        let (temporary_access, holder) = {
            let storage=run.storage.lock().map_err(|_|Diagnostic::new("RUN_STATE_FAILED","Run state is unavailable."))?;
            (storage.journal.temporary_access_enabled,storage.journal.run_id.clone())
        };
        let uses_temporary_state = plugin
            .category
            .helpers
            .iter()
            .flat_map(|helper| &helper.functions)
            .any(|function| function.resource.is_some());
        let _guard = if uses_temporary_state {
            Some(
                tokio::select! {_ = cancel.cancelled()=>return Err(Diagnostic::new("CANCELED","The row transfer was canceled.")),guard=self.helper_transfer_gate.lock()=>guard},
            )
        } else {
            None
        };
        if uses_temporary_state {
            let retained = run
                .storage
                .lock()
                .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
                .journal
                .helper_transfers
                .iter()
                .find(|record| {
                    request.identity.as_ref() == Some(&record.identity)
                        && matches!(record.phase.as_str(), "admitted" | "open")
                })
                .cloned();
            let retained_id = if let Some(retained) = retained {
                if !request.state.is_object() {
                    request.state = json!({});
                }
                request.state["_hostTransfer"] = json!({"operationId":retained.operation_id,"arguments":retained.close_arguments});
                request
                    .state
                    .pointer("/_hostTransfer/operationId")
                    .and_then(Value::as_str)
                    .map(String::from)
            } else {
                None
            };
            // A previous row must close before the next one admits any bytes.
            let errors = self
                .cleanup_helper_transfers(run, connection, retained_id.as_deref())
                .await;
            if !errors.is_empty() {
                return Err(errors[0].clone());
            }
        }
        // Keep the native dispatch future on the heap. The fetch and consumer
        // futures coexist, so neither should embed the full dispatch state.
        let mut result = Box::pin(self.drive_output_pipeline(
                plugin,
                request,
                connection,
                run,
                &holder,
                temporary_access,
                cancel,
                reply,
                issued,
            ))
            .await;
        if uses_temporary_state {
            // Canceling the download does not cancel teardown.
            let errors = self.cleanup_helper_transfers(run, connection, None).await;
            if !errors.is_empty() {
                match &mut result {
                    Ok((
                        GuestReply::Definition {
                            completeness,
                            diagnostics,
                            ..
                        },
                        _,
                    )) => {
                        *completeness = Completeness::Partial;
                        diagnostics.extend(errors);
                    }
                    Ok(_) => result = Err(errors[0].clone()),
                    Err(_) => {}
                }
            }
        }
        result
    }

    pub(super) async fn admit_helper_transfer(
        &self,
        plugin: &Plugin,
        claim: &HelperContract,
        function: &str,
        arguments: &Value,
        run: Option<&Arc<Run>>,
        identity: Option<&Identity>,
    ) -> Result<(), Diagnostic> {
        let declared = claim
            .functions
            .iter()
            .find(|declared| declared.function_id == function)
            .ok_or_else(|| {
                Diagnostic::new(
                    "PLUGIN_CAPABILITY_DENIED",
                    "The helper function is not declared.",
                )
            })?;
        let Some(resource) = &declared.resource else {
            return Ok(());
        };
        let (Some(run), Some(identity)) = (run, identity) else {
            return Err(Diagnostic::new(
                "RETRIEVAL_REQUIRED",
                "Owned server staging requires an accepted durable retrieval run.",
            ));
        };
        let operation = arguments
            .get(&resource.operation_argument)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Diagnostic::new("HELPER_RESOURCE_INVALID", "The owned operation is missing.")
            })?;
        let id = arguments
            .get(&resource.operation_id_argument)
            .and_then(Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .filter(|id| !id.is_nil())
            .ok_or_else(|| {
                Diagnostic::new(
                    "HELPER_RESOURCE_INVALID",
                    "Retain a nonempty operation UUID before dispatch.",
                )
            })?
            .to_string();
        let mut close_arguments = arguments.clone();
        close_arguments[&resource.operation_argument] = json!(resource.close_operation);
        let mut intent = crate::helpers::HelperTransfer {
            operation_id: id.clone(),
            identity: identity.clone(),
            claim: claim.clone(),
            function: function.into(),
            close_arguments,
            phase: "admitted".into(),
            plugin_hash: plugin.hash.clone(),
            dispatch_count: 1,
        };
        let begin = operation == resource.begin_operation;
        let bindings = resource.binding_arguments.clone();
        let storage = run.storage.clone();
        blocking(move || {
            let mut storage = storage
                .lock()
                .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
            if let Some(saved) = storage
                .journal
                .helper_transfers
                .iter_mut()
                .find(|saved| saved.operation_id == id)
            {
                if saved.identity != intent.identity
                    || saved.claim.library_id != intent.claim.library_id
                    || saved.function != intent.function
                    || bindings.iter().any(|key| {
                        saved.close_arguments.get(key) != intent.close_arguments.get(key)
                    })
                    || matches!(saved.phase.as_str(), "closed" | "notAdmitted") && begin
                {
                    return Err(Diagnostic::new(
                        "HELPER_RESOURCE_CONFLICT",
                        "The retained operation belongs to different facts or is already closed.",
                    ));
                }
                if begin {
                    saved.dispatch_count =
                        saved.dispatch_count.checked_add(1).ok_or_else(|| {
                            Diagnostic::new(
                                "HELPER_RESOURCE_COUNTER_OVERFLOW",
                                "The retained attempt counter exceeds its u64 representation.",
                            )
                        })?;
                    storage.checkpoint()?;
                }
            } else {
                if !begin {
                    return Err(Diagnostic::new(
                        "HELPER_RESOURCE_NOT_ADMITTED",
                        "This transfer has no retained local admission record.",
                    ));
                }
                intent.phase = "admitted".into();
                storage.journal.helper_transfers.push(intent);
                storage.checkpoint()?;
            }
            Ok(())
        })
        .await
    }

    pub(super) async fn observe_helper_transfer(
        &self,
        claim: &HelperContract,
        function: &str,
        arguments: &Value,
        response: &Value,
        run: &Arc<Run>,
    ) -> Result<(), Diagnostic> {
        let Some(resource) = claim
            .functions
            .iter()
            .find(|declared| declared.function_id == function)
            .and_then(|declared| declared.resource.as_ref())
        else {
            return Ok(());
        };
        let id = arguments
            .get(&resource.operation_id_argument)
            .and_then(Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or_else(|| {
                Diagnostic::new("HELPER_RESOURCE_INVALID", "The transfer UUID is invalid.")
            })?
            .to_string();
        let closed = response.get("oSuccess").and_then(Value::as_bool) == Some(true)
            && response
                .pointer(&resource.closed_pointer)
                .and_then(Value::as_bool)
                == Some(true);
        let rejected = response
            .pointer(&resource.admission_rejected_pointer)
            .and_then(Value::as_bool)
            == Some(true);
        let observations = resource
            .observations
            .iter()
            .filter_map(|observation| {
                response
                    .pointer(&observation.pointer)
                    .map(|value| (observation.argument.clone(), value.clone()))
            })
            .collect::<Vec<_>>();
        let acknowledge = resource.acknowledge_operation.clone().filter(|operation| {
            arguments
                .get(&resource.operation_argument)
                .and_then(Value::as_str)
                != Some(operation.as_str())
        });
        let operation_argument = resource.operation_argument.clone();
        let storage = run.storage.clone();
        blocking(move || {
            let mut storage = storage
                .lock()
                .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
            let record = storage
                .journal
                .helper_transfers
                .iter_mut()
                .find(|record| record.operation_id == id)
                .ok_or_else(|| {
                    Diagnostic::new(
                        "HELPER_RESOURCE_NOT_ADMITTED",
                        "The retained transfer record is missing.",
                    )
                })?;
            if rejected && record.dispatch_count == 1 {
                record.phase = "notAdmitted".into();
            } else if closed {
                if let Some(operation) = acknowledge {
                    record.phase = "closedPendingAcknowledgement".into();
                    record.close_arguments[&operation_argument] = json!(operation);
                } else {
                    record.phase = "closed".into();
                }
            } else if record.phase != "closed" {
                record.phase = "open".into();
            }
            for (argument, value) in observations {
                record.close_arguments[argument] = value;
            }
            storage.checkpoint()
        })
        .await
    }

    /// Record native Close before removing its closure proof. If the process
    /// stops between these steps, the retained arguments resume acknowledgment.
    async fn cleanup_helper_transfer(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        record: &crate::helpers::HelperTransfer,
    ) -> Result<(), Diagnostic> {
        let resource = record
            .claim
            .functions
            .iter()
            .find(|function| function.function_id == record.function)
            .and_then(|function| function.resource.as_ref())
            .ok_or_else(|| {
                Diagnostic::new(
                    "HELPER_RESOURCE_INVALID",
                    "The retained cleanup contract is missing.",
                )
            })?;
        let cleanup = CancellationToken::new();
        self.ensure_retained_helper(run, connection, record, None, &cleanup)
            .await?;
        let response = self
            .invoke_helper_claim(
                &record.claim,
                &record.function,
                record.close_arguments.clone(),
                connection,
                Some(run),
                Some(&record.identity),
                &cleanup,
            )
            .await?;
        if !resource_closed(resource, &response.value) {
            return Err(Diagnostic::new(
                "SERVER_CLEANUP_PENDING",
                "The helper did not confirm closure.",
            ));
        }
        self.observe_helper_transfer(
            &record.claim,
            &record.function,
            &record.close_arguments,
            &response.value,
            run,
        )
        .await?;

        let Some(acknowledge) = &resource.acknowledge_operation else {
            return Ok(());
        };
        if record
            .close_arguments
            .get(&resource.operation_argument)
            .and_then(Value::as_str)
            == Some(acknowledge.as_str())
        {
            return Ok(());
        }
        let arguments = run
            .storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal
            .helper_transfers
            .iter()
            .find(|saved| saved.operation_id == record.operation_id)
            .ok_or_else(|| {
                Diagnostic::new(
                    "HELPER_RESOURCE_NOT_ADMITTED",
                    "The retained transfer is missing.",
                )
            })?
            .close_arguments
            .clone();
        let response = self
            .invoke_helper_claim(
                &record.claim,
                &record.function,
                arguments.clone(),
                connection,
                Some(run),
                Some(&record.identity),
                &cleanup,
            )
            .await?;
        if !resource_closed(resource, &response.value) {
            return Err(Diagnostic::new(
                "SERVER_CLEANUP_PENDING",
                "The helper did not confirm removal of its closure records.",
            ));
        }
        self.observe_helper_transfer(
            &record.claim,
            &record.function,
            &arguments,
            &response.value,
            run,
        )
        .await
    }

    pub(super) async fn cleanup_helper_transfers(
        &self,
        run: &Arc<Run>,
        connection: &Arc<Connection>,
        keep: Option<&str>,
    ) -> Vec<Diagnostic> {
        let journal = match collection::snapshot(run).await {
            Ok(journal) => journal,
            Err(error) => return vec![error],
        };
        if let Err(error) = self.access_store.location().and_then(|root|
            crate::server_collection::ensure_run_settled(&root.join("server-collections"), &journal.run_id)) {
            return vec![error];
        }
        let records = match run.storage.lock() {
            Ok(storage) => storage
                .journal
                .helper_transfers
                .iter()
                .filter(|record| {
                    !matches!(record.phase.as_str(), "closed" | "notAdmitted")
                        && Some(record.operation_id.as_str()) != keep
                })
                .cloned()
                .collect::<Vec<_>>(),
            Err(_) => {
                return vec![Diagnostic::new(
                    "RUN_STATE_FAILED",
                    "Run state is unavailable for server cleanup.",
                )];
            }
        };
        let mut errors = Vec::new();
        for record in records {
            let result = self.cleanup_helper_transfer(run, connection, &record).await;
            let closed = result.is_ok();
            let storage = run.storage.clone();
            let id = record.operation_id.clone();
            let mut error = Diagnostic::new(
                "SERVER_CLEANUP_PENDING",
                format!(
                    "Owned server transfer {} still needs exact-ID cleanup; the retained record remains retryable. Cause: {}.",
                    record.operation_id,
                    result
                        .as_ref()
                        .err()
                        .map(|cause| format!("{}: {}", cause.code, cause.message))
                        .unwrap_or_default()
                ),
            );
            error.identity = Some(record.identity.clone());
            error.retryable = true;
            let saved_error = error.clone();
            if let Err(persistence) = blocking(move || {
                let mut storage = storage.lock().map_err(|_| {
                    Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                })?;
                let saved = storage
                    .journal
                    .helper_transfers
                    .iter_mut()
                    .find(|saved| saved.operation_id == id)
                    .ok_or_else(|| {
                        Diagnostic::new(
                            "HELPER_RESOURCE_NOT_ADMITTED",
                            "The retained transfer is missing.",
                        )
                    })?;
                // Keep durable Close evidence if only its acknowledgement failed.
                if closed {
                    saved.phase = "closed".into();
                } else if saved.phase != "closedPendingAcknowledgement" {
                    saved.phase = "cleanupPending".into();
                }
                if !closed {
                    storage.journal.diagnostics.push(saved_error);
                }
                storage.checkpoint()
            })
            .await
            {
                errors.push(persistence);
            }
            if !closed {
                errors.push(error);
            }
        }
        errors
    }
}

fn resource_closed(resource: &HelperResourceContract, response: &Value) -> bool {
    response.get("oSuccess").and_then(Value::as_bool) == Some(true)
        && response
            .pointer(&resource.closed_pointer)
            .and_then(Value::as_bool)
            == Some(true)
}
