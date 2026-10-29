//! Decode native export bytes without changing BOMs, text or identity.
use base64::Engine;
use herbert_contract::Diagnostic;
use serde_json::Value;

pub struct DeclaredBlob {
    pub pointer: String,
    pub bytes: Vec<u8>,
    pub extension: String,
}
pub fn decode_bytes(encoded: &Value) -> Result<Vec<u8>, Diagnostic> {
    match encoded {
        Value::String(s) => base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| {
                Diagnostic::new(
                    "INVALID_NATIVE_BYTES",
                    "A declared Epicor byte field is not valid base64.",
                )
            }),
        Value::Array(values) => values
            .iter()
            .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                Diagnostic::new(
                    "INVALID_NATIVE_BYTES",
                    "An Epicor byte array contains a non-byte value.",
                )
            }),
        _ => Err(Diagnostic::new(
            "INVALID_NATIVE_BYTES",
            "A declared Epicor byte field has an unexpected representation.",
        )),
    }
}
pub fn declared_blobs(
    declarations: &[herbert_contract::BinaryExport],
    service: &str,
    method: &str,
    value: &Value,
) -> Result<Vec<DeclaredBlob>, herbert_contract::Diagnostic> {
    declared_blobs_where(declarations, service, method, value, |_| true)
}

pub(crate) fn declared_blobs_where(
    declarations: &[herbert_contract::BinaryExport],
    service: &str,
    method: &str,
    value: &Value,
    retained: impl Fn(&str) -> bool,
) -> Result<Vec<DeclaredBlob>, herbert_contract::Diagnostic> {
    fn visit(value: &Value, parts: &[String], pointer: String, out: &mut Vec<(String, Value)>) {
        if parts.is_empty() {
            if !value.is_null() {
                out.push((pointer, value.clone()));
            }
            return;
        }
        if parts[0] == "*" {
            match value {
                Value::Array(values) => {
                    for (i, v) in values.iter().enumerate() {
                        visit(v, &parts[1..], format!("{pointer}/{i}"), out)
                    }
                }
                Value::Object(values) => {
                    for (k, v) in values {
                        visit(
                            v,
                            &parts[1..],
                            format!("{pointer}/{}", k.replace('~', "~0").replace('/', "~1")),
                            out,
                        )
                    }
                }
                _ => {}
            }
        } else if let Some(v) = value.get(&parts[0]) {
            visit(
                v,
                &parts[1..],
                format!(
                    "{pointer}/{}",
                    parts[0].replace('~', "~0").replace('/', "~1")
                ),
                out,
            );
        }
    }
    use base64::Engine;
    let mut blobs = Vec::new();
    for declaration in declarations.iter().filter(|d| d.service == service && d.method == method) {
        let parts = declaration
            .pointer
            .split('/')
            .skip(1)
            .map(|p| p.replace("~1", "/").replace("~0", "~"))
            .collect::<Vec<_>>();
        let mut fields = Vec::new();
        visit(value, &parts, String::new(), &mut fields);
        for (pointer, encoded) in fields {
            if !retained(&pointer) { continue; }
            let bytes=match encoded{
                Value::String(s)=>base64::engine::general_purpose::STANDARD.decode(s).map_err(|_|herbert_contract::Diagnostic::new("INVALID_NATIVE_BYTES","A declared native byte field is not valid base64. Its original response is preserved."))?,
                Value::Array(values)=>values.into_iter().map(|v|v.as_u64().and_then(|n|u8::try_from(n).ok())).collect::<Option<Vec<_>>>().ok_or_else(||herbert_contract::Diagnostic::new("INVALID_NATIVE_BYTES","An Epicor byte array contains a non-byte value."))?,
                _=>return Err(herbert_contract::Diagnostic::new("INVALID_NATIVE_BYTES","A declared Epicor byte field has an unexpected representation.")),
            };
            let extension = if declaration.extension != "bin" {
                declaration.extension.clone()
            } else if bytes.starts_with(b"PK\x03\x04") {
                "zip".into()
            } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                "png".into()
            } else if bytes.starts_with(b"\xff\xd8\xff") {
                "jpg".into()
            } else if bytes.starts_with(b"MZ") {
                "dll".into()
            } else if std::str::from_utf8(&bytes).is_ok_and(|s| {
                s.trim_start_matches('\u{feff}')
                    .trim_start()
                    .starts_with('<')
            }) {
                "xml".into()
            } else {
                "bin".into()
            };
            blobs.push(DeclaredBlob {
                pointer,
                bytes,
                extension,
            });
        }
    }
    Ok(blobs)
}

// Binary columns are selected from metadata returned by the native dictionary,
// never from guest-provided encoding claims. Calculated text projections stay text.
pub fn query_binary_columns(query: &herbert_contract::QuerySpec) -> Vec<String> {
    query
        .fields
        .iter()
        .filter(|f| f.expression.is_none())
        .flat_map(|f| {
            query
                .tables
                .iter()
                .find(|t| t.alias == f.table)
                .into_iter()
                .flat_map(move |t| {
                    t.metadata
                        .iter()
                        .filter(move |c| {
                            (f.field == "*" || f.field == c.name)
                                && ["binary", "varbinary", "image"]
                                    .iter()
                                    .any(|ty| c.data_type.eq_ignore_ascii_case(ty))
                        })
                        .map(move |c| {
                            if f.field == "*" {
                                c.name.clone()
                            } else {
                                f.alias.clone()
                            }
                        })
                })
        })
        .collect()
}
pub fn query_blobs(columns: &[String], value: &Value) -> Result<Vec<DeclaredBlob>, Diagnostic> {
    if columns.is_empty() {
        return Ok(vec![]);
    }
    let rows = value["rows"].as_array().ok_or_else(|| {
        Diagnostic::new(
            "QUERY_RESULT_UNVERIFIED",
            "A binary query projection omitted its rows.",
        )
    })?;
    let mut blobs = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        for column in columns {
            let encoded = row.get(column).ok_or_else(|| Diagnostic::new("QUERY_BINARY_FIELD_MISSING", "An Epicor query omitted a selected binary column. Its raw response remains preserved."))?;
            if encoded.is_null() {
                continue;
            }
            blobs.push(DeclaredBlob {
                pointer: format!(
                    "/rows/{}/{}",
                    index,
                    column.replace('~', "~0").replace('/', "~1")
                ),
                bytes: decode_bytes(encoded)?,
                extension: "bin".into(),
            });
        }
    }
    Ok(blobs)
}
#[cfg(test)]
mod query_binary_tests {
    use super::*;
    use herbert_contract::*;
    use serde_json::json;
    #[test]
    fn native_binary_metadata_excludes_text_expressions_and_revision_tokens() {
        let q = QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: "CustomReportStoreHistory".into(),
                alias: "H".into(),
                company_field: None,
                metadata: vec![
                    QueryColumn {
                        name: "Content".into(),
                        data_type: "varbinary".into(),
                        format: "".into(),
                    },
                    QueryColumn {
                        name: "SysRevID".into(),
                        data_type: "timestamp".into(),
                        format: "".into(),
                    },
                ],
            }],
            fields: vec![
                QueryField {
                    table: "H".into(),
                    field: "*".into(),
                    alias: "*".into(),
                    expression: None,
                },
                QueryField {
                    table: "H".into(),
                    field: "Content".into(),
                    alias: "Length".into(),
                    expression: Some("DATALENGTH(H.Content)".into()),
                },
            ],
            joins: vec![],
            criteria: vec![],
            parameters: Default::default(),
            order: vec![],
            page_size: None,
            page: 1,
        };
        assert_eq!(query_binary_columns(&q), vec!["Content"]);
        let mut q = q;
        q.fields = vec![QueryField {
            table: "H".into(),
            field: "Content".into(),
            alias: "Archive".into(),
            expression: None,
        }];
        assert_eq!(query_binary_columns(&q), vec!["Archive"]);
    }
    #[test]
    fn native_binary_projection_preserves_base64_and_array_bytes() {
        let value = json!({"rows":[{"Body":[0,1,255]},{"Body":"AAH/"},{"Body":null}]});
        let blobs = query_blobs(&["Body".into()], &value).unwrap();
        assert_eq!(blobs.len(), 2);
        assert_eq!(blobs[0].bytes, vec![0, 1, 255]);
        assert_eq!(blobs[0].bytes, blobs[1].bytes);
        assert_eq!(blobs[1].pointer, "/rows/1/Body");
        assert_eq!(value["rows"][1]["Body"], "AAH/");
        assert!(query_blobs(&["Missing".into()], &value).is_err());
        assert!(query_blobs(&["Body".into()], &json!({"rows":[{"Body":[256]}]})).is_err());
    }
}

#[cfg(test)] mod plugin_declared_exports {
    use super::*;
    #[test] fn previously_unknown_method_uses_plugin_byte_fields() {
        let declarations = vec![herbert_contract::BinaryExport { service:"Vendor.BO.CustomSvc".into(), method:"ExportCustom".into(), pointer:"/returnObj/files/*/bytes".into(), extension:"custom".into() }];
        let response = serde_json::json!({"returnObj":{"files":[{"bytes":"YWJj"}]}});
        let blobs = declared_blobs(&declarations,"Vendor.BO.CustomSvc","ExportCustom",&response).unwrap();
        assert_eq!(blobs.len(),1); assert_eq!(blobs[0].bytes,b"abc"); assert_eq!(blobs[0].extension,"custom");
        assert!(declared_blobs(&[],"Vendor.BO.CustomSvc","ExportCustom",&response).unwrap().is_empty());
    }
}
