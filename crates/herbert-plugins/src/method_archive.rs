//! Build native method packages without changing workflow XML or metadata.
use herbert_contract::Diagnostic;
use serde_json::Value;
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("METHOD_PACKAGE_INVALID", message)
}

/// Epicor packages external holds and BPM forms separately. Let its exporter
/// gather those resources until their local serialization is qualified.
pub fn needs_native_export(dataset: &Value) -> Result<bool, Diagnostic> {
    for row in dataset["BpDirective"].as_array().into_iter().flatten() {
        let body = row["Body"]
            .as_str()
            .ok_or_else(|| invalid("A directive has no workflow body."))?;
        crate::bpm_code::code_files(body)
            .map_err(|_| invalid("The directive workflow is not valid XML."))?;
        let mut reader = quick_xml::Reader::from_str(body);
        loop {
            match reader
                .read_event()
                .map_err(|_| invalid("The directive workflow is not valid XML."))?
            {
                quick_xml::events::Event::Start(node) | quick_xml::events::Event::Empty(node) => {
                    let attribute: &[u8] = match node.local_name().as_ref() {
                        b"HoldCondition" | b"AttachHoldAction" | b"RemoveHoldAction" => b"HoldType",
                        b"BpmDataFormAction" | b"BpmDataFormAction2" => b"FormId",
                        _ => continue,
                    };
                    for item in node.attributes() {
                        let item = item.map_err(|_| invalid("A workflow attribute is invalid."))?;
                        if item.key.local_name().as_ref() == attribute {
                            let value = item
                                .decoded_and_normalized_value(
                                    quick_xml::XmlVersion::Implicit1_0,
                                    reader.decoder(),
                                )
                                .map_err(|_| invalid("A workflow reference is invalid."))?;
                            if !value.is_empty() && !value.eq_ignore_ascii_case("{x:Null}") {
                                return Ok(true);
                            }
                        }
                    }
                }
                quick_xml::events::Event::Eof => break,
                _ => {}
            }
        }
    }
    Ok(false)
}

pub fn build(dataset: &Value, version: &str) -> Result<Vec<u8>, Diagnostic> {
    if version.is_empty() || needs_native_export(dataset)? {
        return Err(invalid(
            "This directive package requires the native dependency exporter.",
        ));
    }
    let xml = crate::baq_archive::dataset_xml(
        dataset,
        "BpMethodDataSet",
        "http://www.epicor.com/Ice/300/BO/BpMethod/BpMethod",
    )?;
    let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(9));
    for (name, bytes) in [
        ("version", format!("{version}\n").into_bytes()),
        ("BpMethodDataset", xml),
    ] {
        archive
            .start_file(name, options)
            .map_err(|_| invalid("The directive package could not be created."))?;
        archive
            .write_all(&bytes)
            .map_err(|_| invalid("The directive package could not be written."))?;
    }
    archive
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|_| invalid("The directive package could not be completed."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn dependency_widgets_use_native_export_and_unknown_widgets_are_preserved() {
        for body in [
            "<Workflow><AttachHoldAction HoldType='Credit'/></Workflow>",
            "<Workflow><BpmDataFormAction2 FormId='Prompt'/></Workflow>",
        ] {
            assert!(needs_native_export(&json!({"BpDirective":[{"Body":body}]})).unwrap());
        }
        let body = "<Workflow><FutureWidget Position='12,34'>unchanged</FutureWidget></Workflow>";
        let dataset =
            json!({"BpMethod":[],"BpArgument":[],"BpDirective":[{"Body":body,"IsProtected":true}]});
        let bytes = build(&dataset, "5.2.100.0").unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut xml = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("BpMethodDataset").unwrap(), &mut xml)
            .unwrap();
        let encoded = xml
            .split("<Body>")
            .nth(1)
            .unwrap()
            .split("</Body>")
            .next()
            .unwrap();
        assert_eq!(quick_xml::escape::unescape(encoded).unwrap(), body);
        assert!(needs_native_export(&json!({"BpDirective":[{"Body":"not XML"}]})).is_err());
    }
}
