//! Native global-file AppContent projection from complete measured XXXDef rows.
//! Acquisition retains all rows; this projection selects the native import payload.
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("CONFIGURATOR_APP_INVALID", message)
}
/// Low-level projection. Production callers use from_visible to establish native
/// file selection; signature applicability is checked separately.
pub fn from_rows(company: &str, config: &str, rows: &[Value]) -> Result<Value, Diagnostic> {
    if company.is_empty() || config.is_empty() {
        return Err(invalid(
            "App projection requires company and configurator identity.",
        ));
    }
    let view = format!("Erp.UICfg.{config}_{company}");
    let mut identities = BTreeSet::new();
    let mut global: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut company_paths = BTreeSet::new();
    for row in rows {
        let owner = row["Company"]
            .as_str()
            .ok_or_else(|| invalid("App row omitted Company."))?;
        if (owner != "" && owner != company)
            || row["ProductID"] != "EP"
            || row["TypeCode"] != "KNTCMetaUI"
            || row["Key1"]
                .as_str()
                .is_none_or(|v| !v.eq_ignore_ascii_case(&view))
            || row["SystemFlag"] != false
        {
            return Err(invalid("App body lacks exact custom ownership."));
        }
        let id = row["SysRowID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("App row omitted SysRowID."))?;
        if !identities.insert(id.to_ascii_lowercase()) || row["SysRevID"].is_null() {
            return Err(invalid(
                "App row has duplicate identity or missing revision.",
            ));
        }
        let content = row["Content"]
            .as_str()
            .ok_or_else(|| invalid("App row omitted Content."))?;
        let bytes = row["HerbertContentBytes"]
            .as_u64()
            .or_else(|| row["HerbertContentBytes"].as_str()?.parse().ok());
        if bytes != Some(content.encode_utf16().count() as u64 * 2) {
            return Err(invalid("App content failed its native length check."));
        }
        let country = row["CGCCode"]
            .as_str()
            .ok_or_else(|| invalid("App row omitted CGCCode."))?;
        let revision = row["Key3"]
            .as_str()
            .ok_or_else(|| invalid("App row omitted Key3."))?;
        // ExportApp binds the base country group and current key. Alternative
        // country/revision rows remain in the caller's retained source graph.
        if country != "" || revision != "" {
            continue;
        }
        let path = row["Key2"]
            .as_str()
            .ok_or_else(|| invalid("App row omitted Key2."))?
            .replace('\\', "/");
        let (prefix, name) = path
            .split_once('/')
            .ok_or_else(|| invalid("App path omitted its owner prefix."))?;
        if !prefix.eq_ignore_ascii_case(&view)
            || name.is_empty()
            || name.contains([':', '\0'])
            || name.split('/').any(|p| matches!(p, "" | "." | ".."))
        {
            return Err(invalid(
                "App path is not a safe relative file for this configurator.",
            ));
        }
        let key = name.to_ascii_lowercase();
        if owner == "" {
            if global.insert(key, (name.into(), content.into())).is_some() {
                return Err(invalid("Multiple global rows map to one native app path."));
            }
        } else {
            company_paths.insert(key);
        }
    }
    // Both verified versions prefer global file bodies. Do not silently
    // substitute company-only content.
    if company_paths.iter().any(|p| !global.contains_key(p)) {
        return Err(invalid(
            "A company-only app file has no native global source; retain source and use native fallback.",
        ));
    }
    Ok(
        json!({"ViewId":config,"CompanyId":Value::Null,"Files":global.into_values().collect::<BTreeMap<_,_>>()}),
    )
}
/// Native GetList supplies session-filtered filenames without downloading bodies.
/// Only verified global rows take the local route. Other ownership selections
/// retain their source and use the native fallback; no tenant identity is guessed.
pub fn from_visible(company: &str, config: &str, source: &Value) -> Result<Value, Diagnostic> {
    if source["complete"] != true {
        return Err(invalid("App acquisition is incomplete."));
    }
    let rows = source["rows"]
        .as_array()
        .ok_or_else(|| invalid("App source omitted rows."))?;
    let visible = source["visibleRows"]
        .as_array()
        .ok_or_else(|| invalid("App source omitted native selection."))?;
    let system = source["systemRows"]
        .as_array()
        .ok_or_else(|| invalid("App source omitted System metadata."))?;
    if !system.is_empty() {
        return Err(invalid(
            "System app metadata requires the filtered native path.",
        ));
    }
    let view = format!("Erp.UICfg.{config}_{company}");
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for item in visible {
        if item["Company"] != ""
            || item["ProductID"] != "EP"
            || item["TypeCode"] != "KNTCMetaUI"
            || item["CGCCode"] != ""
            || item["Key3"] != ""
            || item["SystemFlag"] != false
            || item["Key1"]
                .as_str()
                .is_none_or(|v| !v.eq_ignore_ascii_case(&view))
            || item.get("Content").is_some()
        {
            return Err(invalid(
                "Native app selection requires an unresolved ownership or country path.",
            ));
        }
        let id = item["SysRowID"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("Native app selection omitted identity."))?;
        if !seen.insert(id.to_ascii_lowercase()) {
            return Err(invalid("Native app selection repeated an identity."));
        }
        let matches: Vec<_> = rows
            .iter()
            .filter(|r| {
                r["SysRowID"]
                    .as_str()
                    .is_some_and(|v| v.eq_ignore_ascii_case(id))
            })
            .collect();
        if matches.len() != 1 {
            return Err(invalid(
                "A native-selected file lacks a unique downloaded source.",
            ));
        }
        let row = matches[0];
        for key in [
            "Company",
            "ProductID",
            "TypeCode",
            "CGCCode",
            "Key1",
            "Key2",
            "Key3",
            "SystemFlag",
            "SysRevID",
        ] {
            if item[key].is_null() || item[key] != row[key] {
                return Err(invalid(
                    "Native file selection changed after its body was read.",
                ));
            }
        }
        selected.push(row.clone());
    }
    if selected.is_empty() {
        return Err(invalid("Native app selection contains no files."));
    }
    from_rows(company, config, &selected)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn row(company: &str, name: &str, text: &str) -> Value {
        json!({"Company":company,"ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":"Erp.UICfg.Cfg_C","Key2":format!("Erp.UICfg.Cfg_C/{name}"),"Key3":"","CGCCode":"","SystemFlag":false,"SysRowID":format!("{company}:{name}"),"SysRevID":1,"Content":text,"HerbertContentBytes":text.encode_utf16().count()*2})
    }
    #[test]
    fn projection_preserves_native_global_precedence_and_has_no_export_window() {
        let mut rows: Vec<_> = (0..151)
            .map(|n| row("", &format!("pages/{n}.jsonc"), "Ω\r\n{}"))
            .collect();
        rows.push(row("C", "pages/0.jsonc", "company content"));
        let before = rows.clone();
        let app = from_rows("C", "Cfg", &rows).unwrap();
        assert_eq!(app["Files"].as_object().unwrap().len(), 151);
        assert_eq!(app["Files"]["pages/0.jsonc"], "Ω\r\n{}");
        assert_eq!(rows, before);
        let mut alternate = row("", "archive.jsonc", "old");
        alternate["Key3"] = json!("history");
        rows.push(alternate);
        assert_eq!(from_rows("C", "Cfg", &rows).unwrap(), app);
    }
    #[test]
    fn malformed_ambiguous_and_foreign_app_rows_do_not_make_packages() {
        for field in [
            "Company",
            "ProductID",
            "TypeCode",
            "Key1",
            "SystemFlag",
            "SysRowID",
            "SysRevID",
            "Content",
            "HerbertContentBytes",
            "CGCCode",
            "Key3",
            "Key2",
        ] {
            let mut r = row("", "a.jsonc", "{}");
            r.as_object_mut().unwrap().remove(field);
            assert!(from_rows("C", "Cfg", &[r]).is_err(), "{field}");
        }
        assert!(from_rows("C", "Cfg", &[row("C", "only.jsonc", "{}")]).is_err());
        assert!(from_rows("C", "Cfg", &[row("", "../escape.jsonc", "{}")]).is_err());
        assert!(
            from_rows(
                "C",
                "Cfg",
                &[row("", "A.jsonc", "{}"), row("", "a.jsonc", "other")]
            )
            .is_err()
        );
    }
    #[test]
    fn native_selection_requires_matching_revision_and_custom_global_sources() {
        let r = row("", "a.jsonc", "{}");
        let mut metadata = r.clone();
        metadata.as_object_mut().unwrap().remove("Content");
        metadata
            .as_object_mut()
            .unwrap()
            .remove("HerbertContentBytes");
        let source = json!({"complete":true,"rows":[r],"systemRows":[],"visibleRows":[metadata]});
        assert!(from_visible("C", "Cfg", &source).is_ok());
        for (key, value) in [
            ("Company", json!("T_unknown")),
            ("CGCCode", json!("US")),
            ("SystemFlag", json!(true)),
            ("SysRevID", json!(2)),
            ("Key1", json!("Other")),
            ("SysRowID", json!("missing")),
            ("Content", json!("unexpected")),
        ] {
            let mut bad = source.clone();
            bad["visibleRows"][0][key] = value;
            assert!(from_visible("C", "Cfg", &bad).is_err(), "{key}");
        }
        let mut bad = source.clone();
        bad["complete"] = json!(false);
        assert!(from_visible("C", "Cfg", &bad).is_err());
        let mut bad = source.clone();
        bad["systemRows"] = json!([{"SystemFlag":true}]);
        assert!(from_visible("C", "Cfg", &bad).is_err());
        let mut bad = source.clone();
        bad["visibleRows"] = json!([metadata, metadata]);
        assert!(from_visible("C", "Cfg", &bad).is_err());
        let mut extra = row("", "not-selected.jsonc", "retained source");
        extra["SysRowID"] = json!("extra");
        let mut extended = source.clone();
        extended["rows"].as_array_mut().unwrap().push(extra);
        assert_eq!(
            from_visible("C", "Cfg", &extended).unwrap(),
            from_visible("C", "Cfg", &source).unwrap()
        );
    }
    #[test]
    fn supplied_native_app_projection_matches_both_labs() {
        let Ok(base) = std::env::var("HERBERT_C19_REPLAY") else {
            return;
        };
        for lane in ["2026.1", "2025.2"] {
            let base = std::path::Path::new(&base).join(lane);
            let read = |p: &str| -> Value {
                serde_json::from_slice(&std::fs::read(base.join(p)).unwrap()).unwrap()
            };
            let groups = read("kinetic-app-rows-complete/result.json");
            let rows: Vec<_> = groups
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|g| g["rows"].as_array().unwrap().iter().cloned())
                .collect();
            let actual = from_rows("TESTCO", "H19K1024A", &rows).unwrap();
            assert_eq!(actual, read("oracle/kinetic/app.json"));
            let package = crate::configurator_archive::kinetic_package(
                &read("stored/kinetic/rows.json"),
                &BTreeMap::new(),
                &actual,
            )
            .unwrap();
            assert_eq!(
                package,
                std::fs::read(base.join("oracle/kinetic/rust-local.zip")).unwrap(),
                "Stored-only AppContent produces the previously native-imported package"
            );
            std::fs::write(
                base.join("kinetic-app-rows-complete/local-app.json"),
                serde_json::to_vec_pretty(&actual).unwrap(),
            )
            .unwrap();
        }
    }
}
