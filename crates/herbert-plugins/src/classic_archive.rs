//! Gen3 is the native Classic DataSet export, including its inline XML schema.
//! Keep Content verbatim. Gen1 reconstruction from XXXChunk is not this format.
use herbert_contract::Diagnostic;
use serde_json::Value;

pub const KEYS: [&str; 7] = [
    "Company",
    "ProductID",
    "TypeCode",
    "CGCCode",
    "Key1",
    "Key2",
    "Key3",
];
const NS: &str = "http://www.epicor.com/Ice/300/BO/GenXData/GenXData";
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("CLASSIC_EXPORT_INVALID", message)
}
pub(crate) fn escape(value: &str) -> Result<String, Diagnostic> {
    if value.chars().any(|c| !matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')) {
        return Err(invalid("The definition contains a character that XML 1.0 cannot represent."));
    }
    // XML readers normalize literal CR characters. An entity preserves source CRLF.
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\r', "&#13;"))
}
fn field_type(name: &str, value: &Value) -> &'static str {
    if name == "DataBaseVersion" {
        "int"
    } else if name == "SysRevID" {
        "long"
    } else if name == "LastUpdated" || name.starts_with("SysDate") {
        "dateTime"
    } else if name.starts_with("SysNumber") {
        "decimal"
    } else if name.starts_with("SysCheckBox")
        || matches!(name, "SystemFlag" | "OnMenu" | "Duplicate" | "Cosmetic")
        || value.is_boolean()
    {
        "boolean"
    } else if value.is_number() {
        "decimal"
    } else {
        "string"
    }
}
pub fn build(row: &Value) -> Result<String, Diagnostic> {
    let row = row
        .as_object()
        .ok_or_else(|| invalid("The Classic definition is not a row."))?;
    for key in KEYS {
        if !row.get(key).is_some_and(Value::is_string) {
            return Err(invalid(
                "The Classic definition has an incomplete native identity.",
            ));
        }
    }
    if !row.get("Content").is_some_and(Value::is_string) {
        return Err(invalid("The Classic definition has no Content field."));
    }
    let fields: Vec<_> = row
        .iter()
        .filter(|(key, _)| {
            !key.starts_with("Herbert") && !matches!(key.as_str(), "RowMod" | "BitFlag")
        })
        .collect();
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<GenXDataDataSet xmlns=\"{NS}\"><xs:schema id=\"GenXDataDataSet\" targetNamespace=\"{NS}\" xmlns:mstns=\"{NS}\" xmlns=\"{NS}\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:msdata=\"urn:schemas-microsoft-com:xml-msdata\" xmlns:msprop=\"urn:schemas-microsoft-com:xml-msprop\" attributeFormDefault=\"qualified\" elementFormDefault=\"qualified\"><xs:element name=\"GenXDataDataSet\" msdata:IsDataSet=\"true\" msdata:UseCurrentLocale=\"true\" msprop:ExportFormat=\"Gen3\"><xs:complexType><xs:choice minOccurs=\"0\" maxOccurs=\"unbounded\"><xs:element name=\"XXXDef\"><xs:complexType><xs:sequence>"
    );
    for (name, value) in &fields {
        if !name
            .chars()
            .enumerate()
            .all(|(i, c)| c.is_ascii_alphabetic() || c == '_' || (i > 0 && c.is_ascii_digit()))
        {
            return Err(invalid("A Classic field name is not a valid XML name."));
        }
        if value.is_array() || value.is_object() {
            return Err(invalid("A Classic field is not a scalar value."));
        }
        xml.push_str(&format!(
            "<xs:element name=\"{name}\" type=\"xs:{}\" minOccurs=\"0\"/>",
            field_type(name, value)
        ));
    }
    xml.push_str("</xs:sequence></xs:complexType></xs:element></xs:choice></xs:complexType><xs:unique name=\"Constraint1\" msdata:PrimaryKey=\"true\"><xs:selector xpath=\".//mstns:XXXDef\"/>");
    for key in KEYS {
        xml.push_str(&format!("<xs:field xpath=\"mstns:{key}\"/>"));
    }
    xml.push_str("</xs:unique></xs:element></xs:schema><XXXDef>");
    for (name, value) in fields {
        if value.is_null() {
            continue;
        }
        let text = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        xml.push_str(&format!("<{name}>{}</{name}>", escape(&text)?));
    }
    xml.push_str("</XXXDef></GenXDataDataSet>");
    Ok(xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    #[ignore = "Requires an explicitly collected offline Classic corpus."]
    fn reconstruct_native_corpus() {
        let path = std::path::PathBuf::from(std::env::var("HERBERT_CLASSIC_CORPUS").unwrap());
        let rows: Vec<Value> =
            serde_json::from_slice(&std::fs::read(path.join("rows.json")).unwrap()).unwrap();
        std::fs::create_dir_all(path.join("rebuilt")).unwrap();
        for (index, row) in rows.iter().enumerate() {
            std::fs::write(
                path.join("rebuilt").join(format!("{index}.xml")),
                build(row).unwrap(),
            )
            .unwrap();
            let mut definition = json!({"XXXDef":[row],"herbertSources":{}});
            assert!(crate::classic_sources::append_persisted_scripts(&mut definition).is_empty());
        }
    }
    #[test]
    fn gen3_preserves_content_and_rejects_unrepresentable_xml() {
        let mut row = json!({"Company":"C","ProductID":"EP","TypeCode":"Customization","CGCCode":"","Key1":"A","Key2":"F","Key3":"","Content":"<x>Ω\r\n&amp;</x>","SystemFlag":false});
        let xml = build(&row).unwrap();
        assert!(xml.contains("msprop:ExportFormat=\"Gen3\""));
        assert!(xml.contains("Ω&#13;\n&amp;amp;"));
        row["Content"] = json!("bad\u{1}data");
        assert!(build(&row).is_err());
    }
}
