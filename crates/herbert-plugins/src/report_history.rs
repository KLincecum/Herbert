//! Preserve actual retained report packages by their full native PrintProgram
//! and Version identity. Historical content is never compared as if it were the
//! currently deployed report. The original ZIP is authoritative for this row.
use herbert_contract::{Diagnostic, FileRef, GuestRequest, HostAction};
use serde_json::{Value, json};
fn push(request: &mut GuestRequest, code: &str, message: &str) {
    request.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(Diagnostic::new(code, message)));
    request.state["reportHistoryFailed"] = json!(true);
}
fn done(request: &mut GuestRequest) -> Option<HostAction> {
    request.state["phase"] = json!("reportHistoryComplete");
    request.state["tables"]["retainedReportPackage"]["complete"] =
        json!(request.state["reportHistoryFailed"] == false);
    None
}
fn file(request: &GuestRequest) -> Option<FileRef> {
    serde_json::from_value(request.state["reportHistoryFile"].clone()).ok()
}
pub fn start(request: &mut GuestRequest) -> Option<HostAction> {
    request.state["reportHistoryFailed"] = json!(false);
    request.state["tables"]["retainedReportPackage"] =
        json!({"snapshot":"retainedVersion","currentDeploymentComparison":false});
    let row = request.state["tables"]["CustomReportStoreHistory"][0].clone();
    let identity = request.identity.as_ref().unwrap();
    if row["PrintProgram"].as_str() != identity.keys.get("PrintProgram").map(String::as_str)
        || row["Version"]
            .as_i64()
            .filter(|v| *v > 0)
            .map(|v| v.to_string())
            .as_ref()
            != identity.keys.get("Version")
        || [
            "ContentHash",
            "CreatedBy",
            "CreatedOn",
            "SysRevID",
            "SysRowID",
        ]
        .iter()
        .any(|key| row.get(key).is_none_or(Value::is_null))
    {
        push(
            request,
            "REPORT_HISTORY_IDENTITY_UNVERIFIED",
            "A retained report row omitted its exact full path, version, revision or source metadata.",
        );
        return done(request);
    }
    let source = request.state["binaryFiles"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["pointer"] == "/rows/0/Content")
        .map(|item| item["file"].clone())
        .unwrap_or(Value::Null);
    let Some(file) = serde_json::from_value::<FileRef>(source.clone()).ok() else {
        push(
            request,
            "REPORT_HISTORY_BODY_MISSING",
            "A retained report version has no host-preserved original package bytes.",
        );
        return done(request);
    };
    let expected = row["HerbertContentBytes"].as_u64().or_else(|| {
        row["HerbertContentBytes"]
            .as_str()
            .and_then(|v| v.parse().ok())
    });
    if expected != Some(file.bytes) || file.bytes == 0 {
        push(
            request,
            "REPORT_HISTORY_BODY_LENGTH_MISMATCH",
            "The retained package bytes do not match the independently measured persisted Content length.",
        );
        return done(request);
    }
    request.state["reportHistoryFile"] = source.clone();
    request.state["tables"]["retainedReportPackage"]["sourceFile"] = source;
    request.state["phase"] = json!("reportHistoryInventory");
    Some(HostAction::ParseXmlArchive {
        file,
        entries: vec![],
    })
}
pub fn complete(request: &mut GuestRequest) -> Option<HostAction> {
    if let Some(error) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(error));
        request.state["reportHistoryFailed"] = json!(true);
        return done(request);
    }
    let Some(response) = request.response.take() else {
        push(
            request,
            "REPORT_HISTORY_RESPONSE_MISSING",
            "The retained package operation returned no response.",
        );
        return done(request);
    };
    if response["sourceFile"] != request.state["reportHistoryFile"] {
        push(
            request,
            "REPORT_HISTORY_SOURCE_MISMATCH",
            "The ZIP projection belongs to a different source file.",
        );
        return done(request);
    }
    let archive = &response["parsedXmlArchive"];
    if request.state["phase"] == "reportHistoryInventory" {
        let Some(members) = archive["members"].as_array() else {
            push(
                request,
                "REPORT_HISTORY_DIRECTORY_MISSING",
                "The retained package omitted its ZIP member directory.",
            );
            return done(request);
        };
        let mut entries = Vec::new();
        for member in members {
            if member["directory"] == true {
                continue;
            }
            let Some(name) = member["name"].as_str() else {
                push(
                    request,
                    "REPORT_HISTORY_MEMBER_INVALID",
                    "A retained package member lacks its original name.",
                );
                return done(request);
            };
            entries.push(name.to_owned());
        }
        if entries.is_empty()
            || !entries
                .iter()
                .any(|s| s.to_ascii_lowercase().ends_with(".rdl"))
        {
            push(
                request,
                "REPORT_HISTORY_RDL_MISSING",
                "The retained package contains no RDL source member.",
            );
            return done(request);
        }
        request.state["reportHistoryEntries"] = json!(entries);
        request.state["phase"] = json!("reportHistoryParse");
        return Some(HostAction::ParseXmlArchive {
            file: file(request).unwrap(),
            entries,
        });
    }
    let entries = request.state["reportHistoryEntries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut reports = Vec::new();
    if archive["missing"]
        .as_array()
        .is_none_or(|items| !items.is_empty())
    {
        push(
            request,
            "REPORT_HISTORY_MEMBER_MISSING",
            "A retained package member was absent from its owned archive projection.",
        );
    }
    for entry in entries {
        let name = entry.as_str().unwrap();
        let document = &archive["documents"][name];
        if document["sha256"].as_str().is_none() || document["bytes"].as_u64().is_none() {
            push(
                request,
                "REPORT_HISTORY_MEMBER_UNVERIFIED",
                "A retained member has no verified decompressed byte identity.",
            );
            continue;
        }
        if !name.to_ascii_lowercase().ends_with(".rdl") {
            continue;
        }
        let xml = &document["xml"];
        if !document["diagnostic"].is_null()
            || xml["name"] != "Report"
            || !xml["namespace"].as_str().is_some_and(|ns| {
                ns.starts_with("http://schemas.microsoft.com/sqlserver/reporting/")
                    && ns.ends_with("/reportdefinition")
            })
        {
            push(
                request,
                "REPORT_HISTORY_RDL_INVALID",
                "A retained RDL member is not a complete SQL Server report definition; its original package remains preserved.",
            );
        }
        reports.push(json!({"member":name,"bytes":document["bytes"],"sha256":document["sha256"]}));
    }
    request.state["tables"]["retainedReportPackage"]["archive"] = archive.clone();
    request.state["tables"]["retainedReportPackage"]["reports"] = json!(reports);
    done(request)
}
#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::{Identity, Scope};
    fn request() -> GuestRequest {
        GuestRequest {
            abi: 1,
            operation: "acquire".into(),
            category: "C17".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "C17".into(),
                kind: "PersistedReportHistory".into(),
                scope: Scope::Global,
                keys: std::collections::BTreeMap::from([
                    ("PrintProgram".into(), "/Root/reports/A/Main".into()),
                    ("Version".into(), "7".into()),
                ]),
            }),
            cursor: None,
            page_size: Some(200),
            response: None,
            error: None,
            state: json!({"errors":[],"binaryFiles":[{"pointer":"/rows/0/Content","file":{"path":"native/report.bin","bytes":51,"sha256":"abc","role":"original"}}],"tables":{"CustomReportStoreHistory":[{"PrintProgram":"/Root/reports/A/Main","Version":7,"ContentHash":"source-hash","CreatedBy":"native-user","CreatedOn":"2026-10-17","SysRevID":123,"SysRowID":"native-row","HerbertContentBytes":"51","Content":"preserved native base64"}]}}),
        }
    }
    #[test]
    fn version_name_or_short_content_is_not_a_body() {
        let mut r = request();
        r.state["binaryFiles"] = json!([]);
        assert!(start(&mut r).is_none());
        assert_eq!(
            r.state["tables"]["retainedReportPackage"]["complete"],
            false
        );
        for (field, value) in [("HerbertContentBytes", json!(52)), ("Version", json!(8))] {
            let mut r = request();
            r.state["tables"]["CustomReportStoreHistory"][0][field] = value;
            assert!(start(&mut r).is_none());
            assert_eq!(
                r.state["tables"]["retainedReportPackage"]["complete"],
                false
            );
        }
    }
    #[test]
    fn every_archive_file_is_integrity_checked_and_retained_version_is_separate() {
        let mut r = request();
        assert!(matches!(
            start(&mut r),
            Some(HostAction::ParseXmlArchive { .. })
        ));
        r.response = Some(
            json!({"sourceFile":r.state["reportHistoryFile"],"parsedXmlArchive":{"members":[{"name":"Root/reports/A/Main.rdl","directory":false},{"name":"assets/logo.png","directory":false}]}}),
        );
        let Some(HostAction::ParseXmlArchive { entries, .. }) = complete(&mut r) else {
            panic!()
        };
        assert_eq!(entries.len(), 2);
        r.response = Some(
            json!({"sourceFile":r.state["reportHistoryFile"],"parsedXmlArchive":{"missing":[],"documents":{"Root/reports/A/Main.rdl":{"bytes":20,"sha256":"rdl-sha","xml":{"name":"Report","namespace":"http://schemas.microsoft.com/sqlserver/reporting/2016/01/reportdefinition"},"diagnostic":null},"assets/logo.png":{"bytes":12,"sha256":"png-sha","xml":null,"diagnostic":{"code":"XML_ENCODING_UNSUPPORTED"}}}}}),
        );
        assert!(complete(&mut r).is_none());
        assert_eq!(r.state["tables"]["retainedReportPackage"]["complete"], true);
        assert_eq!(
            r.state["tables"]["retainedReportPackage"]["currentDeploymentComparison"],
            false
        );
        assert!(r.state["errors"].as_array().unwrap().is_empty());
    }
    #[test]
    fn swapped_owned_source_and_invalid_rdl_remain_partial() {
        let mut r = request();
        start(&mut r);
        r.response = Some(json!({"sourceFile":{"path":"another"}}));
        assert!(complete(&mut r).is_none());
        assert_eq!(
            r.state["tables"]["retainedReportPackage"]["complete"],
            false
        );
        let mut r = request();
        start(&mut r);
        r.state["phase"] = json!("reportHistoryParse");
        r.state["reportHistoryEntries"] = json!(["Main.rdl"]);
        r.response = Some(
            json!({"sourceFile":r.state["reportHistoryFile"],"parsedXmlArchive":{"missing":[],"documents":{"Main.rdl":{"bytes":1,"sha256":"bad","xml":null,"diagnostic":{"code":"XML_INVALID"}}}}}),
        );
        complete(&mut r);
        assert_eq!(
            r.state["tables"]["retainedReportPackage"]["complete"],
            false
        );
    }
}
