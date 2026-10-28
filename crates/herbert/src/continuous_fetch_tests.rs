//! Exercise the actual host pipeline with compiled guests and blocked disk workers.
use super::*;
use crate::input::{AuthMode, Credentials};
use std::sync::atomic::AtomicBool;

async fn guest(root: &Path, prepared: bool, fail_last: bool) -> Plugin {
    guest_mode(root, prepared, fail_last, false).await
}
async fn guest_mode(root: &Path, prepared: bool, fail_last: bool, independent: bool) -> Plugin {
    let descriptor = GuestReply::Descriptor {
        abi: 1,
        binary_exports: vec![],
        response_policies: vec![],
        entities: vec![],
        methods: vec![],
        category: Category {
            id: "C01".into(),
            name: "Fixture".into(),
            selected: true,
            description: "fixture".into(),
            plugin_version: Some("test".into()),
            available: true,
            qualification: "fixture".into(),
            roles: vec![
                "collector".into(),
                "pagedDefinitions".into(),
                "prefetchPages".into(),
            ],
            helpers: vec![],
            artifacts: vec![],
            settings: vec![],
            help: vec![],
        },
    };
    let mut data = String::new();
    let mut offset = 4096u32;
    let mut literal = |bytes: &[u8]| {
        let start = offset;
        let escaped = bytes
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        data += &format!("(data (i32.const {start}) \"{escaped}\")\n");
        offset += bytes.len() as u32 + 1;
        (start, bytes.len() as u32)
    };
    let descriptor_marker = literal(b"descriptor");
    let materialize_marker = literal(b"materialize");
    let ownership_marker = literal(b"PAYLOAD_OWNERSHIP_DENIED");
    let second_marker = literal(br#""cursor":"2""#);
    let third_marker = literal(br#""cursor":"3""#);
    let packed = |(p, n): (u32, u32)| ((p as u64) << 32) | n as u64;
    let desc = packed(literal(&serde_json::to_vec(&descriptor).unwrap()));
    let denied = packed(literal(&serde_json::to_vec(&GuestReply::Failure {
        diagnostic: Diagnostic::new("PAYLOAD_OWNERSHIP_DENIED", "Unissued fixture file."),
    }).unwrap()));
    let mut fetches = Vec::new();
    let mut writes = Vec::new();
    for index in 1..=3 {
        let identity = Identity {
            category: "C01".into(),
            kind: "Fixture".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([("ID".into(), index.to_string())]),
        };
        let next = (index < 3).then(|| (index + 1).to_string());
        let reply = GuestReply::InventoryData {
            records: vec![(identity.clone(), json!({"field":index}))],
            inventory: Box::new(GuestReply::Inventory {
                identities: vec![identity],
                next_cursor: if independent { None } else { next.clone() },
                authoritative: true,
                diagnostics: vec![],
            }),
        };
        writes.push(packed(literal(&serde_json::to_vec(&reply).unwrap())));
        let fetch = if fail_last && index == 3 {
            GuestReply::Failure {
                diagnostic: Diagnostic::new("TEST_READ_FAILED", "The fixture read failed."),
            }
        } else if prepared {
            GuestReply::PreparedPage {
                state: json!({"fixture":index}),
                next_cursor: next,
                next_state: Value::Null,
            }
        } else {
            reply
        };
        fetches.push(packed(literal(&serde_json::to_vec(&fetch).unwrap())));
    }
    let has = |(p, n)| format!("(call $has (local.get $len) (i32.const {p}) (i32.const {n}))");
    let choose = |replies: &[u64]| {
        format!(
            "(if (result i64) {} (then (i64.const {})) (else (if (result i64) {} (then (i64.const {})) (else (i64.const {})))))",
            has(third_marker),
            replies[2],
            has(second_marker),
            replies[1],
            replies[0]
        )
    };
    let wat = format!(
        r#"(module
        (memory (export "memory") 2) {data}
        (func (export "hoover_abi") (result i32) i32.const 1)
        (func (export "hoover_alloc") (param i32) (result i32) i32.const 0)
        (func (export "hoover_free") (param i32 i32))
        (func $has (param $len i32) (param $needle i32) (param $n i32) (result i32)
            (local $i i32) (local $j i32)
            (block $absent (loop $outer
                (br_if $absent (i32.gt_u (i32.add (local.get $i) (local.get $n)) (local.get $len)))
                (local.set $j (i32.const 0))
                (block $different (loop $inner
                    (if (i32.eq (local.get $j) (local.get $n)) (then (return (i32.const 1))))
                    (br_if $different (i32.ne
                        (i32.load8_u (i32.add (local.get $i) (local.get $j)))
                        (i32.load8_u (i32.add (local.get $needle) (local.get $j)))))
                    (local.set $j (i32.add (local.get $j) (i32.const 1))) (br $inner)))
                (local.set $i (i32.add (local.get $i) (i32.const 1))) (br $outer)))
            i32.const 0)
        (func (export "hoover_call") (param i32) (param $len i32) (result i64)
            (if (result i64) {is_descriptor} (then (i64.const {desc}))
                (else (if (result i64) {is_denied} (then (i64.const {denied}))
                    (else (if (result i64) {is_materialize} (then {write}) (else {fetch}))))))))"#,
        is_descriptor = has(descriptor_marker),
        is_materialize = has(materialize_marker),
        is_denied = has(ownership_marker),
        write = choose(&writes),
        fetch = choose(&fetches)
    );
    let path = root.join("fixture.wasm");
    std::fs::write(&path, wat::parse_str(wat).unwrap()).unwrap();
    Plugin::load(&path).await.unwrap()
}

#[tokio::test]
async fn prepared_page_files_cross_the_host_handoff_without_trusting_guest_claims() {
    for transfer in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let plugin = guest(root.path(), true, false).await;
        let host = Host::with_roots(root.path().join("state"), root.path().join("plugins"),
            Some(root.path().join("coordination")));
        let (connection, _server) = connection().await;
        let mut journal = storage::new_journal("prepared-native-source".into());
        journal.company = "C".into();
        journal.endpoint = connection.endpoint.to_string();
        journal.caller = "fixture".into();
        journal.temporary_access_enabled = false;
        let run = run(Storage::open(&root.path().join("output"), false, journal).unwrap());
        let file = run.storage.lock().unwrap().preserve_raw(b"<Report><Code>original</Code></Report>", "xml").unwrap();
        let request = GuestRequest { abi:1, operation:"materialize".into(), category:"C01".into(),
            company:"C".into(), identity:None, cursor:None, page_size:Some(100),
            state:json!({"claimedFile":file}), response:None, error:None };
        let reply = GuestReply::Action { action:HostAction::ParseXml { file:file.clone() }, state:request.state.clone() };
        let issued = if transfer { vec![file.clone()] } else { vec![] };
        let (reply, files) = host.drive_with_sources(&plugin, request, &connection, &run,
            &run.cancel, Some(reply), issued).await.unwrap();
        if transfer {
            assert!(matches!(reply, GuestReply::Inventory { .. }));
            assert!(files.iter().any(|issued| issued == &file));
        } else {
            let GuestReply::Failure { diagnostic } = reply else { panic!("Guest claimed unissued bytes"); };
            assert_eq!(diagnostic.code, "PAYLOAD_OWNERSHIP_DENIED");
        }
    }
}

fn run(store: Storage) -> Arc<Run> {
    Arc::new(Run {
        storage: Arc::new(std::sync::Mutex::new(store)),
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
struct AdmissionServer {
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for AdmissionServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}
async fn connection() -> (Arc<Connection>, AdmissionServer) { connection_mode(false).await }
async fn connection_mode(allow_helper_catalog:bool) -> (Arc<Connection>, AdmissionServer) { connection_company(allow_helper_catalog,"C".into()).await }
async fn connection_company(allow_helper_catalog:bool, company:String) -> (Arc<Connection>, AdmissionServer) {
    use std::io::{BufRead, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let thread = std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            if stopped.load(Ordering::SeqCst) {
                break;
            }
            let mut reader = std::io::BufReader::new(&mut stream);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut size = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        size = value.trim().parse().unwrap();
                    }
                }
            }
            let mut body = vec![0; size];
            reader.read_exact(&mut body).unwrap();
            drop(reader);
            let reply = if first.contains("GetUserFile") {
                json!({"returnObj":{"UserFile":[{"DcdUserID":"fixture","SecurityMgr":true,"UserDisabled":false,"LockedOut":false}]}})
            } else if first.contains("GetDatabaseVersion") {
                json!({"returnObj":"5.2.100"})
            } else if allow_helper_catalog && first.contains("BAQDesignerSvc/GetFieldList") {
                let arguments:Value=serde_json::from_slice(&body).unwrap();
                assert_eq!(arguments["schema"],"Ecf");assert_eq!(arguments["dataTableID"],"EfxLibrary");
                let fields=["LibraryID","GlobalID","Notes","OwnedByCompany"].map(|field|json!({"DBSchemaName":"Ecf","DBTableName":"EfxLibrary","DBFieldName":field,"DataType":"nvarchar","FieldFormat":""}));
                json!({"returnObj":{"TableFieldList":fields}})
            } else if allow_helper_catalog && first.contains("DynamicQuerySvc/Execute") {
                let arguments:Value=serde_json::from_slice(&body).unwrap();
                let tables=arguments["queryDS"]["QueryTable"].as_array().unwrap();
                assert!(tables.iter().any(|t|t["DBSchemaName"]=="Ecf"&&t["DBTableName"]=="EfxLibrary"));
                assert!(tables.iter().all(|t|(t["DBSchemaName"]=="Ecf"&&t["DBTableName"]=="EfxLibrary")||t["TableType"]=="TT"));
                json!({"returnObj":{"Results":[],"Errors":[]}})
            } else {
                panic!("The offline pipeline attempted an unexpected server operation: {first}");
            };
            let bytes = serde_json::to_vec(&reply).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).unwrap();
            stream.write_all(&bytes).unwrap();
        }
    });
    let server = AdmissionServer {
        address,
        stop,
        thread: Some(thread),
    };
    let connection = Connection::new(
        url::Url::parse(&format!("http://{address}/fixture/")).unwrap(),
        company,
        AuthMode::BasicApiKey,
        Credentials {
            username: "fixture".into(),
            password: "fixture".into(),
            api_key: Some("fixture".into()),
        },
        OperatingSettings::default(),
        Budget::new(4).unwrap(),
    )
    .unwrap();
    connection.connect(&CancellationToken::new()).await.unwrap();
    (connection, server)
}

#[tokio::test]
async fn producer_fetches_all_pages_while_first_page_waits_for_writers() {
    for prepared in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let plugin = guest(root.path(), prepared, false).await;
        let host = Host::with_roots(
            root.path().join("state"),
            root.path().join("plugins"),
            Some(root.path().join("coordination")),
        );
        let (connection, _server) = connection().await;
        let mut journal = storage::new_journal("pipeline".into());
        journal.company = "C".into();
        journal.temporary_access_enabled = false;
        journal.endpoint = connection.endpoint.to_string();
        journal.caller = "fixture".into();
        let run = run(Storage::open(&root.path().join("output"), false, journal).unwrap());
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = Arc::new(std::sync::Mutex::new(wait));
        host.file_writers.submit_page(
            (0..4)
                .map(|_| {
                    let wait = wait.clone();
                    Box::new(move || {
                        let _ = wait.lock().unwrap().recv();
                    }) as crate::file_writers::Job
                })
                .collect(),
        );
        let collect = host.discover_targets(&run, &connection, &plugin, &[], Some(1));
        let check = async {
            let reached = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    if run
                        .activity
                        .statistics()
                        .pointer("/pageBuffers/C01/memoryPages")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                        >= 2
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
            .await
            .is_ok();
            let before = run.storage.lock().unwrap().journal.artifacts.len();
            for _ in 0..4 {
                release.send(()).unwrap();
            }
            (reached, before)
        };
        let (result, (reached, before)) = tokio::join!(collect, check);
        assert!(
            reached,
            "The producer stopped before queueing later pages: {:?}; result: {:?}",
            run.storage.lock().unwrap().journal.diagnostics,
            result
        );
        assert_eq!(before, 0);
        assert_eq!(result.unwrap().len(), 3);
        assert_eq!(run.storage.lock().unwrap().journal.artifacts.len(), 3);
        assert!(
            run.activity.statistics()["pageBuffers"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn ready_records_are_durable_without_advancing_the_page_cursor() {
    let root = tempfile::tempdir().unwrap();
    let plugin = guest(root.path(), true, false).await;
    let host = Host::with_roots(root.path().join("state"), root.path().join("plugins"), Some(root.path().join("coordination")));
    let (connection, _server) = connection().await;
    let mut journal = storage::new_journal("ready-records".into());
    journal.company = "C".into();
    journal.endpoint = connection.endpoint.to_string();
    journal.caller = "fixture".into();
    journal.cursors.insert("C01".into(), "1".into());
    journal.temporary_access_enabled = false;
    let output = root.path().join("output");
    let run = run(Storage::open(&output, false, journal).unwrap());
    let identity = Identity { category:"C01".into(), kind:"Fixture".into(), scope:Scope::Global,
        keys:BTreeMap::from([("ID".into(),"ready".into())]) };
    let request = GuestRequest { abi:1,operation:"materialize".into(),category:"C01".into(),company:"C".into(),
        identity:None,cursor:None,page_size:Some(100),state:Value::Null,response:None,error:None };
    let reply = GuestReply::ReadyRecords { records:vec![(identity.clone(),json!({"ready":true}))],state:json!({}) };
    // Occupy all disk workers. The plugin must still reach its final record,
    // but the host must not return the inventory or advance its saved cursor.
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let wait = Arc::new(std::sync::Mutex::new(wait));
    host.file_writers.submit_page((0..crate::file_writers::WRITERS).map(|_| {
        let wait = wait.clone();
        Box::new(move || { let _ = wait.lock().unwrap().recv(); }) as crate::file_writers::Job
    }).collect());
    let drive = host.drive_with_reply(&plugin,request,&connection,&run,&run.cancel,Some(reply));
    let check = async {
        let queued = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if run.activity.batches()[0]["files"].as_u64() == Some(2) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }).await.is_ok();
        let saved = run.storage.lock().unwrap().journal.clone();
        for _ in 0..crate::file_writers::WRITERS { release.send(()).unwrap(); }
        (queued, saved)
    };
    let (result, (queued, before)) = tokio::join!(drive, check);
    assert!(queued, "Materialization waited for the blocked writers");
    assert!(before.artifacts.is_empty());
    assert_eq!(before.cursors["C01"], "1");
    let (final_reply, _) = result.unwrap();
    let batches = run.activity.batches();
    assert_eq!(batches.as_array().unwrap().len(), 1);
    assert_eq!(batches[0]["files"], 2);
    assert_eq!(batches[0]["processed"], 2);
    assert_eq!(batches[0]["phase"], "Processed");
    let GuestReply::Inventory { identities, .. } = final_reply else { panic!("Expected final inventory"); };
    assert_eq!(identities.len(),2);
    assert_eq!(identities[0],identity);
    let storage = run.storage.lock().unwrap();
    assert_eq!(storage.journal.artifacts.len(),2);
    assert_eq!(storage.journal.cursors["C01"],"1");
    for artifact in &storage.journal.artifacts {
        for file in &artifact.files { storage.verify(file).unwrap(); }
    }
    // Release the destination before recovery, as a stopped host does.
    let saved = storage.journal.clone();
    drop(storage);
    drop(run);
    let recovered = Storage::open(&output,false,saved).unwrap();
    assert_eq!(recovered.journal.artifacts.len(),2);
    assert_eq!(recovered.journal.cursors["C01"],"1");
}

#[tokio::test]
async fn ready_output_failure_does_not_return_a_successful_page() {
    let root = tempfile::tempdir().unwrap();
    let plugin = guest(root.path(), true, false).await;
    let host = Host::with_roots(root.path().join("state"), root.path().join("plugins"),
        Some(root.path().join("coordination")));
    let (connection, _server) = connection().await;
    let mut journal = storage::new_journal("output-failure".into());
    journal.company = "C".into();
    journal.endpoint = connection.endpoint.to_string();
    journal.caller = "fixture".into();
    journal.cursors.insert("C01".into(), "1".into());
    journal.temporary_access_enabled = false;
    let run = run(Storage::open(&root.path().join("output"), false, journal).unwrap());
    let identity = Identity { category:"C01".into(), kind:"Fixture".into(), scope:Scope::Global,
        keys:BTreeMap::from([("ID".into(),"bad-output".into())]) };
    let request = GuestRequest { abi:1, operation:"materialize".into(), category:"C01".into(),
        company:"C".into(), identity:None, cursor:None, page_size:Some(100),
        state:Value::Null, response:None, error:None };
    let reply = GuestReply::ReadyRecords { records:vec![(identity,json!({
        "herbertDirectOutput":true,"herbertSources":{"nativeFiles":[
            {"name":"missing.rdl","file":{"path":"missing.rdl","bytes":1,
                "sha256":"00".repeat(32),"role":"nativeExport"}}]}}))],state:json!({}) };
    let result = host.drive_with_reply(&plugin,request,&connection,&run,&run.cancel,Some(reply)).await;
    assert!(result.is_err());
    assert_eq!(run.storage.lock().unwrap().journal.cursors["C01"], "1");
    assert!(run.storage.lock().unwrap().journal.artifacts.iter().any(|artifact|
        artifact.identity.keys.get("ID").map(String::as_str) == Some("bad-output")
            && artifact.acquisition == Completeness::Partial));
    assert_eq!(run.activity.batches()[0]["phase"], "Interrupted");
}

#[tokio::test]
async fn independent_materializations_queue_while_writers_wait_and_commit_only_after_join() {
    let root = tempfile::tempdir().unwrap();
    let plugin = guest_mode(root.path(), true, false, true).await;
    let host = Host::with_roots(root.path().join("state"), root.path().join("plugins"), Some(root.path().join("coordination")));
    let (connection, _server) = connection().await;
    let mut journal = storage::new_journal("independent-materialization".into());
    journal.company = "C".into(); journal.endpoint = connection.endpoint.to_string();
    journal.caller = "fixture".into(); journal.temporary_access_enabled = false;
    journal.cursors.insert("C01".into(), "uncommitted".into());
    let run = run(Storage::open(&root.path().join("output"), false, journal).unwrap());
    let request = GuestRequest { abi:1, operation:"materialize".into(), category:"C01".into(),
        company:"C".into(), identity:None, cursor:None, page_size:Some(100),
        state:json!({"_hostWorkItems":[{"cursor":"1"},{"cursor":"2"},{"cursor":"3"}],
            "_hostWorkNextCursor":"next-page","_progress":{"pageRecords":3}}), response:None, error:None };
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let wait = Arc::new(std::sync::Mutex::new(wait));
    host.file_writers.submit_page((0..crate::file_writers::WRITERS).map(|_| {
        let wait=wait.clone();Box::new(move||{let _=wait.lock().unwrap().recv();})as crate::file_writers::Job
    }).collect());
    let drive=host.drive_with_reply(&plugin,request,&connection,&run,&run.cancel,None);
    let check=async {
        let queued=tokio::time::timeout(std::time::Duration::from_secs(10),async{
            loop {
                if run.activity.statistics().pointer("/pageBuffers/C01/memoryPages").and_then(Value::as_u64).unwrap_or(0)>=2{break;}
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }).await.is_ok();
        let before=run.storage.lock().unwrap().journal.clone();
        for _ in 0..crate::file_writers::WRITERS{release.send(()).unwrap();}
        (queued,before)
    };
    let(result,(queued,before))=tokio::join!(drive,check);
    assert!(queued,"Later independent items waited for the first write");
    assert!(before.artifacts.is_empty());assert_eq!(before.cursors["C01"],"uncommitted");
    let(GuestReply::Inventory{identities,next_cursor,authoritative,diagnostics},_)=result.unwrap()else{panic!("Missing final page inventory")};
    assert_eq!(identities.len(),3);assert_eq!(next_cursor.as_deref(),Some("next-page"));assert!(authoritative&&diagnostics.is_empty());
    let saved=run.storage.lock().unwrap();assert_eq!(saved.journal.artifacts.len(),3);
    for artifact in &saved.journal.artifacts{for file in &artifact.files{saved.verify(file).unwrap();}}
}

#[tokio::test]
async fn independent_materialization_failure_preserves_earlier_output_and_cursor() {
    let root=tempfile::tempdir().unwrap();let plugin=guest_mode(root.path(),true,false,true).await;
    let host=Host::with_roots(root.path().join("state"),root.path().join("plugins"),Some(root.path().join("coordination")));
    let(connection,_server)=connection().await;let mut journal=storage::new_journal("independent-failure".into());
    journal.company="C".into();journal.endpoint=connection.endpoint.to_string();journal.caller="fixture".into();journal.temporary_access_enabled=false;journal.cursors.insert("C01".into(),"uncommitted".into());
    let run=run(Storage::open(&root.path().join("output"),false,journal).unwrap());
    let request=GuestRequest{abi:1,operation:"materialize".into(),category:"C01".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state:json!({"_hostWorkItems":[{"cursor":"1"},{"failure":"PAYLOAD_OWNERSHIP_DENIED"}],"_hostWorkNextCursor":null,"_progress":{"pageRecords":2}}),response:None,error:None};
    let result=host.drive_with_reply(&plugin,request,&connection,&run,&run.cancel,None).await;
    assert!(result.is_err());let saved=run.storage.lock().unwrap();assert_eq!(saved.journal.artifacts.len(),1);assert_eq!(saved.journal.cursors["C01"],"uncommitted");
    for file in &saved.journal.artifacts[0].files{saved.verify(file).unwrap();}
}

#[tokio::test]
async fn late_fetch_failure_keeps_written_pages_and_the_next_uncommitted_cursor() {
    let root = tempfile::tempdir().unwrap();
    let plugin = guest(root.path(), true, true).await;
    let host = Host::with_roots(
        root.path().join("state"),
        root.path().join("plugins"),
        Some(root.path().join("coordination")),
    );
    let (connection, _server) = connection().await;
    let mut journal = storage::new_journal("pipeline-failure".into());
    journal.company = "C".into();
    journal.temporary_access_enabled = false;
    journal.endpoint = connection.endpoint.to_string();
    journal.caller = "fixture".into();
    let run = run(Storage::open(&root.path().join("output"), false, journal).unwrap());
    host.collect_plugin(&run, &connection, &plugin, &[], &[], true, Some(1))
        .await
        .unwrap();
    let saved = snapshot(&run).await.unwrap();
    assert_eq!(saved.artifacts.len(), 2, "{:?}", saved.diagnostics);
    assert_eq!(saved.cursors["C01"], "3");
    assert_eq!(saved.discovery_finished["C01"], false);
    assert_eq!(run.activity.plugin_states()["C01"], "failed");
    assert!(
        saved
            .diagnostics
            .iter()
            .any(|d| d.code == "TEST_READ_FAILED")
    );
}

#[tokio::test]
async fn recovery_rereads_a_completed_category_when_an_output_is_missing() {
    for prepared in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let plugin = guest(root.path(), prepared, false).await;
        let host = Host::with_roots(root.path().join("state"), root.path().join("plugins"),
            Some(root.path().join("coordination")));
        let (connection, _server) = connection().await;
        let mut journal = storage::new_journal("pipeline-output-recovery".into());
        journal.company = "C".into();
        journal.endpoint = connection.endpoint.to_string();
        journal.caller = "fixture".into();
        let output = root.path().join("output");
        let first = run(Storage::open(&output, false, journal).unwrap());
        host.discover_targets(&first, &connection, &plugin, &[], Some(1)).await.unwrap();
        let saved = snapshot(&first).await.unwrap();
        assert!(saved.discovery_finished["C01"]);
        let missing = saved.artifacts[0].files[0].clone();
        let path = first.storage.lock().unwrap().confined(&missing.path).unwrap();
        drop(first);
        std::fs::remove_file(path).unwrap();
        let mut storage = Storage::open(&output, false, saved).unwrap();
        storage.reconcile_downloads().unwrap();
        let recovered = run(storage);
        host.discover_targets(&recovered, &connection, &plugin, &[], Some(1)).await.unwrap();
        let storage = recovered.storage.lock().unwrap();
        assert_eq!(storage.journal.artifacts.len(), 3);
        assert!(storage.journal.discovery_finished["C01"]);
        assert!(storage.journal.artifacts.iter().all(|a| a.diagnostics.is_empty()));
        storage.verify(&missing).unwrap();
    }
}

#[tokio::test]
#[ignore="Explicit C19 lab-only host collection; isolated output and direct mode"]
async fn c19_native_host_collection() {
    let lane=std::env::var("HERBERT_C19_LANE").unwrap();
    let (endpoint,profile)=match lane.as_str(){"2026.1"=>("https://kinetic-2026-1.lab.test:9448/","lab-2026-1"),"2025.2"=>("https://kinetic-2025-2.lab.test:9449/","lab-2025-2"),_=>panic!("Only owned lab targets are permitted")};
    let base=PathBuf::from(std::env::var("HERBERT_C19_REPLAY").unwrap()).join(&lane);
    let output=base.join(std::env::var("HERBERT_C19_RUN_NAME").unwrap());
    assert!(!output.exists(),"Preserve earlier run and recovery evidence");
    let credentials=crate::profiles::secure_read(endpoint,profile).unwrap();
    let mode=if credentials.api_key.as_ref().is_some_and(|s|!s.is_empty()){AuthMode::BasicApiKey}else{AuthMode::Session};
    let connection=Connection::new(crate::input::endpoint(endpoint).unwrap(),"TESTCO".into(),mode,credentials,OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
    let cancel=CancellationToken::new();let admitted=connection.connect(&cancel).await.unwrap();
    let plugin=Plugin::load(&PathBuf::from(std::env::var("HERBERT_C19_WASM").unwrap())).await.unwrap();
    assert_eq!(plugin.category.id,"C19");
    let host=Host::with_roots(output.join("state"),output.join("plugins"),Some(output.join("coordination")));
    let mut journal=storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.company="TESTCO".into();journal.endpoint=endpoint.into();journal.caller=admitted["caller"].as_str().unwrap().into();journal.temporary_access_enabled=false;journal.collection_method=herbert_contract::CollectionMethod::DirectOnly;
    let run=run(Storage::open(&output.join("collection"),false,journal).unwrap());
    let result=host.discover_targets(&run,&connection,&plugin,&[],Some(100)).await;
    connection.logout(&cancel).await.unwrap();
    let storage=run.storage.lock().unwrap();storage.checkpoint().unwrap();
    let receipt=json!({"lane":lane,"result":result.as_ref().map(|ids|ids.len()).map_err(|e|e.clone()),"statistics":run.activity.statistics(),"artifacts":storage.journal.artifacts,"diagnostics":storage.journal.diagnostics});
    std::fs::write(output.join("result.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    println!("{}",json!({"lane":lane,"targets":result.as_ref().map(|ids|ids.len()).unwrap_or(0),"artifacts":storage.journal.artifacts.len(),"diagnostics":storage.journal.diagnostics}));
    assert!(result.is_ok(),"{result:?}");
    let mut expected:Vec<String>=std::env::var("HERBERT_C19_EXPECTED_FIXTURES").map(|v|serde_json::from_str(&v).unwrap()).unwrap_or_else(|_|vec!["H19C1024A".into(),"H19K1024A".into(),"H19L1024A".into(),"H19N1024A".into()]);expected.sort();
    assert_eq!(storage.journal.artifacts.len(),expected.len());
    let mut fixture_ids:Vec<_>=storage.journal.artifacts.iter().map(|a|a.identity.keys.get("ConfigID").or_else(||a.identity.keys.get("LookupTblID")).or_else(||a.identity.keys.get("SysRowID")).expect("C19 fixture identity must have a native root or persisted row key").as_str()).collect();fixture_ids.sort();assert_eq!(fixture_ids,expected);
    for artifact in &storage.journal.artifacts {assert_eq!(artifact.acquisition,Completeness::Collected,"{artifact:?}");for file in &artifact.files {storage.verify(file).unwrap();}}
}

#[tokio::test]
#[ignore="Explicit C20 lab-only host collection; isolated output and direct mode"]
async fn c20_native_host_collection() {
    let lane=std::env::var("HERBERT_C20_LANE").unwrap();
    let (endpoint,profile)=match lane.as_str(){"2026.1"=>("https://kinetic-2026-1.lab.test:9448/","lab-2026-1"),"2025.2"=>("https://kinetic-2025-2.lab.test:9449/","lab-2025-2"),_=>panic!("Only owned lab targets are permitted")};
    let base=PathBuf::from(std::env::var("HERBERT_C20_REPLAY").unwrap()).join(&lane);
    let output=base.join(std::env::var("HERBERT_C20_RUN_NAME").unwrap());
    assert!(!output.exists(),"Preserve earlier run and recovery evidence");
    let credentials=crate::profiles::secure_read(endpoint,profile).unwrap();
    let mode=if credentials.api_key.as_ref().is_some_and(|s|!s.is_empty()){AuthMode::BasicApiKey}else{AuthMode::Session};
    let connection=Connection::new(crate::input::endpoint(endpoint).unwrap(),"TESTCO".into(),mode,credentials,OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
    let cancel=CancellationToken::new();let admitted=connection.connect(&cancel).await.unwrap();
    let plugin=Plugin::load(&PathBuf::from(std::env::var("HERBERT_C20_WASM").unwrap())).await.unwrap();
    assert_eq!(plugin.category.id,"C20");
    let host=Host::with_roots(output.join("state"),output.join("plugins"),Some(output.join("coordination")));
    let mut journal=storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.company="TESTCO".into();journal.endpoint=endpoint.into();journal.caller=admitted["caller"].as_str().unwrap().into();journal.temporary_access_enabled=false;journal.collection_method=if std::env::var("HERBERT_C20_MODE").as_deref()==Ok("assisted"){herbert_contract::CollectionMethod::ServerAssisted}else{herbert_contract::CollectionMethod::DirectOnly};
    journal.plugin_settings.insert("C20".into(),json!({"itemsPerPage":std::env::var("HERBERT_C20_SIZE").ok().and_then(|s|s.parse::<u32>().ok()).unwrap_or(100)}));
    let run=run(Storage::open(&output.join("collection"),false,journal).unwrap());
    let result=host.discover_targets(&run,&connection,&plugin,&[],Some(std::env::var("HERBERT_C20_SIZE").ok().and_then(|s|s.parse().ok()).unwrap_or(100))).await;
    connection.logout(&cancel).await.unwrap();
    let storage=run.storage.lock().unwrap();storage.checkpoint().unwrap();
    let receipt=json!({"lane":lane,"result":result.as_ref().map(|ids|ids.len()).map_err(|e|e.clone()),"statistics":run.activity.statistics(),"artifacts":storage.journal.artifacts,"diagnostics":storage.journal.diagnostics});
    std::fs::write(output.join("result.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    println!("{}",json!({"lane":lane,"targets":result.as_ref().map(|ids|ids.len()).unwrap_or(0),"artifacts":storage.journal.artifacts.len(),"diagnostics":storage.journal.diagnostics}));
    assert!(result.is_ok(),"{result:?}");
    assert_eq!(storage.journal.artifacts.len(),std::env::var("HERBERT_C20_EXPECTED").ok().and_then(|s|s.parse().ok()).unwrap_or(5));
    assert!(storage.journal.diagnostics.is_empty());
    for artifact in &storage.journal.artifacts {assert_eq!(artifact.acquisition,Completeness::Collected,"{artifact:?}");for file in &artifact.files {storage.verify(file).unwrap();}}
}

#[tokio::test]
#[ignore="Explicit C21 lab-only host collection; isolated output and direct mode"]
async fn c21_native_host_collection() {
    let lane=std::env::var("HERBERT_C21_LANE").unwrap();
    let (endpoint,profile)=match lane.as_str(){"2026.1"=>("https://kinetic-2026-1.lab.test:9448/","lab-2026-1"),"2025.2"=>("https://kinetic-2025-2.lab.test:9449/","lab-2025-2"),_=>panic!("Only owned lab targets are permitted")};
    let base=PathBuf::from(std::env::var("HERBERT_C21_PROOF_ROOT").unwrap()).join(&lane);
    let output=base.join(std::env::var("HERBERT_C21_RUN_NAME").unwrap_or_else(|_|"host-final".into()));
    assert!(!output.exists(),"Preserve earlier run and recovery evidence");
    let credentials=crate::profiles::secure_read(endpoint,profile).unwrap();
    let mode=if credentials.api_key.as_ref().is_some_and(|s|!s.is_empty()){AuthMode::BasicApiKey}else{AuthMode::Session};
    let connection=Connection::new(crate::input::endpoint(endpoint).unwrap(),"TESTCO".into(),mode,credentials,OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
    let cancel=CancellationToken::new();let admitted=connection.connect(&cancel).await.unwrap();
    let plugin=Plugin::load(&PathBuf::from(std::env::var("HERBERT_C21_WASM").unwrap_or_else(|_|"C:/Tools/Workbench/workspace/build/hoover/wasm32-unknown-unknown/release/c21.wasm".into()))).await.unwrap();
    assert_eq!(plugin.category.id,"C21");
    let host=Host::with_roots(output.join("state"),output.join("plugins"),Some(output.join("coordination")));
    let mut journal=storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.company="TESTCO".into();journal.endpoint=endpoint.into();journal.caller=admitted["caller"].as_str().unwrap().into();journal.temporary_access_enabled=false;journal.collection_method=if std::env::var("HERBERT_C21_MODE").as_deref()==Ok("assisted"){herbert_contract::CollectionMethod::ServerAssisted}else{herbert_contract::CollectionMethod::DirectOnly};
    let run=run(Storage::open(&output.join("collection"),false,journal).unwrap());
    let result=host.discover_targets(&run,&connection,&plugin,&[],Some(std::env::var("HERBERT_C21_SIZE").ok().and_then(|s|s.parse().ok()).unwrap_or(100))).await;
    connection.logout(&cancel).await.unwrap();
    let storage=run.storage.lock().unwrap();storage.checkpoint().unwrap();
    let receipt=json!({"lane":lane,"result":result.as_ref().map(|ids|ids.len()).map_err(|e|e.clone()),"statistics":run.activity.statistics(),"artifacts":storage.journal.artifacts,"diagnostics":storage.journal.diagnostics});
    std::fs::write(output.join("result.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    println!("{}",json!({"lane":lane,"targets":result.as_ref().map(|ids|ids.len()).unwrap_or(0),"artifacts":storage.journal.artifacts.len(),"diagnostics":storage.journal.diagnostics}));
    assert!(result.is_ok(),"{result:?}");
    if let Ok(expected)=std::env::var("HERBERT_C21_EXPECTED"){assert_eq!(storage.journal.artifacts.len(),expected.parse::<usize>().unwrap());}
    assert!(storage.journal.diagnostics.is_empty());
    for artifact in &storage.journal.artifacts {assert_eq!(artifact.acquisition,Completeness::Collected,"{artifact:?}");for file in &artifact.files {storage.verify(file).unwrap();}}
}

#[tokio::test]
#[ignore="Explicit C24 lab-only host collection; isolated output and direct mode"]
async fn c24_native_host_collection() {
    let lane=std::env::var("HERBERT_C24_LANE").unwrap();
    let (endpoint,profile)=match lane.as_str(){"2026.1"=>("https://kinetic-2026-1.lab.test:9448/","lab-2026-1"),"2025.2"=>("https://kinetic-2025-2.lab.test:9449/","lab-2025-2"),_=>panic!("Only owned lab targets are permitted")};
    let base=PathBuf::from(std::env::var("HERBERT_C24_PROOF_ROOT").unwrap()).join(&lane);
    let output=base.join(std::env::var("HERBERT_C24_RUN_NAME").unwrap_or_else(|_|"host-final".into()));
    assert!(!output.exists(),"Preserve earlier run and recovery evidence");
    let credentials=crate::profiles::secure_read(endpoint,profile).unwrap();
    let mode=if credentials.api_key.as_ref().is_some_and(|s|!s.is_empty()){AuthMode::BasicApiKey}else{AuthMode::Session};
    let connection=Connection::new(crate::input::endpoint(endpoint).unwrap(),"TESTCO".into(),mode,credentials,OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
    let cancel=CancellationToken::new();let admitted=connection.connect(&cancel).await.unwrap();
    let plugin=Plugin::load(&PathBuf::from(std::env::var("HERBERT_C24_WASM").unwrap_or_else(|_|"C:/Tools/Workbench/workspace/build/hoover/wasm32-unknown-unknown/release/c24.wasm".into()))).await.unwrap();
    assert_eq!(plugin.category.id,"C24");
    let host=Host::with_roots(output.join("state"),output.join("plugins"),Some(output.join("coordination")));
    let mut journal=storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.company="TESTCO".into();journal.endpoint=endpoint.into();journal.caller=admitted["caller"].as_str().unwrap().into();journal.temporary_access_enabled=false;journal.collection_method=if std::env::var("HERBERT_C24_MODE").as_deref()==Ok("assisted"){herbert_contract::CollectionMethod::ServerAssisted}else{herbert_contract::CollectionMethod::DirectOnly};
    let run=run(Storage::open(&output.join("collection"),false,journal).unwrap());
    let result=host.discover_targets(&run,&connection,&plugin,&[],Some(std::env::var("HERBERT_C24_SIZE").ok().and_then(|s|s.parse().ok()).unwrap_or(100))).await;
    connection.logout(&cancel).await.unwrap();
    let storage=run.storage.lock().unwrap();storage.checkpoint().unwrap();
    let receipt=json!({"lane":lane,"result":result.as_ref().map(|ids|ids.len()).map_err(|e|e.clone()),"statistics":run.activity.statistics(),"artifacts":storage.journal.artifacts,"diagnostics":storage.journal.diagnostics});
    std::fs::write(output.join("result.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    println!("{}",json!({"lane":lane,"targets":result.as_ref().map(|ids|ids.len()).unwrap_or(0),"artifacts":storage.journal.artifacts.len(),"diagnostics":storage.journal.diagnostics}));
    assert!(result.is_ok(),"{result:?}");
    if let Ok(expected)=std::env::var("HERBERT_C24_EXPECTED"){assert_eq!(storage.journal.artifacts.len(),expected.parse::<usize>().unwrap());}
    assert!(storage.journal.diagnostics.is_empty());
    for artifact in &storage.journal.artifacts {assert_eq!(artifact.acquisition,Completeness::Collected,"{artifact:?}");for file in &artifact.files {storage.verify(file).unwrap();}}
}



#[tokio::test]
#[ignore="Explicit C22 lab-only host collection; isolated output and direct mode"]
async fn c22_native_host_collection() {
    let lane=std::env::var("HERBERT_C22_LANE").unwrap();
    let (endpoint,profile)=match lane.as_str(){"2026.1"=>("https://kinetic-2026-1.lab.test:9448/","lab-2026-1"),"2025.2"=>("https://kinetic-2025-2.lab.test:9449/","lab-2025-2"),_=>panic!("Only owned lab targets are permitted")};
    let base=PathBuf::from(std::env::var("HERBERT_C22_PROOF_ROOT").unwrap()).join(&lane);
    let output=base.join(std::env::var("HERBERT_C22_RUN_NAME").unwrap_or_else(|_|"host-final".into()));
    assert!(!output.exists(),"Preserve earlier run and recovery evidence");
    let credentials=crate::profiles::secure_read(endpoint,profile).unwrap();
    let mode=if credentials.api_key.as_ref().is_some_and(|s|!s.is_empty()){AuthMode::BasicApiKey}else{AuthMode::Session};
    let connection=Connection::new(crate::input::endpoint(endpoint).unwrap(),"TESTCO".into(),mode,credentials,OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
    let cancel=CancellationToken::new();let admitted=connection.connect(&cancel).await.unwrap();connection.qualification_http.lock().unwrap().clear();
    let plugin=Plugin::load(&PathBuf::from(std::env::var("HERBERT_C22_WASM").unwrap_or_else(|_|"C:/Tools/Workbench/workspace/build/hoover/wasm32-unknown-unknown/release/c22.wasm".into()))).await.unwrap();
    assert_eq!(plugin.category.id,"C22");
    let host=Host::with_roots(output.join("state"),output.join("plugins"),Some(output.join("coordination")));
    let mut journal=storage::new_journal(uuid::Uuid::new_v4().to_string());
    journal.company="TESTCO".into();journal.endpoint=endpoint.into();journal.caller=admitted["caller"].as_str().unwrap().into();journal.temporary_access_enabled=false;journal.collection_method=if std::env::var("HERBERT_C22_MODE").as_deref()==Ok("assisted"){herbert_contract::CollectionMethod::ServerAssisted}else{herbert_contract::CollectionMethod::DirectOnly};
    if let Ok(cursor)=std::env::var("HERBERT_C22_START_CURSOR"){journal.cursors.insert("C22".into(),cursor);}
    let run=run(Storage::open(&output.join("collection"),false,journal).unwrap());
    assert!(host.prepare_native_artifacts(&run,&connection,&[plugin.clone()]).await.is_empty());
    let result=if let Ok(text)=std::env::var("HERBERT_C22_TARGET"){let identity:Identity=serde_json::from_str(&text).unwrap();host.acquire_targets(&run,&connection,&plugin,&[],vec![identity.clone()],Some(100),true).await.map(|_|vec![identity])}else{host.discover_targets(&run,&connection,&plugin,&[],Some(std::env::var("HERBERT_C22_SIZE").ok().and_then(|s|s.parse().ok()).unwrap_or(100))).await};
    let cleanup=host.cleanup_native_artifacts(&run,&connection).await;assert!(cleanup.is_empty(),"{cleanup:?}");
    connection.logout(&cancel).await.unwrap();
    let final_statistics=run.activity.finish_statistics();
    let storage=run.storage.lock().unwrap();storage.checkpoint().unwrap();
    let receipt=json!({"lane":lane,"result":result.as_ref().map(|ids|ids.len()).map_err(|e|e.clone()),"statistics":final_statistics,"artifacts":storage.journal.artifacts,"diagnostics":storage.journal.diagnostics,"nativeArtifacts":storage.journal.native_artifacts,"cleanup":cleanup,"httpRequests":connection.qualification_http.lock().unwrap().clone()});
    std::fs::write(output.join("result.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    println!("{}",json!({"lane":lane,"targets":result.as_ref().map(|ids|ids.len()).unwrap_or(0),"artifacts":storage.journal.artifacts.len(),"diagnostics":storage.journal.diagnostics}));
    assert!(result.is_ok(),"{result:?}");
    assert_eq!(storage.journal.artifacts.len(),std::env::var("HERBERT_C22_EXPECTED").ok().and_then(|v|v.parse().ok()).unwrap_or(7));
    assert!(storage.journal.diagnostics.is_empty());
    for artifact in &storage.journal.artifacts {if std::env::var("HERBERT_C22_ALLOW_PARTIAL").is_err(){assert_eq!(artifact.acquisition,Completeness::Collected,"{artifact:?}");}for file in &artifact.files {storage.verify(file).unwrap();}}
}


#[path = "reference_collection_tests.rs"]
mod reference_tests;

#[tokio::test]
async fn normal_backups_save_while_compiled_read_waits_and_survive_cancel(){
    for operation in ["materialize","acquire"] {
        let root=tempfile::tempdir().unwrap();
        let template=guest(root.path(),true,false).await;
        let descriptor=GuestReply::Descriptor{abi:1,binary_exports:vec![],response_policies:vec![],entities:vec![],methods:vec![],category:template.category.clone()};
        let action=GuestReply::Action{action:HostAction::RetainedArtifactFunctionCall{alias:"compiled_functions".into(),function:"ReadCompiled".into(),arguments:json!({})},state:json!({})};
        let a=serde_json::to_vec(&descriptor).unwrap();let b=serde_json::to_vec(&action).unwrap();
        let esc=|bytes:&[u8]|bytes.iter().map(|b|format!("\\{b:02x}")).collect::<String>();
        let packed=|p:u64,n:usize|(p<<32)|n as u64;
        let wat=format!(r#"(module (memory (export "memory") 4)
          (func $descriptor (param $len i32) (result i32) (local $i i32)
            (block $end (loop $scan
              (br_if $end (i32.gt_u (i32.add (local.get $i) (i32.const 10)) (local.get $len)))
              (if (i32.and (i64.eq (i64.load (local.get $i)) (i64.const 8390322045806929252)) (i32.eq (i32.load16_u (i32.add (local.get $i) (i32.const 8))) (i32.const 29295))) (then (return (i32.const 1))))
              (local.set $i (i32.add (local.get $i) (i32.const 1))) (br $scan))) i32.const 0)
          (data (i32.const 8192) "{}") (data (i32.const 16384) "{}")
          (func (export "hoover_abi") (result i32) i32.const 1)
          (func (export "hoover_alloc") (param i32) (result i32) i32.const 0)
          (func (export "hoover_free") (param i32 i32))
          (func (export "hoover_call") (param i32) (param $len i32) (result i64)
            (if (result i64) (call $descriptor (local.get $len))
              (then (i64.const {}))
              (else (i64.const {})))))"#,esc(&a),esc(&b),packed(8192,a.len()),packed(16384,b.len()));
        let wasm=root.path().join("compiled-wait.wasm");std::fs::write(&wasm,wat::parse_str(wat).unwrap()).unwrap();let plugin=Plugin::load(&wasm).await.unwrap();
        let host=Host::with_roots(root.path().join("state"),root.path().join("plugins"),Some(root.path().join("coordination")));
        let(connection,_server)=connection().await;let mut journal=storage::new_journal(format!("save-first-{operation}"));journal.company="C".into();journal.endpoint=connection.endpoint.to_string();journal.caller="fixture".into();journal.cursors.insert("C01".into(),"prior".into());journal.temporary_access_enabled=false;
        let run=run(Storage::open(&root.path().join("output"),false,journal).unwrap());
        let owner=Identity{category:"C01".into(),kind:"Fixture".into(),scope:Scope::Global,keys:BTreeMap::from([("ID".into(),"saved".into())])};
        let gate=Arc::new(Mutex::new(()));host.plugin_request_gates.lock().await.insert("C01".into(),gate.clone());let held=gate.lock().await;
        let request=GuestRequest{abi:1,operation:operation.into(),category:"C01".into(),company:"C".into(),identity:(operation=="acquire").then(||owner.clone()),cursor:None,page_size:None,state:json!({}),response:None,error:None};
        let reply=GuestReply::ReadyRecords{records:vec![(owner.clone(),json!({"herbertDirectOutput":true,"herbertSources":{"files":[{"name":"Authored.cs","text":"preserved original"}]}}))],state:json!({"_progress":{"pendingCompiledRead":true}})};
        let drive=host.drive_with_reply(&plugin,request,&connection,&run,&run.cancel,Some(reply));
        let check=async{
            let saved=tokio::time::timeout(std::time::Duration::from_secs(10),async{loop{if run.storage.lock().unwrap().journal.artifacts.iter().any(|a|a.identity==owner){break;}tokio::time::sleep(std::time::Duration::from_millis(1)).await;}}).await.is_ok();
            let pending=run.activity.statistics()["pendingCompiledPages"].clone();
            run.cancel.cancel();drop(held);(saved,pending)
        };
        let(result,(saved,pending))=tokio::join!(drive,check);assert!(saved,"Normal backup waited for the compiled read ({operation})");assert_eq!(pending,1);assert!(result.is_err());assert_eq!(run.activity.statistics()["pendingCompiledPages"],0);
        let store=run.storage.lock().unwrap();assert_eq!(store.journal.cursors["C01"],"prior");let artifact=store.journal.artifacts.iter().find(|a|a.identity==owner).unwrap();for file in &artifact.files{store.verify(file).unwrap();}assert!(artifact.files.iter().any(|f|f.path.ends_with("Authored.cs")));
    }
}

#[tokio::test]
async fn selected_ready_records_cannot_write_a_different_owner(){
 let root=tempfile::tempdir().unwrap();let plugin=guest(root.path(),true,false).await;let host=Host::with_roots(root.path().join("state"),root.path().join("plugins"),Some(root.path().join("coordination")));let(connection,_server)=connection().await;
 let mut journal=storage::new_journal("selected-ready-owner".into());journal.company="C".into();journal.endpoint=connection.endpoint.to_string();journal.caller="fixture".into();journal.temporary_access_enabled=false;let run=run(Storage::open(&root.path().join("output"),false,journal).unwrap());
 let owner=Identity{category:"C01".into(),kind:"Fixture".into(),scope:Scope::Global,keys:BTreeMap::from([("ID".into(),"selected".into())])};let mut other=owner.clone();other.keys.insert("ID".into(),"unselected".into());
 let request=GuestRequest{abi:1,operation:"acquire".into(),category:"C01".into(),company:"C".into(),identity:Some(owner),cursor:None,page_size:None,state:json!({}),response:None,error:None};
 let reply=GuestReply::ReadyRecords{records:vec![(other,json!({"wrong":true}))],state:json!({})};let error=host.drive_with_reply(&plugin,request,&connection,&run,&run.cancel,Some(reply)).await.unwrap_err();assert_eq!(error.code,"PLUGIN_IDENTITY_DENIED");assert!(run.storage.lock().unwrap().journal.artifacts.is_empty());
}

#[tokio::test]
#[ignore="Explicit final C08 WASM and retained native definition fixture; no ERP writes"]
async fn actual_functions_page_saves_identical_backups_during_blocked_compiled_capture(){
 let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let data:Value=serde_json::from_slice(&std::fs::read(std::env::var("HERBERT_FUNCTION_TABLES").unwrap()).unwrap()).unwrap();let id=data["EfxLibrary"][0]["LibraryID"].as_str().unwrap();let version=data["EfxLibrary"][0]["EpicorVersion"].as_str().unwrap();
 let plugin=Plugin::load(&package.join("plugins/Functions/Functions.wasm")).await.unwrap();let mut inventories=vec![];
 for enabled in [false,true]{
  let root=tempfile::tempdir().unwrap();let host=Host::with_roots(root.path().join("state"),package.join("plugins"),Some(root.path().join("coordination")));let(connection,_server)=connection().await;let mut journal=storage::new_journal(format!("actual-functions-save-first-{enabled}"));journal.company="C".into();journal.caller="fixture".into();journal.endpoint=connection.endpoint.to_string();journal.temporary_access_enabled=false;journal.collection_method=CollectionMethod::ServerAssisted;journal.plugin_settings.insert("C08".into(),json!({"downloadCompiledAssemblies":enabled,"decompileCompiledAssemblies":enabled}));journal.cursors.insert("C08".into(),"prior".into());let run=run(Storage::open(&root.path().join("output"),false,journal).unwrap());
  let gate=Arc::new(Mutex::new(()));host.plugin_request_gates.lock().await.insert("C08".into(),gate.clone());let held=gate.lock().await;
  let request=GuestRequest{abi:1,operation:"materialize".into(),category:"C08".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(2),state:json!({"phase":"libraries","ids":[id],"version":version,"system":"ERP","pageDataset":data,"nextCursor":"next"}),response:None,error:None};
  let drive=host.drive(&plugin,request,&connection,&run,&run.cancel);
  let check=async{let saved=tokio::time::timeout(std::time::Duration::from_secs(10),async{loop{if !run.storage.lock().unwrap().journal.artifacts.is_empty(){break;}tokio::time::sleep(std::time::Duration::from_millis(1)).await;}}).await.is_ok();let pending=run.activity.statistics()["pendingCompiledPages"].clone();if enabled{run.cancel.cancel();}drop(held);(saved,pending)};
  let(result,(saved,pending))=tokio::join!(drive,check);assert!(saved,"Actual C08 failed to save before compiled read");if enabled{assert!(result.is_err());assert_eq!(pending,1);}else{assert!(result.is_ok());}
  let store=run.storage.lock().unwrap();assert_eq!(store.journal.cursors["C08"],"prior");let artifact=&store.journal.artifacts[0];let mut files=BTreeMap::new();for file in &artifact.files{store.verify(file).unwrap();files.insert(file.path.clone(),file.sha256.clone());}assert!(files.keys().any(|p|p.ends_with(".efxj")));assert!(files.keys().any(|p|p.ends_with(".efxb")));inventories.push(files);
 }
 assert_eq!(inventories[0],inventories[1],"Enabling decompilation must not change normal backup bytes");
}
