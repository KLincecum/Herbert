//! Host-owned local worker preference. The legacy Functions value is migration input only.
use crate::plugin_state::StateFile;
use herbert_contract::Diagnostic;
use serde_json::{json,Value};
use std::path::Path;
const KEY:&str="localDecompilerWorkers";
fn error()->Diagnostic { Diagnostic::new("INVALID_DECOMPILER_WORKERS","The saved number of decompiler workers is not valid. Enter a whole number of 1 or more. Your other settings were kept.") }
pub(crate) fn validate(count:u32)->Result<(),Diagnostic>{
 if count==0 || count as u64>tokio::sync::Semaphore::MAX_PERMITS as u64 {Err(error())}else{Ok(())}
}
fn count(value:&Value)->Result<u32,Diagnostic>{let n=value.as_u64().and_then(|n|u32::try_from(n).ok()).ok_or_else(error)?;validate(n)?;Ok(n)}
fn file(root:&Path)->StateFile {StateFile::at(root.join("decompiler-preferences.json"))}
pub(crate) fn initialize(root:&Path,legacy:Option<&StateFile>)->Result<u32,Diagnostic>{
 let host=file(root);let before=host.read()?;
 let old=legacy.map(StateFile::read).transpose()?.unwrap_or(Value::Null);
 let selected=if !before.is_null(){count(&before[KEY])?}else if let Some(value)=old.pointer("/config/localDecompilerWorkers"){count(value)?}else{herbert_contract::default_local_decompiler_workers()};
 // Commit the host value first. If cleanup is interrupted, retry retains host precedence.
 if before.is_null(){std::fs::create_dir_all(root).map_err(|_|Diagnostic::new("SETTINGS_UNAVAILABLE","Cannot create host settings directory."))?;host.replace(&before,&json!({KEY:selected}))?;}
 if let Some(source)=legacy {let mut after=old.clone();if let Some(config)=after.get_mut("config").and_then(Value::as_object_mut){if config.remove(KEY).is_some(){source.replace(&old,&after)?;}}}
 Ok(selected)
}
pub(crate) fn write(root:&Path,selected:u32)->Result<(),Diagnostic>{
 validate(selected)?;let host=file(root);let before=host.read()?;
 if !before.is_null(){count(&before[KEY])?;}
 let mut after=if before.is_null(){json!({})}else{before.clone()};after[KEY]=json!(selected);
 std::fs::create_dir_all(root).map_err(|_|Diagnostic::new("SETTINGS_UNAVAILABLE","Cannot create host settings directory."))?;
 host.replace(&before,&after)
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn migration_preserves_saved_eight_and_unrelated_state_and_host_wins(){
  let t=tempfile::tempdir().unwrap();let legacy=StateFile::for_module(&t.path().join("Functions.wasm"));
  let original=json!({"selected":true,"config":{"localDecompilerWorkers":8,"itemsPerPage":100,"downloadCompiledAssemblies":true},"other":{"retain":1}});
  legacy.replace(&Value::Null,&original).unwrap();let root=t.path().join("host");
  assert_eq!(initialize(&root,Some(&legacy)).unwrap(),8);let mut expected=original;expected["config"].as_object_mut().unwrap().remove(KEY);assert_eq!(legacy.read().unwrap(),expected);
  write(&root,7).unwrap();let mut resurrected=expected.clone();resurrected["config"][KEY]=json!(2);legacy.replace(&expected,&resurrected).unwrap();
  assert_eq!(initialize(&root,Some(&legacy)).unwrap(),7);assert_eq!(initialize(&root,Some(&legacy)).unwrap(),7);assert_eq!(legacy.read().unwrap(),expected);
 }
 #[test] fn default_and_invalid_values_do_not_erase_settings(){
  let t=tempfile::tempdir().unwrap();assert_eq!(initialize(t.path(),None).unwrap(),4);
  assert!(write(t.path(),0).is_err());assert_eq!(initialize(t.path(),None).unwrap(),4);
  for bad in [json!(0),json!(-1),json!(1.5),json!(4294967296u64),json!("8")] {
   let d=tempfile::tempdir().unwrap();let old=StateFile::for_module(&d.path().join("Functions.wasm"));let before=json!({"config":{KEY:bad}});old.replace(&Value::Null,&before).unwrap();
   assert!(initialize(&d.path().join("host"),Some(&old)).is_err());assert_eq!(old.read().unwrap(),before);assert!(!d.path().join("host/decompiler-preferences.json").exists());
  }
 }
}

#[cfg(test)] mod host_tests {
 use super::*;
 use crate::application::{Host,Command};
 async fn call(host:&std::sync::Arc<Host>,method:&str,arguments:Value)->crate::application::Reply {host.execute(Command{schema_version:1,method:method.into(),arguments}).await}
 #[tokio::test] async fn host_choice_survives_restart_and_old_clients_preserve_it(){
  let t=tempfile::tempdir().unwrap();let root=t.path().join("state");let plugins=t.path().join("plugins");let h=Host::new(root.clone(),plugins.clone());
  let first=call(&h,"settings.inspect",json!({})).await;assert!(first.ok,"{:?}",first.error);assert_eq!(first.result.unwrap()["effective"][KEY],4);
  let mut setting=serde_json::to_value(herbert_contract::OperatingSettings::default()).unwrap();setting[KEY]=json!(8);
  assert!(call(&h,"settings.update",setting.clone()).await.ok);
  setting.as_object_mut().unwrap().remove(KEY);assert!(call(&h,"settings.update",setting.clone()).await.ok);
  assert_eq!(call(&h,"settings.inspect",json!({})).await.result.unwrap()["effective"][KEY],8);
  setting[KEY]=json!(0);assert!(!call(&h,"settings.update",setting).await.ok);assert_eq!(initialize(&root,None).unwrap(),8);
  for malformed in [json!([]),json!(5),json!("bad")] {assert!(!call(&h,"settings.update",malformed).await.ok);}
  let fresh=Host::new(root,plugins);assert_eq!(call(&fresh,"settings.inspect",json!({})).await.result.unwrap()["effective"][KEY],8);
  assert!(h.shutdown().await.is_empty());assert!(fresh.shutdown().await.is_empty());
 }
}

/// Inspect portable state first. Raw decompilation must not compile every guest
/// merely to find a one-time legacy preference.
pub(crate) fn legacy_candidates(root:&Path)->Result<Vec<std::path::PathBuf>,Diagnostic>{
 let mut folders=vec![root.to_path_buf()];let mut found=vec![];
 while let Some(folder)=folders.pop(){
  let entries=match std::fs::read_dir(&folder){Ok(entries)=>entries,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>continue,Err(_)=>return Err(Diagnostic::new("SETTINGS_UNAVAILABLE","Cannot inspect legacy plugin settings."))};
  for entry in entries {let entry=entry.map_err(|_|Diagnostic::new("SETTINGS_UNAVAILABLE","Cannot inspect legacy plugin settings."))?;let kind=entry.file_type().map_err(|_|Diagnostic::new("SETTINGS_UNAVAILABLE","Cannot inspect legacy plugin settings."))?;
   if kind.is_symlink(){continue;}
   if kind.is_dir(){folders.push(entry.path());}else if kind.is_file()&&entry.path().extension().is_some_and(|v|v.eq_ignore_ascii_case("wasm")){
    let state=StateFile::for_module(&entry.path()).read()?;
    if state.pointer("/config/localDecompilerWorkers").is_some(){found.push(entry.path());}
   }
  }
 }
 found.sort();Ok(found)
}
#[cfg(test)] mod discovery_tests {
 use super::*;
 #[test] fn migration_discovery_does_not_compile_modules_and_only_selects_saved_legacy_values(){
  let t=tempfile::tempdir().unwrap();std::fs::write(t.path().join("unrelated.wasm"),b"not wasm").unwrap();assert!(legacy_candidates(t.path()).unwrap().is_empty());
  let p=t.path().join("renamed.wasm");std::fs::write(&p,b"not compiled during discovery").unwrap();let state=StateFile::for_module(&p);state.replace(&Value::Null,&json!({"config":{KEY:8}})).unwrap();assert_eq!(legacy_candidates(t.path()).unwrap(),vec![p]);
 }
}
