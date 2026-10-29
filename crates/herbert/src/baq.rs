//! Build structured, read-only BAQs without saving or executing stored queries.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("INVALID_QUERY", message)
}
fn identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_alphanumeric() || c == '_')
}
pub fn build(spec: &QuerySpec, company: &str) -> Result<Value, Diagnostic> {
    if spec.tables.is_empty() || spec.fields.is_empty() {
        return Err(invalid(
            "An unsaved query requires tables and selected fields.",
        ));
    }
    let mut aliases = BTreeSet::new();
    let mut columns = BTreeMap::<(String, String), QueryColumn>::new();
    for table in &spec.tables {
        if !identifier(&table.schema)
            || !identifier(&table.table)
            || !identifier(&table.alias)
            || !aliases.insert(&table.alias)
        {
            return Err(invalid(
                "Epicor table identities and aliases must be valid and unique.",
            ));
        }
        for column in &table.metadata {
            if !identifier(&column.name) {
                return Err(invalid(
                    "Explicit metadata contains an invalid field identity.",
                ));
            }
            columns.insert((table.alias.clone(), column.name.clone()), column.clone());
        }
    }
    // An unsaved, executed-only query: the name is never persisted on the server.
    let query_id = format!("Herbert_{}", Uuid::new_v4().simple());
    let subquery = Uuid::new_v4().to_string();
    let base = json!({
        "Company": company,
        "QueryID": query_id,
        "SubQueryID": subquery,
        "RowMod": "A"
    });
    let row = |extra: Value| {
        let mut value = base.clone();
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        value
    };
    let mut tables = vec![];
    for (index, table) in spec.tables.iter().enumerate() {
        tables.push(row(json!({
            "TableID": table.alias,
            "DBSchemaName": table.schema,
            "DBTableName": table.table,
            "TableType": "DB",
            "Seq": index + 1,
            "IsVisibleInDesigner": true
        })));
    }
    tables.push(row(json!({
        "TableID": "Calculated",
        "DBSchemaName": "",
        "DBTableName": "",
        "TableType": "TT",
        "Seq": -1,
        "IsVisibleInDesigner": false
    })));
    let mut fields = vec![];
    let mut selected = BTreeMap::new();
    for field in &spec.fields {
        let table = spec
            .tables
            .iter()
            .find(|t| t.alias == field.table)
            .ok_or_else(|| invalid("A selected field refers to an unknown table alias."))?;
        let expanded = if field.field == "*" {
            if table.metadata.is_empty() {
                return Err(Diagnostic::new(
                    "FIELD_METADATA_REQUIRED",
                    "All-column extraction requires Epicor or explicit metadata. A one-field probe cannot establish a complete definition.",
                ));
            }
            table
                .metadata
                .iter()
                .map(|c| (c.name.clone(), c.name.clone()))
                .collect::<Vec<_>>()
        } else {
            vec![(field.field.clone(), field.alias.clone())]
        };
        for (name, alias) in expanded {
            if !identifier(&alias) || selected.contains_key(&alias) {
                return Err(invalid("Selected aliases must be valid and unique."));
            }
            let column = columns
                .get(&(table.alias.clone(), name.clone()))
                .ok_or_else(|| {
                    Diagnostic::new(
                        "FIELD_METADATA_REQUIRED",
                        "A selected field lacks its explicit/Epicor type contract.",
                    )
                })?;
            let mut value = row(json!({
                "TableID": table.alias,
                "FieldName": name,
                "DBSchemaName": table.schema,
                "DBTableName": table.table,
                "DBFieldName": name,
                "DataType": column.data_type,
                "FieldFormat": column.format,
                "Alias": alias,
                "FieldLabel": name,
                "Seq": fields.len() + 1,
                "IsCalculated": false,
                "Updatable": false
            }));
            if let Some(expression) = &field.expression {
                validate_expression(expression)?;
                value["TableID"] = json!("Calculated");
                value["FieldName"] = json!(alias);
                value["IsCalculated"] = json!(true);
                value["Formula"] = json!(expression);
            } else if column.data_type.eq_ignore_ascii_case("xml") {
                // SQL XML needs an explicit text projection to preserve authored
                // BPM/report bodies. nvarchar(max) is the native unbounded type.
                value["TableID"] = json!("Calculated");
                value["FieldName"] = json!(alias);
                value["IsCalculated"] = json!(true);
                value["Formula"] =
                    json!(format!("CONVERT(nvarchar(max), {}.{})", table.alias, name));
                value["DataType"] = json!("nvarchar");
                value["FieldFormat"] = json!("");
            }
            selected.insert(alias, (field.table.clone(), name));
            fields.push(value);
        }
    }
    let mut parameters = BTreeMap::new();
    let mut criteria = vec![];
    let mut used_parameters = std::collections::BTreeSet::new();
    for criterion in &spec.criteria {
        if !["=", "<>", ">", ">=", "<", "<=", "LIKE"].contains(&criterion.operator.as_str())
            || !identifier(&criterion.parameter)
        {
            return Err(invalid(
                "The query criterion operator or parameter name is invalid.",
            ));
        }
        let column = columns
            .get(&(criterion.table.clone(), criterion.field.clone()))
            .ok_or_else(|| invalid("A query criterion lacks Epicor field metadata."))?;
        let value = spec
            .parameters
            .get(&criterion.parameter)
            .ok_or_else(|| invalid("A criterion has no bound parameter value."))?;
        used_parameters.insert(criterion.parameter.as_str());
        let values = match value.as_array() {
            Some(values) if criterion.operator == "=" && !values.is_empty() => values.clone(),
            Some(_) => return Err(invalid("A list criterion requires equality and at least one value.")),
            None => vec![value.clone()],
        };
        for (index, value) in values.iter().enumerate() {
            if value.is_array() || value.is_object() || value.is_null() {
                return Err(invalid("A query parameter must contain scalar values."));
            }
            // Expand a list into bound equality predicates. Keep the group in
            // parentheses so surrounding company and ownership criteria still apply.
            let parameter = if values.len() == 1 { criterion.parameter.clone() }
                else { format!("HerbertList_{}_{}", criteria.len(), index) };
            if spec.parameters.contains_key(&parameter) && parameter != criterion.parameter {
                return Err(invalid("A query parameter conflicts with a generated list parameter."));
            }
            parameters.insert(parameter.clone(), (column.data_type.clone(), value.clone()));
            criteria.push(row(json!({
            "CriteriaID": Uuid::new_v4().to_string(),
            "TableID": criterion.table,
            "FieldName": criterion.field,
            "CompOp": criterion.operator,
            "RValue": format!("@{parameter}"),
            "IsConst": true,
            "CriteriaType": 2,
            "Seq": criteria.len() + 1,
            "AndOr": if criteria.is_empty() { "" } else if index == 0 { "And" } else { "Or" },
            "Neg": false,
            "LeftP": if values.len() > 1 && index == 0 { "(" } else { "" },
            "RightP": if values.len() > 1 && index + 1 == values.len() { ")" } else { "" },
            "UseHaving": false
        })));
        }
    }
    for table in &spec.tables {
        if let Some(field) = &table.company_field {
            let column = columns
                .get(&(table.alias.clone(), field.clone()))
                .ok_or_else(|| invalid("The company-scoped table lacks company-field metadata."))?;
            let name = format!("HerbertCompany_{}", table.alias);
            parameters.insert(name.clone(), (column.data_type.clone(), json!(company)));
            criteria.push(row(json!({
                "CriteriaID": Uuid::new_v4().to_string(),
                "TableID": table.alias,
                "FieldName": field,
                "CompOp": "=",
                "RValue": format!("@{name}"),
                "IsConst": true,
                "CriteriaType": 2,
                "Seq": criteria.len() + 1,
                "AndOr": if criteria.is_empty() { "" } else { "And" }
            })));
        }
    }
    if spec.parameters.keys().any(|k| !used_parameters.contains(k.as_str())) {
        return Err(invalid("The query contains unused bound parameters."));
    }
    // A join can name a selected calculated alias. Use its validated formula
    // and source-column type; native BAQs support expressions on either side.
    let join_field = |table: &str, name: &str| -> Result<(String, String, bool), Diagnostic> {
        let calculated = spec.fields.iter().find(|field| {
            field.table == table && field.alias == name && field.expression.is_some()
        });
        let source = calculated.map_or(name, |field| field.field.as_str());
        let column = columns
            .get(&(table.to_owned(), source.to_owned()))
            .ok_or_else(|| invalid("A join field lacks Epicor metadata."))?;
        if let Some(field) = calculated {
            let expression = field.expression.as_ref().unwrap();
            validate_expression(expression)?;
            Ok((expression.clone(), column.data_type.clone(), true))
        } else {
            Ok((name.to_owned(), column.data_type.clone(), false))
        }
    };
    let mut relations = vec![];
    let mut relation_fields = vec![];
    let mut relation_groups = BTreeMap::<(String, String), (String, String, usize)>::new();
    for join in &spec.joins {
        if !["Inner", "LeftOuter"].contains(&join.kind.as_str()) {
            return Err(invalid("Epicor join kind must be Inner or LeftOuter."));
        }
        let left = join_field(&join.left_table, &join.left_field)?;
        let right = join_field(&join.right_table, &join.right_field)?;
        // Native BAQs have one relation for a table pair, with all composite
        // equality terms attached to that relation. Two relations for the same
        // aliases produce an ambiguous native join graph.
        let pair = (join.left_table.clone(), join.right_table.clone());
        let (id, kind, count) = relation_groups.entry(pair).or_insert_with(|| {
            let id = Uuid::new_v4().to_string();
            relations.push(row(json!({
                "RelationID": id,
                "ParentTableID": join.left_table,
                "ChildTableID": join.right_table,
                "JoinType": join.kind,
                "IsFK": false,
                "OuterJoin": false
            })));
            (id, join.kind.clone(), 0)
        });
        if kind != &join.kind {
            return Err(invalid(
                "Join terms for the same table pair must use the same join kind.",
            ));
        }
        *count += 1;
        relation_fields.push(row(json!({
            "RelationID": id,
            "Seq": *count,
            "ParentFieldName": left.0,
            "ChildFieldName": right.0,
            "CompOp": "=",
            "ParentFieldDataType": left.1,
            "ChildFieldDataType": right.1,
            "ParentFieldIsExpr": left.2,
            "ChildFieldIsExpr": right.2,
            "AndOr": if *count == 1 { "" } else { "And" }
        })));
    }
    let mut order = vec![];
    for (i, sort) in spec.order.iter().enumerate() {
        let (table, field) = selected
            .get(&sort.field)
            .ok_or_else(|| invalid("A sort field is absent from selected fields."))?;
        order.push(row(
            json!({"TableID":table,"FieldName":field,"Seq":i+1,"IsAsc":!sort.descending}),
        ));
    }
    let mut sub = row(json!({"Name":"TopLevel","Type":"TopLevel","Seq":1}));
    if let Some(size) = spec.page_size {
        if size == 0 || size > i32::MAX as u32 {
            return Err(invalid(
                "pageSize must be a whole number from 1 to 2147483647.",
            ));
        }
        if spec.order.is_empty() {
            return Err(invalid("Paged discovery requires stable Epicor ordering."));
        }
        if spec.page != 1 {
            return Err(Diagnostic::new(
                "KEYSET_CURSOR_REQUIRED",
                "Use a bound keyset criterion for the next page. This query builder does not use offset paging.",
            ));
        }
        sub["SelectListClause"] = json!("Top");
        sub["TopRowExpr"] = json!(size.to_string());
    }
    let mut parameter_rows = vec![];
    let mut controls = vec![];
    let mut execution = vec![];
    for (name, (data_type, value)) in parameters {
        parameter_rows.push(json!({
            "Company": company,
            "QueryID": query_id,
            "ParameterID": name,
            "ParameterType": data_type,
            "ParameterLabel": "",
            "SkipIfEmpty": false,
            "RowMod": "A"
        }));
        controls.push(json!({
            "Company": company,
            "QueryID": query_id,
            "ControlID": Uuid::new_v4().to_string(),
            "DataSource": name,
            "DataType": data_type,
            "FieldFormat": "",
            "IsMandatory": true,
            "DefaultValue": "",
            "ControlType": "TextBox",
            "SourceType": 1,
            "Seq": controls.len() + 1,
            "RowMod": "A"
        }));
        let parameter_value = match &value {
            Value::String(text) => text.clone(),
            _ => value.to_string(),
        };
        execution.push(json!({
            "ParameterID": name,
            "ParameterValue": parameter_value,
            "ValueType": data_type,
            "IsEmpty": value.is_null(),
            "SysRowID": Uuid::new_v4().to_string(),
            "RowMod": "A"
        }));
    }
    Ok(json!({
        "queryDS": {
            "DynamicQuery": [{
                "Company": company,
                "QueryID": query_id,
                "Description": "Unsaved read-only Herbert definition query",
                "Updatable": false,
                "ExtQuery": false,
                "IsShared": false,
                "AllCompanies": false,
                "XCompany": false,
                "RowMod": "A"
            }],
            "QuerySubQuery": [sub],
            "QueryTable": tables,
            "QueryField": fields,
            "QueryWhereItem": criteria,
            "QueryParameter": parameter_rows,
            "QueryCtrl": controls,
            "QueryRelation": relations,
            "QueryRelationField": relation_fields,
            "QuerySortBy": order
        },
        "executionParams": { "ExecutionParameter": execution }
    }))
}
fn validate_expression(expression: &str) -> Result<(), Diagnostic> {
    if expression.contains(';')
        || expression.contains("--")
        || expression.contains("/*")
        || expression
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|t| {
                [
                    "INSERT", "UPDATE", "DELETE", "EXEC", "EXECUTE", "DROP", "ALTER", "CREATE",
                    "TRUNCATE", "MERGE",
                ]
                .contains(&t.to_ascii_uppercase().as_str())
            })
    {
        return Err(invalid(
            "A calculated expression cannot contain SQL statements or comments.",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bound_parameter_never_becomes_sql_and_query_stays_fixed_company() {
        let injection = "x'; DELETE FROM Ice.UDCodes;--";
        let spec = QuerySpec {
            tables: vec![QueryTable {
                schema: "Custom".into(),
                table: "PluginChosenTable".into(),
                alias: "L".into(),
                company_field: None,
                metadata: vec![QueryColumn {
                    name: "LibraryID".into(),
                    data_type: "nvarchar".into(),
                    format: "x(200)".into(),
                }],
            }],
            fields: vec![QueryField {
                table: "L".into(),
                field: "LibraryID".into(),
                alias: "LibraryID".into(),
                expression: None,
            }],
            joins: vec![],
            criteria: vec![QueryCriterion {
                table: "L".into(),
                field: "LibraryID".into(),
                operator: "=".into(),
                parameter: "library".into(),
            }],
            parameters: BTreeMap::from([("library".into(), json!(injection))]),
            order: vec![QueryOrder {
                field: "LibraryID".into(),
                descending: false,
            }],
            page_size: Some(10),
            page: 1,
        };
        let value = build(&spec, "A").unwrap();
        assert_eq!(
            value
                .pointer("/executionParams/ExecutionParameter/0/ParameterValue")
                .unwrap(),
            injection
        );
        assert_eq!(
            value.pointer("/queryDS/QueryWhereItem/0/RValue").unwrap(),
            "@library"
        );
        assert!(!value["queryDS"].to_string().contains(injection));
        assert_eq!(
            value
                .pointer("/queryDS/DynamicQuery/0/AllCompanies")
                .unwrap(),
            false
        );
        assert_eq!(
            value.pointer("/queryDS/DynamicQuery/0/Updatable").unwrap(),
            false
        );
        let mut bulk = spec.clone();
        bulk.parameters.insert("library".into(), json!(["First", injection]));
        bulk.tables[0].company_field = Some("LibraryID".into());
        let value = build(&bulk, "A").unwrap();
        let predicates = value["queryDS"]["QueryWhereItem"].as_array().unwrap();
        assert_eq!(predicates.len(), 3);
        assert_eq!(predicates[0]["LeftP"], "(");
        assert_eq!(predicates[1]["AndOr"], "Or");
        assert_eq!(predicates[1]["RightP"], ")");
        assert_eq!(predicates[2]["AndOr"], "And");
        assert!(!value["queryDS"].to_string().contains(injection));
        assert!(value["executionParams"]["ExecutionParameter"].as_array().unwrap().iter()
            .any(|row| row["ParameterValue"] == injection));
    }
    #[test]
    fn unsaved_query_names_use_the_product_prefix_and_no_development_codename() {
        let spec = QuerySpec {
            tables: vec![QueryTable { schema: "Erp".into(), table: "Part".into(), alias: "P".into(), company_field: Some("Company".into()),
                metadata: vec![QueryColumn { name: "Company".into(), data_type: "nvarchar".into(), format: "x(8)".into() }] }],
            fields: vec![QueryField { table: "P".into(), field: "Company".into(), alias: "Company".into(), expression: None }],
            joins: vec![], criteria: vec![], parameters: BTreeMap::new(), order: vec![], page_size: None, page: 1,
        };
        let value = build(&spec, "A").unwrap();
        let query_id = value.pointer("/queryDS/DynamicQuery/0/QueryID").and_then(Value::as_str).unwrap();
        assert!(query_id.starts_with("Herbert_"), "{query_id}");
        assert!(value["executionParams"]["ExecutionParameter"].as_array().unwrap().iter().any(|row| row["ParameterID"] == "HerbertCompany_P"),
            "{}", value["executionParams"]);
        assert!(!value.to_string().contains("Hoover"), "a development codename reached a native request");
    }
    #[test]
    fn joined_inventory_builds_one_query_with_bound_paging_and_custom_filter() {
        // Exercise the generic query builder independently of which transport
        // a current plugin selects for discovery.
        let mut query=QuerySpec {
            tables: vec![
                QueryTable{schema:"Ice".into(),table:"ReportStyleRule".into(),alias:"Rule".into(),company_field:Some("Company".into()),metadata:vec![]},
                QueryTable{schema:"Ice".into(),table:"ReportStyle".into(),alias:"Style".into(),company_field:Some("Company".into()),metadata:vec![]},
            ],
            fields:vec![QueryField{table:"Rule".into(),field:"*".into(),alias:"".into(),expression:None}],
            joins:["Company","ReportID","StyleNum"].into_iter().map(|field|QueryJoin{left_table:"Rule".into(),left_field:field.into(),right_table:"Style".into(),right_field:field.into(),kind:"Inner".into()}).collect(),
            criteria:vec![QueryCriterion{table:"Style".into(),field:"SystemFlag".into(),operator:"=".into(),parameter:"custom".into()}],
            parameters:BTreeMap::from([("custom".into(),json!(false))]),
            order:vec![QueryOrder{field:"SysRowID".into(),descending:false}],page_size:Some(200),page:1,
        };
        for table in &mut query.tables {
            let fields = if table.table == "ReportStyleRule" {
                vec![
                    ("Company", "nvarchar"),
                    ("ReportID", "nvarchar"),
                    ("StyleNum", "int"),
                    ("SysRowID", "uniqueidentifier"),
                    ("RuleBody", "nvarchar"),
                ]
            } else {
                vec![
                    ("Company", "nvarchar"),
                    ("ReportID", "nvarchar"),
                    ("StyleNum", "int"),
                    ("SystemFlag", "bit"),
                ]
            };
            table.metadata = fields
                .into_iter()
                .map(|(name, data_type)| QueryColumn {
                    name: name.into(),
                    data_type: data_type.into(),
                    format: String::new(),
                })
                .collect();
        }
        let built = build(&query, "C").unwrap();
        assert_eq!(
            built["queryDS"]["QueryRelation"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            built["queryDS"]["QueryRelationField"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(
            built["queryDS"]["QueryField"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["FieldName"] == "RuleBody")
        );
        assert!(
            built["queryDS"]["QueryWhereItem"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["FieldName"] == "SystemFlag")
        );
        let mut invalid = query.clone();
        invalid.joins[1].kind = "LeftOuter".into();
        assert!(build(&invalid, "C").is_err());
        query.fields[0].expression = Some("DELETE FROM Example".into());
        assert!(build(&query, "C").is_err());
    }
}
