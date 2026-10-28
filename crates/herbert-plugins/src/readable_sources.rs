//! Select readable authored text from these plugins' Epicor definitions.
use herbert_contract::GuestReply;
use serde_json::{Value, json};

fn visit(value: &Value, path: &str, views: &mut Vec<Value>) {
    match value {
        Value::Array(rows) => {
            for (index, row) in rows.iter().enumerate() { visit(row, &format!("{path}/{index}"), views); }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                // Declarations are metadata, never another source to extract.
                if key == "herbertSources" { continue; }
                let pointer = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                if matches!(key.as_str(), "Body" | "RuleBody" | "Expression" | "ScriptExpression") {
                    if let Some(text) = value.as_str().filter(|text| !text.is_empty()) {
                        let trimmed = text.trim_start_matches('\u{feff}').trim_start();
                        let parsed = serde_json::from_str::<Value>(text).ok();
                        if !trimmed.starts_with('<') && parsed.is_none() {
                            let extension = if key == "Body" && fields.contains_key("FunctionID") { "cs" } else { "txt" };
                            views.push(json!({"pointer":pointer,"extension":extension,"text":text}));
                        }
                        if key == "Body" {
                            if let Some(body) = parsed {
                                for (field, extension) in [("Code", "cs"), ("Usings", "usings.txt")] {
                                    if let Some(text) = body.get(field).and_then(Value::as_str) {
                                        views.push(json!({"pointer":pointer,"extension":extension,"text":text}));
                                    }
                                }
                            }
                        }
                    }
                }
                visit(value, &pointer, views);
            }
        }
        _ => {}
    }
}

pub fn attach(reply: &mut GuestReply) {
    match reply {
        GuestReply::State { reply, .. } => attach(reply),
        GuestReply::Definition { definition, .. } if definition.is_object() => {
            let mut views = Vec::new();
            visit(definition, "", &mut views);
            if !views.is_empty() {
                if definition.get("herbertSources").is_none() { definition["herbertSources"] = json!({}); }
                // Existing source declarations remain authoritative and must not
                // be discarded when this plugin also supplies ordinary fields.
                if let Some(sources) = definition["herbertSources"].as_object_mut() {
                    let target = sources.entry("views").or_insert_with(|| json!([]));
                    if let Some(target) = target.as_array_mut() { target.extend(views); }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readable_code_preserves_paths_text_and_existing_declarations() {
        let text = "// Ω\r\nreturn 42;";
        let value = json!({"Functions":[{"FunctionID":"f","Body":text},{"Body":json!({"Code":text,"Usings":"using System;"}).to_string()}],"Other":{"Expression":"custom(value)","Content":"unrequested","RuleBody":"<Rules/>"},"herbertSources":{"files":[{"name":"Code/Keep.vb","text":"keep"}]}});
        let mut views=Vec::new();visit(&value,"",&mut views);
        assert_eq!(views.len(),4);
        assert_eq!(views[0],json!({"pointer":"/Functions/0/Body","extension":"cs","text":text}));
        assert_eq!(views[1]["text"],text);
        assert_eq!(views[2]["extension"],"usings.txt");
        assert!(!views.iter().any(|v|v["text"]=="unrequested"||v["text"]=="keep"));
        let mut reply=GuestReply::Definition {
            identity:herbert_contract::Identity {category:"test".into(),kind:"test".into(),scope:herbert_contract::Scope::Global,keys:Default::default()},
            format:"native".into(),definition:value,
            completeness:herbert_contract::Completeness::Collected,diagnostics:vec![],
        };
        attach(&mut reply);
        let GuestReply::Definition {definition,..}=reply else {panic!()};
        assert_eq!(definition["herbertSources"]["files"],json!([{"name":"Code/Keep.vb","text":"keep"}]));
        assert_eq!(definition["herbertSources"]["views"],json!(views));
    }
}
