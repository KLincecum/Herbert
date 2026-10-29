//! Business rules and mappings actual compiled guest and native-response replay through real output storage.
use herbert_contract::*;
use serde_json::{Value,json};
use std::{path::PathBuf,sync::{Arc,Mutex}};
use tokio_util::sync::CancellationToken;
#[tokio::test]
#[ignore="Explicit private retained inputs and compiled Business rules and mappings performance comparison"]
async fn business_rules_and_mappings_local_performance_replay(){
 let source=PathBuf::from(std::env::var("HERBERT_BUSINESS_RULES_AND_MAPPINGS_LOCAL_SOURCE").unwrap());
 let report=PathBuf::from(std::env::var("HERBERT_BUSINESS_RULES_AND_MAPPINGS_LOCAL_REPORT").unwrap());
 let wasm=PathBuf::from(std::env::var("HERBERT_BUSINESS_RULES_AND_MAPPINGS_WASM").unwrap());
 let mut manifest:Value=serde_json::from_slice(&std::fs::read(source.join("Herbert Data/manifest.json")).unwrap()).unwrap();
 let mut retained:Vec<Value>=manifest["retainedFiles"].as_object().unwrap().values().cloned().collect();
 if std::env::var_os("HERBERT_BUSINESS_RULES_AND_MAPPINGS_LOCAL_LAB").is_some(){
  let receipt:Value=serde_json::from_slice(&std::fs::read(source.parent().unwrap().join("result.json")).unwrap()).unwrap();
  manifest["artifacts"]=Value::Object(receipt["artifacts"].as_array().unwrap().iter().enumerate().map(|(i,a)|(i.to_string(),a.clone())).collect());manifest["company"]=json!("TESTCO");
  let replay:Value=serde_json::from_slice(&std::fs::read(source.parent().unwrap().parent().unwrap().join("replay-manifest.json")).unwrap()).unwrap();
  retained=["native","queries"].into_iter().flat_map(|branch|replay[branch].as_object().unwrap().values().map(|p|json!({"path":p}))).collect();
 }
 let mut native=std::collections::BTreeMap::new();
 for file in retained{
  let value:Value=serde_json::from_slice(&std::fs::read(source.join(file["path"].as_str().unwrap())).unwrap()).unwrap();
  for (table,service)in [("IPForm","Ice.BO.InfoPromptFormSvc"),("RptStructuredOutputDef","Ice.BO.StructuredOutputDefinitionSvc"),("ZDataTable","Ice.BO.ZDataTableSvc"),("Results","Ice.BO.DynamicQuerySvc")]{if value["returnObj"].get(table).is_some(){native.insert(service.to_string(),value.clone());}}
 }
 let started=std::time::Instant::now();let plugin=crate::plugins::Plugin::load(&wasm).await.unwrap();let cancel=CancellationToken::new();
 let temporary=tempfile::tempdir().unwrap();let mut state=crate::storage::new_journal("local-performance".into());state.company=manifest["company"].as_str().unwrap().into();
 let mut storage=crate::storage::Storage::open(temporary.path(),false,state).unwrap();
 let mut request=GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"business-rules-and-mappings".into(),company:manifest["company"].as_str().unwrap().into(),identity:None,cursor:None,page_size:Some(100),state:json!({"_hostPreparePage":true}),response:None,error:None};
 let mut calls=vec![];let mut write_ms=0;let mut plugin_ms=0;let mut first_ready_ms=None;let mut items=std::collections::VecDeque::new();let mut after_items=None;let mut reads=0;
 loop{
  request.state["_hostIndependentWorkItems"]=json!(true);
  let input_bytes=serde_json::to_vec(&request).unwrap().len();let phase=request.state["phase"].clone();let op=request.operation.clone();let before=std::time::Instant::now();
  let reply=plugin.call(request.clone(),cancel.clone()).await.unwrap();let elapsed=before.elapsed().as_micros();plugin_ms+=elapsed;
  let output_bytes=serde_json::to_vec(&reply).unwrap().len();calls.push(json!({"operation":op,"phase":phase,"inputBytes":input_bytes,"outputBytes":output_bytes,"micros":elapsed}));
  match reply{
   GuestReply::Action{action,state}=>{reads+=1;request.state=state;request.state["_hostPreparePage"]=json!(true);let service=match action{HostAction::NativeRead{service,..}|HostAction::SharedNativeRead{service,..}=>service,other=>panic!("Unexpected {other:?}")};request.response=Some(native.get(&service).unwrap_or_else(||panic!("Missing retained response for {service}; available: {:?}",native.keys())).clone());}
   GuestReply::PreparedPage{state,..}=>{
    request.operation="materialize".into();request.state=state;request.response=None;
    if let Some((work,next))=crate::application::ready_output::independent_work(&mut request).unwrap(){
     items=work.into();after_items=Some(next);
     if let Some(state)=items.pop_front(){request.state=state;}else{
      let next=after_items.take().unwrap();if next.is_none(){break;}
      request.operation="discover".into();request.cursor=next;request.state=json!({"_hostPreparePage":true});
     }
    }
   }
   GuestReply::ReadyRecords{records,state}=>{if !records.is_empty()&&first_ready_ms.is_none(){first_ready_ms=Some(started.elapsed().as_millis());}let before=std::time::Instant::now();crate::collection_output::write_page(&mut storage,records).unwrap();write_ms+=before.elapsed().as_millis();request.state=state;request.response=None;}
   GuestReply::Inventory{mut next_cursor,authoritative,diagnostics,..}=>{
    assert!(authoritative&&diagnostics.is_empty());
    if after_items.is_some(){
     if let Some(state)=items.pop_front(){request.state=state;request.response=None;continue;}
     next_cursor=after_items.take().unwrap();
    }
    if next_cursor.is_none(){break;}request.operation="discover".into();request.cursor=next_cursor;request.state=json!({"_hostPreparePage":true});request.response=None;
   }
   other=>panic!("Unexpected {other:?}")
  }
 }
 assert_eq!(reads,4);assert_eq!(storage.journal.artifacts.len(),manifest["artifacts"].as_object().unwrap().len());
 assert_eq!(storage.journal.artifacts.iter().map(|a|a.files.len()).sum::<usize>(),manifest["artifacts"].as_object().unwrap().values().map(|a|a["files"].as_array().unwrap().len()).sum::<usize>());
 let mut file_count=0;
 for artifact in manifest["artifacts"].as_object().unwrap().values(){let identity:Identity=serde_json::from_value(artifact["identity"].clone()).unwrap();let produced=storage.journal.artifacts.iter().find(|a|a.identity==identity).unwrap();assert_eq!(produced.acquisition,Completeness::Collected);assert!(produced.diagnostics.is_empty());for expected in artifact["files"].as_array().unwrap(){let name=std::path::Path::new(expected["path"].as_str().unwrap()).file_name().unwrap();let actual=produced.files.iter().find(|f|std::path::Path::new(&f.path).file_name()==Some(name)).unwrap();assert_eq!(std::fs::read(storage.confined(&actual.path).unwrap()).unwrap(),std::fs::read(source.join(expected["path"].as_str().unwrap())).unwrap(),"{identity:?}/{name:?}");file_count+=1;}}
 let result=json!({"source":source,"wasm":wasm,"pluginSha256":plugin.hash,"elapsedMs":started.elapsed().as_millis(),"pluginMicros":plugin_ms,"writerMs":write_ms,"firstReadyMs":first_ready_ms,"calls":calls,"filesByteIdentical":file_count,"artifacts":storage.journal.artifacts.len(),"noNetwork":true});std::fs::write(report,serde_json::to_vec_pretty(&result).unwrap()).unwrap();println!("{}",json!({"elapsedMs":result["elapsedMs"],"pluginMicros":result["pluginMicros"],"firstReadyMs":result["firstReadyMs"],"filesByteIdentical":file_count}));
}
#[tokio::test]
#[ignore="Explicit compiled Business rules and mappings and private two-environment retained native responses"]
async fn business_rules_and_mappings_compiled_recovery_replay(){
 let base=PathBuf::from(std::env::var("HERBERT_BUSINESS_RULES_AND_MAPPINGS_REPLAY").unwrap());let wasm=PathBuf::from(std::env::var("HERBERT_BUSINESS_RULES_AND_MAPPINGS_WASM").unwrap());let mut plugin=crate::plugins::Plugin::load(&wasm).await.unwrap();let cancel=CancellationToken::new();let pool=crate::file_writers::WriterPool::default();
 let independent=std::env::var_os("HERBERT_BUSINESS_RULES_AND_MAPPINGS_INDEPENDENT_RECOVERY").is_some();
 for lane in ["2026.1","2025.2"]{
  let folder=base.join(lane);let manifest:Value=serde_json::from_slice(&std::fs::read(folder.join("replay-manifest.json")).unwrap()).unwrap();
  let read=|branch:&str,key:&str|->Value{let path=manifest[branch][key].as_str().unwrap();serde_json::from_slice(&std::fs::read(folder.join(manifest["collection"].as_str().unwrap()).join(path)).unwrap()).unwrap()};
  let temporary=tempfile::tempdir().unwrap();let mut journal=crate::storage::new_journal("business-rules-and-mappings-wasm-recovery".into());journal.company="TESTCO".into();let shared=Arc::new(Mutex::new(crate::storage::Storage::open(temporary.path(),false,journal).unwrap()));
  let mut r=GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"business-rules-and-mappings".into(),company:"TESTCO".into(),identity:None,cursor:None,page_size:Some(100),state:json!({"_hostPreparePage":true}),response:None,error:None};let mut retained=vec![];let mut reads=0;let mut restarts=0;let mut items=std::collections::VecDeque::new();let mut after_items=None;
  loop{
   r.state["_hostIndependentWorkItems"]=json!(independent);
   match plugin.call(r.clone(),cancel.clone()).await.unwrap(){
    GuestReply::Action{action,state}=>{r.state=serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();r.state["_hostPreparePage"]=json!(true);r.response=Some(match action{
     HostAction::NativeRead{service,method,..}if method=="GetRows"=>read("native",&service),
     HostAction::SharedNativeRead{service,method,..}if method=="GetRows"=>read("native",&service),
     HostAction::NativeRead{method,arguments,..}if method=="Execute"=>{reads+=1;read("queries",arguments["queryDS"]["DynamicQuery"][0]["QueryID"].as_str().unwrap())},
     other=>panic!("Unexpected Business rules and mappings request {other:?}")});}
    GuestReply::PreparedPage{state,..}=>{r.operation="materialize".into();r.state=state;r.response=None;let bytes=serde_json::to_vec(&r).unwrap();let stopped=CancellationToken::new();stopped.cancel();assert!(plugin.call(r.clone(),stopped).await.is_err());plugin=crate::plugins::Plugin::load(&wasm).await.unwrap();r=serde_json::from_slice(&bytes).unwrap();restarts+=1;
     if let Some((work,next))=crate::application::ready_output::independent_work(&mut r).unwrap(){items=work.into();after_items=Some(next);r.state=items.pop_front().expect("Populated test fixture work items");}
    }
    GuestReply::ReadyRecords{records,state}=>{
     assert!(records.iter().all(|(_,d)|d["herbertCollectionStatus"]["completeness"]=="collected"));retained.extend(records.clone());let stopped=CancellationToken::new();stopped.cancel();let before=shared.lock().unwrap().journal.artifacts.len();assert!(crate::collection_output::write_shared_page(shared.clone(),crate::collection_output::prepare_page(records.clone()).unwrap(),&pool,&stopped,|_|{}).is_err());assert_eq!(shared.lock().unwrap().journal.artifacts.len(),before);crate::collection_output::write_shared_page(shared.clone(),crate::collection_output::prepare_page(records).unwrap(),&pool,&cancel,|_|{}).unwrap();shared.lock().unwrap().checkpoint().unwrap();r.state=state;r.response=None;
    }
    GuestReply::Inventory{authoritative,mut next_cursor,diagnostics,..}=>{assert!(authoritative&&diagnostics.is_empty(),"{diagnostics:?}");
     if after_items.is_some(){if let Some(state)=items.pop_front(){r.state=state;r.response=None;continue;}next_cursor=after_items.take().unwrap();}
     if next_cursor.is_none(){break;}r.operation="discover".into();r.cursor=next_cursor;r.state=json!({"_hostPreparePage":true});r.response=None;}
    other=>panic!("Unexpected Business rules and mappings reply {other:?}")
   }
  }
  assert_eq!(reads,1);assert_eq!(retained.len(),5);assert_eq!(restarts,3);
  let storage=Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();let journal=storage.journal.clone();drop(storage);let mut storage=crate::storage::Storage::open(temporary.path(),false,journal).unwrap();let missing=storage.journal.artifacts.iter().flat_map(|a|&a.files).find(|f|f.path.ends_with(".xml")).unwrap().clone();let expected=std::fs::read(storage.confined(&missing.path).unwrap()).unwrap();std::fs::remove_file(storage.confined(&missing.path).unwrap()).unwrap();storage.reconcile_downloads().unwrap();crate::collection_output::write_page(&mut storage,retained).unwrap();assert_eq!(std::fs::read(storage.confined(&missing.path).unwrap()).unwrap(),expected);let mut count=0;for artifact in &storage.journal.artifacts{assert_eq!(artifact.acquisition,Completeness::Collected);for file in &artifact.files{storage.verify(file).unwrap();count+=1;}}
  let result=json!({"lane":lane,"tested":true,"pluginSha256":plugin.hash,"independentWorkItems":independent,"proof":"actual Business rules and mappings WASM with exclusively retained native test responses; actual host writer and storage reopen","cancelBeforeMaterialization":true,"cancelBeforeWriter":true,"freshGuestResumes":restarts,"missingXmlRestoredExactly":true,"artifacts":storage.journal.artifacts.len(),"files":count,"bulkQueries":reads});std::fs::write(folder.join(if independent{"compiled-recovery-independent.json"}else{"compiled-recovery.json"}),serde_json::to_vec_pretty(&result).unwrap()).unwrap();println!("{result}");drop(storage);temporary.close().unwrap();
 }
}
