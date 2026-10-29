//! Native definition collectors. Specs contain reviewed read routes and exact keys.
//! A successful root read does not prove resource, revision or independent coverage.
use herbert_contract::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[path = "access_export_scope.rs"]
mod access_export_scope;
#[path = "automation_params.rs"]
mod automation_params;
#[path = "baq_report_download.rs"]
mod baq_report_download;
#[path = "baq_report_export.rs"]
mod baq_report_export;
#[path = "configurator_download.rs"]
mod configurator_download;
#[path = "configurator_export.rs"]
pub(crate) mod configurator_export;
#[path = "extension_scope.rs"]
mod extension_scope;
#[path = "json_export.rs"]
mod json_export;
#[path = "message_inventory.rs"]
mod message_inventory;
#[path = "report_data_download.rs"]
mod report_data_download;
#[path = "report_export_scope.rs"]
mod report_export_scope;
#[path = "report_images.rs"]
mod report_images;
#[path = "report_style_scope.rs"]
mod report_style_scope;

#[derive(Clone, Deserialize, Serialize)]
struct Read {
    service: String,
    method: String,
    arguments: Value,
    #[serde(default)]
    export: bool,
    #[serde(default)]
    label: String,
    #[serde(default)]
    for_each: Option<String>,
}
#[derive(Deserialize)]
struct Family {
    category: String,
    kind: String,
    service: String,
    root: String,
    list_table: String,
    #[serde(default)]
    inventory_definition: bool,
    keys: Vec<String>,
    list: Read,
    read: Read,
    #[serde(default)]
    children: Vec<String>,
    #[serde(default)]
    extras: Vec<Read>,
    #[serde(default)]
    owner_field: Option<String>,
    #[serde(default)]
    owner_required: bool,
    #[serde(default)]
    coverage: Vec<String>,
}
fn specs() -> Vec<Family> {
    serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/families.json")))
        .expect("Reviewed collector specifications must be valid")
}
pub fn supported(category: &str) -> bool {
    specs().iter().any(|s| s.category == category)
}
pub fn methods(category: &str) -> Vec<(String, String)> {
    // The declared bulk reader supplies Extended Properties without per-table BO reads.
    if category == "extended-properties" {
        return vec![];
    }
    let mut methods = BTreeSet::new();
    for family in specs().into_iter().filter(|s| s.category == category) {
        for read in std::iter::once(&family.list)
            .chain(std::iter::once(&family.read))
            .chain(family.extras.iter())
        {
            methods.insert((read.service.clone(), read.method.clone()));
        }
    }
    if matches!(category, "configurators" | "kinetic-personalizations" | "report-styles-and-layouts") {
        methods.insert(("Ice.Lib.FileStoreSvc".into(), "ReadAllBytes".into()));
    }
    if category == "report-definitions" {
        methods.insert(("Ice.BO.DynamicReportSvc".into(), "ExportToFile".into()));
    }
    if category == "configurators" {
        methods.insert((
            "Erp.BO.ConfiguratorDefSvc".into(),
            "ExportConfiguration".into(),
        ));
        methods.insert(("Erp.BO.ConfigurationDesignSvc".into(), "GetByID".into()));
    }
    if matches!(category, "baqs-and-ubaqs" | "ubaqs" | "bpm-directives" | "data-directives") {
        methods.extend(crate::bpm_package::methods());
    }
    methods.into_iter().collect()
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn native_body(value: &Value) -> &Value {
    value
        .get("returnObj")
        .or_else(|| value.get("ds"))
        .unwrap_or(value)
}
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
// Query ownership follows native metadata, including misleading query names.
fn query_owned(spec: &Family, row: &Value) -> Result<bool, Diagnostic> {
    if !matches!(spec.kind.as_str(), "BAQ" | "UBAQ") {
        return Ok(true);
    }
    let updatable = row["Updatable"].as_bool().ok_or_else(|| {
        Diagnostic::new(
            "QUERY_TYPE_UNVERIFIED",
            "The Epicor query header omitted its boolean Updatable classification.",
        )
    })?;
    Ok(updatable == (spec.kind == "UBAQ"))
}

fn make_identity(spec: &Family, row: &Value, company: &str) -> Result<Identity, GuestReply> {
    let mut keys = BTreeMap::new();
    for key in &spec.keys {
        let Some(value) = row.get(key).and_then(scalar) else {
            return Err(fail(
                "IDENTITY_UNVERIFIED",
                "An Epicor row lacks a required composite-key field.",
            ));
        };
        keys.insert(key.clone(), value);
    }
    let scope = crate::personal_scope::scope(
        row,
        company,
        spec.owner_field.as_deref(),
        spec.owner_required,
    )
    .map_err(|diagnostic| GuestReply::Failure { diagnostic })?;
    let identity = Identity {
        category: spec.category.clone(),
        kind: spec.kind.clone(),
        scope,
        keys,
    };
    validate_identity(spec, &identity, company)?;
    Ok(identity)
}
#[path = "classic_scope.rs"]
mod classic_scope;

// Schema dictionary readers vary the case of native system and table keys.
// Compare those keys without case changes to the saved identity or payload.
fn root_identity_matches(actual: &Identity, expected: &Identity) -> bool {
    if actual.category != "ud-schema" || actual.kind != "UDTableSchema" {
        return actual == expected;
    }
    actual.category == expected.category
        && actual.kind == expected.kind
        && actual.scope == expected.scope
        && actual.keys.len() == expected.keys.len()
        && actual.keys.iter().all(|(key, value)| {
            expected.keys.get(key).is_some_and(|other| {
                if matches!(key.as_str(), "SystemCode" | "DataTableID") {
                    value.eq_ignore_ascii_case(other)
                } else {
                    value == other
                }
            })
        })
}
fn validate_identity(spec: &Family, id: &Identity, company: &str) -> Result<(), GuestReply> {
    if (crate::kind_enabled("ClassicPersonalization") && spec.kind == "ClassicPersonalization")
        && id.keys.get("TypeCode").map(String::as_str) != Some("Personalization")
    {
        return Err(fail(
            "SOURCE_MISMATCH",
            "This definition is not a Classic personalization.",
        ));
    }
    if (crate::kind_enabled("ClassicDefinition") && spec.kind == "ClassicDefinition")
        && !id
            .keys
            .get("TypeCode")
            .is_some_and(|kind| classic_scope::owns(kind))
    {
        return Err(fail(
            "SOURCE_MISMATCH",
            "The Epicor definition type does not belong to Classic customizations.",
        ));
    }
    // A fixed native discovery source also constrains direct requests. Otherwise
    // a caller could pass a different Source under this collection's identity.
    let expected_source = match spec.kind.as_str() {
        "MethodDirectiveGroup" => Some("BO"),
        "DataDirectiveGroup" => Some("DB"),
        _ => spec
            .list
            .arguments
            .get("source")
            .and_then(Value::as_str)
            .filter(|source| !source.starts_with('@')),
    };
    if let Some(source) = expected_source {
        if spec.keys.iter().any(|key| key == "Source")
            && id.keys.get("Source").map(String::as_str) != Some(source)
        {
            return Err(fail(
                "SOURCE_MISMATCH",
                "The selected definition belongs to a different collection source.",
            ));
        }
    }
    if id.category != spec.category
        || id.kind != spec.kind
        || id.keys.keys().collect::<BTreeSet<_>>() != spec.keys.iter().collect::<BTreeSet<_>>()
    {
        return Err(fail(
            "INVALID_IDENTITY",
            "Use the complete Epicor key returned by discovery for this family.",
        ));
    }
    match &id.scope {
        Scope::Company { company: c } | Scope::Personal { company: c, .. }
            if !c.is_empty() && c != company =>
        {
            return Err(fail(
                "SCOPE_MISMATCH",
                "The identity belongs to a different company.",
            ));
        }
        _ => {}
    }
    if id
        .keys
        .get("Company")
        .is_some_and(|c| !c.is_empty() && c != company)
    {
        return Err(fail(
            "SCOPE_MISMATCH",
            "The Epicor Company key is outside this run.",
        ));
    }
    Ok(())
}
fn expand(value: &Value, request: &GuestRequest, page: u64) -> Result<Value, GuestReply> {
    match value {
        Value::String(s) if s == "@company" => Ok(json!(request.company)),
        Value::String(s) if s == "@page" => Ok(json!(page)),
        Value::String(s) if s == "@size" => request
            .page_size
            .map(|n| json!(n))
            .ok_or_else(|| fail("PAGE_SIZE_REQUIRED", "Supply a discovery page size.")),
        Value::String(s) if s.starts_with("@key:") => {
            let key = &s[5..];
            request
                .identity
                .as_ref()
                .and_then(|id| id.keys.get(key))
                .map(|v| json!(v))
                .ok_or_else(|| {
                    fail(
                        "INVALID_IDENTITY",
                        "The request template requires a missing Epicor key.",
                    )
                })
        }
        Value::String(s) if s.starts_with("@integer:") => {
            let key = &s[9..];
            request
                .identity
                .as_ref()
                .and_then(|id| id.keys.get(key))
                .and_then(|v| v.parse::<i64>().ok())
                .map(|v| json!(v))
                .ok_or_else(|| fail("INVALID_IDENTITY", "The Epicor numeric key is invalid."))
        }
        Value::String(s) if s.starts_with("@row:") => request
            .state
            .get("_row")
            .and_then(|row| row.get(&s[5..]))
            .cloned()
            .ok_or_else(|| {
                fail(
                    "CHILD_KEY_UNVERIFIED",
                    "A supplemental read needs a missing Epicor child field.",
                )
            }),
        Value::String(s) => {
            let mut s = s.replace("@companyLiteral", &request.company.replace('\'', "''"));
            // "@idPrefixLike(Field)": a broad server-side superset of the run's literal definitionIdPrefix
            // (" AND Field LIKE 'stem%'", or nothing). The category keeps its exact local starts_with selection.
            while let Some(at) = s.find("@idPrefixLike(") {
                let Some(end) = s[at..].find(')').map(|e| at + e) else {
                    return Err(fail("INVALID_DESCRIPTOR", "An ID prefix placeholder is not closed."));
                };
                let field = &s[at + 14..end];
                if !crate::report_query::identifier(field) {
                    return Err(fail("INVALID_DESCRIPTOR", "An ID prefix placeholder names an invalid field."));
                }
                let prefix = request.state.pointer("/_pluginSettings/definitionIdPrefix").and_then(Value::as_str).unwrap_or("");
                let clause = crate::report_query::like_clause(field, prefix);
                s.replace_range(at..=end, &clause);
            }
            Ok(json!(s))
        }
        Value::Array(values) => values
            .iter()
            .map(|v| expand(v, request, page))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(k, v)| Ok((k.clone(), expand(v, request, page)?)))
            .collect::<Result<serde_json::Map<_, _>, GuestReply>>()
            .map(Value::Object),
        _ => Ok(value.clone()),
    }
}
fn action(read: &Read, request: &GuestRequest, page: u64, state: Value) -> GuestReply {
    match expand(&read.arguments, request, page) {
        Ok(arguments) => GuestReply::Action {
            action: if read.export {
                HostAction::SharedNativeRead {
                    service: read.service.clone(),
                    method: read.method.clone(),
                    arguments,
                    export: true,
                }
            } else {
                HostAction::NativeRead {
                    service: read.service.clone(),
                    method: read.method.clone(),
                    arguments,
                    export: false,
                }
            },
            state,
        },
        Err(error) => error,
    }
}
fn global_menu(spec: &Family, request: &GuestRequest) -> bool {
    spec.kind == "Menu"
        && request
            .identity
            .as_ref()
            .is_some_and(|id| id.keys.get("Company").is_some_and(String::is_empty))
}
// Filter native schema rows before the host stores the response. A table's
// namespace and name identify it. Neither grants permission to collect system rows.
fn custom_schema_read(id: &Identity) -> Read {
    let system = id.keys["SystemCode"].replace('\'', "''");
    let table = id.keys["DataTableID"].replace('\'', "''");
    let mut arguments = json!({"whereClauseZDataTable":format!("SystemCode = '{system}' AND DataTableID = '{table}' AND SystemFlag = false"),"pageSize":1,"absolutePage":1});

    for child in [
        "ZLookupLink",
        "ZLinkColumn",
        "ZLookupField",
        "ZDataField",
        "ZKey",
        "ZKeyField",
    ] {
        arguments[format!("whereClause{child}")] = json!("SystemFlag = false");
    }
    Read {
        service: "Ice.BO.ZDataTableSvc".into(),
        method: "GetRows".into(),
        arguments,
        export: false,
        label: "Custom schema".into(),
        for_each: None,
    }
}
fn root_action(spec: &Family, request: &GuestRequest) -> GuestReply {
    let state = json!({"phase":"root"});
    let id = request.identity.as_ref().unwrap();
    if matches!(spec.category.as_str(), "searches-and-navigation" | "menus-and-launch-definitions") {
        let predicate = spec
            .keys
            .iter()
            .map(|key| format!("{key} = '{}'", id.keys[key].replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let marked = matches!(
            spec.kind.as_str(),
            "QuickSearch" | "PredictiveSearch" | "Menu"
        );
        let mut read = spec.read.clone();
        read.arguments = json!({"pageSize":1,"absolutePage":1});
        read.arguments[format!("whereClause{}", spec.root)] = json!(if marked {
            format!("{predicate} AND SystemFlag = false")
        } else {
            predicate
        });
        for child in &spec.children {
            read.arguments[format!("whereClause{child}")] =
                json!(if marked { "SystemFlag = false" } else { "" });
        }
        return action(&read, request, 1, state);
    }
    if ((crate::category_enabled("automation-and-integrations") && spec.category == "automation-and-integrations")
        && matches!(spec.kind.as_str(), "ProcessSet" | "MessageTemplate"))
        || ((crate::category_enabled("access-and-assignments") && spec.category == "access-and-assignments")
            && spec.kind != "ColumnSecurity")
        || (crate::category_enabled("custom-resources") && spec.category == "custom-resources")
        || ((crate::category_enabled("programs-and-extensions") && spec.category == "programs-and-extensions")
            && spec.kind != "ExtensionSet")
    {
        // Native GetRows filters the marked root before its body or children are read.
        let predicate = spec
            .keys
            .iter()
            .map(|key| format!("{key} = '{}'", id.keys[key].replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut read = spec.read.clone();
        read.arguments = json!({"pageSize":1,"absolutePage":1});
        read.arguments[format!("whereClause{}", spec.root)] =
            json!(format!("{predicate} AND SystemFlag = false"));
        for child in &spec.children {
            read.arguments[format!("whereClause{child}")] = json!(if child == "ProcessTask"
                || (crate::category_enabled("access-and-assignments") && spec.category == "access-and-assignments")
                || ((crate::category_enabled("custom-resources") && spec.category == "custom-resources")
                    && child == "ThemeResource")
                || ((crate::category_enabled("programs-and-extensions") && spec.category == "programs-and-extensions")
                    && matches!(
                        child.as_str(),
                        "ZBODataSets" | "ZRelation" | "ZRelationField" | "ZSubDataSet"
                    )) {
                "SystemFlag = false"
            } else {
                ""
            });
        }
        return action(&read, request, 1, state);
    }
    if crate::category_enabled("report-definitions") && spec.category == "report-definitions" {
        if id.keys.values().any(String::is_empty)
            || !matches!(id.scope, Scope::Global | Scope::Company { .. })
        {
            return fail(
                "INVALID_IDENTITY",
                "A report definition requires nonempty Epicor keys and a global or company scope.",
            );
        }
        let company = match &id.scope {
            Scope::Company { company } => company.as_str(),
            _ => "",
        }
        .replace('\'', "''");
        let predicate = spec
            .keys
            .iter()
            .map(|key| format!("{key} = '{}'", id.keys[key].replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let system =
            if crate::kind_enabled("ReportDataDefinition") && spec.kind == "ReportDataDefinition" {
                "SystemFlag = false AND SystemRpt = false"
            } else {
                "SystemFlag = false"
            };
        let mut read = spec.read.clone();
        read.arguments = json!({"pageSize":1,"absolutePage":1});
        read.arguments[format!("whereClause{}", spec.root)] = json!(format!(
            "Company = '{company}' AND {predicate} AND {system}"
        ));
        for child in &spec.children {
            read.arguments[format!("whereClause{child}")] = json!(if matches!(
                child.as_str(),
                "RptDataSourceField" | "RptDataSourceParameter"
            ) {
                ""
            } else {
                "SystemFlag = false"
            });
        }
        return action(&read, request, 1, state);
    }
    if matches!(
        spec.kind.as_str(),
        "ClassicDefinition" | "ClassicPersonalization" | "ClassicDashboard"
    ) {
        let mut read = spec.read.clone();
        let predicate = spec
            .keys
            .iter()
            .map(|key| format!("{key} = '{}'", id.keys[key].replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" AND ");
        read.arguments = json!({"pageSize":1,"absolutePage":1});
        read.arguments[format!("whereClause{}", spec.root)] =
            json!(format!("{predicate} AND SystemFlag = false"));
        // Native DashBdChunk and DashBdLike expose independent SystemFlag.
        // DashBdBAQ has no marker and belongs to the exact custom root.
        for child in &spec.children {
            read.arguments[format!("whereClause{child}")] =
                json!(if matches!(child.as_str(), "DashBdChunk" | "DashBdLike") {
                    "SystemFlag = false"
                } else {
                    ""
                });
        }
        return action(&read, request, 1, state);
    }
    if (crate::category_enabled("ud-schema") && spec.category == "ud-schema")
        && (crate::kind_enabled("UDTableSchema") && spec.kind == "UDTableSchema")
    {
        return action(&custom_schema_read(id), request, 1, state);
    }
    if matches!(spec.kind.as_str(), "BAQ" | "UBAQ") {
        // Exclude system BAQs before their definitions can enter retained files.
        let company = id.keys["Company"].replace('\'', "''");
        let query = id.keys["QueryID"].replace('\'', "''");
        let mut read = spec.read.clone();
        read.arguments = json!({"whereClauseDynamicQueryDesigner":format!("Company = '{company}' AND QueryID = '{query}' AND SystemFlag = false"),"pageSize":1,"absolutePage":1});
        let filter = read.arguments["whereClauseDynamicQueryDesigner"]
            .as_str()
            .unwrap()
            .to_owned();
        let updatable = spec.kind == "UBAQ";
        read.arguments["whereClauseDynamicQueryDesigner"] =
            json!(format!("{filter} AND Updatable = {updatable}"));
        for child in &spec.children {
            read.arguments[format!("whereClause{child}")] = json!("");
        }
        return action(&read, request, 1, state);
    }
    // These native readers explicitly support All Companies records. The generic
    // GetByID methods reject a blank Company even for an authorized security manager.
    if global_menu(spec, request) {
        let menu = id.keys["MenuID"].replace('\'', "''");
        return action(
            &Read {
                service: spec.service.clone(),
                method: "GetRowsMenuMaint".into(),
                arguments: json!({"whereClauseMenu":format!("Company = '' AND MenuID = '{menu}'"),"pageSize":"@size","absolutePage":1}),
                export: false,
                label: String::new(),
                for_each: None,
            },
            request,
            1,
            state,
        );
    }
    if matches!(
        spec.kind.as_str(),
        "MethodDirectiveGroup" | "DataDirectiveGroup"
    ) {
        // Apply ownership filtering before a native definition reaches saved files.
        let method = id.keys["BpMethodCode"].replace("'", "''");
        return GuestReply::Action {
            action: crate::bpm_package::scoped_native_read(
                &id.keys["Source"],
                &format!("[BpMethodCode] = '{method}'"),
            ),
            state,
        };
    }
    action(&spec.read, request, 1, state)
}
fn inventory(request: GuestRequest, selected: Vec<Family>) -> GuestReply {
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => v.unwrap_or(json!({"family":0,"page":1})),
        Err(_) => {
            return fail(
                "INVALID_CURSOR",
                "The category discovery saved position is invalid.",
            );
        }
    };
    let index = cursor.get("family").and_then(Value::as_u64).unwrap_or(0) as usize;
    let page = cursor.get("page").and_then(Value::as_u64).unwrap_or(1);
    let Some(spec) = selected.get(index) else {
        return fail(
            "INVALID_CURSOR",
            "The discovery saved position names an unavailable family.",
        );
    };
    if spec.kind == "MessageTemplate" {
        let next =
            (index + 1 < selected.len()).then(|| json!({"family":index+1,"page":1}).to_string());
        return message_inventory::discover(request, spec, index, next);
    }
    if crate::kind_enabled("ReportStyles") && spec.kind == "ReportStyles" {
        let next =
            (index + 1 < selected.len()).then(|| json!({"family":index+1,"page":1}).to_string());
        return report_style_scope::discover(request, index, next);
    }
    if matches!(
        spec.kind.as_str(),
        "UDTableSchema" | "ExtendedTableProperties"
    ) {
        let next =
            (index + 1 < selected.len()).then(|| json!({"family":index+1,"page":1}).to_string());
        return crate::authored_schema::discover(request, index, next);
    }
    if page == 0 || page > i32::MAX as u64 {
        return fail(
            "NATIVE_PAGING_RANGE",
            "Herbert could not continue this download from its saved progress (page position). Start the download again.",
        );
    }
    if request.response.is_none() && request.error.is_none() {
        return action(
            &spec.list,
            &request,
            page,
            json!({"phase":"discover","family":index,"page":page}),
        );
    }
    let next_family = || {
        if index + 1 < selected.len() {
            Some(json!({"family":index+1,"page":1}).to_string())
        } else {
            None
        }
    };
    if let Some(error) = request.error {
        // A refused branch must not prevent independent families in this category.
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: next_family(),
            authoritative: false,
            diagnostics: vec![error],
        };
    }
    let response = request.response.as_ref().unwrap();
    let rows = native_body(response)
        .get(&spec.list_table)
        .and_then(Value::as_array);
    let Some(rows) = rows else {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: next_family(),
            authoritative: false,
            diagnostics: vec![Diagnostic::new(
                "INVENTORY_TABLE_MISSING",
                "The Epicor list response omitted its declared root table.",
            )],
        };
    };
    let definitions = if spec.inventory_definition {
        crate::native_children::split_page(&spec.service, &spec.root, native_body(response))
    } else {
        None
    };
    let mut records = Vec::new();
    let mut identities = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen = BTreeSet::new();
    if spec.category=="configurators" && spec.inventory_definition && definitions.is_none() {
        diagnostics.push(Diagnostic::new("CONFIGURATOR_NATIVE_PAGE_INVALID","The downloaded configurator page could not associate its complete Epicor child record set."));
    }
    for (row_index, row) in rows.iter().enumerate() {
        if crate::system_artifacts::marked(row) {
            continue;
        }
        match query_owned(spec, row) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                continue;
            }
        }
        match make_identity(spec, row, &request.company) {
            Ok(identity) => {
                let token = serde_json::to_string(&identity).unwrap();
                if seen.insert(token) {
                    if let Some(definitions) = &definitions {
                        records.push((identity.clone(), json!({"response":{"returnObj":definitions[row_index],"parameters":{"morePages":false}}})));
                    }
                    identities.push(identity);
                } else {
                    diagnostics.push(Diagnostic::new(
                        "DUPLICATE_NATIVE_IDENTITY",
                        "Epicor discovery repeats an exact identity.",
                    ));
                }
            }
            Err(GuestReply::Failure { diagnostic }) => diagnostics.push(diagnostic),
            _ => unreachable!(),
        }
    }
    let more = response
        .pointer("/parameters/morePages")
        .or_else(|| response.get("morePages"))
        .and_then(Value::as_bool);
    let next_cursor = match more {
        Some(true) => Some(json!({"family":index,"page":page+1}).to_string()),
        Some(false) => next_family(),
        None => {
            diagnostics.push(Diagnostic::new("PAGINATION_UNVERIFIED", "The Epicor response omitted morePages; this branch cannot establish exhaustive discovery."));
            next_family()
        }
    };
    let inventory = GuestReply::Inventory {
        identities,
        next_cursor,
        authoritative: diagnostics.is_empty(),
        diagnostics,
    };
    if records.is_empty() {
        inventory
    } else {
        GuestReply::InventoryData {
            records,
            inventory: Box::new(inventory),
        }
    }
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    if request.category == "configurators" && crate::configurator_pages::pending(&request) {
        return crate::configurator_pages::resume(request);
    }
    let selected: Vec<_> = specs()
        .into_iter()
        .filter(|s| s.category == request.category)
        .collect();
    if request.operation == "discover" {
        if request.category == "configurators" {
            let reply = inventory(request.clone(), selected);
            let reply = crate::configurator_output::select_inventory(&request,reply);
            return crate::configurator_pages::begin(request, reply);
        }
        return inventory(request, selected);
    }
    if request.operation != "acquire" {
        return fail(
            "UNSUPPORTED_OPERATION",
            "The collector supports discovery and exact-identity download.",
        );
    }
    let Some(id) = request.identity.as_ref() else {
        return fail("IDENTITY_REQUIRED", "Select an exact Epicor ID.");
    };
    let Some(spec) = selected.iter().find(|s| s.kind == id.kind) else {
        return fail(
            "INVALID_IDENTITY",
            "This category does not collect the selected definition family.",
        );
    };
    if let Err(error) = validate_identity(spec, id, &request.company) {
        return error;
    }
    if spec.kind == "Configurator" {
        return configurator_download::handle(request, spec);
    }
    if spec.kind == "ReportDataDefinition" {
        return report_data_download::handle(request, spec);
    }
    if spec.kind == "BAQReport" {
        return baq_report_download::handle(request, spec);
    }
    if matches!(spec.kind.as_str(), "AccessScope" | "MessageTemplate") {
        return json_export::handle(request, spec);
    }
    if matches!(spec.kind.as_str(), "BAQ" | "UBAQ") {
        return crate::query_download::acquire(request);
    }
    if crate::category_enabled("extended-properties") && spec.category == "extended-properties" {
        return crate::authored_schema::acquire(request);
    }
    if request.state.get("phase").is_none() && request.response.is_none() && request.error.is_none()
    {
        if let Some(response) = request.state.pointer("/_inventory/response").cloned() {
            request.response = Some(response);
            request.state["phase"] = json!("root");
        }
    }
    let phase = request
        .state
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("initial")
        .to_string();
    if crate::kind_enabled("ExtensionSet")
        && spec.kind == "ExtensionSet"
        && phase == "extensionPackage"
    {
        extension_scope::package_complete(&mut request);
        return finish(spec, request);
    }
    if (crate::kind_enabled("ExtensionSet") && spec.kind == "ExtensionSet")
        && matches!(phase.as_str(), "initial" | "extensionAcquire")
    {
        return extension_scope::acquire(request);
    }
    if (crate::kind_enabled("ReportStyles") && spec.kind == "ReportStyles")
        && matches!(phase.as_str(), "initial" | "styleAcquire")
    {
        match report_style_scope::acquire(request) {
            report_style_scope::Acquisition::Reply(reply) => return reply,
            report_style_scope::Acquisition::Ready(mut ready) => {
                let (mut queue, errors) = supplements(spec, &ready.state["definition"], &ready);
                let ssrs = ready.state["definition"]["ReportStyle"][0]["RptTypeID"] == "SSRS";
                queue.retain_mut(|read| {
                    if read.method == "ExportReportStyle" {
                        return ready.state["styleNativeExportAllowed"] == true;
                    }
                    if read.method == "ExportSsrsRdl" && ssrs {
                        if let Some(path) = read.arguments["reportPath"]
                            .as_str()
                            .and_then(|path| crate::report_resources::custom_path(path).ok())
                        {
                            read.arguments["reportPath"] = json!(path);
                            return true;
                        }
                    }
                    false
                });
                ready.state["errors"]
                    .as_array_mut()
                    .unwrap()
                    .extend(errors.into_iter().map(|e| json!(e)));
                ready.state["queue"] = json!(queue);
                ready.state["index"] = json!(0);
                ready.state["phase"] = json!("extras");
                if let Some(extra) = queue.first() {
                    return action(extra, &ready, 1, ready.state.clone());
                }
                return finish(spec, ready);
            }
        }
    }
    if crate::kind_enabled("ReportStyles")
        && matches!(phase.as_str(), "styleImageMetadata" | "styleImageBytes")
    {
        if let Some(action) = report_images::complete(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
        return finish(spec, request);
    }
    if crate::kind_enabled("ProcessSet") && phase == "automationParameters" {
        if let Some(action) = automation_params::complete(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
        return finish(spec, request);
    }
    if (crate::category_enabled("baqs-and-ubaqs")
        || crate::category_enabled("ubaqs")
        || crate::category_enabled("bpm-directives")
        || crate::category_enabled("data-directives"))
        && phase.starts_with("bpm")
        && phase != "bpmComplete"
    {
        if let Some(action) = crate::bpm_package::complete(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
        return finish(spec, request);
    }
    if crate::kind_enabled("ReportStyles")
        && matches!(phase.as_str(), "reportRdlRead" | "reportRdlParse")
    {
        if let Some(action) = crate::report_resources::complete(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
        return finish(spec, request);
    }
    if crate::category_enabled("ud-schema") && phase == "schemaCustomChildren" {
        return crate::schema_children::complete(request);
    }
    if phase == "initial" || phase == "ubaqOwnerVerified" {
        return root_action(spec, &request);
    }
    if phase == "root" {
        if let Some(error) = request.error {
            return GuestReply::Failure { diagnostic: error };
        }
        let Some(response) = request.response.as_ref() else {
            return fail(
                "INVALID_NATIVE_RESPONSE",
                "The Epicor definition response is absent.",
            );
        };
        if global_menu(spec, &request)
            && response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
                != Some(false)
        {
            return fail(
                "ROOT_UNVERIFIED",
                "The exact global menu read did not establish a complete single-key result.",
            );
        }
        if spec.category == "ud-schema" && spec.kind == "UDTableSchema"
            && response.pointer("/parameters/morePages").and_then(Value::as_bool) != Some(false)
        {
            return fail("SCHEMA_READ_INCOMPLETE",
                "The exact schema read did not confirm a complete result.");
        }
        let definition = native_body(response);
        if (crate::category_enabled("access-and-assignments") && spec.category == "access-and-assignments")
            && spec.kind != "ColumnSecurity"
            && response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
                != Some(false)
        {
            return fail(
                "ACCESS_READ_INCOMPLETE",
                "The exact access definition read did not confirm a complete result.",
            );
        }
        if matches!(
            spec.kind.as_str(),
            "ClassicDefinition" | "ClassicPersonalization" | "ClassicDashboard"
        ) && response
            .pointer("/parameters/morePages")
            .and_then(Value::as_bool)
            != Some(false)
        {
            return fail(
                "CLASSIC_READ_INCOMPLETE",
                "The exact Classic definition read did not confirm a complete result.",
            );
        }
        let Some(root) = definition.get(&spec.root).and_then(Value::as_array) else {
            return fail(
                "ROOT_UNVERIFIED",
                "The Epicor response omitted its declared definition root.",
            );
        };
        if crate::category_enabled("report-definitions") && spec.category == "report-definitions" {
            if response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
                != Some(false)
            {
                return fail(
                    "REPORT_READ_INCOMPLETE",
                    "The exact report definition read did not confirm a complete result.",
                );
            }
            if root.is_empty() {
                return fail(
                    "CUSTOM_REPORT_NOT_FOUND",
                    "No custom report definition matches this exact identity. System definitions are excluded.",
                );
            }
            if definition.as_object().is_some_and(|tables| {
                tables
                    .values()
                    .filter_map(Value::as_array)
                    .flatten()
                    .any(crate::system_artifacts::marked)
            }) {
                return crate::system_artifacts::refused();
            }
        }
        if matches!(spec.kind.as_str(), "BAQ" | "UBAQ")
            && response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
                != Some(false)
        {
            return fail(
                "BAQ_READ_INCOMPLETE",
                "The exact BAQ read did not confirm a complete result.",
            );
        }
        if root.is_empty() && matches!(spec.kind.as_str(), "BAQ" | "UBAQ") {
            return fail(
                "CUSTOM_BAQ_NOT_FOUND",
                "No custom BAQ matches this company and query ID. System BAQs are excluded.",
            );
        }
        if root.is_empty()
            && matches!(
                spec.kind.as_str(),
                "ClassicDefinition" | "ClassicPersonalization" | "ClassicDashboard"
            )
        {
            return fail(
                "CUSTOM_CLASSIC_NOT_FOUND",
                "No custom Classic definition matches this identity. System definitions are excluded.",
            );
        }
        if root.is_empty()
            && (crate::category_enabled("ud-schema") && spec.category == "ud-schema")
            && (crate::kind_enabled("UDTableSchema") && spec.kind == "UDTableSchema")
        {
            return crate::schema_children::start(request);
        }
        if root.len() != 1
            || !make_identity(spec, &root[0], &request.company)
                .ok()
                .zip(request.identity.as_ref())
                .is_some_and(|(actual, expected)| root_identity_matches(&actual, expected))
        {
            return fail(
                "IDENTITY_MISMATCH",
                "The returned Epicor root does not match the complete requested identity.",
            );
        }
        if crate::system_artifacts::marked(&root[0]) {
            return crate::system_artifacts::refused();
        }
        match query_owned(spec, &root[0]) {
            Ok(true) => {}
            Ok(false) => {
                return fail(
                    "QUERY_TYPE_MISMATCH",
                    "The Epicor Updatable classification belongs to the other query collection.",
                );
            }
            Err(diagnostic) => return GuestReply::Failure { diagnostic },
        }
        let missing: Vec<_> = spec
            .children
            .iter()
            .filter(|name| !definition.get(name.as_str()).is_some_and(Value::is_array))
            .cloned()
            .collect();
        let (mut queue, mut errors) = supplements(spec, definition, &request);
        if (crate::kind_enabled("ExtensionSet") && spec.kind == "ExtensionSet")
            && request.state["extensionExportAllowed"] != true
        {
            queue.clear();
            errors.push(Diagnostic::new("EXTENSION_EXPORT_MIXED_SYSTEM_SCOPE", "The Epicor extension export includes system-marked schema content. Independently filtered custom definitions and table identities are retained."));
        }
        errors.extend(crate::native_children::validate(
            &spec.service,
            &spec.root,
            definition,
            &spec.children,
        ));
        let state = json!({"phase":"extras","_hostCaller":request.state["_hostCaller"],"_pluginSettings":request.state["_pluginSettings"],"_pluginState":request.state["_pluginState"],"index":0,"definition":definition,"supplements":{},"errors":errors,"missingChildren":missing,"queue":queue});
        if let Some(extra) = queue.first() {
            return action(extra, &request, 1, state);
        }
        request.state = state;
        return finish(spec, request);
    }
    if phase == "extras" {
        let index = request
            .state
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let queue: Vec<Read> = match serde_json::from_value(request.state["queue"].clone()) {
            Ok(q) => q,
            Err(_) => {
                return fail(
                    "INVALID_PLUGIN_STATE",
                    "The supplemental read queue is invalid.",
                );
            }
        };
        let Some(extra) = queue.get(index) else {
            return fail(
                "INVALID_PLUGIN_STATE",
                "A supplement saved progress has an invalid index.",
            );
        };
        let mut state = request.state.clone();
        if let Some(error) = request.error.take() {
            state["errors"].as_array_mut().unwrap().push(json!(error));
        } else if let Some(response) = request.response.take() {
            if extra.export {
                state["errors"].as_array_mut().unwrap().extend(
                    crate::export_contracts::validate(
                        &extra.service,
                        &extra.method,
                        &extra.arguments,
                        &response,
                    )
                    .into_iter()
                    .map(|diagnostic| json!(diagnostic)),
                );
            }
            state["supplements"][if extra.label.is_empty() {
                &extra.method
            } else {
                &extra.label
            }] = response;
        } else {
            state["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(Diagnostic::new(
                    "SUPPLEMENT_UNVERIFIED",
                    "A supplement returned no Epicor response."
                )));
        }
        state["index"] = json!(index + 1);
        if let Some(next) = queue.get(index + 1) {
            return action(next, &request, 1, state);
        }
        request.state = state;
        return finish(spec, request);
    }
    fail(
        "INVALID_PLUGIN_STATE",
        "The Epicor collector saved progress is invalid.",
    )
}
fn supplements(
    spec: &Family,
    definition: &Value,
    request: &GuestRequest,
) -> (Vec<Read>, Vec<Diagnostic>) {
    if (crate::category_enabled("ud-schema") && spec.category == "ud-schema")
        && (crate::kind_enabled("UDTableSchema") && spec.kind == "UDTableSchema")
    {
        // Companion tables have their own inventory entries.
        return (vec![], vec![]);
    }
    let mut queue = Vec::new();
    let mut errors = Vec::new();
    for read in &spec.extras {
        let rows = match read.for_each.as_deref() {
            Some(table) => match definition.get(table).and_then(Value::as_array) {
                Some(rows) => rows.clone(),
                None => {
                    errors.push(Diagnostic::new(
                        "SUPPLEMENT_CHILDREN_UNVERIFIED",
                        "A supplement's declared child table is absent.",
                    ));
                    continue;
                }
            },
            None => vec![Value::Null],
        };
        for (index, row) in rows.into_iter().enumerate() {
            let mut context = request.clone();
            if !context.state.is_object() {
                context.state = json!({});
            }
            context.state["_row"] = row;
            match expand(&read.arguments, &context, 1) {
                Ok(arguments) => {
                    // Native ReportSvc.CollectReports splits PrintProgram locations on
                    // commas. ExportSsrsRdl itself reads only one exact item.
                    let paths =
                        if read.service == "Ice.BO.ReportSvc" && read.method == "ExportSsrsRdl" {
                            arguments["reportPath"]
                                .as_str()
                                .map(|value| {
                                    value
                                        .split(',')
                                        .filter(|path| !path.is_empty())
                                        .map(str::to_owned)
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        } else {
                            vec![]
                        };
                    let count = paths.len().max(1);
                    for location in 0..count {
                        let mut item = read.clone();
                        item.arguments = arguments.clone();
                        if let Some(path) = paths.get(location) {
                            item.arguments["reportPath"] = json!(path);
                        }
                        item.for_each = None;
                        if read.for_each.is_some() {
                            item.label = format!(
                                "{}/{}",
                                if read.label.is_empty() {
                                    &read.method
                                } else {
                                    &read.label
                                },
                                index
                            );
                            if count > 1 {
                                item.label.push_str(&format!("/{location}"));
                            }
                        }
                        queue.push(item);
                    }
                }
                Err(GuestReply::Failure { diagnostic }) => errors.push(diagnostic),
                _ => unreachable!(),
            }
        }
    }
    (queue, errors)
}
fn finish(spec: &Family, mut request: GuestRequest) -> GuestReply {
    if crate::kind_enabled("ExtensionSet")
        && spec.kind == "ExtensionSet"
        && request.state["extensionPackageComplete"] != true
    {
        if let Some(action) = extension_scope::package_start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }
    if (crate::kind_enabled("ProcessSet") && spec.kind == "ProcessSet")
        && request.state["phase"] != "automationParametersComplete"
    {
        if let Some(action) = automation_params::start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }
    if matches!(
        spec.kind.as_str(),
        "MethodDirectiveGroup" | "DataDirectiveGroup" | "UBAQDirectiveGroup"
    ) && request.state["phase"] != "bpmComplete"
    {
        if let Some(action) = crate::bpm_package::start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }
    if (crate::kind_enabled("ReportStyles") && spec.kind == "ReportStyles")
        && request.state["styleImagesComplete"] != true
    {
        if let Some(action) = report_images::start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }
    if (crate::kind_enabled("ReportStyles") && spec.kind == "ReportStyles")
        && request.state["phase"] != "reportRdlComplete"
    {
        if let Some(action) = crate::report_resources::start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }
    let mut diagnostics: Vec<Diagnostic> =
        serde_json::from_value(request.state["errors"].clone()).unwrap_or_default();
    if request.state["missingChildren"]
        .as_array()
        .is_some_and(|v| !v.is_empty())
    {
        diagnostics.push(Diagnostic::new(
            "CHILD_TABLES_MISSING",
            "One or more declared child tables are absent from the Epicor response.",
        ));
    }
    for gap in &spec.coverage {
        diagnostics.push(Diagnostic::new("COVERAGE_UNVERIFIED", gap));
    }
    // A registered method without directives is not an authored BPM artifact.
    if matches!(
        spec.kind.as_str(),
        "MethodDirectiveGroup" | "DataDirectiveGroup"
    ) && request.state["phase"] == "bpmComplete"
        && diagnostics.is_empty()
        && request.state["bpmScope"]["BpDirective"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        return GuestReply::Definition {
            identity: request.identity.unwrap(),
            format: "native-definition-and-supplements-json".into(),
            definition: Value::Null,
            completeness: Completeness::VerifiedAbsent,
            diagnostics,
        };
    }
    let mut definition = json!({"service":spec.service,"definition":request.state["definition"],"supplements":request.state["supplements"]});
    if crate::category_enabled("configurators") && spec.category == "configurators" {
        configurator_export::attach_sources(&mut definition);
        if spec.kind == "ConfiguratorLookup" {
            let id = request.identity.as_ref().unwrap();
            if let Err(diagnostic) = crate::configurator_lookup_archive::append(
                &id.keys["Company"], &id.keys["LookupTblID"], &mut definition,
            ) {
                diagnostics.push(diagnostic);
            }
        }
    }
    if matches!(
        spec.kind.as_str(),
        "MethodDirectiveGroup" | "DataDirectiveGroup"
    ) {
        crate::bpm_code::attach_method_export_owner(
            &mut definition,
            &request.state["definition"]["BpMethod"][0],
            &request.category,
            &request.company,
        );
    }
    if matches!(
        spec.kind.as_str(),
        "UBAQDirectiveGroup" | "MethodDirectiveGroup" | "DataDirectiveGroup"
    ) {
        let scope = request
            .state
            .get("bpmScope")
            .filter(|value| value.is_object())
            .unwrap_or(&request.state["definition"]);
        diagnostics.extend(crate::bpm_code::attach(&mut definition, scope));
    }
    GuestReply::Definition {
        identity: request.identity.unwrap(),
        format: "native-definition-and-supplements-json".into(),
        definition,
        completeness: if diagnostics.is_empty() {
            Completeness::Collected
        } else {
            Completeness::Partial
        },
        diagnostics,
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    fn request(spec: &Family, root: Value) -> GuestRequest {
        let identity = make_identity(spec, &root, "C").unwrap();
        let mut definition = json!({spec.root.clone():[root]});
        for table in &spec.children {
            definition[table] = json!([]);
        }
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: spec.category.clone(),
            company: "C".into(),
            identity: Some(identity),
            cursor: None,
            page_size: Some(200),
            state: json!({"phase":"root"}),
            response: Some(json!({"returnObj":definition,"parameters":{"morePages":false}})),
            error: None,
        }
    }

    #[test]
    fn wave1_exact_schema_requires_explicit_complete_paging() {
        let spec=specs().into_iter().find(|s|s.category=="ud-schema"&&s.kind=="UDTableSchema").unwrap();
        let r=request(&spec,json!({"SystemCode":"Ice","DataTableID":"UD_Test","SystemFlag":false}));
        for value in [json!(true),Value::Null,json!("false"),json!(0)] {
            let mut incomplete=r.clone();incomplete.response.as_mut().unwrap()["parameters"]["morePages"]=value;
            assert!(matches!(handle(incomplete),GuestReply::Failure{diagnostic} if diagnostic.code=="SCHEMA_READ_INCOMPLETE"));
        }
        assert!(!matches!(handle(r),GuestReply::Failure{diagnostic} if diagnostic.code=="SCHEMA_READ_INCOMPLETE"));
    }

    #[test]
    fn custom_resources_filter_stock_discovery_and_theme_children() {
        for spec in specs().into_iter().filter(|s| s.category == "custom-resources") {
            let clause = format!("whereClause{}", spec.root);
            assert_eq!(spec.list.method, "GetRows");
            assert!(spec.inventory_definition);
            assert!(
                spec.list.arguments[&clause]
                    .as_str()
                    .unwrap()
                    .contains("SystemFlag = false")
            );
            if spec.kind == "Theme" {
                let mut req = request(&spec, json!({"ThemeID":"Owner's theme","SystemFlag":false}));
                req.state = Value::Null;
                req.response = None;
                let GuestReply::Action {
                    action:
                        HostAction::NativeRead {
                            method, arguments, ..
                        },
                    ..
                } = root_action(&spec, &req)
                else {
                    panic!()
                };
                assert_eq!(method, "GetRows");
                assert_eq!(
                    arguments["whereClauseTheme"],
                    "ThemeID = 'Owner''s theme' AND SystemFlag = false"
                );
                assert_eq!(arguments["whereClauseThemeResource"], "SystemFlag = false");
            } else {
                for company in ["", "C"] {
                    let mut row = json!({"Company":company,"LangNameID":"HY","SystemFlag":false});
                    if spec.kind == "Translation" {
                        row["OrgTextID"] = json!(6);
                        row["ProgramID"] = json!("");
                    }
                    let req = request(&spec, row);
                    let GuestReply::Action {
                        action:
                            HostAction::NativeRead {
                                method, arguments, ..
                            },
                        ..
                    } = root_action(&spec, &req)
                    else {
                        panic!()
                    };
                    assert_eq!(method, "GetRows");
                    let predicate = arguments[format!("whereClause{}", spec.root)]
                        .as_str()
                        .unwrap();
                    assert!(predicate.contains(&format!("Company = '{company}'")));
                    assert!(predicate.contains("SystemFlag = false"));
                }
            }
        }
    }
    #[test]
    fn access_reads_filter_exact_keys_and_each_marked_child() {
        for spec in specs().into_iter().filter(|s| {
            s.category == "access-and-assignments" && !matches!(s.kind.as_str(), "ColumnSecurity" | "AccessScope")
        }) {
            assert_eq!(spec.list.method, "GetRows");
            assert_eq!(spec.list_table, spec.root);
            for company in ["", "C"] {
                let mut row = json!({"Company":company,"SystemFlag":false});
                for key in &spec.keys {
                    if key != "Company" {
                        row[key] = json!("Owner's value");
                    }
                }
                let mut req = request(&spec, row);
                req.state = Value::Null;
                req.response = None;
                let GuestReply::Action {
                    action:
                        HostAction::NativeRead {
                            method, arguments, ..
                        },
                    ..
                } = handle(req)
                else {
                    panic!()
                };
                assert_eq!(method, "GetRows");
                let predicate = arguments[format!("whereClause{}", spec.root)]
                    .as_str()
                    .unwrap();
                assert!(predicate.contains(&format!("Company = '{company}'")));
                assert!(predicate.contains("Owner''s value"));
                assert!(predicate.ends_with("AND SystemFlag = false"));
                for child in &spec.children {
                    assert_eq!(
                        arguments[format!("whereClause{child}")],
                        "SystemFlag = false"
                    );
                }
            }
        }
    }
    #[test]
    fn automation_marked_roots_are_filtered_before_body_read() {
        for kind in ["ProcessSet"] {
            let spec = specs().into_iter().find(|s| s.kind == kind).unwrap();
            let mut root = json!({"Company":"C","SystemFlag":false});
            for key in &spec.keys {
                if key != "Company" {
                    root[key] = json!("Owner's value");
                }
            }
            let mut req = request(&spec, root);
            req.state = Value::Null;
            req.response = None;
            let GuestReply::Action {
                action:
                    HostAction::NativeRead {
                        method, arguments, ..
                    },
                ..
            } = handle(req)
            else {
                panic!()
            };
            assert_eq!(method, "GetRows");
            let predicate = arguments[format!("whereClause{}", spec.root)]
                .as_str()
                .unwrap();
            assert!(predicate.ends_with("AND SystemFlag = false"));
            assert!(predicate.contains("Company = 'C'"));
            assert!(predicate.contains("Owner''s value"));
            assert!(
                spec.list.arguments[format!("whereClause{}", spec.root)]
                    .as_str()
                    .unwrap()
                    .ends_with("AND SystemFlag = false")
            );
        }
    }

    #[test]
    fn method_directive_native_read_filters_system_roots_before_storage() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "MethodDirectiveGroup")
            .unwrap();
        assert_eq!(spec.list.method, "GetRows");
        assert_eq!(
            spec.list.arguments["whereClauseBpMethod"],
            "Source = 'BO' AND SystemFlag = false"
        );
        assert_eq!(spec.list.arguments["whereClauseBpDirective"], "");
        let mut req = request(
            &spec,
            json!({"Source":"BO","BpMethodCode":"Ice.BO.Owner's.Read","SystemFlag":false}),
        );
        req.state = Value::Null;
        req.response = None;
        let GuestReply::Action {
            action: HostAction::NativeRead {
                method, arguments, ..
            },
            ..
        } = handle(req)
        else {
            panic!()
        };
        assert_eq!(method, "GetRows");
        assert_eq!(arguments["whereClauseBpDirective"], "");
        assert_eq!(
            arguments["whereClauseBpMethod"],
            "Source = 'BO' AND ([BpMethodCode] = 'Ice.BO.Owner''s.Read') AND SystemFlag = false"
        );
    }
    #[test]
    fn method_directive_identity_rejects_other_sources_before_native_reads() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "MethodDirectiveGroup")
            .unwrap();
        for source in ["DQ", "DB", "", "bo"] {
            let mut req = request(
                &spec,
                json!({"Source":"BO","BpMethodCode":"Ice.BO.UserCodes.GetByID","SystemFlag":false}),
            );
            req.state = Value::Null;
            req.response = None;
            req.identity
                .as_mut()
                .unwrap()
                .keys
                .insert("Source".into(), source.into());
            assert!(
                matches!(handle(req), GuestReply::Failure { diagnostic } if diagnostic.code == "SOURCE_MISMATCH")
            );
        }
    }
    #[test]
    fn query_discovery_owns_only_native_type_and_preserves_unknown_coverage() {
        for kind in ["BAQ", "UBAQ"] {
            let spec = specs().into_iter().find(|s| s.kind == kind).unwrap();
            assert!(
                spec.list.arguments["whereClause"]
                    .as_str()
                    .unwrap()
                    .contains(if kind == "UBAQ" {
                        "Updatable = true"
                    } else {
                        "Updatable = false"
                    })
            );
            let rows = json!([
                {"Company":"C","QueryID":"NameSuggestsUBAQ","SystemFlag":false,"Updatable":false},
                {"Company":"C","QueryID":"OrdinaryName","SystemFlag":false,"Updatable":true},
                {"Company":"C","QueryID":"Unknown","SystemFlag":false},
                {"Company":"C","QueryID":"Stock","SystemFlag":true,"Updatable":kind=="UBAQ"}
            ]);
            let mut req = request(&spec, rows[0].clone());
            req.operation = "discover".into();
            req.identity = None;
            req.response = Some(
                json!({"returnObj":{spec.list_table.clone():rows},"parameters":{"morePages":true}}),
            );
            let GuestReply::Inventory {
                identities,
                authoritative,
                diagnostics,
                next_cursor,
            } = inventory(req, vec![spec])
            else {
                panic!()
            };
            assert_eq!(identities.len(), 1);
            assert_eq!(
                identities[0].keys["QueryID"],
                if kind == "UBAQ" {
                    "OrdinaryName"
                } else {
                    "NameSuggestsUBAQ"
                }
            );
            assert!(!authoritative);
            assert!(next_cursor.is_some());
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code, "QUERY_TYPE_UNVERIFIED");
        }
    }
    #[test]
    fn report_locations_are_separate_native_single_item_reads() {
        let spec = specs()
            .into_iter()
            .find(|spec| crate::kind_enabled("ReportStyles") && spec.kind == "ReportStyles")
            .unwrap();
        let request = request(&spec, json!({"ReportID":"R","StyleNum":1}));
        let definition = json!({"ReportStyle":[{"ReportID":"R","StyleNum":1,"PrintProgram":"reports/A/Main,reports/B/Main"}]});
        let (queue, errors) = supplements(&spec, &definition, &request);
        assert!(errors.is_empty());
        let reads: Vec<_> = queue
            .iter()
            .filter(|read| read.method == "ExportSsrsRdl")
            .collect();
        assert_eq!(reads.len(), 2);
        assert_eq!(reads[0].arguments["reportPath"], "reports/A/Main");
        assert_eq!(reads[0].label, "ExportSsrsRdl/0/0");
        assert_eq!(reads[1].arguments["reportPath"], "reports/B/Main");
        assert_eq!(reads[1].label, "ExportSsrsRdl/0/1");
    }
}

#[cfg(test)]
mod custom_schema_tests {
    use super::*;
    #[test]
    fn ud_schema_native_schema_reads_filter_all_tables_and_escape_keys() {
        let id = Identity {
            category: "ud-schema".into(),
            kind: "UDTableSchema".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([
                ("SystemCode".into(), "I'ce".into()),
                ("DataTableID".into(), "Custom'Table".into()),
            ]),
        };
        let root = custom_schema_read(&id);
        assert_eq!(root.method, "GetRows");
        assert_eq!(
            root.arguments["whereClauseZDataTable"],
            "SystemCode = 'I''ce' AND DataTableID = 'Custom''Table' AND SystemFlag = false"
        );
        for table in [
            "ZLookupLink",
            "ZLinkColumn",
            "ZLookupField",
            "ZDataField",
            "ZKey",
            "ZKeyField",
        ] {
            assert_eq!(
                root.arguments[format!("whereClause{table}")],
                "SystemFlag = false"
            );
        }
        assert_eq!(root.arguments["pageSize"], 1);
        assert!(
            !methods("ud-schema")
                .iter()
                .any(|(service, method)| service == "Ice.BO.ZDataTableSvc" && method == "GetByID")
        );
    }
}

#[cfg(test)]
mod inventory_download_tests {
    use super::*;

    #[test]
    fn security_page_downloads_all_custom_roots_and_children_without_rereads() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "ObjectSecurity")
            .unwrap();
        let request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: spec.category.clone(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(200),
            state: json!({}),
            response: Some(json!({"returnObj":{
                "Security":[{"Company":"C","SecCode":"One","SystemFlag":false},
                    {"Company":"C","SecCode":"Two","SystemFlag":false},
                    {"Company":"C","SecCode":"Stock","SystemFlag":true}],
                "ObjectSecurityMethod":[{"Company":"C","SecCode":"Two","Description":"Keep ü & <text>","SystemFlag":false},
                    {"Company":"C","SecCode":"One","Description":"Keep this too","SystemFlag":false},
                    {"Company":"C","SecCode":"Stock","Description":"Exclude","SystemFlag":true}]
            },"parameters":{"morePages":true}})),
            error: None,
        };
        let GuestReply::InventoryData {
            records,
            inventory: next,
        } = inventory(request.clone(), vec![spec])
        else {
            panic!("Full definitions must survive discovery");
        };
        assert_eq!(records.len(), 2);
        assert!(matches!(
            *next,
            GuestReply::Inventory {
                next_cursor: Some(_),
                authoritative: true,
                ..
            }
        ));
        for (identity, data) in records {
            let mut acquire = request.clone();
            acquire.operation = "acquire".into();
            acquire.identity = Some(identity.clone());
            acquire.response = None;
            acquire.state = json!({"_inventory": data});
            let GuestReply::Definition {
                definition,
                completeness: Completeness::Collected,
                ..
            } = handle(acquire)
            else {
                panic!("No server request is needed after a full inventory page");
            };
            assert_eq!(
                definition["definition"]["Security"][0]["SecCode"],
                identity.keys["SecCode"]
            );
            let children = definition["definition"]["ObjectSecurityMethod"]
                .as_array()
                .unwrap();
            assert_eq!(children.len(), 1);
            assert_eq!(children[0]["SecCode"], identity.keys["SecCode"]);
            assert_eq!(
                children[0]["Description"],
                if identity.keys["SecCode"] == "Two" {
                    "Keep ü & <text>"
                } else {
                    "Keep this too"
                }
            );
        }
    }
}

#[cfg(test)]
mod schema_efficiency_tests {
    use super::*;
    #[test]
    fn schema_case_is_accepted_but_other_keys_and_scopes_are_not() {
        let a = Identity {
            category: "ud-schema".into(),
            kind: "UDTableSchema".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([
                ("SystemCode".into(), "ERP".into()),
                ("DataTableID".into(), "Example_UD".into()),
            ]),
        };
        let mut b = a.clone();
        b.keys.insert("SystemCode".into(), "Erp".into());
        assert!(root_identity_matches(&a, &b));
        b.keys.insert("SystemCode".into(), "Ice".into());
        assert!(!root_identity_matches(&a, &b));
        b = a.clone();
        b.keys.insert("DataTableID".into(), "Other_UD".into());
        assert!(!root_identity_matches(&a, &b));
        b = a.clone();
        b.scope = Scope::Company {
            company: "Other".into(),
        };
        assert!(!root_identity_matches(&a, &b));
    }
    #[test]
    fn ud_table_does_not_download_itself_as_a_companion() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "UDTableSchema")
            .unwrap();
        let r = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "ud-schema".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "ud-schema".into(),
                kind: "UDTableSchema".into(),
                scope: Scope::Global,
                keys: BTreeMap::from([
                    ("SystemCode".into(), "ERP".into()),
                    ("DataTableID".into(), "Example_UD".into()),
                ]),
            }),
            cursor: None,
            page_size: None,
            state: Value::Null,
            response: None,
            error: None,
        };
        assert!(supplements(&spec, &json!({}), &r).0.is_empty());
    }
}
