//! Refresh materialized local reference artifacts after their producer index.
use super::*;

impl Host {
    pub(super) async fn invalidate_reference_outputs(
        &self,
        run: &Arc<Run>,
        processors: &[Plugin],
    ) -> Result<(), Diagnostic> {
        let categories = processors
            .iter()
            .map(|p| p.category.id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        collection::update(run,move|storage|{
            for artifact in &mut storage.journal.artifacts{
                if !categories.contains(&artifact.identity.category){continue;}
                artifact.processing="partial".into();artifact.diagnostics.retain(|d|d.code!="REFERENCE_REFRESH_PENDING");
                artifact.diagnostics.push(Diagnostic::new("REFERENCE_REFRESH_PENDING","This reference output has not yet been checked against the reprocessed source index."));
                storage.manifest.artifacts.insert(storage::identity_key(&artifact.identity),artifact.clone());
            }
            for category in &storage.journal.categories{if categories.contains(category){storage.journal.category_outcomes.insert(category.clone(),"discoveryIncomplete".into());}}
            Ok(())
        }).await
    }

    pub(super) async fn refresh_reference_artifacts(
        &self,
        run: &Arc<Run>,
        processors: &[Plugin],
    ) -> Result<(), Diagnostic> {
        for processor in processors {
            let saved = collection::snapshot(run).await?;
            let all =
                saved.scope == "fullBackup" && saved.categories.contains(&processor.category.id);
            let mut targets = saved
                .artifacts
                .iter()
                .filter(|a| a.identity.category == processor.category.id)
                .map(|a| (storage::identity_key(&a.identity), a.identity.clone()))
                .collect::<BTreeMap<_, _>>();
            if !all && targets.is_empty() {
                continue;
            }
            let context =
                super::reference_processing::reference_context(&saved, &processor.category.id);
            let canonical = saved
                .references
                .iter()
                .filter_map(|row| {
                    let id: Identity = serde_json::from_value(row["identity"].clone()).ok()?;
                    (id.category == processor.category.id)
                        .then(|| (storage::identity_key(&id), (id, row.clone())))
                })
                .collect::<BTreeMap<_, _>>();
            let request = GuestRequest {
                abi: PLUGIN_ABI,
                operation: "discover".into(),
                category: processor.category.id.clone(),
                company: saved.company.clone(),
                identity: None,
                cursor: None,
                page_size: None,
                state: context.clone(),
                response: None,
                error: None,
            };
            // Bulk collectors supply ready records with their inventory. Local
            // refresh still verifies each exact target against the canonical
            // retained index below, including old targets that became absent.
            // Do not reinterpret the new envelope as incomplete discovery.
            let reply = match processor.call(request, run.cancel.clone()).await? {
                GuestReply::InventoryData { inventory, .. } => *inventory,
                reply => reply,
            };
            let (identities, mut authoritative, mut diagnostics) =
                match reply {
                    GuestReply::Inventory {
                        identities,
                        next_cursor: None,
                        authoritative,
                        diagnostics,
                    } => (identities, authoritative, diagnostics),
                    GuestReply::Failure { diagnostic } => (vec![], false, vec![diagnostic]),
                    _ => (
                        vec![],
                        false,
                        vec![Diagnostic::new(
                            "PROCESSOR_REFERENCE_INVALID",
                            "A local reference consumer did not return its current inventory.",
                        )],
                    ),
                };
            let mut discovered = std::collections::BTreeSet::new();
            for identity in identities {
                let key = storage::identity_key(&identity);
                if identity.category != processor.category.id
                    || !super::identity_in_scope(&identity, &saved.company)
                    || !canonical.contains_key(&key)
                {
                    authoritative = false;
                    diagnostics.push(Diagnostic::new(
                        "PROCESSOR_REFERENCE_INVALID",
                        "A local consumer returned a reference outside its verified context.",
                    ));
                    continue;
                }
                discovered.insert(key.clone());
                if all {
                    targets.insert(key, identity);
                }
            }
            if canonical.keys().any(|key| !discovered.contains(key)) {
                authoritative = false;
                diagnostics.push(Diagnostic::new(
                    "PROCESSOR_REFERENCE_INVALID",
                    "The local consumer omitted a verified context identity.",
                ));
            }
            let mut exact_context = context;
            exact_context
                .as_object_mut()
                .expect("Reference context is an object")
                .remove("references");
            let mut rows = vec![];
            let mut outcomes = BTreeMap::new();
            for (key, identity) in targets {
                if run.cancel.is_cancelled() {
                    return Err(Diagnostic::new(
                        "CANCELED",
                        "Local reference refresh was canceled.",
                    ));
                }
                let (definition, completeness, item_diagnostics) = if let Some((_, expected)) =
                    canonical.get(&key).filter(|_| discovered.contains(&key))
                {
                    // Exact acquire needs one complete reference, not N copies of
                    // the entire index while N output files are reconstructed.
                    let mut state = exact_context.clone();
                    state["references"] = json!([expected]);
                    let request = GuestRequest {
                        abi: PLUGIN_ABI,
                        operation: "acquire".into(),
                        category: processor.category.id.clone(),
                        company: saved.company.clone(),
                        identity: Some(identity.clone()),
                        cursor: None,
                        page_size: None,
                        state,
                        response: None,
                        error: None,
                    };
                    match processor.call(request, run.cancel.clone()).await? {
                        GuestReply::Definition {
                            identity: returned,
                            definition,
                            completeness,
                            diagnostics,
                            ..
                        } if returned == identity
                            && definition["identity"] == expected["identity"]
                            && definition["occurrences"] == expected["occurrences"]
                            && definition.is_object() =>
                        {
                            (definition, completeness, diagnostics)
                        }
                        GuestReply::Failure { diagnostic } => (
                            json!({"identity":identity,"occurrences":[],"referenceStatus":"unverified"}),
                            Completeness::Partial,
                            vec![diagnostic],
                        ),
                        _ => (
                            json!({"identity":identity,"occurrences":[],"referenceStatus":"unverified"}),
                            Completeness::Partial,
                            vec![Diagnostic::new(
                                "PROCESSOR_REFERENCE_INVALID",
                                "The local consumer did not preserve the exact verified reference.",
                            )],
                        ),
                    }
                } else if authoritative {
                    (
                        json!({"identity":identity,"occurrences":[],"referenceStatus":"notDeclaredInCollectedDefinitions"}),
                        Completeness::VerifiedAbsent,
                        vec![],
                    )
                } else {
                    (
                        json!({"identity":identity,"occurrences":[],"referenceStatus":"unverifiedSourceCoverage"}),
                        Completeness::Partial,
                        vec![Diagnostic::new(
                            "REFERENCE_COVERAGE_PARTIAL",
                            "Incomplete producer coverage cannot establish whether this reference is still declared.",
                        )],
                    )
                };
                let expected =
                    storage::sha256(&serde_json::to_vec_pretty(&definition).map_err(|_| {
                        Diagnostic::new(
                            "OUTPUT_INVALID",
                            "Cannot serialize the local reference artifact.",
                        )
                    })?);
                let mut definition = definition;
                definition["herbertDirectOutput"] = json!(true);
                rows.push((identity.clone(), definition));
                outcomes.insert(
                    key,
                    (identity.clone(), completeness, item_diagnostics, expected),
                );
            }
            let mut written = Ok(());
            // Separate destination owners remain independent when one directory
            // is unavailable. All writes still use the existing measured pool.
            for row in rows {
                let prepared =
                    blocking(move || crate::collection_output::prepare_page(vec![row])).await?;
                let shared = run.storage.clone();
                let pool = self.file_writers.clone();
                let cancel = run.cancel.clone();
                let result = blocking(move || {
                    crate::collection_output::write_shared_page(
                        shared,
                        prepared,
                        &pool,
                        &cancel,
                        |_| {},
                    )
                })
                .await;
                if let Err(error) = result {
                    if written.is_ok() {
                        written = Err(error);
                    }
                    if run.cancel.is_cancelled() {
                        break;
                    }
                }
            }
            let category = processor.category.id.clone();
            collection::update(run,move|storage|{
                storage.journal.diagnostics.retain(|d|d.context.get("referenceConsumer")!=Some(&json!(category)));
                for mut diagnostic in diagnostics{diagnostic.context.insert("referenceConsumer".into(),json!(category));storage.journal.diagnostics.push(diagnostic);}
                if authoritative{storage.journal.category_outcomes.remove(&category);}else{storage.journal.category_outcomes.insert(category.clone(),"discoveryIncomplete".into());}
                for(key,(identity,completeness,diagnostics,expected))in outcomes{
                    let mut artifact=storage.journal.artifacts.iter().find(|a|storage::identity_key(&a.identity)==key).cloned().unwrap_or_else(||Artifact{identity:identity.clone(),classification:"referenceRefreshIncomplete".into(),acquisition:Completeness::Partial,integrity:"currentReferenceOutputUnverified".into(),processing:"partial".into(),files:vec![],dependencies:vec![],diagnostics:vec![],source_consistency:"unverifiedCurrentIndex".into()});
                    let file=artifact.files.iter().find(|f|f.sha256==expected&&matches!(f.role.as_str(),"derived"|"derivedConflict")).cloned();
                    let verified=file.filter(|f|storage.verify(f).is_ok());
                    if let Some(file)=verified{
                        artifact.files=vec![file.clone()];artifact.acquisition=completeness;
                        artifact.processing=if matches!(artifact.acquisition,Completeness::Collected|Completeness::VerifiedAbsent)&&diagnostics.is_empty(){"derived"}else{"partial"}.into();
                        artifact.classification=if artifact.acquisition==Completeness::VerifiedAbsent{"referenceNotDeclared"}else{"localReference"}.into();artifact.source_consistency="derivedFromCollectedDefinitions".into();artifact.integrity="hostHashVerified".into();
                        artifact.diagnostics.retain(|d|d.code!="REFERENCE_OUTPUT_STALE"&&d.code!="REFERENCE_OUTPUT_UNVERIFIED");artifact.diagnostics.extend(diagnostics);
                        let mut input=file;input.role="referenceInput".into();storage.journal.definition_views.insert(key,input);storage.record_artifact(artifact)?;
                    }else{
                        artifact.acquisition=Completeness::Partial;artifact.processing="partial".into();artifact.classification="referenceRefreshIncomplete".into();artifact.integrity="currentReferenceOutputUnverified".into();artifact.source_consistency="unverifiedCurrentIndex".into();
                        let mut diagnostic=Diagnostic::new("REFERENCE_OUTPUT_STALE","The current reference JSON was not saved and verified. Previous files remain preserved; retry local reprocessing after resolving the write failure.");diagnostic.identity=Some(identity);diagnostic.context.insert("expectedSha256".into(),json!(expected));artifact.diagnostics.push(diagnostic);
                        storage.journal.definition_views.remove(&key);storage.manifest.artifacts.insert(key,artifact.clone());storage.journal.artifacts.retain(|a|a.identity!=artifact.identity);storage.journal.artifacts.push(artifact);
                    }
                }
                Ok(())
            }).await?;
            written?;
        }
        Ok(())
    }
}
