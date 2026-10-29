//! Build unsaved, read-only report BAQs. Field names come from the native
//! dictionary; values are quoted separately from validated identifiers.
use serde_json::{Value, json};
pub fn literal(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
}
/// A literal ID prefix's leading run before any LIKE wildcard or bracket character (%, _, [, ]), for a broad
/// server-side `LIKE 'stem%'` superset that needs no escape syntax. None when nothing narrows the read. Callers keep
/// their exact case-sensitive local `starts_with` check; the superset only stops unselected rows reaching the guest.
pub fn like_stem(prefix: &str) -> Option<&str> {
    let stem = &prefix[..prefix.find(['%', '_', '[', ']']).unwrap_or(prefix.len())];
    (!stem.is_empty()).then_some(stem)
}
/// `" AND {field} LIKE 'stem%'"` for a GetRows where clause, or empty when the prefix does not narrow.
pub fn like_clause(field: &str, prefix: &str) -> String {
    like_stem(prefix).map(|stem| format!(" AND {field} LIKE '{}%'", stem.replace('\'', "''"))).unwrap_or_default()
}
pub fn identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
pub fn build(
    company: &str,
    table: &str,
    fields: &Value,
    size: u32,
    criteria: &[(String, String, String, String, String, String)],
    after: Option<&str>,
) -> Result<Value, String> {
    if !identifier(table) || size == 0 || size > i32::MAX as u32 {
        return Err("Invalid native report table or page size.".into());
    }
    let row = |extra: Value| {
        let mut row = json!({"Company":company,"QueryID":"HerbertReportRead","SubQueryID":"00000000-0000-0000-0000-000000000001","RowMod":"A"});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    };
    let mut ds = json!({"DynamicQuery":[{"Company":company,"QueryID":"HerbertReportRead","Description":"Unsaved report backup read","Updatable":false,"ExtQuery":false,"IsShared":false,"AllCompanies":false,"XCompany":true,"RowMod":"A"}],"QuerySubQuery":[row(json!({"Name":"TopLevel","Type":"TopLevel","Seq":1,"OrderByOffSet":"0","OrderByFetch":size.to_string()}))],"QueryTable":[row(json!({"TableID":"R","DBSchemaName":"Ice","DBTableName":table,"TableType":"DB","Seq":1,"IsVisibleInDesigner":true})),row(json!({"TableID":"Calculated","TableType":"TT","Seq":-1,"IsVisibleInDesigner":false}))],"QueryField":[],"QueryWhereItem":[],"QuerySortBy":[row(json!({"TableID":"R","FieldName":"SysRowID","Seq":1,"IsAsc":true}))],"QueryParameter":[],"QueryCtrl":[],"QueryRelation":[],"QueryRelationField":[]});
    for (i, f) in fields
        .as_array()
        .ok_or("The report field dictionary is missing.")?
        .iter()
        .enumerate()
    {
        let name = f["FieldName"]
            .as_str()
            .ok_or("Missing report field name.")?;
        let ty = f["DataType"].as_str().ok_or("Missing report field type.")?;
        if !identifier(name) {
            return Err("Invalid report field name.".into());
        }
        let formula = match ty {
            "timestamp" => format!("CONVERT(bigint,R.{name})"),
            "nvarchar" | "xml" => format!("CONVERT(nvarchar(max),R.{name})"),
            _ => format!("R.{name}"),
        };
        ds["QueryField"].as_array_mut().unwrap().push(row(json!({"TableID":"Calculated","FieldName":name,"Alias":name,"DataType":if ty=="timestamp"{"bigint"}else if ty=="xml"{"nvarchar"}else{ty},"FieldFormat":"","FieldLabel":name,"Seq":i+1,"IsCalculated":true,"Formula":formula,"Updatable":false})));
    }
    let mut predicates = criteria.to_vec();
    if let Some(after) = after {
        if after.len() != 36
            || !after.bytes().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
        {
            return Err("Invalid report row continuation.".into());
        }
        predicates.push((
            "SysRowID".into(),
            ">".into(),
            literal(after),
            if predicates.is_empty() { "" } else { "AND" }.into(),
            "".into(),
            "".into(),
        ));
    }
    for (i, (field, op, value, logic, left, right)) in predicates.iter().enumerate() {
        if !identifier(field)
            || !matches!(op.as_str(), "=" | ">" | "LIKE")
            || !left.chars().all(|c| c == '(')
            || !right.chars().all(|c| c == ')')
        {
            return Err("Invalid report criterion structure.".into());
        }
        let logic = match logic.as_str() {
            "" => "",
            "AND" | "And" => "And",
            "OR" | "Or" => "Or",
            _ => return Err("Invalid report criterion logic.".into()),
        };
        ds["QueryWhereItem"].as_array_mut().unwrap().push(row(json!({"CriteriaID":format!("00000000-0000-0000-0001-{:012}",i+1),"TableID":"R","FieldName":field,"CompOp":op,"RValue":value,"IsConst":true,"CriteriaType":2,"Seq":i+1,"AndOr":logic,"Neg":false,"LeftP":left,"RightP":right,"UseHaving":false})));
    }
    Ok(json!({"queryDS":ds,"executionParams":{"ExecutionParameter":[]}}))
}

#[cfg(test)]
mod like_tests {
    use super::*;
    use herbert_contract::*;
    fn discover(category: &str, settings: Value) -> String {
        let r = GuestRequest { abi: PLUGIN_ABI, operation: "discover".into(), category: category.into(), company: "C".into(), identity: None, cursor: None,
            page_size: Some(10), state: json!({"_pluginSettings":settings,"_collectionMethod":"directOnly"}), response: None, error: None };
        let GuestReply::Action { action, .. } = crate::handle(category, r) else { panic!("{category}: first discovery reply is not a read") };
        serde_json::to_string(&action).unwrap()
    }
    #[test]
    fn stem_is_the_literal_run_before_any_like_metacharacter() {
        assert_eq!(like_stem("Ab%c"), Some("Ab")); assert_eq!(like_stem("Q_1"), Some("Q")); assert_eq!(like_stem("X[y]"), Some("X"));
        for none in ["", "%", "_x", "[", "]"] { assert_eq!(like_stem(none), None, "{none}"); }
        assert_eq!(like_clause("F", "O'k[1"), " AND F LIKE 'O''k%'"); assert_eq!(like_clause("F", ""), "");
    }
    #[test]
    fn each_plugin_sends_a_server_side_prefix_superset_and_none_when_blank() {
        let ud = discover("ud-codes", json!({"codeTypeIdPrefix":"Ab_"}));
        assert!(ud.contains("SystemFlag = false AND CodeTypeID LIKE 'Ab%'"), "{ud}");
        assert!(!discover("ud-codes", json!({"codeTypeIdPrefix":""})).contains("LIKE"));
        let schema = discover("ud-schema", json!({"schemaTableIdPrefix":"Cu'"}));
        assert!(schema.contains("\"whereClauseZDataTable\":\"DataTableID LIKE 'Cu''%'\"") && schema.contains("SystemFlag = false AND DataTableID LIKE 'Cu''%'"), "{schema}");
        assert!(!discover("ud-schema", json!({})).contains("LIKE"));
        let dashboards = discover("classic-dashboards", json!({"dashboardIdPrefix":"Z_1"}));
        assert!(dashboards.contains("\"operator\":\"LIKE\"") && dashboards.contains("\"definitionPrefix\":\"Z%\""), "{dashboards}");
        assert!(!discover("classic-dashboards", json!({"dashboardIdPrefix":""})).contains("LIKE"));
        let properties = discover("extended-properties", json!({"tableIdPrefix":"Pa"}));
        assert!(properties.contains("\"FieldName\":\"DataTableID\"") && properties.contains("\"CompOp\":\"LIKE\"") && properties.contains("N'Pa%'"), "{properties}");
        assert!(!discover("extended-properties", json!({})).contains("LIKE"));
        let configurators = discover("configurators", json!({"definitionIdPrefix":"R2C"}));
        assert!(configurators.contains("ConfigID LIKE 'R2C%'"), "{configurators}");
        assert!(!discover("configurators", json!({})).contains("LIKE"));
    }
}
