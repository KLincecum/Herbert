//! Actual shared API, compiled descriptor and loopback admission. No ERP.
use super::*;
use crate::input::{AuthMode,Credentials};
use std::{io::{BufRead,BufReader,Read,Write},net::{TcpListener,TcpStream},sync::Mutex as StdMutex};

struct Server {endpoint:url::Url,calls:Arc<StdMutex<Vec<String>>>,stop:Arc<AtomicBool>,worker:Option<std::thread::JoinHandle<()>>}
impl Server {
    fn new()->Self {
        let listener=TcpListener::bind(("127.0.0.1",0)).unwrap();let endpoint=url::Url::parse(&format!("http://{}/fixture/",listener.local_addr().unwrap())).unwrap();let calls=Arc::new(StdMutex::new(Vec::new()));let seen=calls.clone();let stop=Arc::new(AtomicBool::new(false));let done=stop.clone();
        let worker=std::thread::spawn(move||for stream in listener.incoming(){let mut stream=stream.unwrap();if done.load(Ordering::SeqCst){break;}let mut reader=BufReader::new(&mut stream);let mut first=String::new();reader.read_line(&mut first).unwrap();let route=first.split_whitespace().nth(1).unwrap().to_owned();let method=route.rsplit('/').next().unwrap().to_owned();let mut length=0;loop{let mut line=String::new();reader.read_line(&mut line).unwrap();if line=="\r\n"{break;}if let Some((key,value))=line.split_once(':'){if key.eq_ignore_ascii_case("content-length"){length=value.trim().parse().unwrap();}}}let mut body=vec![0;length];reader.read_exact(&mut body).unwrap();drop(reader);seen.lock().unwrap().push(method.clone());let value=match method.as_str(){"GetUserFile"=>json!({"returnObj":{"UserFile":[{"DcdUserID":"fixture","SecurityMgr":true,"UserDisabled":false,"LockedOut":false}]}}),"GetDatabaseVersion"=>json!({"returnObj":"5.2.100"}),_=>json!({"unexpected":method})};let text=serde_json::to_string(&value).unwrap();write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).unwrap();});
        Self{endpoint,calls,stop,worker:Some(worker)}
    }
}
impl Drop for Server {fn drop(&mut self){self.stop.store(true,Ordering::SeqCst);let _=TcpStream::connect(self.endpoint.socket_addrs(||None).unwrap()[0]);self.worker.take().unwrap().join().unwrap();}}
fn module(root:&Path,label:&str) {
    std::fs::create_dir_all(root).unwrap();
    let descriptor=GuestReply::Descriptor{abi:PLUGIN_ABI,methods:vec![("Ice.BO.DynamicReportSvc".into(),"ExportToFile".into())],entities:vec![],binary_exports:vec![],response_policies:vec![],category:Category{id:"C16".into(),name:label.into(),selected:true,description:"Local authorization fixture".into(),plugin_version:Some("test".into()),available:true,qualification:"fixture".into(),roles:vec!["collector".into()],helpers:vec![],artifacts:vec![],settings:vec![PluginSetting::integer("itemsPerPage","Batch",100,1,i32::MAX as u32)],help:vec![]}};
    let bytes=serde_json::to_vec(&descriptor).unwrap();let escaped=bytes.iter().map(|b|format!("\\{b:02x}")).collect::<String>();let packed=(65536u64<<32)|bytes.len()as u64;
    let wat=format!(r#"(module (memory (export "memory") 4) (data (i32.const 65536) "{escaped}") (func (export "hoover_abi") (result i32) i32.const 1) (func (export "hoover_alloc") (param i32) (result i32) i32.const 0) (func (export "hoover_free") (param i32 i32)) (func (export "hoover_call") (param i32 i32) (result i64) i64.const {packed}))"#);
    std::fs::write(root.join("fixture.wasm"),wat::parse_str(wat).unwrap()).unwrap();
}
async fn fixture(root:&Path)->(Arc<Host>,Server) {
    let server=Server::new();let plugins=root.join("plugins");module(&plugins,"Original");let host=Host::new(root.join("state"),plugins);let connection=Connection::new(server.endpoint.clone(),"C".into(),AuthMode::BasicApiKey,Credentials{username:"fixture".into(),password:"fictional".into(),api_key:Some("fictional".into())},OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();connection.connect(&CancellationToken::new()).await.unwrap();*host.connection.write().await=Some(connection);(host,server)
}
fn request(root:&Path,id:&str)->Value {json!({"requestId":id,"destination":root.join("output"),"outputMode":"refresh","categories":["C16"],"collectionMethod":"directOnly","temporaryAccess":false,"pluginSettings":{"C16":{"itemsPerPage":100}}})}
async fn call(host:&Arc<Host>,method:&str,args:Value)->Reply{host.execute(Command{schema_version:1,method:method.into(),arguments:args}).await}
async fn preview(host:&Arc<Host>,run:Value)->Value {let reply=call(host,"serverChanges.preview",json!({"fullBackup":true,"run":run})).await;assert!(reply.ok,"{:?}",reply.error);reply.result.unwrap()}
fn acknowledgement(preview:&Value,lifetime:Value)->Value {json!({"previewId":preview["previewId"],"materialDigest":preview["materialDigest"],"lifetime":lifetime})}

#[tokio::test]
async fn explicit_api_acknowledgement_is_idempotent_inspectable_revocable_and_not_human_attestation() {
    let root=tempfile::tempdir().unwrap();let(host,server)=fixture(root.path()).await;let before=server.calls.lock().unwrap().clone();let p=preview(&host,request(root.path(),"request")).await;assert_eq!(p["readOnly"],true);assert_eq!(p["authorizationRequired"],true);assert_eq!(before,*server.calls.lock().unwrap());assert!(!root.path().join("output").exists());
    let ack=acknowledgement(&p,json!({"kind":"session"}));let mut spoof=ack.clone();spoof["origin"]=json!("interactiveGui");assert!(!call(&host,"serverChanges.authorize",spoof).await.ok);
    let reply=call(&host,"serverChanges.authorize",ack.clone()).await;assert!(reply.ok,"{:?}",reply.error);let grant=reply.result.unwrap()["authorization"].clone();assert_eq!(grant["origin"],"explicitLocalClient");
    let repeated=call(&host,"serverChanges.authorize",ack.clone()).await;assert!(repeated.ok);assert_eq!(repeated.result.as_ref().unwrap()["reconciled"],true);assert_eq!(repeated.result.unwrap()["authorization"]["id"],grant["id"]);
    let listed=call(&host,"serverChanges.list",json!({})).await;assert_eq!(listed.result.unwrap()["authorizations"].as_array().unwrap().len(),1);
    let inspected=call(&host,"serverChanges.inspect",json!({"id":grant["id"]})).await;assert_eq!(inspected.result.unwrap(),grant);
    let mut changed=ack;changed["lifetime"]=json!({"kind":"oneRun"});assert!(!call(&host,"serverChanges.authorize",changed).await.ok);
    assert!(call(&host,"serverChanges.revoke",json!({"id":grant["id"]})).await.ok);let revoked=call(&host,"serverChanges.inspect",json!({"id":grant["id"]})).await.result.unwrap();assert!(revoked["revokedAt"].is_number());
    assert_eq!(before,*server.calls.lock().unwrap());assert!(!root.path().join("output").exists());host.shutdown().await;
}
#[tokio::test]
async fn start_requires_explicit_authority_binds_journal_and_reconciles_original_run_after_revocation() {
    let root=tempfile::tempdir().unwrap();let(host,server)=fixture(root.path()).await;let mut run=request(root.path(),"one-logical-request");let before=server.calls.lock().unwrap().clone();
    let denied=call(&host,"backup.start",run.clone()).await;assert!(!denied.ok);assert_eq!(denied.error.unwrap().code,"SERVER_CHANGE_PREVIEW_REQUIRED");assert!(!root.path().join("output").exists());assert_eq!(before,*server.calls.lock().unwrap());
    let p=preview(&host,run.clone()).await;let approved=call(&host,"serverChanges.authorize",acknowledgement(&p,json!({"kind":"oneRun"}))).await;assert!(approved.ok,"{:?}",approved.error);let grant=approved.result.unwrap()["authorization"].clone();run["authorizationId"]=grant["id"].clone();let started=call(&host,"backup.start",run.clone()).await;assert!(started.ok,"{:?}",started.error);let run_id=started.result.unwrap()["runId"].as_str().unwrap().to_owned();
    {let record=crate::recovery::RunIndex::open(&host.state_root).unwrap().get(&run_id).unwrap();let authority=record.journal.server_change_authority.unwrap();assert_eq!(authority.grant_id,grant["id"]);assert_eq!(authority.request_id,"one-logical-request");assert_eq!(authority.material_digest,authority.material_scope.digest());}
    assert!(call(&host,"serverChanges.revoke",json!({"id":grant["id"]})).await.ok);let reconciled=call(&host,"backup.start",run.clone()).await;assert!(reconciled.ok);assert_eq!(reconciled.result.unwrap()["runId"],run_id);
    run["requestId"]=json!("another-run");let another=call(&host,"backup.start",run).await;assert!(!another.ok);host.shutdown().await;
    assert!(server.calls.lock().unwrap().iter().all(|method|matches!(method.as_str(),"GetUserFile"|"GetDatabaseVersion"|"GetFieldList"))); // accepted collection may read native schema; it cannot export or mutate here
    let record=crate::recovery::RunIndex::open(&host.state_root).unwrap().get(&run_id).unwrap();let journal=crate::recovery::read_journal(&record).unwrap();assert_eq!(journal.server_change_authority.unwrap().grant_id,grant["id"]);
}
#[tokio::test]
async fn stale_module_preview_cannot_create_authority_and_workflow_and_session_contexts_are_explicit() {
    let root=tempfile::tempdir().unwrap();let(host,_server)=fixture(root.path()).await;let mut run=request(root.path(),"original");let p=preview(&host,run.clone()).await;module(&host.plugin_root,"Changed native semantics");let stale=call(&host,"serverChanges.authorize",acknowledgement(&p,json!({"kind":"session"}))).await;assert!(!stale.ok);assert!(crate::server_changes::list(&host.state_root).unwrap().is_empty());
    let p=preview(&host,run.clone()).await;let created=call(&host,"serverChanges.authorize",acknowledgement(&p,json!({"kind":"workflow","workflowId":"nightly"}))).await;assert!(created.ok,"{:?}",created.error);let id=created.result.unwrap()["authorization"]["id"].clone();run["authorizationId"]=id;let decoded:StartInput=decode(run.clone()).unwrap();assert!(host.admit_server_changes(&decoded,true).await.is_err());run["workflowId"]=json!("nightly");let decoded:StartInput=decode(run.clone()).unwrap();assert!(host.admit_server_changes(&decoded,true).await.unwrap().is_some());run["pluginSettings"]["C16"]["itemsPerPage"]=json!(300);let tuned=preview(&host,run).await;assert_eq!(tuned["matchingAuthorizations"].as_array().unwrap().len(),1);
    let p=preview(&host,request(root.path(),"session-run")).await;let session=call(&host,"serverChanges.authorize",acknowledgement(&p,json!({"kind":"session"}))).await.result.unwrap()["authorization"].clone();let mut run=request(root.path(),"session-run");run["authorizationId"]=session["id"].clone();let decoded:StartInput=decode(run).unwrap();assert!(host.admit_server_changes(&decoded,true).await.is_ok());*host.authorization_session.write().await=uuid::Uuid::new_v4().to_string();assert!(host.admit_server_changes(&decoded,true).await.is_err());host.shutdown().await;
}
