//! Build native personalization files from the received persisted rows.
//! Keep owner-specific home records separate from caller-resolved exports.
use crate::xml_document::{Content, Element, parse};
use base64::{Engine, engine::general_purpose::STANDARD};
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write};

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("PERSONALIZATION_EXPORT_INVALID", message)
}
fn xml(root: &Element) -> Result<String, Diagnostic> {
    let mut writer = quick_xml::Writer::new(Vec::new());
    root.write(&mut writer)?;
    String::from_utf8(writer.into_inner()).map_err(|_| invalid("The native XML is not UTF-8."))
}
fn cell(row: &mut Element, name: &str, value: String) {
    if let Some(existing) = row.child_mut(name) {
        existing.content = vec![Content::Text(value)];
    } else {
        // Keep namespace aliases when the source uses a prefixed DataSet.
        let qualified = row
            .name
            .rsplit_once(':')
            .map(|(prefix, _)| format!("{prefix}:{name}"));
        let mut field = Element::new(qualified.as_deref().unwrap_or(name));
        field.content.push(Content::Text(value));
        row.push(field);
    }
}
fn text(element: &Element) -> String {
    element
        .content
        .iter()
        .filter_map(|c| match c {
            Content::Text(s) => Some(s.as_str()),
            _ => None,
        })
        .collect()
}
fn dataset(table: &str, row: &Value, root: &str, namespace: &str) -> Result<String, Diagnostic> {
    let mut data = Element::new(root);
    data.set("xmlns", namespace);
    let mut record = Element::new(table);
    for (name, value) in row
        .as_object()
        .ok_or_else(|| invalid("The persisted row is invalid."))?
    {
        if name.starts_with("Herbert") || value.is_null() {
            continue;
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(invalid("A native field has an invalid XML name."));
        }
        let value = match value {
            Value::String(s) => s.clone(),
            Value::Number(_) | Value::Bool(_) => value.to_string(),
            _ => return Err(invalid("A native field is not a scalar value.")),
        };
        cell(&mut record, name, value);
    }
    data.push(record);
    xml(&data)
}
fn checked_text<'a>(row: &'a Value, field: &str, length: &str) -> Result<&'a str, Diagnostic> {
    let value = row[field]
        .as_str()
        .ok_or_else(|| invalid("The native text is missing."))?;
    let expected = row[length]
        .as_u64()
        .or_else(|| row[length].as_str().and_then(|s| s.parse().ok()));
    if expected != Some((value.encode_utf16().count() * 2) as u64) {
        return Err(invalid(
            "The native text failed its length check. The source is retained.",
        ));
    }
    Ok(value)
}
fn with_images(source: &str, images: &[Value]) -> Result<String, Diagnostic> {
    if source.trim().is_empty() {
        return Ok(source.into());
    }
    // Validate namespace-qualified image references before editing the tree.
    let referenced = crate::shell_layout::image_ids(source)?;
    let mut root = parse(source)?;
    for item in &mut root.content {
        let Content::Element(row) = item else {
            continue;
        };
        let fields = match row.name.rsplit(':').next() {
            Some("HomeTile") => ("ImageRowID", "ImageBlob", "ImageFilename"),
            Some("UserOptions") => ("AppLogoRowID", "AppLogoBlob", "AppLogoFilename"),
            _ => continue,
        };
        let Some(id) = row
            .children()
            .find(|e| e.name.rsplit(':').next() == Some(fields.0))
            .map(text)
        else {
            continue;
        };
        if !referenced.contains(&id.to_ascii_lowercase()) {
            continue;
        }
        let image = images
            .iter()
            .find(|i| {
                i["metadata"]["SysRowID"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&id))
            })
            .ok_or_else(|| {
                invalid("A referenced home image is missing. The source XML is retained.")
            })?;
        let bytes = &image["bytes"]["returnObj"];
        let bytes = if let Some(encoded) = bytes.as_str() {
            STANDARD
                .decode(encoded)
                .map_err(|_| invalid("A home image is not valid base64."))?
        } else if let Some(values) = bytes.as_array() {
            values
                .iter()
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u8::try_from(n).ok())
                        .ok_or_else(|| invalid("A home image byte is invalid."))
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            return Err(invalid("A referenced home image could not be read."));
        };
        if !crate::file_store_resources::validate_bytes(&image["metadata"], &image["bytes"])
            .is_empty()
        {
            return Err(invalid("A home image failed its native metadata check."));
        }
        let mut compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        compressed
            .write_all(&bytes)
            .map_err(|_| invalid("Cannot compress the home image."))?;
        let compressed = compressed
            .finish()
            .map_err(|_| invalid("Cannot finish the home image."))?;
        cell(row, fields.1, STANDARD.encode(compressed));
        cell(
            row,
            fields.2,
            image["metadata"]["FileName"]
                .as_str()
                .ok_or_else(|| invalid("The image filename is missing."))?
                .into(),
        );
    }
    xml(&root)
}

fn recovery_root(contract: &Value) -> Result<&str, Diagnostic> {
    if let Some(diagnostic) = contract.get("diagnostic") {
        return Err(serde_json::from_value(diagnostic.clone())
            .unwrap_or_else(|_| invalid("The saved recovery-format diagnostic is malformed.")));
    }
    let parts: Option<Vec<u64>> = contract["version"].as_str().and_then(|version| {
        version.split('.').map(|part| part.parse::<u64>().ok()).collect()
    });
    let Some(parts) = parts.filter(|parts| parts.len() == 4) else {
        return Err(invalid("The frozen native personalization recovery format is missing or malformed."));
    };
    let expected = if parts[0] == 4 && parts[1] == 3 {"Layers"} else {"Personalizations"};
    if contract["zipRoot"] != expected {
        return Err(invalid("The saved ZIP root does not match its native recovery version."));
    }
    Ok(expected)
}

pub fn append(definition: &mut Value, contract: &Value) -> Result<(), Diagnostic> {
    let mut files = Vec::new();
    let mut binaries = Vec::new();
    let mut recovery_error = None;
    if let Some(row) = definition["XXXDef"].get(0) {
        let kind = row["TypeCode"].as_str().unwrap_or("");
        if !matches!(kind, "KNTCPersLayer" | "KineticGridLayout") {
            return Err(invalid("The record is not a Kinetic personalization."));
        }
        for key in crate::kinetic_archive::KEYS {
            if !row[key].is_string() {
                return Err(invalid("The personalization identity is incomplete."));
            }
        }
        let content = checked_text(row, "Content", "HerbertContentBytes")?;
        let app = row["Key2"].as_str().unwrap();
        let name = row["SysCharacter03"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(row["Key1"].as_str().unwrap());
        // Each ZIP contains one exact identity. It cannot lose a device variant
        // through MetaFX's duplicate display-name suppression.
        files.push(json!({"name":"Content.jsonc","text":content}));
        match recovery_root(contract) {
            Ok(root) => {
                let entry = format!("{root}/{app}/{}-{kind}.jsonc", crate::bpm_code::label(name));
                let entity = crate::kinetic_archive::clean_row(row);
                let bytes = crate::kinetic_archive::zip(&BTreeMap::from([(
                    entry, serde_json::to_vec_pretty(&entity).unwrap(),
                )]))?;
                binaries.push(json!({"name":format!("{}-{kind}.zip",crate::bpm_code::label(name)),
                    "bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),
                    "base64":STANDARD.encode(bytes)}));
            }
            Err(error) => recovery_error = Some(error),
        }
    }
    let images = definition["homeResources"]["images"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for table in ["ShellLayout", "ShellLayoutPersonal", "HomeUserOption"] {
        let Some(source) = definition[table].get(0) else {
            continue;
        };
        let mut row = source.clone();
        for (field, length, filename) in [
            ("ShellHomePage", "HerbertHomeBytes", "HomePage.xml"),
            ("ShellUserOptions", "HerbertOptionsBytes", "UserOptions.xml"),
            ("FavoriteItems", "HerbertFavoritesBytes", "Favorites.xml"),
        ] {
            if !row[field].is_string() {
                continue;
            }
            let source = checked_text(&row, field, length)?;
            if source.trim().is_empty() {
                continue;
            }
            let value = with_images(source, &images)?;
            files.push(json!({"name":filename,"text":value}));
            row[field] = json!(value);
        }
        if table == "ShellLayout" {
            // Version 1.0 tells the native importer the inner DataSets are XML,
            // not the legacy base64+BZip2 representation.
            row["Version"] = json!("1.0");
            let value = dataset(
                "ShellLayout",
                &row,
                "ShellLayoutDataSet",
                "http://www.epicor.com/Ice/300/BO/ShellLayout/ShellLayout",
            )?;
            files.push(json!({"name":"ShellLayout.xml","text":value}));
        }
    }
    for table in ["FavFolder", "FavItems"] {
        if let Some(row) = definition[table].get(0) {
            files.push(json!({"name":format!("{table}.xml"),"text":dataset(table,row,"FavoriteDataSet","http://www.epicor.com/Ice/300/BO/Favorite/Favorite")?}));
        }
    }
    if !definition["herbertSources"].is_object() {
        definition["herbertSources"] = json!({});
    }
    definition["herbertSources"]["files"] = json!(files);
    definition["herbertSources"]["binaryFiles"] = json!(binaries);
    if let Some(error) = recovery_error {return Err(error);}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn package_preserves_native_owner_device_country_and_display_name() {
        let row = json!({"Company":"C","ProductID":"EP","TypeCode":"KNTCPersLayer","CGCCode":"CA",
            "Key1":"personal-id","Key2":"Erp.UI.PartEntry","Key3":"Tablet","SysCharacter02":"alice",
            "SysCharacter03":"Friendly name","Content":"{}","HerbertContentBytes":4,"SysRevID":42});
        let mut definition = json!({"XXXDef":[row]});
        append(&mut definition, &json!({"version":"5.2.100.9","zipRoot":"Personalizations"})).unwrap();
        let bytes = STANDARD
            .decode(
                definition["herbertSources"]["binaryFiles"][0]["base64"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut text = String::new();
        archive
            .by_name("Personalizations/Erp.UI.PartEntry/Friendly name-KNTCPersLayer.jsonc")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        let actual: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(actual["SysCharacter02"], "alice");
        assert_eq!(actual["Key3"], "Tablet");
        assert_eq!(actual["CGCCode"], "CA");
        assert_eq!(actual["SysCharacter03"], "Friendly name");
        assert!(actual.get("HerbertContentBytes").is_none());
        assert_eq!(actual["SysRevID"], STANDARD.encode(42_u64.to_be_bytes()));
    }
    #[test]
    fn home_image_gzip_and_unicode_round_trip_without_changing_identity() {
        let id = "a83369f8-766c-4fc1-abde-080d5093b4de";
        let source = format!(
            "<HomePageDataSet xmlns='urn:home'><HomeTile><ImageRowID>{id}</ImageRowID><Title>Ω &amp; A&#13;B</Title></HomeTile></HomePageDataSet>"
        );
        let images = vec![
            json!({"metadata":{"SysRowID":id,"FileName":"image.png","HerbertContentBytes":3},
            "bytes":{"returnObj":"YWJj","parameters":{"fileName":"image.png"}}}),
        ];
        let output = with_images(&source, &images).unwrap();
        let parsed = parse(&output).unwrap();
        let row = parsed.children().next().unwrap();
        assert_eq!(
            text(row.children().find(|e| e.name == "ImageRowID").unwrap()),
            id
        );
        assert_eq!(
            text(row.children().find(|e| e.name == "Title").unwrap()),
            "Ω & A\rB"
        );
        let bytes = STANDARD
            .decode(text(
                row.children().find(|e| e.name == "ImageBlob").unwrap(),
            ))
            .unwrap();
        let mut original = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut original)
            .unwrap();
        assert_eq!(original, b"abc");
        assert!(with_images(&source, &[]).is_err());
    }
    #[test]
    fn truncation_cannot_produce_a_native_package() {
        let mut definition = json!({"XXXDef":[{"Company":"","ProductID":"EP","TypeCode":"KineticGridLayout",
            "CGCCode":"","Key1":"owner","Key2":"App","Key3":"grid","Content":"{}","HerbertContentBytes":8}]});
        assert!(append(&mut definition, &json!({"version":"5.2.100.9","zipRoot":"Personalizations"})).is_err());
        assert!(definition["herbertSources"]["binaryFiles"].is_null());
    }

    fn contract(version: &str, root: &str) -> Value {json!({"version":version,"zipRoot":root})}
    fn native_row() -> Value {json!({"Company":"C","ProductID":"EP","TypeCode":"KNTCPersLayer","CGCCode":"CA",
        "Key1":"source-id","Key2":"Owned.App","Key3":"Tablet","SysCharacter02":"owner Ω","SysCharacter03":"Label",
        "SysCharacter04":"full-source auxiliary field","Content":"{}","HerbertContentBytes":4,"SysRevID":42})}
    fn zip_entry(definition: &Value) -> (String, Vec<u8>) {
        let bytes=STANDARD.decode(definition["herbertSources"]["binaryFiles"][0]["base64"].as_str().unwrap()).unwrap();
        let mut archive=zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();assert_eq!(archive.len(),1);
        let mut file=archive.by_index(0).unwrap();let name=file.name().to_owned();let mut body=Vec::new();file.read_to_end(&mut body).unwrap();(name,body)
    }
    #[test]
    fn native_43_wrapper_preserves_the_complete_modern_entity_and_device() {
        let row=native_row();let mut legacy=json!({"XXXDef":[row.clone()]});let mut modern=legacy.clone();
        append(&mut legacy,&contract("4.3.200.39","Layers")).unwrap();
        append(&mut modern,&contract("5.2.100.9","Personalizations")).unwrap();
        let mut newer= json!({"XXXDef":[row.clone()]});append(&mut newer,&contract("5.1.100.0","Personalizations")).unwrap();
        assert_eq!(newer["herbertSources"]["binaryFiles"],modern["herbertSources"]["binaryFiles"]);
        let (legacy_name,legacy_body)=zip_entry(&legacy);let (modern_name,modern_body)=zip_entry(&modern);
        assert_eq!(legacy_name,"Layers/Owned.App/Label-KNTCPersLayer.jsonc");
        assert_eq!(modern_name,"Personalizations/Owned.App/Label-KNTCPersLayer.jsonc");assert_eq!(legacy_body,modern_body);
        let entity:Value=serde_json::from_slice(&legacy_body).unwrap();
        assert_eq!(entity["Key3"],"Tablet");assert_eq!(entity["SysCharacter02"],"owner Ω");assert_eq!(entity["CGCCode"],"CA");
        assert_eq!(entity["SysCharacter04"],row["SysCharacter04"]);assert_eq!(legacy["XXXDef"][0],row);
        assert_eq!(legacy["herbertSources"]["files"],modern["herbertSources"]["files"]);
    }
    #[test]
    fn missing_or_failed_contract_retains_content_and_original_native_cause() {
        let mut source=json!({"XXXDef":[native_row()]});let row=source["XXXDef"][0].clone();
        let cause=Diagnostic::new("VERSION_DENIED","Original native permission cause");
        let error=append(&mut source,&json!({"diagnostic":cause})).unwrap_err();assert_eq!(error.code,"VERSION_DENIED");
        assert_eq!(error.message,"Original native permission cause");assert_eq!(source["XXXDef"][0],row);
        assert_eq!(source["herbertSources"]["files"],json!([{"name":"Content.jsonc","text":"{}"}]));
        assert_eq!(source["herbertSources"]["binaryFiles"],json!([]));
        for invalid in [Value::Null,json!({}),contract("4.3.200.39","Personalizations"),contract("not-a-version","Layers")] {
            let mut source=json!({"XXXDef":[native_row()]});assert!(append(&mut source,&invalid).is_err());
            assert_eq!(source["herbertSources"]["files"][0]["text"],"{}");assert_eq!(source["herbertSources"]["binaryFiles"],json!([]));
        }
    }
    #[test]
    fn zip_only_contract_failure_does_not_block_native_favorite_output() {
        let mut definition=json!({"FavFolder":[{"UserID":"owner","FolderSeq":2,"Description":"Owned Ω"}]});
        append(&mut definition,&json!({"diagnostic":Diagnostic::new("VERSION_DENIED","original")})).unwrap();
        assert_eq!(definition["herbertSources"]["files"][0]["name"],"FavFolder.xml");
    }
}
