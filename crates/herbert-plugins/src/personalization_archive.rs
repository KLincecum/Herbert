//! Match CustomLayeringManager.ExportPersonalizationLayer: typed inner XML,
//! native identity properties, and both tables even when a table has no rows.
use herbert_contract::Diagnostic;
use quick_xml::Writer;
use serde_json::Value;

const NS: &str = "http://tempuri.org/XMLSchema.xsd";
const COLUMNS: [(&str, [&str; 10]); 2] = [
    (
        "Controls",
        [
            "FormsID",
            "ControlName",
            "Key",
            "ParentControlKey",
            "ControlType",
            "Company",
            "Plant",
            "DcdUserId",
            "GroupID",
            "ControlsID",
        ],
    ),
    (
        "Properties",
        [
            "FormsID",
            "ControlName",
            "Key",
            "PropertyName",
            "PropertyValue",
            "Company",
            "Plant",
            "DcdUserID",
            "GroupID",
            "PropertiesID",
        ],
    ),
];
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("PERSONALIZATION_EXPORT_INVALID", message)
}
use crate::xml_document::{Element, Content, parse};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn row(content: &str) -> Value {
        json!({"Company":"C","ProductID":"EP","TypeCode":"Personalization","CGCCode":"","Key1":"alice","Key2":"Form","Key3":"Customization^Parent","Content":content,"Description":"A & B\r\nnext"})
    }
    #[test]
    fn empty_native_tables_and_owner_metadata_are_preserved() {
        let xml = build(&row("<CustomizationDS/>")).unwrap();
        assert!(xml.contains("name=\"Controls\""));
        assert!(xml.contains("name=\"Properties\""));
        assert!(xml.find("name=\"Controls\"") < xml.find("name=\"Properties\""));
        assert!(xml.contains("msprop:Key1=\"alice\""));
        assert!(xml.contains("A &amp; B&#13;&#10;next"));
        assert!(!xml.contains("ExportFormat"));
        assert!(build(&row("<CustomizationDS>")).is_err());
        assert!(build(&row("<Other/>")).is_err());
    }
    #[test]
    fn decoded_text_preserves_references_cdata_and_xml_line_endings() {
        let node=parse("<CustomizationDS><Properties><PropertyValue>Ω&amp;&lt;&#13;\n<![CDATA[x<y]]></PropertyValue></Properties></CustomizationDS>").unwrap();
        let mut writer = Writer::new(Vec::new());
        node.write(&mut writer).unwrap();
        let xml = String::from_utf8(writer.into_inner()).unwrap();
        assert!(xml.contains("Ω&amp;&lt;&#13;\nx&lt;y"));
    }
    #[test]
    fn native_non_null_columns_and_namespace_are_checked_before_export() {
        assert!(build(&row("<CustomizationDS><Properties><PropertyValue>x</PropertyValue></Properties></CustomizationDS>")).is_err());
        assert!(build(&row(&format!("<CustomizationDS xmlns=\"{NS}\"><Properties><PropertyValue>x</PropertyValue></Properties></CustomizationDS>"))).is_err());
    }
    #[test]
    #[ignore = "Requires an explicitly prepared native personalization corpus."]
    fn reconstruct_native_corpus() {
        let path =
            std::path::PathBuf::from(std::env::var("HERBERT_PERSONALIZATION_CORPUS").unwrap());
        let rows: Vec<Value> =
            serde_json::from_slice(&std::fs::read(path.join("rows.json")).unwrap()).unwrap();
        std::fs::create_dir_all(path.join("rebuilt")).unwrap();
        for (index, row) in rows.iter().enumerate() {
            std::fs::write(
                path.join("rebuilt").join(format!("{index}.xml")),
                build(row).unwrap(),
            )
            .unwrap();
        }
    }
}
fn schema() -> Element {
    // These columns and their order come from CustomizationDS.InitClass.
    let mut schema = Element::new("xs:schema");
    for (key, value) in [
        ("id", "CustomizationDS"),
        ("targetNamespace", NS),
        ("xmlns:mstns", NS),
        ("xmlns", NS),
        ("xmlns:xs", "http://www.w3.org/2001/XMLSchema"),
        ("xmlns:msdata", "urn:schemas-microsoft-com:xml-msdata"),
        ("xmlns:msprop", "urn:schemas-microsoft-com:xml-msprop"),
        ("attributeFormDefault", "qualified"),
        ("elementFormDefault", "qualified"),
    ] {
        schema.set(key, value);
    }
    let mut dataset = Element::new("xs:element");
    dataset.set("name", "CustomizationDS");
    dataset.set("msdata:IsDataSet", "true");
    dataset.set("msdata:Locale", "");
    let mut complex = Element::new("xs:complexType");
    let mut choice = Element::new("xs:choice");
    choice.set("minOccurs", "0");
    choice.set("maxOccurs", "unbounded");
    for (table, columns) in COLUMNS {
        let mut table_node = Element::new("xs:element");
        table_node.set("name", table);
        let mut table_type = Element::new("xs:complexType");
        let mut sequence = Element::new("xs:sequence");
        for column in columns {
            let mut field = Element::new("xs:element");
            field.set("name", column);
            field.set("type", "xs:string");
            sequence.push(field);
        }
        table_type.push(sequence);
        table_node.push(table_type);
        choice.push(table_node);
    }
    complex.push(choice);
    dataset.push(complex);
    schema.push(dataset);
    schema
}
pub fn build(row: &Value) -> Result<String, Diagnostic> {
    for key in crate::classic_archive::KEYS {
        if !row[key].is_string() {
            return Err(invalid("The personalization identity is incomplete."));
        }
    }
    if row["TypeCode"] != "Personalization" || row["Key1"].as_str() == Some("") {
        return Err(invalid("The personalization type or owner is missing."));
    }
    let content = row["Content"]
        .as_str()
        .ok_or_else(|| invalid("The personalization Content is missing."))?;
    let mut root = parse(content)?;
    if root.name != "CustomizationDS" {
        return Err(invalid("Personalization Content is not a CustomizationDS."));
    }
    // The typed reader ignores unqualified rows. Do not turn those rows into
    // a different export by adding a namespace to them.
    if root.attr("xmlns") != Some(NS)
        && root
            .children()
            .any(|e| e.name == "Controls" || e.name == "Properties")
    {
        return Err(invalid(
            "Personalization rows have no native DataSet namespace. The original Content is retained.",
        ));
    }
    if root
        .attr("xmlns")
        .is_some_and(|ns| !ns.is_empty() && ns != NS)
    {
        return Err(invalid(
            "The personalization DataSet has an unexpected namespace.",
        ));
    }
    root.set("xmlns", NS);
    let mut native = schema();
    if let Some(existing) = root.child_mut("schema") {
        // Native ReadXml keeps extra tables and DataSet properties. For the
        // two typed tables, it uses CustomizationDS columns and constraints.
        let dataset = existing
            .child_mut("element")
            .ok_or_else(|| invalid("The personalization schema has no DataSet."))?;
        if dataset.attr("name") != Some("CustomizationDS") {
            return Err(invalid("The personalization schema names another DataSet."));
        }
        let choice = dataset
            .child_mut("complexType")
            .and_then(|e| e.child_mut("choice"))
            .ok_or_else(|| invalid("The personalization schema has no table list."))?;
        let native_choice = native
            .child_mut("element")
            .unwrap()
            .child_mut("complexType")
            .unwrap()
            .child_mut("choice")
            .unwrap();
        choice.content.retain(|c| !matches!(c, Content::Element(e) if matches!(e.attr("name"), Some("Controls" | "Properties"))));
        for table in native_choice.children().rev().cloned().collect::<Vec<_>>() {
            choice.content.insert(0, Content::Element(table));
        }
        // Controls must precede Properties: the native importer uses table 0.
        choice.content.sort_by_key(|c| match c {
            Content::Element(e) if e.attr("name") == Some("Controls") => 0,
            Content::Element(e) if e.attr("name") == Some("Properties") => 1,
            _ => 2,
        });
        existing.set("xmlns:msprop", "urn:schemas-microsoft-com:xml-msprop");
    } else {
        root.content.insert(0, Content::Element(native));
    }
    for child in &mut root.content {
        let Content::Element(record) = child else {
            continue;
        };
        let Some((_, columns)) = COLUMNS.iter().find(|(table, _)| *table == record.name) else {
            continue;
        };
        let mut cells = Vec::new();
        for column in columns {
            let matches: Vec<_> = record.children().filter(|e| e.name == *column).collect();
            if matches.len() != 1
                || matches[0].children().next().is_some()
                || matches[0]
                    .attributes
                    .iter()
                    .any(|(k, v)| k.ends_with(":nil") && matches!(v.as_str(), "true" | "1"))
            {
                return Err(invalid(
                    "A personalization row violates the native typed DataSet columns. The original Content is retained.",
                ));
            }
            cells.push(Content::Element(matches[0].clone()));
        }
        // The typed reader ignores extra columns in these existing tables.
        // The source JSON retains them, just as it retains the full Content.
        record.content = cells;
    }
    let dataset = root
        .child_mut("schema")
        .unwrap()
        .child_mut("element")
        .unwrap();
    for key in crate::classic_archive::KEYS
        .into_iter()
        .chain(["Description", "CommentText"])
    {
        dataset.set(&format!("msprop:{key}"), row[key].as_str().unwrap_or(""));
    }
    let mut writer = Writer::new(Vec::new());
    root.write(&mut writer)?;
    String::from_utf8(writer.into_inner())
        .map_err(|_| invalid("The personalization XML is not UTF-8."))
}
