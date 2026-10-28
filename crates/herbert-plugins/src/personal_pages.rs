//! Process complete personalization pages while the next page is received.
//! Retain native identities and use the existing exact dependency collector.
use herbert_contract::*;
use serde_json::{Value, json};

// Freeze the native recovery contract once. Older saved pages can arrive with a
// pending native response; retain it while acquiring the missing contract.
fn recovery_contract(request: &mut GuestRequest) -> Option<GuestReply> {
    if request.state["scope"].get("recoveryContract").is_some() {
        return None;
    }
    if request.state["phase"] == "archiveVersion" {
        let contract = if let Some(error) = request.error.take() {
            json!({"diagnostic":error})
        } else {
            let response = request.response.take().unwrap_or(Value::Null);
            let parts: Option<Vec<u64>> = ["_Major", "_Minor", "_Build", "_Revision"]
                .into_iter().map(|key| response["returnObj"][key].as_u64()).collect();
            match parts {
                Some(parts) => json!({
                    "version":parts.iter().map(u64::to_string).collect::<Vec<_>>().join("."),
                    "zipRoot":if parts[0] == 4 && parts[1] == 3 {"Layers"} else {"Personalizations"}
                }),
                None => json!({"diagnostic":Diagnostic::new("PERSONAL_RECOVERY_VERSION_INVALID",
                    "The native base version did not identify a personalization recovery format.")}),
            }
        };
        let pending = request.state.as_object_mut().unwrap().remove("_archivePending")
            .unwrap_or(Value::Null);
        if !pending.is_object() {
            return Some(crate::failure("PERSONAL_PAGE_INVALID", "The recovery-format continuation is missing."));
        }
        request.state["scope"]["recoveryContract"] = contract;
        request.state["phase"] = pending["phase"].clone();
        request.state["_progress"] = pending["progress"].clone();
        request.response = (!pending["response"].is_null()).then(|| pending["response"].clone());
        request.error = if pending["error"].is_null() {None} else {
            match serde_json::from_value(pending["error"].clone()) {
                Ok(error) => Some(error),
                Err(_) => return Some(crate::failure("PERSONAL_PAGE_INVALID", "The saved native diagnostic is malformed.")),
            }
        };
        return None;
    }
    if !request.state["scope"].is_object() {request.state["scope"] = json!({});}
    request.state["_archivePending"] = json!({"phase":request.state["phase"],
        "progress":request.state["_progress"],"response":request.response,"error":request.error});
    request.state["phase"] = json!("archiveVersion");
    request.state["_progress"] = json!({"request":"Checking the native personalization recovery format",
        "response":"Recovery format received; preparing saved personalizations"});
    Some(GuestReply::Action {
        action: HostAction::SharedNativeRead {service:"Ice.Lib.SessionModSvc".into(),
            method:"GetBaseLevel".into(),arguments:json!({}),export:false},
        state:request.state.clone(),
    })
}

fn user_prefix(request: &GuestRequest) -> Result<String, Diagnostic> {
    match request.state.pointer("/_pluginSettings/userIdPrefix") {
        None => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(Diagnostic::new("INVALID_SETTING", "User ID starts with must be a string.")),
    }
}
fn with_settings(mut state: Value, settings: Option<&Value>) -> Value {
    if let Some(settings) = settings {state["_pluginSettings"] = settings.clone();}
    state
}
fn selected_owner(identity: &Identity, prefix: &str) -> bool {
    prefix.is_empty() || matches!(&identity.scope, Scope::Personal {owner,..} if owner.starts_with(prefix))
}
fn select_page(mut reply: GuestReply, prefix: &str) -> GuestReply {
    match &mut reply {
        GuestReply::InventoryData {records,inventory} => {
            records.retain(|(id,_)| selected_owner(id,prefix));
            if let GuestReply::Inventory {identities,..} = inventory.as_mut() {identities.retain(|id| selected_owner(id,prefix));}
        }
        GuestReply::Inventory {identities,..} => identities.retain(|id| selected_owner(id,prefix)),
        _ => {},
    }
    // Raw continuation, diagnostics and incomplete status remain unchanged.
    reply
}

pub fn methods() -> Vec<(String, String)> {
    vec![
        ("Ice.Lib.SessionModSvc".into(), "GetBaseLevel".into()),
        ("Ice.BO.BAQDesignerSvc".into(), "GetFieldList".into()),
        ("Ice.BO.DynamicQuerySvc".into(), "Execute".into()),
        (
            "Ice.Lib.SessionModSvc".into(),
            "GetCurrentValuesFromServer".into(),
        ),
        ("Ice.Lib.FileStoreSvc".into(), "ReadAllBytes".into()),
    ]
}

pub fn handle(mut request: GuestRequest) -> GuestReply {
    let prefix = if request.operation == "acquire" {String::new()} else {
        match user_prefix(&request) {Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}}
    };
    let settings = request.state.get("_pluginSettings").cloned();
    let tenant_identity = request
        .identity
        .as_ref()
        .and_then(|id| id.keys.get("Company"))
        .is_some_and(|company| company.starts_with("T_"));
    if (request.operation == "discover" || tenant_identity) && !request.state["scope"].is_object() {
        if request.state["phase"] != "sessionScope" {
            return crate::native(
                "Ice.Lib.SessionModSvc",
                "GetCurrentValuesFromServer",
                json!({}),
                false,
                with_settings(json!({"phase":"sessionScope","_progress":{"request":"Reading personalization company and tenant scope"}}), settings.as_ref()),
            );
        }
        if let Some(error) = request.error {
            return GuestReply::Failure { diagnostic: error };
        }
        let parameters = &request
            .response
            .as_ref()
            .unwrap_or(&serde_json::Value::Null)["parameters"];
        if parameters["companyID"] != request.company || !parameters["tenantID"].is_string() {
            return crate::failure(
                "PERSONAL_SCOPE_INVALID",
                "The session did not confirm the selected company and tenant.",
            );
        }
        request.state["scope"] = json!({"tenant":parameters["tenantID"]});
        request.response = None;
    }
    if let Some(reply) = recovery_contract(&mut request) {return reply;}
    if request.operation == "acquire" {
        let scope = request.state["scope"].clone();
        if tenant_identity {
            let expected = format!("T_{}", scope["tenant"].as_str().unwrap_or(""));
            if scope["tenant"].as_str().is_none_or(str::is_empty)
                || request.identity.as_ref().unwrap().keys.get("Company") != Some(&expected)
            {
                return crate::failure(
                    "PERSONAL_SCOPE_INVALID",
                    "The personalization is outside the current tenant.",
                );
            }
            request.company = expected;
        }
        let contract = scope["recoveryContract"].clone();
        let mut reply = acquire(request, &contract);
        if let GuestReply::Action { state, .. } = &mut reply {
            state["scope"] = scope;
            *state = with_settings(state.clone(),settings.as_ref());
        }
        return reply;
    }
    if request.operation == "materialize" || request.state["phase"] == "processing" {
        return process(request);
    }
    let preparing = request.state["_hostPreparePage"] == true;
    let original = request.clone();
    let inner = request.state.get("inner").cloned().unwrap_or(json!({}));
    request.state = inner;
    request.state = with_settings(request.state,settings.as_ref());
    request.state["_personalTenant"] = original.state["scope"]["tenant"].clone();
    let tenant_branch = request
        .cursor
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .is_some_and(|c| c["tenant"] == true);
    if tenant_branch {
        let Some(tenant) = original.state["scope"]["tenant"]
            .as_str()
            .filter(|s| !s.is_empty())
        else {
            return crate::failure(
                "PERSONAL_SCOPE_INVALID",
                "The tenant page has no verified tenant scope.",
            );
        };
        request.company = format!("T_{tenant}");
    }
    if let Some(caller) = original.state.get("_hostCaller") {
        request.state["_hostCaller"] = caller.clone();
    }
    let combined = original.state["_pluginSettings"]["combinedRead"] == true;
    let reply = select_page(if combined { crate::tables::personal_union(request) } else { crate::tables::handle(request) }, &prefix);
    match reply {
        GuestReply::Action { action, state } => {
            let label = if combined {
                match state["phase"].as_str() {
                    Some("metadata") => format!("Reading field definitions for {}", state["table"].as_str().unwrap_or("personalizations")),
                    Some("children") => "Reading chunk dependencies for the combined batch".to_owned(),
                    _ => "Downloading personalization sources together in one BAQ".to_owned(),
                }
            } else { "Downloading complete personalization definitions".to_owned() };
            GuestReply::Action {
                action,
                state: with_settings(json!({"inner":state,"scope":original.state["scope"],"_progress":{
                    "request":label,
                    "response":"Personalization batch received; preparing native files"}}), settings.as_ref()),
            }
        },
        GuestReply::InventoryData { records, inventory } => {
            let next_cursor = match inventory.as_ref() {
                GuestReply::Inventory { next_cursor, .. } => next_cursor.clone(),
                _ => None,
            };
            if preparing && original.state["_hostIndependentWorkItems"] == true {
                let count = records.len();
                let scope = &original.state["scope"];
                let emitted: std::collections::BTreeSet<_> = records.iter()
                    .map(|(id,_)| serde_json::to_string(id).unwrap()).collect();
                let mut items: Vec<_> = records.into_iter().map(|record| {
                    let identity = record.0.clone();
                    with_settings(json!({"phase":"processing","scope":scope,"records":[record],
                        "inventory":GuestReply::Inventory{identities:vec![identity],next_cursor:None,authoritative:true,diagnostics:vec![]},
                        "index":0,"output":[],"errors":[]}), settings.as_ref())
                }).collect();
                let mut final_inventory = *inventory;
                if let GuestReply::Inventory { next_cursor, identities, .. } = &mut final_inventory {
                    *next_cursor = None;
                    identities.retain(|id| !emitted.contains(&serde_json::to_string(id).unwrap()));
                }
                // Page-wide identities/coverage appear once. Each preceding item
                // retains only its owner through image and native dependencies.
                items.push(with_settings(json!({"phase":"processing","scope":scope,"records":[],
                    "inventory":final_inventory,"index":0,"output":[],"errors":[]}), settings.as_ref()));
                return GuestReply::PreparedPage {
                    // A failed acquisition can emit only a diagnostic. Let the
                    // host count actual outputs rather than require every input.
                    state:with_settings(json!({"scope":scope,"_hostWorkItems":items,"_hostWorkNextCursor":next_cursor,"_progress":{"pageItems":count}}), settings.as_ref()),
                    next_cursor,
                    next_state:with_settings(json!({"scope":scope,"inner":{"metadata":original.state["inner"]["metadata"]}}), settings.as_ref()),
                };
            }
            let state = with_settings(json!({"phase":"processing","scope":original.state["scope"],"records":records,"inventory":inventory,
                "index":0,"output":[],"errors":[]}), settings.as_ref());
            if preparing {
                GuestReply::PreparedPage {
                    state,
                    next_state: with_settings(json!({"scope":original.state["scope"],"inner":{"metadata":original.state["inner"]["metadata"]}}), settings.as_ref()),
                    next_cursor,
                }
            } else {
                // The same page path also supports hosts without prefetch.
                let mut r = original;
                r.operation = "materialize".into();
                r.state = state;
                r.response = None;
                r.error = None;
                process(r)
            }
        }
        other => other,
    }
}

fn process(mut request: GuestRequest) -> GuestReply {
    loop {
        let at = request.state["index"].as_u64().unwrap_or(0) as usize;
        let Some(record) = request.state["records"].get(at).cloned() else {
            let Ok(mut inventory) =
                serde_json::from_value::<GuestReply>(request.state["inventory"].clone())
            else {
                return crate::failure(
                    "PERSONAL_PAGE_INVALID",
                    "The personalization inventory checkpoint is invalid.",
                );
            };
            if let GuestReply::Inventory {
                authoritative,
                diagnostics,
                ..
            } = &mut inventory
            {
                let errors: Vec<Diagnostic> =
                    serde_json::from_value(request.state["errors"].clone()).unwrap_or_default();
                *authoritative &= errors.is_empty();
                diagnostics.extend(errors);
            }
            let Ok(records) = serde_json::from_value(request.state["output"].clone()) else {
                return crate::failure(
                    "PERSONAL_PAGE_INVALID",
                    "The personalization output checkpoint is invalid.",
                );
            };
            return GuestReply::InventoryData {
                records,
                inventory: Box::new(inventory),
            };
        };
        let Ok(identity) = serde_json::from_value::<Identity>(record[0].clone()) else {
            return crate::failure(
                "PERSONAL_PAGE_INVALID",
                "The personalization identity checkpoint is invalid.",
            );
        };
        // Pass only this record to the collector. Copying the page here also
        // copies every finished export once for each remaining record.
        let mut inner = GuestRequest {
            abi: request.abi,
            operation: "acquire".into(),
            category: request.category.clone(),
            company: request.company.clone(),
            identity: Some(identity.clone()),
            cursor: None,
            page_size: request.page_size,
            state: request.state.get("itemState").cloned()
                .unwrap_or_else(|| json!({"_inventory":record[1]})),
            response: request.response.take(),
            error: request.error.take(),
        };
        if let Some(tenant) = request.state["scope"]["tenant"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            if identity.keys.get("Company") == Some(&format!("T_{tenant}")) {
                inner.company = format!("T_{tenant}");
            }
        }
        if let Some(caller) = request.state.get("_hostCaller") {
            inner.state["_hostCaller"] = caller.clone();
        }
        match acquire(inner, &request.state["scope"]["recoveryContract"]) {
            GuestReply::Action { action, state } => {
                request.state["itemState"] = state;
                request.state["_progress"] = json!({"request":format!("Reading personalization dependencies ({} of {})",
                    at+1,request.state["records"].as_array().map_or(0,Vec::len)),
                    "response":"Dependency received; building native personalization files"});
                return GuestReply::Action {
                    action,
                    state: request.state,
                };
            }
            GuestReply::Definition {
                mut definition,
                mut diagnostics,
                completeness,
                ..
            } => {
                for diagnostic in &mut diagnostics {
                    diagnostic.identity = Some(identity.clone());
                }
                definition["herbertCollectionStatus"] = json!({
                    "completeness": completeness, "diagnostics": diagnostics
                });
                request.state["output"].as_array_mut().unwrap()
                    .push(json!([identity, definition]));
                request.state["errors"].as_array_mut().unwrap()
                    .extend(diagnostics.into_iter().map(|d| json!(d)));
            }
            GuestReply::Failure { mut diagnostic } => {
                diagnostic.identity = Some(identity);
                request.state["errors"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(diagnostic));
            }
            _ => {
                return crate::failure(
                    "PERSONAL_PAGE_INVALID",
                    "The personalization dependency returned an unexpected result.",
                );
            }
        }
        request.state["index"] = json!(at + 1);
        request.state.as_object_mut().unwrap().remove("itemState");
        request.response = None;
        request.error = None;
    }
}

/// Resolve only binaries already downloaded by this operation. The host checks
/// the file identity and hash before it gives the plugin the image bytes.
fn acquire(request: GuestRequest, contract: &Value) -> GuestReply {
    let mut reply = if request.state["phase"] == "imageFile" {
        let Ok(mut reply) =
            serde_json::from_value::<GuestReply>(request.state["definition"].clone())
        else {
            return crate::failure(
                "PERSONAL_PAGE_INVALID",
                "The saved image checkpoint is invalid.",
            );
        };
        if let GuestReply::Definition {
            definition,
            diagnostics,
            completeness,
            ..
        } = &mut reply
        {
            let at = request.state["image"].as_u64().unwrap_or(0) as usize;
            if let Some(error) = request.error.clone() {
                diagnostics.push(error);
                *completeness = Completeness::Partial;
                definition["homeResources"]["images"][at]["unavailable"] = json!(true);
            } else if let Some(bytes) = request.response.as_ref().and_then(|r| r["base64"].as_str())
            {
                definition["homeResources"]["images"][at]["bytes"]["returnObj"] = json!(bytes);
            } else {
                return crate::failure(
                    "PERSONAL_IMAGE_INVALID",
                    "The image read did not return verified bytes.",
                );
            }
        }
        reply
    } else {
        crate::tables::handle(request)
    };
    if let GuestReply::Definition { definition, .. } = &mut reply {
        if let Some(images) = definition["homeResources"]["images"].as_array() {
            for (at, image) in images.iter().enumerate() {
                if image["unavailable"] == true {
                    continue;
                }
                if let Some(file) = image.pointer("/bytes/returnObj/nativeFile") {
                    let Ok(file) = serde_json::from_value(file.clone()) else {
                        return crate::failure(
                            "PERSONAL_IMAGE_INVALID",
                            "The image file reference is invalid.",
                        );
                    };
                    return GuestReply::Action {
                        action: HostAction::ReadBinary { file },
                        state: json!({"phase":"imageFile","image":at,"definition":reply}),
                    };
                }
            }
        }
    }
    export(reply, contract)
}

fn export(reply: GuestReply, contract: &Value) -> GuestReply {
    let GuestReply::Definition {
        identity,
        mut definition,
        format,
        mut completeness,
        mut diagnostics,
    } = reply
    else {
        return reply;
    };
    definition["herbertDirectOutput"] = json!(true);
    // Use the native owner, not the connected user. Published layouts have no
    // personal owner and belong in the shared group. The host escapes names
    // and retains the complete identity when output names collide.
    definition["herbertOutputGroup"] = json!(match &identity.scope {
        Scope::Personal { owner, .. } => format!("User - {owner}"),
        _ => "Shared layouts".to_owned(),
    });
    if let Err(mut error) = crate::personal_archive::append(&mut definition, contract) {
        error.identity = Some(identity.clone());
        diagnostics.push(error);
        completeness = Completeness::Partial;
    }
    GuestReply::Definition {
        identity,
        definition,
        format,
        completeness,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C13".into(),
            company: "C".into(),
            identity: None,
            cursor: Some(json!({"family":5,"global":false}).to_string()),
            page_size: Some(100),
            state: json!({"_hostPreparePage":true,"scope":{"tenant":"","recoveryContract":{"version":"5.2.100.9","zipRoot":"Personalizations"}}}),
            response: Some(json!({"rows":[{"UserID":"other-owner","FolderSeq":1,
                "Description":"Favorites","SysRowID":"fixture-row","SysRevID":1}]})),
            error: None,
        }
    }
    fn finish_inventory(mut r: GuestRequest) -> (Value,Option<String>) {
        loop {
            match handle(r.clone()) {
                GuestReply::Action {state,..} => {r.state = state;r.state["_hostPreparePage"] = json!(true);r.response = Some(json!({"rows":[]}));}
                GuestReply::PreparedPage {state,next_cursor,..} => return (state,next_cursor),
                other => panic!("Expected validated discovery page, got {other:?}"),
            }
        }
    }
    fn personal_id(owner: &str) -> Identity {
        Identity {category:"C13".into(),kind:"PersistedFavFolder".into(),scope:Scope::Personal {company:"C".into(),owner:owner.into()},keys:std::collections::BTreeMap::from([("UserID".into(),owner.into()),("FolderSeq".into(),"1".into())])}
    }
    #[test]
    fn user_prefix_filters_both_inventory_shapes_without_hiding_diagnostics_or_continuation() {
        let alpha = personal_id("Alpha");let beta = personal_id("Beta");let shared = Identity {category:"C13".into(),kind:"PersistedShellLayout".into(),scope:Scope::Global,keys:std::collections::BTreeMap::new()};
        let mut diagnostic = Diagnostic::new("SOURCE_UNVERIFIED","Acquired row was malformed");diagnostic.identity = Some(beta.clone());
        let inventory = GuestReply::Inventory {identities:vec![alpha.clone(),beta.clone(),shared.clone()],next_cursor:Some("raw-next".into()),authoritative:false,diagnostics:vec![diagnostic.clone()]};
        for prefix in ["","Al","alpha"," Alpha","%"] {
            let expected = if prefix.is_empty() {vec![alpha.clone(),beta.clone(),shared.clone()]} else if prefix == "Al" {vec![alpha.clone()]} else {vec![]};
            let bare = select_page(inventory.clone(),prefix);
            let GuestReply::Inventory {identities,next_cursor,authoritative,diagnostics} = bare else {panic!()};
            assert_eq!(identities,expected);assert_eq!(next_cursor.as_deref(),Some("raw-next"));assert!(!authoritative);assert_eq!(diagnostics,vec![diagnostic.clone()]);
            let rich = GuestReply::InventoryData {records:vec![(alpha.clone(),json!({"body":"selected"})),(beta.clone(),json!({"body":"EXCLUDED-PRIVATE-PAYLOAD"})),(shared.clone(),json!({"body":"shared"}))],inventory:Box::new(inventory.clone())};
            let reply = select_page(rich,prefix);
            if prefix == "Al" {assert!(!serde_json::to_string(&reply).unwrap().contains("EXCLUDED-PRIVATE-PAYLOAD"));}
            let GuestReply::InventoryData {records,inventory} = reply else {panic!()};assert_eq!(records.iter().map(|(id,_)|id.clone()).collect::<Vec<_>>(),expected);
            assert!(matches!(*inventory,GuestReply::Inventory {authoritative:false,diagnostics,..} if diagnostics == vec![diagnostic.clone()]));
        }
        let literal = personal_id(" Ω%_ '");assert!(selected_owner(&literal," Ω%_ '"));assert!(!selected_owner(&literal,"Ω%_ '"));
    }
    #[test]
    fn user_prefix_uses_native_owner_fields_for_all_personal_sources_and_excludes_shared_standalone() {
        let specs: Vec<Value> = serde_json::from_str::<Vec<Value>>(include_str!("tables.json")).unwrap().into_iter().filter(|spec|spec["category"] == "C13").collect();
        for (family,spec) in specs.iter().enumerate() {
            let rows: Vec<Value> = ["Alpha","Beta"].into_iter().enumerate().map(|(index,owner)| {
                let kind = spec["kind"].as_str().unwrap();
                json!({"Company":"C","ProductID":"EP","TypeCode":spec["filters"]["TypeCode"].as_str().unwrap_or(""),"Key1":if kind == "PersistedXXXDef" {owner.to_owned()} else {format!("NotAnOwner-{owner}")},"Key2":"App","Key3":"Desktop","CGCCode":"","SysCharacter02":owner,
                    "UserID":owner,"TenantID":"","LayoutID":format!("layout-{owner}"),"HomePageType":"Modern","SubType":"","FolderSeq":1,"ItemSeq":1,
                    "SystemFlag":false,"SysRowID":format!("source-{family}-{index}"),"SysRevID":9007199254740993u64,"Content":"{}","HerbertContentBytes":4,
                    "ShellHomePage":"<HomePageDataSet/>","HerbertHomeBytes":36,"ShellUserOptions":"<UserOptionsDataSet/>","HerbertOptionsBytes":42,"FavoriteItems":"<FavoriteDataSet/>","HerbertFavoritesBytes":36})
            }).collect();
            let mut r = request();r.cursor = Some(json!({"family":family,"global":false}).to_string());r.state["_hostCaller"] = json!("caller-is-not-owner");r.state["_pluginSettings"] = json!({"userIdPrefix":"Al"});r.response = Some(json!({"rows":rows}));
            let (state,_) = finish_inventory(r.clone());let records = state["records"].as_array().unwrap();
            if spec["owner_field"].is_string() {
                assert_eq!(records.len(),1,"{}",spec["kind"]);let id: Identity = serde_json::from_value(records[0][0].clone()).unwrap();assert!(matches!(id.scope,Scope::Personal {owner,..} if owner == "Alpha"));
                assert_eq!(records[0][1]["row"][spec["owner_field"].as_str().unwrap()],"Alpha");
            } else {assert!(records.is_empty(),"Ownerless published layouts are not standalone user matches");}
            r.state["_pluginSettings"]["userIdPrefix"] = json!("");let (state,_) = finish_inventory(r);assert_eq!(state["records"].as_array().unwrap().len(),2);
        }
    }
    #[test]
    fn user_prefix_combined_and_separate_favorites_preserve_full_selected_output_and_work_items() {
        let rows = vec![json!({"UserID":"Alpha","FolderSeq":1,"Description":"Ω original","SysRowID":"a","SysRevID":9007199254740993u64}),json!({"UserID":"Beta","FolderSeq":1,"Description":"peer","SysRowID":"b","SysRevID":2})];
        let mut separate = request();separate.response = Some(json!({"rows":rows}));separate.state["_pluginSettings"] = json!({"userIdPrefix":"Al","combinedRead":false});
        let (state,_) = finish_inventory(separate.clone());let mut materialize = separate.clone();materialize.operation = "materialize".into();materialize.state = state;materialize.response = None;
        let GuestReply::InventoryData {records:expected,..} = handle(materialize) else {panic!()};assert_eq!(expected.len(),1);
        let mut combined = separate.clone();combined.state["_pluginSettings"]["combinedRead"] = json!(true);
        combined.state["inner"] = json!({"phase":"union","metadata":{"FavFolder":[{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]}});
        let combined_rows: Vec<_> = rows.iter().map(|row| {let mut fields=serde_json::Map::new();fields.insert("RecordKind".into(),json!(5));for (name,value) in row.as_object().unwrap() {fields.insert(format!("FavFolder_{name}"),value.clone());}Value::Object(fields)}).collect();
        combined.response = Some(json!({"returnObj":{"Results":combined_rows,"Errors":[]}}));
        let (state,_) = finish_inventory(combined.clone());let mut materialize = combined;materialize.operation = "materialize".into();materialize.state = state;materialize.response = None;
        let GuestReply::InventoryData {records,..} = handle(materialize) else {panic!()};assert_eq!(records,expected);
        separate.state["_hostIndependentWorkItems"] = json!(true);let (state,_) = finish_inventory(separate.clone());
        let items = state["_hostWorkItems"].as_array().unwrap();assert_eq!(items.len(),2);
        let mut actual = Vec::new();for item in items {assert_eq!(item["_pluginSettings"]["userIdPrefix"],"Al");let mut r=separate.clone();r.operation="materialize".into();r.state=item.clone();r.response=None;let GuestReply::InventoryData {records,..}=handle(r)else{panic!()};actual.extend(records);}
        assert_eq!(actual,expected);
    }
    #[test]
    fn user_prefix_retains_required_ownerless_base_and_original_image_bytes_for_selected_personal_layout() {
        let image_id = "a83369f8-766c-4fc1-abde-080d5093b4de";
        let row = json!({"TenantID":"","UserID":"Alpha","LayoutID":"owned","HomePageType":"Modern","SubType":"","ShellHomePage":"","HerbertHomeBytes":0,"BaseTenantID":"","BaseLayoutID":"base-outside-user-prefix","BaseHomePageType":"Modern","BaseSubType":""});
        let id = Identity {category:"C13".into(),kind:"PersistedShellLayoutPersonal".into(),scope:Scope::Personal {company:"C".into(),owner:"Alpha".into()},keys:["TenantID","UserID","LayoutID","HomePageType","SubType"].into_iter().map(|name|(name.into(),row[name].as_str().unwrap().into())).collect()};
        let mut r=request();r.operation="materialize".into();r.response=None;r.state=json!({"phase":"processing","scope":{"tenant":"","recoveryContract":{"version":"5.2.100.9","zipRoot":"Personalizations"}},"_pluginSettings":{"userIdPrefix":"Al"},"records":[[id,{"row":row,"children":{},"errors":[]}]],"inventory":GuestReply::Inventory {identities:vec![id],next_cursor:None,authoritative:true,diagnostics:vec![]},"index":0,"output":[],"errors":[]});
        let mut base_reads=0;let mut image_reads=0;
        loop {
            match handle(r.clone()) {
                GuestReply::Action {action,state} => {
                    assert_eq!(state["_pluginSettings"]["userIdPrefix"],"Al");r.state=state;
                    match action {
                        HostAction::Query {query} | HostAction::SharedQuery {query} if query.tables[0].table == "ShellLayout" => {
                            base_reads+=1;let body=query.fields.iter().any(|f|f.field == "*");
                            let mut base=json!({"TenantID":"","LayoutID":"base-outside-user-prefix","HomePageType":"Modern","SubType":"","SystemFlag":false,"SysRowID":"base","SysRevID":1});
                            if body {let xml=format!("<HomePageDataSet><HomeTile><ImageRowID>{image_id}</ImageRowID></HomeTile></HomePageDataSet>");base["HerbertHomeBytes"]=json!(xml.encode_utf16().count()*2);base["ShellHomePage"]=json!(xml);}
                            r.response=Some(json!({"rows":[base]}));
                        }
                        HostAction::Query {query} | HostAction::SharedQuery {query} if query.tables[0].table == "FileStore" => {
                            assert_eq!(query.parameters["id"],image_id);r.response=Some(json!({"rows":[{"SysRowID":image_id,"Company":"C","TenantID":"","FileName":"image.png","SysRevID":1,"HerbertContentBytes":"3"}]}));
                        }
                        HostAction::SharedNativeRead {method,..} if method == "ReadAllBytes" => {image_reads+=1;r.response=Some(json!({"returnObj":"YWJj","parameters":{"fileName":"image.png"}}));}
                        other => panic!("Unexpected {other:?}"),
                    }
                }
                GuestReply::InventoryData {records,inventory} => {
                    assert_eq!(records.len(),1);assert_eq!(base_reads,2);assert_eq!(image_reads,1);
                    assert_eq!(records[0].1["homeResources"]["base"]["LayoutID"],"base-outside-user-prefix");assert_eq!(records[0].1["homeResources"]["images"][0]["bytes"]["returnObj"],"YWJj");
                    assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,..}));break;
                }
                other => panic!("Unexpected {other:?}"),
            }
        }
    }
    #[test]
    fn user_prefix_frozen_session_empty_pages_and_exact_routes_remain_independent() {
        let mut r=request();r.page_size=Some(1);r.state["_pluginSettings"]=json!({"userIdPrefix":"Al"});r.response=Some(json!({"rows":[{"UserID":"Beta","FolderSeq":1,"Description":"peer","SysRowID":"before","SysRevID":1}]}));
        let GuestReply::PreparedPage {state,next_state,next_cursor}=handle(r.clone())else{panic!()};assert_eq!(state["records"],json!([]));assert_eq!(next_state["_pluginSettings"]["userIdPrefix"],"Al");assert!(next_cursor.is_some());
        r.state=next_state;r.cursor=next_cursor;r.response=None;let GuestReply::Action {state,..}=handle(r.clone())else{panic!()};assert_eq!(state["_pluginSettings"]["userIdPrefix"],"Al");
        r.state=state;r.state["_hostPreparePage"]=json!(true);r.response=Some(json!({"rows":[{"UserID":"Alpha","FolderSeq":1,"Description":"later","SysRowID":"later","SysRevID":1}]}));
        let (state,_)=finish_inventory(r.clone());assert_eq!(state["records"].as_array().unwrap().len(),1);
        let mut initial=request();initial.state=json!({"_pluginSettings":{"userIdPrefix":"Al","combinedRead":false}});initial.response=None;
        let GuestReply::Action {state,..}=handle(initial.clone())else{panic!()};assert_eq!(state["_pluginSettings"]["userIdPrefix"],"Al");initial.state=state;initial.response=Some(json!({"parameters":{"companyID":"C","tenantID":"42"}}));
        let GuestReply::Action {state,..}=handle(initial)else{panic!()};assert_eq!(state["scope"]["tenant"],"42");assert_eq!(state["_pluginSettings"]["userIdPrefix"],"Al");
        for id in [personal_id("Beta"),Identity {category:"C13".into(),kind:"PersistedShellLayout".into(),scope:Scope::Global,keys:[("TenantID",""),("LayoutID","shared"),("HomePageType","Modern"),("SubType","")].into_iter().map(|(k,v)|(k.into(),v.into())).collect()}] {
            let mut exact=request();exact.operation="acquire".into();exact.identity=Some(id);exact.cursor=None;exact.response=None;
            let base=serde_json::to_value(handle(exact.clone())).unwrap();assert_eq!(base["type"],"action");exact.state["_pluginSettings"]=json!({"userIdPrefix":"Missing"});
            assert_eq!(serde_json::to_value(handle(exact)).unwrap()["action"],base["action"]);
        }
        for invalid in [Value::Null,json!(false),json!(4),json!([])] {r.state["_pluginSettings"]["userIdPrefix"]=invalid;assert!(matches!(handle(r.clone()),GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));}
    }
    #[test]
    fn independent_personalization_owners_match_legacy_files_and_inventory() {
        let mut r=request();
        r.response.as_mut().unwrap()["rows"].as_array_mut().unwrap().push(json!({"UserID":"second-owner","FolderSeq":2,"Description":"é original","SysRowID":"second-row","SysRevID":7}));
        let GuestReply::PreparedPage{state,..}=handle(r.clone())else{panic!()};
        let mut legacy=r.clone();legacy.operation="materialize".into();legacy.state=state;legacy.response=None;
        let GuestReply::InventoryData{records:expected,inventory:expected_inventory}=handle(legacy)else{panic!()};
        r.state["_hostIndependentWorkItems"]=json!(true);
        let GuestReply::PreparedPage{state,next_cursor,..}=handle(r.clone())else{panic!()};
        assert_eq!(state["_progress"]["pageItems"],2);
        let mut actual=Vec::new();let mut ids=Vec::new();let mut ds=Vec::new();
        for item in state["_hostWorkItems"].as_array().unwrap(){
            assert!(item["records"].as_array().unwrap().len()<=1);
            r.operation="materialize".into();r.state=item.clone();r.response=None;
            let GuestReply::InventoryData{records,inventory}=handle(r.clone())else{panic!()};
            let GuestReply::Inventory { identities, .. } = inventory.as_ref() else { panic!() };
            assert!(records.iter().all(|(owner,_)| identities.contains(owner)));
            actual.extend(records);
            let GuestReply::Inventory{identities,diagnostics,next_cursor:None,authoritative:true}=*inventory else{panic!()};ids.extend(identities);ds.extend(diagnostics);
        }
        assert_eq!(actual,expected);
        assert_eq!(json!(GuestReply::Inventory{identities:ids,diagnostics:ds,next_cursor,authoritative:true}),json!(*expected_inventory));
    }
    #[test]
    fn received_favorites_page_builds_files_without_a_second_read() {
        let mut request = request();
        let GuestReply::PreparedPage {
            state, next_cursor, ..
        } = handle(request.clone())
        else {
            panic!()
        };
        assert!(next_cursor.is_some());
        request.operation = "materialize".into();
        request.state = state;
        request.response = None;
        let GuestReply::InventoryData { records, inventory } = handle(request) else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        assert!(
            matches!(&records[0].0.scope, Scope::Personal { company,owner } if company=="C" && owner=="other-owner")
        );
        assert_eq!(
            records[0].1["herbertSources"]["files"][0]["name"],
            "FavFolder.xml"
        );
        assert_eq!(records[0].1["herbertOutputGroup"], "User - other-owner");
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: true,
                ..
            }
        ));
    }
    #[test]
    fn nonprefetch_preserves_the_selected_company() {
        let mut request = request();
        request.state = json!({"scope":{"tenant":"","recoveryContract":{"version":"5.2.100.9","zipRoot":"Personalizations"}}});
        let GuestReply::InventoryData { records, .. } = handle(request) else {
            panic!()
        };
        assert!(matches!(&records[0].0.scope, Scope::Personal { company, .. } if company=="C"));
    }
    #[test]
    fn favorite_owners_are_grouped_independently_of_the_caller() {
        let mut request = request();
        request.response = Some(json!({"rows":[
            {"UserID":"alice","FolderSeq":1,"Description":"Favorites","SysRowID":"a","SysRevID":1},
            {"UserID":"bob/desktop","FolderSeq":1,"Description":"Favorites","SysRowID":"b","SysRevID":1}
        ]}));
        request.state = json!({"scope":{"tenant":"","recoveryContract":{"version":"5.2.100.9","zipRoot":"Personalizations"}},"_hostCaller":"manager"});
        let GuestReply::InventoryData { records, .. } = handle(request) else { panic!() };
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].1["herbertOutputGroup"], "User - alice");
        assert_eq!(records[1].1["herbertOutputGroup"], "User - bob/desktop");
    }
    #[test]
    fn tenant_scope_comes_from_the_session_and_survives_prefetch() {
        let mut r = request();
        r.state = json!({"scope":{"tenant":"67964"},"_hostPreparePage":true});
        r.cursor = Some(json!({"family":0,"global":true,"tenant":true}).to_string());
        r.response = None;
        let GuestReply::Action {action:HostAction::SharedNativeRead {method,..},state} = handle(r.clone()) else {panic!()};
        assert_eq!(method,"GetBaseLevel");r.state=state;
        r.response=Some(json!({"returnObj":{"_Major":4,"_Minor":3,"_Build":200,"_Revision":39}}));
        let GuestReply::Action {
            action: HostAction::Query { query },
            state,
        } = handle(r.clone())
        else {
            panic!()
        };
        assert!(query.parameters.values().any(|v| v == "T_67964"));
        assert_eq!(state["scope"]["tenant"], "67964");
        r.state = state;
        r.state["_hostPreparePage"] = json!(true);
        r.response = Some(json!({"rows":[]}));
        let GuestReply::PreparedPage {
            next_state,
            next_cursor,
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(next_state["scope"]["tenant"], "67964");
        assert_eq!(next_state["scope"]["recoveryContract"]["zipRoot"], "Layers");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&next_cursor.unwrap()).unwrap()["family"],
            1
        );
    }

    #[test]
    fn wave1_page_preserves_exact_partial_status_and_valid_peer() {
        let mut r = request();
        let row = json!({"Company":"C","ProductID":"EP","TypeCode":"KineticGridLayout","Key1":"owner","Key2":"Form","Key3":"grid","CGCCode":"","SysRowID":"row","SystemFlag":false,"Content":"{}","HerbertContentBytes":4});
        let identity = Identity { category:"C13".into(),kind:"PersistedXXXDef".into(),scope:Scope::Personal{company:"C".into(),owner:"owner".into()},keys:["Company","ProductID","TypeCode","Key1","Key2","Key3","CGCCode"].into_iter().map(|k|(k.into(),row[k].as_str().unwrap().into())).collect() };
        let mut bad = row.clone(); bad["HerbertContentBytes"] = json!(0);
        let mut bad_id = identity.clone(); bad_id.keys.insert("Key3".into(),"bad".into()); bad["Key3"] = json!("bad");
        r.operation="materialize".into(); r.response=None;
        r.state=json!({"phase":"processing","scope":{"tenant":"","recoveryContract":{"version":"5.2.100.9","zipRoot":"Personalizations"}},"records":[[bad_id,{"row":bad,"children":{"XXXChunk":[]},"errors":[]}],[identity,{"row":row,"children":{"XXXChunk":[]},"errors":[]}]],"inventory":{"type":"inventory","identities":[bad_id,identity],"nextCursor":null,"authoritative":true,"diagnostics":[]},"index":0,"output":[],"errors":[]});
        let GuestReply::InventoryData{records,inventory}=handle(r) else {panic!()};
        assert_eq!(records.len(),2);
        assert_eq!(records[0].1["herbertCollectionStatus"]["completeness"],"partial");
        assert_eq!(records[0].1["herbertCollectionStatus"]["diagnostics"][0]["identity"],json!(bad_id));
        assert_eq!(records[1].1["herbertCollectionStatus"]["completeness"],"collected");
        assert!(matches!(*inventory,GuestReply::Inventory{authoritative:false,..}));
    }


    #[test]
    fn recovery_contract_preserves_pending_response_phase_and_native_error() {
        let mut r=request();r.state["scope"].as_object_mut().unwrap().remove("recoveryContract");
        r.state["phase"]=json!("processing");let original=r.response.clone();r.error=Some(Diagnostic::new("SOURCE_DENIED","original source cause"));
        let GuestReply::Action {action:HostAction::SharedNativeRead {method,..},state}=recovery_contract(&mut r).unwrap() else {panic!()};
        assert_eq!(method,"GetBaseLevel");r.state=state;r.response=Some(json!({"returnObj":{"_Major":4,"_Minor":3,"_Build":200,"_Revision":39}}));r.error=None;
        assert!(recovery_contract(&mut r).is_none());assert_eq!(r.response,original);assert_eq!(r.state["phase"],"processing");
        assert_eq!(r.error.as_ref().unwrap().code,"SOURCE_DENIED");
        assert_eq!(r.state["scope"]["recoveryContract"],json!({"version":"4.3.200.39","zipRoot":"Layers"}));
        assert!(recovery_contract(&mut r).is_none());assert!(r.state.get("_archivePending").is_none());
    }
    #[test]
    fn recovery_version_failure_is_frozen_without_losing_native_source_response() {
        for malformed in [false,true] {
            let mut r=request();r.state["scope"].as_object_mut().unwrap().remove("recoveryContract");let original=r.response.clone();
            let GuestReply::Action {state,..}=recovery_contract(&mut r).unwrap() else {panic!()};r.state=state;
            r.response=Some(json!({"returnObj":{"_Major":4}}));r.error=(!malformed).then(|| Diagnostic::new("VERSION_DENIED","original version cause"));
            assert!(recovery_contract(&mut r).is_none());assert_eq!(r.response,original);
            assert_eq!(r.state["scope"]["recoveryContract"]["diagnostic"]["code"],if malformed {"PERSONAL_RECOVERY_VERSION_INVALID"} else {"VERSION_DENIED"});
            assert!(recovery_contract(&mut r).is_none());
        }
    }
    #[test]
    fn prepared_and_continuation_items_keep_the_frozen_recovery_contract() {
        for combined in [false,true] {
            let mut r=request();let contract=json!({"version":"4.3.200.39","zipRoot":"Layers"});
            r.state["scope"]["recoveryContract"]=contract.clone();r.state["_hostIndependentWorkItems"]=json!(true);
            r.state["_pluginSettings"]=json!({"combinedRead":combined,"userIdPrefix":""});
            if combined {
                let rows=r.response.as_ref().unwrap()["rows"].as_array().unwrap().clone();
                r.state["inner"]=json!({"phase":"union","metadata":{"FavFolder":[{"FieldName":"SysRowID","DataType":"uniqueidentifier"}]}});
                let rows:Vec<_>=rows.iter().map(|row| {let mut fields=serde_json::Map::new();fields.insert("RecordKind".into(),json!(5));for (key,value) in row.as_object().unwrap() {fields.insert(format!("FavFolder_{key}"),value.clone());}Value::Object(fields)}).collect();
                r.response=Some(json!({"returnObj":{"Results":rows,"Errors":[]}}));
            }
            let GuestReply::PreparedPage {state,next_state,..}=handle(r.clone()) else {panic!()};
            assert_eq!(state["scope"]["recoveryContract"],contract);
            assert_eq!(next_state["scope"]["recoveryContract"],contract);
            for item in state["_hostWorkItems"].as_array().unwrap() {
                assert_eq!(item["scope"]["recoveryContract"],contract);
                let mut work=r.clone();work.operation="materialize".into();work.state=item.clone();work.response=None;
                assert!(matches!(handle(work),GuestReply::InventoryData {..}));
            }
        }
    }

    #[test]
    fn exact_acquisition_freezes_version_without_applying_discovery_controls() {
        let mut r=request();r.operation="acquire".into();r.identity=Some(personal_id("Beta"));r.cursor=None;r.response=None;
        r.state=json!({"_pluginSettings":{"userIdPrefix":false}});
        let GuestReply::Action {action:HostAction::SharedNativeRead {method,..},state}=handle(r.clone()) else {panic!()};
        assert_eq!(method,"GetBaseLevel");r.state=state;
        r.response=Some(json!({"returnObj":{"_Major":5,"_Minor":1,"_Build":100,"_Revision":0}}));
        let GuestReply::Action {action:HostAction::Query {query},state}=handle(r) else {panic!()};
        assert!(query.parameters.values().any(|value|value=="Beta"));
        assert_eq!(state["scope"]["recoveryContract"],json!({"version":"5.1.100.0","zipRoot":"Personalizations"}));
        assert_eq!(state["_pluginSettings"]["userIdPrefix"],false);
    }
}
