//! Authored schema presence. Stock dictionary parents are context, not artifacts.
//! ZDataTableSvc source proves UD/XT/EX, SystemFlag and UD* override storage.
//! ZColumnProperty is BAQ-inaccessible. A plugin-owned native reader returns
//! custom values and parent keys. Null and an explicit empty override differ.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const SERVICE: &str = "Ice.BO.ZDataTableSvc";
const KEYS: &[&str] = &["SystemCode", "DataTableID", "FieldName"];
const STRINGS: &[&str] = &[
    "UDLikeDataFieldSystemCode",
    "UDLikeDataFieldTableID",
    "UDLikeDataFieldName",
    "UDFieldFormat",
    "UDFieldLabel",
    "UDInitialValue",
    "UDCurrencyCodeColumn",
    "UDCurrencyType",
    "UDCurrencySource",
    "UDPredictiveSearchID",
];
const FLAGS: &[&str] = &["UDRequired", "UDReadOnly"];
const NULLABLE: &[&str] = &["UDUomColumn", "UDLinkedColumn", "UDCodeType", "ZoneBAQ"];
const PROPERTY_FLAGS: &[&str] = &["ZoneSearchOnEmptyControl", "IsHidden"];
pub fn entities(_category: &str) -> Vec<(String, String)> {
    vec![]
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn strings(row: &Value, names: &[&str]) -> Option<Vec<String>> {
    names
        .iter()
        .map(|name| row.get(*name).and_then(Value::as_str).map(String::from))
        .collect()
}
fn present(row: &Value, branch: usize) -> Result<bool, Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            "SCHEMA_OVERRIDE_PROJECTION_MISSING",
            "The native projection omitted or mistyped an authored override field.",
        )
    };
    let mut found = false;
    if branch == 0 {
        for name in STRINGS {
            let value = row.get(*name).ok_or_else(invalid)?;
            // Native ZDataFields projects an unset predictive search as null.
            // The persisted default is absence, unlike an explicit empty column-property override.
            if *name == "UDPredictiveSearchID" && value.is_null() {
                continue;
            }
            found |= !value.as_str().ok_or_else(invalid)?.is_empty();
        }
        for name in FLAGS {
            found |= row
                .get(*name)
                .and_then(Value::as_bool)
                .ok_or_else(invalid)?;
        }
    } else {
        for name in NULLABLE {
            let value = row.get(*name).ok_or_else(invalid)?;
            if !value.is_null() && !value.is_string() {
                return Err(invalid());
            }
            found |= !value.is_null();
        }
        // IsHidden mixes shipped and custom values. The owned property reader
        // establishes its ownership independently. Only validate its type here.
        for name in PROPERTY_FLAGS {
            let value = row
                .get(*name)
                .and_then(Value::as_bool)
                .ok_or_else(invalid)?;
            if *name != "IsHidden" {
                found |= value;
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema_request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C02".into(),
            company: "EXAMPLE".into(),
            identity: None,
            cursor: None,
            page_size: Some(1),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    #[test]
    fn c02_filters_system_tables_before_retaining_native_responses() {
        let GuestReply::Action {
            action: HostAction::NativeRead { arguments, .. },
            ..
        } = discover(schema_request(), 0, None)
        else {
            panic!("Expected native table discovery")
        };
        assert_eq!(arguments["whereClause"], "SystemFlag = false");
        assert_eq!(arguments["pageSize"], 1);
    }
    #[test]
    fn c02_keeps_custom_table_types_and_rejects_system_rows() {
        let mut request = schema_request();
        request.response = Some(json!({"returnObj":{"ZDataTableList":[
            {"SystemCode":"Ice","DataTableID":"Custom_Example","TableType":"EX","SystemFlag":false},
            {"SystemCode":"Ice","DataTableID":"Stock_Example","TableType":"EX","SystemFlag":true}
        ]},"parameters":{"morePages":true}}));
        let GuestReply::Inventory {
            identities,
            next_cursor,
            authoritative,
            ..
        } = discover(request, 0, None)
        else {
            panic!("Expected inventory")
        };
        assert!(authoritative);
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].keys["DataTableID"], "Custom_Example");
        let cursor: Value = serde_json::from_str(&next_cursor.unwrap()).unwrap();
        assert_eq!(cursor["page"], 2);
    }
    #[test]
    fn c02_ud_definition_does_not_invent_a_stock_parent_artifact() {
        let mut request = schema_request();
        request.response = Some(
            json!({"returnObj":{"ZDataTableList":[{"SystemCode":"ICE","DataTableID":"UD01_UD","TableType":"UD","SystemFlag":false}]},"parameters":{"morePages":false}}),
        );
        let GuestReply::Inventory {
            identities,
            authoritative,
            ..
        } = discover(request, 0, None)
        else {
            panic!()
        };
        assert!(authoritative);
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].keys["DataTableID"], "UD01_UD");
    }
    #[test]
    fn c02_uncertain_branches_never_prove_empty_inventory() {
        for branch in 0..4 {
            for case in ["denied", "missing", "invalidIdentity"] {
                let mut r = schema_request();
                r.cursor = Some(json!({"schemaBranch":branch}).to_string());
                if case == "denied" {
                    r.error = Some(Diagnostic::new("ACCESS_DENIED", "Synthetic refusal."));
                } else if case == "missing" {
                    r.response = Some(json!({}));
                } else {
                    r.response = Some(if branch == 0 {
                        json!({"returnObj":{"ZDataTableList":[{}]},"parameters":{"morePages":false}})
                    } else {
                        json!({"rows":[{}]})
                    });
                }
                let GuestReply::Inventory {
                    authoritative,
                    diagnostics,
                    ..
                } = discover(r, 0, None)
                else {
                    panic!()
                };
                assert!(!authoritative, "branch {branch} case {case}");
                assert!(!diagnostics.is_empty());
            }
        }
        let mut r = schema_request();
        r.response = Some(json!({"returnObj":{"ZDataTableList":[]}}));
        let GuestReply::Inventory {
            authoritative,
            diagnostics,
            ..
        } = discover(r, 0, None)
        else {
            panic!()
        };
        assert!(!authoritative);
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "PAGINATION_UNVERIFIED")
        );
    }
    #[test]
    fn c02_empty_table_page_still_visits_custom_child_branches() {
        let mut r = schema_request();
        r.response =
            Some(json!({"returnObj":{"ZDataTableList":[]},"parameters":{"morePages":false}}));
        let GuestReply::Inventory {
            identities,
            next_cursor,
            authoritative,
            ..
        } = discover(r.clone(), 0, None)
        else {
            panic!()
        };
        assert!(identities.is_empty());
        assert!(authoritative);
        r.cursor = next_cursor;
        r.response = Some(
            json!({"rows":[{"SystemCode":"ICE","DataTableID":"Parent","SysRowID":"00000000-0000-0000-0000-000000000001"}]}),
        );
        let GuestReply::Inventory {
            identities,
            next_cursor,
            authoritative,
            ..
        } = discover(r, 0, None)
        else {
            panic!()
        };
        assert!(authoritative);
        assert_eq!(identities.len(), 1);
        assert!(next_cursor.is_some());
    }
    #[test]
    fn nullable_override_preserves_explicit_empty_value() {
        let mut row = json!({"UDUomColumn":null,"UDLinkedColumn":null,"UDCodeType":null,"ZoneBAQ":null,"ZoneSearchOnEmptyControl":false,"IsHidden":false});
        assert!(!present(&row, 1).unwrap());
        row["UDCodeType"] = json!("");
        assert!(present(&row, 1).unwrap());
        row.as_object_mut().unwrap().remove("ZoneBAQ");
        assert!(present(&row, 1).is_err());
    }
    #[test]
    fn stock_persisted_defaults_do_not_create_authored_overrides() {
        let mut row = json!({});
        for field in STRINGS {
            row[*field] = json!("");
        }
        for field in FLAGS {
            row[*field] = json!(false);
        }
        row["UDPredictiveSearchID"] = Value::Null;
        assert!(!present(&row, 0).unwrap());
        row["UDFieldLabel"] = json!("User label");
        assert!(present(&row, 0).unwrap());
    }
}
fn parent(category: &str, system: String, table: String) -> Identity {
    Identity {
        category: category.into(),
        kind: if category == "C02" {
            "UDTableSchema"
        } else {
            "ExtendedTableProperties"
        }
        .into(),
        scope: Scope::Global,
        keys: BTreeMap::from([("SystemCode".into(), system), ("DataTableID".into(), table)]),
    }
}
pub fn discover(request: GuestRequest, family: usize, next_family: Option<String>) -> GuestReply {
    if request.category == "C03" {
        return discover_overrides(request, family, next_family);
    }
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => v.unwrap_or(json!({})),
        Err(_) => return fail("INVALID_CURSOR", "The authored schema cursor is invalid."),
    };
    let c02 = request.category == "C02";
    let branch = cursor["schemaBranch"].as_u64().unwrap_or(0) as usize;
    let branches = if c02 { 4 } else { 2 };
    if branch >= branches {
        return fail(
            "INVALID_CURSOR",
            "The authored schema branch is unavailable.",
        );
    }
    let page = cursor["page"].as_u64().unwrap_or(1);
    let after = cursor.get("after").cloned().unwrap_or(Value::Null);
    let mut seen: BTreeSet<String> = match cursor.get("schemaSeen") {
        Some(v) => match serde_json::from_value(v.clone()) {
            Ok(v) => v,
            Err(_) => {
                return fail(
                    "INVALID_CURSOR",
                    "The authored schema parent set is invalid.",
                );
            }
        },
        None => BTreeSet::new(),
    };
    let Some(size) = request.page_size.filter(|n| *n > 0) else {
        return fail(
            "PAGE_SIZE_REQUIRED",
            "Supply a positive schema discovery page size.",
        );
    };
    let continuation = |branch: usize, page: u64, after: Value, seen: &BTreeSet<String>| {
        if branch == branches {
            next_family.clone()
        } else {
            Some(json!({"family":family,"schemaBranch":branch,"page":page,"after":after,"schemaSeen":seen}).to_string())
        }
    };
    if request.response.is_none() && request.error.is_none() {
        let action = if c02 && branch == 0 {
            if page == 0 || page > i32::MAX as u64 {
                return fail(
                    "NATIVE_PAGING_RANGE",
                    "Native absolutePage requires a positive Int32 value.",
                );
            }
            HostAction::NativeRead {
                service: SERVICE.into(),
                method: "GetList".into(),
                export: false,
                arguments: json!({"whereClause":"SystemFlag = false","pageSize":size,"absolutePage":page}),
            }
        } else if c02 {
            let table = ["ZDataField", "ZLookupLink", "ZKey"][branch - 1];
            let mut criteria = vec![QueryCriterion {
                table: "SchemaSource".into(),
                field: "SystemFlag".into(),
                operator: "=".into(),
                parameter: "authored".into(),
            }];
            let mut parameters = BTreeMap::from([("authored".into(), json!(false))]);
            if !after.is_null() {
                if !after.is_string() {
                    return fail(
                        "INVALID_CURSOR",
                        "The schema row cursor must be a native identity.",
                    );
                }
                criteria.push(QueryCriterion {
                    table: "SchemaSource".into(),
                    field: "SysRowID".into(),
                    operator: ">".into(),
                    parameter: "after".into(),
                });
                parameters.insert("after".into(), after.clone());
            }
            HostAction::Query {
                query: QuerySpec {
                    tables: vec![QueryTable {
                        schema: "Ice".into(),
                        table: table.into(),
                        alias: "SchemaSource".into(),
                        company_field: None,
                        metadata: vec![],
                    }],
                    fields: ["SystemCode", "DataTableID", "SysRowID"]
                        .into_iter()
                        .map(|field| QueryField {
                            table: "SchemaSource".into(),
                            field: field.into(),
                            alias: field.into(),
                            expression: None,
                        })
                        .collect(),
                    criteria,
                    parameters,
                    joins: vec![],
                    order: vec![QueryOrder {
                        field: "SysRowID".into(),
                        descending: false,
                    }],
                    page_size: Some(size),
                    page: 1,
                },
            }
        } else {
            return fail("INVALID_CURSOR", "The schema branch is invalid.");
        };
        return GuestReply::Action {
            action,
            state: json!({"schemaBranch":branch}),
        };
    }
    if let Some(error) = request.error {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: continuation(branch + 1, 1, Value::Null, &seen),
            authoritative: false,
            diagnostics: vec![error],
        };
    }
    let response = request.response.as_ref().unwrap();
    let rows = if c02 && branch == 0 {
        response.pointer("/returnObj/ZDataTableList")
    } else if c02 {
        response.get("rows")
    } else {
        response.get("value")
    };
    let Some(rows) = rows.and_then(Value::as_array) else {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: continuation(branch + 1, 1, Value::Null, &seen),
            authoritative: false,
            diagnostics: vec![Diagnostic::new(
                "SCHEMA_INVENTORY_UNVERIFIED",
                "The native schema presence read omitted its row collection.",
            )],
        };
    };
    let mut identities = Vec::new();
    let mut diagnostics = Vec::new();
    for row in rows {
        if c02 && branch == 0 && crate::system_artifacts::marked(row) {
            continue;
        }
        let Some(keys) = strings(
            row,
            if c02 {
                &["SystemCode", "DataTableID"]
            } else {
                KEYS
            },
        ) else {
            diagnostics.push(Diagnostic::new(
                "SCHEMA_IDENTITY_UNVERIFIED",
                "A schema row omitted its complete parent identity.",
            ));
            continue;
        };
        if !c02 {
            match present(row, branch) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(error) => {
                    diagnostics.push(error);
                    continue;
                }
            }
        }
        let tables = vec![keys[1].clone()];
        if c02 && branch == 0 {
            let Some(kind) = row["TableType"].as_str() else {
                diagnostics.push(Diagnostic::new(
                    "SCHEMA_KIND_UNVERIFIED",
                    "A native schema table omitted its type.",
                ));
                continue;
            };
            let authored =
                matches!(kind, "UD" | "XT" | "EX") || row["SystemFlag"].as_bool() == Some(false);
            if !authored {
                diagnostics.push(Diagnostic::new(
                    "SCHEMA_FILTER_UNVERIFIED",
                    "The native authored-table filter returned a stock table.",
                ));
                continue;
            }
            if kind == "UD" {
                let table = &keys[1];
                // The UD definition is its own artifact. Custom additions to the
                // base table are discovered independently from their persisted rows.
                if table.len() <= 3 || !table.to_ascii_lowercase().ends_with("_ud") {
                    diagnostics.push(Diagnostic::new(
                        "SCHEMA_PARENT_UNVERIFIED",
                        "A UD definition does not have the native _UD base-table identity.",
                    ));
                }
            }
        }
        for table in tables {
            let id = parent(&request.category, keys[0].clone(), table);
            let mut lookup = id.clone();
            for value in lookup.keys.values_mut() {
                *value = value.to_ascii_lowercase();
            }
            let key = serde_json::to_string(&lookup).unwrap();
            if seen.insert(key) {
                identities.push(id);
            }
        }
    }
    let more = if c02 && branch == 0 {
        match response
            .pointer("/parameters/morePages")
            .or_else(|| response.get("morePages"))
            .and_then(Value::as_bool)
        {
            Some(v) => v,
            None => {
                diagnostics.push(Diagnostic::new(
                    "PAGINATION_UNVERIFIED",
                    "The native schema list omitted morePages.",
                ));
                false
            }
        }
    } else {
        rows.len() == size as usize || (!c02 && response.get("@odata.nextLink").is_some())
    };
    let next_cursor = if more {
        if c02 && branch == 0 {
            continuation(branch, page + 1, Value::Null, &seen)
        } else {
            let last = rows.last().and_then(|row| {
                if c02 {
                    row.get("SysRowID").filter(|v| v.is_string()).cloned()
                } else {
                    strings(row, KEYS).map(|v| json!(v))
                }
            });
            match last {
                Some(last) if last != after => continuation(branch, 1, last, &seen),
                _ => {
                    diagnostics.push(Diagnostic::new(
                        "SCHEMA_PAGING_STALLED",
                        "The native page did not advance its complete schema identity.",
                    ));
                    continuation(branch + 1, 1, Value::Null, &seen)
                }
            }
        }
    } else {
        continuation(branch + 1, 1, Value::Null, &seen)
    };
    GuestReply::Inventory {
        identities,
        next_cursor,
        authoritative: diagnostics.is_empty(),
        diagnostics,
    }
}

/// Filter native override storage before returning inventory. The reader emits
/// distinct parent keys only, so stock fields do not cross the host boundary.
fn discover_overrides(
    request: GuestRequest,
    _family: usize,
    next_family: Option<String>,
) -> GuestReply {
    discover_bulk_properties(request, next_family)
}

/// The reader omits shipped values at the native boundary, before the host saves
/// a response. Validate its exact keys and value shape before accepting output.
fn finish_properties(request: GuestRequest) -> GuestReply {
    let id = request.identity.as_ref().unwrap();
    let Some(response) = request.response.as_ref() else {
        return fail(
            "PROPERTY_RESPONSE_MISSING",
            "The property reader returned no response.",
        );
    };
    if response["oSuccess"] != true {
        return fail(
            "PROPERTY_READ_FAILED",
            response["oMessage"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("The property reader did not confirm success."),
        );
    }
    let Some(rows) = response["oJson"]
        .as_str()
        .and_then(|text| serde_json::from_str::<Vec<Value>>(text).ok())
    else {
        return fail(
            "PROPERTY_RESPONSE_INVALID",
            "The property reader did not return a JSON row collection.",
        );
    };
    let Some(fields) = request.state["fields"].as_array() else {
        return fail(
            "PROPERTY_STATE_INVALID",
            "The field override continuation is missing.",
        );
    };
    let mut seen = BTreeSet::new();
    for row in &rows {
        let Some(keys) = strings(
            row,
            &["SystemCode", "DataTableID", "FieldName", "PropertyID"],
        )
        .filter(|keys| keys.iter().all(|s| !s.is_empty())) else {
            return fail(
                "PROPERTY_IDENTITY_INVALID",
                "A property row omitted its native identity.",
            );
        };
        if !keys[0].eq_ignore_ascii_case(&id.keys["SystemCode"])
            || !keys[1].eq_ignore_ascii_case(&id.keys["DataTableID"])
        {
            return fail(
                "PROPERTY_PARENT_MISMATCH",
                "A property belongs to another table.",
            );
        }
        if !seen.insert((keys[2].to_ascii_lowercase(), keys[3].to_ascii_lowercase())) {
            return fail(
                "PROPERTY_DUPLICATE",
                "The reader repeated a property identity.",
            );
        }
        let allowed = [
            "SystemCode",
            "DataTableID",
            "FieldName",
            "PropertyID",
            "PropertyValue",
            "OverrideValue",
        ];
        if row
            .as_object()
            .unwrap()
            .keys()
            .any(|key| !allowed.contains(&key.as_str()))
            || !["PropertyValue", "OverrideValue"]
                .iter()
                .any(|key| row.get(*key).is_some())
            || ["PropertyValue", "OverrideValue"]
                .iter()
                .any(|key| row.get(*key).is_some_and(|v| !v.is_string()))
        {
            return fail(
                "PROPERTY_VALUE_INVALID",
                "The reader returned an unexpected or invalid property value.",
            );
        }
    }
    if fields.is_empty() && rows.is_empty() {
        return fail(
            "CUSTOM_OVERRIDES_NOT_FOUND",
            "This table has no saved custom field overrides.",
        );
    }
    GuestReply::Definition {
        identity: id.clone(),
        format: "native-field-overrides-json".into(),
        definition: json!({"service":SERVICE,"ZDataTable":[{"SystemCode":id.keys["SystemCode"],"DataTableID":id.keys["DataTableID"]}],"ZDataField":fields,"ZColumnProperty":rows}),
        completeness: Completeness::Collected,
        diagnostics: vec![],
    }
}

fn bulk_response(request: &GuestRequest) -> Result<Vec<Value>, Diagnostic> {
    let response = request.response.as_ref().ok_or_else(|| {
        Diagnostic::new(
            "PROPERTY_RESPONSE_MISSING",
            "The property reader returned no response.",
        )
    })?;
    if response["oSuccess"] != true {
        return Err(Diagnostic::new(
            "PROPERTY_READ_FAILED",
            response["oMessage"]
                .as_str()
                .unwrap_or("The property reader failed."),
        ));
    }
    let rows = response["oJson"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok())
        .ok_or_else(|| {
            Diagnostic::new(
                "PROPERTY_RESPONSE_INVALID",
                "The property reader did not return a table collection.",
            )
        })?;
    let mut seen = BTreeSet::new();
    for row in &rows {
        let keys = strings(row, &["SystemCode", "DataTableID"])
            .filter(|k| k.iter().all(|v| !v.is_empty()))
            .ok_or_else(|| {
                Diagnostic::new(
                    "PROPERTY_IDENTITY_INVALID",
                    "A property table omitted its identity.",
                )
            })?;
        if !seen.insert((keys[0].to_ascii_lowercase(), keys[1].to_ascii_lowercase())) {
            return Err(Diagnostic::new(
                "PROPERTY_DUPLICATE",
                "The reader repeated a table identity.",
            ));
        }
        if !row["ZDataField"].is_array() || !row["ZColumnProperty"].is_array() {
            return Err(Diagnostic::new(
                "PROPERTY_RESPONSE_INVALID",
                "A property table omitted its field or property collection.",
            ));
        }
    }
    Ok(rows)
}
fn bulk_action() -> GuestReply {
    GuestReply::Action {
        action: HostAction::SharedArtifactFunctionCall {
            alias: "properties".into(),
            function: "ReadAllProperties".into(),
            arguments: json!({}),
        },
        state: json!({}),
    }
}
fn discover_bulk_properties(mut request: GuestRequest, next_cursor: Option<String>) -> GuestReply {
    let prefix = match request.state.pointer("/_pluginSettings/tableIdPrefix") {
        None => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(_) => return fail("INVALID_SETTING", "Table ID starts with must be a string."),
    };
    if let Some(diagnostic) = request.error {
        return GuestReply::Failure { diagnostic };
    }
    if request.response.is_none() {
        return bulk_action();
    }
    match bulk_response(&request) {
        Err(diagnostic) => GuestReply::Failure { diagnostic },
        Ok(rows) => {
            // Validate once and hand final definitions directly to the writer.
            // Do not clone the environment-wide response for each table.
            request.response = None;
            request.state = Value::Null;
            let mut records = Vec::new();
            for row in rows {
                let id = parent(
                    "C03",
                    row["SystemCode"].as_str().unwrap().into(),
                    row["DataTableID"].as_str().unwrap().into(),
                );
                request.identity = Some(id.clone());
                match finish_table(request.clone(), row) {
                    GuestReply::Definition { definition, .. } => {
                        // Even unselected tables must pass complete field/property
                        // validation. Keep each selected graph unchanged.
                        if id.keys["DataTableID"].starts_with(&prefix) {
                            records.push((id, definition));
                        }
                    },
                    failure => return failure,
                }
            }
            GuestReply::InventoryData {
                inventory: Box::new(GuestReply::Inventory {
                    identities: records.iter().map(|(id, _)| id.clone()).collect(),
                    next_cursor,
                    authoritative: true,
                    diagnostics: vec![],
                }),
                records,
            }
        }
    }
}
pub fn acquire(mut request: GuestRequest) -> GuestReply {
    if let Some(diagnostic) = request.error.take() {
        return GuestReply::Failure { diagnostic };
    }
    if request.identity.is_none() {
        return fail(
            "IDENTITY_REQUIRED",
            "Select a table for its field overrides.",
        );
    }
    if let Some(table) = request.state.pointer("/_inventory/propertyTable").cloned() {
        return finish_table(request, table);
    }
    if request.response.is_none() {
        return bulk_action();
    }
    let rows = match bulk_response(&request) {
        Ok(rows) => rows,
        Err(diagnostic) => return GuestReply::Failure { diagnostic },
    };
    let id = request.identity.as_ref().unwrap();
    let Some(table) = rows.iter().find(|r| {
        r["SystemCode"]
            .as_str()
            .unwrap()
            .eq_ignore_ascii_case(&id.keys["SystemCode"])
            && r["DataTableID"]
                .as_str()
                .unwrap()
                .eq_ignore_ascii_case(&id.keys["DataTableID"])
    }) else {
        return fail(
            "CUSTOM_OVERRIDES_NOT_FOUND",
            "This table has no saved custom overrides.",
        );
    };
    finish_table(request.clone(), table.clone())
}

// Shared validation for direct discovery output and explicit targeted reads.
fn finish_table(mut request: GuestRequest, table: Value) -> GuestReply {
    let id = request.identity.as_ref().unwrap();
    if !table["SystemCode"]
        .as_str()
        .is_some_and(|v| v.eq_ignore_ascii_case(&id.keys["SystemCode"]))
        || !table["DataTableID"]
            .as_str()
            .is_some_and(|v| v.eq_ignore_ascii_case(&id.keys["DataTableID"]))
    {
        return fail(
            "SCHEMA_PARENT_MISMATCH",
            "The saved properties belong to another table.",
        );
    }
    let Some(fields) = table["ZDataField"].as_array() else {
        return fail(
            "PROPERTY_RESPONSE_INVALID",
            "The property table omitted its fields.",
        );
    };
    if !table["ZColumnProperty"].is_array() {
        return fail(
            "PROPERTY_RESPONSE_INVALID",
            "The property table omitted its properties.",
        );
    }
    let mut seen = BTreeSet::new();
    for field in fields {
        let Some(keys) = strings(field, KEYS).filter(|k| k.iter().all(|s| !s.is_empty())) else {
            return fail(
                "SCHEMA_IDENTITY_UNVERIFIED",
                "A field omitted its identity.",
            );
        };
        if !keys[0].eq_ignore_ascii_case(&id.keys["SystemCode"])
            || !keys[1].eq_ignore_ascii_case(&id.keys["DataTableID"])
        {
            return fail(
                "SCHEMA_PARENT_MISMATCH",
                "A field belongs to another table.",
            );
        }
        if !seen.insert(keys[2].to_ascii_lowercase()) {
            return fail("SCHEMA_FIELD_DUPLICATE", "The reader repeated a field.");
        }
        let mut custom = false;
        for (name, value) in field.as_object().unwrap() {
            if KEYS.contains(&name.as_str()) {
                continue;
            }
            if STRINGS.contains(&name.as_str()) && value.as_str().is_some_and(|v| !v.is_empty())
                || FLAGS.contains(&name.as_str()) && value == true
            {
                custom = true;
            } else {
                return fail(
                    "PROPERTY_VALUE_INVALID",
                    "The reader returned an unexpected field property.",
                );
            }
        }
        if !custom {
            return fail(
                "PROPERTY_VALUE_INVALID",
                "The reader returned a field without custom overrides.",
            );
        }
    }
    request.state = json!({"fields":fields});
    request.response = Some(json!({"oSuccess":true,"oJson":table["ZColumnProperty"].to_string()}));
    finish_properties(request)
}

#[cfg(test)]
mod bulk_tests {
    use super::*;
    fn row() -> Value {
        json!({"SystemCode":"Erp","DataTableID":"Part","ZDataField":[{"SystemCode":"ERP","DataTableID":"Part","FieldName":"Name","UDFieldLabel":"Custom Ω"}],"ZColumnProperty":[{"SystemCode":"Erp","DataTableID":"Part","FieldName":"Name","PropertyID":"CodeType","OverrideValue":""}]})
    }
    fn request(rows: Vec<Value>) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "C03".into(),
            company: "C".into(),
            identity: Some(parent("C03", "ERP".into(), "Part".into())),
            cursor: None,
            page_size: Some(200),
            state: Value::Null,
            response: Some(json!({"oSuccess":true,"oJson":serde_json::to_string(&rows).unwrap()})),
            error: None,
        }
    }
    fn named_row(system: &str, table: &str) -> Value {
        let mut value = row(); value["SystemCode"] = json!(system); value["DataTableID"] = json!(table);
        for child in ["ZDataField", "ZColumnProperty"] {
            value[child][0]["SystemCode"] = json!(system); value[child][0]["DataTableID"] = json!(table);
        }
        value
    }
    fn selected_request(rows: Vec<Value>, prefix: Value) -> GuestRequest {
        let mut r = request(rows); r.operation = "discover".into(); r.identity = None;
        r.state = json!({"_pluginSettings":{"tableIdPrefix":prefix}}); r
    }
    fn selected_records(reply: GuestReply) -> Vec<(Identity, Value)> {
        let GuestReply::InventoryData {records, inventory} = reply else {panic!("{reply:?}")};
        let GuestReply::Inventory {identities, authoritative, diagnostics, ..} = *inventory else {panic!()};
        assert!(authoritative); assert!(diagnostics.is_empty());
        assert_eq!(identities, records.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()); records
    }
    #[test]
    fn prefix_selects_complete_literal_property_graphs_across_systems() {
        let rows = vec![row(), named_row("Ice", "Part"), named_row("Erp", "Other"), named_row("Erp", " Ω%_ '")];
        let all = selected_records(discover_bulk_properties(request(rows.clone()), None));
        for (prefix, count) in [("", 4), ("P", 2), ("part", 0), (" Part", 0), ("%", 0), (" Ω%_ '", 1)] {
            let selected = selected_records(discover_bulk_properties(selected_request(rows.clone(), json!(prefix)), None));
            assert_eq!(selected.len(), count, "{prefix}");
            for record in selected {
                assert_eq!(all.iter().find(|(id, _)| id == &record.0).unwrap(), &record);
                assert_eq!(record.1["ZColumnProperty"][0]["OverrideValue"], "");
                assert_eq!(record.1["ZDataField"][0]["UDFieldLabel"], "Custom Ω");
            }
        }
        let mut r = request(rows); r.state = json!({"_pluginState":{"config":{"tableIdPrefix":"missing"}}});
        assert_eq!(selected_records(discover_bulk_properties(r, None)).len(), 4);
    }
    #[test]
    fn prefix_validates_unselected_fields_properties_and_tables_before_output() {
        for case in ["field-owner", "field-value", "property-owner", "null-override", "property-key", "duplicate-table", "missing-array"] {
            let mut bad = named_row("Erp", "Other");
            match case {
                "field-owner" => bad["ZDataField"][0]["DataTableID"] = json!("Wrong"),
                "field-value" => bad["ZDataField"][0]["UDFieldLabel"] = Value::Null,
                "property-owner" => bad["ZColumnProperty"][0]["DataTableID"] = json!("Wrong"),
                "null-override" => bad["ZColumnProperty"][0]["OverrideValue"] = Value::Null,
                "property-key" => bad["ZColumnProperty"][0]["PropertyID"] = json!(""),
                "missing-array" => {bad.as_object_mut().unwrap().remove("ZDataField");},
                _ => {},
            }
            let mut rows = vec![row(), bad.clone()];
            if case == "duplicate-table" { rows.push(bad); }
            assert!(matches!(discover_bulk_properties(selected_request(rows, json!("Part")), None), GuestReply::Failure {..}), "{case}");
        }
        for invalid in [Value::Null, json!(false), json!(1), json!([]), json!({})] {
            assert!(matches!(discover_bulk_properties(selected_request(vec![row()], invalid), None), GuestReply::Failure {diagnostic} if diagnostic.code == "INVALID_SETTING"));
        }
    }
    #[test]
    fn prefix_keeps_bulk_helper_arguments_and_exact_acquisition_independent() {
        let mut r = selected_request(vec![], json!("missing")); r.response = None;
        assert!(matches!(crate::handle("C03", r), GuestReply::Action {action:HostAction::SharedArtifactFunctionCall {alias,function,arguments},..}
            if alias == "properties" && function == "ReadAllProperties" && arguments == json!({})));
        let base = serde_json::to_value(crate::handle("C03", request(vec![row()]))).unwrap();
        assert_eq!(base["type"], "definition");
        let mut exact = request(vec![row()]);
        exact.state = json!({"_pluginSettings":{"tableIdPrefix":"missing"}});
        assert_eq!(serde_json::to_value(crate::handle("C03", exact)).unwrap(), base);
        let mut exact = request(vec![row()]); exact.response = None;
        exact.state = json!({"_pluginSettings":{"tableIdPrefix":"missing"}});
        assert!(matches!(crate::handle("C03", exact), GuestReply::Action {action:HostAction::SharedArtifactFunctionCall {arguments,..},..} if arguments == json!({})));
    }
    #[test]
    fn prefix_current_dispatch_returns_complete_selected_output_and_truthful_empty() {
        let rows = vec![row(), named_row("Erp", "Other")];
        let direct = selected_records(discover_bulk_properties(selected_request(rows.clone(), json!("Part")), None));
        let actual = selected_records(crate::handle("C03", selected_request(rows.clone(), json!("Part"))));
        assert_eq!(actual, direct);
        assert!(selected_records(crate::handle("C03", selected_request(rows, json!("absent")))).is_empty());
        let r = selected_request(vec![], json!("Part"));
        assert!(selected_records(crate::handle("C03", r.clone())).is_empty());
        let mut r = r; r.response = Some(json!({"oSuccess":false,"oMessage":"refused"}));
        assert!(matches!(crate::handle("C03", r), GuestReply::Inventory {authoritative:false,diagnostics,..} if diagnostics[0].code == "PROPERTY_READ_FAILED"));
    }
    #[test]
    fn discovery_and_acquisition_share_the_same_bulk_request() {
        let mut r = request(vec![]);
        r.response = None;
        for reply in [acquire(r.clone()), discover_bulk_properties(r, None)] {
            assert!(
                matches!(reply,GuestReply::Action{action:HostAction::SharedArtifactFunctionCall{function,arguments,..},..}if function=="ReadAllProperties"&&arguments==json!({}))
            );
        }
    }
    #[test]
    fn discovery_returns_validated_final_json_without_another_action() {
        let r = request(vec![row()]);
        let GuestReply::InventoryData { mut records, .. } =
            discover_bulk_properties(r.clone(), None)
        else {
            panic!()
        };
        assert_eq!(records.len(), 1);
        let (_, definition) = records.remove(0);
        assert_eq!(definition["service"], SERVICE);
        assert!(definition.get("propertyTable").is_none());
        assert_eq!(definition["ZDataField"][0]["UDFieldLabel"], "Custom Ω");
        assert_eq!(definition["ZColumnProperty"][0]["OverrideValue"], "");
        let mut invalid = row();
        invalid["ZDataField"][0]["DataTableID"] = json!("Other");
        assert!(matches!(
            discover_bulk_properties(request(vec![invalid]), None),
            GuestReply::Failure { diagnostic } if diagnostic.code == "SCHEMA_PARENT_MISMATCH"
        ));
    }
    #[test]
    fn bulk_read_preserves_values_without_requiring_a_parent_table() {
        let r = request(vec![row()]);
        let GuestReply::Definition {
            definition,
            completeness,
            ..
        } = acquire(r.clone())
        else {
            panic!()
        };
        assert_eq!(completeness, Completeness::Collected);
        assert_eq!(definition["ZDataField"][0]["UDFieldLabel"], "Custom Ω");
        assert_eq!(definition["ZColumnProperty"][0]["OverrideValue"], "");
        let GuestReply::InventoryData { inventory, .. } = discover_bulk_properties(r, None) else {
            panic!()
        };
        let GuestReply::Inventory { identities, .. } = *inventory else {
            panic!()
        };
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].keys["SystemCode"], "Erp");
    }
    #[test]
    fn bulk_read_rejects_wrong_parent_system_data_missing_keys_and_duplicates() {
        for (field, value) in [
            ("SystemCode", json!("Ice")),
            ("SystemFlag", json!(true)),
            ("FieldName", json!("")),
            ("UDFieldLabel", Value::Null),
        ] {
            let mut x = row();
            x["ZDataField"][0][field] = value;
            assert!(matches!(
                acquire(request(vec![x])),
                GuestReply::Failure { .. }
            ));
        }
        let x = row();
        assert!(
            matches!(acquire(request(vec![x.clone(),x])),GuestReply::Failure{diagnostic}if diagnostic.code=="PROPERTY_DUPLICATE")
        );
        let mut x = row();
        x["ZColumnProperty"][0]["SystemFlag"] = json!(true);
        assert!(matches!(
            acquire(request(vec![x])),
            GuestReply::Failure { .. }
        ));
    }
    #[test]
    fn bulk_empty_missing_and_failed_reads_stay_distinct() {
        let GuestReply::InventoryData { inventory, .. } =
            discover_bulk_properties(request(vec![]), None)
        else {
            panic!()
        };
        assert!(
            matches!(*inventory,GuestReply::Inventory{identities,authoritative:true,..}if identities.is_empty())
        );
        assert!(
            matches!(acquire(request(vec![])),GuestReply::Failure{diagnostic}if diagnostic.code=="CUSTOM_OVERRIDES_NOT_FOUND")
        );
        let mut r = request(vec![]);
        r.response = Some(json!({"oSuccess":false,"oMessage":"refused"}));
        assert!(
            matches!(acquire(r),GuestReply::Failure{diagnostic}if diagnostic.code=="PROPERTY_READ_FAILED")
        );
    }
}

#[cfg(test)]
mod schema_case_tests {
    use super::*;
    #[test]
    fn native_parent_and_child_spelling_produce_one_identity() {
        let mut r = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "C02".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(10),
            state: Value::Null,
            response: Some(
                json!({"returnObj":{"ZDataTableList":[{"SystemCode":"ERP","DataTableID":"Example_UD","TableType":"UD","SystemFlag":false}]},"parameters":{"morePages":false}}),
            ),
            error: None,
        };
        let GuestReply::Inventory {
            identities,
            next_cursor,
            ..
        } = discover(r.clone(), 0, None)
        else {
            panic!()
        };
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].keys["SystemCode"], "ERP");
        r.cursor = next_cursor;
        r.response = Some(
            json!({"rows":[{"SystemCode":"Erp","DataTableID":"Example_UD","SysRowID":"one"},{"SystemCode":"Ice","DataTableID":"Example_UD","SysRowID":"two"}]}),
        );
        let GuestReply::Inventory { identities, .. } = discover(r, 0, None) else {
            panic!()
        };
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].keys["SystemCode"], "Ice");
    }
}
