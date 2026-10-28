//! Collect scoped Kinetic records in bulk and build packages on this computer.
//! Metadata selects exact rows; it never substitutes for complete body reads.
use crate::kinetic_archive::{KEYS, TYPES, invalid, view};
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn methods() -> Vec<(String, String)> {
    vec![
        (
            "Ice.Lib.SessionModSvc".into(),
            "GetCurrentValuesFromServer".into(),
        ),
        ("Ice.Lib.UxAppVersionSvc".into(), "GetUxAppVersions".into()),
        ("Ice.BO.GenXDataSvc".into(), "GetRows".into()),
    ]
}
pub fn handle(r: GuestRequest) -> GuestReply {
    collect(r).unwrap_or_else(|diagnostic| GuestReply::Failure { diagnostic })
}
fn application_prefix(r: &GuestRequest) -> Result<String, Diagnostic> {
    match r.state.pointer("/_pluginSettings/applicationIdPrefix") {
        None => Ok(String::new()),
        Some(Value::String(value)) => Ok(value.clone()),
        Some(_) => Err(Diagnostic::new("INVALID_SETTING", "Application ID starts with must be a string.")),
    }
}
fn with_settings(mut state: Value, from: &Value) -> Value {
    if let Some(settings) = from.get("_pluginSettings") {state["_pluginSettings"] = settings.clone();}
    state
}
fn size(r: &GuestRequest) -> Result<u32, Diagnostic> {
    r.page_size
        .or(Some(100))
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid("The Kinetic page size must be positive."))
}
fn query(
    r: &GuestRequest,
    mut state: Value,
    table: &str,
    filters: Vec<(&str, Value)>,
    metadata: bool,
    after: Value,
) -> Result<GuestReply, Diagnostic> {
    let stage = match state["phase"].as_str().unwrap_or("") {
        "bodies" => Some(format!(
            "Downloading saved files and drafts ({} of {} received)",
            state["rows"].as_array().map_or(0, Vec::len),
            state["wanted"].as_array().map_or(0, Vec::len)
        )),
        "resourceMetadata" => Some("Checking referenced Kinetic resources".into()),
        "resources" => Some("Downloading referenced Kinetic files".into()),
        "chunks" => Some("Collecting associated saved data".into()),
        _ => None,
    };
    if let Some(stage) = stage {
        state["_progress"] = json!({"request":stage,"response":"Kinetic data received; preparing packages and draft recovery"});
    }
    let alias = "Source";
    let names = if table == "UXAppVersion" {
        vec![
            "Company",
            "ProductID",
            "TypeCode",
            "CGCCode",
            "Key1",
            "Key2",
            "Key3",
            "SysRowID",
            "Version",
        ]
    } else {
        vec![
            "Company",
            "ProductID",
            "TypeCode",
            "CGCCode",
            "Key1",
            "Key2",
            "Key3",
            "SysRowID",
            "SystemFlag",
        ]
    };
    let mut fields = if metadata {
        names
            .into_iter()
            .map(|name| QueryField {
                table: alias.into(),
                field: name.into(),
                alias: name.into(),
                expression: None,
            })
            .collect::<Vec<_>>()
    } else {
        vec![QueryField {
            table: alias.into(),
            field: "*".into(),
            alias: "*".into(),
            expression: None,
        }]
    };
    if !metadata {
        let lengths = if table == "XXXDef" {
            vec!["Content", "SysCharacter01", "SysCharacter03"]
        } else if table == "XXXChunk" {
            vec!["Chunk"]
        } else {
            vec!["Content"]
        };
        for field in lengths {
            fields.push(QueryField {
                table: alias.into(),
                field: field.into(),
                alias: format!("Herbert{field}Bytes"),
                expression: Some(format!(
                    "CONVERT(nvarchar(max), DATALENGTH(CONVERT(nvarchar(max), Source.{field})))"
                )),
            });
        }
    }
    let mut parameters = BTreeMap::new();
    let mut criteria = Vec::new();
    for (index, (field, value)) in filters.into_iter().enumerate() {
        let parameter = format!("p{index}");
        parameters.insert(parameter.clone(), value);
        criteria.push(QueryCriterion {
            table: if field == "RootSysRowID" {
                "Root"
            } else {
                alias
            }
            .into(),
            field: if field == "RootSysRowID" {
                "SysRowID"
            } else {
                field
            }
            .into(),
            operator: "=".into(),
            parameter,
        });
    }
    if !after.is_null() {
        parameters.insert("after".into(), after.clone());
        criteria.push(QueryCriterion {
            table: alias.into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    let read_size = if metadata {
        r.state
            .pointer("/_pluginSettings/metadataRowsPerPage")
            .or_else(|| r.state.pointer("/_pluginState/config/metadataRowsPerPage"))
            .and_then(Value::as_u64)
            .unwrap_or(1000)
    } else {
        size(r)? as u64
    };
    if read_size == 0 || read_size > u32::MAX as u64 {
        return Err(invalid(
            "The identity page size must be a positive native page size.",
        ));
    }
    state["readPageSize"] = json!(read_size);
    state["after"] = after;
    let mut tables = vec![QueryTable {
        schema: "Ice".into(),
        table: table.into(),
        alias: alias.into(),
        company_field: None,
        metadata: vec![],
    }];
    let mut joins = Vec::new();
    if table == "XXXChunk" {
        tables.push(QueryTable {
            schema: "Ice".into(),
            table: "XXXDef".into(),
            alias: "Root".into(),
            company_field: None,
            metadata: vec![],
        });
        joins = KEYS
            .iter()
            .map(|field| QueryJoin {
                left_table: alias.into(),
                left_field: (*field).into(),
                right_table: "Root".into(),
                right_field: (*field).into(),
                kind: "Inner".into(),
            })
            .collect();
    }
    Ok(GuestReply::Action {
        action: HostAction::Query {
            query: QuerySpec {
                tables,
                fields,
                joins,
                criteria,
                parameters,
                order: vec![QueryOrder {
                    field: "SysRowID".into(),
                    descending: false,
                }],
                page_size: Some(read_size as u32),
                page: 1,
            },
        },
        state,
    })
}
fn page(r: &GuestRequest) -> Result<Vec<Value>, Diagnostic> {
    let rows = r
        .response
        .as_ref()
        .and_then(|v| v["rows"].as_array())
        .ok_or_else(|| invalid("The Kinetic response omitted its rows."))?;
    if rows.len() > r.state["readPageSize"].as_u64().unwrap_or(size(r)? as u64) as usize {
        return Err(invalid(
            "The Kinetic response exceeded its requested page size.",
        ));
    }
    let mut seen = BTreeSet::new();
    for row in rows {
        let id = row["SysRowID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("A Kinetic row has no physical identity."))?;
        if !seen.insert(id) || r.state["after"] == id {
            return Err(invalid(
                "The Kinetic page repeats a row or does not advance.",
            ));
        }
    }
    Ok(rows.clone())
}
fn collect(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if !matches!(r.operation.as_str(), "discover" | "acquire" | "materialize") {
        return Err(invalid(
            "Select Kinetic discovery or an exact application bundle.",
        ));
    }
    if r.operation != "acquire" {application_prefix(&r)?;}
    size(&r)?;
    if let Some(error) = r.error.take() {
        return Err(error);
    }
    if r.operation == "materialize" || r.state["phase"] == "processing" {
        return process(r);
    }
    match r.state["phase"].as_str().unwrap_or("initial") {
        "initial" => Ok(GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.SessionModSvc".into(),
                method: "GetCurrentValuesFromServer".into(),
                arguments: json!({}),
                export: false,
            },
            state: with_settings(json!({"phase":"scope","_progress":{"request":"Reading Kinetic company and tenant scope","response":"Scope received; finding saved applications and drafts"}}), &r.state),
        }),
        "scope" => {
            let p = &r
                .response
                .as_ref()
                .ok_or_else(|| invalid("Session scope is missing."))?["parameters"];
            if p["companyID"] != r.company {
                return Err(invalid(
                    "The session company does not match the requested company.",
                ));
            }
            let tenant = p["tenantID"]
                .as_str()
                .ok_or_else(|| invalid("The session tenant scope is missing."))?;
            let mut scopes = vec![r.company.clone(), String::new()];
            if !tenant.is_empty() {
                scopes.push(format!("T_{tenant}"));
            }
            scopes.sort();
            scopes.dedup();
            r.state = with_settings(json!({"phase":"inventory","scopes":scopes,"tenantCompany":if tenant.is_empty(){Value::Null}else{json!(format!("T_{tenant}"))},"metadata":[],"_progress":{"request":"Finding saved applications, layers and drafts","response":"Kinetic identities received; checking the next inventory page"}}), &r.state);
            inventory(&r, Value::Null)
        }
        "inventory" => {
            let rows = page(&r)?;
            for row in &rows {
                if !r.state["scopes"]
                    .as_array()
                    .unwrap()
                    .contains(&row["Company"])
                    || !TYPES.contains(&row["TypeCode"].as_str().unwrap_or(""))
                    || row["SystemFlag"].as_bool().is_none()
                    || KEYS.iter().any(|k| row[*k].as_str().is_none())
                {
                    return Err(invalid(
                        "A Kinetic inventory row is outside its requested scope or has an incomplete identity.",
                    ));
                }
            }
            r.state["metadata"]
                .as_array_mut()
                .unwrap()
                .extend(rows.clone());
            if rows.len() == r.state["readPageSize"].as_u64().unwrap_or(size(&r)? as u64) as usize {
                return inventory(&r, rows.last().unwrap()["SysRowID"].clone());
            }
            begin_batch(r)
        }
        "nextPage" => begin_batch(r),
        "bodies" => {
            let rows = page(&r)?;
            let wanted = r.state["wanted"].as_array().unwrap();
            if rows
                .iter()
                .any(|row| !wanted.contains(&row["SysRowID"]) || row["SystemFlag"] != false)
            {
                return Err(invalid(
                    "A body read returned an unrequested or system definition.",
                ));
            }
            r.state["rows"].as_array_mut().unwrap().extend(rows.clone());
            let offset = r.state["bodyOffset"].as_u64().unwrap_or(0) as usize;
            let expected =
                (r.state["wanted"].as_array().unwrap().len() - offset).min(size(&r)? as usize);
            if rows.len() != expected {
                return Err(invalid(
                    "Definitions changed or disappeared during collection. Restart this batch.",
                ));
            }
            if offset + expected < r.state["wanted"].as_array().unwrap().len() {
                r.state["bodyOffset"] = json!(offset + expected);
                return bodies(&r, Value::Null);
            }
            let found: BTreeSet<_> = r.state["rows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["SysRowID"].as_str().unwrap_or(""))
                .collect();
            if found.len() != r.state["wanted"].as_array().unwrap().len() {
                return Err(invalid(
                    "Definitions changed or disappeared during collection. Restart this batch to obtain a consistent package.",
                ));
            }
            parents(r)
        }
        "parents" => {
            let more_pages = r.response.as_ref().and_then(|v| v.pointer("/parameters/morePages"))
                .and_then(Value::as_bool).ok_or_else(|| invalid("The native parent response omitted its paging status."))?;
            // Native GenXData reads retain tenant-layer rows that a company BAQ
            // can omit. Its string fields are native full values, not BAQ text
            // projections; record their UTF-16 sizes for the shared writer.
            let native = r.response.as_ref().and_then(|v| v.pointer("/returnObj/XXXDef"))
                .and_then(Value::as_array).ok_or_else(|| invalid("The native parent-layer response omitted its rows."))?;
            let mut native = native.clone();
            for row in &mut native {
                for field in ["Content", "SysCharacter01", "SysCharacter03"] {
                    let text = row[field].as_str().ok_or_else(|| invalid("A native parent layer omitted its full text."))?;
                    row[format!("Herbert{field}Bytes")] = json!(text.encode_utf16().count() * 2);
                }
            }
            r.response = Some(json!({"rows":native}));
            let rows = page(&r)?;
            for row in &rows {
                if row["TypeCode"] != "KNTCCustLayer"
                    || KEYS.iter().any(|k| row[*k].as_str().is_none())
                    || !r.state["scopes"]
                        .as_array()
                        .unwrap()
                        .contains(&row["Company"])
                {
                    return Err(invalid("A parent-layer read returned an out-of-scope row."));
                }
                if r.state["parentPageSeen"].as_array().is_some_and(|seen| seen.contains(&row["SysRowID"])) {
                    return Err(invalid("The native parent pages repeat a physical row."));
                }
                r.state["parentPageSeen"].as_array_mut().ok_or_else(|| invalid("The native parent paging state is missing."))?.push(row["SysRowID"].clone());
                let matches = r.state["parentLookup"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == row["Key1"] && p["view"] == row["Key2"]);
                if matches
                    && !r.state["rows"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|p| p["SysRowID"] == row["SysRowID"])
                {
                    r.state["rows"].as_array_mut().unwrap().push(row.clone());
                    r.state["wanted"]
                        .as_array_mut()
                        .unwrap()
                        .push(row["SysRowID"].clone());
                }
            }
            if more_pages {
                if rows.is_empty() { return Err(invalid("The native parent page made no progress.")); }
                return parent_page(&r, json!(r.state["parentPage"].as_u64().unwrap_or(1) + 1));
            }
            let looked = r.state["parentLookup"].clone();
            r.state["parentChecked"]
                .as_array_mut()
                .unwrap()
                .extend(looked.as_array().unwrap().clone());
            parents(r)
        }
        "resources" => {
            let rows = page(&r)?;
            if rows.iter().any(|v| {
                v["SystemFlag"] != false
                    || !r.state["resourceWanted"]
                        .as_array()
                        .unwrap()
                        .contains(&v["SysRowID"])
            }) {
                return Err(invalid("A resource read returned an unrequested record."));
            }
            r.state["resources"]
                .as_array_mut()
                .unwrap()
                .extend(rows.clone());
            if rows.len() == r.state["readPageSize"].as_u64().unwrap_or(size(&r)? as u64) as usize {
                return resource_bodies(&r, rows.last().unwrap()["SysRowID"].clone());
            }
            if r.state["resourceWanted"]
                .as_array()
                .unwrap()
                .iter()
                .any(|id| {
                    !r.state["resources"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v["SysRowID"] == *id)
                })
            {
                return Err(invalid(
                    "A referenced resource disappeared during collection.",
                ));
            }
            dependencies(r)
        }
        "resourceMetadata" => {
            let rows = page(&r)?;
            r.state["metadata"]
                .as_array_mut()
                .unwrap()
                .extend(rows.clone());
            if rows.len() == r.state["readPageSize"].as_u64().unwrap_or(size(&r)? as u64) as usize {
                return resource_metadata(&r, rows.last().unwrap()["SysRowID"].clone());
            }
            let lookup = r.state["resourceLookup"].as_array().unwrap().clone();
            r.state["resourceLookupDone"]
                .as_array_mut()
                .unwrap()
                .extend(lookup);
            r.state["resourceScanned"] = json!([]);
            r.state["references"] = json!([]);
            r.state["errors"] = json!([]);
            dependencies(r)
        }
        "chunks" => {
            let rows = page(&r)?;
            for row in &rows {
                if !r.state["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| KEYS.iter().all(|k| s[*k] == row[*k]))
                {
                    return Err(invalid(
                        "A Kinetic chunk is outside the selected definitions.",
                    ));
                }
                crate::kinetic_archive::check_text(row, "Chunk")?;
            }
            r.state["chunks"]
                .as_array_mut()
                .unwrap()
                .extend(rows.clone());
            if rows.len() == r.state["readPageSize"].as_u64().unwrap_or(size(&r)? as u64) as usize {
                return chunks(&r, rows.last().unwrap()["SysRowID"].clone());
            }
            prepare(r)
        }
        "nativeHistory" => {
            let rows = r
                .response
                .as_ref()
                .and_then(|v| v["returnObj"].as_array())
                .ok_or_else(|| invalid("The native version response is missing."))?;
            for row in rows {
                if r.state["scopes"]
                    .as_array()
                    .unwrap()
                    .contains(&row["Company"])
                    && r.state["views"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| history_view(row) == v.as_str())
                {
                    if row["Content"].as_str().is_none() {
                        return Err(invalid("A retained version omitted its content."));
                    }
                    if !r.state["versions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v["SysRowID"] == row["SysRowID"])
                    {
                        r.state["versions"]
                            .as_array_mut()
                            .unwrap()
                            .push(row.clone());
                    }
                }
            }
            r.state["historyIndex"] = json!(r.state["historyIndex"].as_u64().unwrap_or(0) + 1);
            native_history(r)
        }
        "helperHistory" => {
            let response = r
                .response
                .as_ref()
                .ok_or_else(|| invalid("The history helper returned no response."))?;
            if response["oSuccess"] != true {
                return Err(invalid(
                    response["oMessage"]
                        .as_str()
                        .unwrap_or("The history helper failed."),
                ));
            }
            let page: Value = response["oJson"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .ok_or_else(|| invalid("The history helper returned invalid JSON."))?;
            let offset = r.state["historyOffset"].as_u64().unwrap_or(0);
            let scanned = page["scanned"]
                .as_u64()
                .ok_or_else(|| invalid("The history page omitted its scanned count."))?;
            let next = page["nextOffset"]
                .as_u64()
                .ok_or_else(|| invalid("The history page omitted its cursor."))?;
            if page["offset"] != offset
                || scanned > size(&r)? as u64
                || next != offset + scanned
                || (page["more"] == true && scanned == 0)
                || page["more"].as_bool().is_none()
            {
                return Err(invalid("The history page did not advance correctly."));
            }
            for row in page["rows"]
                .as_array()
                .ok_or_else(|| invalid("The history page omitted its rows."))?
            {
                if !r.state["scopes"]
                    .as_array()
                    .unwrap()
                    .contains(&row["Company"])
                    || !r.state["views"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v.as_str() == history_view(row))
                    || row["Content"].as_str().is_none()
                    || row["SysRowID"].as_str().is_none()
                    || r.state["versions"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v["SysRowID"] == row["SysRowID"])
                {
                    return Err(invalid(
                        "The history helper returned an invalid, repeated, or out-of-scope row.",
                    ));
                }
                r.state["versions"]
                    .as_array_mut()
                    .unwrap()
                    .push(row.clone());
            }
            r.state["historyOffset"] = json!(next);
            if page["more"] == true {
                helper_history(r)
            } else {
                prepare(r)
            }
        }
        _ => Err(invalid(
            "The Kinetic checkpoint is invalid. Restart this collection.",
        )),
    }
}
fn inventory(r: &GuestRequest, after: Value) -> Result<GuestReply, Diagnostic> {
    query(
        r,
        r.state.clone(),
        "XXXDef",
        vec![
            ("Company", r.state["scopes"].clone()),
            ("TypeCode", json!(TYPES)),
            ("SystemFlag", json!(false)),
        ],
        true,
        after,
    )
}
fn identity(view: &str, company: &str) -> Identity {
    Identity {
        category: "C12".into(),
        kind: "KineticApplicationBundle".into(),
        scope: Scope::Company {
            company: company.into(),
        },
        keys: BTreeMap::from([
            ("ViewId".into(), view.into()),
            ("Company".into(), company.into()),
        ]),
    }
}
fn begin_batch(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let metadata = r.state["metadata"]
        .as_array()
        .cloned()
        .ok_or_else(|| invalid("The Kinetic inventory checkpoint is missing."))?;
    let prefix = if r.identity.is_some() {String::new()} else {application_prefix(&r)?};
    let all: BTreeSet<String> = metadata
        .iter()
        .filter(|v| v["SystemFlag"] == false)
        .filter(|row| authored_root(row))
        .filter_map(view)
        .filter(|view| view.starts_with(&prefix))
        .map(str::to_owned)
        .collect();
    let cursor: Value = r
        .cursor
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| invalid("The Kinetic cursor is invalid."))?
        .unwrap_or(json!({"version":1,"offset":0}));
    if cursor["version"] != 1 {
        return Err(invalid(
            "Restart discovery with the current Kinetic cursor.",
        ));
    }
    let offset = cursor["offset"]
        .as_u64()
        .ok_or_else(|| invalid("The Kinetic cursor offset is missing."))? as usize;
    let selected: Vec<_> = if let Some(id) = &r.identity {
        let view_id = id
            .keys
            .get("ViewId")
            .ok_or_else(|| invalid("Select an exact application bundle."))?;
        if *id != identity(view_id, &r.company) || !all.contains(view_id) {
            return Err(invalid(
                "The selected application bundle is not in this company inventory.",
            ));
        }
        vec![view_id.clone()]
    } else {
        all.iter()
            .skip(offset)
            .take(size(&r)? as usize)
            .cloned()
            .collect()
    };
    let wanted: Vec<_> = metadata
        .iter()
        .filter(|v| {
            v["SystemFlag"] == false
                && !generated_cache(v)
                && view(v).is_some_and(|s| selected.iter().any(|x| x == s))
        })
        .map(|v| v["SysRowID"].clone())
        .collect();
    let history_pass = cursor["history"] == true;
    let next = if r.identity.is_none() && offset + selected.len() < all.len() {
        Some(json!({"version":1,"offset":offset+selected.len(),"history":history_pass}).to_string())
    } else if r.identity.is_none() && !history_pass && include_history(&r) && !all.is_empty() {
        Some(json!({"version":1,"offset":0,"history":true}).to_string())
    } else {
        None
    };
    r.state = json!({"phase":"bodies","scopes":r.state["scopes"],"tenantCompany":r.state["tenantCompany"],"metadata":metadata,"views":selected,"wanted":wanted,"rows":[],"resources":[],"references":[],"resourceScanned":[],"resourceLookupDone":[],"errors":[],"chunks":[],"versions":[],"nextCursor":next,"targeted":r.identity.is_some(),"_hostPreparePage":r.state["_hostPreparePage"],"_pluginSettings":r.state["_pluginSettings"],"_collectionMethod":r.state["_collectionMethod"],"_progress":{"request":format!("Downloading {} Kinetic applications and their saved drafts",selected.len()),"response":"Kinetic definitions received; preparing current exports"}});
    r.state["historyPass"] = json!(history_pass);
    if history_pass {
        // Only small identity records cross native history calls. Current bodies
        // have already been written and must not be serialized on each request.
        r.state["rows"] = json!(
            metadata
                .iter()
                .filter(
                    |row| view(row).is_some_and(|name| selected.iter().any(|v| v == name))
                        && authored_root(row)
                )
                .cloned()
                .collect::<Vec<_>>()
        );
        return start_history(r);
    }
    if wanted.is_empty() {
        return prepare(r);
    }
    bodies(&r, Value::Null)
}
// The native content loader stores generated cache rows with SystemFlag=false.
// Native ExportApp selects Key3=''; cache rows must not create backup items.
fn generated_cache(row: &Value) -> bool {
    row["TypeCode"] == "KNTCMetaUI"
        && (row["Key3"]
            .as_str()
            .is_some_and(|v| v.eq_ignore_ascii_case("cache"))
            || row["Key2"]
                .as_str()
                .is_some_and(|v| v.replace('\\', "/").to_lowercase().contains("/cache/")))
}
fn parents(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    if !r.state["parentChecked"].is_array() {
        r.state["parentChecked"] = json!([]);
    }
    let mut missing = Vec::new();
    for row in r.state["rows"].as_array().unwrap() {
        if row["TypeCode"] != "KNTCCustLayer" {
            continue;
        }
        let text = row["SysCharacter01"].as_str().unwrap_or("");
        if text.is_empty() {
            continue;
        }
        let names: Vec<String> =
            serde_json::from_str(text).map_err(|_| invalid("The parent layer list is invalid."))?;
        for name in names {
            let key = json!({"name":name,"view":row["Key2"]});
            // Query exact parent references even when the parent is a shipped
            // layer. A dependency is not a new standalone backup item.
            if !r.state["parentChecked"].as_array().unwrap().contains(&key)
                && !missing.contains(&key)
            {
                missing.push(key);
            }
        }
    }
    if missing.is_empty() {
        return dependencies(r);
    }
    r.state["parentLookup"] = json!(missing);
    r.state["parentPageSeen"] = json!([]);
    r.state["phase"] = json!("parents");
    r.state["_progress"] = json!({"request":"Reading required parent layers","response":"Parent layers received; preparing complete packages"});
    parent_page(&r, Value::Null)
}
fn parent_page(r: &GuestRequest, after: Value) -> Result<GuestReply, Diagnostic> {
    let lookups = r.state["parentLookup"].as_array().unwrap();
    let names: BTreeSet<_> = lookups.iter().filter_map(|p| p["name"].as_str()).collect();
    let views: BTreeSet<_> = lookups.iter().filter_map(|p| p["view"].as_str()).collect();
    let quote = |v: &str| format!("'{}'", v.replace('\'', "''"));
    let group = |field: &str, values: Vec<&str>| -> Result<String, Diagnostic> {
        if values.is_empty() { return Err(invalid("A native parent lookup requires an explicit scope.")); }
        Ok(format!("({})", values.into_iter().map(|v| format!("{field} = {}", quote(v))).collect::<Vec<_>>().join(" OR ")))
    };
    let scopes = r.state["scopes"].as_array().ok_or_else(|| invalid("Parent scope is missing."))?;
    let filter = format!("{} AND TypeCode = 'KNTCCustLayer' AND {} AND {}",
        group("Company", scopes.iter().filter_map(Value::as_str).collect())?,
        group("Key1", names.into_iter().collect())?, group("Key2", views.into_iter().collect())?);
    let mut state = r.state.clone();
    let page = after.as_u64().unwrap_or(1);
    state["readPageSize"] = json!(size(r)?);
    state["after"] = Value::Null;
    state["parentPage"] = json!(page);
    Ok(GuestReply::Action { action: HostAction::NativeRead {
        service:"Ice.BO.GenXDataSvc".into(), method:"GetRows".into(),
        arguments:json!({"whereClauseXXXDef":filter,"whereClauseXXXChunk":"1 = 0","pageSize":size(r)?,"absolutePage":page}),export:false,
    }, state })
}
fn authored_root(row: &Value) -> bool {
    match row["TypeCode"].as_str() {
        Some("KNTCCustLayer" | "KNTCUDApp") => true,
        Some("KNTCMetaUI") => {
            !generated_cache(row)
                && row["Key3"] == ""
                && row["Key2"]
                    .as_str()
                    .is_some_and(|v| v.to_lowercase().ends_with(".jsonc"))
        }
        _ => false,
    }
}
fn bodies(r: &GuestRequest, after: Value) -> Result<GuestReply, Diagnostic> {
    // Bound the identity list by the requested page, not an arbitrary total cap.
    // This also avoids an extra empty request after every full body page.
    let offset = r.state["bodyOffset"].as_u64().unwrap_or(0) as usize;
    let ids: Vec<_> = r.state["wanted"]
        .as_array()
        .unwrap()
        .iter()
        .skip(offset)
        .take(size(r)? as usize)
        .cloned()
        .collect();
    query(
        r,
        r.state.clone(),
        "XXXDef",
        vec![("SysRowID", json!(ids)), ("SystemFlag", json!(false))],
        false,
        after,
    )
}
fn chunks(r: &GuestRequest, after: Value) -> Result<GuestReply, Diagnostic> {
    query(
        r,
        r.state.clone(),
        "XXXChunk",
        vec![("RootSysRowID", r.state["wanted"].clone())],
        false,
        after,
    )
}
fn resource_bodies(r: &GuestRequest, after: Value) -> Result<GuestReply, Diagnostic> {
    query(
        r,
        r.state.clone(),
        "XXXDef",
        vec![
            ("SysRowID", r.state["resourceWanted"].clone()),
            ("SystemFlag", json!(false)),
        ],
        false,
        after,
    )
}
fn resource_metadata(r: &GuestRequest, after: Value) -> Result<GuestReply, Diagnostic> {
    let paths: Vec<_> = r.state["resourceLookup"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|p| {
            let p = p.as_str().unwrap();
            [p.to_owned(), p.replace('\\', "/")]
        })
        .collect();
    query(
        r,
        r.state.clone(),
        "XXXDef",
        vec![
            ("Company", r.state["scopes"].clone()),
            ("TypeCode", json!("KNTCMetaUI")),
            ("Key2", json!(paths)),
            ("Key3", json!("")),
            ("CGCCode", json!("")),
        ],
        true,
        after,
    )
}
fn dependencies(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let mut wanted = BTreeSet::new();
    let mut lookup = BTreeSet::new();
    let known: Vec<Value> = r.state["rows"]
        .as_array()
        .unwrap()
        .iter()
        .chain(r.state["resources"].as_array().unwrap())
        .cloned()
        .collect();
    for row in &known {
        if row["TypeCode"] != "KNTCMetaUI"
            || r.state["resourceScanned"]
                .as_array()
                .unwrap()
                .contains(&row["SysRowID"])
        {
            continue;
        }
        r.state["resourceScanned"]
            .as_array_mut()
            .unwrap()
            .push(row["SysRowID"].clone());
        crate::kinetic_archive::check_text(row, "Content")?;
        let text = row["Content"].as_str().unwrap();
        if !row["Key2"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .ends_with(".jsonc")
        {
            continue;
        }
        let parsed = match crate::kinetic_resources::jsonc(text) {
            Ok(v) => v,
            Err(error) => {
                r.state["errors"].as_array_mut().unwrap().push(json!(error));
                continue;
            }
        };
        let mut edges = Vec::new();
        let mut errors = Vec::new();
        crate::kinetic_resources::scan(
            &parsed,
            row["Key2"].as_str().unwrap(),
            &mut edges,
            &mut errors,
        );
        r.state["errors"]
            .as_array_mut()
            .unwrap()
            .extend(errors.into_iter().map(|mut e| {
                e.identity = Some(identity(row["Key1"].as_str().unwrap_or(""), &r.company));
                json!(e)
            }));
        for (reference, path) in edges {
            let candidates: Vec<_> = r.state["metadata"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| {
                    m["TypeCode"] == "KNTCMetaUI"
                        && m["Key3"] == ""
                        && m["CGCCode"] == ""
                        && m["Key2"]
                            .as_str()
                            .is_some_and(|p| p.replace('/', "\\").eq_ignore_ascii_case(&path))
                        && (m["Company"] == ""
                            || m["Company"].as_str().is_some_and(|c| c.starts_with("T_")))
                })
                .cloned()
                .collect();
            let global: Vec<_> = candidates
                .iter()
                .cloned()
                .filter(|m| m["Company"] == "")
                .collect();
            let selected = if global.is_empty() {
                candidates
            } else {
                global
            };
            let mut edge = json!({"sourceView":view(row),"sourcePath":row["Key2"],"reference":reference,"targetPath":path});
            if selected.is_empty()
                && !r.state["resourceLookupDone"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(path))
            {
                lookup.insert(path.clone());
            } else if selected.len() != 1 {
                r.state["errors"].as_array_mut().unwrap().push(json!(invalid(format!("Referenced document {path} is missing or ambiguous. Its reference is retained."))));
            } else {
                let target = &selected[0];
                edge["targetSysRowID"] = target["SysRowID"].clone();
                edge["excludedSystem"] = json!(target["SystemFlag"] == true);
                if target["SystemFlag"] == false
                    && !known.iter().any(|v| v["SysRowID"] == target["SysRowID"])
                {
                    wanted.insert(target["SysRowID"].as_str().unwrap().to_owned());
                }
            }
            r.state["references"].as_array_mut().unwrap().push(edge);
        }
    }
    if !lookup.is_empty() {
        r.state["phase"] = json!("resourceMetadata");
        r.state["resourceLookup"] = json!(lookup);
        resource_metadata(&r, Value::Null)
    } else if wanted.is_empty() {
        r.state["phase"] = json!("chunks");
        chunks(&r, Value::Null)
    } else {
        r.state["phase"] = json!("resources");
        r.state["resourceWanted"] = json!(wanted);
        resource_bodies(&r, Value::Null)
    }
}
fn history_view(row: &Value) -> Option<&str> {
    if row["TypeCode"] == "KNTCMetaUI" {
        row["Key1"].as_str()
    } else {
        row["Key2"].as_str()
    }
}
// UXAppVersion is not exposed by BAQ field metadata in the qualified lab.
// Its native reader returns all retained file bodies for an app in one call.
fn start_history(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    // History is optional. Current drafts are already present in the saved rows.
    if !include_history(&r) {
        return prepare(r);
    }
    let mut requests = BTreeMap::<String, Value>::new();
    for row in r.state["rows"].as_array().unwrap() {
        let layer = row["TypeCode"] == "KNTCCustLayer";
        if !layer && row["TypeCode"] != "KNTCMetaUI" {
            continue;
        }
        let company = if row["Company"].as_str().is_some_and(|s| s.starts_with("T_")) {
            row["Company"].as_str().unwrap()
        } else {
            &r.company
        };
        let request = json!({"Company":company,"ProductID":row["ProductID"],"TypeCode":"",
            "Key1":row["Key1"],"Key2":if layer{row["Key2"].as_str().unwrap_or("")}else{""},
            "Key3":if layer{row["Key3"].as_str().unwrap_or("")}else{""},"CGCCode":row["CGCCode"],"Version":0});
        requests.insert(serde_json::to_string(&request).unwrap(), request);
    }
    r.state["historyRequests"] = json!(requests.into_values().collect::<Vec<_>>());
    r.state["historyIndex"] = json!(0);
    if r.state["historyRequests"].as_array().unwrap().is_empty() {
        return prepare(r);
    }
    if r.state["_collectionMethod"] == "serverAssisted" {
        r.state["historyOffset"] = json!(0);
        helper_history(r)
    } else {
        native_history(r)
    }
}
fn helper_history(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    r.state["phase"] = json!("helperHistory");
    r.state["_progress"] = json!({"request":format!("Saving version history in bulk ({} rows received)", r.state["versions"].as_array().unwrap().len()),"response":"History page received; saving older revisions separately"});
    Ok(GuestReply::Action {
        action: HostAction::ArtifactFunctionCall {
            alias: "history".into(),
            function: "ReadPage".into(),
            arguments: json!({"iRequests":r.state["historyRequests"].to_string(),"iOffset":r.state["historyOffset"],"iPageSize":size(&r)?}),
        },
        state: r.state,
    })
}
fn include_history(r: &GuestRequest) -> bool {
    r.state
        .pointer("/_pluginSettings/includeVersionHistory")
        .or_else(|| {
            r.state
                .pointer("/_pluginState/config/includeVersionHistory")
        })
        .and_then(Value::as_bool)
        .unwrap_or(true)
}
fn native_history(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let index = r.state["historyIndex"].as_u64().unwrap_or(0) as usize;
    let Some(args) = r.state["historyRequests"].get(index).cloned() else {
        return prepare(r);
    };
    r.state["phase"] = json!("nativeHistory");
    r.state["_progress"] = json!({"request":format!("Reading saved version history ({} of {})",index+1,r.state["historyRequests"].as_array().unwrap().len()),"response":"Saved versions received; preparing native packages and draft recovery"});
    Ok(GuestReply::Action {
        action: HostAction::NativeRead {
            service: "Ice.Lib.UxAppVersionSvc".into(),
            method: "GetUxAppVersions".into(),
            arguments: json!({"request":args}),
            export: false,
        },
        state: r.state,
    })
}
fn prepare(mut r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    r.state["phase"] = json!("processing");
    let starts_history = r.state["historyPass"] != true
        && r.state["nextCursor"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .is_some_and(|cursor| cursor["history"] == true);
    // Finish writing current packages before installing or fetching history.
    // Other page boundaries retain fetch/processing overlap.
    if r.state["_hostPreparePage"] == true && r.operation != "acquire" && !starts_history {
        Ok(GuestReply::PreparedPage {
            next_state: with_settings(json!({"phase":"nextPage","scopes":r.state["scopes"],"tenantCompany":r.state["tenantCompany"],"metadata":r.state["metadata"]}), &r.state),
            next_cursor: r.state["nextCursor"].as_str().map(str::to_owned),
            state: r.state,
        })
    } else {
        process(r)
    }
}
fn named_rows(rows: &[Value], name: fn(&Value) -> Option<&str>) -> BTreeMap<String, Vec<&Value>> {
    let mut index = BTreeMap::<String, Vec<&Value>>::new();
    for row in rows {
        if let Some(name) = name(row) { index.entry(name.into()).or_default().push(row); }
    }
    index
}
fn chunk_key(row: &Value) -> String {
    serde_json::to_string(&KEYS.map(|key| &row[key])).expect("native row key")
}
fn owned_chunks<'a>(rows: &[Value], index: &BTreeMap<String, Vec<(usize, &'a Value)>>) -> Vec<&'a Value> {
    let mut selected = BTreeMap::new();
    for row in rows {
        if let Some(chunks) = index.get(&chunk_key(row)) {
            for (position, chunk) in chunks { selected.insert(*position, *chunk); }
        }
    }
    selected.into_values().collect()
}
fn process(r: GuestRequest) -> Result<GuestReply, Diagnostic> {
    let rows = r.state["rows"]
        .as_array()
        .ok_or_else(|| invalid("The downloaded Kinetic rows are missing."))?;
    let metadata = r.state["metadata"]
        .as_array()
        .ok_or_else(|| invalid("The Kinetic metadata is missing."))?;
    let views = r.state["views"]
        .as_array()
        .ok_or_else(|| invalid("The Kinetic applications are missing."))?;
    let mut records = Vec::new();
    let mut identities = Vec::new();
    let mut diagnostics: Vec<Diagnostic> = serde_json::from_value(r.state["errors"].clone())
        .map_err(|_| invalid("The Kinetic diagnostics checkpoint is invalid."))?;
    let rows_by_view = named_rows(rows, view);
    let versions_by_view = named_rows(r.state["versions"].as_array().unwrap(), history_view);
    let mut chunks_by_key = BTreeMap::<String, Vec<(usize, &Value)>>::new();
    for (position, chunk) in r.state["chunks"].as_array().unwrap().iter().enumerate() {
        chunks_by_key.entry(chunk_key(chunk)).or_default().push((position, chunk));
    }
    for value in views {
        let name = value
            .as_str()
            .ok_or_else(|| invalid("An application name is invalid."))?;
        let mut id = identity(name, &r.company);
        if r.state["historyPass"] == true {
            id.kind = "KineticVersionHistory".into();
            let versions = versions_by_view.get(name).cloned().unwrap_or_default();
            identities.push(id.clone());
            records.push((
                id,
                json!({"ViewId":name,"UXAppVersion":versions,
                "herbertDirectOutput":true,"herbertOutputGroup":"Version history"}),
            ));
            continue;
        }
        let selected: Vec<Value> = rows_by_view.get(name).into_iter().flatten().map(|row| (*row).clone()).collect();
        let (binary, files, errors) = crate::kinetic_archive::build(name, &selected, metadata, r.state["tenantCompany"].as_str());
        let versions = versions_by_view.get(name).cloned().unwrap_or_default();
        let chunks = owned_chunks(&selected, &chunks_by_key);
        // Resource references form a transitive page graph. Preserve the shared
        // graph and original rows until an owner-closure output contract exists.
        let definition = json!({"XXXDef":selected,"XXXChunk":chunks,"UXAppVersion":versions,
            "versionHistory":if include_history(&r){"Collected separately under Version history"}else{"Not requested"},
            "resourceFiles":r.state["resources"],"resourceReferences":r.state["references"],"herbertDirectOutput":true,"herbertSources":{"files":files,"binaryFiles":binary}});
        for mut error in errors {
            error.identity = Some(id.clone());
            diagnostics.push(error);
        }
        identities.push(id.clone());
        records.push((id, definition));
    }
    if r.state["targeted"] == true {
        let (identity, definition) = records
            .pop()
            .ok_or_else(|| invalid("The selected application is missing."))?;
        Ok(GuestReply::Definition {
            identity,
            definition,
            format: "native-metafx-package-and-recovery".into(),
            completeness: if diagnostics.is_empty() {
                Completeness::Collected
            } else {
                Completeness::Partial
            },
            diagnostics,
        })
    } else {
        Ok(GuestReply::InventoryData {
            records,
            inventory: Box::new(GuestReply::Inventory {
                identities,
                next_cursor: r.state["nextCursor"].as_str().map(str::to_owned),
                authoritative: diagnostics.is_empty(),
                diagnostics,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexes_preserve_native_view_history_and_chunk_union_order() {
        let rows = vec![metadata("B", "b"), metadata("A", "a"), metadata("A", "a2"),
            json!({"TypeCode":"KNTCCustLayer","Key1":"layer","Key2":"A"}), json!({"Key1":"unrelated"})];
        let by_view = named_rows(&rows, view);
        let history = named_rows(&rows, history_view);
        let chunks = vec![rows[3].clone(), rows[0].clone(), rows[1].clone(), rows[4].clone()];
        let mut index = BTreeMap::<String, Vec<(usize, &Value)>>::new();
        for (position, chunk) in chunks.iter().enumerate() { index.entry(chunk_key(chunk)).or_default().push((position,chunk)); }
        for name in ["A", "B", "missing"] {
            let selected: Vec<_> = rows.iter().filter(|row| view(row) == Some(name)).cloned().collect();
            assert_eq!(json!(by_view.get(name).cloned().unwrap_or_default()), json!(selected));
            assert_eq!(history.get(name).cloned().unwrap_or_default(), rows.iter().filter(|row| history_view(row) == Some(name)).collect::<Vec<_>>());
            assert_eq!(owned_chunks(&selected, &index), chunks.iter().filter(|c| selected.iter().any(|s| KEYS.iter().all(|k| s[*k] == c[*k]))).collect::<Vec<_>>());
        }
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C12".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(1),
            response: None,
            error: None,
            state: json!({}),
        }
    }
    fn metadata(view: &str, id: &str) -> Value {
        json!({"Company":"","ProductID":"EP","TypeCode":"KNTCMetaUI","CGCCode":"","Key1":view,"Key2":format!("{view}/layout.jsonc"),"Key3":"","SysRowID":id,"SystemFlag":false})
    }
    fn prefix_request(prefix: &str, history: bool) -> GuestRequest {
        let mut r = request();r.state = json!({"phase":"nextPage","scopes":["C","","T_42"],"tenantCompany":"T_42",
            "metadata":[metadata("Other","other"),metadata("PickA","a"),metadata("PickB","b")],
            "_hostPreparePage":true,"_pluginSettings":{"applicationIdPrefix":prefix,"includeVersionHistory":history}});r
    }
    fn full(mut row: Value) -> Value {
        for (field,text) in [("Content","{}"),("SysCharacter01",""),("SysCharacter03","{\"saved\":true}")] {
            row[field] = json!(text);row[format!("Herbert{field}Bytes")] = json!(text.encode_utf16().count()*2);
        }
        row["SysRevID"] = json!(9007199254740993u64);row
    }
    #[test]
    fn prefix_filters_whole_view_set_before_offsets_and_history_pass() {
        let mut r = prefix_request("Pick",true);
        let GuestReply::Action {action:HostAction::Query {query},state} = handle(r.clone()) else {panic!()};
        assert_eq!(state["views"],json!(["PickA"]));assert_eq!(query.parameters["p0"],json!(["a"]));
        assert_eq!(serde_json::from_str::<Value>(state["nextCursor"].as_str().unwrap()).unwrap()["offset"],1);
        r.cursor = Some(state["nextCursor"].as_str().unwrap().into());
        let GuestReply::Action {state,..} = handle(r.clone()) else {panic!()};assert_eq!(state["views"],json!(["PickB"]));
        let next: Value = serde_json::from_str(state["nextCursor"].as_str().unwrap()).unwrap();assert_eq!(next,json!({"version":1,"offset":0,"history":true}));
        r.cursor = Some(next.to_string());r.state["_collectionMethod"] = json!("directOnly");
        let GuestReply::Action {action:HostAction::NativeRead {method,arguments,..},state} = handle(r) else {panic!()};
        assert_eq!(method,"GetUxAppVersions");assert_eq!(arguments["request"]["Key1"],"PickA");
        assert_eq!(state["historyRequests"].as_array().unwrap().len(),1);assert_eq!(state["views"],json!(["PickA"]));
        for (prefix,expected) in [("",Some("Other")),("PickA",Some("PickA")),("pick",None),(" Pick",None),("%",None)] {
            let reply = handle(prefix_request(prefix,false));
            if let Some(view) = expected {assert!(matches!(reply,GuestReply::Action {state,..} if state["views"] == json!([view])));}
            else {let GuestReply::PreparedPage {state,next_cursor,..} = reply else {panic!()};assert_eq!(state["views"],json!([]));assert!(next_cursor.is_none());}
        }
    }
    #[test]
    fn prefix_preserves_metadata_validation_raw_paging_and_frozen_scope_settings() {
        let mut r = request();let settings = json!({"applicationIdPrefix":"Pick","metadataRowsPerPage":2,"includeVersionHistory":false});r.state = json!({"_pluginSettings":settings});
        let GuestReply::Action {state,..} = handle(r.clone()) else {panic!()};assert_eq!(state["_pluginSettings"],settings);
        r.state = state;r.response = Some(json!({"parameters":{"companyID":"C","tenantID":"42"}}));
        let GuestReply::Action {state,..} = handle(r.clone()) else {panic!()};assert_eq!(state["_pluginSettings"],settings);assert_eq!(state["tenantCompany"],"T_42");
        r.state = state;r.response = Some(json!({"rows":[metadata("OtherA","other-a"),metadata("OtherB","other-b")]}));
        let GuestReply::Action {action:HostAction::Query {query},state} = handle(r.clone()) else {panic!()};assert_eq!(query.parameters["after"],"other-b");assert_eq!(state["_pluginSettings"],settings);
        r.state = state;r.response = Some(json!({"rows":[metadata("PickA","a")]}));
        let GuestReply::Action {state,..} = handle(r.clone()) else {panic!()};assert_eq!(state["wanted"],json!(["a"]));assert_eq!(state["metadata"].as_array().unwrap().len(),3);
        let mut bad = metadata("Other","bad");bad.as_object_mut().unwrap().remove("Key3");
        r.state = json!({"phase":"inventory","readPageSize":2,"metadata":[],"scopes":["C","","T_42"],"_pluginSettings":settings});r.response = Some(json!({"rows":[bad]}));
        assert!(matches!(handle(r.clone()),GuestReply::Failure {..}));
        for invalid in [Value::Null,json!(true),json!(3),json!([])] {r.state["_pluginSettings"]["applicationIdPrefix"] = invalid;assert!(matches!(handle(r.clone()),GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));}
    }
    #[test]
    fn prefix_selected_bundle_keeps_layers_parents_drafts_resources_and_package_bytes() {
        let mut r = prefix_request("Pick",false);r.page_size = Some(10);
        let app = metadata("Pick","a");let other = metadata("Other","other");
        let mut parent = metadata("unused","parent");parent["TypeCode"] = json!("KNTCCustLayer");parent["Key1"] = json!("OutsidePrefixParent");parent["Key2"] = json!("Pick");
        let mut child = parent.clone();child["Key1"] = json!("Child");child["SysRowID"] = json!("child");
        let meta = vec![other.clone(),app.clone(),parent.clone(),child.clone()];r.state["metadata"] = json!(meta);
        let GuestReply::Action {mut state,..} = handle(r.clone()) else {panic!()};
        assert_eq!(state["views"],json!(["Pick"]));assert_eq!(state["wanted"].as_array().unwrap().len(),3);
        let mut child = full(child);child["SysCharacter01"] = json!("[\"OutsidePrefixParent\"]");child["HerbertSysCharacter01Bytes"] = json!(child["SysCharacter01"].as_str().unwrap().encode_utf16().count()*2);
        state["rows"] = json!([full(app),full(parent),child]);state["resources"] = json!([{"File":"outside-prefix/resource","Original":"Ω bytes"}]);state["references"] = json!(["outside-prefix/resource"]);
        r.state = state;r.operation = "materialize".into();r.response = None;
        let GuestReply::InventoryData {records:filtered,inventory} = handle(r.clone()) else {panic!()};assert_eq!(filtered.len(),1);assert!(matches!(*inventory,GuestReply::Inventory {authoritative:true,..}));
        let mut control = r.clone();control.state["views"] = json!(["Other","Pick"]);control.state["rows"].as_array_mut().unwrap().push(full(other));
        let GuestReply::InventoryData {records:all,..} = handle(control) else {panic!()};
        assert_eq!(filtered[0],all.into_iter().find(|(id,_)|id.keys["ViewId"] == "Pick").unwrap());
        assert_eq!(filtered[0].1["XXXDef"].as_array().unwrap().len(),3);assert_eq!(filtered[0].1["resourceFiles"],r.state["resources"]);
        let GuestReply::PreparedPage {next_state,..} = prepare(r).unwrap() else {panic!()};assert_eq!(next_state["_pluginSettings"]["applicationIdPrefix"],"Pick");
    }
    #[test]
    fn prefix_exact_application_bundle_ignores_conflicting_discovery_value() {
        let mut r = prefix_request("",false);r.operation = "acquire".into();r.identity = Some(identity("PickA","C"));
        let baseline = serde_json::to_value(handle(r.clone())).unwrap();assert_eq!(baseline["type"],"action");
        r.state["_pluginSettings"]["applicationIdPrefix"] = json!("Missing");
        let actual = serde_json::to_value(handle(r)).unwrap();
        assert_eq!(actual["action"],baseline["action"]);assert_eq!(actual["state"]["wanted"],baseline["state"]["wanted"]);
        let mut literal = prefix_request(" Ω%_ '",false);literal.state["metadata"] = json!([metadata(" Ω%_ '","literal"),metadata("Other","other")]);
        assert!(matches!(handle(literal),GuestReply::Action {state,..} if state["views"] == json!([" Ω%_ '"])));
    }
    #[test]
    fn prepared_page_retains_next_batch_and_needs_no_network_to_materialize() {
        let mut r = request();
        r.state = json!({"phase":"nextPage","metadata":[metadata("A","a"),metadata("B","b")],"scopes":["C",""],"_hostPreparePage":true});
        let GuestReply::Action {
            action: HostAction::Query { query },
            mut state,
        } = handle(r.clone())
        else {
            panic!("Expected a body page");
        };
        assert_eq!(query.parameters["p0"], json!(["a"]));
        assert!(state["nextCursor"].is_string());
        state["phase"] = json!("processing");
        let mut row = metadata("A", "a");
        for (field, value) in [
            ("Content", "{}"),
            ("SysCharacter01", ""),
            ("SysCharacter03", ""),
        ] {
            row[field] = json!(value);
            row[format!("Herbert{field}Bytes")] = json!(value.len() * 2);
        }
        state["rows"] = json!([row]);
        r.state = state;
        let GuestReply::PreparedPage {
            state,
            next_state,
            next_cursor,
        } = prepare(r.clone()).unwrap()
        else {
            panic!("Expected prefetch handoff");
        };
        assert!(next_cursor.is_some());
        assert_eq!(next_state["phase"], "nextPage");
        r.operation = "materialize".into();
        r.state = state;
        let GuestReply::InventoryData { records, inventory } = handle(r) else {
            panic!("Materialization must not call the server");
        };
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].1["herbertSources"]["binaryFiles"]
                .as_array()
                .unwrap()
                .len(),
            1
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
    fn generated_cache_and_patch_only_apps_do_not_become_exports() {
        let mut cache = metadata("Stock", "cache");
        cache["Key2"] = json!("Stock\\cache\\res.ENU");
        cache["Key3"] = json!("cache");
        let mut patch = metadata("OtherStock", "patch");
        patch["Key2"] = json!("OtherStock\\patch.json");
        assert!(generated_cache(&cache));
        assert!(!authored_root(&cache));
        assert!(!authored_root(&patch));
        let mut r = request();
        r.state = json!({"phase":"nextPage","metadata":[cache,patch,metadata("Actual","a")],
            "scopes":["C",""],"_pluginSettings":{"includeVersionHistory":false}});
        let GuestReply::Action {
            action: HostAction::Query { query },
            state,
        } = handle(r)
        else {
            panic!("Expected authored data only")
        };
        assert_eq!(query.parameters["p0"], json!(["a"]));
        assert_eq!(state["views"], json!(["Actual"]));
        assert!(state["nextCursor"].is_null());
    }
    #[test]
    fn disabled_history_keeps_current_drafts_and_sends_no_history_request() {
        let mut r = request();
        r.state = json!({"_pluginSettings":{"includeVersionHistory":false},"_hostPreparePage":true,
            "metadata":[],"scopes":["C",""],"rows":[{"SysCharacter03":"saved draft"}]});
        let GuestReply::PreparedPage { state, .. } = start_history(r).unwrap() else {
            panic!("History must be skipped")
        };
        assert_eq!(state["rows"][0]["SysCharacter03"], "saved draft");
    }
    #[test]
    fn history_uses_helper_only_in_server_assisted_mode() {
        let mut r = request();
        r.state =
            json!({"rows":[metadata("A","a")],"versions":[],"_collectionMethod":"serverAssisted"});
        assert!(
            matches!(start_history(r.clone()).unwrap(), GuestReply::Action {
            action:HostAction::ArtifactFunctionCall {function,..},..
        } if function=="ReadPage")
        );
        r.state["_collectionMethod"] = json!("directOnly");
        assert!(matches!(start_history(r).unwrap(), GuestReply::Action {
            action:HostAction::NativeRead {method,..},..
        } if method=="GetUxAppVersions"));
    }
    #[test]
    fn exact_bundle_cannot_escape_selected_company() {
        let mut r = request();
        r.operation = "acquire".into();
        r.identity = Some(identity("A", "Other"));
        r.state = json!({"phase":"nextPage","metadata":[metadata("A","a")],"scopes":["C",""]});
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
    }
    #[test]
    fn native_tenant_parents_page_in_bulk_preserve_text_and_reject_scope_or_repetition() {
        let mut r = request();
        r.state=json!({"phase":"parents","scopes":["C","","T_42"],"tenantCompany":"T_42",
            "parentLookup":[{"name":"Parent's","view":"App"},{"name":"Other","view":"App"}],
            "parentChecked":[],"parentPageSeen":[],"rows":[],"wanted":[]});
        let GuestReply::Action {action:HostAction::NativeRead {service,method,arguments,..},state}=parent_page(&r,Value::Null).unwrap() else {panic!()};
        assert_eq!(service,"Ice.BO.GenXDataSvc");assert_eq!(method,"GetRows");
        let filter=arguments["whereClauseXXXDef"].as_str().unwrap();
        assert!(filter.contains("Company = 'T_42'"));assert!(filter.contains("Key1 = 'Parent''s'"));assert!(filter.contains("Key1 = 'Other'"));
        assert_eq!(arguments["whereClauseXXXChunk"],"1 = 0");assert_eq!(arguments["absolutePage"],1);
        r.state=state;
        let row=json!({"Company":"T_42","ProductID":"EP","TypeCode":"KNTCCustLayer","CGCCode":"","Key1":"Parent's","Key2":"App","Key3":"Desktop","SysRowID":"p1","Content":"😀 full content","SysCharacter01":"[]","SysCharacter03":"draft"});
        r.response=Some(json!({"parameters":{"morePages":true},"returnObj":{"XXXDef":[row.clone()]}}));
        let GuestReply::Action {action:HostAction::NativeRead {arguments,..},state}=handle(r.clone()) else {panic!()};
        assert_eq!(arguments["absolutePage"],2);assert_eq!(state["tenantCompany"],"T_42");
        assert_eq!(state["rows"][0]["Content"],row["Content"]);
        assert_eq!(state["rows"][0]["HerbertContentBytes"],30);
        r.state=state;
        assert!(matches!(handle(r.clone()),GuestReply::Failure{..}),"Repeated native page must fail");
        r.state["parentPageSeen"]=json!([]);
        r.response.as_mut().unwrap()["returnObj"]["XXXDef"][0]["Company"]=json!("T_other");
        assert!(matches!(handle(r.clone()),GuestReply::Failure{..}));
        r.response=Some(json!({"parameters":{"morePages":true},"returnObj":{"XXXDef":[]}}));
        assert!(matches!(handle(r),GuestReply::Failure{..}));
    }
}
