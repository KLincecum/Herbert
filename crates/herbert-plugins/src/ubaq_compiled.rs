//! Optional UBAQ BPM binaries. Native association and naming belong to BAQs and UBAQs.
//! Local execution uses only the neutral host bridge; no Functions policy here.
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const DOWNLOAD: &str = "downloadCompiledAssemblies";
pub const DECOMPILE: &str = "decompileCompiledAssemblies";
const ALIAS: &str = "compiled_ubaq";
pub fn normalize(settings: &mut Value) {
    if settings[DECOMPILE] == true {
        settings[DOWNLOAD] = json!(true);
    }
}
pub fn requested(state: &Value) -> bool {
    let mut settings = state["_pluginSettings"].clone();
    normalize(&mut settings);
    settings[DOWNLOAD] == true
}
pub fn compatible(state: &Value) -> bool {
    state.pointer("/_hostCapabilities/localDecompilationV1/protocolVersion") == Some(&json!(1))
        && state.pointer("/_hostCapabilities/localDecompilationV1/retainedArtifactFunctions")
            == Some(&json!(true))
        && state.pointer("/_hostCapabilities/localDecompilationV1/engineId")
            == Some(&json!(MANAGED_ENGINE_ID))
}
pub fn binary_exports() -> Vec<BinaryExport> {
    serde_json::from_value(json!([
    {"service":ALIAS,"method":"ReadCompiled","pointer":"/oDS/Assemblies/*/Data","extension":"dll"},
    {"service":ALIAS,"method":"ReadCompiled","pointer":"/oDS/Assemblies/*/Symbols","extension":"pdb"}
])).expect("static UBAQ binary declarations")
}
fn digest(value: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("JSON value"))
    )
}
fn revision_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}
fn issue(code: &str, message: &str, owner: &Identity) -> Diagnostic {
    let mut d = Diagnostic::new(code, message);
    d.identity = Some(owner.clone());
    d.context.insert("processingIncomplete".into(), json!(true));
    d
}
pub fn begin(state: &Value) -> Option<GuestReply> {
    if !requested(state) || !compatible(state) || state["_collectionMethod"] == "directOnly" {
        return None;
    }
    let queries = state["_ubaqCompiledOwners"]
        .as_array()?
        .iter()
        .map(|entry| &entry["context"]["definition"]["DynamicQueryDesigner"][0])
        .map(|row| json!({"company":row["Company"],"query":row["QueryID"],"rowId":row["SysRowID"]}))
        .collect::<Vec<_>>();
    if queries.is_empty() {
        return None;
    }
    let mut state = state.clone();
    state["phase"] = json!("compiledUbaq");
    state["_progress"]["request"] = json!(
        "Reading original compiled UBAQ method assemblies; normal backups are saving independently"
    );
    state["_progress"]["response"] =
        json!("Original binaries retained; validating exact UBAQ method ownership");
    Some(GuestReply::Action {
        action: HostAction::RetainedLocalArtifactFunctionCall {
            alias: ALIAS.into(),
            function: "ReadCompiled".into(),
            arguments: json!({"iQueryIDs":serde_json::to_string(&queries).expect("query owners")}),
        },
        state,
    })
}
pub fn received(request: &GuestRequest) -> Value {
    let mut state = request.state.clone();
    if let Some(fields) = state.as_object_mut() {
        fields.remove("_ubaqCompiledError");
        fields.remove("_ubaqCompiledResult");
    }
    if let Some(error) = &request.error {
        state["_ubaqCompiledError"] = json!(error);
    } else {
        state["_ubaqCompiledResult"] = request.response.clone().unwrap_or(Value::Null);
    }
    state
}
fn witness(dataset: &Value) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for (table, tag) in [("BpMethod", "M"), ("BpArgument", "A"), ("BpDirective", "D")] {
        for row in dataset[table].as_array()? {
            let code = row["BpMethodCode"].as_str()?;
            let id = row["SysRowID"].as_str()?.to_lowercase();
            let revision = revision_text(&row["SysRevID"])?;
            values.push(format!("{tag}|{code}|{id}|{revision}"));
        }
    }
    values.sort();
    Some(values)
}
fn matching<'a>(rows: &'a Value, owner: &Identity) -> Vec<&'a Value> {
    rows.as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            row["Company"] == owner.keys["Company"] && row["QueryID"] == owner.keys["QueryID"]
        })
        .collect()
}
fn fallback(state: &Value, owner: &Identity) -> Option<Diagnostic> {
    if !compatible(state) {
        return Some(issue(
            "UBAQ_COMPILED_HOST_UNAVAILABLE",
            "This host cannot negotiate optional UBAQ compiled capture. Query definitions, authored BPMs and Epicor BAQ recovery files remain available.",
            owner,
        ));
    }
    if state["_collectionMethod"] == "directOnly" {
        return Some(issue(
            "UBAQ_COMPILED_CAPTURE_UNAVAILABLE",
            "Direct only preserves query definitions and authored BPMs. The hidden compiled store and Epicor runtime association require the declared optional reader.",
            owner,
        ));
    }
    if !state["_ubaqCompiledError"].is_null()
        || state.pointer("/_ubaqCompiledResult/oSuccess") != Some(&json!(true))
    {
        return Some(issue(
            "UBAQ_COMPILED_CAPTURE_UNAVAILABLE",
            "The optional reader could not verify Epicor user-visible UBAQ compiled snapshots. Query definitions, authored BPMs and Epicor BAQ recovery files remain available.",
            owner,
        ));
    }
    None
}
fn text_file(definition: &mut Value, name: String, value: &Value) {
    if !definition["herbertSources"]["files"].is_array() {
        definition["herbertSources"]["files"] = json!([]);
    }
    definition["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":name,"text":serde_json::to_string_pretty(value).expect("capture metadata")}));
}
fn evaluate(
    state: &Value,
    owner: &Identity,
    definition: &Value,
) -> (Vec<LocalDecompilationRequest>, Vec<Diagnostic>, Vec<Value>) {
    let mut jobs = Vec::new();
    let mut diagnostics = Vec::new();
    let native = &definition["supplements"]["updateMethods"];
    let definition_hash = definition["definitionSha256"].clone();
    let unavailable = fallback(state, owner);
    if let Some(diagnostic) = unavailable {
        diagnostics.push(diagnostic);
        return (jobs, diagnostics, vec![]);
    }
    let result = &state["_ubaqCompiledResult"]["oDS"];
    let owners = matching(&result["Owners"], owner);
    let methods = matching(&result["Methods"], owner);
    let assemblies = matching(&result["Assemblies"], owner);
    let native_methods = native["BpMethod"].as_array().cloned().unwrap_or_default();
    let observed = owners.first().copied().unwrap_or(&Value::Null);
    let expected_witness = witness(native);
    let observed_witness = observed["DefinitionWitness"]
        .as_str()
        .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok());
    let revision = &definition["definition"]["DynamicQueryDesigner"][0]["SysRevID"];
    let revision_text = revision_text(revision);
    let source_changed = expected_witness.is_none()
        || expected_witness != observed_witness
        || observed["DefinitionWitness"] != observed["DefinitionWitnessAfter"]
        || revision_text.as_deref() != observed["QuerySysRevID"].as_str()
        || observed["QuerySysRevID"] != observed["QuerySysRevIDAfter"];
    let mut seen_methods = BTreeSet::new();
    let complete_methods = methods.len() == native_methods.len()
        && methods.iter().all(|row| {
            row["BpMethodCode"].as_str().is_some_and(|code| {
                seen_methods.insert(code)
                    && native_methods
                        .iter()
                        .any(|m| m["BpMethodCode"] == code && m["Name"] == row["Method"])
            })
        });
    if owners.len() != 1
        || !complete_methods
        || observed["Methods"].as_u64() != Some(native_methods.len() as u64)
    {
        let diagnostic = issue(
            "UBAQ_COMPILED_OWNER_UNVERIFIED",
            "The reader did not return one complete exact query and method ownership observation. No compiled association or absence was claimed.",
            owner,
        );
        diagnostics.push(diagnostic);
        return (jobs, diagnostics, vec![]);
    }
    let mut observations = Vec::new();
    for method in methods {
        let code = method["BpMethodCode"].as_str().expect("validated method");
        let native_method = native_methods
            .iter()
            .find(|row| row["BpMethodCode"] == code)
            .expect("validated method");
        let expected_assembly = native_method["BusinessObject"].as_str().map(|alias| {
            format!(
                "Ice.Ubaq.{}.{}",
                alias.replace('/', "_").replace('_', "."),
                native_method["Name"].as_str().unwrap_or("")
            )
        });
        let method_folder = format!(
            "BPM/{} - {}",
            crate::bpm_code::label(method["Method"].as_str().unwrap_or("Method")),
            digest(&json!(code))
        );
        let rows = assemblies
            .iter()
            .filter(|row| row["BpMethodCode"] == code)
            .copied()
            .collect::<Vec<_>>();
        let mut method_issues = Vec::new();
        let mut captures = Vec::new();
        let mut seen_selectors = BTreeSet::new();
        let association_valid = method["Folder"] == "Ubaq"
            && expected_assembly.as_deref() == method["Assembly"].as_str()
            && method["BeforeRows"].as_u64() == Some(rows.len() as u64);
        let registration = method["Registration"]
            .as_str()
            .and_then(|v| serde_json::from_str::<Vec<Value>>(v).ok());
        let registration_valid = registration.as_ref().is_some_and(|rows| {
            rows.len() <= 1
                && rows.iter().all(|row| {
                    row["Type"] == "BPM.Ubaq"
                        && row["Target"] == code
                        && row["Scope"] == "##UBAQ##"
                        && (row["Assembly"].is_null()
                            || row["Assembly"] == ""
                            || row["Assembly"] == method["Assembly"])
                })
        });
        let changed = source_changed
            || method["Registration"] != method["RegistrationAfter"]
            || method["BeforeRows"] != method["AfterRows"];
        if !association_valid || !registration_valid {
            method_issues.push(issue("UBAQ_COMPILED_ASSOCIATION_INVALID", "The Epicor runtime or generated selector does not prove this exact UBAQ method association.", owner));
        }
        if changed {
            method_issues.push(issue("UBAQ_COMPILED_SOURCE_CHANGING", "Query, method, directive or runtime-registration revisions changed across the definition/capture observations. Original files are retained as evidence without a consistent-current-pair claim.", owner));
        }
        let absence = rows.is_empty()
            && association_valid
            && registration_valid
            && !changed
            && method["BeforeRows"] == 0
            && method["AfterRows"] == 0;
        if rows.is_empty() && !absence {
            method_issues.push(issue(
                "UBAQ_COMPILED_BINARY_UNAVAILABLE",
                "An empty response did not establish stable absence for this method.",
                owner,
            ));
        }
        for row in rows {
            let selector =
                json!({"folder":row["Folder"],"assembly":row["Assembly"],"version":row["Version"]});
            let valid = association_valid
                && registration_valid
                && row["Folder"] == method["Folder"]
                && row["Assembly"] == method["Assembly"]
                && row["Version"].is_string()
                && seen_selectors.insert(digest(&selector));
            let mut row_issues = Vec::new();
            if !valid {
                row_issues.push(issue("UBAQ_COMPILED_ASSOCIATION_INVALID", "A compiled row has an unexpected or repeated exact method selector. No local project was queued.", owner));
            }
            let input = serde_json::from_value::<FileRef>(row["Data"]["nativeFile"].clone()).ok();
            let symbols =
                serde_json::from_value::<FileRef>(row["Symbols"]["nativeFile"].clone()).ok();
            let row_changed = changed
                || row["SysRevID"] != row["BinarySysRevID"]
                || row["SysRevID"] != row["AfterSysRevID"];
            if row_changed && !changed {
                row_issues.push(issue("UBAQ_COMPILED_SOURCE_CHANGING", "The compiled store row changed during capture; its original receipt remains evidence.", owner));
            }
            if input.is_none() {
                row_issues.push(issue("UBAQ_COMPILED_BINARY_UNAVAILABLE", "The associated store row did not yield an original DLL receipt. Available evidence and symbols remain preserved.", owner));
            }
            let disposition = row["SymbolDisposition"]
                .as_str()
                .unwrap_or("unsupported-unused");
            if symbols.is_some() && disposition != "matched-portable" {
                row_issues.push(issue("UBAQ_COMPILED_SYMBOLS_UNUSED", "External symbols were retained but are not a verified matching portable PDB. They are not supplied to the decompiler.", owner));
            }
            let symbol_mode = if disposition == "matched-portable" && symbols.is_some() {
                ManagedSymbolMode::ExternalPortable
            } else if disposition.contains("embedded-candidate") {
                ManagedSymbolMode::EmbeddedPortable
            } else {
                ManagedSymbolMode::None
            };
            let binary_state = if !valid || input.is_none() {
                "failed"
            } else if row_changed {
                "sourceChanging"
            } else {
                "captured"
            };
            let capture_folder = format!("{method_folder}/Compiled/{}", digest(&selector));
            let project = state["_pluginSettings"][DECOMPILE] == true;
            if project && valid && !row_changed {
                if let Some(input) = &input {
                    jobs.push(LocalDecompilationRequest {
                        protocol_version: 1,
                        request_id: digest(&json!([
                            owner,
                            code,
                            selector,
                            input,
                            symbols,
                            definition_hash
                        ])),
                        expected_engine: MANAGED_ENGINE_ID.into(),
                        owner: owner.clone(),
                        input: input.clone(),
                        symbols: if symbol_mode == ManagedSymbolMode::ExternalPortable {
                            symbols.clone()
                        } else {
                            None
                        },
                        symbol_mode: symbol_mode.clone(),
                        output_subdirectory: format!("{capture_folder}/Decompiled"),
                    });
                }
            }
            let mut observation = row.clone();
            observation
                .as_object_mut()
                .expect("native row")
                .remove("Data");
            observation.as_object_mut().unwrap().remove("Symbols");
            captures.push(json!({"selector":selector,"binaryState":binary_state,"input":input,"symbols":symbols,"symbolMode":symbol_mode,"observation":observation,"requestedProject":project,"projectDisposition":if !project{"notRequested"}else if valid&&!row_changed&&input.is_some(){"hostReceipt"}else{"notQueued"},"projectReceipt":if project&&valid&&!row_changed&&input.is_some(){Some("local-decompilation.json")}else{None},"diagnostics":row_issues,"folder":capture_folder,"freshness":"unknown","variant":if row["Version"]==""{"native-current-store-slot"}else{"stored-only; native lifecycle unknown"}}));
            method_issues.extend(row_issues);
        }
        observations.push(json!({"bpMethodCode":code,"binaryState":if absence{"verifiedAbsent"}else if captures.is_empty(){"unavailable"}else{"see captures"},"captures":captures,"diagnostics":method_issues}));
        for diagnostic in method_issues {
            if !diagnostics.contains(&diagnostic) {
                diagnostics.push(diagnostic);
            }
        }
    }
    (jobs, diagnostics, observations)
}
/// Keep only association inputs after normal artifacts enter the independent writer.
fn context(owner: &Identity, definition: &Value, project: bool) -> Value {
    let native = &definition["supplements"]["updateMethods"];
    let mut methods = json!({});
    for table in ["BpMethod", "BpArgument", "BpDirective"] {
        methods[table] = json!(
            native[table]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| {
                    let mut selected = serde_json::Map::new();
                    for key in ["BpMethodCode", "SysRowID", "SysRevID"] {
                        selected.insert(key.into(), row[key].clone());
                    }
                    if table == "BpMethod" {
                        for key in ["BusinessObject", "Name"] {
                            selected.insert(key.into(), row[key].clone());
                        }
                    }
                    Value::Object(selected)
                })
                .collect::<Vec<_>>()
        );
    }
    let root = &definition["definition"]["DynamicQueryDesigner"][0];
    let mut query = serde_json::Map::new();
    for key in ["Company", "QueryID", "SysRowID", "SysRevID"] {
        query.insert(key.into(), root[key].clone());
    }
    json!({"schemaVersion":1,"owner":owner,"definitionSha256":digest(&json!({"query":definition["definition"],"methods":native})),
 "definition":{"DynamicQueryDesigner":[query]},"supplements":{"updateMethods":methods},
 "requestedProject":project,"applicability":if native["BpMethod"].as_array().is_some_and(Vec::is_empty){"verifiedNotApplicable: the complete native definition has no DQ method owner"}else{"native DQ method owners require exact compiled-store observations"},
 "evidence":{"root":"Copy the whole backup, including Herbert Data and its run journal.","ledger":"The run journal managedProcessing.ledger references retained reads and immutable originals. Filter localReads by category BAQs and UBAQs, alias compiled_ubaq, function ReadCompiled and the exact query tuple; verify native owner/method witnesses against this context. Historical observations do not assert current freshness.","projects":"local-decompilation.json gives actual jobs, inputs and owner-local output FileRefs when a project was queued."},
 "interpretation":{"absence":"Verified absence requires complete exact owner/method observations, equal definition/query/runtime witnesses, and BeforeRows=AfterRows=0. Failed/unavailable reads do not prove absence.","freshness":"Unknown. A stable stored assembly is not proof that it was compiled from the current authored definition.","symbols":"Only a verified matching portable PDB is supplied externally; other symbols remain retained evidence.","recovery":"Native recovery uses the BAQ package. Reconstructed C# is not an authoritative BPM export or guaranteed buildable source."}})
}
pub fn prepare(reply: GuestReply, state: &Value) -> Result<GuestReply, Diagnostic> {
    if !requested(state) || matches!(reply, GuestReply::PreparedPage { .. }) {
        return Ok(reply);
    }
    let GuestReply::InventoryData {
        mut records,
        inventory,
    } = reply
    else {
        return Ok(reply);
    };
    let mut contexts = Vec::new();
    for (owner, definition) in &mut records {
        if owner.kind != "UBAQ" {
            continue;
        }
        let c = context(
            owner,
            definition,
            state["_pluginSettings"][DECOMPILE] == true,
        );
        text_file(definition, "compiled-context.json".into(), &c);
        if c["supplements"]["updateMethods"]["BpMethod"]
            .as_array()
            .is_some_and(|methods| !methods.is_empty())
        {
            contexts.push(json!({"owner":owner,"context":c}));
        }
    }
    if contexts.is_empty() {
        return Ok(GuestReply::InventoryData { records, inventory });
    }
    let mut final_reply = *inventory;
    if let GuestReply::Inventory { identities, .. } = &mut final_reply {
        identities.clear();
    }
    let mut next = state.clone();
    for key in [
        "data",
        "bpm",
        "diagrams",
        "extensionMap",
        "extensionInfo",
        "extensionIds",
        "extensionLevels",
        "bpmPage",
        "_ubaqCompiledResult",
        "_ubaqCompiledError",
        "_ubaqLocalQueue",
        "_ubaqLocalJobs",
        "_ubaqLocalIndex",
        "_ubaqLocalAwaiting",
    ] {
        next.as_object_mut().unwrap().remove(key);
    }
    next["_ubaqCompiledOwners"] = json!(contexts);
    next["_ubaqLocalFinal"] = json!(final_reply);
    next["phase"] = json!("compiledUbaqStart");
    if !next["_progress"].is_object() {
        next["_progress"] = json!({});
    }
    next["_progress"]["pendingCompiledRead"] = json!(true);
    Ok(GuestReply::ReadyRecords {
        records,
        state: next,
    })
}
pub fn start_saved(state: &Value) -> Result<GuestReply, Diagnostic> {
    if let Some(reply) = begin(state) {
        Ok(reply)
    } else {
        finish_saved(state)
    }
}
pub fn finish_saved(state: &Value) -> Result<GuestReply, Diagnostic> {
    let entries = state["_ubaqCompiledOwners"].as_array().ok_or_else(|| {
        Diagnostic::new(
            "UBAQ_COMPILED_STATE_INVALID",
            "The saved UBAQ association context is missing.",
        )
    })?;
    let mut next = state.clone();
    let mut jobs = Vec::new();
    for entry in entries {
        let owner: Identity = serde_json::from_value(entry["owner"].clone()).map_err(|_| {
            Diagnostic::new(
                "UBAQ_COMPILED_STATE_INVALID",
                "The saved UBAQ owner is invalid.",
            )
        })?;
        let (requests, diagnostics, _) = evaluate(state, &owner, &entry["context"]);
        jobs.extend(requests);
        let all = next["_ubaqLocalFinal"]["diagnostics"]
            .as_array_mut()
            .ok_or_else(|| {
                Diagnostic::new(
                    "UBAQ_COMPILED_STATE_INVALID",
                    "The saved UBAQ inventory is invalid.",
                )
            })?;
        for diagnostic in diagnostics {
            let value = json!(diagnostic);
            if !all.contains(&value) {
                all.push(value);
            }
        }
    }
    // Identity discovery authority is unchanged by optional capture/processing gaps.
    // Originals and converted native observations already have durable localReads receipts.
    next["_ubaqLocalQueue"] = json!(true);
    next["_ubaqLocalJobs"] = json!(jobs);
    next["_ubaqLocalIndex"] = json!(0);
    next.as_object_mut().unwrap().remove("_ubaqLocalAwaiting");
    for key in [
        "_ubaqCompiledOwners",
        "_ubaqCompiledResult",
        "_ubaqCompiledError",
    ] {
        next.as_object_mut().unwrap().remove(key);
    }
    let request = GuestRequest {
        abi: PLUGIN_ABI,
        operation: "materialize".into(),
        category: "baqs-and-ubaqs".into(),
        company: String::new(),
        identity: None,
        cursor: None,
        page_size: None,
        state: next,
        response: None,
        error: None,
    };
    queue(&request)
}
pub fn queue_active(state: &Value) -> bool {
    state["_ubaqLocalQueue"] == true
}
pub fn queue(request: &GuestRequest) -> Result<GuestReply, Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            "UBAQ_COMPILED_STATE_INVALID",
            "The UBAQ local-job saved progress is invalid.",
        )
    };
    let mut state = request.state.clone();
    let mut index = state["_ubaqLocalIndex"].as_u64().ok_or_else(invalid)?;
    if state["_ubaqLocalAwaiting"] == true {
        let previous = state["_ubaqLocalJobs"]
            .as_array()
            .and_then(|jobs| jobs.get(index as usize))
            .ok_or_else(invalid)?;
        let previous: LocalDecompilationRequest =
            serde_json::from_value(previous.clone()).map_err(|_| invalid())?;
        index = index.checked_add(1).ok_or_else(invalid)?;
        if let Some(error) = &request.error {
            let mut error = error.clone();
            if error.identity.is_none() {
                error.identity = Some(previous.owner);
            }
            error
                .context
                .insert("processingIncomplete".into(), json!(true));
            state["_ubaqLocalFinal"]["diagnostics"]
                .as_array_mut()
                .ok_or_else(invalid)?
                .push(json!(error));
        }
    }
    if let Some(job) = state["_ubaqLocalJobs"]
        .as_array()
        .and_then(|jobs| jobs.get(index as usize))
    {
        let job: LocalDecompilationRequest =
            serde_json::from_value(job.clone()).map_err(|_| invalid())?;
        state["_ubaqLocalIndex"] = json!(index);
        state["_ubaqLocalAwaiting"] = json!(true);
        return Ok(GuestReply::Action {
            action: HostAction::QueueLocalDecompilation { request: job },
            state,
        });
    }
    serde_json::from_value(state["_ubaqLocalFinal"].clone()).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn owner() -> Identity {
        Identity {
            category: "baqs-and-ubaqs".into(),
            kind: "UBAQ".into(),
            scope: Scope::Company {
                company: "C".into(),
            },
            keys: BTreeMap::from([
                ("Company".into(), "C".into()),
                ("QueryID".into(), "Mixed_Name".into()),
            ]),
        }
    }
    fn definition() -> Value {
        json!({"herbertDirectOutput":true,"definition":{"DynamicQueryDesigner":[{"Company":"C","QueryID":"Mixed_Name","SysRowID":"Query-ID","SysRevID":9007199254740993u64}]},"supplements":{"updateMethods":{
        "BpMethod":[{"Source":"DQ","BpMethodCode":"Ice.C/Mixed_Name.GetList","BusinessObject":"C/Mixed_Name","Name":"GetList","SysRowID":"Method-ID","SysRevID":2}],
        "BpArgument":[{"Source":"DQ","BpMethodCode":"Ice.C/Mixed_Name.GetList","SysRowID":"Argument-ID","SysRevID":3}],"BpDirective":[{"Source":"DQ","BpMethodCode":"Ice.C/Mixed_Name.GetList","SysRowID":"Directive-ID","SysRevID":4,"Body":"authored"}]}},"herbertSources":{"binaryFiles":[{"name":"Mixed_Name.baq","base64":"UEs="}],"files":[{"name":"Code/Authored.cs","text":"original source"}]}})
    }
    fn native(path: &str) -> Value {
        json!({"nativeFile":{"path":path,"bytes":3,"sha256":"a".repeat(64),"role":"native"}})
    }
    fn state() -> Value {
        let witness =
            serde_json::to_string(&witness(&definition()["supplements"]["updateMethods"]).unwrap())
                .unwrap();
        let registration = serde_json::to_string(&json!([{"Type":"BPM.Ubaq","Target":"Ice.C/Mixed_Name.GetList","Scope":"##UBAQ##","Assembly":"Ice.Ubaq.C.Mixed.Name.GetList"}])).unwrap();
        json!({"_pluginSettings":{(DOWNLOAD):true,(DECOMPILE):true},"_collectionMethod":"serverAssisted","_hostCapabilities":{"localDecompilationV1":{"protocolVersion":1,"retainedArtifactFunctions":true,"engineId":MANAGED_ENGINE_ID}},"_ubaqCompiledResult":{"oSuccess":true,"oDS":{
            "Owners":[{"Company":"C","QueryID":"Mixed_Name","Methods":1,"DefinitionWitness":witness,"DefinitionWitnessAfter":witness,"QuerySysRevID":"9007199254740993","QuerySysRevIDAfter":"9007199254740993"}],
            "Methods":[{"Company":"C","QueryID":"Mixed_Name","BpMethodCode":"Ice.C/Mixed_Name.GetList","Method":"GetList","Folder":"Ubaq","Assembly":"Ice.Ubaq.C.Mixed.Name.GetList","Registration":registration,"RegistrationAfter":registration,"BeforeRows":1,"AfterRows":1}],
            "Assemblies":[{"Company":"C","QueryID":"Mixed_Name","BpMethodCode":"Ice.C/Mixed_Name.GetList","Folder":"Ubaq","Assembly":"Ice.Ubaq.C.Mixed.Name.GetList","Version":"","SysRevID":"AA","BinarySysRevID":"AA","AfterSysRevID":"AA","Data":native("issued.dll"),"Symbols":native("issued.pdb"),"SymbolDisposition":"matched-portable"}]}}})
    }
    fn inventory() -> GuestReply {
        GuestReply::InventoryData {
            records: vec![(owner(), definition())],
            inventory: Box::new(GuestReply::Inventory {
                identities: vec![owner()],
                next_cursor: Some("2".into()),
                authoritative: true,
                diagnostics: vec![],
            }),
        }
    }

    fn request(state: Value) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "materialize".into(),
            category: "baqs-and-ubaqs".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: None,
            state,
            response: None,
            error: None,
        }
    }
    fn saved(state: &Value) -> (Vec<(Identity, Value)>, Value) {
        let GuestReply::ReadyRecords { records, state } = prepare(inventory(), state).unwrap()
        else {
            panic!("expected one save-first handoff")
        };
        (records, state)
    }
    fn evaluate_fixture(
        state: &Value,
    ) -> (Vec<LocalDecompilationRequest>, Vec<Diagnostic>, Vec<Value>) {
        evaluate(state, &owner(), &context(&owner(), &definition(), true))
    }
    fn final_inventory(reply: GuestReply) -> (bool, Vec<Diagnostic>) {
        let GuestReply::Inventory {
            identities,
            authoritative,
            diagnostics,
            next_cursor,
        } = reply
        else {
            panic!("expected settled inventory")
        };
        assert!(identities.is_empty());
        assert_eq!(next_cursor.as_deref(), Some("2"));
        (authoritative, diagnostics)
    }
    #[test]
    fn default_off_is_byte_equivalent_and_has_no_optional_action() {
        let s = json!({});
        assert!(begin(&s).is_none());
        let expected = serde_json::to_value(inventory()).unwrap();
        assert_eq!(
            serde_json::to_value(prepare(inventory(), &s).unwrap()).unwrap(),
            expected
        );
    }
    #[test]
    fn normal_records_precede_capture_and_continuation_has_no_payload() {
        let s = state();
        let (records, saved) = saved(&s);
        assert_eq!(records.len(), 1);
        let output = &records[0].1;
        assert_eq!(
            output["herbertSources"]["binaryFiles"],
            definition()["herbertSources"]["binaryFiles"]
        );
        assert_eq!(
            output["herbertSources"]["files"][0],
            definition()["herbertSources"]["files"][0]
        );
        assert_eq!(
            output["herbertSources"]["files"][1]["name"],
            "compiled-context.json"
        );
        assert!(output["herbertSources"]["nativeFiles"].is_null());
        assert!(saved["data"].is_null());
        assert!(saved["bpm"].is_null());
        assert!(saved["_ubaqCompiledResult"].is_null());
        assert!(saved["_ubaqCompiledOwners"][0]["context"]["supplements"]["updateMethods"]["BpDirective"][0]["Body"].is_null());
        assert_eq!(saved["_progress"]["pendingCompiledRead"], true);
        let GuestReply::Action {
            action:
                HostAction::RetainedLocalArtifactFunctionCall {
                    alias, arguments, ..
                },
            ..
        } = start_saved(&saved).unwrap()
        else {
            panic!()
        };
        assert_eq!(alias, "compiled_ubaq");
        let queries: Value =
            serde_json::from_str(arguments["iQueryIDs"].as_str().unwrap()).unwrap();
        assert_eq!(
            queries,
            json!([{"company":"C","query":"Mixed_Name","rowId":"Query-ID"}])
        );
    }
    #[test]
    fn exact_native_association_queues_under_the_owning_method() {
        let (jobs, issues, _) = evaluate_fixture(&state());
        assert!(issues.is_empty());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].owner, owner());
        assert_eq!(jobs[0].input.path, "issued.dll");
        assert_eq!(jobs[0].symbol_mode, ManagedSymbolMode::ExternalPortable);
        assert!(jobs[0].output_subdirectory.starts_with("BPM/GetList - "));
        assert!(jobs[0].output_subdirectory.ends_with("/Decompiled"));
    }
    #[test]
    fn direct_and_older_hosts_preserve_authoritative_normal_backup() {
        for direct in [true, false] {
            let mut s = state();
            if direct {
                s["_collectionMethod"] = json!("directOnly");
            } else {
                s.as_object_mut().unwrap().remove("_hostCapabilities");
            }
            let (records, s) = saved(&s);
            assert_eq!(
                records[0].1["herbertSources"]["binaryFiles"],
                definition()["herbertSources"]["binaryFiles"]
            );
            assert!(begin(&s).is_none());
            let (authoritative, issues) = final_inventory(start_saved(&s).unwrap());
            assert!(authoritative);
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].identity.as_ref(), Some(&owner()));
        }
    }
    #[test]
    fn failed_optional_read_is_explicit_without_changing_discovery_authority() {
        let (_, s) = saved(&state());
        let GuestReply::Action { state: s, .. } = start_saved(&s).unwrap() else {
            panic!()
        };
        let mut r = request(s);
        r.error = Some(Diagnostic::new("READER_FAILED", "Reader unavailable."));
        let s = received(&r);
        let (authoritative, issues) = final_inventory(finish_saved(&s).unwrap());
        assert!(authoritative);
        assert_eq!(issues[0].code, "UBAQ_COMPILED_CAPTURE_UNAVAILABLE");
        assert_eq!(
            issues[0].context.get("processingIncomplete"),
            Some(&json!(true))
        );
        assert_eq!(issues[0].identity.as_ref(), Some(&owner()));
    }
    #[test]
    fn absence_requires_equal_complete_native_method_observations() {
        let mut s = state();
        s["_ubaqCompiledResult"]["oDS"]["Assemblies"] = json!([]);
        s["_ubaqCompiledResult"]["oDS"]["Methods"][0]["BeforeRows"] = json!(0);
        s["_ubaqCompiledResult"]["oDS"]["Methods"][0]["AfterRows"] = json!(0);
        let (jobs, issues, observations) = evaluate_fixture(&s);
        assert!(jobs.is_empty());
        assert!(issues.is_empty());
        assert_eq!(observations[0]["binaryState"], "verifiedAbsent");
        s["_ubaqCompiledResult"]["oDS"]["Methods"][0]["AfterRows"] = json!(1);
        let (_, issues, observations) = evaluate_fixture(&s);
        assert!(
            issues
                .iter()
                .any(|d| d.code == "UBAQ_COMPILED_SOURCE_CHANGING")
        );
        assert_ne!(observations[0]["binaryState"], "verifiedAbsent");
    }
    #[test]
    fn source_changes_keep_original_receipts_but_do_not_queue() {
        for pointer in [
            "/Owners/0/QuerySysRevIDAfter",
            "/Owners/0/DefinitionWitnessAfter",
            "/Methods/0/RegistrationAfter",
            "/Assemblies/0/AfterSysRevID",
        ] {
            let mut s = state();
            *s["_ubaqCompiledResult"]["oDS"]
                .pointer_mut(pointer)
                .unwrap() = json!("changed");
            let (jobs, issues, observations) = evaluate_fixture(&s);
            assert!(jobs.is_empty(), "{pointer}");
            assert!(
                issues
                    .iter()
                    .any(|d| d.code == "UBAQ_COMPILED_SOURCE_CHANGING")
            );
            assert_eq!(
                observations[0]["captures"][0]["input"]["path"],
                "issued.dll"
            );
        }
    }
    #[test]
    fn selectors_preserve_case_and_cannot_borrow_other_categories() {
        for (pointer, value) in [
            ("/Methods/0/Assembly", "ice.ubaq.c.mixed.name.getlist"),
            ("/Methods/0/Folder", ":Efx"),
            ("/Assemblies/0/Assembly", "efx.sample"),
            ("/Assemblies/0/Folder", "BO"),
        ] {
            let mut s = state();
            *s["_ubaqCompiledResult"]["oDS"]
                .pointer_mut(pointer)
                .unwrap() = json!(value);
            let (jobs, issues, _) = evaluate_fixture(&s);
            assert!(jobs.is_empty());
            assert!(
                issues
                    .iter()
                    .any(|d| d.code == "UBAQ_COMPILED_ASSOCIATION_INVALID")
            );
        }
    }
    #[test]
    fn mismatched_symbols_remain_evidence_without_being_selected() {
        let mut s = state();
        s["_ubaqCompiledResult"]["oDS"]["Assemblies"][0]["SymbolDisposition"] =
            json!("mismatched-unused");
        let (jobs, issues, observations) = evaluate_fixture(&s);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].symbol_mode, ManagedSymbolMode::None);
        assert!(jobs[0].symbols.is_none());
        assert_eq!(
            observations[0]["captures"][0]["symbols"]["path"],
            "issued.pdb"
        );
        assert!(
            issues
                .iter()
                .any(|d| d.code == "UBAQ_COMPILED_SYMBOLS_UNUSED")
        );
    }
    #[test]
    fn incomplete_owner_or_method_discovery_does_not_claim_absence() {
        for table in ["Owners", "Methods"] {
            let mut s = state();
            s["_ubaqCompiledResult"]["oDS"][table] = json!([]);
            let (jobs, issues, observations) = evaluate_fixture(&s);
            assert!(jobs.is_empty());
            assert!(
                issues
                    .iter()
                    .any(|d| d.code == "UBAQ_COMPILED_OWNER_UNVERIFIED")
            );
            assert!(observations.is_empty());
        }
    }
    #[test]
    fn capture_only_reads_originals_and_enrolls_no_jobs() {
        let mut s = state();
        s["_pluginSettings"][DECOMPILE] = json!(false);
        let (_, saved) = saved(&s);
        assert!(matches!(
            start_saved(&saved).unwrap(),
            GuestReply::Action {
                action: HostAction::RetainedLocalArtifactFunctionCall { .. },
                ..
            }
        ));
        let (jobs, issues, observations) = evaluate_fixture(&s);
        assert!(jobs.is_empty());
        assert!(issues.is_empty());
        assert_eq!(observations[0]["captures"][0]["binaryState"], "captured");
        assert_eq!(
            observations[0]["captures"][0]["projectDisposition"],
            "notRequested"
        );
        let mut r = request(saved);
        r.response = Some(s["_ubaqCompiledResult"].clone());
        let (authoritative, issues) = final_inventory(finish_saved(&received(&r)).unwrap());
        assert!(authoritative);
        assert!(issues.is_empty());
    }
    #[test]
    fn recovery_context_binds_exact_revision_and_success_clears_stale_failure() {
        let (_, saved) = saved(&state());
        assert_eq!(
            saved["_ubaqCompiledOwners"][0]["context"]["definition"]["DynamicQueryDesigner"][0]["SysRevID"],
            json!(9007199254740993u64)
        );
        let mut r = request(saved);
        r.state["_ubaqCompiledError"] = json!({"code":"OLD","message":"Old read failed"});
        r.response = Some(state()["_ubaqCompiledResult"].clone());
        let fresh = received(&r);
        assert!(fresh["_ubaqCompiledError"].is_null());
        let GuestReply::Action {
            action: HostAction::QueueLocalDecompilation { request },
            state: queued,
        } = finish_saved(&fresh).unwrap()
        else {
            panic!()
        };
        assert_eq!(request.owner, owner());
        assert_eq!(queued["_ubaqLocalFinal"]["identities"], json!([]));
        assert_eq!(queued["_ubaqLocalFinal"]["nextCursor"], "2");
        assert!(queued["_ubaqCompiledOwners"].is_null());
        assert!(queued["_ubaqCompiledResult"].is_null());
        assert!(
            queued["_ubaqLocalFinal"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn ordinary_baq_records_do_not_create_optional_reads() {
        let GuestReply::InventoryData {
            mut records,
            inventory,
        } = inventory()
        else {
            panic!()
        };
        records[0].0.kind = "BAQ".into();
        let normal = GuestReply::InventoryData { records, inventory };
        let expected = serde_json::to_value(&normal).unwrap();
        assert_eq!(
            serde_json::to_value(prepare(normal, &state()).unwrap()).unwrap(),
            expected
        );
    }
    #[test]
    fn query_without_native_method_owners_does_not_need_the_compiled_reader() {
        let mut native = definition();
        native["supplements"]["updateMethods"] =
            json!({"BpMethod":[],"BpArgument":[],"BpDirective":[]});
        let reply = GuestReply::InventoryData {
            records: vec![(owner(), native)],
            inventory: Box::new(GuestReply::Inventory {
                identities: vec![owner()],
                next_cursor: None,
                authoritative: true,
                diagnostics: vec![],
            }),
        };
        let GuestReply::InventoryData { records, inventory } = prepare(reply, &state()).unwrap()
        else {
            panic!()
        };
        let c: Value = serde_json::from_str(
            records[0].1["herbertSources"]["files"][1]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert!(
            c["applicability"]
                .as_str()
                .unwrap()
                .starts_with("verifiedNotApplicable")
        );
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: true,
                ..
            }
        ));
    }
    #[test]
    fn rejected_enrollment_marks_only_its_exact_owner_processing_incomplete() {
        let (_, saved) = saved(&state());
        let mut r = request(saved);
        r.response = Some(state()["_ubaqCompiledResult"].clone());
        let GuestReply::Action { state: queued, .. } = finish_saved(&received(&r)).unwrap() else {
            panic!()
        };
        let mut r = request(queued);
        r.error = Some(Diagnostic::new(
            "MANAGED_INPUT_UNAVAILABLE",
            "An original cannot be verified.",
        ));
        let (authoritative, issues) = final_inventory(queue(&r).unwrap());
        assert!(authoritative);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].identity.as_ref(), Some(&owner()));
        assert_eq!(
            issues[0].context.get("processingIncomplete"),
            Some(&json!(true))
        );
    }
}
