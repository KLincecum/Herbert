//! Collect each inventory page with the native bulk library reader.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const SERVICE: &str = "Ice.Lib.EfxLibraryDesignerSvc";
pub fn methods() -> Vec<(String, String)> {
    [
        (SERVICE, "GetLibraries"),
        (SERVICE, "GetDefaults"),
        ("Ice.Lib.SessionModSvc", "GetSystemCodeApp"),
    ]
    .into_iter()
    .map(|(service, method)| (service.into(), method.into()))
    .collect()
}

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("FUNCTION_PAGE_INVALID", message)
}
fn discovery_prefix(state: &Value) -> Result<&str, Diagnostic> {
    match state.get("_pluginSettings").and_then(|settings| settings.get("libraryIdPrefix")) {
        None => Ok(""),
        Some(Value::String(prefix)) => Ok(prefix),
        Some(_) => Err(Diagnostic::new("INVALID_SETTING", "Library ID starts with must be a string.")),
    }
}
/// Literal part of the prefix before any SQL LIKE metacharacter.
fn server_literal(prefix: &str) -> &str {
    prefix.split(['%', '_', '[', ']']).next().unwrap_or("")
}
fn read(
    service: &str,
    method: &str,
    arguments: Value,
    mut state: Value,
    phase: &str,
    label: &str,
) -> GuestReply {
    state["phase"] = json!(phase);
    state["_progress"] =
        json!({"request":label,"response":"Function data received; preparing local backups"});
    GuestReply::Action {
        action: HostAction::SharedNativeRead {
            service: service.into(),
            method: method.into(),
            arguments,
            export: false,
        },
        state,
    }
}
pub fn handle(request: GuestRequest) -> GuestReply {
    match collect(request) {
        Ok(reply) => reply,
        Err(diagnostic) => GuestReply::Failure { diagnostic },
    }
}

fn collect(mut request: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if request.operation == "discover" {
        discovery_prefix(&request.state)?;
    }
    #[cfg(feature="function-compiled")]
    if crate::function_compiled::queue_active(&request.state) {return crate::function_compiled::queue(&request);}
    #[cfg(feature="function-compiled")]
    if request.state["phase"]=="compiledStart" {return crate::function_compiled::start_saved(&request.state);}
    #[cfg(feature="function-compiled")]
    if request.state["phase"]=="compiledLibraries" {
        let state=if request.operation=="materialize" && request.state["_compiledDefinitions"].is_null(){request.state.clone()}else{crate::function_compiled::received(&request)};
        // Retain old prepared-page compatibility for interrupted earlier hosts.
        if !state["_compiledDefinitions"].is_null(){return crate::function_compiled::finish_saved(&state);}
        let reply=finish(&state,&state["pageDataset"])?;
        return crate::function_compiled::prepare(reply,&state,&request.operation);
    }
    if request.operation == "materialize" {
        request.state["_hostPreparePage"] = json!(false);
        let reply=finish(&request.state, &request.state["pageDataset"])?;
        #[cfg(feature="function-compiled")]
        return crate::function_compiled::save_first(reply,&request.state,&request.operation);
        #[cfg(not(feature="function-compiled"))]
        return Ok(reply);
    }
    if let Some(error) = request.error.take() {
        return Err(error);
    }
    let response = request.response.as_ref().unwrap_or(&Value::Null);
    match request.state["phase"].as_str().unwrap_or("initial") {
        "initial" => Ok(read(
            "Ice.Lib.SessionModSvc",
            "GetSystemCodeApp",
            json!({}),
            json!({}),
            "system",
            "Reading the Function export system",
        )),
        "system" => {
            let system = response["returnObj"]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("The server did not identify its export system."))?;
            Ok(read(
                SERVICE,
                "GetDefaults",
                json!({}),
                json!({"system":system}),
                "version",
                "Reading the native Function export version",
            ))
        }
        "version" | "nextPage" => {
            let version = if request.state["phase"] == "nextPage" {
                &request.state["version"]
            } else { &response["returnObj"]["EfxLibrary"][0]["EpicorVersion"] };
            let version = version
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("The server did not return its Function export version."))?;
            let mut state = request.state.clone();
            state["version"] = json!(version);
            if request.operation == "acquire" {
                let identity = request
                    .identity
                    .as_ref()
                    .ok_or_else(|| invalid("Select a Function library from the inventory."))?;
                let id = identity
                    .keys
                    .get("LibraryID")
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| invalid("The selected library has no identity."))?;
                state["ids"] = json!([id]);
                state["targeted"] = json!(true);
                return Ok(read(
                    SERVICE,
                    "GetLibraries",
                    json!({"libraryIds":[id]}),
                    state,
                    "libraries",
                    "Downloading the selected Function library",
                ));
            }
            state["phase"] = json!("inventory");
            state["_progress"] = json!({"request":"Finding the next page of Function libraries",
                "response":"Library list received; downloading complete definitions"});
            match crate::query_library(&request) {
                GuestReply::Action { mut action, .. } => {
                    // Server LIKE is a broad superset; the exact local
                    // starts_with check below still decides selection.
                    let literal = server_literal(discovery_prefix(&request.state)?);
                    if let (HostAction::Query { query }, false) = (&mut action, literal.is_empty()) {
                        query.parameters.insert("prefix".into(), json!(format!("{literal}%")));
                        query.criteria.push(QueryCriterion {
                            table: "EcfSource".into(),
                            field: "LibraryID".into(),
                            operator: "LIKE".into(),
                            parameter: "prefix".into(),
                        });
                    }
                    Ok(GuestReply::Action { action, state })
                }
                GuestReply::Failure { diagnostic } => Err(diagnostic),
                _ => Err(invalid("The Function inventory route is invalid.")),
            }
        }
        "inventory" => {
            let rows = response["rows"]
                .as_array()
                .ok_or_else(|| invalid("The library inventory is missing."))?;
            // Select before validating: unselected rows produce no diagnostics.
            // Keep native continuation even when this page selects no libraries.
            let prefix = discovery_prefix(&request.state)?;
            let mut ids = Vec::new();
            let mut seen = BTreeSet::new();
            for row in rows {
                if crate::system_artifacts::marked(row) || temporary_helper(row) {
                    continue;
                }
                if !prefix.is_empty() && !row["LibraryID"].as_str().is_some_and(|id| id.starts_with(prefix)) {
                    continue;
                }
                let id = row["LibraryID"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| invalid("An inventory row has no library identity."))?;
                if !seen.insert(id) {
                    return Err(invalid("The inventory repeats a library identity."));
                }
                ids.push(id);
            }
            let mut state = request.state.clone();
            state["ids"] = json!(ids);
            state["nextCursor"] = response["nextCursor"].clone();
            if ids.is_empty() {
                return finish(&state, &json!({}));
            }
            Ok(read(
                SERVICE,
                "GetLibraries",
                json!({"libraryIds":ids}),
                state,
                "libraries",
                &format!("Downloading {} complete Function libraries", ids.len()),
            ))
        }
        "libraries" => {
            // Prefetch ends with native definitions. Optional binary acquisition
            // continues only after these records reach the local output queue.
            #[cfg(feature="function-compiled")]
            if request.operation=="acquire" {request.state["pageDataset"]=response["returnObj"].clone();}
            let reply=finish(&request.state,&response["returnObj"])?;
            #[cfg(feature="function-compiled")]
            return crate::function_compiled::save_first(reply,&request.state,&request.operation);
            #[cfg(not(feature="function-compiled"))]
            Ok(reply)
        },
        _ => Err(invalid("The Function page saved progress is invalid.")),
    }
}

fn finish(state: &Value, dataset: &Value) -> Result<GuestReply, Diagnostic> {
    if state["_hostPreparePage"] == true && state["targeted"] != true {
        let mut prepared = state.clone();
        prepared["pageDataset"] = dataset.clone();
        return Ok(GuestReply::PreparedPage {
            state: prepared,
            next_state: json!({"phase":"nextPage","version":state["version"],"system":state["system"]}),
            next_cursor: state["nextCursor"].as_str().map(str::to_owned),
        });
    }
    let ids = state["ids"]
        .as_array()
        .ok_or_else(|| invalid("The Function page has no identities."))?;
    let mut identities = Vec::new();
    let mut records = Vec::new();
    let mut diagnostics = Vec::new();
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| invalid("A library identity is invalid."))?;
        // Check the complete definition too. A saved page or targeted request
        // can bypass inventory, and helper metadata can change between reads.
        if dataset["EfxLibrary"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| row["LibraryID"] == id && temporary_helper(row))
        }) {
            continue;
        }
        let identity = Identity {
            category: "functions".into(),
            kind: "EfxLibrary".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([("LibraryID".into(), id.into())]),
        };
        identities.push(identity.clone());
        let result = crate::function_archive::build(
            dataset,
            id,
            state["version"].as_str().unwrap_or(""),
            state["system"].as_str().unwrap_or(""),
        );
        match result {
            Ok(export) => {
                // Epicor detects EFXJ by its UTF-8 marker. JSON without this
                // marker parses locally but fails native ImportLibrary.
                let mut bytes = vec![0xef, 0xbb, 0xbf];
                serde_json::to_writer_pretty(&mut bytes, &export)
                    .map_err(|_| invalid("The local Function export could not be encoded."))?;
                let functions = export["Library"]["Functions"].clone();
                // Keep native fields that are not part of the backup format in
                // the readable definition. Do not discard new server metadata.
                let native_tables: serde_json::Map<String, Value> = dataset
                    .as_object()
                    .ok_or_else(|| invalid("The library tableset is invalid."))?
                    .iter()
                    .filter_map(|(table, rows)| {
                        rows.as_array().map(|rows| {
                            (
                                table.clone(),
                                json!(
                                    rows.iter()
                                        .filter(|row| row["LibraryID"] == id)
                                        .collect::<Vec<_>>()
                                ),
                            )
                        })
                    })
                    .collect();
                let mut output =
                    json!({"export":export,"definition":native_tables,"herbertDirectOutput":true});
                for mut error in
                    crate::function_code::attach(&mut output, &json!({"EfxFunction":functions}))
                {
                    error.identity = Some(identity.clone());
                    diagnostics.push(error);
                }
                if !output["herbertSources"].is_object() {
                    output["herbertSources"] = json!({});
                }

                output["herbertSources"]["binaryFiles"] = json!([{
                    "name":format!("{}.efxj",crate::bpm_code::label(id)),"bytes":bytes.len(),
                    "sha256":format!("{:x}",Sha256::digest(&bytes)),
                    "base64":base64::engine::general_purpose::STANDARD.encode(bytes),
                }]);
                match crate::function_binary::build(&export) {
                    Ok(binary) => output["herbertSources"]["binaryFiles"].as_array_mut().unwrap().push(json!({
                        "name":format!("{}.efxb",crate::bpm_code::label(id)),"bytes":binary.len(),
                        "sha256":format!("{:x}",Sha256::digest(&binary)),
                        "base64":base64::engine::general_purpose::STANDARD.encode(binary),
                    })),
                    Err(mut error) => {
                        error.identity = Some(identity.clone());
                        diagnostics.push(error);
                    }
                }
                let owner_diagnostics: Vec<_> = diagnostics.iter()
                    .filter(|d| d.identity.as_ref() == Some(&identity)).cloned().collect();
                if !owner_diagnostics.is_empty() {
                    output["herbertCollectionStatus"] = json!({"completeness":"partial","diagnostics":owner_diagnostics});
                }
                records.push((identity, output));
            }
            Err(mut error) => {
                error.identity = Some(identity);
                diagnostics.push(error);
            }
        }
    }
    if state["targeted"] == true {
        let Some((identity, definition)) = records.pop() else {
            return Err(diagnostics
                .into_iter()
                .next()
                .unwrap_or_else(|| invalid("The selected library was not returned.")));
        };
        return Ok(GuestReply::Definition {
            identity,
            format: "native-function-export".into(),
            definition,
            completeness: if diagnostics.is_empty() {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
        });
    }
    Ok(GuestReply::InventoryData {
        records,
        inventory: Box::new(GuestReply::Inventory {
            identities,
            diagnostics: diagnostics.clone(),
            authoritative: diagnostics.is_empty(),
            next_cursor: state["nextCursor"].as_str().map(str::to_owned),
        }),
    })
}

/// Exclude Herbert infrastructure only when its name, generation and marker
/// agree. An H prefix alone can belong to a normal user library.
pub(super) fn temporary_helper(row: &Value) -> bool {
    let Some(generation) = row["GlobalID"].as_str() else { return false };
    let compact = generation.replace('-', "");
    if compact.len() != 32 || !compact.bytes().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    let id = row["LibraryID"].as_str().unwrap_or("");
    // The original installer used 26 GUID digits; the lease installer uses 29.
    let named = [26, 29].iter().any(|length| id == format!("H{}", &compact[..*length]));
    let notes = row["Notes"].as_str().unwrap_or("");
    named && (notes.lines().any(|line| line == format!("Herbert artifact {generation}"))
        || notes == format!("Herbert temporary helper; lease {generation}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prefix_request(rows: Vec<Value>, settings: Value, prepared: bool) -> GuestRequest {
        GuestRequest { abi: PLUGIN_ABI, operation: "discover".into(), category: "functions".into(),
            company: "C".into(), identity: None, cursor: None, page_size: Some(100),
            state: json!({"phase":"inventory","version":"5.2.100","system":"ERP",
                "_pluginSettings":settings,"_hostPreparePage":prepared}),
            response: Some(json!({"rows":rows,"nextCursor":"{\"after\":\"Other\"}"})), error: None }
    }
    fn complete_libraries(ids: &[&str]) -> Value {
        let mut data = json!({"EfxLibrary":[],"EfxFunction":[],"EfxFunctionSignature":[],
            "EfxRefAssembly":[],"EfxRefTable":[],"EfxRefService":[],"EfxRefLibrary":[],"EfxLibraryMapping":[]});
        for id in ids {
            data["EfxLibrary"].as_array_mut().unwrap().push(json!({"LibraryID":id,
                "GlobalID":"00112233-4455-6677-8899-aabbccddeeff","EpicorVersion":"5.2.100",
                "Revision":4,"Published":true,"Private":false,"Disabled":false,"Mode":1,"IsProtected":true,
                "AllowCustomCodeWidgets":true,"AllowCustomCodeFunctions":true,"DirectDBAccess":0,
                "OwnedByCompany":"","Owner":"","OriginalID":null,"Description":null,"Notes":null,
                "SharedWithGroup":null,"FutureNativeField":{"unchanged":"λ"}}));
            data["EfxFunction"].as_array_mut().unwrap().push(json!({"LibraryID":id,"FunctionID":"Complete",
                "Kind":2,"RequireTransaction":false,"SingleRowMode":false,"Private":false,
                "Disabled":false,"Invalid":false,"Thumbnail":null,"Description":null,"Notes":null,
                "Body":"{\"Code\":\"return;\",\"Usings\":\"\"}"}));
            data["EfxFunctionSignature"].as_array_mut().unwrap().push(json!({"LibraryID":id,"FunctionID":"Complete",
                "Response":false,"ParameterID":1,"ArgumentName":"input","Order":0,"DataType":"System.String",
                "Optional":false,"Description":"original","DataTypeInfo":null,"DefaultValue":null}));
            for (table,key,value) in [("EfxRefAssembly","Assembly","External.Assembly"),
                ("EfxRefTable","TableID","Erp.Part"),("EfxRefService","ServiceID","Erp.BO.Part"),
                ("EfxRefLibrary","LibraryRef","OutsidePrefix")] {
                data[table].as_array_mut().unwrap().push(json!({"LibraryID":id,(key):value,"Updatable":false}));
            }
            data["EfxLibraryMapping"].as_array_mut().unwrap().push(json!({"LibraryID":id,"Company":"OtherCompany","Allowed":true}));
        }
        data
    }
    fn requested_ids(reply: GuestReply) -> Vec<Value> {
        match reply {
            GuestReply::Action { action: HostAction::SharedNativeRead { method, arguments, .. }, .. } => {
                assert_eq!(method,"GetLibraries"); arguments["libraryIds"].as_array().unwrap().clone()
            }
            GuestReply::InventoryData { records, inventory } => {
                assert!(records.is_empty());
                assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,ref diagnostics,..} if diagnostics.is_empty()));
                Vec::new()
            }
            other => panic!("Unexpected selection reply: {other:?}"),
        }
    }
    #[test]
    fn literal_library_prefix_selects_before_validating_ids() {
        let ids = ["Sample","SampleChild","sample"," Sample ","'Quote","%Literal","_Literal","*Literal","λLib","éLib","e\u{301}Lib"];
        let mut rows: Vec<_> = ids.iter().map(|id|json!({"LibraryID":id})).collect();
        rows.push(json!({"LibraryID":"SampleSystem","SystemFlag":true}));
        rows.push(json!({"LibraryID":"H617b2d978aec40babb404183d2d8e",
            "GlobalID":"617b2d97-8aec-40ba-bb40-4183d2d8e654",
            "Notes":"Herbert artifact 617b2d97-8aec-40ba-bb40-4183d2d8e654"}));
        for (prefix, expected) in [("Sample",vec!["Sample","SampleChild"]),("sample",vec!["sample"]),
            (" Sample ",vec![" Sample "]),("'",vec!["'Quote"]),("%",vec!["%Literal"]),
            ("_",vec!["_Literal"]),("*",vec!["*Literal"]),("λ",vec!["λLib"]),
            ("é",vec!["éLib"]),("e\u{301}",vec!["e\u{301}Lib"]),("none",vec![])] {
            assert_eq!(requested_ids(handle(prefix_request(rows.clone(),json!({"libraryIdPrefix":prefix}),false))),json!(expected).as_array().unwrap().clone(),"{prefix:?}");
        }
        for settings in [json!({}),json!({"libraryIdPrefix":""})] {
            assert_eq!(requested_ids(handle(prefix_request(rows.clone(),settings,false))),json!(ids).as_array().unwrap().clone());
        }
        for bad in [Value::Null,json!(false),json!(2),json!([]),json!({})] {
            assert!(matches!(handle(prefix_request(rows.clone(),json!({"libraryIdPrefix":bad}),false)),
                GuestReply::Failure {diagnostic} if diagnostic.code=="INVALID_SETTING"));
        }
        for invalid_rows in [vec![json!({"LibraryID":"Other"}),json!({"LibraryID":"Other"})],vec![json!({"LibraryID":2})]] {
            assert!(matches!(handle(prefix_request(invalid_rows.clone(),json!({"libraryIdPrefix":""}),false)),
                GuestReply::Failure {diagnostic} if diagnostic.code=="FUNCTION_PAGE_INVALID"));
            // Unselected invalid rows are neither validated nor diagnosed.
            assert_eq!(requested_ids(handle(prefix_request(invalid_rows,json!({"libraryIdPrefix":"Sample"}),false))),Vec::<Value>::new());
        }
    }
    #[test]
    fn discovery_query_pushes_down_literal_like_prefix() {
        let mut request=prefix_request(vec![],json!({"libraryIdPrefix":"Ab%c"}),false);
        request.state["phase"]=json!("nextPage");request.response=None;request.cursor=Some("{\"after\":\"Aa\"}".into());
        let GuestReply::Action {action:HostAction::Query {query},..}=handle(request.clone()) else {panic!()};
        assert_eq!(query.parameters["prefix"],"Ab%");assert_eq!(query.parameters["after"],"Aa");
        assert!(query.criteria.iter().any(|c|c.operator=="LIKE"&&c.field=="LibraryID"&&c.parameter=="prefix"));
        for prefix in ["%x","","[a"] {
            request.state["_pluginSettings"]=json!({"libraryIdPrefix":prefix});
            let GuestReply::Action {action:HostAction::Query {query},..}=handle(request.clone()) else {panic!()};
            assert!(!query.parameters.contains_key("prefix"));assert!(query.criteria.iter().all(|c|c.operator!="LIKE"));
        }
    }
    #[test]
    fn empty_selected_library_page_keeps_native_continuation_to_later_match() {
        let request = prefix_request(vec![json!({"LibraryID":"Other"})],json!({"libraryIdPrefix":"Sample"}),true);
        let GuestReply::PreparedPage {state,next_state,next_cursor} = handle(request.clone()) else {panic!()};
        assert_eq!(state["ids"],json!([])); assert_eq!(next_cursor.as_deref(),Some("{\"after\":\"Other\"}"));
        let mut materialize=request.clone();materialize.operation="materialize".into();materialize.state=state;materialize.response=None;
        let GuestReply::InventoryData {records,inventory}=handle(materialize) else {panic!()};
        assert!(records.is_empty());assert!(matches!(*inventory,GuestReply::Inventory{next_cursor:Some(_),authoritative:true,..}));
        let mut next=request;next.state=next_state;next.state["_pluginSettings"]=json!({"libraryIdPrefix":"Sample"});next.cursor=next_cursor;next.response=None;
        let GuestReply::Action {action:HostAction::Query {query},state}=handle(next.clone()) else {panic!()};
        assert_eq!(query.parameters["after"],"Other");
        next.state=state;next.state["_pluginSettings"]=json!({"libraryIdPrefix":"Sample"});
        next.response=Some(json!({"rows":[{"LibraryID":"Sample"}],"nextCursor":null}));
        assert_eq!(requested_ids(handle(next)),json!(["Sample"]).as_array().unwrap().clone());
    }
    #[test]
    fn selected_library_preserves_complete_native_files_and_exact_acquire_ignores_prefix() {
        let data=complete_libraries(&["Sample","Other"]);
        let baseline=finish(&json!({"ids":["Sample","Other"],"version":"5.2.100","system":"ERP"}),&data).unwrap();
        let GuestReply::InventoryData {records:baseline,..}=baseline else {panic!()};
        let request=prefix_request(vec![json!({"LibraryID":"Sample"}),json!({"LibraryID":"Other"})],json!({"libraryIdPrefix":"Sample"}),false);
        let GuestReply::Action {action:HostAction::SharedNativeRead {arguments,..},state}=handle(request.clone()) else {panic!()};
        assert_eq!(arguments["libraryIds"],json!(["Sample"]));
        let mut libraries=request.clone();libraries.state=state;libraries.response=Some(json!({"returnObj":complete_libraries(&["Sample"])}));
        let GuestReply::InventoryData {records,..}=handle(libraries) else {panic!()};
        assert_eq!(records.len(),1);assert_eq!(records[0].1,baseline[0].1);
        for table in ["EfxLibrary","EfxFunction","EfxFunctionSignature","EfxRefAssembly","EfxRefTable","EfxRefService","EfxRefLibrary","EfxLibraryMapping"] {
            assert_eq!(records[0].1["definition"][table],json!([data[table][0]]));
        }
        assert_eq!(records[0].1["herbertSources"]["binaryFiles"].as_array().unwrap().len(),2);
        let mut exact=request;exact.operation="acquire".into();exact.identity=Some(Identity {category:"functions".into(),kind:"EfxLibrary".into(),scope:Scope::Global,keys:BTreeMap::from([("LibraryID".into(),"Other".into())])});
        exact.state["phase"]=json!("nextPage");exact.response=None;
        let GuestReply::Action {action:HostAction::SharedNativeRead {arguments,..},state}=handle(exact.clone()) else {panic!()};
        assert_eq!(arguments["libraryIds"],json!(["Other"]));
        exact.state=state;exact.response=Some(json!({"returnObj":complete_libraries(&["Other"])}));
        let GuestReply::Definition {identity,definition,completeness:Completeness::Collected,..}=handle(exact) else {panic!()};
        assert_eq!(identity.keys["LibraryID"],"Other");assert_eq!(definition,baseline[1].1);
    }
    #[cfg(feature="function-compiled")]
    #[test]
    fn selected_library_normal_files_precede_selected_only_compiled_capture() {
        let mut request=prefix_request(vec![json!({"LibraryID":"Sample"}),json!({"LibraryID":"Other"})],json!({"libraryIdPrefix":"Sample","downloadCompiledAssemblies":true}),true);
        request.state["_collectionMethod"]=json!("serverAssisted");
        request.state["_hostCapabilities"]=json!({"managedDecompilationV1":{"protocolVersion":1,"retainedArtifactFunctions":true}});
        let GuestReply::Action {state,..}=handle(request.clone()) else {panic!()};
        request.state=state;request.response=Some(json!({"returnObj":complete_libraries(&["Sample"])}));
        let GuestReply::PreparedPage {state,..}=handle(request.clone()) else {panic!()};
        request.operation="materialize".into();request.state=state;request.response=None;
        let GuestReply::ReadyRecords {records,state}=handle(request.clone()) else {panic!()};
        assert_eq!(records.len(),1);assert_eq!(records[0].0.keys["LibraryID"],"Sample");
        assert_eq!(records[0].1["herbertSources"]["binaryFiles"].as_array().unwrap().len(),2);
        assert_eq!(state["ids"],json!(["Sample"]));
        request.state=state;
        let GuestReply::Action {action:HostAction::RetainedArtifactFunctionCall {arguments,..},..}=handle(request) else {panic!()};
        assert_eq!(serde_json::from_str::<Value>(arguments["iLibraryIDs"].as_str().unwrap()).unwrap(),json!(["Sample"]));
    }

    #[test]
    fn helper_filter_requires_matching_identity_and_marker() {
        let generation = "617b2d97-8aec-40ba-bb40-4183d2d8e654";
        let mut row = json!({"LibraryID":"H617b2d978aec40babb404183d2d8e",
            "GlobalID":generation,"Notes":format!("\nHerbert artifact {generation}")});
        assert!(temporary_helper(&row));
        row["LibraryID"] = json!("H617b2d978aec40babb404183d2d8e654"[..30].to_owned());
        row["Notes"] = json!(format!("Herbert temporary helper; lease {generation}"));
        assert!(temporary_helper(&row));
        row["Notes"] = json!("User library with no helper marker");
        assert!(!temporary_helper(&row));
        row["Notes"] = json!(format!("Herbert artifact {generation}"));
        row["LibraryID"] = json!("HttpServerTest");
        assert!(!temporary_helper(&row));
        row["LibraryID"] = json!("H617b2d978aec40babb404183d2d8e");
        row["GlobalID"] = json!("00000000-0000-0000-0000-000000000000");
        assert!(!temporary_helper(&row));
    }

    #[test]
    #[ignore = "Requires an explicitly selected saved native response; no server calls."]
    fn saved_page_builds_nonzero_modes_and_excludes_helpers() {
        let path = std::env::var("HERBERT_FUNCTION_SAVED_RESPONSE").unwrap();
        let response: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let data = &response["returnObj"];
        let rows = data["EfxLibrary"].as_array().unwrap();
        let ids: Vec<_> = rows.iter().map(|row| row["LibraryID"].clone()).collect();
        let state = json!({"ids":ids,"version":"5.2.100","system":"ERP"});
        let GuestReply::InventoryData {records, inventory} = finish(&state, data).unwrap() else {panic!()};
        let GuestReply::Inventory {identities, diagnostics, ..} = *inventory else {panic!()};
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(records.len(), rows.iter().filter(|row| !temporary_helper(row)).count());
        assert_eq!(identities.len(), records.len());
        for (identity, output) in records {
            let id = &identity.keys["LibraryID"];
            let row = rows.iter().find(|row| row["LibraryID"] == *id).unwrap();
            assert!(!temporary_helper(row));
            assert_eq!(output["export"]["Library"]["Mode"], row["Mode"]);
            let functions = data["EfxFunction"].as_array().unwrap();
            for function in output["export"]["Library"]["Functions"].as_array().into_iter().flatten() {
                let original = functions.iter().find(|source| source["LibraryID"] == *id
                    && source["FunctionID"] == function["FunctionID"]).unwrap();
                assert_eq!(function["Body"], original["Body"]);
            }
            let files = output["herbertSources"]["binaryFiles"].as_array().unwrap();
            assert_eq!(files.len(), 2);
            for file in files {
                let bytes = base64::engine::general_purpose::STANDARD.decode(file["base64"].as_str().unwrap()).unwrap();
                assert_eq!(format!("{:x}", Sha256::digest(&bytes)), file["sha256"]);
            }
        }
    }
    #[test]
    #[ignore = "Requires the retained native Function tableset."]
    fn prepared_page_materializes_both_native_formats_without_more_reads() {
        let folder = std::path::PathBuf::from(std::env::var("HERBERT_FUNCTION_FIXTURE").unwrap());
        let data: Value = serde_json::from_slice(&std::fs::read(folder.join("function-native-widget-tables.json")).unwrap()).unwrap();
        let state = json!({"ids":["CaretProbeL1"],"version":"5.2.100","system":"ERP",
            "nextCursor":"next","_hostPreparePage":true});
        let GuestReply::PreparedPage { state, next_cursor, .. } = finish(&state, &data).unwrap() else { panic!() };
        assert_eq!(next_cursor.as_deref(), Some("next"));
        let request = GuestRequest { abi: PLUGIN_ABI, operation:"materialize".into(),
            category:"functions".into(),company:"TESTCO".into(),identity:None,cursor:None,
            page_size:Some(2),state,response:None,error:None };
        let GuestReply::InventoryData { records, inventory } = crate::handle("functions", request) else { panic!() };
        assert!(matches!(*inventory, GuestReply::Inventory { authoritative:true, .. }));
        let files = records[0].1["herbertSources"]["binaryFiles"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files[0]["name"].as_str().unwrap().ends_with(".efxj"));
        assert!(files[1]["name"].as_str().unwrap().ends_with(".efxb"));
    }

    #[test]
    #[ignore = "Requires the explicitly retained native Function comparison fixture."]
    fn targeted_read_builds_the_same_local_backup_without_inventory() {
        let folder = std::path::PathBuf::from(std::env::var("HERBERT_FUNCTION_FIXTURE").unwrap());
        let data: Value = serde_json::from_slice(
            &std::fs::read(folder.join("function-native-bulk-2026.json")).unwrap(),
        )
        .unwrap();
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "functions".into(),
            company: "TESTCO".into(),
            identity: Some(Identity {
                category: "functions".into(),
                kind: "EfxLibrary".into(),
                scope: Scope::Global,
                keys: BTreeMap::from([("LibraryID".into(), "CallLib".into())]),
            }),
            cursor: None,
            page_size: None,
            state: Value::Null,
            response: None,
            error: None,
        };
        for (expected, response) in [
            ("GetSystemCodeApp", json!({"returnObj":"ERP"})),
            (
                "GetDefaults",
                json!({"returnObj":{"EfxLibrary":[{"EpicorVersion":"5.2.100"}]}}),
            ),
            ("GetLibraries", json!({"returnObj":data})),
        ] {
            let GuestReply::Action {
                action: HostAction::SharedNativeRead { method, export, .. },
                state,
            } = handle(request.clone())
            else {
                panic!()
            };
            assert_eq!(method, expected);
            assert!(!export);
            request.state = state;
            request.response = Some(response);
        }
        let GuestReply::Definition {
            identity,
            definition,
            completeness: Completeness::Collected,
            diagnostics,
            ..
        } = handle(request)
        else {
            panic!()
        };
        assert_eq!(identity.keys["LibraryID"], "CallLib");
        assert!(diagnostics.is_empty());
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(
                definition["herbertSources"]["binaryFiles"][0]["base64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf]));
        let native: Value = serde_json::from_slice(
            &std::fs::read(folder.join("function-native-CallLib.efxj")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes[3..]).unwrap(),
            native
        );
    }
    #[test]
    #[ignore = "Requires the explicitly retained native Function comparison fixture."]
    fn native_page_builds_files_before_requesting_the_next_page() {
        let folder = std::path::PathBuf::from(std::env::var("HERBERT_FUNCTION_FIXTURE").unwrap());
        let data: Value = serde_json::from_slice(
            &std::fs::read(folder.join("function-native-bulk-2026.json")).unwrap(),
        )
        .unwrap();
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "functions".into(),
            company: "TESTCO".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: Value::Null,
            response: None,
            error: None,
        };
        for (expected, response) in [
            ("GetSystemCodeApp", json!({"returnObj":"ERP"})),
            (
                "GetDefaults",
                json!({"returnObj":{"EfxLibrary":[{"EpicorVersion":"5.2.100"}]}}),
            ),
        ] {
            let GuestReply::Action {
                action: HostAction::SharedNativeRead { method, .. },
                state,
            } = handle(request.clone())
            else {
                panic!()
            };
            assert_eq!(method, expected);
            request.state = state;
            request.response = Some(response);
        }
        let GuestReply::Action {
            action: HostAction::Query { query },
            state,
        } = handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(query.page_size, Some(2));
        request.state = state;
        request.response = Some(
            json!({"rows":[{"LibraryID":"CallLib"},{"LibraryID":"Ferry"}],"nextCursor":"next"}),
        );
        let GuestReply::Action {
            action:
                HostAction::SharedNativeRead {
                    method,
                    arguments,
                    export,
                    ..
                },
            state,
        } = handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(method, "GetLibraries");
        assert_eq!(arguments["libraryIds"], json!(["CallLib", "Ferry"]));
        assert!(!export);
        request.state = state;
        request.response = Some(json!({"returnObj":data}));
        let GuestReply::InventoryData { records, inventory } = handle(request) else {
            panic!()
        };
        assert_eq!(records.len(), 2);
        assert!(
            matches!(*inventory,GuestReply::Inventory {authoritative:true,next_cursor:Some(ref cursor),..} if cursor=="next")
        );
        for (identity, output) in records {
            let encoded = output["herbertSources"]["binaryFiles"][0]["base64"]
                .as_str()
                .unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            assert!(bytes.starts_with(&[0xef, 0xbb, 0xbf]));
            let export: Value = serde_json::from_slice(&bytes[3..]).unwrap();
            assert_eq!(export["LibraryId"], identity.keys["LibraryID"]);
            assert_eq!(output["export"], export);
        }
    }

    #[test]
    fn wave1_binary_failure_retains_valid_bad_valid_libraries_and_continuation() {
        let libraries: Vec<_> = ["First","Bad","Last"].iter().map(|id|json!({
            "LibraryID":id,"GlobalID":if *id=="Bad" {"invalid-guid"} else {"00112233-4455-6677-8899-aabbccddeeff"},
            "EpicorVersion":"5.2.100","Revision":1,"Published":true,"Private":false,"Disabled":false,
            "Mode":1,"IsProtected":true,"AllowCustomCodeWidgets":true,"AllowCustomCodeFunctions":true,
            "DirectDBAccess":0,"OwnedByCompany":"","Owner":"","OriginalID":null,"Description":null,"Notes":null,"SharedWithGroup":null
        })).collect();
        let data=json!({"EfxLibrary":libraries,"EfxFunction":[],"EfxFunctionSignature":[],"EfxRefAssembly":[],
            "EfxRefTable":[],"EfxRefService":[],"EfxRefLibrary":[],"EfxLibraryMapping":[]});
        let state=json!({"ids":["First","Bad","Last"],"version":"5.2.100","system":"ERP","nextCursor":"next"});
        let GuestReply::InventoryData {records,inventory}=finish(&state,&data).unwrap() else {panic!()};
        assert_eq!(records.len(),3);
        for (identity,output) in &records {
            let files=output["herbertSources"]["binaryFiles"].as_array().unwrap();
            assert_eq!(files.len(),if identity.keys["LibraryID"]=="Bad" {1} else {2});
            assert!(files[0]["name"].as_str().unwrap().ends_with(".efxj"));
            assert_eq!(output["definition"]["EfxLibrary"][0]["LibraryID"],identity.keys["LibraryID"]);
            if identity.keys["LibraryID"]=="Bad" {
                assert_eq!(output["herbertCollectionStatus"]["completeness"],"partial");
                assert_eq!(output["herbertCollectionStatus"]["diagnostics"][0]["identity"],json!(identity));
            } else {assert!(output.get("herbertCollectionStatus").is_none());}
        }
        let GuestReply::Inventory {identities,diagnostics,authoritative,next_cursor}=*inventory else {panic!()};
        assert_eq!(identities.len(),3);assert!(!authoritative);assert_eq!(next_cursor.as_deref(),Some("next"));
        assert_eq!(diagnostics.len(),1);assert_eq!(diagnostics[0].code,"FUNCTION_BINARY_INVALID");
        assert_eq!(diagnostics[0].identity.as_ref().unwrap().keys["LibraryID"],"Bad");
        let targeted=json!({"ids":["Bad"],"version":"5.2.100","system":"ERP","targeted":true});
        let GuestReply::Definition {completeness,definition,diagnostics,..}=finish(&targeted,&data).unwrap() else {panic!()};
        assert_eq!(completeness,Completeness::Partial);assert_eq!(diagnostics.len(),1);
        assert_eq!(definition["herbertSources"]["binaryFiles"].as_array().unwrap().len(),1);
    }

}
