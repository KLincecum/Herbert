//! Generic ZIP/XML projection. No filesystem extraction or entity resolution.
//! Ownership of acquired input is checked by Storage before this pure parser.
use herbert_contract::Diagnostic;
use quick_xml::{XmlVersion, events::Event, name::ResolveResult, reader::NsReader};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read},
};
fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn namespace(value: ResolveResult<'_>) -> Result<Value, Diagnostic> {
    match value {
        ResolveResult::Unbound => Ok(Value::Null),
        ResolveResult::Bound(value) => Ok(json!(
            std::str::from_utf8(value.as_ref())
                .map_err(|_| error("XML_ENCODING_UNSUPPORTED", "An XML namespace is not UTF-8."))?
        )),
        ResolveResult::Unknown(_) => Err(error(
            "XML_NAMESPACE_UNBOUND",
            "An XML prefix has no declared namespace.",
        )),
    }
}
fn xml_character(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r')
        || matches!(c as u32,0x20..=0xd7ff|0xe000..=0xfffd|0x10000..=0x10ffff)
}
// Move a completed child into its parent. json! serializes a borrowed Value
// and copies the full subtree at each nesting level.
fn element_content(node: Value) -> Value {
    Value::Object(serde_json::Map::from_iter([("element".into(), node)]))
}
fn decode(bytes: &[u8]) -> Result<String, Diagnostic> {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        if bytes.len() % 2 != 0 {
            return Err(error(
                "XML_ENCODING_INVALID",
                "The UTF-16 XML byte length is odd.",
            ));
        }
        let little = bytes[0] == 0xff;
        let units: Vec<_> = bytes[2..]
            .chunks_exact(2)
            .map(|p| {
                if little {
                    u16::from_le_bytes([p[0], p[1]])
                } else {
                    u16::from_be_bytes([p[0], p[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| {
            error(
                "XML_ENCODING_INVALID",
                "The native XML contains invalid UTF-16.",
            )
        })
    } else {
        String::from_utf8(
            bytes
                .strip_prefix(&[0xef, 0xbb, 0xbf])
                .unwrap_or(bytes)
                .to_vec(),
        )
        .map_err(|_| {
            error(
                "XML_ENCODING_UNSUPPORTED",
                "Native XML must use UTF-8 or BOM-marked UTF-16; original bytes remain preserved.",
            )
        })
    }
}
pub fn parse_xml(bytes: &[u8]) -> Result<Value, Diagnostic> {
    let text = decode(bytes)?;
    if !text.chars().all(xml_character) {
        return Err(error(
            "XML_INVALID_CHARACTER",
            "Native XML contains a character forbidden by XML 1.0.",
        ));
    }
    let mut reader = NsReader::from_str(&text);
    reader.config_mut().check_end_names = true;
    let mut stack = Vec::<Value>::new();
    let mut root = None;
    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|_| error("XML_INVALID", "The native member contains malformed XML."))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let ns = namespace(ns)?;
                let name = std::str::from_utf8(e.local_name().as_ref())
                    .map_err(|_| error("XML_ENCODING_INVALID", "An XML element name is invalid."))?
                    .to_owned();
                let mut attributes = Vec::new();
                let mut names = BTreeSet::new();
                for attribute in e.attributes() {
                    let attribute = attribute.map_err(|_| {
                        error(
                            "XML_INVALID",
                            "An XML attribute is malformed or duplicated.",
                        )
                    })?;
                    let (ns, name) = reader.resolver().resolve_attribute(attribute.key);
                    let ns = namespace(ns)?;
                    let name = std::str::from_utf8(name.as_ref())
                        .map_err(|_| {
                            error("XML_ENCODING_INVALID", "An XML attribute name is invalid.")
                        })?
                        .to_owned();
                    let value = attribute
                        .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                        .map_err(|_| {
                            error(
                                "XML_ENTITY_UNSUPPORTED",
                                "An XML attribute contains an undeclared entity.",
                            )
                        })?;
                    if !value.chars().all(xml_character) {
                        return Err(error(
                            "XML_INVALID_CHARACTER",
                            "An XML attribute contains a forbidden character.",
                        ));
                    }
                    if !names.insert((ns.to_string(), name.clone())) {
                        return Err(error(
                            "XML_INVALID",
                            "XML attributes repeat an expanded name.",
                        ));
                    }
                    attributes.push(json!({"name":name,"namespace":ns,"value":value.as_ref()}));
                }
                let node = json!({"name":name,"namespace":ns,"attributes":attributes,"content":[]});
                if matches!(event, Event::Empty(_)) {
                    if let Some(parent) = stack.last_mut() {
                        parent["content"]
                            .as_array_mut()
                            .unwrap()
                            .push(element_content(node));
                    } else if root.replace(node).is_some() {
                        return Err(error("XML_INVALID", "XML has more than one root element."));
                    }
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                let node = stack
                    .pop()
                    .ok_or_else(|| error("XML_INVALID", "An XML end element has no parent."))?;
                if let Some(parent) = stack.last_mut() {
                    parent["content"]
                        .as_array_mut()
                        .unwrap()
                        .push(element_content(node));
                } else if root.replace(node).is_some() {
                    return Err(error("XML_INVALID", "XML has more than one root element."));
                }
            }
            Event::Text(e) => {
                let decoded = e
                    .xml_content(XmlVersion::Implicit1_0)
                    .map_err(|_| error("XML_ENCODING_INVALID", "XML text cannot be decoded."))?;
                let value = decoded;
                if let Some(parent) = stack.last_mut() {
                    parent["content"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"text":value.as_ref()}));
                } else if !value.trim().is_empty() {
                    return Err(error(
                        "XML_INVALID",
                        "XML contains text outside its root element.",
                    ));
                }
            }
            Event::CData(e) => {
                let value = e
                    .decode()
                    .map_err(|_| error("XML_ENCODING_INVALID", "XML CDATA cannot be decoded."))?;
                let parent = stack
                    .last_mut()
                    .ok_or_else(|| error("XML_INVALID", "CDATA is outside its XML root."))?;
                parent["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"text":value.as_ref()}));
            }
            Event::DocType(_) => {
                return Err(error(
                    "XML_DTD_UNSUPPORTED",
                    "Native XML with a DTD is not resolved; external files and entities are never loaded.",
                ));
            }
            Event::GeneralRef(e) => {
                let value = if let Some(value) = e.resolve_char_ref().map_err(|_| {
                    error(
                        "XML_ENTITY_UNSUPPORTED",
                        "An XML numeric reference is invalid.",
                    )
                })? {
                    if !xml_character(value) {
                        return Err(error(
                            "XML_INVALID_CHARACTER",
                            "A numeric reference is forbidden by XML 1.0.",
                        ));
                    }
                    value.to_string()
                } else {
                    let name = e.decode().map_err(|_| {
                        error("XML_ENCODING_INVALID", "An XML entity name is invalid.")
                    })?;
                    quick_xml::escape::resolve_xml_entity(&name)
                        .ok_or_else(|| {
                            error(
                                "XML_ENTITY_UNSUPPORTED",
                                "Native XML contains an undeclared entity reference.",
                            )
                        })?
                        .to_owned()
                };
                let parent = stack.last_mut().ok_or_else(|| {
                    error(
                        "XML_INVALID",
                        "An entity reference is outside the XML root.",
                    )
                })?;
                parent["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"text":value}));
            }
            Event::Decl(e) => {
                if e.version()
                    .map_err(|_| error("XML_INVALID", "The XML declaration is invalid."))?
                    .as_ref()
                    != b"1.0"
                {
                    return Err(error(
                        "XML_VERSION_UNSUPPORTED",
                        "The native XML uses a version other than XML 1.0; original bytes remain preserved.",
                    ));
                }
            }
            Event::PI(_) | Event::Comment(_) => (),
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(error(
            "XML_INVALID",
            "The XML member ends inside an element.",
        ));
    }
    root.ok_or_else(|| error("XML_INVALID", "The native XML member has no root element."))
}
pub fn parse(bytes: &[u8], requested: &[String]) -> Result<Value, Diagnostic> {
    parse_members(bytes, requested, "xml", parse_xml)
}
pub fn parse_json(bytes: &[u8], requested: &[String]) -> Result<Value, Diagnostic> {
    parse_members(bytes, requested, "json", |bytes| {
        serde_json::from_slice(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)).map_err(
            |_| {
                error(
                    "NATIVE_JSON_INVALID",
                    "The package member contains invalid JSON.",
                )
            },
        )
    })
}
fn parse_members(
    bytes: &[u8],
    requested: &[String],
    format: &str,
    parser: impl Fn(&[u8]) -> Result<Value, Diagnostic>,
) -> Result<Value, Diagnostic> {
    let wanted: BTreeSet<_> = requested.iter().collect();
    if wanted.len() != requested.len() {
        return Err(error(
            "ARCHIVE_REQUEST_INVALID",
            "Requested member names must be unique.",
        ));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| {
        error(
            "ARCHIVE_INVALID",
            "The native package ZIP directory is invalid.",
        )
    })?;
    let mut members = Vec::new();
    let mut documents = BTreeMap::<String, Value>::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|_| {
            error(
                "ARCHIVE_ENTRY_UNAVAILABLE",
                "A native package member cannot be decoded.",
            )
        })?;
        let name = file.name().to_owned();
        members
            .push(json!({"name":name,"bytes":file.size(),"index":index,"directory":file.is_dir()}));
        if !wanted.contains(&name) {
            continue;
        }
        if documents.contains_key(&name) {
            return Err(error(
                "ARCHIVE_MEMBER_AMBIGUOUS",
                "The native package repeats a requested member name.",
            ));
        }
        if file.is_dir() {
            return Err(error(
                "ARCHIVE_MEMBER_INVALID",
                "A requested member is a directory.",
            ));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|_| {
            error(
                "ARCHIVE_ENTRY_INVALID",
                "A native package member failed decompression or CRC validation.",
            )
        })?;
        if bytes.len() as u64 != file.size() {
            return Err(error(
                "ARCHIVE_LENGTH_MISMATCH",
                "The expanded native package length differs from its ZIP directory.",
            ));
        }
        let (xml, diagnostic) = if bytes.is_empty() {
            (Value::Null, None)
        } else {
            match parser(&bytes) {
                Ok(xml) => (xml, None),
                Err(error) => (Value::Null, Some(error)),
            }
        };
        documents.insert(name,json!({"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes)),format:xml,"diagnostic":diagnostic}));
    }
    let missing: Vec<_> = requested
        .iter()
        .filter(|name| !documents.contains_key(*name))
        .cloned()
        .collect();
    Ok(json!({"members":members,"documents":documents,"missing":missing}))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_subtrees_keep_order_text_and_namespace() {
        let tree = parse_xml(br#"<Report xmlns='urn:rdl'><Body><Items><Subreport Name='s'><ReportName>Sub&amp;One</ReportName></Subreport><Image><Source>External</Source><Value>x</Value></Image></Items></Body><Code><![CDATA[Function F()]]></Code></Report>"#).unwrap();
        let body = &tree["content"][0]["element"];
        let items = &body["content"][0]["element"]["content"];
        assert_eq!(items[0]["element"]["name"], "Subreport");
        assert_eq!(items[0]["element"]["attributes"][0]["value"], "s");
        let name = &items[0]["element"]["content"][0]["element"];
        assert_eq!(name["namespace"], "urn:rdl");
        let text: String = name["content"].as_array().unwrap().iter()
            .map(|part| part["text"].as_str().unwrap()).collect();
        assert_eq!(text, "Sub&One");
        assert_eq!(items[1]["element"]["name"], "Image");
        assert_eq!(tree["content"][1]["element"]["content"][0]["text"], "Function F()");
    }

    #[test]
    fn json_members_preserve_values_and_report_only_the_failed_member() {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in [
            (
                "good",
                r#"{"password":"Keep Ω","empty":"","number":42,"flag":false}"#,
            ),
            ("bad", "{"),
            ("unrequested", "not JSON"),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        let result = parse_json(
            &zip.finish().unwrap().into_inner(),
            &["good".into(), "bad".into(), "missing".into()],
        )
        .unwrap();
        assert_eq!(
            result["documents"]["good"]["json"],
            json!({"password":"Keep Ω","empty":"","number":42,"flag":false})
        );
        assert_eq!(
            result["documents"]["bad"]["diagnostic"]["code"],
            "NATIVE_JSON_INVALID"
        );
        assert!(result["documents"].get("unrequested").is_none());
        assert_eq!(result["missing"], json!(["missing"]));
    }
    #[test]
    fn text_cdata_and_namespace_identity_are_preserved() {
        let value=parse_xml(br#"<d xmlns='urn:row' xmlns:x='urn:field'><x:f a='&quot;'> a &amp; b <![CDATA[<code>]]></x:f></d>"#).unwrap();
        assert_eq!(value["namespace"], "urn:row");
        let field = &value["content"][0]["element"];
        assert_eq!(field["namespace"], "urn:field");
        assert_eq!(field["attributes"][0]["value"], "\"");
        let text: String = field["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["text"].as_str().unwrap())
            .collect();
        assert_eq!(text, " a & b <code>");
    }
    #[test]
    fn dtd_external_entity_malformed_and_multiple_roots_fail() {
        for text in [
            "<!DOCTYPE root [<!ENTITY x SYSTEM 'file:///private'>]><root>&x;</root>",
            "<root>&unknown;</root>",
            "<root><child></root>",
            "<a/><b/>",
            "<a>&#1;</a>",
            "<a>\u{0001}</a>",
        ] {
            assert!(parse_xml(text.as_bytes()).is_err());
        }
    }
    #[test]
    fn utf16_preserves_source_text() {
        let mut bytes = vec![0xff, 0xfe];
        for unit in "<root>😀</root>".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        assert_eq!(parse_xml(&bytes).unwrap()["content"][0]["text"], "😀");
    }
    #[test]
    fn member_failures_are_local_and_unrequested_content_is_not_parsed() {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in [
            ("good", "<r>ok</r>"),
            ("bad", "<r>"),
            ("plain", "native non-XML"),
        ] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        let result = parse(&bytes, &["good".into(), "bad".into(), "missing".into()]).unwrap();
        assert_eq!(result["documents"]["good"]["xml"]["name"], "r");
        assert_eq!(
            result["documents"]["bad"]["diagnostic"]["code"],
            "XML_INVALID"
        );
        assert_eq!(result["missing"], json!(["missing"]));
        assert_eq!(result["members"].as_array().unwrap().len(), 3);
    }
    #[test]
    fn bad_zip_crc_is_not_content_proof() {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "data",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"<r>original</r>").unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let at = bytes.windows(8).position(|w| w == b"original").unwrap();
        bytes[at] = b'x';
        assert!(parse(&bytes, &["data".into()]).is_err());
    }
}
