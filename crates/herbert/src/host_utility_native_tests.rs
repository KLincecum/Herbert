//! Explicit native qualification. Ordinary test runs never connect to Epicor.
use super::*;

#[tokio::test]
#[ignore = "Explicit lab-only server lease qualification; creates and removes one helper."]
async fn native_server_lease_storage() {
    use crate::native_artifact_backend as backend;
    let endpoint = std::env::var("HERBERT_QUALIFY_ENDPOINT").unwrap();
    assert!(
        [
            "https://kinetic-2026-1.lab.test:9448/",
            "https://kinetic-2025-2.lab.test:9449/"
        ]
        .contains(&endpoint.as_str())
    );
    let profile = std::env::var("HERBERT_QUALIFY_PROFILE").unwrap();
    let root = PathBuf::from(std::env::var("HERBERT_QUALIFY_OUTPUT").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    let credentials = profiles::secure_read(&endpoint, &profile).unwrap();
    let mode = if credentials.api_key.as_ref().is_some_and(|s| !s.is_empty()) {
        AuthMode::BasicApiKey
    } else {
        AuthMode::Session
    };
    let connection = Connection::new(
        crate::input::endpoint(&endpoint).unwrap(),
        "TESTCO".into(),
        mode,
        credentials,
        OperatingSettings::default(),
        Budget::new(4).unwrap(),
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let admitted = connection.connect(&cancel).await.unwrap();
    let id = uuid::Uuid::new_v4();
    let library = format!("H{}", &id.simple().to_string()[..29]);
    let template = snapshot().unwrap();
    let version = if endpoint.contains("2025-2") {
        "HerbertHost-5.1.100.efxj"
    } else {
        "HerbertHost-5.2.100.efxj"
    };
    let prepared = crate::native_artifact_package::prepare(
        &template.declaration,
        &template.files[version],
        &library,
        id,
        "TESTCO",
        admitted["caller"].as_str().unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("fixture.json"),
        json!({"library":library,"globalId":id,"endpoint":endpoint}).to_string(),
    )
    .unwrap();
    assert!(
        backend::probe(
            &connection,
            NativeArtifactType::FunctionLibrary,
            &library,
            &cancel
        )
        .await
        .unwrap()
        .is_none()
    );
    let result: Result<Value, Diagnostic> = async {
        backend::import(&connection,NativeArtifactType::FunctionLibrary,&library,&prepared.bytes,false,&cancel).await?;
        let identity = backend::probe(&connection,NativeArtifactType::FunctionLibrary,&library,&cancel).await?.unwrap().0;
        backend::enable(&connection,&library,&identity,&prepared.metadata,&cancel).await?;
        backend::publish(&connection,&library,&cancel).await?;
        let op = uuid::Uuid::new_v4().to_string();
        let call = async |request: Value| {
            let response = connection.artifact_function_call(&library,"Lease",json!({"iRequest":request.to_string()}),&cancel).await?;
            let text=response.value["oJson"].as_str().ok_or_else(||error("Lease response is absent."))?;
            serde_json::from_str::<Value>(text).map_err(|_|error("Lease response is invalid."))
        };
        let acquired=call(json!({"action":"acquire","operationId":op,"resources":[{"libraryId":library,"globalId":id,"role":"host"}]})).await?;
        assert_eq!(acquired["accepted"],true);
        assert_eq!(acquired["expiresUtcTicks"].as_i64().unwrap()-acquired["serverUtcTicks"].as_i64().unwrap(),3_000_000_000);
        if let Ok(path) = std::env::var("HERBERT_LEASE_FIXTURES") {
            let fixtures: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let host = Host::with_roots(root.join("state"), root.join("plugins"), None);
            let mut reconciled = Vec::new();
            for fixture in fixtures {
                assert_eq!(fixture["endpoint"], endpoint);
                // The explicit fixture list contains only completed test processes.
                connection.lease_call(&library, json!({"action":"release","operationId":fixture["runId"]}), &cancel).await?;
                let target = fixture["libraryId"].as_str().unwrap();
                host.cleanup_leased_helper(&connection, &library, target, fixture["globalId"].as_str().unwrap()).await?;
                assert!(backend::probe(&connection,NativeArtifactType::FunctionLibrary,target,&cancel).await?.is_none());
                reconciled.push(fixture);
            }
            std::fs::write(root.join("reconciled-fixtures.json"),serde_json::to_vec_pretty(&reconciled).unwrap()).unwrap();
        }
        let renewed=call(json!({"action":"renew","operationId":op})).await?;
        assert_eq!(renewed["accepted"],true);
        std::fs::write(root.join("active-operation.json"),serde_json::to_vec_pretty(
            &json!({"operationId":op,"libraryId":library,"globalId":id,"renew":renewed})).unwrap()).unwrap();
        if std::env::var_os("HERBERT_LEASE_CLOCK_PROBE").is_some() {
            let start = std::time::Instant::now();
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let observed = call(json!({"action":"inspect"})).await?;
            std::fs::write(root.join("clock-probe.json"),serde_json::to_vec_pretty(
                &json!({"elapsedSeconds":start.elapsed().as_secs_f64(),"before":renewed,"after":observed})).unwrap()).unwrap();
        }
        let active=call(json!({"action":"claim","globalId":id})).await?;
        assert!(active["resources"].as_array().unwrap().iter().all(|r|r["globalId"]!=id.to_string()));
        let transfer = uuid::Uuid::new_v4().to_string();
        let execute = async |action: &str| {
            connection.artifact_function_call(&library,"Execute",
                json!({"iRequest":json!({"action":action,"runId":op,"operationId":transfer}).to_string()}),&cancel).await
        };
        let begun = execute("begin").await?;
        let staged: Value = serde_json::from_str(begun.value["oJson"].as_str().unwrap()).unwrap();
        assert_eq!(staged["phase"],"open");
        if std::env::var_os("HERBERT_LEASE_WAIT_EXPIRY").is_some() {
            let started = std::time::Instant::now();
            let deadline = renewed["expiresUtcTicks"].as_i64().unwrap();
            let mut observations = Vec::new();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let observed = call(json!({"action":"inspect"})).await?;
                let server_now = observed["serverUtcTicks"].as_i64().unwrap();
                observations.push(json!({"elapsedSeconds":started.elapsed().as_secs_f64(),"serverUtcTicks":server_now}));
                std::fs::write(root.join("expiry-clock.json"),serde_json::to_vec_pretty(&observations).unwrap()).unwrap();
                if server_now >= deadline { break; }
            }
            let after = call(json!({"action":"renew","operationId":op})).await?;
            std::fs::write(root.join("expiry.json"), serde_json::to_vec_pretty(
                &json!({"elapsedSeconds":started.elapsed().as_secs_f64(),"before":renewed,"after":after})).unwrap()).unwrap();
            if after["accepted"] != false {
                return Err(error("The native clock did not expire the five-minute lease."));
            }
        } else {
            assert_eq!(call(json!({"action":"release","operationId":op})).await?["accepted"],true);
        }
        assert!(execute("begin").await.is_err(), "expired work must be rejected by the server");

        let claim=call(json!({"action":"claim","globalId":id})).await?;
        let item=claim["resources"].as_array().unwrap().iter().find(|r|r["globalId"]==id.to_string()).unwrap();
        assert_eq!(call(json!({"action":"renew","operationId":op})).await?["accepted"],false);
        assert_eq!(call(json!({"action":"reap","globalId":id,"claimId":item["claimId"]})).await?["reaped"],true);

        // This fixture tests registry transitions. Complete its synthetic resource
        // before deleting the only callable test host. No business resource exists.
        assert_eq!(call(json!({"action":"complete","globalId":id,"claimId":item["claimId"]})).await?["completed"],true);
        Ok(json!({"acquire":acquired,"renew":renewed,"releasedClaim":item}))
    }.await;
    // Reconcile independently after success or failure. Never delete another generation.
    let observed = backend::probe(
        &connection,
        NativeArtifactType::FunctionLibrary,
        &library,
        &cancel,
    )
    .await
    .unwrap();
    if let Some((identity, _)) = observed {
        assert_eq!(identity.global_id.as_deref(), Some(id.to_string().as_str()));
        backend::delete(
            &connection,
            NativeArtifactType::FunctionLibrary,
            &identity,
            &cancel,
        )
        .await
        .unwrap();
    }
    assert!(
        backend::probe(
            &connection,
            NativeArtifactType::FunctionLibrary,
            &library,
            &cancel
        )
        .await
        .unwrap()
        .is_none()
    );
    connection.logout(&cancel).await.unwrap();
    std::fs::write(
        root.join("receipt.json"),
        serde_json::to_vec_pretty(&json!({"result":result,"nativeLibraryAbsent":true})).unwrap(),
    )
    .unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
#[ignore = "Explicit operator action: authenticate and create the 2025.2 lab profile."]
async fn create_lab_2025_profile() {
    let root = PathBuf::from(std::env::var("HERBERT_PROFILE_ROOT").unwrap());
    let profiles = profiles::Profiles::new(&root);
    let source = profiles.get("lab-2026-1").unwrap();
    let source_endpoint = crate::input::endpoint(&source.url).unwrap().to_string();
    assert_eq!(source_endpoint, "https://kinetic-2026-1.lab.test:9448/");
    let endpoint = "https://kinetic-2025-2.lab.test:9449/";
    assert!(
        profiles
            .list()
            .unwrap()
            .iter()
            .all(|p| p.id != "lab-2025-2")
    );
    let mut credentials = profiles::secure_read(&source_endpoint, &source.id).unwrap();
    credentials.api_key = None;
    let connection = Connection::new(
        crate::input::endpoint(endpoint).unwrap(),
        "TESTCO".into(),
        AuthMode::Session,
        credentials.clone(),
        OperatingSettings {
            concurrency: 8,
            ..Default::default()
        },
        Budget::new(8).unwrap(),
    )
    .unwrap();
    // Save only after the destination accepts this account. Never print secrets.
    if let Err(error) = connection.connect(&CancellationToken::new()).await {
        panic!("Destination authentication failed: {}", error.code);
    }
    connection.logout(&CancellationToken::new()).await.unwrap();
    profiles::secure_save(endpoint, "lab-2025-2", &credentials).unwrap();
    let mut target = source;
    target.id = "lab-2025-2".into();
    target.name = Some("Lab 2025.2".into());
    target.url = endpoint.into();
    target.auth_mode = AuthMode::Session;
    target.defaults.company = Some("TESTCO".into());
    target.defaults.concurrency = Some(8);
    target.user_file = None;
    profiles.upsert(target).unwrap();
    assert_eq!(profiles.get("lab-2025-2").unwrap().url, endpoint);
    assert_eq!(
        profiles::secure_status(endpoint, "lab-2025-2").unwrap(),
        "saved"
    );
}

async fn request(
    host: &Host,
    plugin: &Plugin,
    run: &Arc<Run>,
    connection: &Arc<Connection>,
    operation: &str,
    identity: &Identity,
    value: HostUtilityRequest,
) -> Result<Value, Diagnostic> {
    host.host_utility(
        plugin,
        operation,
        &value,
        Some(identity),
        run,
        connection,
        &CancellationToken::new(),
    )
    .await
    .map(|result| result.0)
}

#[tokio::test]
#[ignore = "Requires explicit lab profile, plugin path, and retained evidence directory."]
async fn native_host_qualification() {
    let endpoint = std::env::var("HERBERT_QUALIFY_ENDPOINT").unwrap();
    assert!(
        [
            "https://kinetic-2026-1.lab.test:9448/",
            "https://kinetic-2025-2.lab.test:9449/",
        ]
        .contains(&endpoint.as_str()),
        "This test is restricted to the two qualified lab endpoints."
    );
    let profile = std::env::var("HERBERT_QUALIFY_PROFILE").unwrap();
    let root = PathBuf::from(std::env::var("HERBERT_QUALIFY_OUTPUT").unwrap());
    assert!(!root.exists(), "Preserve an earlier qualification result.");
    std::fs::create_dir_all(&root).unwrap();
    let plugin = Plugin::load(Path::new(&std::env::var("HERBERT_QUALIFY_PLUGIN").unwrap()))
        .await
        .unwrap();
    let settings = OperatingSettings {
        concurrency: 8,
        ..Default::default()
    };
    let credentials = profiles::secure_read(&endpoint, &profile).unwrap();
    let mode = if credentials
        .api_key
        .as_ref()
        .is_some_and(|key| !key.is_empty())
    {
        AuthMode::BasicApiKey
    } else {
        AuthMode::Session
    };
    let connection = Connection::new(
        crate::input::endpoint(&endpoint).unwrap(),
        "TESTCO".into(),
        mode,
        credentials,
        settings.clone(),
        Budget::new(8).unwrap(),
    )
    .unwrap();
    let admission = connection.connect(&CancellationToken::new()).await.unwrap();
    let mut journal = storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.endpoint = endpoint;
    journal.company = "TESTCO".into();
    journal.caller = admission["caller"].as_str().unwrap().into();
    journal.settings = settings;
    let run_id = journal.run_id.clone();
    let output = Storage::open(&root.join("output"), false, journal).unwrap();
    let run = Arc::new(Run {
        storage: Arc::new(std::sync::Mutex::new(output)),
        cancel: CancellationToken::new(),
        paused: AtomicBool::new(false),
        wake: Notify::new(),
        active: AtomicBool::new(false),
        network: true,
        helper_restore_attempts: Mutex::new(Default::default()),
        activity: Arc::new(Default::default()),
        shared_reads: Default::default(),
        helper_instances: Mutex::new(Default::default()),
        collection_context: Mutex::new(None),
    });
    let mut host = Host::with_roots(
        root.join("state"),
        root.join("plugins"),
        Some(root.join("coordination")),
    );
    let operation = uuid::Uuid::new_v4().to_string();
    let identity: Identity = serde_json::from_value(json!({
        "category":plugin.category.id,"kind":"HostQualification",
        "scope":{"type":"company","company":"TESTCO"},"keys":{"ID":operation}
    }))
    .unwrap();
    // These calls must fail before a native operation or installation is recorded.
    for method in [CollectionMethod::DirectOnly] {
        collection::update(&run, move |s| {
            s.journal.collection_method = method;
            Ok(())
        })
        .await
        .unwrap();
        let refused = request(
            &host,
            &plugin,
            &run,
            &connection,
            &operation,
            &identity,
            HostUtilityRequest::Begin,
        )
        .await;
        assert!(refused.is_err());
        let saved = collection::snapshot(&run).await.unwrap();
        assert!(saved.native_artifacts.is_empty() && saved.host_transfers.is_empty());
    }
    collection::update(&run, |s| {
        s.journal.collection_method = CollectionMethod::ServerAssisted;
        Ok(())
    })
    .await
    .unwrap();
    request(
        &host,
        &plugin,
        &run,
        &connection,
        &operation,
        &identity,
        HostUtilityRequest::Begin,
    )
    .await
    .unwrap();
    // A fresh server inventory must find the host and preserve its active lease.
    host.sweep_server_helpers(&run, &connection).await.unwrap();
    let file_id = uuid::Uuid::new_v4().to_string();
    let bytes = "Host transfer: π and café.\n".as_bytes();
    let put = request(
        &host,
        &plugin,
        &run,
        &connection,
        &operation,
        &identity,
        HostUtilityRequest::Put {
            file_id,
            name: "nested/π.txt".into(),
            offset: 0,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        },
    )
    .await
    .unwrap();
    let files = vec![StagedFile {
        file_id: put["fileId"].as_str().unwrap().into(),
        name: "nested/π.txt".into(),
        bytes: bytes.len() as u64,
        sha256: storage::sha256(bytes),
    }];
    let sealed = request(
        &host,
        &plugin,
        &run,
        &connection,
        &operation,
        &identity,
        HostUtilityRequest::Seal {
            files: files.clone(),
            compression: ArchiveCompression::Deflate,
        },
    )
    .await
    .unwrap();
    assert!(
        request(
            &host,
            &plugin,
            &run,
            &connection,
            &operation,
            &identity,
            HostUtilityRequest::Close
        )
        .await
        .is_err()
    );
    // Retain one real server chunk, then discard the host's in-memory installation state.
    let chunk = host
        .utility_call(
            &run,
            &connection,
            &operation,
            json!({"action":"read","offset":0,"count":31}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let data = decode_chunk(
        &chunk,
        0,
        sealed["bytes"].as_u64().unwrap(),
        sealed["sha256"].as_str().unwrap(),
        31,
    )
    .unwrap();
    let retained = run
        .storage
        .lock()
        .unwrap()
        .preserve_for(Some(&identity), &data, "bin")
        .unwrap();
    let mut transfer = collection::snapshot(&run).await.unwrap().host_transfers[0].clone();
    transfer.chunks.push(retained.clone());
    transfer.manifest =
        Some(json!({"bytes":sealed["bytes"],"sha256":sealed["sha256"],"files":files}));
    host.save_transfer(&run, transfer.clone()).await.unwrap();
    drop(host);
    // Read the committed checkpoint chain. A JSON round trip cannot prove recovery.
    let restored = crate::checkpoint_log::read_journal(&root.join("output"), &run_id)
        .unwrap()
        .unwrap();
    assert_eq!(restored.host_transfers, vec![transfer.clone()]);
    assert_eq!(restored.collection_method, CollectionMethod::ServerAssisted);
    run.storage.lock().unwrap().journal = restored;
    host = Host::with_roots(
        root.join("state"),
        root.join("plugins"),
        Some(root.join("coordination")),
    );
    // A changed receipt and changed local bytes must each prevent transfer completion.
    let mut changed = transfer.clone();
    changed.manifest.as_mut().unwrap()["bytes"] = json!(0);
    host.save_transfer(&run, changed).await.unwrap();
    assert!(
        request(
            &host,
            &plugin,
            &run,
            &connection,
            &operation,
            &identity,
            HostUtilityRequest::Download { chunk_bytes: 31 }
        )
        .await
        .is_err()
    );
    host.save_transfer(&run, transfer).await.unwrap();
    let local = run
        .storage
        .lock()
        .unwrap()
        .confined(&retained.path)
        .unwrap();
    std::fs::write(&local, b"changed").unwrap();
    assert!(
        request(
            &host,
            &plugin,
            &run,
            &connection,
            &operation,
            &identity,
            HostUtilityRequest::Download { chunk_bytes: 31 }
        )
        .await
        .is_err()
    );
    std::fs::write(&local, &data).unwrap();
    let downloaded = request(
        &host,
        &plugin,
        &run,
        &connection,
        &operation,
        &identity,
        HostUtilityRequest::Download { chunk_bytes: 31 },
    )
    .await
    .unwrap();
    request(
        &host,
        &plugin,
        &run,
        &connection,
        &operation,
        &identity,
        HostUtilityRequest::Close,
    )
    .await
    .unwrap();
    request(
        &host,
        &plugin,
        &run,
        &connection,
        &operation,
        &identity,
        HostUtilityRequest::Purge,
    )
    .await
    .unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    // Eight is Kevin's acceptance setting, not a product workload limit.
    for index in 0..8 {
        let (host, plugin, run, connection) = (
            host.clone(),
            plugin.clone(),
            run.clone(),
            connection.clone(),
        );
        tasks.spawn(async move {
            let operation = uuid::Uuid::new_v4().to_string();
            let identity: Identity = serde_json::from_value(json!({
                "category":plugin.category.id,"kind":"HostQualification",
                "scope":{"type":"company","company":"TESTCO"},"keys":{"ID":operation}
            }))
            .unwrap();
            request(
                &host,
                &plugin,
                &run,
                &connection,
                &operation,
                &identity,
                HostUtilityRequest::Begin,
            )
            .await?;
            let body = format!("Concurrent operation {index}\n");
            let put = request(
                &host,
                &plugin,
                &run,
                &connection,
                &operation,
                &identity,
                HostUtilityRequest::Put {
                    file_id: uuid::Uuid::new_v4().to_string(),
                    name: "result.txt".into(),
                    offset: 0,
                    data: base64::engine::general_purpose::STANDARD.encode(body.as_bytes()),
                },
            )
            .await?;
            request(
                &host,
                &plugin,
                &run,
                &connection,
                &operation,
                &identity,
                HostUtilityRequest::Seal {
                    files: vec![StagedFile {
                        file_id: put["fileId"].as_str().unwrap().into(),
                        name: "result.txt".into(),
                        bytes: body.len() as u64,
                        sha256: storage::sha256(body.as_bytes()),
                    }],
                    compression: if index % 2 == 0 {
                        ArchiveCompression::Store
                    } else {
                        ArchiveCompression::Deflate
                    },
                },
            )
            .await?;
            request(
                &host,
                &plugin,
                &run,
                &connection,
                &operation,
                &identity,
                HostUtilityRequest::Download { chunk_bytes: 37 },
            )
            .await?;
            Ok::<_, Diagnostic>(operation)
        });
    }
    let mut operations = vec![operation];
    let mut transfer_errors = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(operation) => operations.push(operation),
            Err(error) => transfer_errors.push(error),
        }
    }
    let before_cleanup = collection::snapshot(&run).await.unwrap();
    let host_id = before_cleanup.native_artifacts.iter().find(|operation| operation.plugin == OWNER)
        .unwrap().artifacts[0].target.clone();
    let mut shared_journal = storage::new_journal(uuid::Uuid::new_v4().to_string());
    shared_journal.endpoint = connection.endpoint.to_string();
    shared_journal.company = connection.company.clone();
    shared_journal.caller = admission["caller"].as_str().unwrap().into();
    shared_journal.collection_method = CollectionMethod::ServerAssisted;
    let shared = Arc::new(Run {
        storage: Arc::new(std::sync::Mutex::new(Storage::open(&root.join("shared-output"), false, shared_journal).unwrap())),
        cancel: CancellationToken::new(), paused: AtomicBool::new(false), wake: Notify::new(),
        active: AtomicBool::new(false), network: true,
        helper_restore_attempts: Mutex::new(Default::default()), activity: Arc::new(Default::default()),
        shared_reads: Default::default(), helper_instances: Mutex::new(Default::default()),
        collection_context: Mutex::new(None),
    });
    let other_host = Host::with_roots(root.join("shared-state"),root.join("shared-plugins"),None);
    other_host.sweep_server_helpers(&shared,&connection).await.unwrap();
    let shared_run_id = collection::snapshot(&shared).await.unwrap().run_id;
    assert_eq!(other_host.server_leases.lock().await[&shared_run_id].library, host_id);
    let diagnostics = host.cleanup_native_artifacts(&run, &connection).await;
    assert!(crate::native_artifact_backend::probe(&connection,NativeArtifactType::FunctionLibrary,
        &host_id,&CancellationToken::new()).await.unwrap().is_some(), "another active client must retain the host");
    let shared_cleanup = other_host.cleanup_native_artifacts(&shared,&connection).await;
    assert!(shared_cleanup.is_empty(), "{shared_cleanup:?}");
    assert!(crate::native_artifact_backend::probe(&connection,NativeArtifactType::FunctionLibrary,
        &host_id,&CancellationToken::new()).await.unwrap().is_none(), "the last client must remove the inactive host");
    let final_journal = collection::snapshot(&run).await.unwrap();
    assert_eq!(final_journal.host_transfers.len(), 9);
    let replayed = crate::checkpoint_log::read_journal(&root.join("output"), &run_id)
        .unwrap()
        .unwrap();
    assert_eq!(replayed.host_transfers, final_journal.host_transfers);
    std::fs::write(
        root.join("receipt.json"),
        serde_json::to_vec_pretty(&json!({
            "admission":admission,"runId":run_id,"operations":operations,
            "downloaded":downloaded,"beforeCleanup":before_cleanup.native_artifacts,
            "transferErrors":transfer_errors,"cleanupDiagnostics":diagnostics,"sharedCleanupDiagnostics":shared_cleanup,"sharedHostRetainedThenRemoved":true,"journal":final_journal
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(transfer_errors.is_empty(), "{transfer_errors:?}");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(
        final_journal
            .host_transfers
            .iter()
            .all(|t| t.phase == "purged")
    );
    connection.logout(&CancellationToken::new()).await.unwrap();
}
