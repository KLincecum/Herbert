//! Exercise real HTTP dispatch, durable phases and cleanup with a local server.
use super::*;
use base64::Engine as _;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::Mutex as StdMutex,
};

const BYTES: &[u8] = b"<Export>exact source\r\n</Export>";
#[derive(Clone, Copy, PartialEq)]
enum Outcome {
    Success,
    ReadFailed,
    DeleteLost,
    DeleteFailed,
    ExportUncertain,
    ExportLost,
    Canceled,
    Pair,
    PairReadFailed,
    PairDeleteFailed,
    PairUncertain,
    PairCanceled,
    PairMissing,
    CompanionCollision,
}
struct State {
    file: Option<String>,
    companions: Vec<String>,
    calls: Vec<String>,
    outcome: Outcome,
    cancel: CancellationToken,
    revoke_on_exists:Option<(std::path::PathBuf,String)>,
}
struct Server {
    endpoint: url::Url,
    state: Arc<StdMutex<State>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new(outcome: Outcome) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = url::Url::parse(&format!(
            "http://{}/fixture/",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let state = Arc::new(StdMutex::new(State {
            file: None,
            companions: vec![],
            calls: vec![],
            outcome,
            cancel: CancellationToken::new(),
            revoke_on_exists:None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = state.clone();
        let done = stop.clone();
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if done.load(Ordering::SeqCst) {
                    break;
                }
                let mut reader = BufReader::new(&mut stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let method = first
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_owned();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        if k.eq_ignore_ascii_case("content-length") {
                            length = v.trim().parse().unwrap();
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                drop(reader);
                let args: Value = serde_json::from_slice(&body).unwrap();
                let mut s = shared.lock().unwrap();
                s.calls.push(method.clone());
                let mut status = "200 OK";
                let value = match method.as_str() {
                    "GetUserFile" => {
                        json!({"returnObj":{"UserFile":[{"DcdUserID":"tester","SecurityMgr":true,"UserDisabled":false,"LockedOut":false}]}})
                    }
                    "GetDatabaseVersion" => json!({"returnObj":"5.2.100"}),
                    "FileExists" => {
                        if let Some((root,id))=s.revoke_on_exists.take(){crate::server_changes::revoke(&root,&id).unwrap();}
                        json!({"returnObj":s.file.as_deref()==args["serverPath"].as_str() || s.companions.iter().any(|p|Some(p.as_str())==args["serverPath"].as_str()) || (s.outcome==Outcome::CompanionCollision && args["serverPath"].as_str().unwrap().ends_with(".xml"))})
                    }
                    "ExportOwned" => {
                        let path = args["fileName"].as_str().unwrap().to_owned();
                        s.file = Some(path.clone());
                        if matches!(
                            s.outcome,
                            Outcome::Pair
                                | Outcome::PairReadFailed
                                | Outcome::PairDeleteFailed
                                | Outcome::PairUncertain
                                | Outcome::PairCanceled
                        ) {
                            s.companions
                                .push(format!("{}.xml", path.rsplit_once('.').unwrap().0));
                        }
                        if matches!(s.outcome, Outcome::Canceled | Outcome::PairCanceled) {
                            s.cancel.cancel();
                        }
                        if s.outcome == Outcome::ExportLost {
                            continue;
                        }
                        if matches!(s.outcome, Outcome::ExportUncertain | Outcome::PairUncertain) {
                            status = "500 Server Error";
                        }
                        json!({})
                    }
                    "DownloadFile" => {
                        let path = args["serverPath"].as_str().unwrap();
                        let present = s.file.as_deref() == Some(path)
                            || s.companions.iter().any(|p| p == path);
                        if !present {
                            status = "404 Not Found";
                            json!({})
                        } else if s.outcome == Outcome::ReadFailed
                            || (s.outcome == Outcome::PairReadFailed && path.ends_with(".xml"))
                        {
                            status = "500 Server Error";
                            json!({})
                        } else {
                            json!({"returnObj":base64::engine::general_purpose::STANDARD.encode(BYTES)})
                        }
                    }
                    "FileDelete" => {
                        let path = args["serverPath"].as_str().unwrap();
                        assert!(
                            s.file.as_deref() == Some(path)
                                || s.companions.iter().any(|p| p == path)
                        );
                        if s.outcome == Outcome::DeleteFailed
                            || (s.outcome == Outcome::PairDeleteFailed && path.ends_with(".zip"))
                        {
                            status = "500 Server Error";
                        } else {
                            if s.file.as_deref() == Some(path) {
                                s.file = None;
                            }
                            s.companions.retain(|p| p != path);
                        }
                        if s.outcome == Outcome::DeleteLost {
                            continue;
                        }
                        json!({})
                    }
                    _ => panic!("Unexpected route {method}"),
                };
                let text = serde_json::to_string(&value).unwrap();
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();
            }
        });
        Self {
            endpoint,
            state,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.endpoint.socket_addrs(|| None).unwrap()[0]);
        self.worker.take().unwrap().join().unwrap();
    }
}

async fn case(server: &Server) -> (tempfile::TempDir, Arc<Host>, Arc<Connection>, Arc<Run>, Identity) {
    let root = tempfile::tempdir().unwrap();
    let connection = Connection::new(
        server.endpoint.clone(),
        "FIXTURE".into(),
        AuthMode::BasicApiKey,
        Credentials {
            username: "tester".into(),
            password: "fictional".into(),
            api_key: Some("fictional-key".into()),
        },
        OperatingSettings::default(),
        Budget::new(4).unwrap(),
    )
    .unwrap();
    connection.connect(&CancellationToken::new()).await.unwrap();
    let identity = Identity {
        category: "test".into(),
        kind: "Export".into(),
        scope: Scope::Global,
        keys: BTreeMap::new(),
    };
    let mut journal:storage::RunJournal=serde_json::from_value(json!({"schemaVersion":1,"runId":uuid::Uuid::new_v4().to_string(),"requestId":"test","endpoint":server.endpoint,"company":"FIXTURE","caller":"tester","scope":"global","categories":["test"],"pluginHashes":{},"settings":OperatingSettings::default(),"phase":"acquiring","artifacts":[],"diagnostics":[],"pendingFiles":[],"cursors":{}})).unwrap();
    let host=Host::new(root.path().join("state"),root.path().join("plugins"));
    let scope=crate::server_changes::MaterialScope{endpoint:server.endpoint.to_string(),company:"FIXTURE".into(),principal:"tester".into(),capabilities:BTreeMap::from([("test".into(),crate::server_changes::Capability{binding:"fixture".into(),selection:BTreeMap::from([("$nativeTargets".into(),crate::server_changes::Selection::NativeTargets{category:"test".into(),targets:crate::server_changes::Targets::Exact{values:std::collections::BTreeSet::from([serde_json::to_string(&identity).unwrap()])}})]),effects:BTreeMap::from([("nativeFile:Ice.BO.FixtureSvc/ExportOwned".into(),json!({"kind":"nativeExportFiles","service":"Ice.BO.FixtureSvc","method":"ExportOwned","folder":4}))])})])};
    let grant=crate::server_changes::create(&host.state_root,scope.clone(),crate::server_changes::Lifetime::OneRun{request_id:journal.request_id.clone()},None,"explicitLocalClient").unwrap();
    journal.collection_method=CollectionMethod::DirectOnly;
    journal.server_change_authority=Some(crate::server_changes::RunAuthority{grant_id:grant.id,material_digest:scope.digest(),material_scope:scope,request_id:journal.request_id.clone(),original_session:host.authorization_session.read().await.clone(),workflow_id:None});
    let run = Arc::new(Run {
        storage: Arc::new(StdMutex::new(
            Storage::open(&root.path().join("output"), false, journal).unwrap(),
        )),
        cancel: CancellationToken::new(),
        paused: AtomicBool::new(false),
        wake: Notify::new(),
        active: AtomicBool::new(false),
        network: true,
        helper_restore_attempts: Mutex::new(Default::default()),
        activity: Arc::new(Default::default()),
        shared_reads: Default::default(),
        helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
    });
    (root, host, connection, run, identity)
}

#[tokio::test]
async fn export_preserves_bytes_and_reconciles_cleanup_failures() {
    for outcome in [
        Outcome::Success,
        Outcome::ReadFailed,
        Outcome::DeleteLost,
        Outcome::DeleteFailed,
        Outcome::Canceled,
        Outcome::ExportUncertain,
    ] {
        let server = Server::new(outcome);
        let (root, host, connection, run, identity) = case(&server).await;
        let cancel = server.state.lock().unwrap().cancel.clone();
        let result = perform_export(
            &host, "fixture",
            &run,
            &connection,
            identity.clone(),
            "Ice.BO.FixtureSvc",
            "ExportOwned",
            json!({}),
            "fileName",
            4,
            "xml",
            &cancel,
        )
        .await;
        let record = collection::snapshot(&run)
            .await
            .unwrap()
            .native_export_files[0]
            .clone();
        if matches!(outcome, Outcome::Success | Outcome::DeleteLost) {
            let file = result.unwrap();
            assert_eq!(
                std::fs::read(root.path().join("output").join(file.path)).unwrap(),
                BYTES
            );
            assert_eq!(record.phase, "removed");
        } else {
            assert!(result.is_err());
        }
        let calls = server.state.lock().unwrap().calls.clone();
        assert_eq!(
            calls.iter().filter(|m| m.as_str() == "ExportOwned").count(),
            1
        );
        if outcome == Outcome::ExportUncertain {
            assert_eq!(record.phase, "uncertain");
            assert!(!calls.iter().any(|m| m == "FileDelete"));
            assert!(
                perform_export(
            &host, "fixture",
                    &run,
                    &connection,
                    identity,
                    "Ice.BO.FixtureSvc",
                    "ExportOwned",
                    json!({}),
                    "fileName",
                    4,
                    "xml",
                    &CancellationToken::new()
                )
                .await
                .is_err()
            );
            assert_eq!(
                server
                    .state
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|m| m.as_str() == "ExportOwned")
                    .count(),
                1
            );
        } else if outcome == Outcome::DeleteFailed {
            assert_eq!(record.phase, "deleting");
            // Simulate a later recovery using the deserialized durable record.
            server.state.lock().unwrap().outcome = Outcome::Success;
            let mut restored: ExportFile =
                serde_json::from_value(serde_json::to_value(record).unwrap()).unwrap();
            cleanup(&host, &run, &connection, &mut restored).await.unwrap();
            assert_eq!(restored.phase, "removed");
        }
        if outcome != Outcome::ExportUncertain {
            assert!(server.state.lock().unwrap().file.is_none());
        }
    }
}

#[tokio::test]
async fn changed_owner_cannot_export_or_cleanup() {
    let server = Server::new(Outcome::Success);
    let (_root, host, _original, run, identity) = case(&server).await;
    let other = Server::new(Outcome::Success);
    let (_other_root, _other_host, connection, _other_run, _) = case(&other).await;
    let before = other.state.lock().unwrap().calls.len();
    assert!(
        perform_export(
            &host, "fixture",
            &run,
            &connection,
            identity.clone(),
            "Ice.BO.FixtureSvc",
            "ExportOwned",
            json!({}),
            "fileName",
            4,
            "xml",
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    let id = uuid::Uuid::new_v4().to_string();
    let mut record = ExportFile {
        server_path: format!("Herbert-{id}.xml"),
        operation_id: id,
        identity,
        folder: 4,
        phase: "exported".into(),
        file: None,
        authorization:None,
    };
    assert!(cleanup(&host, &run, &connection, &mut record).await.is_err());
    assert_eq!(other.state.lock().unwrap().calls.len(), before);
}

#[tokio::test]
async fn restart_recovers_cleanup_without_replaying_export() {
    let server = Server::new(Outcome::DeleteFailed);
    let (root, host, connection, run, identity) = case(&server).await;
    assert!(
        perform_export(
            &host, "fixture",
            &run,
            &connection,
            identity,
            "Ice.BO.FixtureSvc",
            "ExportOwned",
            json!({}),
            "fileName",
            4,
            "xml",
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    let id = collection::snapshot(&run).await.unwrap().run_id;
    drop(run);
    // Reopen committed disk state after releasing the original destination lock.
    let journal = crate::checkpoint_log::read_journal(&root.path().join("output"), &id)
        .unwrap()
        .unwrap();
    assert_eq!(journal.native_export_files[0].phase, "deleting");
    let mut record = journal.native_export_files[0].clone();
    let resumed = Arc::new(Run {
        storage: Arc::new(StdMutex::new(
            Storage::open(&root.path().join("output"), false, journal).unwrap(),
        )),
        cancel: CancellationToken::new(),
        paused: AtomicBool::new(false),
        wake: Notify::new(),
        active: AtomicBool::new(false),
        network: true,
        helper_restore_attempts: Mutex::new(Default::default()),
        activity: Arc::new(Default::default()),
        shared_reads: Default::default(),
        helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
    });
    server.state.lock().unwrap().outcome = Outcome::Success;
    cleanup(&host, &resumed, &connection, &mut record).await.unwrap();
    assert_eq!(record.phase, "removed");
    let saved = record.file.unwrap();
    assert_eq!(
        std::fs::read(root.path().join("output").join(saved.path)).unwrap(),
        BYTES
    );
    let state = server.state.lock().unwrap();
    assert!(state.file.is_none());
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|m| m.as_str() == "ExportOwned")
            .count(),
        1
    );
}

#[tokio::test]
async fn companion_files_preserve_every_output_and_cleanup_independently() {
    for outcome in [
        Outcome::Pair,
        Outcome::PairReadFailed,
        Outcome::PairDeleteFailed,
        Outcome::PairUncertain,
        Outcome::PairCanceled,
        Outcome::PairMissing,
        Outcome::CompanionCollision,
    ] {
        let server = Server::new(outcome);
        let (root, host, connection, run, identity) = case(&server).await;
        let cancel = server.state.lock().unwrap().cancel.clone();
        let result = perform_exports(
            &host, "fixture",
            &run,
            &connection,
            identity,
            "Ice.BO.FixtureSvc",
            "ExportOwned",
            json!({}),
            "fileName",
            4,
            "zip",
            &["xml".into()],
            &cancel,
        )
        .await;
        let journal = collection::snapshot(&run).await.unwrap();
        assert_eq!(journal.native_export_files.len(), 2);
        let committed =
            crate::checkpoint_log::read_journal(&root.path().join("output"), &journal.run_id)
                .unwrap()
                .unwrap();
        assert_eq!(committed.native_export_files, journal.native_export_files);
        assert_eq!(
            journal.native_export_files[0].operation_id,
            journal.native_export_files[1].operation_id
        );
        let calls = server.state.lock().unwrap().calls.clone();
        if outcome == Outcome::CompanionCollision {
            assert!(result.is_err());
            assert!(
                !calls
                    .iter()
                    .any(|m| m == "ExportOwned" || m == "FileDelete")
            );
            assert!(
                journal
                    .native_export_files
                    .iter()
                    .all(|r| r.phase == "collision")
            );
            continue;
        }
        assert_eq!(
            calls.iter().filter(|m| m.as_str() == "ExportOwned").count(),
            1
        );
        if outcome == Outcome::Pair {
            let files = result.unwrap();
            assert_eq!(files.len(), 2);
            for file in files {
                assert_eq!(
                    std::fs::read(root.path().join("output").join(file.path)).unwrap(),
                    BYTES
                );
            }
        } else {
            assert!(result.is_err());
        }
        if outcome == Outcome::PairUncertain {
            assert!(
                journal
                    .native_export_files
                    .iter()
                    .all(|r| r.phase == "uncertain")
            );
            assert!(!calls.iter().any(|m| m == "FileDelete"));
            continue;
        }
        assert!(server.state.lock().unwrap().companions.is_empty());
        if outcome == Outcome::PairDeleteFailed {
            assert_eq!(journal.native_export_files[0].phase, "deleting");
            assert_eq!(journal.native_export_files[1].phase, "removed");
            server.state.lock().unwrap().outcome = Outcome::Pair;
            for mut record in journal.native_export_files {
                cleanup(&host, &run, &connection, &mut record).await.unwrap();
            }
        }
        assert!(server.state.lock().unwrap().file.is_none());
    }
}

#[tokio::test]
async fn invalid_companion_extensions_do_not_dispatch_or_create_intents() {
    let server = Server::new(Outcome::Success);
    let (_root, host, connection, run, identity) = case(&server).await;
    let before = server.state.lock().unwrap().calls.len();
    for companions in [vec!["ZIP".into()], vec!["../other".into()], vec!["".into()]] {
        assert!(
            perform_exports(
            &host, "fixture",
                &run,
                &connection,
                identity.clone(),
                "Ice.BO.FixtureSvc",
                "ExportOwned",
                json!({}),
                "fileName",
                4,
                "zip",
                &companions,
                &CancellationToken::new()
            )
            .await
            .is_err()
        );
    }
    assert_eq!(server.state.lock().unwrap().calls.len(), before);
    assert!(
        collection::snapshot(&run)
            .await
            .unwrap()
            .native_export_files
            .is_empty()
    );
}

#[test]
fn old_single_file_action_defaults_to_no_companions() {
    let action:HostAction=serde_json::from_value(json!({"type":"nativeFileExport","service":"Ice.BO.FixtureSvc","method":"ExportOwned","arguments":{},"filename_argument":"fileName","folder":4,"extension":"xml"})).unwrap();
    let HostAction::NativeFileExport {
        companion_extensions,
        ..
    } = action
    else {
        panic!()
    };
    assert!(companion_extensions.is_empty());
}

#[tokio::test]
async fn lost_export_response_stays_uncertain_after_disk_restart() {
    let server = Server::new(Outcome::ExportLost);
    let (root, host, connection, run, identity) = case(&server).await;
    assert!(
        perform_export(
            &host, "fixture",
            &run,
            &connection,
            identity.clone(),
            "Ice.BO.FixtureSvc",
            "ExportOwned",
            json!({}),
            "fileName",
            4,
            "xml",
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    let id = collection::snapshot(&run).await.unwrap().run_id;
    drop(run);
    let journal = crate::checkpoint_log::read_journal(&root.path().join("output"), &id)
        .unwrap()
        .unwrap();
    assert_eq!(journal.native_export_files[0].phase, "uncertain");
    let mut record = journal.native_export_files[0].clone();
    let resumed = Arc::new(Run {
        storage: Arc::new(StdMutex::new(
            Storage::open(&root.path().join("output"), false, journal).unwrap(),
        )),
        cancel: CancellationToken::new(),
        paused: AtomicBool::new(false),
        wake: Notify::new(),
        active: AtomicBool::new(false),
        network: true,
        helper_restore_attempts: Mutex::new(Default::default()),
        activity: Arc::new(Default::default()),
        shared_reads: Default::default(),
        helper_instances: Mutex::new(BTreeMap::new()),
            collection_context: Mutex::new(None),
    });
    // A lost response leaves the server's completion unknown even after restart.
    assert!(server.state.lock().unwrap().file.is_some());
    assert!(cleanup(&host, &resumed, &connection, &mut record).await.is_err());
    // An absent file is not evidence that the original request has stopped.
    server.state.lock().unwrap().file = None;
    assert!(cleanup(&host, &resumed, &connection, &mut record).await.is_err());
    assert!(
        perform_export(
            &host, "fixture",
            &resumed,
            &connection,
            identity,
            "Ice.BO.FixtureSvc",
            "ExportOwned",
            json!({}),
            "fileName",
            4,
            "xml",
            &CancellationToken::new()
        )
        .await
        .is_err()
    );
    let state = server.state.lock().unwrap();
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|m| m.as_str() == "ExportOwned")
            .count(),
        1
    );
    assert!(!state.calls.iter().any(|m| m == "FileDelete"));
    assert_eq!(record.phase, "uncertain");
}

#[tokio::test]
async fn missing_revoked_changed_binding_and_outside_target_authority_prevent_native_export() {
    for scenario in ["missing","revoked","binding","target"] {
        let server=Server::new(Outcome::Success);let(_root,host,connection,run,mut identity)=case(&server).await;
        let authority=collection::snapshot(&run).await.unwrap().server_change_authority.unwrap();
        match scenario {
            "missing"=>collection::update(&run,|storage|{storage.journal.server_change_authority=None;storage.checkpoint()}).await.unwrap(),
            "revoked"=>{crate::server_changes::revoke(&host.state_root,&authority.grant_id).unwrap();},
            "target"=>{identity.keys.insert("ID".into(),"outside-selected-identity".into());},
            _=>{},
        }
        let binding=if scenario=="binding"{"changed-source"}else{"fixture"};
        let result=perform_export(&host,binding,&run,&connection,identity,"Ice.BO.FixtureSvc","ExportOwned",json!({}),"fileName",4,"xml",&CancellationToken::new()).await;
        assert_eq!(result.unwrap_err().code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED", "{scenario}");
        let calls=server.state.lock().unwrap().calls.clone();assert!(!calls.iter().any(|method|matches!(method.as_str(),"ExportOwned"|"FileDelete"|"FileExists")));assert!(collection::snapshot(&run).await.unwrap().native_export_files.is_empty());
    }
}
#[tokio::test]
async fn revocation_preserves_original_verified_cleanup_without_replaying_export() {
    let server=Server::new(Outcome::DeleteFailed);let(_root,host,connection,run,identity)=case(&server).await;
    assert!(perform_export(&host,"fixture",&run,&connection,identity,"Ice.BO.FixtureSvc","ExportOwned",json!({}),"fileName",4,"xml",&CancellationToken::new()).await.is_err());
    let journal=collection::snapshot(&run).await.unwrap();let mut record=journal.native_export_files[0].clone();assert_eq!(record.phase,"deleting");let original=journal.server_change_authority.unwrap();assert_eq!(record.authorization.as_ref().unwrap().grant_id,original.grant_id);
    crate::server_changes::revoke(&host.state_root,&original.grant_id).unwrap();
    collection::update(&run,move|storage|{storage.journal.server_change_authority_history.push(original);storage.journal.server_change_authority=None;storage.checkpoint()}).await.unwrap();
    server.state.lock().unwrap().outcome=Outcome::Success;
    cleanup(&host,&run,&connection,&mut record).await.unwrap();assert_eq!(record.phase,"removed");assert!(server.state.lock().unwrap().file.is_none());assert_eq!(server.state.lock().unwrap().calls.iter().filter(|method|method.as_str()=="ExportOwned").count(),1);
}

#[tokio::test]
async fn revocation_after_presence_read_stops_the_write_without_inventing_unknown_completion() {
    let server=Server::new(Outcome::Success);let(_root,host,connection,run,identity)=case(&server).await;
    let grant_id=collection::snapshot(&run).await.unwrap().server_change_authority.unwrap().grant_id;
    server.state.lock().unwrap().revoke_on_exists=Some((host.state_root.clone(),grant_id));
    let result=perform_export(&host,"fixture",&run,&connection,identity,"Ice.BO.FixtureSvc","ExportOwned",json!({}),"fileName",4,"xml",&CancellationToken::new()).await;
    assert_eq!(result.unwrap_err().code,"SERVER_CHANGE_AUTHORIZATION_REQUIRED");
    let calls=server.state.lock().unwrap().calls.clone();assert!(calls.iter().any(|method|method=="FileExists"));assert!(!calls.iter().any(|method|matches!(method.as_str(),"ExportOwned"|"FileDelete")));
    let mut record=collection::snapshot(&run).await.unwrap().native_export_files[0].clone();assert_eq!(record.phase,"reserved");cleanup(&host,&run,&connection,&mut record).await.unwrap();assert_eq!(record.phase,"removed");assert!(server.state.lock().unwrap().file.is_none());
}
