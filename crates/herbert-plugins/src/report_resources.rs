//! Read-only RDL/subreport dependency acquisition. Never use ExportSsrsReports:
//! native CollectReports invokes EnsureReportValidated and can deploy reports.
use herbert_contract::{Diagnostic, FileRef, GuestRequest, HostAction};
use serde_json::{Value, json};
fn issue(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new(code, message)
}
fn push(state: &mut Value, error: Diagnostic) {
    state["rdlFailed"] = json!(true);
    state["errors"].as_array_mut().unwrap().push(json!(error));
}
fn file(response: &Value) -> Option<FileRef> {
    response
        .pointer("/returnObj/nativeFile")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}
fn display(path: &str) -> Result<String, Diagnostic> {
    let path = path.trim().trim_start_matches('/');
    if path.contains(':')
        || path.contains('\\')
        || !path
            .split('/')
            .next()
            .is_some_and(|p| p.eq_ignore_ascii_case("reports"))
    {
        return Err(issue(
            "REPORT_RDL_PATH_UNVERIFIED",
            "A report path is outside the native display-path contract.",
        ));
    }
    let path = path
        .split('/')
        .filter(|p| *p != ".")
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("/");
    if path.ends_with('/') || !path.contains('/') {
        return Err(issue(
            "REPORT_RDL_PATH_UNVERIFIED",
            "A report reference does not name a complete native report path.",
        ));
    }
    Ok(path)
}
/// Normalize a dependency within the native report root. A custom style can
/// reference a shipped subreport; the native exporter includes that dependency.
pub fn report_path(path: &str) -> Result<String, Diagnostic> {
    let path = display(path)?;
    let mut parts = Vec::new();
    for part in path.split('/') {
        if part == ".." {
            if parts.pop().is_none() {
                return Err(issue(
                    "REPORT_LAYOUT_OWNERSHIP_UNVERIFIED",
                    "The report path escapes its native root.",
                ));
            }
        } else {
            parts.push(part);
        }
    }
    if parts.len() < 2
        || !parts[0].eq_ignore_ascii_case("reports")
        || parts[1..].iter().any(|part| part.is_empty())
    {
        return Err(issue(
            "REPORT_LAYOUT_OWNERSHIP_UNVERIFIED",
            "The reference is outside Epicor's native report root.",
        ));
    }
    Ok(parts.join("/"))
}
/// Retain native custom-path classification for legacy discovery callers.
pub fn custom_path(path: &str) -> Result<String, Diagnostic> {
    let path = report_path(path)?;
    if path
        .split('/')
        .nth(1)
        .is_some_and(|p| p.eq_ignore_ascii_case("CustomReports"))
    {
        Ok(path)
    } else {
        Err(issue(
            "REPORT_LAYOUT_OWNERSHIP_UNVERIFIED",
            "The path is not a custom report location.",
        ))
    }
}
fn subreport_path(parent: &str, child: &str) -> Result<String, Diagnostic> {
    if child.trim().is_empty() || child.trim_start().starts_with('=') {
        return Err(issue(
            "REPORT_RDL_DYNAMIC_REFERENCE",
            "A subreport name is empty or expression-based; its original RDL is retained but no static body identity is established.",
        ));
    }
    let parent = format!("/{}", display(parent)?);
    let directory = parent.rsplit_once('/').unwrap().0;
    // Exact SsrsPath.GetSubreportPath: only an initial '..' selects GetAbsolutePath.
    // Otherwise Combine appends the slash-normalized reference to the parent.
    let combined = if child.split('/').next() == Some("..") {
        let mut parts: Vec<_> = directory.split('/').map(str::to_owned).collect();
        for part in child.split('/') {
            if part == ".." {
                if parts.pop().is_none() {
                    return Err(issue(
                        "REPORT_RDL_PATH_UNVERIFIED",
                        "A subreport path escapes its native report root.",
                    ));
                }
            } else {
                parts.push(part.into())
            }
        }
        parts.join("/")
    } else {
        format!("{directory}/{}", child.trim_start_matches('/'))
    };
    display(&combined)
}
fn children(node: &Value) -> impl Iterator<Item = &Value> {
    node["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("element"))
}
fn text(node: &Value) -> Option<String> {
    let mut text = String::new();
    for part in node["content"].as_array()? {
        if part.get("element").is_some() {
            return None;
        }
        text.push_str(part.get("text")?.as_str()?)
    }
    Some(text)
}
fn child_text(node: &Value, name: &str, ns: &Value) -> Option<String> {
    let candidates: Vec<_> = children(node)
        .filter(|child| child["name"] == name && &child["namespace"] == ns)
        .collect();
    if candidates.len() != 1 {
        return None;
    }
    text(candidates[0])
}
fn walk(
    node: &Value,
    ns: &Value,
    subreports: &mut Vec<String>,
    unresolved: &mut Vec<Value>,
    errors: &mut Vec<Diagnostic>,
) {
    if &node["namespace"] != ns {
        return;
    }
    match node["name"].as_str().unwrap_or("") {
        "Subreport" => match child_text(node, "ReportName", ns) {
            Some(name) => subreports.push(name),
            None => errors.push(issue(
                "REPORT_RDL_REFERENCE_INVALID",
                "A subreport has no single complete ReportName value.",
            )),
        },
        "DataSourceReference" | "SharedDataSetReference" => {
            let kind = node["name"].as_str().unwrap();
            unresolved.push(json!({"kind":kind,"value":text(node)}));
            errors.push(issue("REPORT_RDL_SHARED_RESOURCE_UNVERIFIED",format!("The RDL contains {kind}; its referenced source body has no established read contract.")));
        }
        "Image" if child_text(node, "Source", ns).as_deref() == Some("External") => {
            unresolved.push(json!({"kind":"ExternalImage","value":child_text(node,"Value",ns)}));
            errors.push(issue(
                "REPORT_RDL_EXTERNAL_IMAGE_UNVERIFIED",
                "The RDL names an external image; no source-owned body route has been established.",
            ));
        }
        "CodeModule" => {
            unresolved.push(json!({"kind":"CodeModule","value":text(node)}));
            errors.push(issue("REPORT_RDL_CODE_MODULE_UNVERIFIED","The RDL names an external code module whose original bytes have not been acquired."));
        }
        _ => (),
    }
    for child in children(node) {
        walk(child, ns, subreports, unresolved, errors)
    }
}
fn dependencies(xml: &Value, path: &str) -> (Vec<(String, String)>, Vec<Value>, Vec<Diagnostic>) {
    let mut errors = vec![];
    let mut names = vec![];
    let mut unresolved = vec![];
    let ns = &xml["namespace"];
    if xml["name"] != "Report"
        || !ns.as_str().is_some_and(|ns| {
            ns.starts_with("http://schemas.microsoft.com/sqlserver/reporting/")
                && ns.ends_with("/reportdefinition")
        })
    {
        return (
            vec![],
            vec![],
            vec![issue(
                "REPORT_RDL_DOCUMENT_UNVERIFIED",
                "The acquired XML is not a native SQL Server report definition document.",
            )],
        );
    }
    walk(xml, ns, &mut names, &mut unresolved, &mut errors);
    let mut edges = vec![];
    for name in names {
        match subreport_path(path, &name) {
            Ok(path) => match report_path(&path) {
                Ok(path) => edges.push((name, path)),
                Err(error) => {
                    unresolved.push(json!({"kind":"Subreport","value":name,"resolvedPath":path}));
                    errors.push(error);
                }
            },
            Err(error) => {
                unresolved.push(json!({"kind":"Subreport","value":name}));
                errors.push(error)
            }
        }
    }
    (edges, unresolved, errors)
}
pub fn start(request: &mut GuestRequest) -> Option<HostAction> {
    request.state["rdlQueue"] = json!([]);
    request.state["rdlDocuments"] = json!([]);
    request.state["rdlEdges"] = json!([]);
    request.state["rdlIndex"] = json!(0);
    request.state["rdlFailed"] = json!(false);
    let Some(styles) = request.state["definition"]["ReportStyle"]
        .as_array()
        .cloned()
    else {
        push(
            &mut request.state,
            issue(
                "REPORT_RDL_STYLE_SOURCE_MISSING",
                "The report definition omitted its style array.",
            ),
        );
        return next(request);
    };
    for (index, style) in styles.iter().enumerate() {
        if style["RptTypeID"] != "SSRS" {
            push(
                &mut request.state,
                issue(
                    "REPORT_LAYOUT_TYPE_UNVERIFIED",
                    "A report style uses a layout type outside the established SSRS authored-body contract.",
                ),
            );
            continue;
        }
        let Some(path) = style["PrintProgram"].as_str() else {
            push(
                &mut request.state,
                issue(
                    "REPORT_RDL_PATH_UNVERIFIED",
                    "An SSRS style omitted PrintProgram.",
                ),
            );
            continue;
        };
        let paths: Vec<_> = path.split(',').filter(|path| !path.is_empty()).collect();
        if paths.is_empty() {
            push(
                &mut request.state,
                issue(
                    "REPORT_RDL_PATH_UNVERIFIED",
                    "An SSRS style has no report location.",
                ),
            );
        }
        for (location, path) in paths.iter().enumerate() {
            let path = match custom_path(path) {
                Ok(path) => path,
                Err(error) => {
                    push(&mut request.state, error);
                    continue;
                }
            };
            let label = if paths.len() > 1 {
                format!("ExportSsrsRdl/{index}/{location}")
            } else {
                format!("ExportSsrsRdl/{index}")
            };
            let response = &request.state["supplements"][label];
            let Some(source) = file(response) else {
                push(
                    &mut request.state,
                    issue(
                        "REPORT_RDL_SOURCE_MISSING",
                        format!(
                            "Style {} has no preserved native RDL for {path}.",
                            style["StyleNum"]
                        ),
                    ),
                );
                continue;
            };
            let existing = request.state["rdlQueue"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| {
                    entry["path"]
                        .as_str()
                        .is_some_and(|p| p.eq_ignore_ascii_case(&path))
                })
                .cloned();
            if let Some(existing) = existing {
                if existing["file"]["sha256"] != source.sha256
                    || existing["file"]["bytes"] != source.bytes
                {
                    push(
                        &mut request.state,
                        issue(
                            "REPORT_RDL_SOURCE_CHANGED",
                            "Two styles resolved to one report path but returned different retained RDL bytes; both exports remain preserved.",
                        ),
                    );
                }
                continue;
            }
            request.state["rdlQueue"]
                .as_array_mut()
                .unwrap()
                .push(json!({"path":path,"file":source}));
        }
    }
    next(request)
}
/// Read each distinct layout once for a complete bulk style page. Keep the
/// existing dependency parser and cycle detection for its subreports.
pub fn start_page(request: &mut GuestRequest, styles: &[Value]) -> Option<HostAction> {
    request.state["errors"] = json!([]);
    request.state["supplements"] = json!({});
    request.state["rdlQueue"] = json!([]);
    request.state["rdlDocuments"] = json!([]);
    request.state["rdlEdges"] = json!([]);
    request.state["rdlIndex"] = json!(0);
    request.state["rdlFailed"] = json!(false);
    for style in styles {
        if style["RptTypeID"] != "SSRS" {
            push(
                &mut request.state,
                issue(
                    "REPORT_LAYOUT_TYPE_UNVERIFIED",
                    format!(
                        "Report {} style {} uses {}; its stored settings are preserved, but its layout reader is not qualified.",
                        style["ReportID"], style["StyleNum"], style["RptTypeID"]
                    ),
                ),
            );
            continue;
        }
        let Some(paths) = style["PrintProgram"].as_str() else {
            push(
                &mut request.state,
                issue(
                    "REPORT_RDL_PATH_UNVERIFIED",
                    "An SSRS style omitted its layout path.",
                ),
            );
            continue;
        };
        for path in paths.split(',') {
            match report_path(path) {
                Ok(path) => {
                    if !request.state["rdlQueue"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| {
                            entry["path"]
                                .as_str()
                                .is_some_and(|p| p.eq_ignore_ascii_case(&path))
                        })
                    {
                        request.state["rdlQueue"]
                            .as_array_mut()
                            .unwrap()
                            .push(json!({"path":path}));
                    }
                }
                Err(error) => push(&mut request.state, error),
            }
        }
    }
    next(request)
}
fn next(request: &mut GuestRequest) -> Option<HostAction> {
    // Reuse compact dependency results across ready styles. Native file bodies
    // remain in the host; a repeated style needs no second XML projection.
    loop {
        let index = request.state["rdlIndex"].as_u64().unwrap_or(0) as usize;
        let path = request.state["rdlQueue"][index]["path"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        if path.is_empty() {
            break;
        }
        if request.state["layoutPageReceived"] == true {
            if let Some(file) = request.state["layoutBodies"].get(path.to_lowercase()).cloned() {
                request.state["rdlQueue"][index]["file"] = file;
            } else {
                // The bulk reader already attempted every static dependency.
                // Do not turn a missing row into per-layout REST retries.
                push(&mut request.state, issue("REPORT_RDL_SOURCE_MISSING",
                    format!("Layout {path} was not returned in the bulk download.")));
                request.state["rdlIndex"] = json!(index + 1);
                continue;
            }
        }
        let cached = request.state["resolvedLayouts"].get(&path).cloned();
        let Some(cached) = cached else {
            break;
        };
        request.state["rdlQueue"][index]["file"] = cached["document"]["file"].clone();
        request.state["rdlDocuments"]
            .as_array_mut()
            .unwrap()
            .push(cached["document"].clone());
        for error in cached["errors"].as_array().into_iter().flatten() {
            if let Ok(error) = serde_json::from_value(error.clone()) {
                push(&mut request.state, error);
            }
        }
        for edge in cached["edges"].as_array().into_iter().flatten() {
            request.state["rdlEdges"]
                .as_array_mut()
                .unwrap()
                .push(edge.clone());
            let target = edge["target"].as_str().unwrap();
            if !request.state["rdlQueue"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| {
                    e["path"]
                        .as_str()
                        .is_some_and(|p| p.eq_ignore_ascii_case(target))
                })
            {
                request.state["rdlQueue"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"path":target}));
            }
        }
        request.state["rdlIndex"] = json!(index + 1);
    }
    let index = request.state["rdlIndex"].as_u64().unwrap_or(0) as usize;
    let Some(entry) = request.state["rdlQueue"].get(index).cloned() else {
        request.state["phase"] = json!("reportRdlComplete");
        request.state["supplements"]["reportDependencyReconciliation"] = json!({"documents":request.state["rdlDocuments"],"edges":request.state["rdlEdges"],"complete":request.state["rdlFailed"]==false,"snapshot":"nonAtomic"});
        return None;
    };
    if let Some(file) = entry
        .get("file")
        .cloned()
        .and_then(|v| serde_json::from_value::<FileRef>(v).ok())
    {
        request.state["phase"] = json!("reportRdlParse");
        request.state["_progress"] = json!({"request":format!("Checking subreports in {}", entry["path"].as_str().unwrap_or("layout")),
            "response":"Layout checked; preparing ready exports"});
        Some(HostAction::ParseXml { file })
    } else {
        request.state["phase"] = json!("reportRdlRead");
        request.state["_progress"] = json!({"request":format!("Downloading layout {}", entry["path"].as_str().unwrap_or("layout")),
            "response":"Layout received; checking dependencies"});
        Some(HostAction::SharedNativeRead {
            service: "Ice.BO.ReportSvc".into(),
            method: "ExportSsrsRdl".into(),
            arguments: json!({"reportPath":entry["path"]}),
            export: true,
        })
    }
}
pub fn complete(request: &mut GuestRequest) -> Option<HostAction> {
    let index = request.state["rdlIndex"].as_u64().unwrap_or(0) as usize;
    let entry = request.state["rdlQueue"][index].clone();
    if let Some(mut error) = request.error.take() {
        error.message = format!(
            "Layout {}: {}",
            entry["path"].as_str().unwrap_or("unknown"),
            error.message
        );
        push(&mut request.state, error)
    } else if let Some(response) = request.response.take() {
        if request.state["phase"] == "reportRdlRead" {
            request.state["supplements"][format!("ExportSubreport/{index}")] = response.clone();
            if let Some(file) = file(&response) {
                request.state["rdlQueue"][index]["file"] = json!(file);
                return next(request);
            } else {
                push(
                    &mut request.state,
                    issue(
                        "REPORT_RDL_SOURCE_MISSING",
                        "The subreport read omitted its preserved native file.",
                    ),
                );
            }
        } else if response["sourceFile"] != entry["file"] {
            push(
                &mut request.state,
                issue(
                    "REPORT_RDL_SOURCE_MISMATCH",
                    "The XML projection does not belong to this report's recorded native file.",
                ),
            );
        } else {
            let path = entry["path"].as_str().unwrap_or("");
            let (edges, unresolved, errors) = dependencies(&response["parsedXml"], path);
            let document = json!({"path":path,"file":entry["file"],"unresolved":unresolved,
                "embeddedCode":child_text(&response["parsedXml"],"Code",&response["parsedXml"]["namespace"])});
            if !request.state["resolvedLayouts"].is_object() {
                request.state["resolvedLayouts"] = json!({});
            }
            request.state["resolvedLayouts"][path] = json!({"document":document,"errors":errors,
                "edges":edges.iter().map(|(name,target)| json!({"source":path,"reference":name,"target":target})).collect::<Vec<_>>()});
            for error in errors {
                push(&mut request.state, error)
            }
            request.state["rdlDocuments"]
                .as_array_mut()
                .unwrap()
                .push(document);
            for (name, target) in edges {
                request.state["rdlEdges"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"source":path,"reference":name,"target":target}));
                if !request.state["rdlQueue"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| {
                        entry["path"]
                            .as_str()
                            .is_some_and(|p| p.eq_ignore_ascii_case(&target))
                    })
                {
                    request.state["rdlQueue"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"path":target}));
                }
            }
        }
    } else {
        push(
            &mut request.state,
            issue(
                "REPORT_RDL_RESPONSE_MISSING",
                "A report dependency operation returned no response.",
            ),
        );
    }
    request.state["rdlIndex"] = json!(index + 1);
    next(request)
}
#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::{Identity, Scope};
    use std::collections::BTreeMap;
    fn node(name: &str, content: Vec<Value>) -> Value {
        json!({"name":name,"namespace":"http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition","attributes":[],"content":content})
    }
    fn element(value: Value) -> Value {
        json!({"element":value})
    }
    fn report(subreport: Option<&str>) -> Value {
        node(
            "Report",
            subreport
                .map(|name| {
                    vec![element(node(
                        "Subreport",
                        vec![element(node("ReportName", vec![json!({"text":name})]))],
                    ))]
                })
                .unwrap_or_default(),
        )
    }
    fn file_ref(name: &str) -> FileRef {
        FileRef {
            path: format!("native/{name}.rdl"),
            sha256: "a".repeat(64),
            bytes: 10,
            role: "nativeExport".into(),
        }
    }
    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "C17".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "C17".into(),
                kind: "ReportStyles".into(),
                scope: Scope::Global,
                keys: BTreeMap::from([("ReportID".into(), "R".into())]),
            }),
            cursor: None,
            page_size: Some(200),
            state: json!({"errors":[],"definition":{"ReportStyle":[{"ReportID":"R","StyleNum":1,"RptTypeID":"SSRS","PrintProgram":"reports/CustomReports/A/Main"}]},"supplements":{"ExportSsrsRdl/0":{"returnObj":{"nativeFile":file_ref("main")}}}}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn custom_path_classification_casing_and_traversal() {
        assert_eq!(
            custom_path("/Reports/customreports/A/Main").unwrap(),
            "Reports/customreports/A/Main"
        );
        assert_eq!(
            custom_path("reports/CustomReports/A/../B/Sub").unwrap(),
            "reports/CustomReports/B/Sub"
        );
        for path in [
            "reports/Stock/Main",
            "reports/CustomReportsOther/A",
            "reports/CustomReports/../../Stock/Main",
            "reports/CustomReports/A/../../../reports/Stock/Main",
        ] {
            assert!(custom_path(path).is_err(), "{path}");
        }
    }
    #[test]
    fn selected_custom_parent_collects_its_shipped_subreport_dependency() {
        let mut r = request();
        assert!(matches!(start(&mut r), Some(HostAction::ParseXml { .. })));
        r.response = Some(
            json!({"sourceFile":file_ref("main"),"parsedXml":report(Some("../../Stock/Sub"))}),
        );
        let Some(HostAction::SharedNativeRead { arguments, .. }) = complete(&mut r) else {
            panic!("The native subreport dependency must be read.")
        };
        assert_eq!(arguments["reportPath"], "reports/Stock/Sub");
        assert_eq!(r.state["rdlQueue"].as_array().unwrap().len(), 2);
        assert!(r.state["errors"].as_array().unwrap().is_empty());
    }
    #[test]
    fn native_path_contract_and_dynamic_refs() {
        assert_eq!(
            subreport_path("reports/A/Main", "Sub").unwrap(),
            "reports/A/Sub"
        );
        assert_eq!(
            subreport_path("reports/A/Main", "../B/Sub").unwrap(),
            "reports/B/Sub"
        );
        assert_eq!(
            subreport_path("reports/A/Main", "/Sub").unwrap(),
            "reports/A/Sub"
        );
        assert!(subreport_path("reports/A/Main", "=Parameters.Report.Value").is_err());
        assert!(subreport_path("reports/A/Main", "../../Outside").is_err());
    }
    #[test]
    fn namespace_and_shared_resource_gaps_are_precise() {
        let xml = node(
            "Report",
            vec![element(node(
                "DataSourceReference",
                vec![json!({"text":"/reports/Shared"})],
            ))],
        );
        let (_, unresolved, errors) = dependencies(&xml, "reports/A/Main");
        assert_eq!(unresolved.len(), 1);
        assert_eq!(errors[0].code, "REPORT_RDL_SHARED_RESOURCE_UNVERIFIED");
        let mut xml = report(None);
        xml["namespace"] = json!("unrelated");
        assert_eq!(
            dependencies(&xml, "reports/A/Main").2[0].code,
            "REPORT_RDL_DOCUMENT_UNVERIFIED"
        );
    }
    #[test]
    fn child_bytes_are_acquired_and_cycle_terminates() {
        let mut r = request();
        assert!(matches!(start(&mut r), Some(HostAction::ParseXml { .. })));
        r.response = Some(json!({"sourceFile":file_ref("main"),"parsedXml":report(Some("Sub"))}));
        let Some(HostAction::SharedNativeRead {
            method, arguments, ..
        }) = complete(&mut r)
        else {
            panic!()
        };
        assert_eq!(method, "ExportSsrsRdl");
        assert_eq!(arguments["reportPath"], "reports/CustomReports/A/Sub");
        r.response = Some(json!({"returnObj":{"nativeFile":file_ref("sub")}}));
        assert!(matches!(
            complete(&mut r),
            Some(HostAction::ParseXml { .. })
        ));
        r.response = Some(json!({"sourceFile":file_ref("sub"),"parsedXml":report(Some("Main"))}));
        assert!(complete(&mut r).is_none());
        assert_eq!(
            r.state["supplements"]["reportDependencyReconciliation"]["complete"],
            true
        );
        assert_eq!(r.state["rdlDocuments"].as_array().unwrap().len(), 2);
        assert_eq!(r.state["rdlEdges"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn missing_child_and_wrong_owned_projection_never_complete() {
        for wrong in [false, true] {
            let mut r = request();
            start(&mut r);
            if wrong {
                r.response = Some(json!({"sourceFile":file_ref("other"),"parsedXml":report(None)}));
            } else {
                r.error = Some(issue("NATIVE_REFUSED", "No body"));
            }
            assert!(complete(&mut r).is_none());
            assert_eq!(
                r.state["supplements"]["reportDependencyReconciliation"]["complete"],
                false
            );
        }
    }
    #[test]
    fn dependency_free_main_rdl_can_complete() {
        let mut r = request();
        start(&mut r);
        r.response = Some(json!({"sourceFile":file_ref("main"),"parsedXml":report(None)}));
        assert!(complete(&mut r).is_none());
        assert!(r.state["errors"].as_array().unwrap().is_empty());
        assert_eq!(
            r.state["supplements"]["reportDependencyReconciliation"]["complete"],
            true
        );
    }
    #[test]
    fn each_native_print_program_location_requires_its_own_body() {
        let mut r = request();
        r.state["definition"]["ReportStyle"][0]["PrintProgram"] =
            json!("reports/CustomReports/A/Main,reports/CustomReports/B/Main");
        r.state["supplements"] = json!({"ExportSsrsRdl/0/0":{"returnObj":{"nativeFile":file_ref("a")}},"ExportSsrsRdl/0/1":{"returnObj":{"nativeFile":file_ref("b")}}});
        assert!(matches!(start(&mut r), Some(HostAction::ParseXml { .. })));
        assert_eq!(r.state["rdlQueue"].as_array().unwrap().len(), 2);
        r.response = Some(json!({"sourceFile":file_ref("a"),"parsedXml":report(None)}));
        assert!(matches!(
            complete(&mut r),
            Some(HostAction::ParseXml { .. })
        ));
        r.response = Some(json!({"sourceFile":file_ref("b"),"parsedXml":report(None)}));
        assert!(complete(&mut r).is_none());
        assert_eq!(
            r.state["supplements"]["reportDependencyReconciliation"]["complete"],
            true
        );
    }
}
