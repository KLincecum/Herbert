//! Local HTTP integration regressions. Include as a child of helper_installation.rs.
//! No plugin trust bypass: these tests use the real source/binding validators,
//! then exercise the already-admitted restore lifecycle against a fictional server.
use super::*;
use crate::helper_libraries::{LeaseFile, LibraryLease};
use crate::helper_recovery::{self, RecoveryScope};
use crate::helpers::HelperTransfer;
use std::{
    collections::BTreeSet,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Mutex as StdMutex, atomic::AtomicBool},
    thread::JoinHandle,
};

const CALLER: &str = "fixture-caller";
const COMPANY: &str = "FIXTURE";
const HASH: &str = "exact-module-content-hash";
const OPERATION: &str = "e55fe5e9-e334-4314-9097-9fac04406f21";
const STORAGE: &str = "18941a9bd4f044e3bc7f160cc31f47d3";

#[derive(Clone, Copy)]
enum ImportOutcome {
    Succeeded,
    CommittedResponseLost,
    AbsentResponseLost,
}
#[derive(Clone)]
struct Request {
    method: String,
    args: Value,
}
struct ServerState {
    package: Option<Value>,
    requests: Vec<Request>,
    outcome: ImportOutcome,
    errors: Vec<String>,
}
struct NativeFixture {
    endpoint: url::Url,
    state: Arc<StdMutex<ServerState>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl NativeFixture {
    fn new(outcome: ImportOutcome) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let endpoint = url::Url::parse(&format!("http://{address}/fixture/")).unwrap();
        let state = Arc::new(StdMutex::new(ServerState {
            package: None,
            requests: vec![],
            outcome,
            errors: vec![],
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let state_worker = state.clone();
        let stop_worker = stop.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(stream) => stream,
                    Err(error) => {
                        state_worker.lock().unwrap().errors.push(error.to_string());
                        break;
                    }
                };
                if stop_worker.load(Ordering::SeqCst) {
                    break;
                }
                let response = (|| -> Result<Option<Value>, String> {
                    let mut reader = BufReader::new(&mut stream);
                    let mut first = String::new();
                    reader.read_line(&mut first).map_err(|e| e.to_string())?;
                    let path = first.split_whitespace().nth(1).ok_or("No request path")?;
                    let method = path.rsplit('/').next().ok_or("No method")?.to_owned();
                    let mut length = None;
                    loop {
                        let mut line = String::new();
                        reader.read_line(&mut line).map_err(|e| e.to_string())?;
                        if line == "\r\n" {
                            break;
                        }
                        if line.is_empty() {
                            return Err("Truncated headers".into());
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            if name.eq_ignore_ascii_case("content-length") {
                                length =
                                    Some(value.trim().parse::<usize>().map_err(|e| e.to_string())?);
                            }
                        }
                    }
                    let mut bytes = vec![0; length.ok_or("Missing Content-Length")?];
                    reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                    let args: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                    let mut state = state_worker.lock().unwrap();
                    state.requests.push(Request {
                        method: method.clone(),
                        args: args.clone(),
                    });
                    match method.as_str() {
                        "GetUserFile" => Ok(Some(json!({"returnObj":{"UserFile":[{
                            "DcdUserID":CALLER,"SecurityMgr":true,"UserDisabled":false,"LockedOut":false
                        }]}}))),
                        "GetDatabaseVersion" => Ok(Some(json!({"returnObj":"5.2.100"}))),
                        "GetDefaults" => Ok(Some(
                            json!({"returnObj":{"EfxLibrary":[{"EpicorVersion":"5.2.100"}]}}),
                        )),
                        "IsValidLibraryId" => Ok(Some(json!({"returnObj":""}))),
                        "GetLibrary" => {
                            let Some(package) = &state.package else {
                                return Ok(Some(
                                    json!({"returnObj":{"EfxLibrary":[],"EfxFunction":[]}}),
                                ));
                            };
                            if args["libraryId"] != package["LibraryId"] {
                                return Err("GetLibrary targeted a different generation".into());
                            }
                            let id = package["LibraryId"].clone();
                            let rows = package["Library"]["Functions"].as_array().unwrap()
                                .iter().map(|function|json!({
                                    "LibraryID":id,"FunctionID":function["FunctionID"],
                                    "Disabled":function["Disabled"],"Invalid":function["Invalid"]
                                })).collect::<Vec<_>>();
                            Ok(Some(json!({"returnObj":{
                                "EfxLibrary":[{"LibraryID":id,
                                    "GlobalID":package["Library"]["GlobalID"],
                                    "Disabled":package["Library"]["Disabled"]}],
                                "EfxFunction":rows
                            }})))
                        }
                        "ImportLibrary" => {
                            if state.package.is_some() {
                                return Err(
                                    "Import attempted to overwrite a present library".into()
                                );
                            }
                            if args["options"]["OverwriteMode"] != json!(0) {
                                return Err("Restoration did not use OverwriteMode 0".into());
                            }
                            let bytes = base64::engine::general_purpose::STANDARD
                                .decode(args["libraryPackage"].as_str().ok_or("No package")?)
                                .map_err(|e| e.to_string())?;
                            let mut package: Value = serde_json::from_slice(
                                bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes),
                            )
                            .map_err(|e| e.to_string())?;
                            if args["options"]["NewLibraryId"] != package["LibraryId"] {
                                return Err("Import changed the retained LibraryID".into());
                            }
                            match state.outcome {
                                ImportOutcome::AbsentResponseLost => return Ok(None),
                                ImportOutcome::Succeeded | ImportOutcome::CommittedResponseLost => {
                                }
                            }
                            package["Library"]["Disabled"] = json!(true);
                            package["Library"]["Published"] = json!(false);
                            for function in package["Library"]["Functions"].as_array_mut().unwrap()
                            {
                                function["Disabled"] = json!(true);
                            }
                            state.package = Some(package);
                            if matches!(state.outcome, ImportOutcome::CommittedResponseLost) {
                                Ok(None)
                            } else {
                                Ok(Some(json!({"returnObj":{}})))
                            }
                        }
                        "ExportLibrary" => {
                            let package = state
                                .package
                                .as_ref()
                                .ok_or("Export requested absent library")?;
                            if args["libraryID"] != package["LibraryId"] {
                                return Err("Export targeted a different generation".into());
                            }
                            let mut bytes = vec![0xef, 0xbb, 0xbf];
                            bytes.extend(serde_json::to_vec(package).unwrap());
                            Ok(Some(
                                json!({"returnObj":base64::engine::general_purpose::STANDARD.encode(bytes)}),
                            ))
                        }
                        "ApplyChangesWithDiagnostics" => {
                            let package = state
                                .package
                                .as_mut()
                                .ok_or("Compile requested absent library")?;
                            let id = &package["LibraryId"];
                            for table in ["EfxLibrary", "EfxFunction"] {
                                for row in args["libraryTableset"][table]
                                    .as_array()
                                    .ok_or("Missing enable table")?
                                {
                                    if row["LibraryID"] != *id
                                        || row["Disabled"] != json!(false)
                                        || row["RowMod"] != json!("U")
                                    {
                                        return Err(
                                            "Enable mutated an unexpected row or value".into()
                                        );
                                    }
                                }
                            }
                            package["Library"]["Disabled"] = json!(false);
                            for function in package["Library"]["Functions"].as_array_mut().unwrap()
                            {
                                function["Disabled"] = json!(false);
                                function["Invalid"] = json!(false);
                            }
                            Ok(Some(json!({"returnObj":{},"diagnostics":[]})))
                        }
                        "PromoteToProduction" => {
                            let package = state
                                .package
                                .as_mut()
                                .ok_or("Promote requested absent library")?;
                            if args["libraryID"] != package["LibraryId"] {
                                return Err("Promote changed identity".into());
                            }
                            package["Library"]["Published"] = json!(true);
                            Ok(Some(json!({"returnObj":{}})))
                        }
                        "ReadChunk" => {
                            if state.package.is_none() {
                                return Err("Cleanup called absent helper".into());
                            }
                            if args["iOperationId"] != json!(OPERATION)
                                || args["iStorageInstanceId"] != json!(STORAGE)
                                || args["iCompany"] != json!(COMPANY)
                                || args["iAllowCreate"] != json!(false)
                            {
                                return Err(
                                    "Cleanup changed retained operation or storage binding".into(),
                                );
                            }
                            if !matches!(args["iOperation"].as_str(), Some("Close" | "Acknowledge"))
                            {
                                return Err(
                                    "Recovery created or resumed a different snapshot".into()
                                );
                            }
                            Ok(Some(json!({"oSuccess":true,"oDS":{"Transfer":[{
                                "Closed":true,"StorageInstanceId":STORAGE
                            }]},"oAdmissionRejected":false})))
                        }
                        method => Err(format!("Unexpected native method: {method}")),
                    }
                })();
                let response = match response {
                    Ok(None) => continue, // Close socket after the selected mutation outcome.
                    Ok(Some(value)) => value,
                    Err(error) => {
                        state_worker.lock().unwrap().errors.push(error.clone());
                        json!({"fixtureError":error})
                    }
                };
                let bytes = serde_json::to_vec(&response).unwrap();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                if let Err(error) = stream
                    .write_all(header.as_bytes())
                    .and_then(|_| stream.write_all(&bytes))
                {
                    state_worker.lock().unwrap().errors.push(error.to_string());
                }
            }
        });
        Self {
            endpoint,
            state,
            stop,
            thread: Some(thread),
        }
    }
    fn requests(&self, method: &str) -> Vec<Request> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.method == method)
            .cloned()
            .collect()
    }
    fn assert_clean(&self) {
        let errors = self.state.lock().unwrap().errors.clone();
        assert!(errors.is_empty(), "{errors:?}");
    }
}
impl Drop for NativeFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect((
            self.endpoint.host_str().unwrap(),
            self.endpoint.port().unwrap(),
        ));
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

struct Case {
    _root: tempfile::TempDir,
    host: Arc<Host>,
    connection: Arc<Connection>,
    file: LeaseFile,
    run: Arc<Run>,
    transfer: HelperTransfer,
    bytes: Vec<u8>,
}
impl Case {
    async fn new(server: &NativeFixture) -> Self {
        let root = tempfile::tempdir().unwrap();
        let host = Host::with_roots(
            root.path().join("state"),
            root.path().join("plugins"),
            Some(root.path().join("coordination")),
        );
        let connection = Connection::new(
            server.endpoint.clone(),
            COMPANY.into(),
            AuthMode::BasicApiKey,
            Credentials {
                username: CALLER.into(),
                password: "fictional-password".into(),
                api_key: Some("fictional-api-key".into()),
            },
            OperatingSettings::default(),
            Budget::new(4).unwrap(),
        )
        .unwrap();
        connection.connect(&CancellationToken::new()).await.unwrap();
        let template = crate::helper_recovery::recovery_test_contract();
        let holder = uuid::Uuid::new_v4().to_string();
        let mut file = LeaseFile::open(
            &host.state_root,
            &holder,
            &template,
            HASH,
            &connection,
            CALLER,
        )
        .unwrap();
        let native = &template
            .installation
            .as_ref()
            .unwrap()
            .packages
            .iter()
            .find(|p| p.epicor_version == "5.2.100")
            .unwrap()
            .package;
        let (bytes, _) = file.package(native).unwrap();
        file.freeze_package(&bytes).unwrap();
        file.phase("ready").unwrap();
        let transfer = HelperTransfer {
            operation_id: OPERATION.into(),
            identity: Identity {
                category: "fixtureResource".into(),
                kind: "FixtureResource".into(),
                scope: Scope::Global,
                keys: BTreeMap::new(),
            },
            claim: file.lease.instance.clone(),
            function: "ReadChunk".into(),
            close_arguments: json!({
                "iOperation":"Close","iOperationId":OPERATION,"iCompany":COMPANY,
                "iFolderIsNull":false,"iFolder":"fixture-folder",
                "iAssemblyIsNull":false,"iAssembly":"fixture-assembly",
                "iVersionIsNull":true,"iVersion":"",
                "iExpectedFingerprint":"original-snapshot-fingerprint","iStorageInstanceId":STORAGE,
                "iAllowCreate":false,"iPayload":"","iOffset":0,"iRequestedBytes":0
            }),
            phase: "open".into(),
            plugin_hash: HASH.into(),
            dispatch_count: 1,
        };
        let journal: storage::RunJournal = serde_json::from_value(json!({
            "schemaVersion":1,"runId":holder,"requestId":uuid::Uuid::new_v4().to_string(),
            "endpoint":server.endpoint.as_str(),"company":COMPANY,"caller":CALLER,
            "scope":"global","categories":["fixtureResource"],
            "pluginHashes":{"fixtureResource":HASH},"settings":OperatingSettings::default(),
            "phase":"partial","artifacts":[],"diagnostics":[],"pendingFiles":[],"cursors":{},
            "helperTransfers":[transfer],"helperLibraries":[file.lease]
        }))
        .unwrap();
        let run = Arc::new(Run {
            storage: Arc::new(StdMutex::new(
                Storage::open(&root.path().join("output"), false, journal).unwrap(),
            )),
            cancel: CancellationToken::new(),
            paused: AtomicBool::new(false),
            wake: Notify::new(),
            active: AtomicBool::new(false),
            network: true,
            helper_restore_attempts: Mutex::new(BTreeSet::new()),
            activity: Arc::new(Default::default()),
            shared_reads: Default::default(),
            helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
        });
        Case {
            _root: root,
            host,
            connection,
            file,
            run,
            transfer,
            bytes,
        }
    }
    async fn restore(&mut self) -> Result<(), Diagnostic> {
        let original = self.file.lease.clone();
        let scope = RecoveryScope {
            run_id: &original.holder,
            endpoint: &original.endpoint,
            company: &original.company,
            caller: &original.caller,
        };
        helper_recovery::validate_binding(&self.file.lease, &original, &self.transfer, &scope)?;
        assert!(
            !helper_libraries::exists(
                &self.connection,
                &self.file.lease.instance.library_id,
                &CancellationToken::new()
            )
            .await?
        );
        helper_recovery::verify_bound_bytes(&self.file.lease, &self.bytes, true)?;
        self.host
            .restore_helper_generation(
                &mut self.file,
                &self.bytes,
                &self.connection,
                &self.run,
                &CancellationToken::new(),
            )
            .await
    }
    fn retained_transfer(&self) -> HelperTransfer {
        self.run.storage.lock().unwrap().journal.helper_transfers[0].clone()
    }
    fn assert_original_import(&self, server: &NativeFixture, original: &LibraryLease) {
        let requests = server.requests("ImportLibrary");
        assert_eq!(requests.len(), 1);
        let actual = base64::engine::general_purpose::STANDARD
            .decode(requests[0].args["libraryPackage"].as_str().unwrap())
            .unwrap();
        assert_eq!(actual, self.bytes);
        assert_eq!(
            requests[0].args["options"]["NewLibraryId"],
            original.instance.library_id
        );
        assert_eq!(
            self.file.lease.instance.global_id,
            original.instance.global_id
        );
        assert_eq!(self.file.lease.owner_marker, original.owner_marker);
        assert_eq!(self.file.lease.frozen_package, original.frozen_package);
    }
}

#[tokio::test]
async fn missing_helper_restores_exact_generation_and_closes_original_transfer() {
    let server = NativeFixture::new(ImportOutcome::Succeeded);
    let mut case = Case::new(&server).await;
    let original = case.file.lease.clone();
    let before = serde_json::to_value(&case.transfer).unwrap();
    case.restore().await.unwrap();
    assert_eq!(case.file.lease.phase, "ready");
    case.assert_original_import(&server, &original);
    assert_eq!(
        serde_json::to_value(case.retained_transfer()).unwrap(),
        before
    );
    assert_eq!(server.requests("ApplyChangesWithDiagnostics").len(), 1);
    assert_eq!(server.requests("PromoteToProduction").len(), 1);
    // The common ensure path verifies the restored original without a plugin.
    let claim = case
        .host
        .ensure_retained_helper(
            &case.run,
            &case.connection,
            &case.transfer,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(claim.global_id, original.instance.global_id);
    let errors = case
        .host
        .cleanup_helper_transfers(&case.run, &case.connection, None)
        .await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(case.retained_transfer().phase, "closed");
    assert_eq!(
        server
            .requests("ReadChunk")
            .iter()
            .map(|r| r.args["iOperation"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec!["Close", "Acknowledge"]
    );
    case.assert_original_import(&server, &original);
    server.assert_clean();
}

#[tokio::test]
async fn committed_import_with_lost_response_reconciles_without_second_import() {
    let server = NativeFixture::new(ImportOutcome::CommittedResponseLost);
    let mut case = Case::new(&server).await;
    let original = case.file.lease.clone();
    case.restore().await.unwrap();
    case.assert_original_import(&server, &original);
    assert_eq!(case.file.lease.phase, "ready");
    // Once native ownership is resolved, the attempt fence is cleared. A later
    // common recovery call must observe presence before considering another import.
    assert!(case.run.helper_restore_attempts.lock().await.is_empty());
    case.host
        .ensure_retained_helper(
            &case.run,
            &case.connection,
            &case.transfer,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    case.assert_original_import(&server, &original);
    assert_eq!(server.requests("ApplyChangesWithDiagnostics").len(), 1);
    assert_eq!(server.requests("PromoteToProduction").len(), 1);
    assert!(
        case.host
            .cleanup_helper_transfers(&case.run, &case.connection, None)
            .await
            .is_empty()
    );
    case.assert_original_import(&server, &original);
    server.assert_clean();
}

#[tokio::test]
async fn failed_absent_import_is_not_replayed_by_automatic_teardown() {
    let server = NativeFixture::new(ImportOutcome::AbsentResponseLost);
    let mut case = Case::new(&server).await;
    let original = case.file.lease.clone();
    let before = case.transfer.close_arguments.clone();
    assert!(case.restore().await.is_err());
    case.assert_original_import(&server, &original);
    // The common path sees the same attempted generation. It does not search for source or import a package.
    let refused = case
        .host
        .ensure_retained_helper(
            &case.run,
            &case.connection,
            &case.transfer,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, "HELPER_RECOVERY_ATTEMPT_UNRESOLVED");
    let refused = case
        .host
        .restore_helper_generation(
            &mut case.file,
            &case.bytes,
            &case.connection,
            &case.run,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, "HELPER_RECOVERY_ATTEMPT_UNRESOLVED");
    let errors = case
        .host
        .cleanup_helper_transfers(&case.run, &case.connection, None)
        .await;
    assert!(!errors.is_empty());
    case.assert_original_import(&server, &original);
    let retained = case.retained_transfer();
    assert_eq!(retained.operation_id, OPERATION);
    assert_eq!(retained.close_arguments, before);
    assert!(!matches!(retained.phase.as_str(), "closed" | "notAdmitted"));
    assert!(server.requests("ReadChunk").is_empty());
    assert!(server.requests("ApplyChangesWithDiagnostics").is_empty());
    assert!(server.requests("PromoteToProduction").is_empty());
    assert!(server.state.lock().unwrap().package.is_none());
    server.assert_clean();
}

#[tokio::test]
async fn independently_missing_after_resolved_restore_can_restore_same_generation_again() {
    let server = NativeFixture::new(ImportOutcome::Succeeded);
    let mut case = Case::new(&server).await;
    let original = case.file.lease.clone();
    let before = serde_json::to_value(&case.transfer).unwrap();
    case.restore().await.unwrap();
    assert!(case.run.helper_restore_attempts.lock().await.is_empty());
    // This is a new independently observed disappearance after the first import
    // was resolved. A replay fence must not become a permanent count limit.
    server.state.lock().unwrap().package = None;
    case.restore().await.unwrap();
    assert!(case.run.helper_restore_attempts.lock().await.is_empty());
    let imports = server.requests("ImportLibrary");
    assert_eq!(imports.len(), 2);
    for request in imports {
        let actual = base64::engine::general_purpose::STANDARD
            .decode(request.args["libraryPackage"].as_str().unwrap())
            .unwrap();
        assert_eq!(actual, case.bytes);
        assert_eq!(
            request.args["options"]["NewLibraryId"],
            original.instance.library_id
        );
    }
    assert_eq!(
        case.file.lease.instance.global_id,
        original.instance.global_id
    );
    assert_eq!(case.file.lease.owner_marker, original.owner_marker);
    assert_eq!(
        serde_json::to_value(case.retained_transfer()).unwrap(),
        before
    );
    server.assert_clean();
}

#[tokio::test]
async fn missing_retained_lease_refuses_without_inventing_replacement_generation() {
    let server = NativeFixture::new(ImportOutcome::Succeeded);
    let case = Case::new(&server).await;
    let original = serde_json::to_value(case.retained_transfer()).unwrap();
    let key = storage::sha256(
        &serde_json::to_vec(&(&case.file.lease.holder, &case.file.lease.template_id)).unwrap(),
    );
    let path = case
        .host
        .state_root
        .join("helper-libraries")
        .join(format!("{key}.json"));
    // Remove only the exact test-owned file to simulate an unavailable owner record.
    std::fs::remove_file(&path).unwrap();
    let error = case
        .host
        .ensure_retained_helper(
            &case.run,
            &case.connection,
            &case.transfer,
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "HELPER_RECOVERY_LEASE_REQUIRED");
    assert_eq!(
        serde_json::to_value(case.retained_transfer()).unwrap(),
        original
    );
    assert!(!path.exists());
    assert!(server.requests("ImportLibrary").is_empty());
    assert!(server.requests("ReadChunk").is_empty());
    assert_eq!(
        std::fs::read_dir(case.host.state_root.join("helper-libraries"))
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "json"))
            .count(),
        0
    );
    assert_eq!(case.file.frozen_package().unwrap(), case.bytes);
    server.assert_clean();
}

#[tokio::test]
async fn helper_definition_is_read_once_per_run_but_cleanup_reads_current_source() {
    let server = NativeFixture::new(ImportOutcome::Succeeded);
    let mut case = Case::new(&server).await;
    case.restore().await.unwrap();
    let before = server.requests("ExportLibrary").len();
    for _ in 0..3 {
        case.host
            .verify_helper_claim(
                &case.file.lease.instance,
                "ReadChunk",
                &case.connection,
                Some(&case.run),
                None,
                &CancellationToken::new(),
                true,
            )
            .await
            .unwrap();
    }
    assert_eq!(server.requests("ExportLibrary").len(), before + 1);
    server.state.lock().unwrap().package.as_mut().unwrap()["Library"]["Disabled"] = json!(true);
    assert!(
        case.host
            .verify_helper_claim(
                &case.file.lease.instance,
                "ReadChunk",
                &case.connection,
                Some(&case.run),
                None,
                &CancellationToken::new(),
                false,
            )
            .await
            .is_err()
    );
    assert_eq!(server.requests("ExportLibrary").len(), before + 2);
    server.assert_clean();
}
