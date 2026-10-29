//! Compiled Configurators guest replay through host WASM and real output writers.
use herbert_contract::*;
use serde_json::{Value,json};
use std::{path::PathBuf,sync::{Arc,Mutex}};
use tokio_util::sync::CancellationToken;
#[tokio::test]
#[ignore="Requires the explicit Configurators WASM candidate and private two-environment evidence"]
async fn configurators_compiled_page_output_replay() {
 let base=PathBuf::from(std::env::var("HERBERT_CONFIGURATORS_REPLAY").unwrap());
 let wasm=PathBuf::from(std::env::var("HERBERT_CONFIGURATORS_WASM").unwrap());
 let mut plugin=crate::plugins::Plugin::load(&wasm).await.unwrap();
 assert!(plugin.has_role("pagedDefinitions")&&plugin.has_role("prefetchPages"));
 let cancel=CancellationToken::new();
 let pool=crate::file_writers::WriterPool::default();
 for lane in ["2026.1","2025.2"] {
  let folder=base.join(lane);
  let read=|name:&str|->Value{serde_json::from_slice(&std::fs::read(folder.join(name)).unwrap()).unwrap()};
  let visible=read("bulk-two-root-complete/calls/001-GetRows.json")["value"].clone();
  let lookup=read("lookup-page/calls/001-GetRows.json")["value"].clone();
  let mut dictionary=read("dictionary/fields.json");dictionary.as_object_mut().unwrap().extend(read("bulk-supplement-dictionary/fields.json").as_object().unwrap().clone());
  let rows=read("bulk-two-root-complete/rows.json");
  let temporary=tempfile::tempdir().unwrap();
  let mut journal=crate::storage::new_journal("configurators-wasm-replay".into());journal.company="TESTCO".into();
  let shared=Arc::new(Mutex::new(crate::storage::Storage::open(temporary.path(),false,journal).unwrap()));
  let mut r=GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:"configurators".into(),company:"TESTCO".into(),identity:None,cursor:None,page_size:Some(100),state:json!({"_hostPreparePage":true}),response:None,error:None};
  let mut outputs=0;let mut bulk_reads=0;let mut pages=0;let mut retained_records=Vec::new();let mut restarted_pages=0;let mut kinds=std::collections::BTreeSet::new();
  loop {
   match plugin.call(r.clone(),cancel.clone()).await.unwrap() {
    GuestReply::Action {action,state}=>{
     r.state=state;r.state["_hostPreparePage"]=json!(true);
     r.response=Some(match action {
      HostAction::NativeRead {service,method,..} if method=="GetRows"=>if service=="Erp.BO.ConfiguratorDefSvc"{visible.clone()}else {assert_eq!(service,"Erp.BO.PcLookupTblSvc");lookup.clone()},
      HostAction::SharedNativeRead {method,arguments,..} if method=="GetFieldList"=>dictionary[arguments["dataTableID"].as_str().unwrap()]["value"].clone(),
      HostAction::NativeRead {method,arguments,..} if method=="Execute"=>{bulk_reads+=1;json!({"returnObj":{"Results":rows[arguments["queryDS"]["QueryTable"][0]["DBTableName"].as_str().unwrap()]}})},
      HostAction::SharedNativeRead {method,..} if method=="GetLicensedModuleAccess"=>read("signature-capability/001-GetLicensedModuleAccess.json")["value"].clone(),
      HostAction::SharedNativeRead {method,..} if method=="RetrievePcLayeredImagesForExport"=>json!({"parameters":{"configImportExportLayeredImageTableset":{"ConfigImportExportLayeredImage":[]}}}),
      HostAction::Query {query} | HostAction::SharedQuery {query}=>{
       assert_eq!(query.tables.len(),1);
       if query.tables[0].table=="PcInputVar" && query.criteria.iter().any(|c|c.field=="Company" && query.parameters[&c.parameter]=="TESTCO") {json!({"rows":[{"Company":"TESTCO","SysRowID":"00000000-0000-0000-0000-000000000019","VarName":"OfflineProbe","Source":"Ω & <preserved>"}]})}else{json!({"rows":[]})}
      },
      other=>panic!("Unexpected action {other:?}")
     });
    }
    GuestReply::PreparedPage {state,..}=>{
     r.operation="materialize".into();r.state=state;r.response=None;
     // Persist the continuation, discard the guest, and resume in a fresh runtime.
     let request_bytes=serde_json::to_vec(&r).unwrap();
     let stopped=CancellationToken::new();stopped.cancel();
     assert!(plugin.call(r.clone(),stopped).await.is_err());
     plugin=crate::plugins::Plugin::load(&wasm).await.unwrap();
     r=serde_json::from_slice(&request_bytes).unwrap();restarted_pages+=1;
    }
    GuestReply::ReadyRecords {records,state}=>{
     for (id,data) in &records {assert_eq!(data["herbertCollectionStatus"]["completeness"],json!(Completeness::Collected));kinds.insert(id.kind.clone());}
     outputs+=records.len();retained_records.extend(records.clone());
     let stopped=CancellationToken::new();stopped.cancel();
     let before=shared.lock().unwrap().journal.artifacts.len();
     assert!(crate::collection_output::write_shared_page(shared.clone(),crate::collection_output::prepare_page(records.clone()).unwrap(),&pool,&stopped,|_|{}).is_err());
     assert_eq!(shared.lock().unwrap().journal.artifacts.len(),before);
     crate::collection_output::write_shared_page(shared.clone(),crate::collection_output::prepare_page(records).unwrap(),&pool,&cancel,|_|{}).unwrap();
     shared.lock().unwrap().checkpoint().unwrap();
     r.state=state;r.response=None;
    }
    GuestReply::Inventory {authoritative,next_cursor,diagnostics,..}=>{
     assert!(authoritative,"{diagnostics:?}");pages+=1;
     if next_cursor.is_none(){break;}
     r.operation="discover".into();r.cursor=next_cursor;r.state=json!({"_hostPreparePage":true});r.response=None;
    }
    other=>panic!("Unexpected reply {other:?}")
   }
  }
  assert_eq!(bulk_reads,33);assert_eq!(outputs,4);assert!(kinds.contains("ConfiguratorLookup"));assert!(kinds.contains("PersistedPcInputVar"));
  let storage=Arc::try_unwrap(shared).ok().unwrap().into_inner().unwrap();let journal=storage.journal.clone();drop(storage);
  let mut storage=crate::storage::Storage::open(temporary.path(),false,journal).unwrap();
  assert_eq!(storage.journal.artifacts.len(),4);
  let mut files=0;for artifact in &storage.journal.artifacts {assert_eq!(artifact.acquisition,Completeness::Collected);for file in &artifact.files {storage.verify(file).unwrap();files+=1;}}
  assert!(restarted_pages>0);
  // Simulate a missing durable output after reopen, then replay retained Configurators records.
  let missing=storage.journal.artifacts.iter().flat_map(|a| &a.files).find(|f| f.path.ends_with(".xml")).unwrap().clone();
  let expected=std::fs::read(storage.confined(&missing.path).unwrap()).unwrap();
  std::fs::remove_file(storage.confined(&missing.path).unwrap()).unwrap();
  storage.reconcile_downloads().unwrap();
  crate::collection_output::write_page(&mut storage,retained_records).unwrap();
  assert_eq!(std::fs::read(storage.confined(&missing.path).unwrap()).unwrap(),expected);
  for artifact in &storage.journal.artifacts {assert_eq!(artifact.acquisition,Completeness::Collected);for file in &artifact.files{storage.verify(file).unwrap();}}
  let receipt=json!({"cancelBeforeMaterialization":true,"cancelBeforeWriter":true,"freshGuestResumes":restarted_pages,"missingXmlRestoredExactly":true,"lane":lane,"proof":"compiled Configurators WASM with retained test responses, synthetic empty images and persisted row; real shared output writer and reopen","outputs":outputs,"files":files,"pages":pages,"bulkReads":bulk_reads,"kinds":kinds});
  std::fs::write(folder.join("compiled-output-replay.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();println!("{receipt}");
  drop(storage);temporary.close().unwrap();
 }
}
