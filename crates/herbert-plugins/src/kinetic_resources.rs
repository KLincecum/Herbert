//! Follow authored MetaFX $ref documents using the native DBContentLoader keys.
//! Sources: HelperMethods.resolveJsonPointer and DBContentLoader.ReadAllText.
//! This acquires dependency bytes. It does not compose UI/runtime content.
use herbert_contract::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn push(state: &mut Value, diagnostic: Diagnostic) {
    state["resourceClosureFailed"] = json!(true);
    state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(diagnostic));
}
pub(crate) fn jsonc(text: &str) -> Result<Value, Diagnostic> {
    // Newtonsoft's native loader ignores comments and accepts trailing commas.
    // Preserve the original text. This projection is used only to find $ref.
    let bytes = text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut quoted = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            quoted = true;
            output.push(byte);
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            output.extend_from_slice(b"  ");
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' && bytes[index] != b'\r' {
                output.push(b' ');
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            output.extend_from_slice(b"  ");
            index += 2;
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    output.extend_from_slice(b"  ");
                    index += 2;
                    closed = true;
                    break;
                }
                output.push(if matches!(bytes[index], b'\r' | b'\n') {
                    bytes[index]
                } else {
                    b' '
                });
                index += 1;
            }
            if !closed {
                return Err(error(
                    "KINETIC_RESOURCE_JSON_UNVERIFIED",
                    "An authored JSONC document has an unterminated comment.",
                ));
            }
            continue;
        }
        output.push(byte);
        index += 1;
    }
    quoted = false;
    escaped = false;
    for index in 0..output.len() {
        let byte = output[index];
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        if byte == b'"' {
            quoted = true;
            continue;
        }
        if byte == b',' {
            let next = output[index + 1..]
                .iter()
                .copied()
                .find(|b| !b.is_ascii_whitespace());
            if matches!(next, Some(b']' | b'}')) {
                output[index] = b' ';
            }
        }
    }
    { let mut parser = serde_json::Deserializer::from_slice(&output); parser.disable_recursion_limit(); Value::deserialize(&mut parser).and_then(|value| parser.end().map(|_| value)) }.map_err(|_|error("KINETIC_RESOURCE_JSON_UNVERIFIED","An authored document is not a supported JSON/JSONC object; original bytes remain retained and dependency closure is not claimed.")).and_then(|value| if value.is_object() { Ok(value) } else { Err(error("KINETIC_RESOURCE_JSON_UNVERIFIED", "The Epicor JSONC loader requires an object document; original bytes are preserved.")) })
}
fn reference_path(current: &str, reference: &str) -> Result<Option<String>, Diagnostic> {
    let Some(fragment) = reference.find('#') else {
        return Ok(None);
    };
    // Native resolveJsonPointer returns {} for a missing document prefix.
    if fragment == 0 {
        return Ok(None);
    }
    let prefix = &reference[..fragment];
    let current = current.replace('\\', "/");
    if !reference.contains('.') {
        return Ok(Some(current));
    }
    let prefix = prefix.replace('\\', "/");
    if prefix.starts_with('/') || prefix.contains(':') {
        return Err(error(
            "KINETIC_RESOURCE_PATH_UNVERIFIED",
            "An authored reference names an absolute/external path outside the Epicor MetaUI row contract.",
        ));
    }
    let directory = current
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .ok_or_else(|| {
            error(
                "KINETIC_RESOURCE_PATH_UNVERIFIED",
                "An authored file lacks its MetaUI-relative directory.",
            )
        })?;
    let joined = format!("{directory}/{prefix}");
    let joined = if joined.ends_with("/pages/patch.jsonc") {
        joined.replace("/pages", "")
    } else {
        joined
    };
    // DBContentLoader normalizes an absolute Apps/MetaUI path, then removes
    // the root's character count. It does not require that root as a prefix.
    // Keep the MetaUI component to reproduce references through Apps/shared.
    // The returned value is a database key, never a local filesystem path.
    let mut parts = vec!["MetaUI"];
    for component in joined.split('/') {
        match component {
            "" | "." => (),
            ".." => {
                if parts.pop().is_none() {
                    return Err(error(
                        "KINETIC_RESOURCE_PATH_UNVERIFIED",
                        "The Epicor database key depends on a server directory above Apps and cannot be verified locally.",
                    ));
                }
            }
            part => parts.push(part),
        }
    }
    let normalized = parts.join("\\");
    let native_key = String::from_utf16(
        &normalized
            .encode_utf16()
            .skip("MetaUI".len())
            .collect::<Vec<_>>(),
    )
    .map_err(|_| {
        error(
            "KINETIC_RESOURCE_PATH_UNVERIFIED",
            "The Epicor key prefix ends inside a Unicode character.",
        )
    })?;
    let native_key = native_key.trim_start_matches('\\');
    let parts: Vec<_> = native_key.split('\\').collect();
    if parts.len() < 2
        || parts
            .iter()
            .any(|p| p.is_empty() || p.eq_ignore_ascii_case("cache"))
    {
        return Err(error(
            "KINETIC_RESOURCE_PATH_UNVERIFIED",
            "A reference does not name an authored MetaUI document.",
        ));
    }
    Ok(Some(parts.join("\\")))
}
pub(crate) fn scan(
    value: &Value,
    current: &str,
    out: &mut Vec<(String, String)>,
    errors: &mut Vec<Diagnostic>,
) {
    match value {
        Value::Object(fields) => {
            if let Some(reference) = fields.get("$ref") {
                if let Some(reference) = reference.as_str() {
                    match reference_path(current, reference) {
                        Ok(Some(path)) => out.push((reference.into(), path)),
                        Ok(None) => (),
                        Err(mut error) => {
                            // Keep the exact source and reference. A generic path
                            // warning cannot identify the affected saved file.
                            error.message = format!(
                                "{} Source file: {current}. Reference: {reference}",
                                error.message
                            );
                            errors.push(error);
                        }
                    }
                } else {
                    errors.push(error(
                        "KINETIC_RESOURCE_REFERENCE_INVALID",
                        "An authored $ref value is not a string.",
                    ));
                }
            }
            for value in fields.values() {
                scan(value, current, out, errors);
            }
        }
        Value::Array(values) => {
            for value in values {
                scan(value, current, out, errors);
            }
        }
        _ => (),
    }
}
fn enqueue(state: &mut Value, current: &str, content: &str) {
    let value = match jsonc(content) {
        Ok(value) => value,
        Err(error) => {
            push(state, error);
            return;
        }
    };
    let mut edges = Vec::new();
    let mut errors = Vec::new();
    scan(&value, current, &mut edges, &mut errors);
    for error in errors {
        push(state, error);
    }
    for (reference, path) in edges {
        state["resourceReferences"]
            .as_array_mut()
            .unwrap()
            .push(json!({"sourcePath":current,"reference":reference,"targetPath":path}));
        let known = state["resourceKnown"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().is_some_and(|p| p.eq_ignore_ascii_case(&path)));
        if !known {
            state["resourceKnown"]
                .as_array_mut()
                .unwrap()
                .push(json!(path));
            state["resourceQueue"]
                .as_array_mut()
                .unwrap()
                .push(json!(path));
        }
    }
}
pub fn start(request: GuestRequest, mut state: Value) -> GuestReply {
    state["phase"] = json!("resourceFiles");
    state["resourceQueue"] = json!([]);
    state["resourceIndex"] = json!(0);
    state["resourceFiles"] = json!([]);
    state["resourceReferences"] = json!([]);
    state["resourceKnown"] = json!([]);
    state["resourceClosureFailed"] = json!(false);
    state["resourceBodyRead"] = json!(false);
    let rows = state["persistedFiles"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for row in &rows {
        if row["Company"] == "" && row["CGCCode"] == "" {
            if let Some(path) = row["Key2"].as_str() {
                state["resourceKnown"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(path.replace('/', "\\")));
            }
        }
    }
    for row in &rows {
        if let (Some(path), Some(content)) = (row["Key2"].as_str(), row["Content"].as_str()) {
            enqueue(&mut state, path, content);
        } else {
            push(
                &mut state,
                error(
                    "KINETIC_RESOURCE_SOURCE_UNVERIFIED",
                    "An authored app file has no exact path and full body for dependency extraction.",
                ),
            );
        }
    }
    next(request, state)
}
fn next(request: GuestRequest, mut state: Value) -> GuestReply {
    let index = state["resourceIndex"].as_u64().unwrap_or(0) as usize;
    let Some(path) = state["resourceQueue"]
        .get(index)
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        state["resourceClosureComplete"] = json!(state["resourceClosureFailed"] == false);
        return crate::kinetic::finish(request, state);
    };
    // This seven-part native key is an exact lookup. Discovery paging would
    // require ordering and could hide a duplicate key instead of detecting it.
    let root = path.split('\\').next().unwrap_or("");
    let mut params = BTreeMap::from([
        ("company".into(), json!("")),
        ("product".into(), json!("EP")),
        ("kind".into(), json!("KNTCMetaUI")),
        ("root".into(), json!(root)),
        ("path".into(), json!(path)),
        ("authored".into(), json!("")),
        ("cgc".into(), json!("")),
    ]);
    let mut criteria: Vec<QueryCriterion> = [
        ("Company", "company"),
        ("ProductID", "product"),
        ("TypeCode", "kind"),
        ("Key1", "root"),
        ("Key2", "path"),
        ("Key3", "authored"),
        ("CGCCode", "cgc"),
    ]
    .into_iter()
    .map(|(field, parameter)| QueryCriterion {
        table: "Resource".into(),
        field: field.into(),
        operator: "=".into(),
        parameter: parameter.into(),
    })
    .collect();
    // Classify by metadata before requesting a body: host receipts retain query
    // responses before this plugin sees them. Filtering after acquisition leaks
    // explicitly system-owned text into those receipts.
    let fields = if state["resourceBodyRead"] == true {
        params.insert("nonSystem".into(), json!(false));
        criteria.push(QueryCriterion {
            table: "Resource".into(),
            field: "SystemFlag".into(),
            operator: "=".into(),
            parameter: "nonSystem".into(),
        });
        vec![
            QueryField {
                table: "Resource".into(),
                field: "*".into(),
                alias: "*".into(),
                expression: None,
            },
            QueryField {
                table: "Resource".into(),
                field: "Content".into(),
                alias: "HerbertContentBytes".into(),
                expression: Some(
                    "CONVERT(nvarchar(max), DATALENGTH(CONVERT(nvarchar(max), Resource.Content)))"
                        .into(),
                ),
            },
        ]
    } else {
        [
            "Company",
            "ProductID",
            "TypeCode",
            "Key1",
            "Key2",
            "Key3",
            "CGCCode",
            "SysRowID",
            "SystemFlag",
        ]
        .into_iter()
        .map(|field| QueryField {
            table: "Resource".into(),
            field: field.into(),
            alias: field.into(),
            expression: None,
        })
        .collect()
    };
    GuestReply::Action {
        action: HostAction::SharedQuery {
            query: QuerySpec {
                tables: vec![QueryTable {
                    schema: "Ice".into(),
                    table: "XXXDef".into(),
                    alias: "Resource".into(),
                    company_field: None,
                    metadata: vec![],
                }],
                fields,
                joins: vec![],
                criteria,
                parameters: params,
                order: vec![],
                page_size: None,
                page: 1,
            },
        },
        state,
    }
}
pub fn handle(mut request: GuestRequest) -> GuestReply {
    let mut state = request.state.clone();
    let index = state["resourceIndex"].as_u64().unwrap_or(0) as usize;
    let path = state["resourceQueue"][index]
        .as_str()
        .unwrap_or("")
        .to_owned();
    if let Some(error) = request.error.take() {
        push(&mut state, error);
    } else if let Some(rows) = request.response.as_ref().and_then(|r| r["rows"].as_array()) {
        // This is an exact seven-column native primary key, not a result limit.
        if rows.len() != 1 {
            push(
                &mut state,
                error(
                    "KINETIC_RESOURCE_IDENTITY_UNVERIFIED",
                    format!(
                        "Referenced document {path} has {} global Epicor source rows; tenant-specific or filesystem fallback is not established.",
                        rows.len()
                    ),
                ),
            );
        } else {
            let row = &rows[0];
            let root = path.split('\\').next().unwrap_or("");
            let valid = row["Company"] == ""
                && row["ProductID"] == "EP"
                && row["TypeCode"] == "KNTCMetaUI"
                && row["Key1"]
                    .as_str()
                    .is_some_and(|v| v.eq_ignore_ascii_case(root))
                && row["Key2"]
                    .as_str()
                    .is_some_and(|v| v.replace('/', "\\").eq_ignore_ascii_case(&path))
                && row["Key3"] == ""
                && row["CGCCode"] == ""
                && row["SysRowID"].as_str().is_some_and(|v| !v.is_empty());
            if !valid {
                push(
                    &mut state,
                    error(
                        "KINETIC_RESOURCE_IDENTITY_MISMATCH",
                        "The shared resource read returned a different Epicor ID.",
                    ),
                );
            }
            if valid && state["resourceBodyRead"] != true {
                if crate::system_artifacts::marked(row) {
                    for reference in state["resourceReferences"].as_array_mut().unwrap() {
                        if reference["targetPath"]
                            .as_str()
                            .is_some_and(|target| target.eq_ignore_ascii_case(&path))
                        {
                            reference["excludedSystem"] = json!(true);
                        }
                    }
                    state["resourceIndex"] = json!(index + 1);
                    return next(request, state);
                }
                if row["SystemFlag"] == false {
                    state["resourceBodyRead"] = json!(true);
                    return next(request, state);
                }
                push(
                    &mut state,
                    error(
                        "SYSTEM_POLICY_UNVERIFIED",
                        "The referenced document metadata omitted its system ownership marker.",
                    ),
                );
                state["resourceIndex"] = json!(index + 1);
                return next(request, state);
            }
            let content = row["Content"].as_str();
            let measured = row["HerbertContentBytes"]
                .as_u64()
                .or_else(|| row["HerbertContentBytes"].as_str()?.parse().ok());
            if content.map(|s| s.encode_utf16().count() as u64 * 2) != measured || content.is_none()
            {
                push(
                    &mut state,
                    error(
                        "KINETIC_RESOURCE_LENGTH_MISMATCH",
                        "The referenced body is absent or differs from its saved UTF-16 length.",
                    ),
                );
            } else if valid {
                enqueue(&mut state, &path, content.unwrap());
            }
            state["resourceFiles"]
                .as_array_mut()
                .unwrap()
                .push(row.clone());
        }
    } else {
        push(
            &mut state,
            error(
                "KINETIC_RESOURCE_ROWS_MISSING",
                "The referenced Epicor document query omitted its rows.",
            ),
        );
    }
    state["resourceIndex"] = json!(index + 1);
    state["resourceBodyRead"] = json!(false);
    next(request, state)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comments_trailing_commas_and_literal_slashes_keep_references() {
        let value=jsonc("{/*comment*/\"imports\":{\"layout\":{\"$ref\":\"../shared/layout/a.jsonc#/layout\",},},\"url\":\"https://a//b\",}").unwrap();
        let mut edges = vec![];
        let mut errors = vec![];
        scan(&value, "App/layout.jsonc", &mut edges, &mut errors);
        assert!(errors.is_empty());
        assert_eq!(edges[0].1, "shared\\layout\\a.jsonc");
        assert_eq!(value["url"], "https://a//b");
    }
    #[test]
    fn native_pages_patch_and_parent_path_resolution() {
        assert_eq!(
            reference_path("App/pages/Detail.jsonc", "patch.jsonc#/patch").unwrap(),
            Some("App\\patch.jsonc".into())
        );
        assert_eq!(
            reference_path("App/pages/Detail.jsonc", "../../shared/a.jsonc#/x").unwrap(),
            Some("shared\\a.jsonc".into())
        );
        assert!(reference_path("App/layout.jsonc", "../../outside.jsonc#/x").is_err());
        assert_eq!(
            reference_path(
                "App/layout.jsonc",
                "../../shared/combo/SysPrinter.PrinterCombo/default/combo.jsonc#/combo"
            )
            .unwrap(),
            Some("combo\\SysPrinter.PrinterCombo\\default\\combo.jsonc".into())
        );
        assert_eq!(reference_path("App/layout.jsonc", "#/here").unwrap(), None);
    }
    #[test]
    fn cycles_are_retained_without_unbounded_reads() {
        let mut state = json!({"errors":[],"resourceClosureFailed":false,"resourceReferences":[],"resourceKnown":["App\\layout.jsonc"],"resourceQueue":[]});
        enqueue(
            &mut state,
            "App/layout.jsonc",
            "{\"$ref\":\"../shared/a.jsonc#/x\"}",
        );
        enqueue(
            &mut state,
            "shared/a.jsonc",
            "{\"$ref\":\"../App/layout.jsonc#/x\"}",
        );
        assert_eq!(state["resourceQueue"].as_array().unwrap().len(), 1);
        assert_eq!(state["resourceReferences"].as_array().unwrap().len(), 2);
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "kinetic-applications-and-layers".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "kinetic-applications-and-layers".into(),
                kind: "MetaFXApplication".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([("ViewId".into(), "App".into())]),
            }),
            cursor: None,
            page_size: Some(200),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    fn row(path: &str, content: &str) -> Value {
        json!({"Company":"","ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":path.split('\\').next().unwrap(),"Key2":path,"Key3":"","CGCCode":"","SysRowID":path,"SystemFlag":false,"Content":content,"HerbertContentBytes":content.encode_utf16().count()*2})
    }
    #[test]
    fn exact_dependency_is_acquired_measured_and_cycle_closes() {
        let mut request = request();
        let source = row("App\\layout.jsonc", r##"{"$ref":"../shared/a.jsonc#/x"}"##);
        let state = json!({"errors":[],"content":{"export":{}},"persistedFiles":[source]});
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = start(request.clone(), state)
        else {
            panic!()
        };
        assert_eq!(query.parameters["path"], "shared\\a.jsonc");
        assert_eq!(query.parameters["company"], "");
        assert_eq!(query.page_size, None);
        assert!(
            !query
                .fields
                .iter()
                .any(|f| f.field == "*" || f.field == "Content")
        );
        request.state = state;
        request.response =
            Some(json!({"rows":[row("shared\\a.jsonc",r##"{"$ref":"../App/layout.jsonc#/x"}"##)]}));
        let GuestReply::Action {
            action: HostAction::SharedQuery { query },
            state,
        } = handle(request.clone())
        else {
            panic!()
        };
        assert_eq!(query.parameters["nonSystem"], false);
        request.state = state;
        let GuestReply::Definition {
            completeness,
            definition,
            diagnostics,
            ..
        } = handle(request)
        else {
            panic!()
        };
        assert!(diagnostics.is_empty(), "{:?}", diagnostics);
        assert_eq!(completeness, Completeness::Collected);
        assert_eq!(definition["resourceFiles"].as_array().unwrap().len(), 1);
        assert_eq!(
            definition["resourceReferences"].as_array().unwrap().len(),
            2
        );
    }
    #[test]
    fn missing_or_short_dependency_cannot_complete() {
        for missing in [true, false] {
            let mut request = request();
            let state = json!({"errors":[],"content":{"export":{}},"persistedFiles":[row("App\\layout.jsonc",r##"{"$ref":"../shared/a.jsonc#/x"}"##)]});
            let GuestReply::Action { state, .. } = start(request.clone(), state) else {
                panic!()
            };
            request.state = state;
            let mut dependency = row("shared\\a.jsonc", "{}");
            dependency["HerbertContentBytes"] = json!(99);
            request.response = Some(json!({"rows":if missing{vec![]}else{vec![dependency]}}));
            if !missing {
                let GuestReply::Action { state, .. } = handle(request.clone()) else {
                    panic!()
                };
                request.state = state;
            }
            let GuestReply::Definition {
                completeness,
                diagnostics,
                ..
            } = handle(request)
            else {
                panic!()
            };
            assert_eq!(completeness, Completeness::Partial);
            assert!(diagnostics.iter().any(|d| d.code
                == if missing {
                    "KINETIC_RESOURCE_IDENTITY_UNVERIFIED"
                } else {
                    "KINETIC_RESOURCE_LENGTH_MISMATCH"
                }));
        }
    }
    #[test]
    fn system_reference_stops_after_metadata_without_body_retention() {
        let mut request = request();
        let state = json!({"errors":[],"content":{"export":{}},"persistedFiles":[row("App\\layout.jsonc",r##"{"$ref":"../shared/a.jsonc#/x"}"##)]});
        let GuestReply::Action { state, .. } = start(request.clone(), state) else {
            panic!()
        };
        request.state = state;
        let mut system = row("shared\\a.jsonc", "not fetched");
        system["SystemFlag"] = json!(true);
        system.as_object_mut().unwrap().remove("Content");
        system
            .as_object_mut()
            .unwrap()
            .remove("HerbertContentBytes");
        request.response = Some(json!({"rows":[system]}));
        let GuestReply::Definition {
            definition,
            diagnostics,
            ..
        } = handle(request)
        else {
            panic!("system body requested")
        };
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(definition["resourceFiles"], json!([]));
        assert_eq!(definition["resourceReferences"][0]["excludedSystem"], true);
    }
}
