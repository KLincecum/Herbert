//! Category dispatch and the ABI 1 byte bridge.
//! Each compiled module declares its own collection. The host has no category roster.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
mod configurator_bulk;
mod business_pages;
mod business_graph;
mod business_archive;
mod resource_pages;
mod configurator_pages;
mod configurator_output;
mod configurator_archive;
mod configurator_app;
mod configurator_app_bulk;
mod configurator_lookup_archive;
mod report_archive;
mod report_definition_pages;
mod report_query;
mod report_style_pages;
mod report_package_pages;
mod bpm_code;
mod bpm_package;
mod method_archive;
mod method_pages;
#[cfg(feature="ordinary-bpm-compiled")]
mod bpm_compiled;
mod catalog;
mod automation_pages;
mod access_pages;
mod access_archive;
mod function_archive;
mod function_binary;
#[cfg(feature="function-compiled")]
mod function_compiled;
mod function_pages;
mod query_download;
mod baq_archive;
mod baq_pages;
#[cfg(feature="ubaq-compiled")]
mod ubaq_compiled;
mod ubaq_rows;
mod baq_dependencies;
mod xml_dataset;
use catalog::CATALOG;
include!(concat!(env!("OUT_DIR"), "/selected_kinds.rs"));
#[inline(always)]
pub(crate) fn category_enabled(category: &str) -> bool {
    option_env!("HERBERT_COMPILED_CATEGORY").is_none_or(|selected| selected == category)
}

mod authored_schema;
mod classic_sources;
mod classic_archive;
mod classic_pages;
mod dashboard_archive;
mod dashboard_pages;
mod xml_document;
mod personalization_archive;
mod collectors;
mod export_contracts;
mod external_references;
mod families;
mod file_store_resources;
mod function_code;
mod kinetic;
mod kinetic_archive;
mod kinetic_pages;
mod kinetic_files;
mod kinetic_persisted;
mod kinetic_resources;
mod native_children;
mod persisted_rows;
mod personal_scope;
mod personal_pages;
mod personal_archive;
mod plugin_help;
mod readable_sources;
mod references;
mod reference_documents;
mod report_history;
mod report_resources;
mod report_store;
mod response_policy;
mod retained_solutions;
mod solution_pages;
mod programs_pages;
mod programs_graph;
mod schema_children;
mod schema_pages;
mod search_archive;
mod search_pages;
mod menu_pages;
mod shell_layout;
mod system_artifacts;
mod tables;

pub use references::extract as extract_references;

#[inline(always)]
pub fn handle(category: &str, request: GuestRequest) -> GuestReply {
    let mut reply = handle_request(category, request);
    readable_sources::attach(&mut reply);
    reply
}

#[inline(always)]
fn handle_request(category: &str, mut request: GuestRequest) -> GuestReply {
    let category = option_env!("HERBERT_COMPILED_CATEGORY").unwrap_or(category);
    if request.abi != PLUGIN_ABI
        || (request.operation != "descriptor" && request.category != category)
    {
        return failure(
            "PLUGIN_INCOMPATIBLE",
            "The guest ABI or category does not match this plugin.",
        );
    }
    #[cfg(feature="function-compiled")]
    if category=="C08" && request.state["_pluginSettings"].is_object(){crate::function_compiled::normalize(&mut request.state["_pluginSettings"]);}
    #[cfg(feature="ordinary-bpm-compiled")]
    if category=="C06"&&request.state["_pluginSettings"].is_object(){crate::bpm_compiled::normalize(&mut request.state["_pluginSettings"]);}
    #[cfg(feature="ubaq-compiled")]
    if category=="C04" && request.state["_pluginSettings"].is_object(){crate::ubaq_compiled::normalize(&mut request.state["_pluginSettings"]);}
    // The host resolves descriptor defaults and explicit plugin overrides.
    // Keep bare guest calls compatible when no plugin settings were supplied.
    if let Some(size) = request.state.pointer("/_pluginSettings/itemsPerPage").and_then(Value::as_u64) {
        if size == 0 || size > i32::MAX as u64 {
            return failure("INVALID_SETTING", "Page size must fit a positive native Int32.");
        }
        request.page_size = Some(size as u32);
    }
    if request.operation == "configure" {
        let declarations = match response_policy::configured_settings(category, &request) {
            Ok(settings) => settings,
            Err(diagnostic) => return GuestReply::Failure { diagnostic },
        };
        let values = match resolve_plugin_settings(
            &declarations,
            &request.state["values"],
        ) {
            Ok(values) => values,
            Err(diagnostic) => return GuestReply::Failure { diagnostic },
        };
        #[cfg(feature="function-compiled")]
        let values={let mut values=values;if category=="C08"{crate::function_compiled::normalize(&mut values);}values};
        #[cfg(feature="ordinary-bpm-compiled")]
        let values={let mut values=values;if category=="C06"{crate::bpm_compiled::normalize(&mut values);}values};
        #[cfg(feature="ubaq-compiled")]
        let values={let mut values=values;if category=="C04"{crate::ubaq_compiled::normalize(&mut values);}values};
        let mut portable = request.state["_pluginState"].clone();
        if !portable.is_object() {
            portable = json!({});
        }
        portable["config"] = values.clone();
        if let Some(selected) = request.state["selected"].as_bool() {
            portable["selected"] = json!(selected);
        }
        return GuestReply::State {
            plugin_state: portable,
            reply: Box::new(GuestReply::Definition {
                identity: Identity {
                    category: category.into(),
                    kind: "PluginConfiguration".into(),
                    scope: Scope::Global,
                    keys: BTreeMap::new(),
                },
                format: "plugin-configuration".into(),
                definition: values,
                completeness: Completeness::Collected,
                diagnostics: vec![],
            }),
        };
    }
    if request.operation == "descriptor" {
        let settings = match response_policy::configured_settings(category, &request) {
            Ok(settings) => settings,
            Err(diagnostic) => return GuestReply::Failure { diagnostic },
        };
        let Some((_, name, description)) = CATALOG.iter().find(|c| c.0 == category) else {
            return failure("UNKNOWN_CATEGORY", "Unknown plugin category.");
        };
        let available = ["C01", "C08"].contains(&category) || collectors::supported(category);
        let methods = match category {
            "C01" => vec![("Ice.BO.UserCodesSvc".into(), "GetRows".into())],
            "C02" => schema_pages::methods(),
            "C04" => baq_pages::methods(),
            "C06" => method_pages::methods(),
            "C08" => function_pages::methods(),
            "C09" | "C10" => classic_pages::methods(),
            "C11" => dashboard_pages::methods(),
            "C12" => kinetic_pages::methods(),
            "C13" => personal_pages::methods(),
            "C14" => search_pages::methods(),
            "C15" => menu_pages::methods(),
            "C16" => report_definition_pages::methods(),
            "C17" | "C18" => report_style_pages::methods(),
            "C20" => business_pages::methods(),
            "C21" => resource_pages::methods(),
            "C22" => programs_pages::methods(),
            "C24" => solution_pages::methods(),
            "C25" => automation_pages::methods(),
            "C26" => access_pages::methods(),
            _ => collectors::methods(category),
        };
        return GuestReply::Descriptor {
            response_policies: match category {
                "C12" => response_policy::kinetic_page_policies(&methods),
                "C15" => menu_pages::response_policies(&methods),
                "C17" | "C18" => report_style_pages::response_policies(&methods),
                "C20" => response_policy::business_graph_policies(&methods),
                "C21" => resource_pages::policies(),
                "C22" => programs_pages::policies(),
                "C24" => solution_pages::policies(),
                "C25" => automation_pages::policies(),
                "C26" => access_pages::policies(),
                _ => response_policy::for_methods(&methods),
            },
            binary_exports: if cfg!(feature="ubaq-compiled") && category=="C04" {
                #[cfg(feature="ubaq-compiled")] {let mut exports:Vec<BinaryExport>=serde_json::from_str(include_str!("binary-exports.json")).unwrap();exports.extend(ubaq_compiled::binary_exports());exports}
                #[cfg(not(feature="ubaq-compiled"))] {vec![]}
            } else if cfg!(feature="ordinary-bpm-compiled")&&category=="C06" {
                #[cfg(feature="ordinary-bpm-compiled")] {let mut exports:Vec<BinaryExport>=serde_json::from_str(include_str!("binary-exports.json")).unwrap();exports.extend(bpm_compiled::binary_exports());exports}
                #[cfg(not(feature="ordinary-bpm-compiled"))] {vec![]}
            } else if cfg!(feature="function-compiled") && category=="C08" {
                #[cfg(feature="function-compiled")] {let mut exports:Vec<BinaryExport>=serde_json::from_str(include_str!("binary-exports.json")).unwrap();exports.extend(function_compiled::binary_exports());exports}
                #[cfg(not(feature="function-compiled"))] {vec![]}
            } else if category=="C22" { serde_json::from_value(json!([{ "service":"Ice.BO.ExtensionSvc", "method":"GetExtensionSetAssembly", "pointer":"/returnObj/Assembly", "extension":"dll" },{ "service":"Ice.BO.ExtensionSvc", "method":"GetExtensionSetAssembly", "pointer":"/returnObj/Symbols", "extension":"pdb" },{ "service":"Ice.Lib.EcfToolsSvc", "method":"GetAssemblyBytes", "pointer":"/returnObj", "extension":"dll" }])).unwrap() } else if matches!(category,"C21"|"C26"|"C27") {vec![]} else {serde_json::from_str(include_str!("binary-exports.json"))
                .expect("Plugin binary declarations must be valid")},
            abi: PLUGIN_ABI,
            category: Category {
                id: category.into(),
                name: (*name).into(),
                description: (*description).into(),
                plugin_version: Some(env!("CARGO_PKG_VERSION").into()),
                available,
                selected: request
                    .state
                    .pointer("/_pluginState/selected")
                    .and_then(Value::as_bool)
                    // Personalizations remain available, but are opt-in (Kevin, 2026-10-25).
                    .unwrap_or(!matches!(category, "C10" | "C13")),
                qualification: if available {
                    "Collection results report completeness and native diagnostics."
                } else {
                    "This module does not provide an available collection route."
                }
                .into(),
                roles: if matches!(category, "C25" | "C26") || matches!(category, "C01" | "C02" | "C03" | "C04" | "C06" | "C08" | "C09" | "C10" | "C11" | "C12" | "C13" | "C14" | "C15" | "C16" | "C17" | "C18" | "C19" | "C20" | "C21" | "C22" | "C24") {
                    vec!["collector".into(), "pagedDefinitions".into(), "prefetchPages".into()]
                } else if category == "C27" {
                    vec!["collector".into(), "definitionProcessor".into(), "pagedDefinitions".into(), "prefetchPages".into()]
                } else {
                    vec!["collector".into()]
                },
                helpers: if category == "C17" {
                    vec![report_store::contract()]
                } else {
                    vec![]
                },
                artifacts: vec![],
                settings,
                help: plugin_help::sections(category),
            },
            methods,
            entities: if category == "C22" {
                vec![
                    ("Ice.BO.ExtensionSvc".into(), "ExtensionTables".into()),
                    ("Ice.BO.ExtensionSvc".into(), "ExtensionTableKeys".into()),
                ]
            } else if category == "C26" {
                access_pages::entities()
            } else {
                authored_schema::entities(category)
            },
        };
    }
    match category {
        "C04" => baq_pages::handle(request),
        "C06" => method_pages::handle(request),
        "C09" | "C10" => classic_pages::handle(request),
        "C11" => dashboard_pages::handle(request),
        // Retain local processing of an already downloaded persisted record.
        // New discovery uses the grouped bulk route below.
        "C12" if request.operation == "acquire" && request.state["_inventory"].is_object() => collectors::handle(request),
        "C12" => kinetic_pages::handle(request),
        "C13" => personal_pages::handle(request),
        "C14" => search_pages::handle(request),
        "C15" => menu_pages::handle(request),
        "C16" => report_definition_pages::handle(request),
        "C17" | "C18" => report_style_pages::handle(request),
        "C19" => configurator_output::handle(request),
        "C20" => business_pages::handle(request),
        "C21" => resource_pages::handle(request),
        "C22" => programs_pages::handle(request),
        "C24" => solution_pages::handle(request),
        "C25" => automation_pages::handle(request),
        "C26" => access_pages::handle(request),
        "C01" => user_codes(request),
        "C02" => schema_pages::handle(request),
        "C08" => functions(request),
        _ if collectors::supported(category) => collectors::handle(request),
        _ => failure(
            "CATEGORY_UNIMPLEMENTED",
            "The category has no implemented collector.",
        ),
    }
}
fn failure(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn body(value: &Value) -> &Value {
    value
        .get("returnObj")
        .or_else(|| value.get("ds"))
        .unwrap_or(value)
}
fn rows<'a>(v: &'a Value, name: &str) -> Option<&'a Vec<Value>> {
    body(v).get(name)?.as_array()
}
fn native(service: &str, method: &str, args: Value, export: bool, state: Value) -> GuestReply {
    GuestReply::Action {
        action: HostAction::NativeRead {
            service: service.into(),
            method: method.into(),
            arguments: args,
            export,
        },
        state,
    }
}
fn identity(category: &str, kind: &str, company: &str, key: &str, value: &str) -> Identity {
    Identity {
        category: category.into(),
        kind: kind.into(),
        scope: Scope::Company {
            company: company.into(),
        },
        keys: BTreeMap::from([(key.into(), value.into())]),
    }
}
fn required_identity(request: &GuestRequest, key: &str) -> Result<String, GuestReply> {
    let Some(id) = &request.identity else {
        return Err(failure(
            "IDENTITY_REQUIRED",
            "Select an exact native identity.",
        ));
    };
    if id.category != request.category || id.keys.len() != 1 {
        return Err(failure(
            "INVALID_IDENTITY",
            "The identity does not match this category's native key.",
        ));
    }
    match &id.scope {
        Scope::Company { company } if company == &request.company => {}
        Scope::Global if request.category == "C08" => {}
        _ => {
            return Err(failure(
                "SCOPE_MISMATCH",
                "The requested identity does not belong to the selected scope.",
            ));
        }
    }
    id.keys
        .get(key)
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| failure("INVALID_IDENTITY", "The exact native key is required."))
}
fn user_codes(request: GuestRequest) -> GuestReply {
    if let Some(e) = request.error {
        return GuestReply::Failure { diagnostic: e };
    }
    if request.operation == "discover" {
        // Use only this request's frozen settings. Missing selection in an old
        // request retains its original unfiltered meaning, never saved config.
        let prefix = match request.state.get("_pluginSettings")
            .and_then(|settings| settings.get(response_policy::CODE_TYPE_PREFIX_SETTING))
        {
            None => "",
            Some(Value::String(value)) => value.as_str(),
            Some(_) => return failure("INVALID_SETTING", "Code type ID starts with must be a string."),
        };
        let Ok(page) = request.cursor.as_deref().unwrap_or("1").parse::<u64>() else {
            return failure("INVALID_CURSOR", "The inventory cursor is invalid.");
        };
        let Some(size) = request.page_size else {
            return failure(
                "PAGE_SIZE_REQUIRED",
                "Supply a page size until native measurements establish the release default.",
            );
        };
        if request.response.is_none() {
            return native(
                "Ice.BO.UserCodesSvc",
                "GetRows",
                json!({"whereClauseUDCodeType":"SystemFlag = false","whereClauseUDCodes":"SystemFlag = false","pageSize":size,"absolutePage":page}),
                false,
                json!({"page":page}),
            );
        }
        let response = request.response.as_ref().unwrap();
        let Some(types) = rows(response, "UDCodeType") else {
            return failure(
                "INVALID_NATIVE_RESPONSE",
                "The UD Code discovery response does not contain its expected type table.",
            );
        };
        let mut ids = vec![];
        let mut seen = BTreeSet::new();
        for row in types {
            if system_artifacts::marked(row) {
                continue;
            }
            let Some(company) = row.get("Company").and_then(Value::as_str) else {
                return failure(
                    "SCOPE_MISMATCH",
                    "A UD Code type omitted its company scope.",
                );
            };
            if !company.is_empty() && company != request.company {
                return failure(
                    "SCOPE_MISMATCH",
                    "A UD Code type belongs to another company.",
                );
            }
            let Some(key) = row
                .get("CodeTypeID")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            else {
                return failure(
                    "INVALID_NATIVE_RESPONSE",
                    "A UD Code type lacks its exact identity.",
                );
            };
            if !seen.insert(key) {
                return failure(
                    "DUPLICATE_NATIVE_IDENTITY",
                    "Discovery returned a duplicate native type identity.",
                );
            }
            ids.push(identity(
                "C01",
                "UDCodeType",
                &request.company,
                "CodeTypeID",
                key,
            ));
        }
        let Some(more) = response
            .get("parameters")
            .and_then(|v| v.get("morePages"))
            .or_else(|| response.get("morePages"))
            .and_then(Value::as_bool)
        else {
            return failure(
                "PAGING_UNVERIFIED",
                "Native discovery omitted morePages; inventory completeness is unknown.",
            );
        };
        let Some(entries) = rows(response, "UDCodes") else {
            return failure(
                "CHILDREN_UNVERIFIED",
                "The UD Code batch omitted its code entries.",
            );
        };
        let mut records = Vec::new();
        let roots: BTreeMap<_, _> = types
            .iter()
            .filter(|row| !system_artifacts::marked(row))
            .map(|row| (row["CodeTypeID"].as_str().unwrap(), row))
            .collect();
        let mut grouped: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
        for entry in entries.iter().filter(|row| !system_artifacts::marked(row)) {
            let Some(key) = entry["CodeTypeID"]
                .as_str()
                .filter(|key| roots.contains_key(*key))
            else {
                return failure(
                    "IDENTITY_MISMATCH",
                    "A code entry does not belong to a type in this batch.",
                );
            };
            grouped.entry(key).or_default().push(entry);
        }
        for id in &ids {
            let key = &id.keys["CodeTypeID"];
            let root = roots[key.as_str()];
            let children = grouped.remove(key.as_str()).unwrap_or_default();
            let definition = json!({"UDCodeType":[root],"UDCodes":children});
            if let Err(error) = validate_user_code(&definition, key, &request.company) {
                return error;
            }
            // Validate every eligible root and child before selecting output.
            // Removing roots earlier would invent orphan errors for valid
            // unselected children and could hide invalid unselected graphs.
            if key.starts_with(prefix) {
                records.push((id.clone(), definition));
            }
        }
        ids.retain(|id| id.keys["CodeTypeID"].starts_with(prefix));
        let inventory = GuestReply::Inventory {
            identities: ids,
            next_cursor: more.then(|| (page + 1).to_string()),
            authoritative: true,
            diagnostics: vec![],
        };
        return GuestReply::InventoryData {
            records,
            inventory: Box::new(inventory),
        };
    }
    if request.operation != "acquire" {
        return failure(
            "UNSUPPORTED_OPERATION",
            "This plugin does not expose that operation.",
        );
    }
    if request
        .identity
        .as_ref()
        .is_some_and(|id| id.kind != "UDCodeType")
    {
        return failure("INVALID_IDENTITY", "Select a UD Code type identity.");
    }
    let key = match required_identity(&request, "CodeTypeID") {
        Ok(v) => v,
        Err(e) => return e,
    };
    if request.response.is_none() {
        return native(
            "Ice.BO.UserCodesSvc",
            "GetRows",
            // The exact type key admits one root. Filter both levels natively
            // so retained response files never contain system-marked rows.
            json!({"whereClauseUDCodeType":format!("CodeTypeID = '{}' AND SystemFlag = false", key.replace('\'', "''")),
                "whereClauseUDCodes":"SystemFlag = false", "pageSize":1, "absolutePage":1}),
            false,
            Value::Null,
        );
    }
    let response = request.response.as_ref().unwrap();
    if let Err(error) = validate_user_code(body(response), &key, &request.company) {
        return error;
    }
    let mut definition = body(response).clone();
    definition["UDCodes"]
        .as_array_mut()
        .unwrap()
        .retain(|row| !system_artifacts::marked(row));
    GuestReply::Definition {
        identity: request.identity.unwrap(),
        format: "native-tableset-json".into(),
        definition,
        completeness: Completeness::Collected,
        diagnostics: vec![],
    }
}

fn validate_user_code(definition: &Value, key: &str, requested_company: &str) -> Result<(), GuestReply> {
    let Some(types) = rows(definition, "UDCodeType") else {
        return Err(failure(
            "INVALID_NATIVE_RESPONSE",
            "The native definition lacks its type table.",
        ));
    };
    let Some(entries) = rows(definition, "UDCodes") else {
        return Err(failure(
            "CHILDREN_UNVERIFIED",
            "The native definition lacks its code-entry table.",
        ));
    };
    if types.is_empty() {
        return Err(failure(
            "CUSTOM_ARTIFACT_NOT_FOUND",
            "No custom UD Code type matched this identity. It may be missing or marked as system.",
        ));
    }
    if types.len() != 1 || types[0].get("CodeTypeID").and_then(Value::as_str) != Some(key)
    {
        return Err(failure(
            "IDENTITY_MISMATCH",
            "Native returned type does not match the requested identity.",
        ));
    }
    if system_artifacts::marked(&types[0]) {
        return Err(system_artifacts::refused());
    }
    let Some(company) = types[0].get("Company").and_then(Value::as_str) else {
        return Err(failure(
            "SCOPE_MISMATCH",
            "The native type omitted its company scope.",
        ));
    };
    if !company.is_empty() && company != requested_company {
        return Err(failure(
            "SCOPE_MISMATCH",
            "The native type belongs to another company.",
        ));
    }

    let mut seen = BTreeSet::new();
    for row in entries {
        if system_artifacts::marked(row) {
            continue;
        }
        if row.get("CodeTypeID").and_then(Value::as_str) != Some(key) {
            return Err(failure("IDENTITY_MISMATCH", "A code entry belongs to another type."));
        }
        let Some(code) = row
            .get("CodeID")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return Err(failure(
                "CHILDREN_UNVERIFIED",
                "A code entry lacks its native CodeID.",
            ));
        };
        if !seen.insert(code) {
            return Err(failure(
                "DUPLICATE_NATIVE_IDENTITY",
                "The native definition repeats a code-entry identity.",
            ));
        }
        if row.get("Company").and_then(Value::as_str) != Some(company) {
            return Err(failure(
                "SCOPE_MISMATCH",
                "A code entry has a missing or different company scope from its type.",
            ));
        }
    }
    Ok(())
}

fn functions(request: GuestRequest) -> GuestReply {
    if matches!(request.operation.as_str(), "discover" | "materialize") {
        return function_pages::handle(request);
    }
    if request.operation != "acquire" {
        return failure(
            "UNSUPPORTED_OPERATION",
            "This plugin does not expose that operation.",
        );
    }
    if !request
        .identity
        .as_ref()
        .is_some_and(|id| id.kind == "EfxLibrary" && matches!(id.scope, Scope::Global))
    {
        return failure(
            "INVALID_IDENTITY",
            "Functions require an exact global EfxLibrary identity.",
        );
    }
    function_pages::handle(request)
}
fn query_library(request: &GuestRequest) -> GuestReply {
    if request.page_size.is_none() {
        return failure("PAGE_SIZE_REQUIRED", "Supply a discovery page size.");
    }
    let mut parameters = BTreeMap::new();
    let mut criteria = Vec::new();
    if let Some(cursor) = &request.cursor {
        let after = serde_json::from_str::<Value>(cursor)
            .ok()
            .and_then(|value| value["after"].as_str().map(String::from));
        let Some(after) = after else {
            return failure(
                "INVALID_CURSOR",
                "The Function discovery continuation is invalid.",
            );
        };
        parameters.insert("after".into(), json!(after));
        criteria.push(QueryCriterion {
            table: "EcfSource".into(),
            field: "LibraryID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    GuestReply::Action {
        action: HostAction::Query {
            query: QuerySpec {
                tables: vec![QueryTable {
                    schema: "Ecf".into(),
                    table: "EfxLibrary".into(),
                    alias: "EcfSource".into(),
                    company_field: None,
                    metadata: vec![],
                }],
                fields: vec![QueryField {
                    table: "EcfSource".into(),
                    field: "*".into(),
                    alias: "*".into(),
                    expression: None,
                }],
                joins: vec![],
                criteria,
                parameters,
                order: vec![QueryOrder {
                    field: "LibraryID".into(),
                    descending: false,
                }],
                page_size: request.page_size,
                page: 1,
            },
        },
        state: json!({"phase":"discover"}),
    }
}
#[inline(always)]
pub fn dispatch(category: &str, bytes: &[u8]) -> Vec<u8> {
    let reply = match herbert_contract::decode_local_json::<GuestRequest>(bytes) {
        Ok(r) => handle(category, r),
        Err(_) => failure(
            "INVALID_PLUGIN_INPUT",
            "The request does not match the plugin contract.",
        ),
    };
    serde_json::to_vec(&reply).expect("Guest contract serialization is infallible")
}
#[macro_export]
macro_rules! category_plugin {
    ($id:literal) => {
        const _: () = {
            if let Some(selected) = option_env!("HERBERT_COMPILED_CATEGORY") {
                let a = selected.as_bytes();
                let b = $id.as_bytes();
                assert!(a.len() == b.len(), "Plugin build category mismatch");
                let mut i = 0;
                while i < a.len() {
                    assert!(a[i] == b[i], "Plugin build category mismatch");
                    i += 1;
                }
            }
        };
        fn main() {}
        #[unsafe(no_mangle)]
        pub extern "C" fn hoover_abi() -> u32 {
            herbert_contract::PLUGIN_ABI
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn hoover_alloc(len: u32) -> u32 {
            Box::into_raw(vec![0u8; len as usize].into_boxed_slice()) as *mut u8 as u32
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn hoover_free(ptr: u32, len: u32) {
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    ptr as *mut u8,
                    len as usize,
                )));
            }
        }
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn hoover_call(ptr: u32, len: u32) -> u64 {
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            let output = herbert_plugins::dispatch($id, bytes).into_boxed_slice();
            let length = output.len() as u32;
            let pointer = Box::into_raw(output) as *mut u8 as u32;
            ((pointer as u64) << 32) | length as u64
        }
    };
}

#[cfg(test)]
mod removed_operation_tests {
    use super::*;

    fn request(category: &str, operation: &str) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: operation.into(),
            category: category.into(),
            company: "TEST".into(),
            identity: None,
            cursor: None,
            page_size: Some(1),
            state: json!({}),
            response: None,
            error: None,
        }
    }

    #[test]
    fn removed_guest_operations_do_not_request_native_work() {
        for category in CATALOG.iter().map(|entry| entry.0) {
            for operation in ["inspect", "status"] {
                let reply = handle(category, request(category, operation));
                // The category router may reject a missing source identity before
                // it reaches a leaf. Neither rejection may request work or save state.
                assert!(
                    matches!(reply, GuestReply::Failure { .. }),
                    "{category}/{operation} unexpectedly produced {reply:?}"
                );
            }
        }

        // Exercise every collector branch directly so missing router identities
        // cannot hide an old inspect alias at the leaf.
        let branches: &[(&str, fn(GuestRequest) -> GuestReply)] = &[
            ("C01", user_codes),
            ("C08", functions),
            ("C02", families::handle),
            ("C02", tables::handle),
            ("C12", kinetic::handle),
            ("C22", external_references::handle),
            ("C21", file_store_resources::handle),
            ("C24", retained_solutions::handle),
        ];
        for (category, branch) in branches {
            for operation in ["inspect", "status"] {
                let reply = branch(request(category, operation));
                assert!(
                    matches!(&reply, GuestReply::Failure { diagnostic }
                    if diagnostic.code == "UNSUPPORTED_OPERATION"),
                    "{category}/{operation} must reject before collection validation: {reply:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod user_codes_tests;

#[cfg(test)]
mod sensitive_filter_settings_tests {
    use super::*;
    fn request(operation: &str, state: Value) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: operation.into(),
            category: "C25".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state,
            response: None,
            error: None,
        }
    }

    #[test]
    fn release_defaults_are_opt_in_and_preserve_saved_choices() {
        for id in ["C10", "C13", "C04", "C06", "C08"] {
            for saved in [None, Some(true), Some(false)] {
                let mut r=request("descriptor",json!({}));r.category=id.into();
                if let Some(selected)=saved { r.state=json!({"_pluginState":{"selected":selected}}); }
                let GuestReply::Descriptor{category,..}=handle(id,r) else {panic!()};
                assert_eq!(category.selected,saved.unwrap_or(!matches!(id,"C10"|"C13")));
                for setting in category.settings.iter().filter(|s|matches!(s.key(),"downloadCompiledAssemblies"|"decompileCompiledAssemblies")) {
                    assert_eq!(setting.default_value(),false,"{id}");
                }
            }
        }
    }

    #[test]
    fn filtering_defaults_off_and_portable_configuration_round_trips() {
        let GuestReply::Descriptor { category, .. } =
            handle("C25", request("descriptor", json!({})))
        else {
            panic!()
        };
        assert_eq!(category.settings.len(), 1);
        assert_eq!(category.settings[0].key(), "filterSensitiveFields");
        assert_eq!(category.settings[0].default_value(), false);
        let GuestReply::State { plugin_state, .. } = handle(
            "C25",
            request(
                "configure",
                json!({"values":{"filterSensitiveFields":true},"_pluginState":{"selected":true,"other":"retained"}}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(plugin_state["config"]["filterSensitiveFields"], true);
        assert_eq!(plugin_state["other"], "retained");
        let GuestReply::Descriptor { category, .. } = handle(
            "C25",
            request("descriptor", json!({"_pluginState":plugin_state})),
        ) else {
            panic!()
        };
        assert_eq!(category.settings[0].default_value(), true);
        let GuestReply::State { plugin_state, .. } = handle(
            "C25",
            request(
                "configure",
                json!({"selected":false,"values":{},"_pluginState":plugin_state}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(plugin_state["config"]["filterSensitiveFields"], true);
        assert_eq!(plugin_state["selected"], false);
        let GuestReply::State { plugin_state, .. } = handle(
            "C25",
            request(
                "configure",
                json!({"values":{"filterSensitiveFields":false},"_pluginState":plugin_state}),
            ),
        ) else {
            panic!()
        };
        assert_eq!(plugin_state["config"]["filterSensitiveFields"], false);
    }
    #[test]
    fn filtering_setting_requires_boolean() {
        assert!(matches!(
            handle(
                "C25",
                request(
                    "configure",
                    json!({"values":{"filterSensitiveFields":"true"}})
                )
            ),
            GuestReply::Failure { .. }
        ));
    }
}
