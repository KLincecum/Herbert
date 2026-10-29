//! Collect BAQ and UBAQ definitions in pages and build their packages locally.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const SERVICE: &str = "Ice.BO.BAQDesignerSvc";
const TABLES: &[&str] = &[
    "DynamicQueryDesigner",
    "QueryCtrlDesigner",
    "QueryCtrlValuesDesigner",
    "QueryCustomActionDesigner",
    "QueryExecuteSettingDesigner",
    "QueryParameterDesigner",
    "QueryReferenceDesigner",
    "QueryParameterBindingDesigner",
    "QuerySubQueryDesigner",
    "QueryRelationDesigner",
    "QueryRelationFieldDesigner",
    "QuerySortByDesigner",
    "QueryWhereItemDesigner",
    "QueryGroupByDesigner",
    "QueryTableDesigner",
    "QueryFieldDesigner",
    "QueryFieldAttributeDesigner",
    "QueryFunctionCallDesigner",
    "QueryUpdateFieldDesigner",
    "QueryUpdateSettingsDesigner",
    "QueryValueSetItemsDesigner",
];

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("BAQ_PAGE_INVALID", message)
}
pub fn methods() -> Vec<(String, String)> {
    vec![
        (SERVICE.into(), "GetRows".into()),
        (SERVICE.into(), "GetTableList".into()),
        ("Ice.Lib.SessionModSvc".into(), "GetBaseLevel".into()),
        ("Ice.Lib.BOReaderSvc".into(), "GetCodeDescList".into()),
        ("Ice.BO.ExtensionSvc".into(), "GetRows".into()),
        (crate::ubaq_rows::SERVICE.into(), "GetRowsEx".into()),
    ]
}

fn shared(
    service: &str,
    method: &str,
    arguments: Value,
    mut state: Value,
    phase: &str,
) -> GuestReply {
    state["phase"] = json!(phase);
    state["_progress"] = json!({
        "request":"Reading extension information for this batch",
        "response":"Extension information received; preparing local exports"
    });
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

// ExtensionTableset tables from native-child-contracts.json; the GetRows
// signature itself is not yet verified against a server. Only Extension roots are
// used; child where clauses stay blank, as GetByID returned whole children.
const EXTENSION_TABLES: &[&str] = &[
    "Extension",
    "ExtensionSetMapping",
    "ExtensionTable",
    "ExtensionColumn",
    "ExtensionRelation",
    "ExtensionRelationColumn",
    "ExtensionTableKey",
];

fn extension_ids(state: &Value) -> Result<Vec<String>, Diagnostic> {
    state["extensionIds"]
        .as_array()
        .filter(|ids| !ids.is_empty())
        .ok_or_else(|| invalid("The extension saved progress has no identity."))?
        .iter()
        .map(|id| id.as_str().map(str::to_owned).ok_or_else(|| invalid("The extension saved progress has no identity.")))
        .collect()
}

// Bulk-first: one read for every extension this batch needs; rows are matched to IDs locally.
fn extension_info_action(state: Value) -> Result<GuestReply, Diagnostic> {
    let filter: Vec<_> = extension_ids(&state)?
        .iter()
        .map(|id| format!("ExtensionSetID = '{}'", id.replace('\'', "''")))
        .collect();
    let mut arguments = json!({"pageSize":0,"absolutePage":1});
    for table in EXTENSION_TABLES {
        arguments[format!("whereClause{table}")] = json!("");
    }
    arguments["whereClauseExtension"] = json!(filter.join(" OR "));
    Ok(shared(
        "Ice.BO.ExtensionSvc",
        "GetRows",
        arguments,
        state,
        "extensionInfo",
    ))
}

pub fn handle(request: GuestRequest) -> GuestReply {
    match collect(request) {
        Ok(reply) => reply,
        Err(diagnostic) => GuestReply::Failure { diagnostic },
    }
}

/// The prefix's leading run before any LIKE wildcard, bracket or non-ASCII character, for a
/// server-side `LIKE 'stem%'` superset that needs no escape syntax. Non-ASCII stops the stem
/// because the literal is not N-prefixed and could be converted lossily. None when nothing
/// narrows the read.
fn like_stem(prefix: &str) -> Option<&str> {
    let stem = &prefix[..prefix.find(|c: char| matches!(c, '%' | '_' | '[' | ']') || !c.is_ascii()).unwrap_or(prefix.len())];
    (!stem.is_empty()).then_some(stem)
}

/// Marks a page cursor written while the QueryID pushdown narrowed the root read.
const NARROWED: &str = ":narrowed";
/// The page after this one, in the cursor format of the read that produced it.
fn next_page(state: &Value) -> Option<String> {
    (state["more"] == true).then(|| format!("{}{}", state["page"].as_u64().unwrap() + 1, if state["narrowed"] == true { NARROWED } else { "" }))
}
fn read_page(request: &GuestRequest, version: &str, prefix: &str) -> Result<GuestReply, Diagnostic> {
    let narrowed = like_stem(prefix).is_some();
    let page = match &request.cursor {
        Some(cursor) => {
            let (number, marked) = match cursor.strip_suffix(NARROWED) { Some(number) => (number, true), None => (cursor.as_str(), false) };
            let number = number.parse::<u32>().map_err(|_| invalid("The BAQ page saved position is invalid."))?;
            match (narrowed, marked) {
                (true, true) | (false, false) => number,
                // A position saved before the QueryID pushdown counts pages of the unfiltered
                // list. Resuming it in the narrowed list could skip queries, so discovery
                // restarts at page 1; already written queries are written again unchanged.
                (true, false) => 1,
                (false, true) => return Err(invalid("The BAQ page saved position is invalid.")),
            }
        }
        None => 1,
    };
    let size = request
        .page_size
        .ok_or_else(|| invalid("Select a BAQ page size."))?;
    if page == 0 || page > i32::MAX as u32 || size == 0 || size > i32::MAX as u32 {
        return Err(invalid(
            "The page number and page size must each be a whole number from 1 to 2147483647.",
        ));
    }
    let mut arguments = json!({"pageSize":size,"absolutePage":page});
    for table in TABLES {
        arguments[format!("whereClause{table}")] = json!("");
    }
    let mut filter = format!(
        "(Company = '{}' OR Company = '') AND SystemFlag = false",
        request.company.replace('\'', "''")
    );
    // A broad QueryID superset; the literal case-sensitive prefix still selects locally.
    // Child tables keep blank clauses: GetRows returns the children of the returned roots.
    if let Some(stem) = like_stem(prefix) {
        filter.push_str(&format!(" AND QueryID LIKE '{}%'", stem.replace('\'', "''")));
    }
    arguments["whereClauseDynamicQueryDesigner"] = json!(filter);
    Ok(GuestReply::Action {
        action: HostAction::NativeRead {
            service: SERVICE.into(),
            method: "GetRows".into(),
            arguments,
            export: false,
        },
        state: json!({"phase":"rows","version":version,"page":page,"narrowed":narrowed,
            "_progress":{"request":format!("Downloading BAQ definitions - batch {page} (up to {size} queries)"),
                "response":"Definitions received; gathering the rest of this batch"}}),
    })
}

fn collect(mut request: GuestRequest) -> Result<GuestReply, Diagnostic> {
    #[cfg(feature="ubaq-compiled")]
    if crate::ubaq_compiled::queue_active(&request.state) { return crate::ubaq_compiled::queue(&request); }
    #[cfg(feature="ubaq-compiled")]
    if request.state["phase"] == "compiledUbaqStart" { return crate::ubaq_compiled::start_saved(&request.state); }
    #[cfg(feature="ubaq-compiled")]
    if request.state["phase"] == "compiledUbaq" { return crate::ubaq_compiled::finish_saved(&crate::ubaq_compiled::received(&request)); }
    if request.operation == "materialize" {
        request.state["_hostPreparePage"] = json!(false);
        return finish(&request.state);
    }
    if let Some(error) = request.error.take() {
        if request.state["phase"] == "bpmRows" {
            return finish_without_methods(request.state, error);
        }
        return Err(error);
    }
    if let Some(size) = request.state.pointer("/_pluginSettings/queriesPerPage") {
        request.page_size = Some(
            size.as_u64()
                .filter(|n| *n > 0 && *n <= i32::MAX as u64)
                .ok_or_else(|| invalid("Queries per request must be a whole number from 1 to 2147483647."))?
                as u32,
        );
    }
    if request.operation != "discover" {
        return Err(invalid(
            "This collector writes complete definitions during discovery.",
        ));
    }
    // Only frozen run settings choose roots. A missing legacy setting means
    // the original unfiltered guest request, never today's saved defaults.
    let prefix = match request.state.pointer("/_pluginSettings/queryIdPrefix") {
        None => "",
        Some(Value::String(value)) => value.as_str(),
        Some(_) => return Err(Diagnostic::new("INVALID_SETTING", "Query ID starts with must be a string.")),
    };
    match request.state["phase"].as_str().unwrap_or("initial") {
        "nextPage" => read_page(&request, request.state["version"].as_str()
            .ok_or_else(|| invalid("The page export version is missing."))?, prefix),
        "initial" => Ok(GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.SessionModSvc".into(),
                method: "GetBaseLevel".into(),
                arguments: json!({}),
                export: false,
            },
            state: json!({"phase":"version","_progress":{
                "request":"Checking the server's export format",
                "response":"Export format received; preparing the first batch"}}),
        }),
        "version" => {
            let response = request
                .response
                .as_ref()
                .ok_or_else(|| invalid("The server version is missing."))?;
            let version = &response["returnObj"];
            let parts = ["_Major", "_Minor", "_Build", "_Revision"]
                .into_iter()
                .map(|key| {
                    version[key]
                        .as_u64()
                        .map(|v| v.to_string())
                        .ok_or_else(|| invalid("The server base version is invalid."))
                })
                .collect::<Result<Vec<_>, _>>()?;
            read_page(&request, &parts.join("."), prefix)
        }
        "rows" => {
            let response = request
                .response
                .as_ref()
                .ok_or_else(|| invalid("The definition page is missing."))?;
            let data = &response["returnObj"];
            for table in TABLES {
                if !data[*table].is_array() {
                    return Err(invalid("The definition page omitted a required table."));
                }
            }
            let more = response["parameters"]["morePages"]
                .as_bool()
                .ok_or_else(|| invalid("The definition page omitted morePages."))?;
            let roots = data["DynamicQueryDesigner"].as_array().unwrap();
            if more && roots.is_empty() {
                return Err(invalid("Epicor query pagination did not advance."));
            }
            // Only roots the user's literal, case-sensitive prefix keeps are validated.
            // A root without a string QueryID may match, so it is kept and fails below.
            // Native ownership can ignore QueryID case; the prefix is never rewritten.
            let selected_roots: Vec<_> = roots.iter()
                .filter(|root| root["QueryID"].as_str().is_none_or(|id| id.starts_with(prefix)))
                .collect();
            for root in &selected_roots {
                let company = root["Company"]
                    .as_str()
                    .ok_or_else(|| invalid("A query has no company key."))?;
                if (!company.is_empty() && company != request.company)
                    || root["SystemFlag"] != false
                    || !root["Updatable"].is_boolean()
                {
                    return Err(invalid(
                        "The definition page contains a query outside this collection.",
                    ));
                }
                if root["QueryID"].as_str().is_none_or(str::is_empty)
                    || root["SysRowID"].as_str().is_none_or(str::is_empty)
                {
                    return Err(invalid("A query has an incomplete Epicor ID."));
                }
            }
            // A selected root must not share its identity with any root on the page,
            // or native ownership would merge another query's children into it.
            let mut root_ids = BTreeMap::<(&str, String), usize>::new();
            for root in roots {
                if let (Some(company), Some(query)) = (root["Company"].as_str(), root["QueryID"].as_str()) {
                    *root_ids.entry((company, query.to_lowercase())).or_default() += 1;
                }
            }
            for root in &selected_roots {
                if root_ids[&(root["Company"].as_str().unwrap(), root["QueryID"].as_str().unwrap().to_lowercase())] > 1 {
                    return Err(invalid("The definition page repeats a query identity."));
                }
            }
            // A child row no root on this page owns cannot be proven unselected, so it
            // still fails. Unselected roots themselves are not validated.
            for (table, rows) in data.as_object().unwrap() {
                for row in rows
                    .as_array()
                    .ok_or_else(|| invalid("A definition table is not an array."))?
                {
                    if table != "DynamicQueryDesigner" && !roots
                        .iter()
                        .any(|root| crate::baq_archive::owned_by(row, root))
                    {
                        return Err(invalid("A definition row has no parent on this page."));
                    }
                }
            }
            let selected_data: serde_json::Map<String, Value> = data.as_object().unwrap().iter()
                .map(|(table, rows)| (table.clone(), Value::Array(rows.as_array().unwrap().iter()
                    .filter(|row| selected_roots.iter().any(|root| crate::baq_archive::owned_by(row, root)))
                    .cloned().collect())))
                .collect();
            let mut state = request.state.clone();
            state["data"] = Value::Object(selected_data);
            state["more"] = json!(more);
            state["diagrams"] = json!([]);
            state["diagramPage"] = json!(1);
            state["extensionMap"] = json!({});
            state["extensionInfo"] = json!({});
            if selected_roots.is_empty() {
                return finish(&state);
            }
            if state["data"]["QueryTableDesigner"]
                .as_array()
                .unwrap()
                .iter()
                .any(crate::baq_dependencies::extension_table)
            {
                return Ok(shared(
                    SERVICE,
                    "GetTableList",
                    json!({}),
                    state,
                    "extensionTables",
                ));
            }
            diagram_action(&state, request.page_size)
        }
        "extensionTables" => {
            let rows = request
                .response
                .as_ref()
                .and_then(|r| r["returnObj"]["FullTableList"].as_array())
                .ok_or_else(|| invalid("The extension table lookup omitted its rows."))?;
            let mut state = request.state.clone();
            state["extensionMap"] = crate::baq_dependencies::table_map(rows);
            let ids = crate::baq_dependencies::ids(&state["data"], &state["extensionMap"]);
            state["extensionIds"] = json!(ids);
            if ids.is_empty() {
                return diagram_action(&state, request.page_size);
            }
            Ok(shared(
                "Ice.Lib.BOReaderSvc",
                "GetCodeDescList",
                json!({"tableName":"Extension","columnName":"Level"}),
                state,
                "extensionLevels",
            ))
        }
        "extensionLevels" => {
            let codes = request
                .response
                .as_ref()
                .and_then(|r| r["returnObj"].as_str())
                .ok_or_else(|| invalid("The extension level descriptions are missing."))?;
            let mut state = request.state.clone();
            state["extensionLevels"] = json!(codes);
            extension_info_action(state)
        }
        "extensionInfo" => {
            let response = request
                .response
                .as_ref()
                .ok_or_else(|| invalid("The extension lookup response is missing."))?;
            if response["parameters"]["morePages"] == true {
                return Err(invalid("The extension lookup reported more pages."));
            }
            let rows = response["returnObj"]["Extension"]
                .as_array()
                .ok_or_else(|| invalid("The extension lookup omitted its rows."))?;
            let ids = extension_ids(&request.state)?;
            let levels = request.state["extensionLevels"]
                .as_str()
                .ok_or_else(|| invalid("The extension level descriptions are missing."))?;
            let mut found = BTreeMap::new();
            for row in rows {
                let id = row["ExtensionSetID"]
                    .as_str()
                    .and_then(|actual| ids.iter().find(|id| id.eq_ignore_ascii_case(actual)))
                    .ok_or_else(|| invalid("The extension lookup returned an extension that was not requested."))?;
                if found.insert(id, crate::baq_dependencies::info(row, levels)).is_some() {
                    return Err(invalid(&format!("The extension lookup returned extension {id} more than once.")));
                }
            }
            let mut state = request.state.clone();
            for id in &ids {
                let info = found.remove(id).ok_or_else(|| invalid(&format!(
                    "The extension lookup did not return extension {id}. It may have been deleted, or this account may not be allowed to read it.")))?;
                state["extensionInfo"][id] = info;
            }
            diagram_action(&state, request.page_size)
        }
        "diagrams" => {
            let rows = request
                .response
                .as_ref()
                .and_then(|r| r["rows"].as_array())
                .ok_or_else(|| invalid("The diagram query omitted its rows."))?;
            let roots = request.state["data"]["DynamicQueryDesigner"]
                .as_array()
                .unwrap();
            if rows.iter().any(|row| {
                !roots
                    .iter()
                    .any(|root| crate::baq_archive::owned_by(row, root))
            }) {
                return Err(invalid("A diagram row has no parent on this page."));
            }
            let mut state = request.state.clone();
            state["diagrams"]
                .as_array_mut()
                .ok_or_else(|| invalid("The diagram saved progress is invalid."))?
                .extend(rows.iter().cloned());
            if request
                .page_size
                .is_some_and(|size| rows.len() == size as usize)
            {
                let after = rows
                    .last()
                    .and_then(|row| row["SysRowID"].as_str())
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| invalid("The diagram page has no resume point key."))?;
                if state["diagramAfter"] == after {
                    return Err(invalid("Diagram pagination did not advance."));
                }
                state["diagramAfter"] = json!(after);
                return diagram_action(&state, request.page_size);
            }
            if state["data"]["DynamicQueryDesigner"].as_array().unwrap().iter()
                .any(|root| root["Updatable"] == true) {
                state["bpm"] = json!({});
                state["bpmPage"] = json!(1);
                Ok(crate::ubaq_rows::next(state, request.page_size))
            } else { finish(&state) }
        }
        "bpmRows" => {
            let response = request.response.as_ref().ok_or_else(|| invalid("The update-method response is missing."))?;
            let mut state = request.state.clone();
            match crate::ubaq_rows::append(&mut state, response) {
                Ok(true) => Ok(crate::ubaq_rows::next(state, request.page_size)),
                Ok(false) => {
                    finish(&state)
                },
                Err(error) => finish_without_methods(state, error),
            }
        }
        _ => Err(invalid("The local BAQ saved progress is invalid.")),
    }
}

// A failed UBAQ supplement must not discard ordinary queries already received.
// Do not export any UBAQ from an incomplete method dataset.
fn finish_without_methods(mut state: Value, error: Diagnostic) -> Result<GuestReply, Diagnostic> {
    state["bpmError"] = json!(error);
    finish(&state)
}

fn diagram_action(state: &Value, page_size: Option<u32>) -> Result<GuestReply, Diagnostic> {
    let roots = state["data"]["DynamicQueryDesigner"]
        .as_array()
        .ok_or_else(|| invalid("The query page is missing."))?;
    let fields = [
        "Company",
        "QueryID",
        "SubQueryID",
        "Seq",
        "Data",
        "SysRevID",
        "SysRowID",
        "IsKinetic",
        "DiagramVersion",
        "LastUpdated",
    ];
    let mut query = QuerySpec {
        tables: vec![
            QueryTable {
                schema: "Ice".into(),
                table: "QueryDiagram".into(),
                alias: "Diagram".into(),
                company_field: None,
                metadata: vec![],
            },
            QueryTable {
                schema: "Ice".into(),
                table: "QueryHdr".into(),
                alias: "Header".into(),
                company_field: None,
                metadata: vec![],
            },
        ],
        fields: fields
            .into_iter()
            .map(|field| QueryField {
                table: "Diagram".into(),
                field: field.into(),
                alias: field.into(),
                expression: None,
            })
            .collect(),
        joins: ["Company", "QueryID"]
            .into_iter()
            .map(|field| QueryJoin {
                left_table: "Diagram".into(),
                left_field: field.into(),
                right_table: "Header".into(),
                right_field: field.into(),
                kind: "Inner".into(),
            })
            .collect(),
        criteria: vec![QueryCriterion {
            table: "Header".into(),
            field: "SysRowID".into(),
            operator: "=".into(),
            parameter: "SelectedQueries".into(),
        }],
        parameters: BTreeMap::from([(
            "SelectedQueries".into(),
            json!(
                roots
                    .iter()
                    .map(|r| r["SysRowID"].clone())
                    .collect::<Vec<_>>()
            ),
        )]),
        order: vec![QueryOrder {
            field: "SysRowID".into(),
            descending: false,
        }],
        page_size,
        page: 1,
    };
    if let Some(after) = state["diagramAfter"].as_str() {
        query.criteria.push(QueryCriterion {
            table: "Diagram".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "AfterDiagram".into(),
        });
        query.parameters.insert("AfterDiagram".into(), json!(after));
    }
    let mut state = state.clone();
    state["phase"] = json!("diagrams");
    state["_progress"] = json!({
        "request":format!("Downloading designer layouts for {} queries in this batch", roots.len()),
        "response":"Designer layouts received; building BAQ files locally"
    });
    Ok(GuestReply::Action {
        action: HostAction::Query { query },
        state,
    })
}

fn filename(query: &str) -> String {
    // Percent-encode path separators and Windows-reserved filename characters.
    // The host independently validates the final relative name.
    let mut name = String::new();
    for byte in query.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            name.push(byte as char);
        } else {
            name.push_str(&format!("%{byte:02X}"));
        }
    }
    // Windows reserves these names even when a filename has an extension.
    // Encode the first byte so that the mapping stays distinct from literal '%'.
    let upper = name.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && matches!(upper.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        name = format!("%{:02X}{}", name.as_bytes()[0], &name[1..]);
    }
    format!("{name}.baq")
}

fn finish(state: &Value) -> Result<GuestReply, Diagnostic> {
    let reply = finish_definitions(state)?;
    #[cfg(feature="ubaq-compiled")]
    return crate::ubaq_compiled::prepare(reply, state);
    #[cfg(not(feature="ubaq-compiled"))]
    Ok(reply)
}

// Native ownership uses exact Company and ASCII-insensitive QueryID.
fn owner_index(rows: &[Value]) -> BTreeMap<(String, String), Vec<&Value>> {
    let mut index = BTreeMap::<(String, String), Vec<&Value>>::new();
    for row in rows {
        if let (Some(company), Some(query)) = (row["Company"].as_str(), row["QueryID"].as_str()) {
            index.entry((company.into(), query.to_ascii_lowercase())).or_default().push(row);
        }
    }
    index
}
fn finish_definitions(state: &Value) -> Result<GuestReply, Diagnostic> {
    if state["_hostPreparePage"] == true {
        return Ok(GuestReply::PreparedPage {
            state: state.clone(),
            next_state: json!({"phase":"nextPage","version":state["version"]}),
            next_cursor: next_page(state),
        });
    }
    let data = state["data"]
        .as_object()
        .ok_or_else(|| invalid("The query data is missing."))?;
    let roots = data["DynamicQueryDesigner"].as_array().unwrap();
    let tables: BTreeMap<_, _> = data.iter().map(|(name, rows)|
        (name, rows.as_array().map(|rows| owner_index(rows)))).collect();
    let diagrams = owner_index(state["diagrams"].as_array().unwrap());
    let mut records = Vec::new();
    let mut identities = Vec::new();
    let mut diagnostics = Vec::new();
    for root in roots {
        let company = root["Company"].as_str().unwrap();
        let query = root["QueryID"].as_str().unwrap();
        let identity = Identity {
            category: "baqs-and-ubaqs".into(),
            kind: if root["Updatable"] == true { "UBAQ" } else { "BAQ" }.into(),
            scope: if company.is_empty() {
                Scope::Global
            } else {
                Scope::Company {
                    company: company.into(),
                }
            },
            keys: BTreeMap::from([
                ("Company".into(), company.into()),
                ("QueryID".into(), query.into()),
            ]),
        };
        identities.push(identity.clone());
        let result = (|| -> Result<Value, Diagnostic> {
            let key = (company.to_owned(), query.to_ascii_lowercase());
            let mut definition = serde_json::Map::new();
            for (table, index) in &tables {
                let index = index.as_ref().ok_or_else(|| invalid("A definition table is invalid."))?;
                definition.insert((*table).clone(), json!(index.get(&key).cloned().unwrap_or_default()));
            }
            let mut diagrams: Vec<Value> = diagrams.get(&key).into_iter().flatten().map(|row| (*row).clone()).collect();
            for row in &mut diagrams {
                row["RowMod"] = json!("");
            }
            let diagram_data = json!({"QueryDiagram":diagrams});
            let definition = Value::Object(definition);
            let dependencies = if root["ExtQuery"] == true {
                Vec::new()
            } else {
                crate::baq_dependencies::xml(
                    &definition,
                    &state["extensionMap"],
                    &state["extensionInfo"],
                )?
            };
            let bpm = if root["Updatable"] == true {
                if !state["bpmError"].is_null() {
                    return Err(serde_json::from_value(state["bpmError"].clone())
                        .map_err(|_| invalid("The update-method failure saved progress is invalid."))?);
                }
                Some(crate::ubaq_rows::select(&state["bpm"], company, query)?)
            } else { None };
            let package = crate::baq_archive::build_complete(
                &definition,
                &diagram_data,
                state["version"].as_str().unwrap(),
                if root["ExtQuery"] == true {
                    None
                } else {
                    Some(&dependencies)
                },
                bpm.as_ref(),
            )?;
            let mut output = json!({"service":SERVICE,"definition":definition,
                "supplements":{"diagrams":diagram_data},"herbertDirectOutput":true,
                "herbertSources":{"binaryFiles":[{"name":filename(query),"bytes":package.len(),
                    "sha256":format!("{:x}",Sha256::digest(&package)),
                    "base64":base64::engine::general_purpose::STANDARD.encode(&package)}]}});
            if let Some(bpm) = bpm {
                output["supplements"]["updateMethods"] = bpm.clone();
                for mut diagnostic in crate::bpm_code::attach(&mut output, &bpm) {
                    diagnostic.identity = Some(identity.clone());
                    diagnostics.push(diagnostic);
                }
            }
            Ok(output)
        })();
        match result {
            Ok(definition) => records.push((identity, definition)),
            Err(mut diagnostic) => {
                diagnostic.identity = Some(identity.clone());
                if diagnostic.code.starts_with("BAQ_DEPENDENCY_") {
                    // Keep readable owner evidence without advertising an incomplete BAQ package.
                    let key = (company.to_owned(), query.to_ascii_lowercase());
                    let definition: serde_json::Map<String, Value> = tables.iter().map(|(table, index)| {
                        ((*table).clone(), json!(index.as_ref().unwrap().get(&key).cloned().unwrap_or_default()))
                    }).collect();
                    records.push((identity.clone(), json!({"service":SERVICE,
                        "definition":definition,"herbertDirectOutput":true,
                        "herbertCollectionStatus":{"completeness":"partial","diagnostics":[diagnostic.clone()]}})));
                }
                diagnostic.identity = Some(identity);
                diagnostics.push(diagnostic);
            }
        }
    }
    Ok(GuestReply::InventoryData {
        inventory: Box::new(GuestReply::Inventory {
            identities,
            next_cursor: next_page(state),
            authoritative: diagnostics.is_empty(),
            diagnostics,
        }),
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_index_matches_native_filter_with_unknown_and_unrelated_rows() {
        let rows = vec![json!({"Company":"C","QueryID":"b","Future":[3]}),
            json!({"Company":"C","QueryID":"A","Future":{"x":"é"}}),
            json!({"Company":"c","QueryID":"a"}), json!({"Company":"C","QueryID":"a","Order":2}),
            json!({"Company":"C"}), json!({"Company":null,"QueryID":"a"})];
        let index = owner_index(&rows);
        for root in [json!({"Company":"C","QueryID":"a"}), json!({"Company":"C","QueryID":"B"}),
            json!({"Company":"c","QueryID":"A"}), json!({"Company":"","QueryID":"missing"})] {
            let expected: Vec<_> = rows.iter().filter(|row| crate::baq_archive::owned_by(row, &root)).collect();
            assert_eq!(index.get(&(root["Company"].as_str().unwrap().into(),root["QueryID"].as_str().unwrap().to_ascii_lowercase())).cloned().unwrap_or_default(), expected);
        }
    }

    #[test]
    fn prepared_page_defers_export_building_until_materialization() {
        let state = json!({"data":data(),"diagrams":[],"version":"5.2.100.0",
            "more":true,"page":1,"extensionMap":{},"extensionInfo":{},"_hostPreparePage":true});
        let GuestReply::PreparedPage { state, next_cursor, .. } = finish(&state).unwrap() else { panic!() };
        assert_eq!(next_cursor.as_deref(), Some("2"));
        assert!(state.get("herbertSources").is_none());
        let mut request = request("directOnly");
        request.operation = "materialize".into();
        request.state = state;
        let GuestReply::InventoryData { records, inventory } = crate::handle("baqs-and-ubaqs", request) else { panic!() };
        assert_eq!(records.len(), 2);
        assert!(matches!(*inventory, GuestReply::Inventory { authoritative:true, .. }));
    }

    #[test]
    fn filenames_preserve_distinct_names_and_avoid_windows_devices() {
        assert_eq!(filename("CON"), "%43ON.baq");
        assert_eq!(filename("com1"), "%63om1.baq");
        assert_eq!(filename("%43ON"), "%2543ON.baq");
        assert_eq!(filename("LPT10"), "LPT10.baq");
        assert_eq!(filename("../A"), "%2E%2E%2FA.baq");
    }

    fn request(mode: &str) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "baqs-and-ubaqs".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(500),
            state: json!({"_collectionMethod":mode,"_pluginSettings":{"queriesPerPage":2}}),
            response: None,
            error: None,
        }
    }

    fn data() -> Value {
        let mut data = json!({});
        for table in TABLES {
            data[*table] = json!([]);
        }
        data["DynamicQueryDesigner"] = json!([
            {"Company":"C","QueryID":"First","SysRowID":"one","Updatable":false,"SystemFlag":false,"IsProtected":false},
            {"Company":"C","QueryID":"Second","SysRowID":"two","Updatable":false,"SystemFlag":false,"IsProtected":false}
        ]);
        data["QueryFieldDesigner"] = json!([
            {"Company":"C","QueryID":"First","FieldName":"A"},
            {"Company":"C","QueryID":"Second","FieldName":"B"}
        ]);
        data
    }

    fn respond(request: &mut GuestRequest, state: Value, response: Value) {
        request.state = state;
        request.state["_pluginSettings"] = json!({"queriesPerPage":2});
        request.response = Some(response);
    }

    fn prefix_page(prefix: Value, data: Value, more: bool) -> GuestRequest {
        let mut r = request("directOnly");
        // As read_page records it: whether the prefix narrowed the root read.
        let narrowed = prefix.as_str().and_then(like_stem).is_some();
        r.state = json!({"phase":"rows","version":"5.2.100.0","page":1,"narrowed":narrowed,
            "_pluginSettings":{"queriesPerPage":2,"queryIdPrefix":prefix}});
        r.response = Some(json!({"returnObj":data,"parameters":{"morePages":more}}));
        r
    }

    #[test]
    fn prefix_selects_literal_roots_and_complete_native_owned_children() {
        let keys = ["First", "FIRSTOther", "firstLower", " First ", "F%_'", "F_", "Ωé", "Ωe\u{301}"];
        let mut source = data();
        source["DynamicQueryDesigner"] = json!(keys.iter().enumerate().map(|(index, key)| json!({
            "Company":"C","QueryID":key,"SysRowID":format!("root-{index}"),"Updatable":false,"SystemFlag":false,"IsProtected":false,"Extra":"original Ω"
        })).collect::<Vec<_>>());
        source["QueryFieldDesigner"] = json!(keys.iter().flat_map(|key| ["A", "B"].map(|field| json!({
            "Company":"C","QueryID":key.to_ascii_lowercase(),"FieldName":field,"Extra":"complete native child"
        }))).collect::<Vec<_>>());
        source["FutureTable"] = source["QueryFieldDesigner"].clone();
        for (prefix, expected) in [("", keys.to_vec()), ("F", vec!["First", "FIRSTOther", "F%_'", "F_"]),
            ("f", vec!["firstLower"]), (" First ", vec![" First "]), ("F%_'", vec!["F%_'"]),
            ("F_", vec!["F_"]), ("Ωé", vec!["Ωé"]), ("Ωe\u{301}", vec!["Ωe\u{301}"]), ("missing", vec![])] {
            let reply = handle(prefix_page(json!(prefix), source.clone(), false));
            if expected.is_empty() {
                let GuestReply::InventoryData {records, inventory} = reply else {panic!("{prefix:?}")};
                assert!(records.is_empty());
                assert!(matches!(*inventory, GuestReply::Inventory {authoritative:true, next_cursor:None, ..}));
                continue;
            }
            let GuestReply::Action {action:HostAction::Query {query}, state} = reply else {panic!("{prefix:?}")};
            let selected: Vec<_> = source["DynamicQueryDesigner"].as_array().unwrap().iter()
                .filter(|root| expected.contains(&root["QueryID"].as_str().unwrap())).collect();
            assert_eq!(query.parameters["SelectedQueries"], json!(selected.iter().map(|root| &root["SysRowID"]).collect::<Vec<_>>()));
            for (table, rows) in source.as_object().unwrap() {
                let expected: Vec<_> = rows.as_array().unwrap().iter()
                    .filter(|row| selected.iter().any(|root| crate::baq_archive::owned_by(row, root))).collect();
                assert_eq!(state["data"][table], json!(expected), "{prefix:?} {table}");
            }
            assert_eq!(state["data"]["QueryFieldDesigner"].as_array().unwrap().len(), selected.len()*2);
        }
        for state in [json!({"phase":"rows","version":"5.2.100.0","page":1}),
            json!({"phase":"rows","version":"5.2.100.0","page":1,"_pluginSettings":{"queriesPerPage":2},"_pluginState":{"config":{"queryIdPrefix":"missing"}}})] {
            let mut r = prefix_page(json!(""), source.clone(), false);r.state = state;
            let GuestReply::Action {state, ..} = handle(r) else {panic!()};
            assert_eq!(state["data"], source, "Unset frozen prefix cannot inherit a saved filter");
        }
    }

    #[test]
    fn prefix_rejects_invalid_settings_and_validates_only_selected_raw_rows() {
        for value in [Value::Null, json!(false), json!(1), json!([]), json!({})] {
            assert!(matches!(handle(prefix_page(value, data(), false)), GuestReply::Failure {diagnostic} if diagnostic.code=="INVALID_SETTING"));
        }
        let mut cases = Vec::new();
        let mut value = data();value["DynamicQueryDesigner"][0]["Company"] = json!("Foreign");cases.push(value);
        let mut value = data();value["DynamicQueryDesigner"][0]["Updatable"] = json!("no");cases.push(value);
        let mut value = data();value["DynamicQueryDesigner"][1]["QueryID"] = Value::Null;cases.push(value);
        let mut value = data();value["QueryFieldDesigner"][1]["QueryID"] = json!("orphan");cases.push(value);
        let mut value = data();value.as_object_mut().unwrap().remove("QueryFieldDesigner");cases.push(value);
        let mut value = data();value["FutureTable"] = json!({});cases.push(value);
        let mut value = data();let duplicate = value["DynamicQueryDesigner"][0].clone();value["DynamicQueryDesigner"].as_array_mut().unwrap().push(duplicate);cases.push(value);
        // An unselected case variant would merge its children into the selected query.
        let mut value = data();value["DynamicQueryDesigner"][1]["QueryID"] = json!("FIRST");value["QueryFieldDesigner"][1]["QueryID"] = json!("FIRST");cases.push(value);
        for value in cases {
            assert!(matches!(handle(prefix_page(json!("First"), value, false)), GuestReply::Failure {diagnostic} if diagnostic.code=="BAQ_PAGE_INVALID"));
        }
        // Unselected roots are no longer validated and produce no failure or diagnostic.
        let mut accepted = Vec::new();
        let mut value = data();value["DynamicQueryDesigner"][1]["Company"] = json!("Foreign");value["QueryFieldDesigner"][1]["Company"] = json!("Foreign");accepted.push(value);
        for (field, bad) in [("SystemFlag", json!(true)), ("Updatable", Value::Null), ("SysRowID", json!(""))] {
            let mut value = data();value["DynamicQueryDesigner"][1][field] = bad;accepted.push(value);
        }
        let mut value = data();let duplicate = value["DynamicQueryDesigner"][1].clone();value["DynamicQueryDesigner"].as_array_mut().unwrap().push(duplicate);accepted.push(value);
        for value in accepted {
            let GuestReply::Action {action:HostAction::Query {query}, state} = handle(prefix_page(json!("First"), value.clone(), false)) else {panic!("{value}")};
            assert_eq!(query.parameters["SelectedQueries"], json!(["one"]));
            assert_eq!(state["data"]["DynamicQueryDesigner"], json!([data()["DynamicQueryDesigner"][0]]));
            assert_eq!(state["data"]["QueryFieldDesigner"].as_array().unwrap().len(), 1);
        }
        let mut r = prefix_page(json!("First"), data(), false);r.operation = "acquire".into();
        assert!(matches!(handle(r), GuestReply::Failure {..}), "Prefix does not add unsupported exact acquisition");
    }

    #[test]
    fn prefix_empty_selected_page_preserves_native_continuation_before_later_match() {
        for prepare in [false, true] {
            let mut source = data();
            for rows in source.as_object_mut().unwrap().values_mut() {
                rows.as_array_mut().unwrap().retain(|row| row["QueryID"] == "First");
            }
            let mut r = prefix_page(json!("Second"), source, true);
            r.state["_hostPreparePage"] = json!(prepare);
            let (next_state, next_cursor) = match handle(r.clone()) {
                GuestReply::PreparedPage {state, next_state, next_cursor} => {
                    assert!(state["data"]["DynamicQueryDesigner"].as_array().unwrap().is_empty());
                    r.operation = "materialize".into();r.state = state;r.response = None;
                    let GuestReply::InventoryData {records, inventory} = handle(r) else {panic!()};
                    assert!(records.is_empty());
                    assert!(matches!(*inventory, GuestReply::Inventory {next_cursor:Some(ref cursor), authoritative:true, ..} if cursor=="2:narrowed"));
                    (next_state, next_cursor)
                }
                GuestReply::InventoryData {records, inventory} => {
                    assert!(records.is_empty());
                    let GuestReply::Inventory {next_cursor, authoritative, ..} = *inventory else {panic!()};
                    assert!(authoritative);
                    (json!({"phase":"nextPage","version":"5.2.100.0"}), next_cursor)
                }
                _ => panic!("Empty selection must not request diagrams, extensions or compiled context"),
            };
            assert_eq!(next_cursor.as_deref(), Some("2:narrowed"));
            let mut r = request("directOnly");r.cursor = next_cursor;r.state = next_state;
            r.state["_pluginSettings"] = json!({"queriesPerPage":2,"queryIdPrefix":"Second"});
            let GuestReply::Action {action:HostAction::NativeRead {arguments, ..}, mut state} = handle(r.clone()) else {panic!()};
            assert_eq!(arguments["absolutePage"], 2);
            assert_eq!(arguments["whereClauseDynamicQueryDesigner"], "(Company = 'C' OR Company = '') AND SystemFlag = false AND QueryID LIKE 'Second%'");
            state["_pluginSettings"] = r.state["_pluginSettings"].clone();r.state = state;
            r.response = Some(json!({"returnObj":data(),"parameters":{"morePages":false}}));
            let GuestReply::Action {action:HostAction::Query {query}, state} = handle(r) else {panic!()};
            assert_eq!(query.parameters["SelectedQueries"], json!(["two"]));
            assert_eq!(state["data"]["DynamicQueryDesigner"][0]["QueryID"], "Second");
            assert_eq!(state["data"]["QueryFieldDesigner"].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn a_page_position_saved_before_the_pushdown_restarts_narrowed_discovery() {
        let read = |cursor: &str, prefix: &str| {
            let mut r = request("directOnly");r.cursor = Some(cursor.into());
            r.state = json!({"phase":"nextPage","version":"5.2.100.0","_pluginSettings":{"queriesPerPage":2,"queryIdPrefix":prefix}});
            handle(r)
        };
        let page = |reply: GuestReply| {
            let GuestReply::Action {action:HostAction::NativeRead {arguments, ..}, state} = reply else {panic!("{reply:?}")};
            (arguments["absolutePage"].as_u64().unwrap(), state["narrowed"].clone())
        };
        // An old-build position counts pages of the unfiltered list: restart, never skip.
        assert_eq!(page(read("3", "Second")), (1, json!(true)));
        assert_eq!(page(read("3:narrowed", "Second")), (3, json!(true)));
        // Without a stem nothing was narrowed, so old positions stay valid.
        for prefix in ["", "%x", "Ω"] {assert_eq!(page(read("3", prefix)), (3, json!(false)), "{prefix:?}");}
        assert!(matches!(read("3:narrowed", ""), GuestReply::Failure {..}));
        assert!(matches!(read("x", "Second"), GuestReply::Failure {..}));
        assert!(matches!(read("x:narrowed", "Second"), GuestReply::Failure {..}));
        // The next position carries the marker of the read that produced it.
        let state = json!({"more":true,"page":4,"narrowed":true});
        assert_eq!(next_page(&state).as_deref(), Some("5:narrowed"));
        assert_eq!(next_page(&json!({"more":true,"page":4})).as_deref(), Some("5"));
        assert_eq!(next_page(&json!({"more":false,"page":4,"narrowed":true})), None);
    }
    #[test]
    fn prefix_pushes_a_like_stem_superset_onto_the_root_read_only() {
        let base = "(Company = 'C' OR Company = '') AND SystemFlag = false";
        let read = |phase: &str, prefix: &str| {
            let mut r = request("directOnly");
            r.state = json!({"phase":phase,"version":"5.2.100.0","_pluginSettings":{"queriesPerPage":2,"queryIdPrefix":prefix}});
            r.response = Some(json!({"returnObj":{"_Major":5,"_Minor":2,"_Build":100,"_Revision":0}}));
            let GuestReply::Action {action:HostAction::NativeRead {arguments, ..}, ..} = handle(r) else {panic!("{phase} {prefix:?}")};
            for table in TABLES.iter().filter(|table| **table != "DynamicQueryDesigner") {
                assert_eq!(arguments[format!("whereClause{table}")], "", "Child clauses stay blank");
            }
            arguments["whereClauseDynamicQueryDesigner"].as_str().unwrap().to_owned()
        };
        for phase in ["nextPage", "version"] {
            assert_eq!(read(phase, "Ab'c"), format!("{base} AND QueryID LIKE 'Ab''c%'"));
            assert_eq!(read(phase, "Q_1"), format!("{base} AND QueryID LIKE 'Q%'"));
            // Non-ASCII ends the stem; a stem of only a space still narrows.
            assert_eq!(read(phase, " Ω["), format!("{base} AND QueryID LIKE ' %'"));
            for prefix in ["", "%x", "_", "[a", "]", "Ωé"] {assert_eq!(read(phase, prefix), base, "{prefix:?}");}
        }
        // A missing frozen setting adds nothing.
        let mut r = request("directOnly");r.state = json!({"phase":"nextPage","version":"5.2.100.0"});
        let GuestReply::Action {action:HostAction::NativeRead {arguments, ..}, ..} = handle(r) else {panic!()};
        assert_eq!(arguments["whereClauseDynamicQueryDesigner"], base);
    }

    fn package_members(record: &Value) -> BTreeMap<String, Vec<u8>> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(
            record["herbertSources"]["binaryFiles"][0]["base64"].as_str().unwrap()).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut result = BTreeMap::new();
        for index in 0..archive.len() {
            let mut file = archive.by_index(index).unwrap();let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
            result.insert(file.name().to_owned(), bytes);
        }
        result
    }

    #[test]
    fn prefix_selected_ordinary_package_matches_the_unfiltered_complete_record() {
        let baseline = json!({"data":data(),"diagrams":[
            {"Company":"C","QueryID":"First","SysRowID":"first-diagram","Data":"original Ω","RowMod":""},
            {"Company":"C","QueryID":"Second","SysRowID":"second-diagram","Data":"unselected","RowMod":""}],
            "version":"5.2.100.0","more":false,"page":1,"extensionMap":{},"extensionInfo":{}});
        let GuestReply::InventoryData {records:all, ..} = finish_definitions(&baseline).unwrap() else {panic!()};
        let GuestReply::Action {mut state, ..} = handle(prefix_page(json!("First"), baseline["data"].clone(), false)) else {panic!()};
        state["diagrams"] = json!([baseline["diagrams"][0]]);
        let GuestReply::InventoryData {records:selected, ..} = finish_definitions(&state).unwrap() else {panic!()};
        assert_eq!(selected.len(), 1);assert_eq!(selected[0].0.kind, "BAQ");
        let original = all.iter().find(|(id, _)| id.keys["QueryID"] == "First").unwrap();
        assert_eq!(selected[0].1["definition"], original.1["definition"]);
        assert_eq!(selected[0].1["supplements"], original.1["supplements"]);
        assert_eq!(package_members(&selected[0].1), package_members(&original.1));
    }

    fn prefix_closure_state(kind: &str) -> Value {
        let mut source = data();
        for (index, root) in source["DynamicQueryDesigner"].as_array_mut().unwrap().iter_mut().enumerate() {
            root["Updatable"] = json!(true);root["SysRevID"] = json!(index+1);
        }
        source["QueryTableDesigner"] = json!([
            {"Company":"C","QueryID":"first","TableType":kind,"DBSchemaName":"Ice","DBTableName":"Example"},
            {"Company":"C","QueryID":"Second","TableType":"DB","DBSchemaName":"Ice","DBTableName":"UD01"}]);
        let body = r#"<DirectiveDefinition2><DirectiveStep DisplayName="Execute"><CustomCodeAction Code="var value = 1;"/></DirectiveStep></DirectiveDefinition2>"#;
        json!({"data":source,"version":"5.2.100.0","page":1,"more":false,
            "diagrams":[{"Company":"C","QueryID":"FIRST","SysRowID":"diagram-first","Data":"classic and kinetic Ω","RowMod":""},
                {"Company":"C","QueryID":"Second","SysRowID":"diagram-second","Data":"unselected","RowMod":""}],
            "extensionMap":{"ice.example":"RequiredDependencyOutsidePrefix"},
            "extensionInfo":{"RequiredDependencyOutsidePrefix":{"ExtensionSetId":"RequiredDependencyOutsidePrefix","Level":"Customization","Product":"P","Provider":"V","Version":"1"}},
            "bpm":{"BpMethod":[
                {"Source":"DQ","BpMethodCode":"display-does-not-name-owner","BusinessObject":"C/First","Name":"GetList","SysRowID":"method-first","SysRevID":1},
                {"Source":"DQ","BpMethodCode":"second-method","BusinessObject":"C/Second","Name":"GetList","SysRowID":"method-second","SysRevID":2}],
                "BpArgument":[
                    {"Source":"DQ","BpMethodCode":"display-does-not-name-owner","SysRowID":"arg-first","SysRevID":1,"TypeInfo":"{\"company\":\"C\",\"query\":\"First\"}"},
                    {"Source":"DQ","BpMethodCode":"second-method","SysRowID":"arg-second","SysRevID":2,"TypeInfo":"{\"company\":\"C\",\"query\":\"Second\"}"}],
                "BpDirective":[
                    {"Source":"DQ","BpMethodCode":"display-does-not-name-owner","Name":"First directive","IsProtected":false,"Body":body},
                    {"Source":"DQ","BpMethodCode":"second-method","Name":"Second directive","IsProtected":false,"Body":body}]}})
    }

    #[test]
    fn prefix_retains_selected_diagrams_xt_pt_dependencies_and_typeinfo_workflows() {
        for kind in ["XT", "PT"] {
            let baseline = prefix_closure_state(kind);
            let GuestReply::InventoryData {records:all, ..} = finish_definitions(&baseline).unwrap() else {panic!()};
            assert_eq!(all.len(), 2);
            let GuestReply::Action {action:HostAction::SharedNativeRead {method, ..}, mut state} = handle(
                prefix_page(json!("First"), baseline["data"].clone(), false)) else {panic!()};
            assert_eq!(method, "GetTableList");
            assert_eq!(state["data"]["DynamicQueryDesigner"].as_array().unwrap().len(), 1);
            assert_eq!(state["data"]["QueryTableDesigner"][0]["QueryID"], "first", "Preserve original native child key case");
            for key in ["extensionMap", "extensionInfo", "bpm"] {state[key] = baseline[key].clone();}
            state["diagrams"] = json!([baseline["diagrams"][0]]);
            let GuestReply::InventoryData {records:selected, inventory} = finish_definitions(&state).unwrap() else {panic!()};
            assert_eq!(selected.len(), 1);assert_eq!(selected[0].0.kind, "UBAQ");
            assert!(matches!(*inventory, GuestReply::Inventory {authoritative:true, ..}));
            let original = all.iter().find(|(id, _)| id.keys["QueryID"] == "First").unwrap();
            assert_eq!(selected[0].1["definition"], original.1["definition"]);
            assert_eq!(selected[0].1["supplements"], original.1["supplements"]);
            let members = package_members(&selected[0].1);
            assert_eq!(members, package_members(&original.1), "Every selected native package member remains equivalent");
            assert!(String::from_utf8(members["QueryDependencies"].clone()).unwrap().contains("RequiredDependencyOutsidePrefix"));
            let methods = &selected[0].1["supplements"]["updateMethods"];
            assert_eq!(methods["BpMethod"].as_array().unwrap().len(), 1);
            assert_eq!(methods["BpMethod"][0]["BpMethodCode"], "display-does-not-name-owner");
            assert_eq!(methods["BpDirective"].as_array().unwrap().len(), 1);
            assert!(members.contains_key("BpMethodDataset"));
        }
        let mut source = data();
        source["QueryTableDesigner"] = json!([{"Company":"C","QueryID":"Second","TableType":"XT","DBSchemaName":"Ice","DBTableName":"UnselectedMissingMap"}]);
        assert!(matches!(handle(prefix_page(json!("First"), source, false)), GuestReply::Action {action:HostAction::Query {..}, ..}),
            "Unselected extension tables do not trigger dependency lookups");
    }

    #[cfg(feature="ubaq-compiled")]
    #[test]
    fn prefix_optional_compiled_queue_contains_only_selected_ubaq_owners() {
        let baseline = prefix_closure_state("XT");
        let GuestReply::Action {mut state, ..} = handle(prefix_page(json!("First"), baseline["data"].clone(), false)) else {panic!()};
        for key in ["extensionMap", "extensionInfo", "bpm"] {state[key] = baseline[key].clone();}
        state["diagrams"] = json!([baseline["diagrams"][0]]);
        let GuestReply::InventoryData {records:ordinary, ..} = finish_definitions(&state).unwrap() else {panic!()};
        state["_pluginSettings"] = json!({"queriesPerPage":2,"queryIdPrefix":"First","downloadCompiledAssemblies":true,"decompileCompiledAssemblies":true});
        state["_hostCapabilities"] = json!({"localDecompilationV1":{"protocolVersion":1,"retainedArtifactFunctions":true,"engineId":MANAGED_ENGINE_ID}});
        let GuestReply::ReadyRecords {records, state} = finish(&state).unwrap() else {panic!("Selected ordinary package must be saved first")};
        assert_eq!(records.len(), 1);assert_eq!(records[0].1["herbertSources"]["binaryFiles"], ordinary[0].1["herbertSources"]["binaryFiles"]);
        let owners = state["_ubaqCompiledOwners"].as_array().unwrap();assert_eq!(owners.len(), 1);
        assert_eq!(owners[0]["owner"]["keys"]["QueryID"], "First");
        assert!(!serde_json::to_string(owners).unwrap().contains("method-second"));
        assert!(!serde_json::to_string(owners).unwrap().contains("arg-second"));
        assert!(state["data"].is_null());assert!(state["bpm"].is_null());
    }

    #[test]
    fn public_route_builds_each_query_from_bulk_reads_without_a_helper() {
        for mode in ["directOnly", "serverAssisted"] {
            let mut request = request(mode);
            let GuestReply::Action {
                action: HostAction::SharedNativeRead { method, .. },
                state,
            } = crate::handle("baqs-and-ubaqs", request.clone())
            else {
                panic!()
            };
            assert_eq!(method, "GetBaseLevel");
            respond(
                &mut request,
                state,
                json!({"returnObj":{"_Major":5,"_Minor":2,"_Build":100,"_Revision":0}}),
            );
            let GuestReply::Action {
                action:
                    HostAction::NativeRead {
                        method,
                        arguments,
                        export,
                        ..
                    },
                state,
            } = crate::handle("baqs-and-ubaqs", request.clone())
            else {
                panic!()
            };
            assert_eq!(method, "GetRows");
            assert!(!export);
            assert_eq!(arguments["pageSize"], 2);
            assert!(
                TABLES
                    .iter()
                    .all(|table| arguments.get(format!("whereClause{table}")).is_some())
            );
            respond(
                &mut request,
                state,
                json!({"returnObj":data(),"parameters":{"morePages":true}}),
            );
            let GuestReply::Action {
                action: HostAction::Query { query },
                state,
            } = crate::handle("baqs-and-ubaqs", request.clone())
            else {
                panic!()
            };
            assert_eq!(query.parameters["SelectedQueries"], json!(["one", "two"]));
            assert_eq!(query.joins.len(), 2);
            respond(
                &mut request,
                state,
                json!({"rows":[
                    {"Company":"C","QueryID":"First","SysRowID":"diagram-one","Data":"first"},
                    {"Company":"C","QueryID":"Second","SysRowID":"diagram-two","Data":"second"}
                ]}),
            );
            let GuestReply::Action {
                action: HostAction::Query { query },
                state,
            } = crate::handle("baqs-and-ubaqs", request.clone())
            else {
                panic!()
            };
            assert_eq!(query.parameters["AfterDiagram"], "diagram-two");
            respond(&mut request, state, json!({"rows":[]}));
            let GuestReply::InventoryData { records, inventory } = crate::handle("baqs-and-ubaqs", request)
            else {
                panic!()
            };
            assert_eq!(records.len(), 2);
            assert!(
                matches!(*inventory,GuestReply::Inventory {next_cursor:Some(ref cursor),authoritative:true,..} if cursor=="2")
            );
            for (identity, definition) in records {
                assert_eq!(
                    definition["definition"]["QueryFieldDesigner"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
                assert_eq!(
                    definition["definition"]["QueryFieldDesigner"][0]["QueryID"],
                    identity.keys["QueryID"]
                );
                let encoded = definition["herbertSources"]["binaryFiles"][0]["base64"]
                    .as_str()
                    .unwrap();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap();
                let archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
                assert_eq!(archive.len(), 4);
            }
        }
    }

    #[test]
    fn a_protected_query_does_not_suppress_its_valid_peer() {
        let mut data = data();
        data["DynamicQueryDesigner"][0]["IsProtected"] = json!(true);
        let state = json!({"data":data,"diagrams":[],"version":"5.2.100.0","more":false,"page":1,"extensionMap":{},"extensionInfo":{}});
        let GuestReply::InventoryData { records, inventory } = finish(&state).unwrap() else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0.keys["QueryID"], "Second");
        let GuestReply::Inventory {
            identities,
            diagnostics,
            authoritative,
            ..
        } = *inventory
        else {
            panic!()
        };
        assert_eq!(identities.len(), 2);
        assert!(!authoritative);
        assert_eq!(
            diagnostics[0].identity.as_ref().unwrap().keys["QueryID"],
            "First"
        );
    }

    #[test]
    fn failed_method_read_preserves_ordinary_peer_and_marks_only_ubaq_incomplete() {
        let mut data = data();
        data["DynamicQueryDesigner"][0]["Updatable"] = json!(true);
        let mut request = request("directOnly");
        request.state = json!({"phase":"bpmRows","data":data,"diagrams":[],
            "version":"5.2.100.0","more":true,"page":1,"extensionMap":{},"extensionInfo":{}});
        request.error = Some(Diagnostic::new("ACCESS_DENIED", "Update methods are unavailable."));
        let GuestReply::InventoryData { records, inventory } = handle(request) else { panic!() };
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0.keys["QueryID"], "Second");
        let GuestReply::Inventory { identities, diagnostics, authoritative, next_cursor } = *inventory else { panic!() };
        assert_eq!(identities.len(), 2);
        assert!(!authoritative);
        assert_eq!(next_cursor.as_deref(), Some("2"));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "ACCESS_DENIED");
        assert_eq!(diagnostics[0].identity.as_ref().unwrap().kind, "UBAQ");
    }

    #[test]
    fn ubaq_without_methods_still_produces_a_complete_package() {
        let mut data = data();
        data["DynamicQueryDesigner"][0]["Updatable"] = json!(true);
        let state = json!({"data":data,"diagrams":[],"version":"5.2.100.0",
            "more":false,"page":1,"extensionMap":{},"extensionInfo":{},
            "bpm":{"BpMethod":[],"BpArgument":[],"BpDirective":[]}});
        let GuestReply::InventoryData { records, inventory } = finish(&state).unwrap() else { panic!() };
        let GuestReply::Inventory { authoritative, diagnostics, .. } = *inventory else { panic!() };
        assert!(authoritative);
        assert!(diagnostics.is_empty());
        assert_eq!(records.len(), 2);
        let bytes = base64::engine::general_purpose::STANDARD.decode(
            records[0].1["herbertSources"]["binaryFiles"][0]["base64"].as_str().unwrap()).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(archive.by_name("DynamicQuery").is_ok());
        let mut methods = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("BpMethodDataset").unwrap(), &mut methods).unwrap();
        assert!(!methods.contains("<BpMethod>"));
        assert!(!methods.contains("<BpDirective>"));
    }

    #[test]
    fn ubaq_preserves_workflow_and_readable_code_beside_native_package() {
        let mut data = data();
        data["DynamicQueryDesigner"][0]["Updatable"] = json!(true);
        let body = r#"<DirectiveDefinition2><DirectiveStep DisplayName="Execute"><CustomCodeAction Code="var x = 1;"/></DirectiveStep></DirectiveDefinition2>"#;
        let state = json!({"data":data,"diagrams":[],"version":"5.2.100.0",
            "more":false,"page":1,"extensionMap":{},"extensionInfo":{},
            "bpm":{"BpMethod":[{"Source":"DQ","BpMethodCode":"Ice.C/First.GetList"}],
                "BpArgument":[{"Source":"DQ","BpMethodCode":"Ice.C/First.GetList",
                    "TypeInfo":"{\"company\":\"C\",\"query\":\"First\"}"}],
                "BpDirective":[{"Source":"DQ","BpMethodCode":"Ice.C/First.GetList",
                    "Name":"Test","IsProtected":false,"Body":body}]}});
        let GuestReply::InventoryData { records, inventory } = finish(&state).unwrap() else { panic!() };
        assert!(matches!(*inventory, GuestReply::Inventory { authoritative:true, .. }));
        assert_eq!(records.len(), 2);
        let output = &records[0].1;
        assert_eq!(output["supplements"]["updateMethods"]["BpDirective"][0]["Body"], body);
        assert_eq!(output["herbertSources"]["files"][0]["text"], "var x = 1;");
        let bytes = base64::engine::general_purpose::STANDARD.decode(
            output["herbertSources"]["binaryFiles"][0]["base64"].as_str().unwrap()).unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(archive.by_name("BpMethodDataset").is_ok());
    }
    #[cfg(feature="ubaq-compiled")]
    #[test]
    fn compiled_capture_failure_follows_one_normal_baq_record_handoff(){
        let mut data=data();data["DynamicQueryDesigner"][0]["Updatable"]=json!(true);data["DynamicQueryDesigner"][0]["SysRevID"]=json!(1);
        let code="Ice.C/First.GetList";
        let native=json!({"BpMethod":[{"Source":"DQ","BpMethodCode":code,"BusinessObject":"C/First","Name":"GetList","SysRowID":"method-id","SysRevID":2}],"BpArgument":[{"Source":"DQ","BpMethodCode":code,"SysRowID":"argument-id","SysRevID":3,"TypeInfo":"{\"company\":\"C\",\"query\":\"First\"}"}],"BpDirective":[]});
        let base=json!({"data":data,"diagrams":[],"version":"5.2.100.0","more":false,"page":1,"extensionMap":{},"extensionInfo":{},"bpm":native});
        let mut r=request("serverAssisted");r.operation="materialize".into();r.state=base.clone();
        let GuestReply::InventoryData{records:off,..}=handle(r.clone())else{panic!()};
        r.state["_pluginSettings"]=json!({"downloadCompiledAssemblies":true,"decompileCompiledAssemblies":true});r.state["_hostCapabilities"]=json!({"localDecompilationV1":{"protocolVersion":1,"retainedArtifactFunctions":true,"engineId":MANAGED_ENGINE_ID}});
        let GuestReply::ReadyRecords{records,state}=handle(r.clone())else{panic!("normal files must be handed off first")};
        assert_eq!(records.len(),2);assert_eq!(records[0].1["herbertSources"]["binaryFiles"],off[0].1["herbertSources"]["binaryFiles"]);assert_eq!(records[1].1,off[1].1);assert!(state["data"].is_null());assert!(state["bpm"].is_null());assert_eq!(state["_ubaqCompiledOwners"].as_array().unwrap().len(),1);
        r.state=state;let GuestReply::Action{action:HostAction::RetainedLocalArtifactFunctionCall{..},state}=handle(r.clone())else{panic!()};
        r.state=state;r.error=Some(Diagnostic::new("OPTIONAL_READER_FAILED","The optional reader is unavailable."));
        let GuestReply::Inventory{identities,authoritative,diagnostics,..}=handle(r)else{panic!("no duplicate normal records")};assert!(identities.is_empty());assert!(authoritative);assert_eq!(diagnostics.len(),1);assert_eq!(diagnostics[0].identity.as_ref(),Some(&records[0].0));assert_eq!(diagnostics[0].context.get("processingIncomplete"),Some(&json!(true)));
    }


    #[test]
    fn wave1_unresolved_extension_keeps_readable_owner_and_complete_peer() {
        for kind in ["XT","PT"] {
            let mut data=data();
            data["QueryTableDesigner"]=json!([{"Company":"C","QueryID":"First","TableType":kind,"DBSchemaName":"Ice","DBTableName":"Missing"}]);
            let state=json!({"data":data,"diagrams":[],"version":"5.2.100.0","more":false,"page":1,"extensionMap":{},"extensionInfo":{}});
            let GuestReply::InventoryData {records,inventory}=finish(&state).unwrap() else {panic!()};
            assert_eq!(records.len(),2);
            let failed=&records.iter().find(|(id,_)|id.keys["QueryID"]=="First").unwrap().1;
            assert!(failed["definition"]["QueryTableDesigner"].is_array());assert!(failed.get("herbertSources").is_none());
            let peer=&records.iter().find(|(id,_)|id.keys["QueryID"]=="Second").unwrap().1;
            assert!(peer["herbertSources"]["binaryFiles"].is_array());
            assert_eq!(failed["herbertCollectionStatus"]["completeness"],"partial");
            assert_eq!(failed["herbertCollectionStatus"]["diagnostics"][0]["identity"]["keys"]["QueryID"],"First");
            assert!(peer.get("herbertCollectionStatus").is_none());
            let GuestReply::Inventory{authoritative,diagnostics,..}=*inventory else {panic!()};
            assert!(!authoritative);assert_eq!(diagnostics.len(),1);
            assert_eq!(diagnostics[0].code,"BAQ_DEPENDENCY_UNRESOLVED");
            assert_eq!(diagnostics[0].identity.as_ref().unwrap().keys["QueryID"],"First");
        }
    }

    #[test]
    fn extension_metadata_is_one_batched_read_and_a_missing_extension_is_named() {
        let mut source = data();
        source["QueryTableDesigner"] = json!([
            {"Company":"C","QueryID":"First","TableType":"XT","DBSchemaName":"Ice","DBTableName":"First_XT"},
            {"Company":"C","QueryID":"Second","TableType":"PT","DBSchemaName":"Ice","DBTableName":"Second_XT"}]);
        let mut r = request("directOnly");
        respond(&mut r, json!({"phase":"extensionTables","data":source,"diagrams":[],"version":"5.2.100.0","page":1,"more":false,"extensionMap":{},"extensionInfo":{}}),
            json!({"returnObj":{"FullTableList":[
                {"TableType":"XT","FullTableName":"Ice.First_XT","Description":"First\u{1}SetA"},
                {"TableType":"PT","FullTableName":"Ice.Second_XT","Description":"Second\u{1}O'Set"}]}}));
        let GuestReply::Action {action:HostAction::SharedNativeRead {method, ..}, state} = handle(r.clone()) else {panic!()};
        assert_eq!(method, "GetCodeDescList");
        respond(&mut r, state, json!({"returnObj":"1`Customization~2`Vertical"}));
        let GuestReply::Action {action:HostAction::SharedNativeRead {service, method, arguments, ..}, state} = handle(r.clone()) else {panic!()};
        assert_eq!((service.as_str(), method.as_str()), ("Ice.BO.ExtensionSvc", "GetRows"));
        assert!(methods().contains(&(service, method)));
        assert_eq!(arguments["whereClauseExtension"], "ExtensionSetID = 'O''Set' OR ExtensionSetID = 'SetA'");
        let rows = [json!({"ExtensionSetID":"seta","Level":1,"Product":"P","Provider":"V","Version":"1"}),
            json!({"ExtensionSetID":"O'Set","Level":2,"Product":"Q","Provider":null,"Version":"2"})];
        let mut complete = r.clone();
        respond(&mut complete, state.clone(), json!({"returnObj":{"Extension":rows},"parameters":{"morePages":false}}));
        let GuestReply::Action {action:HostAction::Query {..}, state:done} = handle(complete) else {panic!("All metadata arrives in one read")};
        assert_eq!(done["extensionInfo"]["SetA"], crate::baq_dependencies::info(&rows[0], "1`Customization~2`Vertical"));
        assert_eq!(done["extensionInfo"]["O'Set"]["Level"], "Vertical");
        respond(&mut r, state, json!({"returnObj":{"Extension":[rows[0]]},"parameters":{"morePages":false}}));
        let GuestReply::Failure {diagnostic} = handle(r) else {panic!()};
        assert_eq!(diagnostic.code, "BAQ_PAGE_INVALID");
        assert!(diagnostic.message.contains("did not return extension O'Set"), "{}", diagnostic.message);
    }

}
