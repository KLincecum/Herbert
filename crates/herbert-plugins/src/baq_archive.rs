//! Build native BAQ packages from complete native datasets in the local plugin.
//! Epicor imports DynamicQuery with InferSchema and merges it into its typed dataset.
use herbert_contract::Diagnostic;
use serde_json::Value;
use std::io::{Cursor, Write};

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("BAQ_PACKAGE_INVALID", message)
}

pub(crate) fn owned_by(row: &Value, root: &Value) -> bool {
    row["Company"]
        .as_str()
        .is_some_and(|company| Some(company) == root["Company"].as_str())
        && row["QueryID"]
            .as_str()
            .zip(root["QueryID"].as_str())
            .is_some_and(|(query, owner)| query.eq_ignore_ascii_case(owner))
}

fn xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

pub(crate) fn xml_text(value: &str) -> String {
    let mut text = String::new();
    for c in value.chars() {
        match c {
            '&' => text.push_str("&amp;"),
            '<' => text.push_str("&lt;"),
            '>' => text.push_str("&gt;"),
            // Character references preserve CR and XML-invalid controls in .NET datasets.
            '\r' => text.push_str("&#xD;"),
            c if c < ' ' && !matches!(c, '\n' | '\t') => {
                text.push_str(&format!("&#x{:X};", c as u32));
            }
            c => text.push(c),
        }
    }
    text
}

/// Preserve every scalar field. Null fields are absent, as in DataSet.WriteXml.
/// Reject an unexpected nested value instead of producing a partial export.
pub(crate) fn dataset_xml(dataset: &Value, name: &str, namespace: &str) -> Result<Vec<u8>, Diagnostic> {
    let tables = dataset
        .as_object()
        .ok_or_else(|| invalid("The query dataset is not an object."))?;
    let mut text = format!("<{name} xmlns=\"{namespace}\">");
    for (table, rows) in tables {
        let rows = rows
            .as_array()
            .ok_or_else(|| invalid("A query table is not an array."))?;
        if rows.is_empty() {
            continue;
        }
        if !xml_name(table) {
            return Err(invalid("A query table has an invalid XML name."));
        }
        for row in rows {
            let fields = row
                .as_object()
                .ok_or_else(|| invalid("A query row is not an object."))?;
            text.push_str(&format!("<{table}>"));
            for (field, value) in fields {
                if value.is_null() {
                    continue;
                }
                if !xml_name(field) {
                    return Err(invalid("A query field has an invalid XML name."));
                }
                let scalar = match value {
                    Value::String(s) => s.clone(),
                    Value::Bool(b) => b.to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => {
                        return Err(invalid(
                            "A query field contains an unsupported nested value.",
                        ));
                    }
                };
                text.push_str(&format!("<{field}>{}</{field}>", xml_text(&scalar)));
            }
            text.push_str(&format!("</{table}>"));
        }
    }
    text.push_str(&format!("</{name}>"));
    Ok(text.into_bytes())
}

/// Call after the collector verifies the query identity and gathers dependencies.
/// The version belongs to the exporting server, not to the query's saved header.
pub fn build(
    definition: &Value,
    diagrams: &Value,
    version: &str,
    dependencies: Option<&[u8]>,
) -> Result<Vec<u8>, Diagnostic> {
    build_complete(definition, diagrams, version, dependencies, None)
}

/// Updatable packages also contain native method, argument and directive rows.
/// Preserve widget XML and custom-code bodies as scalar text without rewriting.
pub fn build_complete(
    definition: &Value,
    diagrams: &Value,
    version: &str,
    dependencies: Option<&[u8]>,
    bpm: Option<&Value>,
) -> Result<Vec<u8>, Diagnostic> {
    if version.is_empty() {
        return Err(invalid("The exporting server version is missing."));
    }
    let roots = definition["DynamicQueryDesigner"]
        .as_array()
        .filter(|rows| rows.len() == 1)
        .ok_or_else(|| invalid("The package requires exactly one query."))?;
    let root = &roots[0];
    if root["Company"].as_str().is_none() || root["QueryID"].as_str().is_none_or(str::is_empty) {
        return Err(invalid("The query identity is incomplete."));
    }
    if !root["Updatable"].is_boolean() || root["SystemFlag"] != false || root["IsProtected"] != false {
        return Err(invalid(
            "This package builder requires an unprotected custom query.",
        ));
    }
    if root["Updatable"] == true && bpm.is_none() {
        return Err(invalid("An updatable query requires its native BPM dataset."));
    }
    let bpm_xml = bpm.map(|dataset| {
        if dataset["BpDirective"].as_array().into_iter().flatten()
            .any(|row| row["IsProtected"] != false) {
            return Err(invalid("A protected directive cannot be rebuilt locally."));
        }
        dataset_xml(dataset, "BpMethodDataSet", "http://www.epicor.com/Ice/300/BO/BpMethod/BpMethod")
    }).transpose()?;
    let bpm_version = format!("{version}\n");
    let mut data = definition.clone();
    // Native export saves only authored field attribute overrides.
    if let Some(rows) = data["QueryFieldAttributeDesigner"].as_array_mut() {
        rows.retain(|row| row["IsOverriden"] == true);
    }
    for dataset in [&data, diagrams] {
        let tables = dataset
            .as_object()
            .ok_or_else(|| invalid("A package dataset is missing."))?;
        for rows in tables.values() {
            for row in rows
                .as_array()
                .ok_or_else(|| invalid("A package table is invalid."))?
            {
                if !owned_by(row, root) {
                    return Err(invalid(
                        "A package row belongs to a different query or company.",
                    ));
                }
            }
        }
    }
    let query = dataset_xml(
        &data,
        "BAQDesignerDataSet",
        "http://www.epicor.com/Ice/300/BO/BAQDesigner/BAQDesigner",
    )?;
    let design = dataset_xml(
        diagrams,
        "QueryDiagramDataSet",
        "http://www.epicor.com/Ice/300/BO/BAQDesigner/QueryDiagram",
    )?;
    let mut entries = vec![
        ("BAQVersion", version.as_bytes()),
        ("DynamicQuery", query.as_slice()),
        ("DynamicQueryDesign", design.as_slice()),
    ];
    if let Some(dependencies) = dependencies {
        entries.push(("QueryDependencies", dependencies));
    }
    if let Some(bpm_xml) = &bpm_xml {
        entries.push(("version", bpm_version.as_bytes()));
        entries.push(("BpMethodDataset", bpm_xml));
    }
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        archive
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(9)),
            )
            .map_err(|_| invalid("Cannot create a BAQ package member."))?;
        archive
            .write_all(bytes)
            .map_err(|_| invalid("Cannot write a BAQ package member."))?;
    }
    archive
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|_| invalid("Cannot finish the BAQ package."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore = "Requires explicitly gathered lab UBAQ fixtures."]
    fn reconstruct_lab_ubaq() {
        let path = std::path::PathBuf::from(std::env::var("HERBERT_UBAQ_FIXTURE").unwrap());
        let read = |name: &str| -> Value {
            serde_json::from_slice(&std::fs::read(path.join(name)).unwrap()).unwrap()
        };
        let definition = read("ubaq-definitions-2026.json");
        let bpm = crate::ubaq_rows::select(&read("ubaq-bpm-2026.json"), "TESTCO", "CallUBAQ").unwrap();
        let bytes = build_complete(&definition, &read("ubaq-diagrams-2026.json"),
            "5.2.100.0", Some(&[]), Some(&bpm)).unwrap();
        std::fs::write(path.join("ubaq-local-2026.baq"), bytes).unwrap();
    }

    #[test]
    fn xml_preserves_text_numbers_nulls_and_rejects_nested_data() {
        let xml = dataset_xml(&json!({"T":[{"Text":"a<&>\r\nb","Number":9007199254740993u64,"Flag":false,"Null":null}]}), "D", "urn:test").unwrap();
        let text = String::from_utf8(xml).unwrap();
        assert!(text.contains("a&lt;&amp;&gt;&#xD;\nb"));
        assert!(text.contains("9007199254740993"));
        assert!(!text.contains("Null"));
        assert!(dataset_xml(&json!({"T":[{"Nested":{}}]}), "D", "urn:test").is_err());
    }

    #[test]
    fn foreign_diagram_cannot_enter_an_export() {
        let root = json!({"Company":"C","QueryID":"Q","SystemFlag":false,"Updatable":false,"IsProtected":false});
        let data = json!({"DynamicQueryDesigner":[root]});
        let diagrams = json!({"QueryDiagram":[{"Company":"C","QueryID":"Other"}]});
        assert!(build(&data, &diagrams, "5.2.100.0", Some(&[])).is_err());
    }

    #[test]
    fn decimal_digits_survive_the_json_to_xml_boundary() {
        let data: Value =
            serde_json::from_str(r#"{"T":[{"Value":1234567890.1234567890123456789}]}"#).unwrap();
        let xml = String::from_utf8(dataset_xml(&data, "D", "urn:test").unwrap()).unwrap();
        assert!(xml.contains("1234567890.1234567890123456789"), "{xml}");
    }

    #[test]
    #[ignore = "Requires lab evidence gathered by the opt-in host test."]
    fn reconstruct_lab_export() {
        let path = std::path::PathBuf::from(std::env::var("HERBERT_BAQ_FIXTURE").unwrap());
        let read = |name: &str| -> Value {
            serde_json::from_slice(&std::fs::read(path.join(name)).unwrap()).unwrap()
        };
        let response = read("rows-500-1.json");
        let mut diagrams = read("query-0-LoadQueryDiagram.json")["returnObj"].clone();
        let kinetic = read("query-0-LoadKineticQueryDiagram.json");
        diagrams["QueryDiagram"].as_array_mut().unwrap().extend(
            kinetic["returnObj"]["QueryDiagram"]
                .as_array()
                .unwrap()
                .iter()
                .cloned(),
        );
        let version = std::env::var("HERBERT_BAQ_VERSION").unwrap();
        let bytes = build(&response["returnObj"], &diagrams, &version, Some(&[])).unwrap();
        std::fs::write(path.join("local.baq"), bytes).unwrap();
    }
}
