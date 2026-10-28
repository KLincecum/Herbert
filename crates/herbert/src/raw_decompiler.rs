//! Raw scripting/MCP caller of the same category-independent decompiler.
use crate::{decompiler,storage,activity::Activity};
use herbert_contract::{Diagnostic,ManagedSymbolMode,MANAGED_ENGINE_ID};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use std::{collections::BTreeMap,fs,path::{Path,PathBuf},sync::Arc};
use tokio::sync::{Mutex,Semaphore};
use tokio_util::sync::CancellationToken;
use fs2::FileExt;
fn err(message:impl Into<String>)->Diagnostic{Diagnostic::new("DECOMPILER_FAILED",message)}
#[derive(Clone,Deserialize,Serialize,schemars::JsonSchema)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct Start {
 pub assembly:PathBuf,
 pub symbols:Option<PathBuf>,
 pub output:PathBuf,
 #[serde(default="symbol_none")] pub symbol_mode:ManagedSymbolMode,
}
fn symbol_none()->ManagedSymbolMode{ManagedSymbolMode::None}
#[derive(Clone,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Job {engine_sha256:String,id:String,output:PathBuf,input:Value,symbols:Option<Value>,symbol_mode:ManagedSymbolMode,state:String,result:Option<Value>,diagnostic:Option<Diagnostic>}
struct Active {cancel:CancellationToken,finished:tokio::sync::Notify,done:std::sync::atomic::AtomicBool,activity:Arc<Activity>, persistence_error:std::sync::Mutex<Option<Diagnostic>>}
pub(crate) struct Service {active:Mutex<BTreeMap<String,Arc<Active>>>,workers:Mutex<(u32,Arc<Semaphore>)>}
impl Default for Service {fn default()->Self{Self{active:Mutex::new(BTreeMap::new()),workers:Mutex::new((4,Arc::new(Semaphore::new(4))))}}}
fn record(root:&Path,id:&str)->Result<PathBuf,Diagnostic>{uuid::Uuid::parse_str(id).map_err(|_|err("Invalid decompiler job ID."))?;Ok(root.join("decompiler").join(format!("{id}.json")))}
fn save(root:&Path,job:&Job)->Result<(),Diagnostic>{storage::atomic_write(&record(root,&job.id)?,&serde_json::to_vec(job).map_err(|_|err("Cannot encode job."))?)}
fn read(root:&Path,id:&str)->Result<Job,Diagnostic>{serde_json::from_slice(&fs::read(record(root,id)?).map_err(|_|err("Decompiler job is unavailable."))?).map_err(|_|err("Decompiler job record is invalid."))}
fn lease(output:&Path)->Result<fs::File,Diagnostic>{let file=fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(output.join(".herbert-decompiler/lease")).map_err(|_|err("Cannot open decompiler output lease."))?;file.try_lock_exclusive().map_err(|_|err("Another process owns this decompiler output."))?;Ok(file)}
fn retain(source:&Path,output:&Path,name:&str)->Result<Value,Diagnostic>{
 let bytes=fs::read(source).map_err(|_|err("Cannot read the selected input file."))?;
 let path=output.join(".herbert-decompiler").join(name);storage::atomic_write(&path,&bytes)?;
 Ok(json!({"path":path,"bytes":bytes.len(),"sha256":storage::sha256(&bytes)}))
}
impl Service {
 pub async fn start(self:&Arc<Self>,root:PathBuf,plugins:PathBuf,input:Start,worker_count:u32)->Result<Value,Diagnostic>{
  crate::decompiler_settings::validate(worker_count)?;
  if !input.output.is_absolute()||!input.assembly.is_absolute()||input.symbols.as_ref().is_some_and(|p|!p.is_absolute()){return Err(err("Input and output paths must be absolute."));}
  if input.symbol_mode==ManagedSymbolMode::ExternalPortable && input.symbols.is_none(){return Err(err("External portable symbols require a symbols file."));}
  if input.symbols.is_some() && input.symbol_mode!=ManagedSymbolMode::ExternalPortable{return Err(err("Select externalPortable when supplying a symbols file."));}
  let engine=decompiler::Engine::open(&plugins)?;engine.handshake(&CancellationToken::new()).await?;
  fs::create_dir(&input.output).map_err(|_|err("Choose a new output directory; existing files are never overwritten."))?;
  fs::create_dir(input.output.join(".herbert-decompiler")).map_err(|_|err("Cannot create output bookkeeping."))?;
  let output=fs::canonicalize(&input.output).map_err(|_|err("Cannot resolve output directory."))?;let held=lease(&output)?;
  let mut job=Job{engine_sha256:engine.manifest.sha256.clone(),id:uuid::Uuid::new_v4().to_string(),input:Value::Null,symbols:None,symbol_mode:input.symbol_mode,output,state:"preparing".into(),result:None,diagnostic:None};
  // Save ownership before capturing input, so a failed copy stays attributable.
  save(&root,&job)?;
  let captured=(||{job.input=retain(&input.assembly,&job.output,"assembly.dll")?;job.symbols=input.symbols.as_ref().map(|p|retain(p,&job.output,"symbols.pdb")).transpose()?;Ok::<_,Diagnostic>(())})();
  if let Err(e)=captured{job.state="failed".into();job.diagnostic=Some(e.clone());save(&root,&job)?;return Err(e);}
  job.state="queued".into();save(&root,&job)?;let id=job.id.clone();self.launch(root,engine,job,held,worker_count).await;Ok(json!({"jobId":id,"accepted":true}))
 }
 async fn launch(self:&Arc<Self>,root:PathBuf,engine:decompiler::Engine,mut job:Job,held:fs::File,worker_count:u32){
  let active=Arc::new(Active{cancel:CancellationToken::new(),finished:Default::default(),done:Default::default(),activity:Arc::new(Activity::default()),persistence_error:Default::default()});
  let workers={let mut jobs=self.active.lock().await;let mut pool=self.workers.lock().await;
   // Keep one pool while raw jobs overlap. A new idle batch adopts the host choice.
   if jobs.values().all(|job|job.done.load(std::sync::atomic::Ordering::SeqCst)) && pool.0!=worker_count {*pool=(worker_count,Arc::new(Semaphore::new(worker_count as usize)));}
   jobs.insert(job.id.clone(),active.clone());pool.1.clone()};
  tokio::spawn(async move{let _held=held;active.activity.managed_job(&job.id,"queued");let queued=std::time::Instant::now();
   let result=async{
    let _permit=tokio::select!{_ = active.cancel.cancelled()=>return Err(err("Decompiler job cancelled.")),p=workers.acquire_owned()=>p.map_err(|_|err("Decompiler queue is closed."))?};
    active.activity.record(json!({"event":"decompilerAdmitted","queueMs":queued.elapsed().as_millis()}));
    job.state="running".into();save(&root,&job)?;active.activity.managed_job(&job.id,"running");
    let attempt=job.output.join(".herbert-decompiler").join(uuid::Uuid::new_v4().to_string());fs::create_dir(&attempt).map_err(|_|err("Cannot create job attempt."))?;let out=attempt.join("output");fs::create_dir(&out).map_err(|_|err("Cannot create attempt output."))?;
    let request=json!({"protocolVersion":1,"jobId":job.id,"engineId":MANAGED_ENGINE_ID,"input":job.input,"symbols":job.symbols,"symbolMode":job.symbol_mode,"outputDirectory":out,"controlChannel":true});let request_path=attempt.join("request.json");storage::atomic_write(&request_path,&serde_json::to_vec(&request).unwrap())?;
    engine.verify()?;decompiler::execute(&engine,&request_path,&attempt,&active.cancel,&active.activity).await?;
    let result=validate_output(&out,&job.id,job.input["sha256"].as_str().unwrap_or(""),job.symbols.as_ref().and_then(|s|s["sha256"].as_str()))?;
    job.state="committing".into();save(&root,&job)?;active.activity.managed_job(&job.id,"committing");
    // Keep every generation coherent. Recovery never overwrites a previous tree.
    if active.cancel.is_cancelled(){return Err(err("Decompiler job cancelled before commit."));}
    let project=job.output.join("Project");if project.exists(){return Err(err("The output Project directory already exists; preserved for inspection."));}
    fs::rename(&out,&project).map_err(|_|err("Cannot commit the verified project directory."))?;
    let mut result=result;result["outputDirectory"]=json!(project);Ok::<_,Diagnostic>(result)
   }.await;
   match result {Ok(result)=>{job.state=result["outcome"].as_str().unwrap_or("partial").into();job.result=Some(result);job.diagnostic=None;},Err(error)=>{job.state=if active.cancel.is_cancelled(){"cancelled"}else{"failed"}.into();job.diagnostic=Some(error);}}
   if let Err(error)=save(&root,&job){*active.persistence_error.lock().unwrap()=Some(error);}active.activity.managed_job(&job.id,&job.state);drop(_held);active.done.store(true,std::sync::atomic::Ordering::SeqCst);active.finished.notify_waiters();
  });
 }
 pub async fn status(&self,root:&Path,id:&str)->Result<Value,Diagnostic>{
  // Observe completion before reading the saved record: done is published only after save.
  // Reading in the opposite order can report inactive with a stale committing record.
  let jobs=self.active.lock().await;let active=jobs.get(id);
  let done=active.map(|job|job.done.load(std::sync::atomic::Ordering::SeqCst));
  let job=read(root,id)?;let mut value=serde_json::to_value(&job).unwrap();
  if let Some(active)=active{value["active"]=json!(!done.unwrap());value["statistics"]=active.activity.statistics();if let Some(error)=active.persistence_error.lock().unwrap().as_ref(){value["state"]=json!("persistenceFailed");value["diagnostic"]=json!(error);}}
  else {let file=fs::OpenOptions::new().read(true).write(true).open(job.output.join(".herbert-decompiler/lease")).map_err(|_|err("Cannot inspect the output lease."))?;let active=match file.try_lock_exclusive(){Ok(())=>false,Err(e) if e.kind()==std::io::ErrorKind::WouldBlock=>true,Err(_)=>return Err(err("Cannot inspect the output owner."))};if !active {value=serde_json::to_value(read(root,id)?).unwrap();}value["active"]=json!(active);if !active && matches!(value["state"].as_str(),Some("queued"|"running"|"committing")){value["state"]=json!("interrupted");}}
  Ok(value)
 }
 pub async fn cancel(&self,id:&str)->Result<Value,Diagnostic>{let jobs=self.active.lock().await;let job=jobs.get(id).ok_or_else(||err("This host does not own an active job with that ID."))?;job.cancel.cancel();Ok(json!({"cancelRequested":true}))}
 pub async fn recover(self:&Arc<Self>,root:PathBuf,plugins:PathBuf,id:&str,worker_count:u32)->Result<Value,Diagnostic>{
  crate::decompiler_settings::validate(worker_count)?;
  let mut job=read(&root,id)?;let held=lease(&job.output)?;if matches!(job.state.as_str(),"complete"|"partial"){return Ok(json!({"jobId":id,"alreadyFinished":true}));}
  let project=job.output.join("Project");if project.exists(){let mut result=validate_output(&project,id,job.input["sha256"].as_str().unwrap_or(""),job.symbols.as_ref().and_then(|s|s["sha256"].as_str()))?;result["outputDirectory"]=json!(project);job.state=result["outcome"].as_str().unwrap_or("partial").into();job.result=Some(result);job.diagnostic=None;save(&root,&job)?;return Ok(json!({"jobId":id,"recoveredCommit":true}));}
  for file in std::iter::once(&job.input).chain(job.symbols.iter()){let bytes=fs::read(file["path"].as_str().ok_or_else(||err("Input snapshot is missing."))?).map_err(|_|err("Input snapshot is unavailable."))?;if storage::sha256(&bytes)!=file["sha256"].as_str().unwrap_or(""){return Err(err("Input snapshot changed; preserved without execution."));}}
  let engine=decompiler::Engine::open(&plugins)?;if engine.manifest.sha256!=job.engine_sha256{return Err(err("The pinned engine changed; restore it before recovering this job."));}engine.handshake(&CancellationToken::new()).await?;job.state="queued".into();save(&root,&job)?;self.launch(root,engine,job,held,worker_count).await;Ok(json!({"jobId":id,"accepted":true}))
 }
 pub async fn shutdown(&self){let jobs=self.active.lock().await.values().cloned().collect::<Vec<_>>();for job in &jobs{job.cancel.cancel();}for job in jobs{loop{let wait=job.finished.notified();if job.done.load(std::sync::atomic::Ordering::SeqCst){break;}wait.await;}}}
}
fn validate_output(output:&Path,id:&str,input_hash:&str,symbol_hash:Option<&str>)->Result<Value,Diagnostic>{
 serde_json::to_value(decompiler::validate_result(output,id,input_hash,symbol_hash)?).map_err(|_|err("Cannot encode verified output."))
}

#[cfg(test)] mod tests {
 use super::*;
 use crate::application::{Host,Command};
 async fn command(host:&Arc<Host>,method:&str,args:Value)->crate::application::Reply {host.execute(Command{schema_version:1,method:method.into(),arguments:args}).await}
 #[tokio::test]
 #[ignore="Actual pinned companion package and control assembly required"]
 async fn raw_scripting_and_mcp_use_the_same_engine_without_a_connection(){
  let temp=tempfile::tempdir().unwrap();let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let assembly=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap());let original=fs::read(&assembly).unwrap();let host=Host::new(temp.path().join("state"),package.join("plugins"));
  let output=temp.path().join("raw");let started=command(&host,"decompiler.start",json!({"assembly":assembly,"output":output})).await;assert!(started.ok,"{:?}",started.error);let id=started.result.unwrap()["jobId"].as_str().unwrap().to_owned();
  let mut mcp=crate::stdio::Mcp::new(host.clone());mcp.request(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":crate::stdio::MCP_VERSION,"capabilities":{},"clientInfo":{"name":"qualification","version":"1"}}})).await;mcp.request(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await;
  let listing=mcp.request(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await.unwrap();assert!(listing["result"]["tools"].as_array().unwrap().iter().any(|t|t["name"]=="decompiler_start"));
  let result=loop {let response=mcp.request(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"decompiler_status","arguments":{"id":id}}})).await.unwrap();let value=response["result"]["structuredContent"]["result"].clone();if value["active"]==false{break value;}tokio::time::sleep(std::time::Duration::from_millis(20)).await;};
  assert_eq!(result["state"],"partial");assert!(output.join("Project/Reconstructed.csproj").is_file());assert!(result["statistics"]["meaningfulWork"].as_u64().unwrap()>0);assert_eq!(fs::read(assembly.clone()).unwrap(),original);
  let duplicate=command(&host,"decompiler.start",json!({"assembly":assembly,"output":output})).await;assert!(!duplicate.ok);assert_eq!(fs::read(assembly).unwrap(),original);
  let fresh=Host::new(temp.path().join("state"),package.join("plugins"));assert!(command(&fresh,"decompiler.status",json!({"id":id})).await.ok);assert!(command(&fresh,"decompiler.recover",json!({"id":id})).await.ok);assert!(host.shutdown().await.is_empty());assert!(fresh.shutdown().await.is_empty());
 }

 #[tokio::test]
 async fn raw_status_observes_final_record_after_completion_published_while_waiting(){
  use std::{future::Future,task::Poll,sync::atomic::Ordering};
  let temp=tempfile::tempdir().unwrap();let service=Service::default();let id=uuid::Uuid::new_v4().to_string();
  let mut job=Job{engine_sha256:String::new(),id:id.clone(),output:temp.path().join("output"),input:Value::Null,symbols:None,symbol_mode:ManagedSymbolMode::None,state:"committing".into(),result:None,diagnostic:None};save(temp.path(),&job).unwrap();
  let active=Arc::new(Active{cancel:CancellationToken::new(),finished:Default::default(),done:Default::default(),activity:Arc::new(Activity::default()),persistence_error:Default::default()});
  let mut jobs=service.active.lock().await;jobs.insert(id.clone(),active.clone());
  let mut status=Box::pin(service.status(temp.path(),&id));
  std::future::poll_fn(|cx|{assert!(status.as_mut().poll(cx).is_pending());Poll::Ready(())}).await;
  job.state="partial".into();job.result=Some(json!({"outcome":"partial"}));save(temp.path(),&job).unwrap();active.done.store(true,Ordering::SeqCst);drop(jobs);
  let reply=status.await.unwrap();assert_eq!(reply["active"],false);assert_eq!(reply["state"],"partial");assert_eq!(reply["result"]["outcome"],"partial");
 }
 #[tokio::test]
 async fn raw_commands_reject_unknown_arguments_and_invalid_ids_without_connection(){let t=tempfile::tempdir().unwrap();let host=Host::new(t.path().join("state"),t.path().join("plugins"));for(method,args)in[("decompiler.status",json!({"id":"../escape"})),("decompiler.start",json!({"assembly":"a","output":"b","command":"bad"}))]{assert!(!command(&host,method,args).await.ok);}assert!(!t.path().join("escape").exists());}

 #[tokio::test]
 #[ignore="Actual pinned companion and control assembly required"]
 async fn raw_cancel_recovery_and_committed_reconciliation_preserve_inputs(){
  let temp=tempfile::tempdir().unwrap();let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let assembly=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap());let root=temp.path().join("state");let plugins=package.join("plugins");let service=Arc::new(Service::default());let permit=service.workers.lock().await.1.clone().acquire_many_owned(4).await.unwrap();
  let output=temp.path().join("output");let started=service.start(root.clone(),plugins.clone(),Start{assembly,output,symbols:None,symbol_mode:ManagedSymbolMode::None},4).await.unwrap();let id=started["jobId"].as_str().unwrap();
  assert!(Arc::new(Service::default()).recover(root.clone(),plugins.clone(),id,4).await.is_err());
  service.cancel(id).await.unwrap();loop{if service.status(&root,id).await.unwrap()["active"]==false{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
  assert_eq!(service.status(&root,id).await.unwrap()["state"],"cancelled");drop(permit);
  let mut saved=read(&root,id).unwrap();let snapshot=PathBuf::from(saved.input["path"].as_str().unwrap());let original=fs::read(&snapshot).unwrap();fs::write(&snapshot,b"changed input").unwrap();assert!(service.recover(root.clone(),plugins.clone(),id,4).await.is_err());fs::write(&snapshot,&original).unwrap();
  service.recover(root.clone(),plugins.clone(),id,4).await.unwrap();loop{if service.status(&root,id).await.unwrap()["active"]==false{break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
  saved=read(&root,id).unwrap();assert_eq!(saved.state,"partial");let project=saved.output.join("Project");saved.state="committing".into();saved.result=None;save(&root,&saved).unwrap();
  let fresh=Arc::new(Service::default());assert_eq!(fresh.recover(root.clone(),plugins.clone(),id,4).await.unwrap()["recoveredCommit"],true);assert_eq!(read(&root,id).unwrap().result.unwrap()["outputDirectory"],json!(project));
  let generated=project.join("Reconstructed.csproj");fs::write(&generated,b"preserved user edit").unwrap();saved.state="committing".into();save(&root,&saved).unwrap();assert!(fresh.recover(root.clone(),plugins,id,4).await.is_err());assert_eq!(fs::read(&generated).unwrap(),b"preserved user edit");assert_eq!(fs::read(snapshot).unwrap(),original);
 }
}
