//! Emit the typed DataSet XML used by Epicor's QuickSearch solution handler.
//! Keep the original rows beside each export for diagnostics and recovery.
use herbert_contract::Diagnostic;
use serde_json::{json, Value};
use std::collections::BTreeMap;

type Schemas = BTreeMap<String, BTreeMap<String, Vec<(String, String)>>>;

pub const CONTEXT_CHILDREN: &[&str] = &[
    "ContextMenuItem",
    "AlternateKeyLike",
    "LaunchOption",
    "AlternateKey",
    "AlternateKeyBinding",
    "SearchOption",
    "WhereClauseBinding",
    "WhereClauseToken",
    "ContextValidation",
];

pub fn has_custom_context(data: &Value) -> bool {
    data["row"]["SystemFlag"] != true
        || CONTEXT_CHILDREN.iter().any(|table| {
            data["children"][*table]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["SystemFlag"] != true))
        })
}

/// Preserve physical keys. Native RowLoaded changes ContextTypeCode for display
/// and can then omit ContextValidation; neither behavior belongs in a backup.
pub fn context_graph_definition(data: &Value) -> Result<Value, Diagnostic> {
    if data["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(invalid(
            "The legacy navigation child collection failed. Its source is retained.",
        ));
    }
    let mut root = data["row"].clone();
    let system_parent = root["SystemFlag"] == true;
    if system_parent {
        root = json!({"LikeID":root["LikeID"], "ContextTypeCode":root["ContextTypeCode"]});
    }
    root["BitFlag"] = json!(0);
    root["RowMod"] = json!("");
    let mut definition =
        json!({"ContextMenu":[root], "ContextMenuTemp":[], "ContextMenuItemTemp":[]});
    for table in CONTEXT_CHILDREN {
        let source = data["children"][*table]
            .as_array()
            .ok_or_else(|| invalid("The legacy navigation page omitted a child table."))?;
        let mut rows = Vec::new();
        for row in source.iter().filter(|row| row["SystemFlag"] != true) {
            let mut row = row.clone();
            if *table == "WhereClauseBinding" {
                if let Some(text) = row["WhereClauseString"].as_str() {
                    let bytes = row["HerbertWhereClauseBytes"].as_u64().or_else(|| {
                        row["HerbertWhereClauseBytes"]
                            .as_str()
                            .and_then(|s| s.parse().ok())
                    });
                    if bytes != Some((text.encode_utf16().count() * 2) as u64) {
                        return Err(invalid(
                            "A navigation filter failed its source text length check.",
                        ));
                    }
                }
            }
            // These are external DataSet fields. The native BO initializes
            // them; they are not columns in the persisted graph.
            row["BitFlag"] = json!(0);
            row["RowMod"] = json!("");
            if *table == "ContextMenuItem" {
                row["TempID"] = json!("");
                row["ProcessCallOrg"] = json!("");
            }
            rows.push(row);
        }
        definition[*table] = json!(rows);
    }
    if system_parent {
        definition["herbertParentContext"] = json!(
            "The system parent is identified by keys. Restore requires that parent on the target."
        );
    }
    definition["herbertRestore"] = json!({
        "service":"Ice.BO.ContextMenuDataSvc", "method":"Update",
        "requiredContextTypeCode":data["row"]["ContextTypeCode"],
        "insertTableOrder":["ContextMenu","ContextMenuItem","AlternateKeyLike","LaunchOption",
            "AlternateKey","AlternateKeyBinding","SearchOption","WhereClauseBinding",
            "WhereClauseToken","ContextValidation"],
        "instructions":"Restore tables in dependency order. Native whole-graph Update inserts AlternateKeyBinding before its parent. Keep an existing system parent unchanged."
    });
    Ok(definition)
}
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("SEARCH_EXPORT_INVALID", message)
}

pub fn append(service: &str, name: &str, definition: &mut Value) -> Result<(), Diagnostic> {
    let schemas: Schemas = serde_json::from_str(include_str!("search-datasets.json")).unwrap();
    let schema = schemas
        .get(service)
        .ok_or_else(|| invalid("Unknown native search DataSet."))?;
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><{service}DataSet xmlns=\"http://www.epicor.com/Ice/300/BO/{service}/{service}\">"
    );
    for (table, fields) in schema {
        let Some(rows) = definition.get(table).and_then(Value::as_array) else {
            continue;
        };
        for row in rows {
            text.push_str(&format!("<{table}>"));
            for (field, kind) in fields {
                let Some(value) = row.get(field).filter(|v| !v.is_null()) else {
                    continue;
                };
                let value = match (kind.as_str(), value) {
                    ("string" | "Guid" | "DateTime", Value::String(text)) => text.clone(),
                    ("bool", Value::Bool(value)) => value.to_string(),
                    (
                        "int" | "long" | "uint" | "ulong" | "decimal" | "short" | "byte",
                        Value::Number(value),
                    ) => value.to_string(),
                    // The REST serializer represents UInt64 revisions as text.
                    ("ulong", Value::String(text)) if text.parse::<u64>().is_ok() => text.clone(),
                    _ => {
                        return Err(invalid(&format!(
                            "{table}.{field} does not match its native {kind} type."
                        )));
                    }
                };
                // Character references preserve CRLF through XML parsers.
                text.push_str(&format!(
                    "<{field}>{}</{field}>",
                    crate::classic_archive::escape(&value)?
                ));
            }
            text.push_str(&format!("</{table}>"));
        }
    }
    text.push_str(&format!("</{service}DataSet>"));
    definition["herbertSources"] = json!({"files":[{
        "name":format!("{service}_{}.xml",crate::bpm_code::label(name)),"text":text
    }]});
    definition["herbertDirectOutput"] = json!(true);
    Ok(())
}

/// The persisted QuickSearch table holds both normal and predictive searches.
/// Derive the two display bindings exactly as QuickSearchSvc.RowLoaded does.
pub fn quick_definition(data: &Value) -> Result<(String, Value), Diagnostic> {
    if data["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(invalid(
            "The search child collection failed. Its received source is retained.",
        ));
    }
    let mut row = data["row"].clone();
    let predictive = row["IsPredictive"]
        .as_bool()
        .ok_or_else(|| invalid("The search omitted IsPredictive."))?;
    let service = if predictive {
        "PredictiveSearch"
    } else {
        "QuickSearch"
    };
    // These are transient BO bindings, not persisted columns. Native row
    // construction starts them as empty strings before RowLoaded fills them.
    for field in ["LikeField", "ReturnField"] {
        row[field] = json!("");
    }
    // BitFlag is native transport metadata, not a persisted QuickSearch field.
    // The native BO initializes it to zero. REST requires a non-null Int32.
    row["BitFlag"] = json!(0);
    let text = |name: &str| row[name].as_str().unwrap_or("").to_owned();
    let like_table = text("LikeDataFieldTableID");
    let like_field = text("LikeDataFieldName");
    let return_table = text("ReturnFieldTableID");
    let return_field = text("ReturnFieldName");
    if !like_table.is_empty() && !like_field.is_empty() {
        row["LikeField"] = json!(format!("{like_table}.{like_field}"));
    }
    if !return_field.is_empty() {
        row["ReturnField"] = json!(if return_table.is_empty() {
            return_field
        } else {
            format!("{return_table}_{return_field}")
        });
    }
    if predictive {
        row["SourceField"] = json!("");
        row["SearchOnField"] = json!("");
        let text = |name: &str| row[name].as_str().unwrap_or("").to_owned();
        let source = [
            text("SourceSystemCode"),
            text("SourceTableID"),
            text("SourceFieldName"),
        ];
        let search_table = text("SearchOnFieldTableID");
        let search_field = text("SearchOnFieldName");
        let top = text("HotKey");
        if source.iter().all(|s| !s.is_empty()) {
            row["SourceField"] = json!(source.join("."));
        }
        if !search_field.is_empty() {
            row["SearchOnField"] = json!(if search_table.is_empty() {
                search_field
            } else {
                format!("{search_table}_{search_field}")
            });
        }
        row["TopX"] = json!(top.parse::<i32>().unwrap_or(0));
    }
    let mut definition = json!({service:[row]});
    for table in ["QuickSearchCriteria", "QuickSearchValueList"] {
        let rows = data["children"][table]
            .as_array()
            .ok_or_else(|| invalid("The search child batch is missing."))?;
        for row in rows {
            for (field, length) in [
                ("CriteriaValue", "HerbertCriteriaValueBytes"),
                ("DisplayMember", "HerbertDisplayBytes"),
                ("ValueMember", "HerbertValueBytes"),
            ] {
                if let Some(text) = row[field].as_str() {
                    let actual = row[length]
                        .as_u64()
                        .or_else(|| row[length].as_str().and_then(|s| s.parse().ok()));
                    if actual != Some((text.encode_utf16().count() * 2) as u64) {
                        return Err(invalid(
                            "A search child failed its source text length check.",
                        ));
                    }
                }
            }
        }
        // PredictiveSearch's native contract has no child tables. Retain any
        // unexpected persisted children in the recovery JSON, never discard them.
        let mut rows = rows.clone();
        if table == "QuickSearchCriteria" {
            for row in &mut rows {
                row["BitFlag"] = json!(0);
                if let (Some(table), Some(field)) =
                    (row["DataTableID"].as_str(), row["FieldName"].as_str())
                {
                    if !table.is_empty() && !field.is_empty() {
                        row["CriteriaField"] = json!(format!("{table}_{field}"));
                    }
                }
            }
        }
        if table == "QuickSearchValueList" {
            for row in &mut rows {
                row["BitFlag"] = json!(0);
            }
        }
        definition[table] = json!(rows);
    }
    Ok((service.into(), definition))
}

pub fn context_definition(data: &Value) -> Result<Value, Diagnostic> {
    if data["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(invalid(
            "The context-menu child collection failed. Its received source is retained.",
        ));
    }
    let row = &data["row"];
    if row["Key1"] != "CustomContextMenu"
        || !matches!(
            row["TypeCode"].as_str(),
            Some("Customization" | "Personalization")
        )
    {
        return Err(invalid("The definition is not a saved context menu."));
    }
    let content = row["Content"]
        .as_str()
        .ok_or_else(|| invalid("The saved context menu has no XML content."))?;
    let length = row["HerbertContentBytes"].as_u64().or_else(|| {
        row["HerbertContentBytes"]
            .as_str()
            .and_then(|s| s.parse().ok())
    });
    if length != Some((content.encode_utf16().count() * 2) as u64) {
        return Err(invalid(
            "The saved context menu failed its text length check.",
        ));
    }
    crate::xml_document::parse(content)?;
    let export = crate::classic_archive::build(row)?;
    Ok(
        json!({"XXXDef":[row],"XXXChunk":data["children"]["XXXChunk"],"herbertDirectOutput":true,
        "herbertSources":{"files":[{"name":"CustomContextMenu.xml","text":export},
            {"name":"Content.xml","text":content}]}}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn physical_context_keeps_stored_type_validation_and_restore_dependencies() {
        let mut data = json!({"row":{"LikeID":"Part.PartNum","ContextTypeCode":"UserType","SystemFlag":false},"children":{}});
        for table in CONTEXT_CHILDREN {
            data["children"][*table] = json!([]);
        }
        data["children"]["ContextValidation"] = json!([{"LikeID":"Part.PartNum","ContextTypeCode":"UserType","SystemFlag":false,"ValidationType":"List"}]);
        let definition = context_graph_definition(&data).unwrap();
        assert_eq!(definition["ContextMenu"][0]["ContextTypeCode"], "UserType");
        assert_eq!(definition["ContextValidation"][0]["ValidationType"], "List");
        let order = definition["herbertRestore"]["insertTableOrder"]
            .as_array()
            .unwrap();
        assert!(
            order.iter().position(|v| v == "AlternateKey").unwrap()
                < order
                    .iter()
                    .position(|v| v == "AlternateKeyBinding")
                    .unwrap()
        );
        data["row"]["SystemFlag"] = json!(true);
        assert!(has_custom_context(&data));
        let definition = context_graph_definition(&data).unwrap();
        assert!(definition["herbertParentContext"].is_string());
        assert!(definition["ContextMenu"][0].get("SystemFlag").is_none());
        data["children"]["ContextValidation"] = json!([]);
        assert!(!has_custom_context(&data));
        data["children"]
            .as_object_mut()
            .unwrap()
            .remove("WhereClauseToken");
        assert!(context_graph_definition(&data).is_err());
    }
    #[test]
    fn predictive_bindings_and_top_count_are_reconstructed_from_persisted_fields() {
        let data = json!({"row":{"IsPredictive":true,"QuickSearchID":"P","SourceSystemCode":"Erp","SourceTableID":"Part",
            "SourceFieldName":"PartNum","LikeDataFieldTableID":"Part","LikeDataFieldName":"PartNum",
            "ReturnFieldTableID":"Part","ReturnFieldName":"PartNum","SearchOnFieldTableID":"Part","SearchOnFieldName":"PartDescription","HotKey":"45"},
            "children":{"QuickSearchCriteria":[],"QuickSearchValueList":[]}});
        let (service, mut def) = quick_definition(&data).unwrap();
        assert_eq!(service, "PredictiveSearch");
        assert_eq!(def["PredictiveSearch"][0]["TopX"], 45);
        assert_eq!(
            def["PredictiveSearch"][0]["SourceField"],
            "Erp.Part.PartNum"
        );
        assert_eq!(
            def["PredictiveSearch"][0]["SearchOnField"],
            "Part_PartDescription"
        );
        append(&service, "P", &mut def).unwrap();
        assert!(def["herbertSources"]["files"][0]["text"]
            .as_str()
            .unwrap()
            .contains("<TopX>45</TopX>"));
    }
    #[test]
    fn malformed_and_truncated_source_is_not_an_export() {
        let data = json!({"row":{"IsPredictive":false},"children":{"QuickSearchCriteria":[{"CriteriaValue":"long","HerbertCriteriaValueBytes":2}],"QuickSearchValueList":[]}});
        assert!(quick_definition(&data).is_err());
        let mut def = json!({"NamedSearch":[{"NSId":"N","AutoExecute":"false"}]});
        assert!(append("NamedSearch", "N", &mut def).is_err());
        def = json!({"NamedSearch":[{"NSId":"N","Description":"bad\u{1}"}]});
        assert!(append("NamedSearch", "N", &mut def).is_err());
    }
}
