//! Optional ordinary BPM directives compiled capture. Native ownership and paperwork live
//! here; the neutral host queue owns only verified local processing mechanics.
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
pub const DOWNLOAD: &str = "downloadCompiledAssemblies";
pub const DECOMPILE: &str = "decompileCompiledAssemblies";
pub fn normalize(settings: &mut Value) {
    if settings[DECOMPILE] == true {
        settings[DOWNLOAD] = json!(true);
    }
}
fn requested(state: &Value) -> bool {
    state.pointer("/_pluginSettings/downloadCompiledAssemblies") == Some(&json!(true))
        || state.pointer("/_pluginSettings/decompileCompiledAssemblies") == Some(&json!(true))
}
fn compatible(state: &Value) -> bool {
    state.pointer("/_hostCapabilities/localDecompilationV1/protocolVersion") == Some(&json!(1))
        && state.pointer("/_hostCapabilities/localDecompilationV1/engineId")
            == Some(&json!(MANAGED_ENGINE_ID))
        && state.pointer("/_hostCapabilities/localDecompilationV1/retainedArtifactFunctions")
            == Some(&json!(true))
}
pub fn binary_exports() -> Vec<BinaryExport> {
    serde_json::from_value(json!([
 {"service":"compiled_bpm","method":"ReadCompiled","pointer":"/oDS/Assemblies/*/Data","extension":"dll"},
 {"service":"compiled_bpm","method":"ReadCompiled","pointer":"/oDS/Assemblies/*/Symbols","extension":"pdb"}
])).expect("BPM compiled declarations")
}
fn digest(value: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("JSON value"))
    )
}
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("BPM_COMPILED_STATE_INVALID", message)
}
fn diagnostic(owner: &Identity, code: &str, message: &str) -> Diagnostic {
    let mut d = Diagnostic::new(code, message);
    d.identity = Some(owner.clone());
    d
}
fn selected(reply: &GuestReply) -> Vec<&Identity> {
    match reply {
        GuestReply::InventoryData { records, .. } => records.iter().map(|r| &r.0).collect(),
        GuestReply::Definition { identity, .. } => vec![identity],
        _ => vec![],
    }
}
pub fn active(state: &Value) -> bool {
    state["_bpmCompiledStart"] == true
        || state["_bpmCompiledRead"] == true
        || state["_bpmCompiledQueue"] == true
}
// Independent native owners retain their own recovery output first. The final
// work item performs one compiled read using only compact, immutable witnesses.
pub(crate) fn independent_batch(state: &Value, owners: &[(Identity, Value)]) -> Option<Value> {
    if !requested(state) || !compatible(state) || owners.is_empty() { return None; }
    let project = state.pointer("/_pluginSettings/decompileCompiledAssemblies") == Some(&json!(true));
    let contexts: Vec<_> = owners.iter().map(|(owner, dataset)|
        (owner, context(owner, &json!({"definition":dataset}), project))).collect();
    let mut batch = json!({"_bpmCompiledStart":true,"_bpmCompiledOwners":contexts,
        "_bpmCompiledFinal":GuestReply::Inventory { identities:vec![], next_cursor:None, authoritative:true, diagnostics:vec![] },
        "_progress":{"pendingCompiledRead":true}});
    for (key, value) in state.as_object().unwrap() {
        if key.starts_with('_') && key != "_progress" { batch[key] = value.clone(); }
    }
    Some(batch)
}
fn context(owner: &Identity, definition: &Value, project: bool) -> Value {
    let mut native = json!({});
    for table in ["BpMethod", "BpArgument", "BpDirective"] {
        native[table] = json!(
            definition["definition"][table]
                .as_array()
                .into_iter()
                .flatten()
                .map(|row| {
                    let mut item = serde_json::Map::new();
                    for key in ["Source", "BpMethodCode", "SysRowID", "SysRevID"] {
                        item.insert(key.into(), row[key].clone());
                    }
                    if table == "BpMethod" {
                        for key in ["ObjectNS", "BusinessObject", "Name", "SystemFlag"] {
                            item.insert(key.into(), row[key].clone());
                        }
                    }
                    Value::Object(item)
                })
                .collect::<Vec<_>>()
        );
    }
    json!({"schemaVersion":1,"owner":owner,"definitionSha256":digest(&definition["definition"]),"definition":native,"requestedProject":project,
 "evidence":{"root":"Copy the whole backup. Include Herbert Data/raw and the run journal.","ledger":"The run journal managedProcessing.ledger references retained reads and immutable originals. Filter localReads by category BPM directives, alias compiled_bpm, function ReadCompiled and the exact Source/BpMethodCode tuple. Verify native owner/method witnesses against this context. Historical observations do not assert current freshness.","projects":"local-decompilation.json gives actual jobs, inputs and owner-local output FileRefs when a project was queued."},
 "interpretation":{"absence":"Verified absence requires complete exact owner/method observations, equal definition/runtime/store witnesses and BeforeRows=AfterRows=0. Failed or unavailable reads do not prove absence.","freshness":"Unknown. A stable stored assembly is not proof that it was compiled from the current authored definition.","symbols":"Only verified matching portable PDBs are supplied externally. Other symbols remain retained evidence.","recovery":"Native recovery uses the .bpm package and persisted workflow. Reconstructed C# is supplementary source and is not guaranteed to build."}})
}
fn processing_diagnostic(mut diagnostic: Diagnostic) -> Diagnostic {
    diagnostic
        .context
        .insert("processingIncomplete".into(), json!(true));
    diagnostic
}
pub fn begin(reply: GuestReply, request: &GuestRequest) -> Result<GuestReply, Diagnostic> {
    if !requested(&request.state) || selected(&reply).is_empty() {
        return Ok(reply);
    }
    if !compatible(&request.state)
        || !matches!(request.operation.as_str(), "materialize" | "acquire")
    {
        return finish(reply, &request.state, None, request.operation.as_str());
    }
    let (mut records, mut final_reply) = match reply {
        GuestReply::InventoryData { records, inventory } => (records, *inventory),
        GuestReply::Definition {
            identity,
            format,
            definition,
            completeness,
            diagnostics,
        } => (
            vec![(identity.clone(), definition.clone())],
            GuestReply::Definition {
                identity,
                format,
                definition,
                completeness,
                diagnostics,
            },
        ),
        other => return Ok(other),
    };
    let mut contexts = vec![];
    for (owner, definition) in &mut records {
        let saved = context(
            owner,
            definition,
            request
                .state
                .pointer("/_pluginSettings/decompileCompiledAssemblies")
                == Some(&json!(true)),
        );
        contexts.push((owner.clone(), saved.clone()));
        if !definition["herbertSources"].is_object() {
            definition["herbertSources"] = json!({});
        }
        if !definition["herbertSources"]["files"].is_array() {
            definition["herbertSources"]["files"] = json!([]);
        }
        definition["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"compiled-context.json","text":serde_json::to_string_pretty(&saved).expect("context")}));
    }
    if request.state["_bpmCompiledDeferred"] == true {
        return Ok(GuestReply::InventoryData { records, inventory: Box::new(final_reply) });
    }
    match &mut final_reply {
        GuestReply::Inventory { identities, .. } => identities.clear(),
        GuestReply::Definition { definition, .. } => *definition = records[0].1.clone(),
        _ => {}
    }
    let mut state = request.state.clone();
    for key in [
        "data",
        "bpm",
        "dataset",
        "records",
        "inner",
        "identities",
        "_inventory",
        "_streamInput",
        "_bpmCompiledJobs",
        "_bpmCompiledQueueErrors",
        "_bpmCompiledAwaiting",
        "_bpmCompiledIndex",
        "_bpmCompiledRead",
        "_bpmCompiledQueue",
    ] {
        state.as_object_mut().unwrap().remove(key);
    }
    state["_bpmCompiledOwners"] = json!(contexts);
    state["_bpmCompiledFinal"] = json!(final_reply);
    state["_bpmCompiledStart"] = json!(true);
    if !state["_progress"].is_object() {
        state["_progress"] = json!({});
    }
    state["_progress"]["pendingCompiledRead"] = json!(true);
    Ok(GuestReply::ReadyRecords { records, state })
}
fn finish_saved(
    mut state: Value,
    response: Option<&Value>,
    reader_error: Option<&Diagnostic>,
) -> Result<GuestReply, Diagnostic> {
    let contexts: Vec<(Identity, Value)> =
        serde_json::from_value(state["_bpmCompiledOwners"].clone())
            .map_err(|_| invalid("The saved BPM association context is invalid."))?;
    let mut jobs = vec![];
    let mut errors = vec![];
    for (owner, mut context) in contexts {
        if let Some(error) = reader_error {
            // Keep the host's structured cause with every affected saved owner.
            // The generic missing-coverage diagnostic remains supplemental.
            let mut error = error.clone();
            error.identity = Some(owner.clone());
            errors.push(processing_diagnostic(error));
        }
        let (work, diagnostics) = augment(&owner, &mut context, &state, response);
        jobs.extend(work);
        errors.extend(diagnostics.into_iter().map(processing_diagnostic));
    }
    state["_bpmCompiledJobs"] = json!(jobs);
    state["_bpmCompiledIndex"] = json!(0);
    state["_bpmCompiledQueue"] = json!(true);
    state["_bpmCompiledQueueErrors"] = json!(errors);
    state.as_object_mut().unwrap().remove("_bpmCompiledOwners");
    state["_progress"]["pendingCompiledRead"] = json!(false);
    drive(state)
}
pub fn continue_work(request: &GuestRequest) -> Result<GuestReply, Diagnostic> {
    if request.state["_bpmCompiledStart"] == true {
        let mut state = request.state.clone();
        state["_bpmCompiledStart"] = json!(false);
        if compatible(&state) && state["_collectionMethod"] != "directOnly" {
            let contexts: Vec<(Identity, Value)> =
                serde_json::from_value(state["_bpmCompiledOwners"].clone())
                    .map_err(|_| invalid("The saved BPM owners are invalid."))?;
            let owners=contexts.iter().map(|(owner,_)|json!({"source":owner.keys["Source"],"method":owner.keys["BpMethodCode"]})).collect::<Vec<_>>();
            state["_bpmCompiledRead"] = json!(true);
            state["_progress"]["request"] =
                json!("Reading associated ordinary BPM DLL/PDB snapshots");
            state["_progress"]["response"] =
                json!("Stored originals retained; validating native ownership and local projects");
            return Ok(GuestReply::Action {
                action: HostAction::RetainedLocalArtifactFunctionCall {
                    alias: "compiled_bpm".into(),
                    function: "ReadCompiled".into(),
                    arguments: json!({"iOwners":serde_json::to_string(&owners).expect("owners")}),
                },
                state,
            });
        }
        return finish_saved(state, None, None);
    }
    if request.state["_bpmCompiledRead"] == true {
        let mut state = request.state.clone();
        state["_bpmCompiledRead"] = json!(false);
        return finish_saved(
            state,
            request
                .response
                .as_ref()
                .filter(|_| request.error.is_none()),
            request.error.as_ref(),
        );
    }
    let mut state = request.state.clone();
    if state["_bpmCompiledAwaiting"] == true {
        let index = state["_bpmCompiledIndex"].as_u64().unwrap_or(0) as usize;
        if let Some(error) = &request.error {
            let mut error = error.clone();
            error.identity =
                serde_json::from_value(state["_bpmCompiledJobs"][index]["owner"].clone()).ok();
            state["_bpmCompiledQueueErrors"]
                .as_array_mut()
                .ok_or_else(|| invalid("Local queue diagnostics are invalid."))?
                .push(json!(processing_diagnostic(error)));
        }
        state["_bpmCompiledIndex"] = json!(index + 1);
        state["_bpmCompiledAwaiting"] = json!(false);
    }
    drive(state)
}
fn selector(owner: &Identity, definition: &Value) -> Option<(String, String)> {
    let source = owner.keys.get("Source")?;
    let code = owner.keys.get("BpMethodCode")?;
    if owner.category != "bpm-directives"
        || owner.scope != Scope::Global
        || owner.keys.len() != 2
        || owner.kind
            != if source == "BO" {
                "MethodDirectiveGroup"
            } else {
                "DataDirectiveGroup"
            }
    {
        return None;
    }
    let rows = definition.pointer("/definition/BpMethod")?.as_array()?;
    if rows.len() != 1 {
        return None;
    }
    let row = &rows[0];
    if row["Source"] != *source || row["BpMethodCode"] != *code || row["SystemFlag"] != false {
        return None;
    }
    let parts = code.split('.').collect::<Vec<_>>();
    if source == "BO"
        && parts.len() == 4
        && row["ObjectNS"] == parts[1]
        && row["BusinessObject"] == parts[2]
        && row["Name"] == parts[3]
    {
        Some((
            "BO".into(),
            format!("{}.Bpm.{}", parts[0], parts[1..].join(".")),
        ))
    } else if source == "DB"
        && parts.len() == 3
        && parts[2] == "Update"
        && row["BusinessObject"] == parts[1]
        && row["Name"] == "Update"
    {
        Some((
            "DT".into(),
            format!("{}.{}.Bpm.Triggers", parts[0], parts[1]),
        ))
    } else {
        None
    }
}
fn witness(owner: &Identity, definition: &Value) -> Option<Value> {
    let mut result = vec![];
    for table in ["BpMethod", "BpArgument", "BpDirective"] {
        let mut rows = definition["definition"][table]
            .as_array()?
            .iter()
            .filter(|row| {
                row["Source"] == owner.keys["Source"]
                    && row["BpMethodCode"] == owner.keys["BpMethodCode"]
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| row["SysRowID"].as_str().unwrap_or(""));
        for row in rows {
            if !row["SysRowID"].is_string() || row["SysRevID"].is_null() {
                return None;
            }
            result
                .push(json!({"Table":table,"SysRowID":row["SysRowID"],"SysRevID":row["SysRevID"]}));
        }
    }
    Some(json!(result))
}
fn parsed(row: &Value, key: &str) -> Option<Value> {
    serde_json::from_str(row[key].as_str()?).ok()
}
fn augment(
    owner: &Identity,
    definition: &mut Value,
    state: &Value,
    response: Option<&Value>,
) -> (Vec<LocalDecompilationRequest>, Vec<Diagnostic>) {
    let mut diagnostics = vec![];
    let mut jobs = vec![];
    let mut captures = vec![];
    let expected = selector(owner, definition);
    let expected_witness = witness(owner, definition);
    let rows = response
        .and_then(|r| r.pointer("/oDS/Owners"))
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|r| {
                    r["Source"] == owner.keys["Source"]
                        && r["BpMethodCode"] == owner.keys["BpMethodCode"]
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let owner_row = if rows.len() == 1 {
        rows.first().copied()
    } else {
        None
    };
    let unavailable = if !compatible(state) {
        Some(
            "The host does not negotiate optional local capture/decompilation. Normal BPM output is preserved.",
        )
    } else if state["_collectionMethod"] == "directOnly" {
        Some(
            "Direct only preserves native BPM packages, definitions and authored code. The optional hidden compiled store requires its declared read helper.",
        )
    } else if response.is_none_or(|r| r["oSuccess"] != true) {
        Some(
            "The optional compiled reader was unavailable or refused. Normal BPM output is preserved.",
        )
    } else if expected.is_none() || owner_row.is_none() || expected_witness.is_none() {
        Some("The exact BPM owner or native revision witness was not proved.")
    } else {
        None
    };
    let mut native_files = vec![];
    if let Some(message) = unavailable {
        diagnostics.push(diagnostic(owner, "BPM_COMPILED_UNAVAILABLE", message));
        captures.push(json!({"binaryState":"unavailable"}));
    } else {
        let (folder, assembly) = expected.unwrap();
        let evidence = owner_row.unwrap();
        let associated = evidence["Folder"] == folder
            && evidence["Assembly"] == assembly
            && evidence["AssociationValid"] == true;
        let authorized =
            evidence["ScopeComplete"] == true && evidence["ScopeCompleteAfter"] == true;
        let stable = parsed(evidence, "DefinitionWitness") == expected_witness
            && parsed(evidence, "DefinitionWitnessAfter") == expected_witness
            && evidence["Registration"] == evidence["RegistrationAfter"]
            && evidence["BeforeRows"] == evidence["AfterRows"];
        if !associated {
            diagnostics.push(diagnostic(owner,"BPM_COMPILED_ASSOCIATION_INVALID","Epicor registration/generator evidence does not prove the exact requested assembly."));
        }
        if !authorized {
            diagnostics.push(diagnostic(owner,"BPM_COMPILED_SCOPE_PARTIAL","The compiled owner includes directives outside the Epicor user-visible Epicor scope. Its bytes were withheld; available Epicor BPM definitions remain preserved."));
        }
        if !stable {
            diagnostics.push(diagnostic(owner,"BPM_COMPILED_SOURCE_CHANGING","Epicor definition/store/registration checks changed or do not match the collected definition. Available originals are retained without a current-pair claim."));
        }
        let all = response
            .and_then(|r| r.pointer("/oDS/Assemblies"))
            .and_then(Value::as_array);
        let selected = all
            .into_iter()
            .flatten()
            .filter(|r| {
                r["Source"] == owner.keys["Source"]
                    && r["BpMethodCode"] == owner.keys["BpMethodCode"]
            })
            .collect::<Vec<_>>();
        let count_valid = evidence["BeforeRows"].as_u64() == Some(selected.len() as u64)
            && evidence["AfterRows"].as_u64() == Some(selected.len() as u64);
        if selected.is_empty() {
            let state = if associated && authorized && stable && count_valid {
                "verifiedAbsent"
            } else {
                "unavailable"
            };
            if !count_valid {
                diagnostics.push(diagnostic(owner,"BPM_COMPILED_ROWS_MISSING","The compiled row response does not prove the selected owner's complete store membership."));
            }
            captures.push(json!({"binaryState":state,"selector":{"folder":folder,"assembly":assembly},"ownerObservation":evidence}));
        }
        let mut seen = BTreeSet::new();
        for row in selected {
            let exact = row["Folder"] == folder
                && row["Assembly"] == assembly
                && row["Version"].is_string()
                && seen.insert(row["Version"].clone().to_string());
            let input = serde_json::from_value::<FileRef>(row["Data"]["nativeFile"].clone()).ok();
            let symbols =
                serde_json::from_value::<FileRef>(row["Symbols"]["nativeFile"].clone()).ok();
            let consistent = stable
                && count_valid
                && row["SysRevID"].is_string()
                && row["SysRevID"] == row["BinarySysRevID"]
                && row["SysRevID"] == row["AfterSysRevID"];
            let slot = json!({"folder":folder,"assembly":assembly,"version":row["Version"]});
            let directory = format!("Compiled/{}", digest(&slot));
            if exact && associated && authorized {
                if let Some(file) = &input {
                    native_files.push(json!({"name":format!("{directory}/{}.dll",crate::bpm_code::label(&assembly)),"file":file}));
                }
                if let Some(file) = &symbols {
                    native_files.push(json!({"name":format!("{directory}/{}.pdb",crate::bpm_code::label(&assembly)),"file":file}));
                }
            }
            if !exact || !associated {
                diagnostics.push(diagnostic(
                    owner,
                    "BPM_COMPILED_ASSOCIATION_INVALID",
                    "A repeated or unexpected compiled selector was refused.",
                ));
            }
            if !consistent {
                diagnostics.push(diagnostic(owner,"BPM_COMPILED_SOURCE_CHANGING","The compiled row and definition witnesses do not establish a consistent capture. Available originals remain preserved."));
            }
            if input.is_none() {
                diagnostics.push(diagnostic(owner,"BPM_COMPILED_BINARY_UNAVAILABLE","The selected store row did not yield a verified DLL. Normal BPM output and available original symbols remain preserved."));
            }
            let symbol_state = row["SymbolDisposition"]
                .as_str()
                .unwrap_or("unsupported-unused");
            let mode = if symbol_state == "matched-portable" && symbols.is_some() {
                ManagedSymbolMode::ExternalPortable
            } else if symbol_state.contains("embedded-candidate") {
                ManagedSymbolMode::EmbeddedPortable
            } else {
                ManagedSymbolMode::None
            };
            if symbols.is_some() && symbol_state != "matched-portable" {
                diagnostics.push(diagnostic(owner,"BPM_COMPILED_SYMBOLS_UNUSED","Original symbols were preserved but do not prove a matching portable PDB; only selected verified symbol modes reach the engine."));
            }
            let can_process = exact && associated && authorized && consistent && input.is_some();
            if can_process
                && state.pointer("/_pluginSettings/decompileCompiledAssemblies")
                    == Some(&json!(true))
            {
                let mut job = LocalDecompilationRequest {
                    protocol_version: 1,
                    request_id: String::new(),
                    expected_engine: MANAGED_ENGINE_ID.into(),
                    owner: owner.clone(),
                    input: input.clone().unwrap(),
                    symbols: symbols.clone(),
                    symbol_mode: mode.clone(),
                    output_subdirectory: format!("{directory}/Decompiled"),
                };
                job.request_id = digest(&json!([
                    owner,
                    slot,
                    &job.input,
                    &job.symbols,
                    &job.symbol_mode
                ]));
                jobs.push(job);
            }
            let mut observation = row.clone();
            observation.as_object_mut().map(|o| {
                o.remove("Data");
                o.remove("Symbols");
            });
            captures.push(json!({"binaryState":if can_process{"captured"}else{"partial"},"selector":slot,"observation":observation,"ownerObservation":evidence,"input":input,"symbols":symbols,"symbolMode":mode,"projectRequested":state.pointer("/_pluginSettings/decompileCompiledAssemblies")==Some(&json!(true)),"outputDirectory":directory,"freshness":"unknown; stored native variant; no current deployment claim"}));
        }
    }
    if !definition["herbertSources"].is_object() {
        definition["herbertSources"] = json!({});
    }
    if !definition["herbertSources"]["files"].is_array() {
        definition["herbertSources"]["files"] = json!([]);
    }
    if !definition["herbertSources"]["nativeFiles"].is_array() {
        definition["herbertSources"]["nativeFiles"] = json!([]);
    }
    definition["herbertSources"]["nativeFiles"]
        .as_array_mut()
        .unwrap()
        .extend(native_files);
    let manifest = json!({"schemaVersion":1,"owner":owner,"definitionSha256":digest(&definition["definition"]),"captures":captures,"diagnostics":diagnostics,"coverage":"The native .bpm and persisted workflows remain the recovery authority. Compiled projects are supplementary reconstructed source; data-trigger assemblies share both standard and in-transaction directives for the table. Missing dependencies and reconstruction limits remain in local result manifests."});
    definition["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"Compiled/compiled.json","text":serde_json::to_string_pretty(&manifest).expect("capture manifest")}));
    if !diagnostics.is_empty() {
        let mut existing = definition
            .pointer("/herbertCollectionStatus/diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        existing.extend(diagnostics.iter().map(|d| json!(d)));
        definition["herbertCollectionStatus"] =
            json!({"completeness":"partial","diagnostics":existing});
    }
    (jobs, diagnostics)
}
fn finish(
    mut reply: GuestReply,
    state: &Value,
    response: Option<&Value>,
    operation: &str,
) -> Result<GuestReply, Diagnostic> {
    let mut jobs = vec![];
    let mut optional_diagnostics = vec![];
    match &mut reply {
        GuestReply::InventoryData { records, .. } => {
            for (owner, definition) in records {
                let (mut work, mut diagnostics) = augment(owner, definition, state, response);
                jobs.append(&mut work);
                optional_diagnostics.append(&mut diagnostics);
            }
        }
        GuestReply::Definition {
            identity,
            definition,
            diagnostics,
            completeness,
            ..
        } => {
            let (mut work, mut errors) = augment(identity, definition, state, response);
            jobs.append(&mut work);
            if !errors.is_empty() {
                *completeness = Completeness::Partial;
            }
            diagnostics.append(&mut errors);
        }
        _ => return Ok(reply),
    }
    if let GuestReply::InventoryData { inventory, .. } = &mut reply {
        if let GuestReply::Inventory { diagnostics, .. } = &mut **inventory {
            diagnostics.extend(optional_diagnostics);
        }
    }
    if jobs.is_empty() {
        return Ok(reply);
    }
    let mut next = state.clone();
    next["_bpmCompiledJobs"] = json!(jobs);
    next["_bpmCompiledIndex"] = json!(0);
    next["_bpmCompiledQueue"] = json!(true);
    next["_bpmCompiledQueueErrors"] = json!([]);
    if operation == "materialize" {
        if let GuestReply::InventoryData {
            records,
            mut inventory,
        } = reply
        {
            if let GuestReply::Inventory { identities, .. } = &mut *inventory {
                identities.clear();
            }
            next["_bpmCompiledFinal"] = serde_json::to_value(*inventory)
                .map_err(|_| invalid("Cannot save final inventory."))?;
            return Ok(GuestReply::ReadyRecords {
                records,
                state: next,
            });
        }
    }
    next["_bpmCompiledFinal"] =
        serde_json::to_value(reply).map_err(|_| invalid("Cannot save final output."))?;
    drive(next)
}
fn drive(mut state: Value) -> Result<GuestReply, Diagnostic> {
    let index = state["_bpmCompiledIndex"].as_u64().unwrap_or(0) as usize;
    if let Some(job) = state["_bpmCompiledJobs"]
        .as_array()
        .and_then(|jobs| jobs.get(index))
    {
        let request =
            serde_json::from_value(job.clone()).map_err(|_| invalid("Invalid local BPM job."))?;
        state["_bpmCompiledAwaiting"] = json!(true);
        return Ok(GuestReply::Action {
            action: HostAction::QueueLocalDecompilation { request },
            state,
        });
    }
    let mut reply: GuestReply = serde_json::from_value(state["_bpmCompiledFinal"].clone())
        .map_err(|_| invalid("Invalid final BPM output."))?;
    let errors: Vec<Diagnostic> = serde_json::from_value(state["_bpmCompiledQueueErrors"].clone())
        .map_err(|_| invalid("Invalid local job diagnostics."))?;
    match &mut reply {
        GuestReply::Inventory { diagnostics, .. } | GuestReply::Definition { diagnostics, .. } => {
            diagnostics.extend(errors)
        }
        _ => {}
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn owner(source: &str) -> Identity {
        Identity {
            category: "bpm-directives".into(),
            kind: if source == "BO" {
                "MethodDirectiveGroup"
            } else {
                "DataDirectiveGroup"
            }
            .into(),
            scope: Scope::Global,
            keys: BTreeMap::from([
                ("Source".into(), source.into()),
                (
                    "BpMethodCode".into(),
                    if source == "BO" {
                        "Ice.BO.UD39.GetNewUD39"
                    } else {
                        "Ice.UD40.Update"
                    }
                    .into(),
                ),
            ]),
        }
    }
    fn definition(source: &str) -> Value {
        let id = owner(source);
        json!({"definition":{"BpMethod":[{"Source":source,"BpMethodCode":id.keys["BpMethodCode"],"ObjectNS":if source=="BO"{"BO"}else{""},"BusinessObject":if source=="BO"{"UD39"}else{"UD40"},"Name":if source=="BO"{"GetNewUD39"}else{"Update"},"SystemFlag":false,"SysRowID":"method-row","SysRevID":1}],"BpArgument":[],"BpDirective":[{"Source":source,"BpMethodCode":id.keys["BpMethodCode"],"SysRowID":"directive-row","SysRevID":2}]},"herbertDirectOutput":true,"herbertSources":{"files":[{"name":"Authored.cs","text":"authored source"}],"binaryFiles":[{"name":"original.bpm","base64":"YmFja3Vw"}]}})
    }
    fn state() -> Value {
        json!({"_pluginSettings":{(DOWNLOAD):true,(DECOMPILE):true},"_collectionMethod":"serverAssisted","_hostCapabilities":{"localDecompilationV1":{"protocolVersion":1,"engineId":MANAGED_ENGINE_ID,"retainedArtifactFunctions":true}}})
    }
    fn input() -> FileRef {
        FileRef {
            path: "Herbert Data/raw/original.dll".into(),
            sha256: "a".repeat(64),
            bytes: 123,
            role: "native".into(),
        }
    }
    fn response(source: &str, definition: &Value, with_binary: bool) -> Value {
        let id = owner(source);
        let (folder, assembly) = selector(&id, definition).unwrap();
        let witness = serde_json::to_string(&witness(&id, definition).unwrap()).unwrap();
        json!({"oSuccess":true,"oDS":{"Owners":[{"Source":source,"BpMethodCode":id.keys["BpMethodCode"],"Folder":folder,"Assembly":assembly,"AssociationValid":true,"ScopeComplete":true,"ScopeCompleteAfter":true,"DefinitionWitness":witness,"DefinitionWitnessAfter":witness,"Registration":"[]","RegistrationAfter":"[]","BeforeRows":if with_binary{1}else{0},"AfterRows":if with_binary{1}else{0}}],"Assemblies":if with_binary{json!([{"Source":source,"BpMethodCode":id.keys["BpMethodCode"],"Folder":folder,"Assembly":assembly,"Version":"","SysRevID":"AAAA","BinarySysRevID":"AAAA","AfterSysRevID":"AAAA","Data":{"nativeFile":input()},"Symbols":null,"SymbolDisposition":"absent"}])}else{json!([])}}})
    }
    fn reply(source: &str) -> GuestReply {
        GuestReply::InventoryData {
            records: vec![(owner(source), definition(source))],
            inventory: Box::new(GuestReply::Inventory {
                identities: vec![owner(source)],
                authoritative: true,
                diagnostics: vec![],
                next_cursor: None,
            }),
        }
    }
    #[test]
    fn ordinary_settings_off_leave_native_reply_unchanged() {
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "materialize".into(),
            category: "bpm-directives".into(),
            company: "TESTCO".into(),
            identity: None,
            cursor: None,
            page_size: None,
            state: json!({}),
            response: None,
            error: None,
        };
        assert_eq!(
            serde_json::to_value(begin(reply("DB"), &request).unwrap()).unwrap(),
            serde_json::to_value(reply("DB")).unwrap()
        );
    }
    #[test]
    fn direct_only_retains_native_recovery_and_reports_optional_gap() {
        let mut state = state();
        state["_collectionMethod"] = json!("directOnly");
        let GuestReply::InventoryData { records, .. } =
            finish(reply("BO"), &state, None, "materialize").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            records[0].1["herbertSources"]["binaryFiles"],
            definition("BO")["herbertSources"]["binaryFiles"]
        );
        assert_eq!(
            records[0].1["herbertSources"]["files"][0]["text"],
            "authored source"
        );
        assert_eq!(
            records[0].1["herbertCollectionStatus"]["completeness"],
            "partial"
        );
    }
    #[test]
    fn native_absence_is_distinct_from_failed_read() {
        let mut definition = definition("BO");
        let native = response("BO", &definition, false);
        let (jobs, diagnostics) = augment(&owner("BO"), &mut definition, &state(), Some(&native));
        assert!(jobs.is_empty());
        assert!(diagnostics.is_empty());
        let manifest: Value = serde_json::from_str(
            definition["herbertSources"]["files"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["captures"][0]["binaryState"], "verifiedAbsent");
    }
    #[test]
    fn original_files_are_written_before_local_bpm_queue_enrollment() {
        let data = definition("DB");
        let native = response("DB", &data, true);
        let GuestReply::ReadyRecords { records, state } =
            finish(reply("DB"), &state(), Some(&native), "materialize").unwrap()
        else {
            panic!()
        };
        assert_eq!(
            records[0].1["herbertSources"]["nativeFiles"][0]["file"],
            json!(input())
        );
        let GuestReply::Action {
            action: HostAction::QueueLocalDecompilation { request },
            ..
        } = drive(state).unwrap()
        else {
            panic!()
        };
        assert_eq!(request.owner, owner("DB"));
        assert!(request.output_subdirectory.starts_with("Compiled/"));
        assert!(request.output_subdirectory.ends_with("/Decompiled"));
        assert_eq!(request.input, input());
    }
    #[test]
    fn changed_witness_preserves_originals_but_does_not_queue_current_project() {
        let mut definition = definition("DB");
        let mut native = response("DB", &definition, true);
        native["oDS"]["Owners"][0]["DefinitionWitnessAfter"] = json!("[]");
        let (jobs, diagnostics) = augment(&owner("DB"), &mut definition, &state(), Some(&native));
        assert!(jobs.is_empty());
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "BPM_COMPILED_SOURCE_CHANGING")
        );
        assert_eq!(
            definition["herbertSources"]["nativeFiles"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn hidden_directive_scope_and_wrong_selector_never_get_published() {
        for wrong_selector in [false, true] {
            let mut definition = definition("BO");
            let mut native = response("BO", &definition, true);
            if wrong_selector {
                native["oDS"]["Assemblies"][0]["Assembly"] = json!("other");
            } else {
                native["oDS"]["Owners"][0]["ScopeComplete"] = json!(false);
            }
            let (jobs, diagnostics) =
                augment(&owner("BO"), &mut definition, &state(), Some(&native));
            assert!(jobs.is_empty());
            assert!(!diagnostics.is_empty());
            assert!(
                definition["herbertSources"]["nativeFiles"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
    #[test]
    fn save_first_hands_normal_page_once_before_optional_reader() {
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "materialize".into(),
            category: "bpm-directives".into(),
            company: "TESTCO".into(),
            identity: None,
            cursor: None,
            page_size: None,
            state: state(),
            response: None,
            error: None,
        };
        request.state["dataset"] = json!({"BpDirective":[{"Body":"large authored payload"}]});
        request.state["records"] = json!([definition("BO")]);
        let GuestReply::ReadyRecords { records, state } = begin(reply("BO"), &request).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            records[0].1["herbertSources"]["binaryFiles"],
            definition("BO")["herbertSources"]["binaryFiles"]
        );
        assert_eq!(
            records[0].1["herbertSources"]["files"][0]["text"],
            "authored source"
        );
        assert_eq!(
            records[0].1["herbertSources"]["files"][1]["name"],
            "compiled-context.json"
        );
        assert!(state["data"].is_null());
        assert!(state["dataset"].is_null());
        assert!(state["records"].is_null());
        assert!(
            state["_bpmCompiledFinal"]["identities"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        request.state = state;
        let GuestReply::Action {
            action: HostAction::RetainedLocalArtifactFunctionCall { .. },
            state,
        } = continue_work(&request).unwrap()
        else {
            panic!()
        };
        request.state = state;
        request.error = Some(Diagnostic::new(
            "READER_REFUSED",
            "The optional reader is unavailable.",
        ));
        let GuestReply::Inventory {
            identities,
            authoritative,
            diagnostics,
            ..
        } = continue_work(&request).unwrap()
        else {
            panic!("no repeated normal records")
        };
        assert!(identities.is_empty());
        assert!(authoritative);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].code, "READER_REFUSED");
        assert_eq!(diagnostics[1].code, "BPM_COMPILED_UNAVAILABLE");
        assert_eq!(diagnostics[0].identity, Some(owner("BO")));
        assert_eq!(
            diagnostics[0].context.get("processingIncomplete"),
            Some(&json!(true))
        );
    }
    #[test]
    fn reader_import_failure_keeps_structured_cause_for_saved_page_and_selection() {
        let mut failure = Diagnostic::new(
            "NATIVE_REFUSAL",
            "Native operation returned HTTP 500. Server text was omitted.",
        );
        failure.context = BTreeMap::from([
            ("httpStatus".into(), json!(500)),
            ("service".into(), json!("Ice.Lib.EfxLibraryDesignerSvc")),
            ("method".into(), json!("ImportLibrary")),
            ("responseContentType".into(), json!("text/plain")),
            (
                "correlationId".into(),
                json!("11111111-2222-4333-8444-555555555555"),
            ),
        ]);
        for operation in ["materialize", "acquire"] {
            let owners = if operation == "materialize" {
                vec![owner("BO"), owner("DB")]
            } else {
                vec![owner("DB")]
            };
            let initial = if operation == "materialize" {
                GuestReply::InventoryData {
                    records: owners
                        .iter()
                        .map(|id| (id.clone(), definition(&id.keys["Source"])))
                        .collect(),
                    inventory: Box::new(GuestReply::Inventory {
                        identities: owners.clone(),
                        authoritative: true,
                        diagnostics: vec![],
                        next_cursor: None,
                    }),
                }
            } else {
                GuestReply::Definition {
                    identity: owners[0].clone(),
                    format: "native".into(),
                    definition: definition("DB"),
                    completeness: Completeness::Collected,
                    diagnostics: vec![],
                }
            };
            let mut request = GuestRequest {
                abi: PLUGIN_ABI,
                operation: operation.into(),
                category: "bpm-directives".into(),
                company: "TESTCO".into(),
                identity: (operation == "acquire").then(|| owners[0].clone()),
                cursor: None,
                page_size: None,
                state: state(),
                response: None,
                error: None,
            };
            let GuestReply::ReadyRecords {
                records,
                state: next,
            } = begin(initial, &request).unwrap()
            else {
                panic!("Normal records must precede the optional reader");
            };
            assert_eq!(records.len(), owners.len());
            for ((id, saved), expected_owner) in records.iter().zip(&owners) {
                assert_eq!(id, expected_owner);
                let original = definition(&id.keys["Source"]);
                assert_eq!(saved["definition"], original["definition"]);
                assert_eq!(
                    saved["herbertSources"]["binaryFiles"],
                    original["herbertSources"]["binaryFiles"]
                );
                assert_eq!(
                    saved["herbertSources"]["files"][0],
                    original["herbertSources"]["files"][0]
                );
            }
            request.state = next;
            let GuestReply::Action {
                action: HostAction::RetainedLocalArtifactFunctionCall { .. },
                state: next,
            } = continue_work(&request).unwrap()
            else {
                panic!("Only the optional retained reader should run next");
            };
            request.state = next;
            // A stale successful response must not queue work after this failure.
            request.response = Some(response("DB", &definition("DB"), true));
            request.error = Some(failure.clone());
            let diagnostics = match continue_work(&request).unwrap() {
                GuestReply::Inventory {
                    identities,
                    authoritative,
                    diagnostics,
                    ..
                } if operation == "materialize" => {
                    assert!(identities.is_empty(), "Normal records must not repeat");
                    assert!(authoritative);
                    diagnostics
                }
                GuestReply::Definition {
                    identity,
                    definition,
                    completeness,
                    diagnostics,
                    ..
                } if operation == "acquire" => {
                    assert_eq!(identity, owners[0]);
                    assert_eq!(definition, records[0].1);
                    assert_eq!(completeness, Completeness::Collected);
                    diagnostics
                }
                _ => panic!("Reader failure must finish without another write or queued job"),
            };
            assert_eq!(diagnostics.len(), owners.len() * 2);
            for id in &owners {
                let mut expected = failure.clone();
                expected.identity = Some(id.clone());
                expected
                    .context
                    .insert("processingIncomplete".into(), json!(true));
                let causes: Vec<_> = diagnostics
                    .iter()
                    .filter(|d| d.identity.as_ref() == Some(id) && d.code == failure.code)
                    .collect();
                assert_eq!(causes, vec![&expected]);
                let gaps: Vec<_> = diagnostics
                    .iter()
                    .filter(|d| {
                        d.identity.as_ref() == Some(id) && d.code == "BPM_COMPILED_UNAVAILABLE"
                    })
                    .collect();
                assert_eq!(gaps.len(), 1);
                assert_eq!(gaps[0].context.get("processingIncomplete"), Some(&json!(true)));
            }
            assert_eq!(request.error, Some(failure.clone()), "Do not mutate the host cause");
        }
    }
    #[test]
    fn selected_definition_is_saved_before_read_and_keeps_established_terminal_reply() {
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "bpm-directives".into(),
            company: "TESTCO".into(),
            identity: Some(owner("DB")),
            cursor: None,
            page_size: None,
            state: state(),
            response: None,
            error: None,
        };
        let input = GuestReply::Definition {
            identity: owner("DB"),
            format: "native".into(),
            definition: definition("DB"),
            completeness: Completeness::Collected,
            diagnostics: vec![],
        };
        let GuestReply::ReadyRecords { records, state } = begin(input, &request).unwrap() else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        request.state = state;
        let GuestReply::Action { state, .. } = continue_work(&request).unwrap() else {
            panic!()
        };
        request.state = state;
        request.error = Some(Diagnostic::new(
            "READER_REFUSED",
            "The optional reader is unavailable.",
        ));
        let GuestReply::Definition {
            definition: final_definition,
            completeness,
            diagnostics,
            ..
        } = continue_work(&request).unwrap()
        else {
            panic!()
        };
        assert_eq!(final_definition, records[0].1);
        assert_eq!(completeness, Completeness::Collected);
        assert_eq!(
            diagnostics[0].context.get("processingIncomplete"),
            Some(&json!(true))
        );
    }

    #[test]
    fn host_without_neutral_bridge_keeps_normal_selected_definition() {
        let mut settings = state();
        settings["_hostCapabilities"] = json!({});
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "bpm-directives".into(),
            company: "TESTCO".into(),
            identity: Some(owner("BO")),
            cursor: None,
            page_size: None,
            state: settings,
            response: None,
            error: None,
        };
        let input = GuestReply::Definition {
            identity: owner("BO"),
            format: "native".into(),
            definition: definition("BO"),
            completeness: Completeness::Collected,
            diagnostics: vec![],
        };
        let GuestReply::Definition {
            definition: output,
            completeness,
            diagnostics,
            ..
        } = begin(input, &request).unwrap()
        else {
            panic!(
                "A non-negotiating host must receive normal output without a new action or selected ReadyRecords contract"
            )
        };
        assert_eq!(
            output["herbertSources"]["binaryFiles"],
            definition("BO")["herbertSources"]["binaryFiles"]
        );
        assert_eq!(completeness, Completeness::Partial);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "BPM_COMPILED_UNAVAILABLE")
        );
    }
}
