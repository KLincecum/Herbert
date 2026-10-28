//! Read native JSON packages locally after one download.
use herbert_contract::*;
use serde_json::{Value, json};
fn failure(message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new("NATIVE_PACKAGE_INVALID", message),
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
        return failure("The exported definition has a different identity.");
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
        definition["supplements"][if spec.kind == "MessageTemplate" {
            "ExportMessageTemplate"
        } else {
            "ExportAccessScope"
        }] = json!({"returnObj":{"nativeFile":r.state["file"]}});
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
    let message = spec.kind == "MessageTemplate";
    let member = if message {
        "MessageTemplateTableset"
    } else {
        "AccessScopeTableset"
    };
    let method = if message {
        "ExportMessageTemplate"
    } else {
        "ExportAccessScope"
    };
    let phase = r.state["phase"].as_str().unwrap_or("initial").to_owned();
    if matches!(
        phase.as_str(),
        "jsonPackageExport" | "jsonPackageParse" | "jsonPackageFallback"
    ) {
        if let Some(diagnostic) = r.error.take() {
            return GuestReply::Failure { diagnostic };
        }
        let response = r.response.take().unwrap_or(Value::Null);
        if phase == "jsonPackageFallback" {
            if response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
                != Some(false)
            {
                return failure("The filtered definition read did not confirm a complete result.");
            }
            return finish(r, spec, response["returnObj"].clone());
        }
        if phase == "jsonPackageExport" {
            let Some(file) = response
                .pointer("/returnObj/nativeFile")
                .and_then(|v| serde_json::from_value::<FileRef>(v.clone()).ok())
                .filter(|f| f.bytes > 0)
            else {
                return failure("The exporter returned no definition file.");
            };
            r.state["file"] = json!(file);
            r.state["phase"] = json!("jsonPackageParse");
            return GuestReply::Action {
                action: HostAction::ParseJsonArchive {
                    file,
                    entries: vec![member.into()],
                },
                state: r.state,
            };
        }
        if response["sourceFile"] != r.state["file"] {
            return failure("The parsed file does not match the exported definition.");
        }
        let document = &response["parsedJsonArchive"]["documents"][member];
        if !document["diagnostic"].is_null() {
            return failure("The definition package has invalid JSON.");
        }
        return finish(r, spec, document["json"].clone());
    }
    let next = if message {
        if phase != "initial" {
            return failure("The message package checkpoint is invalid.");
        }
        r.state = json!({"errors":[],"accessExportAllowed":true});
        None
    } else if phase == "initial" {
        r.state = json!({"errors":[]});
        super::access_export_scope::start(&mut r)
    } else {
        super::access_export_scope::complete(&mut r)
    };
    if let Some(action) = next {
        return GuestReply::Action {
            action,
            state: r.state,
        };
    }
    if r.state["accessExportAllowed"] == true {
        r.state["phase"] = json!("jsonPackageExport");
        return GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: spec.service.clone(),
                method: method.into(),
                arguments: if message {
                    json!({"messageTemplateID":r.identity.as_ref().unwrap().keys["MessageTemplateID"],"langNameID":r.identity.as_ref().unwrap().keys["LangNameID"]})
                } else {
                    json!({"accessScopeID":r.identity.as_ref().unwrap().keys["AccessScopeID"]})
                },
                export: true,
            },
            state: r.state,
        };
    }
    let GuestReply::Action { action, .. } = super::root_action(spec, &r) else {
        return failure("The filtered definition read is unavailable.");
    };
    r.state["phase"] = json!("jsonPackageFallback");
    GuestReply::Action {
        action,
        state: r.state,
    }
}
