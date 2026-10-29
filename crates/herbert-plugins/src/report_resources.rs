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
/// Diagnostics that tell the user something without making the record or its
/// inventory incomplete: Herbert already holds everything Epicor holds for it.
pub const WARNINGS: [&str; 2] = ["REPORT_STORED_ZIP_INVALID", "REPORT_LABEL_TEMPLATE_EXTERNAL"];
pub fn is_warning(diagnostic: &Diagnostic) -> bool {
    WARNINGS.contains(&diagnostic.code.as_str())
}
/// A BarTender ("Labels") style. Epicor stores only the label template's path
/// (PrintProgram, a .btw on the BarTender machine) and the folder it writes
/// label data to (OutputLocation); ReportSvc has no route that reads or stores
/// the template itself (Update only checks both paths, CopyBartenderStyle copies
/// the row). The style row is the whole Epicor-side definition.
fn label_template(style: &Value) -> Option<Diagnostic> {
    style["RptTypeID"]
        .as_str()
        .filter(|kind| kind.eq_ignore_ascii_case("Labels"))?;
    Some(
        issue(
            "REPORT_LABEL_TEMPLATE_EXTERNAL",
            "This is a BarTender label style. Epicor keeps only the location of its label template, not the template itself, so Herbert saved the style and that location. Back up the label template (.btw) from the BarTender machine.",
        )
        .with_context("report", style["ReportID"].clone())
        .with_context("style", style["StyleNum"].clone())
        .with_context("labelTemplate", style["PrintProgram"].clone())
        .with_context("labelDataFolder", style["OutputLocation"].clone()),
    )
}
fn warn(state: &mut Value, warning: Diagnostic) {
    state["errors"].as_array_mut().unwrap().push(json!(warning));
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
            "A report path is outside the Epicor display-path contract.",
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
            "A report reference does not name a complete Epicor report path.",
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
                    "The report path escapes its Epicor root.",
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
            "The reference is outside Epicor's Epicor report root.",
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
                        "A subreport path escapes its Epicor report root.",
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
/// Epicor's standard platform references, which every deployed report on an
/// instance uses: the shared data source at `/<SSRS root>/reports/SharedReportDataSource`
/// (SaaS `/SaaS583SSRS_67964`, on-prem e.g. `/Epicor2024`) and the `CLSEpicor`
/// report code assembly. They are known dependencies of the instance, not
/// report-owned bodies to acquire. The SSRS root is read only in the package
/// pass, so the reference is recognized by its fixed shape under any one root.
fn platform_reference(kind: &str, value: Option<&str>) -> Option<Value> {
    let value = value?.trim();
    match kind {
        "DataSourceReference" => {
            let parts: Vec<_> = value.strip_prefix('/')?.split('/').collect();
            (parts.len() == 3
                && !parts[0].trim().is_empty()
                && parts[1].eq_ignore_ascii_case("reports")
                && parts[2].eq_ignore_ascii_case("SharedReportDataSource"))
            .then(|| json!({"kind":kind,"value":value,"platform":"EpicorSharedReportDataSource","ssrsRoot":parts[0]}))
        }
        "CodeModule" => value
            .split(',')
            .next()
            .is_some_and(|name| name.trim().eq_ignore_ascii_case("CLSEpicor"))
            .then(|| json!({"kind":kind,"value":value,"platform":"EpicorReportCodeModule"})),
        _ => None,
    }
}
fn walk(
    node: &Value,
    ns: &Value,
    subreports: &mut Vec<String>,
    unresolved: &mut Vec<Value>,
    platform: &mut Vec<Value>,
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
            let value = text(node);
            if let Some(known) = platform_reference(kind, value.as_deref()) {
                platform.push(known);
            } else {
                unresolved.push(json!({"kind":kind,"value":value}));
                errors.push(issue("REPORT_RDL_SHARED_RESOURCE_UNVERIFIED",format!("The RDL contains {kind}; its referenced source body has no established read contract.")));
            }
        }
        "Image" if child_text(node, "Source", ns).as_deref() == Some("External") => {
            unresolved.push(json!({"kind":"ExternalImage","value":child_text(node,"Value",ns)}));
            errors.push(issue(
                "REPORT_RDL_EXTERNAL_IMAGE_UNVERIFIED",
                "The RDL names an external image; no source-owned body route has been established.",
            ));
        }
        "CodeModule" => {
            let value = text(node);
            if let Some(known) = platform_reference("CodeModule", value.as_deref()) {
                platform.push(known);
            } else {
                unresolved.push(json!({"kind":"CodeModule","value":value}));
                errors.push(issue("REPORT_RDL_CODE_MODULE_UNVERIFIED","The RDL names an external code module whose original bytes have not been acquired."));
            }
        }
        _ => (),
    }
    for child in children(node) {
        walk(child, ns, subreports, unresolved, platform, errors)
    }
}
type Dependencies = (Vec<(String, String)>, Vec<Value>, Vec<Value>, Vec<Diagnostic>);
/// Subreport edges, unresolved references, known platform dependencies, and errors.
fn dependencies(xml: &Value, path: &str) -> Dependencies {
    let mut errors = vec![];
    let mut names = vec![];
    let mut unresolved = vec![];
    let mut platform = vec![];
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
            vec![],
            vec![issue(
                "REPORT_RDL_DOCUMENT_UNVERIFIED",
                "The downloaded XML is not an Epicor SQL Server report definition document.",
            )],
        );
    }
    walk(xml, ns, &mut names, &mut unresolved, &mut platform, &mut errors);
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
    (edges, unresolved, platform, errors)
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
        if let Some(warning) = label_template(style) {
            warn(&mut request.state, warning);
            continue;
        }
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
                            "Style {} has no preserved Epicor RDL for {path}.",
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
        if let Some(warning) = label_template(style) {
            warn(&mut request.state, warning);
            continue;
        }
        if style["RptTypeID"] != "SSRS" {
            push(
                &mut request.state,
                issue(
                    "REPORT_LAYOUT_TYPE_UNVERIFIED",
                    "This report style uses a layout type Herbert cannot read yet. Its settings were saved; the layout itself was not.",
                )
                .with_context("report", style["ReportID"].clone())
                .with_context("style", style["StyleNum"].clone())
                .with_context("layoutType", style["RptTypeID"].clone()),
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
                        "The subreport read omitted its preserved Epicor file.",
                    ),
                );
            }
        } else if response["sourceFile"] != entry["file"] {
            push(
                &mut request.state,
                issue(
                    "REPORT_RDL_SOURCE_MISMATCH",
                    "The XML projection does not belong to this report's recorded Epicor file.",
                ),
            );
        } else {
            let path = entry["path"].as_str().unwrap_or("");
            let (edges, unresolved, platform, errors) = dependencies(&response["parsedXml"], path);
            let document = json!({"path":path,"file":entry["file"],"unresolved":unresolved,"platformDependencies":platform,
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
            category: "report-styles-and-layouts".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "report-styles-and-layouts".into(),
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
        let (_, unresolved, platform, errors) = dependencies(&xml, "reports/A/Main");
        assert_eq!(unresolved.len(), 1);
        assert!(platform.is_empty());
        assert_eq!(errors[0].code, "REPORT_RDL_SHARED_RESOURCE_UNVERIFIED");
        let mut xml = report(None);
        xml["namespace"] = json!("unrelated");
        assert_eq!(
            dependencies(&xml, "reports/A/Main").3[0].code,
            "REPORT_RDL_DOCUMENT_UNVERIFIED"
        );
    }
    fn with(kind: &str, value: &str) -> Value {
        node("Report", vec![element(node(kind, vec![json!({"text":value})]))])
    }
    #[test]
    fn epicor_platform_references_are_known_dependencies() {
        // Values from a SaaS 2026.1 tenant's RDLs, plus an on-prem root.
        for (kind, value, platform) in [
            ("DataSourceReference", "/SaaS583SSRS_67964/reports/SharedReportDataSource", "EpicorSharedReportDataSource"),
            ("DataSourceReference", "/Epicor2024/reports/SharedReportDataSource", "EpicorSharedReportDataSource"),
            ("DataSourceReference", " /epicor2024/Reports/sharedreportdatasource ", "EpicorSharedReportDataSource"),
            ("CodeModule", "CLSEpicor, Version=1.0.0.0, Culture=neutral, PublicKeyToken=null", "EpicorReportCodeModule"),
            ("CodeModule", "CLSEpicor", "EpicorReportCodeModule"),
        ] {
            let (_, unresolved, known, errors) = dependencies(&with(kind, value), "reports/A/Main");
            assert!(errors.is_empty() && unresolved.is_empty(), "{value}");
            assert_eq!(known.len(), 1, "{value}");
            assert_eq!(known[0]["platform"], platform);
            assert_eq!(known[0]["value"], value.trim());
        }
        assert_eq!(
            dependencies(&with("DataSourceReference", "/SaaS583SSRS_67964/reports/SharedReportDataSource"), "reports/A/Main").2[0]["ssrsRoot"],
            "SaaS583SSRS_67964"
        );
    }
    #[test]
    fn other_shared_resources_and_code_modules_stay_flagged() {
        for (kind, value, code) in [
            ("DataSourceReference", "/Epicor2024/reports/OtherDataSource", "REPORT_RDL_SHARED_RESOURCE_UNVERIFIED"),
            ("DataSourceReference", "/reports/SharedReportDataSource", "REPORT_RDL_SHARED_RESOURCE_UNVERIFIED"),
            ("DataSourceReference", "/A/B/reports/SharedReportDataSource", "REPORT_RDL_SHARED_RESOURCE_UNVERIFIED"),
            ("DataSourceReference", "Epicor2024/reports/SharedReportDataSource", "REPORT_RDL_SHARED_RESOURCE_UNVERIFIED"),
            ("SharedDataSetReference", "/Epicor2024/reports/SharedReportDataSource", "REPORT_RDL_SHARED_RESOURCE_UNVERIFIED"),
            ("CodeModule", "CLSEpicorExtra, Version=1.0.0.0", "REPORT_RDL_CODE_MODULE_UNVERIFIED"),
            ("CodeModule", "Custom.Code, Version=1.0.0.0", "REPORT_RDL_CODE_MODULE_UNVERIFIED"),
        ] {
            let (_, unresolved, known, errors) = dependencies(&with(kind, value), "reports/A/Main");
            assert!(known.is_empty(), "{value}");
            assert_eq!(unresolved.len(), 1, "{value}");
            assert_eq!(errors[0].code, code, "{value}");
        }
    }
    #[test]
    fn label_styles_are_a_warning_naming_the_external_template() {
        // The shape of a SaaS tenant's BarTender style (GenRcpt 1001): a drive-letter .btw and a UNC data folder.
        let label = json!({"ReportID":"R","StyleNum":1001,"RptTypeID":"Labels","PrintProgram":"D:\\Labels\\Receipts\\Receipt.btw","OutputLocation":"\\\\server.example\\share\\BarTender\\In"});
        let other = json!({"ReportID":"R","StyleNum":2,"RptTypeID":"Crystal","PrintProgram":"reports/X"});
        let mut r = request();
        assert!(start_page(&mut r, &[label.clone(), other]).is_none());
        let errors: Vec<Diagnostic> = serde_json::from_value(r.state["errors"].clone()).unwrap();
        assert_eq!(errors.iter().map(|e| e.code.as_str()).collect::<Vec<_>>(), ["REPORT_LABEL_TEMPLATE_EXTERNAL", "REPORT_LAYOUT_TYPE_UNVERIFIED"]);
        assert!(is_warning(&errors[0]) && !is_warning(&errors[1]));
        assert_eq!(errors[0].context["labelTemplate"], label["PrintProgram"]);
        assert_eq!(errors[0].context["labelDataFolder"], label["OutputLocation"]);
        assert!(errors[0].message.contains("BarTender"));
        // A labels-only page reads nothing from Epicor and is not marked failed.
        let mut r = request();
        assert!(start_page(&mut r, &[label.clone()]).is_none());
        assert_eq!(r.state["rdlFailed"], false);
        assert_eq!(r.state["supplements"]["reportDependencyReconciliation"]["complete"], true);
        // The legacy per-definition path treats a label style the same way.
        let mut r = request();
        r.state["definition"]["ReportStyle"] = json!([label]);
        assert!(start(&mut r).is_none());
        assert_eq!(r.state["errors"][0]["code"], "REPORT_LABEL_TEMPLATE_EXTERNAL");
        assert_eq!(r.state["rdlFailed"], false);
    }
    #[test]
    fn platform_references_leave_the_layout_complete() {
        let mut r = request();
        start(&mut r);
        let xml = node("Report", vec![
            element(node("DataSourceReference", vec![json!({"text":"/SaaS583SSRS_67964/reports/SharedReportDataSource"})])),
            element(node("CodeModule", vec![json!({"text":"CLSEpicor, Version=1.0.0.0, Culture=neutral, PublicKeyToken=null"})])),
        ]);
        r.response = Some(json!({"sourceFile":file_ref("main"),"parsedXml":xml}));
        assert!(complete(&mut r).is_none());
        assert!(r.state["errors"].as_array().unwrap().is_empty());
        let reconciliation = &r.state["supplements"]["reportDependencyReconciliation"];
        assert_eq!(reconciliation["complete"], true);
        let document = &reconciliation["documents"][0];
        assert!(document["unresolved"].as_array().unwrap().is_empty());
        assert_eq!(document["platformDependencies"].as_array().unwrap().len(), 2);
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
