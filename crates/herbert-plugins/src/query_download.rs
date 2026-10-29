//! Download a query once. Its package is the source for diagrams and directives.
use herbert_contract::*;
use serde_json::json;

fn fail(message: &str) -> GuestReply {
    GuestReply::Failure {
        diagnostic: Diagnostic::new("QUERY_EXPORT_INVALID", message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    fn request(kind: &str) -> GuestRequest {
        let category = if kind == "UBAQ" { "ubaqs" } else { "baqs-and-ubaqs" };
        let mut id = crate::identity(category, kind, "C", "QueryID", "Owner's Query");
        id.keys.insert("Company".into(), "C".into());
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: category.into(),
            company: "C".into(),
            identity: Some(id),
            cursor: None,
            page_size: Some(200),
            state: Value::Null,
            response: None,
            error: None,
        }
    }
    #[test]
    fn query_download_requests_one_export_and_parses_all_members_locally() {
        for kind in ["BAQ", "UBAQ"] {
            let mut request = request(kind);
            let GuestReply::Action {
                action:
                    HostAction::SharedNativeRead {
                        method,
                        arguments,
                        export,
                        ..
                    },
                state,
            } = acquire(request.clone())
            else {
                panic!()
            };
            assert_eq!(method, "ExportBaq");
            assert!(export);
            assert_eq!(arguments["queryId"], "Owner's Query");
            request.state = state;
            request.response = Some(
                json!({"returnObj":{"nativeFile":{"path":"q.baq","sha256":"test","bytes":1,"role":"native"}}}),
            );
            let GuestReply::Action {
                action: HostAction::ParseXmlArchive { entries, .. },
                ..
            } = acquire(request)
            else {
                panic!()
            };
            assert_eq!(
                entries,
                vec!["DynamicQuery", "DynamicQueryDesign", "BpMethodDataset"]
            );
        }
    }
    #[test]
    fn failed_export_does_not_trigger_definition_or_diagram_reads() {
        for code in ["ACCESS_DENIED", "CANCELED", "TRANSFER_FAILED"] {
            let mut request = request("UBAQ");
            request.error = Some(Diagnostic::new(code, "Test failure."));
            assert!(
                matches!(acquire(request),GuestReply::Failure{diagnostic} if diagnostic.code==code)
            );
        }
    }
    #[test]
    fn invalid_package_does_not_trigger_a_second_download() {
        let mut request = request("UBAQ");
        request.state = json!({"phase":"queryPackage","file":{"path":"q.baq"}});
        request.response = Some(
            json!({"sourceFile":{"path":"q.baq"},"parsedXmlArchive":{"members":[],"documents":{}}}),
        );
        assert!(matches!(acquire(request), GuestReply::Failure { .. }));
    }
}

pub fn acquire(mut request: GuestRequest) -> GuestReply {
    if let Some(diagnostic) = request.error.take() {
        return GuestReply::Failure { diagnostic };
    }
    let Some(id) = request.identity.clone() else {
        return fail("Select a query from the inventory.");
    };
    let Some(query) = id.keys.get("QueryID").filter(|s| !s.is_empty()) else {
        return fail("The query ID is missing.");
    };
    let Some(company) = id.keys.get("Company") else {
        return fail("The query company is missing.");
    };
    if !company.is_empty() && company != &request.company {
        return fail("The query belongs to another company.");
    }
    match request.state["phase"].as_str().unwrap_or("initial") {
        "initial" => GuestReply::Action {
            action: HostAction::SharedNativeRead {
                service: "Ice.BO.BAQDesignerSvc".into(),
                method: "ExportBaq".into(),
                arguments: json!({"queryId":query,"options":{}}),
                export: true,
            },
            state: json!({"phase":"queryExport"}),
        },
        "queryExport" | "serverQueryExport" => {
            let Some(file) = request
                .response
                .as_ref()
                .and_then(|r| r.pointer("/returnObj/nativeFile").or_else(|| r.get("nativeFile")))
                .and_then(|v| serde_json::from_value::<FileRef>(v.clone()).ok())
            else {
                return fail("The query export returned no file.");
            };
            GuestReply::Action {
                action: HostAction::ParseXmlArchive {
                    file: file.clone(),
                    entries: vec![
                        "DynamicQuery".into(),
                        "DynamicQueryDesign".into(),
                        "BpMethodDataset".into(),
                    ],
                },
                state: json!({"phase":"queryPackage","file":file}),
            }
        }
        "queryPackage" => {
            let Some(response) = request.response else {
                return fail("The query package could not be read.");
            };
            if response["sourceFile"] != request.state["file"] {
                return fail("The parsed file does not match the downloaded query.");
            }
            let archive = &response["parsedXmlArchive"];
            let Some(members) = archive["members"].as_array() else {
                return fail("The query package has no member list.");
            };
            let dataset =
                match crate::xml_dataset::parse(&archive["documents"]["DynamicQuery"]["xml"]) {
                    Ok(ds) => ds,
                    Err(diagnostic) => return GuestReply::Failure { diagnostic },
                };
            let data = json!(dataset.tables);
            let Some(roots) = data["DynamicQueryDesigner"]
                .as_array()
                .filter(|r| r.len() == 1)
            else {
                return fail("The package must contain one query definition.");
            };
            let row = &roots[0];
            if row["Company"].as_str() != Some(company)
                || !row["QueryID"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case(query))
            {
                return fail("The package belongs to a different query or company.");
            }
            if crate::system_artifacts::marked(row) {
                return crate::system_artifacts::refused();
            }
            // The XML reader preserves scalar text exactly as exported.
            let updatable = match row["Updatable"].as_str() {
                Some("true" | "1") => Some(true),
                Some("false" | "0") => Some(false),
                _ => row["Updatable"].as_bool(),
            };
            if updatable != Some(id.kind == "UBAQ") {
                return fail("The package has a different query type from the inventory.");
            }
            let mut definition = json!({"service":"Ice.BO.BAQDesignerSvc","definition":data,"supplements":{"ExportBaq":{"returnObj":{"nativeFile":request.state["file"]}},"diagrams":archive["documents"]["DynamicQueryDesign"]}});
            if id.kind == "BAQ" {
                definition["herbertDirectOutput"] = json!(true);
                definition["supplements"].as_object_mut().unwrap().remove("ExportBaq");
            }
            let mut diagnostics = Vec::new();
            if !members.iter().any(|m| m["name"] == "DynamicQueryDesign") {
                diagnostics.push(Diagnostic::new(
                    "QUERY_DIAGRAM_MISSING",
                    "The export omitted its diagram member.",
                ));
            } else {
                let diagram = &archive["documents"]["DynamicQueryDesign"];
                if !diagram["diagnostic"].is_null() || !diagram["xml"]["name"].is_string() {
                    diagnostics.push(Diagnostic::new(
                        "QUERY_DIAGRAM_INVALID",
                        "The exported diagram could not be read. The original package is retained.",
                    ));
                }
            }
            if members.iter().any(|m| m["name"] == "BpMethodDataset") {
                match crate::xml_dataset::parse(&archive["documents"]["BpMethodDataset"]["xml"]) {
                    Ok(ds) => {
                        let bpm = json!(ds.tables);
                        match crate::bpm_package::query_target(&bpm) {
                            Ok((owner_company, owner_query))
                                if owner_company == *company
                                    && owner_query.eq_ignore_ascii_case(query) =>
                            {
                                diagnostics.extend(crate::bpm_code::attach(&mut definition, &bpm));
                                definition["supplements"]["queryDirectiveSource"] = bpm;
                            }
                            _ => diagnostics.push(Diagnostic::new(
                                "QUERY_DIRECTIVE_IDENTITY_INVALID",
                                "The embedded directives do not identify this query.",
                            )),
                        }
                    }
                    Err(error) => diagnostics.push(error),
                }
            }
            GuestReply::Definition {
                identity: id,
                format: "native-query-export".into(),
                definition,
                completeness: if diagnostics.is_empty() {
                    Completeness::Collected
                } else {
                    Completeness::Partial
                },
                diagnostics,
            }
        }
        _ => fail("The query download saved progress is invalid."),
    }
}
