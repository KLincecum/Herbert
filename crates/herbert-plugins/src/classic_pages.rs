//! Read full persisted Classic pages, then build native files while the next page downloads.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const TYPES: [&str; 5] = [
    "Customization",
    "BaseExtension",
    "Productization",
    "Verticalization",
    "Localization",
];
fn types(category: &str) -> &[&str] {
    if category == "classic-personalizations" {
        &["Personalization"]
    } else {
        &TYPES
    }
}
pub fn methods() -> Vec<(String, String)> {
    vec![(
        "Ice.Lib.SessionModSvc".into(),
        "GetCurrentValuesFromServer".into(),
    )]
}
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("CLASSIC_PAGE_INVALID", message)
}
fn selection(request: &GuestRequest) -> Result<(String, String), Diagnostic> {
    let first = if request.category == "classic-personalizations" { "userIdPrefix" } else { "layerNamePrefix" };
    let prefix = |key: &str| match request.state.get("_pluginSettings").and_then(|settings| settings.get(key)) {
        None => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(Diagnostic::new("INVALID_SETTING", "Classic selection prefixes must be strings.")),
    };
    Ok((prefix(first)?, prefix("formNamePrefix")?))
}
/// The prefix's leading run before any LIKE wildcard, bracket or non-ASCII character, for a
/// server-side `LIKE 'stem%'` superset that needs no escape syntax. Non-ASCII stops the stem
/// because the literal is not N-prefixed and could be converted lossily. None when nothing
/// narrows the read.
fn like_stem(prefix: &str) -> Option<&str> {
    let stem = &prefix[..prefix.find(|c: char| matches!(c, '%' | '_' | '[' | ']') || !c.is_ascii()).unwrap_or(prefix.len())];
    (!stem.is_empty()).then_some(stem)
}
fn with_settings(mut state: Value, from: &Value) -> Value {
    if let Some(settings) = from.get("_pluginSettings") {state["_pluginSettings"] = settings.clone();}
    state
}
fn validate_content(row: &Value) -> Result<(), Diagnostic> {
    let expected = row["HerbertContentBytes"].as_u64()
        .or_else(|| row["HerbertContentBytes"].as_str().and_then(|value| value.parse().ok()))
        .ok_or_else(|| invalid("The Classic page omitted its Content length check."))?;
    let content = row["Content"].as_str().ok_or_else(|| invalid("The Classic Content is missing."))?;
    if expected != (content.encode_utf16().count() * 2) as u64 {
        return Err(invalid("The downloaded Classic Content is truncated; its Epicor XML was not built."));
    }
    Ok(())
}
pub fn handle(request: GuestRequest) -> GuestReply {
    collect(request).unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic })
}
fn collect(mut request: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if !matches!(
        request.operation.as_str(),
        "discover" | "acquire" | "materialize"
    ) {
        return Err(invalid("Select discovery or an exact Classic identity."));
    }
    let (first_prefix, form_prefix) = if request.operation == "acquire" {
        (String::new(), String::new())
    } else { selection(&request)? };
    if let Some(error) = request.error.take() {
        return Err(error);
    }
    if request.operation == "materialize" || request.state["phase"] == "processing" {
        return process(request);
    }
    match request.state["phase"].as_str().unwrap_or("initial") {
        "initial" => Ok(GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.SessionModSvc".into(),
                method: "GetCurrentValuesFromServer".into(),
                arguments: json!({}),
                export: false,
            },
            state: with_settings(json!({"phase":"scope","_progress":{"request":"Reading Classic company and tenant scope","response":"Scope received; downloading complete Classic definitions"}}), &request.state),
        }),
        "scope" => {
            let p = &request
                .response
                .as_ref()
                .ok_or_else(|| invalid("The session scope is missing."))?["parameters"];
            if p["companyID"] != request.company {
                return Err(invalid(
                    "The session company does not match the selected company.",
                ));
            }
            let tenant = p["tenantID"]
                .as_str()
                .ok_or_else(|| invalid("The session did not identify its tenant scope."))?;
            let mut scopes = vec![request.company.clone(), String::new()];
            // GenXDataSvc.whereClauseCompanyPortion uses this exact native convention.
            if !tenant.is_empty() {
                scopes.push(format!("T_{tenant}"));
            }
            scopes.dedup();
            request.state = with_settings(json!({"phase":"nextPage","scopes":scopes}), &request.state);
            read_page(&request)
        }
        "nextPage" => read_page(&request),
        "rows" => {
            let rows = request
                .response
                .as_ref()
                .and_then(|v| v["rows"].as_array())
                .ok_or_else(|| invalid("The Classic page omitted its rows."))?;
            let size = request.page_size.unwrap_or(24) as usize;
            if rows.len() > size {
                return Err(invalid("The Classic page exceeded the requested size."));
            }
            // Progress saved by the former scope-by-type branches has no lists.
            if !request.state["companies"].is_array() || !request.state["codes"].is_array() {
                return Err(invalid("Restart Classic discovery with the current saved position format."));
            }
            // Discovery validates only rows its literal prefixes keep. A row is skipped
            // only when a string Key1/Key2 provably misses its prefix; anything unreadable
            // may be selected and is validated. Exact acquisition validates every row.
            let skip = |row: &Value| request.operation != "acquire"
                && (row["Key1"].as_str().is_some_and(|k| !k.starts_with(&first_prefix))
                    || row["Key2"].as_str().is_some_and(|k| !k.starts_with(&form_prefix)));
            let mut seen = BTreeSet::new();
            let mut kept = Vec::new();
            let mut identities = Vec::new();
            for row in rows {
                if skip(row) { continue; }
                // Scope comes from the row's own Company, exactly as before;
                // the merged query only widens which listed values may appear.
                let id = identity(
                    row,
                    &request.state["companies"],
                    &request.state["codes"],
                    &request.category,
                )?;
                if !seen.insert(serde_json::to_string(&id).unwrap()) {
                    return Err(invalid("The Classic page repeats an identity."));
                }
                kept.push(row); identities.push(id);
            }
            // One SysRowID keyset spans every scope and type: a short page ends discovery.
            let next = if request.operation == "acquire" {
                None
            } else if rows.len() == size {
                let after = rows
                    .last()
                    .and_then(|r| r["SysRowID"].as_str())
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("The full Classic page has no saved position."))?;
                if request.state["after"] == after {
                    return Err(invalid("The Classic page saved position did not advance."));
                }
                Some(json!({"version":CURSOR_VERSION,"after":after}).to_string())
            } else {
                None
            };
            if request.operation == "acquire"
                && (identities.len() != 1 || identities.first() != request.identity.as_ref())
            {
                return Err(invalid("The exact Classic identity was not returned."));
            }
            // Every kept row has string keys matching both prefixes, so it is selected;
            // process() validates its Content. Unselected rows leave no diagnostics.
            let mut state = request.state.clone();
            state["rows"] = json!(kept);
            state["identities"] = json!(identities);
            state["_selectionDiagnostics"] = json!([]);
            state["phase"] = json!("processing");
            state["targeted"] = json!(request.operation == "acquire");
            state["nextCursor"] = json!(next);
            if request.state["_hostPreparePage"] == true && request.operation != "acquire" {
                return Ok(GuestReply::PreparedPage {
                    next_state: with_settings(json!({"phase":"nextPage","scopes":state["scopes"]}), &state),
                    state,
                    next_cursor: next,
                });
            }
            request.state = state;
            process(request)
        }
        _ => Err(invalid("The Classic page saved progress is invalid.")),
    }
}
// Version 2: one SysRowID keyset over every scope and type. Version 1 named one
// of the former scope-by-type branches and cannot resume the merged stream.
const CURSOR_VERSION: u64 = 2;
fn read_page(request: &GuestRequest) -> Result<GuestReply, Diagnostic> {
    let cursor: Value = request
        .cursor
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| invalid("The Classic page saved position is invalid."))?
        .unwrap_or(json!({"version":CURSOR_VERSION}));
    if cursor["version"] != CURSOR_VERSION {
        return Err(invalid(
            "Restart Classic discovery with the current saved position format.",
        ));
    }
    if !cursor["after"].is_null() && !cursor["after"].as_str().is_some_and(|s| !s.is_empty()) {
        return Err(invalid("The Classic page saved position is invalid."));
    }
    let scopes = request.state["scopes"]
        .as_array()
        .ok_or_else(|| invalid("The Classic scope saved progress is missing."))?;
    let all: Vec<&str> = scopes.iter().map(|v| v.as_str().ok_or_else(|| invalid("The Classic page scope is invalid.")))
        .collect::<Result<_, _>>()?;
    let mut companies = all.clone();
    let mut codes = types(&request.category).to_vec();
    if companies.is_empty() {
        return Err(invalid("The Classic page scope is invalid."));
    }
    if let Some(id) = &request.identity {
        if id.category != request.category || id.kind != "PersistedXXXDef" || id.keys.len() != 7 {
            return Err(invalid("Select an exact Classic identity."));
        }
        let company = id
            .keys
            .get("Company")
            .ok_or_else(|| invalid("The Classic identity has no company."))?;
        let code = id
            .keys
            .get("TypeCode")
            .ok_or_else(|| invalid("The Classic identity has no type."))?;
        if !all.contains(&company.as_str()) || !types(&request.category).contains(&code.as_str()) {
            return Err(invalid(
                "The Classic identity is outside the selected scope.",
            ));
        }
        for key in crate::classic_archive::KEYS {
            if !id.keys.contains_key(key) {
                return Err(invalid("The Classic identity is incomplete."));
            }
        }
        (companies, codes) = (vec![company.as_str()], vec![code.as_str()]);
    }
    if request.page_size == Some(0) {
        return Err(invalid("The Classic page size must be positive."));
    }
    let original = request;
    let mut request = request.clone();
    request.page_size = Some(request.page_size.unwrap_or(24));
    let label: Vec<&str> = companies.iter().map(|c| if c.is_empty() {"global"} else {*c}).collect();
    let mut reply = crate::tables::classic_page(
        &request,
        companies[0],
        codes[0],
        cursor["after"].clone(),
        with_settings(json!({"phase":"rows","scopes":scopes,"companies":companies,"codes":codes,"after":cursor["after"],
        "_progress":{"request":format!("Downloading {} definitions ({})",codes.join(", "),label.join(", ")),"response":"Classic batch received; building XML and readable code"}}), &request.state),
    );
    if original.identity.is_none() {
        // Bulk-first: one bound Company list and one TypeCode list replace the former
        // scope-by-type branches. The host expands each list into a parenthesized
        // equality group of bound parameters, so values are never spliced into SQL.
        // Layer/owner and form prefixes add a broad bound `LIKE 'stem%'` superset;
        // the literal case-sensitive check stays local.
        if !matches!(reply, GuestReply::Action { .. }) {
            return Ok(reply);
        }
        let GuestReply::Action { action: HostAction::Query { query } | HostAction::SharedQuery { query }, .. } = &mut reply else {
            return Err(invalid("The Classic page did not produce a query."));
        };
        for (field, values) in [("Company", &companies), ("TypeCode", &codes)] {
            let mut found = query.criteria.iter()
                .filter(|c| c.table == "DefinitionSource" && c.field == field && c.operator == "=");
            let (Some(criterion), None) = (found.next(), found.next()) else {
                return Err(invalid("The Classic page query lacks its company or type condition."));
            };
            let parameter = criterion.parameter.clone();
            query.parameters.insert(parameter, if values.len() == 1 { json!(values[0]) } else { json!(values) });
        }
        // Key1 is the layer name or personalization owner; Key2 is the form.
        let (first, form) = selection(original)?;
        for (field, prefix) in [("Key1", &first), ("Key2", &form)] {
            // The stem is a bound parameter value, so its quotes are not doubled.
            let Some(stem) = like_stem(prefix) else { continue };
            let parameter = format!("{field}Prefix");
            query.criteria.push(QueryCriterion { table: "DefinitionSource".into(), field: field.into(), operator: "LIKE".into(), parameter: parameter.clone() });
            query.parameters.insert(parameter, json!(format!("{stem}%")));
        }
    }
    Ok(reply)
}
fn identity(
    row: &Value,
    companies: &Value,
    codes: &Value,
    category: &str,
) -> Result<Identity, Diagnostic> {
    let within = |list: &Value, value: &Value| value.is_string() && list.as_array().is_some_and(|l| l.contains(value));
    if !within(companies, &row["Company"]) || !within(codes, &row["TypeCode"]) || row["SystemFlag"] != false {
        return Err(invalid(
            "The Classic page returned a row outside its requested scope.",
        ));
    }
    let mut keys = BTreeMap::new();
    for key in crate::classic_archive::KEYS {
        keys.insert(
            key.into(),
            row[key]
                .as_str()
                .ok_or_else(|| invalid("The Classic row has an incomplete identity."))?
                .to_owned(),
        );
    }
    Ok(Identity {
        category: category.into(),
        kind: "PersistedXXXDef".into(),
        scope: if category == "classic-personalizations" {
            if keys["Key1"].is_empty() {
                return Err(invalid("The personalization owner is missing."));
            }
            Scope::Personal {
                company: keys["Company"].clone(),
                owner: keys["Key1"].clone(),
            }
        } else if keys["Company"].is_empty() {
            Scope::Global
        } else {
            Scope::Company {
                company: keys["Company"].clone(),
            }
        },
        keys,
    })
}
fn export_filename(identity: &Identity) -> String {
    if identity.category == "classic-personalizations" {
        // CustomPersonalizationPropertyPage uses FormNameKey and UserID.
        return crate::bpm_code::label(&format!(
            "{}_{}.xml",
            identity.keys["Key2"], identity.keys["Key1"]
        ));
    }
    // CustomToolsDialog.mnuFileExportCustomization_Click uses FormNameKey,
    // layer type, and layer name. CustomLayeringManager maps them to these keys.
    crate::bpm_code::label(&format!(
        "{}_{}_{}_CustomExport.xml",
        identity.keys["Key2"], identity.keys["TypeCode"], identity.keys["Key1"]
    ))
}
fn process(request: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let rows = request.state["rows"]
        .as_array()
        .ok_or_else(|| invalid("The downloaded Classic rows are missing."))?;
    let identities: Vec<Identity> = serde_json::from_value(request.state["identities"].clone())
        .map_err(|_| invalid("The downloaded Classic identities are invalid."))?;
    if rows.len() != identities.len() {
        return Err(invalid(
            "The downloaded Classic rows and identities do not match.",
        ));
    }
    let mut records = Vec::new();
    let mut diagnostics: Vec<Diagnostic> = match request.state.get("_selectionDiagnostics") {
        None => Vec::new(),
        Some(value) => serde_json::from_value(value.clone()).map_err(|_| invalid("The Classic selection diagnostics are invalid."))?,
    };
    for (row, id) in rows.iter().zip(&identities) {
        let mut definition =
            json!({"XXXDef":[row],"herbertDirectOutput":true,"herbertSources":{"files":[]}});
        let mut errors = crate::classic_sources::append_persisted_scripts(&mut definition);
        let xml = (|| {
            validate_content(row)?;
            if request.category == "classic-personalizations" {
                crate::personalization_archive::build(row)
            } else {
                crate::classic_archive::build(row)
            }
        })();
        match xml {
            Ok(xml) => definition["herbertSources"]["files"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":export_filename(id),"text":xml})),
            Err(error) => errors.push(error),
        }
        for mut error in errors {
            error.identity = Some(id.clone());
            diagnostics.push(error);
        }
        records.push((id.clone(), definition));
    }
    if request.state["targeted"] == true {
        let (identity, definition) = records
            .pop()
            .ok_or_else(|| invalid("The selected Classic definition is missing."))?;
        return Ok(GuestReply::Definition {
            identity,
            definition,
            format: if request.category == "classic-personalizations" {
                "native-classic-personalization"
            } else {
                "native-classic-gen3"
            }
            .into(),
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
            next_cursor: request.state["nextCursor"].as_str().map(str::to_owned),
            authoritative: diagnostics.is_empty(),
            diagnostics,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "classic-customizations".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: json!({}),
            response: None,
            error: None,
        }
    }
    fn row(company: &str, country: &str) -> Value {
        json!({"Company":company,"ProductID":"EP","TypeCode":"Customization","CGCCode":country,"Key1":"SameName","Key2":"Form","Key3":"","SysRowID":format!("row-{country}"),"SystemFlag":false,"Content":"<CustomizationDS/>","HerbertContentBytes":36})
    }
    fn page() -> GuestRequest {
        let mut r = request();
        r.state = json!({"phase":"rows","companies":["C","","T_67964"],"codes":TYPES,"scopes":["C","","T_67964"],"_hostPreparePage":true});
        r.response = Some(json!({"rows":[row("C","US"),row("C","ZZ")]}));
        r
    }
    fn prefix_page(category: &str) -> GuestRequest {
        let mut r = page(); r.category = category.into(); r.page_size = Some(4);
        let code = if category == "classic-personalizations" {"Personalization"} else {"Customization"};
        r.state["codes"] = json!(types(category));
        let rows: Vec<_> = [("Alpha","FormA","US"),("Alpha","FormB","ZZ"),("Beta","FormA","CA"),("Beta","FormB","UK")]
            .into_iter().enumerate().map(|(index,(first,form,country))| {
                let mut value = row("C",country); value["TypeCode"] = json!(code); value["Key1"] = json!(first); value["Key2"] = json!(form);
                value["Key3"] = json!(format!("Customization^Parent{index}"));
                value["SysRevID"] = json!(9007199254740993u64); value["SysNumber01"] = json!("1234567890.123456789");
                let property = |name: &str,text: &str,id: usize| format!("<Properties><FormsID>F</FormsID><ControlName>Customization</ControlName><Key>K</Key><PropertyName>{name}</PropertyName><PropertyValue>{text}</PropertyValue><Company>C</Company><Plant>P</Plant><DcdUserID>{first}</DcdUserID><GroupID/><PropertiesID>{id}</PropertiesID></Properties>");
                let language = if index % 2 == 0 {"CSharp"} else {"VisualBasic"};
                let content = format!("<CustomizationDS xmlns=\"http://tempuri.org/XMLSchema.xsd\">{}{}</CustomizationDS>",
                    property("Script"," a &lt; b &amp; Ω 😀&#13;\n",1),property("Language",language,2));
                value["HerbertContentBytes"] = json!(content.encode_utf16().count()*2); value["Content"] = json!(content); value
            }).collect();
        r.response = Some(json!({"rows":rows})); r
    }
    fn prefix_settings(category: &str,first: Value,form: Value) -> Value {
        let mut settings = json!({"formNamePrefix":form});
        settings[if category == "classic-personalizations" {"userIdPrefix"} else {"layerNamePrefix"}] = first; settings
    }
    fn records(mut r: GuestRequest, prepared: bool) -> (Vec<(Identity,Value)>,bool,Vec<Diagnostic>,Option<String>) {
        r.state["_hostPreparePage"] = json!(prepared);
        let reply = handle(r.clone());
        let reply = if let GuestReply::PreparedPage {state,..} = reply {
            r.state = state; r.operation = "materialize".into(); r.response = None; handle(r)
        } else {reply};
        let GuestReply::InventoryData {records,inventory} = reply else {panic!("{reply:?}")};
        let GuestReply::Inventory {authoritative,diagnostics,next_cursor,..} = *inventory else {panic!()};
        (records,authoritative,diagnostics,next_cursor)
    }
    #[test]
    fn prefixes_select_complete_classic_xml_scripts_and_composite_owners_with_and_semantics() {
        for category in ["classic-customizations","classic-personalizations"] {
            let r = prefix_page(category); let (baseline,complete,_,_) = records(r.clone(),false); assert!(complete);
            for (first,form,count) in [("","",4),("Alpha","",2),("","FormA",2),("Alpha","FormA",1),("alpha","FormA",0),(" Alpha","",0),("%","",0)] {
                let mut selected = r.clone(); selected.state["_pluginSettings"] = prefix_settings(category,json!(first),json!(form));
                for prepared in [false,true] {
                    let (actual,complete,diagnostics,_) = records(selected.clone(),prepared);
                    assert_eq!(actual.len(),count);assert!(complete);assert!(diagnostics.is_empty());
                    for record in actual {assert_eq!(&record,baseline.iter().find(|(id,_)| id == &record.0).unwrap());}
                }
            }
            assert!(baseline[0].1["herbertSources"]["files"].as_array().unwrap().iter().any(|f|f["name"].as_str().unwrap().ends_with(".cs")));
            assert!(baseline[1].1["herbertSources"]["files"].as_array().unwrap().iter().any(|f|f["name"].as_str().unwrap().ends_with(".vb")));
            let mut literal = r.clone();literal.response.as_mut().unwrap()["rows"][0]["Key1"] = json!(" Ω%_ '");
            literal.state["_pluginSettings"] = prefix_settings(category,json!(" Ω%_ '"),json!("FormA"));
            assert_eq!(records(literal,false).0.len(),1);
            let mut saved = r;saved.state["_pluginState"] = json!({"config":prefix_settings(category,json!("missing"),json!("missing"))});
            assert_eq!(records(saved,false).0,baseline);
        }
    }
    #[test]
    fn prefixes_skip_unselected_content_without_diagnostics_and_keep_selected_diagnostics() {
        for category in ["classic-customizations","classic-personalizations"] {
            for problem in ["missing","type","length-missing","length-type","length","utf16"] {
                let mut r = prefix_page(category); r.state["_pluginSettings"] = prefix_settings(category,json!("Alpha"),json!("FormA"));
                let bad = &mut r.response.as_mut().unwrap()["rows"][3];
                bad["Content"] = json!("PRIVATE-MALFORMED-PAYLOAD 😀");
                match problem {
                    "missing" => {bad.as_object_mut().unwrap().remove("Content");},
                    "type" => bad["Content"] = json!(true),
                    "length-missing" => {bad.as_object_mut().unwrap().remove("HerbertContentBytes");},
                    "length-type" => bad["HerbertContentBytes"] = json!(1.5),
                    "utf16" => bad["HerbertContentBytes"] = json!(bad["Content"].as_str().unwrap().chars().count()*2),
                    _ => bad["HerbertContentBytes"] = json!(0),
                }
                for prepared in [false,true] {
                    let mut input = r.clone();input.state["_hostPreparePage"] = json!(prepared);
                    let reply = handle(input.clone());
                    assert!(!serde_json::to_string(&reply).unwrap().contains("PRIVATE-MALFORMED-PAYLOAD"));
                    let (records,complete,diagnostics,_) = records(r.clone(),prepared);
                    // Unselected malformed Content is neither validated nor reported.
                    assert_eq!(records.len(),1);assert!(complete);assert!(diagnostics.is_empty());
                    assert!(records[0].1["herbertSources"]["files"].as_array().unwrap().iter().any(|f|f["name"].as_str().unwrap().ends_with(".xml")));
                }
            }
            {
                let mut r = prefix_page(category);r.state["_pluginSettings"] = prefix_settings(category,json!("Missing"),json!(""));
                r.response.as_mut().unwrap()["rows"][3]["HerbertContentBytes"] = json!(0);
                let (records,complete,diagnostics,_) = records(r,false);assert!(records.is_empty());assert!(complete);assert!(diagnostics.is_empty());
            }
            // A selected invalid row still reports its diagnostic.
            let mut r = prefix_page(category);r.state["_pluginSettings"] = prefix_settings(category,json!("Beta"),json!("FormB"));
            r.response.as_mut().unwrap()["rows"][3]["HerbertContentBytes"] = json!(0);
            let (records,complete,diagnostics,_) = records(r,false);assert_eq!(records.len(),1);assert!(!complete);assert_eq!(diagnostics.len(),1);
            assert_eq!(diagnostics[0].identity.as_ref().unwrap().keys["Key1"],"Beta");
        }
    }
    #[test]
    fn prefixes_validate_selected_identities_skip_unselected_rows_and_keep_partial_results() {
        for category in ["classic-customizations","classic-personalizations"] {
            for field in ["Company","TypeCode","SystemFlag","Key3"] {
                for (index,skipped) in [(3,true),(0,false)] {
                    let mut r = prefix_page(category);r.state["_pluginSettings"] = prefix_settings(category,json!("Alpha"),json!("FormA"));
                    r.response.as_mut().unwrap()["rows"][index][field] = if field == "SystemFlag" {json!(true)} else {Value::Null};
                    if skipped {
                        let (records,complete,diagnostics,_) = records(r,false);assert_eq!(records.len(),1);assert!(complete);assert!(diagnostics.is_empty());
                    } else {
                        assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "CLASSIC_PAGE_INVALID"));
                    }
                }
            }
            // An unreadable Key1 with no form prefix cannot be proven unselected, so it is validated.
            let mut r = prefix_page(category);r.state["_pluginSettings"] = prefix_settings(category,json!("Alpha"),json!(""));
            r.response.as_mut().unwrap()["rows"][3]["Key1"] = Value::Null;
            assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "CLASSIC_PAGE_INVALID"));
            let mut r = prefix_page(category);r.state["_pluginSettings"] = prefix_settings(category,json!("Alpha"),json!(""));
            r.response.as_mut().unwrap()["rows"][1]["HerbertContentBytes"] = json!(0);
            let (records,complete,diagnostics,_) = records(r,false);assert_eq!(records.len(),2);assert!(!complete);assert_eq!(diagnostics.len(),1);
            assert_eq!(diagnostics[0].identity.as_ref().unwrap().keys["Key2"],"FormB");
            for invalid in [Value::Null,json!(false),json!(4),json!([])] {
                let mut r = prefix_page(category);r.state["_pluginSettings"] = prefix_settings(category,invalid,json!(""));
                assert!(matches!(handle(r),GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));
            }
        }
    }
    #[test]
    fn prefixes_continue_from_raw_cursor_through_empty_selection_and_frozen_scope_states() {
        for category in ["classic-customizations","classic-personalizations"] {
            let mut r = prefix_page(category);let settings = prefix_settings(category,json!("Later"),json!("FormA"));r.state["_pluginSettings"] = settings.clone();
            let GuestReply::PreparedPage {state,next_state,next_cursor} = handle(r.clone()) else {panic!()};
            assert_eq!(state["rows"],json!([]));assert_eq!(next_state["_pluginSettings"],settings);
            let cursor: Value = serde_json::from_str(next_cursor.as_ref().unwrap()).unwrap();assert_eq!(cursor["after"],"row-UK");
            r.state = next_state;r.cursor = next_cursor;r.response = None;
            let GuestReply::Action {state,..} = handle(r.clone()) else {panic!()};assert_eq!(state["_pluginSettings"],settings);
            r.state = state;r.state["_hostPreparePage"] = json!(true);
            let mut later = prefix_page(category).response.unwrap()["rows"][0].clone();later["Key1"] = json!("Later");later["SysRowID"] = json!("later");
            r.response = Some(json!({"rows":[later]}));
            let (records,complete,_,next) = records(r,false);assert_eq!(records.len(),1);assert!(complete);
            // One merged keyset: a short page ends discovery instead of opening the next branch.
            assert_eq!(next,None);
            let mut initial = request();initial.category = category.into();initial.state = json!({"_pluginSettings":settings});
            let GuestReply::Action {state,..} = handle(initial.clone()) else {panic!()};assert_eq!(state["_pluginSettings"],settings);
            initial.state = state;initial.response = Some(json!({"parameters":{"companyID":"C","tenantID":"native"}}));
            let GuestReply::Action {state,..} = handle(initial) else {panic!()};
            assert_eq!(state["scopes"],json!(["C","","T_native"]));assert_eq!(state["_pluginSettings"],settings);
        }
    }
    #[test]
    fn prefixes_do_not_narrow_exact_classic_composite_acquisition() {
        for category in ["classic-customizations","classic-personalizations"] {
            let mut r = prefix_page(category);let row = r.response.as_ref().unwrap()["rows"][0].clone();
            r.response = Some(json!({"rows":[row.clone()]}));
            r.identity = Some(identity(&row,&json!(["C"]),&json!([row["TypeCode"]]),category).unwrap());r.operation = "acquire".into();
            let baseline = serde_json::to_value(handle(r.clone())).unwrap();assert_eq!(baseline["type"],"definition");
            let action = serde_json::to_value(read_page(&r).unwrap()).unwrap()["action"].clone();
            r.state["_pluginSettings"] = prefix_settings(category,json!("Missing"),json!("Missing"));
            assert_eq!(serde_json::to_value(handle(r.clone())).unwrap(),baseline);
            assert_eq!(serde_json::to_value(read_page(&r).unwrap()).unwrap()["action"],action);
            let GuestReply::Action {action:HostAction::Query {query}|HostAction::SharedQuery {query},..} = read_page(&r).unwrap() else {panic!()};
            assert!(!query.criteria.iter().any(|c| c.operator == "LIKE"),"Exact acquisition ignores prefixes");
        }
    }
    #[test]
    fn personalizations_use_the_owner_key_and_preserve_parent_variants() {
        let mut r = page();
        r.category = "classic-personalizations".into();
        r.state["codes"] = json!(["Personalization"]);
        for (index, row) in r.response.as_mut().unwrap()["rows"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            row["TypeCode"] = json!("Personalization");
            row["Key1"] = json!("alice");
            row["Key3"] = json!(format!("Customization^parent{index}"));
        }
        let GuestReply::PreparedPage { state, .. } = handle(r.clone()) else {
            panic!()
        };
        r.operation = "materialize".into();
        r.state = state;
        let GuestReply::InventoryData { records, .. } = handle(r) else {
            panic!()
        };
        for (id, definition) in &records {
            assert_eq!(id.category, "classic-personalizations");
            assert_eq!(
                id.scope,
                Scope::Personal {
                    company: "C".into(),
                    owner: "alice".into()
                }
            );
            assert_eq!(
                definition["herbertSources"]["files"][0]["name"],
                "Form_alice.xml"
            );
        }
        assert_ne!(records[0].0, records[1].0);
    }
    #[test]
    fn session_scope_uses_the_native_tenant_and_never_the_company_suffix() {
        let mut r = request();
        r.state = json!({"phase":"scope"});
        r.response = Some(json!({"parameters":{"companyID":"C","tenantID":"67964"}}));
        let GuestReply::Action { state, .. } = handle(r.clone()) else {
            panic!()
        };
        assert_eq!(state["scopes"], json!(["C", "", "T_67964"]));
        r.response.as_mut().unwrap()["parameters"]["companyID"] = json!("OTHER");
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
    #[test]
    fn a_prepared_page_keeps_variants_and_materializes_without_server_calls() {
        let GuestReply::PreparedPage {
            state,
            next_state,
            next_cursor,
        } = handle(page())
        else {
            panic!()
        };
        assert_eq!(next_state["phase"], "nextPage");
        assert_eq!(
            serde_json::from_str::<Value>(&next_cursor.unwrap()).unwrap()["after"],
            "row-ZZ"
        );
        let mut r = request();
        r.operation = "materialize".into();
        r.state = state;
        let GuestReply::InventoryData { records, inventory } = handle(r) else {
            panic!()
        };
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].0, records[1].0);
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: true,
                ..
            }
        ));
        assert!(
            records
                .iter()
                .all(|(_, d)| d["herbertSources"]["files"][0]["name"]
                    == "Form_Customization_SameName_CustomExport.xml")
        );
    }
    #[test]
    fn foreign_system_duplicate_and_stalled_rows_are_not_successful_pages() {
        for field in ["Company", "SystemFlag", "SysRowID"] {
            let mut r = page();
            if field == "Company" {
                r.response.as_mut().unwrap()["rows"][0][field] = json!("OTHER");
            }
            if field == "SystemFlag" {
                r.response.as_mut().unwrap()["rows"][0][field] = json!(true);
            }
            if field == "SysRowID" {
                r.state["after"] = json!("row-ZZ");
            }
            assert!(matches!(handle(r), GuestReply::Failure { .. }));
        }
        let mut r = page();
        r.response.as_mut().unwrap()["rows"][1] = row("C", "US");
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
    #[test]
    fn truncated_content_is_retained_but_never_claimed_as_a_native_export() {
        let mut r = page();
        r.state["_hostPreparePage"] = json!(false);
        r.response.as_mut().unwrap()["rows"][0]["HerbertContentBytes"] = json!(2000);
        let GuestReply::InventoryData { records, inventory } = handle(r) else {
            panic!()
        };
        assert!(
            records[0].1["herbertSources"]["files"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: false,
                ..
            }
        ));
    }
    #[test]
    fn query_binds_tenant_and_full_identity_without_country_view_filtering() {
        let mut r = request();
        r.state = json!({"phase":"nextPage","scopes":["C","","T_67964"]});
        let GuestReply::Action {
            action: HostAction::Query { query },
            ..
        } = handle(r)
        else {
            panic!()
        };
        assert!(
            query
                .criteria
                .iter()
                .any(|c| c.field == "Company" && query.parameters[&c.parameter] == json!(["C","","T_67964"]))
        );
        assert!(!query.criteria.iter().any(|c| c.field == "CGCCode"));
        assert!(query.fields.iter().any(|f| f.field == "*"));
    }
    fn discovery_query(category: &str, cursor: Option<Value>, settings: Value) -> Result<(QuerySpec, Value), Diagnostic> {
        let mut r = request();r.category = category.into();r.cursor = cursor.map(|c| c.to_string());
        r.state = json!({"phase":"nextPage","scopes":["C","","T_67964"],"_pluginSettings":settings});
        match handle(r) {
            GuestReply::Action {action:HostAction::Query {query},state} => Ok((query,state)),
            GuestReply::Failure {diagnostic} => Err(diagnostic),
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn discovery_issues_one_query_over_every_scope_and_type_with_prefix_pushdown() {
        for category in ["classic-customizations","classic-personalizations"] {
            let (query,state) = discovery_query(category,None,prefix_settings(category,json!("Alpha"),json!("FormA"))).unwrap();
            let only = |field: &str| {let found: Vec<_> = query.criteria.iter().filter(|c| c.field == field).collect(); assert_eq!(found.len(),1,"{field}"); assert_eq!(found[0].operator,"="); query.parameters[&found[0].parameter].clone()};
            assert_eq!(only("Company"),json!(["C","","T_67964"]));
            assert_eq!(only("TypeCode"),if category == "classic-personalizations" {json!("Personalization")} else {json!(TYPES)});
            assert_eq!(only("SystemFlag"),json!(false));
            // Layer/owner and form prefixes push a LIKE superset; no SysRowID keyset on the first page.
            let like: Vec<_> = query.criteria.iter().filter(|c| matches!(c.field.as_str(),"Key1"|"Key2"))
                .map(|c| (c.field.as_str(),c.operator.as_str(),query.parameters[&c.parameter].clone())).collect();
            assert_eq!(like,vec![("Key1","LIKE",json!("Alpha%")),("Key2","LIKE",json!("FormA%"))]);
            assert!(!query.criteria.iter().any(|c| matches!(c.field.as_str(),"SysRowID"|"CGCCode")));
            assert_eq!(query.order.len(),1);assert_eq!(query.order[0].field,"SysRowID");assert!(!query.order[0].descending);
            assert_eq!(query.page_size,Some(2));
            assert_eq!(state["companies"],json!(["C","","T_67964"]));assert_eq!(state["codes"],json!(types(category)));
            assert!(state.get("branch").is_none());
        }
    }
    #[test]
    fn prefixes_push_a_bound_like_stem_superset_through_continuation() {
        for category in ["classic-customizations","classic-personalizations"] {
            let like = |cursor: Option<Value>,first: &str,form: &str| {
                let (query,_) = discovery_query(category,cursor,prefix_settings(category,json!(first),json!(form))).unwrap();
                query.criteria.iter().filter(|c| c.operator == "LIKE").map(|c| format!("{} {}",c.field,query.parameters[&c.parameter].as_str().unwrap())).collect::<Vec<_>>()
            };
            // Bound parameters carry the quote verbatim; wildcard/bracket characters end the stem.
            assert_eq!(like(None,"Ab'c","O'Form"),vec!["Key1 Ab'c%","Key2 O'Form%"]);
            assert_eq!(like(None,"Q_1","F[x]"),vec!["Key1 Q%","Key2 F%"]);
            // Non-ASCII ends the stem; a stem of only a space still narrows.
            assert_eq!(like(None," Ω ",""),vec!["Key1  %"]);
            assert!(like(None,"Ω","é").is_empty());
            for (first,form) in [("",""),("%x","_y"),("[a","]b")] {assert!(like(None,first,form).is_empty(),"{first:?} {form:?}");}
            assert_eq!(like(Some(json!({"version":2,"after":"row-ZZ"})),"Alpha","FormA"),vec!["Key1 Alpha%","Key2 FormA%"]);
        }
        // Personalizations read the owner from userIdPrefix, never layerNamePrefix.
        let (query,_) = discovery_query("classic-personalizations",None,json!({"layerNamePrefix":"Layer","userIdPrefix":"alice"})).unwrap();
        assert_eq!(query.criteria.iter().filter(|c| c.operator == "LIKE").map(|c| query.parameters[&c.parameter].clone()).collect::<Vec<_>>(),vec![json!("alice%")]);
    }
    #[test]
    fn continuation_uses_the_sysrowid_keyset_and_old_branch_positions_restart() {
        let (query,state) = discovery_query("classic-customizations",Some(json!({"version":2,"after":"row-ZZ"})),json!({})).unwrap();
        let keyset: Vec<_> = query.criteria.iter().filter(|c| c.field == "SysRowID").collect();
        assert_eq!(keyset.len(),1);assert_eq!(keyset[0].operator,">");assert_eq!(query.parameters[&keyset[0].parameter],"row-ZZ");
        assert_eq!(state["after"],"row-ZZ");
        assert!(query.criteria.iter().any(|c| c.field == "Company" && query.parameters[&c.parameter] == json!(["C","","T_67964"])));
        for old in [json!({"version":1,"branch":0}),json!({"version":1,"branch":7,"after":"row-ZZ"}),json!({"branch":3})] {
            let error = discovery_query("classic-customizations",Some(old),json!({})).unwrap_err();
            assert!(error.message.starts_with("Restart Classic discovery"),"{}",error.message);
        }
        for bad in [json!({"version":2,"after":""}),json!({"version":2,"after":7})] {
            assert_eq!(discovery_query("classic-customizations",Some(bad),json!({})).unwrap_err().code,"CLASSIC_PAGE_INVALID");
        }
        // Rows saved under the former single-branch state cannot be validated against the merged lists.
        let mut old = page();old.state = json!({"phase":"rows","branch":0,"company":"C","code":"Customization","scopes":["C","","T_67964"]});
        assert!(matches!(handle(old),GuestReply::Failure {diagnostic} if diagnostic.message.starts_with("Restart Classic discovery")));
        // A full page continues from its last SysRowID in the version 2 format, with no branch.
        let GuestReply::PreparedPage {next_cursor,..} = handle(page()) else {panic!()};
        assert_eq!(serde_json::from_str::<Value>(&next_cursor.unwrap()).unwrap(),json!({"version":2,"after":"row-ZZ"}));
    }
    #[test]
    fn merged_page_keeps_each_row_scope_and_identity_from_its_own_company_and_type() {
        let mut r = page();r.page_size = Some(6);r.state["_hostPreparePage"] = json!(false);
        let rows: Vec<Value> = [("C","Customization","A"),("","BaseExtension","B"),("T_67964","Localization","C"),("C","Productization","D"),("","Verticalization","E")]
            .into_iter().map(|(company,code,country)| {let mut v = row(company,country);v["TypeCode"] = json!(code);v}).collect();
        r.response = Some(json!({"rows":rows}));
        let (records,complete,diagnostics,next) = records(r.clone(),false);
        assert!(complete);assert!(diagnostics.is_empty());assert_eq!(next,None);assert_eq!(records.len(),5);
        for ((id,_),row) in records.iter().zip(&rows) {
            // Identical to the identity a single former scope-by-type branch produced for this row.
            assert_eq!(id,&identity(row,&json!([row["Company"]]),&json!([row["TypeCode"]]),"classic-customizations").unwrap());
            assert_eq!(id.keys["Company"],row["Company"].as_str().unwrap());assert_eq!(id.keys["TypeCode"],row["TypeCode"].as_str().unwrap());
        }
        assert_eq!(records.iter().map(|(id,_)| id.scope.clone()).collect::<Vec<_>>(),vec![
            Scope::Company {company:"C".into()},Scope::Global,Scope::Company {company:"T_67964".into()},Scope::Company {company:"C".into()},Scope::Global]);
        for (field,value) in [("Company",json!("OTHER")),("Company",json!("T_OTHER")),("TypeCode",json!("Personalization")),("TypeCode",json!("customization"))] {
            let mut bad = r.clone();bad.response.as_mut().unwrap()["rows"][2][field] = value;
            assert!(matches!(handle(bad),GuestReply::Failure {diagnostic} if diagnostic.message.contains("outside its requested scope")));
        }
        let mut personal = r;personal.category = "classic-personalizations".into();personal.state["codes"] = json!(["Personalization"]);
        assert!(matches!(handle(personal),GuestReply::Failure {diagnostic} if diagnostic.message.contains("outside its requested scope")));
    }
}
