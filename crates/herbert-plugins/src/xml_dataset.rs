//! Epicor flat DataSet interpretation belongs to the collector, not the host.
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::collections::BTreeMap;
const XSD: &str = "http://www.w3.org/2001/XMLSchema";
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";
fn error(message: &str) -> Diagnostic {
    Diagnostic::new("PACKAGE_DATASET_INVALID", message)
}
fn elements(node: &Value) -> impl Iterator<Item = &Value> {
    node["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("element"))
}
fn attr<'a>(node: &'a Value, name: &str) -> Option<&'a str> {
    node["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["name"] == name && a["namespace"].is_null())?["value"]
        .as_str()
}
fn child<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    elements(node).find(|e| e["namespace"] == XSD && e["name"] == name)
}
#[derive(Debug)]
pub struct Dataset {
    pub tables: BTreeMap<String, Vec<Value>>,
}
fn schemas(
    node: &Value,
    result: &mut BTreeMap<String, BTreeMap<String, Value>>,
) -> Result<(), Diagnostic> {
    if node["namespace"] == XSD && node["name"] == "element" {
        if let Some(sequence) = child(node, "complexType").and_then(|e| child(e, "sequence")) {
            let table = attr(node, "name")
                .ok_or_else(|| error("An XML DataSet schema table has no name."))?;
            let mut columns = BTreeMap::new();
            for field in elements(sequence) {
                if field["namespace"] != XSD || field["name"] != "element" {
                    return Err(error(
                        "The DataSet schema has an unsupported non-column sequence.",
                    ));
                }
                let name = attr(field, "name")
                    .ok_or_else(|| error("An XML DataSet column has no name."))?;
                let typ = attr(field, "type")
                    .ok_or_else(|| error("An XML DataSet column has no scalar type."))?;
                if columns.insert(name.into(),json!({"type":typ,"default":attr(field,"default"),"optional":attr(field,"minOccurs")==Some("0")})).is_some(){return Err(error("The DataSet schema repeats a column."));}
            }
            if result.insert(table.into(), columns).is_some() {
                return Err(error("The DataSet schema repeats a table."));
            }
        }
    }
    for element in elements(node) {
        schemas(element, result)?;
    }
    Ok(())
}
pub fn parse(node: &Value) -> Result<Dataset, Diagnostic> {
    node["name"]
        .as_str()
        .ok_or_else(|| error("The package member has no DataSet root."))?;
    let namespace = &node["namespace"];
    let mut columns = BTreeMap::new();
    let schema = elements(node)
        .filter(|e| e["namespace"] == XSD && e["name"] == "schema")
        .collect::<Vec<_>>();
    if schema.is_empty() {
        return Err(error("The Epicor DataSet has no inline schema."));
    } else if schema.len() == 1 {
        schemas(schema[0], &mut columns)?;
    } else {
        return Err(error("The Epicor DataSet repeats its inline XSD schema."));
    }
    let mut tables: BTreeMap<String, Vec<Value>> =
        columns.keys().map(|k| (k.clone(), vec![])).collect();
    for row in elements(node) {
        if row["namespace"] == XSD && row["name"] == "schema" {
            continue;
        }
        let table = row["name"]
            .as_str()
            .ok_or_else(|| error("An XML DataSet row has no table name."))?;
        if &row["namespace"] != namespace || !columns.contains_key(table) {
            return Err(error(
                "An Epicor DataSet row is outside its declared table namespace or schema.",
            ));
        }
        let mut fields = serde_json::Map::new();
        for cell in elements(row) {
            let field = cell["name"]
                .as_str()
                .ok_or_else(|| error("A DataSet cell has no field name."))?;
            if &cell["namespace"] != namespace || !columns[table].contains_key(field) {
                return Err(error(
                    "An Epicor DataSet cell is outside its declared namespace or schema.",
                ));
            }
            if elements(cell).next().is_some() {
                return Err(error(
                    "A flat Epicor DataSet cell contains nested elements.",
                ));
            }
            let nil = cell["attributes"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|a| a["namespace"] == XSI && a["name"] == "nil" && a["value"] == "true");
            let text: String = cell["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["text"].as_str())
                .collect();
            let value = if nil {
                if !text.is_empty() {
                    return Err(error("A nil DataSet cell contains text."));
                }
                Value::Null
            } else {
                json!(text)
            };
            if fields.insert(field.into(), value).is_some() {
                return Err(error("An Epicor DataSet row repeats a field."));
            }
        }
        // DataSet XML omits values equal to an explicit schema default.
        for (field, column) in &columns[table] {
            if !fields.contains_key(field) && column["default"].is_string() {
                fields.insert(field.clone(), column["default"].clone());
            }
        }
        tables.get_mut(table).unwrap().push(Value::Object(fields));
    }
    Ok(Dataset { tables })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn element(
        name: &str,
        namespace: &str,
        attributes: &[(&str, &str)],
        content: Vec<Value>,
    ) -> Value {
        json!({"name":name,"namespace":namespace,"attributes":attributes.iter().map(|(key,value)|json!({"name":key,"namespace":null,"value":value})).collect::<Vec<_>>(),"content":content})
    }
    fn nested(node: Value) -> Value {
        json!({"element":node})
    }
    #[test]
    fn omitted_defaults_and_explicit_values_survive_native_dataset_parsing() {
        let field = element(
            "element",
            XSD,
            &[
                ("name", "Updatable"),
                ("type", "xs:boolean"),
                ("default", "false"),
            ],
            vec![],
        );
        let sequence = element("sequence", XSD, &[], vec![nested(field)]);
        let complex = element("complexType", XSD, &[], vec![nested(sequence)]);
        let table = element(
            "element",
            XSD,
            &[("name", "DynamicQueryDesigner")],
            vec![nested(complex)],
        );
        let schema = element("schema", XSD, &[], vec![nested(table)]);
        let mut document = element(
            "DynamicQueryDataSet",
            "native",
            &[],
            vec![
                nested(schema),
                nested(element("DynamicQueryDesigner", "native", &[], vec![])),
            ],
        );
        assert_eq!(
            parse(&document).unwrap().tables["DynamicQueryDesigner"][0]["Updatable"],
            "false"
        );
        document["content"][1]["element"]["content"] = json!([nested(element(
            "Updatable",
            "native",
            &[],
            vec![json!({"text":"true"})]
        ))]);
        assert_eq!(
            parse(&document).unwrap().tables["DynamicQueryDesigner"][0]["Updatable"],
            "true"
        );
    }
}

/// Read DataSet.GetXml output. This format has rows but no inline schema.
pub fn parse_flat(node: &Value) -> Result<Dataset, Diagnostic> {
    if !node["name"].is_string() {
        return Err(error("The export has no DataSet root."));
    }
    let mut tables = BTreeMap::<String, Vec<Value>>::new();
    for row in elements(node) {
        let table = row["name"]
            .as_str()
            .ok_or_else(|| error("A report row has no table name."))?;
        if row["namespace"] != node["namespace"] {
            return Err(error("A report row has an unexpected namespace."));
        }
        let mut fields = serde_json::Map::new();
        for cell in elements(row) {
            let field = cell["name"]
                .as_str()
                .ok_or_else(|| error("A report field has no name."))?;
            if cell["namespace"] != node["namespace"] || elements(cell).next().is_some() {
                return Err(error(
                    "A report field has an unexpected namespace or nested value.",
                ));
            }
            let nil = cell["attributes"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|a| a["namespace"] == XSI && a["name"] == "nil" && a["value"] == "true");
            let text: String = cell["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["text"].as_str())
                .collect();
            if nil && !text.is_empty() {
                return Err(error("A null report field contains text."));
            }
            if fields
                .insert(field.into(), if nil { Value::Null } else { json!(text) })
                .is_some()
            {
                return Err(error("A report row repeats a field."));
            }
        }
        tables
            .entry(table.into())
            .or_default()
            .push(Value::Object(fields));
    }
    Ok(Dataset { tables })
}
