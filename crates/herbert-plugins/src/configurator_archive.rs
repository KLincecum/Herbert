//! Local ConfigImportExport XML projection. Original rows remain with the caller.
//! Rule eligibility must come from the native BOM selection contract, not row-ID presence.
use herbert_contract::Diagnostic;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
struct Schema {
    root: String,
    namespace: String,
    order: Vec<String>,
    tables: BTreeMap<String, Vec<(String, String)>>,
}
fn invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("CONFIGURATOR_XML_INVALID", message)
}
fn key<'a>(row: &'a Value, field: &str) -> Result<&'a str, Diagnostic> {
    row[field]
        .as_str()
        .ok_or_else(|| invalid(format!("Configurator row omitted {field}.")))
}
fn rule_key(row: &Value) -> Result<String, Diagnostic> {
    let fields = ["PartNum", "RevisionNum", "AltMethod", "RuleSetID"];
    let values: Vec<_> = fields
        .iter()
        .map(|f| {
            row.get(*f)
                .filter(|v| !v.is_null())
                .ok_or_else(|| invalid(format!("Rule row omitted {f}.")))
        })
        .collect::<Result<_, _>>()?;
    Ok(serde_json::to_string(&values).unwrap())
}

/// Construct the full-options native XML payload. Kinetic app ZIP construction
/// and any required native signature are separate operations; XML alone does
/// not establish an importable Kinetic or signed deployment package.
pub fn xml(data: &Value, rule_eligibility: &BTreeMap<String, bool>) -> Result<String, Diagnostic> {
    let schema: Schema = serde_json::from_str(include_str!("configurator-dataset.json")).unwrap();
    let roots = data["PcStatus"]
        .as_array()
        .filter(|r| r.len() == 1)
        .ok_or_else(|| invalid("Configurator XML requires one root."))?;
    let root = &roots[0];
    let company = key(root, "Company")?;
    let config = key(root, "ConfigID")?;
    if config.is_empty() {
        return Err(invalid("Configurator ID is empty."));
    }
    let config_type = key(root, "ConfigType")?;
    let nic = config_type.eq_ignore_ascii_case("NIC");
    let ewc = config_type.eq_ignore_ascii_case("EWC");
    let input_tables = [
        "PcDynLst",
        "PcDynLstCriteria",
        "PcDynLstParams",
        "PcInputs",
        "PcInputsExpr",
        "PcInputsLayerHeader",
        "PcInputsLayerDetail",
        "PcInputsRule",
        "PcInputsRuleDef",
        "PcPage",
        "PcPageExpr",
        "QBuildMapping",
        "QBuildObj",
        "FileStore",
    ];
    // Even excluded source tables must have been read. An absent table is not
    // proof that it is empty; keep export selection separate from acquisition.
    for table in &schema.order {
        let rows = data[table]
            .as_array()
            .ok_or_else(|| invalid(format!("Configurator data omitted {table}.")))?;
        let mut seen = BTreeSet::new();
        for row in rows {
            if key(row, "Company")? != company
                || (!matches!(table.as_str(), "FileStore" | "PcInputVar")
                    && key(row, "ConfigID")? != config)
            {
                return Err(invalid(format!(
                    "{table} contains another company or configurator."
                )));
            }
            let id = key(row, "SysRowID")?;
            if id.is_empty() || !seen.insert(id.to_ascii_lowercase()) {
                return Err(invalid(format!(
                    "{table} has an absent or repeated row identity."
                )));
            }
        }
    }
    let mut eligible = BTreeSet::new();
    for row in data["PcRuleSet"].as_array().unwrap() {
        let id = key(row, "SysRowID")?;
        match rule_eligibility.get(id) {
            Some(true) => {
                eligible.insert(rule_key(row)?);
            }
            Some(false) => {}
            None => {
                return Err(invalid(format!(
                    "Native BOM eligibility is unresolved for rule set {id}."
                )));
            }
        }
    }
    let mut selected_rules = BTreeSet::new();
    for row in data["PcRules"].as_array().unwrap() {
        if eligible.contains(&rule_key(row)?) {
            let seq = row
                .get("RuleSeq")
                .filter(|v| v.is_i64())
                .ok_or_else(|| invalid("Rule omitted its integer sequence."))?;
            selected_rules.insert((rule_key(row)?, seq.to_string()));
        }
    }
    let drawings: BTreeSet<_> = data["PcInputs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| {
            r["ControlType"].as_str().is_some_and(|s| {
                s.eq_ignore_ascii_case(
                    "Erp.Lib.Configurator.PcControls.EpiPc2DRunTime, Erp.Lib.Configurator",
                )
            })
        })
        .map(|r| Ok(format!("{config}_{}.dat", key(r, "InputName")?)))
        .collect::<Result<_, Diagnostic>>()?;
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><{} xmlns=\"{}\">",
        schema.root, schema.namespace
    );
    for table in &schema.order {
        if (nic
            && (input_tables.contains(&table.as_str())
                || matches!(table.as_str(), "PcStrComp" | "PcInputVar")))
            || (ewc && matches!(table.as_str(), "QBuildMapping" | "QBuildObj" | "FileStore"))
        {
            continue;
        }
        for row in data[table].as_array().unwrap() {
            if table == "PcInputsExpr"
                && !(row["TypeCode"] == "ReadOnlyExpression"
                    || ((row["TypeCode"] == "Validate" || row["TypeCode"] == "Changed")
                        && row["Expression"].as_str().is_some_and(|s| !s.is_empty())))
            {
                continue;
            }
            if matches!(table.as_str(), "PcRuleSet" | "PcRules")
                && !eligible.contains(&rule_key(row)?)
            {
                continue;
            }
            if table == "PcRulesExpr"
                && !selected_rules.contains(&(rule_key(row)?, row["RuleSeq"].to_string()))
            {
                continue;
            }
            if table == "FileStore" && !drawings.contains(key(row, "FileName")?) {
                return Err(invalid("FileStore contains an unrelated drawing."));
            }
            text.push_str(&format!("<{table}>"));
            for (field, kind) in &schema.tables[table] {
                let value = row
                    .get(field)
                    .ok_or_else(|| invalid(format!("{table}.{field} was not downloaded.")))?;
                if value.is_null() {
                    continue;
                }
                let mut value = match (kind.as_str(), value) {
                    ("string" | "Guid" | "DateTime" | "byte[]", Value::String(s)) => s.clone(),
                    ("bool", Value::Bool(b)) => b.to_string(),
                    (
                        "int" | "long" | "uint" | "ulong" | "decimal" | "short" | "byte" | "double",
                        Value::Number(n),
                    ) => n.to_string(),
                    ("ulong", Value::String(s)) if s.parse::<u64>().is_ok() => s.clone(),
                    _ => {
                        return Err(invalid(format!(
                            "{table}.{field} does not match native {kind}."
                        )));
                    }
                };
                if table == "PcInputs"
                    && !ewc
                    && matches!(field.as_str(), "OnLeave" | "OnLeave2")
                    && value.trim().replace("\r\n", "").replace('\n', "") == "..."
                {
                    value.clear();
                }
                if nic {
                    if table == "PcTargetEntity" && field == "IncomingSmartString" {
                        value = "false".into();
                    }
                    if table == "PcStatus" {
                        match field.as_str() {
                            "SampleSmartString" | "StringStyle" | "Separator" | "NumberFormat" => {
                                value.clear()
                            }
                            "StartNumber" => value = "0".into(),
                            "PrefacePart" | "CrtCustPart" | "CrtPartDesc" => value = "false".into(),
                            _ => {}
                        }
                    }
                }
                // Native XmlWriter normalizes line endings. Original source is
                // retained in raw rows, independently of this import projection.
                value = value.replace("\r\n", "\n").replace('\r', "\n");
                let escaped = crate::classic_archive::escape(&value)?.replace('\n', "\r\n");
                text.push_str(&format!("<{field}>{escaped}</{field}>"));
            }
            // SampleSmartString is not persisted. The native exporter explicitly
            // assigns its empty DataSet value when smart strings are disabled.
            if nic && table == "PcStatus" {
                text.push_str("<SampleSmartString></SampleSmartString>");
            }
            text.push_str(&format!("</{table}>"));
        }
    }
    text.push_str(&format!("</{}>", schema.root));
    Ok(text)
}

/// Package native AppContent with the locally projected configurator XML.
/// The caller must separately establish signature applicability and export policy.
/// AppContent remains intact; this does not resolve referenced external resources.
pub fn kinetic_package(
    data: &Value,
    rule_eligibility: &BTreeMap<String, bool>,
    app: &Value,
) -> Result<Vec<u8>, Diagnostic> {
    let text = xml(data, rule_eligibility)?;
    validate_kinetic_app(data, app)?;
    let config = key(&data["PcStatus"][0], "ConfigID")?;
    let mut app_text = serde_json::to_vec(app)
        .map_err(|e| invalid(format!("Cannot serialize AppContent: {e}")))?;
    // Native ExportApp writes one JSON object followed by the server newline.
    // The importer parses JSON; package validity does not depend on its spelling.
    app_text.push(b'\n');
    crate::kinetic_archive::zip(&BTreeMap::from([
        (format!("{config}.jsonc"), app_text),
        (format!("{config}.xml"), text.into_bytes()),
    ]))
}

pub fn validate_kinetic_app(data: &Value, app: &Value) -> Result<(), Diagnostic> {
    let root = &data["PcStatus"][0];
    if root["Kinetic"] != true {
        return Err(invalid(
            "A Kinetic package requires a Kinetic configurator.",
        ));
    }
    let config = key(root, "ConfigID")?;
    let company = key(root, "Company")?;
    if config.contains(['/', '\\', ':', '\0']) || matches!(config, "." | "..") {
        return Err(invalid(
            "Configurator ID cannot be used as a native package filename.",
        ));
    }
    let view = format!("Erp.UICfg.{config}_{company}");
    if !(app["ViewId"] == config || app["ViewId"] == view)
        || !(app["CompanyId"].is_null() || app["CompanyId"] == company)
    {
        return Err(invalid(
            "AppContent belongs to another configurator or company.",
        ));
    }
    let files = app["Files"]
        .as_object()
        .ok_or_else(|| invalid("AppContent omitted its file map."))?;
    for (name, value) in files {
        if !value.is_string()
            || name.is_empty()
            || name.starts_with('/')
            || name.contains(['\\', ':', '\0'])
            || name.split('/').any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(invalid(
                "AppContent contains an invalid relative filename or text value.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_document::Content;
    #[test]
    fn supplied_native_nic_bulk_replay() {
        let Ok(base) = std::env::var("HERBERT_C19_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let folder = std::path::Path::new(&base).join(lane);
            let grouped: Value = serde_json::from_slice(
                &std::fs::read(folder.join("bulk-two-root/grouped.json")).unwrap(),
            )
            .unwrap();
            let actual = xml(&grouped["H19N1024A"], &BTreeMap::new()).unwrap();
            let native = std::fs::read_to_string(folder.join("oracle/nic/native.xml")).unwrap();
            assert_eq!(normalize(&actual), normalize(&native), "{lane} NIC");
            std::fs::write(folder.join("oracle/nic/rust-local.xml"), actual).unwrap();
        }
    }
    #[test]
    fn supplied_native_kinetic_package_replay() {
        use std::io::{Cursor, Read};
        let Ok(base) = std::env::var("HERBERT_C19_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let base = std::path::Path::new(&base).join(lane);
            let rows: Value = serde_json::from_slice(
                &std::fs::read(base.join("stored/kinetic/rows.json")).unwrap(),
            )
            .unwrap();
            let app: Value = serde_json::from_slice(
                &std::fs::read(base.join("oracle/kinetic/app.json")).unwrap(),
            )
            .unwrap();
            let original = app.clone();
            let bytes = kinetic_package(&rows, &BTreeMap::new(), &app).unwrap();
            assert_eq!(app, original);
            let mut local = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let mut native = zip::ZipArchive::new(Cursor::new(
                std::fs::read(base.join("oracle/kinetic/native.zip")).unwrap(),
            ))
            .unwrap();
            assert_eq!(local.len(), 2);
            assert_eq!(native.len(), 2);
            let mut native_app = String::new();
            native
                .by_name("H19K1024A.jsonc")
                .unwrap()
                .read_to_string(&mut native_app)
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(&native_app).unwrap(), app);
            let mut local_app = String::new();
            local
                .by_name("H19K1024A.jsonc")
                .unwrap()
                .read_to_string(&mut local_app)
                .unwrap();
            assert_eq!(serde_json::from_str::<Value>(&local_app).unwrap(), app);
            let mut local_xml = String::new();
            local
                .by_name("H19K1024A.xml")
                .unwrap()
                .read_to_string(&mut local_xml)
                .unwrap();
            let native_xml =
                std::fs::read_to_string(base.join("oracle/kinetic/native.xml")).unwrap();
            assert_eq!(normalize(&local_xml), normalize(&native_xml));
            std::fs::write(base.join("oracle/kinetic/rust-local.zip"), &bytes).unwrap();
            let mut wrong = app.clone();
            wrong["ViewId"] = serde_json::json!("Other");
            assert!(kinetic_package(&rows, &BTreeMap::new(), &wrong).is_err());
            let mut wrong = app.clone();
            wrong["CompanyId"] = serde_json::json!("Other");
            assert!(kinetic_package(&rows, &BTreeMap::new(), &wrong).is_err());
            let mut wrong = app.clone();
            wrong["Files"]["../outside.jsonc"] = serde_json::json!("{}");
            assert!(kinetic_package(&rows, &BTreeMap::new(), &wrong).is_err());
        }
    }
    fn fixture() -> Value {
        let schema: Schema =
            serde_json::from_str(include_str!("configurator-dataset.json")).unwrap();
        let mut data = serde_json::json!({});
        for table in &schema.order {
            data[table] = serde_json::json!([]);
        }
        let mut root = serde_json::json!({});
        for (field, kind) in &schema.tables["PcStatus"] {
            root[field] = match kind.as_str() {
                "bool" => serde_json::json!(false),
                "int" | "long" | "decimal" | "double" | "uint" | "ulong" => serde_json::json!(0),
                "DateTime" => Value::Null,
                _ => serde_json::json!(""),
            };
        }
        root["Company"] = serde_json::json!("C");
        root["ConfigID"] = serde_json::json!("Cfg");
        root["ConfigType"] = serde_json::json!("PC");
        root["SysRowID"] = serde_json::json!("00000000-0000-0000-0000-000000000001");
        data["PcStatus"] = serde_json::json!([root]);
        data
    }
    #[test]
    fn native_line_endings_and_nic_projection_preserve_original_rows() {
        let mut data = fixture();
        data["PcStatus"][0]["Comments"] = serde_json::json!("one\r\ntwo\nthree\rfour Ω & <text>");
        let before = data.clone();
        let text = xml(&data, &BTreeMap::new()).unwrap();
        assert_eq!(data, before);
        assert!(text.contains("one\r\ntwo\r\nthree\r\nfour Ω &amp; &lt;text&gt;"));
        data["PcStatus"][0]["ConfigType"] = serde_json::json!("NIC");
        data["PcStatus"][0]["SampleSmartString"] = serde_json::json!("source");
        data["PcStatus"][0]["StartNumber"] = serde_json::json!(123);
        let text = xml(&data, &BTreeMap::new()).unwrap();
        assert!(text.contains("<StartNumber>0</StartNumber>"));
        assert!(text.contains("<SampleSmartString></SampleSmartString>"));
        assert_eq!(data["PcStatus"][0]["SampleSmartString"], "source");
    }
    #[test]
    fn unresolved_rules_and_incomplete_reads_cannot_be_exported() {
        let mut data = fixture();
        data.as_object_mut().unwrap().remove("PcFunctionParam");
        assert!(xml(&data, &BTreeMap::new()).is_err());
        let mut data = fixture();
        data["PcRuleSet"] = serde_json::json!([{"Company":"C","ConfigID":"Cfg","SysRowID":"rule"}]);
        assert!(
            xml(&data, &BTreeMap::new())
                .unwrap_err()
                .message
                .contains("eligibility")
        );
        let mut data = fixture();
        let row = data["PcStatus"][0].clone();
        data["PcStatus"].as_array_mut().unwrap().push(row);
        assert!(xml(&data, &BTreeMap::new()).is_err());
    }
    fn normalize(xml: &str) -> BTreeMap<(String, String), BTreeMap<String, String>> {
        let schema: Schema =
            serde_json::from_str(include_str!("configurator-dataset.json")).unwrap();
        crate::xml_document::parse(xml)
            .unwrap()
            .children()
            .map(|r| {
                let fields: BTreeMap<_, _> = r
                    .children()
                    .map(|f| {
                        let mut value: String = f
                            .content
                            .iter()
                            .filter_map(|c| {
                                if let Content::Text(t) = c {
                                    Some(t.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        value = value.replace("\r\n", "\n").replace('\r', "\n");
                        let kind = schema.tables[&r.name]
                            .iter()
                            .find(|(n, _)| n == &f.name)
                            .map(|(_, k)| k.as_str());
                        if matches!(kind, Some("decimal" | "double")) && value.contains('.') {
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
    fn supplied_native_configurator_replay() {
        let Ok(base) = std::env::var("HERBERT_C19_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            for fixture in ["classic", "classic-function", "kinetic"] {
                let base = std::path::Path::new(&base).join(lane);
                let rows: Value = serde_json::from_slice(
                    &std::fs::read(base.join(format!("stored/{fixture}/rows.json"))).unwrap(),
                )
                .unwrap();
                let before = rows.clone();
                let actual = xml(&rows, &BTreeMap::new()).unwrap();
                assert_eq!(rows, before);
                assert!(
                    !actual.replace("\r\n", "").contains('\n'),
                    "Native XML uses CRLF text line endings"
                );
                let native =
                    std::fs::read_to_string(base.join(format!("oracle/{fixture}/native.xml")))
                        .unwrap();
                assert_eq!(normalize(&actual), normalize(&native), "{lane}/{fixture}");
                std::fs::write(
                    base.join(format!("oracle/{fixture}/rust-local.xml")),
                    actual,
                )
                .unwrap();
                let mut wrong = rows.clone();
                wrong["PcInputs"][0]["Company"] = serde_json::json!("Other");
                assert!(xml(&wrong, &BTreeMap::new()).is_err());
                let mut missing = rows.clone();
                missing.as_object_mut().unwrap().remove("PcFunctionParam");
                assert!(xml(&missing, &BTreeMap::new()).is_err());
                let mut rules = rows.clone();
                rules["PcRuleSet"] = serde_json::json!([{"Company":"TESTCO","ConfigID":rows["PcStatus"][0]["ConfigID"],"SysRowID":"unresolved"}]);
                assert!(
                    xml(&rules, &BTreeMap::new())
                        .unwrap_err()
                        .message
                        .contains("eligibility")
                );
            }
        }
    }
}
