//! Local XML projection for reference scanning. Original text is never rewritten.
use quick_xml::{Reader, events::Event};
use serde_json::{Map, Value};

/// Native JSONC permits comments and trailing commas. Keep literal contents;
/// replacing syntax with whitespace preserves every original string value.
pub fn json_candidate(text: &str) -> bool {
    let mut text = text;
    loop {
        text = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if let Some(comment) = text.strip_prefix("//") {
            let Some(end) = comment.find('\n') else { return false; };
            text = &comment[end + 1..];
        } else if let Some(comment) = text.strip_prefix("/*") {
            // Keep malformed candidate diagnostics for an unclosed comment.
            let Some(end) = comment.find("*/") else { return true; };
            text = &comment[end + 2..];
        } else {
            // SQL and C# commonly begin with comments too. A comment alone
            // does not make their remaining source a JSON document.
            return text.starts_with('{') || text.starts_with('[');
        }
    }
}

pub fn json(text: &str) -> Result<Value, String> {
    let text = text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let bytes = text.as_bytes();
    let mut output = bytes.to_vec();
    let mut i = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            quoted = true;
            i += 1;
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                output[i] = b' ';
                i += 1;
            }
            continue;
        }
        if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            output[i] = b' ';
            output[i + 1] = b' ';
            i += 2;
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    output[i] = b' ';
                    output[i + 1] = b' ';
                    i += 2;
                    closed = true;
                    break;
                }
                output[i] = b' ';
                i += 1;
            }
            if !closed {
                return Err("Unclosed JSONC comment".into());
            }
            continue;
        }
        i += 1;
    }
    quoted = false;
    escaped = false;
    for i in 0..output.len() {
        let b = output[i];
        if quoted {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                quoted = false;
            }
            continue;
        }
        if b == b'"' {
            quoted = true;
            continue;
        }
        if b == b','
            && output[i + 1..]
                .iter()
                .find(|b| !b.is_ascii_whitespace())
                .is_some_and(|b| *b == b'}' || *b == b']')
        {
            output[i] = b' ';
        }
    }
    use serde::Deserialize;
    let mut decoder = serde_json::Deserializer::from_slice(&output);
    decoder.disable_recursion_limit();
    Value::deserialize(&mut decoder)
        .and_then(|v| decoder.end().map(|_| v))
        .map_err(|e| e.to_string())
}

#[derive(Default)]
struct Node {
    name: String,
    fields: Map<String, Value>,
    text: String,
}
fn add(fields: &mut Map<String, Value>, name: String, value: Value) {
    match fields.entry(name) {
        serde_json::map::Entry::Vacant(e) => {
            e.insert(value);
        }
        serde_json::map::Entry::Occupied(mut e) => {
            if let Value::Array(a) = e.get_mut() {
                a.push(value);
            } else {
                let first = e.get().clone();
                e.insert(Value::Array(vec![first, value]));
            }
        }
    }
}
fn finish(node: Node) -> Value {
    if node.fields.is_empty() {
        Value::String(node.text)
    } else {
        let mut fields = node.fields;
        if !node.text.trim().is_empty() {
            fields.insert("#text".into(), Value::String(node.text));
        }
        Value::Object(fields)
    }
}
pub fn xml(text: &str) -> Result<Value, String> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut stack = vec![Node::default()];
    let mut roots = 0usize;
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                let empty = matches!(&event, Event::Empty(_));
                let e = match event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                if stack.len() == 1 {
                    roots += 1;
                    if roots != 1 {
                        return Err("XML contains multiple roots".into());
                    }
                }
                let name = String::from_utf8(e.local_name().as_ref().to_vec())
                    .map_err(|e| e.to_string())?;
                let mut node = Node {
                    name,
                    ..Default::default()
                };
                for attribute in e.attributes() {
                    let a = attribute.map_err(|e| e.to_string())?;
                    let name = String::from_utf8(a.key.local_name().as_ref().to_vec())
                        .map_err(|e| e.to_string())?;
                    if name == "xmlns" || a.key.as_ref().starts_with(b"xmlns:") {
                        continue;
                    }
                    add(
                        &mut node.fields,
                        name,
                        Value::String(
                            a.decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .map_err(|e| e.to_string())?
                            .into_owned(),
                        ),
                    );
                }
                if empty {
                    let name = node.name.clone();
                    add(&mut stack.last_mut().unwrap().fields, name, finish(node));
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err("Unbalanced XML".into());
                }
                let node = stack.pop().unwrap();
                let name = node.name.clone();
                add(&mut stack.last_mut().unwrap().fields, name, finish(node));
            }
            Event::Text(e) => {
                let value = e.decode().map_err(|e| e.to_string())?;
                if stack.len() == 1 && !value.trim().is_empty() {
                    return Err("XML text is outside its root".into());
                }
                stack.last_mut().unwrap().text.push_str(&value);
            }
            Event::CData(e) => {
                if stack.len() == 1 {
                    return Err("XML CDATA is outside its root".into());
                }
                stack
                    .last_mut()
                    .unwrap()
                    .text
                    .push_str(&e.decode().map_err(|e| e.to_string())?);
            }
            Event::GeneralRef(e) => {
                if stack.len() == 1 {
                    return Err("XML entity is outside its root".into());
                }
                let entity = e.decode().map_err(|e| e.to_string())?;
                let value = quick_xml::escape::unescape(&format!("&{entity};"))
                    .map_err(|e| e.to_string())?
                    .into_owned();
                stack.last_mut().unwrap().text.push_str(&value);
            }
            Event::DocType(_) => {
                return Err(
                    "DTD-based definitions require an unsupported external entity contract".into(),
                );
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 || roots != 1 {
        return Err("XML needs one complete root".into());
    }
    Ok(Value::Object(stack.pop().unwrap().fields))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_one_root_and_no_outside_text() {
        for text in [
            "<a/><a/>",
            "text<a/>",
            "<a/>text",
            "<a>",
            "<a></b>",
            "<a/><b/>",
            "<![CDATA[text]]><a/>",
        ] {
            assert!(xml(text).is_err(), "{text}");
        }
        assert!(
            xml(" <?xml version=\"1.0\"?><a><row DBSchemaName=\"Erp\" DBTableName=\"Part\"/></a> ")
                .is_ok()
        );
    }
    #[test]
    fn namespaces_values_and_entities_are_preserved() {
        let d=xml("<d xmlns='native'><row><DBSchemaName>Ice</DBSchemaName><DBTableName>A&amp;B</DBTableName></row></d>").unwrap();
        assert_eq!(d["d"]["row"]["DBTableName"], "A&B");
    }
    #[test]
    fn jsonc_comments_commas_and_literal_delimiters() {
        let d = json("{/* comment */\"TableName\":\"a,}//b/*c\",//line\n\"items\":[1,],}").unwrap();
        assert_eq!(d["TableName"], "a,}//b/*c");
        assert_eq!(d["items"][0], 1);
        assert!(json("{/* missing").is_err());
        assert!(json("{} {}").is_err());
    }

    #[test]
    fn wave1_leading_jsonc_comments_bom_arrays_and_literals() {
        for prefix in ["// lead\n", "/* lead */", " \u{feff} \n/* one */ // two\n"] {
            let text=format!("{prefix}[{{\"dbTable\":\"Erp.Part\",\"literal\":\"// untouched /* text */\",}},] // tail");
            assert!(json_candidate(&text));
            let value=json(&text).unwrap();
            assert_eq!(value[0]["dbTable"],"Erp.Part");
            assert_eq!(value[0]["literal"],"// untouched /* text */");
        }
        assert!(json_candidate("/* dbTable unterminated"));
        assert!(json("/* dbTable unterminated").is_err());
        assert!(!json_candidate("ordinary descriptive text"));
        assert!(!json_candidate("/* query description */ SELECT TableName FROM Tables"));
        assert!(!json_candidate("// code comment\nvar TableName = \"Part\";"));
        assert!(!json_candidate("// TableName is discussed here"));
    }

}
