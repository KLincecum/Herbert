//! DynamicReport.ExportToFile uses UserData (native SpecialFolder value 4).
//! The host owns the unpredictable filename, readback and verified cleanup.
use herbert_contract::*;
use serde_json::json;
pub fn start(request: &mut GuestRequest) -> Option<HostAction> {
    if request.state["reportExportAllowed"] != true {
        return None;
    }
    request.state["phase"] = json!("baqReportExport");
    Some(HostAction::NativeFileExport {
        identity: None,
        service: "Ice.BO.DynamicReportSvc".into(),
        method: "ExportToFile".into(),
        arguments: json!({"baqRptID":request.identity.as_ref().unwrap().keys["BAQRptID"],"newBaqRptID":""}),
        filename_argument: "fileName".into(),
        folder: 4,
        extension: "xml".into(),
        companion_extensions: vec![],
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn req() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "report-definitions".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "report-definitions".into(),
                kind: "BAQReport".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([("BAQRptID".into(), "R".into())]),
            }),
            cursor: None,
            page_size: None,
            state: json!({"reportExportAllowed":true,"errors":[],"supplements":{}}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn report_baq_export_uses_owned_filename_capability() {
        let mut r = req();
        let Some(HostAction::NativeFileExport {
            arguments,
            folder,
            filename_argument,
            ..
        }) = start(&mut r)
        else {
            panic!()
        };
        assert_eq!(folder, 4);
        assert_eq!(filename_argument, "fileName");
        assert!(arguments.get("fileName").is_none());
        assert_eq!(arguments["baqRptID"], "R");
    }
}
