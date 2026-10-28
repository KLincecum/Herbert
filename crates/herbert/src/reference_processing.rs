//! Generic local definition processors run before page definitions are discarded.
use super::*;
use crate::collection_output::PreparedRecord;

pub(super) fn reference_error(d: &Diagnostic) -> bool {
    d.code != "REFERENCE_SCAN_COVERAGE" && d.code != "USER_EDIT_CONFLICT"
}

pub(super) fn reference_context(journal: &storage::RunJournal, consumer: &str) -> Value {
    let producers = journal
        .artifacts
        .iter()
        .filter(|a| a.identity.category != consumer)
        .collect::<Vec<_>>();
    let inspected = producers
        .iter()
        .filter(|a| {
            a.diagnostics
                .iter()
                .any(|d| d.code == "REFERENCE_SCAN_COVERAGE")
        })
        .count();
    let partial = producers.iter().any(|a| {
        a.acquisition != Completeness::Collected
            || a.processing != "derived"
            || !a
                .diagnostics
                .iter()
                .any(|d| d.code == "REFERENCE_SCAN_COVERAGE")
            || a.diagnostics.iter().any(|d| {
                (d.code.starts_with("REFERENCE_") || d.code.starts_with("PROCESSOR_"))
                    && reference_error(d)
            })
    });
    json!({"references":journal.references,"producerCount":inspected,"producerReferenceErrors":partial})
}

/// Copy only the processor's input, not every retained file and run receipt.
/// The storage lock keeps the producer summary and reference index consistent.
pub(super) async fn current_reference_context(run: &Arc<Run>, consumer: &str) -> Result<Value, Diagnostic> {
    let shared = run.storage.clone();
    let consumer = consumer.to_owned();
    blocking(move || {
        let storage = shared.lock().map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?;
        Ok(reference_context(&storage.journal, &consumer))
    }).await
}

impl Host {
    /// Fill references for retained direct outputs collected before this processor
    /// was selected. Read verified files; never reacquire native definitions.
    pub(super) async fn process_existing_reference_inputs(
        &self,
        run: &Arc<Run>,
        processors: &[Plugin],
    ) -> Result<(), Diagnostic> {
        self.process_reference_inputs(run, processors, false).await
    }
    pub(super) async fn process_reference_inputs(
        &self,
        run: &Arc<Run>,
        processors: &[Plugin],
        force: bool,
    ) -> Result<(), Diagnostic> {
        if processors.is_empty() {
            return Ok(());
        }
        let journal = collection::snapshot(run).await?;
        let current = journal
            .artifacts
            .iter()
            .map(|a| storage::identity_key(&a.identity))
            .collect::<std::collections::BTreeSet<_>>();
        let mut sources = run
            .storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .manifest
            .artifacts
            .clone();
        for artifact in &journal.artifacts {
            sources.insert(storage::identity_key(&artifact.identity), artifact.clone());
        }
        for (key, mut artifact) in sources {
            if !current.contains(&key) {
                artifact.classification = "retainedReferenceInput".into();
            }
            if processors
                .iter()
                .any(|p| p.category.id == artifact.identity.category)
            {
                continue;
            }
            if force
                && journal
                    .definition_views
                    .get(&key)
                    .is_some_and(|f| f.role != "referenceInput")
            {
                continue;
            }
            if !force
                && current.contains(&key)
                && artifact.acquisition == Completeness::Collected
                && artifact.processing == "derived"
                && processors.iter().all(|p| {
                    artifact.diagnostics.iter().any(|d| {
                        d.code == "REFERENCE_SCAN_COVERAGE"
                            && d.context.get("processorSha256") == Some(&json!(p.hash))
                    })
                })
            {
                continue;
            }
            if matches!(
                artifact.acquisition,
                Completeness::VerifiedAbsent
                    | Completeness::VerifiedNotApplicable
                    | Completeness::Excluded
            ) {
                continue;
            }
            if run.cancel.is_cancelled() {
                return Err(Diagnostic::new(
                    "CANCELED",
                    "Reference processing was canceled.",
                ));
            }
            let name = format!(
                "{}.json",
                crate::output_layout::artifact_name(&artifact.identity)
            );
            let file = journal
                .definition_views
                .get(&storage::identity_key(&artifact.identity))
                .cloned()
                .or_else(|| {
                    artifact
                        .files
                        .iter()
                        .find(|f| f.path.rsplit('/').next() == Some(name.as_str()))
                        .cloned()
                });
            let loaded = if let Some(file) = file {
                let shared = run.storage.clone();
                let input = file.clone();
                blocking(move || {
                    let storage = shared.lock().map_err(|_| {
                        Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable.")
                    })?;
                    storage.verify(&input)?;
                    let bytes = std::fs::read(storage.confined(&input.path)?).map_err(|_| {
                        Diagnostic::new(
                            "REFERENCE_INPUT_UNAVAILABLE",
                            "The retained reference input is unavailable.",
                        )
                    })?;
                    let value =
                        herbert_contract::decode_local_json::<Value>(&bytes).map_err(|_| {
                            Diagnostic::new(
                                "REFERENCE_INPUT_UNPARSED",
                                "The retained reference input is not structured JSON.",
                            )
                        })?;
                    Ok((file, value))
                })
                .await
            } else {
                Err(Diagnostic::new(
                    "REFERENCE_INPUT_UNAVAILABLE",
                    "The producer has no retained structured input. Its files remain available; absence of references is not established.",
                ))
            };
            let (records, diagnostics, complete, file) = match loaded {
                Ok((file, definition)) => {
                    let (records, diagnostics, complete) = self
                        .process_references(
                            processors,
                            &artifact.identity,
                            &definition,
                            &journal.company,
                            &run.cancel,
                        )
                        .await;
                    (records, diagnostics, complete, Some(file))
                }
                Err(mut d) => {
                    d.context.insert("referenceInput".into(), json!(true));
                    d.identity = Some(artifact.identity.clone());
                    (vec![], vec![d], false, None)
                }
            };
            collection::update(run, move |storage| {
                {
                    crate::processing::replace_references(
                        storage,
                        &artifact.identity,
                        records.clone(),
                    );
                    artifact.dependencies = records
                        .iter()
                        .filter_map(|r| serde_json::from_value(r["identity"].clone()).ok())
                        .collect();
                }
                if let Some(mut input) = file {
                    if input.role != "processingInput" {
                        input.role = "referenceInput".into();
                    }
                    storage
                        .journal
                        .definition_views
                        .insert(storage::identity_key(&artifact.identity), input);
                }
                artifact.diagnostics.retain(|d| {
                    !d.code.starts_with("REFERENCE_")
                        && !d.code.starts_with("PROCESSOR_")
                        && !d.context.contains_key("processorCategory")
                        && d.context.get("referenceInput") != Some(&json!(true))
                });
                let prior_failure = artifact.acquisition != Completeness::Collected
                    || (artifact.processing != "derived"
                        && artifact
                            .diagnostics
                            .iter()
                            .any(|d| d.code != "USER_EDIT_CONFLICT"));
                artifact.processing = if prior_failure || diagnostics.iter().any(reference_error) {
                    "partial"
                } else {
                    "derived"
                }
                .into();
                artifact.diagnostics.extend(diagnostics);
                if complete {
                    match storage.record_artifact(artifact.clone()) {
                        Ok(()) => return Ok(()),
                        Err(mut error) => {
                            error.identity = Some(artifact.identity.clone());
                            artifact.diagnostics.push(error);
                            artifact.processing = "partial".into();
                            artifact.acquisition = Completeness::Partial;
                        }
                    }
                }
                storage
                    .manifest
                    .artifacts
                    .insert(storage::identity_key(&artifact.identity), artifact.clone());
                storage
                    .journal
                    .artifacts
                    .retain(|a| a.identity != artifact.identity);
                storage.journal.artifacts.push(artifact);
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    pub(super) async fn prepare_reference_page(
        &self,
        run: &Arc<Run>,
        records: Vec<(Identity, Value)>,
        cancel: &CancellationToken,
    ) -> Result<Vec<PreparedRecord>, Diagnostic> {
        let processors = run
            .collection_context
            .lock()
            .await
            .as_ref()
            .map(|c| c.processors.clone())
            .unwrap_or_default();
        let company = run
            .storage
            .lock()
            .map_err(|_| Diagnostic::new("RUN_STATE_FAILED", "Run state is unavailable."))?
            .journal
            .company
            .clone();
        self.prepare_reference_records(records, &processors, &company, cancel)
            .await
    }

    pub(super) async fn prepare_reference_records(
        &self,
        records: Vec<(Identity, Value)>,
        processors: &[Plugin],
        company: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<PreparedRecord>, Diagnostic> {
        let mut updates = Vec::with_capacity(records.len());
        for (identity, definition) in &records {
            if cancel.is_cancelled() {
                return Err(Diagnostic::new(
                    "CANCELED",
                    "Reference processing was canceled.",
                ));
            }
            if processors.is_empty()
                || processors
                    .iter()
                    .all(|p| p.category.id == identity.category)
            {
                updates.push(None);
                continue;
            }
            updates.push(Some(
                self.process_references(processors, identity, definition, company, cancel)
                    .await,
            ));
        }
        let mut prepared =
            blocking(move || crate::collection_output::prepare_page(records)).await?;
        for (record, update) in prepared.iter_mut().zip(updates) {
            if let Some((records, diagnostics, complete)) = update {
                record.set_reference_processing(records, diagnostics, complete);
            }
        }
        Ok(prepared)
    }
}

#[cfg(test)]
#[path = "reference_processing_tests.rs"]
mod tests;
