//! Build the DiffGram read by DashboardDataManager.prepareForImport.
//! Keep the persisted dataset intact. Native export clears only CGCCode and
//! replaces ExportBAQs with the selected native BAQ packages.
use crate::xml_document::{Content, Element, parse};
use herbert_contract::Diagnostic;
use std::collections::{BTreeMap, BTreeSet};

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("DASHBOARD_EXPORT_INVALID", message)
}
fn text(element: &Element) -> String {
    element
        .content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}
fn dataset(xml: &str) -> Result<Element, Diagnostic> {
    let root = parse(xml)?;
    if root.name != "DashboardDataSet"
        || root.attr("xmlns") != Some("http://tempuri.org/DashboardDataSet.xsd")
    {
        return Err(invalid(
            "The stored dashboard is not an Epicor DashboardDataSet.",
        ));
    }
    Ok(root)
}
pub fn queries(xml: &str) -> Result<BTreeSet<String>, Diagnostic> {
    Ok(dataset(xml)?
        .children()
        .filter(|r| r.name == "DashboardQuery")
        .flat_map(|r| r.children().filter(|c| c.name == "DynamicQueryID"))
        .map(text)
        .filter(|s| !s.is_empty())
        .collect())
}
pub fn build(xml: &str, packages: &BTreeMap<String, String>) -> Result<String, Diagnostic> {
    let mut root = dataset(xml)?;
    root.content.retain(|c| !matches!(c, Content::Element(e) if e.name == "ExportBAQs" || e.name.ends_with(":schema")));
    for child in &mut root.content {
        let Content::Element(row) = child else {
            continue;
        };
        if row.name == "DashboardProperties"
            && row
                .children()
                .any(|c| c.name == "PropertyID" && text(c) == "CGCCode")
        {
            if let Some(value) = row.child_mut("PropertyValue") {
                value.content.clear();
            }
        }
    }
    for (id, bytes) in packages {
        let mut row = Element::new("ExportBAQs");
        for (name, value) in [("OriginalExportID", id), ("DynamicQueryDataSet", bytes)] {
            let mut field = Element::new(name);
            field.content.push(Content::Text(value.clone()));
            row.push(field);
        }
        root.push(row);
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for child in &mut root.content {
        let Content::Element(row) = child else {
            continue;
        };
        let index = counts.entry(row.name.clone()).or_default();
        row.set("diffgr:id", &format!("{}{n}", row.name, n = *index + 1));
        row.set("msdata:rowOrder", &index.to_string());
        *index += 1;
    }
    let mut diff = Element::new("diffgr:diffgram");
    diff.set("xmlns:msdata", "urn:schemas-microsoft-com:xml-msdata");
    diff.set("xmlns:diffgr", "urn:schemas-microsoft-com:xml-diffgram-v1");
    diff.push(root);
    let mut writer = quick_xml::Writer::new(Vec::new());
    diff.write(&mut writer)?;
    String::from_utf8(writer.into_inner())
        .map_err(|_| invalid("The dashboard export from Epicor is not readable text, so no dashboard file was built."))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diffgram_preserves_layout_and_embeds_unique_native_queries() {
        let xml = r#"<DashboardDataSet xmlns="http://tempuri.org/DashboardDataSet.xsd"><DashboardDefinition><DefinitionID>D</DefinitionID><DockLayoutXML>&lt;Layout x=&quot;a&quot;/&gt;&#13;
</DockLayoutXML></DashboardDefinition><DashboardProperties><PropertyID>CGCCode</PropertyID><PropertyValue>US</PropertyValue></DashboardProperties><DashboardQuery><DynamicQueryID>Q</DynamicQueryID></DashboardQuery><DashboardQuery><DynamicQueryID>Q</DynamicQueryID></DashboardQuery><ExportBAQs><OriginalExportID>old</OriginalExportID></ExportBAQs></DashboardDataSet>"#;
        assert_eq!(queries(xml).unwrap(), BTreeSet::from(["Q".into()]));
        let output = build(xml, &BTreeMap::from([("Q".into(), "UEs=".into())])).unwrap();
        assert!(output.contains("diffgr:diffgram"));
        assert!(output.contains("&#13;\n"));
        assert!(!output.contains(">US<"));
        assert!(!output.contains(">old<"));
        assert_eq!(output.matches("<ExportBAQs ").count(), 1);
        assert!(build("<Other/>", &BTreeMap::new()).is_err());
    }
    #[test]
    #[ignore = "Requires the explicitly prepared native dashboard corpus."]
    fn reconstruct_native_corpus() {
        let path = std::path::PathBuf::from(std::env::var("HERBERT_DASHBOARD_CORPUS").unwrap());
        let response: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.join("rows.json")).unwrap()).unwrap();
        std::fs::create_dir_all(path.join("rebuilt")).unwrap();
        for (index, row) in response["returnObj"]["DashBdDef"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            std::fs::write(
                path.join("rebuilt").join(format!("{index}.dbd")),
                build(row["DashboardSchema"].as_str().unwrap(), &BTreeMap::new()).unwrap(),
            )
            .unwrap();
        }
    }
}
