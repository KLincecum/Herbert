//! Native FileStore resources with explicit customization-owner metadata.
//! Content is never selected during inventory. Native byte reads use exact SysRowID.
use base64::Engine;
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const KIND: &str = "FileStoreCustomizationResource";
const ALIAS: &str = "ResourceSource";
fn owners() -> Vec<[String; 2]> {
    serde_json::from_str(include_str!("filestore-owner-tables.json")).unwrap()
}
pub fn owns(category: &str, kind: &str) -> bool {
    category == "custom-resources" && kind == KIND
}
pub fn methods() -> Vec<(String, String)> {
    vec![
        ("Ice.Lib.FileStoreSvc".into(), "ReadAllBytes".into()),
        ("Ice.BO.BAQDesignerSvc".into(), "GetFieldList".into()),
    ]
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn scope(company: &str) -> Scope {
    if company.is_empty() {
        Scope::Global
    } else {
        Scope::Company {
            company: company.into(),
        }
    }
}
fn identity(row: &Value, company: &str) -> Result<Identity, Diagnostic> {
    let mut keys: BTreeMap<String, String> = BTreeMap::new();
    for key in [
        "SysRowID",
        "RelatedToSchemaName",
        "RelatedToTable",
        "ForeignSysRowID",
    ] {
        let value = row[key].as_str().ok_or_else(|| {
            Diagnostic::new(
                "RESOURCE_IDENTITY_UNVERIFIED",
                "A resource lacks its complete FileStore identity and declared owner.",
            )
        })?;
        keys.insert(key.into(), value.into());
    }
    let actual = row["Company"].as_str().ok_or_else(|| {
        Diagnostic::new(
            "RESOURCE_SCOPE_UNVERIFIED",
            "A resource lacks Epicor company scope.",
        )
    })?;
    if !actual.is_empty() && actual != company {
        return Err(Diagnostic::new(
            "SCOPE_MISMATCH",
            "The resource belongs to another company.",
        ));
    }
    if !owners().iter().any(|o| {
        o[0].eq_ignore_ascii_case(&keys["RelatedToSchemaName"])
            && o[1].eq_ignore_ascii_case(&keys["RelatedToTable"])
    }) {
        return Err(Diagnostic::new(
            "RESOURCE_OWNER_OUTSIDE_SCOPE",
            "This stored file belongs to something other than the customizations being downloaded, so Herbert did not copy it.",
        ));
    }
    Ok(Identity {
        category: "custom-resources".into(),
        kind: KIND.into(),
        scope: scope(actual),
        keys,
    })
}
fn query(
    links: BTreeMap<String, Value>,
    after: Option<Value>,
    size: Option<u32>,
    state: Value,
    measure: bool,
) -> GuestReply {
    let mut criteria = Vec::new();
    let mut parameters = BTreeMap::new();
    for (field, value) in links {
        let parameter = format!("p{}", parameters.len());
        criteria.push(QueryCriterion {
            table: ALIAS.into(),
            field,
            operator: "=".into(),
            parameter: parameter.clone(),
        });
        parameters.insert(parameter, value);
    }
    if let Some(after) = after {
        criteria.push(QueryCriterion {
            table: ALIAS.into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
        parameters.insert("after".into(), after);
    }
    let mut fields: Vec<_> = [
        "SysRowID",
        "ForeignSysRowID",
        "RelatedToSchemaName",
        "RelatedToTable",
        "Company",
        "TenantID",
        "SecCode",
        "FileName",
        "Category",
        "FileType",
        "CreatedBy",
        "CreatedOn",
        "ModifiedBy",
        "ModifiedOn",
        "SysRevID",
    ]
    .iter()
    .map(|f| QueryField {
        table: ALIAS.into(),
        field: (*f).into(),
        alias: (*f).into(),
        expression: None,
    })
    .collect();
    if measure {
        fields.push(QueryField {
            table: ALIAS.into(),
            field: "FileName".into(),
            alias: "HerbertContentBytes".into(),
            expression: Some("CONVERT(nvarchar(max), DATALENGTH(ResourceSource.Content))".into()),
        });
    }
    GuestReply::Action {
        action: HostAction::Query {
            query: QuerySpec {
                tables: vec![QueryTable {
                    schema: "Ice".into(),
                    table: "FileStore".into(),
                    alias: ALIAS.into(),
                    company_field: None,
                    metadata: vec![],
                }],
                fields,
                joins: vec![],
                criteria,
                parameters,
                order: vec![QueryOrder {
                    field: "SysRowID".into(),
                    descending: false,
                }],
                page_size: size,
                page: 1,
            },
        },
        state,
    }
}
fn id_query(request: &GuestRequest, state: Value) -> GuestReply {
    let id = request.identity.as_ref().unwrap();
    let mut links: BTreeMap<_, _> = id.keys.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    let company = match &id.scope {
        Scope::Global => "",
        Scope::Company { company } => company,
        _ => unreachable!(),
    };
    links.insert("Company".into(), json!(company));
    query(links, None, None, state, true)
}
fn discovery(request: GuestRequest) -> GuestReply {
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => v.unwrap_or(json!({"owner":0,"global":false,"after":null})),
        Err(_) => {
            return fail(
                "INVALID_CURSOR",
                "The resource discovery saved position is invalid.",
            );
        }
    };
    let index = cursor["owner"].as_u64().unwrap_or(0) as usize;
    let global = cursor["global"].as_bool().unwrap_or(false);
    let owners = owners();
    let Some(owner) = owners.get(index) else {
        return fail(
            "INVALID_CURSOR",
            "The resource saved position names an unavailable owner family.",
        );
    };
    let next = || {
        if !global {
            Some(json!({"owner":index,"global":true,"after":null}).to_string())
        } else if index + 1 < owners.len() {
            Some(json!({"owner":index+1,"global":false,"after":null}).to_string())
        } else {
            None
        }
    };
    if request.response.is_none() && request.error.is_none() {
        if request.page_size.is_none() {
            return fail(
                "PAGE_SIZE_REQUIRED",
                "Supply a resource discovery page size.",
            );
        }
        return query(
            BTreeMap::from([
                ("RelatedToSchemaName".into(), json!(owner[0])),
                ("RelatedToTable".into(), json!(owner[1])),
                (
                    "Company".into(),
                    json!(if global { "" } else { &request.company }),
                ),
            ]),
            cursor.get("after").filter(|v| !v.is_null()).cloned(),
            request.page_size,
            cursor.clone(),
            true,
        );
    }
    if let Some(error) = request.error {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: next(),
            authoritative: false,
            diagnostics: vec![error],
        };
    }
    let Some(rows) = request.response.as_ref().and_then(|v| v["rows"].as_array()) else {
        return fail(
            "RESOURCE_INVENTORY_UNVERIFIED",
            "The FileStore metadata query omitted rows.",
        );
    };
    let mut identities = Vec::new();
    let mut records = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if crate::system_artifacts::marked(row) {
            continue;
        }
        if row["RelatedToSchemaName"]
            .as_str()
            .is_none_or(|s| !s.eq_ignore_ascii_case(&owner[0]))
            || row["RelatedToTable"]
                .as_str()
                .is_none_or(|s| !s.eq_ignore_ascii_case(&owner[1]))
            || row["Company"].as_str() != Some(if global { "" } else { &request.company })
        {
            diagnostics.push(Diagnostic::new(
                "RESOURCE_SCOPE_UNVERIFIED",
                "The resource inventory returned a different owner or company branch.",
            ));
            continue;
        }
        match identity(row, &request.company) {
            Ok(id) => {
                let token = serde_json::to_string(&id).unwrap();
                if seen.insert(token) {
                    records.push((id.clone(), json!({"metadata":row})));
                    identities.push(id)
                } else {
                    diagnostics.push(Diagnostic::new(
                        "DUPLICATE_NATIVE_IDENTITY",
                        "A resource inventory repeats its exact FileStore identity.",
                    ));
                }
            }
            Err(e) => diagnostics.push(e),
        }
    }
    let next_cursor = if request.page_size.is_some_and(|s| rows.len() == s as usize) {
        rows.last()
            .and_then(|r| r.get("SysRowID"))
            .map(|after| json!({"owner":index,"global":global,"after":after}).to_string())
    } else {
        next()
    };
    let inventory = GuestReply::Inventory {
        identities,
        next_cursor,
        authoritative: diagnostics.is_empty(),
        diagnostics,
    };
    GuestReply::InventoryData {
        records,
        inventory: Box::new(inventory),
    }
}
pub(crate) fn validate_bytes(metadata: &Value, response: &Value) -> Vec<Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            "RESOURCE_BYTES_UNVERIFIED",
            "The Epicor resource bytes, filename, or database byte length do not reconcile.",
        )
    };
    if response.pointer("/parameters/fileName") != metadata.get("FileName") {
        return vec![invalid()];
    }
    let Some(expected) = metadata["HerbertContentBytes"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| metadata["HerbertContentBytes"].as_u64())
    else {
        return vec![invalid()];
    };
    let Some(payload) = response.get("returnObj") else {
        return vec![invalid()];
    };
    let actual = if let Some(f) = payload.get("nativeFile") {
        serde_json::from_value::<FileRef>(f.clone())
            .ok()
            .filter(|f| !f.path.is_empty() && f.sha256.len() == 64)
            .map(|f| f.bytes)
    } else {
        match payload {
            Value::String(s) => base64::engine::general_purpose::STANDARD
                .decode(s)
                .ok()
                .map(|v| v.len() as u64),
            Value::Array(a) => a
                .iter()
                .all(|v| v.as_u64().is_some_and(|n| n <= 255))
                .then_some(a.len() as u64),
            _ => None,
        }
    };
    if actual == Some(expected) {
        vec![]
    } else {
        vec![invalid()]
    }
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.operation == "discover" {
        return discovery(request);
    }
    if request.operation != "acquire" {
        return fail(
            "UNSUPPORTED_OPERATION",
            "The resource collector supports discovery and exact download.",
        );
    }
    let Some(id) = request.identity.as_ref() else {
        return fail(
            "IDENTITY_REQUIRED",
            "Select an exact FileStore resource identity.",
        );
    };
    let mut declared = json!(id.keys);
    declared["Company"] = json!(match &id.scope {
        Scope::Global => "",
        Scope::Company { company } => company,
        _ =>
            return fail(
                "SCOPE_MISMATCH",
                "FileStore scope must be Epicor company or global scope."
            ),
    });
    if !owns(&request.category, &id.kind)
        || identity(&declared, &request.company).ok().as_ref() != Some(id)
    {
        return fail(
            "INVALID_IDENTITY",
            "Use a complete resource identity returned by discovery.",
        );
    }
    let phase = request.state["phase"]
        .as_str()
        .unwrap_or("initial")
        .to_string();
    if phase == "initial" {
        if request.state["_inventory"]["metadata"].is_object() {
            let metadata = request.state["_inventory"]["metadata"].clone();
            request.state = json!({"phase":"metadata"});
            request.response = Some(json!({"rows":[metadata]}));
            return handle(request);
        }
        return id_query(&request, json!({"phase":"metadata"}));
    }
    if let Some(error) = request.error.take() {
        if phase == "bytes" {
            request.state["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(error));
            return finish(request);
        }
        return GuestReply::Failure { diagnostic: error };
    }
    let Some(response) = request.response.as_ref() else {
        return fail(
            "RESOURCE_RESPONSE_MISSING",
            "A resource download response is absent.",
        );
    };
    if phase == "metadata" {
        let Some(rows) = response["rows"].as_array() else {
            return fail(
                "RESOURCE_METADATA_UNVERIFIED",
                "The resource metadata query omitted rows.",
            );
        };
        if rows.len() != 1 || identity(&rows[0], &request.company).ok().as_ref() != Some(id) {
            return fail(
                "RESOURCE_IDENTITY_MISMATCH",
                "The Epicor resource metadata no longer matches its full identity.",
            );
        }
        if crate::system_artifacts::marked(&rows[0]) {
            return crate::system_artifacts::refused();
        }
        if !rows[0]["FileName"].is_string()
            || !(rows[0]["HerbertContentBytes"].as_u64().is_some()
                || rows[0]["HerbertContentBytes"]
                    .as_str()
                    .is_some_and(|s| s.parse::<u64>().is_ok()))
        {
            return fail(
                "RESOURCE_METADATA_UNVERIFIED",
                "The resource metadata omitted its filename or byte length.",
            );
        }
        return GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.BO.BAQDesignerSvc".into(),
                method: "GetFieldList".into(),
                arguments: json!({"schema":id.keys["RelatedToSchemaName"],"dataTableID":id.keys["RelatedToTable"]}),
                export: false,
            },
            state: json!({"phase":"owner-schema","metadata":rows[0],"errors":[]}),
        };
    }
    if phase == "owner-schema" {
        let Some(columns) = response
            .pointer("/returnObj/TableFieldList")
            .and_then(Value::as_array)
        else {
            return fail(
                "RESOURCE_OWNER_UNVERIFIED",
                "The Epicor owner dictionary omitted its field list.",
            );
        };
        let names: BTreeSet<_> = columns
            .iter()
            .filter_map(|c| c["FieldName"].as_str())
            .collect();
        if !names.contains("SysRowID") {
            return fail(
                "RESOURCE_OWNER_UNVERIFIED",
                "The Epicor owner table has no stable row identity.",
            );
        }
        let mut state = request.state.clone();
        state["phase"] = json!("owner");
        state["ownerFields"] = json!(
            ["SysRowID", "Company", "SystemFlag"]
                .into_iter()
                .filter(|f| names.contains(f))
                .collect::<Vec<_>>()
        );
        return owner_batch(&request, state);
    }
    if phase == "owner" {
        let Some(rows) = response["rows"].as_array() else {
            return fail(
                "RESOURCE_OWNER_UNVERIFIED",
                "The Epicor owner query omitted rows.",
            );
        };
        let mut state = request.state.clone();
        if state["ownerRows"].is_null() {
            state["ownerRows"] = json!([]);
        }
        for row in rows {
            let Some(resource) = row["ResourceID"].as_str().filter(|v| !v.is_empty()) else {
                return fail(
                    "RESOURCE_OWNER_UNVERIFIED",
                    "The owner batch omitted its resource identity.",
                );
            };
            if resource.eq_ignore_ascii_case(&id.keys["SysRowID"]) {
                state["ownerRows"].as_array_mut().unwrap().push(row.clone());
            }
        }
        let Some(size) = request.page_size.filter(|v| *v > 0) else {
            return fail(
                "PAGE_SIZE_REQUIRED",
                "Supply a positive resource batch size.",
            );
        };
        if rows.len() == size as usize {
            let after = &rows.last().unwrap()["ResourceID"];
            if state["ownerAfter"] == *after {
                return fail(
                    "RESOURCE_CURSOR_INVALID",
                    "The resource owner batch did not advance.",
                );
            }
            state["ownerAfter"] = after.clone();
            return owner_batch(&request, state);
        }
        request.state = state;
        let rows = request.state["ownerRows"].as_array().unwrap();
        if rows.len() > 1
            || rows
                .iter()
                .any(|r| r["SysRowID"].as_str() != Some(id.keys["ForeignSysRowID"].as_str()))
        {
            return fail(
                "RESOURCE_OWNER_UNVERIFIED",
                "The Epicor owner query did not reconcile the exact declared owner.",
            );
        }
        if request.state["ownerFields"]
            .as_array()
            .is_some_and(|fields| {
                rows.iter().any(|r| {
                    fields.iter().any(|f| {
                        f.as_str()
                            .is_none_or(|f| r.get(f).is_none_or(Value::is_null))
                    })
                })
            })
        {
            return fail(
                "RESOURCE_OWNER_UNVERIFIED",
                "The Epicor owner row omitted a declared ownership field.",
            );
        }
        if rows.iter().any(crate::system_artifacts::marked) {
            return crate::system_artifacts::refused();
        }
        if rows.iter().any(|r| {
            r["Company"]
                .as_str()
                .is_some_and(|c| !c.is_empty() && c != request.company)
        }) {
            return fail(
                "SCOPE_MISMATCH",
                "The resource owner belongs to another company.",
            );
        }
        // An absent owner is not evidence of system ownership: preserve orphan resources.
        let mut state = request.state.clone();
        state["phase"] = json!("bytes");
        state["owner"] = json!(rows);
        return GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.Lib.FileStoreSvc".into(),
                method: "ReadAllBytes".into(),
                arguments: json!({"id":id.keys["SysRowID"]}),
                export: true,
            },
            state,
        };
    }
    if phase == "bytes" {
        let errors = validate_bytes(&request.state["metadata"], response);
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .extend(errors.into_iter().map(|e| json!(e)));
        request.state["bytes"] = response.clone();
        return finish(request);
    }
    fail(
        "INVALID_PLUGIN_STATE",
        "The FileStore resource saved progress is invalid.",
    )
}
// Match owner metadata to resources in pages. Each owner family and company
// shares this query; no full owner table or per-file owner request is needed.
fn owner_batch(request: &GuestRequest, state: Value) -> GuestReply {
    let id = request.identity.as_ref().unwrap();
    let Some(size) = request.page_size.filter(|v| *v > 0) else {
        return fail(
            "PAGE_SIZE_REQUIRED",
            "Supply a positive resource batch size.",
        );
    };
    let company = match &id.scope {
        Scope::Company { company } => company.as_str(),
        _ => "",
    };
    let mut fields: Vec<_> = state["ownerFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| QueryField {
            table: "Owner".into(),
            field: f.as_str().unwrap().into(),
            alias: f.as_str().unwrap().into(),
            expression: None,
        })
        .collect();
    fields.push(QueryField {
        table: ALIAS.into(),
        field: "SysRowID".into(),
        alias: "ResourceID".into(),
        expression: None,
    });
    let mut parameters = BTreeMap::from([
        ("schema".into(), json!(id.keys["RelatedToSchemaName"])),
        ("table".into(), json!(id.keys["RelatedToTable"])),
        ("company".into(), json!(company)),
    ]);
    let mut criteria: Vec<_> = [
        ("RelatedToSchemaName", "schema"),
        ("RelatedToTable", "table"),
        ("Company", "company"),
    ]
    .iter()
    .map(|(field, parameter)| QueryCriterion {
        table: ALIAS.into(),
        field: (*field).into(),
        operator: "=".into(),
        parameter: (*parameter).into(),
    })
    .collect();
    if let Some(after) = state["ownerAfter"].as_str() {
        parameters.insert("after".into(), json!(after));
        criteria.push(QueryCriterion {
            table: ALIAS.into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    GuestReply::Action {
        action: HostAction::SharedQuery {
            query: QuerySpec {
                tables: vec![
                    QueryTable {
                        schema: id.keys["RelatedToSchemaName"].clone(),
                        table: id.keys["RelatedToTable"].clone(),
                        alias: "Owner".into(),
                        company_field: None,
                        metadata: vec![],
                    },
                    QueryTable {
                        schema: "Ice".into(),
                        table: "FileStore".into(),
                        alias: ALIAS.into(),
                        company_field: None,
                        metadata: vec![],
                    },
                ],
                fields,
                criteria,
                parameters,
                joins: vec![QueryJoin {
                    left_table: ALIAS.into(),
                    left_field: "ForeignSysRowID".into(),
                    right_table: "Owner".into(),
                    right_field: "SysRowID".into(),
                    kind: "Inner".into(),
                }],
                order: vec![QueryOrder {
                    field: "ResourceID".into(),
                    descending: false,
                }],
                page_size: Some(size),
                page: 1,
            },
        },
        state,
    }
}
fn finish(request: GuestRequest) -> GuestReply {
    let diagnostics: Vec<Diagnostic> =
        serde_json::from_value(request.state["errors"].clone()).unwrap_or_default();
    GuestReply::Definition {
        identity: request.identity.unwrap(),
        format: "owned-filestore-resource-json".into(),
        definition: json!({"metadata":request.state["metadata"],"bytes":request.state["bytes"],"owner":request.state["owner"]}),
        completeness: if diagnostics.is_empty() {
            Completeness::Collected
        } else {
            Completeness::Partial
        },
        diagnostics,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_owner_batches_are_shared_paged_and_matched_locally() {
        let metadata = json!({"SysRowID":"id","ForeignSysRowID":"owner","RelatedToSchemaName":"Ice","RelatedToTable":"XXXDef","Company":"C","FileName":"a.bin","HerbertContentBytes":"1"});
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "custom-resources".into(),
            company: "C".into(),
            identity: Some(identity(&metadata, "C").unwrap()),
            cursor: None,
            page_size: Some(2),
            state: json!({"phase":"owner","metadata":metadata,"errors":[],"ownerFields":["SysRowID","Company","SystemFlag"]}),
            response: None,
            error: None,
        };
        let GuestReply::Action { action: first, .. } = owner_batch(&request, request.state.clone())
        else {
            panic!()
        };
        let mut second = request.clone();
        second
            .identity
            .as_mut()
            .unwrap()
            .keys
            .insert("SysRowID".into(), "other-file".into());
        second
            .identity
            .as_mut()
            .unwrap()
            .keys
            .insert("ForeignSysRowID".into(), "other-owner".into());
        let GuestReply::Action { action: other, .. } = owner_batch(&second, second.state.clone())
        else {
            panic!()
        };
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(other).unwrap()
        );
        request.response = Some(
            json!({"rows":[{"ResourceID":"id","SysRowID":"owner","Company":"C","SystemFlag":false},{"ResourceID":"other-file","SysRowID":"other-owner","Company":"C","SystemFlag":true}]}),
        );
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "other-file");
        assert_eq!(query.page_size, Some(2));
        assert!(!query.parameters.contains_key("owner"));
        request.state = state;
        request.response = Some(json!({"rows":[]}));
        let GuestReply::Action {
            action:
                HostAction::SharedNativeRead {
                    method, arguments, ..
                },
            state,
        } = handle(request)
        else {
            panic!("The unrelated System owner must not reject the custom resource")
        };
        assert_eq!(method, "ReadAllBytes");
        assert_eq!(arguments, json!({"id":"id"}));
        assert_eq!(state["owner"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn downloaded_resource_finishes_locally_and_retains_its_bytes() {
        let metadata = json!({"ResourceID":"id","SysRowID":"id","ForeignSysRowID":"owner","RelatedToSchemaName":"Ice","RelatedToTable":"XXXDef","Company":"C","FileName":"a.bin","HerbertContentBytes":"1"});
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "custom-resources".into(),
            company: "C".into(),
            identity: Some(identity(&metadata, "C").unwrap()),
            cursor: None,
            page_size: Some(200),
            state: json!({"phase":"bytes","metadata":metadata,"errors":[]}),
            response: Some(json!({"returnObj":"YQ==","parameters":{"fileName":"a.bin"}})),
            error: None,
        };
        let GuestReply::Definition {
            definition,
            completeness,
            ..
        } = handle(request)
        else {
            panic!("No confirmation request should follow the download.")
        };
        assert_eq!(completeness, Completeness::Collected);
        assert_eq!(definition["bytes"]["returnObj"], "YQ==");
    }
    #[test]
    fn owner_guard_precedes_bytes_without_discarding_orphans() {
        let metadata = json!({"ResourceID":"id","SysRowID":"id","ForeignSysRowID":"owner","RelatedToSchemaName":"Ice","RelatedToTable":"XXXDef","Company":"C"});
        let request = GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "custom-resources".into(),
            company: "C".into(),
            identity: Some(identity(&metadata, "C").unwrap()),
            cursor: None,
            page_size: Some(200),
            state: json!({"phase":"owner","metadata":metadata,"errors":[],"ownerFields":["SysRowID","Company","SystemFlag"]}),
            response: None,
            error: None,
        };
        for (row, code) in [
            (
                json!({"ResourceID":"id","SysRowID":"owner","Company":"C","SystemFlag":true}),
                "SYSTEM_ARTIFACT_EXCLUDED",
            ),
            (
                json!({"ResourceID":"id","SysRowID":"owner","Company":"OTHER","SystemFlag":false}),
                "SCOPE_MISMATCH",
            ),
            (
                json!({"ResourceID":"id","SysRowID":"wrong","Company":"C","SystemFlag":false}),
                "RESOURCE_OWNER_UNVERIFIED",
            ),
            (
                json!({"ResourceID":"id","SysRowID":"owner","Company":"C"}),
                "RESOURCE_OWNER_UNVERIFIED",
            ),
        ] {
            let mut r = request.clone();
            r.response = Some(json!({"rows":[row]}));
            let GuestReply::Failure { diagnostic } = handle(r) else {
                panic!("unsafe owner proceeded")
            };
            assert_eq!(diagnostic.code, code);
        }
        for rows in [
            json!([]),
            json!([{"ResourceID":"id","SysRowID":"owner","Company":"C","SystemFlag":false}]),
        ] {
            let mut r = request.clone();
            r.response = Some(json!({"rows":rows}));
            let GuestReply::Action {
                action: HostAction::SharedNativeRead { method, .. },
                ..
            } = handle(r)
            else {
                panic!("valid or orphan resource rejected")
            };
            assert_eq!(method, "ReadAllBytes");
        }
    }
    #[test]
    fn inventory_has_no_payload_and_preserves_owner_scope() {
        let reply = query(
            BTreeMap::from([("RelatedToTable".into(), json!("PcInputs"))]),
            None,
            Some(200),
            json!({}),
            false,
        );
        let GuestReply::Action {
            action: HostAction::Query { query },
            ..
        } = reply
        else {
            panic!()
        };
        assert!(
            query
                .fields
                .iter()
                .all(|f| f.field != "*" && f.field != "Content")
        );
        let row = json!({"ResourceID":"id","SysRowID":"id","ForeignSysRowID":"owner","RelatedToSchemaName":"Erp","RelatedToTable":"PcInputs","Company":""});
        assert_eq!(identity(&row, "C").unwrap().scope, Scope::Global);
        let mut business = row;
        business["RelatedToTable"] = json!("OrderHed");
        assert!(identity(&business, "C").is_err());
    }
    #[test]
    fn original_empty_bytes_and_name_length_mismatch_are_distinct() {
        let metadata = json!({"FileName":"empty.txt","HerbertContentBytes":"0"});
        assert!(
            validate_bytes(
                &metadata,
                &json!({"returnObj":"","parameters":{"fileName":"empty.txt"}})
            )
            .is_empty()
        );
        assert!(
            !validate_bytes(
                &metadata,
                &json!({"returnObj":null,"parameters":{"fileName":"empty.txt"}})
            )
            .is_empty()
        );
        assert!(
            !validate_bytes(
                &metadata,
                &json!({"returnObj":"YQ==","parameters":{"fileName":"empty.txt"}})
            )
            .is_empty()
        );
        assert!(
            !validate_bytes(
                &metadata,
                &json!({"returnObj":"","parameters":{"fileName":"other.txt"}})
            )
            .is_empty()
        );
    }
}
