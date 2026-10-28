//! Encode native EFXB v1: EF 01 followed by a BSON document.
use base64::Engine;
use herbert_contract::Diagnostic;
use serde_json::{Value, json};

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("FUNCTION_BINARY_INVALID", message)
}
fn length(n: usize) -> Result<[u8; 4], Diagnostic> {
    // BSON lengths are signed 32-bit values in the wire format.
    Ok(i32::try_from(n)
        .map_err(|_| invalid("The BSON document exceeds its Int32 length field."))?
        .to_le_bytes())
}
fn document(fields: impl IntoIterator<Item = (String, Value)>) -> Result<Vec<u8>, Diagnostic> {
    let mut out = vec![0; 4];
    for (name, value) in fields {
        if name.contains('\0') {
            return Err(invalid("A BSON field name contains a null character."));
        }
        let (kind, bytes) = encode(&value)?;
        out.push(kind);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend(bytes);
    }
    out.push(0);
    let size = length(out.len())?;
    out[..4].copy_from_slice(&size);
    Ok(out)
}
fn encode(value: &Value) -> Result<(u8, Vec<u8>), Diagnostic> {
    Ok(match value {
        Value::Null => (0x0a, vec![]),
        Value::Bool(v) => (0x08, vec![u8::from(*v)]),
        Value::String(v) => {
            let mut bytes = length(v.len() + 1)?.to_vec();
            bytes.extend_from_slice(v.as_bytes());
            bytes.push(0);
            (0x02, bytes)
        }
        Value::Number(v) => {
            if let Some(n) = v.as_i64() {
                match i32::try_from(n) {
                    Ok(n) => (0x10, n.to_le_bytes().to_vec()),
                    Err(_) => (0x12, n.to_le_bytes().to_vec()),
                }
            } else {
                return Err(invalid(
                    "An unexpected numeric field cannot be encoded without loss.",
                ));
            }
        }
        Value::Array(v) => (
            0x04,
            document(
                v.iter()
                    .enumerate()
                    .map(|(i, v)| (i.to_string(), v.clone())),
            )?,
        ),
        Value::Object(v) if v.contains_key("$binary") => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(
                    v["$binary"]
                        .as_str()
                        .ok_or_else(|| invalid("A binary field is invalid."))?,
                )
                .map_err(|_| invalid("A binary field is not base64."))?;
            let mut out = length(bytes.len())?.to_vec();
            out.push(
                v["$subtype"]
                    .as_u64()
                    .and_then(|n| u8::try_from(n).ok())
                    .ok_or_else(|| invalid("A binary subtype is invalid."))?,
            );
            out.extend(bytes);
            (0x05, out)
        }
        Value::Object(v) => (
            0x03,
            document(v.iter().map(|(k, v)| (k.clone(), v.clone())))?,
        ),
    })
}
pub fn build(export: &Value) -> Result<Vec<u8>, Diagnostic> {
    let mut binary = export.clone();
    let guid = binary["Library"]["GlobalID"]
        .as_str()
        .ok_or_else(|| invalid("The library GUID is missing."))?;
    let compact = guid.replace('-', "");
    if compact.len() != 32 || !compact.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("The library GUID is invalid."));
    }
    let mut bytes = (0..32)
        .step_by(2)
        .map(|i| u8::from_str_radix(&compact[i..i + 2], 16).unwrap())
        .collect::<Vec<_>>();
    // Native .NET Guid.ToByteArray reverses the first three GUID components.
    // Lab native EFXB declares these bytes with subtype 4.
    bytes[..4].reverse();
    bytes[4..6].reverse();
    bytes[6..8].reverse();
    binary["Library"]["GlobalID"] =
        json!({"$binary":base64::engine::general_purpose::STANDARD.encode(bytes),"$subtype":4});
    if let Some(functions) = binary["Library"]["Functions"].as_array_mut() {
        for function in functions {
            if let Some(thumbnail) = function["Thumbnail"].as_str() {
                function["Thumbnail"] = json!({"$binary":thumbnail,"$subtype":0});
            }
        }
    }
    let mut output = vec![0xef, 0x01];
    output.extend(document(
        binary
            .as_object()
            .ok_or_else(|| invalid("The export is not an object."))?
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    )?);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Requires the retained native EFXJ and independent BSON fixture."]
    fn retained_export_generates_binary_for_independent_verification() {
        let root = std::path::PathBuf::from(std::env::var("HERBERT_BINARY_FIXTURE").unwrap());
        let export: Value =
            serde_json::from_slice(&std::fs::read(root.join("source.efxj")).unwrap()).unwrap();
        let bytes = build(&export).unwrap();
        assert_eq!(&bytes[..2], &[0xef, 0x01]);
        assert_eq!(
            i32::from_le_bytes(bytes[2..6].try_into().unwrap()) as usize,
            bytes.len() - 2
        );
        std::fs::write(root.join("local.efxb"), bytes).unwrap();
    }
}
