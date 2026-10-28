//! Inventory selects reports. ExportToFile supplies each complete definition.
use herbert_contract::*;
use serde_json::{Value, json};
fn failure(message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new("BAQ_REPORT_EXPORT_INVALID", message),
    }
}
fn finish(r: GuestRequest, spec: &super::Family, mut data: Value) -> GuestReply {
    let roots = data[&spec.root].as_array();
    if !roots.is_some_and(|rows| {
        rows.len() == 1
            && super::make_identity(spec, &rows[0], &r.company)
                .ok()
                .zip(r.identity.as_ref())
                .is_some_and(|(actual, expected)| super::root_identity_matches(&actual, expected))
    }) {
        return failure("The exported report has a different identity.");
    }
    if data.as_object().is_none_or(|tables| {
        tables
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .any(crate::system_artifacts::marked)
    }) {
        return crate::system_artifacts::refused();
    }
    for child in &spec.children {
        if data.get(child).is_none() {
            data[child] = json!([]);
        }
    }
    let diagnostics: Vec<Diagnostic> =
        serde_json::from_value(r.state["errors"].clone()).unwrap_or_default();
    let mut definition = json!({"service":spec.service,"definition":data,"supplements":{}});
    if !r.state["file"].is_null() {
        definition["supplements"]["ExportToFile"] =
            json!({"returnObj":{"nativeFile":r.state["file"]}});
    }
    GuestReply::Definition {
        identity: r.identity.unwrap(),
        format: "native-definition-and-supplements-json".into(),
        definition,
        completeness: if diagnostics.is_empty() {
            Completeness::Collected
        } else {
            Completeness::Partial
        },
        diagnostics,
    }
}
pub(super) fn handle(mut r: GuestRequest, spec: &super::Family) -> GuestReply {
    let phase = r.state["phase"].as_str().unwrap_or("initial").to_owned();
    if matches!(
        phase.as_str(),
        "baqReportExport" | "baqReportParse" | "baqReportFallback"
    ) {
        if let Some(diagnostic) = r.error.take() {
            return GuestReply::Failure { diagnostic };
        }
        let response = r.response.take().unwrap_or(Value::Null);
        if phase == "baqReportFallback" {
            if response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
                != Some(false)
            {
                return failure("The filtered report read did not confirm a complete result.");
            }
            return finish(r, spec, response["returnObj"].clone());
        }
        if phase == "baqReportExport" {
            let Some(file) = response
                .pointer("/returnObj/nativeFile")
                .and_then(|v| serde_json::from_value::<FileRef>(v.clone()).ok())
                .filter(|f| f.bytes > 0)
            else {
                return failure("The exporter returned no report file.");
            };
            r.state["file"] = json!(file);
            r.state["phase"] = json!("baqReportParse");
            return GuestReply::Action {
                action: HostAction::ParseXml { file },
                state: r.state,
            };
        }
        if response["sourceFile"] != r.state["file"] {
            return failure("The parsed file does not match the exported report.");
        }
        return match crate::xml_dataset::parse_flat(&response["parsedXml"]) {
            Ok(ds) => finish(r, spec, json!(ds.tables)),
            Err(diagnostic) => GuestReply::Failure { diagnostic },
        };
    }
    let next = if phase == "initial" {
        r.state = json!({"errors":[]});
        super::report_export_scope::start(&mut r, &spec.children)
    } else {
        super::report_export_scope::complete(&mut r)
    };
    if let Some(action) = next {
        return GuestReply::Action {
            action,
            state: r.state,
        };
    }
    if let Some(action) = super::baq_report_export::start(&mut r) {
        return GuestReply::Action {
            action,
            state: r.state,
        };
    }
    let GuestReply::Action { action, .. } = super::root_action(spec, &r) else {
        return failure("The filtered report read is unavailable.");
    };
    r.state["phase"] = json!("baqReportFallback");
    GuestReply::Action {
        action,
        state: r.state,
    }
}
