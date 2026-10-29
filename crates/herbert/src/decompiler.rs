use crate::storage;
// Category-independent managed assembly engine. Callers own acquisition, placement and metadata.
use herbert_contract::*;
use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
use std::{collections::BTreeSet,fs,io::Read,path::{Path,PathBuf},process::Stdio,sync::Arc,time::Duration};
use tokio_util::sync::CancellationToken;
fn issue(code:&str,message:impl Into<String>)->Diagnostic {Diagnostic::new(code,message)}
fn failure(message:impl Into<String>)->Diagnostic {issue("MANAGED_DECOMPILATION_FAILED",message)}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct EngineManifest { pub(crate) protocol_version:u32,pub(crate) engine_id:String,pub(crate) target_rid:String,pub(crate) executable:String,pub(crate) sha256:String,pub(crate) bytes:u64,pub(crate) cancel_grace_milliseconds:u64 }
#[derive(Clone)]
pub(crate) struct Engine { pub(crate) manifest:EngineManifest,pub(crate) path:PathBuf }
pub(crate) fn rid()->Option<&'static str> {
    match (std::env::consts::OS,std::env::consts::ARCH) { ("windows","x86_64")=>Some("win-x64"),("linux","x86_64")=>Some("linux-x64"),_=>None }
}
impl Engine {
    pub(crate) fn open(plugin_root:&Path)->Result<Self,Diagnostic> {let engine=Self::descriptor(plugin_root)?;engine.verify()?;Ok(engine)}
    pub(crate) fn descriptor(plugin_root:&Path)->Result<Self,Diagnostic> {
        let rid=rid().ok_or_else(||issue("MANAGED_ENGINE_UNAVAILABLE","Decompiling compiled code is not available on this operating system. The compiled files were still saved."))?;
        let root=plugin_root.parent().ok_or_else(||failure("Plugin package root is unavailable."))?.join("companions/managed-decompiler").join(rid);
        let data=fs::read(root.join("companion.json")).map_err(|_|issue("MANAGED_ENGINE_UNAVAILABLE","The pinned decompiler decompiler add-on is absent. Captured binaries remain available."))?;
        let manifest:EngineManifest=serde_json::from_slice(&data).map_err(|_|failure("The decompiler add-on manifest is invalid."))?;
        if manifest.protocol_version!=MANAGED_DECOMPILATION_PROTOCOL || manifest.engine_id!=MANAGED_ENGINE_ID || manifest.target_rid!=rid { return Err(failure("The installed decompiler does not match this Herbert version or this computer. Reinstall Herbert's optional decompiler from this release. Compiled files were still saved.")); }
        let relative=storage::safe_relative(&manifest.executable)?;
        if relative.components().count()!=1 {return Err(failure("The decompiler add-on executable must be a package leaf."));}
        let path=root.join(relative);
        Ok(Self {manifest,path})
    }

    pub(crate) async fn handshake(&self,cancel:&CancellationToken)->Result<(),Diagnostic>{
        self.verify()?;
        let mut command=tokio::process::Command::new(&self.path);command.arg("--protocol").current_dir(worker_directory()).env_clear().stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(windows)]{command.env("SystemRoot",std::env::var_os("SystemRoot").unwrap_or_else(||r"C:\Windows".into())).creation_flags(0x08000000);}
        let child=command.spawn().map_err(|_|failure("Cannot start the verified decompiler add-on protocol check."))?;
        let output=tokio::select!{_ = cancel.cancelled()=>return Err(issue("MANAGED_JOB_CANCELLED","Decompiler add-on negotiation was cancelled.")),output=child.wait_with_output()=>output.map_err(|_|failure("Cannot reap the decompiler add-on protocol check."))?};
        let protocol:Value=serde_json::from_slice(&output.stdout).map_err(|_|failure("The decompiler add-on protocol handshake is invalid."))?;
        if !output.status.success() || protocol["protocolVersion"]!=1 || protocol["engineId"]!=MANAGED_ENGINE_ID || protocol["resolverPolicy"]!="target-only-v1" || protocol["aot"]!=true || protocol["controlChannel"]!="line-or-eof-v1" || protocol["generationLeases"]!="os-lock-token-v1" || protocol["symbolModes"]!=json!(["none","externalPortable","embeddedPortable"]) {return Err(failure("The decompiler add-on does not implement the required AOT, resolver, cancellation, generation or symbol protocol."));}
        Ok(())
    }
    pub(crate) fn verify(&self)->Result<(),Diagnostic> {
        let metadata=fs::symlink_metadata(&self.path).map_err(|_|failure("The decompiler add-on executable is missing."))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len()!=self.manifest.bytes {return Err(failure("The decompiler add-on file type/length is invalid."));}
        let mut file=fs::File::open(&self.path).map_err(|_|failure("Cannot read the decompiler add-on executable."))?;
        use sha2::{Digest,Sha256};let mut digest=Sha256::new();let mut buffer=[0_u8;65536];
        loop {let read=file.read(&mut buffer).map_err(|_|failure("Cannot hash the decompiler add-on executable."))?;if read==0 {break;}digest.update(&buffer[..read]);}
        if hex::encode(digest.finalize())!=self.manifest.sha256 {return Err(failure("The decompiler add-on executable hash does not match its pinned manifest."));}Ok(())
    }
}
pub(crate) fn worker_directory()->PathBuf{
    // CreateProcess rejects a long current-directory path even when all file APIs
    // accept extended paths. Every request/input/output is already absolute.
    #[cfg(windows)]{std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(||PathBuf::from(r"C:\Windows"))}
    #[cfg(not(windows))]{PathBuf::from("/")}
}

pub(crate) fn qualification_boundary(name:&str)->Result<(),Diagnostic>{
    // Fault injection is compiled only into the existing test host. The shipped
    // host cannot activate it through environment variables or protocol input.
    #[cfg(test)] if std::env::var("HERBERT_MANAGED_FAULT").as_deref()==Ok(name){
        if std::env::var("HERBERT_MANAGED_FAULT_KIND").as_deref()==Ok("write-error"){return Err(issue("OUTPUT_WRITE_FAILED",format!("Injected production-boundary write failure at {name}; the owned candidate is preserved.")));}
        if std::env::var("HERBERT_MANAGED_FAULT_KIND").as_deref()==Ok("hold"){use std::io::{BufRead,Write};println!("MANAGED_BOUNDARY_READY:{name}");std::io::stdout().flush().unwrap();let mut line=String::new();std::io::stdin().lock().read_line(&mut line).unwrap();assert_eq!(line.trim(),"continue");return Ok(());}
        std::process::exit(73);
    }
    let _=name;Ok(())
}

pub(crate) async fn execute(engine:&Engine, request_path:&Path, attempt:&Path, cancel:&CancellationToken, activity:&Arc<crate::activity::Activity>)->Result<(),Diagnostic>{
    let stdout=fs::File::create(attempt.join("stdout.txt")).map_err(|_|failure("Cannot create worker output log."))?;
    let stderr=fs::File::create(attempt.join("stderr.txt")).map_err(|_|failure("Cannot create worker diagnostic log."))?;
    let mut command=tokio::process::Command::new(&engine.path);command.arg(&request_path).current_dir(worker_directory()).env_clear().env("TEMP",&attempt).env("TMP",&attempt).env("TMPDIR",&attempt).env("HOME",&attempt).stdin(Stdio::piped()).stdout(stdout).stderr(stderr).kill_on_drop(true);
    #[cfg(windows)]{command.env("SystemRoot",std::env::var_os("SystemRoot").unwrap_or_else(||r"C:\Windows".into())).creation_flags(0x08000000);}
    let mut child=command.spawn().map_err(|error|failure("Herbert could not start its decompiler program. Check that security software is not blocking files in the Herbert folder, then retry. The downloaded files are unaffected.").with_cause(&error).with_context("osError",error.raw_os_error()))?;
    qualification_boundary("after-worker-spawn")?;
    #[cfg(test)] if std::env::var("HERBERT_MANAGED_FORCE_CANCEL").as_deref()==Ok("1"){cancel.cancel();}
    // Tokio Child::wait closes its owned stdin. Keep the control pipe separate so
    // a normal wait cannot impersonate parent death and cancel the worker.
    let mut control=child.stdin.take();
    let mut observed=0;
    // 200 ms samples match human-visible progress; this is observation cadence, not a work limit.
    let mut samples=tokio::time::interval(Duration::from_millis(200));
    let status=loop {tokio::select!{result=child.wait()=>break result.map_err(|_|failure("Cannot reap the owned worker."))?,
        _=samples.tick()=>{let count=fs::read_dir(attempt.join("output")).map(|entries|entries.filter_map(Result::ok).filter(|e|e.file_type().is_ok_and(|t|t.is_file())).count()).unwrap_or(observed);if count>observed{activity.meaningful_work("decompiledFiles",(count-observed)as u64);observed=count;}},_ = cancel.cancelled()=>{
        use tokio::io::AsyncWriteExt;
        if let Some(mut pipe)=control.take(){let _=pipe.write_all(b"cancel\n").await;let _=pipe.shutdown().await;}
        let cooperative=tokio::time::timeout(Duration::from_millis(engine.manifest.cancel_grace_milliseconds),child.wait()).await;
        let forced=match cooperative{Ok(Ok(_))=>false,Ok(Err(_))=>return Err(issue("MANAGED_WORKER_RECONCILIATION_REQUIRED","Cannot reap the owned worker after cancellation; its exact attempt is preserved.")),Err(_)=>{child.kill().await.map_err(|_|issue("MANAGED_WORKER_RECONCILIATION_REQUIRED","Cannot kill/reap the owned worker; its exact attempt remains available."))?;true}};
        return Err(issue("MANAGED_JOB_CANCELLED",if forced{"Local work exceeded the measured cancellation grace; its owned worker was killed and reaped. Verified originals and staging were preserved; explicit retry is available."}else{"Local work was cancelled cooperatively; verified originals and staging were preserved. Explicit retry is available."}));
    }}};
    if !status.success(){return Err(failure(format!("The decompiler add-on failed. Detailed diagnostics remain in {}.",attempt.join("stderr.txt").display())));}
    activity.meaningful_work("decompilerFinished",1);
    Ok(())
}

#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct GeneratedFile {pub(crate) path:String,pub(crate) bytes:u64,pub(crate) sha256:String,pub(crate) kind:String,pub(crate) logical_name:Option<String>}
#[derive(Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct GeneratedResult {pub(crate) protocol_version:u32,pub(crate) job_id:String,pub(crate) engine_id:String,pub(crate) input_sha256:String,pub(crate) outcome:String,pub(crate) symbol_disposition:String,pub(crate) symbol_sha256:Option<String>,pub(crate) unresolved_references:Vec<String>,pub(crate) coverage:Vec<Value>,pub(crate) files:Vec<GeneratedFile>,pub(crate) diagnostics:Vec<String>,pub(crate) elapsed_milliseconds:u64}

pub(crate) fn validate_result(out:&Path,id:&str,input_hash:&str,symbol_hash:Option<&str>)->Result<GeneratedResult,Diagnostic>{
    for path in [out.to_path_buf(),out.join("result.json")] {let metadata=fs::symlink_metadata(&path).map_err(|_|failure("The generated output is unavailable."))?;if metadata.file_type().is_symlink(){return Err(failure("Generated output must not follow symbolic links."));}}
    let bytes=fs::read(out.join("result.json")).map_err(|_|failure("A successful worker omitted its result manifest."))?;
    let result:GeneratedResult=serde_json::from_slice(&bytes).map_err(|_|failure("The worker result schema is invalid."))?;
    if result.protocol_version!=1||result.job_id!=id||result.engine_id!=MANAGED_ENGINE_ID||result.input_sha256!=input_hash||!matches!(result.outcome.as_str(),"complete"|"partial") {return Err(failure("The worker result identity/outcome does not match the job."));}
    if result.outcome=="complete"&&(!result.unresolved_references.is_empty()||!result.diagnostics.is_empty()||result.coverage.iter().any(|c|matches!(c["outcome"].as_str(),Some("failed"|"partial"|"unavailable")))) {return Err(failure("The worker claimed complete despite unresolved coverage."));}
    if symbol_hash.is_some_and(|symbols| result.symbol_disposition!="matched-used" || result.symbol_sha256.as_deref()!=Some(symbols)) {return Err(failure("The decompiler add-on did not validate/use the exact selected external symbols; the original pair and diagnostics were preserved."));}
    if symbol_hash.is_none() && (result.symbol_disposition=="matched-used" || result.symbol_sha256.is_some()) {return Err(failure("The decompiler add-on reported unapproved external symbols."));}
    let mut names=BTreeSet::from([storage::collision_key("result.json")]);
    for file in &result.files {
        if !matches!(file.kind.as_str(),"source"|"project"|"raw-resource"|"notice") || (file.kind=="raw-resource" && file.logical_name.is_none()) {return Err(failure("The generated file kind/logical-name coverage is invalid."));}
        let relative=storage::safe_relative(&file.path)?;if relative.components().count()!=1||!names.insert(storage::collision_key(&file.path)){return Err(failure("Worker output paths collide or are not confined leaves."));}
        let path=out.join(relative);let metadata=fs::symlink_metadata(&path).map_err(|_|failure("The worker omitted a listed file."))?;
        if !metadata.is_file()||metadata.file_type().is_symlink()||metadata.len()!=file.bytes||storage::sha256(&fs::read(&path).map_err(|_|failure("Cannot verify worker output."))?)!=file.sha256 {return Err(failure("The worker output file type, length or hash is invalid."));}
    }
    let actual=fs::read_dir(&out).map_err(|_|failure("Cannot inspect worker output."))?.map(|e|e.map(|e|storage::collision_key(&e.file_name().to_string_lossy()))).collect::<Result<BTreeSet<_>,_>>().map_err(|_|failure("Cannot inspect worker output membership."))?;
    if actual!=names {return Err(failure("The worker output contains unlisted files."));}
    let project=result.files.iter().filter(|file|file.kind=="project").collect::<Vec<_>>();
    if project.len()!=1{return Err(failure("A project result must contain exactly one project file."));}
    let text=fs::read_to_string(out.join(&project[0].path)).map_err(|_|failure("Cannot inspect project relationships."))?;
    let members=text.split("Include=\"").skip(1).map(|part|part.split('"').next().unwrap_or("").to_owned()).collect::<BTreeSet<_>>();
    let expected=result.files.iter().filter(|f|matches!(f.kind.as_str(),"source"|"raw-resource")).map(|f|f.path.clone()).collect::<BTreeSet<_>>();
    if members!=expected || members.iter().any(|name|storage::safe_relative(name).is_err()||name.contains('/')){return Err(failure("Project member relationships do not resolve to the verified candidate."));}
    Ok(result)
}
