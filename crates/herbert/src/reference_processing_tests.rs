use super::*;
use std::collections::BTreeSet;

#[test]
#[ignore = "Read-only local timing probe; requires HERBERT_PROFILE_JOURNAL"]
fn reference_context_profile() {
    let path = std::env::var_os("HERBERT_PROFILE_JOURNAL").expect("journal path");
    let journal: storage::RunJournal = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let start = std::time::Instant::now();
    let snapshot = journal.clone();
    let cloned = start.elapsed();
    let start = std::time::Instant::now();
    let context = reference_context(&snapshot, "configuration-references");
    let prepared = start.elapsed();
    let start = std::time::Instant::now();
    let bytes = serde_json::to_vec(&context).unwrap();
    let serialized = start.elapsed();
    println!("reference profile: artifacts={} references={} context_bytes={} clone_us={} context_us={} serialize_us={}",
        journal.artifacts.len(), journal.references.len(), bytes.len(), cloned.as_micros(), prepared.as_micros(), serialized.as_micros());
}

#[tokio::test]
#[ignore = "Local compiled Configuration references and read-only journal fixture; no network"]
async fn reference_bulk_saved_index_profile() {
    let source = PathBuf::from(std::env::var_os("HERBERT_PROFILE_JOURNAL").expect("journal path"));
    let saved: storage::RunJournal = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
    let original_root = source.parent().unwrap().parent().unwrap().parent().unwrap().parent().unwrap();
    let committed = crate::checkpoint_log::read_journal(original_root, &saved.run_id).unwrap().unwrap();
    assert!(saved == committed, "Materialized journal must equal committed terminal state");
    let plugin = Plugin::load(&PathBuf::from(std::env::var_os("HERBERT_CONFIGURATION_REFERENCES_WASM").expect("compiled Configuration references"))).await.unwrap();
    assert!(plugin.has_role("pagedDefinitions") && plugin.has_role("prefetchPages"));
    let context = reference_context(&saved, "configuration-references");
    let request = GuestRequest { abi:PLUGIN_ABI, operation:"discover".into(), category:"configuration-references".into(), company:saved.company.clone(), identity:None, cursor:None, page_size:None, state:context, response:None, error:None };
    let start = std::time::Instant::now();
    let GuestReply::InventoryData { records, inventory } = plugin.call(request, CancellationToken::new()).await.unwrap() else { panic!("Compiled Configuration references must return all records in one bulk result") };
    let guest_ms = start.elapsed().as_millis();
    let GuestReply::Inventory { identities, next_cursor, .. } = *inventory else { panic!() };
    assert!(next_cursor.is_none());
    assert_eq!(records.len(), saved.references.len());
    assert_eq!(identities.len(), records.len());
    let expected = records.iter().map(|(id, value)| (storage::identity_key(id), value.clone())).collect::<BTreeMap<_,_>>();
    for reference in &saved.references {
        let id: Identity = serde_json::from_value(reference["identity"].clone()).unwrap();
        assert_eq!(expected[&storage::identity_key(&id)], *reference);
    }
    let root = tempfile::tempdir().unwrap();
    let mut journal = storage::new_journal("reference-bulk-profile".into());
    journal.company = saved.company.clone();
    journal.category_names = saved.category_names.clone();
    journal.references = saved.references.clone();
    let shared = Arc::new(std::sync::Mutex::new(Storage::open(root.path(), false, journal).unwrap()));
    let start = std::time::Instant::now();
    crate::collection_output::write_shared_page(shared.clone(), crate::collection_output::prepare_page(records).unwrap(), &crate::file_writers::WriterPool::default(), &CancellationToken::new(), |_| {}).unwrap();
    let s = shared.lock().unwrap();
    s.checkpoint().unwrap();
    let write_ms = start.elapsed().as_millis();
    assert_eq!(s.journal.artifacts.len(), expected.len());
    for artifact in &s.journal.artifacts {
        assert_eq!(artifact.acquisition, Completeness::Collected);
        assert_eq!(artifact.processing, "derived");
        assert_eq!(artifact.files.len(), 1);
        let file = &artifact.files[0];
        s.verify(file).unwrap();
        if let Some(original) = saved.artifacts.iter().find(|a| a.identity == artifact.identity) {
            assert!(original.files.iter().any(|f| f.role == "derived" && f.sha256 == file.sha256 && f.bytes == file.bytes), "Bulk output must preserve the original named JSON bytes");
        }
        let actual: Value = serde_json::from_slice(&std::fs::read(s.confined(&file.path).unwrap()).unwrap()).unwrap();
        assert_eq!(actual, expected[&storage::identity_key(&artifact.identity)]);
    }
    println!("bulk reference profile: references={} compiled_guest_ms={} writer_and_checkpoint_ms={} all_outputs_verified=true", expected.len(), guest_ms, write_ms);
}

#[test]
#[ignore = "Read-only retained source diagnostic replay; requires HERBERT_PROFILE_JOURNAL"]
fn reference_saved_diagnostics_replay() {
    let source = PathBuf::from(std::env::var_os("HERBERT_PROFILE_JOURNAL").unwrap());
    let saved: storage::RunJournal = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
    let root = source.parent().unwrap().parent().unwrap().parent().unwrap().parent().unwrap();
    let mut before = BTreeMap::<String,usize>::new();
    let mut after = BTreeMap::<String,usize>::new();
    let mut scanned = 0;
    for artifact in &saved.artifacts {
        if !artifact.diagnostics.iter().any(|d| matches!(d.code.as_str(), "REFERENCE_CONTENT_UNPARSED" | "REFERENCE_PAIR_UNPARSED")) { continue; }
        let key = storage::identity_key(&artifact.identity);
        let input = saved.definition_views.get(&key).expect("Retained processor input");
        let bytes = std::fs::read(root.join(storage::safe_relative(&input.path).unwrap())).unwrap();
        assert_eq!(storage::sha256(&bytes), input.sha256);
        let definition: Value = serde_json::from_slice(&bytes).unwrap();
        let reply = herbert_plugins::handle("configuration-references", GuestRequest { abi:PLUGIN_ABI, operation:"processDefinition".into(), category:"configuration-references".into(), company:saved.company.clone(), identity:Some(artifact.identity.clone()), cursor:None, page_size:None, state:json!({"definition":definition,"_hostReferenceDiagnostics":true}), response:None, error:None });
        let GuestReply::References { diagnostics, .. } = reply else { panic!("Reference scan failed") };
        for d in &artifact.diagnostics { if d.code != "REFERENCE_SCAN_COVERAGE" { *before.entry(format!("{}:{}", artifact.identity.category,d.code)).or_default()+=1; } }
        for d in diagnostics { if d.code != "REFERENCE_SCAN_COVERAGE" {
            *after.entry(format!("{}:{}:{}", artifact.identity.category,d.code,d.context.get("pointer").and_then(Value::as_str).unwrap_or(""))).or_default()+=1;
        } }
        scanned+=1;
    }
    println!("retained reference replay: scanned={scanned} before={} after={}", serde_json::to_string(&before).unwrap(),serde_json::to_string(&after).unwrap());
}


#[test]
fn reference_generic_processor_failure_marks_paged_processing_partial() {
    let directory = tempfile::tempdir().unwrap();
    let identity = Identity {
        category: "fixture".into(),
        kind: "definition".into(),
        scope: Scope::Global,
        keys: BTreeMap::from([("ID".into(), "one".into())]),
    };
    let mut records =
        crate::collection_output::prepare_page(vec![(identity, json!({"definition":"original"}))])
            .unwrap();
    records[0].set_reference_processing(
        vec![],
        vec![
            Diagnostic::new("REFERENCE_SCAN_COVERAGE", "The scan returned coverage."),
            Diagnostic::new("FIXTURE_TIMEOUT", "A processor timed out."),
        ],
        false,
    );
    let shared = Arc::new(std::sync::Mutex::new(
        Storage::open(
            directory.path(),
            false,
            storage::new_journal("reference-failed".into()),
        )
        .unwrap(),
    ));
    crate::collection_output::write_shared_page(
        shared.clone(),
        records,
        &crate::file_writers::WriterPool::default(),
        &CancellationToken::new(),
        |_| {},
    )
    .unwrap();
    let s = shared.lock().unwrap();
    let artifact = &s.journal.artifacts[0];
    assert_eq!(artifact.acquisition, Completeness::Collected);
    assert_eq!(artifact.processing, "partial");
    assert!(
        artifact
            .diagnostics
            .iter()
            .any(|d| d.code == "FIXTURE_TIMEOUT")
    );
    assert!(
        artifact
            .diagnostics
            .iter()
            .any(|d| d.code == "PROCESSOR_FAILED")
    );
    assert!(s.journal.references.is_empty());
    assert_eq!(
        reference_context(&s.journal, "configuration-references")["producerReferenceErrors"],
        true
    );
}
fn owned_run(storage: Storage) -> Arc<Run> {
    Arc::new(Run {
        storage: Arc::new(std::sync::Mutex::new(storage)),
        cancel: CancellationToken::new(),
        paused: AtomicBool::new(false),
        wake: Notify::new(),
        active: AtomicBool::new(true),
        network: false,
        helper_restore_attempts: Mutex::new(Default::default()),
        activity: Arc::new(Default::default()),
        shared_reads: Default::default(),
        helper_instances: Mutex::new(Default::default()),
        collection_context: Mutex::new(None),
    })
}
fn req(operation: &str, state: Value) -> GuestRequest {
    GuestRequest {
        abi: PLUGIN_ABI,
        operation: operation.into(),
        category: "configuration-references".into(),
        company: "TESTCO".into(),
        identity: None,
        cursor: None,
        page_size: None,
        state,
        response: None,
        error: None,
    }
}

#[test]
fn references_reply_compatibility_and_scope_diagnostics() {
    let old = json!({"reply":"references","records":[]});
    // The tagged field is derived from the actual contract instead of assuming
    // that an external serializer uses the same enum representation.
    let mut current = serde_json::to_value(GuestReply::References {
        records: vec![],
        diagnostics: vec![Diagnostic::new("REFERENCE_SCAN_COVERAGE", "contract")],
    })
    .unwrap();
    let object = current.as_object_mut().unwrap();
    object.remove("diagnostics");
    assert!(
        matches!(serde_json::from_value::<GuestReply>(current).unwrap(),GuestReply::References{diagnostics,..}if diagnostics.is_empty())
    );
    assert!(!reference_error(&Diagnostic::new(
        "REFERENCE_SCAN_COVERAGE",
        "contract"
    )));
    assert!(reference_error(&Diagnostic::new(
        "REFERENCE_CONTENT_UNPARSED",
        "partial"
    )));
    // Old ABI 1 hosts reject unknown fields; guests send diagnostics only
    // when the host explicitly negotiates them.
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
    enum OldReply {
        References { records: Vec<Value> },
    }
    let encoded = serde_json::to_value(GuestReply::References {
        records: vec![],
        diagnostics: vec![Diagnostic::new("REFERENCE_SCAN_COVERAGE", "contract")],
    })
    .unwrap();
    assert!(serde_json::from_value::<OldReply>(encoded).is_err());
    let compatible = serde_json::to_value(GuestReply::References {
        records: vec![],
        diagnostics: vec![],
    })
    .unwrap();
    let OldReply::References { records } = serde_json::from_value::<OldReply>(compatible).unwrap();
    assert!(records.is_empty());
    drop(old);
}

#[tokio::test]
#[ignore = "Requires explicit pinned Configuration references WASM and private retained native producer evidence"]
async fn configuration_references_actual_compiled_output_and_host_recovery() {
    let proof = PathBuf::from(std::env::var("HERBERT_CONFIGURATION_REFERENCES_PROOF").unwrap());
    let wasm = PathBuf::from(std::env::var("HERBERT_CONFIGURATION_REFERENCES_WASM").unwrap());
    let cases: Value =
        serde_json::from_slice(&std::fs::read(proof.join("test-inputs.json")).unwrap())
            .unwrap();
    let plugin = Plugin::load(&wasm).await.unwrap();
    assert!(
        plugin.methods.is_empty() && plugin.entities.is_empty() && plugin.binary_exports.is_empty()
    );
    let first = &cases[0];
    let original: Value =
        serde_json::from_slice(&std::fs::read(first["path"].as_str().unwrap()).unwrap()).unwrap();
    let identity: Identity = serde_json::from_value(first["identity"].clone()).unwrap();
    for negotiation in [None, Some(false), Some(true)] {
        let mut state = json!({"definition":original});
        if let Some(value) = negotiation {
            state["_hostReferenceDiagnostics"] = json!(value);
        }
        let mut request = req("processDefinition", state);
        request.identity = Some(identity.clone());
        let reply = plugin
            .call(request, CancellationToken::new())
            .await
            .unwrap();
        let encoded = serde_json::to_value(&reply).unwrap();
        assert_eq!(
            encoded.get("diagnostics").is_some(),
            negotiation == Some(true)
        );
        if negotiation != Some(true) {
            #[derive(serde::Deserialize)]
            #[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
            enum OldReply {
                References { records: Vec<Value> },
            }
            let OldReply::References { records } =
                serde_json::from_value::<OldReply>(encoded).unwrap();
            assert_eq!(records.len(), 2);
        }
    }
    let missing = plugin
        .call(
            req("discover", json!({"references":[],"producerCount":0})),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        missing,
        GuestReply::Inventory {
            authoritative: false,
            ..
        }
    ));
    let mut deep = json!({"DBSchemaName":"Erp","DBTableName":"Part"});
    for _ in 0..180 {
        deep = json!({"child":deep});
    }
    let mut request = req(
        "processDefinition",
        json!({"definition":deep,"_hostReferenceDiagnostics":true}),
    );
    request.identity = Some(identity.clone());
    let GuestReply::References {
        records,
        diagnostics,
    } = plugin
        .call(request, CancellationToken::new())
        .await
        .unwrap()
    else {
        panic!("A supported deeply nested definition must reach Configuration references");
    };
    assert_eq!(records.len(), 1);
    assert!(
        diagnostics
            .iter()
            .all(|d| d.code == "REFERENCE_SCAN_COVERAGE")
    );
    let mut results = vec![];
    for case in cases.as_array().unwrap() {
        let source = PathBuf::from(case["path"].as_str().unwrap());
        let original = std::fs::read(&source).unwrap();
        assert_eq!(storage::sha256(&original), case["sha256"].as_str().unwrap());
        let definition: Value = serde_json::from_slice(&original).unwrap();
        let identity: Identity = serde_json::from_value(case["identity"].clone()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let host = Host::with_roots(
            dir.path().join("state"),
            dir.path().join("plugins"),
            Some(dir.path().join("coordination")),
        );
        let mut journal = storage::new_journal("configuration-references-reference-proof".into());
        journal.company = "TESTCO".into();
        let run = owned_run(Storage::open(&dir.path().join("output"), false, journal).unwrap());
        *run.collection_context.lock().await = Some(collection::CollectionContext {
            processors: vec![plugin.clone()],
            page_size: None,
            full: true,
            processed_inputs: Arc::new(Mutex::new(BTreeSet::new())),
        });
        let mut broken = definition.clone();
        broken["herbertDirectOutput"] = json!(true);
        broken["herbertSources"] = json!({"nativeFiles":[{"name":"missing.bin","file":{"path":"Herbert Data/raw/missing.bin","sha256":"missing","bytes":1,"role":"native"}}]});
        let partial = host
            .prepare_reference_page(&run, vec![(identity.clone(), broken)], &run.cancel)
            .await
            .unwrap();
        assert!(
            crate::collection_output::write_shared_page(
                run.storage.clone(),
                partial,
                &host.file_writers,
                &run.cancel,
                |_| {}
            )
            .is_err()
        );
        {
            let s = run.storage.lock().unwrap();
            assert!(!s.journal.references.is_empty());
            assert_eq!(
                reference_context(&s.journal, "configuration-references")["producerReferenceErrors"],
                true
            );
            let input = &s.journal.definition_views[&storage::identity_key(&identity)];
            s.verify(input).unwrap();
            assert_eq!(s.journal.artifacts[0].acquisition, Completeness::Partial);
            assert!(!s.journal.artifacts[0].files.is_empty());
        }
        let before_cancel = run.storage.lock().unwrap().journal.references.clone();
        let data = vec![(identity.clone(), definition.clone())];
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            host.prepare_reference_page(&run, data.clone(), &cancelled)
                .await
                .is_err()
        );
        let prepared = host
            .prepare_reference_page(&run, data.clone(), &run.cancel)
            .await
            .unwrap();
        assert!(
            crate::collection_output::write_shared_page(
                run.storage.clone(),
                prepared,
                &host.file_writers,
                &cancelled,
                |_| {}
            )
            .is_err()
        );
        assert_eq!(
            run.storage.lock().unwrap().journal.references,
            before_cancel
        );
        let prepared = host
            .prepare_reference_page(&run, data.clone(), &run.cancel)
            .await
            .unwrap();
        crate::collection_output::write_shared_page(
            run.storage.clone(),
            prepared,
            &host.file_writers,
            &run.cancel,
            |_| {},
        )
        .unwrap();
        let (refs, context) = {
            let s = run.storage.lock().unwrap();
            let refs = s.journal.references.clone();
            let context = reference_context(&s.journal, "configuration-references");
            assert_eq!(s.journal.artifacts[0].processing, "derived");
            (refs, context)
        };
        let actual = refs
            .iter()
            .map(|r| {
                serde_json::to_string(
                    &json!({"kind":r["identity"]["kind"],"keys":r["identity"]["keys"]}),
                )
                .unwrap()
            })
            .collect::<BTreeSet<_>>();
        let expected = case["expectedReferences"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "{}", source.display());
        let GuestReply::Inventory {
            identities,
            authoritative,
            diagnostics,
            ..
        } = plugin
            .call(req("discover", context.clone()), run.cancel.clone())
            .await
            .unwrap()
        else {
            panic!()
        };
        assert!(authoritative && diagnostics.is_empty());
        assert_eq!(identities.len(), actual.len());
        for id in identities {
            let mut request = req("acquire", context.clone());
            request.identity = Some(id.clone());
            let GuestReply::Definition { definition, .. } =
                plugin.call(request, run.cancel.clone()).await.unwrap()
            else {
                panic!()
            };
            crate::collection_output::write_page(
                &mut run.storage.lock().unwrap(),
                vec![(id, definition)],
            )
            .unwrap();
        }
        // Repeat source processing must replace its occurrences, not multiply them.
        let prepared = host
            .prepare_reference_page(&run, data, &run.cancel)
            .await
            .unwrap();
        crate::collection_output::write_shared_page(
            run.storage.clone(),
            prepared,
            &host.file_writers,
            &run.cancel,
            |_| {},
        )
        .unwrap();
        {
            let s = run.storage.lock().unwrap();
            assert_eq!(s.journal.references, refs);
            s.checkpoint().unwrap();
            for a in &s.journal.artifacts {
                for f in &a.files {
                    s.verify(f).unwrap();
                }
            }
        }
        assert_eq!(std::fs::read(&source).unwrap(), original);
        results.push(json!({"version":case["version"],"source":source,"sourceSha256":storage::sha256(&original),"pluginSha256":plugin.hash,"references":refs,"cancelledWriteCommittedNoNewReferences":true,"partialSavedCoreReferences":true,"repeatStable":true,"sourceUnchanged":true}));
        let saved = {
            let mut s = run.storage.lock().unwrap();
            s.journal.references.clear();
            s.manifest.references.clear();
            s.journal.definition_views.clear();
            for a in &mut s.journal.artifacts {
                a.diagnostics
                    .retain(|d| d.code != "REFERENCE_SCAN_COVERAGE");
            }
            s.checkpoint().unwrap();
            s.journal.clone()
        };
        drop(run);
        let mut fresh = saved;
        fresh.artifacts.clear();
        let reopened = owned_run(Storage::open(&dir.path().join("output"), false, fresh).unwrap());
        host.process_existing_reference_inputs(&reopened, &[plugin.clone()])
            .await
            .unwrap();
        {
            let s = reopened.storage.lock().unwrap();
            assert_eq!(s.journal.references, refs);
            assert_eq!(
                s.journal.definition_views[&storage::identity_key(&identity)].role,
                "referenceInput"
            );
            s.checkpoint().unwrap();
        }
        drop(reopened);
        drop(host);
        dir.close().unwrap();
    }
    std::fs::write(
        proof.join("compiled-host-result.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
}

#[path = "reference_cross_source_tests.rs"]
mod current_source_tests;

#[test]
fn scanned_partial_artifacts_are_not_rescanned_but_refreshes_and_missing_coverage_are() {
    let coverage = |hash: &str| {
        let mut d = Diagnostic::new("REFERENCE_SCAN_COVERAGE", "The scan returned coverage.");
        d.context.insert("processorSha256".into(), json!(hash));
        d
    };
    let artifact = |acquisition: &str, processing: &str, diagnostics: Vec<Diagnostic>| -> Artifact {
        serde_json::from_value(json!({
            "identity": {"category":"extended-properties","kind":"ExtendedTableProperties","scope":{"type":"global"},"keys":{"DataTableID":"UD37"}},
            "classification":"discoveredDefinition","acquisition":acquisition,"integrity":"hostHashComputed",
            "processing":processing,"sourceConsistency":"nonAtomicUnknown","files":[],"diagnostics":diagnostics,"dependencies":[]
        })).unwrap()
    };
    let direct_partial = Diagnostic::new("EXTENDED_PROPERTIES_DIRECT_PARTIAL", "Direct-only coverage.");
    // Already scanned by both processors: no second read, Collected or Partial.
    assert!(already_scanned(&artifact("collected", "derived", vec![coverage("a"), coverage("b")]), &["a", "b"]));
    assert!(already_scanned(&artifact("partial", "partial", vec![direct_partial.clone(), coverage("a"), coverage("b")]), &["a", "b"]));
    // Still scanned: a processor without coverage, a refresh request, or processing not yet derived.
    assert!(!already_scanned(&artifact("partial", "partial", vec![coverage("a")]), &["a", "b"]));
    let refresh = Diagnostic::new("REFERENCE_REFRESH_PENDING", "The reference index changed.");
    assert!(!already_scanned(&artifact("partial", "partial", vec![refresh, coverage("a")]), &["a"]));
    assert!(!already_scanned(&artifact("collected", "pending", vec![coverage("a")]), &["a"]));
    assert!(!already_scanned(&artifact("excluded", "partial", vec![coverage("a")]), &["a"]));
}
