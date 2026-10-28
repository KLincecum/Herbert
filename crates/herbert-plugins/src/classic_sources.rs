//! Extract readable Classic scripts from the downloaded source.
use herbert_contract::*;
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
use std::collections::BTreeMap;

// Native CustomizationDS.Properties pairs Script and Language for each form.
// Keep the exact decoded text. Never infer VB versus C# from code heuristics.
fn authored_scripts(xml: &str) -> Result<Vec<Value>, Diagnostic> {
    let mut reader = Reader::from_str(xml);
    let mut stack = Vec::<String>::new();
    let mut row = BTreeMap::<String, String>::new();
    let mut properties = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if name == "Properties" {
                    row.clear();
                }
                stack.push(name);
            }
            Ok(Event::Text(e)) => {
                if stack.len() >= 2 && stack[stack.len() - 2] == "Properties" {
                    let decoded = e.decode().map_err(|_| {
                        Diagnostic::new(
                            "CLASSIC_XML_INVALID",
                            "Classic source text could not be decoded.",
                        )
                    })?;
                    let text = quick_xml::escape::unescape(&decoded).map_err(|_| {
                        Diagnostic::new(
                            "CLASSIC_XML_INVALID",
                            "Classic source text contains invalid XML escapes.",
                        )
                    })?;
                    row.entry(stack.last().unwrap().clone())
                        .or_default()
                        .push_str(&text);
                }
            }
            Ok(Event::CData(e)) => {
                if stack.len() >= 2 && stack[stack.len() - 2] == "Properties" {
                    row.entry(stack.last().unwrap().clone())
                        .or_default()
                        .push_str(&e.decode().map_err(|_| {
                            Diagnostic::new(
                                "CLASSIC_XML_INVALID",
                                "Classic source CDATA could not be decoded.",
                            )
                        })?);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if stack.len() >= 2 && stack[stack.len() - 2] == "Properties" {
                    let name = e.decode().map_err(|_| {
                        Diagnostic::new(
                            "CLASSIC_XML_INVALID",
                            "Classic XML reference could not be decoded.",
                        )
                    })?;
                    let escaped = format!("&{name};");
                    let text = quick_xml::escape::unescape(&escaped).map_err(|_| {
                        Diagnostic::new(
                            "CLASSIC_XML_INVALID",
                            "Classic XML contains an unresolved entity.",
                        )
                    })?;
                    row.entry(stack.last().unwrap().clone())
                        .or_default()
                        .push_str(&text);
                }
            }
            Ok(Event::End(e)) => {
                if e.local_name().as_ref() == b"Properties" {
                    properties.push(row.clone());
                }
                stack.pop();
            }
            Ok(Event::Eof) => break,
            Ok(_) => (),
            Err(_) => {
                return Err(Diagnostic::new(
                    "CLASSIC_XML_INVALID",
                    "Classic Content is malformed XML; the original definition is retained.",
                ));
            }
        }
    }
    if !stack.is_empty() {
        return Err(Diagnostic::new(
            "CLASSIC_XML_INVALID",
            "Classic Content XML is incomplete.",
        ));
    }
    let mut files = Vec::new();
    for row in &properties {
        if row.get("PropertyName").map(String::as_str) != Some("Script") {
            continue;
        }
        let Some(text) = row.get("PropertyValue").filter(|s| !s.is_empty()) else {
            continue;
        };
        let languages: Vec<_> = properties
            .iter()
            .filter(|other| {
                other.get("PropertyName").map(String::as_str) == Some("Language")
                    && other.get("FormsID") == row.get("FormsID")
                    && other.get("ControlName") == row.get("ControlName")
            })
            .filter_map(|other| other.get("PropertyValue"))
            .collect();
        let extension = match languages.as_slice() {
            [language] if language.as_str() == "CSharp" => "cs",
            [language] if language.as_str() == "VisualBasic" => "vb",
            // A new native language remains readable without an invented extension.
            [language] if !language.is_empty() => "txt",
            _ => "txt",
        };
        files
            .push(json!({"name":format!("Code/Script-{}.{}",files.len()+1,extension),"text":text,"languageUnverified":languages.len()!=1 || languages.first().is_some_and(|s|s.is_empty())}));
    }
    Ok(files)
}

pub fn append_persisted_scripts(definition: &mut Value) -> Vec<Diagnostic> {
    let Some(xml) = definition
        .pointer("/XXXDef/0/Content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return vec![];
    };
    match authored_scripts(xml) {
        Ok(mut files) => {
            let unknown = files.iter().filter(|f| f["languageUnverified"] == true).count();
            for file in &mut files { file.as_object_mut().unwrap().remove("languageUnverified"); }
            definition["herbertSources"]["files"] = json!(files);
            if unknown == 0 { vec![] } else { vec![Diagnostic::new("CLASSIC_LANGUAGE_UNVERIFIED",
                "One or more Classic scripts have no unique language property. Their exact text is saved as .txt; original XML is preserved.")] }
        }
        Err(error) => vec![error],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classic_scripts_preserve_native_language_unicode_and_xml_escapes() {
        for (language, extension) in [
            ("CSharp", "cs"),
            ("VisualBasic", "vb"),
            ("FutureLanguage", "txt"),
        ] {
            let xml = format!(
                "<CustomizationDS><Properties><FormsID>F</FormsID><ControlName>Customization</ControlName><PropertyName>Script</PropertyName><PropertyValue> a &lt; b &amp; café Ω\n</PropertyValue></Properties><Properties><FormsID>F</FormsID><ControlName>Customization</ControlName><PropertyName>Language</PropertyName><PropertyValue>{language}</PropertyValue></Properties></CustomizationDS>"
            );
            let files = authored_scripts(&xml).unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(files[0]["name"], format!("Code/Script-1.{extension}"));
            assert_eq!(files[0]["text"], " a < b & café Ω\n");
        }
    }
    #[test]
    fn malformed_and_unknown_language_scripts_are_not_silently_accepted() {
        assert!(authored_scripts("<CustomizationDS>").is_err());
        let xml="<CustomizationDS><Properties><PropertyName>Script</PropertyName><PropertyValue>code Ω</PropertyValue></Properties></CustomizationDS>";
        let files=authored_scripts(xml).unwrap();assert_eq!(files[0]["name"],"Code/Script-1.txt");assert_eq!(files[0]["text"],"code Ω");
        let mut definition=json!({"XXXDef":[{"Content":xml}]});
        assert_eq!(append_persisted_scripts(&mut definition).len(),1);
        assert_eq!(definition["herbertSources"]["files"][0]["text"],"code Ω");
        assert!(authored_scripts("<CustomizationDS/>").unwrap().is_empty());
    }
}
