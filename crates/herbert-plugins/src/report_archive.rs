//! Write the native report DataSet formats from complete downloaded rows.
//! The native RDD exporter adds ExportVersion=1; BAQ reports use plain XML.
use base64::Engine;
use herbert_contract::{Diagnostic,GuestRequest,Identity};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub fn prefix(request: &GuestRequest,key: &str) -> Result<String,Diagnostic> {
    match request.state.get("_pluginSettings").and_then(|settings|settings.get(key)) {
        None=>Ok(String::new()),Some(Value::String(value))=>Ok(value.clone()),
        Some(_)=>Err(Diagnostic::new("INVALID_SETTING","Report selection prefixes must be strings.")),
    }
}
pub fn with_settings(mut state: Value,from: &Value) -> Value {
    for key in ["_pluginSettings","_collectionMethod"] {if let Some(value)=from.get(key) {if !state.is_object(){state=json!({});}state[key]=value.clone();}}state
}
pub fn graph_errors(identity: &Identity,data: &Value) -> Vec<Diagnostic> {
    data["errors"].as_array().into_iter().flatten().map(|value|{
        let mut error=serde_json::from_value::<Diagnostic>(value.clone()).unwrap_or_else(|_|Diagnostic::new("REPORT_GRAPH_INVALID","An acquired report graph has an invalid failure diagnostic."));error.identity=Some(identity.clone());error
    }).collect()
}

/// Supply the host with the exact byte count and hash for each binary output.
pub fn binary(name: &str, bytes: &[u8]) -> Value {
    json!({"name":name,"base64":base64::engine::general_purpose::STANDARD.encode(bytes),"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))})
}

pub fn append_binary(data: &mut Value, file: Value) {
    if !data["herbertSources"]["binaryFiles"].is_array() {
        data["herbertSources"]["binaryFiles"] = json!([]);
    }
    data["herbertSources"]["binaryFiles"]
        .as_array_mut()
        .unwrap()
        .push(file);
}

type Schemas = BTreeMap<String, BTreeMap<String, Vec<(String, String)>>>;
pub fn schema(service: &str) -> BTreeMap<String, Vec<(String, String)>> {
    let schemas: Schemas = serde_json::from_str(include_str!("report-datasets.json")).unwrap();
    schemas[service].clone()
}
pub fn xml(service: &str, data: &Value) -> Result<String, Diagnostic> {
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><{service}DataSet xmlns=\"http://www.epicor.com/Ice/300/BO/{service}/{service}\">"
    );
    for (table, fields) in schema(service) {
        let Some(rows) = data.get(&table).and_then(Value::as_array) else {
            continue;
        };
        for row in rows {
            text.push_str(&format!("<{table}>"));
            for (field, kind) in &fields {
                let Some(value) = row.get(field).filter(|v| !v.is_null()) else {
                    continue;
                };
                let value = match (kind.as_str(), value) {
                    ("string" | "Guid" | "DateTime" | "byte[]", Value::String(s)) => s.clone(),
                    ("bool", Value::Bool(v)) => v.to_string(),
                    (
                        "int" | "long" | "uint" | "ulong" | "decimal" | "short" | "byte" | "double",
                        Value::Number(v),
                    ) => v.to_string(),
                    ("ulong", Value::String(s)) if s.parse::<u64>().is_ok() => s.clone(),
                    _ => {
                        return Err(Diagnostic::new(
                            "REPORT_XML_INVALID",
                            format!("{table}.{field} does not match its native {kind} type."),
                        ));
                    }
                };
                text.push_str(&format!(
                    "<{field}>{}</{field}>",
                    crate::classic_archive::escape(&value)?
                ));
            }
            text.push_str(&format!("</{table}>"));
        }
    }
    if service == "RptDataDef" {
        text.push_str("<ExportVersion><Version>1</Version></ExportVersion>");
    }
    text.push_str(&format!("</{service}DataSet>"));
    Ok(text)
}
pub fn append(service: &str, name: &str, data: &mut Value) -> Result<(), Diagnostic> {
    let text = xml(service, data)?;
    data["herbertSources"] = json!({"files":[{"name":format!("{}_{}.xml",service,crate::bpm_code::label(name)),"text":text}]});
    data["herbertDirectOutput"] = json!(true);
    Ok(())
}
