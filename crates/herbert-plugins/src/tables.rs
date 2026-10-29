//! Independent persisted-definition reads, including orphaned child definitions.
use herbert_contract::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[path = "classic_scope.rs"]
mod classic_scope;
#[path = "kinetic_registration.rs"]
mod kinetic_registration;
#[path = "report_behavior.rs"]
mod report_behavior;
#[path = "table_batches.rs"]
mod table_batches;
#[path = "personal_union.rs"]
mod personal_union;

pub(crate) fn personal_union(request: GuestRequest) -> GuestReply {
    personal_union::handle(request)
}

/// Quick Search's BO excludes predictive and foreign-country definitions.
/// Read the persisted parents and their children in bounded pages instead.
pub(crate) fn search_page(request: GuestRequest) -> GuestReply {
    let all = specs();
    let mut parent = all.iter().find(|s| s.category == "searches-and-navigation" && s.table == "QuickSearch")
        .expect("Searches and navigation QuickSearch specification").clone();
    parent.children = ["QuickSearchCriteria", "QuickSearchValueList"].into_iter().map(|table| Child {
        table: table.into(),
        links: ["Company", "GlbCompany", "QuickSearchID"].into_iter()
            .map(|key| (key.into(), key.into())).collect(),
        filters: BTreeMap::new(),
    }).collect();
    let mut selected=vec![parent];
    selected.extend(all.into_iter().filter(|s|s.kind=="PersistedContextMenuXml"));
    discovery(request, selected)
}

/// Native ContextMenu RowLoaded replaces its stored type code. Read the graph
/// without that display mutation so parent and child identities stay intact.
pub(crate) fn context_page(request: GuestRequest) -> GuestReply {
    let mut parent = specs().into_iter()
        .find(|s| s.category == "searches-and-navigation" && s.table == "ContextMenu")
        .expect("Searches and navigation ContextMenu specification");
    // Custom children can belong to a system parent. Filter complete graphs
    // after collection, keeping the keys of any required system parent.
    parent.filters.clear();
    parent.children = crate::search_archive::CONTEXT_CHILDREN.iter().map(|table| Child {
        table: (*table).into(),
        links: ["LikeID", "ContextTypeCode"].into_iter()
            .map(|key| (key.into(), key.into())).collect(),
        filters: BTreeMap::new(),
    }).collect();
    discovery(request, vec![parent])
}

// KineticGridLayout is a serialized options string, not a MetaFX layer. Native
// UserSettingsForView.SaveGridLayout writes Content unchanged. GetGridLayouts
// returns Key3 -> Content. Its owner is Key1, form is Key2, and grid is Key3.
// Exact rows/chunks and generic length/stream validation therefore preserve its
// source without inventing parent-layer or external-resource reads.

#[derive(Clone, Deserialize)]
struct Child {
    table: String,
    links: BTreeMap<String, String>,
    #[serde(default)]
    filters: BTreeMap<String, Value>,
}
#[derive(Clone, Deserialize)]
struct TableFamily {
    category: String,
    schema: String,
    table: String,
    kind: String,
    keys: Vec<String>,
    order: String,
    #[serde(default)]
    company: bool,
    #[serde(default)]
    owner_field: Option<String>,
    #[serde(default)]
    owner_required: bool,
    #[serde(default)]
    filters: BTreeMap<String, Value>,
    #[serde(default)]
    type_codes: Vec<String>,
    #[serde(default)]
    excluded_filters: BTreeMap<String, Vec<Value>>,
    #[serde(default)]
    children: Vec<Child>,
    #[serde(default)]
    discovery_fields: Vec<String>,
    #[serde(default)]
    discovery_filters: BTreeMap<String, Value>,
    #[serde(default)]
    coverage: Vec<String>,
}
fn specs() -> Vec<TableFamily> {
    let families: Vec<TableFamily> =
        serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/tables.json")))
            .expect("Reviewed table specifications must be valid");
    // The host query contract supports bound scalar equality, not IN arrays.
    // Expand native types into independent paged branches with the same identity
    // kind. This preserves existing saved identities and full source coverage.
    families
        .into_iter()
        .flat_map(|spec| {
            if spec.type_codes.is_empty() {
                return vec![spec];
            }
            spec.type_codes
                .iter()
                .map(|code| {
                    let mut branch = spec.clone();
                    branch.filters.insert("TypeCode".into(), json!(code));
                    branch
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod search_scope_tests {
    use super::*;
    #[test]
    fn menu_reads_preserve_global_and_company_keys_without_display_or_enabled_filters() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "PersistedMenu")
            .unwrap();
        for company in ["", "C"] {
            let id = identity(
                &spec,
                &json!({"Company":company,"MenuID":"Q'1","SysRowID":"owned"}),
                "C",
            )
            .unwrap();
            let req = GuestRequest {
                abi: 1,
                operation: "acquire".into(),
                category: "menus-and-launch-definitions".into(),
                company: "C".into(),
                identity: Some(id),
                cursor: None,
                page_size: None,
                state: Value::Null,
                response: None,
                error: None,
            };
            let GuestReply::Action {
                action: HostAction::Query { query },
                ..
            } = handle(req)
            else {
                panic!()
            };
            assert!(
                query
                    .criteria
                    .iter()
                    .any(|c| c.field == "Company" && query.parameters[&c.parameter] == company)
            );
            assert!(
                query
                    .criteria
                    .iter()
                    .any(|c| c.field == "MenuID" && query.parameters[&c.parameter] == "Q'1")
            );
            assert!(
                query
                    .criteria
                    .iter()
                    .any(|c| c.field == "SystemFlag" && query.parameters[&c.parameter] == false)
            );
            assert!(!query.criteria.iter().any(|c| matches!(
                c.field.as_str(),
                "Enabled" | "DoNotDisplayInMenu" | "OptionType" | "IsProtected"
            )));
            assert!(
                query
                    .fields
                    .iter()
                    .any(|f| f.alias == "HerbertArgumentsBytes")
            );
        }
    }
    #[test]
    fn persisted_searches_use_full_keys_and_filter_only_real_native_markers() {
        for spec in specs().into_iter().filter(|s| s.category == "searches-and-navigation") {
            let mut row = json!({"Company":"C","UserID":"another-owner","SysRowID":"owned-row"});
            for key in &spec.keys {
                if key != "Company" && key != "UserID" {
                    row[key] = json!("owned");
                }
            }
            for (key,value) in &spec.filters {row[key]=value.clone();}
            if let Some(owner)=&spec.owner_field {row[owner]=json!("another-owner");}
            let id = identity(&spec, &row, "C").unwrap();
            let req = GuestRequest {
                abi: 1,
                operation: "acquire".into(),
                category: "searches-and-navigation".into(),
                company: "C".into(),
                identity: Some(id),
                cursor: None,
                page_size: None,
                state: Value::Null,
                response: None,
                error: None,
            };
            let GuestReply::Action {
                action: HostAction::Query { query },
                ..
            } = crate::handle("searches-and-navigation", req)
            else {
                panic!()
            };
            for key in &spec.keys {
                assert!(
                    query
                        .criteria
                        .iter()
                        .any(|c| c.field == *key && query.parameters[&c.parameter] == row[key])
                );
            }
            let marked = !matches!(
                spec.table.as_str(),
                "ESSearchFields" | "NamedSearch" | "ControlSetting" | "WhereClause"
            );
            assert_eq!(
                query.criteria.iter().any(|c| c.table == "DefinitionSource"
                    && c.field == "SystemFlag"
                    && query.parameters[&c.parameter] == false),
                marked
            );
            if spec.table == "ESSearchFields" {
                assert_eq!(query.joins.len(), 3);
                assert!(query.criteria.iter().any(|c| c.table == "SearchOwner"
                    && c.field == "SystemFlag"
                    && query.parameters[&c.parameter] == false));
                assert!(
                    query
                        .tables
                        .iter()
                        .any(|t| t.alias == "SearchOwner" && t.table == "ESSearch")
                );
                assert!(query.joins.iter().all(|j| j.kind == "Inner"));
            }
            if spec.owner_field.is_some() {
                assert_eq!(
                    identity(&spec, &row, "C").unwrap().scope,
                    Scope::Personal {
                        company: "C".into(),
                        owner: "another-owner".into()
                    }
                );
            }
        }
    }
}

#[cfg(test)]
mod download_budget_tests {
    use super::*;
    #[test]
    fn one_menu_page_downloads_all_rows_without_per_menu_reads() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "PersistedMenu")
            .unwrap();
        let mut request = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "menus-and-launch-definitions".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(200),
            state: Value::Null,
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::Query { query },
            ..
        } = discovery(request.clone(), vec![spec.clone()])
        else {
            panic!()
        };
        assert_eq!(query.page_size, Some(200));
        assert!(query.fields.iter().any(|field| field.field == "*"));
        assert!(
            query
                .criteria
                .iter()
                .any(|c| c.field == "SystemFlag" && query.parameters[&c.parameter] == false)
        );
        request.response = Some(json!({"rows":[
            {"Company":"C","MenuID":"One","SysRowID":"1","SystemFlag":false,"Arguments":"preserve me","Enabled":false},
            {"Company":"C","MenuID":"Two","SysRowID":"2","SystemFlag":false,"Arguments":"Ω value"},
            {"Company":"C","MenuID":"Stock","SysRowID":"3","SystemFlag":true}
        ]}));
        let GuestReply::InventoryData { records, inventory } =
            discovery(request.clone(), vec![spec])
        else {
            panic!()
        };
        assert_eq!(records.len(), 2);
        assert!(matches!(
            *inventory,
            GuestReply::Inventory {
                authoritative: true,
                ..
            }
        ));
        for (identity, data) in records {
            request.operation = "acquire".into();
            request.identity = Some(identity);
            request.response = None;
            request.state = json!({"_inventory":data});
            let GuestReply::Definition {
                definition,
                completeness,
                ..
            } = handle(request.clone())
            else {
                panic!("Acquisition must reuse this downloaded row, without another request.")
            };
            assert_eq!(completeness, Completeness::Collected);
            assert_eq!(definition["Menu"][0], data["row"]);
        }
    }
}
pub fn supported(category: &str) -> bool {
    specs().iter().any(|s| s.category == category)
}
/// Exact table ownership keeps the persisted source separate from BO handlers.
pub fn owns(category: &str, kind: &str) -> bool {
    specs()
        .iter()
        .any(|s| s.category == category && s.kind == kind)
}
fn fail(code: &str, message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new(code, message),
    }
}
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
fn identity(spec: &TableFamily, row: &Value, company: &str) -> Result<Identity, GuestReply> {
    if spec.category == "classic-customizations" && !row["TypeCode"].as_str().is_some_and(classic_scope::owns) {
        return Err(fail(
            "SOURCE_MISMATCH",
            "The Epicor definition type does not belong to Classic customizations.",
        ));
    }
    if spec.category == "classic-customizations"
        && !row["TypeCode"]
            .as_str()
            .zip(spec.filters.get("TypeCode").and_then(Value::as_str))
            .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
    {
        return Err(fail(
            "SOURCE_MISMATCH",
            "The Epicor definition type does not match the requested discovery branch.",
        ));
    }
    let mut keys = BTreeMap::new();
    for key in &spec.keys {
        let Some(value) = row.get(key).and_then(scalar) else {
            return Err(fail(
                "IDENTITY_UNVERIFIED",
                "A saved definition lacks a complete Epicor ID.",
            ));
        };
        keys.insert(key.clone(), value);
    }
    if spec.kind == kinetic_registration::KIND && !kinetic_registration::valid_keys(&keys) {
        return Err(fail(
            "SOURCE_MISMATCH",
            "The Epicor row does not identify a user-defined Kinetic application registration.",
        ));
    }
    let scope = crate::personal_scope::scope(
        row,
        company,
        spec.owner_field.as_deref(),
        spec.owner_required,
    )
    .map_err(|diagnostic| GuestReply::Failure { diagnostic })?;
    Ok(Identity {
        category: spec.category.clone(),
        kind: spec.kind.clone(),
        scope,
        keys,
    })
}
fn reuse_inventory(spec: &TableFamily) -> bool {
    // These sources classify custom rows in the query itself. Other sources
    // still need their native parent classification before reading a body.
    matches!(
        spec.category.as_str(),
        // classic-dashboards is absent: dashboard_pages re-reads every selected root with DashBoardSvc.GetRows,
        // so its discovery page is key-only (tables.json discovery_fields; CLASSIC-DASHBOARDS.md).
        "classic-customizations" | "classic-personalizations" | "searches-and-navigation" | "menus-and-launch-definitions" | "report-behavior" | "configurators" | "business-rules-and-mappings" | "solutions-and-packages"
    ) || spec.category == "kinetic-personalizations"
        || crate::shell_layout::owns(&spec.kind)
        || crate::kinetic_persisted::owns(&spec.kind)
        || spec.kind == "PersistedKineticPersonalization"
        || spec.kind == kinetic_registration::KIND
}
fn query(
    spec: &TableFamily,
    request: &GuestRequest,
    table: &str,
    links: BTreeMap<String, Value>,
    discovery: bool,
    state: Value,
) -> GuestReply {
    let mut parameters = BTreeMap::new();
    let mut criteria = Vec::new();
    for (field, value) in links {
        let parameter = format!("key_{}", parameters.len());
        criteria.push(QueryCriterion {
            table: "DefinitionSource".into(),
            field,
            operator: "=".into(),
            parameter: parameter.clone(),
        });
        parameters.insert(parameter, value);
    }
    let filters = if table == spec.table {
        Some(&spec.filters)
    } else {
        spec.children
            .iter()
            .find(|child| child.table == table)
            .map(|child| &child.filters)
    };
    // Child ownership markers are independent of their parent. Apply the
    // reviewed descriptor filters before the host retains any child body.
    if let Some(filters) = filters {
        for (field, value) in filters {
            let parameter = format!("filter_{}", parameters.len());
            criteria.push(QueryCriterion {
                table: "DefinitionSource".into(),
                field: field.clone(),
                operator: "=".into(),
                parameter: parameter.clone(),
            });
            parameters.insert(parameter, value.clone());
        }
    }
    if table == spec.table {
        for (field, values) in &spec.excluded_filters {
            for value in values {
                let parameter = format!("exclude_{}", parameters.len());
                criteria.push(QueryCriterion {
                    table: "DefinitionSource".into(),
                    field: field.clone(),
                    operator: "<>".into(),
                    parameter: parameter.clone(),
                });
                parameters.insert(parameter, value.clone());
            }
        }
    }
    // These dashboard children have independent native ownership markers.
    // Filter at query execution, before the host retains their response.
    if (crate::category_enabled("classic-dashboards") && spec.category == "classic-dashboards")
        && matches!(table, "DashBdChunk" | "DashBdLike")
    {
        criteria.push(QueryCriterion {
            table: "DefinitionSource".into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "customDashboardChild".into(),
        });
        parameters.insert("customDashboardChild".into(), json!(false));
    }
    if discovery {
        // Inventory eligibility is distinct from explicit retrieval. An
        // explicitly requested system identity still reaches the native
        // ownership check, while ordinary discovery omits that identity.
        for (field, value) in &spec.discovery_filters {
            let parameter = format!("discovery_{}", parameters.len());
            criteria.push(QueryCriterion {
                table: "DefinitionSource".into(),
                field: field.clone(),
                operator: "=".into(),
                parameter: parameter.clone(),
            });
            parameters.insert(parameter, value.clone());
        }
        if request.page_size.is_none() {
            return fail("PAGE_SIZE_REQUIRED", "Supply a discovery page size.");
        }
        if let Some(after) = state.get("after").filter(|v| !v.is_null()) {
            criteria.push(QueryCriterion {
                table: "DefinitionSource".into(),
                field: spec.order.clone(),
                operator: ">".into(),
                parameter: "after".into(),
            });
            parameters.insert("after".into(), after.clone());
        }
    }
    let mut fields = vec![QueryField {
        table: "DefinitionSource".into(),
        field: "*".into(),
        alias: "*".into(),
        expression: None,
    }];
    if discovery && !reuse_inventory(spec) && !spec.discovery_fields.is_empty() {
        fields = spec
            .discovery_fields
            .iter()
            .map(|field| QueryField {
                table: "DefinitionSource".into(),
                field: field.clone(),
                alias: field.clone(),
                expression: None,
            })
            .collect();
    }
    if !discovery || reuse_inventory(spec) {
        if table == "CustomReportStoreHistory" {
            fields.push(QueryField {
                table: "DefinitionSource".into(),
                field: "PrintProgram".into(),
                alias: "HerbertContentBytes".into(),
                expression: Some(
                    "CONVERT(nvarchar(max), DATALENGTH(DefinitionSource.Content))".into(),
                ),
            });
        }
        let projections: &[(&str, &str)] = match table {
            "BpDirective" => &[("Body", "HerbertBodyBytes")],
            "ReportStyleRule" => &[("RuleBody", "HerbertRuleBodyBytes")],
            "PcFunctionDef" => &[
                ("Expression", "HerbertExpressionBytes"),
                ("ScriptExpression", "HerbertScriptExpressionBytes"),
            ],
            "XXXDef" if spec.kind == kinetic_registration::KIND => &[
                ("Content", "HerbertContentBytes"),
                ("SysCharacter01", "HerbertSecurityBytes"),
                ("SysCharacter02", "HerbertCreatorBytes"),
            ],
            "XXXDef" if (crate::category_enabled("kinetic-applications-and-layers") && spec.category == "kinetic-applications-and-layers") => &[
                ("Content", "HerbertContentBytes"),
                ("SysCharacter03", "HerbertDraftBytes"),
                ("SysCharacter01", "HerbertParentsBytes"),
            ],
            "XXXDef" => &[("Content", "HerbertContentBytes")],
            "DashBdDef" => &[
                ("DashboardSchema", "HerbertDashboardSchemaBytes"),
                ("DashboardAssembly", "HerbertDashboardAssemblyBytes"),
            ],
            "XXXChunk" | "DashBdChunk" => &[("Chunk", "HerbertChunkBytes")],
            "ShellLayout" => &[
                ("ShellHomePage", "HerbertHomeBytes"),
                ("ShellUserOptions", "HerbertOptionsBytes"),
                ("FavoriteItems", "HerbertFavoritesBytes"),
            ],
            "ShellLayoutPersonal" => &[("ShellHomePage", "HerbertHomeBytes")],
            "HomeUserOption" => &[("ShellUserOptions", "HerbertOptionsBytes")],
            "ControlSetting" => &[("ControlValue", "HerbertControlValueBytes")],
            "WhereClause" => &[("WhereClause", "HerbertWhereClauseBytes")],
            "WhereClauseBinding" => &[("WhereClauseString", "HerbertWhereClauseBytes")],
            "QuickSearchCriteria" => &[("CriteriaValue", "HerbertCriteriaValueBytes")],
            "QuickSearchValueList" => &[
                ("DisplayMember", "HerbertDisplayBytes"),
                ("ValueMember", "HerbertValueBytes"),
            ],
            "Menu" => &[
                ("Arguments", "HerbertArgumentsBytes"),
                ("Comment", "HerbertCommentBytes"),
                ("WebResourceURL", "HerbertWebUrlBytes"),
            ],
            _ => &[],
        };
        for (field, alias) in projections {
            // XML source needs a text metadata anchor. The expression itself
            // reads the original Body/RuleBody. Native metadata confirms both
            // PcFunctionDef expression columns are nvarchar (x(1000) is display).
            fields.push(QueryField { table: "DefinitionSource".into(), field: if matches!(table, "BpDirective" | "ReportStyleRule") { "Company".into() } else { (*field).into() }, alias: (*alias).into(),
                expression: Some(format!("CONVERT(nvarchar(max), DATALENGTH(CONVERT(nvarchar(max), DefinitionSource.{field})))")) });
        }
    }

    let mut source_tables = vec![QueryTable {
        schema: spec.schema.clone(),
        table: table.into(),
        alias: "DefinitionSource".into(),
        company_field: None,
        metadata: vec![],
    }];
    let mut joins = Vec::new();
    if (crate::category_enabled("searches-and-navigation") && spec.category == "searches-and-navigation") && table == "ESSearchFields" {
        // These children have no independent marker. Classify their exact
        // persisted parent in the same query before retaining field bodies.
        source_tables.push(QueryTable {
            schema: "Ice".into(),
            table: "ESSearch".into(),
            alias: "SearchOwner".into(),
            company_field: None,
            metadata: vec![],
        });
        for field in ["Company", "SearchID", "GlbCompany"] {
            joins.push(QueryJoin {
                left_table: "DefinitionSource".into(),
                left_field: field.into(),
                right_table: "SearchOwner".into(),
                right_field: field.into(),
                kind: "Inner".into(),
            });
        }
        criteria.push(QueryCriterion {
            table: "SearchOwner".into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "customSearchOwner".into(),
        });
        parameters.insert("customSearchOwner".into(), json!(false));
    }
    if spec.table == "ReportStyleRule" && table == spec.table {
        source_tables.push(QueryTable {
            schema: "Ice".into(),
            table: "ReportStyle".into(),
            alias: "RuleStyle".into(),
            company_field: None,
            metadata: vec![],
        });
        for field in ["Company", "ReportID", "StyleNum"] {
            joins.push(QueryJoin {
                left_table: "DefinitionSource".into(),
                left_field: field.into(),
                right_table: "RuleStyle".into(),
                right_field: field.into(),
                kind: "Inner".into(),
            });
        }
        criteria.push(QueryCriterion {
            table: "RuleStyle".into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "customRuleStyle".into(),
        });
        parameters.insert("customRuleStyle".into(), json!(false));
    }
    let query = QuerySpec {
        tables: source_tables,
        fields,
        joins,
        criteria,
        parameters,
        order: if table == spec.table {
            vec![QueryOrder {
                field: spec.order.clone(),
                descending: false,
            }]
        } else {
            vec![]
        },
        page_size: if discovery { request.page_size } else { None },
        page: 1,
    };
    GuestReply::Action {
        action: if matches!(
            spec.kind.as_str(),
            "PersistedKineticApplicationFile" | "PersistedReportStyleRule"
        ) {
            HostAction::SharedQuery { query }
        } else {
            HostAction::Query { query }
        },
        state,
    }
}
// Both report plugins consume the same rule bodies and length metadata.
pub(crate) fn report_rule_page(
    request: &GuestRequest,
    company: &str,
    after: Value,
    state: Value,
) -> GuestReply {
    let spec = specs()
        .into_iter()
        .find(|s| s.kind == "PersistedReportStyleRule")
        .unwrap();
    let reply = query(
        &spec,
        request,
        &spec.table,
        BTreeMap::from([("Company".into(), json!(company))]),
        true,
        json!({"after":after}),
    );
    match reply {
        GuestReply::Action { action, .. } => GuestReply::Action { action, state },
        other => other,
    }
}
// Use the same page for the standalone files and the native application package.
// ExportApp can omit files after its first 100 rows; the page supplies those bytes.
pub(crate) fn kinetic_file_page(
    request: &GuestRequest,
    company: &str,
    after: Value,
    state: Value,
) -> GuestReply {
    let spec = specs()
        .into_iter()
        .find(|s| s.kind == "PersistedKineticApplicationFile")
        .unwrap();
    let reply = query(
        &spec,
        request,
        &spec.table,
        BTreeMap::from([("Company".into(), json!(company))]),
        true,
        json!({"after":after}),
    );
    match reply {
        GuestReply::Action { action, .. } => GuestReply::Action { action, state },
        other => other,
    }
}
/// Read full Classic rows without the GenXData country-view suppression.
/// The caller verifies company and tenant scope from the authenticated session.
pub(crate) fn classic_page(request: &GuestRequest, company: &str, code: &str, after: Value, state: Value) -> GuestReply {
    let spec = specs().into_iter().find(|s| s.category == request.category && s.filters.get("TypeCode").and_then(Value::as_str) == Some(code)).unwrap();
    let mut links = BTreeMap::from([("Company".into(), json!(company))]);
    if let Some(id) = &request.identity {
        for (key, value) in &id.keys { links.insert(key.clone(), json!(value)); }
    }
    match query(&spec, request, &spec.table, links, true, json!({"after":after})) {
        GuestReply::Action { action, .. } => GuestReply::Action { action, state },
        other => other,
    }
}
/// Read the full persisted identity without the native country-view filter.
pub(crate) fn dashboard_page(request: &GuestRequest, company: &str, after: Value, state: Value, prefix: &str) -> GuestReply {
    let spec = specs().into_iter().find(|s| s.category == "classic-dashboards").unwrap();
    let mut links = BTreeMap::from([("Company".into(), json!(company))]);
    if let Some(id) = &request.identity {
        for (key, value) in &id.keys { links.insert(key.clone(), json!(value)); }
    }
    match query(&spec, request, &spec.table, links, true, json!({"after":after})) {
        GuestReply::Action { action: HostAction::Query { mut query }, .. } if request.identity.is_none() => {
            // Broad server-side superset of the literal DefinitionID prefix; dashboard_pages keeps the exact check.
            if let Some(stem) = crate::report_query::like_stem(prefix) {
                query.criteria.push(QueryCriterion { table: "DefinitionSource".into(), field: "DefinitionID".into(), operator: "LIKE".into(), parameter: "definitionPrefix".into() });
                query.parameters.insert("definitionPrefix".into(), json!(format!("{stem}%")));
            }
            GuestReply::Action { action: HostAction::Query { query }, state }
        }
        GuestReply::Action { action, .. } => GuestReply::Action { action, state },
        other => other,
    }
}
// Only the native Searches and navigation legacy graph reads system parents to find authored children.
// Keep the full graph until search_archive filters pure stock graphs and emits
// prerequisite parent keys. Every other persisted family keeps stock exclusion.
fn inventory_root_allowed(spec: &TableFamily, row: &Value) -> bool {
    if !crate::system_artifacts::marked(row) { return true; }
    let links: BTreeMap<String, String> = ["LikeID", "ContextTypeCode"].into_iter()
        .map(|key| (key.into(), key.into())).collect();
    row["SystemFlag"] == true
        && spec.category == "searches-and-navigation" && spec.schema == "Ice"
        && spec.table == "ContextMenu" && spec.kind == "PersistedContextMenu"
        && !spec.company && spec.keys == ["LikeID", "ContextTypeCode"]
        && spec.children.len() == crate::search_archive::CONTEXT_CHILDREN.len()
        && spec.children.iter().map(|child| child.table.as_str()).collect::<BTreeSet<_>>()
            == crate::search_archive::CONTEXT_CHILDREN.iter().copied().collect()
        && spec.children.iter().all(|child| child.links == links)
}

fn discovery(request: GuestRequest, selected: Vec<TableFamily>) -> GuestReply {
    let cursor = match request
        .cursor
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
    {
        Ok(v) => {
            v.unwrap_or(json!({"family":0,"global":false,"after":null,"classicTypesVersion":1}))
        }
        Err(_) => {
            return fail(
                "INVALID_CURSOR",
                "The saved-definition saved position is invalid.",
            );
        }
    };
    if request.category == "classic-customizations" && cursor["classicTypesVersion"] != 1 {
        return fail(
            "CLASSIC_CURSOR_UPGRADE_REQUIRED",
            "Restart Classic discovery to use the Epicor type branches. The old saved position cannot establish complete coverage.",
        );
    }
    let index = cursor.get("family").and_then(Value::as_u64).unwrap_or(0) as usize;
    let Some(spec) = selected.get(index) else {
        return fail(
            "INVALID_CURSOR",
            "The saved position names an unavailable definition family.",
        );
    };
    if request.state["phase"] == "inventoryChildren" {
        return table_batches::resume(spec, request);
    }
    let global = cursor
        .get("global")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tenant = request.category == "kinetic-personalizations" && cursor["tenant"] == true;
    let has_tenant = request.category == "kinetic-personalizations"
        && request.state["_personalTenant"].as_str().is_some_and(|s| !s.is_empty());
    let next_branch = || {
        if spec.company && !global {
            Some(
                json!({"family":index,"global":true,"after":null,"classicTypesVersion":1})
                    .to_string(),
            )
        } else if spec.company && has_tenant && !tenant {
            Some(json!({"family":index,"global":true,"tenant":true,"after":null,"classicTypesVersion":1}).to_string())
        } else if index + 1 < selected.len() {
            Some(
                json!({"family":index+1,"global":false,"after":null,"classicTypesVersion":1})
                    .to_string(),
            )
        } else {
            None
        }
    };
    if request.response.is_none() && request.error.is_none() {
        let links = if spec.company {
            BTreeMap::from([(
                "Company".into(),
                json!(if global && !tenant { "" } else { &request.company }),
            )])
        } else {
            BTreeMap::new()
        };
        return query(spec, &request, &spec.table, links, true, cursor);
    }
    if let Some(error) = request.error {
        return GuestReply::Inventory {
            identities: vec![],
            next_cursor: next_branch(),
            authoritative: false,
            diagnostics: vec![error],
        };
    }
    let response = request.response.as_ref().unwrap();
    let Some(rows) = response.get("rows").and_then(Value::as_array) else {
        return fail(
            "INVALID_NATIVE_RESPONSE",
            "The saved inventory omitted its Results rows.",
        );
    };
    let mut identities = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if !inventory_root_allowed(spec, row) {
            continue;
        }
        match identity(spec, row, &request.company) {
            Ok(id) => {
                if seen.insert(serde_json::to_string(&id).unwrap()) {
                    identities.push(id);
                } else {
                    diagnostics.push(Diagnostic::new(
                        "DUPLICATE_NATIVE_IDENTITY",
                        "The saved inventory repeats an exact definition identity.",
                    ));
                }
            }
            Err(GuestReply::Failure { diagnostic }) => diagnostics.push(diagnostic),
            _ => unreachable!(),
        }
    }
    if request.page_size.is_some_and(|n| rows.len() >= n as usize) {
        let after = rows
            .last()
            .and_then(|row| row.get(&spec.order))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        if after.is_none()
            || after == cursor["after"].as_str()
            || request.page_size.is_some_and(|n| rows.len() > n as usize)
        {
            diagnostics.push(Diagnostic::new("PAGING_UNVERIFIED", "The full inventory page has no advancing Epicor saved position. Empty or complete coverage cannot be established."));
            return GuestReply::Inventory {
                identities,
                next_cursor: next_branch(),
                authoritative: false,
                diagnostics,
            };
        }
    }
    let next_cursor = if request.page_size.is_some_and(|n| rows.len() == n as usize) {
        rows.last().and_then(|r| r.get(&spec.order)).map(|after| {
            json!({"family":index,"global":global,"tenant":tenant,"after":after,"classicTypesVersion":1})
                .to_string()
        })
    } else {
        next_branch()
    };
    let inventory = GuestReply::Inventory {
        identities,
        next_cursor,
        authoritative: diagnostics.is_empty(),
        diagnostics,
    };
    if reuse_inventory(spec) {
        let roots: Vec<_> = rows
            .iter()
            .filter(|row| {
                inventory_root_allowed(spec, row)
                    && identity(spec, row, &request.company).is_ok()
            })
            .cloned()
            .collect();
        if !spec.children.is_empty() && !roots.is_empty() {
            return table_batches::start(spec, request, &roots, inventory);
        }
        let records = rows
            .iter()
            .filter(|row| inventory_root_allowed(spec, row))
            .filter_map(|row| {
                identity(spec, row, &request.company)
                    .ok()
                    .map(|id| (id, json!({"row":row})))
            })
            .collect();
        GuestReply::InventoryData {
            records,
            inventory: Box::new(inventory),
        }
    } else {
        inventory
    }
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    let selected: Vec<_> = specs()
        .into_iter()
        .filter(|s| s.category == request.category)
        .collect();
    if request.operation == "discover" {
        return discovery(request, selected);
    }
    if request.operation != "acquire" {
        return fail(
            "UNSUPPORTED_OPERATION",
            "The collector supports definition discovery and exact-identity retrieval.",
        );
    }
    let Some(id) = request.identity.as_ref() else {
        return fail("IDENTITY_REQUIRED", "Select an exact definition identity.");
    };
    if request.category == "classic-customizations"
        && !id
            .keys
            .get("TypeCode")
            .is_some_and(|code| classic_scope::owns(code))
    {
        return fail(
            "SOURCE_MISMATCH",
            "The Epicor definition type does not belong to Classic customizations.",
        );
    }
    let Some(spec) = selected.iter().find(|s| {
        s.kind == id.kind
            && (s.type_codes.is_empty()
                || id
                    .keys
                    .get("TypeCode")
                    .zip(s.filters.get("TypeCode").and_then(Value::as_str))
                    .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected)))
    }) else {
        return fail(
            "INVALID_IDENTITY",
            "This category does not own the requested definition family.",
        );
    };
    if id.keys.keys().collect::<BTreeSet<_>>() != spec.keys.iter().collect::<BTreeSet<_>>() {
        return fail(
            "INVALID_IDENTITY",
            "The complete Epicor definition key is required.",
        );
    }
    if spec.kind == kinetic_registration::KIND && !kinetic_registration::valid_keys(&id.keys) {
        return fail(
            "SOURCE_MISMATCH",
            "The Epicor key does not identify a user-defined Kinetic application registration.",
        );
    }
    if id
        .keys
        .get("Company")
        .is_some_and(|c| !c.is_empty() && c != &request.company)
    {
        return fail(
            "SCOPE_MISMATCH",
            "The definition key belongs to another company.",
        );
    }
    let phase = request
        .state
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("initial")
        .to_string();

    if crate::category_enabled("report-behavior")
        && spec.table == "ReportStyleRule"
        && phase.starts_with("reportRule")
        && phase != "reportRuleScopeVerified"
    {
        if let Some(action) = report_behavior::complete(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
        return finish(spec, request);
    }
    if crate::category_enabled("report-styles-and-layouts")
        && matches!(
            phase.as_str(),
            "reportHistoryInventory" | "reportHistoryParse"
        )
    {
        if let Some(action) = crate::report_history::complete(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
        return finish(spec, request);
    }

    if crate::category_enabled("kinetic-personalizations")
        && crate::shell_layout::owns(&spec.kind)
        && phase.starts_with("shell")
        && phase != "shellComplete"
    {
        if let Some(action) = crate::shell_layout::complete(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
        return finish(spec, request);
    }
    if phase == "initial" || phase == "reportRuleScopeVerified" {
        if reuse_inventory(spec) && request.state["_inventory"]["row"].is_object() {
            let row = request.state["_inventory"]["row"].clone();
            request.state = json!({"phase":"root","_inventory":request.state["_inventory"],"_hostCaller":request.state["_hostCaller"]});
            request.response = Some(json!({"rows":[row]}));
            return handle(request);
        }
        let mut links: BTreeMap<_, _> =
            id.keys.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        if spec.company && !links.contains_key("Company") {
            links.insert(
                "Company".into(),
                json!(if matches!(id.scope, Scope::Global) {
                    ""
                } else {
                    &request.company
                }),
            );
        }
        return query(
            spec,
            &request,
            &spec.table,
            links,
            false,
            json!({"phase":"root","_hostCaller":request.state["_hostCaller"]}),
        );
    }
    if phase == "root" {
        if let Some(error) = request.error {
            return GuestReply::Failure { diagnostic: error };
        }
        let Some(rows) = request
            .response
            .as_ref()
            .and_then(|v| v.get("rows"))
            .and_then(Value::as_array)
        else {
            return fail(
                "ROOT_UNVERIFIED",
                "The saved definition omitted its root rows.",
            );
        };
        if rows.len() != 1
            || identity(spec, &rows[0], &request.company).ok().as_ref() != request.identity.as_ref()
        {
            return fail(
                "IDENTITY_MISMATCH",
                "The independent root does not match the complete requested identity.",
            );
        }
        if crate::system_artifacts::marked(&rows[0]) {
            return crate::system_artifacts::refused();
        }
        let mut state = json!({"phase":"children","index":0,"root":rows[0],"tables":{spec.table.clone():rows},"binaryFiles":request.response.as_ref().and_then(|v|v.get("binaryFiles")).cloned().unwrap_or(json!([])),"errors":[],"_hostCaller":request.state["_hostCaller"]});
        if let Some(children) = request.state["_inventory"]["children"].as_object() {
            state["errors"] = request.state["_inventory"]["errors"]
                .as_array()
                .map(|v| json!(v))
                .unwrap_or(json!([]));
            for child in &spec.children {
                if let Some(rows) = children.get(&child.table).and_then(Value::as_array) {
                    let errors = crate::persisted_rows::validate(
                        &child.table,
                        rows,
                        &state["root"],
                        &child.links,
                    );
                    state["errors"]
                        .as_array_mut()
                        .unwrap()
                        .extend(errors.into_iter().map(|error| json!(error)));
                    state["tables"][&child.table] = json!(rows);
                } else {
                    state["errors"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!(Diagnostic::new(
                            "CHILDREN_UNVERIFIED",
                            "The downloaded batch omitted a child table."
                        )));
                }
            }
            request.state = state;
            return finish(spec, request);
        }
        if let Some(child) = spec.children.first() {
            return child_action(spec, child, &request, state);
        }
        request.state = state;
        return finish(spec, request);
    }
    if phase == "children" {
        let index = request
            .state
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let Some(child) = spec.children.get(index) else {
            return fail(
                "INVALID_PLUGIN_STATE",
                "A child-table saved progress has an invalid index.",
            );
        };
        let mut state = request.state.clone();
        if let Some(error) = request.error.take() {
            state["errors"].as_array_mut().unwrap().push(json!(error));
        } else if let Some(rows) = request
            .response
            .as_ref()
            .and_then(|v| v.get("rows"))
            .and_then(Value::as_array)
        {
            let errors =
                crate::persisted_rows::validate(&child.table, rows, &state["root"], &child.links);
            state["errors"]
                .as_array_mut()
                .unwrap()
                .extend(errors.into_iter().map(|diagnostic| json!(diagnostic)));
            state["tables"][&child.table] = json!(rows);
        } else {
            state["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!(Diagnostic::new(
                    "CHILDREN_UNVERIFIED",
                    "A saved child read omitted its Results rows."
                )));
        }
        state["index"] = json!(index + 1);
        if let Some(next) = spec.children.get(index + 1) {
            return child_action(spec, next, &request, state);
        }
        request.state = state;
        return finish(spec, request);
    }
    fail(
        "INVALID_PLUGIN_STATE",
        "The saved-definition saved progress is invalid.",
    )
}
fn child_action(
    spec: &TableFamily,
    child: &Child,
    request: &GuestRequest,
    state: Value,
) -> GuestReply {
    let mut links = BTreeMap::new();
    for (field, parent) in &child.links {
        let Some(value) = state["root"].get(parent).filter(|value| !value.is_null()) else {
            return fail(
                "PARENT_KEY_UNVERIFIED",
                "A child read cannot bind its Epicor parent key.",
            );
        };
        links.insert(field.clone(), value.clone());
    }
    query(spec, request, &child.table, links, false, state)
}
fn finish(spec: &TableFamily, mut request: GuestRequest) -> GuestReply {
    if crate::category_enabled("report-behavior")
        && spec.table == "ReportStyleRule"
        && request.state["phase"] != "reportRuleComplete"
    {
        if let Some(action) = report_behavior::start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }
    if crate::category_enabled("kinetic-personalizations")
        && crate::shell_layout::owns(&spec.kind)
        && request.state["phase"] != "shellComplete"
    {
        if let Some(action) = crate::shell_layout::start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }

    if spec.table == "CustomReportStoreHistory" && request.state["phase"] != "reportHistoryComplete"
    {
        if let Some(action) = crate::report_history::start(&mut request) {
            return GuestReply::Action {
                action,
                state: request.state,
            };
        }
    }
    let mut diagnostics: Vec<Diagnostic> =
        serde_json::from_value(request.state["errors"].clone()).unwrap_or_default();
    if !crate::kinetic_persisted::owns(&spec.kind) {
        diagnostics.extend(
            spec.coverage
                .iter()
                .map(|g| Diagnostic::new("COVERAGE_UNVERIFIED", g)),
        );
    }
    let mut definition = request.state["tables"].clone();
    if spec.kind == kinetic_registration::KIND {
        kinetic_registration::append(&mut definition);
    }
    if spec.table == "DashBdDef" {
        let row = &definition["DashBdDef"][0];
        if row["HasDashboardAssembly"].as_bool() == Some(true)
            && row["DashboardAssembly"].as_str().is_none_or(str::is_empty)
            && definition["DashBdChunk"]
                .as_array()
                .is_none_or(|rows| rows.is_empty())
        {
            diagnostics.push(Diagnostic::new("CLASSIC_ASSEMBLY_UNVERIFIED", "The dashboard declares a deployed assembly, but neither the saved assembly field nor retained legacy chunks contain it."));
        }
    }
    // The collector declares source relationships. The host only supplies the
    // generic text-length and compressed-stream processing mechanics.
    let root_pointer = format!("/{}/0", spec.table);
    let lengths = match spec.table.as_str() {
        "PcFunctionDef" => vec![
            json!({"row":root_pointer,"field":"Expression","lengthField":"HerbertExpressionBytes"}),
            json!({"row":root_pointer,"field":"ScriptExpression","lengthField":"HerbertScriptExpressionBytes"}),
        ],
        "BpDirective" => {
            vec![json!({"row":root_pointer,"field":"Body","lengthField":"HerbertBodyBytes"})]
        }
        // Native exports contain the complete text, without query-only length columns.
        "ReportStyleRule" if definition.get("nativeReportStyleExport").is_some() => vec![],
        "ReportStyleRule" => vec![
            json!({"row":root_pointer,"field":"RuleBody","lengthField":"HerbertRuleBodyBytes"}),
        ],
        "XXXDef" if spec.kind == kinetic_registration::KIND => [
            ("Content", "HerbertContentBytes"),
            ("SysCharacter01", "HerbertSecurityBytes"),
            ("SysCharacter02", "HerbertCreatorBytes"),
        ]
        .into_iter()
        .filter(|(field, _)| definition["XXXDef"][0][*field].is_string())
        .map(|(field, length)| json!({"row":root_pointer,"field":field,"lengthField":length}))
        .collect(),
        "XXXDef" if (crate::category_enabled("kinetic-applications-and-layers") && spec.category == "kinetic-applications-and-layers") => [
            ("Content", "HerbertContentBytes"),
            ("SysCharacter03", "HerbertDraftBytes"),
            ("SysCharacter01", "HerbertParentsBytes"),
        ]
        .into_iter()
        .filter(|(field, _)| definition["XXXDef"][0][*field].is_string())
        .map(|(field, length)| json!({"row":root_pointer,"field":field,"lengthField":length}))
        .collect(),
        "XXXDef" => {
            vec![json!({"row":root_pointer,"field":"Content","lengthField":"HerbertContentBytes"})]
        }
        "ShellLayout" | "ShellLayoutPersonal" | "HomeUserOption" => [
            ("ShellHomePage", "HerbertHomeBytes"),
            ("ShellUserOptions", "HerbertOptionsBytes"),
            ("FavoriteItems", "HerbertFavoritesBytes"),
        ]
        .into_iter()
        .filter(|(field, _)| definition[&spec.table][0][*field].is_string())
        .map(|(field, length)| json!({"row":root_pointer,"field":field,"lengthField":length}))
        .collect(),
        "ControlSetting" => vec![
            json!({"row":root_pointer,"field":"ControlValue","lengthField":"HerbertControlValueBytes"}),
        ],
        "WhereClause" => vec![
            json!({"row":root_pointer,"field":"WhereClause","lengthField":"HerbertWhereClauseBytes"}),
        ],
        "WhereClauseBinding" => vec![
            json!({"row":root_pointer,"field":"WhereClauseString","lengthField":"HerbertWhereClauseBytes"}),
        ],
        "QuickSearchCriteria" => vec![
            json!({"row":root_pointer,"field":"CriteriaValue","lengthField":"HerbertCriteriaValueBytes"}),
        ],
        "QuickSearchValueList" => vec![
            json!({"row":root_pointer,"field":"DisplayMember","lengthField":"HerbertDisplayBytes"}),
            json!({"row":root_pointer,"field":"ValueMember","lengthField":"HerbertValueBytes"}),
        ],
        "Menu" => [
            ("Arguments", "HerbertArgumentsBytes"),
            ("Comment", "HerbertCommentBytes"),
            ("WebResourceURL", "HerbertWebUrlBytes"),
        ]
        .into_iter()
        .map(|(field, length)| json!({"row":root_pointer,"field":field,"lengthField":length}))
        .collect(),
        "DashBdDef" => vec![
            json!({"row":root_pointer,"field":"DashboardSchema","lengthField":"HerbertDashboardSchemaBytes"}),
            json!({"row":root_pointer,"field":"DashboardAssembly","lengthField":"HerbertDashboardAssemblyBytes"}),
        ],
        _ => vec![],
    };
    let streams: Vec<_> = spec.children.iter().filter(|child| matches!(child.table.as_str(), "XXXChunk" | "DashBdChunk"))
        .map(|child| json!({"rows":format!("/{}", child.table),"root":root_pointer,"links":child.links,"lengthField":"HerbertChunkBytes","encoding":"base64+bzip2-or-xml"})).collect();
    if !lengths.is_empty() || !streams.is_empty() {
        definition["herbertSources"] = json!({"textLengths":lengths,"chunkStreams":streams});
    }
    if (crate::category_enabled("classic-customizations") && spec.category == "classic-customizations") && spec.table == "XXXDef" {
        diagnostics.extend(crate::classic_sources::append_persisted_scripts(
            &mut definition,
        ));
    }
    if crate::kind_enabled("PersistedKineticPersonalization")
        && spec.kind == "PersistedKineticPersonalization"
    {}
    if crate::category_enabled("kinetic-applications-and-layers") && crate::kinetic_persisted::owns(&spec.kind) {
        crate::kinetic_persisted::append(&request.state, &mut definition);
    }
    if crate::category_enabled("report-behavior") && spec.table == "ReportStyleRule" {
        diagnostics.extend(report_behavior::attach(&mut definition));
    }
    if crate::category_enabled("kinetic-personalizations") && crate::shell_layout::owns(&spec.kind) {
        crate::shell_layout::append(&request.state, &mut definition);
    }
    GuestReply::Definition {
        identity: request.identity.unwrap(),
        format: "persisted-definition-tables-json".into(),
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
mod solution_type_scope_tests {
    use super::*;

    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: "solutions-and-packages".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(100),
            state: json!({}),
            response: None,
            error: None,
        }
    }

    #[test]
    fn solution_type_filters_system_at_source_for_discovery_and_exact_reads() {
        // Native Ice.SolutionType dictionary reports SystemFlag as non-null bit.
        // The host requires criterion metadata before executing the query; an
        // older dictionary without this field must fail, never read unfiltered.
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "PersistedSolutionType")
            .unwrap();
        for discovery in [true, false] {
            let links = if discovery {
                BTreeMap::new()
            } else {
                BTreeMap::from([("SysRowID".into(), json!("exact-row"))])
            };
            let GuestReply::Action {
                action: HostAction::Query { query },
                ..
            } = query(&spec, &request(), &spec.table, links, discovery, json!({}))
            else {
                panic!()
            };
            let guard = query
                .criteria
                .iter()
                .find(|c| c.field == "SystemFlag")
                .unwrap();
            assert_eq!(guard.table, "DefinitionSource");
            assert_eq!(guard.operator, "=");
            assert_eq!(query.parameters[&guard.parameter], false);
            assert_eq!(
                query
                    .criteria
                    .iter()
                    .any(|c| c.field == "SysRowID" && c.operator == "="),
                !discovery
            );
        }
    }

    #[test]
    fn solution_type_missing_schema_stays_incomplete_and_continues_next_family() {
        let selected: Vec<_> = specs()
            .into_iter()
            .filter(|s| s.category == "solutions-and-packages")
            .collect();
        let index = selected
            .iter()
            .position(|s| s.kind == "PersistedSolutionType")
            .unwrap();
        let mut r = request();
        r.cursor = Some(json!({"family":index,"global":false,"after":null}).to_string());
        r.error = Some(Diagnostic::new(
            "INVALID_QUERY",
            "A query criterion lacks native field metadata.",
        ));
        let GuestReply::Inventory {
            authoritative,
            identities,
            diagnostics,
            next_cursor,
        } = discovery(r, selected)
        else {
            panic!()
        };
        assert!(!authoritative);
        assert!(identities.is_empty());
        assert_eq!(diagnostics[0].code, "INVALID_QUERY");
        let next: Value = serde_json::from_str(&next_cursor.unwrap()).unwrap();
        assert_eq!(next["family"], index + 1);
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    #[test]
    fn exact_persisted_metadata_can_complete() {
        let spec = specs()
            .into_iter()
            .find(|s| s.kind == "PersistedSolutionType")
            .unwrap();
        let row = json!({"SysRowID":"native-row","SystemFlag":false,"Description":"custom values preserved"});
        let identity = identity(&spec, &row, "C").unwrap();
        let request = GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: spec.category.clone(),
            company: "C".into(),
            identity: Some(identity),
            cursor: None,
            page_size: Some(200),
            state: json!({"phase":"root"}),
            response: Some(json!({"rows":[row]})),
            error: None,
        };
        let GuestReply::Definition {
            completeness,
            diagnostics,
            definition,
            ..
        } = handle(request)
        else {
            panic!()
        };
        assert_eq!(completeness, Completeness::Collected);
        assert_eq!(
            definition["SolutionType"][0]["Description"],
            "custom values preserved"
        );
        assert!(diagnostics.is_empty());
    }
    #[test]
    fn report_history_discovery_uses_exact_keys_without_package_payloads() {
        let spec = specs()
            .into_iter()
            .find(|s| s.table == "CustomReportStoreHistory")
            .unwrap();
        let r = GuestRequest {
            abi: 1,
            operation: "discover".into(),
            category: "report-styles-and-layouts".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(200),
            state: json!({}),
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::Query { query: q },
            ..
        } = query(&spec, &r, &spec.table, BTreeMap::new(), true, json!({}))
        else {
            panic!()
        };
        assert_eq!(
            q.fields
                .iter()
                .map(|f| f.field.as_str())
                .collect::<Vec<_>>(),
            vec!["PrintProgram", "Version", "SysRowID"]
        );
        assert_eq!(q.order[0].field, "SysRowID");
        assert_eq!(q.tables[0].company_field, None);
    }
    #[test]
    fn retained_history_body_file_survives_table_continuation() {
        let spec = specs()
            .into_iter()
            .find(|s| s.table == "CustomReportStoreHistory")
            .unwrap();
        let row = json!({"PrintProgram":"/Root/reports/A/Main","Version":3,"SysRowID":"row","SysRevID":3,"ContentHash":"native","CreatedBy":"native","CreatedOn":"2026-10-17","Content":"base64 source retained","HerbertContentBytes":"22"});
        let id = identity(&spec, &row, "C").unwrap();
        let mut r = GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "report-styles-and-layouts".into(),
            company: "C".into(),
            identity: Some(id),
            cursor: None,
            page_size: Some(200),
            state: json!({}),
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::Query { query: q },
            state,
        } = handle(r.clone())
        else {
            panic!()
        };
        assert_eq!(
            q.fields
                .iter()
                .find(|f| f.alias == "HerbertContentBytes")
                .unwrap()
                .expression
                .as_deref(),
            Some("CONVERT(nvarchar(max), DATALENGTH(DefinitionSource.Content))")
        );
        r.state = state;
        r.response = Some(
            json!({"rows":[row],"binaryFiles":[{"pointer":"/rows/0/Content","file":{"path":"native/body.bin","bytes":22,"sha256":"abc","role":"original"}}]}),
        );
        let GuestReply::Action {
            action: HostAction::ParseXmlArchive { file, .. },
            state,
        } = handle(r)
        else {
            panic!()
        };
        assert_eq!(file.bytes, 22);
        assert_eq!(
            state["tables"]["CustomReportStoreHistory"][0]["Content"],
            "base64 source retained"
        );
    }
}

#[cfg(test)]
mod grid_contract_tests {
    use super::*;
    fn acquire(foreign_child: bool) -> (GuestReply, String) {
        let spec = specs()
            .into_iter()
            .find(|s| {
                s.category == "kinetic-personalizations"
                    && s.filters.get("TypeCode") == Some(&json!("KineticGridLayout"))
            })
            .unwrap();
        let content = "  {\"columns\":[{\"field\":\"PartNum\",\"title\":\"Part 😀\"}],\"options\":{\"group\":[]},\"embedded\":\"AAEC\"} \r\n".to_owned();
        let row = json!({"Company":"C","ProductID":"EP","TypeCode":"KineticGridLayout","Key1":"another-user","Key2":"Native.Form","Key3":"detailsGrid","CGCCode":"","SysRowID":"grid-physical-row","Content":content,"HerbertContentBytes":content.encode_utf16().count()*2});
        let id = identity(&spec, &row, "C").unwrap();
        let mut request = GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "kinetic-personalizations".into(),
            company: "C".into(),
            identity: Some(id),
            cursor: None,
            page_size: Some(200),
            state: json!({}),
            response: None,
            error: None,
        };
        let GuestReply::Action {
            action: HostAction::Query { query },
            state,
        } = handle(request.clone())
        else {
            panic!("grid root must use its persisted read")
        };
        assert_eq!(query.tables[0].table, "XXXDef");
        for key in [
            "Company",
            "ProductID",
            "TypeCode",
            "Key1",
            "Key2",
            "Key3",
            "CGCCode",
        ] {
            let criterion = query.criteria.iter().find(|c| c.field == key).unwrap();
            assert_eq!(query.parameters[&criterion.parameter], row[key]);
        }
        assert!(
            query
                .fields
                .iter()
                .any(|f| f.alias == "HerbertContentBytes")
        );
        request.state = state;
        request.response = Some(json!({"rows":[row]}));
        let GuestReply::Action {
            action: HostAction::Query { query },
            state,
        } = handle(request.clone())
        else {
            panic!("grid children must use the same full owner/form/grid key")
        };
        assert_eq!(query.tables[0].table, "XXXChunk");
        assert!(query.fields.iter().any(|f| f.alias == "HerbertChunkBytes"));
        let children = if foreign_child {
            let mut child = row.clone();
            child["Key1"] = json!("wrong-owner");
            child["SysRowID"] = json!("chunk-row");
            vec![child]
        } else {
            vec![]
        };
        request.state = state;
        request.response = Some(json!({"rows":children}));
        (handle(request), content)
    }
    #[test]
    fn opaque_grid_options_preserve_other_owner_full_key_body_and_processing_declarations() {
        let (reply, content) = acquire(false);
        let GuestReply::Definition {
            identity,
            definition,
            completeness,
            diagnostics,
            ..
        } = reply
        else {
            panic!("grid contract must not dispatch a MetaFX or resource request")
        };
        assert_eq!(
            identity.scope,
            Scope::Personal {
                company: "C".into(),
                owner: "another-user".into()
            }
        );
        assert_eq!(definition["XXXDef"][0]["Content"], content);
        assert_eq!(completeness, Completeness::Collected, "{diagnostics:?}");
        assert!(diagnostics.is_empty());
        assert_eq!(
            definition["herbertSources"]["textLengths"][0]["row"],
            "/XXXDef/0"
        );
        let stream = &definition["herbertSources"]["chunkStreams"][0];
        assert_eq!(stream["rows"], "/XXXChunk");
        assert_eq!(stream["root"], "/XXXDef/0");
        assert_eq!(stream["links"].as_object().unwrap().len(), 7);
        assert_eq!(stream["encoding"], "base64+bzip2-or-xml");
    }
    #[test]
    fn foreign_owner_chunk_still_prevents_grid_completion() {
        let (reply, _) = acquire(true);
        let GuestReply::Definition {
            definition,
            completeness,
            diagnostics,
            ..
        } = reply
        else {
            panic!()
        };
        assert_eq!(completeness, Completeness::Partial);
        assert_eq!(definition["XXXChunk"][0]["Key1"], "wrong-owner");
        assert!(
            diagnostics
                .iter()
                .any(|d| d.code == "CHILD_IDENTITY_MISMATCH")
        );
    }
}

#[cfg(test)]
mod classic_persisted_tests {
    use super::*;
    #[test]
    fn absent_declared_assembly_is_partial_but_undeployed_dashboard_can_complete() {
        let spec = specs().into_iter().find(|s| s.category == "classic-dashboards").unwrap();
        let mut row = json!({"Company":"C","ProductID":"EP","GlbCompany":"","DefinitionID":"D","CGCCode":"","SysRowID":"r","SysRevID":1,"DashboardSchema":"<schema/>","DashboardAssembly":"","HasDashboardAssembly":false});
        let id = identity(&spec, &row, "C").unwrap();
        for deployed in [false, true] {
            row["HasDashboardAssembly"] = json!(deployed);
            let request = GuestRequest {
                abi: 1,
                operation: "acquire".into(),
                category: "classic-dashboards".into(),
                company: "C".into(),
                identity: Some(id.clone()),
                cursor: None,
                page_size: None,
                state: json!({"phase":"children","root":row,"tables":{"DashBdDef":[row],"DashBdChunk":[],"DashBdBAQ":[],"DashBdLike":[]},"errors":[]}),
                response: None,
                error: None,
            };
            let GuestReply::Definition {
                completeness,
                diagnostics,
                definition,
                ..
            } = finish(&spec, request)
            else {
                panic!()
            };
            assert_eq!(
                completeness,
                if deployed {
                    Completeness::Partial
                } else {
                    Completeness::Collected
                }
            );
            assert_eq!(
                diagnostics
                    .iter()
                    .any(|d| d.code == "CLASSIC_ASSEMBLY_UNVERIFIED"),
                deployed
            );
            assert!(
                definition["herbertSources"]["textLengths"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|s| s["field"] == "DashboardAssembly")
            );
        }
    }
    #[test]
    fn dashboard_discovery_reads_only_key_fields() {
        // dashboard_pages re-reads each selected root with GetRows; discovery must not download bodies twice.
        let request = GuestRequest { abi: 1, operation: "discover".into(), category: "classic-dashboards".into(), company: "C".into(), identity: None, cursor: None, page_size: Some(16), state: json!({}), response: None, error: None };
        let GuestReply::Action { action, .. } = dashboard_page(&request, "C", Value::Null, json!({"phase":"inventory"}), "") else { panic!() };
        let text = serde_json::to_string(&action).unwrap();
        for body in ["DashboardSchema", "DashboardAssembly", "HerbertDashboardSchemaBytes", "\"*\""] { assert!(!text.contains(body), "{body} in {text}"); }
        for key in ["Company", "ProductID", "GlbCompany", "DefinitionID", "CGCCode", "SysRowID", "SystemFlag"] { assert!(text.contains(&format!("\"{key}\"")), "{key}"); }
    }
}

#[cfg(test)]
mod classic_classification_tests {
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
    fn row(code: &str) -> Value {
        json!({"Company":"C","ProductID":"EP","TypeCode":code,"Key1":"Kinetic-looking name","Key2":"Form","Key3":"","CGCCode":"GB","SysRowID":"row","SystemFlag":false})
    }
    #[test]
    fn downloaded_inventory_never_labels_kinetic_or_system_rows_as_classic() {
        for code in [
            "KNTCUDApp",
            "KNTCMetaUI",
            "KNTCCustLayer",
            "KNTCEventLayout",
            "KNTCLayerBackup",
            "KNTCPersLayer",
        ] {
            let mut r = request();
            r.response = Some(json!({"rows":[row(code)]}));
            let GuestReply::InventoryData { records, inventory } = handle(r) else {
                panic!()
            };
            assert!(records.is_empty());
            assert!(
                matches!(*inventory,GuestReply::Inventory{identities,..} if identities.is_empty())
            );
        }
        let mut r = request();
        let mut system = row("Customization");
        system["SystemFlag"] = json!(true);
        r.response = Some(json!({"rows":[system]}));
        let GuestReply::InventoryData { records, inventory } = handle(r) else {
            panic!()
        };
        assert!(records.is_empty());
        assert!(matches!(*inventory,GuestReply::Inventory{identities,..} if identities.is_empty()));
    }
    #[test]
    fn classic_failed_missing_and_nonadvancing_pages_are_not_authoritative() {
        let mut r = request();
        r.error = Some(Diagnostic::new("DENIED", "Synthetic refusal."));
        assert!(matches!(
            handle(r),
            GuestReply::Inventory {
                authoritative: false,
                next_cursor: Some(_),
                ..
            }
        ));
        let mut r = request();
        r.response = Some(json!({}));
        assert!(matches!(handle(r), GuestReply::Failure { .. }));
        for key in [Value::Null, json!("same")] {
            let mut r = request();
            r.page_size = Some(1);
            r.cursor = Some(
                json!({"family":0,"global":false,"after":"same","classicTypesVersion":1})
                    .to_string(),
            );
            let mut value = row("Customization");
            value["SysRowID"] = key;
            r.response = Some(json!({"rows":[value]}));
            assert!(
                matches!(handle(r),GuestReply::Inventory{authoritative:false,diagnostics,..} if diagnostics.iter().any(|d|d.code=="PAGING_UNVERIFIED"))
            );
        }
        let mut r = request();
        r.cursor = Some(json!({"family":0,"global":false,"after":"old"}).to_string());
        assert!(
            matches!(handle(r),GuestReply::Failure{diagnostic} if diagnostic.code=="CLASSIC_CURSOR_UPGRADE_REQUIRED")
        );
    }
}

#[cfg(test)]
mod persisted_cursor_completeness_tests {
    use super::*;

    fn row(spec: &TableFamily, name: &str, cursor: Value) -> Value {
        let mut row = json!({"SystemFlag": false});
        for field in &spec.keys {
            row[field] = json!(name);
        }
        if spec.company {
            row["Company"] = json!("C");
        }
        for (field, value) in &spec.filters {
            row[field] = value.clone();
        }
        row[&spec.order] = cursor;
        row
    }

    fn assert_incomplete_page(category: &str, rows: Vec<Value>, page_size: u32) {
        let selected: Vec<_> = specs()
            .into_iter()
            .filter(|spec| spec.category == category)
            .collect();
        let spec = &selected[0];
        // A broken continuation must not discard valid definitions already read.
        let expected: Vec<_> = rows
            .iter()
            .map(|row| identity(spec, row, "C").unwrap())
            .collect();
        assert!(rows.iter().all(|row| row["SystemFlag"] == false));
        let r = GuestRequest {
            abi: PLUGIN_ABI,
            operation: "discover".into(),
            category: category.into(),
            company: "C".into(),
            identity: None,
            cursor: Some(
                json!({"family":0,"global":false,"after":"previous","classicTypesVersion":1})
                    .to_string(),
            ),
            page_size: Some(page_size),
            state: json!({}),
            response: Some(json!({"rows":rows})),
            error: None,
        };
        let GuestReply::Inventory {
            identities,
            authoritative,
            next_cursor,
            diagnostics,
        } = discovery(r, selected.clone())
        else {
            panic!()
        };
        assert_eq!(
            identities, expected,
            "{category}: retain every valid earlier identity"
        );
        assert!(!authoritative, "{category}");
        assert_eq!(diagnostics.len(), 1, "{category}: {diagnostics:?}");
        assert_eq!(diagnostics[0].code, "PAGING_UNVERIFIED");
        let expected_next = spec.company || selected.len() > 1;
        assert_eq!(next_cursor.is_some(), expected_next);
        if let Some(next) = next_cursor {
            let cursor: Value = serde_json::from_str(&next).unwrap();
            assert!(cursor["after"].is_null());
            assert_eq!(cursor["classicTypesVersion"], 1);
            if spec.company {
                assert_eq!(cursor["family"], 0);
                assert_eq!(cursor["global"], true);
            } else {
                assert_eq!(cursor["family"], 1);
                assert_eq!(cursor["global"], false);
            }
        }
    }

    #[test]
    fn invalid_full_page_cursor_retains_identities_and_advances_independent_branch() {
        for category in ["classic-customizations", "kinetic-applications-and-layers", "report-styles-and-layouts"] {
            let spec = specs()
                .into_iter()
                .find(|spec| spec.category == category)
                .unwrap();
            for key in [
                Value::Null,
                json!("previous"),
                json!(""),
                json!(42),
                json!(false),
                json!([]),
                json!({}),
            ] {
                assert_incomplete_page(
                    category,
                    vec![
                        row(&spec, "earlier", json!("advancing-earlier")),
                        row(&spec, "last", key),
                    ],
                    2,
                );
            }
        }
    }

    #[test]
    fn oversized_page_with_advancing_cursor_retains_identities_but_is_incomplete() {
        for category in ["classic-customizations", "kinetic-applications-and-layers", "report-styles-and-layouts"] {
            let spec = specs()
                .into_iter()
                .find(|spec| spec.category == category)
                .unwrap();
            assert_incomplete_page(
                category,
                vec![
                    row(&spec, "first", json!("advancing-first")),
                    row(&spec, "second", json!("advancing-second")),
                    row(&spec, "last", json!("advancing-last")),
                ],
                2,
            );
        }
    }
}

#[cfg(test)]
mod kinetic_local_download_tests {
    use super::*;
    #[test]
    fn downloaded_personalizations_and_registrations_preserve_values_without_owner_reads() {
        for kind in [
            "PersistedKineticPersonalization",
            kinetic_registration::KIND,
        ] {
            let spec = specs().into_iter().find(|s| s.kind == kind).unwrap();
            let row = json!({"Company":"C","ProductID":"EP","TypeCode":spec.filters["TypeCode"],"Key1":"Authored","Key2":"View","Key3":"Desktop","CGCCode":"","SystemFlag":false,"SysRowID":"row","Content":"{\"keep\":\"Ω\"}","SysCharacter01":"[\"ParentReference\"]","SysCharacter02":"another-user","SysCharacter03":"Original draft or key","HerbertContentBytes":"24","HerbertSecurityBytes":"38","HerbertCreatorBytes":"24"});
            let id = identity(&spec, &row, "C").unwrap();
            if kind == "PersistedKineticPersonalization" {
                assert_eq!(
                    id.scope,
                    Scope::Personal {
                        company: "C".into(),
                        owner: "another-user".into()
                    }
                );
            }
            let mut r = GuestRequest {
                abi: PLUGIN_ABI,
                operation: "acquire".into(),
                category: spec.category.clone(),
                company: "C".into(),
                identity: Some(id),
                cursor: None,
                page_size: Some(200),
                response: None,
                error: None,
                state: json!({"_inventory":{"row":row,"children":{"XXXChunk":[]},"errors":[]}}),
            };
            let GuestReply::Definition {
                definition,
                completeness,
                ..
            } = handle(r.clone())
            else {
                panic!("The saved row must finish without a parent, owner or app download")
            };
            assert_eq!(completeness, Completeness::Collected);
            assert_eq!(definition["XXXDef"][0], row);
            if kind == kinetic_registration::KIND {
                assert_eq!(definition["applicationReference"]["ViewId"], "Authored");
            }
            r.state["_inventory"]["row"]["SystemFlag"] = json!(true);
            assert!(
                matches!(handle(r),GuestReply::Failure{diagnostic}if diagnostic.code=="SYSTEM_ARTIFACT_EXCLUDED")
            );
        }
    }
}
