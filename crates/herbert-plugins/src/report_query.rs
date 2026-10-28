//! Build unsaved, read-only report BAQs. Field names come from the native
//! dictionary; values are quoted separately from validated identifiers.
use serde_json::{Value, json};
pub fn literal(value: &str) -> String {
    format!("N'{}'", value.replace('\'', "''"))
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
            || !matches!(op.as_str(), "=" | ">")
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
