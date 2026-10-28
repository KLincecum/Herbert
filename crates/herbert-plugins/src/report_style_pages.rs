//! Establish native visibility, then read the stored report graph in bulk.
//! Report.GetRows clears images and changes style fields for the UI. Its rows
//! select the graph; the physical rows supply the backup data.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const TABLES: &[&str] = &[
    "Report",
    "ReportStyle",
    "ReportStyleRule",
    "ReportStyleImage",
    "RptStylePrinters",
];
pub fn response_policies(methods: &[(String, String)]) -> Vec<NativeResponsePolicy> {
    let mut policies = crate::response_policy::for_methods(methods);
    for policy in &mut policies {
        if policy.service == "Ice.BO.DynamicQuerySvc" {
            // Native visibility selects the exact report/style keys. Physical
            // reads can include shared owners, so validate keys in this plugin.
            if let Some(tables) = &mut policy.tables {
                tables.unscoped_tables.push("Results".into());
            }
        }
    }
    policies
}
fn failure(message: &str) -> GuestReply {
    crate::failure("REPORT_STYLE_PAGE_INVALID", message)
}
pub fn methods() -> Vec<(String, String)> {
    let mut methods: Vec<(String, String)> = [
        ("Ice.BO.ReportSvc", "GetRows"),
        ("Ice.BO.BAQDesignerSvc", "GetFieldList"),
        ("Ice.BO.DynamicQuerySvc", "Execute"),
        ("Ice.BO.ReportSvc", "ExportSsrsRdl"),
        ("Ice.BO.ReportSvc", "ExportReportStyleImages"),
    ]
    .into_iter()
    .map(|(s, m)| (s.into(), m.into()))
    .collect();
    methods.extend(crate::report_package_pages::methods());
    methods
}
fn action(
    mut r: GuestRequest,
    phase: &str,
    service: &str,
    method: &str,
    args: Value,
    label: String,
) -> GuestReply {
    r.state["phase"] = json!(phase);
    r.state["_progress"] =
        json!({"request":label,"response":"Report data received; preparing native exports"});
    crate::native(service, method, args, false, r.state)
}
fn pair(row: &Value) -> Result<(String, String, i64), &'static str> {
    Ok((
        row["Company"]
            .as_str()
            .ok_or("Missing report company.")?
            .into(),
        row["ReportID"].as_str().ok_or("Missing report ID.")?.into(),
        row["StyleNum"]
            .as_i64()
            .ok_or("Missing report style number.")?,
    ))
}
fn child_matches(table: &str, row: &Value, key: &(String, String, i64), company: &str) -> bool {
    if table == "ReportStyleImage" {
        row["ImageCompany"] == company && row["ReportID"] == key.1 && row["StyleNum"] == key.2
    } else {
        pair(row).ok().as_ref() == Some(key)
    }
}
pub fn handle(mut r: GuestRequest) -> GuestReply {
    let selected_prefix=if r.operation=="acquire" {String::new()}else {match crate::report_archive::prefix(&r,"reportIdPrefix"){Ok(value)=>value,Err(diagnostic)=>return GuestReply::Failure {diagnostic}}};
    if r.category=="C17" && r.operation!="acquire" {if let Err(diagnostic)=crate::report_archive::prefix(&r,"packagePathPrefix"){return GuestReply::Failure {diagnostic};}}
    if r.category == "C17" {
        if r.identity
            .as_ref()
            .is_some_and(|id| id.kind == "PersistedReportHistory")
        {
            return crate::report_package_pages::handle(r);
        }
        if r.identity
            .as_ref()
            .is_some_and(|id| id.kind == "CurrentCustomReportPackage")
        {
            return crate::report_store::handle(r);
        }
        if r.state["packageRows"].is_array()
            || r.cursor
                .as_deref()
                .and_then(|c| serde_json::from_str::<Value>(c).ok())
                .is_some_and(|c| c["stream"].as_u64().is_some_and(|s| s > 0))
        {
            return crate::report_package_pages::handle(r);
        }
    }
    if r.operation == "materialize" || r.state["records"].is_array() {
        return materialize(r);
    }
    if r.state["phase"] == "pageLayouts" {
        return receive_layout_page(r);
    }
    if !matches!(r.operation.as_str(), "discover" | "acquire") {
        return failure("Reports support discovery and exact collection.");
    }
    if r.operation == "acquire" {
        let expected = if r.category == "C17" {
            "ReportStyle"
        } else {
            "ReportBehavior"
        };
        let Some(id) = r
            .identity
            .as_ref()
            .filter(|id| id.category == r.category && id.kind == expected && id.keys.len() == 3)
        else {
            return failure("Select one exact report style or behavior identity.");
        };
        if !id.keys.contains_key("Company")
            || id.keys.get("ReportID").is_none_or(String::is_empty)
            || id
                .keys
                .get("StyleNum")
                .and_then(|v| v.parse::<i32>().ok())
                .is_none()
        {
            return failure("The report identity has invalid keys.");
        }
        let owner = &id.keys["Company"];
        let scope = if owner.is_empty() {
            Scope::Global
        } else if *owner == r.company {
            Scope::Company {
                company: owner.clone(),
            }
        } else {
            Scope::SharedCompany {
                company: r.company.clone(),
                owner_company: owner.clone(),
            }
        };
        if id.scope != scope {
            return failure("The report identity has a different company scope.");
        }
    }
    if let Some(diagnostic) = r.error.take() {
        if r.category == "C17" && r.operation == "discover" {
            return GuestReply::Inventory {
                identities: vec![],
                next_cursor: Some(json!({"stream":1,"page":1}).to_string()),
                authoritative: false,
                diagnostics: vec![diagnostic],
            };
        }
        return GuestReply::Failure { diagnostic };
    }
    if !r.state.is_object() {
        r.state = json!({});
    }
    let cursor = match r
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => v.unwrap_or(json!({"page":1})),
        Err(_) => return failure("Invalid report continuation."),
    };
    let Some(page @ 1..=2147483647) = cursor["page"].as_u64() else {
        return failure("The native page must be a positive Int32.");
    };
    let Some(size @ 1..=2147483647) = r.page_size else {
        return failure("The native page size must be a positive Int32.");
    };
    match r.state["phase"].as_str() {
        Some("visible") => {
            let response = r.response.take().unwrap_or(Value::Null);
            let Some(more) = response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
            else {
                return failure("The visible report page omitted morePages.");
            };
            let body = crate::body(&response);
            let Some(reports) = body["Report"].as_array() else {
                return failure("The report page omitted its parents.");
            };
            if more && reports.is_empty() {
                return failure("The report page did not advance.");
            }
            let Some(styles) = body["ReportStyle"].as_array() else {
                return failure("The report page omitted its styles.");
            };
            let Some(rules) = body["ReportStyleRule"].as_array() else {
                return failure("The report page omitted its rules.");
            };
            let mut visible_keys=BTreeSet::new();let mut parent_ids=BTreeSet::new();
            for report in reports {
                let Some(id)=report["ReportID"].as_str().filter(|value|!value.is_empty())else{return failure("A visible report parent omitted its native ID.");};
                let Some(rowid)=report["SysRowID"].as_str()else{return failure("A visible report parent omitted its row identity.");};
                if !report["Company"].is_string() || !parent_ids.insert(rowid){return failure("The visible report parents have invalid scope or duplicate identities.");}
                let _=id;
            }
            for style in styles {
                let key=match pair(style){Ok(key)=>key,Err(message)=>return failure(message)};
                if key.1.is_empty() || !style["SystemFlag"].is_boolean() || !style["SysRowID"].is_string() || !visible_keys.insert(key.clone()) || !reports.iter().any(|report|report["ReportID"]==key.1){return failure("A visible report style has invalid native keys, parent or classification.");}
            }
            for rule in rules {
                let key=match pair(rule){Ok(key)=>key,Err(message)=>return failure(message)};
                if !visible_keys.contains(&key){return failure("An acquired visible rule has no native-visible style parent.");}
            }
            let mut selected = Vec::new();
            for style in styles {
                let key = match pair(style) {
                    Ok(k) => k,
                    Err(e) => return failure(e),
                };
                let wanted = if r.category == "C17" {
                    style["SystemFlag"] == false
                } else {
                    rules
                        .iter()
                        .any(|rule| pair(rule).ok() == Some(key.clone()))
                };
                let exact = r.identity.as_ref().is_none_or(|id| {
                    id.keys["Company"] == key.0
                        && id.keys["ReportID"] == key.1
                        && id.keys["StyleNum"] == key.2.to_string()
                });
                if wanted && exact && (r.operation=="acquire" || key.1.starts_with(&selected_prefix)) {
                    selected.push(style.clone());
                }
            }
            if r.operation == "acquire" && selected.len() != 1 {
                return failure("The exact visible report style was not returned.");
            }
            r.state["visible"] = json!(selected);
            r.state["parents"] = json!(reports);
            r.state["next"] = if more && r.operation == "discover" {
                json!(json!({"page":page+1}).to_string())
            } else if r.category == "C17" && r.operation == "discover" {
                json!(json!({"stream":1,"page":1}).to_string())
            } else {
                Value::Null
            };
            r.state["tableIndex"] = json!(0);
            r.state["tables"] = json!({});
            r.state["after"] = Value::Null;
        }
        Some("dictionary") => {
            let table = TABLES[r.state["tableIndex"].as_u64().unwrap() as usize];
            let response = r.response.take().unwrap_or(Value::Null);
            let Some(fields) = response
                .pointer("/returnObj/TableFieldList")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
            else {
                return failure("The report field dictionary is unavailable.");
            };
            if !r.state["fields"].is_object() {
                r.state["fields"] = json!({});
            }
            r.state["fields"][table] = json!(fields);
        }
        Some("physical") => {
            let table = TABLES[r.state["tableIndex"].as_u64().unwrap() as usize];
            let response = r.response.take().unwrap_or(Value::Null);
            let body = crate::body(&response);
            if body["Errors"].as_array().is_some_and(|a| !a.is_empty()) {
                return failure("The stored report read returned query errors.");
            }
            let Some(rows) = body["Results"].as_array() else {
                return failure("The stored report read omitted its rows.");
            };
            if rows.len() > size as usize {
                return failure("The stored report read exceeded the requested page.");
            }
            if !r.state["tables"][table].is_array() {
                r.state["tables"][table] = json!([]);
            }
            let existing = r.state["tables"][table].as_array_mut().unwrap();
            let mut seen: BTreeSet<String> = existing
                .iter()
                .filter_map(|x| x["SysRowID"].as_str().map(str::to_owned))
                .collect();
            for row in rows {
                let Some(id) = row["SysRowID"].as_str() else {
                    return failure("A stored report row has no identity.");
                };
                if !seen.insert(id.into()) {
                    return failure("The stored report read repeated a row.");
                }
                existing.push(row.clone());
            }
            if rows.len() == size as usize {
                r.state["after"] = rows.last().unwrap()["SysRowID"].clone();
            } else {
                r.state["after"] = Value::Null;
                r.state["tableIndex"] = json!(r.state["tableIndex"].as_u64().unwrap() + 1);
            }
        }
        _ => {
            let filter = r
                .identity
                .as_ref()
                .map(|id| format!("ReportID = '{}'", id.keys["ReportID"].replace('\'', "''")))
                .unwrap_or_default();
            let args = json!({"whereClauseReport":filter,"whereClauseReportStyle":"","whereClauseReportStyleImage":"","whereClauseReportStyleRule":"","whereClauseRptStylePrinters":"","whereClauseReportComp":"","pageSize":size,"absolutePage":page});
            return action(
                r,
                "visible",
                "Ice.BO.ReportSvc",
                "GetRows",
                args,
                "Reading visible report definitions and styles".into(),
            );
        }
    }
    if r.state["visible"].as_array().is_some_and(Vec::is_empty) {
        return prepare(r);
    }
    let index = r.state["tableIndex"].as_u64().unwrap_or(0) as usize;
    if index == TABLES.len() {
        return prepare(r);
    }
    let table = TABLES[index];
    if !r.state["fields"][table].is_array() {
        return action(
            r,
            "dictionary",
            "Ice.BO.BAQDesignerSvc",
            "GetFieldList",
            json!({"dataTableID":table,"schema":"Ice"}),
            format!("Reading {table} field definitions"),
        );
    }
    // Restrict physical reads to the native-visible identities before sending
    // the query. A shared report ID does not authorize another company's rows.
    let mut criteria = Vec::new();
    if table == "Report" {
        let ids: BTreeSet<_> = r.state["visible"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["ReportID"].as_str())
            .collect();
        let rows = r.state["parents"].as_array().unwrap();
        let keys = rows
            .iter()
            .filter(|row| row["ReportID"].as_str().is_some_and(|id| ids.contains(id)))
            .filter_map(|row| row["SysRowID"].as_str())
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return failure("The visible style has no parent identity.");
        }
        for (i, key) in keys.iter().enumerate() {
            criteria.push((
                "SysRowID".into(),
                "=".into(),
                crate::report_query::literal(key),
                if i == 0 { "" } else { "OR" }.into(),
                if i == 0 { "(" } else { "" }.into(),
                if i + 1 == keys.len() { ")" } else { "" }.into(),
            ));
        }
    } else {
        let rows = r.state["visible"].as_array().unwrap();
        for (i, row) in rows.iter().enumerate() {
            let (company, id, style) = match pair(row) {
                Ok(v) => v,
                Err(e) => return failure(e),
            };
            // Native image export selects ImageCompany, not the style owner.
            let (company_field, company_value) = if table == "ReportStyleImage" {
                ("ImageCompany", r.company.as_str())
            } else {
                ("Company", company.as_str())
            };
            criteria.push((
                company_field.into(),
                "=".into(),
                crate::report_query::literal(company_value),
                if i == 0 { "" } else { "OR" }.into(),
                if i == 0 { "((" } else { "(" }.into(),
                "".into(),
            ));
            criteria.push((
                "ReportID".into(),
                "=".into(),
                crate::report_query::literal(&id),
                "AND".into(),
                "".into(),
                "".into(),
            ));
            criteria.push((
                "StyleNum".into(),
                "=".into(),
                style.to_string(),
                "AND".into(),
                "".into(),
                if i + 1 == rows.len() { "))" } else { ")" }.into(),
            ));
        }
    }
    let args = match crate::report_query::build(
        &r.company,
        table,
        &r.state["fields"][table],
        size,
        &criteria,
        r.state["after"].as_str(),
    ) {
        Ok(a) => a,
        Err(e) => return failure(&e),
    };
    action(
        r,
        "physical",
        "Ice.BO.DynamicQuerySvc",
        "Execute",
        args,
        format!("Reading stored {table} rows for this batch"),
    )
}
fn prepare(mut r: GuestRequest) -> GuestReply {
    if r.category == "C17"
        && r.state["_collectionMethod"] == "serverAssisted"
        && r.state["layoutPageReceived"] != true
    {
        let paths: BTreeSet<String> = r.state["visible"].as_array().into_iter().flatten()
            .filter(|style| style["RptTypeID"] == "SSRS")
            .filter_map(|style| style["PrintProgram"].as_str())
            .flat_map(|paths| paths.split(','))
            .filter_map(|path| crate::report_resources::report_path(path).ok())
            .collect();
        if !paths.is_empty() {
            r.state["phase"] = json!("pageLayouts");
            r.state["_progress"] = json!({
                "request":format!("Downloading {} layout roots and their subreports in one request", paths.len()),
                "response":"Layout batch received; preparing native exports"
            });
            return GuestReply::Action {
                action: HostAction::HelperCall {
                    library: crate::report_store::contract().library_id,
                    function: "ReadLayouts".into(),
                    arguments: json!({"iCompany":r.company,"iPaths":serde_json::to_string(&paths).unwrap()}),
                },
                state:r.state,
            };
        }
        r.state["layoutPageReceived"] = json!(true);
        r.state["layoutBodies"] = json!({});
    }
    let next = r.state["next"].as_str().map(str::to_owned);
    if r.operation == "discover" && r.state["_hostPreparePage"] == true {
        if r.state["_hostIndependentWorkItems"] == true {
            // Build the owner records once. materialize stops at the local split
            // below, before any layout, image or ZIP dependency action.
            r.state["splitPreparedStyles"] = json!(true);
            return materialize(r);
        }
        // This is the actual visible page, not the requested page size. Keep
        // one progress total while its independent layout groups become ready.
        r.state["_progress"] = json!({"pageRecords":r.state["visible"].as_array().map(Vec::len)});
        GuestReply::PreparedPage {
            next_cursor: next,
            next_state: crate::report_archive::with_settings(json!({"fields":r.state["fields"]}),&r.state),
            state: r.state,
        }
    } else {
        materialize(r)
    }
}
fn receive_layout_page(mut r: GuestRequest) -> GuestReply {
    r.state["layoutPageReceived"] = json!(true);
    r.state["layoutBodies"] = json!({});
    r.state["layoutPageErrors"] = json!([]);
    if let Some(error) = r.error.take() {
        r.state["layoutPageErrors"] = json!([error]);
        return prepare(r);
    }
    let response = r.response.take().unwrap_or(Value::Null);
    if response["oSuccess"] != true {
        r.state["layoutPageErrors"] = json!([Diagnostic::new(
            "REPORT_LAYOUT_PAGE_REFUSED", "The layout reader refused the batch. Report settings remain available."
        )]);
        return prepare(r);
    }
    let Some(layouts) = response.pointer("/oDS/Layouts").and_then(Value::as_array) else {
        return failure("The layout batch omitted its layout table.");
    };
    for layout in layouts {
        let Some(path) = layout["Path"].as_str() else {
            return failure("A bulk layout has no native display path.");
        };
        let path = match crate::report_resources::report_path(path) {
            Ok(path) => path.to_lowercase(),
            Err(diagnostic) => return GuestReply::Failure { diagnostic },
        };
        let Some(file) = layout.pointer("/Content/nativeFile")
            .and_then(|value| serde_json::from_value::<FileRef>(value.clone()).ok()) else {
            return failure("A bulk layout has no host-retained original bytes.");
        };
        if !r.state["layoutBodies"][&path].is_null() {
            return failure("The layout batch repeats a native report path.");
        }
        r.state["layoutBodies"][path] = json!(file);
    }
    let Some(errors) = response.pointer("/oDS/Errors").and_then(Value::as_array) else {
        return failure("The layout batch omitted its failure table.");
    };
    for error in errors {
        let Some(path) = error["Path"].as_str() else {
            return failure("A layout failure has no report path.");
        };
        r.state["layoutPageErrors"].as_array_mut().unwrap().push(json!(Diagnostic::new(
            "REPORT_LAYOUT_READ_FAILED", format!("Layout {path} could not be collected. Other layouts in this batch are preserved.")
        )));
    }
    prepare(r)
}
fn materialize(mut r: GuestRequest) -> GuestReply {
    // Discovery labels must not leak into local parsing or layout requests.
    r.state.as_object_mut().unwrap().remove("_progress");
    if r.state["phase"] == "behaviorImages" {
        return next_images(r);
    }
    if r.state["phase"] == "nextStyle" {
        return start_style(r);
    }
    if r.state["phase"] == "pageZip" {
        if let Some(error) = r.error.take() {
            r.state["errors"].as_array_mut().unwrap().push(json!(error));
        } else if let Some(file) = r.response.take().and_then(|v| v.get("file").cloned()) {
            for record in r.state["records"].as_array_mut().unwrap() {
                record[1]["herbertSources"]["nativeFiles"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name":"reports.zip","file":file}));
            }
        } else {
            return failure("The host did not return the report ZIP receipt.");
        }
        return emit_style(r);
    }
    if r.state["records"].is_array() {
        if r.state["phase"] == "pageImages" {
            let index = r.state["imageIndex"].as_u64().unwrap() as usize;
            if let Some(error) = r.error.take() {
                r.state["errors"].as_array_mut().unwrap().push(json!(error));
            } else {
                let response = r.response.take().unwrap_or(Value::Null);
                let image = crate::body(&response);
                if !image["Image"].is_array() || !image["FileStore"].is_array() {
                    return failure("The native image package omitted its image or file table.");
                }
                r.state["records"][index][1]["imageExport"] = image.clone();
            }
            r.state["imageIndex"] = json!(index + 1);
            return next_images(r);
        }
        if let Some(action) = crate::report_resources::complete(&mut r) {
            return GuestReply::Action {
                action,
                state: r.state,
            };
        }
        return next_images(r);
    }
    let Some(styles) = r.state["visible"].as_array() else {
        return failure("The prepared report page has no visible styles.");
    };
    let mut records = Vec::new();
    let mut selection_errors=Vec::<Diagnostic>::new();
    for visible in styles {
        let key = match pair(visible) {
            Ok(v) => v,
            Err(e) => return failure(e),
        };
        let stored = r.state["tables"]["ReportStyle"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| pair(row).ok() == Some(key.clone()))
            .collect::<Vec<_>>();
        if stored.len() != 1 || stored[0]["SysRowID"] != visible["SysRowID"] {
            return failure("The stored style no longer matches the visible style.");
        }
        let parents = r.state["tables"]["Report"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| {
                row["ReportID"] == key.1
                    && r.state["parents"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|v| v["SysRowID"] == row["SysRowID"]))
            })
            .collect::<Vec<_>>();
        if parents.len() != 1 {
            return failure("The style has no unambiguous visible report parent.");
        }
        let mut data = json!({"Report":[parents[0]],"ReportStyle":[stored[0]],"ReportComp":[]});
        for table in &TABLES[2..] {
            data[*table] = json!(
                r.state["tables"][*table]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|row| { child_matches(table, row, &key, &r.company) })
                    .collect::<Vec<_>>()
            );
        }
        let scope = if key.0.is_empty() {
            Scope::Global
        } else if key.0 == r.company {
            Scope::Company {
                company: key.0.clone(),
            }
        } else {
            Scope::SharedCompany {
                company: r.company.clone(),
                owner_company: key.0.clone(),
            }
        };
        let identity = Identity {
            category: r.category.clone(),
            kind: if r.category == "C17" {
                "ReportStyle"
            } else {
                "ReportBehavior"
            }
            .into(),
            scope,
            keys: BTreeMap::from([
                ("Company".into(), key.0),
                ("ReportID".into(), key.1.clone()),
                ("StyleNum".into(), key.2.to_string()),
            ]),
        };
        // Epicor's style exporter clears CompanyList in the importable copy.
        // Preserve the exact physical value in the accompanying definition.
        let mut exported = data.clone();
        exported["ReportStyle"][0]["CompanyList"] = json!("");
        let mut errors=crate::report_archive::graph_errors(&identity,&data);
        data["herbertSources"]=json!({"files":[]});
        match crate::report_archive::xml("Report",&exported) {
            Ok(text)=>data["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":format!("{}_{}.xml",crate::bpm_code::label(&key.1),key.2),"text":text})),
            Err(mut diagnostic)=>{diagnostic.identity=Some(identity.clone());errors.push(diagnostic);},
        }
        if !errors.is_empty(){data["herbertCollectionStatus"]=json!({"completeness":"partial","diagnostics":errors});selection_errors.extend(errors);}
        if r.category == "C18" {
            for (i, rule) in data["ReportStyleRule"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                if let Some(body) = rule["RuleBody"].as_str() {
                    data["herbertSources"]["files"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"name":format!("RuleBody-{i}.xml"),"text":body}));
                }
            }
        }
        data["herbertDirectOutput"] = json!(true);
        records.push((identity, data));
    }
    r.state["records"] = json!(records);
    if r.category=="C17" {
        let mut errors=r.state["layoutPageErrors"].as_array().cloned().unwrap_or_default();errors.extend(selection_errors.iter().map(|error|json!(error)));r.state["layoutPageErrors"]=json!(errors);
    }else {r.state["selectionErrors"]=json!(selection_errors);}
    if r.category == "C18" && r.state["splitPreparedStyles"] == true {
        return independent_behaviors(r.state);
    }
    if r.category == "C17" {
        if r.state["splitPreparedStyles"] == true {
            return independent_styles(r.state);
        }
        r.state["pendingRecords"] = r.state["records"].take();
        r.state["pageErrors"] = r.state["layoutPageErrors"].as_array()
            .cloned().map_or_else(|| json!([]), |errors| json!(errors));
        // These discovery tables have already become exact per-style records.
        for key in ["tables", "visible", "parents", "fields"] {
            r.state.as_object_mut().unwrap().remove(key);
        }
        return start_style(r);
    }
    r.state["errors"] = r.state["selectionErrors"].clone();
    next_images(r)
}

fn independent_styles(mut state: Value) -> GuestReply {
    let records = state["records"].take().as_array().cloned().unwrap_or_default();
    let count = records.len();
    let mut positions = BTreeMap::new();
    let mut groups: Vec<Vec<Value>> = Vec::new();
    for record in records {
        let style = &record[1]["ReportStyle"][0];
        let key = json!([style["PrintProgram"],style["RptTypeID"]]).to_string();
        let at = *positions.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len()-1
        });
        groups[at].push(record);
    }
    let next = state["next"].as_str().map(str::to_owned);
    let next_state = crate::report_archive::with_settings(json!({"fields":state["fields"]}),&state);
    let mut items = Vec::new();
    for records in groups {
        let mut item = json!({"phase":"nextStyle","pendingRecords":records,
            "pageErrors":[],"next":null,"layoutPageReceived":state["layoutPageReceived"],
            "layoutBodies":state["layoutBodies"]});
        // The catalog contains host-owned file references, not file bytes. All
        // entries remain necessary until parsing reveals the subreport closure.
        // Definition tables, other style groups and finished exports are absent.
        for (key,value) in state.as_object().into_iter().flatten().filter(|(k,_)|k.starts_with('_')&&k.as_str()!="_progress") {
            item[key]=value.clone();
        }
        items.push(item);
    }
    if items.is_empty() {
        items.push(json!({"phase":"nextStyle","pendingRecords":[],"pageErrors":[],"next":null}));
    }
    items[0]["pageErrors"] = state["layoutPageErrors"].as_array().cloned().map_or_else(||json!([]),|errors|json!(errors));
    GuestReply::PreparedPage {
        state:crate::report_archive::with_settings(json!({"_hostWorkItems":items,"_hostWorkNextCursor":next,"_progress":{"pageRecords":count}}),&state),
        next_cursor:next,
        next_state,
    }
}
fn independent_behaviors(mut state: Value) -> GuestReply {
    let records=state["records"].take().as_array().cloned().unwrap_or_default();
    let count=records.len();
    let mut items:Vec<_>=records.into_iter().map(|record|json!({"phase":"behaviorImages",
        "records":[record],"errors":[],"imageIndex":0,"next":null})).collect();
    if items.is_empty(){items.push(json!({"phase":"behaviorImages","records":[],"errors":[],"imageIndex":0,"next":null}));}
    if state["selectionErrors"].is_array(){items[0]["errors"]=state["selectionErrors"].clone();}
    for item in &mut items {if let Some(settings)=state.get("_pluginSettings"){item["_pluginSettings"]=settings.clone();}}
    let next=state["next"].as_str().map(str::to_owned);
    GuestReply::PreparedPage{
        state:crate::report_archive::with_settings(json!({"_hostWorkItems":items,"_hostWorkNextCursor":next,"_progress":{"pageRecords":count}}),&state),
        next_cursor:next,next_state:crate::report_archive::with_settings(json!({"fields":state["fields"]}),&state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection_request(category:&str,prefix:&str,more:bool)->GuestRequest {
        let parents=json!([{"Company":"C","ReportID":"Alpha","SysRowID":"parent-a"},{"Company":"C","ReportID":"Beta","SysRowID":"parent-b"}]);
        let styles=json!([{"Company":"C","ReportID":"Alpha","StyleNum":1001,"SysRowID":"style-a","SystemFlag":false,"RptTypeID":"SSRS","PrintProgram":"reports/CustomReports/Outside/Main"},{"Company":"C","ReportID":"Beta","StyleNum":1002,"SysRowID":"style-b","SystemFlag":false,"RptTypeID":"SSRS","PrintProgram":"reports/CustomReports/Beta/Main"}]);
        let rules=json!([{"Company":"C","ReportID":"Alpha","StyleNum":1001,"SysRowID":"rule-a1","RuleBody":"<disabled>original Ω</disabled>","Disabled":true},{"Company":"C","ReportID":"Alpha","StyleNum":1001,"SysRowID":"rule-a2","RuleBody":"<enabled>original</enabled>","Disabled":false},{"Company":"C","ReportID":"Beta","StyleNum":1002,"SysRowID":"rule-b","RuleBody":"<unselected/>"}]);
        GuestRequest {abi:PLUGIN_ABI,operation:"discover".into(),category:category.into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),error:None,response:Some(json!({"returnObj":{"Report":parents,"ReportStyle":styles,"ReportStyleRule":rules},"parameters":{"morePages":more}})),state:json!({"phase":"visible","_hostPreparePage":true,"_pluginSettings":{"reportIdPrefix":prefix,"packagePathPrefix":"MissingPackage"}})}
    }
    fn acquired(mut r:GuestRequest)->GuestRequest {
        let body=crate::body(r.response.as_ref().unwrap()).clone();
        let GuestReply::Action {action:HostAction::NativeRead {method,..},state}=handle(r.clone())else{panic!("selected owners must reach the dictionary")};
        assert_eq!(method,"GetFieldList");r.state=state;r.response=None;
        let selected=r.state["visible"].as_array().unwrap();
        let selected_id: BTreeSet<_>=selected.iter().map(|style|style["ReportID"].as_str().unwrap().to_owned()).collect();
        let mut tables=json!({});for table in TABLES {tables[*table]=json!(body[*table].as_array().into_iter().flatten().filter(|row|selected_id.contains(row["ReportID"].as_str().unwrap_or(""))).cloned().collect::<Vec<_>>());}
        r.state["tables"]=tables;r.state["tableIndex"]=json!(TABLES.len());r.state["fields"]=json!({});r
    }
    fn behavior_output(mut r:GuestRequest,independent:bool)->(Vec<(Identity,Value)>,Vec<Diagnostic>){
        r.operation="materialize".into();r.state["splitPreparedStyles"]=json!(independent);
        if independent {let GuestReply::PreparedPage {state,next_state,..}=materialize(r.clone())else{panic!()};assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);let items=state["_hostWorkItems"].as_array().unwrap();let mut records=vec![];let mut diagnostics=vec![];for item in items {assert_eq!(item["_pluginSettings"],r.state["_pluginSettings"]);r.state=item.clone();let GuestReply::InventoryData {records:one,inventory}=materialize(r.clone())else{panic!()};records.extend(one);let GuestReply::Inventory {diagnostics:errors,..}=*inventory else{panic!()};diagnostics.extend(errors);}return (records,diagnostics);}
        let GuestReply::InventoryData {records,inventory}=materialize(r)else{panic!()};let GuestReply::Inventory {diagnostics,..}=*inventory else{panic!()};(records,diagnostics)
    }
    #[test]
    fn report_prefix_selects_before_physical_and_layout_reads_with_independent_package_scope() {
        let mut r=acquired(selection_request("C17","Alpha",true));assert_eq!(r.state["visible"].as_array().unwrap().len(),1);
        // A physical read is built from the selected native-visible parent only.
        r.state["phase"]=json!("dictionary");r.state["tableIndex"]=json!(0);r.response=Some(json!({"returnObj":{"TableFieldList":[{"FieldName":"SysRowID","DataType":"uniqueidentifier"},{"FieldName":"ReportID","DataType":"nvarchar"}]}}));
        let GuestReply::Action {action:HostAction::NativeRead {method,arguments,..},state}=handle(r.clone())else{panic!()};assert_eq!(method,"Execute");let query=serde_json::to_string(&arguments).unwrap();assert!(query.contains("parent-a"));assert!(!query.contains("parent-b"));assert_eq!(state["_pluginSettings"],r.state["_pluginSettings"]);
        let mut r=acquired(selection_request("C17","Alpha",false));r.state["_collectionMethod"]=json!("serverAssisted");
        let GuestReply::Action {action:HostAction::HelperCall {function,arguments,..},state}=prepare(r.clone())else{panic!()};assert_eq!(function,"ReadLayouts");let paths=arguments["iPaths"].as_str().unwrap();assert!(paths.contains("Outside/Main"));assert!(!paths.contains("Beta"));assert_eq!(state["_pluginSettings"],r.state["_pluginSettings"]);
        r.state["layoutPageReceived"]=json!(true);let GuestReply::PreparedPage {state,next_state,..}=prepare(r.clone())else{panic!()};assert_eq!(state["visible"].as_array().unwrap().len(),1);assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);assert_eq!(next_state["_collectionMethod"],"serverAssisted");
        assert!(!methods().iter().any(|(_,method)|matches!(method.as_str(),"ExportSsrsReports"|"ExportSsrsReportsToZip"|"PrepareZipWithSsrsReports")));
    }
    #[test]
    fn behavior_prefix_keeps_complete_rules_xml_and_independent_owner_equivalence() {
        let baseline=behavior_output(acquired(selection_request("C18","",false)),false);
        let r=acquired(selection_request("C18","Alpha",false));let filtered=behavior_output(r.clone(),false);let independent=behavior_output(r,true);
        assert_eq!(filtered,independent);assert_eq!(filtered.0,baseline.0.into_iter().filter(|(id,_)|id.keys["ReportID"]=="Alpha").collect::<Vec<_>>());assert!(filtered.1.is_empty());assert_eq!(filtered.0.len(),1);
        let data=&filtered.0[0].1;assert_eq!(data["ReportStyleRule"].as_array().unwrap().len(),2);assert_eq!(data["ReportStyleRule"][0]["Disabled"],true);let files=data["herbertSources"]["files"].as_array().unwrap();assert!(files.iter().any(|file|file["text"]=="<disabled>original Ω</disabled>"));assert!(files.iter().any(|file|file["text"].as_str().is_some_and(|text|text.contains("<ReportStyleRule>"))));
    }
    #[test]
    fn report_selection_validates_excluded_visibility_and_preserves_raw_empty_exact_states() {
        for category in ["C17","C18"] {for prefix in ["alpha"," Alpha","%","Missing"] {let r=selection_request(category,prefix,true);let GuestReply::PreparedPage {state,next_cursor,next_state}=handle(r.clone())else{panic!()};assert!(state["visible"].as_array().unwrap().is_empty());assert_eq!(serde_json::from_str::<Value>(next_cursor.as_deref().unwrap()).unwrap()["page"],2);assert_eq!(next_state["_pluginSettings"],r.state["_pluginSettings"]);}
            for defect in ["flag","parent","rule"] {let mut r=selection_request(category,"Missing",false);let body=&mut r.response.as_mut().unwrap()["returnObj"];match defect {"flag"=>body["ReportStyle"][1]["SystemFlag"]=json!("bad"),"parent"=>body["ReportStyle"][1]["ReportID"]=json!("NoParent"),_=>body["ReportStyleRule"][2]["StyleNum"]=json!(999)};assert!(matches!(handle(r),GuestReply::Failure {..}),"{category}/{defect}");}
            let mut r=selection_request(category,"",false);r.operation="acquire".into();r.state["_pluginSettings"]=json!({"reportIdPrefix":false,"packagePathPrefix":false});r.identity=Some(Identity {category:category.into(),kind:if category=="C17"{"ReportStyle"}else{"ReportBehavior"}.into(),scope:Scope::Company {company:"C".into()},keys:BTreeMap::from([("Company".into(),"C".into()),("ReportID".into(),"Alpha".into()),("StyleNum".into(),"1001".into())])});let GuestReply::Action {state,..}=handle(r)else{panic!()};assert_eq!(state["visible"].as_array().unwrap().len(),1);
        }
    }
    #[test]
    fn selected_invalid_typed_graph_keeps_valid_peers_and_incomplete_behavior_work_items() {
        let mut r=acquired(selection_request("C18","",false));r.state["tables"]["ReportStyleRule"][0]["RuleBody"]=json!(false);
        for independent in [false,true] {let (records,errors)=behavior_output(r.clone(),independent);assert_eq!(records.len(),2);assert!(errors.iter().any(|error|error.code=="REPORT_XML_INVALID" && error.identity.as_ref().is_some_and(|id|id.keys["ReportID"]=="Alpha")));assert_eq!(records[0].1["herbertCollectionStatus"]["completeness"],"partial");assert!(records[1].1["herbertSources"]["files"].as_array().unwrap().iter().any(|file|file["name"]=="Beta_1002.xml"));}
    }
    #[test]
    fn independent_behavior_images_match_legacy_output_without_other_owner_state() {
        fn run(independent:bool)->Vec<(Identity,Value)>{
            let records:Vec<_>=(1..=2).map(|n|json!([Identity{category:"C18".into(),kind:"ReportBehavior".into(),scope:Scope::Company{company:"C".into()},keys:BTreeMap::from([("ReportID".into(),"R".into()),("StyleNum".into(),n.to_string())])},{"ReportStyle":[{"ReportID":"R","StyleNum":n}],"ReportStyleImage":[{"ImageID":format!("image{n}")}],"herbertSources":{"files":[{"name":"behavior.xml","text":format!("é original {n}")}]}}])).collect();
            let mut state=json!({"phase":"behaviorImages","records":records,"errors":[],"imageIndex":0,"next":null});
            let mut work=std::collections::VecDeque::new();
            if independent{
                let GuestReply::PreparedPage{state:prepared,..}=independent_behaviors(state)else{panic!()};
                work=prepared["_hostWorkItems"].as_array().unwrap().clone().into();state=work.pop_front().unwrap();
            }
            let mut r=GuestRequest{abi:PLUGIN_ABI,operation:"materialize".into(),category:"C18".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state,response:None,error:None};
            let mut output=Vec::new();let mut calls=0;
            loop{match materialize(r.clone()){
                GuestReply::Action{action:HostAction::SharedNativeRead{method,..},state}=>{
                    assert_eq!(method,"ExportReportStyleImages");calls+=1;
                    if independent{assert_eq!(state["records"].as_array().unwrap().len(),1);}
                    r.state=state;r.response=Some(json!({"returnObj":{"Image":[],"FileStore":[]}}));
                },
                GuestReply::InventoryData{records,..}=>{output.extend(records);if let Some(state)=work.pop_front(){r.state=state;r.response=None;}else{break;}},
                other=>panic!("{other:?}"),
            }}
            assert_eq!(calls,2);output
        }
        assert_eq!(run(true),run(false));
    }
    #[test]
    fn independent_style_groups_match_legacy_bytes_and_resolve_catalog_subreports() {
        fn run(independent:bool)->(Vec<(Identity,Value)>,usize,usize){
            let record=|n:u32,path:&str|json!([Identity{category:"C17".into(),kind:"ReportStyle".into(),scope:Scope::Company{company:"C".into()},keys:BTreeMap::from([("ReportID".into(),"R".into()),("StyleNum".into(),n.to_string())])},{"ReportStyle":[{"RptTypeID":"SSRS","PrintProgram":path}],"herbertDirectOutput":true,"herbertSources":{"files":[{"name":"original.xml","text":format!("é {n}")}]}}]);
            let records=json!([record(1,"reports/CustomReports/R/Main"),record(2,"reports/CustomReports/R/Main"),record(3,"reports/CustomReports/R/Other")]);
            let file=|name:&str|FileRef{path:format!("native/{name}.rdl"),sha256:"a".repeat(64),bytes:200,role:"nativeExport".into()};
            let mut state=json!({"phase":"nextStyle","pendingRecords":records,"pageErrors":[],"next":null,"layoutPageReceived":true,"layoutBodies":{"reports/customreports/r/main":file("main"),"reports/customreports/r/child":file("child"),"reports/customreports/r/other":file("other")}});
            let mut work=std::collections::VecDeque::new();
            if independent{
                state["records"]=state["pendingRecords"].take();
                let GuestReply::PreparedPage{state:prepared,..}=independent_styles(state)else{panic!()};
                work=prepared["_hostWorkItems"].as_array().unwrap().clone().into();
                assert_eq!(work.len(),2);
                assert_eq!(work[0]["pendingRecords"].as_array().unwrap().len(),2);
                assert_eq!(work[1]["pendingRecords"].as_array().unwrap().len(),1);
                state=work.pop_front().unwrap();
            }
            let mut r=GuestRequest{abi:PLUGIN_ABI,operation:"materialize".into(),category:"C17".into(),company:"C".into(),identity:None,cursor:None,page_size:Some(100),state,response:None,error:None};
            let mut output=Vec::new();let mut parses=0;let mut zips=0;
            loop{match materialize(r.clone()){
                GuestReply::Action{action,state}=>{
                    if independent{assert!(state["pendingRecords"].as_array().unwrap().is_empty());}
                    r.state=state;
                    r.response=Some(match action{
                        HostAction::ParseXml{file:source}=>{
                            parses+=1;
                            let ns="http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition";
                            let content=if source.path.ends_with("main.rdl"){json!([{"element":{"name":"Subreport","namespace":ns,"attributes":[],"content":[{"element":{"name":"ReportName","namespace":ns,"attributes":[],"content":[{"text":"Child"}]}}]}}])}else{json!([])};
                            json!({"sourceFile":source,"parsedXml":{"name":"Report","namespace":ns,"attributes":[],"content":content}})
                        },
                        HostAction::BuildZip{entries}=>{zips+=1;assert_eq!(entries.len(),if zips==1{2}else{1});json!({"file":file(if zips==1{"main-zip"}else{"other-zip"})})},
                        other=>panic!("No extra native read is needed: {other:?}"),
                    });
                },
                GuestReply::ReadyRecords{records,state}=>{output.extend(records);r.state=state;r.response=None;},
                GuestReply::InventoryData{records,..}=>{
                    output.extend(records);
                    if let Some(state)=work.pop_front(){r.state=state;r.response=None;}else{break;}
                },
                other=>panic!("{other:?}"),
            }}
            (output,parses,zips)
        }
        let legacy=run(false);let independent=run(true);
        assert_eq!(independent,legacy);assert_eq!(independent.0.len(),3);assert_eq!((independent.1,independent.2),(3,2));
    }
    #[test]
    fn ready_styles_keep_bodies_in_host_and_group_shared_layouts() {
        ready_styles(false);
    }
    #[test]
    fn bulk_layout_page_never_requests_individual_layouts() {
        ready_styles(true);
    }
    fn ready_styles(bulk: bool) {
        let record = |number: u32| {
            let id = Identity {
                category: "C17".into(),
                kind: "ReportStyle".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([
                    ("ReportID".into(), "R".into()),
                    ("StyleNum".into(), number.to_string()),
                ]),
            };
            json!([id,{"ReportStyle":[{"RptTypeID":"SSRS","PrintProgram":"reports/CustomReports/R/Main"}],
                "herbertDirectOutput":true,"herbertSources":{"files":[]}}])
        };
        let mut other = record(3);
        other[1]["ReportStyle"][0]["PrintProgram"] = json!("reports/CustomReports/R/Other");
        let mut r = GuestRequest {
            abi: 1,
            operation: "materialize".into(),
            category: "C17".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(100),
            response: None,
            error: None,
            state: json!({"phase":"nextStyle","pendingRecords":[record(1),record(2),other],"pageErrors":[],
                "_progress":{"request":"stale printer label"}}),
        };
        let file = FileRef {
            path: "native/main.rdl".into(),
            sha256: "a".repeat(64),
            bytes: 200000,
            role: "nativeExport".into(),
        };
        if bulk {
            r.state["layoutPageReceived"] = json!(true);
            r.state["layoutBodies"] = json!({
                "reports/customreports/r/main":file,
                "reports/customreports/r/other":file
            });
        }
        let mut reads = 0;
        let mut parses = 0;
        let mut zips = 0;
        let mut ready = 0;
        loop {
            match materialize(r.clone()) {
                GuestReply::Action { action, state } => {
                    r.state = state;
                    assert_ne!(r.state["_progress"]["request"], "stale printer label");
                    r.response = Some(match action {
                        HostAction::SharedNativeRead { method, .. } => {
                            assert_eq!(method, "ExportSsrsRdl");
                            reads += 1;
                            json!({"returnObj":{"nativeFile":file}})
                        }
                        HostAction::ParseXml { .. } => {
                            parses += 1;
                            json!({"sourceFile":file,"parsedXml":{
                            "name":"Report","namespace":"http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition",
                            "attributes":[],"content":[]}})
                        }
                        HostAction::BuildZip { entries } => {
                            zips += 1;
                            assert_eq!(entries.len(), 1);
                            json!({"file":file})
                        }
                        _ => panic!("Unexpected action: {action:?}"),
                    });
                }
                GuestReply::ReadyRecords { records, state } => {
                    ready += records.len();
                    assert_eq!(ready, 2);
                    assert!(!serde_json::to_string(&records).unwrap().contains("base64"));
                    r.state = state;
                    r.response = None;
                }
                GuestReply::InventoryData { records, .. } => {
                    assert_eq!(records.len(), 1);
                    break;
                }
                other => panic!("Unexpected reply: {other:?}"),
            }
        }
        assert_eq!((reads, parses, zips, ready), (if bulk { 0 } else { 2 }, 2, 2, 2));
    }
    #[test]
    fn images_use_native_image_company_without_leaking_foreign_links() {
        let key = ("STYLE_OWNER".into(), "ReportA".into(), 2);
        let mut image =
            json!({"Company":"","ImageCompany":"CURRENT","ReportID":"ReportA","StyleNum":2});
        assert!(child_matches("ReportStyleImage", &image, &key, "CURRENT"));
        image["ImageCompany"] = json!("OTHER");
        assert!(!child_matches("ReportStyleImage", &image, &key, "CURRENT"));
        image["ImageCompany"] = json!("CURRENT");
        image["StyleNum"] = json!(3);
        assert!(!child_matches("ReportStyleImage", &image, &key, "CURRENT"));
        let rule = json!({"Company":"STYLE_OWNER","ReportID":"ReportA","StyleNum":2});
        assert!(child_matches("ReportStyleRule", &rule, &key, "CURRENT"));
    }
}
fn next_images(mut r: GuestRequest) -> GuestReply {
    let mut index = r.state["imageIndex"].as_u64().unwrap_or(0) as usize;
    while let Some(record) = r.state["records"].get(index) {
        if record[1]["ReportStyleImage"]
            .as_array()
            .is_some_and(|v| !v.is_empty())
        {
            let style = &record[1]["ReportStyle"][0];
            let args = json!({"reportId":style["ReportID"],"styleNumber":style["StyleNum"]});
            r.state["phase"] = json!("pageImages");
            r.state["imageIndex"] = json!(index);
            return GuestReply::Action {
                action: HostAction::SharedNativeRead {
                    service: "Ice.BO.ReportSvc".into(),
                    method: "ExportReportStyleImages".into(),
                    arguments: args,
                    export: false,
                },
                state: r.state,
            };
        }
        index += 1;
    }
    finish(r)
}
fn report_image_bytes(images: &Value, row: &Value) -> Result<Vec<u8>, &'static str> {
    let key = row["ImageSysRowID"].as_str().filter(|s| !s.is_empty())
        .ok_or("the FileStore association is missing or invalid.")?;
    let matches: Vec<_> = images["FileStore"].as_array().into_iter().flatten()
        .filter(|f| f["SysRowID"].as_str().is_some_and(|id| id.eq_ignore_ascii_case(key)))
        .collect();
    let first = matches.first().ok_or("the associated FileStore row is absent.")?;
    let content = first["Content"].as_str().ok_or("FileStore content is absent or is not a string.")?;
    if matches.iter().any(|f| f["Content"].as_str() != Some(content)) {
        return Err("the associated FileStore rows contain conflicting content.");
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(content)
        .map_err(|_| "FileStore content is not valid base64.")?;
    if bytes.is_empty() { return Err("FileStore content is empty."); }
    Ok(bytes)
}
fn finish(mut r: GuestRequest) -> GuestReply {
    let mut records: Vec<(Identity, Value)> =
        match serde_json::from_value(r.state["records"].clone()) {
            Ok(v) => v,
            Err(_) => return failure("Invalid prepared report records."),
        };
    let mut diagnostics: Vec<Diagnostic> =
        serde_json::from_value(r.state["errors"].clone()).unwrap_or_default();
    for (identity, data) in &mut records {
        if data["imageExport"].is_object() {
            let images = data["imageExport"].clone();
            data["herbertSources"]["files"].as_array_mut().unwrap().push(json!({"name":"ReportStyleImages.json","text":serde_json::to_string_pretty(&images).unwrap()}));
            for row in images["Image"].as_array().into_iter().flatten() {
                let result = report_image_bytes(&images, row);
                match result {
                    Ok(bytes) => {
                        let name = format!("Images/{}.{}",
                            crate::bpm_code::label(row["ImageID"].as_str().unwrap_or("Image")),
                            crate::bpm_code::label(row["FileType"].as_str().unwrap_or("bin")));
                        crate::report_archive::append_binary(data,
                            crate::report_archive::binary(&name, &bytes));
                    }
                    Err(message) => {
                        let mut diagnostic = Diagnostic::new("REPORT_IMAGE_UNAVAILABLE",
                            format!("Report image {}: {message}", row["ImageID"]));
                        diagnostic.identity = Some(identity.clone());
                        diagnostics.push(diagnostic.clone());
                        r.state["errors"].as_array_mut().unwrap().push(json!(diagnostic));
                    }
                }
            }
        }
    }
    if r.category == "C17" {
        for (_, data) in &mut records {
            let mut reachable: BTreeSet<String> = data["ReportStyle"][0]["PrintProgram"]
                .as_str()
                .unwrap_or("")
                .split(',')
                .filter_map(|p| crate::report_resources::report_path(p).ok())
                .map(|p| p.to_lowercase())
                .collect();
            loop {
                let before = reachable.len();
                for edge in r.state["rdlEdges"].as_array().into_iter().flatten() {
                    if edge["source"]
                        .as_str()
                        .is_some_and(|p| reachable.contains(&p.to_lowercase()))
                    {
                        if let Some(target) = edge["target"].as_str() {
                            reachable.insert(target.to_lowercase());
                        }
                    }
                }
                if before == reachable.len() {
                    break;
                }
            }
            let documents = r.state["rdlDocuments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| {
                    d["path"]
                        .as_str()
                        .is_some_and(|p| reachable.contains(&p.to_lowercase()))
                })
                .cloned()
                .collect::<Vec<_>>();
            data["layoutCollection"] = json!({"snapshot":"nonAtomic","documents":documents});
            data["herbertSources"]["nativeFiles"] = json!([]);
            let mut entries = Vec::new();
            for entry in r.state["rdlQueue"].as_array().into_iter().flatten() {
                let path = entry["path"].as_str().unwrap_or("");
                if !reachable.contains(&path.to_lowercase()) {
                    continue;
                }
                if entry["file"].is_object() {
                    data["herbertSources"]["nativeFiles"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"name":format!("Layouts/{path}.rdl"),"file":entry["file"]}));
                }
                if let Ok(file) = serde_json::from_value::<FileRef>(entry["file"].clone()) {
                    entries.push((format!("{path}.rdl"), file));
                }
            }
            // Native report ZIPs contain RDLs and subreports. External sources,
            // images and assemblies remain references with separate diagnostics.
            let complete = entries.len() == reachable.len()
                && !reachable.is_empty()
                && documents.iter().all(|d| {
                    d["unresolved"]
                        .as_array()
                        .is_some_and(|v| v.iter().all(|r| r["kind"] != "Subreport"))
                });
            data["layoutCollection"]["complete"] = json!(complete);
            if complete {
                r.state["zipEntries"] = json!(entries);
            }
            for doc in documents {
                if let (Some(path), Some(code)) =
                    (doc["path"].as_str(), doc["embeddedCode"].as_str())
                {
                    data["herbertSources"]["files"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"name":format!("Code/{path}.vb"),"text":code}));
                }
            }
        }
    }
    if r.category == "C17" {
        r.state["records"] = json!(records);
        if let Ok(entries) =
            serde_json::from_value::<Vec<(String, FileRef)>>(r.state["zipEntries"].take())
        {
            r.state["phase"] = json!("pageZip");
            return GuestReply::Action {
                action: HostAction::BuildZip { entries },
                state: r.state,
            };
        }
        return emit_style(r);
    }
    if r.operation == "acquire" {
        if records.len() != 1 || r.identity.as_ref() != Some(&records[0].0) {
            return failure("The exact report export does not match the requested identity.");
        }
        let (identity, definition) = records.remove(0);
        return GuestReply::Definition {
            identity,
            definition,
            format: "native-report-style".into(),
            completeness: if diagnostics.is_empty() {
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
            authoritative: diagnostics.is_empty(),
            diagnostics,
        }),
        records,
    }
}

/// Resolve one style and publish it before starting the next. SharedNativeRead
/// still reuses a layout across styles; layout bodies stay in the host.
fn start_style(mut r: GuestRequest) -> GuestReply {
    let pending = r.state["pendingRecords"].as_array_mut().unwrap();
    if pending.is_empty() {
        r.state["records"] = json!([]);
        r.state["errors"] = json!([]);
        return emit_style(r);
    }
    let record = pending.remove(0);
    let styles = record[1]["ReportStyle"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let program = record[1]["ReportStyle"][0]["PrintProgram"].clone();
    let kind = record[1]["ReportStyle"][0]["RptTypeID"].clone();
    let mut ready = vec![record];
    // Identical layout roots become ready together. Build their ZIP once, while
    // preserving each style's own settings, images, identity and output folder.
    let mut index = 0;
    while index < pending.len() {
        if pending[index][1]["ReportStyle"][0]["PrintProgram"] == program
            && pending[index][1]["ReportStyle"][0]["RptTypeID"] == kind
        {
            ready.push(pending.remove(index));
        } else {
            index += 1;
        }
    }
    r.state["records"] = json!(ready);
    r.state["errors"] = json!([]);
    r.state["imageIndex"] = json!(0);
    r.state.as_object_mut().unwrap().remove("supplements");
    if let Some(action) = crate::report_resources::start_page(&mut r, &styles) {
        GuestReply::Action {
            action,
            state: r.state,
        }
    } else {
        next_images(r)
    }
}

fn emit_style(mut r: GuestRequest) -> GuestReply {
    let mut records: Vec<(Identity, Value)> =
        serde_json::from_value(r.state["records"].take()).unwrap();
    let errors:Vec<Diagnostic> = serde_json::from_value(r.state["errors"].clone()).unwrap_or_default();
    // Every style in this ready group shares these layout roots. Attribute the
    // diagnostic to each affected style instead of leaving an anonymous run issue.
    for (identity,data) in &mut records {
        let diagnostics:Vec<_>=errors.iter().filter(|d| d.identity.as_ref().is_none_or(|owner| owner == identity)).cloned().map(|mut d|{if d.identity.is_none(){d.identity=Some(identity.clone());}d}).collect();
        if !diagnostics.is_empty(){
            data["herbertCollectionStatus"]=json!({"completeness":"partial","diagnostics":diagnostics});
            r.state["pageErrors"].as_array_mut().unwrap().extend(diagnostics.into_iter().map(|d|json!(d)));
        }
    }
    if r.operation == "acquire" {
        let diagnostics: Vec<Diagnostic> =
            serde_json::from_value(r.state["pageErrors"].take()).unwrap();
        if records.len() != 1 || r.identity.as_ref() != Some(&records[0].0) {
            return failure("The exact report export does not match the requested identity.");
        }
        let (identity, definition) = records.into_iter().next().unwrap();
        return GuestReply::Definition {
            identity,
            definition,
            format: "native-report-style".into(),
            completeness: if diagnostics.is_empty() {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
        };
    }
    if r.state["pendingRecords"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        r.state["phase"] = json!("nextStyle");
        for key in ["rdlQueue", "rdlDocuments", "rdlEdges", "supplements"] {
            r.state.as_object_mut().unwrap().remove(key);
        }
        GuestReply::ReadyRecords {
            records,
            state: r.state,
        }
    } else {
        let diagnostics: Vec<Diagnostic> =
            serde_json::from_value(r.state["pageErrors"].take()).unwrap();
        GuestReply::InventoryData {
            inventory: Box::new(GuestReply::Inventory {
                identities: records.iter().map(|(id, _)| id.clone()).collect(),
                next_cursor: r.state["next"].as_str().map(str::to_owned),
                authoritative: diagnostics.is_empty(),
                diagnostics,
            }),
            records,
        }
    }
}

#[cfg(test)]
mod wave1_image_tests {
    use super::*;
    #[test]
    fn wave1_image_associations_require_unambiguous_nonempty_bytes() {
        let row=json!({"ImageID":"Logo","ImageSysRowID":"image"});
        for files in [json!([]),json!([{"SysRowID":"image"}]),json!([{"SysRowID":"image","Content":null}]),json!([{"SysRowID":"image","Content":42}]),json!([{"SysRowID":"image","Content":"!"}]),json!([{"SysRowID":"image","Content":""}]),json!([{"SysRowID":"image","Content":"UEs="},{"SysRowID":"IMAGE","Content":"AA=="}])] {
            assert!(report_image_bytes(&json!({"FileStore":files}),&row).is_err());
        }
        assert_eq!(report_image_bytes(&json!({"FileStore":[{"SysRowID":"IMAGE","Content":"UEs="},{"SysRowID":"image","Content":"UEs="}]}),&row).unwrap(),b"PK");
    }
}
