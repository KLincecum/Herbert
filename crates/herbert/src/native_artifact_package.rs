//! Native export preparation. Only structured ownership metadata is changed.
//! Custom code, expressions and unrelated ZIP members are never rewritten.
use herbert_contract::{Diagnostic, NativeArtifactDeclaration, NativeArtifactType};
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read, Write},
};

pub(crate) struct PreparedArtifact {
    pub bytes: Vec<u8>,
    pub source_id: String,
    pub metadata: Value,
}
fn invalid(message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("NATIVE_ARTIFACT_PACKAGE_INVALID", message)
}
fn child<'a>(node: &'a Value, name: &str) -> Result<&'a Value, Diagnostic> {
    let children = node["content"]
        .as_array()
        .ok_or_else(|| invalid("Native XML has no content."))?;
    let mut matches = children
        .iter()
        .filter_map(|v| v.get("element"))
        .filter(|v| v["name"].as_str() == Some(name));
    let result = matches
        .next()
        .ok_or_else(|| invalid(format!("Native XML omits {name}.")))?;
    if matches.next().is_some() {
        return Err(invalid(format!("Native XML repeats {name}.")));
    }
    Ok(result)
}
fn text(node: &Value) -> Result<String, Diagnostic> {
    let content = node["content"]
        .as_array()
        .ok_or_else(|| invalid("Native XML field has no content."))?;
    let mut value = String::new();
    for part in content {
        if let Some(part) = part["text"].as_str() {
            value.push_str(part);
        } else {
            return Err(invalid("Native metadata contains nested content."));
        }
    }
    Ok(value)
}
fn xml_text(bytes: &[u8]) -> Result<String, Diagnostic> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if bytes.len() % 2 != 0 {
            return Err(invalid("Native UTF-16 has an odd byte length."));
        }
        let little = bytes[0] == 0xff;
        let units = bytes[2..]
            .chunks_exact(2)
            .map(|v| {
                if little {
                    u16::from_le_bytes([v[0], v[1]])
                } else {
                    u16::from_be_bytes([v[0], v[1]])
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16(&units).map_err(|_| invalid("Native UTF-16 is malformed."))
    } else {
        String::from_utf8(
            bytes
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(bytes)
                .to_vec(),
        )
        .map_err(|_| invalid("Native XML must be UTF-8 or BOM-marked UTF-16."))
    }
}

/// Change one direct DataSet row field by XML token position. This preserves all
/// other text, including escaped custom code and schema namespace declarations.
fn replace_root_field(
    xml: &str,
    row: &str,
    field: &str,
    value: &str,
) -> Result<String, Diagnostic> {
    let mut reader = Reader::from_str(xml);
    let mut stack = Vec::<Vec<u8>>::new();
    let mut found = None;
    let mut start = None;
    let mut insert = None;
    let mut qualified = field.to_string();
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader
            .read_event()
            .map_err(|_| invalid("Native XML is malformed."))?;
        let direct = stack.len() == 2 && stack[1].as_slice() == row.as_bytes();
        match event {
            Event::Start(e) => {
                if stack.len() == 1 && e.local_name().as_ref() == row.as_bytes() {
                    let name = String::from_utf8(e.name().as_ref().to_vec())
                        .map_err(|_| invalid("Native XML name is malformed."))?;
                    if let Some((prefix, _)) = name.split_once(':') {
                        qualified = format!("{prefix}:{field}");
                    }
                }
                if direct && e.local_name().as_ref() == field.as_bytes() {
                    if found.is_some() || start.is_some() {
                        return Err(invalid("Native ownership field is repeated."));
                    }
                    qualified = String::from_utf8(e.name().as_ref().to_vec())
                        .map_err(|_| invalid("Native XML name is malformed."))?;
                    start = Some(before);
                }
                stack.push(e.local_name().as_ref().to_vec());
            }
            Event::Empty(e) if direct && e.local_name().as_ref() == field.as_bytes() => {
                qualified = String::from_utf8(e.name().as_ref().to_vec())
                    .map_err(|_| invalid("Native XML name is malformed."))?;
                if found
                    .replace((before, reader.buffer_position() as usize))
                    .is_some()
                {
                    return Err(invalid("Native ownership field is repeated."));
                }
            }
            Event::End(e) => {
                if stack.len() == 3
                    && stack[1].as_slice() == row.as_bytes()
                    && e.local_name().as_ref() == field.as_bytes()
                {
                    found = Some((
                        start
                            .take()
                            .ok_or_else(|| invalid("Native field token mismatch."))?,
                        reader.buffer_position() as usize,
                    ));
                }
                if stack.len() == 2 && e.local_name().as_ref() == row.as_bytes() {
                    insert = Some(before);
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let (begin, end) = found
        .or_else(|| insert.map(|p| (p, p)))
        .ok_or_else(|| invalid("Native artifact root row is absent."))?;
    let replacement = format!(
        "<{qualified}>{}</{qualified}>",
        quick_xml::escape::escape(value)
    );
    let mut result = xml.to_owned();
    result.replace_range(begin..end, &replacement);
    Ok(result)
}

pub(crate) fn prepare(
    declaration: &NativeArtifactDeclaration,
    source: &[u8],
    target: &str,
    owner: uuid::Uuid,
    company: &str,
    caller: &str,
) -> Result<PreparedArtifact, Diagnostic> {
    let marker = format!("Herbert artifact {owner}");
    if declaration.artifact_type == NativeArtifactType::FunctionLibrary {
        let mut package: Value =
            serde_json::from_slice(source.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(source))
                .map_err(|_| invalid("The Function export is not an EFXJ JSON package."))?;
        let source_id = package["LibraryId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("The Function export omits LibraryId."))?
            .to_owned();
        let library = package
            .get_mut("Library")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| invalid("The Function export omits its Library object."))?;
        if !library.get("Functions").is_some_and(Value::is_array) {
            return Err(invalid(
                "The Function export omits its function definitions.",
            ));
        }
        let notes = library.get("Notes").and_then(Value::as_str).unwrap_or("");
        library.insert("Notes".into(), json!(format!("{notes}\n{marker}")));
        library.insert("GlobalID".into(), json!(owner));
        library.insert("Owner".into(), json!(caller));
        library.insert("OwnedByCompany".into(), json!(company));
        library.insert(
            "LibraryMappings".into(),
            json!([{"Company":company,"Allowed":true}]),
        );
        library.insert("Published".into(), json!(false));
        package["LibraryId"] = json!(target);
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(
            serde_json::to_vec(&package)
                .map_err(|_| invalid("Cannot serialize the native Function export."))?,
        );
        return Ok(PreparedArtifact {
            bytes,
            source_id,
            metadata: package,
        });
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(source))
        .map_err(|_| invalid("The BAQ export is not a native ZIP package."))?;
    let mut names = BTreeSet::new();
    for index in 0..archive.len() {
        let member = archive
            .by_index(index)
            .map_err(|_| invalid("Cannot read native package directory."))?;
        if !names.insert(member.name().to_string()) {
            return Err(invalid("Native package repeats a member name."));
        }
    }
    let mut xml_bytes = Vec::new();
    archive
        .by_name("DynamicQuery")
        .map_err(|_| invalid("The BAQ export omits DynamicQuery."))?
        .read_to_end(&mut xml_bytes)
        .map_err(|_| invalid("The native query member is damaged."))?;
    let parsed = crate::xml_archive::parse_xml(&xml_bytes)?;
    if parsed["name"] != "BAQDesignerDataSet"
        || parsed["namespace"] != "http://www.epicor.com/Ice/300/BO/BAQDesigner/BAQDesigner"
    {
        return Err(invalid(
            "The BAQ export has a different native DataSet identity.",
        ));
    }
    let row = child(&parsed, "DynamicQueryDesigner")?;
    let source_id = text(child(row, "QueryID")?)?;
    let updatable = text(child(row, "Updatable")?)?;
    if updatable
        != if declaration.artifact_type == NativeArtifactType::Ubaq {
            "true"
        } else {
            "false"
        }
    {
        return Err(invalid(
            "The declared BAQ/UBAQ type differs from the native export.",
        ));
    }
    let comment = text(child(row, "Comment")?)?;
    let mut xml = xml_text(&xml_bytes)?;
    xml = replace_root_field(
        &xml,
        "DynamicQueryDesigner",
        "Comment",
        &format!("{comment}\n{marker}"),
    )?;
    // New operation instances belong to the admitted company. Native import
    // remaps child row identities. The host never creates an All Companies fixture.

    xml = replace_root_field(&xml, "DynamicQueryDesigner", "AllCompanies", "false")?;
    xml = replace_root_field(&xml, "DynamicQueryDesigner", "SystemFlag", "false")?;
    // Native import receives the desired query name as a separate argument. It
    // owns identity remapping. Custom code is untouched.
    let utf16 = xml_bytes.starts_with(&[0xff, 0xfe]) || xml_bytes.starts_with(&[0xfe, 0xff]);
    let rewritten = if utf16 {
        let little = xml_bytes[0] == 0xff;
        let mut b = xml_bytes[..2].to_vec();
        for c in xml.encode_utf16() {
            b.extend(if little {
                c.to_le_bytes()
            } else {
                c.to_be_bytes()
            });
        }
        b
    } else {
        xml.into_bytes()
    };
    let mut output = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..archive.len() {
        let member = archive
            .by_index(index)
            .map_err(|_| invalid("Cannot read native package member."))?;
        if member.name() == "DynamicQuery" {
            output
                .start_file(
                    "DynamicQuery",
                    zip::write::SimpleFileOptions::default()
                        .compression_method(member.compression()),
                )
                .map_err(|_| invalid("Cannot write native query member."))?;
            output
                .write_all(&rewritten)
                .map_err(|_| invalid("Cannot preserve native query bytes."))?;
        } else {
            output
                .raw_copy_file(member)
                .map_err(|_| invalid("Cannot preserve a native package member."))?;
        }
    }
    let bytes = output
        .finish()
        .map_err(|_| invalid("Cannot complete native package."))?
        .into_inner();
    Ok(PreparedArtifact {
        bytes,
        source_id,
        metadata: parsed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_edit_does_not_replace_query_names_in_code_or_schema() {
        let source = "<Root xmlns='native'><Schema><Comment>schema</Comment></Schema><DynamicQueryDesigner><QueryID>A</QueryID><Comment/><Code>Call(\"A\"); &amp; x</Code></DynamicQueryDesigner></Root>";
        let changed =
            replace_root_field(source, "DynamicQueryDesigner", "Comment", "owned & safe").unwrap();
        assert_eq!(
            changed,
            source.replace("<Comment/>", "<Comment>owned &amp; safe</Comment>")
        );
        assert!(
            replace_root_field(
                "<Root><DynamicQueryDesigner><Comment/><Comment/></DynamicQueryDesigner></Root>",
                "DynamicQueryDesigner",
                "Comment",
                "x"
            )
            .is_err()
        );
    }
}

/// Compare authored UBAQ settings after native import. Native identity and
/// calculated dictionary columns can change. Update expressions and actions cannot.
pub(crate) fn verify_update_configuration(
    source: &Value,
    native: &Value,
) -> Result<(), Diagnostic> {
    for table in [
        "QueryUpdateSettingsDesigner",
        "QueryUpdateFieldDesigner",
        "QueryCustomActionDesigner",
    ] {
        let expected = source["content"]
            .as_array()
            .ok_or_else(|| invalid("Native query content is missing."))?
            .iter()
            .filter_map(|v| v.get("element"))
            .filter(|v| v["name"].as_str() == Some(table))
            .collect::<Vec<_>>();
        let observed = native
            .get("returnObj")
            .unwrap_or(native)
            .get(table)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Diagnostic::new(
                    "ARTIFACT_UBAQ_CONFIGURATION_UNVERIFIED",
                    format!("Native readback omitted {table}."),
                )
            })?;
        let mut remaining = observed.iter().collect::<Vec<_>>();
        for row in expected {
            let mut fields = Vec::new();
            for entry in row["content"]
                .as_array()
                .ok_or_else(|| invalid("Native query row is malformed."))?
            {
                let Some(field) = entry.get("element") else {
                    continue;
                };
                let name = field["name"]
                    .as_str()
                    .ok_or_else(|| invalid("Native field name is missing."))?;
                if matches!(
                    name,
                    "Company"
                        | "QueryID"
                        | "SysRowID"
                        | "SysRevID"
                        | "SystemFlag"
                        | "RowMod"
                        | "BitFlag"
                        | "NetDataType"
                        | "Description"
                        | "QualifiedName"
                        | "Required"
                        | "DataType"
                        | "NullableType"
                ) {
                    continue;
                }
                fields.push((name, text(field)?));
            }
            let matched = remaining.iter().position(|row| {
                fields.iter().all(|(name, expected)| {
                    row.get(*name).is_some_and(|v| {
                        let actual = if v.is_null() {
                            String::new()
                        } else {
                            v.as_str()
                                .map(String::from)
                                .unwrap_or_else(|| v.to_string())
                        };
                        actual == *expected
                    })
                })
            });
            let Some(index) = matched else {
                return Err(Diagnostic::new(
                    "ARTIFACT_UBAQ_CONFIGURATION_CHANGED",
                    format!(
                        "Native import did not preserve a supplied {table} row. The UBAQ will not be executed."
                    ),
                ));
            };
            remaining.remove(index);
        }
        if !remaining.is_empty() {
            return Err(Diagnostic::new(
                "ARTIFACT_UBAQ_CONFIGURATION_CHANGED",
                format!(
                    "Native import added unexpected {table} rows. The UBAQ will not be executed."
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod update_verification_tests {
    use super::*;
    #[test]
    fn native_identity_changes_are_allowed_but_lost_update_logic_is_rejected() {
        let source = crate::xml_archive::parse_xml(br#"<BAQDesignerDataSet><QueryUpdateFieldDesigner><Company>OLD</Company><QueryID>Source</QueryID><SysRowID>old</SysRowID><Expression>ttResult.Value</Expression><Direction>false</Direction></QueryUpdateFieldDesigner></BAQDesignerDataSet>"#).unwrap();
        let mut native = json!({"QueryUpdateSettingsDesigner":[],"QueryCustomActionDesigner":[],"QueryUpdateFieldDesigner":[{"Company":"NEW","QueryID":"Target","SysRowID":"new","Expression":"ttResult.Value","Direction":false}]});
        assert!(verify_update_configuration(&source, &native).is_ok());
        native["QueryUpdateFieldDesigner"][0]["Expression"] = json!("different");
        assert!(verify_update_configuration(&source, &native).is_err());
        native["QueryUpdateFieldDesigner"] = json!([]);
        assert!(verify_update_configuration(&source, &native).is_err());
    }
}

fn bpm_member(bytes: &[u8]) -> Result<Option<Value>, Diagnostic> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| invalid("Native query export is not a ZIP package."))?;
    let mut member = match archive.by_name("BpMethodDataset") {
        Ok(member) => member,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(_) => return Err(invalid("Cannot read the native UBAQ BPM member.")),
    };
    let mut bytes = Vec::new();
    member
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("Cannot read native UBAQ BPM definitions."))?;
    crate::xml_archive::parse_xml(&bytes).map(Some)
}
pub(crate) fn has_bpm(bytes: &[u8]) -> Result<bool, Diagnostic> {
    Ok(bpm_member(bytes)?.is_some())
}
fn normalized_xml(mut node: Value) -> Value {
    if let Some(attributes) = node["attributes"].as_array_mut() {
        attributes.retain(|a| {
            a["namespace"].as_str() != Some("http://www.w3.org/2000/xmlns/")
                && a["name"].as_str() != Some("xmlns")
        });
        attributes.sort_by_key(|a| (a["namespace"].to_string(), a["name"].to_string()));
    }
    if let Some(content) = node["content"].as_array_mut() {
        content.retain(|p| !p["text"].as_str().is_some_and(|t| t.trim().is_empty()));
        for part in content {
            if let Some(element) = part.get_mut("element") {
                *element = normalized_xml(element.take());
            }
        }
    }
    // Native XAML serialization expands an empty AdditionalReferences property
    // into List<ReferencesGroup>(Capacity=0). Both represent no references.
    if node["name"] == "DirectiveDefinition2.AdditionalReferences" {
        let empty_list = node["content"].as_array().is_some_and(|parts| {
            parts.len() == 1
                && parts[0]["element"]["name"] == "List"
                && parts[0]["element"]["namespace"]
                    .as_str()
                    .is_some_and(|ns| ns.starts_with("clr-namespace:System.Collections.Generic;"))
                && parts[0]["element"]["content"]
                    .as_array()
                    .is_some_and(|v| v.is_empty())
                && parts[0]["element"]["attributes"]
                    .as_array()
                    .is_some_and(|attrs| {
                        attrs.iter().all(|a| {
                            (a["name"] == "Capacity" && a["value"] == "0")
                                || (a["name"] == "TypeArguments"
                                    && a["value"]
                                        .as_str()
                                        .is_some_and(|v| v.ends_with(":ReferencesGroup")))
                        })
                    })
        });
        if empty_list {
            node["content"] = json!([]);
        }
    }
    node
}
pub(crate) fn verify_bpm(source: &[u8], exported: &[u8]) -> Result<(), Diagnostic> {
    let Some(expected) = bpm_member(source)? else {
        return Ok(());
    };
    let observed = bpm_member(exported)?.ok_or_else(|| Diagnostic::new("ARTIFACT_UBAQ_BPM_MISSING", "Native import omitted the supplied UBAQ BPM definitions. The UBAQ will not be executed."))?;
    fn directives(
        root: &Value,
    ) -> Result<Vec<(String, String, String, String, Value)>, Diagnostic> {
        let mut result = Vec::new();
        for part in root["content"]
            .as_array()
            .ok_or_else(|| invalid("BPM dataset has no content."))?
        {
            let Some(row) = part.get("element").filter(|r| r["name"] == "BpDirective") else {
                continue;
            };
            let method = text(child(row, "BpMethodCode")?)?;
            let method = method.rsplit('.').next().unwrap_or(&method).to_owned();
            let body = text(child(row, "Body")?)?;
            result.push((
                method,
                text(child(row, "Name")?)?,
                text(child(row, "DirectiveType")?)?,
                text(child(row, "IsEnabled")?)?,
                normalized_xml(crate::xml_archive::parse_xml(body.as_bytes())?),
            ));
        }
        Ok(result)
    }
    let expected = directives(&expected)?;
    let mut observed = directives(&observed)?;
    for directive in expected {
        let Some(index) = observed.iter().position(|row| row == &directive) else {
            return Err(Diagnostic::new(
                "ARTIFACT_UBAQ_BPM_CHANGED",
                format!(
                    "Native import did not preserve UBAQ directive {} on method {}. Its original package is retained and the UBAQ will not be executed.",
                    directive.1, directive.0
                ),
            ));
        };
        observed.remove(index);
    }
    Ok(())
}

#[cfg(test)]
mod bpm_verification_tests {
    use super::*;
    fn archive(method: &str, code: &str) -> Vec<u8> {
        let body = format!("<Action Code=\"{}\"/>", quick_xml::escape::escape(code));
        let xml = format!(
            "<BpMethodDataSet><BpDirective><BpMethodCode>{method}</BpMethodCode><Name>Owned</Name><DirectiveType>2</DirectiveType><IsEnabled>true</IsEnabled><Body>{}</Body></BpDirective></BpMethodDataSet>",
            quick_xml::escape::escape(&body)
        );
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("BpMethodDataset", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap().into_inner()
    }
    #[test]
    fn imported_bpm_keeps_its_logic_across_native_query_renaming() {
        let source = archive("Ice.OLD/Source.RunCustomAction", "return;");
        let mapped = archive("Ice.NEW/Target.RunCustomAction", "return;");
        assert!(verify_bpm(&source, &mapped).is_ok());
        assert!(
            verify_bpm(
                &source,
                &archive("Ice.NEW/Target.RunCustomAction", "throw;")
            )
            .is_err()
        );
        let empty = zip::ZipWriter::new(Cursor::new(Vec::new()))
            .finish()
            .unwrap()
            .into_inner();
        assert!(verify_bpm(&source, &empty).is_err());
    }
}
