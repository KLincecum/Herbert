//! Native lookup XML from stored rows.
use herbert_contract::Diagnostic;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct Field {
    name: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct Schema {
    root: String,
    namespace: String,
    order: Vec<String>,
    tables: BTreeMap<String, Vec<Field>>,
}
fn schema() -> Schema {
    serde_json::from_str(include_str!("configurator-lookup-dataset.json")).unwrap()
}
fn invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("CONFIGURATOR_LOOKUP_XML_INVALID", message)
}
fn key<'a>(row: &'a Value, name: &str) -> Result<&'a str, Diagnostic> {
    row[name]
        .as_str()
        .ok_or_else(|| invalid(format!("Lookup row omitted {name}.")))
}
// GetExportPcLookupTables copies stored entities into fresh native rows.
// These are constructor defaults, not GetByID's computed display fields.
fn defaults(table: &str) -> Value {
    let mut value = match table {
        "PcLookupTblHed" => json!({"EnableGlobalLookup":false,"GlbFlag":false}),
        "PcLookupColSetHed" => json!({"UsedExternally":false}),
        "PcLookupColSetDtl" => json!({"StringDigits":0,"DisplayFormat":"Format"}),
        "PcLookupTblValues" => json!({"DataType":""}),
        _ => unreachable!(),
    };
    value["BitFlag"] = json!(0);
    value["RowMod"] = json!("");
    value
}

pub fn xml(company: &str, lookup: &str, data: &Value) -> Result<String, Diagnostic> {
    let schema = schema();
    if lookup.is_empty()
        || data["PcLookupTblHed"]
            .as_array()
            .is_none_or(|r| r.len() != 1)
    {
        return Err(invalid("Lookup XML requires exactly one identified root."));
    }
    let mut sets = BTreeSet::new();
    let mut columns = BTreeSet::new();
    let mut values = BTreeSet::new();
    for table in &schema.order {
        let rows = data[table]
            .as_array()
            .ok_or_else(|| invalid(format!("Lookup data omitted {table}.")))?;
        let mut seen = BTreeSet::new();
        for row in rows {
            if key(row, "Company")? != company || key(row, "LookupTblID")? != lookup {
                return Err(invalid(format!(
                    "{table} contains a different lookup or company."
                )));
            }
            let id = key(row, "SysRowID")?;
            if id.is_empty() || !seen.insert(id.to_ascii_lowercase()) {
                return Err(invalid(format!(
                    "{table} has a missing or repeated row identity."
                )));
            }
            let unique = match table.as_str() {
                "PcLookupColSetHed" => sets.insert(key(row, "ColSetID")?.to_lowercase()),
                "PcLookupColSetDtl" => columns.insert((
                    key(row, "ColSetID")?.to_lowercase(),
                    key(row, "ColName")?.to_lowercase(),
                )),
                "PcLookupTblValues" => values.insert((
                    key(row, "ColSetID")?.to_lowercase(),
                    key(row, "ColName")?.to_lowercase(),
                    row["RowNum"]
                        .as_i64()
                        .ok_or_else(|| invalid("Lookup value omitted its integer RowNum."))?,
                )),
                _ => true,
            };
            if !unique {
                return Err(invalid(format!("{table} repeats a native key.")));
            }
        }
    }
    if columns.iter().any(|(set, _)| !sets.contains(set))
        || values
            .iter()
            .any(|(set, col, _)| !columns.contains(&(set.clone(), col.clone())))
    {
        return Err(invalid(
            "Lookup children reference an absent column set or column.",
        ));
    }
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><{} xmlns=\"{}\">",
        schema.root, schema.namespace
    );
    for table in &schema.order {
        let native_defaults = defaults(table);
        for row in data[table].as_array().unwrap() {
            text.push_str(&format!("<{table}>"));
            for field in &schema.tables[table] {
                let value = native_defaults
                    .get(&field.name)
                    .or_else(|| row.get(&field.name))
                    .ok_or_else(|| {
                        invalid(format!("{table}.{} was not downloaded.", field.name))
                    })?;
                // Native nullable dates are omitted by DataSet.WriteXml.
                if value.is_null() {
                    if field.kind == "DateTime" {
                        continue;
                    }
                    return Err(invalid(format!(
                        "{table}.{} has an unexpected null.",
                        field.name
                    )));
                }
                let value = match (field.kind.as_str(), value) {
                    ("string" | "Guid" | "DateTime", Value::String(s)) => s.clone(),
                    ("bool", Value::Bool(b)) => b.to_string(),
                    ("int", Value::Number(n))
                        if n.as_i64().is_some_and(|v| i32::try_from(v).is_ok()) =>
                    {
                        n.to_string()
                    }
                    ("long", Value::Number(n)) if n.as_i64().is_some() => n.to_string(),
                    ("decimal", Value::Number(n)) => n.to_string(),
                    _ => {
                        return Err(invalid(format!(
                            "{table}.{} does not match native {}.",
                            field.name, field.kind
                        )));
                    }
                };
                text.push_str(&format!(
                    "<{}>{}</{}>",
                    field.name,
                    crate::classic_archive::escape(&value)?,
                    field.name
                ));
            }
            text.push_str(&format!("</{table}>"));
        }
    }
    text.push_str(&format!("</{}>", schema.root));
    Ok(text)
}

pub fn append(company: &str, lookup: &str, definition: &mut Value) -> Result<(), Diagnostic> {
    let text = xml(company, lookup, &definition["definition"])?;
    if !definition["herbertSources"]["files"].is_array() {
        definition["herbertSources"]["files"] = json!([]);
    }
    definition["herbertSources"]["files"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name":format!("Lookup_{}.xml",crate::bpm_code::label(lookup)), "text":text
        }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_document::{Content, Element};
    fn fixture() -> Value {
        let mut data = json!({});
        for (table, fields) in schema().tables {
            let mut row = json!({});
            for field in fields {
                row[&field.name] = match field.kind.as_str() {
                    "bool" => json!(false),
                    "int" | "long" | "decimal" => json!(0),
                    "DateTime" => Value::Null,
                    _ => json!(""),
                };
            }
            row["Company"] = json!("C");
            row["LookupTblID"] = json!("L");
            row["SysRowID"] = json!("00000000-0000-0000-0000-000000000001");
            if row.get("ColSetID").is_some() {
                row["ColSetID"] = json!("S");
            }
            if row.get("ColName").is_some() {
                row["ColName"] = json!("Text");
            }
            data[&table] = json!([row]);
        }
        data
    }
    #[test]
    fn stored_values_survive_and_ui_fields_use_native_export_defaults() {
        let mut data = fixture();
        data["PcLookupTblValues"][0]["DataValue"] = json!("Ω & <source>\r\nsecond line");
        data["PcLookupColSetDtl"][0]["DisplayFormat"] = json!("UI formatted value");
        data["PcLookupTblHed"][0]["EnableGlobalLookup"] = json!(true);
        let text = xml("C", "L", &data).unwrap();
        assert!(text.contains("<DisplayFormat>Format</DisplayFormat>"));
        assert!(text.contains("<EnableGlobalLookup>false</EnableGlobalLookup>"));
        assert_eq!(
            normalized(&text)[&(
                "PcLookupTblValues".into(),
                "00000000-0000-0000-0000-000000000001".into()
            )]["DataValue"],
            "Ω & <source>\r\nsecond line"
        );
        assert_eq!(
            data["PcLookupColSetDtl"][0]["DisplayFormat"],
            "UI formatted value"
        );
    }
    #[test]
    fn incomplete_or_ambiguous_graphs_do_not_become_import_packages() {
        let mut d = fixture();
        d["PcLookupTblValues"][0]
            .as_object_mut()
            .unwrap()
            .remove("DataValue");
        assert!(xml("C", "L", &d).is_err());
        let mut d = fixture();
        d["PcLookupTblValues"][0]["RowNum"] = json!(1.5);
        assert!(xml("C", "L", &d).is_err());
        let mut d = fixture();
        d["PcLookupTblValues"][0]["Company"] = json!("Other");
        assert!(xml("C", "L", &d).is_err());
        let mut d = fixture();
        d["PcLookupColSetHed"] = json!([]);
        assert!(xml("C", "L", &d).is_err());
        let mut d = fixture();
        let mut row = d["PcLookupTblValues"][0].clone();
        row["SysRowID"] = json!("00000000-0000-0000-0000-000000000002");
        d["PcLookupTblValues"].as_array_mut().unwrap().push(row);
        assert!(xml("C", "L", &d).is_err());
    }
    fn normalized(xml: &str) -> BTreeMap<(String, String), BTreeMap<String, String>> {
        let root = crate::xml_document::parse(xml).unwrap();
        fn text(e: &Element) -> String {
            e.content
                .iter()
                .filter_map(|c| {
                    if let Content::Text(t) = c {
                        Some(t.as_str())
                    } else {
                        None
                    }
                })
                .collect()
        }
        root.children()
            .map(|r| {
                let fields: BTreeMap<_, _> = r
                    .children()
                    .map(|f| {
                        let mut value = text(f);
                        if f.name == "DataValueDecimal" && value.contains('.') {
                            value = value.trim_end_matches('0').trim_end_matches('.').to_owned();
                        }
                        (f.name.clone(), value)
                    })
                    .collect();
                ((r.name.clone(), fields["SysRowID"].clone()), fields)
            })
            .collect()
    }
    #[test]
    fn supplied_native_lookup_replay() {
        let Ok(base) = std::env::var("HERBERT_C19_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let data: Value = serde_json::from_slice(
                &std::fs::read(folder.join("stored/lookup/rows.json")).unwrap(),
            )
            .unwrap();
            let actual = xml("TESTCO", "H19L1024A", &data).unwrap();
            let native = std::fs::read_to_string(folder.join("oracle/lookup/native.xml")).unwrap();
            assert_eq!(normalized(&actual), normalized(&native), "{lane}");
            let ui: Value = serde_json::from_slice(
                &std::fs::read(folder.join("fixtures/lookup/readback.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                normalized(&xml("TESTCO", "H19L1024A", &ui).unwrap()),
                normalized(&native),
                "{lane} UI projection"
            );
            let page: Value = serde_json::from_slice(
                &std::fs::read(folder.join("lookup-page/calls/001-GetRows.json")).unwrap(),
            )
            .unwrap();
            let request = herbert_contract::GuestRequest {
                abi: herbert_contract::PLUGIN_ABI,
                operation: "discover".into(),
                category: "C19".into(),
                company: "TESTCO".into(),
                identity: None,
                cursor: Some(json!({"family":1,"page":1}).to_string()),
                page_size: Some(100),
                state: json!({}),
                response: Some(page["value"].clone()),
                error: None,
            };
            let herbert_contract::GuestReply::InventoryData { records, .. } =
                crate::families::handle(request.clone())
            else {
                panic!("Lookup page did not retain its definitions");
            };
            assert_eq!(records.len(), 1);
            let (identity, inventory) = records.into_iter().next().unwrap();
            let mut acquire = request;
            acquire.operation = "acquire".into();
            acquire.identity = Some(identity);
            acquire.cursor = None;
            acquire.response = None;
            acquire.state = json!({"_inventory":inventory});
            let herbert_contract::GuestReply::Definition {
                definition,
                completeness,
                diagnostics,
                ..
            } = crate::families::handle(acquire)
            else {
                panic!("Lookup acquisition repeated a server read");
            };
            assert!(
                matches!(completeness, herbert_contract::Completeness::Collected),
                "{diagnostics:?}"
            );
            let emitted = definition["herbertSources"]["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["name"].as_str().is_some_and(|n| n.ends_with(".xml")))
                .unwrap()["text"]
                .as_str()
                .unwrap();
            assert_eq!(
                normalized(emitted),
                normalized(&native),
                "{lane} collected page"
            );
            std::fs::write(folder.join("oracle/lookup/rust-local.xml"), actual).unwrap();
            let mut wrong = data.clone();
            wrong["PcLookupTblValues"][0]["Company"] = json!("Other");
            assert!(xml("TESTCO", "H19L1024A", &wrong).is_err());
            let mut missing = data.clone();
            missing.as_object_mut().unwrap().remove("PcLookupColSetDtl");
            assert!(xml("TESTCO", "H19L1024A", &missing).is_err());
            let mut duplicate = data.clone();
            let row = duplicate["PcLookupTblValues"][0].clone();
            duplicate["PcLookupTblValues"]
                .as_array_mut()
                .unwrap()
                .push(row);
            assert!(xml("TESTCO", "H19L1024A", &duplicate).is_err());
            let mut orphan = data.clone();
            orphan["PcLookupTblValues"][0]["ColName"] = json!("Absent");
            assert!(xml("TESTCO", "H19L1024A", &orphan).is_err());
        }
    }
}
