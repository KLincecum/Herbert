//! Read complete schema pages, retaining stock parents only as keys for custom children.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const CHILDREN: &[&str] = &[
    "ZLookupLink", "ZLinkColumn", "ZLookupField", "ZDataField", "ZKey", "ZKeyField",
];
pub fn methods() -> Vec<(String, String)> {
    // Keep exact-identity reads available to targeted acquisition callers.
    crate::collectors::methods("C02")
}
fn key(row: &Value) -> Result<(String, String), &'static str> {
    let text = |name| row[name].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    Ok((text("SystemCode").ok_or("Schema row lacks SystemCode.")?,
        text("DataTableID").ok_or("Schema row lacks DataTableID.")?))
}
fn normalized(k: &(String, String)) -> (String, String) {
    (k.0.to_ascii_lowercase(), k.1.to_ascii_lowercase())
}
fn schema_records(body: &Value) -> Result<Vec<(Identity, Value)>, &'static str> {
    let roots = body["ZDataTable"].as_array().ok_or("Schema response omitted ZDataTable.")?;
    let mut definitions = BTreeMap::new();
    for row in roots {
        let k = key(row)?;
        let system = row["SystemFlag"].as_bool().ok_or("Table omitted SystemFlag.")?;
        let mut definition = json!({"ZDataTable":[if system {
            json!({"SystemCode":k.0,"DataTableID":k.1})
        } else { row.clone() }]});
        if system {
            definition["herbertSchemaContext"] = json!("Parent keys identify the owner of custom additions; the system table definition is not included.");
        }
        for name in CHILDREN { definition[*name] = json!([]); }
        if definitions.insert(normalized(&k), (k, system, definition)).is_some() {
            return Err("Schema page repeated a table identity.");
        }
    }
    for name in CHILDREN {
        let rows = body[*name].as_array().ok_or("Schema response omitted a child table.")?;
        let mut seen = BTreeSet::new();
        for row in rows {
            if row["SystemFlag"].as_bool() != Some(false) {
                return Err("Schema child filter returned a system row or omitted SystemFlag.");
            }
            let id = row["SysRowID"].as_str().filter(|s| !s.is_empty())
                .ok_or("Schema child omitted SysRowID.")?;
            if !seen.insert(id) { return Err("Schema page repeated a child identity."); }
            let k = normalized(&key(row)?);
            let (_, _, definition) = definitions.get_mut(&k)
                .ok_or("Schema child has no parent in this page.")?;
            definition[*name].as_array_mut().unwrap().push(row.clone());
        }
    }
    Ok(definitions.into_values().filter_map(|(k, system, definition)| {
        if system && CHILDREN.iter().all(|name| definition[*name].as_array().unwrap().is_empty()) {
            return None;
        }
        Some((Identity {
            category:"C02".into(),kind:"UDTableSchema".into(),scope:Scope::Global,
            keys:BTreeMap::from([("SystemCode".into(),k.0),("DataTableID".into(),k.1)]),
        },definition))
    }).collect())
}
fn mapping_records(body: &Value, company: &str) -> Result<Vec<(Identity, Value)>, &'static str> {
    let roots = body["UDMapTable"].as_array().ok_or("Mapping response omitted UDMapTable.")?;
    let children = body["UDMapColumn"].as_array().ok_or("Mapping response omitted UDMapColumn.")?;
    let mut definitions = BTreeMap::new();
    for row in roots {
        let owner=row["Company"].as_str().ok_or("Mapping omitted Company.")?;
        if !owner.is_empty() && owner!=company { return Err("Mapping belongs to another company."); }
        let id=row["MapTableID"].as_i64().ok_or("Mapping omitted its integer identity.")?;
        if definitions.insert(id,(owner.to_owned(),json!({"UDMapTable":[row],"UDMapColumn":[]}))).is_some() {
            return Err("Mapping page repeated an identity.");
        }
    }
    let mut seen=BTreeSet::new();
    for row in children {
        let id=row["MapTableID"].as_i64().ok_or("Mapping column omitted its parent identity.")?;
        let (owner,definition)=definitions.get_mut(&id).ok_or("Mapping column has no parent in this page.")?;
        // Native UDMapColumn keys are MapTableID/Seq; Company is not in its DTO.
        // Preserve and check it only if a future native contract actually supplies it.
        if let Some(child_owner) = row.get("Company") {
            if child_owner.as_str()!=Some(owner.as_str()) { return Err("Mapping column belongs to another company."); }
        }
        let row_id=row["SysRowID"].as_str().filter(|s|!s.is_empty()).ok_or("Mapping column omitted SysRowID.")?;
        if !seen.insert(row_id) { return Err("Mapping page repeated a column."); }
        definition["UDMapColumn"].as_array_mut().unwrap().push(row.clone());
    }
    Ok(definitions.into_iter().map(|(id,(owner,definition))| (Identity {
        category:"C02".into(),kind:"UDMapping".into(),
        scope:if owner.is_empty(){Scope::Global}else{Scope::Company{company:owner}},
        keys:BTreeMap::from([("MapTableID".into(),id.to_string())]),
    },definition)).collect())
}
// Validate each retained owner through the reviewed native key contracts.
// Normalize dictionary parent-key casing only in the validation copy.
fn child_diagnostics(records: &[(Identity, Value)], stream: u64) -> Vec<Diagnostic> {
    let (service, root, tables) = if stream == 0 {
        ("Ice.BO.ZDataTableSvc", "ZDataTable", CHILDREN.to_vec())
    } else { ("Ice.BO.UDMapSvc", "UDMapTable", vec!["UDMapColumn"]) };
    let declared = tables.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut diagnostics = Vec::new();
    for (identity, definition) in records {
        let mut checked = definition.clone();
        if stream == 0 {
            for table in &tables {
                for row in checked[*table].as_array_mut().unwrap() {
                    for field in ["SystemCode", "DataTableID"] {
                        row[field] = definition[root][0][field].clone();
                    }
                }
            }
        }
        for mut diagnostic in crate::native_children::validate(service, root, &checked, &declared) {
            diagnostic.identity = Some(identity.clone());
            diagnostics.push(diagnostic);
        }
    }
    diagnostics
}
pub fn handle(request: GuestRequest) -> GuestReply {
    if request.operation!="discover" {
        return crate::collectors::handle(request);
    }
    let family = match request.state.pointer("/_pluginSettings/definitionFamily") {
        None => "schemaAndAllMappings",
        Some(Value::String(value)) if matches!(value.as_str(), "schema" | "mappings" | "schemaAndAllMappings") => value,
        Some(_) => return crate::failure("INVALID_SETTING", "Definition family must select schema tables, UD mappings, or schema tables and all UD mappings."),
    };
    let prefix = match request.state.pointer("/_pluginSettings/schemaTableIdPrefix") {
        None => "",
        Some(Value::String(value)) => value.as_str(),
        Some(_) => return crate::failure("INVALID_SETTING", "Schema table ID starts with must be a string."),
    };
    let include_schema = family != "mappings";
    let include_mappings = family != "schema";
    let fail=|message|crate::failure("SCHEMA_PAGE_UNVERIFIED",message);
    let cursor=match request.cursor.as_deref().map(serde_json::from_str::<Value>).transpose() {
        Ok(v)=>v.unwrap_or(json!({"stream":if include_schema {0}else{1},"page":1})),
        Err(_)=>return fail("Invalid schema page cursor."),
    };
    let (Some(stream @ 0..=1),Some(page @ 1..=2147483647))=(cursor["stream"].as_u64(),cursor["page"].as_u64()) else {
        return fail("Native schema pagination requires a positive Int32 page and a known stream.");
    };
    if stream == 0 && !include_schema || stream == 1 && !include_mappings {
        return fail("The schema page cursor selects an excluded definition family.");
    }
    let incomplete = |diagnostic: Diagnostic| GuestReply::Inventory {
        identities: vec![],
        next_cursor: if stream == 0 && include_mappings { Some(json!({"stream":1,"page":1}).to_string()) } else { None },
        authoritative: false, diagnostics: vec![diagnostic],
    };
    let page_failure = |message: &str| incomplete(Diagnostic::new("SCHEMA_PAGE_UNVERIFIED",
        &format!("Stream {stream}, page {page}: {message}")));
    if let Some(diagnostic) = request.error { return incomplete(diagnostic); }
    let Some(size)=request.page_size.filter(|n|*n>0) else {return page_failure("Supply a positive schema page size.");};
    if request.response.is_none() {
        let mut args=json!({"pageSize":size,"absolutePage":page});
        let service=if stream==0 {
            // Include standard parents so custom children on them are not lost.
            // The response retention policy excludes their full definitions.
            args["whereClauseZDataTable"]=json!("");
            for name in CHILDREN {args[format!("whereClause{name}")]=json!("SystemFlag = false");}
            "Ice.BO.ZDataTableSvc"
        } else {
            args["whereClauseUDMapTable"]=json!(format!("(Company = '{}' OR Company = '')",request.company.replace('\'',"''")));
            args["whereClauseUDMapColumn"]=json!("");
            "Ice.BO.UDMapSvc"
        };
        return crate::native(service,"GetRows",args,false,cursor);
    }
    let response=request.response.as_ref().unwrap();
    let Some(more)=response.pointer("/parameters/morePages").and_then(Value::as_bool) else {
        return page_failure("Native schema response omitted morePages.");
    };
    let records=match if stream==0 {schema_records(crate::body(response))} else {mapping_records(crate::body(response),&request.company)} {
        Ok(records)=>records,Err(message)=>return page_failure(message),
    };
    let root=if stream==0{"ZDataTable"}else{"UDMapTable"};
    if more && crate::body(response)[root].as_array().is_none_or(Vec::is_empty) {
        return page_failure("Native schema pagination did not advance.");
    }
    let next_cursor=if more {Some(json!({"stream":stream,"page":page+1}).to_string())}
        else if stream==0 && include_mappings {Some(json!({"stream":1,"page":1}).to_string())}else{None};
    let diagnostics = child_diagnostics(&records, stream);
    let mut records = records;
    // Validate every acquired owner first, including unselected natural keys.
    // UD mappings have no native table association and remain independent.
    if stream == 0 { records.retain(|(id, _)| id.keys["DataTableID"].starts_with(prefix)); }
    for (identity, definition) in &mut records {
        let owner_diagnostics: Vec<_> = diagnostics.iter()
            .filter(|d| d.identity.as_ref() == Some(identity)).cloned().collect();
        if !owner_diagnostics.is_empty() {
            definition["herbertCollectionStatus"] = json!({"completeness":"partial","diagnostics":owner_diagnostics});
        }
    }
    GuestReply::InventoryData {
        inventory:Box::new(GuestReply::Inventory{
            identities:records.iter().map(|(id,_)|id.clone()).collect(),next_cursor,
            authoritative:diagnostics.is_empty(),diagnostics,
        }),records,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page() -> Value {
        let mut v=json!({"ZDataTable":[
            {"SystemCode":"Ice","DataTableID":"Stock","SystemFlag":true,"NeverExport":"stock"},
            {"SystemCode":"Ice","DataTableID":"Custom","SystemFlag":false,"Description":"kept"},
            {"SystemCode":"Ice","DataTableID":"Unused","SystemFlag":true}
        ]});
        for name in CHILDREN {v[*name]=json!([]);}
        v["ZDataField"]=json!([{"SystemCode":"ICE","DataTableID":"Stock","SystemFlag":false,"SysRowID":"field","FieldName":"Example_c"}]);
        v
    }
    fn discovery(body: Value, settings: Value, more: bool) -> GuestRequest {
        GuestRequest { abi: PLUGIN_ABI, operation: "discover".into(), category: "C02".into(),
            company: "C".into(), identity: None, cursor: None, page_size: Some(3),
            state: json!({"_pluginSettings":settings}),
            response: Some(json!({"returnObj":body,"parameters":{"morePages":more}})), error: None }
    }
    fn unpack(reply: GuestReply) -> (Vec<(Identity, Value)>, Option<String>, bool, Vec<Diagnostic>) {
        let GuestReply::InventoryData { records, inventory } = reply else { panic!("{reply:?}") };
        let GuestReply::Inventory { next_cursor, authoritative, diagnostics, .. } = *inventory else { panic!() };
        (records, next_cursor, authoritative, diagnostics)
    }
    #[test]
    fn prefix_preserves_complete_custom_and_stock_child_graphs_literally() {
        let baseline = unpack(handle(discovery(page(), json!({}), false))).0;
        for (prefix, expected) in [("", 2), ("C", 1), ("Custom", 1), ("custom", 0), (" Custom", 0), ("%", 0), ("Stock", 1)] {
            let (records, _, authoritative, diagnostics) = unpack(handle(discovery(page(), json!({"schemaTableIdPrefix":prefix}), false)));
            assert_eq!(records.len(), expected, "{prefix}"); assert!(authoritative); assert!(diagnostics.is_empty());
            for record in records { assert_eq!(baseline.iter().find(|(id, _)| id == &record.0).unwrap(), &record); }
        }
        let mut body = page();
        body["ZDataTable"].as_array_mut().unwrap().extend([
            json!({"SystemCode":"Erp","DataTableID":"Custom","SystemFlag":false,"Description":"other system"}),
            json!({"SystemCode":"Ice","DataTableID":" Ω%_ '","SystemFlag":false}),
        ]);
        let (records, _, _, _) = unpack(handle(discovery(body.clone(), json!({"schemaTableIdPrefix":"Custom"}), false)));
        assert_eq!(records.len(), 2); assert_ne!(records[0].0.keys["SystemCode"], records[1].0.keys["SystemCode"]);
        assert_eq!(unpack(handle(discovery(body, json!({"schemaTableIdPrefix":" Ω%_ '"}), false))).0.len(), 1);
        let mut r = discovery(page(), json!({}), false);
        r.state = json!({"_pluginState":{"config":{"schemaTableIdPrefix":"missing","definitionFamily":"mappings"}}});
        assert_eq!(unpack(handle(r)).0.len(), 2, "effective settings only; no saved-value fallback");
    }
    #[test]
    fn prefix_validates_unselected_raw_rows_and_natural_keys_before_selection() {
        let mut body = page(); body["ZDataField"][0].as_object_mut().unwrap().remove("FieldName");
        let (records, _, authoritative, diagnostics) = unpack(handle(discovery(body.clone(), json!({"schemaTableIdPrefix":"Custom"}), false)));
        assert_eq!(records.len(), 1); assert!(!authoritative);
        assert!(records[0].1.get("herbertCollectionStatus").is_none());
        assert_eq!(diagnostics[0].identity.as_ref().unwrap().keys["DataTableID"], "Stock");
        body["ZDataField"][0]["DataTableID"] = json!("Missing");
        assert!(matches!(handle(discovery(body, json!({"schemaTableIdPrefix":"Custom"}), false)), GuestReply::Inventory {authoritative:false,..}));
        for bad in [Value::Null, json!(true), json!(3), json!([]), json!({})] {
            assert!(matches!(handle(discovery(page(), json!({"schemaTableIdPrefix":bad}), false)), GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));
        }
        for bad in [Value::Null, json!("all"), json!(true), json!(1)] {
            assert!(matches!(handle(discovery(page(), json!({"definitionFamily":bad}), false)), GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));
        }
    }
    #[test]
    fn prefix_empty_page_continues_natively_and_family_choice_controls_streams() {
        let settings = json!({"definitionFamily":"schema","schemaTableIdPrefix":"Later"});
        let (records, next, authoritative, _) = unpack(handle(discovery(page(), settings.clone(), true)));
        assert!(records.is_empty()); assert!(authoritative);
        assert_eq!(serde_json::from_str::<Value>(next.as_ref().unwrap()).unwrap(), json!({"stream":0,"page":2}));
        let mut later = page(); later["ZDataTable"][1]["DataTableID"] = json!("Later");
        let mut r = discovery(later, settings, false); r.cursor = next;
        let (records, next, _, _) = unpack(handle(r)); assert_eq!(records.len(), 1); assert!(next.is_none());
        for family in ["schema", "mappings", "schemaAndAllMappings"] {
            let mut r = discovery(page(), json!({"definitionFamily":family,"schemaTableIdPrefix":"missing"}), false);
            r.response = None;
            let GuestReply::Action {action:HostAction::NativeRead {service,..},..} = handle(r.clone()) else {panic!()};
            assert_eq!(service, if family == "mappings" {"Ice.BO.UDMapSvc"} else {"Ice.BO.ZDataTableSvc"});
            r.error = Some(Diagnostic::new("DENIED", "refused"));
            let GuestReply::Inventory {next_cursor,authoritative,..} = handle(r) else {panic!()};
            assert!(!authoritative); assert_eq!(next_cursor.is_some(), family == "schemaAndAllMappings");
        }
    }
    #[test]
    fn mapping_scope_is_independent_of_schema_prefix_and_retains_native_sequence_graph() {
        let body = json!({"UDMapTable":[{"MapTableID":9,"MapID":"unrelated","Company":"C"},{"MapTableID":10,"Company":""}],
            "UDMapColumn":[{"MapTableID":9,"Seq":1,"SysRowID":"one","SourceColumnName":"A","TargetColumnName":"B"}]});
        for family in ["mappings", "schemaAndAllMappings"] {
            let mut r = discovery(body.clone(), json!({"definitionFamily":family,"schemaTableIdPrefix":"missing"}), true);
            if family != "mappings" {r.cursor = Some(json!({"stream":1,"page":1}).to_string());}
            let (records, next, authoritative, _) = unpack(handle(r.clone()));
            assert_eq!(records.len(), 2); assert!(authoritative);
            assert_eq!(records[0].1["UDMapColumn"], body["UDMapColumn"]);
            assert_eq!(serde_json::from_str::<Value>(next.as_ref().unwrap()).unwrap(), json!({"stream":1,"page":2}));
            r.cursor = next; r.response.as_mut().unwrap()["parameters"]["morePages"] = json!(false);
            assert!(unpack(handle(r)).1.is_none());
        }
    }
    #[test]
    fn exact_acquisition_ignores_discovery_family_and_prefix_through_current_dispatch() {
        for id in [schema_records(&page()).unwrap()[0].0.clone(), mapping_records(&json!({"UDMapTable":[{"MapTableID":9,"Company":"C"}],"UDMapColumn":[]}), "C").unwrap()[0].0.clone()] {
            let mut r = discovery(Value::Null, json!({}), false);
            let excluded_family = if id.kind == "UDMapping" { "schema" } else { "mappings" };
            r.operation = "acquire".into(); r.identity = Some(id); r.response = None;
            let base = serde_json::to_value(crate::handle("C02", r.clone())).unwrap();
            assert_eq!(base["type"], "action");
            r.state = json!({"_pluginSettings":{"definitionFamily":excluded_family,"schemaTableIdPrefix":"missing"}});
            let selected = serde_json::to_value(crate::handle("C02", r)).unwrap();
            assert_eq!(selected["type"], "action"); assert_eq!(selected["action"], base["action"]);
        }
    }
    #[test]
    fn preserves_custom_parent_and_stock_custom_children_without_stock_definition() {
        let rows=schema_records(&page()).unwrap();
        assert_eq!(rows.len(),2);
        let custom=&rows.iter().find(|(id,_)|id.keys["DataTableID"]=="Custom").unwrap().1;
        assert_eq!(custom["ZDataTable"][0]["Description"],"kept");
        let stock=&rows.iter().find(|(id,_)|id.keys["DataTableID"]=="Stock").unwrap().1;
        assert!(stock["ZDataTable"][0].get("NeverExport").is_none());
        assert_eq!(stock["ZDataField"][0]["FieldName"],"Example_c");
    }
    #[test]
    fn refuses_missing_or_orphaned_or_duplicate_children() {
        let mut p=page();p.as_object_mut().unwrap().remove("ZKeyField");
        assert!(schema_records(&p).is_err());
        p=page();p["ZDataField"][0]["DataTableID"]=json!("Missing");
        assert!(schema_records(&p).is_err());
        p=page();let duplicate=p["ZDataField"][0].clone();p["ZDataField"].as_array_mut().unwrap().push(duplicate);
        assert!(schema_records(&p).is_err());
    }
    #[test]
    fn pages_then_mappings_need_no_discovery_or_per_table_reads() {
        let mut r=GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:"C02".into(),
            company:"C".into(),identity:None,cursor:None,page_size:Some(3),state:Value::Null,response:None,error:None};
        for (stream,number,more) in [(0,1,true),(0,2,false),(1,1,false)] {
            let GuestReply::Action{action:HostAction::NativeRead{service,method,arguments,..},..}=handle(r.clone()) else {panic!("Expected one native read");};
            assert_eq!(method,"GetRows");
            assert_eq!(service,if stream==0{"Ice.BO.ZDataTableSvc"}else{"Ice.BO.UDMapSvc"});
            assert_eq!(arguments["absolutePage"],number);
            let data=if stream==0{page()}else{json!({"UDMapTable":[{"Company":"C","MapTableID":4}],
                "UDMapColumn":[{"Company":"C","MapTableID":4,"SysRowID":"col","Seq":1,"ColumnName":"field"}]})};
            r.response=Some(json!({"returnObj":data,"parameters":{"morePages":more}}));
            let GuestReply::InventoryData{records,inventory}=handle(r.clone())else{panic!("Expected final definitions");};
            assert_eq!(records.len(),if stream==0{2}else{1});
            let GuestReply::Inventory{next_cursor,authoritative,..}=*inventory else{panic!()};
            assert!(authoritative);
            r.cursor=next_cursor;
            r.response=None;
        }
        assert!(r.cursor.is_none());
    }
    #[test]
    fn native_refusal_and_missing_paging_are_not_success() {
        let mut r=GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:"C02".into(),
            company:"C".into(),identity:None,cursor:None,page_size:Some(3),state:Value::Null,response:Some(json!({"returnObj":page()})),error:None};
        assert!(matches!(handle(r.clone()),GuestReply::Inventory{authoritative:false,next_cursor:Some(_),..}));
        r.error=Some(Diagnostic::new("DENIED","Native refusal"));
        assert!(matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,..} if diagnostics[0].code=="DENIED"));
    }
    #[test]
    fn native_mapping_columns_have_no_company_field() {
        let body=json!({"UDMapTable":[{"Company":"C","MapTableID":9}],
            "UDMapColumn":[{"MapTableID":9,"Seq":1,"SysRowID":"native","SourceColumnName":"A","TargetColumnName":"B"}]});
        let records=mapping_records(&body,"C").unwrap();
        assert_eq!(records[0].1["UDMapColumn"],body["UDMapColumn"]);
        let mut foreign=body.clone();foreign["UDMapColumn"][0]["Company"]=json!("OTHER");
        assert!(mapping_records(&foreign,"C").is_err());
        foreign=body.clone();foreign["UDMapColumn"][0]["MapTableID"]=json!(10);
        assert!(mapping_records(&foreign,"C").is_err());
    }


    #[test]
    fn wave1_failed_schema_continues_mapping_without_erasing_failure() {
        for failure in ["refused", "missing-table", "missing-paging", "stalled"] {
            let mut body=page();
            if failure=="missing-table" {body.as_object_mut().unwrap().remove("ZKey");}
            if failure=="stalled" {body["ZDataTable"]=json!([]);body["ZDataField"]=json!([]);}
            let mut r=GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:"C02".into(),company:"C".into(),
                identity:None,cursor:Some(json!({"stream":0,"page":2}).to_string()),page_size:Some(3),state:Value::Null,
                response:Some(json!({"returnObj":body,"parameters":{"morePages":failure=="stalled"}})),error:None};
            if failure=="refused" {r.error=Some(Diagnostic::new("DENIED","Schema refused."));}
            if failure=="missing-paging" {r.response.as_mut().unwrap()["parameters"]=json!({});}
            let GuestReply::Inventory {next_cursor, authoritative, diagnostics,..}=handle(r.clone()) else {panic!("{failure}")};
            assert!(!authoritative);assert_eq!(diagnostics.len(),1);
            r.cursor=next_cursor;r.response=None;r.error=None;
            let GuestReply::Action {action:HostAction::NativeRead{service,..},..}=handle(r.clone()) else {panic!()};
            assert_eq!(service,"Ice.BO.UDMapSvc");
            r.response=Some(json!({"returnObj":{"UDMapTable":[{"Company":"C","MapTableID":1}],
                "UDMapColumn":[{"MapTableID":1,"Seq":1,"SysRowID":"row"}]},"parameters":{"morePages":false}}));
            let GuestReply::InventoryData {records,inventory}=handle(r) else {panic!()};
            assert_eq!(records.len(),1);
            assert!(matches!(*inventory,GuestReply::Inventory{next_cursor:None,authoritative:true,..}));
        }
    }
    #[test]
    fn wave1_schema_native_keys_are_owner_scoped_and_case_tolerant() {
        let mut body=page();
        assert!(child_diagnostics(&schema_records(&body).unwrap(),0).is_empty());
        body["ZDataField"][0].as_object_mut().unwrap().remove("FieldName");
        let rows=schema_records(&body).unwrap();assert_eq!(rows.len(),2);
        let errors=child_diagnostics(&rows,0);
        assert_eq!(errors[0].code,"CHILD_IDENTITY_UNVERIFIED");
        assert!(errors.iter().all(|e|e.identity.as_ref().unwrap().keys["DataTableID"]=="Stock"));
        body=page();let mut duplicate=body["ZDataField"][0].clone();duplicate["SysRowID"]=json!("different");
        duplicate["SystemCode"]=json!("Ice");duplicate["DataTableID"]=json!("STOCK");
        body["ZDataField"].as_array_mut().unwrap().push(duplicate);
        assert!(child_diagnostics(&schema_records(&body).unwrap(),0).iter().any(|e|e.code=="CHILD_IDENTITY_DUPLICATE"));
        body=page();body["ZDataField"][0]["FieldName"]=json!(42);
        assert!(child_diagnostics(&schema_records(&body).unwrap(),0).iter().any(|e|e.code=="CHILD_IDENTITY_UNVERIFIED"));
    }
    #[test]
    fn wave1_mapping_native_sequence_key_is_required_and_unique_without_company() {
        let mut body=json!({"UDMapTable":[{"Company":"C","MapTableID":1}],
            "UDMapColumn":[{"MapTableID":1,"Seq":2,"SysRowID":"one"}]});
        assert!(child_diagnostics(&mapping_records(&body,"C").unwrap(),1).is_empty());
        let mut duplicate=body["UDMapColumn"][0].clone();duplicate["SysRowID"]=json!("two");
        body["UDMapColumn"].as_array_mut().unwrap().push(duplicate);
        assert!(child_diagnostics(&mapping_records(&body,"C").unwrap(),1).iter().any(|e|e.code=="CHILD_IDENTITY_DUPLICATE"));
        body["UDMapColumn"].as_array_mut().unwrap().truncate(1);
        for invalid in [Value::Null,json!(true),json!(1.5)] {
            body["UDMapColumn"][0]["Seq"]=invalid;
            assert!(child_diagnostics(&mapping_records(&body,"C").unwrap(),1).iter().any(|e|e.code=="CHILD_IDENTITY_UNVERIFIED"));
        }
    }


    #[test]
    fn wave1_schema_partial_status_is_limited_to_the_malformed_owner() {
        let mut body=page();body["ZDataField"][0].as_object_mut().unwrap().remove("FieldName");
        let r=GuestRequest{abi:PLUGIN_ABI,operation:"discover".into(),category:"C02".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state:Value::Null,response:Some(json!({"returnObj":body,"parameters":{"morePages":false}})),error:None};
        let GuestReply::InventoryData{records,inventory}=handle(r)else{panic!()};
        assert_eq!(records.len(),2);
        for (identity,data) in records {
            if identity.keys["DataTableID"]=="Stock" {
                assert_eq!(data["herbertCollectionStatus"]["completeness"],"partial");
                assert_eq!(data["herbertCollectionStatus"]["diagnostics"][0]["identity"],json!(identity));
            } else {assert!(data.get("herbertCollectionStatus").is_none());}
        }
        assert!(matches!(*inventory,GuestReply::Inventory{authoritative:false,..}));
    }

}
