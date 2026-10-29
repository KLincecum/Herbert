//! Collect retained history and current stored packages without per-item reads.
//! Deployed layouts, stored current ZIPs, and historical ZIPs are distinct sources.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn fail(message: &str) -> GuestReply {
    crate::failure("REPORT_PACKAGE_PAGE_INVALID", message)
}
fn assisted(r: &GuestRequest) -> bool {
    r.state["_collectionMethod"] == "serverAssisted"
}
/// The prefix's leading run before any LIKE wildcard, bracket or non-ASCII character, for a
/// server-side `LIKE 'stem%'` superset that needs no escape syntax. Non-ASCII stops the stem
/// because the literal is not N-prefixed and could be converted lossily. None when nothing
/// narrows the read.
fn like_stem(prefix: &str) -> Option<&str> {
    let stem = &prefix[..prefix.find(|c: char| matches!(c, '%' | '_' | '[' | ']') || !c.is_ascii()).unwrap_or(prefix.len())];
    (!stem.is_empty()).then_some(stem)
}
pub fn methods() -> Vec<(String, String)> {
    [
        ("Ice.BO.CustomReportStoreHistorySvc", "GetRows"),
        ("Ice.BO.DynamicReportSvc", "GetSsrsConfigurationInformation"),
        ("Ice.Lib.SessionModSvc", "GetCurrentValuesFromServer"),
    ]
    .into_iter()
    .map(|(s, m)| (s.into(), m.into()))
    .collect()
}
pub fn handle(mut r: GuestRequest) -> GuestReply {
    if r.operation!="acquire" {if let Err(diagnostic)=crate::report_archive::prefix(&r,"packagePathPrefix"){return GuestReply::Failure {diagnostic};}}
    if r.operation == "materialize" {
        return materialize(r);
    }
    if let Some(diagnostic) = r.error.take() {
        if r.operation == "discover" && r.state["phase"] != "current" && assisted(&r) {
            return GuestReply::Inventory {
                identities: vec![],
                next_cursor: Some(json!({"stream":2}).to_string()),
                authoritative: false,
                diagnostics: vec![diagnostic],
            };
        }
        return GuestReply::Failure { diagnostic };
    }
    if r.operation == "acquire" {
        let Some(id) = r.identity.as_ref().filter(|id| {
            id.category == "report-styles-and-layouts"
                && id.kind == "PersistedReportHistory"
                && id.scope == Scope::Global
                && id.keys.len() == 2
        }) else {
            return fail("Select one retained report version.");
        };
        if id.keys.get("PrintProgram").is_none_or(String::is_empty)
            || id
                .keys
                .get("Version")
                .and_then(|v| v.parse::<i32>().ok())
                .is_none()
        {
            return fail("The retained report identity has invalid keys.");
        }
    }
    let cursor = match r
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => v.unwrap_or(json!({"stream":1,"page":1})),
        Err(_) => return fail("Herbert could not continue this download from its saved progress (report package position). Start the download again."),
    };
    let Some(stream @ 1..=2) = cursor["stream"].as_u64() else {
        return fail("Herbert could not continue this download from its saved progress (report package source). Start the download again.");
    };
    if !r.state.is_object() {
        r.state = json!({});
    }
    if stream == 2 {
        if !assisted(&r) {
            return fail("Current stored report packages can only be read with Server-assisted. Choose Server-assisted, then retry.");
        }
        if r.response.is_none() {
            let Some(size @ 1..=2147483647) = r.page_size else {
                return fail("Enter a page size from 1 to 2147483647.");
            };
            let after = cursor["after"].as_str().unwrap_or("");
            r.state["phase"] = json!("current");
            r.state["_progress"] = json!({"request":"Reading a batch of current stored report packages","response":"Stored packages received; writing their original ZIP files"});
            return GuestReply::Action {
                action: HostAction::HelperCall {
                    library: crate::report_store::contract().library_id,
                    function: "ReadPage".into(),
                    arguments: json!({"iCompany":r.company,"iPageSize":size,"iHasCursor":!after.is_empty(),"iAfter":after}),
                },
                state: r.state,
            };
        }
        let response = r.response.take().unwrap();
        if response["oSuccess"] != true {
            return fail("The current report package reader refused the page.");
        }
        let Some(rows) = response.pointer("/oDS/Store").and_then(Value::as_array) else {
            return fail("The current package page omitted its rows.");
        };
        let Some(c) = response
            .pointer("/oDS/Cursor")
            .and_then(Value::as_array)
            .filter(|v| v.len() == 1)
            .map(|v| &v[0])
        else {
            return fail("The current package page omitted its resume point.");
        };
        let next = match c["More"].as_bool() {
            Some(false) => Value::Null,
            Some(true) => {
                let Some(after) = c["After"].as_str().filter(|v| !v.is_empty()) else {
                    return fail("The package resume point has no key.");
                };
                if rows.last().and_then(|v| v["PrintProgram"].as_str()) != Some(after)
                    || cursor["after"] == after
                {
                    return fail("The package resume point did not advance.");
                }
                json!(json!({"stream":2,"after":after}).to_string())
            }
            None => return fail("The package resume point omitted its More flag."),
        };
        r.state["packageRows"] = json!(rows);
        r.state["packageStream"] = json!(stream);
        r.state["next"] = next;
        return prepare(r);
    }
    match r.state["phase"].as_str() {
        Some("config") => {
            let response = r.response.take().unwrap_or(Value::Null);
            let Some(root) = response
                .pointer("/parameters/rootFolder")
                .and_then(Value::as_str)
            else {
                return fail("The report configuration omitted its Epicor root.");
            };
            r.state["reportRoot"] = json!(format!("/{}/reports/", root.trim_matches('/')));
        }
        Some("session") => {
            let response = r.response.take().unwrap_or(Value::Null);
            let p = &response["parameters"];
            if p["companyID"] != r.company || !p["tenantID"].is_string() {
                return fail("The report session scope was not confirmed.");
            }
            r.state["tenant"] = p["tenantID"].clone();
        }
        Some("history") => {
            let response = r.response.take().unwrap_or(Value::Null);
            let Some(rows) = crate::body(&response)["CustomReportStoreHistory"].as_array() else {
                return fail("The report history page omitted its rows.");
            };
            let Some(more) = response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
            else {
                return fail("The history page omitted morePages.");
            };
            if more && rows.is_empty() {
                return fail("The report history page did not advance.");
            }
            r.state["packageRows"] = json!(rows);
            r.state["packageStream"] = json!(1);
            r.state["next"] = if r.operation == "acquire" {
                Value::Null
            } else if more {
                json!(json!({"stream":1,"page":cursor["page"].as_u64().unwrap_or(1)+1}).to_string())
            } else if assisted(&r) {
                json!(json!({"stream":2}).to_string())
            } else {
                Value::Null
            };
            return prepare(r);
        }
        _ => {}
    }
    if !r.state["reportRoot"].is_string() {
        r.state["phase"] = json!("config");
        return crate::native(
            "Ice.BO.DynamicReportSvc",
            "GetSsrsConfigurationInformation",
            json!({}),
            false,
            r.state,
        );
    }
    if !r.state["tenant"].is_string() {
        r.state["phase"] = json!("session");
        return crate::native(
            "Ice.Lib.SessionModSvc",
            "GetCurrentValuesFromServer",
            json!({}),
            false,
            r.state,
        );
    }
    let Some(size @ 1..=2147483647) = r.page_size else {
        return fail("Enter a page size from 1 to 2147483647.");
    };
    let Some(page @ 1..=2147483647) = cursor["page"].as_u64() else {
        return fail("Supply a positive Epicor history page.");
    };
    let quote = |v: &str| {
        v.replace('\'', "''")
            .replace('[', "[[]")
            .replace('%', "[%]")
            .replace('_', "[_]")
    };
    let root = r.state["reportRoot"].as_str().unwrap();
    let tenant = r.state["tenant"].as_str().unwrap();
    let custom = format!("{root}CustomReports/");
    let mut filter = if tenant.is_empty() {
        format!(
            "PrintProgram LIKE '{}%' AND NOT PrintProgram LIKE '{}Tenants/%'",
            quote(root),
            quote(&custom)
        )
    } else {
        format!(
            "PrintProgram LIKE '{}%' AND (NOT PrintProgram LIKE '{}%' OR PrintProgram LIKE '{}Tenants/{}/%')",
            quote(root),
            quote(&custom),
            quote(&custom),
            quote(tenant)
        )
    };
    if let Some(id) = r.identity.as_ref() {
        filter = format!(
            "({filter}) AND PrintProgram = '{}' AND Version = {}",
            id.keys["PrintProgram"].replace('\'', "''"),
            id.keys["Version"]
        );
    } else if r.operation != "acquire" {
        // packagePathPrefix matches PrintProgram itself, so this LIKE is a broad superset of the
        // exact case-sensitive starts_with in materialize. Settings are frozen per run, so
        // absolutePage paging stays stable. The current-package helper has no filter input.
        let prefix = crate::report_archive::prefix(&r, "packagePathPrefix").unwrap_or_default();
        if let Some(stem) = like_stem(&prefix) {
            filter = format!("({filter}) AND PrintProgram LIKE '{}%'", stem.replace('\'', "''"));
        }
    }
    r.state["phase"] = json!("history");
    r.state["_progress"] = json!({"request":"Reading a batch of retained report versions","response":"History batch received; writing original ZIP files"});
    crate::native(
        "Ice.BO.CustomReportStoreHistorySvc",
        "GetRows",
        json!({"whereClauseCustomReportStoreHistory":filter,"pageSize":size,"absolutePage":page}),
        false,
        r.state,
    )
}
fn prepare(r: GuestRequest) -> GuestReply {
    if r.state["_hostPreparePage"] == true {
        GuestReply::PreparedPage {
            next_cursor: r.state["next"].as_str().map(str::to_owned),
            next_state: crate::report_archive::with_settings(json!({"reportRoot":r.state["reportRoot"],"tenant":r.state["tenant"]}),&r.state),
            state: r.state,
        }
    } else {
        materialize(r)
    }
}
fn materialize(r: GuestRequest) -> GuestReply {
    let prefix=if r.operation=="acquire" {String::new()}else {match crate::report_archive::prefix(&r,"packagePathPrefix"){Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}}};
    let Some(rows)=r.state["packageRows"].as_array()else{return fail("The prepared package rows are missing.");};
    let current=r.state["packageStream"]==2;let mut seen=BTreeSet::new();let mut records=Vec::new();let mut diagnostics=Vec::new();
    for row in rows {
        let path=row["PrintProgram"].as_str().filter(|value|!value.is_empty());
        // Selection precedes validation: a row whose path misses the exact case-sensitive prefix is
        // read but neither validated nor diagnosed. A row without a path cannot be attributed, so it
        // stays selected and still fails. Exact acquire uses an empty prefix and checks every row.
        if !path.is_none_or(|path|path.starts_with(&prefix)) {continue;}
        let version=if current {Some(String::new())}else{row["Version"].as_i64().map(|value|value.to_string())};
        let id=path.zip(version).map(|(path,version)|Identity {category:"report-styles-and-layouts".into(),kind:if current {"CurrentCustomReportPackage"}else{"PersistedReportHistory"}.into(),scope:Scope::Global,keys:if current {BTreeMap::from([("PrintProgram".into(),path.into())])}else{BTreeMap::from([("PrintProgram".into(),path.into()),("Version".into(),version)])}});
        // A payload failure can preserve metadata only after native identity and
        // root/tenant admission. An identity in a diagnostic is not admission.
        if let Err(mut diagnostic)=admit(row,current,&r.state) {
            diagnostic.identity=id;
            if r.operation=="acquire" {return GuestReply::Failure {diagnostic};}
            diagnostics.push(diagnostic);continue;
        }
        if id.as_ref().is_some_and(|id|!seen.insert(id.keys.clone())) {let mut diagnostic=Diagnostic::new("REPORT_PACKAGE_PAGE_INVALID","The downloaded package page repeated an Epicor ID.");diagnostic.identity=id;diagnostics.push(diagnostic);continue;}
        // Run the existing native package materializer/validator for each selected
        // row. Only this row is copied; whole-page original byte payloads are not
        // cloned once per item.
        let single=GuestRequest {abi:r.abi,operation:"materialize".into(),category:r.category.clone(),company:r.company.clone(),identity:None,cursor:None,page_size:r.page_size,response:None,error:None,
            state:json!({"packageRows":[row],"packageStream":r.state["packageStream"],"reportRoot":r.state["reportRoot"],"tenant":r.state["tenant"],"next":null})};
        match materialize_page(single) {
            GuestReply::InventoryData {records:validated,inventory}=>{
                if let GuestReply::Inventory {diagnostics:errors,..}=*inventory {diagnostics.extend(errors);}
                records.extend(validated);
            },
            GuestReply::Failure {mut diagnostic}=>{
                diagnostic.identity=id.clone();diagnostics.push(diagnostic.clone());
                if let Some(identity)=id {
                    let mut metadata=row.clone();metadata.as_object_mut().unwrap().remove("Content");
                    records.push((identity,json!({"metadata":metadata,"snapshot":if current{"currentStoredPackage"}else{"retainedVersion"},"herbertCollectionStatus":{"completeness":"partial","diagnostics":[diagnostic]},"herbertDirectOutput":true,"herbertSources":{"files":[],"binaryFiles":[]}})));
                }
            },
            _=>return fail("The acquired package validation returned an invalid reply."),
        }
    }
    if r.operation=="acquire" {
        if records.len()!=1 || r.identity.as_ref()!=Some(&records[0].0){return fail("The retained version was not returned with its exact identity.");}
        let (identity,definition)=records.remove(0);return GuestReply::Definition {identity,definition,format:"native-report-package".into(),completeness:if diagnostics.iter().all(crate::report_resources::is_warning){Completeness::Collected}else{Completeness::Partial},diagnostics};
    }
    GuestReply::InventoryData {inventory:Box::new(GuestReply::Inventory {identities:records.iter().map(|(id,_)|id.clone()).collect(),next_cursor:r.state["next"].as_str().map(str::to_owned),authoritative:diagnostics.iter().all(crate::report_resources::is_warning),diagnostics}),records}
}
fn admit(row:&Value,current:bool,state:&Value)->Result<Identity,Diagnostic> {
    let error=|message|Diagnostic::new("REPORT_PACKAGE_PAGE_INVALID",message);
    let path=row["PrintProgram"].as_str().filter(|value|!value.is_empty()).ok_or_else(||error("A stored report package has no full path."))?;
    let mut keys=BTreeMap::from([("PrintProgram".into(),path.into())]);
    if !current {
        let version=row["Version"].as_i64().ok_or_else(||error("A historical package has no version."))?;
        keys.insert("Version".into(),version.to_string());
        let root=state["reportRoot"].as_str().unwrap_or("").to_lowercase();let tenant=state["tenant"].as_str().unwrap_or("").to_lowercase();let scoped_path=path.to_lowercase();let custom=format!("{root}customreports/");
        let allowed=scoped_path.starts_with(&root) && if tenant.is_empty(){!scoped_path.starts_with(&format!("{custom}tenants/"))}else{!scoped_path.starts_with(&custom)||scoped_path.starts_with(&format!("{custom}tenants/{tenant}/"))};
        if root.is_empty()||!allowed {return Err(error("A historical report package is outside the selected root or tenant."));}
    }
    Ok(Identity {category:"report-styles-and-layouts".into(),kind:if current{"CurrentCustomReportPackage"}else{"PersistedReportHistory"}.into(),scope:Scope::Global,keys})
}
fn materialize_page(r: GuestRequest) -> GuestReply {
    let current = r.state["packageStream"] == 2;
    let Some(rows) = r.state["packageRows"].as_array() else {
        return fail("The prepared package rows are missing.");
    };
    let mut records = Vec::new();
    let mut seen = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for row in rows {
        let id=match admit(row,current,&r.state){Ok(identity)=>identity,Err(diagnostic)=>return GuestReply::Failure {diagnostic}};
        let path=id.keys["PrintProgram"].as_str();
        if !seen.insert(id.keys.clone()) {return fail("The package page repeated an identity.");}
        let Some(content) = row["Content"].as_str() else {
            return fail("A stored report package omitted its original bytes.");
        };
        let bytes = match base64::engine::general_purpose::STANDARD.decode(content) {
            Ok(v) => v,
            Err(_) => return fail("Stored report bytes are not valid base64."),
        };
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if current
            && (row["Length"].as_u64() != Some(bytes.len() as u64)
                || row["SHA256"].as_str() != Some(&digest))
        {
            return fail("Current package bytes differ from their server hash or length.");
        }
        let mut metadata = row.clone();
        metadata.as_object_mut().unwrap().remove("Content");
        let validation=validate_zip(&bytes);
        let extension=if validation.is_ok(){"zip"}else{"bin"};
        let name = if current {format!("Current report.{extension}")}
            else {format!("Version-{}.{extension}",id.keys["Version"])};
        let binary_files = vec![crate::report_archive::binary(&name, &bytes)];
        let mut files = Vec::new();
        let mut record_diagnostics=Vec::new();
        if let Err(message) = validation {
            // A warning: every stored byte was saved (current packages also match Epicor's own hash and length),
            // so the record stays collected. The damage is in Epicor and is repaired there.
            let mut error=Diagnostic::new("REPORT_STORED_ZIP_INVALID",
                "This report package is damaged in Epicor: it is not a valid ZIP file, so it cannot be imported as is. Herbert saved all of its bytes as a .bin file. To repair it, re-save the report in Epicor.");
            error.identity=Some(id.clone());
            error.context.insert("path".into(),json!(path));
            error.context.insert("cause".into(),json!(message));
            error.context.insert("container".into(),json!(container(&bytes)));
            error.context.insert("byteLength".into(),json!(bytes.len()));
            let zip=zip::ZipArchive::new(std::io::Cursor::new(&bytes));
            error.context.insert("zipReadable".into(),json!(zip.is_ok()));
            if let Ok(zip)=zip {error.context.insert("zipEntries".into(),json!(zip.len()));}
            record_diagnostics.push(error.clone());diagnostics.push(error);
        }
        files.push(
            json!({"name":"Metadata.json","text":serde_json::to_string_pretty(&metadata).unwrap()}),
        );
        records.push((id,json!({"metadata":metadata,"snapshot":if current{"currentStoredPackage"}else{"retainedVersion"},"currentDeploymentComparison":false,"herbertCollectionStatus":{"completeness":if record_diagnostics.iter().all(crate::report_resources::is_warning){"collected"}else{"partial"},"diagnostics":record_diagnostics},"herbertOutputGroup":if current{"Stored packages"}else{"History"},"herbertDirectOutput":true,"herbertSources":{"files":files,"binaryFiles":binary_files}})));
    }
    if r.operation == "acquire" {
        if records.len() != 1 || r.identity.as_ref() != Some(&records[0].0) {
            return fail("The retained version was not returned with its exact identity.");
        }
        let (identity, definition) = records.remove(0);
        return GuestReply::Definition {
            identity,
            definition,
            format: "native-report-package".into(),
            completeness: if diagnostics.iter().all(crate::report_resources::is_warning) {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
        };
    }
    GuestReply::InventoryData {
        inventory: Box::new(GuestReply::Inventory {
            identities: records.iter().map(|(id, _)| id.clone()).collect(),
            next_cursor: r.state["next"].as_str().map(str::to_owned),
            authoritative: diagnostics.iter().all(crate::report_resources::is_warning),
            diagnostics,
        }),
        records,
    }
}
fn container(bytes:&[u8])->&'static str {
    if bytes.is_empty(){"empty"}else if bytes.starts_with(b"PK"){"zip"}
    else if bytes.starts_with(&[0x1f,0x8b]){"gzip"}
    else if bytes.iter().copied().skip_while(|b|b.is_ascii_whitespace()).next()==Some(b'<') || bytes.starts_with(&[0xef,0xbb,0xbf]){"xmlCandidate"}
    else {"unknown"}
}
fn validate_zip(bytes: &[u8]) -> Result<(), &'static str> {
    use std::io::Cursor;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "The stored file is not a readable ZIP.")?;
    let mut rdl = false;
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|_| "A ZIP member cannot be opened.")?;
        if file.is_dir() {
            continue;
        }
        rdl |= file.name().to_ascii_lowercase().ends_with(".rdl");
        let mut sink = std::io::sink();
        std::io::copy(&mut file, &mut sink)
            .map_err(|_| "A ZIP member failed its integrity check.")?;
    }
    if !rdl {
        return Err("The stored ZIP contains no RDL member.");
    }
    Ok(())
}

#[cfg(test)]
mod stored_format_tests {
    use super::*;
    fn zip_bytes()->Vec<u8>{
        use std::io::{Cursor,Write};let mut writer=zip::ZipWriter::new(Cursor::new(Vec::new()));writer.start_file("Main.rdl",zip::write::SimpleFileOptions::default()).unwrap();writer.write_all(b"<Report>original</Report>").unwrap();writer.finish().unwrap().into_inner()
    }
    fn package(path:&str,version:i32,bytes:&[u8])->Value {json!({"PrintProgram":path,"Version":version,"Content":base64::engine::general_purpose::STANDARD.encode(bytes),"Length":bytes.len(),"SHA256":format!("{:x}",Sha256::digest(bytes))})}
    fn request(current:bool,prefix:&str,rows:Vec<Value>)->GuestRequest {
        GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:"report-styles-and-layouts".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),response:None,error:None,state:json!({"packageRows":rows,"packageStream":if current{2}else{1},"reportRoot":"/Root/reports/","tenant":"tenant","next":"raw-next","_pluginSettings":{"reportIdPrefix":"UnrelatedReport","packagePathPrefix":prefix}})}
    }
    #[test]
    fn stored_path_selection_preserves_all_matching_versions_and_original_bytes_independent_of_styles() {
        let bytes=zip_bytes();let path="/Root/reports/CustomReports/Tenants/tenant/Alpha";let rows=vec![package(path,1,&bytes),package(path,2,&bytes),package("/Root/reports/CustomReports/Tenants/tenant/Beta",1,&bytes)];
        let GuestReply::InventoryData {records:all,..}=materialize(request(false,"",rows.clone()))else{panic!()};let GuestReply::InventoryData {records,inventory}=materialize(request(false,path,rows))else{panic!()};assert_eq!(records,all.into_iter().filter(|(id,_)|id.keys["PrintProgram"]==path).collect::<Vec<_>>());assert_eq!(records.len(),2);assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,next_cursor:Some(ref cursor),..} if cursor=="raw-next"));
        for (_,data) in records {assert_eq!(base64::engine::general_purpose::STANDARD.decode(data["herbertSources"]["binaryFiles"][0]["base64"].as_str().unwrap()).unwrap(),bytes);}
        let GuestReply::InventoryData {records,inventory}=materialize(request(true,path,vec![package(path,0,&bytes)]))else{panic!()};assert_eq!(records.len(),1);assert_eq!(records[0].0.kind,"CurrentCustomReportPackage");assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,..}));
    }
    #[test]
    fn invalid_packages_are_silent_when_unselected_and_incomplete_when_selected() {
        let bytes=zip_bytes();let good="/Root/reports/CustomReports/Tenants/tenant/Alpha";let bad="/Root/reports/CustomReports/Tenants/tenant/Other";
        for current in [false,true] {for defect in ["zip","base64","hash","scope","version","duplicate"] {
            let mut bad_row=package(bad,1,&bytes);
            match defect {"zip"=>bad_row=package(bad,1,b"PRIVATE_EXCLUDED_PAYLOAD"),"base64"=>bad_row["Content"]=json!("PRIVATE_EXCLUDED_PAYLOAD"),"hash"=>{bad_row["SHA256"]=json!("wrong");if !current{bad_row["Content"]=json!("invalid!");}},"scope"=>{if current{bad_row["Length"]=json!(0);}else{bad_row["PrintProgram"]=json!("/Other/reports/Foreign");}},"version"=>{if current{bad_row["Length"]=json!(0);}else{bad_row["Version"]=json!(false);}},_=>{}};
            let bad_path=bad_row["PrintProgram"].as_str().unwrap().to_owned();
            let mut rows=vec![package(good,1,&bytes),bad_row.clone()];if defect=="duplicate"{rows.push(bad_row);}
            // Unselected defects are read but silent and leave coverage authoritative.
            for prefix in [good,"Missing"] {
                let GuestReply::InventoryData {records,inventory}=materialize(request(current,prefix,rows.clone()))else{panic!()};assert_eq!(records.len(),usize::from(prefix==good));assert!(records.iter().all(|(id,_)|id.keys["PrintProgram"]==good));assert!(!serde_json::to_string(&records).unwrap().contains("PRIVATE_EXCLUDED_PAYLOAD"));
                let GuestReply::Inventory {authoritative,diagnostics,next_cursor,..}=*inventory else{panic!()};assert!(authoritative,"{current}/{defect}/{prefix}");assert!(diagnostics.is_empty(),"{current}/{defect}/{prefix}: {diagnostics:?}");assert_eq!(next_cursor.as_deref(),Some("raw-next"));
            }
            // Selected defects are reported without leaking the payload or emitting the unselected peer. A damaged ZIP
            // is a warning (every byte is saved), so its coverage stays authoritative; every other defect is incomplete.
            let GuestReply::InventoryData {records,inventory}=materialize(request(current,&bad_path,rows))else{panic!()};assert!(records.iter().all(|(id,_)|id.keys["PrintProgram"]==bad_path));let GuestReply::Inventory {authoritative,diagnostics,..}=*inventory else{panic!()};assert_eq!(authoritative,defect=="zip","{current}/{defect}");assert!(!diagnostics.is_empty());assert!(!serde_json::to_string(&diagnostics).unwrap().contains("PRIVATE_EXCLUDED_PAYLOAD"));
        }
        // A row without a path cannot be attributed to the prefix, so it still fails under any selection.
        let mut pathless=package(good,1,&bytes);pathless.as_object_mut().unwrap().remove("PrintProgram");
        let GuestReply::InventoryData {records,inventory}=materialize(request(current,"Missing",vec![pathless]))else{panic!()};assert!(records.is_empty());assert!(matches!(*inventory,GuestReply::Inventory {authoritative:false,ref diagnostics,..} if diagnostics.len()==1));
        }
    }
    #[test]
    fn history_discovery_pushes_package_path_like_superset_and_exact_history_ignores_it() {
        let clause=|r:GuestRequest|{let GuestReply::Action {action:HostAction::NativeRead {method,arguments,..},..}=handle(r)else{panic!()};assert_eq!(method,"GetRows");arguments["whereClauseCustomReportStoreHistory"].as_str().unwrap().to_owned()};
        let read=|prefix:&str|{let mut r=request(false,prefix,vec![]);r.state.as_object_mut().unwrap().remove("packageRows");r};
        let base=clause(read(""));assert!(!base.contains(") AND PrintProgram LIKE"));
        for (prefix,stem) in [("/Root/re%ports",Some("/Root/re")),("/Root/O'B_x",Some("/Root/O''B")),("/Root/Ωx",Some("/Root/")),("/Root/a[b]",Some("/Root/a")),("%x",None),("Ω",None)] {
            let expected=stem.map_or_else(||base.clone(),|stem|format!("({base}) AND PrintProgram LIKE '{stem}%'"));assert_eq!(clause(read(prefix)),expected,"{prefix}");
        }
        let mut r=read("Zed");r.operation="acquire".into();r.identity=Some(Identity {category:"report-styles-and-layouts".into(),kind:"PersistedReportHistory".into(),scope:Scope::Global,keys:BTreeMap::from([("PrintProgram".into(),"/Root/reports/A".into()),("Version".into(),"1".into())])});
        let exact=clause(r);assert!(exact.contains("PrintProgram = '/Root/reports/A' AND Version = 1"));assert!(!exact.contains("Zed"));
    }
    #[test]
    fn package_raw_continuation_frozen_settings_literal_paths_and_exact_history_ignore_discovery_filters() {
        let bytes=zip_bytes();let path="/Root/reports/CustomReports/Tenants/tenant/Alpha";let row=package(path,1,&bytes);
        for prefix in ["/root/reports/"," /Root/reports/","%"] {let GuestReply::InventoryData {records,inventory}=materialize(request(false,prefix,vec![row.clone()]))else{panic!()};assert!(records.is_empty());assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,next_cursor:Some(_),..}));}
        let mut r=request(false,"Missing",vec![]);r.state.as_object_mut().unwrap().remove("packageRows");r.state["phase"]=json!("history");r.state["_hostPreparePage"]=json!(true);r.cursor=Some(json!({"stream":1,"page":1}).to_string());r.response=Some(json!({"returnObj":{"CustomReportStoreHistory":[row.clone()]},"parameters":{"morePages":true}}));
        let GuestReply::PreparedPage {state,next_state,next_cursor}=handle(r.clone())else{panic!()};assert_eq!(state["packageRows"].as_array().unwrap().len(),1);assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap()["page"],2);
        r.operation="materialize".into();r.state=state;r.response=None;let GuestReply::InventoryData {records,inventory}=handle(r.clone())else{panic!()};assert!(records.is_empty());assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,..}));
        r.operation="acquire".into();r.identity=Some(Identity {category:"report-styles-and-layouts".into(),kind:"PersistedReportHistory".into(),scope:Scope::Global,keys:BTreeMap::from([("PrintProgram".into(),path.into()),("Version".into(),"1".into())])});r.state["_pluginSettings"]=json!({"reportIdPrefix":false,"packagePathPrefix":false});assert!(matches!(materialize(r),GuestReply::Definition {completeness:Completeness::Collected,..}));
    }
    #[test]
    fn rejected_history_root_or_tenant_never_emits_metadata_under_blank_matching_or_exact_selection() {
        let bytes=zip_bytes();let good="/Root/reports/CustomReports/Tenants/tenant/Alpha";
        for bad in ["/Other/reports/Foreign","/Root/reports/CustomReports/Tenants/foreign/Other"] {
            let mut rejected=package(bad,1,&bytes);rejected["PrivateMetadata"]=json!("REJECTED_SCOPE_METADATA");
            for prefix in ["",bad] {
                let GuestReply::InventoryData {records,inventory}=materialize(request(false,prefix,vec![package(good,1,&bytes),rejected.clone()]))else{panic!()};
                assert!(records.iter().all(|(identity,_)|identity.keys["PrintProgram"]!=bad),"Rejected scope became output: {bad}/{prefix}");assert!(!serde_json::to_string(&records).unwrap().contains("REJECTED_SCOPE_METADATA"));
                assert_eq!(records.len(),if prefix.is_empty(){1}else{0});let GuestReply::Inventory {authoritative,diagnostics,next_cursor,..}=*inventory else{panic!()};assert!(!authoritative);assert_eq!(next_cursor.as_deref(),Some("raw-next"));assert!(diagnostics.iter().any(|diagnostic|diagnostic.identity.as_ref().is_some_and(|identity|identity.keys["PrintProgram"]==bad)));assert!(!serde_json::to_string(&diagnostics).unwrap().contains("REJECTED_SCOPE_METADATA"));
            }
            let mut exact=request(false,"",vec![rejected]);exact.operation="acquire".into();exact.identity=Some(Identity {category:"report-styles-and-layouts".into(),kind:"PersistedReportHistory".into(),scope:Scope::Global,keys:BTreeMap::from([("PrintProgram".into(),bad.into()),("Version".into(),"1".into())])});
            assert!(matches!(materialize(exact),GuestReply::Failure {diagnostic} if diagnostic.identity.as_ref().is_some_and(|identity|identity.keys["PrintProgram"]==bad)),"Rejected exact scope must fail admission");
        }
    }
    #[test]
    fn unreadable_current_and_history_packages_preserve_exact_bytes_with_a_warning() {
        for current in [false,true] {
            let bytes=b"<unrecognized>exact original bytes</unrecognized>";
            let path="/Root/reports/CustomReports/Tenants/tenant/Fixture";
            let row=json!({"PrintProgram":path,"Version":3,"Content":base64::engine::general_purpose::STANDARD.encode(bytes),"Length":bytes.len(),"SHA256":format!("{:x}",Sha256::digest(bytes))});
            let mut r:GuestRequest=serde_json::from_value(json!({"abi":herbert_contract::PLUGIN_ABI,"operation":"discover","category":"report-styles-and-layouts","company":"C","state":{"packageRows":[row.clone()],"packageStream":if current{2}else{1},"reportRoot":"/Root/reports/","tenant":"tenant"}})).unwrap();
            let GuestReply::InventoryData{inventory,records}=materialize(r.clone()) else {panic!("expected preserved records")};
            let GuestReply::Inventory{diagnostics,authoritative,..}=*inventory else {panic!()};
            // Every stored byte is saved, so the damaged package is a collected record with a warning, not an incomplete one.
            assert!(authoritative);assert_eq!(diagnostics[0].identity.as_ref(),Some(&records[0].0));assert_eq!(diagnostics[0].code,"REPORT_STORED_ZIP_INVALID");
            assert!(diagnostics[0].message.contains("re-save the report in Epicor"));
            let data=&records[0].1;assert_eq!(data["herbertCollectionStatus"]["completeness"],"collected");
            assert_eq!(data["herbertCollectionStatus"]["diagnostics"][0]["code"],"REPORT_STORED_ZIP_INVALID");
            let file=&data["herbertSources"]["binaryFiles"][0];
            assert!(file["name"].as_str().unwrap().ends_with(".bin"));
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(file["base64"].as_str().unwrap()).unwrap(),bytes);
            assert_eq!(data["metadata"]["PrintProgram"],row["PrintProgram"]);
            r.operation="acquire".into();r.identity=Some(records[0].0.clone());
            let GuestReply::Definition{completeness,diagnostics,..}=materialize(r) else {panic!()};
            assert_eq!(completeness,Completeness::Collected);assert_eq!(diagnostics[0].code,"REPORT_STORED_ZIP_INVALID");
        }
    }
}
