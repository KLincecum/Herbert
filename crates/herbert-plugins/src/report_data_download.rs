//! Native report XML is the downloaded definition. Only mixed-System exports
//! use the filtered tableset route instead.
use herbert_contract::*;
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("REPORT_EXPORT_INVALID", message)
}
fn flat_xml(text: &str) -> Result<Value, Diagnostic> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().expand_empty_elements = true;
    let mut names = Vec::<String>::new();
    let mut tables = serde_json::Map::new();
    let mut row = serde_json::Map::new();
    let mut value = String::new();
    let mut nil = false;
    let mut roots = 0;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if names.is_empty() {
                    roots += 1;
                }
                if names.len() == 1 {
                    row.clear();
                }
                if names.len() == 2 {
                    value.clear();
                    nil = false;
                    for a in e.attributes() {
                        let a = a.map_err(|_| invalid("The export has invalid XML attributes."))?;
                        if a.key.local_name().as_ref() == b"nil" && a.value.as_ref() == b"true" {
                            nil = true;
                        }
                    }
                }
                names.push(name);
                if names.len() > 3 {
                    return Err(invalid(
                        "The report export contains a nested field instead of a flat DataSet.",
                    ));
                }
            }
            Ok(Event::Text(e)) => {
                if names.len() == 3 {
                    value.push_str(
                        &e.decode()
                            .map_err(|_| invalid("The report text cannot be decoded."))?,
                    );
                }
            }
            Ok(Event::CData(e)) => {
                if names.len() == 3 {
                    value.push_str(
                        &e.decode()
                            .map_err(|_| invalid("The report CDATA cannot be decoded."))?,
                    );
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if names.len() == 3 {
                    let name = e
                        .decode()
                        .map_err(|_| invalid("The report XML reference is invalid."))?;
                    value.push_str(&quick_xml::escape::unescape(&format!("&{name};")).map_err(
                        |_| invalid("The report contains an unresolved XML reference."),
                    )?);
                }
            }
            Ok(Event::End(_)) => {
                if names.len() == 3 {
                    if row
                        .insert(
                            names[2].clone(),
                            if nil { Value::Null } else { json!(value) },
                        )
                        .is_some()
                    {
                        return Err(invalid("The report repeats a field."));
                    }
                } else if names.len() == 2 {
                    tables
                        .entry(names[1].clone())
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(json!(row));
                }
                names.pop();
            }
            Ok(Event::DocType(_)) => {
                return Err(invalid(
                    "Document types are not part of the Epicor report XML format.",
                ));
            }
            Ok(Event::Eof) => break,
            Ok(_) => (),
            Err(_) => return Err(invalid("The report export is malformed XML.")),
        }
    }
    if roots != 1 || !names.is_empty() {
        return Err(invalid("The report XML is incomplete."));
    }
    Ok(Value::Object(tables))
}
fn finish(r: GuestRequest, data: Value, xml: Option<String>) -> GuestReply {
    let id = r.identity.clone().unwrap();
    let valid = data["RptDataDef"].as_array().is_some_and(|rows| {
        rows.len() == 1 && rows[0]["RptDefID"].as_str() == Some(&id.keys["RptDefID"])
    });
    if !valid
        || data.as_object().is_none_or(|tables| {
            tables
                .values()
                .filter_map(Value::as_array)
                .flatten()
                .any(crate::system_artifacts::marked)
        })
    {
        return GuestReply::Failure {
            diagnostic: invalid(
                "The report response has another identity or contains System-marked rows.",
            ),
        };
    }
    let diagnostics: Vec<Diagnostic> =
        serde_json::from_value(r.state["errors"].clone()).unwrap_or_default();
    let mut definition =
        json!({"service":"Ice.BO.RptDataDefSvc","definition":data,"supplements":{}});
    if let Some(text) = xml {
        definition["supplements"]["ExportReport"] = json!({"returnObj":text});
        definition["herbertSources"] =
            json!({"files":[{"name":"ReportDataDefinition.xml","text":text}]});
    }
    GuestReply::Definition {
        identity: id,
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
    if r.identity
        .as_ref()
        .and_then(|id| id.keys.get("RptDefID"))
        .is_none_or(|id| id.is_empty())
    {
        return GuestReply::Failure {
            diagnostic: invalid("A report definition ID is required."),
        };
    }
    let phase = r.state["phase"].as_str().unwrap_or("initial").to_owned();
    if matches!(phase.as_str(), "reportDataExport" | "reportDataFallback") {
        if let Some(diagnostic) = r.error.take() {
            return GuestReply::Failure { diagnostic };
        }
        let response = r.response.take().unwrap_or(Value::Null);
        if phase == "reportDataFallback" {
            if response
                .pointer("/parameters/morePages")
                .and_then(Value::as_bool)
                != Some(false)
            {
                return GuestReply::Failure {
                    diagnostic: invalid(
                        "The filtered report read did not confirm a complete result.",
                    ),
                };
            }
            return finish(r, response["returnObj"].clone(), None);
        }
        let Some(text) = response["returnObj"].as_str() else {
            return GuestReply::Failure {
                diagnostic: invalid("The exporter did not return report XML."),
            };
        };
        return match flat_xml(text) {
            Ok(data) => finish(r, data, Some(text.into())),
            Err(diagnostic) => GuestReply::Failure { diagnostic },
        };
    }
    let action = if phase == "initial" {
        r.state = json!({"errors":[]});
        super::report_export_scope::start(&mut r, &spec.children)
    } else {
        super::report_export_scope::complete(&mut r)
    };
    if let Some(action) = action {
        r.state["phase"] = json!("reportDataScope");
        return GuestReply::Action {
            action,
            state: r.state,
        };
    }
    if r.state["reportExportAllowed"] == true {
        r.state["phase"] = json!("reportDataExport");
        GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: spec.service.clone(),
                method: "ExportReport".into(),
                arguments: json!({"rptDefId":r.identity.as_ref().unwrap().keys["RptDefID"]}),
                export: true,
            },
            state: r.state,
        }
    } else {
        let GuestReply::Action { action, .. } = super::root_action(spec, &r) else {
            return GuestReply::Failure {
                diagnostic: invalid("The filtered report read is unavailable."),
            };
        };
        r.state["phase"] = json!("reportDataFallback");
        GuestReply::Action {
            action,
            state: r.state,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_export_replaces_definition_reads_and_system_children_use_filtered_fallback() {
        for mixed in [false, true] {
            let spec = super::super::specs()
                .into_iter()
                .find(|s| s.kind == "ReportDataDefinition")
                .unwrap();
            let mut r = GuestRequest {
                abi: PLUGIN_ABI,
                operation: "acquire".into(),
                category: "report-definitions".into(),
                company: "C".into(),
                identity: Some(Identity {
                    category: "report-definitions".into(),
                    kind: "ReportDataDefinition".into(),
                    scope: Scope::Global,
                    keys: std::collections::BTreeMap::from([("RptDefID".into(), "Custom".into())]),
                }),
                cursor: None,
                page_size: Some(2),
                state: Value::Null,
                response: None,
                error: None,
            };
            loop {
                match handle(r.clone(), &spec) {
                    GuestReply::Action {
                        action: HostAction::SharedQuery { .. },
                        state,
                    } => {
                        r.state = state;
                        r.response = Some(
                            json!({"rows":if mixed{vec![json!({"RptDefID":"Custom","SysRowID":"system"})]}else{vec![]}}),
                        );
                    }
                    GuestReply::Action {
                        action: HostAction::SharedNativeRead { method, .. },
                        state,
                    } => {
                        assert!(!mixed);
                        assert_eq!(method, "ExportReport");
                        r.state = state;
                        r.response = Some(
                            json!({"returnObj":"<Data><RptDataDef><RptDefID>Custom</RptDefID><SystemFlag>false</SystemFlag><Description>Original Ω</Description></RptDataDef></Data>"}),
                        );
                        let GuestReply::Definition {
                            definition,
                            completeness,
                            ..
                        } = handle(r, &spec)
                        else {
                            panic!()
                        };
                        assert_eq!(completeness, Completeness::Collected);
                        assert_eq!(
                            definition["definition"]["RptDataDef"][0]["Description"],
                            "Original Ω"
                        );
                        break;
                    }
                    GuestReply::Action {
                        action:
                            HostAction::NativeRead {
                                method, arguments, ..
                            },
                        ..
                    } => {
                        assert!(mixed);
                        assert_eq!(method, "GetRows");
                        assert!(
                            arguments["whereClauseRptDataDef"]
                                .as_str()
                                .unwrap()
                                .contains("SystemFlag = false")
                        );
                        break;
                    }
                    _ => panic!(),
                }
            }
        }
    }
    #[test]
    fn report_xml_preserves_text_empty_values_and_rejects_broken_documents() {
        let data=flat_xml("<Data><RptDataDef><RptDefID>Custom</RptDefID><Description>Keep &amp; Ω</Description><Empty/><Code><![CDATA[x < y]]></Code></RptDataDef></Data>").unwrap();
        assert_eq!(data["RptDataDef"][0]["Description"], "Keep & Ω");
        assert_eq!(data["RptDataDef"][0]["Empty"], "");
        assert_eq!(data["RptDataDef"][0]["Code"], "x < y");
        assert!(flat_xml("<Data><Row>").is_err());
    }
}
