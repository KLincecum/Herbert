//! Build native MetaFX packages from complete persisted records.
//! Draft application files remain recovery files: ImportLayers ignores them.
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Write};

pub const TYPES: [&str; 5] = [
    "KNTCMetaUI",
    "KNTCCustLayer",
    "KNTCEventLayout",
    "KNTCLayerBackup",
    "KNTCUDApp",
];
pub const KEYS: [&str; 7] = [
    "Company",
    "ProductID",
    "TypeCode",
    "CGCCode",
    "Key1",
    "Key2",
    "Key3",
];
pub fn invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("KINETIC_PACKAGE_INVALID", message)
}
pub fn view(row: &Value) -> Option<&str> {
    row[if matches!(row["TypeCode"].as_str(), Some("KNTCMetaUI" | "KNTCUDApp")) {
        "Key1"
    } else {
        "Key2"
    }]
    .as_str()
    .filter(|s| !s.is_empty())
}
pub fn check_text(row: &Value, field: &str) -> Result<(), Diagnostic> {
    let text = row[field]
        .as_str()
        .ok_or_else(|| invalid(format!("The {field} text is missing.")))?;
    let length = &row[format!("Herbert{field}Bytes")];
    let expected = length
        .as_u64()
        .or_else(|| length.as_str().and_then(|s| s.parse().ok()));
    if expected != Some((text.encode_utf16().count() * 2) as u64) {
        return Err(invalid(format!(
            "The {field} text failed its length check. Its package was not built."
        )));
    }
    Ok(())
}
pub(crate) fn clean_row(row: &Value) -> Value {
    use base64::Engine;
    let mut row = row.clone();
    if let Some(fields) = row.as_object_mut() {
        fields.retain(|k, _| !k.starts_with("Herbert"));
    }
    // The database entity serializer writes SQL rowversion as base64. BAQ
    // projections return its numeric value; preserve the same eight bytes.
    if let Some(version) = row["SysRevID"]
        .as_u64()
        .or_else(|| row["SysRevID"].as_str().and_then(|s| s.parse::<u64>().ok()))
    {
        row["SysRevID"] =
            json!(base64::engine::general_purpose::STANDARD.encode(version.to_be_bytes()));
    }
    row["$id"] = json!("1");
    row
}
fn path(value: &str) -> Result<String, Diagnostic> {
    let value = value.replace('\\', "/");
    if value.is_empty()
        || value.starts_with('/')
        || value.contains(':')
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err(invalid(
            "An authored path is not a safe relative package path.",
        ));
    }
    Ok(value)
}
fn insert(
    entries: &mut BTreeMap<String, Vec<u8>>,
    name: String,
    bytes: Vec<u8>,
) -> Result<(), Diagnostic> {
    let key = name.to_lowercase();
    if let Some((_, previous)) = entries.iter().find(|(n, _)| n.to_lowercase() == key) {
        if previous != &bytes {
            return Err(invalid(format!(
                "Different definitions map to the package entry {name}. Both source records are preserved."
            )));
        }
    } else {
        entries.insert(name, bytes);
    }
    Ok(())
}
pub fn zip(entries: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, Diagnostic> {
    // Native import depends on entry bytes, not compression level. Use fast
    // Deflate so local compression does not hold up the received batch.
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(
                path(name)?,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(1)),
            )
            .map_err(|e| invalid(format!("Cannot create package entry: {e}")))?;
        writer
            .write_all(bytes)
            .map_err(|e| invalid(format!("Cannot write package entry: {e}")))?;
    }
    writer
        .finish()
        .map(|c| c.into_inner())
        .map_err(|e| invalid(format!("Cannot finish package: {e}")))
}
fn layer(
    entries: &mut BTreeMap<String, Vec<u8>>,
    row: &Value,
    rows: &[Value],
    context: &LayerScope<'_>,
    active: &mut BTreeSet<String>,
    done: &mut BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), Diagnostic> {
    let key = KEYS
        .iter()
        .map(|k| row[*k].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\0");
    if done.contains(&key) {
        return Ok(());
    }
    if !active.insert(key.clone()) {
        return Err(invalid("Layer inheritance contains a cycle."));
    }
    check_text(row, "Content")?;
    check_text(row, "SysCharacter03")?;
    check_text(row, "SysCharacter01")?;
    let parents = row["SysCharacter01"].as_str().unwrap_or("");
    if !parents.is_empty() {
        let names: Vec<String> = serde_json::from_str(parents)
            .map_err(|_| invalid("The parent layer list is invalid."))?;
        for name in names {
            let candidates: Vec<_> = rows
                .iter()
                .filter(|p| {
                    p["TypeCode"] == "KNTCCustLayer"
                        && p["Key1"] == name
                        && p["Key2"] == row["Key2"]
                        && p["Key3"] == row["Key3"]
                        && p["ProductID"] == row["ProductID"]
                        && context.company_rank(p).is_some()
                        && (p["CGCCode"] == context.country || p["CGCCode"] == "")
                })
                .collect();
            let best = candidates
                .iter()
                .map(|p| {
                    context.company_rank(p).unwrap() * 2
                        + usize::from(p["CGCCode"] == context.country)
                })
                .max();
            let selected: Vec<_> = candidates
                .into_iter()
                .filter(|p| {
                    Some(
                        context.company_rank(p).unwrap() * 2
                            + usize::from(p["CGCCode"] == context.country),
                    ) == best
                })
                .collect();
            if selected.is_empty() {
                // Native ExportLayers still writes the child when no stored
                // parent exists. Preserve that export and report the dependency.
                diagnostics.push(Diagnostic::new("KINETIC_PARENT_NOT_FOUND", format!(
                    "Parent layer {name} was not found in the selected scope. The child layer package was saved with its original parent reference."
                )));
                continue;
            }
            if selected.len() != 1 {
                return Err(invalid(format!(
                    "Parent layer {name} is missing or ambiguous. The original definitions are preserved."
                )));
            }
            layer(entries, selected[0], rows, context, active, done, diagnostics)?;
        }
    }
    let view = path(
        row["Key2"]
            .as_str()
            .ok_or_else(|| invalid("The layer application is missing."))?,
    )?;
    let name = row["Key1"]
        .as_str()
        .ok_or_else(|| invalid("The layer name is missing."))?;
    // Native server sanitization differs by OS. A Windows-safe segment is also
    // accepted by the importer, which reads the identity from the JSON record.
    let name = crate::bpm_code::label(name);
    insert(
        entries,
        format!("Layers/{view}/{name}.jsonc"),
        serde_json::to_vec_pretty(&clean_row(row))
            .map_err(|_| invalid("Cannot serialize the layer."))?,
    )?;
    active.remove(&key);
    done.insert(key);
    Ok(())
}
struct LayerScope<'a> {
    company: &'a str,
    country: &'a str,
    tenant_company: Option<&'a str>,
}
impl LayerScope<'_> {
    fn company_rank(&self, row: &Value) -> Option<usize> {
        let company = row["Company"].as_str()?;
        // Keep the established owner-before-global policy, with only the
        // authenticated tenant as an additional fallback. Native BuildCompany
        // maps blank company/CGC to T_<session tenant>; never infer that tenant
        // from another row. MetaFX export resolves transitive parents against
        // the same visible collection, so retain this package's scope throughout.
        if company == self.company { Some(2) }
        else if self.tenant_company == Some(company) { Some(1) }
        else if company.is_empty() { Some(0) }
        else { None }
    }
}
pub fn build(
    view_id: &str,
    rows: &[Value],
    metadata: &[Value],
    tenant_company: Option<&str>,
) -> (Vec<Value>, Vec<Value>, Vec<Diagnostic>) {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let mut binaries = Vec::new();
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let variants: BTreeSet<_> = rows
        .iter()
        .filter(|r| view(r) == Some(view_id))
        .map(|r| {
            (
                r["Company"].as_str().unwrap_or("").to_owned(),
                r["CGCCode"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    let mut packages = Vec::new();
    for (company, country) in variants {
        let mut devices: BTreeSet<String> = rows
            .iter()
            .filter(|r| {
                view(r) == Some(view_id)
                    && r["Company"] == company
                    && r["CGCCode"] == country
                    && r["TypeCode"] == "KNTCCustLayer"
            })
            .map(|r| r["Key3"].as_str().unwrap_or("").to_owned())
            .collect();
        if devices.is_empty() {
            devices.insert(String::new());
        }
        let separate = devices.len() > 1;
        for device in devices {
            packages.push((company.clone(), country.clone(), device, separate));
        }
    }
    for (company, country, device, separate) in packages {
        let selected: Vec<_> = rows
            .iter()
            .filter(|r| {
                view(r) == Some(view_id)
                    && r["Company"] == company
                    && r["CGCCode"] == country
                    && (r["TypeCode"] != "KNTCCustLayer" || r["Key3"] == device)
            })
            .collect();
        let mut label = crate::bpm_code::label(&format!(
            "{view_id}-{}-{}",
            if company.is_empty() {
                "Global"
            } else {
                &company
            },
            if country.is_empty() { "All" } else { &country }
        ));
        if separate {
            label.push('-');
            label.push_str(&crate::bpm_code::label(if device.is_empty() {
                "Default"
            } else {
                &device
            }));
        }
        let result = (|| {
            let mut entries = BTreeMap::new();
            let mut active = BTreeSet::new();
            let mut done = BTreeSet::new();
            let scope = LayerScope { company: &company, country: &country, tenant_company };
            let record_export = view_id.eq_ignore_ascii_case("shared")
                || metadata.iter().any(|r| {
                    view(r) == Some(view_id)
                        && r["TypeCode"] == "KNTCMetaUI"
                        && r["SystemFlag"] == true
                });
            for row in &selected {
                for field in ["Content", "SysCharacter01", "SysCharacter03"] {
                    check_text(row, field)?;
                }
                if row["TypeCode"] == "KNTCCustLayer" {
                    layer(&mut entries, row, rows, &scope, &mut active, &mut done, &mut errors)?;
                }
                if row["TypeCode"] == "KNTCCustLayer" {
                    let layer_name =
                        crate::bpm_code::label(row["Key1"].as_str().unwrap_or("Layer"));
                    let device = crate::bpm_code::label(row["Key3"].as_str().unwrap_or(""));
                    for (field, stage) in [("Content", "Published"), ("SysCharacter03", "Draft")] {
                        let text = row[field].as_str().unwrap();
                        if !text.is_empty() {
                            files.push(json!({"name":format!("Recovery/{label}/{layer_name}-{device}/{stage}.jsonc"),"text":text}));
                        }
                    }
                }
                if matches!(
                    row["TypeCode"].as_str(),
                    Some("KNTCEventLayout" | "KNTCLayerBackup")
                ) {
                    files.push(json!({"name":format!("Recovery/{label}/{}-{}.json",row["TypeCode"].as_str().unwrap(),row["SysRowID"].as_str().unwrap_or("record")),"text":serde_json::to_string_pretty(&clean_row(row)).unwrap()}));
                }
                if row["TypeCode"] == "KNTCMetaUI" {
                    let full = path(
                        row["Key2"]
                            .as_str()
                            .ok_or_else(|| invalid("An application file path is missing."))?,
                    )?;
                    let relative = full.strip_prefix(&format!("{view_id}/")).ok_or_else(|| {
                        invalid("An application file is outside its application directory.")
                    })?;
                    let text = row["Content"].as_str().unwrap();
                    files.push(json!({"name":format!("Recovery/{label}/{relative}"),"text":text}));
                    if record_export {
                        // ImportApp clears the complete application directory.
                        // Gen3 restores one native record without deleting its neighbours.
                        let xml = crate::classic_archive::build(row)?;
                        files.push(json!({"name":format!("Native records/{label}/{relative}.xml"),"text":xml}));
                    }
                    if !record_export
                        && relative.to_lowercase().ends_with(".jsonc")
                        && !relative.to_lowercase().ends_with(".draft.jsonc")
                    {
                        insert(
                            &mut entries,
                            format!("Apps/{view_id}/{relative}"),
                            text.as_bytes().to_vec(),
                        )?;
                    }
                }
            }
            if !entries.is_empty() {
                let bytes = zip(&entries)?;
                binaries.push(json!({"name":format!("{label}.zip"),"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),"base64":base64::engine::general_purpose::STANDARD.encode(bytes)}));
            }
            Ok::<(), Diagnostic>(())
        })();
        if let Err(error) = result {
            errors.push(error);
        }
    }
    (binaries, files, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::io::Read;
    pub fn row(
        kind: &str,
        key1: &str,
        key2: &str,
        content: &str,
        draft: &str,
        parents: &str,
    ) -> Value {
        let mut row = json!({"Company":"","ProductID":"EP","TypeCode":kind,"CGCCode":"","Key1":key1,"Key2":key2,"Key3":"","Content":content,"SysCharacter03":draft,"SysCharacter01":parents,"SystemFlag":false,"SysCheckBox01":true,"SysRowID":format!("{kind}-{key1}-{key2}")});
        for field in ["Content", "SysCharacter03", "SysCharacter01"] {
            row[format!("Herbert{field}Bytes")] =
                json!(row[field].as_str().unwrap().encode_utf16().count() * 2);
        }
        row
    }
    fn entries(binary: &Value) -> BTreeMap<String, String> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(binary["base64"].as_str().unwrap())
            .unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|i| {
                let mut e = zip.by_index(i).unwrap();
                let mut text = String::new();
                e.read_to_string(&mut text).unwrap();
                (e.name().to_owned(), text)
            })
            .collect()
    }
    fn scoped_layer(name: &str, company: &str, country: &str, parents: &str, marker: &str) -> Value {
        let mut value = row("KNTCCustLayer", name, "App", &json!({"marker":marker}).to_string(), "{\"draft\":true}", parents);
        value["Company"] = json!(company);
        value["CGCCode"] = json!(country);
        value["FutureNativeField"] = json!({"preserve":"exact"});
        value
    }
    fn parent_record(root: &Value, candidates: &[Value], tenant: Option<&str>) -> (BTreeMap<String, Value>, Vec<Diagnostic>) {
        let scope = LayerScope {company:root["Company"].as_str().unwrap(),country:root["CGCCode"].as_str().unwrap(),tenant_company:tenant};
        let mut output = BTreeMap::new();
        let mut errors = Vec::new();
        layer(&mut output, root, candidates, &scope, &mut BTreeSet::new(), &mut BTreeSet::new(), &mut errors).unwrap();
        (output.into_iter().map(|(path, bytes)| (path,serde_json::from_slice(&bytes).unwrap())).collect(),errors)
    }
    #[test]
    fn verified_tenant_parent_is_included_with_original_content_and_draft() {
        let child = scoped_layer("Child","C","","[\"Parent\"]","child");
        let parent = scoped_layer("Parent","T_verified","","","tenant original");
        let rows = vec![child.clone(),parent.clone()];
        let (binaries,_,errors) = build("App", &rows, &rows, Some("T_verified"));
        assert!(errors.is_empty(),"{errors:?}");
        let package = binaries.iter().map(entries).find(|e| e.contains_key("Layers/App/Child.jsonc")).unwrap();
        let actual:Value=serde_json::from_str(&package["Layers/App/Parent.jsonc"]).unwrap();
        assert_eq!(actual,clean_row(&parent));
        let (without,errors)=parent_record(&child,&rows,None);
        assert!(!without.contains_key("Layers/App/Parent.jsonc"));
        assert_eq!(errors[0].code,"KINETIC_PARENT_NOT_FOUND");
    }
    #[test]
    fn parent_selection_preserves_company_tenant_global_and_country_precedence() {
        let child=scoped_layer("Child","C","US","[\"Parent\"]","child");
        let company=scoped_layer("Parent","C","","","company");
        let tenant_exact=scoped_layer("Parent","T_verified","US","","tenant country");
        let tenant_generic=scoped_layer("Parent","T_verified","","","tenant generic");
        let global=scoped_layer("Parent","","US","","global");
        for (candidates,expected) in [
            (vec![global.clone(),tenant_exact.clone(),company.clone()],company),
            (vec![global.clone(),tenant_generic.clone(),tenant_exact.clone()],tenant_exact),
            (vec![global.clone(),tenant_generic.clone()],tenant_generic),
            (vec![global.clone()],global),
        ] {
            let (output,errors)=parent_record(&child,&candidates,Some("T_verified"));
            assert!(errors.is_empty());
            assert_eq!(output["Layers/App/Parent.jsonc"],clean_row(&expected));
        }
    }
    #[test]
    fn parent_selection_excludes_other_tenant_product_app_device_and_country() {
        let child=scoped_layer("Child","C","US","[\"Parent\"]","child");
        let mut candidates=vec![scoped_layer("Parent","T_unrelated","US","","foreign tenant")];
        for (key,value) in [("ProductID","OTHER"),("Key2","OtherApp"),("Key3","Phone"),("CGCCode","GB")] {
            let mut wrong=scoped_layer("Parent","T_verified","US","","wrong native key");
            wrong[key]=json!(value);candidates.push(wrong);
        }
        let (output,errors)=parent_record(&child,&candidates,Some("T_verified"));
        assert_eq!(output.len(),1);
        assert_eq!(output["Layers/App/Child.jsonc"],clean_row(&child));
        assert_eq!(errors.len(),1);assert_eq!(errors[0].code,"KINETIC_PARENT_NOT_FOUND");
    }
    #[test]
    fn transitive_tenant_parent_retains_root_company_and_country_context() {
        let child=scoped_layer("Child","C","US","[\"Parent\"]","child");
        let parent=scoped_layer("Parent","T_verified","","[\"Grand\"]","parent");
        let company_grand=scoped_layer("Grand","C","US","[\"Base\"]","company override");
        let tenant_grand=scoped_layer("Grand","T_verified","","","tenant fallback");
        let base=scoped_layer("Base","","US","","global country base");
        let candidates=vec![parent.clone(),tenant_grand,company_grand.clone(),base.clone()];
        let (output,errors)=parent_record(&child,&candidates,Some("T_verified"));
        assert!(errors.is_empty(),"{errors:?}");assert_eq!(output.len(),4);
        for value in [child,parent,company_grand,base] {
            assert_eq!(output[&format!("Layers/App/{}.jsonc",value["Key1"].as_str().unwrap())],clean_row(&value));
        }
    }
    #[test]
    fn tenant_parent_ambiguity_and_cross_scope_cycles_remain_errors() {
        let child=scoped_layer("Child","C","","[\"Parent\"]","child");
        let scope=LayerScope{company:"C",country:"",tenant_company:Some("T_verified")};
        let parent=scoped_layer("Parent","T_verified","","","one");
        let duplicate=scoped_layer("Parent","T_verified","","","different original");
        let cycle=scoped_layer("Parent","T_verified","","[\"Child\"]","cycle");
        for (candidates,message) in [(vec![parent,duplicate],"ambiguous"),(vec![child.clone(),cycle],"cycle")] {
            let result=layer(&mut BTreeMap::new(),&child,&candidates,&scope,
                &mut BTreeSet::new(),&mut BTreeSet::new(),&mut Vec::new());
            assert!(result.unwrap_err().message.contains(message));
        }
    }
    #[test]
    fn native_package_keeps_layer_draft_and_parent_but_app_draft_stays_recovery() {
        let rows = vec![
            row(
                "KNTCCustLayer",
                "Child",
                "App",
                "{\"published\":1}",
                "{\"draft\":2}",
                "[\"Parent\"]",
            ),
            row("KNTCCustLayer", "Parent", "App", "{}", "", ""),
            row(
                "KNTCMetaUI",
                "App",
                "App/layout.jsonc",
                "{\"layout\":{}}",
                "",
                "",
            ),
            row(
                "KNTCMetaUI",
                "App",
                "App/layout.draft.jsonc",
                "{\"layout\":{\"draft\":true}}",
                "",
                "",
            ),
        ];
        let (binary, files, errors) = build("App", &rows, &rows, None);
        assert!(errors.is_empty(), "{errors:?}");
        let entries = entries(&binary[0]);
        assert_eq!(entries.len(), 3);
        assert!(entries.contains_key("Layers/App/Parent.jsonc"));
        let child: Value = serde_json::from_str(&entries["Layers/App/Child.jsonc"]).unwrap();
        assert_eq!(child["Content"], rows[0]["Content"]);
        assert_eq!(child["SysCharacter03"], rows[0]["SysCharacter03"]);
        assert!(child.get("HerbertContentBytes").is_none());
        assert!(
            files
                .iter()
                .any(|f| f["name"].as_str().unwrap().ends_with("layout.draft.jsonc"))
        );
        assert!(!entries.keys().any(|k| k.ends_with(".draft.jsonc")));
    }
    #[test]
    fn missing_parent_preserves_native_child_package_and_reports_dependency() {
        let rows = vec![row(
            "KNTCCustLayer",
            "Child",
            "App",
            "{}",
            "",
            "[\"Absent\"]",
        )];
        let (binary, _, errors) = build("App", &rows, &rows, None);
        assert_eq!(binary.len(), 1);
        let files = entries(&binary[0]);
        let child: Value = serde_json::from_str(&files["Layers/App/Child.jsonc"]).unwrap();
        assert_eq!(child["SysCharacter01"], "[\"Absent\"]");
        assert!(!errors.is_empty());
    }
    #[test]
    fn local_app_package_does_not_stop_at_native_first_hundred() {
        let rows: Vec<_> = (0..137)
            .map(|i| {
                row(
                    "KNTCMetaUI",
                    "App",
                    &format!("App/file{i}.jsonc"),
                    "{}",
                    "",
                    "",
                )
            })
            .collect();
        let (binary, _, errors) = build("App", &rows, &rows, None);
        assert!(errors.is_empty());
        assert_eq!(entries(&binary[0]).len(), 137);
    }
    #[test]
    fn mixed_resource_group_uses_native_records_without_replacing_the_group() {
        let custom = row(
            "KNTCMetaUI",
            "shared",
            "shared/search/Custom/strings.jsonc",
            "{\"text\":\"Keep & restore\\r\\n\"}",
            "",
            "",
        );
        let mut system = row(
            "KNTCMetaUI",
            "shared",
            "shared/search/System/strings.jsonc",
            "{}",
            "",
            "",
        );
        system["SystemFlag"] = json!(true);
        let (binary, files, errors) = build("shared", &[custom.clone()], &[custom.clone(), system], None);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            binary.is_empty(),
            "A partial Apps/shared ZIP would replace unrelated resources."
        );
        let native = files
            .iter()
            .find(|f| f["name"].as_str().unwrap().starts_with("Native records/"))
            .unwrap();
        let xml = native["text"].as_str().unwrap();
        assert!(xml.contains("msprop:ExportFormat=\"Gen3\""));
        assert!(xml.contains("<TypeCode>KNTCMetaUI</TypeCode>"));
        assert!(xml.contains("<Key2>shared/search/Custom/strings.jsonc</Key2>"));
        assert!(!xml.contains("search/System"));
        assert_eq!(
            files
                .iter()
                .filter(|f| f["name"].as_str().unwrap().starts_with("Recovery/"))
                .count(),
            1
        );
    }
    #[test]
    fn cycle_truncation_and_path_escape_never_make_successful_packages() {
        for rows in [
            vec![
                row("KNTCCustLayer", "A", "App", "{}", "", "[\"B\"]"),
                row("KNTCCustLayer", "B", "App", "{}", "", "[\"A\"]"),
            ],
            vec![row("KNTCMetaUI", "App", "App/../bad.jsonc", "{}", "", "")],
            {
                let mut r = row("KNTCMetaUI", "App", "App/layout.jsonc", "{}", "", "");
                r["HerbertContentBytes"] = json!(100);
                vec![r]
            },
        ] {
            let (binary, _, errors) = build("App", &rows, &rows, None);
            assert!(binary.is_empty());
            assert!(!errors.is_empty());
        }
    }
}
