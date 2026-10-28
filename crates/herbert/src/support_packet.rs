//! On-demand, allowlisted support export. Never copy logs, definitions or native payloads verbatim.
use herbert_contract::Diagnostic;
use serde_json::{Value,json};
use std::{collections::{BTreeMap,BTreeSet},io::{BufRead,BufReader,Read,Write},path::Path};
use zip::{ZipWriter,write::SimpleFileOptions};

#[derive(Default)]
struct Projection { aliases:BTreeMap<String,String> }
impl Projection {
    fn alias(&mut self,kind:&str,value:&str)->String {
        let key=format!("{kind}:{value}");
        let next=format!("{kind}-{}",self.aliases.len()+1);
        self.aliases.entry(key).or_insert(next).clone()
    }
    fn vocabulary()-> &'static BTreeSet<&'static str> {
        static WORDS:std::sync::OnceLock<BTreeSet<&'static str>>=std::sync::OnceLock::new();
        WORDS.get_or_init(||include_str!("support_vocabulary.txt").lines().collect())
    }
    fn label(&mut self,value:&Value)->Value {
        match value.as_str() {
            Some(s) if Self::vocabulary().contains(s)=>json!(s),
            Some(s)=>json!(self.alias("name",s)),_=>Value::Null,
        }
    }
    fn identity(&mut self,id:&Value)->Value {
        if !id.is_object(){return Value::Null;}
        let mut keys=serde_json::Map::new();
        for (k,v) in id["keys"].as_object().into_iter().flatten() {
            let name=self.label(&json!(k)).as_str().unwrap().to_owned();
            keys.insert(name,json!({"value":self.alias("key",&v.to_string()),"type":kind(v)}));
        }
        json!({"item":self.alias("item",&id.to_string()),"category":self.label(&id["category"]),
            "kind":self.label(&id["kind"]),"scope":self.label(&id["scope"]["type"]),"keys":keys})
    }
    fn structure(&mut self,value:&Value)->Value {
        match value {
            Value::Number(_)|Value::Bool(_)|Value::Null=>value.clone(),
            Value::String(_)=>self.label(value),
            Value::Array(a)=>Value::Array(a.iter().map(|v|self.structure(v)).collect()),
            Value::Object(o)=>Value::Object(o.iter().map(|(k,v)|{
                let name=self.label(&json!(k)).as_str().unwrap().to_owned();(name,self.structure(v))
            }).collect()),
        }
    }
    fn shape(&mut self,v:&Value)->Value {
        let mut out=serde_json::Map::new();
        for (table,shape) in v.as_object().into_iter().flatten() {
            let mut fields=serde_json::Map::new();
            for (field,types) in shape["fieldTypes"].as_object().into_iter().flatten() {
                let mut counts=serde_json::Map::new();
                for ty in ["null","boolean","number","string","array","object"] {if let Some(n)=types[ty].as_u64(){counts.insert(ty.into(),json!(n));}}
                fields.insert(self.label(&json!(field)).as_str().unwrap().into(),json!(counts));
            }
            out.insert(self.label(&json!(table)).as_str().unwrap().into(),json!({"rows":shape["rows"].as_u64(),"duplicateIds":shape["duplicateIds"].as_u64(),"fieldTypes":fields}));
        }Value::Object(out)
    }
    fn statistics(&mut self,v:&Value)->Value {
        let mut out=serde_json::Map::new();
        for key in ["elapsedMs","requests","requestMs","receivedBytes","pageWriteMs","checkpointMs","pages","peakBufferedMemoryBytesEstimate","spilledPages"] {
            if let Some(n)=v[key].as_u64(){out.insert(key.into(),json!(n));}
        }
        if let Some(done)=v["finished"].as_bool(){out.insert("finished".into(),json!(done));}
        let mut timings=serde_json::Map::new();
        for(category,operations)in v["workTimings"].as_object().into_iter().flatten(){
            let mut ops=serde_json::Map::new();for(op,counts)in operations.as_object().into_iter().flatten(){
                ops.insert(self.label(&json!(op)).as_str().unwrap().into(),json!({"calls":counts["calls"].as_u64(),"durationMicros":counts["durationMicros"].as_u64()}));
            }timings.insert(self.label(&json!(category)).as_str().unwrap().into(),json!(ops));
        }out.insert("workTimings".into(),json!(timings));Value::Object(out)
    }
    fn diagnostic(&mut self,d:&Value)->Value {
        // Only exact, authored static messages can leave the machine. Formatted
        // messages may contain business identifiers and are never copied.
        static MESSAGES:std::sync::OnceLock<BTreeSet<String>>=std::sync::OnceLock::new();
        let messages=MESSAGES.get_or_init(||serde_json::from_str(include_str!("support_messages.json")).expect("Static support message vocabulary"));
        let safe_message=d["message"].as_str().filter(|message|messages.contains(*message));
        let mut context=serde_json::Map::new();
        for key in ["category","service","method","operation","authMode","evidence","correlationId","responseContentType","table","httpStatus","stream","page",
            "rowIndex","rootCount","childCount","duplicateCount","sameContent","sameOwner","hasCompany",
            "sameNativeKey","check","container","byteLength","zipReadable","zipEntries","structure"] {
            if let Some(v)=d.get("context").and_then(|c|c.get(key)) {context.insert(key.into(),if key=="structure"{self.shape(v)}else if key=="correlationId"{v.as_str().and_then(|s|uuid::Uuid::parse_str(s).ok()).map(|id|json!(id.hyphenated().to_string())).unwrap_or(Value::Null)}else if key=="responseContentType"{match v.as_str(){Some("application/json"|"text/plain"|"text/html"|"application/problem+json"|"other")=>v.clone(),_=>Value::Null}}else{self.structure(v)});}
        }
        json!({"code":self.label(&d["code"]),"identity":self.identity(&d["identity"]),
            "retryable":d["retryable"].as_bool().unwrap_or(false),"context":context,
            "message":safe_message,"messageOmitted":safe_message.is_none(),"detail":self.alias("issue",&d.to_string())})
    }
    fn event(&mut self,event:&Value)->Value {
        let mut out=serde_json::Map::new();
        for key in ["event","category","plugin","operation","state","code","failureCode"] {
            if let Some(v)=event.get(key){out.insert(key.into(),self.label(v));}
        }
        for key in ["elapsedMs","unixMs","attempt","requestElapsedMs","responseBytes","retryDelayMs",
            "durationMicros","estimatedBytes","records","writeElapsedMs","checkpointElapsedMs","retainedResponseBytes"] {
            if let Some(v)=event.get(key).filter(|v|v.is_number()||v.is_null()) {out.insert(key.into(),v.clone());}
        }
        for key in ["route","rowCounts","structure"] {
            if let Some(v)=event.get(key){out.insert(key.into(),if key=="structure"{self.shape(v)}else{self.structure(v)});}
        }
        Value::Object(out)
    }
    fn summary(&mut self,j:&Value)->Value {
        let mut categories=serde_json::Map::new();
        for id in j["categories"].as_array().into_iter().flatten() {
            let name=self.label(id).as_str().unwrap_or("unknown").to_owned();
            categories.insert(name,json!({"outcome":self.label(&j["categoryOutcomes"][id.as_str().unwrap_or("")]),
                "discoveryFinished":j["discoveryFinished"][id.as_str().unwrap_or("")].as_bool()}));
        }
        let hashes:BTreeMap<_,_>=j["pluginHashes"].as_object().into_iter().flatten().filter_map(|(k,v)| {
            let s=v.as_str()?;if s.len()!=64||!s.bytes().all(|b|b.is_ascii_hexdigit()){return None;}
            Some((self.label(&json!(k)).as_str().unwrap().to_owned(),s.to_owned()))
        }).collect();
        let artifacts:Vec<_>=j["artifacts"].as_array().into_iter().flatten().map(|a| {
            let files:Vec<_>=a["files"].as_array().into_iter().flatten().map(|f|json!({
                "file":self.alias("file",f["path"].as_str().unwrap_or("")),"bytes":f["bytes"].as_u64(),
                "role":self.label(&f["role"]),"conflict":f["path"].as_str().is_some_and(|p|p.starts_with("Conflicts/"))})).collect();
            let diagnostics:Vec<_>=a["diagnostics"].as_array().into_iter().flatten().map(|d|self.diagnostic(d)).collect();
            json!({"identity":self.identity(&a["identity"]),"acquisition":self.label(&a["acquisition"]),
                "processing":self.label(&a["processing"]),"files":files,"diagnostics":diagnostics})
        }).collect();
        let diagnostics:Vec<_>=j["diagnostics"].as_array().into_iter().flatten().map(|d|self.diagnostic(d)).collect();
        let version=j["serverVersion"].as_str().filter(|s|!s.is_empty()&&s.split('.').all(|p|!p.is_empty()&&p.parse::<u64>().is_ok()));
        let mut settings=serde_json::Map::new();
        for (k,v) in j["settings"].as_object().into_iter().flatten() {
            if v.is_boolean()||v.is_number()||v.is_null(){settings.insert(self.label(&json!(k)).as_str().unwrap().into(),v.clone());}
        }
        json!({"schemaVersion":1,"sanitization":"allowlist-v1","hostVersion":env!("CARGO_PKG_VERSION"),
            "os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "run":self.alias("run",j["runId"].as_str().unwrap_or("")),"serverVersion":version,
            "authentication":self.label(&j["authMode"]),"phase":self.label(&j["phase"]),
            "collectionMethod":self.label(&j["collectionMethod"]),"temporaryAccess":j["temporaryAccessEnabled"].as_bool(),
            "categories":categories,"pluginHashes":hashes,"settings":settings,
            "statistics":self.statistics(&j["statistics"]),"artifacts":artifacts,"diagnostics":diagnostics,
            "omitted":["credentials","endpoint","company and user names","source code","business values","request arguments","response bodies","server error text","file names and paths","downloaded-file hashes","raw logs","alias mapping"]})
    }
}
fn kind(v:&Value)->&'static str {match v {Value::Null=>"null",Value::Bool(_)=>"boolean",Value::Number(_)=>"number",Value::String(_)=>"string",Value::Array(_)=>"array",Value::Object(_)=>"object"}}
fn problem()->Diagnostic {Diagnostic::new("SUPPORT_EXPORT_FAILED","The sanitized diagnostic packet could not be written. Choose a new ZIP filename in a writable folder.")}

pub(crate) fn export(journal:Value,root:&Path,destination:&Path)->Result<Value,Diagnostic> {
    let absolute=std::path::absolute(destination).map_err(|_|problem())?;
    let destination=absolute.as_path();
    if destination.exists(){return Err(Diagnostic::new("SUPPORT_EXPORT_EXISTS","Choose a new filename; existing files are preserved."));}
    let parent=destination.parent().filter(|p|p.is_dir()).ok_or_else(problem)?;
    let tmp=tempfile::NamedTempFile::new_in(parent).map_err(|_|problem())?;
    let mut zip=ZipWriter::new(tmp);let options=SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut projection=Projection::default();let mut summary=projection.summary(&journal);
    let host_hash=std::env::current_exe().ok().and_then(|p|std::fs::File::open(p).ok()).and_then(|mut f|{
        use sha2::{Digest,Sha256};let mut hash=Sha256::new();let mut buf=[0;65536];loop{let n=f.read(&mut buf).ok()?;if n==0{break;}hash.update(&buf[..n]);}Some(format!("{:x}",hash.finalize()))
    });summary["hostSha256"]=json!(host_hash);
    zip.start_file("summary.json",options).map_err(|_|problem())?;
    serde_json::to_writer_pretty(&mut zip,&summary).map_err(|_|problem())?;
    zip.start_file("events.jsonl",options).map_err(|_|problem())?;
    let directory=root.join("Herbert Data/diagnostics");let prefix=format!("{}-",journal["runId"].as_str().unwrap_or(""));
    let mut logs=0u64;let mut events=0u64;let mut invalid=0u64;
    if uuid::Uuid::parse_str(journal["runId"].as_str().unwrap_or("")).is_ok() && directory.exists() {
        let canonical=root.canonicalize().map_err(|_|problem())?;
        let mut entries=std::fs::read_dir(&directory).map_err(|_|problem())?.collect::<Result<Vec<_>,_>>().map_err(|_|problem())?;
        entries.sort_by_key(|e|e.file_name());
        for entry in entries {
            let name=entry.file_name();let Some(name)=name.to_str().filter(|n|n.starts_with(&prefix)&&n.ends_with(".jsonl")) else {continue;};
            let path=directory.join(name).canonicalize().map_err(|_|problem())?;
            if !path.starts_with(&canonical)||!path.is_file(){return Err(problem());}
            let file=std::fs::File::open(path).map_err(|_|problem())?;let length=file.metadata().map_err(|_|problem())?.len();logs+=1;
            // Snapshot the current length; do not wait for or reread an active log's tail.
            for line in BufReader::new(file.take(length)).lines() {
                let Ok(line)=line else {invalid+=1;continue;};
                let Ok(value)=serde_json::from_str::<Value>(&line) else {invalid+=1;continue;};
                let event=projection.event(&value);
                serde_json::to_writer(&mut zip,&event).map_err(|_|problem())?;zip.write_all(b"\n").map_err(|_|problem())?;events+=1;
            }
        }
    }
    zip.start_file("README.txt",options).map_err(|_|problem())?;
    write!(zip,"Herbert sanitized support packet\nAllowlist schema: 1\nLogs: {logs}; projected events: {events}; unreadable/partial records omitted: {invalid}\n\nNo downloaded files, source, business values, credentials, endpoint, company/user names, raw messages, original filenames/paths or alias lookup are included. Names are replaced with packet-local aliases. Unknown labels are aliased. Never include this packet's local alias mapping: it is intentionally not exported.\n\nThis is an observational snapshot; active runs may have changed while it was written. Older runs may lack version/context data. Native failures omit server text. Validated server diagnostic correlation IDs are retained for support lookup. No network request or native operation was performed to export this packet.\n").map_err(|_|problem())?;
    let tmp=zip.finish().map_err(|_|problem())?;tmp.as_file().sync_all().map_err(|_|problem())?;
    tmp.persist_noclobber(destination).map_err(|_|problem())?;
    Ok(json!({"saved":true,"path":destination,"sanitized":true,"schemaVersion":1,"logs":logs,"events":events,"omittedInvalidEvents":invalid}))
}

/// Capture only structure on a failed operation, never a value or complete response.
pub(crate) fn response_structure(value:&Value)->Value {
    let body=value.get("returnObj").unwrap_or(value);let mut tables=serde_json::Map::new();
    for (name,rows) in body.as_object().into_iter().flatten() {
        let Some(rows)=rows.as_array() else {continue;};let mut shapes=BTreeMap::<String,BTreeMap<&str,u64>>::new();let mut ids=BTreeSet::new();let mut duplicate_ids=0;
        for row in rows {for(k,v)in row.as_object().into_iter().flatten(){*shapes.entry(k.clone()).or_default().entry(kind(v)).or_default()+=1;}
            if let Some(id)=row.get("SysRowID"){if !ids.insert(id.to_string()){duplicate_ids+=1;}}
        }
        tables.insert(name.clone(),json!({"rows":rows.len(),"duplicateIds":duplicate_ids,"fieldTypes":shapes}));
    }Value::Object(tables)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packet_excludes_confidential_canaries_and_preserves_useful_structure() {
        let temp=tempfile::tempdir().unwrap();let id=uuid::Uuid::new_v4().to_string();let directory=temp.path().join("Herbert Data/diagnostics");std::fs::create_dir_all(&directory).unwrap();
        let secret="CONFIDENTIAL_PRODUCT_AND_PASSWORD_CANARY";
        let event=json!({"event":"requestFailed","plugin":"C02","route":["Ice.BO.UDMapSvc","GetRows"],"code":"SCHEMA_PAGE_UNVERIFIED","arguments":secret,"body":secret,"stage":secret,"responseBytes":100});
        std::fs::write(directory.join(format!("{id}-one.jsonl")),format!("{event}\n{{partial")).unwrap();
        let journal=json!({"authMode":"basicApiKey","settings":{"retryAttempts":3,"connectionTimeoutMs":10000},"runId":id,"endpoint":secret,"company":secret,"caller":secret,"phase":"failed","serverVersion":"5.2.100.9","categories":["C02"],"statistics":{"requests":3},
            "diagnostics":[{"code":"SCHEMA_PAGE_UNVERIFIED","message":secret,"nativeDetails":secret,"context":{"correlationId":"7f6e4f3f-bd79-4b76-b9a8-f3866aedc226","responseContentType":"text/plain","table":"UDMapColumn","httpStatus":400,"raw":secret,"structure":{"secretField":secret}}}],
            "artifacts":[{"identity":{"category":"C02","kind":"UDMapping","keys":{"MapTableID":secret}},"files":[{"path":secret,"sha256":secret,"bytes":12}]}]});
        let zip_path=temp.path().join("support.zip");let result=export(journal,temp.path(),&zip_path).unwrap();assert_eq!(result["omittedInvalidEvents"],1);
        let mut zip=zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();assert_eq!(zip.len(),3);
        let mut all=String::new();for i in 0..zip.len(){zip.by_index(i).unwrap().read_to_string(&mut all).unwrap();}
        assert!(!all.contains(secret));assert!(!all.contains("secretField"));assert!(all.contains("SCHEMA_PAGE_UNVERIFIED"));assert!(all.contains("UDMapColumn"));assert!(all.contains("5.2.100.9"));assert!(all.contains("400"));assert!(all.contains("basicApiKey"));assert!(all.contains("retryAttempts"));assert!(all.contains("connectionTimeoutMs"));assert!(all.contains("7f6e4f3f-bd79-4b76-b9a8-f3866aedc226"));assert!(all.contains("text/plain"));
        assert!(export(json!({}),temp.path(),&zip_path).is_err());
    }
    #[test]
    fn response_shape_never_copies_row_values() {
        let s=response_structure(&json!({"returnObj":{"UDMapColumn":[{"Seq":1,"Value":"secret","SysRowID":"same"},{"Seq":2,"Value":"other","SysRowID":"same"}]}}));
        assert_eq!(s["UDMapColumn"]["duplicateIds"],1);assert_eq!(s["UDMapColumn"]["fieldTypes"]["Value"]["string"],2);assert!(!s.to_string().contains("secret"));
    }
}

/// Enrich only error paths; successful pages do not scan row contents for diagnostics.
pub(crate) fn annotate(reply:&mut herbert_contract::GuestReply,category:&str,operation:&str,identity:Option<&herbert_contract::Identity>,response:Option<&Value>) {
    use herbert_contract::GuestReply;
    fn add(d:&mut Diagnostic,category:&str,operation:&str,id:Option<&herbert_contract::Identity>,response:Option<&Value>) {
        d.context.entry("category".into()).or_insert(json!(category));
        d.context.entry("operation".into()).or_insert(json!(operation));
        if d.identity.is_none(){d.identity=id.cloned();}
        if let Some(value)=response {d.context.entry("structure".into()).or_insert_with(||response_structure(value));}
    }
    match reply {
        GuestReply::State{reply,..}=>annotate(reply,category,operation,identity,response),
        GuestReply::Failure{diagnostic}=>add(diagnostic,category,operation,identity,response),
        GuestReply::Inventory{diagnostics,..}=>{
            let evidence=(diagnostics.len()>1 && response.is_some()).then(||uuid::Uuid::new_v4().to_string());
            for (index,d) in diagnostics.iter_mut().enumerate(){if let Some(e)=&evidence{d.context.insert("evidence".into(),json!(e));}add(d,category,operation,identity,if index==0{response}else{None});}
        },
        GuestReply::Definition{identity,diagnostics,..}=>{
            let evidence=(diagnostics.len()>1 && response.is_some()).then(||uuid::Uuid::new_v4().to_string());
            for (index,d) in diagnostics.iter_mut().enumerate(){if let Some(e)=&evidence{d.context.insert("evidence".into(),json!(e));}add(d,category,operation,Some(identity),if index==0{response}else{None});}
        },
        GuestReply::InventoryData{inventory,..}=>annotate(inventory,category,operation,identity,response),
        _=>{}
    }
}

#[cfg(test)]
mod performance_tests {
    use super::*;
    #[test]
    fn large_support_export_is_on_demand_and_streams_projected_events() {
        let t=tempfile::tempdir().unwrap();let id=uuid::Uuid::new_v4().to_string();let d=t.path().join("Herbert Data/diagnostics");std::fs::create_dir_all(&d).unwrap();
        let line=serde_json::to_string(&json!({"event":"requestFailed","category":"C17","responseBytes":4000,"arguments":"confidential-source".repeat(40)})).unwrap()+"\n";
        let mut file=std::fs::File::create(d.join(format!("{id}-test.jsonl"))).unwrap();for _ in 0..10000{file.write_all(line.as_bytes()).unwrap();}drop(file);
        let start=std::time::Instant::now();let result=export(json!({"runId":id}),t.path(),&t.path().join("support.zip")).unwrap();
        assert_eq!(result["events"],10000);
        println!("support export: 10000 events, {} input bytes, {} ms, {} ZIP bytes",line.len()*10000,start.elapsed().as_millis(),std::fs::metadata(t.path().join("support.zip")).unwrap().len());
    }
}

#[cfg(test)]
mod shared_evidence_tests {
    use super::*;
    #[test]
    fn support_zip_omits_private_selection_and_native_predicates() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("selection-support.zip");
        let secret = "private-selection-'%_λ";
        let frozen_error = crate::application::resolve_frozen_plugin_settings(
            &[herbert_contract::PluginSetting::filter("prefix", "ID starts with", secret)],
            &json!({}),
        ).unwrap_err();
        let journal = json!({"runId":uuid::Uuid::new_v4().to_string(),
            "pluginSettings":{"C01":{"prefix":secret}},
            "pluginSelectionContracts":{"C01":[{"description":secret}]},
            "diagnostics":[{"code":"SELECTION_FAILED","message":secret,"retryable":false,
                "context":{"filter":secret,"selection":{"prefix":secret},
                    "nativeClause":secret,"service":"Ice.BO.UserCodesSvc","method":"GetRows","page":2}},
                frozen_error]});
        export(journal, root.path(), &destination).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(destination).unwrap()).unwrap();
        for name in ["summary.json", "events.jsonl", "README.txt"] {
            let mut content = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut content).unwrap();
            assert!(!content.contains(secret));
            assert!(!content.contains("nativeClause"));
            assert!(!content.contains("pluginSettings"));
            assert!(!content.contains("pluginSelectionContracts"));
            if name == "summary.json" {
                let summary: Value = serde_json::from_str(&content).unwrap();
                assert_eq!(summary["diagnostics"][0]["context"]["page"], 2);
                assert!(summary["diagnostics"][0]["messageOmitted"].as_bool().unwrap());
                assert_eq!(summary["diagnostics"][1]["code"], "FROZEN_SELECTION_REQUIRED");
                assert_eq!(summary["diagnostics"][1]["message"], frozen_error.message);
                assert_eq!(summary["diagnostics"][1]["messageOmitted"], false);
            }
        }
    }
    #[test]
    fn many_diagnostics_share_one_structural_projection() {
        use herbert_contract::GuestReply;
        let mut reply=GuestReply::Inventory{identities:vec![],next_cursor:None,authoritative:false,diagnostics:(0..1000).map(|_|Diagnostic::new("SCHEMA_PAGE_UNVERIFIED","Owned regression")).collect()};
        annotate(&mut reply,"C02","discover",None,Some(&json!({"returnObj":{"UDMapColumn":[{"SysRowID":"same","Seq":1}]}})));
        let GuestReply::Inventory{diagnostics,..}=reply else {panic!()};
        assert_eq!(diagnostics.iter().filter(|d|d.context.contains_key("structure")).count(),1);
        let evidence=&diagnostics[0].context["evidence"];assert!(diagnostics.iter().all(|d|d.context.get("evidence")==Some(evidence)));
    }
}

#[cfg(test)]
mod builtin_label_tests {
    use super::*;
    #[test]
    fn builtin_error_and_token_route_remain_readable(){
        let mut projection=Projection::default();
        let value=projection.diagnostic(&json!({"code":"NATIVE_REFUSAL","message":"runtime value stays private","context":{"method":"TokenResource","service":"api","httpStatus":400}}));
        assert_eq!(value["code"],"NATIVE_REFUSAL");assert_eq!(value["context"]["method"],"TokenResource");assert_eq!(value["context"]["service"],"api");assert!(value["messageOmitted"].as_bool().unwrap());
    }
}
