//! Durable local decompilation. Captures survive optional engine failures.
//! This sidecar is separate from the strict v1 RunJournal so an older host leaves
//! newer processing state intact instead of deserializing it as an old journal.
use super::*;
use std::{collections::{BTreeMap, BTreeSet}, fs, process::Stdio};
use serde::{Deserialize, Serialize};
const STATE: &str = "Herbert Data/managed-decompilation/v1/state.json";
#[path="local_decompilation.rs"]
mod local;
pub(super) use local::{record_local_read,validate_local_reader_scope,read_local_artifact};


pub(crate) use crate::decompiler::qualification_boundary;

fn issue(code:&str,message:impl Into<String>)->Diagnostic { Diagnostic::new(code,message) }
fn failure(message:impl Into<String>)->Diagnostic { issue("MANAGED_DECOMPILATION_FAILED",message) }

use crate::decompiler::{Engine,GeneratedResult};
#[cfg(test)] use crate::decompiler::{EngineManifest,rid};
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct CaptureRecord { request:ManagedDecompilationRequest,job_ids:Vec<String>,#[serde(default)] current_job_id:Option<String>,project_state:String,diagnostics:Vec<Diagnostic> }
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Job { id:String,engine_sha256:String,input:FileRef,symbols:Option<FileRef>,symbol_mode:ManagedSymbolMode,state:String,attempt:Option<String>,files:Vec<FileRef>,actual_root:Option<String>,user_conflict:bool,elapsed_milliseconds:Option<u64>,#[serde(default)] attempt_history:Vec<Value>,#[serde(default)] retirement:Option<AttemptRetirement>,diagnostics:Vec<Diagnostic> }
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct CurrentOwnerSnapshot { expected_captures:usize,capture_ids:Vec<String> }
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct Ledger { #[serde(default,skip_serializing_if="BTreeMap::is_empty")] local_enrollments:BTreeMap<String,local::Enrollment>,#[serde(default,skip_serializing_if="BTreeMap::is_empty")] local_reads:BTreeMap<String,local::Read>,#[serde(default,skip_serializing_if="BTreeMap::is_empty")] local_owner_state:BTreeMap<String,local::OwnerState>,protocol_version:u32,run_id:String,requests:BTreeMap<String,String>,captures:BTreeMap<String,CaptureRecord>,jobs:BTreeMap<String,Job>,#[serde(default)] owner_processing:BTreeMap<String,String>,#[serde(default)] reads:BTreeMap<String,ReadReceipt>,#[serde(default)] current_snapshots:BTreeMap<String,CurrentOwnerSnapshot> }
fn load(store:&Storage)->Result<Ledger,Diagnostic> {
    let path=store.confined(STATE)?;
    match fs::read(path) {
        Ok(bytes)=>{if let Ok(ledger)=serde_json::from_slice::<Ledger>(&bytes){if ledger.protocol_version!=1||ledger.run_id!=store.journal.run_id{return Err(failure("The managed-job sidecar has another protocol or run owner."));}return Ok(ledger);}
            let mut value:Value=serde_json::from_slice(&bytes).map_err(|_|failure("The managed-job sidecar cannot be read; it was preserved."))?;
            // Preserve links from the private prototype's single-job envelope.
            if let Some(captures)=value["captures"].as_object_mut(){for capture in captures.values_mut(){if let Some(fields)=capture.as_object_mut(){if let Some(job)=fields.remove("jobId"){fields.entry("jobIds").or_insert_with(||if job.is_null(){json!([])}else{json!([job])});}}}}
            let ledger:Ledger=serde_json::from_value(value).map_err(|_|failure("The managed-job sidecar schema cannot be read; it was preserved."))?;
            if ledger.protocol_version!=1||ledger.run_id!=store.journal.run_id {return Err(failure("The managed-job sidecar has another protocol or run owner."));}Ok(ledger)},
        Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Ok(Ledger{local_enrollments:BTreeMap::new(),local_reads:BTreeMap::new(),local_owner_state:BTreeMap::new(),protocol_version:1,run_id:store.journal.run_id.clone(),requests:BTreeMap::new(),captures:BTreeMap::new(),jobs:BTreeMap::new(),owner_processing:BTreeMap::new(),reads:BTreeMap::new(),current_snapshots:BTreeMap::new()}),
        Err(_)=>Err(failure("Cannot read the managed-job sidecar.")),
    }
}
fn current_capture(ledger:&Ledger,id:&str,capture:&CaptureRecord)->bool {ledger.current_snapshots.get(&capture.request.capture.owner.keys["LibraryID"]).is_none_or(|snapshot|snapshot.capture_ids.iter().any(|saved|saved==id))}
fn save(store:&Storage,ledger:&Ledger)->Result<(),Diagnostic> {storage::atomic_write(&store.confined(STATE)?,&serde_json::to_vec(ledger).map_err(|_|failure("Cannot encode the managed-job sidecar."))?)}
fn key(value:&Value)->String {storage::sha256(&serde_json::to_vec(value).expect("JSON values serialize"))}
fn job_key(capture:&ManagedCapture,engine:&Engine)->String {
    job_key_for(capture.input.as_ref().expect("validated captured input"),capture.symbols.as_ref(),&capture.symbol_mode,engine)
}
fn job_key_for(input:&FileRef,symbols:Option<&FileRef>,mode:&ManagedSymbolMode,engine:&Engine)->String {
    let symbols=match mode {ManagedSymbolMode::ExternalPortable=>symbols.map(|f|json!([f.bytes,f.sha256])),_=>None};
    key(&json!({"input":[input.bytes,input.sha256],"symbols":symbols,"symbolMode":mode,
        "engine":engine.manifest.sha256,"engineId":engine.manifest.engine_id,"protocol":1,"settings":{"language":"latest","resources":"raw"},"resolver":"target-only-v1","frameworkMetadata":[],"rid":engine.manifest.target_rid,"outputSchema":1}))
}

fn ensure_job(ledger:&mut Ledger,capture:&ManagedCapture,engine:&Engine)->String{
    ensure_job_for(ledger,capture.input.as_ref().expect("validated input"),capture.symbols.as_ref(),&capture.symbol_mode,engine)
}
fn ensure_job_for(ledger:&mut Ledger,input:&FileRef,symbols:Option<&FileRef>,mode:&ManagedSymbolMode,engine:&Engine)->String {
    let id=job_key_for(input,symbols,mode,engine);
    ledger.jobs.entry(id.clone()).or_insert_with(||Job{id:id.clone(),engine_sha256:engine.manifest.sha256.clone(),input:input.clone(),symbols:if *mode==ManagedSymbolMode::ExternalPortable{symbols.cloned()}else{None},symbol_mode:mode.clone(),state:"queued".into(),attempt:None,files:vec![],actual_root:None,user_conflict:false,elapsed_milliseconds:None,attempt_history:vec![],retirement:None,diagnostics:vec![]});id
}

fn valid_owner(owner:&Identity,category:&str,target:Option<&Identity>,store:&Storage)->bool {
    owner.category==category && owner.category=="C08" && owner.kind=="EfxLibrary" && matches!(owner.scope,Scope::Global)
        && owner.keys.get("LibraryID").is_some_and(|id|!id.is_empty())
        && match target {Some(target)=>target==owner,None=>store.manifest.artifacts.contains_key(&storage::identity_key(owner))}
}

pub(super) struct Supervisor {
    handles:Mutex<BTreeMap<(String,String),tokio::task::JoinHandle<Result<(),Diagnostic>>>>,
    workers:Arc<tokio::sync::Semaphore>,
    worker_override:bool,
    run_workers:Mutex<BTreeMap<String,Arc<tokio::sync::Semaphore>>>,
    checked_engines:Mutex<BTreeSet<String>>,
}
impl Default for Supervisor {
    fn default()->Self {let mut supervisor=Self::with_workers(4);supervisor.worker_override=false;supervisor}
}
impl Supervisor {
    // Default four is measured on the same 24-assembly corpus on Windows/Linux:
    // 12.7–13.2/10.8–11.1 s vs 20.5–21.1/18.0–18.3 s at one worker.
    // This is tunable per run; neither four nor the measured corpus is a cap.
    pub(super) fn with_workers(workers:usize)->Self {assert!(workers>0);Self{handles:Mutex::new(BTreeMap::new()),workers:Arc::new(tokio::sync::Semaphore::new(workers)),worker_override:true,run_workers:Mutex::new(BTreeMap::new()),checked_engines:Mutex::new(BTreeSet::new())}}
    async fn engine(&self,plugin_root:&Path,run:&Arc<Run>)->Result<Engine,Diagnostic>{
        let engine=Engine::descriptor(plugin_root)?;let mut checked=self.checked_engines.lock().await;
        if !checked.contains(&engine.manifest.sha256){engine.handshake(&run.cancel).await?;checked.insert(engine.manifest.sha256.clone());}Ok(engine)
    }
    async fn permits(&self,run:&Arc<Run>)->Result<Arc<tokio::sync::Semaphore>,Diagnostic>{
        if self.worker_override{return Ok(self.workers.clone());}
        let(id,count)={let store=run.storage.lock().map_err(|_|lock_error())?;
            let count=store.journal.settings.local_decompiler_workers;
            crate::decompiler_settings::validate(count)?;
            (store.journal.run_id.clone(),count as usize)};
        Ok(self.run_workers.lock().await.entry(id).or_insert_with(||Arc::new(tokio::sync::Semaphore::new(count))).clone())
    }
    pub(super) async fn enqueue(&self,plugin_root:&Path,run:&Arc<Run>,category:&str,target:Option<&Identity>,issued:&[FileRef],request:ManagedDecompilationRequest)->Result<ManagedDecompilationReceipt,Diagnostic> {
        if let Some(directory)=&request.output_subdirectory {storage::safe_relative(directory)?;}
        let engine=if request.create_project {Some(self.engine(plugin_root,run).await)}else{None};
        let receipt={
            let mut store=run.storage.lock().map_err(|_|failure("Run storage is unavailable."))?;
            if request.protocol_version!=1||request.expected_engine!=MANAGED_ENGINE_ID||request.request_id.is_empty()||!valid_owner(&request.capture.owner,category,target,&store) {return Err(failure("The managed capture protocol, engine or owner is invalid."));}
            if request.capture.input.is_some() && request.capture.selector.as_ref().is_none_or(|selector|selector.folder!=":Efx" || selector.assembly!=format!("efx.{}",request.capture.owner.keys["LibraryID"].to_lowercase())) {return Err(failure("The compiled selector does not match its Function owner."));}
            for file in request.capture.input.iter().chain(request.capture.symbols.iter()) {
                if file.role!="native" || !issued.contains(file) {return Err(issue("MANAGED_INPUT_OWNERSHIP_DENIED","Local processing accepts only immutable native files issued to this operation."));}
                store.verify(file)?;
            }
            if request.capture.symbol_mode==ManagedSymbolMode::ExternalPortable && request.capture.symbols.is_none() {return Err(failure("External-symbol processing needs a retained symbol receipt."));}
            let capture_id=key(&json!([store.journal.endpoint,store.journal.company,store.journal.caller,store.journal.run_id,request.capture]));
            let mut ledger=load(&store)?;
            if ledger.requests.get(&request.request_id).is_some_and(|old|old!=&capture_id) {return Err(failure("A managed request ID was reused for a different capture."));}
            let mut record=ledger.captures.get(&capture_id).cloned().unwrap_or_else(||CaptureRecord{request:request.clone(),job_ids:vec![],current_job_id:None,project_state:"not-requested".into(),diagnostics:request.capture.diagnostics.clone()});
            record.request.create_project|=request.create_project;
            let job_id=if request.capture.input.is_none(){
                record.project_state=if request.capture.binary_state==ManagedBinaryState::VerifiedAbsent{"not-applicable"}else{"unavailable"}.into();None
            }else{match &engine{
                None=>None,
                Some(Err(error))=>{record.project_state="unavailable".into();if !record.diagnostics.contains(error){record.diagnostics.push(error.clone());}None},
                Some(Ok(engine))=>{
                    let id=ensure_job(&mut ledger,&request.capture,engine);
                    record.current_job_id=Some(id.clone());record.diagnostics=request.capture.diagnostics.clone();
                    if !record.job_ids.contains(&id){record.job_ids.push(id.clone());record.job_ids.sort();}
                    record.project_state=ledger.jobs[&id].state.clone();Some(id)
                }
            }};
            ledger.requests.insert(request.request_id.clone(),capture_id.clone());ledger.captures.insert(capture_id.clone(),record.clone());
            let owner_id=&request.capture.owner.keys["LibraryID"];
            // A failed/unavailable owner observation supersedes prior current
            // rows without deleting any retained capture or project history.
            if request.capture.selector.is_none(){ledger.current_snapshots.insert(owner_id.clone(),CurrentOwnerSnapshot{expected_captures:1,capture_ids:vec![]});}
            if let Some(snapshot)=ledger.current_snapshots.get_mut(owner_id){if !snapshot.capture_ids.contains(&capture_id){snapshot.capture_ids.push(capture_id.clone());}}
            let owned=store.journal.artifact_acquisitions.entry(storage::identity_key(&request.capture.owner)).or_default();
            for file in request.capture.input.iter().chain(request.capture.symbols.iter()) {if !owned.contains(file){owned.push(file.clone());}}
            // Durable enrollment precedes acknowledgement and any discovery checkpoint.
            if store.journal.managed_processing.is_none(){store.journal.managed_processing=Some(ManagedRunMarker{protocol_version:1,ledger:STATE.into()});store.checkpoint()?;}
            save(&store,&ledger)?;store.checkpoint()?;qualification_boundary("enrollment-before-ack")?;
            ManagedDecompilationReceipt{capture_id,job_id,project_state:record.project_state}
        };
        if let (Some(id),Some(Ok(engine)))=(&receipt.job_id,engine) {self.schedule(run.clone(),engine,id.clone()).await?;}
        Ok(receipt)
    }
    async fn schedule(&self,run:Arc<Run>,engine:Engine,id:String)->Result<(),Diagnostic> {
        let run_id=run.storage.lock().map_err(|_|failure("Run storage is unavailable."))?.journal.run_id.clone();
        let key=(run_id,id.clone());let mut handles=self.handles.lock().await;
        if handles.contains_key(&key){return Ok(());}
        let should_run={let store=run.storage.lock().map_err(|_|failure("Run storage is unavailable."))?;let ledger=load(&store)?;ledger.jobs.get(&id).is_some_and(|job|job.state=="queued")};
        if should_run {
            let workers=self.permits(&run).await?;
            run.activity.managed_job(&id,"queued");
            handles.insert(key,tokio::spawn(async move {
                let queued=std::time::Instant::now();
                let result=async {
                let _permit=tokio::select!{_ = run.cancel.cancelled()=>return Err(issue("MANAGED_JOB_CANCELLED","Local work was cancelled before admission.")),permit=workers.acquire_owned()=>permit.map_err(|_|failure("The local worker queue closed."))?};
                while run.paused.load(Ordering::SeqCst){tokio::select!{_ = run.cancel.cancelled()=>return Err(issue("MANAGED_JOB_CANCELLED","Local work was cancelled while paused.")),_ = run.wake.notified()=>{}}}
                run.activity.record(json!({"event":"decompilerAdmitted","job":id,"queueMs":queued.elapsed().as_millis()}));
                run.activity.managed_job(&id,"running");
                let identity={let store=run.storage.lock().map_err(|_|lock_error())?;local::project_identity(&load(&store)?,&id)?};
                let _activity=run.activity.begin(&identity.category,Some(identity.clone()),"Creating a local decompiled C# project");
                execute_job(&run,&engine,&id).await
                }.await;
                if let Err(error)=&result {let store=run.storage.lock().map_err(|_|failure("Run storage is unavailable."))?;let mut ledger=load(&store)?;if let Some(job)=ledger.jobs.get_mut(&id){job.state=if error.code=="MANAGED_WORKER_RECONCILIATION_REQUIRED"{"recovery-required"}else if run.cancel.is_cancelled(){"cancelled"}else{"failed"}.into();job.diagnostics.push(error.clone());}save(&store,&ledger)?;}
                run.activity.managed_job(&id,if result.is_ok(){"finished"}else{"failed"});
                result
            }));
        }Ok(())
    }
    pub(super) async fn drain(&self,run:&Arc<Run>)->Vec<Diagnostic> {
        let id=match run.storage.lock(){Ok(s)=>s.journal.run_id.clone(),Err(_)=>return vec![failure("Run storage is unavailable.")]};
        let mut handles=self.handles.lock().await;let keys=handles.keys().filter(|(run,_)|run==&id).cloned().collect::<Vec<_>>();let pending=keys.into_iter().filter_map(|k|handles.remove(&k)).collect::<Vec<_>>();drop(handles);
        let mut errors=vec![];for task in pending {match task.await {Ok(Ok(()))=>{},Ok(Err(error))=>errors.push(error),Err(_)=>errors.push(failure("A local worker task stopped unexpectedly."))}}
        errors
    }
    pub(super) async fn resume(&self,plugin_root:&Path,run:&Arc<Run>)->Result<(),Diagnostic>{
        let needed={let store=run.storage.lock().map_err(|_|lock_error())?;let ledger=load(&store)?;!ledger.jobs.is_empty() || !ledger.local_enrollments.is_empty() || ledger.captures.values().any(|record|record.request.create_project&&record.request.capture.input.is_some())};
        if !needed{return Ok(());}
        let engine=self.engine(plugin_root,run).await;
        let pending={let store=run.storage.lock().map_err(|_|lock_error())?;let mut ledger=load(&store)?;
            if let Ok(engine)=&engine{
                let ids=ledger.captures.keys().cloned().collect::<Vec<_>>();
                for capture_id in ids{let mut record=ledger.captures[&capture_id].clone();if !record.request.create_project || record.request.capture.input.is_none(){continue;}
                    let verified=store.verify(record.request.capture.input.as_ref().expect("captured input")).and_then(|_|record.request.capture.symbols.as_ref().map_or(Ok(()),|file|store.verify(file)));
                    if let Err(error)=verified{let diagnostic=issue("MANAGED_INPUT_UNAVAILABLE",format!("An original compiled input is missing or changed: {} Other jobs can continue; restore the exact original before retry.",error.message));record.project_state="unavailable".into();record.diagnostics=record.request.capture.diagnostics.clone();record.diagnostics.push(diagnostic.clone());if let Some(job)=record.current_job_id.as_ref().and_then(|id|ledger.jobs.get_mut(id)){job.state="recovery-required".into();if !job.diagnostics.contains(&diagnostic){job.diagnostics.push(diagnostic);}}ledger.captures.insert(capture_id,record);continue;}
                    let id=ensure_job(&mut ledger,&record.request.capture,engine);record.current_job_id=Some(id.clone());if !record.job_ids.contains(&id){record.job_ids.push(id.clone());record.job_ids.sort();}record.project_state=ledger.jobs[&id].state.clone();record.diagnostics=record.request.capture.diagnostics.clone();ledger.captures.insert(capture_id,record);
                }local::resume_enrollments(&store,&mut ledger,engine)?;save(&store,&ledger)?;store.checkpoint()?;
            }
            ledger.jobs.values().cloned().collect::<Vec<_>>()};
        let mut ids=vec![];
        for job in pending{
            if job.state=="queued"{if engine.as_ref().is_ok_and(|engine|job.engine_sha256==engine.manifest.sha256){ids.push(job.id.clone());}continue;}
            if matches!(job.state.as_str(),"running"|"committing"|"recovery-required"){
                let reconciled=(||{
                    let (root,_admission,_worker)=job_leases(run,&job.id)?;
                    // Invalidate a delayed old process before examining its output.
                    storage::atomic_write(&root.join("generation.txt"),uuid::Uuid::new_v4().to_string().as_bytes())?;
                    let engine=engine.as_ref().map_err(Clone::clone)?;
                    if job.engine_sha256!=engine.manifest.sha256{return Err(failure("Interrupted output requires its exact enrolled companion to reconcile."));}
                    let attempt=job.attempt.as_ref().ok_or_else(||failure("The interrupted job has no admitted attempt."))?;
                    let path={let store=run.storage.lock().map_err(|_|lock_error())?;store.verify(&job.input)?;if let Some(file)=&job.symbols{store.verify(file)?;}store.confined(attempt)?};
                    commit_result(run,engine,&job.id,&path,&path.join("output"),&job)
                })();
                if let Err(error)=reconciled{let store=run.storage.lock().map_err(|_|lock_error())?;let mut ledger=load(&store)?;let saved=ledger.jobs.get_mut(&job.id).expect("saved interrupted job");saved.state="recovery-required".into();if !saved.diagnostics.contains(&error){saved.diagnostics.push(error);}save(&store,&ledger)?;}
            }else if matches!(job.state.as_str(),"complete"|"partial"){
                let store=run.storage.lock().map_err(|_|lock_error())?;
                if job.files.iter().any(|file|store.verify(file).is_err()){
                    let mut ledger=load(&store)?;let saved=ledger.jobs.get_mut(&job.id).expect("saved completed job");saved.state="recovery-required".into();saved.diagnostics.push(issue("MANAGED_PROJECT_CHANGED","A completed project member is missing or edited. Originals and user files are preserved; explicit retry will publish a coherent new candidate."));save(&store,&ledger)?;
                }
            }
        }
        if !ids.is_empty(){let engine=engine?;for id in ids{self.schedule(run.clone(),engine.clone(),id).await?;}}
        Ok(())
    }

}

fn lock_error()->Diagnostic {failure("Run storage is unavailable.")}
fn project_identity(id:&str)->Identity {Identity{category:"C08".into(),kind:"ManagedProject".into(),scope:Scope::Global,keys:BTreeMap::from([("Project".into(),id.into())])}}
fn job_leases(run:&Arc<Run>,id:&str)->Result<(PathBuf,fs::File,fs::File),Diagnostic>{
    use fs2::FileExt;
    let store=run.storage.lock().map_err(|_|lock_error())?;
    let root=store.confined(&format!("Herbert Data/managed-decompilation/v1/jobs/{id}"))?;
    fs::create_dir_all(&root).map_err(|_|failure("Cannot create the owned job lease root."))?;
    let open=|name:&str|{let path=store.confined(&format!("Herbert Data/managed-decompilation/v1/jobs/{id}/{name}"))?;fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).map_err(|_|failure("Cannot open an owned job lease."))};
    let admission=open("admission.lock")?;admission.try_lock_exclusive().map_err(|_|issue("MANAGED_WORKER_RECONCILIATION_REQUIRED","Another host still owns this job admission lease; no duplicate worker was started."))?;
    let worker=open("worker.lock")?;worker.try_lock_exclusive().map_err(|_|issue("MANAGED_WORKER_RECONCILIATION_REQUIRED","The previous worker still owns this exact job token. Its private attempt is preserved; retry recovery after it exits."))?;
    Ok((root,admission,worker))
}
async fn execute_job(run:&Arc<Run>,engine:&Engine,id:&str)->Result<(),Diagnostic> {
    engine.verify()?;
    let (lease_root,_admission,_worker)=job_leases(run,id)?;
    let generation=uuid::Uuid::new_v4().to_string();
    storage::atomic_write(&lease_root.join("generation.txt"),generation.as_bytes())?;
    drop(_worker);
    let (attempt,request_path,out,job)={let store=run.storage.lock().map_err(|_|lock_error())?;let mut ledger=load(&store)?;let job=ledger.jobs.get_mut(id).ok_or_else(||failure("The queued job is absent."))?;
        if job.engine_sha256!=engine.manifest.sha256 {return Err(failure("Resume requires the exact enrolled companion bytes."));}
        store.verify(&job.input)?;if let Some(symbols)=&job.symbols{store.verify(symbols)?;}
        let attempt=format!("Herbert Data/managed-decompilation/v1/staging/{id}/{}",uuid::Uuid::new_v4());let path=store.confined(&attempt)?;fs::create_dir_all(path.join("output")).map_err(|_|failure("Cannot create an owned worker attempt."))?;
        let input=json!({"path":store.confined(&job.input.path)?,"bytes":job.input.bytes,"sha256":job.input.sha256});
        let symbols=job.symbols.as_ref().map(|f|Ok::<Value,Diagnostic>(json!({"path":store.confined(&f.path)?,"bytes":f.bytes,"sha256":f.sha256}))).transpose()?;
        let request_path=path.join("request.json");storage::atomic_write(&request_path,&serde_json::to_vec(&json!({"protocolVersion":1,"jobId":id,"engineId":MANAGED_ENGINE_ID,"input":input,"symbols":symbols,"symbolMode":job.symbol_mode,"outputDirectory":path.join("output"),"controlChannel":true,"lease":{"root":lease_root,"generation":generation}})).map_err(|_|failure("Cannot serialize the worker request."))?)?;
        if let Some(previous)=job.attempt.as_ref(){job.attempt_history.push(json!({"attempt":previous,"state":job.state,"diagnostics":job.diagnostics}));}
        job.state="running".into();job.attempt=Some(attempt.clone());let saved=job.clone();save(&store,&ledger)?;(path.clone(),request_path,path.join("output"),saved)};
    qualification_boundary("admitted-before-spawn")?;
    crate::decompiler::execute(engine,&request_path,&attempt,&run.cancel,&run.activity).await?;
    run.activity.managed_job(id,"committing");
    let category={let store=run.storage.lock().map_err(|_|lock_error())?;local::project_identity(&load(&store)?,id)?.category};
    let _commit=run.activity.work(&category,"decompilerCommit");
    commit_result(run,engine,id,&attempt,&out,&job)
}

fn commit_result(run:&Arc<Run>,engine:&Engine,id:&str,_attempt:&Path,out:&Path,job:&Job)->Result<(),Diagnostic>{
    let result=crate::decompiler::validate_result(out,id,&job.input.sha256,job.symbols.as_ref().map(|s|s.sha256.as_str()))?;
    let bytes=serde_json::to_vec(&result).map_err(|_|failure("Cannot encode verified output."))?;
    let store=run.storage.lock().map_err(|_|lock_error())?;let mut ledger=load(&store)?;let previous=ledger.jobs.get(id).ok_or_else(||failure("The job disappeared before commit."))?.clone();
    ledger.jobs.get_mut(id).expect("validated job").state="committing".into();save(&store,&ledger)?;qualification_boundary("committing-before-files")?;
    let identity=local::project_identity(&ledger,id)?;
    let (owner,folder)=local::output_binding(&ledger,id)?;
    let owner_folder=store.artifact_folder(&owner)?;
    let folder=folder.as_str();
    storage::safe_relative(folder)?;
    let base=store.artifact_folder_at(&identity,&format!("{owner_folder}/{folder}"))?;
    let previous_root=previous.actual_root.as_deref().unwrap_or(&base);
    let expected_names=result.files.iter().map(|f|f.path.as_str()).chain(std::iter::once("result.json")).collect::<BTreeSet<_>>();
    let existing=store.confined(previous_root)?;
    let unknown=if existing.exists(){fs::read_dir(&existing).map_err(|_|failure("Cannot inspect the existing project tree."))?.any(|entry|entry.is_err()||entry.is_ok_and(|entry|!expected_names.contains(entry.file_name().to_string_lossy().as_ref())))}else{false};
    let edited=unknown||previous.files.iter().any(|file|store.verify(file).is_err());
    let mut prefix=if edited{format!("Candidates/{}/",uuid::Uuid::new_v4())}else{previous_root.strip_prefix(&format!("{base}/")).map(|p|format!("{p}/")).unwrap_or_default()};
    let mut conflict=edited||previous.user_conflict;
    let mut committed=vec![];let mut actual_root=String::new();let mut coherent=false;
    // One nominal generation plus one fresh grouped candidate is the commit
    // protocol, not a supported workload limit. A conflict in that fresh root
    // leaves staging recoverable rather than chasing concurrent edits forever.
    for pass in 0..2{
        committed.clear();let mut redirected=false;
        for file in &result.files{let mut input=fs::File::open(out.join(&file.path)).map_err(|_|failure("Cannot read the validated candidate."))?;qualification_boundary("commit-member-write")?;let(receipt,changed)=store.write_derived_stream(&identity,&format!("{prefix}{}",file.path),&mut input)?;qualification_boundary("after-commit-member")?;redirected|=changed;committed.push(receipt);}
        redirected|=committed.iter().any(|file|store.verify(file).is_err());
        if !redirected{
            actual_root=format!("{base}/{}",prefix.trim_end_matches('/')).trim_end_matches('/').to_owned();
            qualification_boundary("before-project-manifest")?;
            let mut manifest:Value=serde_json::from_slice(&bytes).map_err(|_|failure("Cannot read the validated result."))?;
            manifest["actualRoot"]=json!(actual_root);manifest["userConflict"]=json!(conflict);manifest["preservedUserRoot"]=json!(if conflict{Some(previous_root)}else{None});manifest["engineSha256"]=json!(engine.manifest.sha256);manifest["committedFiles"]=json!(committed);
            let(receipt,changed)=store.write_derived(&identity,&format!("{prefix}result.json"),&serde_json::to_vec_pretty(&manifest).map_err(|_|failure("Cannot encode the final project manifest."))?)?;
            redirected|=changed;committed.push(receipt);redirected|=committed.iter().any(|file|store.verify(file).is_err());qualification_boundary("after-project-manifest")?;
        }
        if !redirected{coherent=true;break;}
        if pass==0{conflict=true;prefix=format!("Candidates/{}/",uuid::Uuid::new_v4());}
    }
    if !coherent{return Err(failure("The fresh grouped project candidate conflicted. Verified staging and actual-path receipts remain preserved; no candidate was enrolled as complete."));}
    let saved=ledger.jobs.get_mut(id).ok_or_else(||failure("The job disappeared during commit."))?;saved.state=result.outcome.clone();saved.files=committed;saved.actual_root=Some(actual_root);saved.user_conflict=conflict;saved.elapsed_milliseconds=Some(result.elapsed_milliseconds);
    saved.diagnostics.retain(|d| d.code != "MANAGED_PROJECT_PARTIAL" && d.code != "MANAGED_PROJECT_GENERATION_FAILED");
    saved.diagnostics.extend(project_failures(&result.coverage));
    save(&store,&ledger)?;store.checkpoint()?;qualification_boundary("before-owner-links")?;
    // Candidate retirement is deliberately separate until restart/commit fault
    // qualification establishes every retained-attempt recovery boundary (W5).
    Ok(())
}


// Missing references, unavailable external resources and fidelity notes describe
// reconstruction limits, not collection failures. Keep them in result.json.
fn project_failures(coverage: &[Value]) -> Vec<Diagnostic> {
    let failed = coverage.iter().filter(|item| item["outcome"] == "failed").count();
    if failed == 0 { vec![] } else {
        vec![issue("MANAGED_PROJECT_GENERATION_FAILED", format!("Local reconstruction failed for {failed} type/resource entries. Saved output and details remain in result.json."))]
    }
}
fn job_run_diagnostics(store: &Storage, job: &Job) -> Vec<Diagnostic> {
    if !job.diagnostics.iter().any(|d| d.code == "MANAGED_PROJECT_PARTIAL") {
        return job.diagnostics.clone();
    }
    // Reclassify older jobs only from their hash-verified result, retaining all
    // unrelated failures and the original detailed result manifest.
    let result = job.files.iter().find(|f| f.path.ends_with("/result.json"))
        .and_then(|file| { store.verify(file).ok()?; let bytes=fs::read(store.confined(&file.path).ok()?).ok()?; serde_json::from_slice::<Value>(&bytes).ok() });
    let Some(coverage) = result.as_ref().and_then(|r|r["coverage"].as_array()) else { return job.diagnostics.clone(); };
    let mut diagnostics=job.diagnostics.iter().filter(|d| d.code != "MANAGED_PROJECT_PARTIAL").cloned().collect::<Vec<_>>();
    for diagnostic in project_failures(coverage) { if !diagnostics.contains(&diagnostic) { diagnostics.push(diagnostic); } }
    diagnostics
}


#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct ReadReceipt {library_id:String,selector:ManagedStoreSelector,sys_rev_id:Value,definition_revision:Value,input:FileRef,symbols:Option<FileRef>,observation:Value}
fn capture_read(record:&CaptureRecord)->Option<ReadReceipt>{
    let capture=&record.request.capture;if capture.binary_state!=ManagedBinaryState::Captured{return None;}
    Some(ReadReceipt{library_id:capture.owner.keys.get("LibraryID")?.clone(),selector:capture.selector.clone()?,sys_rev_id:capture.observation["SysRevID"].clone(),definition_revision:capture.definition_revision.clone(),input:capture.input.clone()?,symbols:capture.symbols.clone(),observation:capture.observation.clone()})
}
pub(super) fn known_captures(store:&Storage,arguments:&Value)->Result<Value,Diagnostic>{
    let ids:Vec<String>=serde_json::from_str(arguments["iLibraryIDs"].as_str().ok_or_else(||failure("The compiled owner selection is invalid."))?).map_err(|_|failure("The compiled owner selection is invalid."))?;
    let ledger=load(store)?;let mut seen=BTreeSet::new();let mut known=vec![];
    let candidates=ledger.captures.iter().filter_map(|(id,record)|capture_read(record).map(|read|(id.clone(),read))).chain(ledger.reads.iter().map(|(id,read)|(id.clone(),read.clone())));
    for(id,read)in candidates{
        if !ids.contains(&read.library_id)||store.verify(&read.input).is_err()||read.symbols.as_ref().is_some_and(|file|store.verify(file).is_err())||!read.sys_rev_id.is_string()||!seen.insert((read.library_id.clone(),read.selector.folder.clone(),read.selector.assembly.clone(),read.selector.version.clone(),read.sys_rev_id.to_string(),read.definition_revision.to_string())){continue;}
        known.push(json!({"captureId":id,"libraryId":read.library_id,"folder":read.selector.folder,"assembly":read.selector.assembly,"version":read.selector.version,"sysRevID":read.sys_rev_id,"definitionRevision":read.definition_revision}));
    }Ok(json!(known))
}
pub(super) fn validate_reader_scope(store:&Storage,value:&Value)->Result<(),Diagnostic>{
    if value["oSuccess"]!=true{return Ok(());}
    let scope=value.pointer("/oDS/Scope").and_then(Value::as_array).filter(|rows|rows.len()==1).ok_or_else(||failure("The compiled reader did not attest its native caller/company."))?;
    if scope[0]["Caller"]!=store.journal.caller || scope[0]["Company"]!=store.journal.company{return Err(issue("MANAGED_INPUT_OWNERSHIP_DENIED","The reader ran under another native caller or company; no files were enrolled."));}Ok(())
}
pub(super) fn restore_retained_rows(store:&Storage,value:&mut Value,issued:&mut Vec<FileRef>)->Result<(),Diagnostic>{
    if value["oSuccess"]!=true{return Ok(());}validate_reader_scope(store,value)?;let ledger=load(store)?;
    for row in value.pointer_mut("/oDS/Assemblies").and_then(Value::as_array_mut).into_iter().flatten(){
        let reused=row["ReusedCaptureId"].as_str().unwrap_or("").to_owned();
        if !reused.is_empty(){
            let saved=ledger.reads.get(&reused).cloned().or_else(||ledger.captures.get(&reused).and_then(capture_read)).ok_or_else(||failure("The native reader referenced unknown retained evidence."))?;
            if row["LibraryID"]!=saved.library_id || row["Folder"]!=saved.selector.folder || row["Assembly"]!=saved.selector.assembly || row["Version"]!=saved.selector.version || row["SysRevID"]!=saved.sys_rev_id || row["DefinitionRevision"]!=saved.definition_revision{return Err(failure("The retained-byte selector/revision differs from this native observation."));}
            for(name,file)in [("Data",Some(&saved.input)),("Symbols",saved.symbols.as_ref())]{if let Some(file)=file{store.verify(file)?;row[name]=json!({"nativeFile":file,"representation":"decoded native bytes"});if !issued.contains(file){issued.push(file.clone());}}else{row[name]=Value::Null;}}
            for name in ["SymbolDisposition","ModuleVersionId","AssemblyFullName"]{row[name]=saved.observation[name].clone();}
        }
        row.as_object_mut().ok_or_else(||failure("A compiled row is invalid."))?.remove("ReusedCaptureId");
    }Ok(())
}
pub(super) fn record_retained_read(store:&mut Storage,value:&Value,arguments:&Value,issued:&[FileRef])->Result<(),Diagnostic>{
    if value["oSuccess"]!=true{return Ok(());}validate_reader_scope(store,value)?;
    let ids:Vec<String>=serde_json::from_str(arguments["iLibraryIDs"].as_str().ok_or_else(||failure("The compiled owner selection is invalid."))?).map_err(|_|failure("The compiled owner selection is invalid."))?;
    let mut ledger=load(store)?;
    // The complete native owner batch starts a new current observation. Keep
    // prior receipts/jobs as history; enrollment acknowledgement fills this set.
    for id in &ids {let count=value.pointer("/oDS/Assemblies").and_then(Value::as_array).into_iter().flatten().filter(|row|row["LibraryID"]==*id).count();ledger.current_snapshots.insert(id.clone(),CurrentOwnerSnapshot{expected_captures:count.max(1),capture_ids:vec![]});}
    for row in value.pointer("/oDS/Assemblies").and_then(Value::as_array).into_iter().flatten(){
        let library_id=row["LibraryID"].as_str().ok_or_else(||failure("A native compiled row has no owner."))?.to_owned();
        let selector:ManagedStoreSelector=serde_json::from_value(json!({"folder":row["Folder"],"assembly":row["Assembly"],"version":row["Version"]})).map_err(|_|failure("A native compiled selector is invalid."))?;
        if !ids.contains(&library_id)||selector.folder!=":Efx"||selector.assembly!=format!("efx.{}",library_id.to_lowercase()){return Err(issue("MANAGED_INPUT_OWNERSHIP_DENIED","A compiled read returned a selector outside its exact owner selection."));}
        if row["SysRevID"]!=row["BinarySysRevID"]||row["SysRevID"]!=row["AfterSysRevID"]||row["DefinitionRevision"]!=row["DefinitionRevisionAfter"]{continue;}
        let Ok(input)=serde_json::from_value::<FileRef>(row["Data"]["nativeFile"].clone())else{continue;};let symbols=if row["Symbols"].is_null(){None}else{Some(serde_json::from_value::<FileRef>(row["Symbols"]["nativeFile"].clone()).map_err(|_|failure("A symbol receipt is invalid."))?)};
        for file in std::iter::once(&input).chain(symbols.iter()){if file.role!="native"||!issued.contains(file){return Err(issue("MANAGED_INPUT_OWNERSHIP_DENIED","A cached native read has no issued immutable receipt."));}store.verify(file)?;}
        let mut observation=row.clone();observation.as_object_mut().expect("native row").remove("Data");observation.as_object_mut().expect("native row").remove("Symbols");
        let receipt=ReadReceipt{library_id,selector,sys_rev_id:row["SysRevID"].clone(),definition_revision:row["DefinitionRevision"].clone(),input,symbols,observation};let id=format!("read:{}",key(&serde_json::to_value(&receipt).map_err(|_|failure("Cannot encode the native-read receipt."))?));ledger.reads.insert(id,receipt);
    }
    // Durable native receipt precedes the guest response and job enrollment.
    // An interrupted page can revalidate metadata without fetching equal bytes.
    if store.journal.managed_processing.is_none(){store.journal.managed_processing=Some(ManagedRunMarker{protocol_version:1,ledger:STATE.into()});store.checkpoint()?;}save(store,&ledger)?;store.checkpoint()?;qualification_boundary("after-native-receipt")
}

pub(super) fn native_retry_required(artifact:&Artifact)->bool{
    artifact.diagnostics.iter().any(|diagnostic|matches!(diagnostic.code.as_str(),"MANAGED_SOURCE_CHANGING"|"MANAGED_INPUT_UNAVAILABLE"|"MANAGED_BINARY_UNAVAILABLE"|"MANAGED_CAPTURE_UNAVAILABLE"|"MANAGED_OWNER_UNVERIFIED"|"MANAGED_HOST_UNAVAILABLE"|"MANAGED_CAPTURE_ENROLLMENT_PENDING"))
}
pub(super) fn retry_failed(store:&mut Storage)->Result<(),Diagnostic>{
    if store.journal.managed_processing.is_none(){return Ok(());}
    let mut ledger=load(store)?;
    for job in ledger.jobs.values_mut(){if matches!(job.state.as_str(),"failed"|"cancelled"|"recovery-required"){
        let verified=store.verify(&job.input).and_then(|_|job.symbols.as_ref().map_or(Ok(()),|file|store.verify(file)));
        if let Err(error)=verified{job.state="recovery-required".into();let diagnostic=issue("MANAGED_INPUT_UNAVAILABLE",format!("This job's original input is missing or changed: {} Other retryable jobs continue.",error.message));if !job.diagnostics.contains(&diagnostic){job.diagnostics.push(diagnostic);}continue;}
        job.attempt_history.push(json!({"explicitRetry":true,"state":job.state,"attempt":job.attempt,"diagnostics":job.diagnostics}));
        job.state="queued".into();job.diagnostics.clear();
    }}
    save(store,&ledger)?;store.checkpoint()
}



#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct RetiredLeaf {path:String,bytes:u64,sha256:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct AttemptRetirement {attempt:String,leaves:Vec<RetiredLeaf>,receipt:Value}
fn retirement_intent(store:&Storage,job:&Job,relative:&str)->Result<AttemptRetirement,Diagnostic>{
    let prefix=format!("Herbert Data/managed-decompilation/v1/staging/{}/",job.id);if !relative.strip_prefix(&prefix).is_some_and(|leaf|uuid::Uuid::parse_str(leaf).is_ok()){return Err(failure("An attempt retirement path has no exact job/generation ownership."));}
    let attempt=store.confined(relative)?;let out=attempt.join("output");let entries=fs::read_dir(&attempt).map_err(|_|failure("Cannot inspect the completed worker attempt."))?.collect::<Result<Vec<_>,_>>().map_err(|_|failure("Cannot inspect the completed worker attempt."))?;
    if entries.iter().any(|entry|!matches!(entry.file_name().to_str(),Some("request.json"|"stdout.txt"|"stderr.txt"|"output"))||entry.file_type().is_err()||entry.file_type().is_ok_and(|kind|kind.is_symlink())){return Err(failure("The completed attempt contains unknown material; it was preserved."));}
    let bytes=fs::read(out.join("result.json")).map_err(|_|failure("The completed attempt has no readable result; it was preserved."))?;let result:GeneratedResult=serde_json::from_slice(&bytes).map_err(|_|failure("The completed attempt result changed; it was preserved."))?;
    if result.job_id!=job.id||result.input_sha256!=job.input.sha256||result.engine_id!=MANAGED_ENGINE_ID{return Err(failure("The completed attempt identity changed; it was preserved."));}
    let mut names=BTreeSet::from(["result.json".to_owned()]);let mut leaves=vec![RetiredLeaf{path:"output/result.json".into(),bytes:bytes.len()as u64,sha256:storage::sha256(&bytes)}];
    for file in &result.files{if storage::safe_relative(&file.path)?.components().count()!=1||!names.insert(file.path.clone()){return Err(failure("The completed attempt membership is invalid; it was preserved."));}let path=out.join(&file.path);let metadata=fs::symlink_metadata(&path).map_err(|_|failure("A completed staging member changed; it was preserved."))?;if !metadata.is_file()||metadata.file_type().is_symlink()||metadata.len()!=file.bytes||storage::sha256(&fs::read(&path).map_err(|_|failure("Cannot verify a completed staging member."))?)!=file.sha256{return Err(failure("A completed staging member changed; it was preserved."));}leaves.push(RetiredLeaf{path:format!("output/{}",file.path),bytes:file.bytes,sha256:file.sha256.clone()});}
    let actual=fs::read_dir(&out).map_err(|_|failure("Cannot inspect completed staging membership."))?.map(|entry|entry.map(|entry|entry.file_name().to_string_lossy().into_owned())).collect::<Result<BTreeSet<_>,_>>().map_err(|_|failure("Cannot inspect completed staging membership."))?;if actual!=names{return Err(failure("Completed staging contains unknown files; they were preserved."));}
    let mut logs=BTreeMap::new();for name in ["request.json","stdout.txt","stderr.txt"]{let path=attempt.join(name);if path.exists(){let metadata=fs::symlink_metadata(&path).map_err(|_|failure("Cannot inspect a completed attempt log."))?;if !metadata.is_file()||metadata.file_type().is_symlink(){return Err(failure("A completed attempt log changed type; it was preserved."));}let data=fs::read(&path).map_err(|_|failure("Cannot read a completed attempt log."))?;if name!="request.json"&&!data.is_empty(){logs.insert(name.to_owned(),String::from_utf8_lossy(&data).into_owned());}leaves.push(RetiredLeaf{path:name.into(),bytes:data.len()as u64,sha256:storage::sha256(&data)});}}
    Ok(AttemptRetirement{attempt:relative.into(),leaves,receipt:json!({"attempt":relative,"state":"committed-staging-retired","resultSha256":storage::sha256(&bytes),"files":names.len(),"logs":logs})})
}
fn retire_committed_attempts(store:&Storage,ledger:&mut Ledger)->Result<(),Diagnostic>{
    let ids=ledger.jobs.keys().cloned().collect::<Vec<_>>();
    for id in ids{let job=ledger.jobs[&id].clone();if !matches!(job.state.as_str(),"complete"|"partial")||job.files.is_empty()||job.files.iter().any(|file|store.verify(file).is_err()){continue;}let Some(relative)=job.attempt.clone()else{continue;};
        let intent=match job.retirement.clone(){Some(intent)=>Ok(intent),None=>retirement_intent(store,&job,&relative)};
        let result=(||->Result<(),Diagnostic>{let intent=intent?;if intent.attempt!=relative{return Err(failure("The retirement intent has another attempt owner."));}
            ledger.jobs.get_mut(&id).expect("saved job").retirement=Some(intent.clone());save(store,ledger)?;qualification_boundary("retirement-after-intent")?;
            for leaf in &intent.leaves{let safe=storage::safe_relative(&leaf.path)?;if !(safe.components().count()==1||safe.components().count()==2&&leaf.path.starts_with("output/")){return Err(failure("The retirement intent has an unsafe leaf."));}let path=store.confined(&format!("{relative}/{}",leaf.path))?;match fs::symlink_metadata(&path){Err(error)if error.kind()==std::io::ErrorKind::NotFound=>continue,Err(_)=>return Err(failure("Cannot inspect a retirement leaf.")),Ok(metadata)=>{if !metadata.is_file()||metadata.file_type().is_symlink()||metadata.len()!=leaf.bytes||storage::sha256(&fs::read(&path).map_err(|_|failure("Cannot verify a retirement leaf."))?)!=leaf.sha256{return Err(failure("A completed staging leaf changed after the retirement intent; it was preserved."));}fs::remove_file(path).map_err(|_|failure("Completed staging retirement is incomplete; remaining files were preserved."))?;qualification_boundary("retirement-after-leaf")?;}}}
            let attempt=store.confined(&relative)?;let out=attempt.join("output");if out.exists(){fs::remove_dir(&out).map_err(|_|failure("Completed staging gained unknown files; they were preserved."))?;}if attempt.exists(){fs::remove_dir(&attempt).map_err(|_|failure("Completed staging gained unknown material; it was preserved."))?;}
            let saved=ledger.jobs.get_mut(&id).expect("saved job");saved.attempt_history.push(intent.receipt);saved.attempt=None;saved.retirement=None;save(store,ledger)
        })();
        if let Err(error)=result{let diagnostic=issue("MANAGED_STAGING_RETAINED",error.message);let saved=ledger.jobs.get_mut(&id).expect("saved job");if !saved.diagnostics.contains(&diagnostic){saved.diagnostics.push(diagnostic);}save(store,ledger)?;}
    }Ok(())
}


// Project execution is shared by content, but each caller owns its presentation.
fn owner_project(store:&mut Storage,owner:&Identity,job:&Job,directory:&str)->Result<Vec<FileRef>,Diagnostic>{
 let owner_folder=store.artifact_folder(owner)?;
 storage::safe_relative(directory)?;
 let requested_root=format!("{owner_folder}/{directory}");
 if job.actual_root.as_ref().is_some_and(|root|root==&requested_root||root.strip_prefix(&requested_root).is_some_and(|suffix|suffix.starts_with("/Candidates/"))){return Ok(job.files.clone());}
 let mut placement_keys=BTreeMap::from([("Project".into(),job.id.clone()),("Owner".into(),storage::identity_key(owner))]);
 // Preserve the established default Function placement identity. Other caller
 // directories are distinct placements, including multiple copies for one owner.
 if directory!="Decompiled"{placement_keys.insert("Placement".into(),directory.into());}
 let identity=Identity{category:owner.category.clone(),kind:"ManagedProjectPlacement".into(),scope:owner.scope.clone(),keys:placement_keys};
 storage::safe_relative(directory)?;let base=store.artifact_folder_at(&identity,&format!("{owner_folder}/{directory}"))?;
 let previous=store.manifest.artifacts.get(&storage::identity_key(owner)).map(|a|a.files.iter().filter(|f|f.path.starts_with(&format!("{base}/"))).cloned().collect::<Vec<_>>()).unwrap_or_default();
 let destination_changed=|prefix:&str|->Result<bool,Diagnostic>{
  for source in &job.files {let name=Path::new(&source.path).file_name().and_then(|n|n.to_str()).ok_or_else(||failure("Invalid generated member name."))?;let relative=format!("{base}/{prefix}{name}");let path=store.confined(&relative)?;if path.exists(){let expected=if name=="result.json"{previous.iter().find(|f|f.path==relative).cloned()}else{Some(FileRef{path:relative,sha256:source.sha256.clone(),bytes:source.bytes,role:"derived".into()})};if expected.as_ref().is_none_or(|f|store.verify(f).is_err()){return Ok(true);}}}Ok(false)
 };
 let edited=destination_changed("")?;
 let prefix=if edited{let stable=format!("Candidates/{}/",job.id);if destination_changed(&stable)?{format!("Candidates/{}/",uuid::Uuid::new_v4())}else{stable}}else{String::new()};
 let mut files=vec![];
 for source in job.files.iter().filter(|f|!f.path.ends_with("/result.json")){
  store.verify(source)?;let name=Path::new(&source.path).file_name().and_then(|n|n.to_str()).ok_or_else(||failure("Invalid generated member name."))?;
  let mut input=fs::File::open(store.confined(&source.path)?).map_err(|_|failure("Cannot read shared project output."))?;
  let(receipt,conflict)=store.write_derived_stream(&identity,&format!("{prefix}{name}"),&mut input)?;
  if conflict{return Err(failure("A project placement changed concurrently; existing files were preserved."));}files.push(receipt);
 }
 let source=job.files.iter().find(|f|f.path.ends_with("/result.json")).ok_or_else(||failure("Missing shared project manifest."))?;store.verify(source)?;
 let mut result:Value=serde_json::from_slice(&fs::read(store.confined(&source.path)?).map_err(|_|failure("Cannot read shared result."))?).map_err(|_|failure("Invalid shared result."))?;
 result["actualRoot"]=json!(format!("{base}/{}",prefix.trim_end_matches('/')).trim_end_matches('/'));result["committedFiles"]=json!(files);result["userConflict"]=json!(edited);
 let(receipt,conflict)=store.write_derived(&identity,&format!("{prefix}result.json"),&serde_json::to_vec_pretty(&result).map_err(|_|failure("Cannot encode placed result."))?)?;if conflict{return Err(failure("The project manifest changed concurrently; existing files were preserved."));}files.push(receipt);Ok(files)
}

pub(super) fn attach_outputs(store:&mut Storage)->Result<(),Diagnostic> {
    if store.journal.managed_processing.is_none(){return Ok(());}
    let mut ledger=load(store)?;let owners=ledger.captures.values().map(|c|(storage::identity_key(&c.request.capture.owner),c.request.capture.owner.clone())).collect::<BTreeMap<_,_>>();
    for (owner_key,owner) in owners {
        let directory=ledger.captures.values().find(|c|c.request.capture.owner==owner).and_then(|c|c.request.output_subdirectory.as_deref()).unwrap_or("Decompiled");
        let mut placements=BTreeMap::<String,Vec<FileRef>>::new();
        for job in ledger.captures.values().filter(|c|c.request.capture.owner==owner).flat_map(|c|c.job_ids.iter().filter_map(|id|ledger.jobs.get(id))).filter(|j|matches!(j.state.as_str(),"complete"|"partial")){if !placements.contains_key(&job.id){placements.insert(job.id.clone(),owner_project(store,&owner,job,directory)?);}}
        let captures=ledger.captures.iter().filter(|(_,c)|c.request.capture.owner==owner).map(|(id,c)|json!({"captureId":id,"capture":c.request.capture,"projectState":c.current_job_id.as_ref().and_then(|id|ledger.jobs.get(id)).map(|job|job.state.as_str()).unwrap_or(c.project_state.as_str()),"currentObservation":current_capture(&ledger,id,c),"currentJobId":c.current_job_id,"jobs":c.job_ids.iter().filter_map(|id|ledger.jobs.get(id)).collect::<Vec<_>>(),"diagnostics":c.diagnostics,"freshness":"unknown","sharedFilesExternal":true,"ownerProjectFiles":c.job_ids.iter().filter_map(|id|placements.get(id)).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let (file,conflict)=store.write_derived(&owner,"compiled.json",&serde_json::to_vec_pretty(&json!({"schemaVersion":1,"captures":captures,"originals":"Immutable native files referenced from the run-owned Herbert Data area; copy the whole download to retain shared files."})).map_err(|_|failure("Cannot encode compiled capture ownership."))?)?;
        let jobs=ledger.captures.values().filter(|c|c.request.capture.owner==owner).flat_map(|c|c.job_ids.iter().filter_map(|id|ledger.jobs.get(id))).collect::<Vec<_>>();
        let active=ledger.captures.iter().filter(|(id,c)|c.request.capture.owner==owner&&current_capture(&ledger,id,c)).map(|(_,c)|c).collect::<Vec<_>>();
        let active_jobs=active.iter().filter_map(|c|c.current_job_id.as_ref().and_then(|id|ledger.jobs.get(id))).collect::<Vec<_>>();
        let mut diagnostics=active.iter().flat_map(|c|c.diagnostics.clone()).collect::<Vec<_>>();diagnostics.extend(active_jobs.iter().flat_map(|j|job_run_diagnostics(store,j)));
        if ledger.current_snapshots.get(&owner.keys["LibraryID"]).is_some_and(|snapshot|snapshot.capture_ids.len()<snapshot.expected_captures){diagnostics.push(issue("MANAGED_CAPTURE_ENROLLMENT_PENDING","The latest native owner snapshot is durable but capture enrollment is incomplete. Retained history is preserved; recover this run to finish the current observation."));}
        for job in &active_jobs{if !matches!(job.state.as_str(),"complete"|"partial"){diagnostics.push(issue("MANAGED_PROJECT_INCOMPLETE",format!("Local project {} is {}. Original compiled files remain available.",job.id,job.state)));}}
        if conflict {diagnostics.push(issue("MANAGED_OWNER_MANIFEST_CONFLICT","Your edited compiled manifest was preserved; the new ownership view is in Conflicts."));}
        let nominal=format!("{}/compiled.json",store.artifact_folder(&owner)?);
        let mut project_prefixes=jobs.iter().map(|job|store.artifact_folder(&project_identity(&job.id)).map(|root|format!("{root}/"))).collect::<Result<Vec<_>,_>>()?;
        for files in placements.values(){if let Some(file)=files.first(){if let Some(parent)=Path::new(&file.path).parent().and_then(|p|p.to_str()){project_prefixes.push(format!("{parent}/"));}}}
        // Remove superseded receipts for the entire mirrored placement, including
        // its nominal tree when a user edit redirects the new view to Candidates.
        let owner_folder=store.artifact_folder(&owner)?;
        for job in &jobs {if placements.contains_key(&job.id) && !job.actual_root.as_ref().is_some_and(|root|root.starts_with(&format!("{owner_folder}/"))){
            let identity=Identity{category:owner.category.clone(),kind:"ManagedProjectPlacement".into(),scope:owner.scope.clone(),keys:BTreeMap::from([("Project".into(),job.id.clone()),("Owner".into(),storage::identity_key(&owner))])};
            project_prefixes.push(format!("{}/",store.artifact_folder(&identity)?));
        }}
        let current=placements.values().flatten().filter(|file|store.verify(file).is_ok()).cloned().collect::<Vec<_>>();
        if let Some(artifact)=store.manifest.artifacts.get_mut(&owner_key){
            let only_managed_partial=artifact.processing=="partial" && artifact.acquisition==Completeness::Collected && !artifact.diagnostics.is_empty() && artifact.diagnostics.iter().all(|diagnostic|diagnostic.code.starts_with("MANAGED_"));
            let baseline=if only_managed_partial{ledger.owner_processing.get(&owner_key).cloned().unwrap_or_else(||"derived".into())}else{let baseline=artifact.processing.clone();ledger.owner_processing.insert(owner_key.clone(),baseline.clone());baseline};
            artifact.diagnostics.retain(|d|!d.code.starts_with("MANAGED_"));artifact.processing=baseline;
            // A changed derived manifest replaces its old receipt. Retaining both
            // hashes for one mutable output path creates a false integrity failure.
            artifact.files.retain(|old|old.path!=nominal && old.path!=file.path && !project_prefixes.iter().any(|prefix|old.path.starts_with(prefix)));
            artifact.files.push(file);for file in current{if !artifact.files.contains(&file){artifact.files.push(file);}}
            if !diagnostics.is_empty(){artifact.processing="partial".into();for diagnostic in diagnostics{if !artifact.diagnostics.contains(&diagnostic){artifact.diagnostics.push(diagnostic);}}}
            let artifact=artifact.clone();if let Some(saved)=store.journal.artifacts.iter_mut().find(|a|a.identity==owner){*saved=artifact;}
        }
    }local::attach_outputs(store,&mut ledger)?;save(store,&ledger)?;store.checkpoint()?;retire_committed_attempts(store,&mut ledger)?;save(store,&ledger)?;store.checkpoint()
}

#[cfg(test)]
mod tests {
 use super::*;
 pub(super) fn owner(id:&str)->Identity {Identity{category:"C08".into(),kind:"EfxLibrary".into(),scope:Scope::Global,keys:BTreeMap::from([("LibraryID".into(),id.into())])}}
 pub(super) fn run(store:Storage)->Arc<Run>{Arc::new(Run{storage:Arc::new(std::sync::Mutex::new(store)),cancel:CancellationToken::new(),paused:AtomicBool::new(false),wake:Notify::new(),active:AtomicBool::new(true),network:false,helper_restore_attempts:Mutex::new(BTreeSet::new()),activity:Arc::new(Default::default()),shared_reads:Default::default(),helper_instances:Mutex::new(BTreeMap::new()),collection_context:Mutex::new(None)})}
 fn capture(owner:Identity,input:Option<FileRef>)->ManagedDecompilationRequest{ManagedDecompilationRequest{output_subdirectory:Some("Decompiled".into()),protocol_version:1,request_id:owner.keys["LibraryID"].clone(),expected_engine:MANAGED_ENGINE_ID.into(),create_project:false,capture:ManagedCapture{selector:Some(ManagedStoreSelector{folder:":Efx".into(),assembly:format!("efx.{}",owner.keys["LibraryID"].to_lowercase()),version:String::new()}),owner,binary_state:if input.is_some(){ManagedBinaryState::Captured}else{ManagedBinaryState::VerifiedAbsent},diagnostics:vec![],observation:json!({"freshness":"unknown"}),definition_sha256:"a".repeat(64),definition_revision:json!(1),input,symbols:None,symbol_mode:ManagedSymbolMode::None}}}
 fn definition(store:&mut Storage,id:&Identity){crate::collection_output::write_record(store,id.clone(),json!({"herbertDirectOutput":true,"definition":{"LibraryID":id.keys["LibraryID"]},"herbertSources":{"files":[{"name":"Authored.cs","text":"original source"},{"name":"compiled.json","text":"pending enrollment"}]}})).unwrap();store.checkpoint().unwrap();}

 #[tokio::test]
 async fn retained_replay_is_run_scoped_revision_checked_and_hash_verified(){
  let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-retained-replay".into())).unwrap();let owner=owner("Sample");definition(&mut store,&owner);
  let input=store.preserve_raw(b"immutable original","dll").unwrap();let mut request=capture(owner,Some(input.clone()));request.capture.observation=json!({"SysRevID":"AAAA","SymbolDisposition":"absent","ModuleVersionId":"mvid","AssemblyFullName":"Sample"});
  let run=run(store);let receipt=Supervisor::default().enqueue(&root.path().join("plugins"),&run,"C08",None,&[input.clone()],request).await.unwrap();
  let store=run.storage.lock().unwrap();let known=known_captures(&store,&json!({"iLibraryIDs":"[\"Sample\"]"})).unwrap();assert_eq!(known.as_array().unwrap().len(),1);assert_eq!(known[0]["captureId"],receipt.capture_id);
  let mut response=json!({"oSuccess":true,"oDS":{"Scope":[{"Caller":store.journal.caller,"Company":store.journal.company}],"Assemblies":[{"LibraryID":"Sample","Folder":":Efx","Assembly":"efx.sample","Version":"","SysRevID":"AAAA","DefinitionRevision":1,"ReusedCaptureId":receipt.capture_id,"Data":null,"Symbols":null}]}});
  let mut issued=vec![];restore_retained_rows(&store,&mut response,&mut issued).unwrap();assert_eq!(issued,vec![input.clone()]);assert_eq!(response["oDS"]["Assemblies"][0]["Data"]["nativeFile"]["sha256"],input.sha256);assert!(response["oDS"]["Assemblies"][0].get("ReusedCaptureId").is_none());
  let mut wrong=response.clone();wrong["oDS"]["Scope"][0]["Caller"]=json!("another caller");assert!(validate_reader_scope(&store,&wrong).is_err());
  wrong=response.clone();wrong["oDS"]["Assemblies"][0]["ReusedCaptureId"]=json!(receipt.capture_id);wrong["oDS"]["Assemblies"][0]["SysRevID"]=json!("changed");assert!(restore_retained_rows(&store,&mut wrong,&mut vec![]).is_err());
  fs::write(store.confined(&input.path).unwrap(),b"edited original").unwrap();assert!(known_captures(&store,&json!({"iLibraryIDs":"[\"Sample\"]"})).unwrap().as_array().unwrap().is_empty());
 }

 #[tokio::test]
 async fn fresh_native_owner_snapshot_retires_only_historical_status_not_evidence(){
  let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-current-owner".into())).unwrap();let owner=owner("Sample");definition(&mut store,&owner);let baseline=store.manifest.artifacts[&storage::identity_key(&owner)].processing.clone();let input=store.preserve_raw(b"retained native bytes","dll").unwrap();let run=run(store);let supervisor=Supervisor::default();
  let mut unavailable=capture(owner.clone(),None);unavailable.request_id="unavailable".into();unavailable.capture.selector=None;unavailable.capture.binary_state=ManagedBinaryState::Unavailable;unavailable.capture.diagnostics.push(issue("MANAGED_CAPTURE_UNAVAILABLE","Earlier refused observation"));let old=supervisor.enqueue(&root.path().join("plugins"),&run,"C08",None,&[],unavailable).await.unwrap();attach_outputs(&mut run.storage.lock().unwrap()).unwrap();assert_eq!(run.storage.lock().unwrap().manifest.artifacts[&storage::identity_key(&owner)].processing,"partial");
  let response={let store=run.storage.lock().unwrap();json!({"oSuccess":true,"oDS":{"Scope":[{"Caller":store.journal.caller,"Company":store.journal.company}],"Assemblies":[{"LibraryID":"Sample","Folder":":Efx","Assembly":"efx.sample","Version":"","SysRevID":"AAAA","BinarySysRevID":"AAAA","AfterSysRevID":"AAAA","DefinitionRevision":1,"DefinitionRevisionAfter":1,"SymbolDisposition":"absent","Data":{"nativeFile":input},"Symbols":null}]}})};
  {let mut store=run.storage.lock().unwrap();record_retained_read(&mut store,&response,&json!({"iLibraryIDs":"[\"Sample\"]"}),&[input.clone()]).unwrap();attach_outputs(&mut store).unwrap();assert!(store.manifest.artifacts[&storage::identity_key(&owner)].diagnostics.iter().any(|d|d.code=="MANAGED_CAPTURE_ENROLLMENT_PENDING"));}
  let mut current=capture(owner.clone(),Some(input.clone()));current.request_id="current".into();let fresh=supervisor.enqueue(&root.path().join("plugins"),&run,"C08",None,&[input.clone()],current).await.unwrap();let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();let ledger=load(&store).unwrap();assert!(ledger.captures.contains_key(&old.capture_id));assert!(!current_capture(&ledger,&old.capture_id,&ledger.captures[&old.capture_id]));assert!(current_capture(&ledger,&fresh.capture_id,&ledger.captures[&fresh.capture_id]));let artifact=&store.manifest.artifacts[&storage::identity_key(&owner)];assert_eq!(artifact.processing,baseline);assert!(!artifact.diagnostics.iter().any(|d|d.code=="MANAGED_CAPTURE_UNAVAILABLE"||d.code=="MANAGED_CAPTURE_ENROLLMENT_PENDING"));let view:Value=serde_json::from_slice(&fs::read(store.confined(&artifact.files.iter().find(|f|f.path.ends_with("/compiled.json")).unwrap().path).unwrap()).unwrap()).unwrap();assert_eq!(view["captures"].as_array().unwrap().iter().find(|c|c["captureId"]==old.capture_id).unwrap()["currentObservation"],false);assert_eq!(view["captures"].as_array().unwrap().iter().find(|c|c["captureId"]==fresh.capture_id).unwrap()["currentObservation"],true);store.verify(&input).unwrap();
  // A later native absence supersedes every prior selector, including removed
  // retained-version rows, without deleting its immutable evidence.
  let empty=json!({"oSuccess":true,"oDS":{"Scope":[{"Caller":store.journal.caller,"Company":store.journal.company}],"Assemblies":[]}});record_retained_read(&mut store,&empty,&json!({"iLibraryIDs":"[\"Sample\"]"}),&[]).unwrap();drop(store);let mut absent=capture(owner.clone(),None);absent.request_id="verified-absent".into();let receipt=supervisor.enqueue(&root.path().join("plugins"),&run,"C08",None,&[],absent).await.unwrap();let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.current_snapshots["Sample"].capture_ids,vec![receipt.capture_id]);assert_eq!(ledger.captures.len(),3);store.verify(&input).unwrap();
 }

 #[tokio::test]
 async fn default_off_resume_does_not_need_or_start_an_engine(){
  let root=tempfile::tempdir().unwrap();let run=run(Storage::open(root.path(),false,storage::new_journal("managed-off-no-engine".into())).unwrap());let supervisor=Supervisor::default();supervisor.resume(&root.path().join("missing/plugins"),&run).await.unwrap();assert!(supervisor.checked_engines.lock().await.is_empty());assert!(supervisor.run_workers.lock().await.is_empty());let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();retry_failed(&mut store).unwrap();assert!(!store.confined(STATE).unwrap().exists(),"Defaults must not create a managed sidecar");
 }
 #[tokio::test]
 async fn worker_tuning_is_per_run_and_accepts_values_above_the_default(){
  let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-workers-seven".into())).unwrap();store.journal.settings.local_decompiler_workers=7;let run=run(store);let supervisor=Supervisor::default();assert_eq!(supervisor.permits(&run).await.unwrap().available_permits(),7);
 }

 #[tokio::test]
 async fn binary_only_enrollment_is_durable_and_owner_scoped(){let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-binary-only".into())).unwrap();let id=owner("Sample");definition(&mut store,&id);let file=store.preserve_raw(b"original bytes","dll").unwrap();let request=capture(id.clone(),Some(file.clone()));let run=run(store);let jobs=Supervisor::default();let receipt=jobs.enqueue(&root.path().join("plugins"),&run,"C08",None,std::slice::from_ref(&file),request.clone()).await.unwrap();assert_eq!(receipt.project_state,"not-requested");assert!(receipt.job_id.is_none());let again=jobs.enqueue(&root.path().join("plugins"),&run,"C08",None,&[file.clone()],request.clone()).await.unwrap();assert_eq!(again.capture_id,receipt.capture_id);let mut other=request;other.capture.owner=owner("Other");other.capture.selector.as_mut().unwrap().assembly="efx.other".into();assert!(jobs.enqueue(&root.path().join("plugins"),&run,"C08",None,&[file],other).await.is_err());let mut store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.captures.len(),1);assert!(store.journal.managed_processing.is_some());attach_outputs(&mut store).unwrap();assert!(store.manifest.artifacts[&storage::identity_key(&id)].files.iter().any(|f|f.path.ends_with("compiled.json")));}
 #[tokio::test]
 async fn missing_engine_preserves_verified_original_and_optional_failure(){let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-no-engine".into())).unwrap();let id=owner("Sample");definition(&mut store,&id);let file=store.preserve_raw(b"original bytes","dll").unwrap();let mut request=capture(id.clone(),Some(file.clone()));request.create_project=true;let run=run(store);let receipt=Supervisor::default().enqueue(&root.path().join("plugins"),&run,"C08",None,&[file.clone()],request).await.unwrap();assert_eq!(receipt.project_state,"unavailable");let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();store.verify(&file).unwrap();let artifact=&store.manifest.artifacts[&storage::identity_key(&id)];assert_eq!(artifact.acquisition,Completeness::Collected);assert_eq!(artifact.processing,"partial");}
 #[test]
 fn processing_identity_retains_symbol_variants_and_ignores_owner_order(){let file=FileRef{path:"issued.dll".into(),sha256:"a".repeat(64),bytes:10,role:"native".into()};let mut a=capture(owner("A"),Some(file));let engine=Engine{manifest:EngineManifest{protocol_version:1,engine_id:MANAGED_ENGINE_ID.into(),target_rid:"win-x64".into(),executable:"companion.exe".into(),sha256:"c".repeat(64),bytes:100,cancel_grace_milliseconds:185},path:"not-executed".into()};let first=job_key(&a.capture,&engine);a.capture.owner=owner("B");a.capture.observation=json!({"differentSource":true});assert_eq!(job_key(&a.capture,&engine),first);a.capture.symbols=Some(FileRef{path:"issued.pdb".into(),sha256:"b".repeat(64),bytes:20,role:"native".into()});a.capture.symbol_mode=ManagedSymbolMode::ExternalPortable;let external=job_key(&a.capture,&engine);assert_ne!(external,first);a.capture.symbol_mode=ManagedSymbolMode::EmbeddedPortable;assert_ne!(job_key(&a.capture,&engine),external);assert_ne!(job_key(&a.capture,&engine),first);}




 fn boundary_journal(root:&Path)->storage::RunJournal{let id=fs::read_to_string(root.join("fixture-run-id.txt")).unwrap();crate::checkpoint_log::read_journal(root,id.trim()).unwrap().expect("A stopped fixture has a committed journal")}
 struct PreserveFailedBoundary(Option<tempfile::TempDir>);
 impl PreserveFailedBoundary{fn path(&self)->&Path{self.0.as_ref().expect("owned fixture").path()}}
 impl Drop for PreserveFailedBoundary{fn drop(&mut self){if std::thread::panicking(){if let Some(root)=self.0.take(){let path=root.keep();eprintln!("Managed boundary failure retained at {}",path.display());}}}}

 #[tokio::test]
 #[ignore="Owned child-process fixture; invoked only by the production-boundary parent test"]
 async fn managed_boundary_child(){
  let Some(root)=std::env::var_os("HERBERT_MANAGED_CHILD_ROOT")else{return;};let root=PathBuf::from(root);fs::create_dir_all(&root).unwrap();let phase=std::env::var("HERBERT_MANAGED_CHILD_PHASE").unwrap();let id="managed-process-boundaries";
  let journal=if phase=="initial" || phase=="force-cancel"{let journal=storage::new_journal(id.into());fs::write(root.join("fixture-run-id.txt"),&journal.run_id).unwrap();journal}else{boundary_journal(&root)};let mut store=Storage::open(&root,false,journal).unwrap();let owner=owner("Sample");if !store.manifest.artifacts.contains_key(&storage::identity_key(&owner)){definition(&mut store,&owner);}
  let arguments=json!({"iLibraryIDs":"[\"Sample\"]"});let mut issued=vec![];
  let mut native=if phase=="initial" || phase=="force-cancel"{let input=store.preserve_raw(&fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap(),"dll").unwrap();issued.push(input.clone());json!({"oSuccess":true,"oDS":{"Scope":[{"Caller":store.journal.caller,"Company":store.journal.company}],"Assemblies":[{"LibraryID":"Sample","Folder":":Efx","Assembly":"efx.sample","Version":"","SysRevID":"AAAA","BinarySysRevID":"AAAA","AfterSysRevID":"AAAA","DefinitionRevision":1,"DefinitionRevisionAfter":1,"SymbolDisposition":"absent","Data":{"nativeFile":input},"Symbols":null}]}})}else{let known=known_captures(&store,&arguments).unwrap();assert_eq!(known.as_array().unwrap().len(),1,"Restart uses the durable native receipt without another DLL read");json!({"oSuccess":true,"oDS":{"Scope":[{"Caller":store.journal.caller,"Company":store.journal.company}],"Assemblies":[{"LibraryID":"Sample","Folder":":Efx","Assembly":"efx.sample","Version":"","SysRevID":"AAAA","BinarySysRevID":"AAAA","AfterSysRevID":"AAAA","DefinitionRevision":1,"DefinitionRevisionAfter":1,"ReusedCaptureId":known[0]["captureId"],"Data":null,"Symbols":null}]}})};
  restore_retained_rows(&store,&mut native,&mut issued).unwrap();record_retained_read(&mut store,&native,&arguments,&issued).unwrap();let input:FileRef=serde_json::from_value(native["oDS"]["Assemblies"][0]["Data"]["nativeFile"].clone()).unwrap();let mut request=capture(owner,Some(input.clone()));request.create_project=true;let run=run(store);let supervisor=Supervisor::default();let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());
  supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&issued,request).await.unwrap();
  if phase=="initial" || phase=="force-cancel"{let mut store=run.storage.lock().unwrap();store.journal.cursors.insert("C08".into(),"advanced-page".into());store.checkpoint().unwrap();qualification_boundary("page-checkpoint").unwrap();}
  supervisor.resume(&package.join("plugins"),&run).await.unwrap();
  if phase=="explicit"{retry_failed(&mut run.storage.lock().unwrap()).unwrap();supervisor.resume(&package.join("plugins"),&run).await.unwrap();}
  let errors=supervisor.drain(&run).await;if phase=="force-cancel"{assert_eq!(errors.len(),1);assert_eq!(errors[0].code,"MANAGED_JOB_CANCELLED");assert!(errors[0].message.contains("killed and reaped"));let store=run.storage.lock().unwrap();store.verify(&input).unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.jobs.values().next().unwrap().state,"cancelled");fs::write(root.join("forced-cancel-proof.json"),serde_json::to_vec(&errors).unwrap()).unwrap();return;}if !errors.is_empty() && phase=="initial"{std::process::exit(74);}assert!(errors.is_empty(),"{errors:?}");let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();store.verify(&input).unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.captures.len(),1);assert_eq!(ledger.jobs.len(),1);let job=ledger.jobs.values().next().unwrap();for file in &job.files{store.verify(file).unwrap();}
  fs::write(root.join("child-proof.json"),serde_json::to_vec(&json!({"state":job.state,"attempt":job.attempt,"files":job.files,"cursor":store.journal.cursors.get("C08"),"input":input,"captures":ledger.captures.len(),"jobs":ledger.jobs.len()})).unwrap()).unwrap();
 }


 #[tokio::test]
 #[ignore="Actual pinned AOT companion is required"]
 async fn deleted_and_truncated_outputs_require_explicit_coherent_repair(){
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-output-repair".into())).unwrap();let owner=owner("Sample");definition(&mut store,&owner);let input=store.preserve_raw(&fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap(),"dll").unwrap();let mut request=capture(owner,Some(input.clone()));request.create_project=true;let run=run(store);let supervisor=Supervisor::default();let id=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[input.clone()],request).await.unwrap().job_id.unwrap();assert!(supervisor.drain(&run).await.is_empty());attach_outputs(&mut run.storage.lock().unwrap()).unwrap();
  for deleted in [true,false]{let old={let store=run.storage.lock().unwrap();let job=load(&store).unwrap().jobs[&id].clone();let file=job.files.iter().find(|file|if deleted{file.path.ends_with(".cs")}else{file.path.ends_with(".bin")}).unwrap();let path=store.confined(&file.path).unwrap();if deleted{fs::remove_file(path).unwrap();}else{fs::write(path,b"truncated resource").unwrap();}job};supervisor.resume(&package.join("plugins"),&run).await.unwrap();assert!(supervisor.drain(&run).await.is_empty());assert_eq!(load(&run.storage.lock().unwrap()).unwrap().jobs[&id].state,"recovery-required");retry_failed(&mut run.storage.lock().unwrap()).unwrap();supervisor.resume(&package.join("plugins"),&run).await.unwrap();assert!(supervisor.drain(&run).await.is_empty());let mut store=run.storage.lock().unwrap();let job=load(&store).unwrap().jobs[&id].clone();assert_eq!(job.state,"partial");assert_ne!(job.actual_root,old.actual_root);assert!(job.user_conflict);for file in &job.files{store.verify(file).unwrap();}store.verify(&input).unwrap();attach_outputs(&mut store).unwrap();}
 }

 #[tokio::test]
 #[ignore="Actual pinned AOT companion is required"]
 async fn missing_original_blocks_only_its_job_during_resume_and_explicit_retry(){
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-missing-input-isolation".into())).unwrap();for id in ["Missing","Valid"]{definition(&mut store,&owner(id));}let bytes=fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap();let missing=store.preserve_raw(&bytes,"dll").unwrap();let mut other=bytes;other.extend_from_slice(b"\nindependent valid input\n");let valid=store.preserve_raw(&other,"dll").unwrap();let run=run(store);let supervisor=Supervisor::default();let mut ids=vec![];
  for(name,input)in [("Missing",missing.clone()),("Valid",valid.clone())]{let mut request=capture(owner(name),Some(input.clone()));request.create_project=true;ids.push(supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[input],request).await.unwrap().job_id.unwrap());}assert!(supervisor.drain(&run).await.is_empty());
  {let mut store=run.storage.lock().unwrap();let mut ledger=load(&store).unwrap();fs::remove_file(store.confined(&missing.path).unwrap()).unwrap();for id in &ids{ledger.jobs.get_mut(id).unwrap().state="failed".into();}save(&store,&ledger).unwrap();retry_failed(&mut store).unwrap();let saved=load(&store).unwrap();assert_eq!(saved.jobs[&ids[0]].state,"recovery-required");assert_eq!(saved.jobs[&ids[1]].state,"queued");}
  supervisor.resume(&package.join("plugins"),&run).await.unwrap();assert!(supervisor.drain(&run).await.is_empty());let store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.jobs[&ids[0]].state,"recovery-required");assert_eq!(ledger.jobs[&ids[1]].state,"partial");store.verify(&valid).unwrap();
 }

 #[test]
 #[ignore="Actual pinned AOT companion is required; starts and stops only owned test hosts"]
 fn actual_process_exit_and_commit_failure_boundaries_recover_without_duplicate_work(){
  let points=["after-native-receipt","enrollment-before-ack","page-checkpoint","admitted-before-spawn","after-worker-spawn","committing-before-files","after-commit-member","before-project-manifest","after-project-manifest","before-owner-links","retirement-after-intent","retirement-after-leaf","half-result","missing-result","truncated-member","disk-full","access-denied","rename-failed"];
  for case in points{let workspace=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap()).parent().unwrap().join("tests");fs::create_dir_all(&workspace).unwrap();let root=PreserveFailedBoundary(Some(tempfile::Builder::new().prefix("managed-boundary-").tempdir_in(&workspace).unwrap()));fs::write(root.path().join("fixture-owner.json"),serde_json::to_vec(&json!({"owner":"function-decomp-b34f76f1","case":case,"purpose":"Actual host crash/recovery control; retain on failure"})).unwrap()).unwrap();let fault=match case{"half-result"|"missing-result"|"truncated-member"=>"committing-before-files","disk-full"|"access-denied"|"rename-failed"=>"commit-member-write",other=>other};let write_error=matches!(case,"disk-full"|"access-denied"|"rename-failed");
   let launch=|phase:&str,point:&str,kind:&str|std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","application::managed_decompilation::tests::managed_boundary_child","--ignored","--nocapture"]).env("HERBERT_MANAGED_CHILD_ROOT",root.path()).env("HERBERT_MANAGED_CHILD_PHASE",phase).env("HERBERT_MANAGED_FAULT",point).env("HERBERT_MANAGED_FAULT_KIND",kind).output().unwrap();
   let stopped=launch("initial",fault,if write_error{"write-error"}else{"exit"});assert_eq!(stopped.status.code(),Some(if write_error{74}else{73}),"{case}: {}",String::from_utf8_lossy(&stopped.stderr));
   if matches!(case,"half-result"|"missing-result"|"truncated-member"){let journal=boundary_journal(root.path());let store=Storage::open(root.path(),false,journal).unwrap();let ledger=load(&store).unwrap();let job=ledger.jobs.values().next().unwrap();let out=store.confined(job.attempt.as_ref().unwrap()).unwrap().join("output");match case{"half-result"=>fs::write(out.join("result.json"),b"{\"protocolVersion\":").unwrap(),"missing-result"=>fs::remove_file(out.join("result.json")).unwrap(),_=>{let result:Value=serde_json::from_slice(&fs::read(out.join("result.json")).unwrap()).unwrap();fs::write(out.join(result["files"][0]["path"].as_str().unwrap()),b"truncated").unwrap();}}}
   let resumed=launch("resume","","");assert!(resumed.status.success(),"{case}: {}",String::from_utf8_lossy(&resumed.stderr));let before:Value=serde_json::from_slice(&fs::read(root.path().join("child-proof.json")).unwrap()).unwrap();assert!(matches!(before["state"].as_str(),Some("partial"|"complete"|"failed"|"recovery-required")),"{case}: {before}");
   // A parent-death worker must release its OS lease. This waits for the actual
   // owned token, never a reused PID, before the explicit recovery request.
   let journal=boundary_journal(root.path());{let store=Storage::open(root.path(),false,journal).unwrap();let ledger=load(&store).unwrap();let job=ledger.jobs.values().next().unwrap();let root=store.confined(&format!("Herbert Data/managed-decompilation/v1/jobs/{}",job.id)).unwrap();if root.join("worker.lock").exists(){use fs2::FileExt;let file=fs::OpenOptions::new().read(true).write(true).open(root.join("worker.lock")).unwrap();file.lock_exclusive().unwrap();}}
   let retried=launch("explicit","","");assert!(retried.status.success(),"{case}: {}",String::from_utf8_lossy(&retried.stderr));let final_proof:Value=serde_json::from_slice(&fs::read(root.path().join("child-proof.json")).unwrap()).unwrap();assert_eq!(final_proof["state"],"partial","{case}: {final_proof}");assert_eq!(final_proof["jobs"],1);assert_eq!(final_proof["captures"],1);if before["state"]=="partial"{assert_eq!(before["attempt"],final_proof["attempt"],"An already committed variant cannot be rerun: {case}");}if !matches!(case,"after-native-receipt"|"enrollment-before-ack"){assert_eq!(final_proof["cursor"],"advanced-page");}
  }
 }


 #[test]
 #[ignore="Actual pinned AOT companion is required; blocks only an owned worker lease"]
 fn forced_cancel_reaps_only_owned_worker_and_explicit_retry_recovers(){
  use std::io::{BufRead,BufReader,Write};use fs2::FileExt;
  let workspace=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap()).parent().unwrap().join("tests");fs::create_dir_all(&workspace).unwrap();let root=PreserveFailedBoundary(Some(tempfile::Builder::new().prefix("managed-forced-cancel-").tempdir_in(&workspace).unwrap()));
  let mut child=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","application::managed_decompilation::tests::managed_boundary_child","--ignored","--nocapture"]).env("HERBERT_MANAGED_CHILD_ROOT",root.path()).env("HERBERT_MANAGED_CHILD_PHASE","force-cancel").env("HERBERT_MANAGED_FAULT","admitted-before-spawn").env("HERBERT_MANAGED_FAULT_KIND","hold").env("HERBERT_MANAGED_FORCE_CANCEL","1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
  let mut lines=BufReader::new(child.stdout.take().unwrap());loop{let mut line=String::new();assert!(lines.read_line(&mut line).unwrap()>0,"Child exited before the admission barrier");if line.contains("MANAGED_BOUNDARY_READY:admitted-before-spawn"){break;}}
  let ledger:Ledger=serde_json::from_slice(&fs::read(root.path().join(STATE)).unwrap()).unwrap();let job=ledger.jobs.values().next().unwrap();assert_eq!(job.id.len(),64);let lease=fs::OpenOptions::new().read(true).write(true).open(root.path().join(format!("Herbert Data/managed-decompilation/v1/jobs/{}/worker.lock",job.id))).unwrap();lease.lock_exclusive().unwrap();
  // The real native worker blocks on this exact owned OS token. It cannot read
  // the cooperative cancellation pipe before admission, so the host must reap it.
  child.stdin.take().unwrap().write_all(b"continue\n").unwrap();let stopped=child.wait_with_output().unwrap();assert!(stopped.status.success(),"{}",String::from_utf8_lossy(&stopped.stderr));let proof:Value=serde_json::from_slice(&fs::read(root.path().join("forced-cancel-proof.json")).unwrap()).unwrap();assert!(proof[0]["message"].as_str().unwrap().contains("killed and reaped"));drop(lease);
  let resumed=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","application::managed_decompilation::tests::managed_boundary_child","--ignored","--nocapture"]).env("HERBERT_MANAGED_CHILD_ROOT",root.path()).env("HERBERT_MANAGED_CHILD_PHASE","explicit").env_remove("HERBERT_MANAGED_FORCE_CANCEL").env_remove("HERBERT_MANAGED_FAULT").output().unwrap();assert!(resumed.status.success(),"{}",String::from_utf8_lossy(&resumed.stderr));let final_proof:Value=serde_json::from_slice(&fs::read(root.path().join("child-proof.json")).unwrap()).unwrap();assert_eq!(final_proof["state"],"partial");assert_eq!(final_proof["jobs"],1);assert_eq!(final_proof["captures"],1);
 }

 #[tokio::test]
 #[ignore="Actual qualified and legacy prototype AOT binaries are required"]
 async fn actual_engine_absence_tamper_rid_and_protocol_failures_preserve_originals(){
  let source=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let qualified=Engine::open(&source.join("plugins")).unwrap();let root=tempfile::tempdir().unwrap();let folder=root.path().join("package/companions/managed-decompiler").join(rid().unwrap());fs::create_dir_all(&folder).unwrap();let legacy=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_LEGACY_ENGINE").unwrap());
  for case in ["missing","tampered","wrong-rid","manifest-protocol","legacy-handshake"]{let mut manifest=qualified.manifest.clone();let path=folder.join(&manifest.executable);fs::copy(&qualified.path,&path).unwrap();match case{"missing"=>fs::remove_file(&path).unwrap(),"tampered"=>fs::write(&path,b"altered executable").unwrap(),"wrong-rid"=>manifest.target_rid="unqualified-rid".into(),"manifest-protocol"=>manifest.protocol_version=2,"legacy-handshake"=>{let bytes=fs::read(&legacy).unwrap();fs::write(&path,&bytes).unwrap();manifest.sha256=storage::sha256(&bytes);manifest.bytes=bytes.len()as u64;},_=>unreachable!()};
   fs::write(folder.join("companion.json"),serde_json::to_vec(&manifest).unwrap()).unwrap();let mut store=Storage::open(&root.path().join(case),false,storage::new_journal(format!("managed-engine-{case}"))).unwrap();let owner=owner("Sample");definition(&mut store,&owner);let input=store.preserve_raw(b"original DLL remains useful","dll").unwrap();let mut request=capture(owner.clone(),Some(input.clone()));request.create_project=true;let run=run(store);let receipt=Supervisor::default().enqueue(&root.path().join("package/plugins"),&run,"C08",None,&[input.clone()],request).await.unwrap();assert_eq!(receipt.project_state,"unavailable","{case}");assert!(receipt.job_id.is_none());let mut store=run.storage.lock().unwrap();store.verify(&input).unwrap();attach_outputs(&mut store).unwrap();assert_eq!(store.manifest.artifacts[&storage::identity_key(&owner)].acquisition,Completeness::Collected);assert_eq!(load(&store).unwrap().jobs.len(),0);
  }
 }

 #[tokio::test]
 #[ignore="Actual pinned AOT companion and portable/embedded controls are required"]
 async fn actual_late_owners_and_reversed_matched_symbol_variants_are_idempotent(){
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let dll=fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap();let pdb=fs::read(std::env::var("HERBERT_MANAGED_TEST_PDB").unwrap()).unwrap();let mut other=pdb.clone();let offset=other.windows(b"namedLocal\0".len()).position(|bytes|bytes==b"namedLocal\0").unwrap();other[offset..offset+b"otherLocal\0".len()].copy_from_slice(b"otherLocal\0");
  let mut sets=vec![];
  for reversed in [false,true]{let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal(format!("managed-symbol-order-{reversed}"))).unwrap();let input=store.preserve_raw(&dll,"dll").unwrap();let symbols=store.preserve_raw(&pdb,"pdb").unwrap();let conflicting=store.preserve_raw(&other,"pdb").unwrap();for name in ["None","External","Conflicting","Late"]{definition(&mut store,&owner(name));}let run=run(store);let supervisor=Supervisor::default();let mut order=vec![0,1,2];if reversed{order.reverse();}let mut first_attempts=BTreeMap::new();
   for index in order{let(name,symbols,mode)=match index{0=>("None",None,ManagedSymbolMode::None),1=>("External",Some(symbols.clone()),ManagedSymbolMode::ExternalPortable),_=>("Conflicting",Some(conflicting.clone()),ManagedSymbolMode::ExternalPortable)};let mut request=capture(owner(name),Some(input.clone()));request.create_project=true;request.capture.symbols=symbols;request.capture.symbol_mode=mode;let id=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[input.clone(),symbols_for_test(&pdb,&run),symbols_for_test(&other,&run)],request).await.unwrap().job_id.unwrap();assert!(supervisor.drain(&run).await.is_empty());let store=run.storage.lock().unwrap();let job=load(&store).unwrap().jobs[&id].clone();assert_eq!(job.state,"partial");first_attempts.insert(id,job.attempt);}
   let mut request=capture(owner("Late"),Some(input.clone()));request.create_project=true;let late=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[input.clone()],request).await.unwrap().job_id.unwrap();assert!(supervisor.drain(&run).await.is_empty());let mut store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.jobs.len(),3);for(id,attempt)in &first_attempts{assert_eq!(&ledger.jobs[id].attempt,attempt,"A late owner cannot rerun an existing job");}assert!(first_attempts.contains_key(&late));assert_eq!(ledger.captures.len(),4);attach_outputs(&mut store).unwrap();for artifact in store.manifest.artifacts.values(){let owned=format!("{}/",store.artifact_folder(&artifact.identity).unwrap());let projects=artifact.files.iter().filter(|f|f.path.ends_with(".csproj")).collect::<Vec<_>>();assert!(!projects.is_empty());assert!(projects.iter().all(|f|f.path.starts_with(&owned)),"Each owner must carry its own project");for file in &artifact.files{store.verify(file).unwrap();}}let late_owner=owner("Late");let late_folder=store.artifact_folder(&late_owner).unwrap();
   let edited=store.manifest.artifacts[&storage::identity_key(&late_owner)].files.iter().find(|f|f.path.starts_with(&format!("{late_folder}/Decompiled"))&&f.path.ends_with(".cs")).unwrap().path.clone();
   fs::write(store.confined(&edited).unwrap(),b"preserved mirrored-owner edit").unwrap();attach_outputs(&mut store).unwrap();
   assert_eq!(fs::read(store.confined(&edited).unwrap()).unwrap(),b"preserved mirrored-owner edit");
   for artifact in store.manifest.artifacts.values(){for file in &artifact.files{store.verify(file).unwrap();}}
   sets.push(ledger.jobs.keys().cloned().collect::<BTreeSet<_>>());
  }assert_eq!(sets[0],sets[1],"Reversing valid symbol observations must preserve the exact variant-key set");
 }
 fn symbols_for_test(bytes:&[u8],run:&Arc<Run>)->FileRef {run.storage.lock().unwrap().preserve_raw(bytes,"pdb").unwrap()}
 #[tokio::test]
 #[ignore="Actual pinned AOT companion is required"]
 async fn changed_pinned_engine_creates_a_new_variant_from_retained_originals(){
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let root=tempfile::tempdir().unwrap();let mut store=Storage::open(&root.path().join("run"),false,storage::new_journal("managed-engine-variant".into())).unwrap();let owner=owner("Sample");definition(&mut store,&owner);let input=store.preserve_raw(&fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap(),"dll").unwrap();let mut request=capture(owner,Some(input.clone()));request.create_project=true;let run=run(store);let supervisor=Supervisor::default();let old=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[input.clone()],request).await.unwrap();assert!(supervisor.drain(&run).await.is_empty());let old_job=load(&run.storage.lock().unwrap()).unwrap().jobs[old.job_id.as_ref().unwrap()].clone();
  let mut engine=Engine::open(&package.join("plugins")).unwrap();let folder=root.path().join("changed/companions/managed-decompiler").join(rid().unwrap());fs::create_dir_all(&folder).unwrap();let mut bytes=fs::read(&engine.path).unwrap();bytes.extend_from_slice(b"\nHerbert qualified engine variant boundary\n");let path=folder.join(&engine.manifest.executable);fs::write(&path,&bytes).unwrap();#[cfg(unix)]{use std::os::unix::fs::PermissionsExt;fs::set_permissions(&path,fs::Permissions::from_mode(0o700)).unwrap();}engine.manifest.sha256=storage::sha256(&bytes);engine.manifest.bytes=bytes.len()as u64;fs::write(folder.join("companion.json"),serde_json::to_vec(&engine.manifest).unwrap()).unwrap();
  supervisor.resume(&root.path().join("changed/plugins"),&run).await.unwrap();assert!(supervisor.drain(&run).await.is_empty());let mut store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.jobs.len(),2);assert_eq!(ledger.jobs[&old_job.id].attempt,old_job.attempt);let record=&ledger.captures[&old.capture_id];assert_eq!(record.job_ids.len(),2);assert_ne!(record.current_job_id,old.job_id);assert_eq!(ledger.jobs[record.current_job_id.as_ref().unwrap()].state,"partial");store.verify(&input).unwrap();attach_outputs(&mut store).unwrap();
 }

 #[tokio::test]
 #[ignore="Actual pinned AOT companion and managed control are required."]
 async fn actual_aot_worker_commits_verified_project_and_owner_links(){let source=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let dll=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap());let pdb=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PDB").unwrap());let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-actual-process".into())).unwrap();let id=owner("Sample");definition(&mut store,&id);let file=store.preserve_raw(&fs::read(dll).unwrap(),"dll").unwrap();let symbols=store.preserve_raw(&fs::read(pdb).unwrap(),"pdb").unwrap();let mut request=capture(id.clone(),Some(file.clone()));request.create_project=true;request.capture.symbols=Some(symbols.clone());request.capture.symbol_mode=ManagedSymbolMode::ExternalPortable;let run=run(store);let supervisor=Supervisor::default();let receipt=supervisor.enqueue(&source.join("plugins"),&run,"C08",None,&[file,symbols],request).await.unwrap();assert!(receipt.job_id.is_some());let errors=supervisor.drain(&run).await;assert!(errors.is_empty(),"{errors:?}");let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();let ledger=load(&store).unwrap();let job=&ledger.jobs[receipt.job_id.as_ref().unwrap()];assert_eq!(job.state,"partial");assert!(!job.files.is_empty());for file in &job.files {store.verify(file).unwrap();}let project=job.files.iter().find(|f|f.path.ends_with("Reconstructed.csproj")).unwrap();assert!(fs::read_to_string(store.confined(&project.path).unwrap()).unwrap().contains("Compile Include="));assert!(project.path.starts_with(&format!("{}/Decompiled/",store.artifact_folder(&id).unwrap())));assert!(store.manifest.artifacts[&storage::identity_key(&id)].files.contains(project));for file in &store.manifest.artifacts[&storage::identity_key(&id)].files{store.verify(file).unwrap();}}
 #[tokio::test]
 #[ignore="Actual pinned native AOT companion is required"]
 async fn actual_worker_recovery_and_conflicts_preserve_one_coherent_tree(){
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());
  for member in ["source","resource","project","manifest","unknown"]{
   let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal(format!("managed-conflict-{member}"))).unwrap();let owner=owner("Sample");definition(&mut store,&owner);let file=store.preserve_raw(&fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap(),"dll").unwrap();let mut request=capture(owner,Some(file.clone()));request.create_project=true;let run=run(store);let supervisor=Supervisor::default();let id=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[file],request).await.unwrap().job_id.unwrap();assert!(supervisor.drain(&run).await.is_empty());
   let(old,edited,user_text)={let store=run.storage.lock().unwrap();let mut ledger=load(&store).unwrap();let job=ledger.jobs.get_mut(&id).unwrap();let old=job.clone();let relative=if member=="unknown"{format!("{}/extra-user-file.txt",job.actual_root.as_ref().unwrap())}else{job.files.iter().find(|file|match member{"source"=>file.path.ends_with(".cs"),"resource"=>file.path.ends_with(".bin"),"project"=>file.path.ends_with(".csproj"),_=>file.path.ends_with("/result.json")}).unwrap().path.clone()};let edited=store.confined(&relative).unwrap();let user_text=format!("preserved user edit: {member}");fs::write(&edited,&user_text).unwrap();job.state="committing".into();save(&store,&ledger).unwrap();(old,edited,user_text)};
   if member=="source"{let(_,admission,worker)=job_leases(&run,&id).unwrap();drop(admission);supervisor.resume(&package.join("plugins"),&run).await.unwrap();assert_eq!(load(&run.storage.lock().unwrap()).unwrap().jobs[&id].state,"recovery-required");drop(worker);}
   supervisor.resume(&package.join("plugins"),&run).await.unwrap();let mut store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();let job=&ledger.jobs[&id];assert_eq!(job.state,"partial");assert_eq!(job.attempt,old.attempt,"Validated staging is reused, without another process");assert!(job.user_conflict);assert_ne!(job.actual_root,old.actual_root);assert_eq!(fs::read_to_string(&edited).unwrap(),user_text);let prefix=format!("{}/",job.actual_root.as_ref().unwrap());for file in &job.files{assert!(file.path.starts_with(&prefix));store.verify(file).unwrap();}let manifest:Value=serde_json::from_slice(&fs::read(store.confined(&format!("{prefix}result.json")).unwrap()).unwrap()).unwrap();assert_eq!(manifest["actualRoot"],job.actual_root.as_ref().unwrap().as_str());assert_eq!(manifest["userConflict"],true);assert_eq!(manifest["preservedUserRoot"],old.actual_root.unwrap());assert_eq!(manifest["committedFiles"].as_array().unwrap().len()+1,job.files.len());attach_outputs(&mut store).unwrap();
  }
 }
 #[tokio::test]
 #[ignore="Actual pinned native AOT companion is required"]
 async fn actual_failure_isolated_and_retry_is_explicit(){
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-failure-isolation".into())).unwrap();let owner=owner("Sample");definition(&mut store,&owner);
  let good=store.preserve_raw(&fs::read(std::env::var("HERBERT_MANAGED_TEST_DLL").unwrap()).unwrap(),"dll").unwrap();let bad=store.preserve_raw(b"MZ malformed managed image","dll").unwrap();let run=run(store);let supervisor=Supervisor::with_workers(2);
  let mut first=capture(owner.clone(),Some(bad.clone()));first.create_project=true;first.request_id="bad".into();let failed=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[bad.clone()],first).await.unwrap().job_id.unwrap();
  let mut second=capture(owner,Some(good.clone()));second.create_project=true;second.request_id="good".into();let valid=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[good.clone()],second).await.unwrap().job_id.unwrap();assert_eq!(supervisor.drain(&run).await.len(),1);
  let original_attempt={let store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.jobs[&failed].state,"failed");assert_eq!(ledger.jobs[&valid].state,"partial");store.verify(&good).unwrap();store.verify(&bad).unwrap();ledger.jobs[&failed].attempt.clone()};
  supervisor.resume(&package.join("plugins"),&run).await.unwrap();assert!(supervisor.drain(&run).await.is_empty());assert_eq!(load(&run.storage.lock().unwrap()).unwrap().jobs[&failed].attempt,original_attempt,"A malformed input cannot create an automatic crash loop");
  {let mut store=run.storage.lock().unwrap();retry_failed(&mut store).unwrap();}
  supervisor.resume(&package.join("plugins"),&run).await.unwrap();assert_eq!(supervisor.drain(&run).await.len(),1);let store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.jobs[&failed].state,"failed");assert_ne!(ledger.jobs[&failed].attempt,original_attempt);assert_eq!(ledger.jobs[&valid].state,"partial");
 }
 #[tokio::test]
 #[ignore="Actual pinned companion is required for admission verification"]
 async fn cancellation_before_cpu_admission_is_durable(){
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());let root=tempfile::tempdir().unwrap();let mut store=Storage::open(root.path(),false,storage::new_journal("managed-cancel-queue".into())).unwrap();let owner=owner("Sample");definition(&mut store,&owner);let file=store.preserve_raw(b"not consumed before admission","dll").unwrap();let mut request=capture(owner,Some(file.clone()));request.create_project=true;
  let run=run(store);let supervisor=Supervisor::with_workers(1);let permit=supervisor.workers.acquire().await.unwrap();let id=supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&[file.clone()],request).await.unwrap().job_id.unwrap();run.cancel.cancel();assert_eq!(supervisor.drain(&run).await.len(),1);drop(permit);let store=run.storage.lock().unwrap();let ledger=load(&store).unwrap();assert_eq!(ledger.jobs[&id].state,"cancelled");assert!(ledger.jobs[&id].attempt.is_none());store.verify(&file).unwrap();
 }

 #[tokio::test(flavor="multi_thread",worker_threads=4)]
 #[ignore="Explicit read-only retained corpus and private output path required"]
 async fn retained_corpus_performance() {
  let source=PathBuf::from(std::env::var("HERBERT_PERF_SOURCE").unwrap());
  let destination=PathBuf::from(std::env::var("HERBERT_PERF_OUTPUT").unwrap());assert!(!destination.exists());
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());
  let workers:usize=std::env::var("HERBERT_PERF_WORKERS").unwrap().parse().unwrap();
  let old:Ledger=serde_json::from_slice(&fs::read(source.join(STATE)).unwrap()).unwrap();
  let mut store=Storage::open(&destination,false,storage::new_journal("retained-performance".into())).unwrap();store.journal.category_names.insert("C08".into(),"Functions".into());
  let mut requests=vec![];
  for record in old.captures.values() {
   let mut request=record.request.clone();definition(&mut store,&request.capture.owner);
   for receipt in [&mut request.capture.input,&mut request.capture.symbols] {
    if let Some(file)=receipt {let bytes=fs::read(source.join(&file.path)).unwrap();assert_eq!(storage::sha256(&bytes),file.sha256);*file=store.preserve_raw(&bytes,if file.path.ends_with(".pdb"){"pdb"}else{"dll"}).unwrap();}
   } requests.push(request);
  }
  let run=run(store);let supervisor=Supervisor::with_workers(workers);let started=std::time::Instant::now();
  for request in requests {let issued=request.capture.input.iter().chain(request.capture.symbols.iter()).cloned().collect::<Vec<_>>();supervisor.enqueue(&package.join("plugins"),&run,"C08",None,&issued,request).await.unwrap();}
  let enrollment_ms=started.elapsed().as_millis();assert!(supervisor.drain(&run).await.is_empty());let drain_ms=started.elapsed().as_millis();
  let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();let total_ms=started.elapsed().as_millis();let ledger=load(&store).unwrap();
  for job in ledger.jobs.values(){assert!(matches!(job.state.as_str(),"partial"|"complete"));for file in &job.files{store.verify(file).unwrap();}}
  let receipt=json!({"workers":workers,"enrollmentMs":enrollment_ms,"drainMs":drain_ms,"totalMs":total_ms,"jobs":ledger.jobs.len(),"statistics":run.activity.statistics(),"engineMs":ledger.jobs.values().filter_map(|j|j.elapsed_milliseconds).sum::<u64>()});
  fs::write(std::env::var("HERBERT_PERF_RECEIPT").unwrap(),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();println!("{receipt}");
 }

}

#[cfg(test)]
mod native_qualification {
 use super::*;
 use crate::input::AuthMode;
 #[tokio::test]
 #[ignore="Explicit owned-lab compiled Function acceptance through actual C08 WASM and native AOT worker"]
 async fn compiled_functions_native_host_collection(){
  let lane=std::env::var("HERBERT_MANAGED_LANE").unwrap();
  let (endpoint,profile)=match lane.as_str(){"2025.2"=>("https://kinetic-2025-2.lab.test:9449/","lab-2025-2"),"2026.1"=>("https://kinetic-2026-1.lab.test:9448/","lab-2026-1"),_=>panic!("Only authorized owned labs")};
  let package=PathBuf::from(std::env::var("HERBERT_MANAGED_TEST_PACKAGE").unwrap());
  let output=PathBuf::from(std::env::var("HERBERT_MANAGED_OUTPUT").unwrap());assert!(!output.exists(),"Preserve previous evidence");
  let credentials=crate::profiles::secure_read(endpoint,profile).unwrap();let mode=if credentials.api_key.as_ref().is_some_and(|s|!s.is_empty()){AuthMode::BasicApiKey}else{AuthMode::Session};
  let connection=Connection::new(crate::input::endpoint(endpoint).unwrap(),"TESTCO".into(),mode,credentials,OperatingSettings::default(),Budget::new(4).unwrap()).unwrap();
  let cancel=CancellationToken::new();let admitted=connection.connect(&cancel).await.unwrap();connection.qualification_http.lock().unwrap().clear();
  let plugin=Plugin::load(&package.join("plugins/Functions/Functions.wasm")).await.unwrap();assert_eq!(plugin.category.id,"C08");
  let host=Host::with_roots(output.join("state"),package.join("plugins"),Some(output.join("coordination")));
  let mut journal=storage::new_journal(uuid::Uuid::new_v4().to_string());journal.endpoint=endpoint.into();journal.company="TESTCO".into();journal.caller=admitted["caller"].as_str().unwrap().into();journal.temporary_access_enabled=false;journal.collection_method=CollectionMethod::ServerAssisted;
  let project=std::env::var("HERBERT_MANAGED_PROJECT").as_deref()!=Ok("false");let download=std::env::var("HERBERT_MANAGED_DOWNLOAD").as_deref()!=Ok("false");journal.plugin_settings.insert("C08".into(),json!({"itemsPerPage":2,"downloadCompiledAssemblies":download,"decompileCompiledAssemblies":project}));
  let run=tests::run(Storage::open(&output.join("collection"),false,journal).unwrap());
  assert!(host.prepare_native_artifacts(&run,&connection,&[plugin.clone()]).await.is_empty());
  let result=if let Ok(id)=std::env::var("HERBERT_MANAGED_TARGET"){let identity=tests::owner(&id);host.acquire_targets(&run,&connection,&plugin,&[],vec![identity],Some(2),true).await.map(|_|1)}else{host.discover_targets(&run,&connection,&plugin,&[],Some(2)).await.map(|ids|ids.len())};
  let jobs=host.managed.drain(&run).await;
  {let mut store=run.storage.lock().unwrap();attach_outputs(&mut store).unwrap();}
  let cleanup=host.cleanup_native_artifacts(&run,&connection).await;connection.logout(&cancel).await.unwrap();
  let statistics=run.activity.finish_statistics();let store=run.storage.lock().unwrap();store.checkpoint().unwrap();
  let receipt=json!({"lane":lane,"result":result,"jobs":jobs,"cleanup":cleanup,"statistics":statistics,"artifacts":store.journal.artifacts,"diagnostics":store.journal.diagnostics,"nativeArtifacts":store.journal.native_artifacts,"httpRequests":connection.qualification_http.lock().unwrap().clone()});
  fs::write(output.join("result.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
  assert!(result.is_ok(),"{result:?}");assert!(jobs.is_empty(),"{jobs:?}");assert!(cleanup.is_empty(),"{cleanup:?}");
  for artifact in &store.journal.artifacts{assert_eq!(artifact.acquisition,Completeness::Collected,"{artifact:?}");for file in &artifact.files{store.verify(file).unwrap();}}
  if download{let ledger=load(&store).unwrap();assert!(!ledger.captures.is_empty());if project{assert!(ledger.jobs.values().any(|job|matches!(job.state.as_str(),"partial"|"complete")));}}
 }
}
