//! Host-side decoding of independently preserved legacy chunk streams.
//! Keep in the native host: bzip2-sys is not a portable Wasm guest dependency.
use base64::Engine;
use herbert_contract::Diagnostic;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;

pub struct Reconstruction {
    pub bytes: Vec<u8>,
    pub encoding: &'static str,
    pub sequence: Vec<i64>,
}
fn error(message: &str) -> Diagnostic {
    Diagnostic::new("PERSISTED_SOURCE_UNVERIFIED", message)
}
fn integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}
/// SQL nvarchar byte length uses UTF-16 code units, including surrogate pairs.
/// Request CONVERT(nvarchar(max), DATALENGTH(CONVERT(nvarchar(max), alias.field)))
/// anchored to the same native text field, as verified with the Function source.
pub fn verify_text_length(row: &Value, field: &str, length_field: &str) -> Result<(), Diagnostic> {
    match (row.get(field), row.get(length_field)) {
        (Some(Value::Null), Some(Value::Null)) => Ok(()),
        (Some(Value::String(text)), Some(length)) => {
            let Some(expected) = integer(length).and_then(|n| usize::try_from(n).ok()) else {
                return Err(error(
                    "A source byte-length projection is absent or malformed.",
                ));
            };
            if text.encode_utf16().count().checked_mul(2) == Some(expected) {
                Ok(())
            } else {
                Err(error(
                    "A saved text field differs from its Epicor DATALENGTH.",
                ))
            }
        }
        _ => Err(error(
            "A saved text field or Epicor byte-length projection is absent.",
        )),
    }
}
/// Caller must reconcile all full parent-key fields before decoding. Physical
/// XXXChunk key adds SeqNum to Company/ProductID/TypeCode/Key1/Key2/Key3/CGCCode.
/// Empty child arrays are legitimate when a native root already holds Content.
/// Numeric sequence ordering and uniqueness are required. No undocumented start
/// value, gap policy, chunk length, or resource-size cap is imposed.
pub fn reconstruct(
    rows: &[Value],
    length_field: &str,
) -> Result<Option<Reconstruction>, Diagnostic> {
    reconstruct_impl(rows, length_field, false)
}
/// Native GenX import also accepts uncompressed XML. Keep that representation
/// explicit: corrupt compressed bytes never become successful arbitrary text.
pub fn reconstruct_with_xml(
    rows: &[Value],
    length_field: &str,
) -> Result<Option<Reconstruction>, Diagnostic> {
    reconstruct_impl(rows, length_field, true)
}
fn reconstruct_impl(
    rows: &[Value],
    length_field: &str,
    allow_xml: bool,
) -> Result<Option<Reconstruction>, Diagnostic> {
    if rows.is_empty() {
        return Ok(None);
    }
    let mut ordered = BTreeMap::new();
    for row in rows {
        let sequence = row
            .get("SeqNum")
            .and_then(integer)
            .ok_or_else(|| error("A saved chunk lacks its numeric SeqNum."))?;
        verify_text_length(row, "Chunk", length_field)?;
        let text = row
            .get("Chunk")
            .and_then(Value::as_str)
            .ok_or_else(|| error("A saved chunk payload is not text."))?;
        if ordered.insert(sequence, text).is_some() {
            return Err(error("The saved chunk stream repeats a SeqNum."));
        }
    }
    let joined: String = ordered.values().copied().collect();
    if allow_xml && joined.trim_start_matches('﻿').trim_start().starts_with('<') {
        validate_xml_structure(&joined)?;
        return Ok(Some(Reconstruction {
            bytes: joined.into_bytes(),
            encoding: "xml-text",
            sequence: ordered.keys().copied().collect(),
        }));
    }
    // Convert.FromBase64String used by GenXHelper permits ASCII whitespace.
    let compact: String = joined
        .chars()
        .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
        .collect();
    let compressed = base64::engine::general_purpose::STANDARD.decode(compact)
        .map_err(|_| error("The saved chunk stream is not valid base64; its original rows remain preserved."))?;
    if !compressed.starts_with(b"BZh") {
        return Err(error(
            "The saved chunk encoding is unrecognized; its original bytes remain preserved.",
        ));
    }
    let mut decoded = Vec::new();
    bzip2::read::MultiBzDecoder::new(compressed.as_slice())
        .read_to_end(&mut decoded)
        .map_err(|_| {
            error("The saved bzip2 stream failed decoding or integrity verification.")
        })?;
    Ok(Some(Reconstruction {
        bytes: decoded,
        encoding: "base64+bzip2",
        sequence: ordered.keys().copied().collect(),
    }))
}

// This verifies tokens, element boundaries and a single root. DTD/entity
// declarations remain opaque retained bytes. The reader neither resolves them
// nor proves XML entity validity.
fn validate_xml_structure(text: &str) -> Result<(), Diagnostic> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut depth = 0usize;
    let mut roots = 0usize;
    loop {
        match reader
            .read_event()
            .map_err(|_| error("The uncompressed retained chunk stream has invalid XML token or element structure."))?
        {
            Event::Start(_) => {
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
            }
            Event::Empty(_) => {
                if depth == 0 {
                    roots += 1;
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| error("The retained XML closes an absent element."))?;
            }
            Event::Text(text)
                if depth == 0 && !text.iter().all(u8::is_ascii_whitespace) =>
            {
                return Err(error("The retained XML contains text outside its root."));
            }
            Event::GeneralRef(_) if depth == 0 => {
                return Err(error(
                    "The retained XML contains an entity outside its root.",
                ));
            }
            Event::CData(_) if depth == 0 => {
                return Err(error("The retained XML contains CDATA outside its root."));
            }
            Event::Eof => break,
            _ => (),
        }
    }
    if depth != 0 || roots != 1 {
        return Err(error(
            "The uncompressed retained chunk stream lacks one complete XML root.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    fn rows(bytes: &[u8]) -> Vec<Value> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
        encoder.write_all(bytes).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(encoder.finish().unwrap());
        let split = encoded.len() / 2;
        [(20, &encoded[split..]), (10, &encoded[..split])].into_iter().map(|(seq,text)| json!({"SeqNum":seq,"Chunk":text,"HerbertChunkBytes":(text.encode_utf16().count()*2).to_string()})).collect()
    }
    #[test]
    fn numeric_order_and_lossless_bzip2_reconstruction_are_verified() {
        let source = b"<Customization BuildVersion=\"3.2.200.0\">preserved</Customization>";
        let output = reconstruct(&rows(source), "HerbertChunkBytes")
            .unwrap()
            .unwrap();
        assert_eq!(output.bytes, source);
        assert_eq!(output.sequence, [10, 20]);
        assert!(reconstruct(&[], "HerbertChunkBytes").unwrap().is_none());
    }
    #[test]
    fn duplicate_sequence_truncated_length_and_corrupt_stream_fail() {
        let mut values = rows(b"test");
        values[1]["SeqNum"] = values[0]["SeqNum"].clone();
        assert!(reconstruct(&values, "HerbertChunkBytes").is_err());
        let mut values = rows(b"test");
        values[0]["HerbertChunkBytes"] = json!(1);
        assert!(reconstruct(&values, "HerbertChunkBytes").is_err());
        let mut values = rows(b"test");
        values[0]["Chunk"] = json!("broken");
        values[0]["HerbertChunkBytes"] = json!(12);
        assert!(reconstruct(&values, "HerbertChunkBytes").is_err());
    }
    #[test]
    fn utf16_length_accounts_for_surrogates_and_native_nulls() {
        assert!(
            verify_text_length(&json!({"Content":"😀","Bytes":"4"}), "Content", "Bytes").is_ok()
        );
        assert!(
            verify_text_length(&json!({"Content":null,"Bytes":null}), "Content", "Bytes").is_ok()
        );
        assert!(
            verify_text_length(&json!({"Content":"😀","Bytes":2}), "Content", "Bytes").is_err()
        );
        assert!(verify_text_length(&json!({"Content":"text"}), "Content", "Bytes").is_err());
    }

    #[test]
    fn native_uncompressed_xml_retains_unicode_and_original_line_endings() {
        let text = "<Legacy>😀\r\nsource</Legacy>";
        let rows = vec![
            json!({"SeqNum":20,"Chunk":"source</Legacy>","HerbertChunkBytes":"30"}),
            json!({"SeqNum":10,"Chunk":"<Legacy>😀\r\n","HerbertChunkBytes":"24"}),
        ];
        let output = reconstruct_with_xml(&rows, "HerbertChunkBytes")
            .unwrap()
            .unwrap();
        assert_eq!(output.bytes, text.as_bytes());
        assert_eq!(output.encoding, "xml-text");
        assert_eq!(output.sequence, [10, 20]);
        assert!(reconstruct(&rows, "HerbertChunkBytes").is_err());
    }
    #[test]
    fn native_base64_whitespace_is_permitted_and_corruption_remains_failure() {
        let mut values = rows(b"<Legacy/>");
        for row in &mut values {
            let text = format!(" \r\n{}\t", row["Chunk"].as_str().unwrap());
            row["HerbertChunkBytes"] = json!((text.encode_utf16().count() * 2).to_string());
            row["Chunk"] = json!(text);
        }
        assert_eq!(
            reconstruct_with_xml(&values, "HerbertChunkBytes")
                .unwrap()
                .unwrap()
                .bytes,
            b"<Legacy/>"
        );
        values[0]["Chunk"] = json!("broken");
        values[0]["HerbertChunkBytes"] = json!(12);
        assert!(reconstruct_with_xml(&values, "HerbertChunkBytes").is_err());
    }
    #[test]
    fn malformed_or_multiple_xml_roots_never_complete() {
        for text in [
            "<Legacy>",
            "<A/><B/>",
            "<A></B>",
            "<A/>trailing",
            "&amp;<A/>",
        ] {
            let values = vec![
                json!({"SeqNum":3,"Chunk":text,"HerbertChunkBytes":(text.encode_utf16().count()*2).to_string()}),
            ];
            assert!(
                reconstruct_with_xml(&values, "HerbertChunkBytes").is_err(),
                "{text}"
            );
        }
    }
}
