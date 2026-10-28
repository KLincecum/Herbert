use super::*;
use base64::Engine as _;
use std::{io::{BufRead, BufReader, Read, Write}, net::{TcpListener,TcpStream}, sync::Mutex as StdMutex};
struct Fixture { endpoint:url::Url, stop:Arc<AtomicBool>, worker:Option<std::thread::JoinHandle<()>>, calls:Arc<StdMutex<Vec<String>>> }
impl Fixture {
 fn new(failures:usize, committed:bool, status:u16)->Self {
  let listener=TcpListener::bind("127.0.0.1:0").unwrap();let addr=listener.local_addr().unwrap();
  let stop=Arc::new(AtomicBool::new(false));let done=stop.clone();let calls=Arc::new(StdMutex::new(Vec::new()));let seen=calls.clone();
  let worker=std::thread::spawn(move||{let mut package:Option<Value>=None;let mut imports=0;
   for stream in listener.incoming(){let mut stream=stream.unwrap();if done.load(Ordering::SeqCst){break;}
    let mut reader=BufReader::new(&mut stream);let mut line=String::new();reader.read_line(&mut line).unwrap();let method=line.split_whitespace().nth(1).unwrap().rsplit('/').next().unwrap().to_owned();let mut length=0;
    loop{let mut header=String::new();reader.read_line(&mut header).unwrap();if header=="\r\n"{break;}if let Some((key,value))=header.split_once(':'){if key.eq_ignore_ascii_case("content-length"){length=value.trim().parse().unwrap();}}}
    let mut bytes=vec![0;length];reader.read_exact(&mut bytes).unwrap();drop(reader);let args:Value=serde_json::from_slice(&bytes).unwrap_or(json!({}));seen.lock().unwrap().push(method.clone());let mut code=200;
    let value=match method.as_str(){
     "GetUserFile"=>json!({"returnObj":{"UserFile":[{"DcdUserID":"fixture","SecurityMgr":true,"UserDisabled":false,"LockedOut":false}]}}),
     "GetDatabaseVersion"=>json!({"returnObj":"5.2.100"}),
     "GetLibrary"=>if let Some(p)=&package {let mut row=p["Library"].clone();row["LibraryID"]=p["LibraryId"].clone();row["SysRowID"]=json!("11111111-2222-4333-8444-555555555555");json!({"returnObj":{"EfxLibrary":[row],"EfxFunction":[]}})} else {json!({"returnObj":{"EfxLibrary":[],"EfxFunction":[]}})},
     "ImportLibrary"=>{imports+=1;assert!(package.is_none(),"duplicate import");assert_eq!(args["options"]["OverwriteMode"],0);let b=base64::engine::general_purpose::STANDARD.decode(args["libraryPackage"].as_str().unwrap()).unwrap();let p:Value=serde_json::from_slice(b.strip_prefix(&[0xef,0xbb,0xbf]).unwrap_or(&b)).unwrap();if imports>failures||committed{package=Some(p);}if imports<=failures{code=status;}json!({"returnObj":{"BOUpdError":[]}})},
     "ApplyChangesWithDiagnostics"=>json!({}),
     "PromoteToProduction"=>{package.as_mut().unwrap()["Library"]["Published"]=json!(true);json!({})},
     "ExportLibrary"=>json!({"returnObj":base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(package.as_ref().unwrap()).unwrap())}),
     _=>json!({})
    };let body=serde_json::to_vec(&value).unwrap();write!(stream,"HTTP/1.1 {} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",code,body.len()).unwrap();stream.write_all(&body).unwrap();
   }
  });Self{endpoint:url::Url::parse(&format!("http://{addr}/fixture/")).unwrap(),stop,worker:Some(worker),calls}
 }
}
impl Drop for Fixture {fn drop(&mut self){self.stop.store(true,Ordering::SeqCst);let _=TcpStream::connect(self.endpoint.socket_addrs(||None).unwrap()[0]);self.worker.take().unwrap().join().unwrap();}}
async fn exercise(failures:usize, committed:bool,status:u16)->(Result<(),Diagnostic>,usize,String){exercise_inner(failures,committed,status,false).await}
async fn exercise_inner(failures:usize, committed:bool,status:u16,cancel_wait:bool)->(Result<(),Diagnostic>,usize,String){
 let server=Fixture::new(failures,committed,status);let root=tempfile::tempdir().unwrap();let host=Host::new(root.path().join("state"),root.path().join("plugins"));
 let connection=Connection::new(server.endpoint.clone(),"FIXTURE".into(),AuthMode::BasicApiKey,Credentials{username:"fixture".into(),password:"fixture".into(),api_key:Some("fixture".into())},OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();connection.connect(&CancellationToken::new()).await.unwrap();
 let mut journal=storage::new_journal("retry-fixture".into());journal.collection_method=CollectionMethod::ServerAssisted;
 let store=Storage::open(root.path().join("output").as_path(),false,journal).unwrap();
 let run=Arc::new(Run{storage:Arc::new(std::sync::Mutex::new(store)),cancel:CancellationToken::new(),paused:AtomicBool::new(false),wake:Notify::new(),active:AtomicBool::new(true),network:false,helper_restore_attempts:Mutex::new(BTreeSet::new()),activity:Arc::new(Default::default()),shared_reads:Default::default(),helper_instances:Mutex::new(BTreeMap::new()),collection_context:Mutex::new(None)});
 let declaration=serde_json::from_value(json!({"alias":"reader","type":"functionLibrary","file":"reader.efxj","lifetime":"temporary","policy":"createInstance","effects":{"description":"Fixture"}})).unwrap();
 let source=serde_json::to_vec(&json!({"Mode":"Backup","LibraryId":"Fixture","Library":{"Disabled":false,"Functions":[]}})).unwrap();let snapshot=crate::plugin_artifacts::NativeArtifactSnapshot{declaration,files:BTreeMap::from([("reader.efxj".into(),Arc::new(source))])};
 // Host control helper avoids introducing an unrelated server-lease fixture.
 let mut file=OperationFile::open(&host.state_root,"retry-fixture",super::super::host_utility::OWNER,ArtifactScope{endpoint:connection.endpoint.to_string(),company:"FIXTURE".into(),caller:"fixture".into()},vec![snapshot]).unwrap();
 let cancel=CancellationToken::new();let trigger=cancel.clone();let calls=server.calls.clone();
 let canceller=if cancel_wait {Some(tokio::spawn(async move {loop {if calls.lock().unwrap().iter().any(|s|s=="ImportLibrary"){break;}tokio::time::sleep(std::time::Duration::from_millis(5)).await;}tokio::time::sleep(std::time::Duration::from_millis(50)).await;trigger.cancel();}))}else{None};
 let result=host.install_native_artifact(&mut file,0,&run,&connection,&cancel).await;
 if let Some(task)=canceller {task.abort();}
 let count=server.calls.lock().unwrap().iter().filter(|s|s.as_str()=="ImportLibrary").count();
 if failures>3 {let _=host.install_native_artifact(&mut file,0,&run,&connection,&CancellationToken::new()).await;assert_eq!(server.calls.lock().unwrap().iter().filter(|s|s.as_str()=="ImportLibrary").count(),count,"later page reset exhausted budget");}
 let phase=file.record.artifacts[0].phase.clone();assert!(host.shutdown().await.is_empty());(result,count,phase)
}
#[tokio::test] async fn sql_500_absent_import_retries_same_identity_and_recovers(){let (r,n,p)=exercise(1,false,500).await;assert!(r.is_ok(),"{r:?}");assert_eq!(n,2);assert_eq!(p,"ready");}
#[tokio::test] async fn committed_500_is_reconciled_without_duplicate_import(){let (r,n,p)=exercise(1,true,500).await;assert!(r.is_ok(),"{r:?}");assert_eq!(n,1);assert_eq!(p,"ready");}
#[tokio::test] async fn transient_exhaustion_stops_after_three_retries_and_stays_recoverable(){let (r,n,p)=exercise(10,false,503).await;assert_eq!(r.unwrap_err().context["helperSetupRetries"],3);assert_eq!(n,4);assert_eq!(p,"importAbsent");}
#[tokio::test] async fn permission_refusal_is_not_retried(){let (r,n,_)=exercise(1,false,403).await;assert_eq!(r.unwrap_err().code,"ACCESS_DENIED");assert_eq!(n,1);}
#[test] fn semantic_failures_are_terminal(){for code in ["ACCESS_DENIED","ARTIFACT_OWNER_MISMATCH","ARTIFACT_COMPILATION_FAILED","ARTIFACT_PACKAGE_INVALID","CANCELED"]{assert!(!helper_setup_transient(&issue(code,"fixture")));}}

#[tokio::test] async fn cancellation_interrupts_retry_wait_without_another_import(){let (r,n,_)=exercise_inner(1,false,500,true).await;assert_eq!(r.unwrap_err().code,"CANCELED");assert_eq!(n,1);}
