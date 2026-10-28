//! Match extension dependencies and write the native DataContract XML shape.
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn extension_table(row: &Value) -> bool {
    // Native DynamicQueryConstants.ExtTableTypes contains XT and PT.
    row["TableType"]
        .as_str()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("XT") || kind.eq_ignore_ascii_case("PT"))
}

pub fn table_key(row: &Value) -> String {
    format!(
        "{}.{}",
        row["DBSchemaName"].as_str().unwrap_or(""),
        row["DBTableName"].as_str().unwrap_or("")
    )
    .to_lowercase()
}

pub fn table_map(rows: &[Value]) -> Value {
    let mut map = serde_json::Map::new();
    for row in rows.iter().filter(|row| extension_table(row)) {
        if let Some((_, id)) = row["Description"]
            .as_str()
            .and_then(|s| s.rsplit_once('\u{1}'))
        {
            if !id.is_empty() {
                if let Some(name) = row["FullTableName"].as_str() {
                    map.insert(name.to_lowercase(), json!(id));
                }
            }
        }
    }
    Value::Object(map)
}

pub fn ids(data: &Value, map: &Value) -> Vec<String> {
    let mut ids = BTreeMap::new();
    if let Some(rows) = data["QueryTableDesigner"].as_array() {
        for row in rows.iter().filter(|row| extension_table(row)) {
            if let Some(id) = map[table_key(row)].as_str().filter(|id| !id.is_empty()) {
                ids.entry(id.to_lowercase())
                    .or_insert_with(|| id.to_owned());
            }
        }
    }
    ids.into_values().collect()
}

pub fn info(row: &Value, level_codes: &str) -> Value {
    let level = row["Level"]
        .as_i64()
        .map(|n| n.to_string())
        .unwrap_or_default();
    let prefix = format!("{level}`");
    let description = level_codes
        .split('~')
        .find_map(|entry| entry.strip_prefix(&prefix))
        .filter(|s| !s.is_empty())
        .unwrap_or(&level);
    json!({"ExtensionSetId":row["ExtensionSetID"],"Level":description,
        "Product":row["Product"],"Provider":row["Provider"],"Version":row["Version"]})
}

pub fn xml(data: &Value, map: &Value, info: &Value) -> Result<Vec<u8>, Diagnostic> {
    let rows = data["QueryTableDesigner"].as_array().ok_or_else(|| {
        Diagnostic::new("BAQ_DEPENDENCY_INVALID", "The query table list is missing.")
    })?;
    if !rows.iter().any(extension_table) {
        return Ok(vec![]);
    }
    for row in rows.iter().filter(|row| extension_table(row)) {
        let key = table_key(row);
        if map[&key].as_str().is_none_or(str::is_empty) {
            return Err(Diagnostic::new("BAQ_DEPENDENCY_UNRESOLVED",
                &format!("The extension table {key} has no resolved extension mapping.")));
        }
    }
    let names = ids(data, map);
    let mut seen = BTreeSet::new();
    let mut result = String::from(
        "<ArrayOfExtensionSetDependencyInfo xmlns=\"http://schemas.datacontract.org/2004/07/Ice.Lib.BAQExportImport.Handlers.Baq\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\">",
    );
    for id in names {
        if !seen.insert(id.to_lowercase()) {
            continue;
        }
        let row = info
            .as_object()
            .and_then(|rows| {
                rows.iter()
                    .find(|(key, _)| key.eq_ignore_ascii_case(&id))
                    .map(|(_, row)| row)
            })
            .unwrap_or(&Value::Null);
        if !row.is_object() {
            return Err(Diagnostic::new(
                "BAQ_DEPENDENCY_INVALID",
                "An extension dependency has no metadata.",
            ));
        }
        result.push_str("<ExtensionSetDependencyInfo>");
        // DataContractSerializer orders these unannotated public properties by name.
        for field in ["ExtensionSetId", "Level", "Product", "Provider", "Version"] {
            if row[field].is_null() {
                result.push_str(&format!("<{field} i:nil=\"true\"/>"));
            } else if let Some(text) = row[field].as_str() {
                result.push_str(&format!(
                    "<{field}>{}</{field}>",
                    crate::baq_archive::xml_text(text)
                ));
            } else {
                return Err(Diagnostic::new(
                    "BAQ_DEPENDENCY_INVALID",
                    "An extension dependency field is not text.",
                ));
            }
        }
        result.push_str("</ExtensionSetDependencyInfo>");
    }
    result.push_str("</ArrayOfExtensionSetDependencyInfo>");
    Ok(result.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_xml_matches_the_native_serializer_specimen() {
        let data = json!({"QueryTableDesigner":[{"TableType":"XT","DBSchemaName":"Ice","DBTableName":"Example"}]});
        let map = json!({"ice.example":"Example<&>"});
        let info = json!({"Example<&>":{"ExtensionSetId":"Example<&>","Level":"Customization","Product":"Product","Provider":"Provider","Version":"1.2.3"}});
        let actual = String::from_utf8(xml(&data, &map, &info).unwrap()).unwrap();
        let expected = "<ArrayOfExtensionSetDependencyInfo xmlns=\"http://schemas.datacontract.org/2004/07/Ice.Lib.BAQExportImport.Handlers.Baq\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\"><ExtensionSetDependencyInfo><ExtensionSetId>Example&lt;&amp;&gt;</ExtensionSetId><Level>Customization</Level><Product>Product</Product><Provider>Provider</Provider><Version>1.2.3</Version></ExtensionSetDependencyInfo></ArrayOfExtensionSetDependencyInfo>";
        assert_eq!(actual, expected);
    }

    #[test]
    fn only_native_extension_table_types_create_dependencies() {
        let data = json!({"QueryTableDesigner":[{"TableType":"BAQ"},{"TableType":"DB"}]});
        assert!(xml(&data, &json!({}), &json!({})).unwrap().is_empty());
        let map = table_map(&[
            json!({"TableType":"PT","FullTableName":"Ice.Example","Description":"A description\u{1}Package"}),
        ]);
        let data = json!({"QueryTableDesigner":[{"TableType":"pt","DBSchemaName":"ICE","DBTableName":"EXAMPLE"}]});
        assert_eq!(ids(&data, &map), vec!["Package"]);
        assert!(xml(&data, &map, &json!({})).is_err());
    }

    #[test]
    fn dependency_metadata_uses_the_native_case_insensitive_identity() {
        let data = json!({"QueryTableDesigner":[{"TableType":"XT","DBSchemaName":"Ice","DBTableName":"Example"}]});
        let map = json!({"ice.example":"PACKAGE"});
        let metadata = json!({"Package":{"ExtensionSetId":"Package","Level":"Customization","Product":"P","Provider":"V","Version":"1"}});
        assert!(xml(&data, &map, &metadata).is_ok());
    }

    #[test]
    fn wave1_every_extension_table_needs_a_mapping() {
        let data=json!({"QueryTableDesigner":[
            {"TableType":"XT","DBSchemaName":"ICE","DBTableName":"First"},
            {"TableType":"pt","DBSchemaName":"Ice","DBTableName":"Second"}]});
        for map in [json!({}),json!({"ice.first":"Set"})] {
            assert_eq!(xml(&data,&map,&json!({})).unwrap_err().code,"BAQ_DEPENDENCY_UNRESOLVED");
        }
        let map=table_map(&[
            json!({"TableType":"xt","FullTableName":"ICE.First","Description":"First\u{1}Set"}),
            json!({"TableType":"PT","FullTableName":"ice.SECOND","Description":"Second\u{1}SET"})]);
        let bytes=xml(&data,&map,&json!({"sEt":{"ExtensionSetId":"Set","Level":"Customization","Product":null,"Provider":null,"Version":null}})).unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap().matches("<ExtensionSetDependencyInfo>").count(),1);
    }

}
