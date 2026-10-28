//! Optional C08 compiled snapshots. Definitions remain independently useful.
use herbert_contract::*;
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use std::collections::BTreeSet;

pub const DOWNLOAD:&str="downloadCompiledAssemblies";
pub const DECOMPILE:&str="decompileCompiledAssemblies";
pub fn normalize(settings:&mut Value) {if settings[DECOMPILE]==true {settings[DOWNLOAD]=json!(true);}}
pub fn requested(state:&Value)->bool {let mut settings=state["_pluginSettings"].clone();normalize(&mut settings);settings[DOWNLOAD]==true}
pub fn compatible(state:&Value)->bool {state.pointer("/_hostCapabilities/managedDecompilationV1/protocolVersion")==Some(&json!(1)) && state.pointer("/_hostCapabilities/managedDecompilationV1/retainedArtifactFunctions")==Some(&json!(true))}
pub fn binary_exports()->Vec<BinaryExport> {serde_json::from_value(json!([
 {"service":"compiled_functions","method":"ReadCompiled","pointer":"/oDS/Assemblies/*/Data","extension":"dll"},
 {"service":"compiled_functions","method":"ReadCompiled","pointer":"/oDS/Assemblies/*/Symbols","extension":"pdb"}
])).expect("static compiled-byte declarations")}
fn diagnostic(code:&str,message:&str,owner:Option<&Identity>)->Diagnostic {let mut d=Diagnostic::new(code,message);d.identity=owner.cloned();d}
fn digest(value:&Value)->String {format!("{:x}",Sha256::digest(serde_json::to_vec(value).expect("JSON value")))}

pub fn begin(state:&Value,dataset:&Value)->Option<GuestReply> {
 if !requested(state)||!compatible(state)||state["_collectionMethod"]=="directOnly" {return None;}
 let ids=state["ids"].as_array()?.iter().filter(|id|dataset["EfxLibrary"].as_array().is_some_and(|rows|rows.iter().any(|row|row["LibraryID"]==**id&&!crate::function_pages::temporary_helper(row)))).cloned().collect::<Vec<_>>();
 if ids.is_empty(){return None;}
 let mut state=state.clone();state["phase"]=json!("compiledLibraries");state["pageDataset"]=dataset.clone();state["_compiledSelected"]=json!(ids);
 state["_progress"]=json!({"request":"Reading associated stored Function DLL/PDB snapshots","response":"Compiled bytes received; preserving originals and queuing local projects"});
 Some(GuestReply::Action {action:HostAction::RetainedArtifactFunctionCall {alias:"compiled_functions".into(),function:"ReadCompiled".into(),arguments:json!({"iLibraryIDs":serde_json::to_string(&ids).expect("library identities")})},state})
}
/// Hand normal backups to the independent writer before optional native reads.
/// The continuation retains only native definitions needed for association checks.
pub fn save_first(reply:GuestReply,state:&Value,operation:&str)->Result<GuestReply,Diagnostic> {
 if !requested(state)||!compatible(state)||!matches!(operation,"materialize"|"acquire"){return prepare(reply,state,operation);}
 let mut next=state.clone();
 let (records,mut final_reply)=match reply {
  GuestReply::InventoryData{records,inventory}=>(records,*inventory),
  reply @ GuestReply::Definition{..}=>{
   let GuestReply::Definition{identity,definition,..}=&reply else{unreachable!()};
   (vec![(identity.clone(),definition.clone())],reply)
  },
  other=>return Ok(other),
 };
 if let GuestReply::Inventory{identities,..}=&mut final_reply{identities.clear();}
 next["_compiledFinal"]=serde_json::to_value(final_reply).map_err(|_|diagnostic("MANAGED_STATE_INVALID","Cannot retain the normal backup result.",None))?;
 next["_compiledDefinitions"]=json!(records.iter().map(|(owner,definition)|(owner,json!({"definition":definition["definition"]}))).collect::<Vec<_>>());
 next["phase"]=json!("compiledStart");
 next["_progress"]["pendingCompiledRead"]=json!(true);
 Ok(GuestReply::ReadyRecords{records,state:next})
}
pub fn start_saved(state:&Value)->Result<GuestReply,Diagnostic>{
 if let Some(reply)=begin(state,&state["pageDataset"]){Ok(reply)}else{finish_saved(state)}
}
pub fn finish_saved(state:&Value)->Result<GuestReply,Diagnostic>{
 let definitions:Vec<(Identity,Value)>=serde_json::from_value(state["_compiledDefinitions"].clone()).map_err(|_|diagnostic("MANAGED_STATE_INVALID","The saved Function owners are invalid.",None))?;
 let pending=definitions.iter().flat_map(|(owner,definition)|capture_requests(state,owner,definition)).collect::<Vec<_>>();
 let mut next=state.clone();next["_compiledJobs"]=json!(pending);next["_compiledJobIndex"]=json!(0);next["_compiledQueue"]=json!(true);
 next.as_object_mut().unwrap().remove("pageDataset");next.as_object_mut().unwrap().remove("_compiledDefinitions");
 drive(next,None)
}
pub fn received(request:&GuestRequest)->Value {
 let mut state=request.state.clone();
 if let Some(error)=&request.error {state["_compiledCaptureError"]=serde_json::to_value(error).expect("diagnostic");}
 else {state["_compiledCaptureResult"]=request.response.clone().unwrap_or(Value::Null);}
 state
}
fn unavailable(state:&Value,owner:&Identity)->Option<Diagnostic> {
 if !compatible(state){return Some(diagnostic("MANAGED_HOST_UNAVAILABLE","This host cannot negotiate optional compiled capture/local jobs. Normal Function backups remain available.",Some(owner)));}
 if state["_collectionMethod"]=="directOnly" {return Some(diagnostic("MANAGED_CAPTURE_UNAVAILABLE","This native version hides exact Function-store reads from direct services/BAQs. Direct only preserves normal definitions; the optional compiled snapshot requires the declared read helper.",Some(owner)));}
 if !state["_compiledCaptureError"].is_null() {return Some(diagnostic("MANAGED_CAPTURE_UNAVAILABLE","The optional compiled-store read was refused or failed. Normal Function backups remain available.",Some(owner)));}
 if state.pointer("/_compiledCaptureResult/oSuccess")!=Some(&json!(true)) {return Some(diagnostic("MANAGED_CAPTURE_UNAVAILABLE","The optional compiled-store reader did not return a verified successful response.",Some(owner)));}
 None
}
fn capture_requests(state:&Value,owner:&Identity,definition:&Value)->Vec<ManagedDecompilationRequest> {
 let id=&owner.keys["LibraryID"];let revision=definition.pointer(&format!("/definition/EfxLibrary/0/Revision")).cloned().unwrap_or(Value::Null);
 let definition_hash=digest(&definition["definition"]);
 let mut diagnostics=vec![];
 let rows=state.pointer("/_compiledCaptureResult/oDS/Assemblies").and_then(Value::as_array);
 if let Some(error)=unavailable(state,owner){diagnostics.push(error);}
 let owners=state.pointer("/_compiledCaptureResult/oDS/Owners").and_then(Value::as_array).map(|rows|rows.iter().filter(|row|row["LibraryID"]==*id).collect::<Vec<_>>()).unwrap_or_default();
 let visible=owners.len()==1;
 let owner_observation=owners.first().copied().cloned().unwrap_or(Value::Null);
 let owner_changed=visible && (owner_observation["DefinitionRevision"]!=revision || owner_observation["DefinitionRevisionAfter"]!=revision || owner_observation["BeforeRows"]!=owner_observation["AfterRows"]);
 let rows=rows.into_iter().flatten().filter(|row|row["LibraryID"]==*id).collect::<Vec<_>>();
 let mut captures=vec![];let mut seen=BTreeSet::new();
 if !diagnostics.is_empty() || !visible {
  if diagnostics.is_empty(){diagnostics.push(diagnostic("MANAGED_OWNER_UNVERIFIED","The compiled reader did not prove this exact caller-visible library owner.",Some(owner)));}
  captures.push(ManagedCapture{owner:owner.clone(),selector:None,binary_state:ManagedBinaryState::Unavailable,diagnostics,observation:json!({"association":"unavailable"}),definition_sha256:definition_hash,definition_revision:revision,input:None,symbols:None,symbol_mode:ManagedSymbolMode::None});
 } else if rows.is_empty() && (owner_changed || owner_observation["BeforeRows"]!=0 || owner_observation["AfterRows"]!=0){
  diagnostics.push(diagnostic("MANAGED_SOURCE_CHANGING","The empty row response does not prove stable absence across owner/store observations. No absent-binary claim was made.",Some(owner)));
  captures.push(ManagedCapture{owner:owner.clone(),selector:None,binary_state:ManagedBinaryState::SourceChanging,diagnostics,observation:owner_observation,definition_sha256:definition_hash,definition_revision:revision,input:None,symbols:None,symbol_mode:ManagedSymbolMode::None});
 } else if rows.is_empty(){
  captures.push(ManagedCapture{owner:owner.clone(),selector:Some(ManagedStoreSelector{folder:":Efx".into(),assembly:format!("efx.{}",id.to_lowercase()),version:String::new()}),binary_state:ManagedBinaryState::VerifiedAbsent,diagnostics,observation:json!({"association":"proved","read":"authoritative selected-owner store query","freshness":"not-applicable"}),definition_sha256:definition_hash,definition_revision:revision,input:None,symbols:None,symbol_mode:ManagedSymbolMode::None});
 }else{for row in rows{
  let mut diagnostics=vec![];
  let selector=ManagedStoreSelector{folder:row["Folder"].as_str().unwrap_or("").into(),assembly:row["Assembly"].as_str().unwrap_or("").into(),version:row["Version"].as_str().unwrap_or("").into()};
  let association_invalid=selector.folder!=":Efx"||selector.assembly!=format!("efx.{}",id.to_lowercase())||!row["Version"].is_string()||!seen.insert((selector.folder.clone(),selector.assembly.clone(),selector.version.clone()));
  if association_invalid{
   diagnostics.push(diagnostic("MANAGED_ASSOCIATION_INVALID","A compiled-store row has an unexpected or repeated exact selector.",Some(owner)));
  }
  let input=serde_json::from_value::<FileRef>(row["Data"]["nativeFile"].clone()).ok();
  let symbols=serde_json::from_value::<FileRef>(row["Symbols"]["nativeFile"].clone()).ok();
  if input.is_none(){diagnostics.push(diagnostic("MANAGED_BINARY_UNAVAILABLE","The associated store row did not yield a verified DLL receipt. Definitions and available symbols remain preserved.",Some(owner)));}
  let changed=owner_changed || row["SysRevID"]!=row["AfterSysRevID"] || (!row["BinarySysRevID"].is_null() && row["BinarySysRevID"]!=row["SysRevID"]) || row["DefinitionRevision"]!=row["DefinitionRevisionAfter"] || row["DefinitionRevision"]!=revision;
  if changed {diagnostics.push(diagnostic("MANAGED_SOURCE_CHANGING","Definition/store revision observations changed during capture. Original evidence is retained without a consistent-current-pair claim.",Some(owner)));}
  let symbol_state=row["SymbolDisposition"].as_str().unwrap_or("unsupported-unused");
  if symbols.is_some() && symbol_state!="matched-portable" {diagnostics.push(diagnostic("MANAGED_SYMBOLS_UNUSED","Original external symbols were retained but were not validated as a matching portable PDB. They are not supplied to the decompiler.",Some(owner)));}
  let mode=if !changed && symbol_state=="matched-portable" && symbols.is_some() {ManagedSymbolMode::ExternalPortable}else if !changed && symbol_state.contains("embedded-candidate"){ManagedSymbolMode::EmbeddedPortable}else{ManagedSymbolMode::None};
  let binary_state=if association_invalid || input.is_none(){ManagedBinaryState::Failed}else if changed {ManagedBinaryState::SourceChanging}else{ManagedBinaryState::Captured};
  let mut observation=row.clone();if let Some(map)=observation.as_object_mut(){map.remove("Data");map.remove("Symbols");}observation["association"]=json!("proved by native generator/repository identity; exact selector");observation["variant"]=json!(if selector.version.is_empty(){"native-current-store-slot"}else{"stored-only; native lifecycle unknown"});observation["freshness"]=json!("unknown");observation["pairConsistency"]=json!(if changed{"source-changing"}else{"single row read plus equal before/after revision observations; no atomic snapshot guarantee"});
  captures.push(ManagedCapture{owner:owner.clone(),selector:Some(selector),binary_state,diagnostics,observation,definition_sha256:definition_hash.clone(),definition_revision:revision.clone(),input,symbols,symbol_mode:mode});
 }}
 captures.into_iter().map(|capture|ManagedDecompilationRequest{output_subdirectory:Some("Decompiled".into()),protocol_version:1,request_id:digest(&serde_json::to_value(&capture).expect("capture")),expected_engine:MANAGED_ENGINE_ID.into(),create_project:state.pointer("/_pluginSettings/decompileCompiledAssemblies")==Some(&json!(true)),capture}).collect()
}
fn augment(definition:&mut Value,requests:&[ManagedDecompilationRequest]) {
 if !definition["herbertSources"].is_object(){definition["herbertSources"]=json!({});}
 if !definition["herbertSources"]["files"].is_array(){definition["herbertSources"]["files"]=json!([]);}
 let manifest=json!({"schemaVersion":1,"definitionStatus":"collected","captures":requests.iter().map(|r|json!({"capture":r.capture,"requestedProject":r.create_project,"projectState":if !r.create_project{"not-requested"}else if r.capture.input.is_none(){if r.capture.binary_state==ManagedBinaryState::VerifiedAbsent{"not-applicable"}else{"unavailable"}}else{"pending host enrollment"}})).collect::<Vec<_>>(),"sharedFilesExternal":true,"originals":"Referenced immutable files are in this run's Herbert Data area. Copy the whole download to retain all originals and shared projects."});
 definition["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"compiled.json","text":serde_json::to_string_pretty(&manifest).expect("compiled manifest")}));
}
pub fn prepare(reply:GuestReply,state:&Value,operation:&str)->Result<GuestReply,Diagnostic> {
 if !requested(state)||matches!(reply,GuestReply::PreparedPage{..}){return Ok(reply);}
 let mut pending=vec![];let mut next=state.clone();
 let mut final_reply=match reply{
  GuestReply::InventoryData{mut records,inventory}=>{
   for(owner,definition)in &mut records{let requests=capture_requests(state,owner,definition);augment(definition,&requests);pending.extend(requests);}
   let mut final_value=serde_json::to_value(*inventory).map_err(|_|Diagnostic::new("MANAGED_STATE_INVALID","Cannot serialize final inventory."))?;
   if compatible(state)&&operation=="materialize" {final_value["identities"]=json!([]);next["_compiledFinal"]=final_value;next["_compiledJobs"]=json!(pending);next["_compiledJobIndex"]=json!(0);next.as_object_mut().unwrap().remove("pageDataset");next["_compiledQueue"]=json!(true);return Ok(GuestReply::ReadyRecords{records,state:next});}
   if !compatible(state){for diagnostic in pending.iter().flat_map(|request|request.capture.diagnostics.iter()){let value=serde_json::to_value(diagnostic).expect("diagnostic");let diagnostics=final_value["diagnostics"].as_array_mut().ok_or_else(||Diagnostic::new("MANAGED_STATE_INVALID","The final inventory diagnostics are invalid."))?;if !diagnostics.contains(&value){diagnostics.push(value);}}}
   return Ok(GuestReply::InventoryData{records,inventory:Box::new(serde_json::from_value(final_value).map_err(|_|Diagnostic::new("MANAGED_STATE_INVALID","The final inventory is invalid."))?)});
  },
  GuestReply::Definition{identity,format,mut definition,completeness,diagnostics}=>{let requests=capture_requests(state,&identity,&definition);augment(&mut definition,&requests);pending.extend(requests);GuestReply::Definition{identity,format,definition,completeness,diagnostics}},
  other=>return Ok(other),
 };
 if !compatible(state){if let GuestReply::Definition{diagnostics,..}=&mut final_reply{for diagnostic in pending.iter().flat_map(|request|request.capture.diagnostics.iter()){if !diagnostics.contains(diagnostic){diagnostics.push(diagnostic.clone());}}}return Ok(final_reply);}
 next["_compiledFinal"]=serde_json::to_value(final_reply).map_err(|_|Diagnostic::new("MANAGED_STATE_INVALID","Cannot serialize final definition."))?;next["_compiledJobs"]=json!(pending);next["_compiledJobIndex"]=json!(0);next["_compiledQueue"]=json!(true);
 next.as_object_mut().unwrap().remove("pageDataset");
 drive(next,None)
}
pub fn queue_active(state:&Value)->bool {state["_compiledQueue"]==true}
pub fn queue(request:&GuestRequest)->Result<GuestReply,Diagnostic>{let mut state=request.state.clone();if state["_compiledAwaiting"]==true{let index=state["_compiledJobIndex"].as_u64().unwrap_or(0);state["_compiledJobIndex"]=json!(index+1);state["_compiledAwaiting"]=json!(false);}drive(state,request.error.clone())}
fn drive(mut state:Value,error:Option<Diagnostic>)->Result<GuestReply,Diagnostic>{
 if let Some(error)=error{let mut final_reply:GuestReply=serde_json::from_value(state["_compiledFinal"].clone()).map_err(|_|Diagnostic::new("MANAGED_STATE_INVALID","The saved final reply is invalid."))?;match &mut final_reply {GuestReply::Inventory{diagnostics,..}|GuestReply::Definition{diagnostics,..}=>diagnostics.push(error),_=>{}}state["_compiledFinal"]=serde_json::to_value(final_reply).unwrap();}
 let index=state["_compiledJobIndex"].as_u64().unwrap_or(0)as usize;
 if let Some(value)=state["_compiledJobs"].as_array().and_then(|jobs|jobs.get(index)){
  let request=serde_json::from_value(value.clone()).map_err(|_|Diagnostic::new("MANAGED_STATE_INVALID","The queued capture is invalid."))?;state["_compiledAwaiting"]=json!(true);return Ok(GuestReply::Action{action:HostAction::QueueManagedDecompilation{request},state});
 }
 serde_json::from_value(state["_compiledFinal"].clone()).map_err(|_|Diagnostic::new("MANAGED_STATE_INVALID","The final capture reply is invalid."))
}

#[cfg(test)]
mod tests {
 use super::*;use std::collections::BTreeMap;
 fn owner()->Identity{Identity{category:"C08".into(),kind:"EfxLibrary".into(),scope:Scope::Global,keys:BTreeMap::from([("LibraryID".into(),"Sample".into())])}}
 fn definition()->Value{json!({"definition":{"EfxLibrary":[{"LibraryID":"Sample","Revision":4}]},"herbertDirectOutput":true,"herbertSources":{"files":[{"name":"Authored.cs","text":"original"}],"binaryFiles":[{"name":"Sample.efxj","base64":"77u/e30="}]}})}
 fn reply()->GuestReply{GuestReply::InventoryData{records:vec![(owner(),definition())],inventory:Box::new(GuestReply::Inventory{identities:vec![owner()],next_cursor:Some("next".into()),authoritative:true,diagnostics:vec![]})}}
 fn state()->Value{json!({"ids":["Sample"],"_pluginSettings":{(DOWNLOAD):true,(DECOMPILE):true},"_collectionMethod":"serverAssisted","_hostCapabilities":{"managedDecompilationV1":{"protocolVersion":1,"retainedArtifactFunctions":true}},"_compiledCaptureResult":{"oSuccess":true,"oDS":{"Owners":[{"LibraryID":"Sample","DefinitionRevision":4,"DefinitionRevisionAfter":4,"BeforeRows":0,"AfterRows":0}],"Assemblies":[]}}})}
 #[test]fn options_off_preserve_the_existing_reply_and_avoid_optional_reads(){let state=json!({"ids":["Sample"]});let before=serde_json::to_value(reply()).unwrap();assert!(begin(&state,&json!({"EfxLibrary":[{"LibraryID":"Sample"}]})).is_none());assert_eq!(serde_json::to_value(prepare(reply(),&state,"materialize").unwrap()).unwrap(),before);}
 #[test]fn decompilation_normalizes_the_download_choice(){let mut values=json!({(DOWNLOAD):false,(DECOMPILE):true});normalize(&mut values);assert_eq!(values[DOWNLOAD],true);assert!(requested(&json!({"_pluginSettings":values})));}
 #[test]fn old_host_never_receives_a_new_action(){let mut state=state();state.as_object_mut().unwrap().remove("_hostCapabilities");assert!(begin(&state,&json!({"EfxLibrary":[{"LibraryID":"Sample"}]})).is_none());let GuestReply::InventoryData{records,..}=prepare(reply(),&state,"materialize").unwrap()else{panic!()};assert_eq!(records[0].1["herbertSources"]["binaryFiles"],definition()["herbertSources"]["binaryFiles"]);let manifest=&records[0].1["herbertSources"]["files"][1]["text"];assert!(manifest.as_str().unwrap().contains("MANAGED_HOST_UNAVAILABLE"));}
 #[test]fn authoritative_absence_is_distinct_from_helper_failure(){let owner=owner();let request=capture_requests(&state(),&owner,&definition()).remove(0);assert_eq!(request.capture.binary_state,ManagedBinaryState::VerifiedAbsent);assert!(request.capture.input.is_none());let mut state=state();state["_compiledCaptureError"]=json!({"code":"REFUSED"});let request=capture_requests(&state,&owner,&definition()).remove(0);assert_eq!(request.capture.binary_state,ManagedBinaryState::Unavailable);assert!(!request.capture.diagnostics.is_empty());}
 #[test]fn originals_are_written_before_full_page_job_enrollment(){let GuestReply::ReadyRecords{records,state}=prepare(reply(),&state(),"materialize").unwrap()else{panic!()};assert_eq!(records.len(),1);assert_eq!(records[0].1["herbertSources"]["binaryFiles"],definition()["herbertSources"]["binaryFiles"]);let GuestReply::Action{action:HostAction::QueueManagedDecompilation{request},..}=drive(state,None).unwrap()else{panic!()};assert_eq!(request.capture.owner,owner());assert_eq!(request.capture.binary_state,ManagedBinaryState::VerifiedAbsent);}
 #[test]fn direct_only_never_installs_the_optional_reader(){let mut state=state();state["_collectionMethod"]=json!("directOnly");assert!(begin(&state,&json!({"EfxLibrary":[{"LibraryID":"Sample"}]})).is_none());let request=capture_requests(&state,&owner(),&definition()).remove(0);assert_eq!(request.capture.binary_state,ManagedBinaryState::Unavailable);}
 #[test]fn only_a_validated_stable_external_pdb_is_selected(){let mut state=state();let native=|path:&str|json!({"nativeFile":{"path":path,"sha256":"a".repeat(64),"bytes":3,"role":"native"}});state["_compiledCaptureResult"]["oDS"]["Assemblies"]=json!([{"LibraryID":"Sample","Folder":":Efx","Assembly":"efx.sample","Version":"","DefinitionRevision":4,"DefinitionRevisionAfter":4,"SysRevID":"AAAA","AfterSysRevID":"AAAA","SymbolDisposition":"matched-portable","Data":native("dll"),"Symbols":native("pdb")}]);let r=capture_requests(&state,&owner(),&definition()).remove(0);assert_eq!(r.capture.symbol_mode,ManagedSymbolMode::ExternalPortable);state["_compiledCaptureResult"]["oDS"]["Assemblies"][0]["AfterSysRevID"]=json!("changed");let r=capture_requests(&state,&owner(),&definition()).remove(0);assert_eq!(r.capture.binary_state,ManagedBinaryState::SourceChanging);assert_eq!(r.capture.symbol_mode,ManagedSymbolMode::None);assert!(r.capture.symbols.is_some());}
}

#[cfg(test)]
mod save_first_tests {
 use super::*;
 use std::collections::BTreeMap;
 fn owner()->Identity{Identity{category:"C08".into(),kind:"EfxLibrary".into(),scope:Scope::Global,keys:BTreeMap::from([("LibraryID".into(),"Sample".into())])}}
 fn state()->Value{json!({"ids":["Sample"],"pageDataset":{"EfxLibrary":[{"LibraryID":"Sample","Revision":4}]},"_pluginSettings":{(DOWNLOAD):true,(DECOMPILE):true},"_collectionMethod":"serverAssisted","_hostCapabilities":{"managedDecompilationV1":{"protocolVersion":1,"retainedArtifactFunctions":true}}})}
 fn records()->Vec<(Identity,Value)>{vec![(owner(),json!({"definition":{"EfxLibrary":[{"LibraryID":"Sample","Revision":4}]},"herbertSources":{"files":[{"name":"Authored.cs","text":"original"}]}}))]}
 #[test]fn normal_records_are_ready_before_compiled_action_and_failure_does_not_rewrite_them(){
  let originals=records();let reply=GuestReply::InventoryData{records:originals.clone(),inventory:Box::new(GuestReply::Inventory{identities:vec![owner()],next_cursor:Some("next".into()),authoritative:true,diagnostics:vec![]})};
  let GuestReply::ReadyRecords{records,state}=save_first(reply,&state(),"materialize").unwrap()else{panic!()};assert_eq!(records,originals);assert_eq!(state["_progress"]["pendingCompiledRead"],true);
  let GuestReply::Action{action:HostAction::RetainedArtifactFunctionCall{..},state}=start_saved(&state).unwrap()else{panic!()};
  let mut state=state;state["_compiledCaptureError"]=json!({"code":"REFUSED"});
  let GuestReply::Action{action:HostAction::QueueManagedDecompilation{request},state}=finish_saved(&state).unwrap()else{panic!()};assert_eq!(request.capture.binary_state,ManagedBinaryState::Unavailable);assert_eq!(request.capture.owner,owner());
  let mut state=state;state["_compiledJobIndex"]=json!(1);
  let GuestReply::Inventory{identities,next_cursor,..}=drive(state,None).unwrap()else{panic!()};assert!(identities.is_empty());assert_eq!(next_cursor.as_deref(),Some("next"));
 }
 #[test]fn selected_library_also_yields_normal_backup_before_read(){
  let (identity,definition)=records().remove(0);let reply=GuestReply::Definition{identity,definition,format:"native-function-export".into(),completeness:Completeness::Collected,diagnostics:vec![]};
  let GuestReply::ReadyRecords{records,state}=save_first(reply,&state(),"acquire").unwrap()else{panic!()};assert_eq!(records.len(),1);assert!(matches!(start_saved(&state).unwrap(),GuestReply::Action{action:HostAction::RetainedArtifactFunctionCall{..},..}));
 }
}
