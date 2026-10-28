//! Read complete search pages, then build their exports while fetching continues.
use herbert_contract::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

struct Selection { family: String, prefix: String }
fn acquired_graph_errors(identity: &Identity, data: &Value) -> Vec<Diagnostic> {
    let mut errors=Vec::new();
    if let Some(values)=data["errors"].as_array() {
        for value in values {
            let mut diagnostic=serde_json::from_value::<Diagnostic>(value.clone()).unwrap_or_else(|_|Diagnostic::new("SEARCH_PAGE_INVALID","An acquired child graph has an invalid failure diagnostic."));
            diagnostic.identity=Some(identity.clone());errors.push(diagnostic);
        }
    }
    // Reuse the owning materialization validators. These validate saved XML,
    // native text lengths and required child structure before any graph can
    // disappear through selection. The returned definition is not emitted.
    let validation=match identity.kind.as_str() {
        "PersistedQuickSearch"=>crate::search_archive::quick_definition(data).map(|_|()),
        "PersistedContextMenuXml"=>crate::search_archive::context_definition(data).map(|_|()),
        "PersistedContextMenu"=>crate::search_archive::context_graph_definition(data).map(|_|()),
        _=>Ok(()), // Native Named/Enterprise parent/child validation precedes this point.
    };
    if let Err(mut diagnostic)=validation {
        if errors.is_empty() {diagnostic.identity=Some(identity.clone());errors.push(diagnostic);}
    }
    errors
}
impl Selection {
    fn read(request: &GuestRequest) -> Result<Self, Diagnostic> {
        let text = |name: &str, default: &str| match request.state.get("_pluginSettings").and_then(|settings|settings.get(name)) {
            None => Ok(default.to_owned()),
            Some(Value::String(value)) => Ok(value.clone()),
            Some(_) => Err(Diagnostic::new("INVALID_SETTING","Search selection settings must be strings.")),
        };
        let family=text("definitionFamily","all")?;
        if !matches!(family.as_str(),"all"|"quickSearch"|"predictiveSearch"|"namedSearch"|"enterpriseSearch"|"savedContextMenu"|"legacyContextMenu") {
            return Err(Diagnostic::new("INVALID_SETTING","Select a declared search definition family."));
        }
        Ok(Self {family,prefix:text("searchIdPrefix","")?})
    }
    fn matches(&self, identity: &Identity, data: Option<&Value>) -> Result<bool, Diagnostic> {
        let (family,id_key)=match identity.kind.as_str() {
            "PersistedQuickSearch" => {
                let predictive=match data {
                    Some(data)=>Some(data["row"]["IsPredictive"].as_bool().ok_or_else(||Diagnostic::new("SEARCH_PAGE_INVALID","The acquired search omitted its native IsPredictive flag."))?),
                    None=>None,
                };
                (predictive.map(|p|if p {"predictiveSearch"}else{"quickSearch"}),Some("QuickSearchID"))
            },
            "PersistedNamedSearch" => (Some("namedSearch"),Some("NSId")),
            "PersistedESSearch" => (Some("enterpriseSearch"),Some("SearchID")),
            "PersistedContextMenuXml" => (Some("savedContextMenu"),None),
            "PersistedContextMenu" => (Some("legacyContextMenu"),None),
            _ => return Err(Diagnostic::new("SEARCH_PAGE_INVALID","The acquired search has an undeclared source family.")),
        };
        // A diagnostic-only QuickSearch identity cannot establish IsPredictive.
        // Keep its diagnostic, but do not guess a family-specific selected ID.
        let family_match=self.family=="all" || family==Some(self.family.as_str());
        let prefix_match=match id_key {
            Some(key)=>identity.keys.get(key).ok_or_else(||Diagnostic::new("SEARCH_PAGE_INVALID","The acquired search omitted its native search ID."))?.starts_with(&self.prefix),
            None=>true,
        };
        Ok(family_match && prefix_match)
    }
    fn select(&self, mut reply: GuestReply) -> Result<GuestReply, Diagnostic> {
        match &mut reply {
            GuestReply::InventoryData {records,inventory} => {
                let mut selected=BTreeSet::new();let mut selected_records=Vec::new();
                let mut acquired=BTreeSet::new();let mut errors=Vec::new();
                for (identity,data) in std::mem::take(records) {
                    let key=serde_json::to_string(&identity).unwrap();acquired.insert(key.clone());
                    let record_errors=acquired_graph_errors(&identity,&data);let invalid=!record_errors.is_empty();errors.extend(record_errors);
                    let matches=match self.matches(&identity,Some(&data)) {
                        Ok(matches)=>matches,
                        Err(mut diagnostic)=>{
                            if !invalid {diagnostic.identity=Some(identity.clone());errors.push(diagnostic);}
                            // Preserve the previous all-family recovery-source
                            // path when the ID matches, without guessing a
                            // restricted Quick/Predictive family from bad data.
                            self.family=="all" && self.matches(&identity,None).unwrap_or(false)
                        },
                    };
                    if matches {selected.insert(key);selected_records.push((identity,data));}
                }
                *records=selected_records;
                if let GuestReply::Inventory {identities,authoritative,diagnostics,..}=inventory.as_mut() {
                    if !errors.is_empty() {*authoritative=false;diagnostics.extend(errors);}
                    let mut kept=Vec::new();
                    for identity in std::mem::take(identities) {
                        let key=serde_json::to_string(&identity).unwrap();
                        if selected.contains(&key) || !acquired.contains(&key) && self.matches(&identity,None)? {kept.push(identity);}
                    }
                    *identities=kept;
                }
            },
            GuestReply::Inventory {identities,..} => {
                let mut kept=Vec::new();for identity in std::mem::take(identities) {if self.matches(&identity,None)? {kept.push(identity);}}*identities=kept;
            },
            _=>{},
        }
        Ok(reply)
    }
}
fn with_settings(mut state: Value, from: &Value) -> Value {
    if let Some(settings)=from.get("_pluginSettings") {if !state.is_object() {state=json!({});}state["_pluginSettings"]=settings.clone();}
    state
}

fn family(
    stream: u64,
) -> (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
) {
    match stream {
        1 => (
            "NamedSearch",
            &["NamedSearch", "ControlSetting", "WhereClause"],
            &["Company", "SearchForm", "NSId", "UserID", "CalledFrom"],
        ),
        2 => (
            "ESSearch",
            &["ESSearch", "ESSearchFields"],
            &["Company", "GlbCompany", "SearchID"],
        ),
        _ => unreachable!(),
    }
}
pub fn methods() -> Vec<(String, String)> {
    let mut methods = crate::collectors::methods("C14");
    for stream in 1..=2 {
        methods.push((format!("Ice.BO.{}Svc", family(stream).0), "GetRows".into()));
    }
    methods.sort();
    methods.dedup();
    methods
}
fn failure(message: &str) -> GuestReply {
    crate::failure("SEARCH_PAGE_INVALID", message)
}
fn key(row: &Value, keys: &[&str]) -> Result<Vec<String>, &'static str> {
    keys.iter()
        .map(|key| {
            row[*key]
                .as_str()
                .map(str::to_owned)
                .ok_or("A search row omitted a native identity field.")
        })
        .collect()
}
fn native_records(
    stream: u64,
    body: &Value,
    company: &str,
) -> Result<Vec<(Identity, Value)>, &'static str> {
    let (_, tables, keys) = family(stream);
    let roots = body[tables[0]]
        .as_array()
        .ok_or("The search page omitted its parent table.")?;
    let mut records = BTreeMap::new();
    for row in roots {
        let values = key(row, keys)?;
        if row["Company"].as_str() != Some(company) {
            return Err("The native search page belongs to another company.");
        }
        let owner = row["UserID"].as_str().unwrap_or("");
        let id = Identity {
            category: "C14".into(),
            kind: format!("Persisted{}", tables[0]),
            keys: keys
                .iter()
                .zip(&values)
                .map(|(k, v)| ((*k).into(), v.clone()))
                .collect(),
            scope: if !owner.is_empty() {
                Scope::Personal {
                    company: company.into(),
                    owner: owner.into(),
                }
            } else {
                Scope::Company {
                    company: company.into(),
                }
            },
        };
        let mut data = json!({});
        for table in tables {
            data[*table] = json!([]);
        }
        data[tables[0]] = json!([row]);
        if records.insert(values, (id, data)).is_some() {
            return Err("The native search page repeats a parent.");
        }
    }
    for table in &tables[1..] {
        let rows = body[*table]
            .as_array()
            .ok_or("The search page omitted a child table.")?;
        let mut seen = BTreeSet::new();
        for row in rows {
            let id = row["SysRowID"]
                .as_str()
                .ok_or("A search child omitted its row ID.")?;
            if !seen.insert(id) {
                return Err("The search page repeats a child row.");
            }
            let (_, data) = records
                .get_mut(&key(row, keys)?)
                .ok_or("A search child has no parent in this page.")?;
            data[*table].as_array_mut().unwrap().push(row.clone());
        }
    }
    Ok(records.into_values().collect())
}

pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.operation == "materialize" {
        return materialize(request);
    }
    if request.operation != "discover" {
        return crate::collectors::handle(request);
    }
    let selection=match Selection::read(&request) {Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}};
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => v.unwrap_or(json!({"searchVersion":1,"stream":0,"page":1,"cursor":null})),
        Err(_) => return failure("The search continuation is invalid."),
    };
    let Some(stream @ 0..=3) = cursor["stream"].as_u64() else {
        return failure("The search source is invalid.");
    };
    if cursor["searchVersion"] != 1 {
        return failure("Restart search discovery to use the bulk collector.");
    }
    let original = request.clone();
    let reply = if stream == 0 || stream == 3 {
        request.cursor = cursor["cursor"].as_str().map(str::to_owned);
        request.state = with_settings(request.state.get("inner").cloned().unwrap_or(Value::Null),&original.state);
        if stream == 3 {
            crate::tables::context_page(request)
        } else {
            crate::tables::search_page(request)
        }
    } else {
        let (service, tables, _) = family(stream);
        if let Some(error) = request.error {
            GuestReply::Failure { diagnostic: error }
        } else if let Some(response) = request.response {
            let Some(more) = response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
            else {
                return failure("The native search page omitted morePages.");
            };
            let body = crate::body(&response);
            if more && body[tables[0]].as_array().is_none_or(Vec::is_empty) {
                return failure("The native search page did not advance.");
            }
            let records = match native_records(stream, body, &request.company) {
                Ok(records) => records,
                Err(message) => return failure(message),
            };
            GuestReply::InventoryData {
                inventory: Box::new(GuestReply::Inventory {
                    identities: records.iter().map(|(id, _)| id.clone()).collect(),
                    next_cursor: more
                        .then(|| (cursor["page"].as_u64().unwrap_or(1) + 1).to_string()),
                    authoritative: true,
                    diagnostics: vec![],
                }),
                records,
            }
        } else {
            let Some(page @ 1..=2147483647) = cursor["page"].as_u64() else {
                return failure("The native page must be a positive Int32.");
            };
            let Some(size) = request.page_size.filter(|n| *n > 0) else {
                return failure("Supply a positive search page size.");
            };
            let mut args = json!({"pageSize":size,"absolutePage":page});
            for table in tables {
                args[format!("whereClause{table}")] = json!("");
            }
            if stream == 2 {
                args["whereClauseESSearch"] = json!("SystemFlag = false");
            }
            crate::native(
                &format!("Ice.BO.{service}Svc"),
                "GetRows",
                args,
                false,
                Value::Null,
            )
        }
    };
    let next = |inner: Option<String>| {
        if let Some(inner) = inner {
            Some(
                if stream == 0 || stream == 3 {
                    json!({"searchVersion":1,"stream":stream,"cursor":inner})
                } else {
                    json!({"searchVersion":1,"stream":stream,"page":inner.parse::<u64>().unwrap()})
                }
                .to_string(),
            )
        } else if stream < 3 {
            Some(json!({"searchVersion":1,"stream":stream+1,"page":1}).to_string())
        } else {
            None
        }
    };
    let reply=selection.select(reply).unwrap_or_else(|diagnostic|GuestReply::Failure {diagnostic});
    match reply {
        GuestReply::Action { action, state } => GuestReply::Action {
            action,
            state: with_settings(json!({"inner":with_settings(state,&original.state),"_progress":{
            "request":match stream {
                0=>"Reading Quick Searches, Predictive Searches and saved context menus",
                1=>"Reading saved searches, control settings and filter clauses",
                2=>"Reading enterprise searches and their fields",
                _=>"Reading legacy context menus and navigation definitions",
            },
            "response":"Search batch received; building native exports"}}),&original.state),
        },
        GuestReply::InventoryData {
            mut records,
            mut inventory,
        } => {
            let GuestReply::Inventory {
                next_cursor,
                identities,
                ..
            } = inventory.as_mut()
            else {
                return failure("Invalid search inventory.");
            };
            if stream == 3 {
                records.retain(|(_, data)| crate::search_archive::has_custom_context(data));
                *identities = records.iter().map(|(id, _)| id.clone()).collect();
            }
            *next_cursor = next(next_cursor.take());
            let next_cursor = next_cursor.clone();
            let state = with_settings(json!({"stream":stream,"records":records,"inventory":inventory}),&original.state);
            if original.state["_hostPreparePage"] == true {
                GuestReply::PreparedPage {
                    next_cursor,
                    next_state: with_settings(json!({}),&original.state),
                    state,
                }
            } else {
                let mut r = original;
                r.state = state;
                materialize(r)
            }
        }
        GuestReply::Failure { diagnostic } => GuestReply::Inventory {
            identities: vec![],
            next_cursor: next(None),
            authoritative: false,
            diagnostics: vec![diagnostic],
        },
        GuestReply::Inventory {identities,next_cursor,authoritative,diagnostics} => GuestReply::Inventory {identities,next_cursor:next(next_cursor),authoritative,diagnostics},
        other => other,
    }
}

fn materialize(request: GuestRequest) -> GuestReply {
    let Ok(records) =
        serde_json::from_value::<Vec<(Identity, Value)>>(request.state["records"].clone())
    else {
        return failure("Invalid search page records.");
    };
    let Ok(mut inventory) =
        serde_json::from_value::<GuestReply>(request.state["inventory"].clone())
    else {
        return failure("Invalid search page inventory.");
    };
    let stream = request.state["stream"].as_u64().unwrap_or(0);
    let mut output = Vec::new();
    for (identity, data) in records {
        let export = if identity.kind == "PersistedContextMenuXml" {
            crate::search_archive::context_definition(&data)
                .map(|definition| ("ContextXml".into(), definition))
        } else if stream == 3 {
            crate::search_archive::context_graph_definition(&data)
                .map(|definition| ("ContextMenuData".into(), definition))
        } else if stream == 0 {
            crate::search_archive::quick_definition(&data)
        } else {
            Ok((family(stream).0.into(), data.clone()))
        };
        let result = export.and_then(|(service, mut definition)| {
            let root = if service == "ContextMenuData" {
                "ContextMenu"
            } else {
                service.as_str()
            };
            let name = ["QuickSearchID", "NSId", "SearchID", "LikeID"]
                .into_iter()
                .find_map(|key| definition[root][0][key].as_str())
                .unwrap_or("Search")
                .to_owned();
            if service != "ContextXml" {
                crate::search_archive::append(&service, &name, &mut definition)?;
            }
            definition["herbertOutputGroup"] = json!(match &identity.scope {
                Scope::Personal { owner, .. } => format!("User - {owner}"),
                _ => "Shared searches and navigation".into(),
            });
            Ok(definition)
        });
        match result {
            Ok(definition) => output.push((identity, definition)),
            Err(mut error) => {
                error.identity = Some(identity.clone());
                if let GuestReply::Inventory {
                    authoritative,
                    diagnostics,
                    ..
                } = &mut inventory
                {
                    *authoritative = false;
                    diagnostics.push(error);
                }
                output.push((identity, json!({"recoverySource":data})));
            }
        }
    }
    GuestReply::InventoryData {
        records: output,
        inventory: Box::new(inventory),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C14".into(),
            company: "C".into(),
            identity: None,
            cursor: Some(json!({"searchVersion":1,"stream":1,"page":1}).to_string()),
            page_size: Some(100),
            state: json!({"_hostPreparePage":true}),
            response: None,
            error: None,
        }
    }
    fn page() -> Value {
        let root = |owner| json!({"Company":"C","SearchForm":"Form","NSId":"Same","UserID":owner,"CalledFrom":"", "Description":"Saved search"});
        let child = |owner, id| json!({"Company":"C","SearchForm":"Form","NSId":"Same","UserID":owner,"CalledFrom":"", "TableName":"Part","WhereClause":"PartNum = 'A'\r\nAND Description <> 'Ω & < >'","SysRowID":id});
        json!({"NamedSearch":[root("alice"),root("bob")],"ControlSetting":[],"WhereClause":[child("alice","a"),child("bob","b")]})
    }
    fn selected_request(family: &str,prefix: &str) -> GuestRequest {
        let mut r=request();r.state["_pluginSettings"]=json!({"definitionFamily":family,"searchIdPrefix":prefix});r
    }
    fn fixture_id(kind: &str,keys: &[(&str,&str)]) -> Identity {
        Identity {category:"C14".into(),kind:kind.into(),scope:Scope::Company {company:"C".into()},keys:keys.iter().map(|(key,value)|(key.to_string(),value.to_string())).collect()}
    }
    fn selected_records() -> Vec<(Identity,Value)> {
        let mut records=vec![
            (fixture_id("PersistedQuickSearch",&[("QuickSearchID","Alpha")]),json!({"row":{"IsPredictive":false},"children":{"QuickSearchCriteria":[],"QuickSearchValueList":[]}})),
            (fixture_id("PersistedQuickSearch",&[("QuickSearchID","AlphaPredictive")]),json!({"row":{"IsPredictive":true},"children":{"QuickSearchCriteria":[],"QuickSearchValueList":[]}})),
            (fixture_id("PersistedQuickSearch",&[("QuickSearchID","Beta")]),json!({"row":{"IsPredictive":false},"children":{"QuickSearchCriteria":[],"QuickSearchValueList":[]}})),
            (fixture_id("PersistedNamedSearch",&[("NSId","Alpha"),("SearchForm","NotAnID"),("UserID","not-the-caller")]),json!({"NamedSearch":[{"NSId":"Alpha"}],"ControlSetting":[{"Original":"whole controls"}],"WhereClause":[{"Original":"whole clauses"}]})),
            (fixture_id("PersistedESSearch",&[("SearchID","Alpha")]),json!({"ESSearch":[{"SearchID":"Alpha"}],"ESSearchFields":[{"Original":"whole fields"}]})),
            (fixture_id("PersistedContextMenuXml",&[("Key1","CustomContextMenu"),("Key2","OtherOwner"),("Key3","Other")]),json!({"row":{"Company":"C","ProductID":"EP","TypeCode":"Customization","CGCCode":"","Key1":"CustomContextMenu","Key2":"OtherOwner","Key3":"Other","Content":"<CustomizationDS/>","HerbertContentBytes":36},"children":{"XXXChunk":[{"Chunk":"whole chunk"}]}})),
            (fixture_id("PersistedContextMenu",&[("LikeID","OtherLike"),("ContextTypeCode","OtherType")]),json!({"row":{"LikeID":"OtherLike","ContextTypeCode":"OtherType","SystemFlag":true},"children":{"ContextMenuItem":[{"SystemFlag":false,"Original":"authored child"}]}})),
        ];
        for table in crate::search_archive::CONTEXT_CHILDREN {if records[6].1["children"].get(*table).is_none() {records[6].1["children"][*table]=json!([]);}}
        records
    }
    fn inventory(records: Vec<(Identity,Value)>) -> GuestReply {
        GuestReply::InventoryData {inventory:Box::new(GuestReply::Inventory {identities:records.iter().map(|(id,_)|id.clone()).collect(),next_cursor:Some("raw-next".into()),authoritative:true,diagnostics:vec![]}),records}
    }
    #[test]
    fn selection_uses_native_family_flags_and_id_keys_preserving_complete_context_graphs() {
        let records=selected_records();
        for (family,prefix,count) in [("all","",7),("all","Alpha",6),("quickSearch","Alpha",1),("predictiveSearch","Alpha",1),("namedSearch","Alpha",1),("enterpriseSearch","Alpha",1),("savedContextMenu","Missing",1),("legacyContextMenu","Missing",1),("all","alpha",2),("all"," Alpha",2),("all","%",2)] {
            let selection=Selection::read(&selected_request(family,prefix)).unwrap();
            let GuestReply::InventoryData {records:actual,inventory}=selection.select(inventory(records.clone())).unwrap() else {panic!()};
            assert_eq!(actual.len(),count,"{family}/{prefix}");
            for record in &actual {assert_eq!(Some(record),records.iter().find(|(id,_)|id==&record.0));}
            assert!(matches!(*inventory,GuestReply::Inventory {identities,authoritative:true,next_cursor:Some(ref cursor),..} if identities==actual.iter().map(|(id,_)|id.clone()).collect::<Vec<_>>() && cursor=="raw-next"));
        }
        let mut literal=selected_records();literal[0].0.keys.insert("QuickSearchID".into()," Ω%_ ' ".into());
        let selection=Selection::read(&selected_request("quickSearch"," Ω%_ ' ")).unwrap();let GuestReply::InventoryData {records,..}=selection.select(inventory(literal)).unwrap()else{panic!()};assert_eq!(records.len(),1);
    }
    #[test]
    fn selection_preserves_diagnostics_and_raw_bare_inventory_without_guessing_predictive_family() {
        let records=selected_records();let ids=records.iter().map(|(id,_)|id.clone()).collect::<Vec<_>>();
        let mut diagnostic=Diagnostic::new("SOURCE_UNVERIFIED","Acquired source coverage is incomplete");diagnostic.identity=Some(records[2].0.clone());
        for family in ["all","predictiveSearch","savedContextMenu"] {
            let selection=Selection::read(&selected_request(family,"Alpha")).unwrap();
            let reply=GuestReply::Inventory {identities:ids.clone(),next_cursor:Some("raw-next".into()),authoritative:false,diagnostics:vec![diagnostic.clone()]};
            let GuestReply::Inventory {identities,next_cursor,authoritative,diagnostics}=selection.select(reply).unwrap()else{panic!()};
            assert_eq!(next_cursor.as_deref(),Some("raw-next"));assert!(!authoritative);assert_eq!(diagnostics,vec![diagnostic.clone()]);
            assert_eq!(identities.len(),if family=="all" {6}else if family=="savedContextMenu" {1}else{0});
        }
        let mut malformed=selected_records();malformed[2].1["row"]["IsPredictive"]=Value::Null;
        let selection=Selection::read(&selected_request("namedSearch","Alpha")).unwrap();
        let GuestReply::InventoryData {records,inventory:coverage}=selection.select(inventory(malformed.clone())).unwrap()else{panic!()};assert_eq!(records.len(),1);assert_eq!(records[0].0.kind,"PersistedNamedSearch");assert!(!serde_json::to_string(&records).unwrap().contains("excluded"));
        assert!(matches!(*coverage,GuestReply::Inventory {authoritative:false,diagnostics,..} if diagnostics.len()==1 && diagnostics[0].identity.as_ref().unwrap().keys["QuickSearchID"]=="Beta"));
        let selection=Selection::read(&selected_request("all","")).unwrap();let GuestReply::InventoryData {records,..}=selection.select(inventory(malformed.clone())).unwrap()else{panic!()};assert_eq!(records,malformed);
    }
    #[test]
    fn excluded_acquired_graph_errors_keep_selected_good_peers_and_incomplete_coverage() {
        let mut records=selected_records();let failed=records[2].0.clone();records[2].1["errors"]=json!([Diagnostic::new("NATIVE_CHILD_DENIED","The child definition read was denied.")]);
        let selection=Selection::read(&selected_request("namedSearch","Alpha")).unwrap();
        let GuestReply::InventoryData {records:actual,inventory:coverage}=selection.select(inventory(records.clone())).unwrap()else{panic!()};assert_eq!(actual,vec![records[3].clone()]);
        let GuestReply::Inventory {identities,authoritative,diagnostics,next_cursor}=*coverage else{panic!()};assert!(!authoritative);assert_eq!(identities,vec![records[3].0.clone()]);assert_eq!(next_cursor.as_deref(),Some("raw-next"));assert_eq!(diagnostics.len(),1);assert_eq!(diagnostics[0].code,"NATIVE_CHILD_DENIED");assert_eq!(diagnostics[0].identity.as_ref(),Some(&failed));
        let selection=Selection::read(&selected_request("namedSearch","Missing")).unwrap();let GuestReply::InventoryData {records,inventory:coverage}=selection.select(inventory(records)).unwrap()else{panic!()};assert!(records.is_empty());assert!(matches!(*coverage,GuestReply::Inventory {authoritative:false,..}));
    }
    #[test]
    fn excluded_malformed_content_and_child_lengths_validate_before_selection_without_payload_leaks() {
        for defect in ["xml","length","children"] {
            let mut records=selected_records();let failed=if defect=="children" {records[2].0.clone()}else{records[5].0.clone()};
            if defect=="xml" {records[5].1["row"]["Content"]=json!("<PRIVATE_EXCLUDED_PAYLOAD>");records[5].1["row"]["HerbertContentBytes"]=json!("<PRIVATE_EXCLUDED_PAYLOAD>".encode_utf16().count()*2);}
            else if defect=="length" {records[5].1["row"]["HerbertContentBytes"]=json!(0);}
            else {records[2].1["children"]["QuickSearchCriteria"]=json!([{"CriteriaValue":"PRIVATE_EXCLUDED_PAYLOAD","HerbertCriteriaValueBytes":0}]);}
            let selection=Selection::read(&selected_request("namedSearch","Alpha")).unwrap();let GuestReply::InventoryData {records:actual,inventory:coverage}=selection.select(inventory(records.clone())).unwrap()else{panic!()};assert_eq!(actual,vec![records[3].clone()]);assert!(!serde_json::to_string(&actual).unwrap().contains("PRIVATE_EXCLUDED_PAYLOAD"));
            let GuestReply::Inventory {authoritative,diagnostics,..}=*coverage else{panic!()};assert!(!authoritative);assert_eq!(diagnostics.len(),1);assert_eq!(diagnostics[0].identity.as_ref(),Some(&failed));assert!(!serde_json::to_string(&diagnostics).unwrap().contains("PRIVATE_EXCLUDED_PAYLOAD"));
        }
    }
    #[test]
    fn selected_named_and_enterprise_pages_equal_unfiltered_native_xml_and_complete_children() {
        for stream in [1,2] {
            let body=if stream==1 {
                let mut data=page();data["NamedSearch"][1]["NSId"]=json!("Other");data["WhereClause"][1]["NSId"]=json!("Other");data
            } else {json!({"ESSearch":[{"Company":"C","GlbCompany":"","SearchID":"Same","SystemFlag":false,"Description":"Ω whole"},{"Company":"C","GlbCompany":"","SearchID":"Other","SystemFlag":false}],"ESSearchFields":[{"Company":"C","GlbCompany":"","SearchID":"Same","SysRowID":"field","FieldName":"Original"}]})};
            let mut r=selected_request("all","");r.cursor=Some(json!({"searchVersion":1,"stream":stream,"page":1}).to_string());r.response=Some(json!({"returnObj":body,"parameters":{"morePages":false}}));
            let GuestReply::PreparedPage {state,..}=handle(r.clone())else{panic!()};let mut materialize_request=r.clone();materialize_request.operation="materialize".into();materialize_request.state=state;materialize_request.response=None;
            let GuestReply::InventoryData {records:baseline,..}=handle(materialize_request.clone())else{panic!()};assert_eq!(baseline.len(),2);
            r.state["_pluginSettings"]["searchIdPrefix"]=json!("Same");let GuestReply::PreparedPage {state,..}=handle(r.clone())else{panic!()};materialize_request.state=state;
            let GuestReply::InventoryData {records,inventory}=handle(materialize_request)else{panic!()};assert_eq!(records.len(),1);assert_eq!(records[0],baseline.into_iter().find(|(id,_)|id.keys.get(if stream==1 {"NSId"}else{"SearchID"}).map(String::as_str)==Some("Same")).unwrap());
            assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,..}));
        }
    }
    #[test]
    fn selection_validates_unselected_native_children_and_keeps_raw_empty_page_continuation() {
        let mut r=selected_request("namedSearch","Missing");r.response=Some(json!({"returnObj":page(),"parameters":{"morePages":true}}));
        let GuestReply::PreparedPage {state,next_cursor,next_state}=handle(r.clone())else{panic!()};assert_eq!(state["records"],json!([]));assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap()["page"],2);assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);
        r.state=next_state;r.cursor=next_cursor;r.response=None;let GuestReply::Action {state,..}=handle(r.clone())else{panic!()};assert_eq!(state["_pluginSettings"]["searchIdPrefix"],"Missing");
        r.state=state;r.state["_hostPreparePage"]=json!(true);let mut later=page();later["NamedSearch"][0]["NSId"]=json!("MissingLater");later["WhereClause"][0]["NSId"]=json!("MissingLater");r.response=Some(json!({"returnObj":later,"parameters":{"morePages":false}}));
        let GuestReply::PreparedPage {state,next_cursor,..}=handle(r.clone())else{panic!()};assert_eq!(state["records"].as_array().unwrap().len(),1);assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap()["stream"],2);
        let mut broken=page();broken["WhereClause"][1]["UserID"]=json!("foreign");r.response=Some(json!({"returnObj":broken,"parameters":{"morePages":false}}));assert!(matches!(handle(r),GuestReply::Failure {..}));
    }
    #[test]
    fn selection_freezes_outer_inner_stream_states_and_exact_routes_ignore_conflicts() {
        for stream in [0,1,2,3] {
            let mut r=selected_request("savedContextMenu"," Ω%_ ' ");r.cursor=Some(json!({"searchVersion":1,"stream":stream,"page":1}).to_string());
            let GuestReply::Action {state,..}=handle(r.clone())else{panic!()};assert_eq!(state["_pluginSettings"],r.state["_pluginSettings"]);
            if stream==0 || stream==3 {assert_eq!(state["inner"]["_pluginSettings"],r.state["_pluginSettings"]);}
        }
        let mut exact=request();exact.operation="acquire".into();exact.cursor=None;exact.identity=Some(fixture_id("PersistedNamedSearch",&[("Company","C"),("SearchForm","Form"),("NSId","Same"),("UserID","alice"),("CalledFrom","")]));
        let baseline=serde_json::to_value(handle(exact.clone())).unwrap();assert_eq!(baseline["type"],"action");exact.state["_pluginSettings"]=json!({"definitionFamily":"invalid-discovery-choice","searchIdPrefix":false});assert_eq!(serde_json::to_value(handle(exact)).unwrap()["action"],baseline["action"]);
        for invalid in [Value::Null,json!(false),json!(2),json!([])] {let mut r=selected_request("all","");r.state["_pluginSettings"]["searchIdPrefix"]=invalid;assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code=="INVALID_SETTING"));}
        let mut r=selected_request("not-a-family","");assert!(matches!(handle(r.clone()),GuestReply::Failure {diagnostic} if diagnostic.code=="INVALID_SETTING"));r.state["_pluginSettings"]["definitionFamily"]=Value::Null;assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code=="INVALID_SETTING"));
    }
    #[test]
    fn complete_native_page_preserves_owner_and_starts_next_page_before_exports() {
        let mut r = request();
        let GuestReply::Action {
            action: HostAction::NativeRead {
                method, arguments, ..
            },
            ..
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(method, "GetRows");
        assert_eq!(arguments["pageSize"], 100);
        r.response = Some(json!({"returnObj":page(),"parameters":{"morePages":true}}));
        let GuestReply::PreparedPage {
            state, next_cursor, ..
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(
            serde_json::from_str::<Value>(&next_cursor.unwrap()).unwrap()["page"],
            2
        );
        r.operation = "materialize".into();
        r.state = state;
        r.response = None;
        let GuestReply::InventoryData { records, .. } = handle(r) else {
            panic!()
        };
        assert_eq!(records.len(), 2);
        for (id, data) in records {
            let owner = &id.keys["UserID"];
            assert_eq!(data["WhereClause"][0]["UserID"], owner.as_str());
            assert_eq!(data["herbertOutputGroup"], format!("User - {owner}"));
            assert!(data["herbertSources"]["files"][0]["text"]
                .as_str()
                .unwrap()
                .contains("&#13;\n"));
        }
    }
    #[test]
    fn missing_foreign_or_duplicate_children_cannot_complete() {
        let mut p = page();
        p.as_object_mut().unwrap().remove("ControlSetting");
        assert!(native_records(1, &p, "C").is_err());
        p = page();
        p["WhereClause"][0]["UserID"] = json!("foreign");
        assert!(native_records(1, &p, "C").is_err());
        p = page();
        let duplicate = p["WhereClause"][0].clone();
        p["WhereClause"].as_array_mut().unwrap().push(duplicate);
        assert!(native_records(1, &p, "C").is_err());
    }
    #[test]
    fn failed_source_remains_visible_and_advances_to_independent_source() {
        let mut r = request();
        r.error = Some(Diagnostic::new("DENIED", "Read denied"));
        let GuestReply::Inventory {
            authoritative,
            diagnostics,
            next_cursor,
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert!(!authoritative);
        assert_eq!(diagnostics[0].code, "DENIED");
        assert_eq!(
            serde_json::from_str::<Value>(&next_cursor.unwrap()).unwrap()["stream"],
            2
        );
    }
}
