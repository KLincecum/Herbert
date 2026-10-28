//! C19 converts inventory envelopes into finished records before direct writing.
use herbert_contract::*;
use serde_json::{Value, json};

fn selection(r:&GuestRequest)->Result<(String,String),Diagnostic>{
    let settings=&r.state["_pluginSettings"];
    let family=match settings.get("definitionFamily"){None=>"all",Some(Value::String(value)) if matches!(value.as_str(),"all"|"configurators"|"lookups"|"persistedSources")=>value,_=>return Err(Diagnostic::new("INVALID_SETTING","Select a declared configurator definition family."))};
    let prefix=match settings.get("definitionIdPrefix"){None=>"",Some(Value::String(value))=>value,_=>return Err(Diagnostic::new("INVALID_SETTING","The configurator definition prefix must be a string."))};
    Ok((family.into(),prefix.into()))
}
fn selected(id:&Identity,family:&str,prefix:&str)->bool {
    let (group,key)=match id.kind.as_str(){"Configurator"=>("configurators",Some("ConfigID")),"ConfiguratorLookup"=>("lookups",Some("LookupTblID")),_=>("persistedSources",None)};
    (family=="all"||family==group)&&key.is_none_or(|key|id.keys.get(key).is_some_and(|value|value.starts_with(prefix)))
}
fn settings(mut state:Value,from:&Value)->Value {if let Some(value)=from.get("_pluginSettings"){if !state.is_object(){state=json!({});}state["_pluginSettings"]=value.clone();}state}
fn carry(reply:GuestReply,from:&Value)->GuestReply {match reply {
    GuestReply::Action {action,state}=>GuestReply::Action {action,state:settings(state,from)},
    GuestReply::ReadyRecords {records,state}=>GuestReply::ReadyRecords {records,state:settings(state,from)},
    GuestReply::PreparedPage {mut state,next_cursor,next_state}=>{
        if let Some(items)=state["_hostWorkItems"].as_array_mut(){for item in items {*item=settings(item.take(),from);}}
        GuestReply::PreparedPage {state:settings(state,from),next_cursor,next_state:settings(next_state,from)}
    },other=>other,
}}
/// Native selection runs before stored-body/dependency acquisition; persisted
/// sources use the same family choice after their complete native page validates.
pub fn select_inventory(r:&GuestRequest,reply:GuestReply)->GuestReply {
    if r.operation!="discover" {return reply;}
    let (family,prefix)=match selection(r){Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}};
    let (inventory,mut records)=match reply {GuestReply::InventoryData {inventory,records}=>(*inventory,records),other=>(other,vec![])};
    let GuestReply::Inventory {mut identities,next_cursor,mut authoritative,mut diagnostics}=inventory else{return inventory;};
    let mut root_ids=std::collections::BTreeSet::new();
    for (id,data) in &records {
        let definition=&data["response"]["returnObj"];
        let mut errors=Vec::new();
        if id.kind=="ConfiguratorLookup" {
            errors.extend(crate::native_children::validate("Erp.BO.PcLookupTblSvc","PcLookupTblHed",definition,&["PcLookupTblHedAttch","PcLookupColSetHed","PcLookupColSetDtl","PcLookupTblValues"].map(str::to_owned)));
            let mut exported=json!({"definition":definition});
            if let Err(error)=crate::configurator_lookup_archive::append(&id.keys["Company"],&id.keys["LookupTblID"],&mut exported){errors.push(error);}
        }else if id.kind=="Configurator" {
            errors.extend(crate::native_children::validate("Erp.BO.ConfiguratorDefSvc","PcStatus",definition,&["PcStatusAttch","PcDocRules","PcDocRulesExpr","PcDocVar","PcInputs","PcStrComp","PcTargetEntity"].map(str::to_owned)));
            if definition["PcStatus"][0]["SysRowID"].as_str().is_none_or(str::is_empty) {errors.push(Diagnostic::new("CONFIGURATOR_BULK_INVALID","An acquired visible configurator omitted its row identity."));}
            if !root_ids.insert((id.keys["Company"].to_lowercase(),id.keys["ConfigID"].to_lowercase())) {errors.push(Diagnostic::new("CONFIGURATOR_BULK_INVALID","The acquired native page repeats a configurator identity."));}
        }
        for mut error in errors {error.identity=Some(id.clone());diagnostics.push(error);authoritative=false;}
    }
    identities.retain(|id|selected(id,&family,&prefix));records.retain(|(id,_)|selected(id,&family,&prefix));
    let inventory=GuestReply::Inventory {identities,next_cursor,authoritative,diagnostics};
    if records.is_empty(){inventory}else{GuestReply::InventoryData {inventory:Box::new(inventory),records}}
}

pub fn handle(r: GuestRequest) -> GuestReply {
    let from=r.state.clone();
    if r.operation!="acquire" {if let Err(diagnostic)=selection(&r){return GuestReply::Failure {diagnostic};}}
    carry(handle_inner(r),&from)
}
fn handle_inner(mut r: GuestRequest) -> GuestReply {
    if r.state["c19Output"].is_object() {
        return drive(r);
    }
    if r.state["c19Prepared"].is_object() {
        r.state = settings(r.state["c19Prepared"].clone(),&r.state);
        let reply = crate::collectors::handle(r.clone());
        return receive_page(r, reply);
    }
    let reply = crate::collectors::handle(r.clone());
    let reply=if r.state["source"]=="tables" || r.cursor.as_deref().and_then(|cursor|serde_json::from_str::<Value>(cursor).ok()).is_some_and(|cursor|cursor["source"]=="tables") {select_inventory(&r,reply)}else{reply};
    if r.operation == "discover" && r.state["_hostPreparePage"] == true {
        match reply {
            GuestReply::PreparedPage {
                state,
                next_cursor,
                next_state,
            } => {
                // This prepared native page already has all stored graphs. Its
                // materialize operation only associates them with their owners;
                // do that once before giving the host independent owner states.
                // Native dependencies are still resolved by drive for each owner.
                if r.state["_hostIndependentWorkItems"] == true
                    && state.pointer("/inner/configuratorPage") == Some(&json!(true))
                {
                    let local = GuestRequest {
                        operation: "materialize".into(),
                        state,
                        response: None,
                        error: None,
                        ..r.clone()
                    };
                    return receive_page(r, crate::collectors::handle(local));
                }
                GuestReply::PreparedPage {
                    state: json!({"c19Prepared":state}),
                    next_cursor,
                    next_state,
                }
            },
            other => receive_page(r, other),
        }
    } else {
        reply
    }
}
fn receive_page(r: GuestRequest, reply: GuestReply) -> GuestReply {
    let (inventory, records) = match reply {
        GuestReply::InventoryData { records, inventory } => (*inventory, records),
        inventory @ GuestReply::Inventory { .. } => (inventory, vec![]),
        other => return other,
    };
    let GuestReply::Inventory {
        identities,
        next_cursor,
        authoritative,
        diagnostics,
    } = inventory
    else {
        return crate::failure(
            "CONFIGURATOR_OUTPUT_INVALID",
            "Prepared inventory is invalid.",
        );
    };
    let mut by_identity = std::collections::BTreeMap::new();
    for (id, data) in records {
        by_identity.entry(serde_json::to_string(&id).unwrap()).or_insert(data);
    }
    let queue: Vec<_> = identities
        .iter()
        .map(|id| {
            json!([
                id,
                by_identity
                    .remove(&serde_json::to_string(id).unwrap())
                    .unwrap_or(Value::Null)
            ])
        })
        .collect();
    if r.operation == "discover" && r.state["_hostIndependentWorkItems"] == true {
        let count = queue.len();
        let mut items: Vec<_> = queue.into_iter().zip(identities).map(|(entry, id)|
            json!({"c19Output":{"queue":[entry],"index":0,"identities":[id],"next":null,"authoritative":true,"diagnostics":[]},"inner":{}})).collect();
        // Preserve page-level coverage exactly once, including an empty page.
        // Individual owner errors continue to be attributed in drive below.
        if items.is_empty() {
            items.push(json!({"c19Output":{"queue":[],"index":0,"identities":[],"next":null},"inner":{}}));
        }
        items[0]["c19Output"]["authoritative"] = json!(authoritative);
        items[0]["c19Output"]["diagnostics"] = json!(diagnostics);
        return GuestReply::PreparedPage {
            state: json!({"_hostWorkItems":items,"_hostWorkNextCursor":next_cursor,"_progress":{"pageRecords":count}}),
            next_cursor,
            next_state: json!({}),
        };
    }
    let state = json!({"c19Output":{"queue":queue,"index":0,"identities":identities,"next":next_cursor,"authoritative":authoritative,"diagnostics":diagnostics},"inner":{}});
    if r.operation == "discover" {
        GuestReply::PreparedPage {
            state,
            next_cursor,
            next_state: json!({}),
        }
    } else {
        drive(GuestRequest {
            state,
            response: None,
            error: None,
            ..r
        })
    }
}
fn drive(mut r: GuestRequest) -> GuestReply {
    let mut page = r.state["c19Output"].take();
    let Some(index) = page["index"].as_u64().map(|v| v as usize) else {
        return crate::failure("CONFIGURATOR_OUTPUT_INVALID", "Missing output index.");
    };
    let Some(queue) = page["queue"].as_array() else {
        return crate::failure("CONFIGURATOR_OUTPUT_INVALID", "Missing output records.");
    };
    if index == queue.len() {
        return GuestReply::Inventory {
            identities: serde_json::from_value(page["identities"].take()).unwrap(),
            next_cursor: page["next"].as_str().map(str::to_owned),
            authoritative: page["authoritative"] == true,
            diagnostics: serde_json::from_value(page["diagnostics"].take()).unwrap(),
        };
    }
    let Some(entry) = queue.get(index) else {
        return crate::failure("CONFIGURATOR_OUTPUT_INVALID", "Invalid output index.");
    };
    let identity: Identity = match serde_json::from_value(entry[0].clone()) {
        Ok(id) => id,
        Err(_) => return crate::failure("CONFIGURATOR_OUTPUT_INVALID", "Invalid output identity."),
    };
    let retained = entry[1].clone();
    let mut inner = r.state["inner"].take();
    if !inner.is_object() {
        inner = json!({});
    }
    for (key, value) in r
        .state
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| key.starts_with('_'))
    {
        inner[key] = value.clone();
    }
    if retained.is_object() {
        inner["_inventory"] = retained.clone();
    }
    let child = GuestRequest {
        operation: "acquire".into(),
        identity: Some(identity.clone()),
        cursor: None,
        state: inner,
        ..r.clone()
    };
    let reply = crate::collectors::handle(child);
    let (mut definition, completeness, mut diagnostics) = match reply {
        GuestReply::Action { action, state } => {
            return GuestReply::Action {
                action,
                state: json!({"c19Output":page,"inner":state}),
            };
        }
        GuestReply::Definition {
            identity: actual,
            definition,
            completeness,
            diagnostics,
            ..
        } if actual == identity => (definition, completeness, diagnostics),
        GuestReply::Failure { diagnostic } => (
            json!({"retainedInventory":retained}),
            Completeness::Partial,
            vec![diagnostic],
        ),
        _ => (
            json!({"retainedInventory":retained}),
            Completeness::Partial,
            vec![Diagnostic::new(
                "CONFIGURATOR_OUTPUT_INVALID",
                "Acquisition did not return the requested definition.",
            )],
        ),
    };
    for diagnostic in &mut diagnostics {
        if diagnostic.identity.is_none() {
            diagnostic.identity = Some(identity.clone());
        }
    }
    if completeness != Completeness::Collected {
        page["authoritative"] = json!(false);
    }
    page["diagnostics"]
        .as_array_mut()
        .unwrap()
        .extend(diagnostics.iter().map(|d| json!(d)));
    definition["herbertDirectOutput"] = json!(true);
    definition["herbertCollectionStatus"] =
        json!({"completeness":completeness,"diagnostics":diagnostics});
    page["index"] = json!(index + 1);
    // Release the already materialized input from the continuation.
    page["queue"][index] = Value::Null;
    GuestReply::ReadyRecords {
        records: vec![(identity, definition)],
        state: json!({"c19Output":page,"inner":{}}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selected_request(family:&str,prefix:&str)->GuestRequest {let mut r=request();r.state["_pluginSettings"]=json!({"definitionFamily":family,"definitionIdPrefix":prefix});r}
    fn selected_fixture(r:&GuestRequest)->GuestReply {
        let mut reply=bulk_fixture(true);
        if let GuestReply::InventoryData {records,..}=&mut reply {for (id,data) in records {data["response"]["returnObj"]["PcStatus"][0]["SysRowID"]=json!(if id.keys["ConfigID"]=="A"{"00000000-0000-0000-0000-000000000001"}else{"00000000-0000-0000-0000-000000000002"});for table in ["PcStatusAttch","PcDocRules","PcDocRulesExpr","PcDocVar","PcInputs","PcStrComp","PcTargetEntity"]{data["response"]["returnObj"][table]=json!([]);}}}
        select_inventory(r,reply)
    }
    #[test]
    fn public_excluded_missing_nonarray_duplicate_and_invalid_children_remain_incomplete_with_selected_peer() {
        for defect in ["missing","nonarray","duplicate","invalid"] {for prefix in ["A","Missing"] {
            let mut r=selected_request("configurators",prefix);let mut body=json!({"PcStatus":[],"PcStatusAttch":[],"PcAudit":[],"PcDocRules":[],"PcDocRulesExpr":[],"PcDocVar":[],"PcInputs":[],"PcStrComp":[],"PcTargetEntity":[]});for (id,n) in [("A",1),("B",2)]{body["PcStatus"].as_array_mut().unwrap().push(json!({"Company":"TESTCO","ConfigID":id,"SysRowID":format!("00000000-0000-0000-0000-{n:012}"),"SystemFlag":false}));}
            let row=json!({"Company":"TESTCO","ConfigID":"B","DrawingSeq":1,"Description":"EXCLUDED_PRIVATE_CHILD"});match defect {"missing"=>{body.as_object_mut().unwrap().remove("PcStatusAttch");},"nonarray"=>body["PcStatusAttch"]=json!({"private":"EXCLUDED_PRIVATE_CHILD"}),"duplicate"=>body["PcStatusAttch"]=json!([row.clone(),row]),_=>{let mut row=row;row["DrawingSeq"]=json!(false);body["PcStatusAttch"]=json!([row]);}}
            r.response=Some(json!({"returnObj":body,"parameters":{"morePages":true}}));let reply=handle(r);
            let inventory=if prefix=="A" {let GuestReply::Action {state,..}=reply else{panic!("valid selected peer lost: {defect}")};assert!(state["inner"]["bulk"]["records"]["A"].is_object());assert!(state["inner"]["bulk"]["records"]["B"].is_null());state["inner"]["inventory"].clone()}else{let GuestReply::PreparedPage {state,..}=reply else{panic!()};assert!(state["c19Output"]["queue"].as_array().unwrap().is_empty());json!({"authoritative":state["c19Output"]["authoritative"],"diagnostics":state["c19Output"]["diagnostics"]})};
            assert_eq!(inventory["authoritative"],false,"{defect}/{prefix}");let diagnostics=inventory["diagnostics"].as_array().unwrap();assert!(!diagnostics.is_empty());assert!(!serde_json::to_string(&diagnostics).unwrap().contains("EXCLUDED_PRIVATE_CHILD"));
        }}
    }
    #[test]
    fn public_native_selection_preserves_raw_empty_pages_and_excluded_graph_failures() {
        let mut r=selected_request("configurators","A");let mut body=json!({"PcStatus":[],"PcStatusAttch":[],"PcAudit":[],"PcDocRules":[],"PcDocRulesExpr":[],"PcDocVar":[],"PcInputs":[],"PcStrComp":[],"PcTargetEntity":[]});for (id,n) in [("A",1),("B",2)]{body["PcStatus"].as_array_mut().unwrap().push(json!({"Company":"TESTCO","ConfigID":id,"SysRowID":format!("00000000-0000-0000-0000-{n:012}"),"SystemFlag":false}));}
        r.response=Some(json!({"returnObj":body,"parameters":{"morePages":true}}));let GuestReply::Action {state,..}=handle(r.clone())else{panic!()};assert!(state["inner"]["bulk"]["records"]["A"].is_object());assert!(state["inner"]["bulk"]["records"]["B"].is_null());assert_eq!(state["_pluginSettings"],r.state["_pluginSettings"]);
        r.state["_pluginSettings"]["definitionIdPrefix"]=json!("Missing");let GuestReply::PreparedPage {state,next_cursor,next_state}=handle(r.clone())else{panic!()};assert_eq!(state["c19Output"]["queue"].as_array().unwrap().len(),0);let outer:Value=serde_json::from_str(next_cursor.as_deref().unwrap()).unwrap();let inner:Value=serde_json::from_str(outer["cursor"].as_str().unwrap()).unwrap();assert_eq!(inner["page"],2);assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);
        r.response.as_mut().unwrap()["returnObj"]["PcInputs"]=json!([{"Company":"TESTCO","ConfigID":"MissingParent","SysRowID":"orphan"}]);let GuestReply::PreparedPage {state,..}=handle(r.clone())else{panic!()};assert_eq!(state["c19Output"]["authoritative"],false);assert!(state["c19Output"]["diagnostics"].as_array().unwrap().iter().any(|error|error["code"]=="CONFIGURATOR_NATIVE_PAGE_INVALID"));
        r.response.as_mut().unwrap()["returnObj"]["PcInputs"]=json!([]);r.response.as_mut().unwrap()["returnObj"]["PcStatus"][0]["Company"]=json!("FOREIGN");r.state["_pluginSettings"]["definitionIdPrefix"]=json!("A");let GuestReply::PreparedPage {state,..}=handle(r)else{panic!()};assert!(state["c19Output"]["queue"].as_array().unwrap().is_empty());assert_eq!(state["c19Output"]["authoritative"],false);
    }
    #[test]
    fn family_prefix_selects_before_bulk_and_keeps_independent_persisted_sources_and_raw_cursor() {
        let r=selected_request("configurators","A");let reply=selected_fixture(&r);
        let GuestReply::Action {state,..}=crate::configurator_pages::begin(r.clone(),reply)else{panic!()};assert!(state["bulk"]["records"]["A"].is_object());assert!(state["bulk"]["records"]["B"].is_null());
        let mut r=selected_request("all","Missing");let mut reply=selected_fixture(&r);let id=Identity {category:"C19".into(),kind:"PersistedPcInputVar".into(),scope:Scope::Company {company:"TESTCO".into()},keys:std::collections::BTreeMap::from([("Company".into(),"TESTCO".into()),("SysRowID".into(),"row".into())])};
        if let GuestReply::Inventory {identities,..}=&mut reply{identities.push(id.clone());}else{panic!()};let GuestReply::Inventory {identities,next_cursor,authoritative,diagnostics}=select_inventory(&r,reply)else{panic!()};assert_eq!(identities,vec![id.clone()]);assert_eq!(next_cursor.as_deref(),Some("next"));assert!(!authoritative);assert_eq!(diagnostics.len(),1);
        r.state["_pluginSettings"]["definitionFamily"]=json!("configurators");assert!(matches!(select_inventory(&r,GuestReply::Inventory {identities:vec![id],next_cursor:Some("raw".into()),authoritative:true,diagnostics:vec![]}),GuestReply::Inventory {identities,next_cursor:Some(_),..} if identities.is_empty()));
        for prefix in ["a"," A","%","Ω"]{let r=selected_request("all",prefix);assert!(matches!(selected_fixture(&r),GuestReply::Inventory {identities,next_cursor:Some(_),..} if identities.is_empty()));}
    }
    #[test]
    fn excluded_visible_failures_and_lookup_xml_stay_incomplete_without_payload() {
        let r=selected_request("configurators","Missing");let mut reply=bulk_fixture(true);if let GuestReply::InventoryData {inventory,records}=&mut reply {records[1].1["response"]["returnObj"]["PcStatus"][0]["SysRowID"]=json!(false);if let GuestReply::Inventory {diagnostics,authoritative,..}=inventory.as_mut(){diagnostics.clear();*authoritative=true;}}
        let GuestReply::Inventory {identities,diagnostics,authoritative,..}=select_inventory(&r,reply)else{panic!()};assert!(identities.is_empty());assert!(!authoritative);assert!(diagnostics.iter().all(|error|error.identity.is_some()));
        let schema:Value=serde_json::from_str(include_str!("configurator-lookup-dataset.json")).unwrap();let mut data=json!({"PcLookupTblHedAttch":[]});
        for (table,fields) in schema["tables"].as_object().unwrap(){let mut row=json!({});for field in fields.as_array().unwrap(){row[field["name"].as_str().unwrap()]=match field["type"].as_str().unwrap(){"bool"=>json!(false),"int"|"long"|"decimal"=>json!(0),"DateTime"=>Value::Null,_=>json!("")};}row["Company"]=json!("TESTCO");row["LookupTblID"]=json!("LookupA");row["SysRowID"]=json!("00000000-0000-0000-0000-000000000001");if row.get("ColSetID").is_some(){row["ColSetID"]=json!("S");}if row.get("ColName").is_some(){row["ColName"]=json!("Text");}data[table]=json!([row]);}
        let id=Identity {category:"C19".into(),kind:"ConfiguratorLookup".into(),scope:Scope::Company {company:"TESTCO".into()},keys:std::collections::BTreeMap::from([("Company".into(),"TESTCO".into()),("LookupTblID".into(),"LookupA".into())])};
        let reply=|data|GuestReply::InventoryData {inventory:Box::new(GuestReply::Inventory {identities:vec![id.clone()],next_cursor:Some("raw".into()),authoritative:true,diagnostics:vec![]}),records:vec![(id.clone(),json!({"response":{"returnObj":data}}))]};
        let r=selected_request("lookups","Lookup");let GuestReply::InventoryData {records,inventory}=select_inventory(&r,reply(data.clone()))else{panic!()};assert_eq!(records[0].1["response"]["returnObj"],data);assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,..}));
        data["PcLookupTblValues"][0]["DataValue"]=json!(false);let r=selected_request("configurators","Missing");let GuestReply::Inventory {identities,authoritative,diagnostics,..}=select_inventory(&r,reply(data))else{panic!()};assert!(identities.is_empty());assert!(!authoritative);assert!(diagnostics.iter().any(|error|error.code=="CONFIGURATOR_LOOKUP_XML_INVALID"));
    }
    #[test]
    fn settings_follow_wrapped_prepared_independent_ready_and_exact_actions() {
        let r=selected_request("configurators","A");let from=r.state.clone();let GuestReply::PreparedPage {state,next_state,..}=carry(GuestReply::PreparedPage {state:json!({"_hostWorkItems":[{"inner":{}}]}),next_cursor:Some("raw".into()),next_state:json!({})},&from)else{panic!()};assert_eq!(state["_pluginSettings"],from["_pluginSettings"]);assert_eq!(state["_hostWorkItems"][0]["_pluginSettings"],from["_pluginSettings"]);assert_eq!(next_state["_pluginSettings"],from["_pluginSettings"]);
        let mut exact=request();exact.operation="acquire".into();if let GuestReply::InventoryData {records,..}=bulk_fixture(true){exact.identity=Some(records[0].0.clone());exact.state=json!({"_inventory":records[0].1,"_pluginSettings":{"definitionFamily":false,"definitionIdPrefix":false}});}let GuestReply::Action {state,..}=handle(exact.clone())else{panic!()};assert_eq!(state["_pluginSettings"],exact.state["_pluginSettings"]);
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C19".into(),
            company: "TESTCO".into(),
            identity: None,
            cursor: None,
            page_size: Some(100),
            state: json!({"_hostPreparePage":true}),
            response: None,
            error: None,
        }
    }
    fn bulk_fixture(valid: bool) -> GuestReply {
        let records:Vec<_>=["A","B"].into_iter().map(|name|{
            let id=Identity{category:"C19".into(),kind:"Configurator".into(),scope:Scope::Company{company:"TESTCO".into()},keys:std::collections::BTreeMap::from([("Company".into(),"TESTCO".into()),("ConfigID".into(),name.into())])};
            let row=json!({"Company":"TESTCO","ConfigID":name,"SysRowID":name,"SystemFlag":false});
            (id,json!({"response":{"returnObj":if valid{json!({"PcStatus":[row]})}else{json!({})}},"original":format!("é exact {name}")}))
        }).collect();
        GuestReply::InventoryData{inventory:Box::new(GuestReply::Inventory{identities:records.iter().map(|(id,_)|id.clone()).collect(),next_cursor:Some("next".into()),authoritative:false,diagnostics:vec![Diagnostic::new("PAGE_PARTIAL","Retain page coverage once.")]}),records}
    }
    #[test]
    fn configurator_independent_items_preserve_legacy_output_and_page_diagnostics() {
        let mut legacy=request();
        let GuestReply::PreparedPage{state,..}=receive_page(legacy.clone(),bulk_fixture(false))else{panic!()};
        legacy.state=state;legacy.operation="materialize".into();
        let mut expected=Vec::new();let expected_diagnostics;
        loop{match handle(legacy.clone()){
            GuestReply::ReadyRecords{records,state}=>{expected.extend(records);legacy.state=state;},
            GuestReply::Inventory{diagnostics,authoritative,next_cursor,..}=>{assert!(!authoritative);assert_eq!(next_cursor.as_deref(),Some("next"));expected_diagnostics=diagnostics;break;},
            other=>panic!("{other:?}"),
        }}
        let mut r=request();r.state["_hostIndependentWorkItems"]=json!(true);
        let GuestReply::PreparedPage{state,next_cursor,..}=receive_page(r.clone(),bulk_fixture(false))else{panic!()};
        assert_eq!(next_cursor.as_deref(),Some("next"));assert_eq!(state["_hostWorkNextCursor"],"next");
        let mut actual=Vec::new();let mut diagnostics=Vec::new();
        for item in state["_hostWorkItems"].as_array().unwrap(){
            assert_eq!(item["c19Output"]["queue"].as_array().unwrap().len(),1);
            r.operation="materialize".into();r.state=item.clone();
            let GuestReply::ReadyRecords{records,state}=handle(r.clone())else{panic!()};
            actual.extend(records);r.state=state;
            let GuestReply::Inventory{diagnostics:ds,next_cursor:None,authoritative:false,..}=handle(r.clone())else{panic!()};diagnostics.extend(ds);
        }
        assert_eq!(actual,expected);assert_eq!(json!(diagnostics),json!(expected_diagnostics));
    }
    #[test]
    fn configurator_owner_dependency_and_resume_never_carry_another_owner() {
        let mut r=request();r.state["_hostIndependentWorkItems"]=json!(true);
        let GuestReply::PreparedPage{state,..}=receive_page(r.clone(),bulk_fixture(true))else{panic!()};
        let item=state["_hostWorkItems"][0].clone();
        r.operation="materialize".into();r.state=item.clone();
        let GuestReply::Action{action:HostAction::SharedNativeRead{method,arguments,..},state}=handle(r.clone())else{panic!()};
        assert_eq!(method,"RetrievePcLayeredImagesForExport");assert_eq!(arguments["configID"],"A");
        assert!(!serde_json::to_string(&state).unwrap().contains("exact B"));
        // A native failure still emits this owner's retained definition promptly.
        r.state=state;r.error=Some(Diagnostic::new("TEST_DENIED","Synthetic image refusal."));
        let GuestReply::ReadyRecords{records,state}=handle(r.clone())else{panic!()};
        assert_eq!(records[0].0.keys["ConfigID"],"A");
        assert_eq!(records[0].1["herbertCollectionStatus"]["completeness"],"partial");
        assert_eq!(state["c19Output"]["queue"][0],Value::Null);
        // An unstarted item's saved state can be resumed without its sibling.
        r.state=item;r.error=None;
        assert!(matches!(handle(r),GuestReply::Action{..}));
    }
    #[test]
    fn configurator_empty_independent_page_retains_coverage_and_cursor() {
        let mut r=request();r.state["_hostIndependentWorkItems"]=json!(true);
        let GuestReply::PreparedPage{state,..}=receive_page(r.clone(),GuestReply::Inventory{identities:vec![],next_cursor:Some("next".into()),authoritative:false,diagnostics:vec![Diagnostic::new("EMPTY_PARTIAL","Keep this.")]})else{panic!()};
        assert_eq!(state["_progress"]["pageRecords"],0);
        r.operation="materialize".into();r.state=state["_hostWorkItems"][0].clone();
        assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,next_cursor:None,..}if diagnostics.len()==1));
    }
    #[test]
    fn supplied_complete_public_materialization_replay() {
        let Ok(base) = std::env::var("HERBERT_C19_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let read = |name: &str| -> Value {
                serde_json::from_slice(&std::fs::read(folder.join(name)).unwrap()).unwrap()
            };
            let visible = read("bulk-two-root-complete/calls/001-GetRows.json")["value"].clone();
            let mut dictionary = read("dictionary/fields.json");
            dictionary.as_object_mut().unwrap().extend(
                read("bulk-supplement-dictionary/fields.json")
                    .as_object()
                    .unwrap()
                    .clone(),
            );
            let rows = read("bulk-two-root-complete/rows.json");
            let mut r = request();
            let mut outputs = 0;
            let mut bulk_reads = 0;
            loop {
                match crate::handle("C19", r.clone()) {
                    GuestReply::Action { action, state } => {
                        r.state = state;
                        r.state["_hostPreparePage"] = json!(true);
                        r.response =
                            Some(
                                match action {
                                    HostAction::NativeRead { method, .. }
                                        if method == "GetRows" =>
                                    {
                                        visible.clone()
                                    }
                                    HostAction::SharedNativeRead {
                                        method, arguments, ..
                                    } if method == "GetFieldList" => dictionary
                                        [arguments["dataTableID"].as_str().unwrap()]["value"]
                                        .clone(),
                                    HostAction::NativeRead {
                                        method, arguments, ..
                                    } if method == "Execute" => {
                                        bulk_reads += 1;
                                        json!({"returnObj":{"Results":rows[arguments["queryDS"]["QueryTable"][0]["DBTableName"].as_str().unwrap()]}})
                                    }
                                    HostAction::SharedNativeRead { method, .. }
                                        if method == "GetLicensedModuleAccess" =>
                                    {
                                        read(
                                            "signature-capability/001-GetLicensedModuleAccess.json",
                                        )["value"]
                                            .clone()
                                    }
                                    HostAction::SharedNativeRead { method, .. }
                                        if method == "RetrievePcLayeredImagesForExport" =>
                                    {
                                        json!({"parameters":{"configImportExportLayeredImageTableset":{"ConfigImportExportLayeredImage":[]}}})
                                    }
                                    other => panic!("Unexpected action {other:?}"),
                                },
                            );
                    }
                    GuestReply::PreparedPage { state, .. } => {
                        r.operation = "materialize".into();
                        r.state = state;
                        r.response = None;
                    }
                    GuestReply::ReadyRecords { records, state } => {
                        assert_eq!(records.len(), 1);
                        let definition = &records[0].1;
                        assert_eq!(definition["herbertDirectOutput"], true);
                        assert_eq!(
                            definition["herbertCollectionStatus"]["completeness"],
                            json!(Completeness::Collected)
                        );
                        assert!(
                            definition["herbertSources"]["files"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .any(|f| f["name"].as_str().unwrap().ends_with(".xml"))
                        );
                        outputs += 1;
                        r.state = state;
                        r.response = None;
                    }
                    GuestReply::Inventory {
                        authoritative,
                        identities,
                        next_cursor,
                        ..
                    } => {
                        assert!(authoritative);
                        assert_eq!(identities.len(), 2);
                        assert!(next_cursor.is_some());
                        break;
                    }
                    other => panic!("Unexpected reply {other:?}"),
                }
            }
            assert_eq!(outputs, 2);
            assert_eq!(bulk_reads, 33);
        }
    }
    #[test]
    fn failed_records_preserve_input_and_do_not_stop_the_page() {
        let identities: Vec<Identity> = ["A", "B"]
            .into_iter()
            .map(|id| Identity {
                category: "C19".into(),
                kind: "Configurator".into(),
                scope: Scope::Company {
                    company: "TESTCO".into(),
                },
                keys: std::collections::BTreeMap::from([
                    ("Company".into(), "TESTCO".into()),
                    ("ConfigID".into(), id.into()),
                ]),
            })
            .collect();
        let records = identities
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    json!({"response":{"returnObj":{}},"original":"keep"}),
                )
            })
            .collect();
        let mut r = request();
        let GuestReply::PreparedPage { state, .. } = receive_page(
            r.clone(),
            GuestReply::InventoryData {
                records,
                inventory: Box::new(GuestReply::Inventory {
                    identities: identities.clone(),
                    next_cursor: Some("next".into()),
                    authoritative: true,
                    diagnostics: vec![],
                }),
            },
        ) else {
            panic!()
        };
        r.state = state;
        r.operation = "materialize".into();
        for identity in identities {
            let GuestReply::ReadyRecords { records, state } = handle(r.clone()) else {
                panic!()
            };
            assert_eq!(records[0].0, identity);
            assert_eq!(records[0].1["retainedInventory"]["original"], "keep");
            assert_eq!(
                records[0].1["herbertCollectionStatus"]["completeness"],
                json!(Completeness::Partial)
            );
            r.state = state;
        }
        assert!(
            matches!(handle(r),GuestReply::Inventory {authoritative:false,diagnostics,next_cursor:Some(_),..} if diagnostics.len()==2)
        );
    }
}
