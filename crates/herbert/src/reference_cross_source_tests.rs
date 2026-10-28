//! Supplemental local cases; these do not claim native producer qualification.
use super::*;

fn identity(name: &str) -> Identity {
    Identity {
        category: "fixture".into(),
        kind: "Definition".into(),
        scope: Scope::Company {
            company: "TESTCO".into(),
        },
        keys: BTreeMap::from([("ID".into(), name.into())]),
    }
}
async fn save(
    host: &Arc<Host>,
    run: &Arc<Run>,
    plugin: &Plugin,
    data: Vec<(Identity, Value)>,
    cancel: &CancellationToken,
) -> Result<(), Diagnostic> {
    let prepared = host
        .prepare_reference_records(data, &[plugin.clone()], "TESTCO", &run.cancel)
        .await?;
    crate::collection_output::write_shared_page(
        run.storage.clone(),
        prepared,
        &host.file_writers,
        cancel,
        |_| {},
    )
}

// A completed driver releases destination ownership. The public entry opens a
// fresh Storage/Run before each later operation; retain that ownership here.
async fn reprocess(host: &Arc<Host>, run: &mut Arc<Run>) {
    let root = run.storage.lock().unwrap().root().to_path_buf();
    host.clone().reprocess(run.clone()).await;
    let journal = collection::snapshot(run).await.unwrap();
    *run = owned_run(Storage::open(&root, false, journal).unwrap());
}

fn verify_index(run: &Arc<Run>) {
    let storage = run.storage.lock().unwrap();
    for reference in &storage.journal.references {
        for occurrence in reference["occurrences"].as_array().unwrap() {
            let source: Identity = serde_json::from_value(occurrence["source"].clone()).unwrap();
            let input = &storage.journal.definition_views[&storage::identity_key(&source)];
            storage.verify(input).unwrap();
            let bytes = std::fs::read(storage.confined(&input.path).unwrap()).unwrap();
            let definition: Value = herbert_contract::decode_local_json(&bytes).unwrap();
            let row = definition
                .pointer(occurrence["pointer"].as_str().unwrap())
                .unwrap();
            for (field, value) in occurrence["fields"].as_object().unwrap() {
                assert_eq!(row.get(field), Some(value));
            }
        }
    }
}
fn reference_id(schema: &str, table: &str) -> Identity {
    Identity {
        category: "C27".into(),
        kind: "DatabaseTableReference".into(),
        scope: Scope::Company {
            company: "TESTCO".into(),
        },
        keys: BTreeMap::from([
            ("Schema".into(), schema.into()),
            ("Table".into(), table.into()),
        ]),
    }
}
fn consumer_file(run: &Arc<Run>, id: &Identity) -> (PathBuf, Value) {
    let storage = run.storage.lock().unwrap();
    let artifact = storage
        .journal
        .artifacts
        .iter()
        .find(|a| &a.identity == id)
        .unwrap();
    let file = artifact
        .files
        .iter()
        .find(|f| matches!(f.role.as_str(), "derived" | "derivedConflict"))
        .unwrap();
    storage.verify(file).unwrap();
    let path = storage.confined(&file.path).unwrap();
    let data = herbert_contract::decode_local_json(&std::fs::read(&path).unwrap()).unwrap();
    (path, data)
}
#[tokio::test]
#[ignore = "Explicit compiled C27; supplemental cross-source/error/reprocess cases, no ERP"]
async fn c27_reference_cross_source_changes_failures_and_reprocess() {
    let proof = PathBuf::from(std::env::var("HERBERT_C27_PROOF").unwrap());
    let wasm = PathBuf::from(std::env::var("HERBERT_C27_WASM").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let temporary = dir.path().to_path_buf();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir(&plugins).unwrap();
    std::fs::copy(&wasm, plugins.join("c27.wasm")).unwrap();
    let plugin = Plugin::load(&plugins.join("c27.wasm")).await.unwrap();
    let host = Host::with_roots(
        dir.path().join("state"),
        plugins,
        Some(dir.path().join("coordination")),
    );
    let mut journal = storage::new_journal("cross-source-reference-proof".into());
    journal.company = "TESTCO".into();
    journal.scope = "fullBackup".into();
    journal.categories = vec!["fixture".into(), "C27".into()];
    let mut run = owned_run(Storage::open(&dir.path().join("output"), false, journal).unwrap());
    let a = identity("A");
    let b = identity("B");
    let c = identity("C");
    let row = json!({"herbertDirectOutput":true,"DBSchemaName":"Erp","DBTableName":"Part","unknown":{"keep":"Unicode Î©"}});
    save(
        &host,
        &run,
        &plugin,
        vec![(a.clone(), row.clone()), (b.clone(), row.clone())],
        &run.cancel,
    )
    .await
    .unwrap();
    let shared = run.storage.lock().unwrap().journal.references.clone();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0]["occurrences"].as_array().unwrap().len(), 2);
    verify_index(&run);
    save(
        &host,
        &run,
        &plugin,
        vec![(b.clone(), row.clone()), (a.clone(), row.clone())],
        &run.cancel,
    )
    .await
    .unwrap();
    assert_eq!(run.storage.lock().unwrap().journal.references, shared);
    let replacement = json!({"herbertDirectOutput":true,"DBSchemaName":"Ice","DBTableName":"UD01"});
    save(
        &host,
        &run,
        &plugin,
        vec![(a.clone(), replacement)],
        &run.cancel,
    )
    .await
    .unwrap();
    {
        let storage = run.storage.lock().unwrap();
        assert_eq!(storage.journal.references.len(), 2);
        let part = storage
            .journal
            .references
            .iter()
            .find(|r| r["identity"]["keys"]["Table"] == "Part")
            .unwrap();
        assert_eq!(part["occurrences"].as_array().unwrap().len(), 1);
        assert_eq!(part["occurrences"][0]["source"], json!(b));
    }
    // Accepted but unparseable current source must not retain its previous table.
    save(
        &host,
        &run,
        &plugin,
        vec![(b.clone(), Value::Null)],
        &run.cancel,
    )
    .await
    .unwrap();
    {
        let storage = run.storage.lock().unwrap();
        assert_eq!(storage.journal.references.len(), 1);
        assert_eq!(
            storage.journal.references[0]["identity"]["keys"]["Table"],
            "UD01"
        );
        let failed = storage
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity == b)
            .unwrap();
        assert_eq!(failed.processing, "partial");
        assert!(failed.dependencies.is_empty());
        assert_eq!(
            reference_context(&storage.journal, "C27")["producerReferenceErrors"],
            true
        );
    }
    verify_index(&run);
    save(
        &host,
        &run,
        &plugin,
        vec![(b.clone(), row.clone())],
        &run.cancel,
    )
    .await
    .unwrap();
    verify_index(&run);
    reprocess(&host, &mut run).await;
    assert_eq!(run.storage.lock().unwrap().journal.phase, "completed");
    let part_id = reference_id("Erp", "Part");
    let removed_id = reference_id("Ice", "UD01");
    assert_eq!(
        consumer_file(&run, &part_id).1["occurrences"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Exercise the public production reprocess driver. It preserves user edits,
    // clears only the unverified source's index, and still processes other sources.
    let (input_path, input_bytes) = {
        let storage = run.storage.lock().unwrap();
        let input = &storage.journal.definition_views[&storage::identity_key(&b)];
        let path = storage.confined(&input.path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        (path, bytes)
    };
    std::fs::write(&input_path, b"user edit").unwrap();
    reprocess(&host, &mut run).await;
    {
        let storage = run.storage.lock().unwrap();
        assert_eq!(storage.journal.phase, "partial");
        assert_eq!(storage.journal.references.len(), 1);
        assert_eq!(
            storage.journal.references[0]["identity"]["keys"]["Table"],
            "UD01"
        );
    }
    assert_eq!(std::fs::read(&input_path).unwrap(), b"user edit");
    verify_index(&run);
    std::fs::write(&input_path, &input_bytes).unwrap();
    run.active.store(true, Ordering::SeqCst);
    reprocess(&host, &mut run).await;
    {
        let storage = run.storage.lock().unwrap();
        assert_eq!(
            storage.journal.phase, "completed",
            "{:?}",
            storage.journal.artifacts
        );
        assert_eq!(storage.journal.references.len(), 2);
        assert_eq!(
            reference_context(&storage.journal, "C27")["producerReferenceErrors"],
            false
        );
    }
    verify_index(&run);
    let before = run.storage.lock().unwrap().journal.references.clone();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        save(
            &host,
            &run,
            &plugin,
            vec![(c.clone(), row.clone())],
            &cancel
        )
        .await
        .is_err()
    );
    assert_eq!(run.storage.lock().unwrap().journal.references, before);
    // A partial artifact with a saved complete JSON core still supplies useful,
    // verifiable references; its missing resource keeps consumer coverage partial.
    let mut partial = row.clone();
    partial["herbertSources"] = json!({"nativeFiles":[{"name":"missing.bin","file":{"path":"Herbert Data/raw/missing.bin","sha256":"missing","bytes":1,"role":"native"}}]});
    assert!(
        save(
            &host,
            &run,
            &plugin,
            vec![(c.clone(), partial)],
            &run.cancel
        )
        .await
        .is_err()
    );
    verify_index(&run);
    {
        let storage = run.storage.lock().unwrap();
        assert_eq!(
            reference_context(&storage.journal, "C27")["producerReferenceErrors"],
            true
        );
        let part = storage
            .journal
            .references
            .iter()
            .find(|r| r["identity"]["keys"]["Table"] == "Part")
            .unwrap();
        assert_eq!(part["occurrences"].as_array().unwrap().len(), 2);
    }
    save(
        &host,
        &run,
        &plugin,
        vec![
            (c.clone(), row.clone()),
            (
                a.clone(),
                json!({"herbertDirectOutput":true,"note":"no declarations"}),
            ),
        ],
        &run.cancel,
    )
    .await
    .unwrap();
    verify_index(&run);
    {
        let storage = run.storage.lock().unwrap();
        assert_eq!(storage.journal.references.len(), 1);
        assert_eq!(
            storage.journal.references[0]["occurrences"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            storage
                .journal
                .artifacts
                .iter()
                .find(|s| s.identity == a)
                .unwrap()
                .dependencies
                .is_empty()
        );
        storage.checkpoint().unwrap();
    }
    // The index advances before its consumer: the old materialization must
    // already be pending, even if the run never reaches a consumer write slot.
    assert_eq!(
        run.storage
            .lock()
            .unwrap()
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity == part_id)
            .unwrap()
            .processing,
        "partial"
    );
    let (part_path, _) = consumer_file(&run, &part_id);
    let folder = part_path.parent().unwrap().to_path_buf();
    let root_checked = std::fs::canonicalize(&temporary).unwrap();
    let checked = std::fs::canonicalize(&folder).unwrap();
    assert!(checked.starts_with(&root_checked));
    assert_ne!(checked, root_checked);
    let backup = folder.with_file_name(format!(
        "{} - owned backup",
        folder.file_name().unwrap().to_string_lossy()
    ));
    assert!(!backup.exists());
    assert!(
        checked
            .parent()
            .unwrap()
            .join(backup.file_name().unwrap())
            .starts_with(&root_checked)
    );
    std::fs::rename(&folder, &backup).unwrap();
    std::fs::write(&folder, b"owned directory blocker").unwrap();
    run.active.store(true, Ordering::SeqCst);
    reprocess(&host, &mut run).await;
    let failed = run.storage.lock().unwrap().journal.clone();
    assert_eq!(std::fs::read(&folder).unwrap(), b"owned directory blocker");
    std::fs::remove_file(&folder).unwrap();
    std::fs::rename(&backup, &folder).unwrap();
    let stale = failed
        .artifacts
        .iter()
        .find(|a| a.identity == part_id)
        .unwrap();
    assert_eq!(stale.processing, "partial");
    assert!(
        stale
            .diagnostics
            .iter()
            .any(|d| d.code == "REFERENCE_OUTPUT_STALE")
    );
    assert_ne!(failed.phase, "completed");
    verify_index(&run);
    // The independent removed-reference output still completes while Part's
    // directory is blocked, and asserts only absence from collected definitions.
    assert_eq!(
        consumer_file(&run, &removed_id).1["referenceStatus"],
        "notDeclaredInCollectedDefinitions"
    );
    run.active.store(true, Ordering::SeqCst);
    reprocess(&host, &mut run).await;
    assert_eq!(run.storage.lock().unwrap().journal.phase, "completed");
    let (current_path, current) = consumer_file(&run, &part_id);
    assert_eq!(current["occurrences"].as_array().unwrap().len(), 2);
    verify_index(&run);
    std::fs::write(&current_path, b"keep my reference edits").unwrap();
    run.active.store(true, Ordering::SeqCst);
    reprocess(&host, &mut run).await;
    assert_eq!(
        std::fs::read(&current_path).unwrap(),
        b"keep my reference edits"
    );
    let (conflict, current) = consumer_file(&run, &part_id);
    assert_ne!(conflict, current_path);
    assert_eq!(current["occurrences"].as_array().unwrap().len(), 2);
    assert_eq!(run.storage.lock().unwrap().journal.phase, "completed");
    drop(run);
    drop(host);
    drop(plugin);
    dir.close().unwrap();
    assert!(!temporary.exists());
    std::fs::write(proof.join("cross-source-result.json"),serde_json::to_vec_pretty(&json!({"supplementalLocalCases":true,"sourceReplacement":true,"independentSourcesPreserved":true,"orderStable":true,"hardFailureClearsStaleOccurrences":true,"partialSavedCoreReferencesVerified":true,"cancelCommitsNoNewReferences":true,"publicReprocessUserEditPreserved":true,"publicReprocessRetryRecovered":true,"consumerFilesMatchIndex":true,"consumerWriteFailureExplicit":true,"independentConsumerWritesFinish":true,"consumerUserEditsPreserved":true,"removedReferenceHasTypedLocalAbsence":true,"noNativeRequests":true,"temporaryOutputAbsent":true})).unwrap()).unwrap();
}

#[tokio::test]
#[ignore = "Explicit compiled C27; independent producer write failure/retry, no ERP"]
async fn c27_reference_producer_write_failure_does_not_block_reprocess() {
    let proof = PathBuf::from(std::env::var("HERBERT_C27_PROOF").unwrap());
    let wasm = PathBuf::from(std::env::var("HERBERT_C27_WASM").unwrap());
    let dir = tempfile::tempdir().unwrap();
    let temporary = dir.path().to_path_buf();
    let plugins = dir.path().join("plugins");
    std::fs::create_dir(&plugins).unwrap();
    std::fs::copy(wasm, plugins.join("c27.wasm")).unwrap();
    let plugin = Plugin::load(&plugins.join("c27.wasm")).await.unwrap();
    let host = Host::with_roots(
        dir.path().join("state"),
        plugins,
        Some(dir.path().join("coordination")),
    );
    let mut journal = storage::new_journal("independent-reference-producers".into());
    journal.company = "TESTCO".into();
    journal.scope = "fullBackup".into();
    journal.categories = vec!["fixture".into(), "C27".into()];
    let mut run = owned_run(Storage::open(&dir.path().join("output"), false, journal).unwrap());
    let blocked = identity("D-blocked");
    let independent = identity("E-independent");
    let row = json!({"DBSchemaName":"Erp","DBTableName":"Part"});
    save(
        &host,
        &run,
        &plugin,
        vec![(blocked.clone(), row.clone()), (independent.clone(), row)],
        &run.cancel,
    )
    .await
    .unwrap();
    reprocess(&host, &mut run).await;
    assert_eq!(run.storage.lock().unwrap().journal.phase, "completed");
    let (folder, backup, old_receipts) = {
        let mut s = run.storage.lock().unwrap();
        let a = s
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity == blocked)
            .unwrap();
        let old = a.files.clone();
        let file = a.files.iter().find(|f| f.role == "derived").unwrap();
        let path = s.confined(&file.path).unwrap();
        let folder = path.parent().unwrap().to_path_buf();
        assert!(folder.starts_with(temporary.canonicalize().unwrap()));
        let other = s
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity == independent)
            .unwrap();
        for file in other.files.iter().filter(|f| f.role == "derived") {
            assert!(!s.confined(&file.path).unwrap().starts_with(&folder));
        }
        // Model the ordinary producer path with its verified immutable input,
        // rather than the direct-output referenceInput path covered above.
        let original = json!({"DBSchemaName":"Erp","DBTableName":"Part"});
        let input = s
            .preserve_immutable(
                &serde_json::to_vec_pretty(&original).unwrap(),
                "json",
                "processingInput",
            )
            .unwrap();
        s.journal
            .definition_views
            .insert(storage::identity_key(&blocked), input);
        let changed = json!({"DBSchemaName":"Ice","DBTableName":"UD02"});
        let input = s
            .preserve_immutable(
                &serde_json::to_vec_pretty(&changed).unwrap(),
                "json",
                "processingInput",
            )
            .unwrap();
        s.journal
            .definition_views
            .insert(storage::identity_key(&independent), input);
        s.checkpoint().unwrap();
        let backup = folder.with_file_name(format!(
            "{}-owned-backup",
            folder.file_name().unwrap().to_string_lossy()
        ));
        assert!(!backup.exists());
        (folder, backup, old)
    };
    std::fs::rename(&folder, &backup).unwrap();
    std::fs::write(&folder, b"owned destination blocker").unwrap();
    run.active.store(true, Ordering::SeqCst);
    reprocess(&host, &mut run).await;
    {
        let s = run.storage.lock().unwrap();
        assert_eq!(s.journal.phase, "partial");
        let a = s
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity == blocked)
            .unwrap();
        assert_eq!(a.processing, "partial");
        for file in &old_receipts {
            assert!(a.files.contains(file));
        }
        assert!(
            a.diagnostics
                .iter()
                .any(|d| d.code == "REPROCESS_OUTPUT_FAILED")
        );
        let b = s
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity == independent)
            .unwrap();
        assert_eq!(b.processing, "derived");
        assert!(
            s.journal
                .references
                .iter()
                .any(|r| r["identity"]["keys"]["Table"] == "UD02")
        );
    }
    assert_eq!(
        consumer_file(&run, &reference_id("Ice", "UD02")).1["occurrences"][0]["source"],
        json!(independent)
    );
    std::fs::remove_file(&folder).unwrap();
    std::fs::rename(&backup, &folder).unwrap();
    run.active.store(true, Ordering::SeqCst);
    reprocess(&host, &mut run).await;
    {
        let s = run.storage.lock().unwrap();
        assert_eq!(s.journal.phase, "completed");
        let a = s
            .journal
            .artifacts
            .iter()
            .find(|a| a.identity == blocked)
            .unwrap();
        assert_eq!(a.processing, "derived");
        assert!(
            !a.diagnostics
                .iter()
                .any(|d| d.code == "REPROCESS_OUTPUT_FAILED")
        );
        for file in &a.files {
            s.verify(file).unwrap();
        }
    }
    verify_index(&run);
    drop(run);
    drop(host);
    drop(plugin);
    dir.close().unwrap();
    assert!(!temporary.exists());
    std::fs::write(proof.join("independent-producer-result.json"),serde_json::to_vec_pretty(&json!({"blockedProducerPreservedReceipts":true,"independentChangedProducerCompleted":true,"consumerRefreshedDespiteProducerFailure":true,"targetedRetryCompleted":true,"ownedTemporaryDirectoryAbsent":true})).unwrap()).unwrap();
}
