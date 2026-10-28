//! Explicit owned-lab acceptance for the C04 caller and actual shared engine.
use super::*;
use crate::input::AuthMode;
use std::fs;
use std::collections::BTreeSet;

#[tokio::test]
#[ignore = "Requires explicit lab reservation, matching C04 WASM/host/companion and owned output"]
async fn compiled_ubaq_native_host_collection() {
    let lane = std::env::var("HERBERT_UBAQ_LANE").unwrap();
    let (endpoint, profile) = match lane.as_str() {
        "2025.2" => ("https://kinetic-2025-2.lab.test:9449/", "lab-2025-2"),
        "2026.1" => ("https://kinetic-2026-1.lab.test:9448/", "lab-2026-1"),
        _ => panic!("Only authorized owned labs"),
    };
    let package = PathBuf::from(std::env::var("HERBERT_UBAQ_PACKAGE").unwrap());
    let output = PathBuf::from(std::env::var("HERBERT_UBAQ_OUTPUT").unwrap());
    assert!(!output.exists(), "Preserve earlier evidence");
    let credentials = crate::profiles::secure_read(endpoint, profile).unwrap();
    let mode = if credentials.api_key.as_ref().is_some_and(|s| !s.is_empty()) {
        AuthMode::BasicApiKey
    } else {
        AuthMode::Session
    };
    let connection = Connection::new(
        crate::input::endpoint(endpoint).unwrap(),
        "TESTCO".into(),
        mode,
        credentials,
        OperatingSettings::default(),
        Budget::new(4).unwrap(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let admitted = connection.connect(&cancel).await.unwrap();
    connection.qualification_http.lock().unwrap().clear();
    let plugin = Plugin::load(&package.join("plugins/BAQs and UBAQs/BAQs and UBAQs.wasm"))
        .await
        .unwrap();
    assert_eq!(plugin.category.id, "C04");
    let host = Host::with_roots(
        output.join("state"),
        package.join("plugins"),
        Some(output.join("coordination")),
    );
    let mut journal = storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.endpoint = endpoint.into();
    journal.company = "TESTCO".into();
    journal.caller = admitted["caller"].as_str().unwrap().into();
    journal.temporary_access_enabled = false;
    journal.collection_method = if std::env::var("HERBERT_UBAQ_DIRECT").as_deref() == Ok("true") {
        CollectionMethod::DirectOnly
    } else {
        CollectionMethod::ServerAssisted
    };
    let project = std::env::var("HERBERT_UBAQ_PROJECT").as_deref() != Ok("false");
    let download = std::env::var("HERBERT_UBAQ_DOWNLOAD").as_deref() != Ok("false");
    journal.plugin_settings.insert("C04".into(),json!({"queriesPerPage":100,"downloadCompiledAssemblies":download,"decompileCompiledAssemblies":project}));
    let run = make_run(Storage::open(&output.join("collection"), false, journal).unwrap());
    assert!(
        host.prepare_native_artifacts(&run, &connection, &[plugin.clone()])
            .await
            .is_empty()
    );
    let result = if let Ok(query) = std::env::var("HERBERT_UBAQ_TARGET") {
        let company =
            std::env::var("HERBERT_UBAQ_TARGET_COMPANY").unwrap_or_else(|_| "TESTCO".into());
        let identity = Identity {
            category: "C04".into(),
            kind: "UBAQ".into(),
            scope: if company.is_empty() {
                Scope::Global
            } else {
                Scope::Company {
                    company: company.clone(),
                }
            },
            keys: BTreeMap::from([("Company".into(), company), ("QueryID".into(), query)]),
        };
        host.acquire_targets(
            &run,
            &connection,
            &plugin,
            &[],
            vec![identity],
            Some(100),
            true,
        )
        .await
        .map(|_| 1)
    } else {
        host.discover_targets(&run, &connection, &plugin, &[], Some(100))
            .await
            .map(|ids| ids.len())
    };
    let jobs = host.managed.drain(&run).await;
    {
        let mut store = run.storage.lock().unwrap();
        managed_decompilation::attach_outputs(&mut store).unwrap();
    }
    let cleanup = host.cleanup_native_artifacts(&run, &connection).await;
    connection.logout(&cancel).await.unwrap();
    let statistics = run.activity.finish_statistics();
    let store = run.storage.lock().unwrap();
    store.checkpoint().unwrap();
    let receipt = json!({"lane":lane,"result":result,"jobs":jobs,"cleanup":cleanup,"statistics":statistics,"artifacts":store.journal.artifacts,"diagnostics":store.journal.diagnostics,"nativeArtifacts":store.journal.native_artifacts,"httpRequests":connection.qualification_http.lock().unwrap().clone()});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert!(jobs.is_empty(), "{jobs:?}");
    assert!(cleanup.is_empty(), "{cleanup:?}");
    for artifact in &store.journal.artifacts {
        for file in &artifact.files {
            store.verify(file).unwrap();
        }
    }
}

fn make_run(store: Storage) -> Arc<Run> {
    Arc::new(Run {
        storage: Arc::new(std::sync::Mutex::new(store)),
        cancel: CancellationToken::new(),
        paused: AtomicBool::new(false),
        wake: Notify::new(),
        active: AtomicBool::new(true),
        network: false,
        helper_restore_attempts: Mutex::new(BTreeSet::new()),
        activity: Arc::new(Default::default()),
        shared_reads: Default::default(),
        helper_instances: Mutex::new(BTreeMap::new()),
        collection_context: Mutex::new(None),
    })
}
fn status_owner(query: &str) -> Identity {
    Identity {
        category: "C04".into(),
        kind: "UBAQ".into(),
        scope: Scope::Company {
            company: "LAB".into(),
        },
        keys: BTreeMap::from([
            ("Company".into(), "LAB".into()),
            ("QueryID".into(), query.into()),
        ]),
    }
}
fn status_failure(owner: Identity) -> Diagnostic {
    let mut d = Diagnostic::new(
        "OPTIONAL_CAPTURE_FAILED",
        "Requested optional capture failed.",
    );
    d.identity = Some(owner);
    d.context.insert("processingIncomplete".into(), json!(true));
    d
}
#[tokio::test]
async fn optional_processing_failure_preserves_acquisition_files_and_discovery_then_reacquisition_clears_it()
 {
    let temp = tempfile::tempdir().unwrap();
    let owner = status_owner("one");
    let mut store = Storage::open(
        temp.path(),
        false,
        storage::new_journal("optional-status".into()),
    )
    .unwrap();
    let original = store.preserve_raw(b"native BAQ package", "baq").unwrap();
    let (derived, conflict) = store
        .write_derived(&owner, "Authored.cs", b"authored source")
        .unwrap();
    assert!(!conflict);
    let artifact = Artifact {
        identity: owner.clone(),
        classification: "discoveredDefinition".into(),
        acquisition: Completeness::Collected,
        integrity: "hostHashVerified".into(),
        processing: "derived".into(),
        files: vec![original.clone(), derived.clone()],
        dependencies: vec![],
        diagnostics: vec![],
        source_consistency: "nonAtomicUnknown".into(),
    };
    store.record_artifact(artifact.clone()).unwrap();
    store
        .journal
        .category_outcomes
        .insert("C04".into(), "discovered".into());
    store.journal.discovery_finished.insert("C04".into(), true);
    let run = make_run(store);
    let failure = status_failure(owner.clone());
    record_page_diagnostic(&run, "C04", &[owner.clone()], failure.clone())
        .await
        .unwrap();
    record_page_diagnostic(&run, "C04", &[owner.clone()], failure.clone())
        .await
        .unwrap();
    let mut store = run.storage.lock().unwrap();
    let mut current = store.manifest.artifacts[&storage::identity_key(&owner)].clone();
    assert_eq!(current.acquisition, Completeness::Collected);
    assert_eq!(current.files, artifact.files);
    assert_eq!(current.processing, "partial");
    assert_eq!(current.diagnostics, vec![failure]);
    assert_eq!(store.journal.category_outcomes["C04"], "discovered");
    assert_eq!(store.journal.discovery_finished["C04"], true);
    assert_eq!(collection::terminal_phase(&store.journal), "partial");
    for file in &current.files {
        store.verify(file).unwrap();
    }
    // Local reprocessing of the same preserved input must retain the unresolved failure.
    current.processing = "derived".into();
    store.record_artifact(current).unwrap();
    assert_eq!(store.journal.artifacts[0].processing, "partial");
    // A fresh successful producer record owns its new diagnostics; old failures are not merged.
    store.record_artifact(artifact.clone()).unwrap();
    assert_eq!(store.journal.artifacts[0].processing, "derived");
    assert!(store.journal.artifacts[0].diagnostics.is_empty());
    assert_eq!(store.journal.artifacts[0].files, artifact.files);
    assert_eq!(collection::terminal_phase(&store.journal), "completed");
    for file in &artifact.files {
        store.verify(file).unwrap();
    }
}
#[tokio::test]
async fn optional_processing_failure_cannot_mutate_another_page_owner() {
    let temp = tempfile::tempdir().unwrap();
    let run = make_run(
        Storage::open(
            temp.path(),
            false,
            storage::new_journal("optional-scope".into()),
        )
        .unwrap(),
    );
    let owner = status_owner("one");
    for other in [
        status_owner("two"),
        Identity {
            category: "C06".into(),
            ..owner.clone()
        },
    ] {
        assert!(
            record_page_diagnostic(&run, "C04", &[owner.clone()], status_failure(other))
                .await
                .is_err()
        );
    }
    assert!(run.storage.lock().unwrap().journal.diagnostics.is_empty());
    assert!(run.storage.lock().unwrap().manifest.artifacts.is_empty());
}
#[test]
fn verified_absence_without_a_processing_failure_does_not_force_partial() {
    let mut journal = storage::new_journal("optional-absent".into());
    journal
        .category_outcomes
        .insert("C04".into(), "discovered".into());
    journal.artifacts.push(Artifact {
        identity: status_owner("one"),
        classification: "discoveredDefinition".into(),
        acquisition: Completeness::Collected,
        integrity: "hostHashVerified".into(),
        processing: "derived".into(),
        files: vec![],
        dependencies: vec![],
        diagnostics: vec![],
        source_consistency: "nonAtomicUnknown".into(),
    });
    assert_eq!(collection::terminal_phase(&journal), "completed");
}
