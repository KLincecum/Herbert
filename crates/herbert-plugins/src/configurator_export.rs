//! ConfiguratorDef native export writes UserData XML and, for Kinetic, a sibling ZIP.
//! Native ExportConfiguration/ExportApp retain both files. The host owns their stem.
use herbert_contract::*;
use serde_json::{Value, json};

fn error(request: &mut GuestRequest, message: &str) {
    request.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(Diagnostic::new(
            "CONFIGURATOR_EXPORT_UNVERIFIED",
            message
        )));
}
pub fn start(request: &mut GuestRequest) -> Option<HostAction> {
    request.state["phase"] = json!("configuratorExportComplete");
    let root = &request.state["definition"]["PcStatus"][0];
    let id = request.identity.as_ref().unwrap();
    let Some(kinetic) = root["Kinetic"].as_bool() else {
        error(
            request,
            "The configurator omitted its Epicor Kinetic export mode.",
        );
        return None;
    };
    if root["ConfigID"].as_str() != id.keys.get("ConfigID").map(String::as_str)
        || root["Company"].as_str() != id.keys.get("Company").map(String::as_str)
    {
        error(
            request,
            "Epicor's export root does not match the selected company and configurator.",
        );
        return None;
    }
    let Some(config_type) = root["ConfigType"].as_str() else {
        error(
            request,
            "The configurator omitted its Epicor configuration type.",
        );
        return None;
    };
    // Native NIC configurations reject inputs, smart strings and global variables.
    let inputs = !config_type.eq_ignore_ascii_case("NIC");
    // Only this native export consumes the layered-image tableset, so it is read here, once, for the fallback
    // route; the local XML/ZIP route never asks for it (the stored graph keeps PcInputsLayerHeader/Detail).
    // configurator_download's configImages phase stores the reply and re-enters this function.
    if request.state["supplements"].get("RetrievePcLayeredImagesForExport").is_none() {
        request.state["phase"] = json!("configImages");
        return Some(HostAction::SharedNativeRead {
            service: "Erp.BO.ConfiguratorDefSvc".into(),
            method: "RetrievePcLayeredImagesForExport".into(),
            arguments: json!({"configID":id.keys["ConfigID"],"configImportExportLayeredImageTableset":{"ConfigImportExportLayeredImage":[],"ExtensionTables":[]}}),
            export: false,
        });
    }
    let Some(images) = request.state["supplements"]["RetrievePcLayeredImagesForExport"]
        .pointer("/parameters/configImportExportLayeredImageTableset")
        .cloned()
    else {
        error(
            request,
            "The Epicor layered-image export tableset is unavailable.",
        );
        return None;
    };
    if !images["ConfigImportExportLayeredImage"].is_array() {
        error(
            request,
            "The Epicor layered-image export tableset is malformed.",
        );
        return None;
    }
    if images["ConfigImportExportLayeredImage"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| {
            row["Company"].as_str() != id.keys.get("Company").map(String::as_str)
                || row["ConfigID"].as_str() != id.keys.get("ConfigID").map(String::as_str)
        })
    {
        error(
            request,
            "The layered-image export contains another company or configurator.",
        );
        return None;
    }
    let config = id.keys["ConfigID"].clone();
    if kinetic && request.state["configuratorAppPolicyChecked"] != true {
        return Some(policy_start(request, "configuratorExportPolicy"));
    }
    if kinetic && request.state["configuratorAppPolicyAllowed"] != true {
        return None;
    }
    request.state["configuratorExportKinetic"] = json!(kinetic);
    request.state["phase"] = json!("configuratorExport");
    Some(HostAction::NativeFileExport {
        identity: request.identity.clone(),
        service: "Erp.BO.ConfiguratorDefSvc".into(),
        method: "ExportConfiguration".into(),
        arguments: json!({"configID":config,"path":"","doInputs":inputs,"doComments":true,"doSequence":true,"doPartCreation":true,"doSmartString":inputs,"doDocRules":true,"doPcRules":true,"doGlbVar":inputs,"configImportExportLayeredImageTS":images}),
        filename_argument: "fileName".into(),
        folder: 4,
        extension: if kinetic { "zip" } else { "xml" }.into(),
        companion_extensions: if kinetic { vec!["xml".into()] } else { vec![] },
    })
}
pub fn complete(request: &mut GuestRequest) {
    if let Some(diagnostic) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(diagnostic));
    } else {
        let response = request.response.take().unwrap_or(Value::Null);
        let primary = response
            .pointer("/returnObj/nativeFile")
            .cloned()
            .and_then(|v| serde_json::from_value::<FileRef>(v).ok());
        let valid = primary
            .as_ref()
            .is_some_and(|f| f.bytes > 0 && !f.path.is_empty() && !f.sha256.is_empty());
        let kinetic = request.state["configuratorExportKinetic"] == true;
        let files = response
            .pointer("/returnObj/nativeFiles")
            .and_then(Value::as_array);
        let companions_valid = !kinetic
            || files.is_some_and(|fs| {
                ["xml", "zip"].iter().all(|ext| {
                    fs.iter().any(|v| {
                        serde_json::from_value::<FileRef>(v.clone())
                            .ok()
                            .is_some_and(|f| {
                                f.bytes > 0
                                    && !f.sha256.is_empty()
                                    && f.path.ends_with(&format!(".{ext}"))
                            })
                    })
                })
            });
        if valid && companions_valid {
            request.state["supplements"]["ExportConfiguration"] = response;
        } else {
            error(
                request,
                "Epicor's export did not preserve every required nonempty output file.",
            );
        }
    }
    request.state["phase"] = json!("configuratorExportComplete");
}

/// Native event/expression columns not covered by generic Body/Expression views.
/// Keep original text without guessing its language or parsing it as JSON.
pub fn attach_sources(definition: &mut Value) {
    let mut files = Vec::new();
    let root = &definition["definition"]["PcStatus"][0];
    if let (Some(company), Some(config)) = (root["Company"].as_str(), root["ConfigID"].as_str()) {
        if !company.is_empty() && !config.is_empty() {
            definition["herbertExportIdentity"] = json!(Identity {
                category: "configurators".into(),
                kind: "Configurator".into(),
                scope: Scope::Company {
                    company: company.into()
                },
                keys: std::collections::BTreeMap::from([
                    ("Company".into(), company.into()),
                    ("ConfigID".into(), config.into())
                ]),
            });
        }
    }
    if let Some(app_files) = definition
        .pointer("/supplements/ConfiguratorCurrentFiles/files")
        .and_then(Value::as_object)
    {
        for (name, text) in app_files {
            if text.is_string() {
                files.push(json!({"name":format!("NativeApp/{name}"),"text":text}));
            }
        }
    }
    for (table, rows) in definition["definition"].as_object().into_iter().flatten() {
        for (index, row) in rows.as_array().into_iter().flatten().enumerate() {
            for field in [
                "OnLeave",
                "OnLeave2",
                "PromptWhen",
                "ReadOnlyExpression",
                "RuleExpr",
                "RuleString",
            ] {
                if let Some(text) = row[field].as_str().filter(|s| !s.is_empty()) {
                    files.push(
                        json!({"name":format!("Code/{table}/{index}/{field}.txt"),"text":text}),
                    );
                }
            }
        }
    }
    if !files.is_empty() {
        definition["herbertSources"] = json!({"files":files});
    }
}

// Same pre-body ownership inventory as the Kinetic plugin. Whole-app native
// exports cannot filter system children. Query metadata only before either
// direct ExportApp or the ConfiguratorDef ZIP which embeds ExportApp.
pub fn policy_start(request: &mut GuestRequest, phase: &str) -> HostAction {
    request.state["phase"] = json!(phase);
    request.state["configuratorPolicyBranch"] = json!(0);
    request.state["configuratorPolicyAfter"] = Value::Null;
    request.state["configuratorAppPolicyAllowed"] = json!(false);
    policy_query(request)
}
fn policy_query(request: &GuestRequest) -> HostAction {
    let id = request.identity.as_ref().unwrap();
    let branch = request.state["configuratorPolicyBranch"].as_u64().unwrap();
    let event = branch >= 2;
    let mut parameters = std::collections::BTreeMap::from([
        (
            "company".into(),
            json!(if branch % 2 == 0 {
                ""
            } else {
                &id.keys["Company"]
            }),
        ),
        ("product".into(), json!("EP")),
        (
            "kind".into(),
            json!(if event {
                "KNTCEventLayout"
            } else {
                "KNTCMetaUI"
            }),
        ),
        ("system".into(), json!(true)),
    ]);
    let mut criteria: Vec<_> = [
        ("Company", "company"),
        ("ProductID", "product"),
        ("TypeCode", "kind"),
        ("SystemFlag", "system"),
    ]
    .into_iter()
    .map(|(field, parameter)| QueryCriterion {
        table: "Ownership".into(),
        field: field.into(),
        operator: "=".into(),
        parameter: parameter.into(),
    })
    .collect();
    // Only this app can block its export. Scanning unrelated System metadata
    // consumed over 200 pages in testing.
    parameters.insert(
        "configView".into(),
        json!(format!(
            "Erp.UICfg.{}_{}",
            id.keys["ConfigID"], id.keys["Company"]
        )),
    );
    criteria.push(QueryCriterion {
        table: "Ownership".into(),
        field: if event { "Key2" } else { "Key1" }.into(),
        operator: "=".into(),
        parameter: "configView".into(),
    });
    if !request.state["configuratorPolicyAfter"].is_null() {
        parameters.insert(
            "after".into(),
            request.state["configuratorPolicyAfter"].clone(),
        );
        criteria.push(QueryCriterion {
            table: "Ownership".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Ice".into(),
                table: "XXXDef".into(),
                alias: "Ownership".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields: [
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
                table: "Ownership".into(),
                field: field.into(),
                alias: field.into(),
                expression: None,
            })
            .collect(),
            joins: vec![],
            criteria,
            parameters,
            order: vec![QueryOrder {
                field: "SysRowID".into(),
                descending: false,
            }],
            page_size: request.page_size,
            page: 1,
        },
    }
}
pub fn policy_complete(request: &mut GuestRequest) -> Option<HostAction> {
    let response = request.response.take().unwrap_or(Value::Null);
    if let Some(diagnostic) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(diagnostic));
    } else if let Some(rows) = response["rows"].as_array() {
        let id = request.identity.as_ref().unwrap();
        let view = format!("Erp.UICfg.{}_{}", id.keys["ConfigID"], id.keys["Company"]);
        let matches = rows.iter().any(|row| {
            let field = if row["TypeCode"] == "KNTCEventLayout" {
                "Key2"
            } else {
                "Key1"
            };
            row[field]
                .as_str()
                .is_some_and(|v| v.eq_ignore_ascii_case(&view))
        });
        let size = request.page_size.unwrap_or(0) as usize;
        if size == 0
            || rows.len() > size
            || rows.iter().any(|row| {
                !crate::system_artifacts::marked(row)
                    || row[if request.state["configuratorPolicyBranch"].as_u64().unwrap() >= 2 {
                        "Key2"
                    } else {
                        "Key1"
                    }]
                    .as_str()
                    .is_none_or(|v| !v.eq_ignore_ascii_case(&view))
                    || row["SysRowID"].as_str().is_none_or(str::is_empty)
            })
        {
            error(
                request,
                "The System-child inventory returned invalid metadata.",
            );
        } else if matches {
            error(
                request,
                "The Epicor app export was withheld because it includes System-marked children.",
            );
        } else if rows.len() == size {
            let after = rows.last().unwrap()["SysRowID"].clone();
            if after == request.state["configuratorPolicyAfter"] {
                error(
                    request,
                    "The System-child inventory saved position did not advance.",
                );
            } else {
                request.state["configuratorPolicyAfter"] = after;
                return Some(policy_query(request));
            }
        } else {
            let branch = request.state["configuratorPolicyBranch"].as_u64().unwrap() + 1;
            if branch < 4 {
                request.state["configuratorPolicyBranch"] = json!(branch);
                request.state["configuratorPolicyAfter"] = Value::Null;
                return Some(policy_query(request));
            }
            request.state["configuratorAppPolicyAllowed"] = json!(true);
            request.state["configuratorAppPolicyChecked"] = json!(true);
            return None;
        }
    } else {
        error(
            request,
            "The Epicor app ownership inventory did not return metadata rows; app export was withheld.",
        );
    }
    request.state["configuratorAppPolicyChecked"] = json!(true);
    None
}

pub fn files_start(request: &mut GuestRequest, phase: &str) -> Option<HostAction> {
    request.state["phase"] = json!(phase);
    request.state["configuratorFileBranch"] = json!(0);
    request.state["configuratorFileAfter"] = Value::Null;
    if !request.state["supplements"]["ConfiguratorCurrentFiles"].is_object() {
        request.state["supplements"]["ConfiguratorCurrentFiles"] = json!({"rows":[],"files":{}});
    }
    if request.page_size.filter(|v| *v > 0).is_none() {
        error(
            request,
            "The current app file inventory requires the configured query page size.",
        );
        request.state["configuratorFilesComplete"] = json!(true);
        return None;
    }
    Some(files_query(request))
}
fn files_query(request: &GuestRequest) -> HostAction {
    let id = request.identity.as_ref().unwrap();
    let company = if request.state["configuratorFileBranch"] == 0 {
        ""
    } else {
        &id.keys["Company"]
    };
    let GuestReply::Action { mut action, .. } = crate::tables::kinetic_file_page(
        request,
        company,
        request.state["configuratorFileAfter"].clone(),
        Value::Null,
    ) else {
        unreachable!("The file descriptor produces a query")
    };
    {
        let query = match &mut action {
            HostAction::SharedQuery { query } | HostAction::Query { query } => query,
            _ => unreachable!(),
        };
        let alias = query.tables[0].alias.clone();
        let view = format!("Erp.UICfg.{}_{}", id.keys["ConfigID"], id.keys["Company"]);
        query.criteria.push(QueryCriterion {
            table: alias.clone(),
            field: "Key1".into(),
            operator: "=".into(),
            parameter: "configView".into(),
        });
        query.parameters.insert("configView".into(), json!(view));
        // ExportApp omits files beyond its native page. Download only paths
        // absent from the package; do not fetch their content a second time.
        for (index, name) in request.state["nativeAppFiles"]
            .as_object()
            .into_iter()
            .flat_map(|files| files.keys())
            .enumerate()
        {
            for (variant, separator) in ["/", "\\"].iter().enumerate() {
                let parameter = format!("saved{index}_{variant}");
                let path = format!("{view}/{name}").replace('/', separator);
                query.criteria.push(QueryCriterion {
                    table: alias.clone(),
                    field: "Key2".into(),
                    operator: "<>".into(),
                    parameter: parameter.clone(),
                });
                query.parameters.insert(parameter, json!(path));
            }
        }
    }
    action
}
pub fn files_complete(request: &mut GuestRequest) -> Option<HostAction> {
    let response = request.response.take().unwrap_or(Value::Null);
    if let Some(diagnostic) = request.error.take() {
        request.state["errors"]
            .as_array_mut()
            .unwrap()
            .push(json!(diagnostic));
        request.state["configuratorFilesComplete"] = json!(true);
        return None;
    }
    let Some(rows) = response["rows"].as_array() else {
        error(request, "The current app file query omitted rows.");
        request.state["configuratorFilesComplete"] = json!(true);
        return None;
    };
    let id = request.identity.as_ref().unwrap();
    let view = format!("Erp.UICfg.{}_{}", id.keys["ConfigID"], id.keys["Company"]);
    let company = if request.state["configuratorFileBranch"] == 0 {
        String::new()
    } else {
        id.keys["Company"].clone()
    };
    let size = request.page_size.unwrap() as usize;
    if rows.len() > size {
        error(
            request,
            "The current app file query exceeded its requested page size.",
        );
        request.state["configuratorFilesComplete"] = json!(true);
        return None;
    }
    for row in rows {
        if row["ProductID"] != "EP"
            || row["Key1"]
                .as_str()
                .is_some_and(|v| !v.eq_ignore_ascii_case(&view))
        {
            continue;
        }
        let path = row["Key2"].as_str().unwrap_or("").replace('\\', "/");
        let prefix = format!("{view}/");
        let name = path.get(prefix.len()..).filter(|_| {
            path.get(..prefix.len())
                .is_some_and(|v| v.eq_ignore_ascii_case(&prefix))
        });
        let measured = row["HerbertContentBytes"]
            .as_u64()
            .or_else(|| row["HerbertContentBytes"].as_str()?.parse().ok());
        let valid = row["Company"] == company
            && row["ProductID"] == "EP"
            && row["TypeCode"] == "KNTCMetaUI"
            && row["Key1"]
                .as_str()
                .is_some_and(|v| v.eq_ignore_ascii_case(&view))
            && row["Key3"] == ""
            && row["SystemFlag"] == false
            && row["SysRowID"].as_str().is_some_and(|v| !v.is_empty())
            && !row["SysRevID"].is_null()
            && row["Content"]
                .as_str()
                .is_some_and(|v| Some(v.encode_utf16().count() as u64 * 2) == measured)
            && name.is_some_and(|v| {
                !v.is_empty()
                    && !v.contains(':')
                    && !v.split('/').any(|p| p == ".." || p.is_empty())
            });
        if !valid {
            error(
                request,
                "An app file lacks exact custom ownership, identity, safe path or measured content length.",
            );
            continue;
        }
        let name = name.unwrap();
        let existing = request.state["supplements"]["ConfiguratorCurrentFiles"]["files"]
            .as_object()
            .unwrap()
            .keys()
            .find(|key| key.eq_ignore_ascii_case(name))
            .cloned();
        if existing.is_some() {
            error(
                request,
                "Multiple current app rows map to the same export filename; all original rows are retained without choosing a winner.",
            );
        } else {
            request.state["supplements"]["ConfiguratorCurrentFiles"]["files"][name] =
                row["Content"].clone();
        }
        request.state["supplements"]["ConfiguratorCurrentFiles"]["rows"]
            .as_array_mut()
            .unwrap()
            .push(row.clone());
    }
    if rows.len() == size {
        let after = rows.last().unwrap()["SysRowID"].clone();
        if after == request.state["configuratorFileAfter"]
            || after.as_str().is_none_or(str::is_empty)
        {
            error(request, "The current app file saved position did not advance.");
            request.state["configuratorFilesComplete"] = json!(true);
            return None;
        }
        request.state["configuratorFileAfter"] = after;
        return Some(files_query(request));
    }
    if request.state["configuratorFileBranch"] == 0 {
        request.state["configuratorFileBranch"] = json!(1);
        request.state["configuratorFileAfter"] = Value::Null;
        return Some(files_query(request));
    }
    request.state["configuratorFilesComplete"] = json!(true);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    #[test]
    fn configurator_native_event_text_is_declared_verbatim() {
        let text = "// Ω\r\nvar x = \"exact\";";
        let mut d = json!({"definition":{"PcInputs":[{"OnLeave":text,"OnLeave2":""}],"PcPage":[{"PromptWhen":"true"}]}});
        attach_sources(&mut d);
        assert_eq!(d["herbertSources"]["files"].as_array().unwrap().len(), 2);
        assert_eq!(d["herbertSources"]["files"][0]["text"], text);
    }
    fn req(kinetic: bool, kind: &str) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "configurators".into(),
            company: "C".into(),
            identity: Some(Identity {
                category: "configurators".into(),
                kind: "Configurator".into(),
                scope: Scope::Company {
                    company: "C".into(),
                },
                keys: BTreeMap::from([
                    ("Company".into(), "C".into()),
                    ("ConfigID".into(), "Cfg".into()),
                ]),
            }),
            cursor: None,
            page_size: None,
            state: json!({"errors":[],"configuratorFilesComplete":true,"configuratorAppPolicyChecked":true,"configuratorAppPolicyAllowed":true,"definition":{"PcStatus":[{"Company":"C","ConfigID":"Cfg","Kinetic":kinetic,"ConfigType":kind}]},"supplements":{"RetrievePcLayeredImagesForExport":{"parameters":{"configImportExportLayeredImageTableset":{"ConfigImportExportLayeredImage":[],"ExtensionTables":[]}}}}}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn layered_images_are_read_only_by_the_export_fallback() {
        let mut r = req(false, "PC");
        r.state["supplements"] = json!({});
        let Some(HostAction::SharedNativeRead { method, arguments, .. }) = start(&mut r) else { panic!() };
        assert_eq!(method, "RetrievePcLayeredImagesForExport");
        assert_eq!(arguments["configID"], "Cfg");
        assert_eq!(r.state["phase"], "configImages");
        // Once the reply is stored the same call proceeds to the native export (no second image read).
        r.state["supplements"] = req(false, "PC").state["supplements"].clone();
        assert!(matches!(start(&mut r), Some(HostAction::NativeFileExport { .. })));
    }
    #[test]
    fn app_policy_and_fallback_reads_are_scoped_before_download() {
        let mut r = req(true, "PC");
        r.page_size = Some(100);
        for branch in 0..4 {
            r.state["configuratorPolicyBranch"] = json!(branch);
            let HostAction::SharedQuery { query } = policy_query(&r) else {
                panic!()
            };
            assert_eq!(query.parameters["configView"], "Erp.UICfg.Cfg_C");
            assert!(
                query
                    .criteria
                    .iter()
                    .any(|c| c.field == if branch >= 2 { "Key2" } else { "Key1" }
                        && c.operator == "="
                        && c.parameter == "configView")
            );
        }
        let Some(HostAction::SharedQuery { query }) = files_start(&mut r, "fixture") else {
            panic!()
        };
        assert_eq!(query.parameters["configView"], "Erp.UICfg.Cfg_C");
        assert!(
            query
                .criteria
                .iter()
                .any(|c| c.field == "Key1" && c.operator == "=" && c.parameter == "configView")
        );
    }
    #[test]
    fn configurator_native_modes_declare_exact_output_set() {
        for kinetic in [false, true] {
            let Some(HostAction::NativeFileExport {
                arguments,
                extension,
                companion_extensions,
                ..
            }) = start(&mut req(kinetic, "PC"))
            else {
                panic!()
            };
            assert_eq!(extension, if kinetic { "zip" } else { "xml" });
            assert_eq!(
                companion_extensions,
                if kinetic { vec!["xml"] } else { vec![] }
            );
            assert!(arguments.get("fileName").is_none());
            assert_eq!(arguments["doInputs"], true);
        }
    }
    #[test]
    fn configurator_current_files_page_beyond_native_export_window() {
        let mut r = req(true, "PC");
        r.page_size = Some(17);
        let mut action = files_start(&mut r, "configuratorExportFiles");
        let mut index = 0;
        while let Some(HostAction::SharedQuery { query }) = action {
            assert!(
                query
                    .criteria
                    .iter()
                    .any(|v| v.field == "SystemFlag" && query.parameters[&v.parameter] == false)
            );
            let mut rows = Vec::new();
            if query.parameters[&query
                .criteria
                .iter()
                .find(|c| c.field == "Company")
                .unwrap()
                .parameter]
                == ""
            {
                for i in index..(index + 17).min(103) {
                    let text = format!("file {i} Ω");
                    rows.push(json!({"Company":"","ProductID":"EP","TypeCode":"KNTCMetaUI","Key1":"Erp.UICfg.Cfg_C","Key2":format!("Erp.UICfg.Cfg_C\\file{i}.jsonc"),"Key3":"","SystemFlag":false,"SysRowID":format!("row-{i:04}"),"SysRevID":1,"Content":text,"HerbertContentBytes":text.encode_utf16().count()*2}));
                }
                index += rows.len();
            }
            r.response = Some(json!({"rows":rows}));
            action = files_complete(&mut r);
        }
        assert_eq!(index, 103);
        assert_eq!(
            r.state["supplements"]["ConfiguratorCurrentFiles"]["files"]
                .as_object()
                .unwrap()
                .len(),
            103
        );
        assert!(r.state["errors"].as_array().unwrap().is_empty());
        let mut definition = json!({"definition":{"PcStatus":[{"Company":"C","ConfigID":"Cfg"}]},"supplements":r.state["supplements"]});
        attach_sources(&mut definition);
        assert_eq!(definition["herbertExportIdentity"]["kind"], "Configurator");
        assert_eq!(
            definition["herbertSources"]["files"]
                .as_array()
                .unwrap()
                .len(),
            103
        );
    }
    #[test]
    fn configurator_zip_system_child_is_withheld_before_export() {
        let mut r = req(true, "PC");
        r.state["configuratorAppPolicyChecked"] = json!(false);
        r.page_size = Some(2);
        let Some(HostAction::SharedQuery { query }) = start(&mut r) else {
            panic!()
        };
        assert!(
            query
                .fields
                .iter()
                .all(|f| f.field != "*" && f.field != "Content")
        );
        assert!(query.criteria.iter().any(|c| c.field == "Key1"));
        r.response = Some(
            json!({"rows":[{"SystemFlag":true,"SysRowID":"system-child","TypeCode":"KNTCMetaUI","Key1":"Erp.UICfg.Cfg_C"}]}),
        );
        assert!(policy_complete(&mut r).is_none());
        assert!(start(&mut r).is_none());
        assert_eq!(r.state["configuratorAppPolicyAllowed"], false);
    }
    #[test]
    fn configurator_queries_use_distinct_owner_scopes_and_reject_unrelated_policy_rows() {
        let mut first = req(true, "PC");
        first.page_size = Some(1);
        let mut second = first.clone();
        second
            .identity
            .as_mut()
            .unwrap()
            .keys
            .insert("ConfigID".into(), "Other".into());
        let a = policy_start(&mut first, "policy");
        let b = policy_start(&mut second, "policy");
        assert_ne!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
        first.response = Some(
            json!({"rows":[{"SystemFlag":true,"SysRowID":"a","TypeCode":"KNTCMetaUI","Key1":"OtherApplication"}]}),
        );
        assert!(policy_complete(&mut first).is_none());
        assert_eq!(first.state["configuratorAppPolicyAllowed"], false);
        assert!(!first.state["errors"].as_array().unwrap().is_empty());
        assert_ne!(
            serde_json::to_value(files_start(&mut first, "files")).unwrap(),
            serde_json::to_value(files_start(&mut second, "files")).unwrap()
        );
    }
    #[test]
    fn configurator_nic_respects_native_rejected_options() {
        let Some(HostAction::NativeFileExport { arguments, .. }) = start(&mut req(false, "NIC"))
        else {
            panic!()
        };
        for field in ["doInputs", "doSmartString", "doGlbVar"] {
            assert_eq!(arguments[field], false);
        }
        for field in ["doComments", "doSequence", "doDocRules", "doPcRules"] {
            assert_eq!(arguments[field], true);
        }
    }
    #[test]
    fn configurator_export_refuses_wrong_root_and_missing_companion() {
        let mut r = req(true, "PC");
        r.state["definition"]["PcStatus"][0]["Company"] = json!("Other");
        assert!(start(&mut r).is_none());
        let mut r = req(true, "PC");
        start(&mut r);
        r.response = Some(
            json!({"returnObj":{"nativeFile":{"path":"raw/a.zip","sha256":"hash","bytes":10,"role":"native"}}}),
        );
        complete(&mut r);
        assert_eq!(r.state["errors"].as_array().unwrap().len(), 1);
    }
}
