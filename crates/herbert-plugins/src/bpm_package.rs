//! Download each native directive package once and process it locally.
use herbert_contract::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
// DQ method names are generated CLR identifiers (for example _/Example_Query).
// Their native BAQ result TypeInfo retains the authored company and query ID.
pub(crate) fn query_target(dataset: &Value) -> Result<(String, String), Diagnostic> {
    let mut identities = BTreeSet::new();
    for row in dataset["BpArgument"].as_array().into_iter().flatten() {
        if let Some(info) = row["TypeInfo"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        {
            if let (Some(company), Some(query)) = (info["company"].as_str(), info["query"].as_str())
            {
                if !query.is_empty() {
                    identities.insert((company.to_owned(), query.to_owned()));
                }
            }
        }
    }
    if identities.len() != 1 {
        return Err(Diagnostic::new(
            "BPM_QUERY_IDENTITY_UNVERIFIED",
            "Native query argument TypeInfo did not resolve exactly one authored company/query identity.",
        ));
    }
    Ok(identities.into_iter().next().unwrap())
}
const SERVICE: &str = "Ice.BO.BpMethodSvc";
fn push(r: &mut GuestRequest, code: &str, message: &str) {
    r.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(Diagnostic::new(code, message)));
}
fn done(r: &mut GuestRequest) -> Option<HostAction> {
    r.state["phase"] = json!("bpmComplete");
    None
}
fn root(r: &GuestRequest) -> &Value {
    &r.state["definition"]["BpMethod"][0]
}
pub fn methods() -> Vec<(String, String)> {
    ["GetRows", "ExportByService", "ExportByTable"]
        .into_iter()
        .map(|m| (SERVICE.into(), m.into()))
        .collect()
}
pub fn scoped_native_read(source: &str, filter: &str) -> HostAction {
    HostAction::NativeRead {
        service: SERVICE.into(),
        method: "GetRows".into(),
        arguments: json!({
            "whereClauseBpMethod":format!("Source = '{}' AND ({filter}) AND SystemFlag = false",source.replace('\'',"''")),
            "whereClauseBpArgument":"","whereClauseBpDirective":"","pageSize":0,"absolutePage":1
        }),
        export: false,
    }
}
fn export_arguments(row: &Value) -> (&'static str, Value) {
    if row["Source"] == "BO" {
        (
            "ExportByService",
            json!({"systemCode":row["SystemCode"],"serviceKind":row["ObjectNS"],"serviceName":row["BusinessObject"]}),
        )
    } else {
        (
            "ExportByTable",
            json!({"systemCode":row["SystemCode"],"tableName":row["BusinessObject"]}),
        )
    }
}
fn same_export(row: &Value, wanted: &Value) -> bool {
    ["Source", "SystemCode", "BusinessObject"]
        .iter()
        .all(|key| row[*key] == wanted[*key])
        && (wanted["Source"] != "BO" || row["ObjectNS"] == wanted["ObjectNS"])
}
// The native exporter cannot filter a service/table package. Read its small
// classification rows in shared pages once per run, never once per directive.
fn classification(r: &GuestRequest) -> HostAction {
    let mut parameters = BTreeMap::from([("source".into(), root(r)["Source"].clone())]);
    let mut criteria = vec![QueryCriterion {
        table: "Method".into(),
        field: "Source".into(),
        operator: "=".into(),
        parameter: "source".into(),
    }];
    if let Some(after) = r.state["bpmAfter"].as_str() {
        parameters.insert("after".into(), json!(after));
        criteria.push(QueryCriterion {
            table: "Directive".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
    }
    let mut fields: Vec<_> = [
        "Source",
        "SystemCode",
        "BusinessObject",
        "ObjectNS",
        "SystemFlag",
    ]
    .into_iter()
    .map(|f| QueryField {
        table: "Method".into(),
        field: f.into(),
        alias: f.into(),
        expression: None,
    })
    .collect();
    fields.extend(["Company", "SysRowID"].into_iter().map(|f| QueryField {
        table: "Directive".into(),
        field: f.into(),
        alias: f.into(),
        expression: None,
    }));
    HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![
                QueryTable {
                    schema: "Ice".into(),
                    table: "BpMethod".into(),
                    alias: "Method".into(),
                    company_field: None,
                    metadata: vec![],
                },
                QueryTable {
                    schema: "Ice".into(),
                    table: "BpDirective".into(),
                    alias: "Directive".into(),
                    company_field: None,
                    metadata: vec![],
                },
            ],
            fields,
            joins: ["Source", "BpMethodCode"]
                .into_iter()
                .map(|f| QueryJoin {
                    left_table: "Method".into(),
                    left_field: f.into(),
                    right_table: "Directive".into(),
                    right_field: f.into(),
                    kind: "Inner".into(),
                })
                .collect(),
            criteria,
            parameters,
            order: vec![QueryOrder {
                field: "SysRowID".into(),
                descending: false,
            }],
            page_size: r.page_size,
            page: 1,
        },
    }
}
pub fn start(r: &mut GuestRequest) -> Option<HostAction> {
    let row = root(r);
    if !matches!(row["Source"].as_str(), Some("BO" | "DB"))
        || ["SystemCode", "BusinessObject"]
            .iter()
            .any(|f| row[*f].as_str().is_none_or(str::is_empty))
        || (row["Source"] == "BO" && row["ObjectNS"].as_str().is_none_or(str::is_empty))
    {
        push(
            r,
            "BPM_EXPORT_SCOPE_MISSING",
            "The directive is missing its native service or table identity.",
        );
        return done(r);
    }
    // The inventory already supplied these full native rows, including protected
    // bodies. Keep them; do not download them again to compare their revisions.
    r.state["bpmScope"] = r.state["definition"].clone();
    let Some(rows) = r.state["definition"]["BpDirective"].as_array() else {
        push(
            r,
            "BPM_DIRECTIVES_MISSING",
            "The inventory omitted the directive table.",
        );
        return done(r);
    };
    if rows.is_empty() || (rows.iter().all(|row| row["IsProtected"] == true)
        && !crate::method_archive::needs_native_export(&r.state["definition"]).unwrap_or(true)) {
        return done(r);
    }
    r.state["phase"] = json!("bpmClassification");
    r.state["bpmAfter"] = Value::Null;
    Some(classification(r))
}
pub fn complete(r: &mut GuestRequest) -> Option<HostAction> {
    if let Some(error) = r.error.take() {
        r.state["errors"].as_array_mut().unwrap().push(json!(error));
        return done(r);
    }
    let Some(response) = r.response.take() else {
        push(
            r,
            "BPM_RESPONSE_MISSING",
            "The directive download returned no response.",
        );
        return done(r);
    };
    match r.state["phase"].as_str().unwrap_or("") {
        "bpmClassification" => {
            let Some(rows) = response["rows"].as_array() else {
                push(
                    r,
                    "BPM_CLASSIFICATION_MISSING",
                    "The directive classification query omitted its rows.",
                );
                return done(r);
            };
            for row in rows {
                if !row["SystemFlag"].is_boolean()
                    || !row["Company"].is_string()
                    || !row["SysRowID"].is_string()
                {
                    push(
                        r,
                        "BPM_CLASSIFICATION_INVALID",
                        "A directive classification row is incomplete.",
                    );
                    return done(r);
                }
                if same_export(row, root(r))
                    && (crate::system_artifacts::marked(row)
                        || row["Company"]
                            .as_str()
                            .is_some_and(|c| !c.is_empty() && c != r.company))
                {
                    push(
                        r,
                        "BPM_EXPORT_SCOPE_EXCLUDED",
                        "The native package includes System directives or another company. The selected custom definitions are retained without that package.",
                    );
                    return done(r);
                }
            }
            if r.page_size.is_some_and(|size| rows.len() >= size as usize) {
                let Some(after) = rows
                    .last()
                    .and_then(|row| row["SysRowID"].as_str())
                    .filter(|after| Some(*after) != r.state["bpmAfter"].as_str())
                else {
                    push(
                        r,
                        "BPM_PAGING_INVALID",
                        "The directive classification page did not advance.",
                    );
                    return done(r);
                };
                r.state["bpmAfter"] = json!(after);
                return Some(classification(r));
            }
            let (method, arguments) = export_arguments(root(r));
            r.state["phase"] = json!("bpmExport");
            Some(HostAction::SharedNativeRead {
                service: SERVICE.into(),
                method: method.into(),
                arguments,
                export: true,
            })
        }
        "bpmExport" => {
            let (method, args) = export_arguments(root(r));
            r.state["errors"].as_array_mut().unwrap().extend(
                crate::export_contracts::validate(SERVICE, method, &args, &response)
                    .into_iter()
                    .map(|e| json!(e)),
            );
            r.state["supplements"]["directivePackage"] = response.clone();
            let Some(file) = response
                .pointer("/returnObj/Data/nativeFile")
                .cloned()
                .and_then(|f| serde_json::from_value::<FileRef>(f).ok())
            else {
                push(
                    r,
                    "BPM_PACKAGE_BYTES_MISSING",
                    "The directive export returned no package file.",
                );
                return done(r);
            };
            r.state["bpmFile"] = json!(file);
            r.state["phase"] = json!("bpmArchive");
            Some(HostAction::ParseXmlArchive {
                file,
                entries: vec!["BpMethodDataset".into()],
            })
        }
        "bpmArchive" => {
            if response["sourceFile"] != r.state["bpmFile"] {
                push(
                    r,
                    "BPM_PACKAGE_SOURCE_MISMATCH",
                    "The parsed file is not the downloaded directive package.",
                );
                return done(r);
            }
            let dataset = match crate::xml_dataset::parse(
                &response["parsedXmlArchive"]["documents"]["BpMethodDataset"]["xml"],
            ) {
                Ok(ds) => ds,
                Err(error) => {
                    r.state["errors"].as_array_mut().unwrap().push(json!(error));
                    return done(r);
                }
            };
            let roots = dataset.tables.get("BpMethod");
            if roots.is_none_or(|rows| {
                !rows
                    .iter()
                    .any(|row| row["BpMethodCode"] == root(r)["BpMethodCode"])
                    || rows.iter().any(|row| {
                        !same_export(row, root(r)) || crate::system_artifacts::marked(row)
                    })
            }) {
                push(
                    r,
                    "BPM_PACKAGE_IDENTITY_INVALID",
                    "The package does not match the selected custom directive service or table.",
                );
            }
            done(r)
        }
        _ => {
            push(
                r,
                "BPM_STATE_INVALID",
                "The directive download checkpoint is invalid.",
            );
            done(r)
        }
    }
}

#[cfg(test)]
mod download_tests {
    use super::*;
    fn request(method: &str, protected: bool) -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "C06".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(200),
            response: None,
            error: None,
            state: json!({"errors":[],"supplements":{},"definition":{
                "BpMethod":[{"Source":"BO","BpMethodCode":method,"SystemCode":"Ice","ObjectNS":"BO","BusinessObject":"Example"}],
                "BpArgument":[],"BpDirective":[{"Company":"C","Source":"BO","BpMethodCode":method,"IsProtected":protected,"Body":"<Workflow>Ω unchanged</Workflow>"}]
            }}),
        }
    }
    #[test]
    fn methods_share_classification_and_export_without_per_directive_reads() {
        let mut keys = Vec::new();
        for method in ["Example.Update", "Example.GetByID"] {
            let mut r = request(method, false);
            let Some(action @ HostAction::SharedQuery { .. }) = start(&mut r) else {
                panic!()
            };
            keys.push(serde_json::to_value(action).unwrap());
            r.response = Some(json!({"rows":[]}));
            let Some(HostAction::SharedNativeRead {
                method,
                arguments,
                export,
                ..
            }) = complete(&mut r)
            else {
                panic!()
            };
            assert_eq!(method, "ExportByService");
            assert_eq!(
                arguments,
                json!({"systemCode":"Ice","serviceKind":"BO","serviceName":"Example"})
            );
            assert!(export);
            assert_eq!(
                r.state["bpmScope"]["BpDirective"][0]["Body"],
                "<Workflow>Ω unchanged</Workflow>"
            );
            r.error = Some(Diagnostic::new("ACCESS_DENIED", "Fixture refusal"));
            assert!(complete(&mut r).is_none());
            assert_eq!(r.state["errors"][0]["code"], "ACCESS_DENIED");
        }
        assert_eq!(
            keys[0], keys[1],
            "Both methods must use the same cached metadata query"
        );
    }
    #[test]
    fn protected_bodies_and_empty_methods_need_no_more_reads() {
        let mut r = request("Example.Update", true);
        assert!(start(&mut r).is_none());
        assert_eq!(
            r.state["bpmScope"]["BpDirective"][0]["Body"],
            "<Workflow>Ω unchanged</Workflow>"
        );
        assert!(r.state["errors"].as_array().unwrap().is_empty());
        r.state["definition"]["BpDirective"] = json!([]);
        assert!(start(&mut r).is_none());
    }
    #[test]
    fn shared_classification_pages_advance_and_exclude_mixed_system_packages() {
        let mut r = request("Example.Update", false);
        r.page_size = Some(1);
        start(&mut r);
        r.response = Some(
            json!({"rows":[{"Source":"BO","SystemCode":"Ice","ObjectNS":"BO","BusinessObject":"Other","SystemFlag":true,"Company":"C","SysRowID":"one"}]}),
        );
        let Some(HostAction::SharedQuery { query }) = complete(&mut r) else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "one");
        r.response = Some(
            json!({"rows":[{"Source":"BO","SystemCode":"Ice","ObjectNS":"BO","BusinessObject":"Example","SystemFlag":true,"Company":"C","SysRowID":"two"}]}),
        );
        assert!(complete(&mut r).is_none());
        assert_eq!(r.state["errors"][0]["code"], "BPM_EXPORT_SCOPE_EXCLUDED");
    }
}
