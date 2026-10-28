//! Successful HTTP status is not proof that a declared export exists.
use base64::Engine;
use herbert_contract::{Diagnostic, FileRef};
use serde_json::Value;
fn bytes(value: &Value) -> bool {
    if let Some(file) = value.get("nativeFile") {
        return serde_json::from_value::<FileRef>(file.clone())
            .is_ok_and(|f| !f.path.is_empty() && !f.sha256.is_empty() && f.bytes > 0);
    }
    match value {
        Value::String(text) => base64::engine::general_purpose::STANDARD
            .decode(text)
            .is_ok_and(|b| !b.is_empty()),
        Value::Array(items) => {
            !items.is_empty() && items.iter().all(|v| v.as_u64().is_some_and(|v| v <= 255))
        }
        _ => false,
    }
}
/// Validate only reviewed required output paths. This proves response shape and
/// preserved bytes, not archive identity or independent source reconciliation.
pub fn validate(
    service: &str,
    method: &str,
    arguments: &Value,
    response: &Value,
) -> Vec<Diagnostic> {
    let valid = match (service, method) {
        ("Ice.BO.BAQDesignerSvc", "ExportBaq")
        | ("Ice.BO.ReportSvc", "ExportSsrsRdl")
        | ("Ice.BO.MessageTemplateSvc", "ExportMessageTemplate")
        | ("Ice.BO.AccessScopeSvc", "ExportAccessScope") => {
            response.get("returnObj").is_some_and(bytes)
        }
        ("Ice.BO.BpMethodSvc", "ExportByService" | "ExportByTable") => {
            response.pointer("/returnObj/Data").is_some_and(bytes)
        }
        ("Ice.BO.RptDataDefSvc", "ExportReport") => response
            .get("returnObj")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty()),
        // Native description explicitly defines null as no compiled assembly.
        // Preserve that absence. Do not manufacture bytes or reject this shape.
        ("Ice.BO.ExtensionSvc", "ExportExtensionSet") => response
            .get("returnObj")
            .is_some_and(|v| v.is_null() || bytes(v)),
        // Empty image dictionaries are a valid no-images result.
        ("Ice.BO.ReportSvc", "ExportReportStyleImages") => {
            response.get("returnObj").is_some_and(Value::is_object)
        }
        ("Ice.BO.ReportSvc", "ExportReportStyle") => response
            .pointer("/returnObj/ReportStyle")
            .and_then(Value::as_array)
            .is_some_and(|rows| {
                rows.len() == 1
                    && rows[0].get("ReportID") == arguments.get("reportId")
                    && rows[0].get("StyleNum") == arguments.get("styleNumber")
            }),
        _ => {
            return vec![Diagnostic::new(
                "EXPORT_CONTRACT_UNVERIFIED",
                &format!(
                    "The required {service}.{method} export output contract has not been declared."
                ),
            )];
        }
    };
    if valid {
        vec![]
    } else {
        vec![Diagnostic::new(
            "NATIVE_EXPORT_UNVERIFIED",
            &format!(
                "The {service}.{method} response omitted or misrepresented its required export output."
            ),
        )]
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn missing_invalid_empty_and_preserved_bytes_are_distinguished() {
        for response in [
            json!({}),
            json!({"returnObj":null}),
            json!({"returnObj":""}),
            json!({"returnObj":"not base64"}),
        ] {
            assert!(
                !validate("Ice.BO.BAQDesignerSvc", "ExportBaq", &json!({}), &response).is_empty()
            );
        }
        assert!(
            validate(
                "Ice.BO.BAQDesignerSvc",
                "ExportBaq",
                &json!({}),
                &json!({"returnObj":"YQ=="})
            )
            .is_empty()
        );
        assert!(validate("Ice.BO.BAQDesignerSvc", "ExportBaq", &json!({}), &json!({"returnObj":{"nativeFile":{"path":"native/x.baq","sha256":"digest","bytes":1,"role":"nativeExport"}}})).is_empty());
    }
    #[test]
    fn native_empty_extension_and_image_exports_are_valid() {
        assert!(
            validate(
                "Ice.BO.ExtensionSvc",
                "ExportExtensionSet",
                &json!({}),
                &json!({"returnObj":null})
            )
            .is_empty()
        );
        assert!(
            validate(
                "Ice.BO.ReportSvc",
                "ExportReportStyleImages",
                &json!({}),
                &json!({"returnObj":{}})
            )
            .is_empty()
        );
    }
    #[test]
    fn unqualified_posting_export_is_rejected_and_style_identity_is_checked() {
        assert!(
            validate(
                "Erp.BO.ACTTypeSvc",
                "ExportToXML",
                &json!({}),
                &json!({"parameters":{"cXML":[{"XML":"<ACTType/>"}]}})
            )
            .is_empty()==false
        );
        assert!(
            !validate(
                "Erp.BO.ACTTypeSvc",
                "ExportToXML",
                &json!({}),
                &json!({"returnObj":"<ACTType/>"})
            )
            .is_empty()
        );
        let args = json!({"reportId":"R","styleNumber":1});
        let mut response = json!({"returnObj":{"ReportStyle":[{"ReportID":"R","StyleNum":1}]}});
        assert!(validate("Ice.BO.ReportSvc", "ExportReportStyle", &args, &response).is_empty());
        response["returnObj"]["ReportStyle"][0]["StyleNum"] = json!(2);
        assert!(!validate("Ice.BO.ReportSvc", "ExportReportStyle", &args, &response).is_empty());
    }
}
